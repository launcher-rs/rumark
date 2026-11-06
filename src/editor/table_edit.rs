//! 原生表格运行时安装与表格编辑操作。

use super::*;
use crate::components::{
    TableAlignColumnCenter, TableAlignColumnLeft, TableAlignColumnRight, TableDeleteColumn,
    TableDeleteRow, TableInsertColumnLeft, TableInsertColumnRight, TableInsertRowAbove,
    TableInsertRowBelow, TableMoveColumnLeft, TableMoveColumnRight, TableMoveRowDown,
    TableMoveRowUp, ToggleTableHeaders,
};

impl Editor {
    /// 新建表格块实体。
    pub(crate) fn new_table_block(cx: &mut Context<Self>, table: TableModel) -> Entity<Block> {
        Self::new_block(cx, BlockRecord::table(table))
    }

    /// 为表格块安装单元格运行时（表头与正文单元格实体）。
    pub(super) fn install_table_runtime_for_block(
        &mut self,
        table_block: &Entity<Block>,
        table: &TableModel,
        cx: &mut Context<Self>,
    ) {
        let header = table
            .header
            .iter()
            .cloned()
            .enumerate()
            .map(|(column, title)| {
                let alignment = table
                    .alignments
                    .get(column)
                    .copied()
                    .unwrap_or(TableColumnAlignment::Default);
                let position = CellPosition { row: 0, column };
                let cell = Self::new_table_cell_block(cx, title, position, alignment);
                self.table_cells.insert(
                    cell.entity_id(),
                    CellBinding {
                        table_block: table_block.clone(),
                        cell: cell.clone(),
                        position,
                    },
                );
                cell
            })
            .collect::<Vec<_>>();

        let rows = table
            .rows
            .iter()
            .cloned()
            .enumerate()
            .map(|(body_row_index, row)| {
                row.into_iter()
                    .enumerate()
                    .map(|(column, title)| {
                        let alignment = table
                            .alignments
                            .get(column)
                            .copied()
                            .unwrap_or(TableColumnAlignment::Default);
                        let position = CellPosition {
                            row: body_row_index + 1,
                            column,
                        };
                        let cell = Self::new_table_cell_block(cx, title, position, alignment);
                        self.table_cells.insert(
                            cell.entity_id(),
                            CellBinding {
                                table_block: table_block.clone(),
                                cell: cell.clone(),
                                position,
                            },
                        );
                        cell
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        table_block.update(cx, {
            let runtime = TableRuntime { header, rows };
            move |block, _cx| block.set_table_runtime(runtime.clone())
        });
    }

    /// 重建所有可见表格的运行时并同步行列高亮。
    pub(super) fn rebuild_table_runtimes(&mut self, cx: &mut Context<Self>) {
        self.table_cells.clear();
        self.table_axis_preview = None;
        let visible = self.document.visible_blocks().to_vec();
        for block in &visible {
            block
                .entity
                .update(cx, |block, _cx| block.clear_table_runtime());
        }
        for visible in visible {
            let Some(table) = visible.entity.read(cx).record.table.clone() else {
                continue;
            };
            if visible.entity.read(cx).kind() == BlockKind::Table {
                self.install_table_runtime_for_block(&visible.entity, &table, cx);
            }
        }
        self.rebuild_image_runtimes(cx);
        self.sync_table_axis_visuals(cx);
    }

    /// 把单元格运行时的编辑内容回写到表格记录。
    pub(super) fn sync_table_record_from_runtime(
        &mut self,
        table_block: &Entity<Block>,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = table_block.read(cx).table_runtime.clone() else {
            return;
        };
        let alignments = table_block
            .read(cx)
            .record
            .table
            .as_ref()
            .map(|table| table.alignments.clone())
            .unwrap_or_default();
        let header = runtime
            .header
            .iter()
            .map(|cell| cell.read(cx).record.title.clone())
            .collect::<Vec<_>>();
        let rows = runtime
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.read(cx).record.title.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(TableModel {
                header,
                rows,
                alignments,
            });
        });
    }

    /// 在表格末尾追加一列并聚焦新表头单元格。
    pub(super) fn append_table_column(
        &mut self,
        table_block: &Entity<Block>,
        cx: &mut Context<Self>,
    ) {
        self.sync_table_record_from_runtime(table_block, cx);

        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        let alignment = table
            .alignments
            .last()
            .copied()
            .unwrap_or(TableColumnAlignment::Default);
        table.add_column(alignment);

        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        if let Some(cell) = table_block
            .read(cx)
            .table_runtime
            .as_ref()
            .and_then(|runtime| runtime.header.last())
        {
            self.focus_block(cell.entity_id());
        }
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 在表格末尾追加一行并聚焦新行首个单元格。
    pub(super) fn append_table_row(&mut self, table_block: &Entity<Block>, cx: &mut Context<Self>) {
        self.sync_table_record_from_runtime(table_block, cx);

        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        table.add_row();

        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        if let Some(cell) = table_block
            .read(cx)
            .table_runtime
            .as_ref()
            .and_then(|runtime| runtime.rows.last())
            .and_then(|row| row.first())
        {
            self.focus_block(cell.entity_id());
        }
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 在可视行处插入一空行并聚焦新行同列单元格（`0` 为表头上方）。
    pub(super) fn insert_table_row_at(
        &mut self,
        table_block: &Entity<Block>,
        visual_row: usize,
        column: usize,
        cx: &mut Context<Self>,
    ) {
        self.sync_table_record_from_runtime(table_block, cx);
        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        table.insert_row_at_visual(visual_row);
        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        self.focus_table_cell_position(
            table_block,
            CellPosition {
                row: visual_row,
                column,
            },
            cx,
        );
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 在指定列左侧插入一空列并聚焦同行新列单元格。
    pub(super) fn insert_table_column_at(
        &mut self,
        table_block: &Entity<Block>,
        visual_row: usize,
        column: usize,
        cx: &mut Context<Self>,
    ) {
        self.sync_table_record_from_runtime(table_block, cx);
        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        let alignment = table
            .alignments
            .get(column)
            .copied()
            .unwrap_or(TableColumnAlignment::Default);
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        table.insert_column_at(column, alignment);
        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        self.focus_table_cell_position(
            table_block,
            CellPosition {
                row: visual_row,
                column,
            },
            cx,
        );
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 取当前聚焦单元格的表格绑定（非单元格聚焦时返回空，快捷键静默忽略）。
    fn focused_table_cell_binding(&self, window: &Window, cx: &App) -> Option<CellBinding> {
        let id = self
            .focused_edit_target_entity_id(window, cx)
            .or(self.active_entity_id)?;
        self.table_cell_binding(id)
    }

    /// 快捷键守卫：弹窗打开时表格快捷键忽略，避免与输入框抢键。
    fn table_shortcut_guarded(&self) -> bool {
        self.link_dialog.is_none() && self.table_insert_dialog.is_none() && self.find_bar.is_none()
    }

    /// 快捷键动作：在聚焦行上方插入空行。
    pub(super) fn on_table_insert_row_above_action(
        &mut self,
        _: &TableInsertRowAbove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        let position = binding.position;
        self.insert_table_row_at(&binding.table_block, position.row, position.column, cx);
    }

    /// 快捷键动作：在聚焦行下方插入空行。
    pub(super) fn on_table_insert_row_below_action(
        &mut self,
        _: &TableInsertRowBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        let position = binding.position;
        self.insert_table_row_at(&binding.table_block, position.row + 1, position.column, cx);
    }

    /// 快捷键动作：删除聚焦行（表头语义与右键菜单一致）。
    pub(super) fn on_table_delete_row_action(
        &mut self,
        _: &TableDeleteRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        let table_block = binding.table_block.clone();
        if binding.position.is_header() {
            let row_count = table_block
                .read(cx)
                .record
                .table
                .as_ref()
                .map(|table| table.rows.len());
            if row_count == Some(0) {
                self.remove_table_block(&table_block, cx);
            } else {
                self.delete_table_header_row(&table_block, cx);
            }
            return;
        }
        let Some(body_index) = binding.position.body_row_index() else {
            return;
        };
        self.delete_table_row(&table_block, body_index, cx);
    }

    /// 快捷键动作：在聚焦列左侧插入空列。
    pub(super) fn on_table_insert_column_left_action(
        &mut self,
        _: &TableInsertColumnLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        let position = binding.position;
        self.insert_table_column_at(&binding.table_block, position.row, position.column, cx);
    }

    /// 快捷键动作：在聚焦列右侧插入空列。
    pub(super) fn on_table_insert_column_right_action(
        &mut self,
        _: &TableInsertColumnRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        let position = binding.position;
        self.insert_table_column_at(&binding.table_block, position.row, position.column + 1, cx);
    }

    /// 快捷键动作：删除聚焦列（唯一列时移除整个表格，与右键菜单一致）。
    pub(super) fn on_table_delete_column_action(
        &mut self,
        _: &TableDeleteColumn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        let table_block = binding.table_block.clone();
        let column_count = table_block
            .read(cx)
            .record
            .table
            .as_ref()
            .map(|table| table.column_count());
        if column_count == Some(1) {
            self.remove_table_block(&table_block, cx);
        } else {
            self.delete_table_column(&table_block, binding.position.column, cx);
        }
    }

    /// 快捷键动作：聚焦行上移（越界时忽略）。
    pub(super) fn on_table_move_row_up_action(
        &mut self,
        _: &TableMoveRowUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        self.move_table_row(&binding.table_block, binding.position.row, -1, cx);
    }

    /// 快捷键动作：聚焦行下移（越界时忽略）。
    pub(super) fn on_table_move_row_down_action(
        &mut self,
        _: &TableMoveRowDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        self.move_table_row(&binding.table_block, binding.position.row, 1, cx);
    }

    /// 快捷键动作：聚焦列左移（越界时忽略）。
    pub(super) fn on_table_move_column_left_action(
        &mut self,
        _: &TableMoveColumnLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        self.move_table_column(&binding.table_block, binding.position.column, -1, cx);
    }

    /// 快捷键动作：聚焦列右移（越界时忽略）。
    pub(super) fn on_table_move_column_right_action(
        &mut self,
        _: &TableMoveColumnRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        self.move_table_column(&binding.table_block, binding.position.column, 1, cx);
    }

    /// 快捷键对齐聚焦列：弹窗打开时忽略，非单元格聚焦静默忽略。
    fn align_focused_table_column(
        &mut self,
        alignment: TableColumnAlignment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.table_shortcut_guarded() {
            return;
        }
        let Some(binding) = self.focused_table_cell_binding(window, cx) else {
            return;
        };
        self.set_table_column_alignment(
            &binding.table_block,
            binding.position.column,
            alignment,
            cx,
        );
    }

    /// 快捷键动作：聚焦列左对齐（即无标记默认对齐）。
    pub(super) fn on_table_align_column_left_action(
        &mut self,
        _: &TableAlignColumnLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.align_focused_table_column(TableColumnAlignment::Default, window, cx);
    }

    /// 快捷键动作：聚焦列居中对齐。
    pub(super) fn on_table_align_column_center_action(
        &mut self,
        _: &TableAlignColumnCenter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.align_focused_table_column(TableColumnAlignment::Center, window, cx);
    }

    /// 快捷键动作：聚焦列右对齐。
    pub(super) fn on_table_align_column_right_action(
        &mut self,
        _: &TableAlignColumnRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.align_focused_table_column(TableColumnAlignment::Right, window, cx);
    }

    /// 快捷键动作：切换表格首行表头显示（全局生效，持久化到偏好）。
    pub(super) fn on_toggle_table_headers_action(
        &mut self,
        _: &ToggleTableHeaders,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = !crate::config::EditorSettings::table_headers_visible(cx);
        crate::config::EditorSettings::set_table_headers_visible(cx, next);
        cx.notify();
    }

    /// 悬停时预览行列高亮，移出且仍持有预览时清除。
    pub(super) fn preview_table_axis(
        &mut self,
        table_block_id: EntityId,
        kind: TableAxisKind,
        index: usize,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let marker = AxisSelection {
            table_block_id,
            kind,
            index,
        };
        if hovered {
            self.set_table_axis_preview(Some(marker), cx);
        } else if self.table_axis_preview == Some(marker) {
            // 仅当移出的仍是当前预览持有者时才清除。相邻手柄共用一个
            // 预览槽，移出事件可能晚于下一个手柄的移入到达；无条件清除
            // 会抹掉指针刚移到的高亮。
            self.set_table_axis_preview(None, cx);
        }
    }

    /// 选中指定行列（同时设为预览）。
    pub(super) fn select_table_axis(
        &mut self,
        table_block_id: EntityId,
        kind: TableAxisKind,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let selection = AxisSelection {
            table_block_id,
            kind,
            index,
        };
        self.set_table_axis_preview(Some(selection), cx);
        self.set_table_axis_selection(Some(selection), cx);
    }

    /// 设置指定列的对齐方式并聚焦其表头单元格。
    pub(super) fn set_table_column_alignment(
        &mut self,
        table_block: &Entity<Block>,
        column: usize,
        alignment: TableColumnAlignment,
        cx: &mut Context<Self>,
    ) {
        self.sync_table_record_from_runtime(table_block, cx);
        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        table.assign_column_alignment(column, alignment);
        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        let selection = AxisSelection {
            table_block_id: table_block.entity_id(),
            kind: TableAxisKind::Column,
            index: column,
        };
        self.set_table_axis_selection(Some(selection), cx);
        self.focus_table_cell_position(table_block, CellPosition { row: 0, column }, cx);
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 按可视行号移动表格行（含表头行）。
    pub(super) fn move_table_row(
        &mut self,
        table_block: &Entity<Block>,
        visual_row: usize,
        delta: i32,
        cx: &mut Context<Self>,
    ) {
        self.sync_table_record_from_runtime(table_block, cx);
        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        let next_row = if delta < 0 {
            visual_row.checked_sub(delta.unsigned_abs() as usize)
        } else {
            visual_row.checked_add(delta as usize)
        };
        let Some(next_row) = next_row else {
            return;
        };
        // 可视行含表头（序号 0）与全部正文行，故末尾合法下标为行数。
        if next_row > table.rows.len() {
            return;
        }
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        table.exchange_visual_rows(visual_row, next_row);
        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        let selection = AxisSelection {
            table_block_id: table_block.entity_id(),
            kind: TableAxisKind::Row,
            index: next_row,
        };
        self.set_table_axis_selection(Some(selection), cx);
        self.focus_table_cell_position(
            table_block,
            CellPosition {
                row: next_row,
                column: 0,
            },
            cx,
        );
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 左右移动表格列并聚焦移动后的表头单元格。
    pub(super) fn move_table_column(
        &mut self,
        table_block: &Entity<Block>,
        column: usize,
        delta: i32,
        cx: &mut Context<Self>,
    ) {
        self.sync_table_record_from_runtime(table_block, cx);
        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        let next_column = if delta < 0 {
            column.checked_sub(delta.unsigned_abs() as usize)
        } else {
            column.checked_add(delta as usize)
        };
        let Some(next_column) = next_column else {
            return;
        };
        if next_column >= table.column_count() {
            return;
        }
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        table.exchange_columns(column, next_column);
        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        let selection = AxisSelection {
            table_block_id: table_block.entity_id(),
            kind: TableAxisKind::Column,
            index: next_column,
        };
        self.set_table_axis_selection(Some(selection), cx);
        self.focus_table_cell_position(
            table_block,
            CellPosition {
                row: 0,
                column: next_column,
            },
            cx,
        );
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 删除指定正文行并把焦点移到相邻行首个单元格。
    pub(super) fn delete_table_row(
        &mut self,
        table_block: &Entity<Block>,
        row_index: usize,
        cx: &mut Context<Self>,
    ) {
        self.sync_table_record_from_runtime(table_block, cx);
        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        if row_index >= table.rows.len() {
            return;
        }
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        table.delete_body_row(row_index);
        let remaining_body_rows = table.rows.len();
        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        // 行选区按可视下标寻址：首个正文行为 1（表头为 0）。无正文行时
        // 回落到表头，保证焦点落在仍存在的单元格上。
        let focus_visual_row = if remaining_body_rows == 0 {
            0
        } else {
            row_index.min(remaining_body_rows - 1) + 1
        };
        if remaining_body_rows == 0 {
            self.clear_table_axis_selection(cx);
        } else {
            self.set_table_axis_selection(
                Some(AxisSelection {
                    table_block_id: table_block.entity_id(),
                    kind: TableAxisKind::Row,
                    index: focus_visual_row,
                }),
                cx,
            );
        }
        self.focus_table_cell_position(
            table_block,
            CellPosition {
                row: focus_visual_row,
                column: 0,
            },
            cx,
        );
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 删除表头行：首个正文行上提为新表头。
    pub(super) fn delete_table_header_row(
        &mut self,
        table_block: &Entity<Block>,
        cx: &mut Context<Self>,
    ) {
        self.sync_table_record_from_runtime(table_block, cx);
        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        // 首个正文行将上提为表头，因此至少要有一行正文才能删表头。
        if table.rows.is_empty() {
            return;
        }
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        table.delete_header_row();
        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        self.clear_table_axis_selection(cx);
        self.focus_table_cell_position(table_block, CellPosition { row: 0, column: 0 }, cx);
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 删除指定列并把焦点移到相邻列的表头单元格。
    pub(super) fn delete_table_column(
        &mut self,
        table_block: &Entity<Block>,
        column: usize,
        cx: &mut Context<Self>,
    ) {
        self.sync_table_record_from_runtime(table_block, cx);
        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return;
        };
        if table.column_count() <= 1 || column >= table.column_count() {
            return;
        }
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        table.remove_column(column);
        let focus_column = column.min(table.column_count().saturating_sub(1));
        table_block.update(cx, move |block, _cx| {
            block.record.table = Some(table.clone());
        });
        self.rebuild_table_runtimes(cx);
        let selection = AxisSelection {
            table_block_id: table_block.entity_id(),
            kind: TableAxisKind::Column,
            index: focus_column,
        };
        self.set_table_axis_selection(Some(selection), cx);
        self.focus_table_cell_position(
            table_block,
            CellPosition {
                row: 0,
                column: focus_column,
            },
            cx,
        );
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 整体移除表格块，用空段落占位以承接光标。
    ///
    /// 删除最后一行或最后一列导致表格清空时使用。
    pub(super) fn remove_table_block(
        &mut self,
        table_block: &Entity<Block>,
        cx: &mut Context<Self>,
    ) {
        let Some(location) = self.document.find_block_location(table_block.entity_id()) else {
            return;
        };
        let started_local_capture = if self.pending_undo_capture.is_none() {
            self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
            true
        } else {
            false
        };
        // 先在表格后插入替代段落再删表，文档不会瞬间变空。
        let paragraph = Self::new_block(cx, BlockRecord::paragraph(String::new()));
        self.document.insert_blocks_at(
            location.parent.clone(),
            location.index + 1,
            vec![paragraph.clone()],
            cx,
        );
        let table_id = table_block.entity_id();
        self.document.with_structure_mutation(cx, |document, cx| {
            let _ = document.remove_block_by_id_raw(table_id, cx);
        });
        self.rebuild_table_runtimes(cx);
        self.clear_table_axis_selection(cx);
        self.focus_block(paragraph.entity_id());
        self.mark_dirty(cx);
        self.request_active_block_scroll_into_view(cx);
        if started_local_capture {
            self.finalize_pending_undo_capture(cx);
        }
        cx.notify();
    }

    /// 由行列选区构造行列标记。
    pub(super) fn table_axis_marker(selection: AxisSelection) -> AxisMarker {
        AxisMarker {
            kind: selection.kind,
            index: selection.index,
        }
    }

    /// 清除行列悬停预览并同步高亮。
    pub(super) fn clear_table_axis_preview(&mut self, cx: &mut Context<Self>) {
        if self.table_axis_preview.take().is_some() {
            self.sync_table_axis_visuals(cx);
        }
    }

    /// 清除行列选中并同步高亮。
    pub(super) fn clear_table_axis_selection(&mut self, cx: &mut Context<Self>) {
        if self.table_axis_selection.take().is_some() {
            self.sync_table_axis_visuals(cx);
        }
    }

    /// 设置行列悬停预览（变化时同步高亮）。
    pub(super) fn set_table_axis_preview(
        &mut self,
        preview: Option<AxisSelection>,
        cx: &mut Context<Self>,
    ) {
        if self.table_axis_preview != preview {
            self.table_axis_preview = preview;
            self.sync_table_axis_visuals(cx);
        }
    }

    /// 设置行列选中（变化时同步高亮）。
    pub(super) fn set_table_axis_selection(
        &mut self,
        selection: Option<AxisSelection>,
        cx: &mut Context<Self>,
    ) {
        if self.table_axis_selection != selection {
            self.table_axis_selection = selection;
            self.sync_table_axis_visuals(cx);
        }
    }

    /// 校验行列选区是否仍指向有效行列。
    pub(super) fn table_axis_selection_valid(&self, selection: AxisSelection, cx: &App) -> bool {
        let Some(table_block) = self.table_block_by_id(selection.table_block_id, cx) else {
            return false;
        };
        let Some(runtime) = table_block.read(cx).table_runtime.as_ref() else {
            return false;
        };
        match selection.kind {
            TableAxisKind::Column => selection.index < runtime.header.len(),
            // 可视行号：0 为表头，1..=行数为正文行。
            TableAxisKind::Row => selection.index <= runtime.rows.len(),
        }
    }

    /// 规范化行列状态：丢弃指向已删除行列的选中与预览。
    pub(super) fn normalize_table_axis_state(&mut self, cx: &mut Context<Self>) {
        if let Some(selection) = self.table_axis_selection
            && !self.table_axis_selection_valid(selection, cx)
        {
            self.table_axis_selection = None;
        }
        if let Some(preview) = self.table_axis_preview
            && !self.table_axis_selection_valid(preview, cx)
        {
            self.table_axis_preview = None;
        }
    }

    /// 同步所有可见表格的行列选中与预览高亮。
    pub(super) fn sync_table_axis_visuals(&mut self, cx: &mut Context<Self>) {
        self.normalize_table_axis_state(cx);

        let visible_tables = self
            .document
            .visible_blocks()
            .iter()
            .filter(|visible| visible.entity.read(cx).kind() == BlockKind::Table)
            .map(|visible| visible.entity.clone())
            .collect::<Vec<_>>();

        for table_block in &visible_tables {
            let block_id = table_block.entity_id();
            let preview_marker = self
                .table_axis_preview
                .filter(|selection| selection.table_block_id == block_id)
                .map(Self::table_axis_marker);
            let selected_marker = self
                .table_axis_selection
                .filter(|selection| selection.table_block_id == block_id)
                .map(Self::table_axis_marker);

            table_block.update(cx, move |block, cx| {
                block.set_table_axis_visual_state(preview_marker, selected_marker);
                cx.notify();
            });

            let Some(runtime) = table_block.read(cx).table_runtime.clone() else {
                continue;
            };

            let selected = self
                .table_axis_selection
                .filter(|selection| selection.table_block_id == block_id);
            let preview = self
                .table_axis_preview
                .filter(|selection| selection.table_block_id == block_id);

            // `row` 为可视行号：0 为表头，后续 1.. 为正文行，与行选区寻址一致。
            let mut apply_highlight = |cell: &Entity<Block>, row: usize, column: usize| {
                let highlight = if selected.is_some_and(|selection| match selection.kind {
                    TableAxisKind::Column => selection.index == column,
                    TableAxisKind::Row => selection.index == row,
                }) {
                    TableAxisHighlight::Selected
                } else if preview.is_some_and(|selection| match selection.kind {
                    TableAxisKind::Column => selection.index == column,
                    TableAxisKind::Row => selection.index == row,
                }) {
                    TableAxisHighlight::Preview
                } else {
                    TableAxisHighlight::None
                };

                cell.update(cx, move |block, cx| {
                    block.set_table_axis_highlight(highlight);
                    cx.notify();
                });
            };

            for (column, cell) in runtime.header.iter().enumerate() {
                apply_highlight(cell, 0, column);
            }
            for (body_row_index, row) in runtime.rows.iter().enumerate() {
                for (column, cell) in row.iter().enumerate() {
                    apply_highlight(cell, body_row_index + 1, column);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rgpui::{App, AppContext, Entity, TestAppContext};

    use super::Editor;
    use crate::components::{Block, BlockKind, TableAxisKind, TableColumnAlignment};
    use crate::editor::AxisSelection;
    use crate::i18n::I18nManager;

    /// 表格示例文档：两列三行。
    const SAMPLE: &str = "| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\n";

    /// 初始化测试应用并返回承载表格块的编辑器（测试辅助）。
    fn editor_with_table(cx: &mut TestAppContext) -> Entity<Editor> {
        cx.update(|cx| {
            I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
        let (editor, _cx) =
            cx.add_window_view(|_window, cx| Editor::from_markdown(cx, SAMPLE.to_string(), None));
        editor
    }

    /// 取文档里的表格块实体（测试辅助）。
    fn table_block(editor: &Editor, cx: &App) -> Entity<Block> {
        editor
            .document
            .visible_blocks()
            .iter()
            .find(|visible| visible.entity.read(cx).kind() == BlockKind::Table)
            .map(|visible| visible.entity.clone())
            .expect("document should hold a table block")
    }

    /// 读回文档源码（测试辅助）。
    fn source(editor: &Entity<Editor>, cx: &mut TestAppContext) -> String {
        editor.update(cx, |editor, cx| editor.document.markdown_text(cx))
    }

    /// 去掉首尾竖线后按单元格切分一行（测试辅助）。
    fn cells(line: &str) -> Vec<String> {
        line.trim()
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect()
    }

    #[rgpui::test]
    async fn axis_marker_carries_selection_kind_and_index(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let table_block_id = cx.new(|_cx| 0usize).entity_id();
            let selection = AxisSelection {
                table_block_id,
                kind: TableAxisKind::Row,
                index: 2,
            };
            let marker = Editor::table_axis_marker(selection);

            assert_eq!(marker.kind, TableAxisKind::Row);
            assert_eq!(marker.index, 2);
        });
    }

    #[rgpui::test]
    async fn append_table_column_widens_header_delimiter_and_every_body_row(
        cx: &mut TestAppContext,
    ) {
        let editor = editor_with_table(cx);
        editor.update(cx, |editor, cx| {
            let table = table_block(editor, cx);
            editor.append_table_column(&table, cx);
        });

        let text = source(&editor, cx);
        let lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
        assert_eq!(lines.len(), 4, "text: {text:?}");
        for line in &lines {
            assert_eq!(cells(line).len(), 3, "line: {line:?}");
        }
        assert_eq!(cells(&lines[0]), ["a", "b", ""]);
        assert_eq!(cells(&lines[2]), ["1", "2", ""]);
    }

    #[rgpui::test]
    async fn insert_table_row_at_adds_a_blank_visual_row(cx: &mut TestAppContext) {
        let editor = editor_with_table(cx);
        // 可视行 1 是首个正文行之前，插入后新行占据该位置。
        editor.update(cx, |editor, cx| {
            let table = table_block(editor, cx);
            editor.insert_table_row_at(&table, 1, 0, cx);
        });

        let text = source(&editor, cx);
        let lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
        assert_eq!(lines.len(), 5, "text: {text:?}");
        assert!(
            cells(&lines[2]).iter().all(|cell| cell.is_empty()),
            "inserted row should be blank: {:?}",
            lines[2]
        );
        assert_eq!(cells(&lines[3]), ["1", "2"]);
        assert_eq!(cells(&lines[4]), ["3", "4"]);
    }

    #[rgpui::test]
    async fn delete_table_row_removes_only_the_target_body_row(cx: &mut TestAppContext) {
        let editor = editor_with_table(cx);
        editor.update(cx, |editor, cx| {
            let table = table_block(editor, cx);
            editor.delete_table_row(&table, 0, cx);
        });

        let text = source(&editor, cx);
        let lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
        assert_eq!(lines.len(), 3, "text: {text:?}");
        assert_eq!(cells(&lines[0]), ["a", "b"]);
        assert_eq!(cells(&lines[2]), ["3", "4"]);
    }

    #[rgpui::test]
    async fn move_table_row_down_swaps_adjacent_body_rows(cx: &mut TestAppContext) {
        let editor = editor_with_table(cx);
        // 可视行 1（首个正文行）下移一行。
        editor.update(cx, |editor, cx| {
            let table = table_block(editor, cx);
            editor.move_table_row(&table, 1, 1, cx);
        });

        let text = source(&editor, cx);
        let lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
        assert_eq!(cells(&lines[2]), ["3", "4"]);
        assert_eq!(cells(&lines[3]), ["1", "2"]);
    }

    #[rgpui::test]
    async fn set_table_column_alignment_rewrites_the_delimiter_row(cx: &mut TestAppContext) {
        let editor = editor_with_table(cx);
        editor.update(cx, |editor, cx| {
            let table = table_block(editor, cx);
            editor.set_table_column_alignment(&table, 1, TableColumnAlignment::Center, cx);
        });

        let text = source(&editor, cx);
        let delimiter = text
            .lines()
            .nth(1)
            .unwrap_or_default()
            .trim()
            .trim_matches('|')
            .to_string();
        assert_eq!(delimiter.trim(), "--- | :---:");
    }

    #[rgpui::test]
    async fn axis_selection_is_dropped_once_its_axis_shrinks(cx: &mut TestAppContext) {
        let editor = editor_with_table(cx);

        // 两列表格：列 1 有效，列 2 越界。
        editor.update(cx, |editor, cx| {
            let table_id = table_block(editor, cx).entity_id();
            let column_one = AxisSelection {
                table_block_id: table_id,
                kind: TableAxisKind::Column,
                index: 1,
            };
            assert!(editor.table_axis_selection_valid(column_one, cx));

            let column_two = AxisSelection {
                table_block_id: table_id,
                kind: TableAxisKind::Column,
                index: 2,
            };
            assert!(!editor.table_axis_selection_valid(column_two, cx));

            // 可视行上界为正文行数（2），3 已越界。
            let last_row = AxisSelection {
                table_block_id: table_id,
                kind: TableAxisKind::Row,
                index: 2,
            };
            assert!(editor.table_axis_selection_valid(last_row, cx));
            let beyond = AxisSelection {
                table_block_id: table_id,
                kind: TableAxisKind::Row,
                index: 3,
            };
            assert!(!editor.table_axis_selection_valid(beyond, cx));
        });

        // 删掉末列后，选中列自动回落到仍然存在的列。
        editor.update(cx, |editor, cx| {
            let table_id = table_block(editor, cx).entity_id();
            editor.set_table_axis_selection(
                Some(AxisSelection {
                    table_block_id: table_id,
                    kind: TableAxisKind::Column,
                    index: 1,
                }),
                cx,
            );
            let table = table_block(editor, cx);
            editor.delete_table_column(&table, 1, cx);
            editor.normalize_table_axis_state(cx);

            let selection = editor
                .table_axis_selection
                .expect("selection should fall back to a live column");
            assert_eq!(selection.index, 0);
            assert!(editor.table_axis_selection_valid(selection, cx));
        });

        // 指向已消失列的陈旧选中会被规范化清除。
        editor.update(cx, |editor, cx| {
            let table_id = table_block(editor, cx).entity_id();
            editor.set_table_axis_selection(
                Some(AxisSelection {
                    table_block_id: table_id,
                    kind: TableAxisKind::Column,
                    index: 5,
                }),
                cx,
            );
            editor.normalize_table_axis_state(cx);
            assert!(editor.table_axis_selection.is_none());
        });
    }
}
