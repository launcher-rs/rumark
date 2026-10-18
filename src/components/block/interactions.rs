//! 聚焦块上按键触发的动作处理器。每个处理器对应 [`crate::components::actions`] 中声明的命名动作，结构变更经 `BlockEvent` 委托给父编辑器。

use std::time::Duration;

use rgpui::*;

use super::CaretAffinity;
use super::{
    Block, BlockEvent, BlockKind, InlineFormat, InlineTextTree, PastedImageSource,
    SelectExtendMode, UndoKind,
};
use crate::components::markdown::paste::should_split_plain_multiline_paste;
use crate::components::{
    BlockDown, BlockUp, BoldSelection, CodeSelection, Copy, Cut, Delete, DeleteBack,
    DismissTransientUi, End, ExitCodeBlock, FocusNext, FocusPrev, Home, IndentBlock,
    ItalicSelection, MoveLeft, MoveRight, Newline, OutdentBlock, Paste, SelectAll, SelectEnd,
    SelectHome, SelectLeft, SelectRight, UnderlineSelection, WordDeleteBack, WordDeleteForward,
    WordMoveLeft, WordMoveRight, WordSelectLeft, WordSelectRight,
};

impl Block {
    /// 从剪贴板条目解析粘贴图片来源。
    fn pasted_image_source_from_clipboard(item: &ClipboardItem) -> Option<PastedImageSource> {
        item.entries().iter().find_map(|entry| match entry {
            ClipboardEntry::Image(image) => Some(PastedImageSource::ClipboardImage(image.clone())),
            ClipboardEntry::String(_) => None,
            ClipboardEntry::ExternalPaths(_) => None,
        })
    }

    /// 从剪贴板纯文本解析粘贴图片来源。
    fn pasted_image_source_from_text(text: &str) -> Option<PastedImageSource> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.contains('\n') || trimmed.contains('\r') {
            return None;
        }

        Self::pasted_image_path_from_text_item(trimmed).map(PastedImageSource::LocalPath)
    }

    /// 把单个剪贴板文本项解析为本地图片路径。
    ///
    /// Windows 文件复制粘贴以纯盘符路径到达界面，需先按路径判断，因为 `url::Url` 会把盘符当作网址协议。
    fn pasted_image_path_from_text_item(text: &str) -> Option<std::path::PathBuf> {
        let unquoted = text
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(text);
        let direct_path = std::path::PathBuf::from(unquoted);
        let path = if Self::is_supported_local_image_path(&direct_path) {
            direct_path
        } else if let Ok(url) = url::Url::parse(unquoted) {
            if url.scheme() == "file" {
                url.to_file_path().ok()?
            } else {
                return None;
            }
        } else {
            return None;
        };
        if !Self::is_supported_local_image_path(&path) {
            return None;
        }
        Some(path)
    }

    /// 是否为受支持的本地图片路径。
    fn is_supported_local_image_path(path: &std::path::Path) -> bool {
        if !path.is_file() {
            return false;
        }
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            return false;
        };
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff"
        )
    }

    /// 拆分粘贴图片时的首尾文本。
    fn paste_image_split(&self) -> (InlineTextTree, InlineTextTree) {
        let clean_selected = self.selection_clean_range();
        let (leading, tail) = self.record.title.split_at(clean_selected.start);
        let (_, trailing) = tail.split_at(clean_selected.end.saturating_sub(clean_selected.start));
        (leading, trailing)
    }

    /// 是否为无子块的单行引用。
    fn is_leaf_quote(&self) -> bool {
        self.kind() == BlockKind::Quote
            && self.children.is_empty()
            && !self.rendered_text().contains('\n')
    }

    /// 是否为无子块的提示框。
    fn is_leaf_callout(&self) -> bool {
        matches!(self.kind(), BlockKind::Callout(_)) && self.children.is_empty()
    }

    /// 是否为空的单行引用叶子。
    fn is_empty_leaf_quote(&self) -> bool {
        self.is_leaf_quote() && self.selected_range.is_empty() && self.is_empty()
    }

    /// 把行首的空提示框叶子降级为引用。
    fn downgrade_leaf_callout_to_quote_at_start(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.is_leaf_callout() || !self.selected_range.is_empty() || self.caret_offset() != 0 {
            return false;
        }

        let BlockKind::Callout(variant) = self.kind() else {
            return false;
        };
        let heading_markdown = variant.heading_markdown(&self.record.title.serialize_markdown());
        self.record.kind = BlockKind::Quote;
        self.record
            .set_title(InlineTextTree::from_markdown(&heading_markdown));
        self.sync_edit_mode_from_kind();
        self.refresh_render_cache();
        self.assign_collapsed_selection_offset(0, CaretAffinity::Default, None);
        self.marked_range = None;
        self.cursor_blink_epoch = std::time::Instant::now();
        cx.emit(BlockEvent::Changed);
        cx.notify();
        true
    }

    /// 把空引用叶子降级为段落。
    fn downgrade_empty_leaf_quote_to_paragraph(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_empty_leaf_quote() {
            self.make_paragraph(cx);
            return true;
        }
        false
    }

    /// 追加列按钮是否应保持可见。
    fn table_append_column_should_stay_visible(&self) -> bool {
        self.table_append_column_edge_hovered
            || self.table_append_column_zone_hovered
            || self.table_append_column_button_hovered
    }

    /// 追加行按钮是否应保持可见。
    fn table_append_row_should_stay_visible(&self) -> bool {
        self.table_append_row_edge_hovered
            || self.table_append_row_zone_hovered
            || self.table_append_row_button_hovered
    }

    /// 计划关闭追加列按钮。
    fn schedule_table_append_column_close(&mut self, cx: &mut Context<Self>) {
        if !self.table_append_column_hovered {
            return;
        }

        self.table_append_column_close_task = Some(cx.spawn(
            async |this: WeakEntity<Block>, cx: &mut AsyncApp| {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                let _ = this.update(cx, |block, cx| {
                    block.table_append_column_close_task = None;
                    if !block.table_append_column_should_stay_visible() {
                        block.table_append_column_hovered = false;
                        cx.notify();
                    }
                });
            },
        ));
    }

    /// 计划关闭追加行按钮。
    fn schedule_table_append_row_close(&mut self, cx: &mut Context<Self>) {
        if !self.table_append_row_hovered {
            return;
        }

        self.table_append_row_close_task = Some(cx.spawn(
            async |this: WeakEntity<Block>, cx: &mut AsyncApp| {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                let _ = this.update(cx, |block, cx| {
                    block.table_append_row_close_task = None;
                    if !block.table_append_row_should_stay_visible() {
                        block.table_append_row_hovered = false;
                        cx.notify();
                    }
                });
            },
        ));
    }

    /// 设置追加列悬停子状态。
    fn set_table_append_column_hover_part(
        &mut self,
        edge_hovered: Option<bool>,
        zone_hovered: Option<bool>,
        button_hovered: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        if let Some(edge_hovered) = edge_hovered
            && self.table_append_column_edge_hovered != edge_hovered
        {
            self.table_append_column_edge_hovered = edge_hovered;
            changed = true;
        }
        if let Some(zone_hovered) = zone_hovered
            && self.table_append_column_zone_hovered != zone_hovered
        {
            self.table_append_column_zone_hovered = zone_hovered;
            changed = true;
        }
        if let Some(button_hovered) = button_hovered
            && self.table_append_column_button_hovered != button_hovered
        {
            self.table_append_column_button_hovered = button_hovered;
            changed = true;
        }

        if self.table_append_column_should_stay_visible() {
            self.table_append_column_close_task = None;
            if !self.table_append_column_hovered {
                self.table_append_column_hovered = true;
                changed = true;
            }
        } else if self.table_append_column_hovered && self.table_append_column_close_task.is_none()
        {
            self.schedule_table_append_column_close(cx);
        }

        if changed {
            cx.notify();
        }
    }

    /// 设置追加行悬停子状态。
    fn set_table_append_row_hover_part(
        &mut self,
        edge_hovered: Option<bool>,
        zone_hovered: Option<bool>,
        button_hovered: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        if let Some(edge_hovered) = edge_hovered
            && self.table_append_row_edge_hovered != edge_hovered
        {
            self.table_append_row_edge_hovered = edge_hovered;
            changed = true;
        }
        if let Some(zone_hovered) = zone_hovered
            && self.table_append_row_zone_hovered != zone_hovered
        {
            self.table_append_row_zone_hovered = zone_hovered;
            changed = true;
        }
        if let Some(button_hovered) = button_hovered
            && self.table_append_row_button_hovered != button_hovered
        {
            self.table_append_row_button_hovered = button_hovered;
            changed = true;
        }

        if self.table_append_row_should_stay_visible() {
            self.table_append_row_close_task = None;
            if !self.table_append_row_hovered {
                self.table_append_row_hovered = true;
                changed = true;
            }
        } else if self.table_append_row_hovered && self.table_append_row_close_task.is_none() {
            self.schedule_table_append_row_close(cx);
        }

        if changed {
            cx.notify();
        }
    }

    /// 若代码块末行为裸围栏（三个以上反引号或波浪线、无信息串），返回应裁掉的字节偏移，否则为 `None`。
    fn trailing_code_fence_line_start(&self) -> Option<usize> {
        let text = self.rendered_text();
        let line_start = text.rfind('\n').map(|idx| idx + 1).unwrap_or(0);
        let is_bare_fence = BlockKind::parse_fence_opening(&text[line_start..])
            .is_some_and(|fence| fence.language.is_none());
        // 除仅剩围栏一行外，连前面的换行一起裁掉。
        is_bare_fence.then(|| line_start.saturating_sub(1))
    }

    /// 处理回车换行。
    pub(crate) fn on_newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        // 回车按特殊编辑器到富文本拆分的顺序处理：表格/源码/代码/引用类块保留本地换语义，普通渲染块发出编辑器级拆分请求。
        if self.is_table_cell() {
            cx.emit(BlockEvent::RequestTableCellMoveVertical { delta: 1 });
            return;
        }

        if self.editor_selection_range.is_some() {
            cx.emit(BlockEvent::RequestReplaceCrossBlockSelection {
                text: "\n".to_string(),
                selected_range_relative: None,
                mark_inserted_text: false,
                undo_kind: UndoKind::NonCoalescible,
            });
            return;
        }

        if self.is_source_raw_mode() {
            if !self.selected_range.is_empty() {
                self.replace_text_in_range(None, "", window, cx);
            }
            self.replace_text_in_range(None, "\n", window, cx);
            return;
        }

        if self.kind() == BlockKind::Paragraph
            && self.selected_range.is_empty()
            && self.caret_offset() == self.plain_len()
            && BlockKind::parse_separator(self.rendered_text())
            // 短横线串也是 Setext 下划线；交给编辑器处理，以便前一段落可变为标题（无标题目标时编辑器回退为分隔线）。
            && BlockKind::parse_setext_rule(self.rendered_text()).is_none()
        {
            self.convert_to_separator(cx);
            cx.emit(BlockEvent::RequestNewline {
                trailing: InlineTextTree::plain(String::new()),
                source_already_mutated: true,
            });
            return;
        }

        // `$$` 后按回车打开展示数学块。以光标紧跟行首 `$$` 为触发（而非整行恰为 `$$`），在已有行首 Home 后补围栏时同样生效：行内剩余部分成为数学正文，而非拆出新段落。
        if self.kind() == BlockKind::Paragraph
            && self.selected_range.is_empty()
            && self.caret_offset() == "$$".len()
            && self.rendered_text().starts_with("$$")
        {
            let body = self.rendered_text()["$$".len()..].to_string();
            self.open_math_block(&body, cx);
            return;
        }

        if self.kind() == BlockKind::Paragraph
            && self.selected_range.is_empty()
            && self.caret_offset() == self.plain_len()
            && let Some(fence) = BlockKind::parse_fence_opening(self.rendered_text())
        {
            self.open_code_block(fence.language, cx);
            return;
        }

        if self.kind().is_separator() {
            cx.emit(BlockEvent::RequestNewline {
                trailing: InlineTextTree::plain(String::new()),
                source_already_mutated: false,
            });
            return;
        }

        if self.kind().is_list_item() && self.selected_range.is_empty() && self.is_empty() {
            cx.emit(BlockEvent::RequestOutdent);
            return;
        }

        if self.kind() == BlockKind::Quote {
            if !self.selected_range.is_empty() {
                self.replace_text_in_range(None, "", window, cx);
            }
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            self.replace_text_in_range(None, "\n", window, cx);
            return;
        }

        if matches!(self.kind(), BlockKind::Callout(_)) {
            cx.emit(BlockEvent::RequestEnterCalloutBody);
            return;
        }

        // 在代码块中，回车向块内容插入换行而非拆块。空代码块上按回车退回段落。
        if self.kind().is_code_block() {
            if self.selected_range.is_empty() && self.is_empty() {
                self.make_paragraph(cx);
                return;
            }
            // 在末行键入裸结束围栏后按回车即离开该块，与源码模式一致。
            if self.selected_range.is_empty()
                && self.caret_offset() == self.plain_len()
                && let Some(fence_start) = self.trailing_code_fence_line_start()
            {
                let fence_end = self.plain_len();
                self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
                self.replace_text_in_visible_range(fence_start..fence_end, "", None, false, cx);
                cx.emit(BlockEvent::RequestNewline {
                    trailing: InlineTextTree::plain(String::new()),
                    source_already_mutated: true,
                });
                return;
            }
            if !self.selected_range.is_empty() {
                self.replace_text_in_range(None, "", window, cx);
            }
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            self.replace_text_in_range(None, "\n", window, cx);
            return;
        }

        if !self.selected_range.is_empty() {
            self.replace_text_in_range(None, "", window, cx);
        }

        let cursor = self.caret_offset();
        let (leading, trailing) = self.split_title(cursor);
        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        self.record.set_title(leading);
        self.mark_changed(cx);
        let cursor = self.plain_len();
        self.assign_collapsed_selection_offset(cursor, CaretAffinity::Default, None);
        self.marked_range = None;
        cx.emit(BlockEvent::RequestNewline {
            trailing,
            source_already_mutated: true,
        });
    }

    /// 处理退格删除。
    pub(crate) fn on_delete_back(
        &mut self,
        _: &DeleteBack,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_table_cell() {
            if self.selected_range.is_empty() {
                let previous = self.previous_boundary(self.caret_offset());
                if previous == self.caret_offset() {
                    return;
                }
                self.select_to(previous, cx);
            }
            self.replace_text_in_range(None, "", window, cx);
            return;
        }

        if self.is_source_raw_mode() {
            if self.selected_range.is_empty() {
                self.select_to(self.previous_boundary(self.caret_offset()), cx);
            }
            self.replace_text_in_range(None, "", window, cx);
            return;
        }

        if self.selected_range.is_empty() && self.caret_offset() == 0 {
            if self.kind() == BlockKind::Paragraph && self.is_direct_list_child() && self.is_empty()
            {
                cx.emit(BlockEvent::RequestOutdent);
                return;
            }
            if self.is_nested_list_item() {
                cx.emit(BlockEvent::RequestDowngradeNestedListItemToChildParagraph);
                return;
            }
            match self.kind() {
                BlockKind::BulletedListItem
                | BlockKind::TaskListItem { .. }
                | BlockKind::NumberedListItem => {
                    cx.emit(BlockEvent::RequestOutdent);
                    return;
                }
                BlockKind::Heading { .. } => {
                    self.make_paragraph(cx);
                    return;
                }
                BlockKind::Quote => {
                    if self.is_leaf_quote() {
                        self.make_paragraph(cx);
                    }
                    return;
                }
                BlockKind::Callout(_) => {
                    if self.downgrade_leaf_callout_to_quote_at_start(cx) {
                        return;
                    }
                    return;
                }
                BlockKind::Separator => {
                    self.make_paragraph(cx);
                    return;
                }
                BlockKind::CodeBlock { .. } => {
                    self.make_paragraph(cx);
                    return;
                }
                _ => {}
            }
        }

        if self.downgrade_leaf_callout_to_quote_at_start(cx)
            || self.downgrade_empty_leaf_quote_to_paragraph(cx)
        {
            return;
        }

        if self.selected_range.is_empty() && self.rendered_text().is_empty() {
            cx.emit(BlockEvent::RequestDelete);
            return;
        }

        if self.selected_range.is_empty() && self.caret_offset() == 0 {
            cx.emit(BlockEvent::RequestMergeIntoPrev {
                content: self.record.title.clone(),
            });
            return;
        }

        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.caret_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    /// 处理向前删除。
    pub(crate) fn on_delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_table_cell() {
            if self.selected_range.is_empty() {
                let next = self.next_boundary(self.caret_offset());
                if next == self.caret_offset() {
                    return;
                }
                self.select_to(next, cx);
            }
            self.replace_text_in_range(None, "", window, cx);
            return;
        }

        if self.is_source_raw_mode() {
            if self.selected_range.is_empty() {
                self.select_to(self.next_boundary(self.caret_offset()), cx);
            }
            self.replace_text_in_range(None, "", window, cx);
            return;
        }

        if self.downgrade_leaf_callout_to_quote_at_start(cx)
            || self.downgrade_empty_leaf_quote_to_paragraph(cx)
        {
            return;
        }

        if self.kind().is_separator() {
            self.make_paragraph(cx);
            return;
        }

        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.caret_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    /// 按词向后删除。
    pub(crate) fn on_word_delete_back(
        &mut self,
        _: &WordDeleteBack,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            if self.caret_offset() == 0 {
                // 块内左侧已无内容；交给字形退格处理块合并与降级。
                self.on_delete_back(&DeleteBack, window, cx);
                return;
            }
            self.select_to(self.prev_word_boundary(self.caret_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    /// 按词向前删除。
    pub(crate) fn on_word_delete_forward(
        &mut self,
        _: &WordDeleteForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            if self.caret_offset() == self.plain_len() {
                // 块内右侧已无内容；交给字形删除处理块合并与分隔线移除。
                self.on_delete(&Delete, window, cx);
                return;
            }
            self.select_to(self.next_word_boundary(self.caret_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    /// 缩进块。
    pub(crate) fn on_indent_block(
        &mut self,
        _: &IndentBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_table_cell() {
            cx.emit(BlockEvent::RequestTableCellMoveHorizontal { delta: 1 });
            return;
        }
        if self.can_change_list_nesting() {
            cx.emit(BlockEvent::RequestIndent);
            return;
        }
        if self.kind() == BlockKind::Paragraph || self.kind().is_code_block() {
            self.replace_text_in_range(None, "    ", window, cx);
        }
    }

    /// 取消缩进块。
    pub(crate) fn on_outdent_block(
        &mut self,
        _: &OutdentBlock,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_table_cell() {
            cx.emit(BlockEvent::RequestTableCellMoveHorizontal { delta: -1 });
            return;
        }
        if self.can_unindent_list_nesting() {
            cx.emit(BlockEvent::RequestOutdent);
        }
    }

    /// 处理块按键按下。
    pub(crate) fn on_block_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key != "tab" {
            return;
        }

        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt || modifiers.function {
            return;
        }

        if self.code_language_focus_handle.is_focused(window) {
            return;
        }

        if modifiers.shift {
            self.on_outdent_block(&OutdentBlock, window, cx);
        } else {
            self.on_indent_block(&IndentBlock, window, cx);
        }
        cx.stop_propagation();
    }

    /// 把焦点移到上一块。
    pub(crate) fn on_focus_prev(
        &mut self,
        _: &FocusPrev,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let preferred_x = self.vertical_anchor_x();
        if !self.move_cursor_vertically(-1, preferred_x, cx) {
            if self.is_table_cell() {
                cx.emit(BlockEvent::RequestTableCellMoveVertical { delta: -1 });
                return;
            }
            cx.emit(BlockEvent::RequestFocusPrev {
                preferred_x: Some(f32::from(preferred_x)),
            });
        }
    }

    /// 把焦点移到下一块。
    pub(crate) fn on_focus_next(
        &mut self,
        _: &FocusNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let preferred_x = self.vertical_anchor_x();
        if !self.move_cursor_vertically(1, preferred_x, cx) {
            if self.is_table_cell() {
                cx.emit(BlockEvent::RequestTableCellMoveVertical { delta: 1 });
                return;
            }
            // 在代码块中，从末行内容按 Down 先进入语言栏而非离开块，以便键盘可达；再按 Down 才退出。
            if self.kind().is_code_block() && !self.code_language_focus_handle.is_focused(window) {
                self.code_language_focus_handle.focus(window, cx);
                cx.notify();
                return;
            }
            cx.emit(BlockEvent::RequestFocusNext {
                preferred_x: Some(f32::from(preferred_x)),
            });
        }
    }

    /// 左移光标。
    pub(crate) fn on_move_left(
        &mut self,
        _: &MoveLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            if let Some((target, affinity)) = self.projected_move_left_target(self.caret_offset()) {
                self.assign_collapsed_selection_offset(target, affinity, None);
                self.cursor_blink_epoch = std::time::Instant::now();
                cx.notify();
            } else {
                let previous = self.previous_boundary(self.caret_offset());
                // 在表格单元格开头，步入上一单元格而非卡在边缘（与 Shift+Tab 同路）。
                if previous == self.caret_offset() && self.is_table_cell() {
                    cx.emit(BlockEvent::RequestTableCellMoveHorizontal { delta: -1 });
                    return;
                }
                self.move_to(previous, cx);
            }
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    /// 右移光标。
    pub(crate) fn on_move_right(
        &mut self,
        _: &MoveRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            if let Some((target, affinity)) =
                self.projected_move_right_target(self.selected_range.end)
            {
                self.assign_collapsed_selection_offset(target, affinity, None);
                self.cursor_blink_epoch = std::time::Instant::now();
                cx.notify();
            } else {
                let next = self.next_boundary(self.selected_range.end);
                // 在表格单元格末尾，步入下一单元格而非卡在边缘（与 Tab 同路）。
                if next == self.selected_range.end && self.is_table_cell() {
                    cx.emit(BlockEvent::RequestTableCellMoveHorizontal { delta: 1 });
                    return;
                }
                self.move_to(next, cx);
            }
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    /// 移到行首。
    pub(crate) fn on_home(&mut self, _: &Home, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    /// 移到行尾。
    pub(crate) fn on_end(&mut self, _: &End, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.plain_len(), cx);
    }

    /// 向左扩展选区。
    pub(crate) fn on_select_left(
        &mut self,
        _: &SelectLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((target, _)) = self.projected_move_left_target(self.caret_offset()) {
            self.select_to(target, cx);
        } else {
            self.select_to(self.previous_boundary(self.caret_offset()), cx);
        }
    }

    /// 向右扩展选区。
    pub(crate) fn on_select_right(
        &mut self,
        _: &SelectRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((target, _)) = self.projected_move_right_target(self.caret_offset()) {
            self.select_to(target, cx);
        } else {
            self.select_to(self.next_boundary(self.caret_offset()), cx);
        }
    }

    /// 按词左移。
    pub(crate) fn on_word_move_left(
        &mut self,
        _: &WordMoveLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to(self.prev_word_boundary(self.caret_offset()), cx);
    }

    /// 按词右移。
    pub(crate) fn on_word_move_right(
        &mut self,
        _: &WordMoveRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to(self.next_word_boundary(self.caret_offset()), cx);
    }

    /// 按词向左扩展选区。
    pub(crate) fn on_word_select_left(
        &mut self,
        _: &WordSelectLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.prev_word_boundary(self.caret_offset()), cx);
    }

    /// 按词向右扩展选区。
    pub(crate) fn on_word_select_right(
        &mut self,
        _: &WordSelectRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.next_word_boundary(self.caret_offset()), cx);
    }

    /// 上移一块。
    pub(crate) fn on_block_up(
        &mut self,
        _: &BlockUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.emit(BlockEvent::RequestBlockUp);
    }

    /// 下移一块。
    pub(crate) fn on_block_down(
        &mut self,
        _: &BlockDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.emit(BlockEvent::RequestBlockDown);
    }

    /// 全选块内文本。
    fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.plain_len(), cx);
    }

    /// 处理全选。
    pub(crate) fn on_select_all(
        &mut self,
        _: &SelectAll,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.show_source_line_numbers() {
            self.select_all_text(cx);
        } else {
            cx.emit(BlockEvent::RequestRenderedSelectAll);
        }
    }

    /// 选中到行首。
    pub(crate) fn on_select_home(
        &mut self,
        _: &SelectHome,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(0, cx);
    }

    /// 选中到行尾。
    pub(crate) fn on_select_end(
        &mut self,
        _: &SelectEnd,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.plain_len(), cx);
    }

    /// 处理复制。
    pub(crate) fn on_copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.rendered_text()[self.selected_range.clone()].to_string(),
            ));
        }
    }

    /// 处理剪切。
    pub(crate) fn on_cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.rendered_text()[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    /// 处理粘贴。
    pub(crate) fn on_paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind().is_separator() && !self.uses_raw_text_editing() {
            return;
        }

        if let Some(item) = cx.read_from_clipboard() {
            if let Some(source) = Self::pasted_image_source_from_clipboard(&item) {
                let (leading, trailing) = self.paste_image_split();
                cx.emit(BlockEvent::RequestPasteImage {
                    leading,
                    source,
                    trailing,
                });
                return;
            }

            let Some(text) = item.text() else {
                return;
            };
            if let Some(source) = Self::pasted_image_source_from_text(&text) {
                let (leading, trailing) = self.paste_image_split();
                cx.emit(BlockEvent::RequestPasteImage {
                    leading,
                    source,
                    trailing,
                });
                return;
            }

            // 仅渲染的富文本块做粘贴修正。纯文本/代码上下文保留字节，表格单元格拍平换行以免误拆表结构。
            if self.editor_selection_range.is_some() {
                cx.emit(BlockEvent::RequestReplaceCrossBlockSelection {
                    text,
                    selected_range_relative: None,
                    mark_inserted_text: false,
                    undo_kind: UndoKind::NonCoalescible,
                });
                return;
            }

            if self.is_table_cell() {
                let flattened = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
                self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
                self.replace_text_in_range(None, &flattened, window, cx);
                return;
            }

            if self.uses_raw_text_editing() {
                self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
                self.replace_text_in_range(None, &text, window, cx);
                return;
            }

            if text.contains('\n') || text.contains('\r') {
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                if self.quote_depth > 0 {
                    self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
                    self.replace_text_in_range(None, &normalized, window, cx);
                    return;
                }
                let clean_selected = self.selection_clean_range();
                let (leading, tail) = self.record.title.split_at(clean_selected.start);
                let (_, trailing) =
                    tail.split_at(clean_selected.end.saturating_sub(clean_selected.start));
                let lines = normalized
                    .split('\n')
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                let split_physical_lines = should_split_plain_multiline_paste(&lines);
                cx.emit(BlockEvent::RequestPasteMultiline {
                    leading,
                    lines,
                    trailing,
                    split_physical_lines,
                });
                return;
            }

            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    /// 处理代码语言框回车。
    pub(crate) fn on_code_language_newline(
        &mut self,
        _: &Newline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// 关闭代码语言框。
    pub(crate) fn on_code_language_dismiss(
        &mut self,
        _: &DismissTransientUi,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// 删除代码语言框字符（退格）。
    pub(crate) fn on_code_language_delete_back(
        &mut self,
        _: &DeleteBack,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        if self.code_language_selected_range.is_empty() {
            let previous = self.previous_code_language_boundary(self.code_language_cursor_offset());
            self.select_code_language_to(previous, cx);
        }
        self.replace_code_language_text_in_range(
            self.code_language_selected_range.clone(),
            "",
            None,
            false,
            cx,
        );
    }

    /// 删除代码语言框字符（删除键）。
    pub(crate) fn on_code_language_delete(
        &mut self,
        _: &Delete,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        if self.code_language_selected_range.is_empty() {
            let next = self.next_code_language_boundary(self.code_language_cursor_offset());
            self.select_code_language_to(next, cx);
        }
        self.replace_code_language_text_in_range(
            self.code_language_selected_range.clone(),
            "",
            None,
            false,
            cx,
        );
    }

    /// 左移代码语言框光标。
    pub(crate) fn on_code_language_move_left(
        &mut self,
        _: &MoveLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        if self.code_language_selected_range.is_empty() {
            self.move_code_language_to(
                self.previous_code_language_boundary(self.code_language_cursor_offset()),
                cx,
            );
        } else {
            self.move_code_language_to(self.code_language_selected_range.start, cx);
        }
    }

    /// 右移代码语言框光标。
    pub(crate) fn on_code_language_move_right(
        &mut self,
        _: &MoveRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        if self.code_language_selected_range.is_empty() {
            self.move_code_language_to(
                self.next_code_language_boundary(self.code_language_cursor_offset()),
                cx,
            );
        } else {
            self.move_code_language_to(self.code_language_selected_range.end, cx);
        }
    }

    /// 移到代码语言框行首。
    pub(crate) fn on_code_language_home(
        &mut self,
        _: &Home,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        self.move_code_language_to(0, cx);
    }

    /// 移到代码语言框行尾。
    pub(crate) fn on_code_language_end(
        &mut self,
        _: &End,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        self.move_code_language_to(self.code_language_text().len(), cx);
    }

    /// 在代码语言框向左扩展选区。
    pub(crate) fn on_code_language_select_left(
        &mut self,
        _: &SelectLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        self.select_code_language_to(
            self.previous_code_language_boundary(self.code_language_cursor_offset()),
            cx,
        );
    }

    /// 在代码语言框向右扩展选区。
    pub(crate) fn on_code_language_select_right(
        &mut self,
        _: &SelectRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        self.select_code_language_to(
            self.next_code_language_boundary(self.code_language_cursor_offset()),
            cx,
        );
    }

    /// 全选代码语言框文本。
    pub(crate) fn on_code_language_select_all(
        &mut self,
        _: &SelectAll,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        self.move_code_language_to(0, cx);
        self.select_code_language_to(self.code_language_text().len(), cx);
    }

    /// 复制代码语言框文本。
    pub(crate) fn on_code_language_copy(
        &mut self,
        _: &Copy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        if !self.code_language_selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.code_language_text()[self.code_language_selected_range.clone()].to_string(),
            ));
        }
    }

    /// 剪切代码语言框文本。
    pub(crate) fn on_code_language_cut(
        &mut self,
        _: &Cut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        if !self.code_language_selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.code_language_text()[self.code_language_selected_range.clone()].to_string(),
            ));
            self.replace_code_language_text_in_range(
                self.code_language_selected_range.clone(),
                "",
                None,
                false,
                cx,
            );
        }
    }

    /// 粘贴代码语言框文本。
    pub(crate) fn on_code_language_paste(
        &mut self,
        _: &Paste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_code_language_text_in_range(
                self.code_language_selected_range.clone(),
                &text,
                None,
                false,
                cx,
            );
        }
    }

    /// 把焦点从语言框移回正文。
    pub(crate) fn on_code_language_focus_content(
        &mut self,
        _: &FocusPrev,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// 把焦点从语言框移到下一块。
    pub(crate) fn on_code_language_focus_next(
        &mut self,
        _: &FocusNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_language_focus_handle.is_focused(window) {
            return;
        }
        cx.stop_propagation();
        // 从语言栏按 Down 离开代码块：编辑器聚焦下一块，末尾代码块先补尾段落；回车不退出（见 on_code_language_newline）。
        cx.emit(BlockEvent::RequestFocusNext { preferred_x: None });
    }

    /// 吞掉语言框内的缩进键。
    pub(crate) fn on_code_language_indent(
        &mut self,
        _: &IndentBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code_language_focus_handle.is_focused(window) {
            cx.stop_propagation();
        }
    }

    /// 吞掉语言框内的取消缩进键。
    pub(crate) fn on_code_language_outdent(
        &mut self,
        _: &OutdentBlock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code_language_focus_handle.is_focused(window) {
            cx.stop_propagation();
        }
    }

    /// 处理语言框鼠标按下。
    pub(crate) fn on_code_language_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.code_language_is_selecting = true;
        self.code_language_focus_handle.focus(window, cx);
        let offset = self.code_language_index_for_mouse_position(event.position);
        if event.modifiers.shift {
            self.select_code_language_to(offset, cx);
        } else {
            self.move_code_language_to(offset, cx);
        }
    }

    /// 处理语言框鼠标松开。
    pub(crate) fn on_code_language_mouse_up(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.code_language_is_selecting = false;
    }

    /// 处理语言框外部鼠标松开。
    pub(crate) fn on_code_language_mouse_up_out(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 界面在捕获阶段派发 mouse_up_out；此处不截断传播，否则指针下控件无法合成 on_click。
        if self.code_language_is_selecting {
            self.code_language_is_selecting = false;
            cx.notify();
        }
    }

    /// 处理语言框鼠标移动。
    pub(crate) fn on_code_language_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code_language_is_selecting {
            // 漏掉鼠标松开会残留选中标记，仅平台仍报告拖拽时才扩展选区。
            if !event.dragging() {
                self.code_language_is_selecting = false;
                cx.notify();
                return;
            }
            cx.stop_propagation();
            self.select_code_language_to(
                self.code_language_index_for_mouse_position(event.position),
                cx,
            );
        }
    }

    /// 处理块鼠标按下。
    pub(crate) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 行虚拟化与布局切换期间，上一帧的命中盒可能把按下事件投递到本块之外的位置；
        // 若按下点明显落在本块最近一次绘制出的外壳之外，直接忽略，
        // 避免误聚焦抢走相邻块（例如正文 HTML 块源码行）的光标。
        if let Some(shell) = self.last_shell_bounds {
            let tolerance = rgpui::px(2.0);
            let within = event.position.x >= shell.left() - tolerance
                && event.position.x <= shell.right() + tolerance
                && event.position.y >= shell.top() - tolerance
                && event.position.y <= shell.bottom() + tolerance;
            if !within {
                cx.stop_propagation();
                return;
            }
        }
        if self.showing_rendered_image() {
            self.is_selecting = false;
            self.request_image_edit_expansion();
            if self.focus_handle.is_focused(window) {
                if self.sync_image_focus_state(true) {
                    cx.notify();
                }
            } else {
                cx.emit(BlockEvent::RequestFocus);
            }
            cx.stop_propagation();
            return;
        }

        let offset = self.index_at_mouse(event.position);
        let was_focused = self.focus_handle.is_focused(window);

        // Cmd/Ctrl+点击跟随渲染链接而非编辑，块既不聚焦也不选中；链接在鼠标松开时打开。
        if event.modifiers.secondary() && self.pointer_link_hit(event.position).is_some() {
            self.is_selecting = false;
            cx.stop_propagation();
            return;
        }

        if was_focused {
            self.is_selecting = true;
            if event.modifiers.shift {
                self.select_extend_mode = SelectExtendMode::Char;
                self.select_to(offset, cx);
            } else if event.click_count == 2 {
                // 双击选词；拖拽延续按词扩展。
                self.select_extend_mode = SelectExtendMode::Word;
                self.select_word_at_offset(offset, cx);
            } else if event.click_count >= 3 {
                // 三击选整行；拖拽延续按行扩展。
                self.select_extend_mode = SelectExtendMode::Line;
                self.select_line_at_offset(offset, cx);
            } else {
                self.select_extend_mode = SelectExtendMode::Char;
                self.move_to(offset, cx);
            }
        } else {
            self.is_selecting = false;
            self.select_extend_mode = SelectExtendMode::Char;
            self.move_to(offset, cx);
            cx.emit(BlockEvent::RequestFocus);
            // 未聚焦时的多击：先聚焦，再补上同样的词/行选择，
            // 保证双击永远选词、三击永远选行。
            if !event.modifiers.shift && event.click_count == 2 {
                self.select_extend_mode = SelectExtendMode::Word;
                self.select_word_at_offset(offset, cx);
            } else if !event.modifiers.shift && event.click_count >= 3 {
                self.select_extend_mode = SelectExtendMode::Line;
                self.select_line_at_offset(offset, cx);
            }
        }
    }

    /// 依据最近渲染布局解析指针下的行内链接，若无布局、为源码态或指针不在链接上则返回 `None`。
    pub(crate) fn pointer_link_hit(&self, position: Point<Pixels>) -> Option<super::InlineLinkHit> {
        self.last_layout
            .as_ref()
            .zip(self.last_bounds)
            .and_then(|(lines, bounds)| {
                super::element::link_at_position(
                    self,
                    lines,
                    bounds,
                    self.last_line_height,
                    position,
                )
            })
            .cloned()
    }

    /// 处理渲染行内链接的鼠标按下（混合行内视觉块）。此处认领 Cmd/Ctrl+点击以跟随链接而非聚焦块，目标在匹配的鼠标松开时打开。
    pub(crate) fn on_rendered_link_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 仅 Cmd/Ctrl+点击跟随链接；普通点击穿透以便块聚焦编辑，与其他行内文本一致。
        if event.modifiers.secondary() {
            cx.stop_propagation();
        }
    }

    /// 经编辑器提示打开渲染行内链接的目标。
    pub(crate) fn open_rendered_link(
        &mut self,
        link: &super::InlineLinkHit,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        cx.emit(BlockEvent::RequestOpenLink {
            prompt_target: link.prompt_target.clone(),
            open_target: link.open_target.clone(),
        });
    }

    /// 处理块鼠标松开。
    pub(crate) fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = false;
        self.select_extend_mode = SelectExtendMode::Char;

        // Cmd/Ctrl+点击跟随渲染链接，使用与下方双击手势相同的打开链接提示。
        if event.modifiers.secondary()
            && let Some(link) = self.pointer_link_hit(event.position)
        {
            self.open_rendered_link(&link, cx);
            return;
        }

        if event.click_count >= 2 {
            let footnote = self
                .last_layout
                .as_ref()
                .zip(self.last_bounds)
                .and_then(|(lines, bounds)| {
                    super::element::footnote_at_position(
                        self,
                        lines,
                        bounds,
                        self.last_line_height,
                        event.position,
                    )
                })
                .cloned();
            if let Some(footnote) = footnote {
                cx.stop_propagation();
                cx.emit(BlockEvent::RequestJumpToFootnoteDefinition { id: footnote.id });
                return;
            }

            if let Some(link) = self.pointer_link_hit(event.position) {
                self.open_rendered_link(&link, cx);
            }
        }
    }

    /// 处理脚注回链鼠标按下。
    pub(crate) fn on_footnote_backref_mouse_down(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if !self.focus_handle.is_focused(window) {
            cx.emit(BlockEvent::RequestFocus);
        }
    }

    /// 处理脚注回链鼠标松开。
    pub(crate) fn on_footnote_backref_mouse_up(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.footnote_definition_id() else {
            return;
        };
        cx.stop_propagation();
        cx.emit(BlockEvent::RequestJumpToFootnoteBackref { id });
    }

    /// 处理块鼠标移动。
    pub(crate) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            // 漏掉鼠标松开会残留选中标记，仅平台仍报告拖拽时才扩展选区。
            if !event.dragging() {
                self.is_selecting = false;
                self.select_extend_mode = SelectExtendMode::Char;
                cx.notify();
                return;
            }
            let offset = self.index_at_mouse(event.position);
            // 双击/三击后的拖拽按词/行吸附，单击拖拽按字符。
            match self.select_extend_mode {
                SelectExtendMode::Word => self.extend_word_selection_to(offset, cx),
                SelectExtendMode::Line => self.extend_line_selection_to(offset, cx),
                SelectExtendMode::Char => self.select_to(offset, cx),
            }
        }
    }

    /// 处理任务复选框鼠标按下。
    pub(crate) fn on_task_checkbox_mouse_down(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if !self.focus_handle.is_focused(window) {
            cx.emit(BlockEvent::RequestFocus);
        }
    }

    /// 处理任务复选框鼠标松开。
    pub(crate) fn on_task_checkbox_mouse_up(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.kind().is_task_list_item() || self.is_source_raw_mode() {
            return;
        }

        cx.stop_propagation();
        cx.emit(BlockEvent::ToggleTaskChecked);
    }

    /// 处理追加列热区悬停。
    pub(crate) fn on_table_append_column_zone_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_table_append_column_hover_part(None, Some(*hovered), None, cx);
    }

    /// 处理追加列按钮悬停。
    pub(crate) fn on_table_append_column_button_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_table_append_column_hover_part(None, None, Some(*hovered), cx);
    }

    /// 处理追加行热区悬停。
    pub(crate) fn on_table_append_row_zone_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_table_append_row_hover_part(None, Some(*hovered), None, cx);
    }

    /// 处理追加行按钮悬停。
    pub(crate) fn on_table_append_row_button_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_table_append_row_hover_part(None, None, Some(*hovered), cx);
    }

    /// 处理追加列边缘悬停。
    pub(crate) fn on_table_append_column_edge_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_table_append_column_hover_part(Some(*hovered), None, None, cx);
    }

    /// 处理追加行边缘悬停。
    pub(crate) fn on_table_append_row_edge_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_table_append_row_hover_part(Some(*hovered), None, None, cx);
    }

    /// 处理追加表格列点击。
    pub(crate) fn on_append_table_column(
        &mut self,
        _: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.kind() == BlockKind::Table {
            cx.emit(BlockEvent::RequestAppendTableColumn);
        }
    }

    /// 处理追加表格行点击。
    pub(crate) fn on_append_table_row(
        &mut self,
        _: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.kind() == BlockKind::Table {
            cx.emit(BlockEvent::RequestAppendTableRow);
        }
    }

    /// 处理加粗选区。
    pub(crate) fn on_bold_selection(
        &mut self,
        _: &BoldSelection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_span_format(InlineFormat::Bold, cx);
    }

    /// 处理斜体选区。
    pub(crate) fn on_italic_selection(
        &mut self,
        _: &ItalicSelection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_span_format(InlineFormat::Italic, cx);
    }

    /// 处理下划线选区。
    pub(crate) fn on_underline_selection(
        &mut self,
        _: &UnderlineSelection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_span_format(InlineFormat::Underline, cx);
    }

    /// 处理行内代码选区。
    pub(crate) fn on_code_selection(
        &mut self,
        _: &CodeSelection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_span_format(InlineFormat::Code, cx);
    }

    /// 处理退出代码块。
    pub(crate) fn on_exit_code_block(
        &mut self,
        _: &ExitCodeBlock,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let exits_multiline_block = self.is_table_cell() || self.kind().is_multiline_text_block();

        if exits_multiline_block {
            cx.emit(BlockEvent::RequestNewline {
                trailing: InlineTextTree::plain(String::new()),
                source_already_mutated: false,
            });
        } else if self.callout_depth > 0 {
            cx.emit(BlockEvent::RequestCalloutBreak);
        } else if self.quote_depth > 0 {
            cx.emit(BlockEvent::RequestQuoteBreak);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Block;
    use crate::components::{BlockKind, BlockRecord, InlineTextTree, PastedImageSource};
    use rgpui::{AppContext, TestAppContext};
    use std::fs;

    /// 生成粘贴图片测试用的临时图片路径（测试辅助）。
    fn temp_image_path(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("rumark-paste-image-path-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("temp image dir should exist");
        let path = root.join(name);
        fs::write(
            &path,
            b"not a real image; extension is enough for paste routing",
        )
        .expect("temp image should be written");
        path
    }

    /// 删除临时图片及其父目录（测试辅助）。
    fn remove_temp_image(path: &std::path::Path) {
        let _ = path.parent().map(fs::remove_dir_all);
    }

    #[rgpui::test]
    async fn append_column_button_stays_visible_while_crossing_hover_gap(cx: &mut TestAppContext) {
        let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph(String::new())));

        block.update(cx, |block, cx| {
            block.set_table_append_column_hover_part(Some(true), None, None, cx);
            assert!(block.table_append_column_hovered);

            block.set_table_append_column_hover_part(Some(false), None, Some(true), cx);
            assert!(block.table_append_column_hovered);
            assert!(!block.table_append_column_edge_hovered);
            assert!(!block.table_append_column_zone_hovered);
            assert!(block.table_append_column_button_hovered);
            assert!(block.table_append_column_close_task.is_none());
        });
    }

    #[test]
    fn paste_image_text_accepts_plain_local_image_path() {
        let path = temp_image_path("copied.png");
        let text = path.to_string_lossy().to_string();
        #[cfg(target_os = "windows")]
        assert!(
            text.contains(':'),
            "test should exercise Windows drive-letter paths"
        );

        let source = Block::pasted_image_source_from_text(&text);

        assert_eq!(source, Some(PastedImageSource::LocalPath(path.clone())));
        remove_temp_image(&path);
    }

    #[test]
    fn paste_image_text_accepts_quoted_local_image_path() {
        let path = temp_image_path("quoted image.png");
        let text = format!("\"{}\"", path.display());

        let source = Block::pasted_image_source_from_text(&text);

        assert_eq!(source, Some(PastedImageSource::LocalPath(path.clone())));
        remove_temp_image(&path);
    }

    #[test]
    fn paste_image_text_accepts_file_url() {
        let path = temp_image_path("url image.png");
        let url = url::Url::from_file_path(&path).expect("temp image path should form file URL");

        let source = Block::pasted_image_source_from_text(url.as_str());

        assert_eq!(source, Some(PastedImageSource::LocalPath(path.clone())));
        remove_temp_image(&path);
    }

    #[test]
    fn paste_image_text_rejects_non_image_path() {
        let path = temp_image_path("notes.txt");
        let text = path.to_string_lossy().to_string();

        let source = Block::pasted_image_source_from_text(&text);

        assert_eq!(source, None);
        remove_temp_image(&path);
    }

    #[rgpui::test]
    async fn append_row_button_stays_visible_while_crossing_hover_gap(cx: &mut TestAppContext) {
        let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph(String::new())));

        block.update(cx, |block, cx| {
            block.set_table_append_row_hover_part(Some(true), None, None, cx);
            assert!(block.table_append_row_hovered);

            block.set_table_append_row_hover_part(Some(false), None, Some(true), cx);
            assert!(block.table_append_row_hovered);
            assert!(!block.table_append_row_edge_hovered);
            assert!(!block.table_append_row_zone_hovered);
            assert!(block.table_append_row_button_hovered);
            assert!(block.table_append_row_close_task.is_none());
        });
    }

    #[rgpui::test]
    async fn column_edge_hover_reveals_only_column_append_control(cx: &mut TestAppContext) {
        let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph(String::new())));

        block.update(cx, |block, cx| {
            block.set_table_append_column_hover_part(Some(true), None, None, cx);
            assert!(block.table_append_column_edge_hovered);
            assert!(block.table_append_column_hovered);
            assert!(!block.table_append_row_hovered);
            assert!(block.table_append_column_close_task.is_none());
            assert!(block.table_append_row_close_task.is_none());
        });
    }

    #[rgpui::test]
    async fn row_edge_hover_reveals_only_row_append_control(cx: &mut TestAppContext) {
        let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph(String::new())));

        block.update(cx, |block, cx| {
            block.set_table_append_row_hover_part(Some(true), None, None, cx);
            assert!(block.table_append_row_edge_hovered);
            assert!(block.table_append_row_hovered);
            assert!(!block.table_append_column_hovered);
            assert!(block.table_append_column_close_task.is_none());
            assert!(block.table_append_row_close_task.is_none());
        });
    }

    #[rgpui::test]
    async fn multiline_quote_is_not_treated_as_leaf(cx: &mut TestAppContext) {
        let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph(String::new())));

        block.update(cx, |block, cx| {
            block.record.kind = BlockKind::Quote;
            block.record.set_title(InlineTextTree::plain("first\n"));
            block.sync_edit_mode_from_kind();
            block.refresh_render_cache();
            cx.notify();

            assert!(!block.is_leaf_quote());
        });
    }
}
