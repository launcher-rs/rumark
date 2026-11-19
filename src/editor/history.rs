//! 撤销历史与选区快照恢复。

use super::*;

impl Editor {
    /// 构造空选区快照。
    pub(super) fn empty_selection_snapshot() -> SelectionUndoSnapshot {
        SelectionUndoSnapshot {
            range: 0..0,
            reversed: false,
        }
    }

    /// 按当前视图模式捕获源码偏移下的选区快照。
    pub(super) fn capture_source_selection_snapshot(&self, cx: &App) -> SelectionUndoSnapshot {
        if let Some(snapshot) = self.cross_block_source_selection_snapshot(cx) {
            return snapshot;
        }

        if self.view_mode == ViewMode::Source {
            return self
                .document
                .first_root()
                .map(|block| {
                    let block_ref = block.read(cx);
                    SelectionUndoSnapshot {
                        range: block_ref.selected_range.clone(),
                        reversed: block_ref.selection_reversed,
                    }
                })
                .unwrap_or_else(Self::empty_selection_snapshot);
        }

        let Some(target) = self.current_edit_target_from_state(cx) else {
            return self.last_selection_snapshot.clone();
        };
        // 借用派生缓存并按实体标识索引，避免每帧为取一个光标偏移就克隆整份映射。
        let derived = self.derived_document(cx);
        let Some(&index) = derived.mapping_index_by_entity.get(&target.entity_id()) else {
            return self.last_selection_snapshot.clone();
        };
        let Some(mapping) = derived.mappings.get(index) else {
            return self.last_selection_snapshot.clone();
        };

        let selected_range = target.read(cx).selected_range.clone();
        let content_range = target
            .read(cx)
            .current_range_to_markdown_range(selected_range);
        let max_offset = mapping.content_to_source.len().saturating_sub(1);
        let start = mapping.full_source_range.start
            + mapping.content_to_source[content_range.start.min(max_offset)];
        let end = mapping.full_source_range.start
            + mapping.content_to_source[content_range.end.min(max_offset)];

        SelectionUndoSnapshot {
            range: start..end,
            reversed: target.read(cx).selection_reversed,
        }
    }

    /// 捕获当前文档的历史记录条目（含源码与选区）。
    pub(super) fn capture_history_entry(&self, kind: UndoKind, cx: &App) -> UndoEntry {
        UndoEntry {
            source_text: self.current_document_source_arc(cx),
            selection: self.capture_source_selection_snapshot(cx),
            timestamp: Instant::now(),
            kind,
        }
    }

    /// 基于上次稳定快照捕获历史记录条目。
    pub(super) fn capture_stable_history_entry(&self, kind: UndoKind) -> UndoEntry {
        UndoEntry {
            source_text: self.last_stable_source_text.clone(),
            selection: self.last_selection_snapshot.clone(),
            timestamp: Instant::now(),
            kind,
        }
    }

    /// 准备一次挂起的撤销捕获（已有捕获或恢复中则跳过）。
    pub(super) fn prepare_undo_capture(&mut self, kind: UndoKind, cx: &mut Context<Self>) {
        if self.history_restore_in_progress || self.pending_undo_capture.is_some() {
            return;
        }
        self.pending_undo_capture = Some(UndoCapture {
            snapshot: self.capture_history_entry(kind, cx),
            block_version: None,
        });
    }

    /// 从稳定快照准备一次挂起的撤销捕获。
    pub(super) fn prepare_undo_capture_from_stable_snapshot(
        &mut self,
        kind: UndoKind,
        block: &Entity<Block>,
        cx: &App,
    ) {
        if self.history_restore_in_progress || self.pending_undo_capture.is_some() {
            return;
        }
        let block_version = Some((block.entity_id(), block.read(cx).record.content_version()));
        self.pending_undo_capture = Some(UndoCapture {
            snapshot: self.capture_stable_history_entry(kind),
            block_version,
        });
    }

    /// 刷新稳定的文档快照（源码文本与选区）。
    pub(super) fn refresh_stable_document_snapshot(&mut self, cx: &App) {
        self.last_selection_snapshot = self.capture_source_selection_snapshot(cx);
        self.last_stable_source_text = self.current_document_source_arc(cx);
        self.stable_snapshot_stale = false;
        self.last_document_edit = None;
    }

    /// 大文档连续输入时暂缓昂贵的全篇快照刷新，输入停顿后再统一计算。
    pub(super) fn should_defer_large_document_refresh(&self) -> bool {
        const LARGE_DOCUMENT_BLOCKS: usize = 256;
        const INPUT_IDLE_DELAY: Duration = Duration::from_millis(300);
        self.document.visible_block_count() >= LARGE_DOCUMENT_BLOCKS
            && self
                .last_document_edit
                .is_some_and(|edited| edited.elapsed() < INPUT_IDLE_DELAY)
    }

    /// 连续输入历史已超出合并窗口时，下一次输入前必须先固化当前文本。
    pub(super) fn text_history_merge_window_elapsed(&self) -> bool {
        self.undo_history.last().is_some_and(|entry| {
            matches!(entry.kind, UndoKind::CoalescibleText)
                && entry.timestamp.elapsed() > Self::UNDO_COALESCE_WINDOW
        })
    }

    /// 收尾挂起的撤销捕获：无变化丢弃，相邻输入按时间窗合并。
    pub(super) fn finalize_pending_undo_capture(&mut self, cx: &mut Context<Self>) {
        if self.history_restore_in_progress {
            self.pending_undo_capture = None;
            return;
        }

        let Some(pending) = self.pending_undo_capture.take() else {
            if self.should_defer_large_document_refresh() {
                self.stable_snapshot_stale = true;
                return;
            }
            self.refresh_stable_document_snapshot(cx);
            return;
        };

        let block_changed = pending.block_version.is_some_and(|(entity_id, version)| {
            self.document
                .block_entity_by_id(entity_id)
                .is_some_and(|block| block.read(cx).record.content_version() != version)
        });
        if matches!(pending.snapshot.kind, UndoKind::CoalescibleText)
            && self.document.visible_block_count() >= 256
            && block_changed
        {
            self.redo_history.clear();
            let should_merge = self.undo_history.last().is_some_and(|entry| {
                matches!(entry.kind, UndoKind::CoalescibleText)
                    && pending
                        .snapshot
                        .timestamp
                        .saturating_duration_since(entry.timestamp)
                        <= Self::UNDO_COALESCE_WINDOW
            });
            if !should_merge {
                self.undo_history.push(pending.snapshot);
                Self::trim_history(
                    &mut self.undo_history,
                    Self::UNDO_LIMIT,
                    Self::HISTORY_BYTES_LIMIT,
                );
            }
            self.stable_snapshot_stale = true;
            return;
        }

        // 直接复用派生缓存的共享文本做比较，避免为「有无变化」再拷贝整篇源码。
        let current_source = self.current_document_source_arc(cx);
        if *current_source == *pending.snapshot.source_text {
            self.refresh_stable_document_snapshot(cx);
            return;
        }

        // 新编辑会使可重做的前进历史失效。
        self.redo_history.clear();

        let should_merge = matches!(pending.snapshot.kind, UndoKind::CoalescibleText)
            && self.undo_history.last().is_some_and(|entry| {
                matches!(entry.kind, UndoKind::CoalescibleText)
                    && pending
                        .snapshot
                        .timestamp
                        .saturating_duration_since(entry.timestamp)
                        <= Self::UNDO_COALESCE_WINDOW
            });
        if !should_merge {
            self.undo_history.push(pending.snapshot);
            Self::trim_history(
                &mut self.undo_history,
                Self::UNDO_LIMIT,
                Self::HISTORY_BYTES_LIMIT,
            );
        }
        self.refresh_stable_document_snapshot(cx);
    }

    /// 从最旧的条目开始裁剪历史：先限条数，再限源码字节总量。
    ///
    /// 撤销/重做回压也会推入条目，只限制 `finalize_pending_undo_capture`
    /// 会让这两条路径无限增长；这里给两个栈统一套用。
    ///
    /// 至少保留最近一条：即使单条超过字节上限也不全部裁掉，否则撤销完全失效。
    fn trim_history(history: &mut Vec<UndoEntry>, max_entries: usize, max_bytes: usize) {
        if history.len() > max_entries {
            let overflow = history.len() - max_entries;
            history.drain(0..overflow);
        }

        let mut total: usize = history.iter().map(|entry| entry.source_text.len()).sum();
        if total <= max_bytes {
            return;
        }
        // 保留最近一条：即使它自身超过字节上限，也好过撤销全失效。
        let max_removable = history.len().saturating_sub(1);
        let mut remove = 0;
        for entry in history.iter().take(max_removable) {
            if total <= max_bytes {
                break;
            }
            total = total.saturating_sub(entry.source_text.len());
            remove += 1;
        }
        history.drain(0..remove);
    }

    /// 在当前视图模式下应用选区快照。
    pub(super) fn apply_selection_snapshot_in_current_mode(
        &mut self,
        snapshot: &SelectionUndoSnapshot,
        cx: &mut Context<Self>,
    ) {
        match self.view_mode {
            ViewMode::Source => {
                let Some(block) = self.document.first_root().cloned() else {
                    return;
                };
                let selected_range = crate::text_offsets::normalize_utf8_range(
                    block.read(cx).rendered_text(),
                    snapshot.range.clone(),
                );
                block.update(cx, move |block, cx| {
                    block.selected_range = selected_range.clone();
                    block.selection_reversed = snapshot.reversed;
                    block.marked_range = None;
                    block.vertical_motion_x = None;
                    block.cursor_blink_epoch = Instant::now();
                    cx.notify();
                });
                self.pending_focus = Some(block.entity_id());
                self.active_entity_id = Some(block.entity_id());
            }
            ViewMode::Rendered => {
                if self.apply_cross_block_selection_snapshot_if_possible(snapshot, cx) {
                    return;
                }

                let mappings = self.build_source_target_mappings(cx);
                let exact_mapping = mappings.iter().find(|mapping| {
                    let contains_start = Self::source_range_contains(
                        &mapping.full_source_range,
                        snapshot.range.start,
                    );
                    let contains_end =
                        Self::source_range_contains(&mapping.full_source_range, snapshot.range.end);
                    if !contains_start || !contains_end {
                        return false;
                    }
                    let local_start = snapshot
                        .range
                        .start
                        .saturating_sub(mapping.full_source_range.start);
                    let local_end = snapshot
                        .range
                        .end
                        .saturating_sub(mapping.full_source_range.start);
                    let content_start = mapping.source_to_content
                        [local_start.min(mapping.source_to_content.len().saturating_sub(1))];
                    let content_end = mapping.source_to_content
                        [local_end.min(mapping.source_to_content.len().saturating_sub(1))];
                    let max_content = mapping.content_to_source.len().saturating_sub(1);
                    mapping.content_to_source[content_start.min(max_content)] == local_start
                        && mapping.content_to_source[content_end.min(max_content)] == local_end
                });

                if let Some(mapping) = exact_mapping {
                    let local_start = snapshot.range.start - mapping.full_source_range.start;
                    let local_end = snapshot.range.end - mapping.full_source_range.start;
                    let content_start = mapping.source_to_content[local_start];
                    let content_end = mapping.source_to_content[local_end];
                    let selected_range = mapping
                        .entity
                        .read(cx)
                        .markdown_range_to_current_range(content_start..content_end);
                    mapping.entity.update(cx, move |block, cx| {
                        block.selected_range = selected_range.clone();
                        block.selection_reversed = snapshot.reversed;
                        block.marked_range = None;
                        block.vertical_motion_x = None;
                        block.cursor_blink_epoch = Instant::now();
                        cx.notify();
                    });
                    self.pending_focus = Some(mapping.entity.entity_id());
                    self.active_entity_id = Some(mapping.entity.entity_id());
                    return;
                }

                let caret_offset = snapshot.range.end;
                let best = mappings.iter().min_by_key(|mapping| {
                    Self::source_offset_distance(&mapping.full_source_range, caret_offset)
                });
                let Some(mapping) = best else {
                    self.pending_focus = self.first_focusable_entity_id(cx);
                    self.active_entity_id = self.pending_focus;
                    return;
                };
                let local_source = if caret_offset <= mapping.full_source_range.start {
                    0
                } else if caret_offset >= mapping.full_source_range.end {
                    mapping.full_source_range.len()
                } else {
                    caret_offset - mapping.full_source_range.start
                };
                let content_offset = mapping.source_to_content
                    [local_source.min(mapping.source_to_content.len().saturating_sub(1))];
                let current_offset = mapping
                    .entity
                    .read(cx)
                    .markdown_offset_to_current_offset(content_offset);
                mapping.entity.update(cx, move |block, cx| {
                    block.assign_collapsed_selection_offset(
                        current_offset,
                        crate::components::CaretAffinity::Default,
                        None,
                    );
                    block.marked_range = None;
                    block.cursor_blink_epoch = Instant::now();
                    cx.notify();
                });
                self.pending_focus = Some(mapping.entity.entity_id());
                self.active_entity_id = Some(mapping.entity.entity_id());
            }
        }
    }

    /// 判断源码区间是否包含指定偏移（含端点）。
    pub(super) fn source_range_contains(range: &std::ops::Range<usize>, offset: usize) -> bool {
        if range.start == range.end {
            offset == range.start
        } else {
            offset >= range.start && offset <= range.end
        }
    }

    /// 计算偏移到源码区间的距离（区间内为零）。
    pub(super) fn source_offset_distance(range: &std::ops::Range<usize>, offset: usize) -> usize {
        if Self::source_range_contains(range, offset) {
            0
        } else if offset < range.start {
            range.start - offset
        } else {
            offset.saturating_sub(range.end)
        }
    }

    /// 按历史条目重建文档块并恢复选区与滚动状态。
    pub(super) fn restore_history_entry(&mut self, entry: &UndoEntry, cx: &mut Context<Self>) {
        match self.view_mode {
            ViewMode::Rendered => {
                let mut roots =
                    Self::build_root_blocks_from_markdown(cx, entry.source_text.as_ref());
                if roots.is_empty() {
                    roots.push(Self::new_block(cx, BlockRecord::paragraph(String::new())));
                }
                self.document.replace_roots(roots, cx);
                self.rebuild_table_runtimes(cx);
                self.rebuild_image_runtimes(cx);
            }
            ViewMode::Source => {
                let block =
                    Self::new_block(cx, BlockRecord::paragraph(entry.source_text.to_string()));
                block.update(cx, |block, _cx| block.enable_source_document_mode());
                self.document.replace_roots(vec![block], cx);
                self.table_cells.clear();
            }
        }

        self.apply_selection_snapshot_in_current_mode(&entry.selection, cx);
        self.pending_scroll_active_block_into_view = true;
        self.pending_scroll_recheck_after_layout = true;
        self.last_scroll_viewport_size = None;
        self.refresh_stable_document_snapshot(cx);
    }

    /// 渲染模式下按当前源码重新规范化引用块结构。
    pub(super) fn normalize_rendered_quote_structure(&mut self, cx: &mut Context<Self>) {
        if self.view_mode != ViewMode::Rendered {
            return;
        }

        let selection_snapshot = self.capture_source_selection_snapshot(cx);
        let source = self.document.markdown_text(cx);
        let mut roots = Self::build_root_blocks_from_markdown(cx, &source);
        if roots.is_empty() {
            roots.push(Self::new_block(cx, BlockRecord::paragraph(String::new())));
        }
        self.document.replace_roots(roots, cx);
        self.rebuild_table_runtimes(cx);
        self.rebuild_image_runtimes(cx);
        self.apply_selection_snapshot_in_current_mode(&selection_snapshot, cx);
        self.pending_scroll_active_block_into_view = true;
        self.pending_scroll_recheck_after_layout = true;
        self.last_scroll_viewport_size = None;
    }

    /// 执行撤销：恢复上条历史并把当前状态压入重做栈。
    pub(super) fn undo_document(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.undo_history.pop() else {
            return;
        };

        // 记录当前文档快照，供重做时向前恢复。
        let current = self.capture_history_entry(UndoKind::NonCoalescible, cx);
        self.pending_undo_capture = None;
        self.history_restore_in_progress = true;
        self.clear_cross_block_selection(cx);
        self.restore_history_entry(&entry, cx);
        self.history_restore_in_progress = false;
        self.redo_history.push(current);
        Self::trim_history(
            &mut self.redo_history,
            Self::UNDO_LIMIT,
            Self::HISTORY_BYTES_LIMIT,
        );
        self.mark_dirty(cx);
        self.sync_table_axis_visuals(cx);
        self.dismiss_contextual_overlays(cx);
        cx.notify();
    }

    /// 执行重做：恢复下条历史并把当前状态压回撤销栈。
    pub(super) fn redo_document(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.redo_history.pop() else {
            return;
        };

        // 记录当前文档快照，供撤销时回退。
        let current = self.capture_history_entry(UndoKind::NonCoalescible, cx);
        self.pending_undo_capture = None;
        self.history_restore_in_progress = true;
        self.clear_cross_block_selection(cx);
        self.restore_history_entry(&entry, cx);
        self.history_restore_in_progress = false;
        self.undo_history.push(current);
        Self::trim_history(
            &mut self.undo_history,
            Self::UNDO_LIMIT,
            Self::HISTORY_BYTES_LIMIT,
        );
        self.mark_dirty(cx);
        self.sync_table_axis_visuals(cx);
        self.dismiss_contextual_overlays(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    // 不能用 `use super::*`：会把 `rgpui::*` 的 `test` 属性宏带进来遮蔽内置 `#[test]`。
    use super::{Editor, UndoEntry, UndoKind};

    /// 构造源码区间（测试辅助）。
    fn range(start: usize, end: usize) -> std::ops::Range<usize> {
        start..end
    }

    #[test]
    fn source_range_contains_includes_both_endpoints() {
        assert!(Editor::source_range_contains(&range(4, 9), 4));
        assert!(Editor::source_range_contains(&range(4, 9), 9));
        assert!(Editor::source_range_contains(&range(4, 9), 6));
        assert!(!Editor::source_range_contains(&range(4, 9), 3));
        assert!(!Editor::source_range_contains(&range(4, 9), 10));
    }

    #[test]
    fn empty_source_range_only_contains_its_own_offset() {
        assert!(Editor::source_range_contains(&range(5, 5), 5));
        assert!(!Editor::source_range_contains(&range(5, 5), 4));
        assert!(!Editor::source_range_contains(&range(5, 5), 6));
    }

    #[test]
    fn source_offset_distance_is_zero_inside_and_grows_outside() {
        assert_eq!(Editor::source_offset_distance(&range(4, 9), 4), 0);
        assert_eq!(Editor::source_offset_distance(&range(4, 9), 7), 0);
        assert_eq!(Editor::source_offset_distance(&range(4, 9), 9), 0);
        assert_eq!(Editor::source_offset_distance(&range(4, 9), 1), 3);
        assert_eq!(Editor::source_offset_distance(&range(4, 9), 12), 3);
        assert_eq!(Editor::source_offset_distance(&range(5, 5), 5), 0);
        assert_eq!(Editor::source_offset_distance(&range(5, 5), 8), 3);
    }

    #[test]
    fn empty_selection_snapshot_is_collapsed_at_origin() {
        let snapshot = Editor::empty_selection_snapshot();
        assert_eq!(snapshot.range, 0..0);
        assert!(!snapshot.reversed);
    }

    /// 构造一条历史记录（测试辅助）。
    fn history_entry(text: &str) -> UndoEntry {
        UndoEntry {
            source_text: Arc::from(text),
            selection: Editor::empty_selection_snapshot(),
            timestamp: Instant::now(),
            kind: UndoKind::NonCoalescible,
        }
    }

    /// 收集历史栈中的源码文本，便于断言裁剪结果。
    fn history_texts(history: &[UndoEntry]) -> Vec<&str> {
        history
            .iter()
            .map(|entry| entry.source_text.as_ref())
            .collect()
    }

    #[test]
    fn trim_history_enforces_entry_limit_and_keeps_newest() {
        let mut history: Vec<UndoEntry> = ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(history_entry)
            .collect();

        Editor::trim_history(&mut history, 3, usize::MAX);

        assert_eq!(history_texts(&history), vec!["c", "d", "e"]);
    }

    #[test]
    fn trim_history_enforces_byte_limit_and_keeps_newest() {
        // 5 条 × 10 字节 = 50，上限 25 时须从最旧开始丢弃 3 条。
        let mut history: Vec<UndoEntry> = (0..5)
            .map(|index| history_entry(&format!("{:0>10}", index)))
            .collect();

        Editor::trim_history(&mut history, usize::MAX, 25);

        assert_eq!(history_texts(&history), vec!["0000000003", "0000000004"]);
        assert!(history.iter().map(|e| e.source_text.len()).sum::<usize>() <= 25);
    }

    #[test]
    fn trim_history_applies_both_limits_in_order() {
        // 先按条数裁剪，再按字节裁剪，两个约束都要满足。
        let mut history: Vec<UndoEntry> = (0..6)
            .map(|index| history_entry(&format!("{:0>10}", index)))
            .collect();

        Editor::trim_history(&mut history, 4, 25);

        assert!(history.len() <= 4);
        assert!(history.iter().map(|e| e.source_text.len()).sum::<usize>() <= 25);
        assert_eq!(history_texts(&history), vec!["0000000004", "0000000005"]);
    }

    #[test]
    fn cloned_history_entries_share_source_text_allocation() {
        let entry = history_entry("# 共享的整篇源码快照");
        let cloned = entry.clone();

        assert!(Arc::ptr_eq(&entry.source_text, &cloned.source_text));
    }

    #[test]
    fn trim_history_keeps_at_least_one_entry_even_if_it_exceeds_byte_limit() {
        // 单条 100 字节，字节上限 50：不能全部裁掉，至少保留最近一条。
        let huge = "x".repeat(100);
        let mut history: Vec<UndoEntry> = vec![history_entry(&huge)];

        Editor::trim_history(&mut history, usize::MAX, 50);

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].source_text.as_ref(), huge.as_str());
    }

    #[test]
    fn trim_history_with_mixed_sizes_keeps_newest_over_limit() {
        // 3 条：小、小、超大。字节上限 30。前两条被裁掉，第三条虽超限但保留。
        let huge = "z".repeat(100);
        let mut history: Vec<UndoEntry> = [
            history_entry("aaaa"),
            history_entry("bbbb"),
            history_entry(&huge),
        ]
        .into_iter()
        .collect();

        Editor::trim_history(&mut history, usize::MAX, 30);

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].source_text.as_ref(), huge.as_str());
    }
}
