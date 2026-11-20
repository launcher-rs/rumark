//! 渲染模式右键菜单与原生表格插入对话框.

use std::ops::Range;

use rgpui::input_ui::{Input, InputEvent, InputState};
use rgpui::menu::{PopupMenu, PopupMenuItem};
use rgpui::prelude::FluentBuilder as _;
use rgpui::*;

use super::Editor;
use crate::components::{
    Block, BlockKind, BlockRecord, ConvertHeading1, ConvertHeading2, ConvertHeading3,
    ConvertHeading4, ConvertHeading5, ConvertHeading6, ConvertParagraph, Copy as CopyAction,
    Cut as CutAction, DismissTransientUi, InlineFormat, InsertBulletedList, InsertCodeBlock,
    InsertImage, InsertLink, InsertMathBlock, InsertNumberedList, InsertParagraphAbove,
    InsertParagraphBelow, InsertQuote, InsertSeparator, InsertTable, InsertTaskList,
    Paste as PasteAction, SelectAll as SelectAllAction, TableAxisKind, TableAxisMenuOp,
    TableColumnAlignment, TableModel,
};
use crate::i18n::{I18nManager, I18nSnapshot};
use crate::theme::{ThemeDimensions, ThemeTypography};

/// 原生表格的插入目标块位置。
#[derive(Clone, Copy)]
pub(super) enum TableInsertTarget {
    /// 紧跟在引用块之后插入表格。
    After(EntityId),
    /// 紧贴在引用块之前插入（段落“上方”用）。
    Before(EntityId),
    /// 把表格追加到当前根列表末尾。
    Append,
}

/// 编辑器中当前打开的渲染模式右键菜单。
pub(super) enum ContextMenuState {
    /// 通用块右键菜单的插入落点（面板由框架弹出菜单渲染）。
    Insert { target: TableInsertTarget },
}

/// 从右键菜单打开的表格插入对话框状态。
pub(super) struct TableInsertDialogState {
    pub target: TableInsertTarget,
    pub body_rows: usize,
    pub columns: usize,
}

/// 从右键菜单打开的链接/图片插入对话框状态。
///
/// 文本与 URL 用 rgpui 原生输入框编辑（自带剪切/复制/粘贴右键菜单），
/// 回车直接确认，与点击确认按钮等价。打开时若聚焦块有选中文本，
/// 则进入包裹模式：确认后把选中区包成链接而非插入新块。
pub(super) struct LinkDialogState {
    pub target: TableInsertTarget,
    pub is_image: bool,
    pub label: Entity<InputState>,
    pub url: Entity<InputState>,
    pub wrap: Option<WrapLinkTarget>,
}

/// 链接包裹模式的作用目标：块与其选中时的可见文本区间。
#[derive(Clone)]
pub(super) struct WrapLinkTarget {
    pub block: Entity<Block>,
    pub range: Range<usize>,
}

/// 右键插入菜单点击时读取的 owned 快照（空域取追加语义）。
#[derive(Clone)]
struct InsertMenuData {
    target: TableInsertTarget,
    active_id: Option<EntityId>,
    can_insert_table: bool,
    can_format: bool,
    has_selection: bool,
    has_focused_block: bool,
    /// 聚焦块的语义类型（段落子菜单的 ✓ 状态与可用性据此派生）。
    focused_kind: Option<BlockKind>,
    /// 聚焦块是否为可转换段落/标题的叶子文本块。
    can_convert_paragraph: bool,
}

/// 判断块类型是否支持段落/标题互转（叶子文本块；容器与结构块排除，避免丢内容）。
fn can_convert_paragraph_kind(kind: &BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Paragraph
            | BlockKind::Heading { .. }
            | BlockKind::BulletedListItem
            | BlockKind::NumberedListItem
            | BlockKind::TaskListItem { .. }
    )
}

/// 右键菜单条目统一回调形状（与既有 `on_*` 处理器一致）。
type InsertMenuOp = fn(&mut Editor, &ClickEvent, &mut Window, &mut Context<Editor>);

/// 外壳边界命判，带少量容差以吸收边框/亚像素舍入。
fn shell_contains(bounds: Bounds<Pixels>, point: Point<Pixels>) -> bool {
    let tolerance = px(2.0);
    point.x >= bounds.left() - tolerance
        && point.x <= bounds.right() + tolerance
        && point.y >= bounds.top() - tolerance
        && point.y <= bounds.bottom() + tolerance
}

impl Editor {
    /// 向上查找到块所属的根块标识。
    pub(super) fn root_ancestor_entity_id(&self, entity_id: EntityId) -> EntityId {
        let mut current = entity_id;
        while let Some(location) = self.document.find_block_location(current) {
            let Some(parent) = location.parent else {
                break;
            };
            current = parent.entity_id();
        }
        current
    }

    /// 关闭表格插入对话框。
    pub(super) fn close_table_insert_dialog(&mut self, cx: &mut Context<Self>) {
        if self.table_insert_dialog.take().is_some() {
            cx.notify();
        }
    }

    /// 关闭当前右键菜单。
    pub(super) fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    /// 关闭所有右键菜单与插入对话框浮层。
    pub(super) fn dismiss_contextual_overlays(&mut self, cx: &mut Context<Self>) {
        let had_menu = self.context_menu.take().is_some();
        let had_dialog = self.table_insert_dialog.take().is_some();
        let had_link_dialog = self.link_dialog.take().is_some();
        let had_find_bar = self.find_bar.take().is_some();
        if had_link_dialog || had_find_bar {
            // 链接弹窗与查找栏曾抢走焦点，关闭后把焦点还给原聚焦块，避免键盘失焦。
            self.pending_focus = self.active_entity_id.or(self.pending_focus);
        }
        if had_menu || had_dialog || had_link_dialog || had_find_bar {
            cx.notify();
        }
    }

    /// 点击浮层空白处关闭右键菜单。
    pub(super) fn on_dismiss_context_menu_overlay(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_contextual_overlays(cx);
    }

    /// 收到全局关闭浮层动作时关闭右键菜单。
    pub(super) fn on_dismiss_transient_ui(
        &mut self,
        _: &DismissTransientUi,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_contextual_overlays(cx);
    }

    /// 点击「表格」项：关闭菜单并打开表格插入对话框。
    pub(super) fn on_open_table_insert_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ContextMenuState::Insert { target, .. }) = self.context_menu.take() else {
            return;
        };
        self.table_insert_dialog = Some(TableInsertDialogState {
            target,
            body_rows: 2,
            columns: 2,
        });
        cx.notify();
    }

    /// 在右键菜单的目标位置后插入一个新的 Markdown 块。
    ///
    /// 复用表格插入的落点逻辑：目标块后插入（找不到则追到末尾），
    /// 结构块后补空段落保证光标可落，新块自动聚焦并标脏。
    fn insert_block_after_target(
        &mut self,
        target: TableInsertTarget,
        record: BlockRecord,
        cx: &mut Context<Self>,
    ) {
        let new_block = Self::new_block(cx, record);
        match target {
            TableInsertTarget::After(entity_id) => {
                if let Some(location) = self.document.find_block_location(entity_id) {
                    self.document.insert_blocks_at(
                        location.parent,
                        location.index + 1,
                        vec![new_block.clone()],
                        cx,
                    );
                } else {
                    self.document.insert_blocks_at(
                        None,
                        self.document.root_count(),
                        vec![new_block.clone()],
                        cx,
                    );
                }
            }
            TableInsertTarget::Before(entity_id) => {
                if let Some(location) = self.document.find_block_location(entity_id) {
                    self.document.insert_blocks_at(
                        location.parent,
                        location.index,
                        vec![new_block.clone()],
                        cx,
                    );
                } else {
                    self.document
                        .insert_blocks_at(None, 0, vec![new_block.clone()], cx);
                }
            }
            TableInsertTarget::Append => {
                self.document.insert_blocks_at(
                    None,
                    self.document.root_count(),
                    vec![new_block.clone()],
                    cx,
                );
            }
        }

        self.ensure_trailing_paragraph_after_structural(&new_block, cx);
        self.focus_block(new_block.entity_id());
        self.mark_dirty(cx);
        cx.notify();
    }

    /// 从打开的插入菜单中取出插入目标，同时关闭菜单。
    fn take_insert_target(&mut self) -> Option<TableInsertTarget> {
        let Some(ContextMenuState::Insert { target, .. }) = self.context_menu.take() else {
            return None;
        };
        Some(target)
    }

    /// 为指定块（空域传 `None`）计算右键插入菜单快照；表格单元格走表格轴菜单，返回空。
    fn describe_insert_menu(&self, block_id: Option<EntityId>, cx: &App) -> Option<InsertMenuData> {
        if let Some(id) = block_id
            && self.table_cell_binding(id).is_some()
        {
            return None;
        }
        let focus_id = block_id.or(self.active_entity_id);
        let focus_block = focus_id.and_then(|id| self.focusable_entity_by_id(id));
        let target = match focus_id {
            Some(id) => TableInsertTarget::After(self.root_ancestor_entity_id(id)),
            None => TableInsertTarget::Append,
        };
        let can_insert_table = match target {
            TableInsertTarget::After(id) | TableInsertTarget::Before(id) => self
                .focusable_entity_by_id(id)
                .is_some_and(|block| block.read(cx).kind().allows_table_insert_from_context()),
            TableInsertTarget::Append => true,
        };
        let has_focused_block = focus_block.is_some();
        let (can_format, has_selection) = focus_block
            .as_ref()
            .map(|block| {
                let block = block.read(cx);
                (
                    !block.kind().is_separator(),
                    !block.selected_range.is_empty(),
                )
            })
            .unwrap_or((false, false));
        let focused_kind = focus_block.map(|block| block.read(cx).kind());
        let can_convert_paragraph = focused_kind
            .as_ref()
            .is_some_and(can_convert_paragraph_kind);
        Some(InsertMenuData {
            target,
            active_id: focus_id,
            can_insert_table,
            can_format,
            has_selection,
            has_focused_block,
            focused_kind,
            can_convert_paragraph,
        })
    }

    /// 判断窗口坐标是否落在某个自带右键菜单的块外壳内（表格单元格除外，
    /// 它们走表格轴菜单）。外壳边界由块渲染时记录，含块内边距但不含块间间距。
    fn context_point_over_block_shell(&self, point: Point<Pixels>, cx: &App) -> bool {
        self.document.visible_blocks().iter().any(|visible| {
            let id = visible.entity.entity_id();
            if self.table_cell_binding(id).is_some() {
                return false;
            }
            visible
                .entity
                .read(cx)
                .last_shell_bounds
                .is_some_and(|bounds| shell_contains(bounds, point))
        })
    }

    /// 返回指定块最近一次绘制出的外壳边界。
    fn block_shell_bounds(&self, block_id: EntityId, cx: &App) -> Option<Bounds<Pixels>> {
        self.document.visible_blocks().iter().find_map(|visible| {
            (visible.entity.entity_id() == block_id)
                .then(|| visible.entity.read(cx).last_shell_bounds)
                .flatten()
        })
    }

    /// 为指定块（空域传 `None`）构造框架右键菜单（Typora 式三段：剪贴板、
    /// 行内格式工具条、段落/插入二级菜单）；表格单元格返回空菜单。
    pub(super) fn build_insert_context_popup(
        menu: PopupMenu,
        editor: &Entity<Editor>,
        block_id: Option<EntityId>,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let Some(data) = editor.read(cx).describe_insert_menu(block_id, cx) else {
            return menu;
        };
        // 空白域菜单与块行菜单会同时响应同一次右键（框架不拦截冒泡），且行虚拟化
        // 期间命中盒可能把右键误投到外壳之外的块（与 Block::on_mouse_down 同理），
        // 二者都会造成多层菜单叠加。以块外壳边界为唯一判据：
        // 落在某个块外壳内时，只有该块的行菜单生效，空白菜单与其它块的行菜单让位。
        let point = window.mouse_position();
        match block_id {
            None => {
                if editor.read(cx).context_point_over_block_shell(point, cx) {
                    return menu;
                }
            }
            Some(id) => {
                if let Some(shell) = editor.read(cx).block_shell_bounds(id, cx)
                    && !shell_contains(shell, point)
                {
                    return menu;
                }
            }
        }
        #[cfg(test)]
        super::MENU_BUILD_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let image_data = editor.read(cx).describe_image_menu(block_id, cx);
        let strings = cx.global::<I18nManager>().snapshot();
        // 图片专属条目（定位/复制/缩放/内嵌）置顶，其后接通用块操作。
        let menu = match &image_data {
            Some(image_data) => {
                let menu = Self::build_image_menu_items(menu, editor, image_data, window, cx);
                menu.separator()
            }
            None => menu,
        };
        menu.item(Self::insert_popup_item(
            strings.t("keys_cut", &[]),
            Some(Box::new(CutAction)),
            data.has_selection,
            false,
            editor,
            &data,
            Self::on_clipboard_cut,
        ))
        .item(Self::insert_popup_item(
            strings.t("keys_copy", &[]),
            Some(Box::new(CopyAction)),
            data.has_selection,
            false,
            editor,
            &data,
            Self::on_clipboard_copy,
        ))
        .item(Self::insert_popup_item(
            strings.t("keys_paste", &[]),
            Some(Box::new(PasteAction)),
            data.has_focused_block,
            false,
            editor,
            &data,
            Self::on_clipboard_paste,
        ))
        .item(Self::insert_popup_item(
            strings.t("keys_select_all", &[]),
            Some(Box::new(SelectAllAction)),
            data.has_focused_block,
            false,
            editor,
            &data,
            Self::on_clipboard_select_all,
        ))
        .separator()
        .item(Self::insert_toolbar_element(editor, &data))
        .separator()
        .submenu(strings.t("context_paragraph", &[]), window, cx, {
            let editor = editor.clone();
            let data = data.clone();
            let strings = strings.clone();
            move |sub, _window, _cx| Self::build_paragraph_submenu(sub, &editor, &data, &strings)
        })
        .submenu(strings.t("context_insert", &[]), window, cx, {
            let editor = editor.clone();
            let data = data.clone();
            let strings = strings.clone();
            move |sub, _window, _cx| Self::build_insert_submenu(sub, &editor, &data, &strings)
        })
    }

    /// 构造「段落」子菜单：当前块原地转标题/段落（✓ 标出当前类型）。
    fn build_paragraph_submenu(
        sub: PopupMenu,
        editor: &Entity<Editor>,
        data: &InsertMenuData,
        strings: &I18nSnapshot,
    ) -> PopupMenu {
        const HEADING_OPS: [InsertMenuOp; 6] = [
            Editor::on_convert_heading_1,
            Editor::on_convert_heading_2,
            Editor::on_convert_heading_3,
            Editor::on_convert_heading_4,
            Editor::on_convert_heading_5,
            Editor::on_convert_heading_6,
        ];
        // 与 `HEADING_OPS` 同序，用于展示快捷键提示。
        fn heading_action(level: u8) -> Box<dyn Action> {
            match level {
                1 => Box::new(ConvertHeading1),
                2 => Box::new(ConvertHeading2),
                3 => Box::new(ConvertHeading3),
                4 => Box::new(ConvertHeading4),
                5 => Box::new(ConvertHeading5),
                _ => Box::new(ConvertHeading6),
            }
        }
        let mut sub = sub;
        for (index, op) in HEADING_OPS.into_iter().enumerate() {
            let level = index as u8 + 1;
            let label = strings.t(&format!("context_heading_{level}"), &[]);
            let checked = data.focused_kind == Some(BlockKind::Heading { level });
            sub = sub.item(Self::insert_popup_item(
                label,
                Some(heading_action(level)),
                data.can_convert_paragraph,
                checked,
                editor,
                data,
                op,
            ));
        }
        sub.separator().item(Self::insert_popup_item(
            strings.t("context_paragraph", &[]),
            Some(Box::new(ConvertParagraph)),
            data.can_convert_paragraph,
            data.focused_kind == Some(BlockKind::Paragraph),
            editor,
            data,
            Self::on_convert_paragraph,
        ))
    }

    /// 构造「插入」子菜单（Typora 式分组：媒体/结构/上下段落/列表引用）。
    fn build_insert_submenu(
        sub: PopupMenu,
        editor: &Entity<Editor>,
        data: &InsertMenuData,
        strings: &I18nSnapshot,
    ) -> PopupMenu {
        sub.item(Self::insert_popup_item(
            strings.t("insert_image", &[]),
            Some(Box::new(InsertImage)),
            true,
            false,
            editor,
            data,
            Self::on_insert_image,
        ))
        .item(Self::insert_popup_item(
            strings.t("insert_link", &[]),
            Some(Box::new(InsertLink)),
            true,
            false,
            editor,
            data,
            Self::on_insert_link,
        ))
        .item(Self::insert_popup_item(
            strings.t("insert_separator", &[]),
            Some(Box::new(InsertSeparator)),
            true,
            false,
            editor,
            data,
            Self::on_insert_separator,
        ))
        .item(Self::insert_popup_item(
            strings.t("context_table", &[]),
            Some(Box::new(InsertTable)),
            data.can_insert_table,
            false,
            editor,
            data,
            Self::on_open_table_insert_dialog,
        ))
        .item(Self::insert_popup_item(
            strings.t("insert_code_block", &[]),
            Some(Box::new(InsertCodeBlock)),
            true,
            false,
            editor,
            data,
            Self::on_insert_code_block,
        ))
        .item(Self::insert_popup_item(
            strings.t("insert_math", &[]),
            Some(Box::new(InsertMathBlock)),
            true,
            false,
            editor,
            data,
            Self::on_insert_math_block,
        ))
        .item(Self::insert_popup_item(
            strings.t("insert_mermaid", &[]),
            None,
            true,
            false,
            editor,
            data,
            Self::on_insert_mermaid_block,
        ))
        .separator()
        .item(Self::insert_popup_item(
            strings.t("insert_paragraph_above", &[]),
            Some(Box::new(InsertParagraphAbove)),
            true,
            false,
            editor,
            data,
            Self::on_insert_paragraph_above,
        ))
        .item(Self::insert_popup_item(
            strings.t("insert_paragraph_below", &[]),
            Some(Box::new(InsertParagraphBelow)),
            true,
            false,
            editor,
            data,
            Self::on_insert_paragraph_below,
        ))
        .separator()
        .item(Self::insert_popup_item(
            strings.t("insert_quote", &[]),
            Some(Box::new(InsertQuote)),
            true,
            false,
            editor,
            data,
            Self::on_insert_quote,
        ))
        .item(Self::insert_popup_item(
            strings.t("insert_bulleted_list", &[]),
            Some(Box::new(InsertBulletedList)),
            true,
            false,
            editor,
            data,
            Self::on_insert_bulleted_list,
        ))
        .item(Self::insert_popup_item(
            strings.t("insert_numbered_list", &[]),
            Some(Box::new(InsertNumberedList)),
            true,
            false,
            editor,
            data,
            Self::on_insert_numbered_list,
        ))
        .item(Self::insert_popup_item(
            strings.t("insert_task_list", &[]),
            Some(Box::new(InsertTaskList)),
            true,
            false,
            editor,
            data,
            Self::on_insert_task_list,
        ))
    }

    /// 构造行内格式工具条（两行四按钮；点击冒泡到行即关闭菜单）。
    fn insert_toolbar_element(editor: &Entity<Editor>, data: &InsertMenuData) -> PopupMenuItem {
        let editor = editor.clone();
        let data = data.clone();
        PopupMenuItem::element(move |_window: &mut Window, cx: &mut App| {
            let theme = Theme::global(cx).clone();
            let button = |id: &'static str, glyph: AnyElement, enabled: bool, op: InsertMenuOp| {
                let editor = editor.clone();
                let data = data.clone();
                let mut button = div()
                    .id(id)
                    .flex_1()
                    .h(px(26.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .border(px(1.0))
                    .border_color(theme.border)
                    .text_size(px(13.0))
                    .child(glyph);
                if enabled {
                    button = button
                        .text_color(theme.foreground)
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.button_secondary_hover))
                        .on_click(Self::insert_menu_click_handler(editor, data, op));
                } else {
                    button = button.text_color(theme.muted_foreground).opacity(0.5);
                }
                button.into_any_element()
            };
            let glyph_bold = div()
                .font_weight(FontWeight::BOLD)
                .child("B")
                .into_any_element();
            let glyph_italic = div().italic().child("I").into_any_element();
            let glyph_code = div().text_size(px(11.0)).child("</>").into_any_element();
            let glyph_link = Icon::new(IconName::ExternalLink)
                .xsmall()
                .text_color(theme.foreground)
                .into_any_element();
            let glyph_quote = div()
                .font_weight(FontWeight::BOLD)
                .text_size(px(15.0))
                .child("\u{201c}")
                .into_any_element();
            let glyph_ordered = div().child("1.").into_any_element();
            let glyph_bullet = div().text_size(px(15.0)).child("•").into_any_element();
            let glyph_task = div().child("☑").into_any_element();
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .gap(px(4.0))
                        .child(button(
                            "format-toolbar-bold",
                            glyph_bold,
                            data.can_format,
                            Self::on_format_bold,
                        ))
                        .child(button(
                            "format-toolbar-italic",
                            glyph_italic,
                            data.can_format,
                            Self::on_format_italic,
                        ))
                        .child(button(
                            "format-toolbar-code",
                            glyph_code,
                            data.can_format,
                            Self::on_format_inline_code,
                        ))
                        .child(button(
                            "format-toolbar-link",
                            glyph_link,
                            true,
                            Self::on_insert_link,
                        )),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .gap(px(4.0))
                        .child(button(
                            "format-toolbar-quote",
                            glyph_quote,
                            true,
                            Self::on_insert_quote,
                        ))
                        .child(button(
                            "format-toolbar-ordered",
                            glyph_ordered,
                            true,
                            Self::on_insert_numbered_list,
                        ))
                        .child(button(
                            "format-toolbar-bullet",
                            glyph_bullet,
                            true,
                            Self::on_insert_bulleted_list,
                        ))
                        .child(button(
                            "format-toolbar-task",
                            glyph_task,
                            true,
                            Self::on_insert_task_list,
                        )),
                )
        })
    }

    /// 构造菜单点击闭包：先对齐作用块与插入落点，再调既有处理器（菜单项与工具条按钮共用）。
    fn insert_menu_click_handler(
        editor: Entity<Editor>,
        data: InsertMenuData,
        op: InsertMenuOp,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + Send + Sync + 'static {
        move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
            editor.update(cx, |editor, cx| {
                if let Some(id) = data.active_id {
                    editor.active_entity_id = Some(id);
                }
                editor.context_menu = Some(ContextMenuState::Insert {
                    target: data.target,
                });
                op(editor, event, window, cx);
            });
        }
    }

    /// 构造单个插入菜单项：`action` 仅用于展示快捷键，点计算由统一闭包处理。
    fn insert_popup_item(
        label: String,
        action: Option<Box<dyn Action>>,
        enabled: bool,
        checked: bool,
        editor: &Entity<Editor>,
        data: &InsertMenuData,
        op: InsertMenuOp,
    ) -> PopupMenuItem {
        let mut item = PopupMenuItem::new(label)
            .disabled(!enabled)
            .checked(checked);
        if let Some(action) = action {
            item = item.action(action);
        }
        item.on_click(Self::insert_menu_click_handler(
            editor.clone(),
            data.clone(),
            op,
        ))
    }

    /// 右键菜单「插入」项的统一入口：取出目标并插入指定 kind 的空块。
    fn insert_kind_after_target(
        &mut self,
        kind: BlockKind,
        _event: &ClickEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.take_insert_target() else {
            return;
        };
        let record = BlockRecord::with_plain_text(kind, String::new());
        self.insert_block_after_target(target, record, cx);
    }

    /// 把聚焦块原地转为指定类型（Typora 式转换语义：标题/段落互转保留文本）。
    fn convert_focused_block_kind(&mut self, kind: BlockKind, cx: &mut Context<Self>) {
        let Some(block) = self.focused_format_block() else {
            return;
        };
        self.close_context_menu(cx);
        block.update(cx, |block, cx| match kind {
            BlockKind::Heading { level } => block.convert_to_heading(level, cx),
            _ => block.make_paragraph(cx),
        });
        self.focus_block(block.entity_id());
        self.mark_dirty(cx);
        self.finalize_pending_undo_capture(cx);
        cx.notify();
    }

    /// 点击「段落 → 一级标题」：把聚焦块转为一级标题。
    pub(super) fn on_convert_heading_1(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_focused_block_kind(BlockKind::Heading { level: 1 }, cx);
    }

    /// 点击「段落 → 二级标题」：把聚焦块转为二级标题。
    pub(super) fn on_convert_heading_2(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_focused_block_kind(BlockKind::Heading { level: 2 }, cx);
    }

    /// 点击「段落 → 三级标题」：把聚焦块转为三级标题。
    pub(super) fn on_convert_heading_3(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_focused_block_kind(BlockKind::Heading { level: 3 }, cx);
    }

    /// 点击「段落 → 四级标题」：把聚焦块转为四级标题。
    pub(super) fn on_convert_heading_4(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_focused_block_kind(BlockKind::Heading { level: 4 }, cx);
    }

    /// 点击「段落 → 五级标题」：把聚焦块转为五级标题。
    pub(super) fn on_convert_heading_5(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_focused_block_kind(BlockKind::Heading { level: 5 }, cx);
    }

    /// 点击「段落 → 六级标题」：把聚焦块转为六级标题。
    pub(super) fn on_convert_heading_6(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_focused_block_kind(BlockKind::Heading { level: 6 }, cx);
    }

    /// 点击「段落 → 段落」：把聚焦块转回普通段落。
    pub(super) fn on_convert_paragraph(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_focused_block_kind(BlockKind::Paragraph, cx);
    }

    /// 点击「插入公式块」：在目标后插入空公式块。
    pub(super) fn on_insert_math_block(
        &mut self,
        event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_after_target(BlockKind::MathBlock, event, cx);
    }

    /// 点击「插入 Mermaid」：在目标后插入空 Mermaid 围栏块。
    pub(super) fn on_insert_mermaid_block(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.take_insert_target() else {
            return;
        };
        self.insert_block_after_target(
            target,
            // 与解析路径（document.rs 把 ```mermaid 围栏建为 MermaidBlock）保持同一类型，
            // 新插入的块点击预览、编辑与保存行为才和文件里加载的旧图一致。
            BlockRecord::mermaid("```mermaid\nflowchart TD\n    A --> B\n```"),
            cx,
        );
    }

    /// 在目标附近插入空段落（上方走 `Before` 落点，下方走既有 `After` 落点）。
    fn insert_paragraph_near_target(&mut self, before: bool, cx: &mut Context<Self>) {
        let Some(target) = self.take_insert_target() else {
            return;
        };
        let target = match target {
            TableInsertTarget::After(id) if before => TableInsertTarget::Before(id),
            TableInsertTarget::Before(id) if !before => TableInsertTarget::After(id),
            target => target,
        };
        self.insert_block_after_target(target, BlockRecord::paragraph(String::new()), cx);
    }

    /// 点击「段落（上方）」：在目标块之前插入空段落。
    pub(super) fn on_insert_paragraph_above(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_paragraph_near_target(true, cx);
    }

    /// 点击「段落（下方）」：在目标块之后插入空段落。
    pub(super) fn on_insert_paragraph_below(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_paragraph_near_target(false, cx);
    }

    /// 点击「插入无序列表」：在目标后插入空无序列表项。
    pub(super) fn on_insert_bulleted_list(
        &mut self,
        event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_after_target(BlockKind::BulletedListItem, event, cx);
    }

    /// 点击「插入有序列表」：在目标后插入空有序列表项。
    pub(super) fn on_insert_numbered_list(
        &mut self,
        event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_after_target(BlockKind::NumberedListItem, event, cx);
    }

    /// 点击「插入任务列表」：在目标后插入空未勾选任务项。
    pub(super) fn on_insert_task_list(
        &mut self,
        event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_after_target(BlockKind::TaskListItem { checked: false }, event, cx);
    }

    /// 点击「插入引用」：在目标后插入空引用块。
    pub(super) fn on_insert_quote(
        &mut self,
        event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_after_target(BlockKind::Quote, event, cx);
    }

    /// 点击「插入代码块」：在目标后插入空代码块。
    pub(super) fn on_insert_code_block(
        &mut self,
        event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_after_target(BlockKind::CodeBlock { language: None }, event, cx);
    }

    /// 点击「插入分隔线」：在目标后插入分隔线块。
    pub(super) fn on_insert_separator(
        &mut self,
        event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_after_target(BlockKind::Separator, event, cx);
    }

    /// 组装链接/图片 Markdown：空文本与空 URL 自动回退默认值，裸域名自动补 `https://`。
    fn link_markdown(label: &str, url: &str, is_image: bool) -> String {
        let label = label.trim();
        let url = url.trim();
        let label = if label.is_empty() {
            if is_image { "image" } else { "text" }
        } else {
            label
        };
        let url = if url.is_empty() {
            "https://example.com".to_string()
        } else {
            Self::normalize_link_url(url)
        };
        if is_image {
            format!("![{label}]({url})")
        } else {
            format!("[{label}]({url})")
        }
    }

    /// 规范化链接 URL：已有协议/相对路径/锚点/`data:` 原样返回，裸域名与 `www.` 开头补 `https://`。
    fn normalize_link_url(url: &str) -> String {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return "https://example.com".to_string();
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("://")
            || lower.starts_with("mailto:")
            || lower.starts_with("tel:")
            || lower.starts_with("data:")
            || trimmed.starts_with('#')
            || trimmed.starts_with('/')
            || trimmed.starts_with("./")
            || trimmed.starts_with("../")
        {
            return trimmed.to_string();
        }
        // 裸域名（首段含点且无空格）视为网页地址补协议；其他原样返回。
        let head = trimmed.split(['/', '?', '#']).next().unwrap_or("");
        if !head.is_empty() && !head.contains(' ') && head.contains('.') {
            return format!("https://{trimmed}");
        }
        trimmed.to_string()
    }

    /// 插入链接/图片段落；链接模式下选中标签文本方便直接改写。
    ///
    /// 标签用字节偏移定位（展示文本即标签本身），调用方保证标签非空。
    fn insert_link_block(
        &mut self,
        target: TableInsertTarget,
        label: &str,
        url: &str,
        is_image: bool,
        cx: &mut Context<Self>,
    ) {
        let record = BlockRecord::paragraph(Self::link_markdown(label, url, is_image));
        self.insert_block_after_target(target, record, cx);
        if !is_image
            && let Some(id) = self.active_entity_id
            && let Some(block) = self.focusable_entity_by_id(id)
        {
            block.update(cx, |block, _cx| {
                block.selected_range = 0..label.trim().len();
                block.selection_reversed = false;
                block.marked_range = None;
            });
            cx.notify();
        }
    }

    /// 打开链接/图片对话框（文本 + URL 双输入框，回车确认）。
    ///
    /// `wrap` 为包裹模式：文本框预填选中文本并聚焦 URL 框，确认后原地包裹。
    fn open_link_dialog_at(
        &mut self,
        target: TableInsertTarget,
        is_image: bool,
        wrap: Option<WrapLinkTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 包裹模式用选中文本做标签（截断防超长），插入模式用占位默认值。
        let seed_label = if is_image {
            None
        } else {
            wrap.as_ref().and_then(|wrap| {
                let text = wrap.block.read(cx).rendered_text();
                let selected = text.get(wrap.range.clone())?.trim();
                (!selected.is_empty()).then(|| selected.chars().take(120).collect::<String>())
            })
        };
        let default_url = if is_image {
            "https://example.com/image.png"
        } else {
            "https://example.com"
        };
        let default_label =
            seed_label
                .as_deref()
                .unwrap_or(if is_image { "image" } else { "text" });
        self.close_table_insert_dialog(cx);
        self.close_find_bar(cx);
        let label = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(default_label)
                .submit_on_enter(true)
        });
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(default_url)
                .submit_on_enter(true)
        });
        cx.subscribe(&label, Self::on_link_dialog_input_event)
            .detach();
        cx.subscribe(&url, Self::on_link_dialog_input_event)
            .detach();
        // 包裹模式聚焦 URL 框（标签已预填），插入模式聚焦文本框；
        // 全选默认文本，键盘直接改写，回退键走输入框自身绑定。
        if wrap.is_some() {
            url.update(cx, |state, cx| {
                let len = state.value().len();
                state.set_selected_range(0..len, cx);
            });
            url.update(cx, |state, cx| {
                state.focus(window, cx);
            });
        } else {
            label.update(cx, |state, cx| {
                let len = state.value().len();
                state.set_selected_range(0..len, cx);
            });
            label.update(cx, |state, cx| {
                state.focus(window, cx);
            });
        }
        self.link_dialog = Some(LinkDialogState {
            target,
            is_image,
            label,
            url,
            wrap,
        });
        cx.notify();
    }

    /// 输入框内回车：与点击确认按钮等价。
    fn on_link_dialog_input_event(
        &mut self,
        _input: Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::PressEnter { .. }) {
            self.confirm_link_dialog(cx);
        }
    }

    /// 关闭链接/图片插入对话框。
    fn close_link_dialog(&mut self, cx: &mut Context<Self>) {
        if self.link_dialog.take().is_some() {
            // 取消即回原聚焦块，确认路径由 `insert_link_block` 另行聚焦新块。
            self.pending_focus = self.active_entity_id.or(self.pending_focus);
            cx.notify();
        }
    }

    /// 确认链接对话框：包裹模式原地包链接，否则读取输入并插入对应块。
    fn confirm_link_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.link_dialog.take() else {
            return;
        };
        let label = dialog.label.read(cx).value().to_string();
        let url = dialog.url.read(cx).value().to_string();
        if !dialog.is_image
            && let Some(wrap) = dialog.wrap
        {
            self.wrap_selection_with_link(wrap, &label, &url, cx);
            return;
        }
        self.insert_link_block(dialog.target, &label, &url, dialog.is_image, cx);
    }

    /// 原地包裹：把目标块选中区替换为 `[标签](地址)` 并聚焦该块。
    ///
    /// 标签被清空时回退选中原文，避免吞字；区间夹取到当前可见长度。
    fn wrap_selection_with_link(
        &mut self,
        wrap: WrapLinkTarget,
        label: &str,
        url: &str,
        cx: &mut Context<Self>,
    ) {
        let block = wrap.block.clone();
        let fallback = block
            .read(cx)
            .rendered_text()
            .get(wrap.range.clone())
            .unwrap_or("")
            .to_string();
        let label = if label.trim().is_empty() {
            fallback.as_str()
        } else {
            label
        };
        let link = Self::link_markdown(label, url, false);
        block.update(cx, |block, cx| {
            let len = block.plain_len();
            let start = wrap.range.start.min(len);
            let end = wrap.range.end.min(len).max(start);
            block.replace_text_in_visible_range(start..end, &link, None, false, cx);
        });
        self.focus_block(block.entity_id());
        self.mark_dirty(cx);
        cx.notify();
    }

    /// 聚焦块有非空选中时构造包裹目标（分隔线与图片模式不用）。
    fn wrap_target_for_link(&self, cx: &App) -> Option<WrapLinkTarget> {
        let block = self.focused_format_block()?;
        if block.read(cx).kind().is_separator() {
            return None;
        }
        let range = block.read(cx).selected_range.clone();
        (!range.is_empty()).then(|| WrapLinkTarget {
            block: block.clone(),
            range,
        })
    }

    /// 点击取消：关闭链接/图片对话框。
    pub(super) fn on_cancel_link_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_link_dialog(cx);
    }

    /// 点击确认：提交链接/图片对话框。
    pub(super) fn on_confirm_link_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_link_dialog(cx);
    }

    /// 点击「插入链接」：有选中则包裹选中，否则打开链接输入对话框。
    pub(super) fn on_insert_link(
        &mut self,
        _event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let wrap = self.wrap_target_for_link(cx);
        let Some(target) = self.take_insert_target() else {
            return;
        };
        self.open_link_dialog_at(target, false, wrap, window, cx);
    }

    /// 插入图片：打开图片对话框（文本 + URL 双输入框，可浏览本地文件）。
    pub(super) fn on_insert_image(
        &mut self,
        _event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.take_insert_target() else {
            return;
        };
        self.open_link_dialog_at(target, true, None, window, cx);
    }

    /// 快捷键动作（默认 Ctrl+K）：有选中则包裹选中，否则在当前聚焦块后打开链接对话框。
    pub(super) fn on_insert_link_action(
        &mut self,
        _: &InsertLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.link_dialog.is_some() || self.table_insert_dialog.is_some() {
            return;
        }
        let wrap = self.wrap_target_for_link(cx);
        let target = self.action_insert_target();
        self.open_link_dialog_at(target, false, wrap, window, cx);
    }

    /// 快捷键落点：聚焦块所在根块之后，无聚焦时追加到末尾。
    fn action_insert_target(&self) -> TableInsertTarget {
        match self.active_entity_id {
            Some(entity_id) => TableInsertTarget::After(self.root_ancestor_entity_id(entity_id)),
            None => TableInsertTarget::Append,
        }
    }

    /// 快捷键插入空块：对话框打开时忽略，避免落点错乱。
    fn insert_kind_for_action(&mut self, kind: BlockKind, cx: &mut Context<Self>) {
        if self.link_dialog.is_some() || self.table_insert_dialog.is_some() {
            return;
        }
        let target = self.action_insert_target();
        let record = BlockRecord::with_plain_text(kind, String::new());
        self.insert_block_after_target(target, record, cx);
    }

    /// 快捷键动作：在当前聚焦块后打开图片对话框。
    pub(super) fn on_insert_image_action(
        &mut self,
        _: &InsertImage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.link_dialog.is_some() || self.table_insert_dialog.is_some() {
            return;
        }
        let target = self.action_insert_target();
        self.open_link_dialog_at(target, true, None, window, cx);
    }

    /// 快捷键动作：在当前聚焦块后打开表格插入对话框。
    pub(super) fn on_insert_table_action(
        &mut self,
        _: &InsertTable,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.link_dialog.is_some() || self.table_insert_dialog.is_some() {
            return;
        }
        let target = self.action_insert_target();
        self.table_insert_dialog = Some(TableInsertDialogState {
            target,
            body_rows: 2,
            columns: 2,
        });
        cx.notify();
    }

    /// 快捷键动作：在当前聚焦块后插入空代码块。
    pub(super) fn on_insert_code_block_action(
        &mut self,
        _: &InsertCodeBlock,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_for_action(BlockKind::CodeBlock { language: None }, cx);
    }

    /// 快捷键动作：在当前聚焦块后插入空公式块。
    pub(super) fn on_insert_math_block_action(
        &mut self,
        _: &InsertMathBlock,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_for_action(BlockKind::MathBlock, cx);
    }

    /// 快捷键动作：在当前聚焦块后插入空引用块。
    pub(super) fn on_insert_quote_action(
        &mut self,
        _: &InsertQuote,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_for_action(BlockKind::Quote, cx);
    }

    /// 快捷键动作：在当前聚焦块后插入空无序列表项。
    pub(super) fn on_insert_bulleted_list_action(
        &mut self,
        _: &InsertBulletedList,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_for_action(BlockKind::BulletedListItem, cx);
    }

    /// 快捷键动作：在当前聚焦块后插入空有序列表项。
    pub(super) fn on_insert_numbered_list_action(
        &mut self,
        _: &InsertNumberedList,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_for_action(BlockKind::NumberedListItem, cx);
    }

    /// 快捷键动作：在当前聚焦块后插入空任务列表项。
    pub(super) fn on_insert_task_list_action(
        &mut self,
        _: &InsertTaskList,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_for_action(BlockKind::TaskListItem { checked: false }, cx);
    }

    /// 快捷键动作：在当前聚焦块后插入分隔线。
    pub(super) fn on_insert_separator_action(
        &mut self,
        _: &InsertSeparator,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_kind_for_action(BlockKind::Separator, cx);
    }

    /// 快捷键动作：在当前聚焦块上方插入空段落。
    pub(super) fn on_insert_paragraph_above_action(
        &mut self,
        _: &InsertParagraphAbove,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.link_dialog.is_some() || self.table_insert_dialog.is_some() {
            return;
        }
        let target = match self.action_insert_target() {
            TableInsertTarget::After(id) => TableInsertTarget::Before(id),
            target => target,
        };
        self.insert_block_after_target(target, BlockRecord::paragraph(String::new()), cx);
    }

    /// 快捷键动作：在当前聚焦块下方插入空段落。
    pub(super) fn on_insert_paragraph_below_action(
        &mut self,
        _: &InsertParagraphBelow,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.link_dialog.is_some() || self.table_insert_dialog.is_some() {
            return;
        }
        let target = self.action_insert_target();
        self.insert_block_after_target(target, BlockRecord::paragraph(String::new()), cx);
    }

    /// 快捷键转换段落/标题：对话框打开时忽略，避免与输入框抢键。
    fn convert_kind_for_action(&mut self, kind: BlockKind, cx: &mut Context<Self>) {
        if self.link_dialog.is_some() || self.table_insert_dialog.is_some() {
            return;
        }
        self.convert_focused_block_kind(kind, cx);
    }

    /// 快捷键动作：聚焦块转一级标题。
    pub(super) fn on_convert_heading_1_action(
        &mut self,
        _: &ConvertHeading1,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_kind_for_action(BlockKind::Heading { level: 1 }, cx);
    }

    /// 快捷键动作：聚焦块转二级标题。
    pub(super) fn on_convert_heading_2_action(
        &mut self,
        _: &ConvertHeading2,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_kind_for_action(BlockKind::Heading { level: 2 }, cx);
    }

    /// 快捷键动作：聚焦块转三级标题。
    pub(super) fn on_convert_heading_3_action(
        &mut self,
        _: &ConvertHeading3,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_kind_for_action(BlockKind::Heading { level: 3 }, cx);
    }

    /// 快捷键动作：聚焦块转四级标题。
    pub(super) fn on_convert_heading_4_action(
        &mut self,
        _: &ConvertHeading4,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_kind_for_action(BlockKind::Heading { level: 4 }, cx);
    }

    /// 快捷键动作：聚焦块转五级标题。
    pub(super) fn on_convert_heading_5_action(
        &mut self,
        _: &ConvertHeading5,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_kind_for_action(BlockKind::Heading { level: 5 }, cx);
    }

    /// 快捷键动作：聚焦块转六级标题。
    pub(super) fn on_convert_heading_6_action(
        &mut self,
        _: &ConvertHeading6,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_kind_for_action(BlockKind::Heading { level: 6 }, cx);
    }

    /// 快捷键动作：聚焦块转回普通段落。
    pub(super) fn on_convert_paragraph_action(
        &mut self,
        _: &ConvertParagraph,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.convert_kind_for_action(BlockKind::Paragraph, cx);
    }

    /// 当前聚焦的可编辑块（右键菜单中行内格式的作用目标）。
    fn focused_format_block(&self) -> Option<Entity<Block>> {
        let id = self.active_entity_id?;
        self.focusable_entity_by_id(id)
    }

    /// 对聚焦块应用行内格式：有选中走样式开关，无选中插入成对标记。
    /// 调用后关闭菜单并标脏。
    fn apply_inline_format_to_focused_block(
        &mut self,
        format: InlineFormat,
        cx: &mut Context<Self>,
    ) {
        let Some(block) = self.focused_format_block() else {
            return;
        };
        self.close_context_menu(cx);
        block.update(cx, |block, cx| {
            if block.selected_range.is_empty() {
                block.insert_format_markers(format, cx);
            } else {
                block.toggle_span_format(format, cx);
            }
        });
        self.mark_dirty(cx);
    }

    /// 点击「加粗」：对聚焦块应用加粗格式。
    pub(super) fn on_format_bold(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_inline_format_to_focused_block(InlineFormat::Bold, cx);
    }

    /// 点击「斜体」：对聚焦块应用斜体格式。
    pub(super) fn on_format_italic(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_inline_format_to_focused_block(InlineFormat::Italic, cx);
    }

    /// 点击「行内代码」：对聚焦块应用行内代码格式。
    pub(super) fn on_format_inline_code(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_inline_format_to_focused_block(InlineFormat::Code, cx);
    }

    /// 右键菜单剪贴板项：统一转发给当前聚焦块的同名键盘逻辑。
    fn with_focused_block_for_clipboard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&Entity<Block>, &mut Window, &mut Context<Self>),
    ) {
        let Some(block) = self.focused_format_block() else {
            return;
        };
        self.close_context_menu(cx);
        apply(&block, window, cx);
        self.mark_dirty(cx);
    }

    /// 点击「剪切」：转发给聚焦块的剪切逻辑。
    pub(super) fn on_clipboard_cut(
        &mut self,
        _event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_focused_block_for_clipboard(window, cx, |block, window, cx| {
            block.update(cx, |block, cx| {
                block.on_cut(&CutAction, window, cx);
            });
        });
    }

    /// 点击「复制」：转发给聚焦块的复制逻辑。
    pub(super) fn on_clipboard_copy(
        &mut self,
        _event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_focused_block_for_clipboard(window, cx, |block, window, cx| {
            block.update(cx, |block, cx| {
                block.on_copy(&CopyAction, window, cx);
            });
        });
    }

    /// 点击「粘贴」：转发给聚焦块的粘贴逻辑。
    pub(super) fn on_clipboard_paste(
        &mut self,
        _event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_focused_block_for_clipboard(window, cx, |block, window, cx| {
            block.update(cx, |block, cx| {
                block.on_paste(&PasteAction, window, cx);
            });
        });
    }

    /// 点击「全选」：转发给聚焦块的全选逻辑。
    pub(super) fn on_clipboard_select_all(
        &mut self,
        _event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_focused_block_for_clipboard(window, cx, |block, window, cx| {
            block.update(cx, |block, cx| {
                block.on_select_all(&SelectAllAction, window, cx);
            });
        });
    }

    /// 点击行数减一：正文行数下限为一行。
    pub(super) fn on_table_rows_decrement(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.table_insert_dialog.as_mut() {
            dialog.body_rows = dialog.body_rows.saturating_sub(1).max(1);
            cx.notify();
        }
    }

    /// 点击行数加一：增加表格正文行数。
    pub(super) fn on_table_rows_increment(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.table_insert_dialog.as_mut() {
            dialog.body_rows += 1;
            cx.notify();
        }
    }

    /// 点击列数减一：列数下限为一列。
    pub(super) fn on_table_columns_decrement(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.table_insert_dialog.as_mut() {
            dialog.columns = dialog.columns.saturating_sub(1).max(1);
            cx.notify();
        }
    }

    /// 点击列数加一：增加表格列数。
    pub(super) fn on_table_columns_increment(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.table_insert_dialog.as_mut() {
            dialog.columns += 1;
            cx.notify();
        }
    }

    /// 点击取消：关闭表格插入对话框。
    pub(super) fn on_cancel_table_insert_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_table_insert_dialog(cx);
    }

    /// 点击确认：在目标位置插入指定行列的空表格。
    pub(super) fn on_confirm_table_insert_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.table_insert_dialog.take() else {
            return;
        };

        let table = TableModel::new_empty(dialog.body_rows, dialog.columns);
        let new_block = Self::new_table_block(cx, table);

        match dialog.target {
            TableInsertTarget::After(entity_id) => {
                if let Some(location) = self.document.find_block_location(entity_id) {
                    self.document.insert_blocks_at(
                        location.parent,
                        location.index + 1,
                        vec![new_block.clone()],
                        cx,
                    );
                } else {
                    self.document.insert_blocks_at(
                        None,
                        self.document.root_count(),
                        vec![new_block.clone()],
                        cx,
                    );
                }
            }
            TableInsertTarget::Before(entity_id) => {
                if let Some(location) = self.document.find_block_location(entity_id) {
                    self.document.insert_blocks_at(
                        location.parent,
                        location.index,
                        vec![new_block.clone()],
                        cx,
                    );
                } else {
                    self.document
                        .insert_blocks_at(None, 0, vec![new_block.clone()], cx);
                }
            }
            TableInsertTarget::Append => {
                self.document.insert_blocks_at(
                    None,
                    self.document.root_count(),
                    vec![new_block.clone()],
                    cx,
                );
            }
        }

        // 表格作为容器末尾插入时下方无行，光标在渲染模式下无法越过表格，
        // 因此补一个空段落承接光标。
        self.ensure_trailing_paragraph_after_structural(&new_block, cx);

        self.rebuild_table_runtimes(cx);
        if let Some(first_cell) = new_block
            .read(cx)
            .table_runtime
            .as_ref()
            .and_then(|runtime| runtime.header.first())
        {
            self.focus_block(first_cell.entity_id());
        }
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        cx.notify();
    }

    /// 执行表格轴右键菜单操作（参数由菜单事件显式携带，不再读浮层 state）。
    pub(super) fn apply_table_axis_op(
        &mut self,
        table_block: &Entity<Block>,
        kind: TableAxisKind,
        index: usize,
        op: TableAxisMenuOp,
        cx: &mut Context<Self>,
    ) {
        let table = table_block.read(cx).record.table.clone();
        match (kind, op) {
            (TableAxisKind::Column, TableAxisMenuOp::AlignColumnLeft) => {
                // 左对齐为默认值，直接输出无标记的 `---` 形式。
                self.set_table_column_alignment(
                    table_block,
                    index,
                    TableColumnAlignment::Default,
                    cx,
                );
            }
            (TableAxisKind::Column, TableAxisMenuOp::AlignColumnCenter) => {
                self.set_table_column_alignment(
                    table_block,
                    index,
                    TableColumnAlignment::Center,
                    cx,
                );
            }
            (TableAxisKind::Column, TableAxisMenuOp::AlignColumnRight) => {
                self.set_table_column_alignment(
                    table_block,
                    index,
                    TableColumnAlignment::Right,
                    cx,
                );
            }
            (TableAxisKind::Column, TableAxisMenuOp::MoveColumnLeft) => {
                if index == 0 {
                    return;
                }
                self.move_table_column(table_block, index, -1, cx);
            }
            (TableAxisKind::Column, TableAxisMenuOp::MoveColumnRight) => {
                let can_move = table
                    .as_ref()
                    .map(|table| index + 1 < table.column_count())
                    .unwrap_or(false);
                if !can_move {
                    return;
                }
                self.move_table_column(table_block, index, 1, cx);
            }
            (TableAxisKind::Column, TableAxisMenuOp::DeleteColumn) => {
                let column_count = table.as_ref().map(|table| table.column_count());
                // 删掉唯一列会清空表格，因此直接移除整个块。
                if column_count == Some(1) {
                    self.remove_table_block(table_block, cx);
                } else {
                    self.delete_table_column(table_block, index, cx);
                }
            }
            (_, TableAxisMenuOp::ToggleHeaders) => {
                let next = !crate::config::EditorSettings::table_headers_visible(cx);
                crate::config::EditorSettings::set_table_headers_visible(cx, next);
                // 该偏好在渲染表格单元格时读取；重绘编辑器（连带所有表格）即可反映。
                cx.notify();
            }
            (TableAxisKind::Row, TableAxisMenuOp::MoveRowUp) => {
                if index == 0 {
                    return;
                }
                self.move_table_row(table_block, index, -1, cx);
            }
            (TableAxisKind::Row, TableAxisMenuOp::MoveRowDown) => {
                let can_move = table
                    .as_ref()
                    .map(|table| index < table.rows.len())
                    .unwrap_or(false);
                if !can_move {
                    return;
                }
                self.move_table_row(table_block, index, 1, cx);
            }
            (TableAxisKind::Row, TableAxisMenuOp::DeleteRow) => {
                let row_count = table.as_ref().map(|table| table.rows.len());
                // 可视序号 0 为表头：删除它会把首个正文行上提，
                // 若已无正文行则说明是表格最后一行，直接移除整表。
                if index == 0 {
                    if row_count == Some(0) {
                        self.remove_table_block(table_block, cx);
                    } else {
                        self.delete_table_header_row(table_block, cx);
                    }
                } else {
                    self.delete_table_row(table_block, index - 1, cx);
                }
            }
            // 轴种类与操作错配（菜单按种类构造，不会发生，防御性忽略）。
            _ => {}
        }
    }

    /// 渲染表格插入对话框（框架 `Dialog` 裸挂外壳 + 自研遮罩层，与链接框同模式）。
    ///
    /// 状态为纯数据（无输入框实体/订阅），但全局 `dismiss` 扇出同样无 `Window`，
    /// 故同样不经 `Root` 托管。步进器 `-`/`+` 自渲染留 `content` 内（经句柄路由），
    /// 取消/确认进 `footer`（`cx.listener` 直连）。
    pub(super) fn render_table_insert_dialog_overlay(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.table_insert_dialog.as_ref()?;
        let d = &ThemeDimensions::DEFAULT;
        let i18n = cx.global::<I18nManager>().snapshot();
        let title = i18n.t("table_insert_title", &[]);
        let description = i18n.t("table_insert_description", &[]);
        let rows_label = i18n.t("table_insert_body_rows", &[]);
        let columns_label = i18n.t("table_insert_columns", &[]);
        let cancel = i18n.t("table_insert_cancel", &[]);
        let confirm = i18n.t("table_insert_confirm", &[]);
        let body_rows = dialog.body_rows;
        let columns = dialog.columns;
        let width = d.dialog_width.min(d.table_insert_dialog_width);

        // 对话框主体：描述 + 双步进器进 `content`，取消/确认进 `footer`。
        // 步进器按钮路由用实体直连（外层有 `Context`，见 `dialog_button_click`）。
        let editor = cx.entity();
        let content = Dialog::new(cx)
            .width(px(width))
            .title(title)
            .keyboard(false)
            .overlay(false)
            .close_button(false)
            .content(move |content, _, cx| {
                // 构建闭包是 `Fn`，捕获先克隆为本轮局部。
                let description = description.clone();
                let rows_label = rows_label.clone();
                let columns_label = columns_label.clone();
                let theme = cx.theme().clone();
                let t = &ThemeTypography::DEFAULT;
                // 步进器：标签 + `-` 数值 `+`。
                let stepper = |id_prefix: &'static str,
                               text: String,
                               value: usize,
                               on_dec: fn(
                    &mut Editor,
                    &ClickEvent,
                    &mut Window,
                    &mut Context<Editor>,
                ),
                               on_inc: fn(
                    &mut Editor,
                    &ClickEvent,
                    &mut Window,
                    &mut Context<Editor>,
                )| {
                    let dec_editor = editor.clone();
                    let inc_editor = editor.clone();
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(d.table_insert_stepper_gap))
                        .child(
                            div()
                                .text_size(px(t.dialog_body_size))
                                .font_weight(t.dialog_button_weight)
                                .text_color(theme.foreground)
                                .child(text),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(d.table_insert_stepper_gap))
                                .child(
                                    Button::new((id_prefix, 0usize))
                                        .label("-")
                                        .secondary()
                                        .compact()
                                        .size(px(d.table_insert_stepper_button_size))
                                        .on_click(Self::dialog_button_click(dec_editor, on_dec)),
                                )
                                .child(
                                    div()
                                        .min_w(px(d.table_insert_stepper_value_min_width))
                                        .h(px(d.table_insert_stepper_button_size))
                                        .px(px(d.table_insert_stepper_value_padding_x))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(d.table_insert_stepper_radius))
                                        .border(px(d.dialog_border_width))
                                        .border_color(theme.border)
                                        .bg(theme.popover)
                                        .text_size(px(t.dialog_body_size))
                                        .text_color(theme.popover_foreground)
                                        .child(value.to_string()),
                                )
                                .child(
                                    Button::new((id_prefix, 1usize))
                                        .label("+")
                                        .secondary()
                                        .compact()
                                        .size(px(d.table_insert_stepper_button_size))
                                        .on_click(Self::dialog_button_click(inc_editor, on_inc)),
                                ),
                        )
                };
                content
                    .child(
                        div()
                            .text_size(px(t.dialog_body_size))
                            .font_weight(t.dialog_body_weight)
                            .text_color(theme.foreground)
                            .child(description),
                    )
                    .child(stepper(
                        "table-body-rows",
                        rows_label,
                        body_rows,
                        Self::on_table_rows_decrement,
                        Self::on_table_rows_increment,
                    ))
                    .child(stepper(
                        "table-columns",
                        columns_label,
                        columns,
                        Self::on_table_columns_decrement,
                        Self::on_table_columns_increment,
                    ))
            })
            .footer(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(d.dialog_button_gap))
                    .child(
                        Button::new("cancel-table-insert-dialog")
                            .label(cancel)
                            .secondary()
                            .on_click(cx.listener(Self::on_cancel_table_insert_dialog)),
                    )
                    .child(
                        Button::new("confirm-table-insert-dialog")
                            .label(confirm)
                            .primary()
                            .on_click(cx.listener(Self::on_confirm_table_insert_dialog)),
                    ),
            );

        Some(
            div()
                .id("table-insert-dialog-overlay")
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.overlay)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(Self::on_dismiss_context_menu_overlay),
                )
                .child(
                    div()
                        .w_full()
                        .px(px(d.editor_padding))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                    cx.stop_propagation()
                                })
                                .child(content),
                        ),
                )
                .into_any_element(),
        )
    }

    /// 渲染链接/图片插入对话框（框架 `Dialog` 裸挂外壳 + 自研遮罩层）。
    ///
    /// 刻意不经 `Root` 托管：输入框实体、回车订阅、校验与全局 `dismiss` 扇出
    /// （多处无 `Window` 调用点）全部沿用，`Dialog` 只提供统一样式外壳与无障碍角色。
    /// 隐式关闭语义不变：遮罩点击走旧 `on_dismiss`、ESC 走全局 `DismissTransientUi`、
    /// 回车走输入订阅确认，故 `Dialog` 自身的 `keyboard`/`overlay`/`close_button` 全禁。
    pub(super) fn render_link_dialog_overlay(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.link_dialog.as_ref()?;
        let d = &ThemeDimensions::DEFAULT;
        let i18n = cx.global::<I18nManager>().snapshot();
        let title = if dialog.is_image {
            i18n.t("dialog_link_image_title", &[])
        } else {
            i18n.t("dialog_link_title", &[])
        };
        let label_text = i18n.t("dialog_link_text_label", &[]);
        let url_text = i18n.t("dialog_link_url", &[]);
        let browse_text = i18n.t("dialog_link_browse", &[]);
        let cancel = i18n.t("table_insert_cancel", &[]);
        let confirm = i18n.t("table_insert_confirm", &[]);
        let label = dialog.label.clone();
        let url = dialog.url.clone();
        let is_image = dialog.is_image;
        let width = d.dialog_width.min(d.table_insert_dialog_width);
        let row_gap = d.table_insert_stepper_gap;

        // 对话框主体：标题 + 双输入行（+ 图片模式的浏览按钮）进 `content`，
        // 取消/确认进 `footer`（`Context` 可用，直接 `cx.listener` 直连；
        // 浏览按钮在 content 构建闭包内，改用实体直连，见 `dialog_button_click`）。
        let editor = cx.entity();
        let content = Dialog::new(cx)
            .width(px(width))
            .title(title)
            .keyboard(false)
            .overlay(false)
            .close_button(false)
            .content(move |content, _, cx| {
                // 构建闭包是 `Fn`，捕获先克隆为本轮局部。
                let label_text = label_text.clone();
                let url_text = url_text.clone();
                let browse_text = browse_text.clone();
                let browse_editor = editor.clone();
                let theme = cx.theme().clone();
                let t = &ThemeTypography::DEFAULT;
                // 单个输入行：上方标签，下方原生输入框（自带右键剪贴板菜单）。
                let field_row = |text: String, input: Entity<InputState>| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(row_gap))
                        .child(
                            div()
                                .text_size(px(t.dialog_body_size))
                                .font_weight(t.dialog_button_weight)
                                .text_color(theme.foreground)
                                .child(text),
                        )
                        .child(Input::new(&input).cleanable(true))
                };
                content
                    .child(field_row(label_text, label.clone()))
                    .child(field_row(url_text, url.clone()))
                    .when(is_image, |this| {
                        this.child(
                            Button::new("browse-link-image-file")
                                .label(browse_text.clone())
                                .secondary()
                                .on_click(Self::dialog_button_click(
                                    browse_editor,
                                    Self::on_browse_link_image_file,
                                )),
                        )
                    })
            })
            .footer(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(d.dialog_button_gap))
                    .child(
                        Button::new("cancel-link-insert-dialog")
                            .label(cancel)
                            .secondary()
                            .on_click(cx.listener(Self::on_cancel_link_dialog)),
                    )
                    .child(
                        Button::new("confirm-link-insert-dialog")
                            .label(confirm)
                            .primary()
                            .on_click(cx.listener(Self::on_confirm_link_dialog)),
                    ),
            );

        Some(
            div()
                .id("link-insert-dialog-overlay")
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.overlay)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(Self::on_dismiss_link_dialog_overlay),
                )
                .child(
                    div()
                        .w_full()
                        .px(px(d.editor_padding))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                    cx.stop_propagation()
                                })
                                .child(content),
                        ),
                )
                .into_any_element(),
        )
    }

    /// 点击浮层空白处关闭链接/图片对话框。
    pub(super) fn on_dismiss_link_dialog_overlay(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_link_dialog(cx);
    }

    /// 图片对话框「选择本地文件」：原生文件框选图，经图片粘贴管线
    /// 落盘/定位后回填 URL 栏（标签保持用户已填内容）。
    pub(super) fn on_browse_link_image_file(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.link_dialog.as_ref() else {
            return;
        };
        if !dialog.is_image {
            return;
        }
        let editor = cx.entity();
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |_weak: WeakEntity<Self>, cx| {
            let Ok(Ok(Some(mut paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };
            let Some(window) = cx.update(|cx| cx.active_window()) else {
                return;
            };
            let _ = cx.update_window(window, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    match editor.local_image_url_for_path(&path) {
                        Ok(url) => {
                            if let Some(dialog) = editor.link_dialog.as_ref() {
                                dialog.url.update(cx, |input, cx| {
                                    input.set_value(url, window, cx);
                                });
                                cx.notify();
                            }
                        }
                        Err(err) => editor.show_image_paste_error(err, cx),
                    }
                });
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BlockKind, BlockRecord, ContextMenuState, Editor, InlineFormat, InsertLink,
        TableInsertTarget,
    };
    use rgpui::{AppContext, ClickEvent, TestAppContext};

    #[rgpui::test]
    async fn insert_menu_adds_block_after_target(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        editor.update(cx, |editor, cx| {
            let target = editor
                .document
                .first_root()
                .map(|block| block.entity_id())
                .expect("one root block");
            editor.insert_block_after_target(
                TableInsertTarget::After(target),
                BlockRecord::with_plain_text(BlockKind::Heading { level: 2 }, String::new()),
                cx,
            );

            assert_eq!(editor.document.root_count(), 2);
            // 新菜单关闭，焦点落到新插入的标题块。
            assert!(editor.context_menu.is_none());
            let kinds = editor
                .document
                .root_blocks()
                .iter()
                .map(|block| block.read(cx).kind())
                .collect::<Vec<_>>();
            assert_eq!(
                kinds,
                vec![BlockKind::Paragraph, BlockKind::Heading { level: 2 },]
            );
        });
    }

    /// 右键「插入 Mermaid」必须建原生 MermaidBlock，
    /// 与解析 ```mermaid 围栏（document.rs）保持同一类型与行为。
    #[rgpui::test]
    async fn insert_mermaid_creates_native_mermaid_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        let target = editor.read_with(cx, |editor, _| {
            editor
                .document
                .first_root()
                .map(|block| block.entity_id())
                .expect("one root block")
        });
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.context_menu = Some(ContextMenuState::Insert {
                    target: TableInsertTarget::After(target),
                });
                editor.on_insert_mermaid_block(&rgpui::ClickEvent::default(), window, cx);
            });
        });

        editor.update(cx, |editor, cx| {
            let kinds = editor
                .document
                .root_blocks()
                .iter()
                .map(|block| block.read(cx).kind())
                .collect::<Vec<_>>();
            // 多行编辑块插入后自动补尾部空段落承接光标（既有插入规则）。
            assert_eq!(
                kinds,
                vec![
                    BlockKind::Paragraph,
                    BlockKind::MermaidBlock,
                    BlockKind::Paragraph
                ]
            );
            let mermaid = editor.document.root_blocks()[1].read(cx);
            // 编辑文本是去围栏正文；序列化输出仍含完整围栏。
            assert_eq!(mermaid.rendered_text(), "flowchart TD\n    A --> B");
            assert!(
                editor
                    .document
                    .markdown_text(cx)
                    .contains("```mermaid\nflowchart TD\n    A --> B\n```")
            );
            assert!(editor.context_menu.is_none());
        });
    }

    #[rgpui::test]
    async fn insert_menu_appends_block_at_end(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        editor.update(cx, |editor, cx| {
            editor.insert_block_after_target(
                TableInsertTarget::Append,
                BlockRecord::with_plain_text(BlockKind::Separator, String::new()),
                cx,
            );

            assert_eq!(editor.document.root_count(), 3);
            let kinds = editor
                .document
                .root_blocks()
                .iter()
                .map(|block| block.read(cx).kind())
                .collect::<Vec<_>>();
            // 分隔线是结构块，末尾会自动补一个空段落承接光标。
            assert_eq!(
                kinds,
                vec![
                    BlockKind::Paragraph,
                    BlockKind::Separator,
                    BlockKind::Paragraph,
                ]
            );
        });
    }

    #[rgpui::test]
    async fn convert_paragraph_to_heading_keeps_text(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::i18n::I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
        let (editor, cx) =
            cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.on_convert_heading_2(&ClickEvent::default(), window, cx);

                let block = editor
                    .focusable_entity_by_id(editor.active_entity_id.expect("active block"))
                    .expect("focused block");
                assert_eq!(block.read(cx).kind(), BlockKind::Heading { level: 2 });
                assert_eq!(
                    block.read(cx).record.title.plain_text().to_string(),
                    "alpha"
                );
                assert!(editor.context_menu.is_none());
            });
        });
    }

    #[rgpui::test]
    async fn convert_heading_back_to_paragraph(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::i18n::I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
        let (editor, cx) = cx
            .add_window_view(|_window, cx| Editor::from_markdown(cx, "# alpha".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let kind = editor
                    .focusable_entity_by_id(editor.active_entity_id.expect("active block"))
                    .expect("focused block")
                    .read(cx)
                    .kind();
                assert_eq!(kind, BlockKind::Heading { level: 1 });

                editor.on_convert_paragraph(&ClickEvent::default(), window, cx);

                let block = editor
                    .focusable_entity_by_id(editor.active_entity_id.expect("active block"))
                    .expect("focused block");
                assert_eq!(block.read(cx).kind(), BlockKind::Paragraph);
                assert_eq!(
                    block.read(cx).record.title.plain_text().to_string(),
                    "alpha"
                );
            });
        });
    }

    #[rgpui::test]
    async fn insert_paragraph_above_inserts_before_target(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        editor.update(cx, |editor, cx| {
            let target = editor
                .document
                .first_root()
                .map(|block| block.entity_id())
                .expect("one root block");
            editor.context_menu = Some(ContextMenuState::Insert {
                target: TableInsertTarget::After(target),
            });
            editor.insert_paragraph_near_target(true, cx);

            assert_eq!(editor.document.root_count(), 2);
            let kinds = editor
                .document
                .root_blocks()
                .iter()
                .map(|block| block.read(cx).kind())
                .collect::<Vec<_>>();
            assert_eq!(kinds, vec![BlockKind::Paragraph, BlockKind::Paragraph]);
            // 新段落在目标之前，焦点落到新块。
            let focused = editor.active_entity_id.expect("focus on new block");
            assert_eq!(editor.document.root_blocks()[0].entity_id(), focused);
            assert!(editor.context_menu.is_none());
        });
    }

    #[rgpui::test]
    async fn format_without_selection_inserts_markers(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        editor.update(cx, |editor, cx| {
            let target = editor
                .document
                .first_root()
                .map(|block| block.entity_id())
                .expect("one root block");
            editor.active_entity_id = Some(target);
            let block = editor
                .focusable_entity_by_id(target)
                .expect("first block focusable");
            // 光标折叠在开头，无选中。
            block.update(cx, |block, _cx| {
                block.selected_range = 0..0;
            });
            editor.apply_inline_format_to_focused_block(InlineFormat::Bold, cx);

            let text = block.read(cx).record.title.plain_text().to_string();
            assert_eq!(text, "****alpha");
            // 光标落在首尾标记之间。
            assert_eq!(block.read(cx).selected_range, 2..2);
            assert!(editor.context_menu.is_none());
        });
    }

    #[rgpui::test]
    async fn insert_link_template_selects_label(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        editor.update(cx, |editor, cx| {
            editor.insert_link_block(
                TableInsertTarget::Append,
                "text",
                "https://example.com",
                false,
                cx,
            );

            assert_eq!(editor.document.root_count(), 2);
            let id = editor.active_entity_id.expect("focus lands on new block");
            let block = editor
                .focusable_entity_by_id(id)
                .expect("new block focusable");
            let markdown = block.read(cx).record.title_to_markdown();
            assert_eq!(markdown, "[text](https://example.com)");
            // 选中链接标签，输入即改写。
            assert_eq!(block.read(cx).selected_range, 0..4);
        });
    }

    #[test]
    fn link_markdown_falls_back_on_empty_input() {
        assert_eq!(
            Editor::link_markdown("docs", "https://x.com/y", false),
            "[docs](https://x.com/y)"
        );
        assert_eq!(
            Editor::link_markdown("pic", "https://x.com/y.png", true),
            "![pic](https://x.com/y.png)"
        );
        assert_eq!(
            Editor::link_markdown("  ", "  ", false),
            "[text](https://example.com)"
        );
        assert_eq!(
            Editor::link_markdown("", "", true),
            "![image](https://example.com)"
        );
        // 裸域名与 www 开头自动补协议；相对路径与锚点不动。
        assert_eq!(
            Editor::link_markdown("docs", "example.com/y", false),
            "[docs](https://example.com/y)"
        );
        assert_eq!(
            Editor::link_markdown("docs", "www.example.com", false),
            "[docs](https://www.example.com)"
        );
        assert_eq!(
            Editor::link_markdown("docs", "./relative/path", false),
            "[docs](./relative/path)"
        );
        assert_eq!(
            Editor::link_markdown("docs", "#anchor", false),
            "[docs](#anchor)"
        );
    }

    #[rgpui::test]
    async fn insert_link_action_opens_dialog_at_active_block(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::i18n::I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
        let (editor, cx) =
            cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.on_insert_link_action(&InsertLink, window, cx);

                let dialog = editor.link_dialog.as_ref().expect("dialog open");
                assert!(!dialog.is_image);
                assert!(editor.context_menu.is_none());
            });
        });
    }

    #[rgpui::test]
    async fn link_dialog_confirm_inserts_edited_values(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::i18n::I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
        let (editor, cx) =
            cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let target = TableInsertTarget::Append;
                editor.open_link_dialog_at(target, false, None, window, cx);
                assert!(editor.link_dialog.is_some());
                // 模拟用户改写 URL 后确认。
                let url = editor
                    .link_dialog
                    .as_ref()
                    .expect("dialog open")
                    .url
                    .clone();
                url.update(cx, |input, cx| {
                    input.set_value("https://x.com/y", window, cx);
                });
                editor.confirm_link_dialog(cx);

                assert!(editor.link_dialog.is_none());
                assert_eq!(editor.document.root_count(), 2);
                let markdown = editor
                    .document
                    .root_blocks()
                    .last()
                    .expect("new block")
                    .read(cx)
                    .record
                    .title_to_markdown();
                assert_eq!(markdown, "[text](https://x.com/y)");
            });
        });
    }

    #[rgpui::test]
    async fn link_action_with_selection_opens_wrap_dialog(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::i18n::I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
        let (editor, cx) =
            cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let id = editor.active_entity_id.expect("active block");
                let block = editor.focusable_entity_by_id(id).expect("focused block");
                block.update(cx, |block, _cx| {
                    block.selected_range = 0..5;
                });
                editor.on_insert_link_action(&InsertLink, window, cx);

                let dialog = editor.link_dialog.as_ref().expect("dialog open");
                assert!(dialog.wrap.is_some());
                assert_eq!(dialog.label.read(cx).value().to_string(), "alpha");
            });
        });
    }

    #[rgpui::test]
    async fn link_dialog_confirm_wraps_selection_in_place(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::i18n::I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
        let (editor, cx) =
            cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let id = editor.active_entity_id.expect("active block");
                let block = editor.focusable_entity_by_id(id).expect("focused block");
                block.update(cx, |block, _cx| {
                    block.selected_range = 0..5;
                });
                editor.on_insert_link_action(&InsertLink, window, cx);

                let url = editor
                    .link_dialog
                    .as_ref()
                    .expect("dialog open")
                    .url
                    .clone();
                url.update(cx, |input, cx| {
                    input.set_value("example.com/y", window, cx);
                });
                editor.confirm_link_dialog(cx);

                assert!(editor.link_dialog.is_none());
                // 原地包裹，不新增块；裸域名自动补协议。
                assert_eq!(editor.document.root_count(), 1);
                let markdown = editor
                    .document
                    .root_blocks()
                    .first()
                    .expect("block")
                    .read(cx)
                    .record
                    .title_to_markdown();
                assert_eq!(markdown, "[alpha](https://example.com/y)");
            });
        });
    }

    #[rgpui::test]
    async fn link_dialog_renders_in_framework_shell(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::i18n::I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
        let (editor, cx) =
            cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        // 图片变体（含浏览按钮）完整绘制一帧不断言（冒烟测试）。
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.open_link_dialog_at(TableInsertTarget::Append, true, None, window, cx);
                assert!(editor.link_dialog.is_some());
            });
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }
}
