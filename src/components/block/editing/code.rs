//! 代码块运行时缓存管理。

use super::*;

/// 规范化代码语言输入。
fn normalize_code_language_input(text: &str) -> String {
    text.replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string()
}

impl Block {
    /// 取代码高亮结果。
    pub(crate) fn code_highlight_result(&self) -> Option<&HighlightResult> {
        self.code_highlight.as_ref()
    }

    /// 请求后台计算代码块高亮；供首次布局可见块时调用。
    ///
    /// 语法高亮由 tree-sitter 完成，单个大代码块可能耗时数十毫秒。若放在
    /// 布局帧内同步执行，会让滚动出现尖峰；这里派发到后台线程池，完成后再
    /// 回填结果并通知重绘。尚未回填期间按无高亮渲染，不影响内容正确性。
    pub(crate) fn ensure_code_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(language) = (match &self.record.kind {
            BlockKind::CodeBlock { language } => Some(language.clone()),
            _ => None,
        }) else {
            self.code_highlight = None;
            self.code_highlight_key = None;
            self.code_highlight_pending_key = None;
            return;
        };

        let key = (language.clone(), self.record.content_version());
        if self.code_highlight_key.as_ref() == Some(&key)
            || self.code_highlight_pending_key.as_ref() == Some(&key)
        {
            return;
        }

        let source = self.render_cache.plain_text().to_string();
        self.code_highlight_pending_key = Some(key.clone());
        let pending_key = key;
        self.code_highlight_task = Some(cx.spawn(
            async move |this: WeakEntity<Block>, cx: &mut AsyncApp| {
                let language = language.map(|value| value.to_string());
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        crate::components::markdown::highlight::highlight_code_block_async(
                            language, source,
                        )
                        .await
                    })
                    .await;
                let _ = update_entity_forgiving(&this, cx, move |block, cx| {
                    // 请求已被更新的内容版本取代（或已取消）时丢弃过期结果；
                    // 首次回填前 code_highlight_key 为空，不能用它判断有效性。
                    if block.code_highlight_pending_key.as_ref() != Some(&pending_key) {
                        return;
                    }
                    block.code_highlight_pending_key = None;
                    block.code_highlight_key = Some(pending_key);
                    block.code_highlight = result;
                    cx.notify();
                });
            },
        ));
    }

    /// 同步代码高亮。
    ///
    /// 高亮只由语言与块内容决定；内容版本号未变时（光标移动、滚动、同一次
    /// 交互里的重复刷新）直接复用上次结果，避免 tree-sitter 全量重解析。
    /// 内容变更必须走 `set_title`/`set_kind`，否则版本号不递增、高亮会陈旧。
    pub(super) fn sync_code_highlight(&mut self) {
        let Some(language) = (match &self.record.kind {
            BlockKind::CodeBlock { language } => Some(language.clone()),
            _ => None,
        }) else {
            self.code_highlight = None;
            self.code_highlight_key = None;
            return;
        };

        let key = (language.clone(), self.record.content_version());
        if self.code_highlight_key.as_ref() == Some(&key) {
            return;
        }
        count_highlight_recompute();
        self.code_highlight =
            highlight_code_block(language.as_deref(), self.render_cache.plain_text());
        self.code_highlight_key = Some(key);
        // 同步物化了更新版本的结果；在途的后台任务是旧版本，回填时按
        // pending 失配丢弃，不得覆盖这份新结果。
        self.code_highlight_pending_key = None;
    }

    /// 取代码语言文本。
    pub(crate) fn code_language_text(&self) -> &str {
        match &self.record.kind {
            BlockKind::CodeBlock {
                language: Some(language),
            } => language.as_ref(),
            _ => "",
        }
    }

    /// 取代码语言框光标偏移。
    pub(crate) fn code_language_cursor_offset(&self) -> usize {
        if self.code_language_selection_reversed {
            self.code_language_selected_range.start
        } else {
            self.code_language_selected_range.end
        }
    }

    /// 代码语言范围转 UTF-16。
    pub(crate) fn code_language_range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        Self::utf8_range_to_utf16_in(self.code_language_text(), range)
    }

    /// 代码语言范围从 UTF-16 转回。
    pub(crate) fn code_language_range_from_utf16(
        &self,
        range_utf16: &Range<usize>,
    ) -> Range<usize> {
        Self::utf16_range_to_utf8_in(self.code_language_text(), range_utf16)
    }

    /// 取代码语言前一字形边界。
    pub(crate) fn previous_code_language_boundary(&self, offset: usize) -> usize {
        self.code_language_text()
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    /// 取代码语言后一字形边界。
    pub(crate) fn next_code_language_boundary(&self, offset: usize) -> usize {
        self.code_language_text()
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.code_language_text().len())
    }

    /// 移动代码语言框光标（落到字符边界）。
    pub(crate) fn move_code_language_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let clamped = crate::text_offsets::floor_char_boundary(self.code_language_text(), offset);
        self.code_language_selected_range = clamped..clamped;
        self.code_language_selection_reversed = false;
        self.code_language_marked_range = None;
        self.cursor_blink_epoch = Instant::now();
        cx.notify();
    }

    /// 把代码语言选区扩展到指定偏移（落到字符边界）。
    pub(crate) fn select_code_language_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let clamped = crate::text_offsets::floor_char_boundary(self.code_language_text(), offset);
        if self.code_language_selection_reversed {
            self.code_language_selected_range.start = clamped;
        } else {
            self.code_language_selected_range.end = clamped;
        }
        if self.code_language_selected_range.end < self.code_language_selected_range.start {
            self.code_language_selection_reversed = !self.code_language_selection_reversed;
            self.code_language_selected_range =
                self.code_language_selected_range.end..self.code_language_selected_range.start;
        }
        self.cursor_blink_epoch = Instant::now();
        cx.notify();
    }

    /// 替换代码语言框指定范围文本。
    pub(crate) fn replace_code_language_text_in_range(
        &mut self,
        range: Range<usize>,
        new_text: &str,
        selected_range_relative: Option<Range<usize>>,
        mark_inserted_text: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.kind().is_code_block() {
            return;
        }

        self.prepare_undo_capture(UndoKind::CoalescibleText, cx);

        let current = self.code_language_text().to_string();
        let range = crate::text_offsets::normalize_utf8_range(&current, range);
        let inserted = new_text.replace("\r\n", " ").replace(['\r', '\n'], " ");
        let mut raw_next = String::new();
        raw_next.push_str(&current[..range.start]);
        raw_next.push_str(&inserted);
        raw_next.push_str(&current[range.end..]);

        let trimmed_start = raw_next.len() - raw_next.trim_start().len();
        let normalized = normalize_code_language_input(&raw_next);
        let normalized_len = normalized.len();
        let raw_inserted_end = range.start + inserted.len();
        let selection_reversed = selected_range_relative
            .as_ref()
            .is_some_and(|relative| relative.end < relative.start);
        let selected_range_relative = selected_range_relative
            .map(|relative| crate::text_offsets::normalize_utf8_range(&inserted, relative));
        let next_cursor = selected_range_relative
            .as_ref()
            .map(|relative| range.start + relative.end)
            .unwrap_or(raw_inserted_end)
            .saturating_sub(trimmed_start)
            .min(normalized_len);
        let next_selection = selected_range_relative
            .as_ref()
            .map(|relative| {
                let start = (range.start + relative.start)
                    .saturating_sub(trimmed_start)
                    .min(normalized_len);
                let end = (range.start + relative.end)
                    .saturating_sub(trimmed_start)
                    .min(normalized_len);
                start.min(end)..start.max(end)
            })
            .unwrap_or_else(|| next_cursor..next_cursor);
        let next_marked = if mark_inserted_text && !inserted.is_empty() {
            let start = range
                .start
                .saturating_sub(trimmed_start)
                .min(normalized_len);
            let end = raw_inserted_end
                .saturating_sub(trimmed_start)
                .min(normalized_len);
            (start < end).then_some(start..end)
        } else {
            None
        };

        let old_language = match &self.record.kind {
            BlockKind::CodeBlock { language } => language.clone(),
            _ => None,
        };
        self.record.set_kind(BlockKind::CodeBlock {
            language: (!normalized.is_empty()).then(|| SharedString::from(normalized)),
        });
        self.code_language_selected_range =
            crate::text_offsets::normalize_utf8_range(self.code_language_text(), next_selection);
        self.code_language_selection_reversed = selection_reversed;
        self.code_language_marked_range = next_marked.map(|range| {
            crate::text_offsets::normalize_utf8_range(self.code_language_text(), range)
        });
        self.cursor_blink_epoch = Instant::now();
        self.sync_code_highlight();

        let next_language = match &self.record.kind {
            BlockKind::CodeBlock { language } => language.clone(),
            _ => None,
        };
        if old_language != next_language {
            cx.emit(BlockEvent::Changed);
        }
        cx.notify();
    }

    /// 按鼠标位置计算代码语言偏移。
    pub(crate) fn code_language_index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let text = self.code_language_text();
        if text.is_empty() {
            return 0;
        }

        let (Some(bounds), Some(line)) = (
            self.code_language_last_bounds.as_ref(),
            self.code_language_last_layout.as_ref(),
        ) else {
            return 0;
        };
        if position.x <= bounds.left() {
            return 0;
        }
        if position.x >= bounds.right() {
            return text.len();
        }
        crate::text_offsets::floor_char_boundary(
            text,
            line.closest_index_for_x(position.x - bounds.left()),
        )
    }

    /// 重置代码语言输入布局。
    pub(crate) fn reset_code_language_input_layout(&mut self) {
        self.code_language_last_layout = None;
        self.code_language_last_bounds = None;
        self.code_language_is_selecting = false;
    }
}

// 测试观测点：实际执行高亮重算的次数（生产构建不编译）。
#[cfg(test)]
thread_local! {
    static HIGHLIGHT_RECOMPUTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_highlight_recompute() {
    HIGHLIGHT_RECOMPUTES.with(|runs| runs.set(runs.get() + 1));
}

#[cfg(not(test))]
fn count_highlight_recompute() {}

/// 读取并清零高亮重算计数（测试辅助）。
#[cfg(test)]
pub(super) fn take_highlight_recomputes() -> usize {
    HIGHLIGHT_RECOMPUTES.with(|runs| runs.replace(0))
}
