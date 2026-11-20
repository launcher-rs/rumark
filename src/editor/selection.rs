//! 编辑器级跨渲染块选区。

use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;

use rgpui::*;

use super::{
    CrossBlockDrag, CrossBlockSelection, CrossBlockSelectionEndpoint, DocumentTree, Editor,
    SelectionUndoSnapshot, SourceMapEntry, ViewMode,
};
use crate::components::{
    Block, BlockKind, Copy, Cut, Delete, DeleteBack, End, FocusNext, FocusPrev, Home,
    HorizontalSelectMotion, MoveLeft, MoveRight, UndoKind, WordMoveLeft, WordMoveRight,
    table_to_markdown_lines, update_entity_forgiving,
};

/// 触发拖拽自动滚动的视口边缘宽度。
const DRAG_AUTOSCROLL_EDGE: f32 = 24.0;
/// 拖拽自动滚动的最大滚动速度（每帧逻辑像素）。
const DRAG_AUTOSCROLL_MAX_STEP: f32 = 22.0;

/// 按可见块位置排序后的跨块选区。
#[derive(Clone, Copy)]
struct CanonicalCrossBlockSelection {
    start: CrossBlockSelectionEndpoint,
    end: CrossBlockSelectionEndpoint,
    start_index: usize,
    end_index: usize,
    reversed: bool,
}

impl Editor {
    /// 清除各块上的跨块选区高亮，变化时返回真。
    fn clear_cross_block_selection_visuals(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        for visible in self.document.visible_blocks().to_vec() {
            visible.entity.update(cx, |block, cx| {
                let mut block_changed = block.editor_selection_range.take().is_some();
                if block.cross_block_selected {
                    block.cross_block_selected = false;
                    block_changed = true;
                }
                if block_changed {
                    changed = true;
                    cx.notify();
                }
            });
        }
        changed
    }

    /// 清除跨块选区（含拖拽与高亮）。
    pub(super) fn clear_cross_block_selection(&mut self, cx: &mut Context<Self>) {
        let had_selection = self.cross_block_selection.take().is_some();
        self.cross_block_drag = None;
        let changed_visuals = self.clear_cross_block_selection_visuals(cx);
        let changed = had_selection || changed_visuals;
        if changed {
            cx.notify();
        }
    }

    /// 在指定点开始跨块拖拽选区。
    fn begin_cross_block_drag_at_point(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let had_selection = self.cross_block_selection.take().is_some();
        let changed_visuals = self.clear_cross_block_selection_visuals(cx);
        let changed = had_selection || changed_visuals;
        let anchor = self.cross_block_endpoint_for_point(position, cx);
        self.cross_block_drag = anchor.map(|anchor| CrossBlockDrag { anchor });
        // 分隔线无可见文本，无法用「同块不同偏移」建立选区；按整块原子处理，
        // 按下鼠标即选中，随后可复制或删除。
        if let Some(anchor) = anchor
            && self
                .document
                .block_entity_by_id(anchor.entity_id)
                .is_some_and(|entity| Editor::block_is_whole_block_selectable(entity.read(cx)))
        {
            self.cross_block_selection = Some(CrossBlockSelection {
                anchor,
                focus: anchor,
            });
            self.sync_cross_block_selection_visuals(cx);
        }
        if changed {
            cx.notify();
        }
    }

    /// 判断块是否能作为整体跨块选中（分隔线与表格：无可见文本，需整块选中）。
    fn block_is_whole_block_selectable(block: &Block) -> bool {
        block.kind().is_separator() || block.kind() == BlockKind::Table
    }

    /// 判断点是否落在表格某个单元格的文本区域内。
    ///
    /// 用于区分「点单元格」（进入单元格编辑）与「点表格框」（整块选中表格）。
    fn table_point_inside_cell(table: &Entity<Block>, position: Point<Pixels>, cx: &App) -> bool {
        let block = table.read(cx);
        let Some(runtime) = block.table_runtime.as_ref() else {
            return false;
        };
        runtime
            .header
            .iter()
            .chain(runtime.rows.iter().flatten())
            .any(|cell| {
                cell.read(cx).last_bounds.is_some_and(|bounds| {
                    position.x >= bounds.left()
                        && position.x <= bounds.right()
                        && position.y >= bounds.top()
                        && position.y <= bounds.bottom()
                })
            })
    }

    /// 编辑器鼠标按下捕获：左键在渲染模式开始跨块拖拽，Shift+点击延伸选区。
    pub(super) fn on_editor_capture_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            cx.propagate();
            return;
        }

        if self.view_mode != ViewMode::Rendered {
            cx.propagate();
            return;
        }

        // 上一轮拖拽若漏掉松开（如窗口失焦），丢弃残留的自动滚动循环。
        self.drag_autoscroll_task = None;
        self.drag_pointer = Some(event.position);

        if event.modifiers.shift
            && self.extend_cross_block_selection_to_point(event.position, window, cx)
        {
            cx.stop_propagation();
            return;
        }

        self.begin_cross_block_drag_at_point(event.position, cx);
        cx.propagate();
    }

    /// Shift+点击：把选区固定端点延伸到点击处，返回真表示已按跨块处理。
    /// 返回假时点击落在同一块内，交由块自身的 Shift 扩展逻辑。
    fn extend_cross_block_selection_to_point(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(focus) = self.cross_block_endpoint_for_point(position, cx) else {
            return false;
        };
        let anchor = self
            .cross_block_selection
            .map(|selection| selection.anchor)
            .or_else(|| self.shift_click_anchor(window, cx));
        let Some(anchor) = anchor else {
            return false;
        };

        if anchor.entity_id == focus.entity_id {
            // 点击回到锚点块：撤掉跨块高亮，块内 Shift 点击自行收缩选区。
            self.clear_cross_block_selection(cx);
            return false;
        }

        let selection = CrossBlockSelection { anchor, focus };
        self.cross_block_selection =
            (!self.cross_block_selection_is_empty(selection, cx)).then_some(selection);
        self.cross_block_drag = Some(CrossBlockDrag { anchor });
        self.sync_cross_block_selection_visuals(cx);
        cx.notify();
        true
    }

    /// Shift 点击的固定端点：有选区时取远离光标的一端，否则取光标位置。
    fn shift_click_anchor(&self, window: &Window, cx: &App) -> Option<CrossBlockSelectionEndpoint> {
        let entity = self.focused_edit_target(window, cx)?;
        let entity_id = entity.entity_id();
        let block = entity.read(cx);
        let offset = if block.selected_range.is_empty() {
            block.caret_offset()
        } else if block.selection_reversed {
            block.selected_range.end
        } else {
            block.selected_range.start
        };
        Some(CrossBlockSelectionEndpoint { entity_id, offset })
    }

    /// Shift+上/下：按视觉行扩展选区，焦点端越过块边界时接续到相邻可见块。
    ///
    /// 块内扩展统一走块自身的视觉行计算；已有跨块选区时只移动焦点端点，
    /// 不碰块的原生选区，避免块内高亮与跨块高亮重叠。
    pub(super) fn extend_vertical_selection(
        &mut self,
        block: &Entity<Block>,
        delta: i32,
        preferred_x: f32,
        cx: &mut Context<Self>,
    ) {
        let preferred = px(preferred_x);
        let Some(selection) = self.cross_block_selection else {
            if block.update(cx, |block, cx| {
                block.select_vertically(delta, preferred, cx)
            }) {
                cx.notify();
                return;
            }
            let block_ref = block.read(cx);
            let (anchor_offset, _) = block_ref.selection_anchor_focus();
            let focus = CrossBlockSelectionEndpoint {
                entity_id: block.entity_id(),
                offset: block_ref.caret_offset(),
            };
            self.extend_vertical_selection_beyond(
                CrossBlockSelectionEndpoint {
                    entity_id: block.entity_id(),
                    offset: anchor_offset,
                },
                focus,
                delta,
                preferred,
                cx,
            );
            return;
        };

        // 焦点仍在当前块内：只更新跨块选区的焦点端点并移动光标。
        let focus_block = self.document.block_entity_by_id(selection.focus.entity_id);
        if let Some(focus_block) = focus_block
            && let Some(target) = focus_block.read(cx).vertical_move_target(delta, preferred)
        {
            let focus = CrossBlockSelectionEndpoint {
                entity_id: selection.focus.entity_id,
                offset: target,
            };
            focus_block.update(cx, |focus_block, cx| {
                focus_block.move_to_with_preferred_x(target, Some(preferred), cx);
            });
            let next = CrossBlockSelection {
                anchor: selection.anchor,
                focus,
            };
            self.cross_block_selection =
                (!self.cross_block_selection_is_empty(next, cx)).then_some(next);
            self.sync_cross_block_selection_visuals(cx);
            cx.notify();
            return;
        }

        self.extend_vertical_selection_beyond(
            selection.anchor,
            selection.focus,
            delta,
            preferred,
            cx,
        );
    }

    /// 焦点端已在首末行：把选区接入相邻可见块并放置光标。
    fn extend_vertical_selection_beyond(
        &mut self,
        anchor: CrossBlockSelectionEndpoint,
        focus: CrossBlockSelectionEndpoint,
        delta: i32,
        preferred_x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let Some(focus_index) = self.document.visible_index_for_entity_id(focus.entity_id) else {
            return;
        };
        let target_index = focus_index as i32 + delta;
        if target_index < 0 {
            return;
        }
        let Some(target) = self.document.visible_block_at(target_index as usize) else {
            return;
        };
        let target_id = target.entity_id();
        let offset = target
            .read(cx)
            .vertical_focus_offset(delta < 0, Some(preferred_x));
        let next_focus = CrossBlockSelectionEndpoint {
            entity_id: target_id,
            offset,
        };

        if anchor.entity_id == target_id {
            // 焦点回到锚点块：退回块内选区，避免残留单点跨块选区。
            self.cross_block_selection = None;
            self.clear_cross_block_selection_visuals(cx);
            self.focus_block(target_id);
            target.update(cx, |target, cx| {
                target.move_to(anchor.offset, cx);
                target.select_to(offset, cx);
            });
            cx.notify();
            return;
        }

        self.collapse_anchor_block_native(anchor, cx);

        self.focus_block(target_id);
        target.update(cx, |target, cx| {
            target.move_to_with_preferred_x(offset, Some(preferred_x), cx);
        });

        let next = CrossBlockSelection {
            anchor,
            focus: next_focus,
        };
        self.cross_block_selection =
            (!self.cross_block_selection_is_empty(next, cx)).then_some(next);
        self.sync_cross_block_selection_visuals(cx);
        cx.notify();
    }

    /// 跨块选区绘制期间收起锚点块自身的原生选区。
    fn collapse_anchor_block_native(
        &self,
        anchor: CrossBlockSelectionEndpoint,
        cx: &mut Context<Self>,
    ) {
        let Some(anchor_block) = self.document.block_entity_by_id(anchor.entity_id) else {
            return;
        };
        let anchor_offset = anchor.offset;
        anchor_block.update(cx, |anchor_block, cx| {
            let collapsed = anchor_block.selected_range.is_empty()
                && anchor_block.caret_offset() == anchor_offset;
            if !collapsed {
                anchor_block.move_to(anchor_offset, cx);
            }
        });
    }

    /// Shift+左/右、Shift+Home/End：扩展选区。
    ///
    /// 没有跨块选区时按块内计算的落点就地扩选；已有跨块选区时只移动焦点端点，
    /// 焦点越过块边界则接续到相邻可见块，回到锚点块则收敛为块内原生选区。
    pub(super) fn extend_horizontal_selection(
        &mut self,
        block: &Entity<Block>,
        motion: HorizontalSelectMotion,
        target: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = self.cross_block_selection else {
            block.update(cx, |block, cx| block.select_to(target, cx));
            return;
        };
        // 拖拽后键盘焦点常留在锚点块，跨块扩选一律以选区焦点端点为基准。
        let Some(focus_block) = self.document.block_entity_by_id(selection.focus.entity_id) else {
            return;
        };

        // 跨块拖拽不会同步焦点块的原生光标，落点一律按跨块端点重新计算。
        let (focus_len, focus_offset, local_target) = {
            let block_ref = focus_block.read(cx);
            let len = block_ref.plain_len();
            let offset = selection.focus.offset.min(len);
            let target = match motion {
                HorizontalSelectMotion::CharLeft => block_ref
                    .projected_move_left_target(offset)
                    .map(|(target, _)| target)
                    .unwrap_or_else(|| block_ref.previous_boundary(offset)),
                HorizontalSelectMotion::CharRight => block_ref
                    .projected_move_right_target(offset)
                    .map(|(target, _)| target)
                    .unwrap_or_else(|| block_ref.next_boundary(offset)),
                HorizontalSelectMotion::WordLeft => block_ref.prev_word_boundary(offset),
                HorizontalSelectMotion::WordRight => block_ref.next_word_boundary(offset),
                HorizontalSelectMotion::Home => 0,
                HorizontalSelectMotion::End => len,
            };
            (len, offset, target)
        };

        let crosses = match motion {
            HorizontalSelectMotion::CharLeft | HorizontalSelectMotion::WordLeft => {
                focus_offset == 0
            }
            HorizontalSelectMotion::CharRight | HorizontalSelectMotion::WordRight => {
                focus_offset >= focus_len
            }
            HorizontalSelectMotion::Home | HorizontalSelectMotion::End => false,
        };
        if crosses {
            let delta = if matches!(
                motion,
                HorizontalSelectMotion::CharLeft | HorizontalSelectMotion::WordLeft
            ) {
                -1
            } else {
                1
            };
            self.extend_horizontal_selection_beyond(selection, delta, cx);
            return;
        }

        let step = local_target.min(focus_len);
        focus_block.update(cx, |focus_block, cx| focus_block.move_to(step, cx));
        let next = CrossBlockSelection {
            anchor: selection.anchor,
            focus: CrossBlockSelectionEndpoint {
                entity_id: selection.focus.entity_id,
                offset: step,
            },
        };
        self.cross_block_selection =
            (!self.cross_block_selection_is_empty(next, cx)).then_some(next);
        self.sync_cross_block_selection_visuals(cx);
        cx.notify();
    }

    /// 焦点端已到块首/块尾：把选区接入相邻可见块并放置光标。
    fn extend_horizontal_selection_beyond(
        &mut self,
        selection: CrossBlockSelection,
        delta: i32,
        cx: &mut Context<Self>,
    ) {
        let Some(focus_index) = self
            .document
            .visible_index_for_entity_id(selection.focus.entity_id)
        else {
            return;
        };
        let target_index = focus_index as i32 + delta;
        if target_index < 0 {
            return;
        }
        let Some(target) = self.document.visible_block_at(target_index as usize) else {
            return;
        };
        let target_id = target.entity_id();
        let offset = if delta < 0 {
            target.read(cx).plain_len()
        } else {
            0
        };

        if selection.anchor.entity_id == target_id {
            // 焦点回到锚点块：退回块内选区，避免残留单点跨块选区。
            self.cross_block_selection = None;
            self.clear_cross_block_selection_visuals(cx);
            self.focus_block(target_id);
            target.update(cx, |target, cx| {
                target.move_to(selection.anchor.offset, cx);
                target.select_to(offset, cx);
            });
            cx.notify();
            return;
        }

        self.collapse_anchor_block_native(selection.anchor, cx);
        self.focus_block(target_id);
        target.update(cx, |target, cx| target.move_to(offset, cx));

        let next = CrossBlockSelection {
            anchor: selection.anchor,
            focus: CrossBlockSelectionEndpoint {
                entity_id: target_id,
                offset,
            },
        };
        self.cross_block_selection =
            (!self.cross_block_selection_is_empty(next, cx)).then_some(next);
        self.sync_cross_block_selection_visuals(cx);
        cx.notify();
    }

    /// 编辑器鼠标移动：拖拽中更新选区并维持越界自动滚动。
    pub(super) fn on_editor_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() || self.cross_block_drag.is_none() {
            return;
        }

        self.drag_pointer = Some(event.position);
        if self.apply_drag_selection_at(event.position, cx) {
            cx.notify();
        }
        self.ensure_drag_autoscroll(cx);
    }

    /// 按指针位置更新跨块拖拽选区，返回选区是否变化。
    fn apply_drag_selection_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(drag) = self.cross_block_drag else {
            return false;
        };
        let Some(focus) = self.cross_block_endpoint_for_point(position, cx) else {
            return false;
        };

        if self.cross_block_selection.is_none() && drag.anchor.entity_id == focus.entity_id {
            // 同块拖拽由块内指针会话负责；分隔线等整块可选中项无可见文本，
            // 仍需编辑器接管，否则拖不出选区。
            let anchor_is_whole = self
                .document
                .block_entity_by_id(drag.anchor.entity_id)
                .is_some_and(|entity| Editor::block_is_whole_block_selectable(entity.read(cx)));
            if !anchor_is_whole {
                return false;
            }
        }

        let selection = CrossBlockSelection {
            anchor: drag.anchor,
            focus,
        };
        let next = if self.cross_block_selection_is_empty(selection, cx) {
            None
        } else {
            Some(selection)
        };
        if next == self.cross_block_selection {
            return false;
        }
        self.cross_block_selection = next;
        self.sync_cross_block_selection_visuals(cx);
        true
    }

    /// 拖拽指针越过视口上下边缘时启动自动滚动循环（已在运行则忽略）。
    fn ensure_drag_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self.drag_autoscroll_task.is_some() || self.cross_block_drag.is_none() {
            return;
        }
        self.drag_autoscroll_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                // 借用冲突（嵌套派发）时保留循环，下一拍重试。
                let Some(result) = update_entity_forgiving(&this, cx, |editor, cx| {
                    editor.drag_autoscroll_tick(cx)
                }) else {
                    continue;
                };
                match result {
                    Ok(true) => {}
                    Ok(false) | Err(_) => break,
                }
            }
        }));
    }

    /// 拖拽自动滚动单帧：滚动视口并用视口内探测点延伸选区；拖拽结束返回假。
    fn drag_autoscroll_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if self.cross_block_drag.is_none() {
            return false;
        }
        let Some(pointer) = self.drag_pointer else {
            return false;
        };
        let viewport = self.scroll_handle.bounds();
        if viewport.size.height <= px(0.0) {
            return true;
        }

        let edge = px(DRAG_AUTOSCROLL_EDGE);
        let overshoot = if pointer.y < viewport.top() + edge {
            pointer.y - (viewport.top() + edge)
        } else if pointer.y > viewport.bottom() - edge {
            pointer.y - (viewport.bottom() - edge)
        } else {
            return true;
        };

        // 越界越远滚得越快，指针停在边缘外也能持续滚动。
        let step = overshoot
            .as_f32()
            .clamp(-DRAG_AUTOSCROLL_MAX_STEP, DRAG_AUTOSCROLL_MAX_STEP);
        let current = self.scroll_handle.offset().y;
        let max_offset_y = self.scroll_handle.max_offset().y.max(px(0.0));
        let target = (current - px(step)).min(px(0.0)).max(-max_offset_y);
        if target != current {
            self.set_vertical_scroll_offset(target, cx);
        }

        // 指针停在视口外时不再有新的鼠标事件，用夹取到视口内的探测点
        // 继续延伸选区，内容随滚动逐帧进入探测点。
        let probe = point(
            pointer.x,
            pointer
                .y
                .clamp(viewport.top() + px(2.0), viewport.bottom() - px(2.0)),
        );
        if self.apply_drag_selection_at(probe, cx) {
            cx.notify();
        } else if self.cross_block_selection.is_none() {
            self.extend_anchor_block_selection_at(probe, cx);
        }
        true
    }

    /// 自动滚动时用探测点延伸锚点块的块内选区（块已滚出视口时由跨块路径接管）。
    fn extend_anchor_block_selection_at(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.cross_block_drag else {
            return;
        };
        let Some(entity) = self.document.block_entity_by_id(drag.anchor.entity_id) else {
            return;
        };
        entity.update(cx, |block, cx| {
            block.extend_pointer_selection_at(position, cx);
        });
    }

    /// 编辑器鼠标松开：结束跨块拖拽、自动滚动与块内指针选区。
    pub(super) fn on_editor_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cross_block_drag = None;
        self.drag_pointer = None;
        self.drag_autoscroll_task = None;
        self.end_block_pointer_selection_sessions(cx);
    }

    /// 复制捕获：有跨块选区时写剪贴板并拦截事件。
    pub(super) fn on_copy_capture(
        &mut self,
        _: &Copy,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(markdown) = self.cross_block_selected_markdown(cx) else {
            cx.propagate();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(markdown));
        cx.stop_propagation();
    }

    /// 剪切捕获：有跨块选区时写剪贴板并删除选区。
    pub(super) fn on_cut_capture(&mut self, _: &Cut, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(markdown) = self.cross_block_selected_markdown(cx) else {
            cx.propagate();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(markdown));
        self.delete_cross_block_selection(cx);
        cx.stop_propagation();
    }

    /// 删除捕获：有跨块选区时删除选区并拦截事件。
    pub(super) fn on_delete_capture(
        &mut self,
        _: &Delete,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.delete_cross_block_selection(cx) {
            cx.propagate();
            return;
        }
        cx.stop_propagation();
    }

    /// 退格删除捕获：有跨块选区时删除选区并拦截事件。
    pub(super) fn on_delete_back_capture(
        &mut self,
        _: &DeleteBack,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.delete_cross_block_selection(cx) {
            cx.propagate();
            return;
        }
        cx.stop_propagation();
    }

    /// 方向键折叠跨块选区：移动到选区起点（`at_end` 为假）或终点，并拦截事件。
    fn collapse_cross_block_selection_for_navigation(
        &mut self,
        at_end: bool,
        cx: &mut Context<Self>,
    ) {
        if self.cross_block_selection.is_none() {
            cx.propagate();
            return;
        }
        let endpoint = self.normalized_cross_block_selection(cx).map(|selection| {
            if at_end {
                selection.end
            } else {
                selection.start
            }
        });
        self.cross_block_drag = None;
        self.cross_block_selection = None;
        self.clear_cross_block_selection_visuals(cx);
        if let Some(endpoint) = endpoint
            && let Some(block) = self.document.block_entity_by_id(endpoint.entity_id)
        {
            self.focus_block(endpoint.entity_id);
            block.update(cx, |block, cx| block.move_to(endpoint.offset, cx));
        }
        cx.notify();
        cx.stop_propagation();
    }

    /// 左移捕获。
    pub(super) fn on_move_left_capture(
        &mut self,
        _: &MoveLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.collapse_cross_block_selection_for_navigation(false, cx);
    }

    /// 右移捕获。
    pub(super) fn on_move_right_capture(
        &mut self,
        _: &MoveRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.collapse_cross_block_selection_for_navigation(true, cx);
    }

    /// 按词左移捕获。
    pub(super) fn on_word_move_left_capture(
        &mut self,
        _: &WordMoveLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.collapse_cross_block_selection_for_navigation(false, cx);
    }

    /// 按词右移捕获。
    pub(super) fn on_word_move_right_capture(
        &mut self,
        _: &WordMoveRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.collapse_cross_block_selection_for_navigation(true, cx);
    }

    /// 上移捕获。
    pub(super) fn on_focus_prev_capture(
        &mut self,
        _: &FocusPrev,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.collapse_cross_block_selection_for_navigation(false, cx);
    }

    /// 下移捕获。
    pub(super) fn on_focus_next_capture(
        &mut self,
        _: &FocusNext,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.collapse_cross_block_selection_for_navigation(true, cx);
    }

    /// 行首捕获。
    pub(super) fn on_home_capture(
        &mut self,
        _: &Home,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.collapse_cross_block_selection_for_navigation(false, cx);
    }

    /// 行尾捕获。
    pub(super) fn on_end_capture(&mut self, _: &End, _window: &mut Window, cx: &mut Context<Self>) {
        self.collapse_cross_block_selection_for_navigation(true, cx);
    }

    /// 判断渲染文档是否已被跨块选区整体选中。
    fn rendered_document_is_fully_selected(&self, cx: &App) -> bool {
        let visible = self.document.visible_blocks().to_vec();
        let Some(first) = visible.first() else {
            return false;
        };
        let Some(last) = visible.last() else {
            return false;
        };
        let Some(selection) = self.cross_block_selection else {
            return false;
        };
        let last_len = last.entity.read(cx).plain_len();
        selection.anchor
            == CrossBlockSelectionEndpoint {
                entity_id: first.entity.entity_id(),
                offset: 0,
            }
            && selection.focus
                == CrossBlockSelectionEndpoint {
                    entity_id: last.entity.entity_id(),
                    offset: last_len,
                }
    }

    /// 全选：选中渲染文档的全部块。
    fn select_all_rendered_document(&mut self, cx: &mut Context<Self>) {
        if self.rendered_document_is_fully_selected(cx) {
            return;
        }

        let visible = self.document.visible_blocks().to_vec();
        let Some(first) = visible.first() else {
            return;
        };
        let Some(last) = visible.last() else {
            return;
        };
        let first_id = first.entity.entity_id();
        let last_id = last.entity.entity_id();
        let last_len = last.entity.read(cx).plain_len();

        self.end_block_pointer_selection_sessions(cx);
        self.dismiss_contextual_overlays(cx);
        self.clear_table_axis_preview(cx);
        self.clear_table_axis_selection(cx);
        for visible in &visible {
            visible.entity.update(cx, |block, cx| {
                let cursor = block.caret_offset();
                let collapsed = cursor..cursor;
                if block.selected_range != collapsed {
                    block.selected_range = collapsed;
                    cx.notify();
                }
            });
        }

        self.cross_block_drag = None;
        self.cross_block_selection = Some(CrossBlockSelection {
            anchor: CrossBlockSelectionEndpoint {
                entity_id: first_id,
                offset: 0,
            },
            focus: CrossBlockSelectionEndpoint {
                entity_id: last_id,
                offset: last_len,
            },
        });
        self.sync_cross_block_selection_visuals(cx);
        cx.notify();
    }

    /// 渲染模式全选按键：一次选中整篇渲染文档。
    pub(super) fn on_rendered_select_all_press(
        &mut self,
        _block: Entity<Block>,
        cx: &mut Context<Self>,
    ) {
        if self.view_mode != ViewMode::Rendered {
            return;
        }
        self.select_all_rendered_document(cx);
    }

    /// 把跨块选区换算为源码偏移快照（无选区返回空）。
    pub(super) fn cross_block_source_selection_snapshot(
        &self,
        cx: &App,
    ) -> Option<SelectionUndoSnapshot> {
        let normalized = self.normalized_cross_block_selection(cx)?;
        let range = self.cross_block_source_range_for_normalized(normalized, cx)?;
        Some(SelectionUndoSnapshot {
            range,
            reversed: normalized.reversed,
        })
    }

    /// 尝试按源码快照恢复跨块选区，成功返回真。
    pub(super) fn apply_cross_block_selection_snapshot_if_possible(
        &mut self,
        snapshot: &SelectionUndoSnapshot,
        cx: &mut Context<Self>,
    ) -> bool {
        if snapshot.range.is_empty() {
            return false;
        }

        let mappings = self.build_source_target_mappings(cx);
        let Some(start) = self.endpoint_for_source_offset(snapshot.range.start, &mappings, cx)
        else {
            return false;
        };
        let Some(end) = self.endpoint_for_source_offset(snapshot.range.end, &mappings, cx) else {
            return false;
        };
        let Some(start_index) = self.document.visible_index_for_entity_id(start.entity_id) else {
            return false;
        };
        let Some(end_index) = self.document.visible_index_for_entity_id(end.entity_id) else {
            return false;
        };
        if start_index == end_index {
            return false;
        }

        self.cross_block_selection = Some(if snapshot.reversed {
            CrossBlockSelection {
                anchor: end,
                focus: start,
            }
        } else {
            CrossBlockSelection {
                anchor: start,
                focus: end,
            }
        });
        self.cross_block_drag = None;
        self.sync_cross_block_selection_visuals(cx);
        let focus = if snapshot.reversed { start } else { end };
        self.focus_block(focus.entity_id);
        cx.notify();
        true
    }

    /// 按鼠标位置计算跨块选区端点（含块间与首尾回落）。
    fn cross_block_endpoint_for_point(
        &self,
        position: Point<Pixels>,
        cx: &App,
    ) -> Option<CrossBlockSelectionEndpoint> {
        let mut previous: Option<(Entity<Block>, Bounds<Pixels>)> = None;
        for visible in self.document.visible_blocks() {
            let entity = visible.entity.clone();
            let bounds = entity.read(cx).last_bounds;
            let Some(bounds) = bounds else {
                continue;
            };

            if position.y < bounds.top() {
                if let Some((previous, _)) = previous {
                    let offset = previous.read(cx).plain_len();
                    return Some(CrossBlockSelectionEndpoint {
                        entity_id: previous.entity_id(),
                        offset,
                    });
                }
                return Some(CrossBlockSelectionEndpoint {
                    entity_id: entity.entity_id(),
                    offset: 0,
                });
            }

            if position.y <= bounds.bottom() {
                // 表格：落在单元格上的点击交给单元格编辑，不作为整块选中。
                if entity.read(cx).kind() == BlockKind::Table
                    && Editor::table_point_inside_cell(&entity, position, cx)
                {
                    return None;
                }
                let offset = entity.read(cx).index_at_mouse(position);
                return Some(CrossBlockSelectionEndpoint {
                    entity_id: entity.entity_id(),
                    offset,
                });
            }

            previous = Some((entity, bounds));
        }

        previous.map(|(entity, _)| CrossBlockSelectionEndpoint {
            entity_id: entity.entity_id(),
            offset: entity.read(cx).plain_len(),
        })
    }

    /// 判断跨块选区是否为空（同块同偏移）。
    fn cross_block_selection_is_empty(&self, selection: CrossBlockSelection, cx: &App) -> bool {
        let Some(anchor_index) = self
            .document
            .visible_index_for_entity_id(selection.anchor.entity_id)
        else {
            return true;
        };
        let Some(focus_index) = self
            .document
            .visible_index_for_entity_id(selection.focus.entity_id)
        else {
            return true;
        };
        if anchor_index == focus_index && selection.anchor.offset == selection.focus.offset {
            // 可整块选中的原子块（分隔线）即便端点重合也是有效选区。
            return !self
                .document
                .block_entity_by_id(selection.anchor.entity_id)
                .is_some_and(|entity| Editor::block_is_whole_block_selectable(entity.read(cx)));
        }
        false
    }

    /// 按可见顺序规范化跨块选区（含方向与端点夹取）。
    fn normalized_cross_block_selection(&self, cx: &App) -> Option<CanonicalCrossBlockSelection> {
        let selection = self.cross_block_selection?;
        let anchor = self.clamp_cross_block_endpoint(selection.anchor, cx)?;
        let focus = self.clamp_cross_block_endpoint(selection.focus, cx)?;
        let anchor_index = self
            .document
            .visible_index_for_entity_id(anchor.entity_id)?;
        let focus_index = self.document.visible_index_for_entity_id(focus.entity_id)?;
        let reversed = focus_index < anchor_index
            || (focus_index == anchor_index && focus.offset < anchor.offset);
        let (start, end, start_index, end_index) = if reversed {
            (focus, anchor, focus_index, anchor_index)
        } else {
            (anchor, focus, anchor_index, focus_index)
        };
        if start_index == end_index && start.offset == end.offset {
            // 可整块选中的原子块（分隔线）端点重合仍算有效选区。
            let whole_block_selectable = self
                .document
                .block_entity_by_id(start.entity_id)
                .is_some_and(|entity| Editor::block_is_whole_block_selectable(entity.read(cx)));
            if !whole_block_selectable {
                return None;
            }
        }
        Some(CanonicalCrossBlockSelection {
            start,
            end,
            start_index,
            end_index,
            reversed,
        })
    }

    /// 把选区端点偏移夹取到块可见长度内。
    fn clamp_cross_block_endpoint(
        &self,
        endpoint: CrossBlockSelectionEndpoint,
        cx: &App,
    ) -> Option<CrossBlockSelectionEndpoint> {
        let entity = self.document.block_entity_by_id(endpoint.entity_id)?;
        let len = entity.read(cx).plain_len();
        Some(CrossBlockSelectionEndpoint {
            entity_id: endpoint.entity_id,
            offset: endpoint.offset.min(len),
        })
    }

    /// 同步跨块选区在各块上的高亮区间。
    pub(super) fn sync_cross_block_selection_visuals(&mut self, cx: &mut Context<Self>) {
        let normalized = self.normalized_cross_block_selection(cx);
        let visible_blocks = self.document.visible_blocks().to_vec();
        for (index, visible) in visible_blocks.into_iter().enumerate() {
            let in_selection = normalized.is_some_and(|selection| {
                index >= selection.start_index && index <= selection.end_index
            });
            let next_range = normalized.and_then(|selection| {
                if !in_selection {
                    return None;
                }
                let block = visible.entity.read(cx);
                let len = block.plain_len();
                let range = if selection.start_index == selection.end_index {
                    selection.start.offset.min(len)..selection.end.offset.min(len)
                } else if index == selection.start_index {
                    selection.start.offset.min(len)..len
                } else if index == selection.end_index {
                    0..selection.end.offset.min(len)
                } else {
                    0..len
                };
                // 分隔线等整块可选中项长度为零，空区间也要保留以便渲染整块高亮。
                let whole_block_selectable = Editor::block_is_whole_block_selectable(block);
                (!range.is_empty() || whole_block_selectable).then_some(range)
            });

            visible.entity.update(cx, |block, cx| {
                let mut changed = block.editor_selection_range != next_range;
                if changed {
                    block.editor_selection_range = next_range.clone();
                }
                if block.cross_block_selected != in_selection {
                    block.cross_block_selected = in_selection;
                    changed = true;
                }
                if changed {
                    cx.notify();
                }
            });
        }
    }

    /// 按实体标识索引源码映射表。
    fn source_mapping_by_entity_id(&self, cx: &App) -> HashMap<EntityId, SourceMapEntry> {
        self.build_source_target_mappings(cx)
            .into_iter()
            .map(|mapping| (mapping.entity.entity_id(), mapping))
            .collect()
    }

    /// 把选区端点换算为源码偏移（原子块返回空）。
    fn endpoint_source_offset(
        &self,
        endpoint: CrossBlockSelectionEndpoint,
        mappings: &HashMap<EntityId, SourceMapEntry>,
        cx: &App,
    ) -> Option<usize> {
        let mapping = mappings.get(&endpoint.entity_id)?;
        let block = mapping.entity.read(cx);
        let plain_len = block.plain_len();
        if endpoint.offset == 0 {
            return Some(mapping.full_source_range.start);
        }
        if endpoint.offset >= plain_len {
            return Some(mapping.full_source_range.end);
        }
        let markdown_offset = block
            .current_range_to_markdown_range(endpoint.offset..endpoint.offset)
            .start;
        let max_content = mapping.content_to_source.len().saturating_sub(1);
        Some(
            mapping.full_source_range.start
                + mapping.content_to_source[markdown_offset.min(max_content)],
        )
    }

    /// 按源码偏移反查最近的选区端点。
    fn endpoint_for_source_offset(
        &self,
        offset: usize,
        mappings: &[SourceMapEntry],
        cx: &App,
    ) -> Option<CrossBlockSelectionEndpoint> {
        let mapping = mappings.iter().min_by_key(|mapping| {
            Self::source_offset_distance(&mapping.full_source_range, offset)
        })?;
        let local = if offset <= mapping.full_source_range.start {
            0
        } else if offset >= mapping.full_source_range.end {
            mapping.full_source_range.len()
        } else {
            offset - mapping.full_source_range.start
        };
        let content_offset =
            mapping.source_to_content[local.min(mapping.source_to_content.len().saturating_sub(1))];
        let block = mapping.entity.read(cx);
        Some(CrossBlockSelectionEndpoint {
            entity_id: mapping.entity.entity_id(),
            offset: block.markdown_offset_to_current_offset(content_offset),
        })
    }

    /// 计算规范化选区对应的源码区间（含原子块整体并入）。
    fn cross_block_source_range_for_normalized(
        &self,
        selection: CanonicalCrossBlockSelection,
        cx: &App,
    ) -> Option<Range<usize>> {
        let (mapping_list, block_ranges) = self.build_source_target_mappings_with_block_ranges(cx);
        let mappings: HashMap<EntityId, SourceMapEntry> = mapping_list
            .into_iter()
            .map(|mapping| (mapping.entity.entity_id(), mapping))
            .collect();
        let visible = self.document.visible_blocks();

        // 把端点解析为源码偏移。原子块（如表格）无块内文本映射，
        // 此时 `endpoint_source_offset` 返回空，回退到块自身源码区间，
        // 取能把块留在选区内的一侧。
        let endpoint_offset =
            |endpoint: CrossBlockSelectionEndpoint, index: usize, at_end: bool| -> Option<usize> {
                if let Some(offset) = self.endpoint_source_offset(endpoint, &mappings, cx) {
                    return Some(offset);
                }
                let entity = visible.get(index)?.entity.clone();
                let range = block_ranges.get(&entity.entity_id())?;
                Some(if at_end { range.end } else { range.start })
            };

        let start = endpoint_offset(selection.start, selection.start_index, false)?;
        let end = endpoint_offset(selection.end, selection.end_index, true)?;
        let (mut lo, mut hi) = (start.min(end), start.max(end));

        // 端点偏移永远指不到零可见长度（原子）块之后，选区尾部的表格会被落下，
        // 故把选区内每个原子块的完整源码区间并入，保证整块删除。
        for index in selection.start_index..=selection.end_index {
            let entity = visible.get(index)?.entity.clone();
            if entity.read(cx).plain_len() == 0
                && let Some(range) = block_ranges.get(&entity.entity_id())
            {
                lo = lo.min(range.start);
                hi = hi.max(range.end);
            }
        }
        Some(lo..hi)
    }

    /// 按新源码重建文档块（含表格与图片运行时）。
    pub(super) fn rebuild_after_cross_block_source_edit(
        &mut self,
        source: String,
        cx: &mut Context<Self>,
    ) {
        match self.view_mode {
            ViewMode::Rendered => {
                let mut roots = Self::build_root_blocks_from_markdown(cx, &source);
                if roots.is_empty() {
                    roots.push(Self::new_block(
                        cx,
                        crate::components::BlockRecord::paragraph(String::new()),
                    ));
                }
                self.document.replace_roots(roots, cx);
                self.rebuild_table_runtimes(cx);
                self.rebuild_image_runtimes(cx);
            }
            ViewMode::Source => {
                let block = Self::new_block(
                    cx,
                    crate::components::BlockRecord::paragraph(source.clone()),
                );
                block.update(cx, |block, _cx| block.enable_source_document_mode());
                self.document.replace_roots(vec![block], cx);
                self.table_cells.clear();
            }
        }
    }

    /// 在同块内标记源码区间对应的输入法组合态。
    fn apply_marked_source_range(&mut self, source_range: Range<usize>, cx: &mut Context<Self>) {
        if source_range.is_empty() {
            return;
        }
        let mappings = self.build_source_target_mappings(cx);
        let Some(start) = self.endpoint_for_source_offset(source_range.start, &mappings, cx) else {
            return;
        };
        let Some(end) = self.endpoint_for_source_offset(source_range.end, &mappings, cx) else {
            return;
        };
        if start.entity_id != end.entity_id {
            return;
        }
        let Some(block) = self.focusable_entity_by_id(start.entity_id) else {
            return;
        };
        block.update(cx, |block, cx| {
            block.marked_range = Some(start.offset.min(end.offset)..start.offset.max(end.offset));
            cx.notify();
        });
    }

    /// 用新文本替换跨块选区并恢复光标，成功返回真。
    pub(super) fn replace_cross_block_selection_with_text(
        &mut self,
        new_text: &str,
        selected_range_relative: Option<Range<usize>>,
        mark_inserted_text: bool,
        undo_kind: UndoKind,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(selection) = self.normalized_cross_block_selection(cx) else {
            return false;
        };
        let Some(source_range) = self.cross_block_source_range_for_normalized(selection, cx) else {
            return false;
        };

        self.prepare_undo_capture(undo_kind, cx);
        let mut source = self.current_document_source(cx);
        let range = crate::text_offsets::normalize_utf8_range(&source, source_range.clone());
        let start = range.start;
        source.replace_range(range, new_text);
        self.cross_block_selection = None;
        self.cross_block_drag = None;

        let inserted_start = start;
        let inserted_end = inserted_start + new_text.len();
        let selected_source_range = selected_range_relative
            .map(|relative| {
                inserted_start + relative.start.min(new_text.len())
                    ..inserted_start + relative.end.min(new_text.len())
            })
            .unwrap_or(inserted_end..inserted_end);
        let marked_source_range =
            (mark_inserted_text && !new_text.is_empty()).then_some(inserted_start..inserted_end);

        self.rebuild_after_cross_block_source_edit(source, cx);
        self.apply_selection_snapshot_in_current_mode(
            &SelectionUndoSnapshot {
                range: selected_source_range,
                reversed: false,
            },
            cx,
        );
        if let Some(marked_source_range) = marked_source_range {
            self.apply_marked_source_range(marked_source_range, cx);
        }
        self.mark_dirty(cx);
        self.finalize_pending_undo_capture(cx);
        self.sync_table_axis_visuals(cx);
        self.dismiss_contextual_overlays(cx);
        self.sync_cross_block_selection_visuals(cx);
        self.request_active_block_scroll_into_view(cx);
        cx.notify();
        true
    }

    /// 读取跨块选区的 Markdown 文本（无选区返回空）。
    pub(super) fn cross_block_selected_markdown(&self, cx: &App) -> Option<String> {
        let selection = self.normalized_cross_block_selection(cx)?;
        let source = self.current_document_source(cx);
        let mappings = self.source_mapping_by_entity_id(cx);
        let visible = self.document.visible_blocks();

        // 按文档序列化相同的间距拼接块：块间空行、紧凑列表项与列表项子块的
        // 分隔规则统一复刻 `tree.rs`（见 `cross_block_separator_newlines`）。
        let mut result = String::new();
        let mut wrote_chunk = false;
        // 上一个块是否贡献了空片段（空标题引用这类自身不占源码行的容器）：
        // 此时它已经带走了行尾换行，下一块不再补行首换行。
        let mut previous_chunk_empty = false;
        let mut pending_empty = 0usize;

        for index in selection.start_index..=selection.end_index {
            let entity = visible.get(index)?.entity.clone();
            let block = entity.read(cx);
            let len = block.plain_len();
            let range = if selection.start_index == selection.end_index {
                selection.start.offset.min(len)..selection.end.offset.min(len)
            } else if index == selection.start_index {
                selection.start.offset.min(len)..len
            } else if index == selection.end_index {
                0..selection.end.offset.min(len)
            } else {
                0..len
            };
            let empty_root_paragraph = Editor::is_empty_root_paragraph(block);
            // 原子块（表格、分隔线、图片等）长度为零；分隔线单独一块时也能整体选中，
            // 故不再要求选区跨块。
            let include_atomic = len == 0 && !empty_root_paragraph;
            let full_block = range.start == 0
                && range.end == len
                && (selection.start_index != selection.end_index || len > 0 || include_atomic);
            if range.is_empty() && !(full_block || include_atomic) {
                continue;
            }

            // 空段落是空行分隔符而非内容，延迟处理以复现块间空行而非折叠；
            // 原子内容（表格、分隔线、图片）长度亦为零，但不是空段落，仍需序列化。
            if (full_block || include_atomic) && Editor::is_empty_root_paragraph(block) {
                pending_empty += 1;
                continue;
            }

            let chunk = self.markdown_chunk_for_block(
                &entity,
                range,
                full_block || include_atomic,
                &source,
                &mappings,
                cx,
            );
            if wrote_chunk {
                // 扁平可见序把「列表项子块」「引用容器子块」「根层兄弟」混在一起，
                // 一律套用根层规则会在列表项与其子块之间多插空行（粘贴后子块脱离列表）。
                let newlines = self
                    .cross_block_separator_newlines(&entity, block, pending_empty, cx)
                    .saturating_sub(usize::from(previous_chunk_empty));
                if newlines > 0 {
                    // 空行若落在引用/列表容器内会带容器逐行前缀（`> `、缩进），
                    // 需要补回该前缀，否则粘回引用会断开容器。
                    let blank_prefix = if newlines > 1 {
                        self.container_blank_line_prefix(&entity, &source, &mappings)
                    } else {
                        String::new()
                    };
                    result.push('\n');
                    for _ in 1..newlines {
                        result.push_str(&blank_prefix);
                        result.push('\n');
                    }
                }
            }
            result.push_str(&chunk);
            wrote_chunk = true;
            previous_chunk_empty = chunk.is_empty();
            pending_empty = 0;
        }

        Some(result)
    }

    /// 容器内空行的逐行前缀（`> `、列表缩进）：取父容器自己那一行的前导
    /// `>` 与空格串，与 `tree.rs` 把空行交给外层容器逐行加前缀的做法一致。
    fn container_blank_line_prefix(
        &self,
        entity: &Entity<Block>,
        source: &str,
        mappings: &HashMap<EntityId, SourceMapEntry>,
    ) -> String {
        let Some(parent) = self
            .document
            .find_block_location(entity.entity_id())
            .and_then(|location| location.parent)
        else {
            return String::new();
        };
        let Some(mapping) = mappings.get(&parent.entity_id()) else {
            return String::new();
        };
        let line_start = source
            .get(..mapping.full_source_range.start)
            .and_then(|head| head.rfind('\n'))
            .map_or(0, |index| index + 1);
        source
            .get(line_start..)
            .unwrap_or_default()
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '>')
            .collect()
    }

    /// 复刻 `tree.rs` 的块间分隔规则，返回跨块拼接时该块之前应插入的换行数。
    ///
    /// 返回值含「结束上一块行尾的换行」；脚注首段紧贴 `[^id]: ` 头行时返回 0。
    fn cross_block_separator_newlines(
        &self,
        entity: &Entity<Block>,
        block: &Block,
        pending_empty: usize,
        cx: &App,
    ) -> usize {
        let entity_id = entity.entity_id();

        // 列表项子块：仅纯段落子块前加空行；代码块、表格、子列表等紧贴父项。
        if block.parent_is_list_item {
            return usize::from(DocumentTree::list_child_requires_leading_blank_line(block)) + 1;
        }

        let Some(location) = self.document.find_block_location(entity_id) else {
            return pending_empty + 2;
        };
        let Some(parent) = location.parent else {
            // 根层兄弟：空段落记入延迟空行，连续列表项之间不加空行。
            let previous_is_list_item = self
                .previous_non_empty_root_sibling_is_list_item(entity_id, cx)
                .unwrap_or(false);
            return if previous_is_list_item && block.kind().is_list_item() {
                pending_empty + 1
            } else {
                pending_empty + 2
            };
        };

        if parent.read(cx).kind().is_footnote_definition() {
            // 脚注定义的首个段落接在 `[^id]: ` 头行同一行上，完全不换行；
            // 其余子块与根层同规则（tree.rs 传 `blank_line_between_siblings = true`）。
            let previous = self.document.previous_sibling(entity_id, cx);
            if previous.is_none() && block.kind() == BlockKind::Paragraph {
                return 0;
            }
            let previous_is_list_item =
                previous.is_some_and(|sibling| sibling.read(cx).kind().is_list_item());
            return usize::from(!(previous_is_list_item && block.kind().is_list_item())) + 1;
        }

        // 引用 / 标注容器等：子块之间不主动加空行，但源码里显式写出的空段落
        // （如引用内的 `> `）仍占一行，需按延迟空行数补回。
        pending_empty + 1
    }

    /// 返回根层中紧邻 `entity_id` 之前、跳过空段落后的兄弟是否为列表项。
    fn previous_non_empty_root_sibling_is_list_item(
        &self,
        entity_id: EntityId,
        cx: &App,
    ) -> Option<bool> {
        let mut cursor = entity_id;
        while let Some(sibling) = self.document.previous_sibling(cursor, cx) {
            let sibling_ref = sibling.read(cx);
            if !Editor::is_empty_root_paragraph(sibling_ref) {
                return Some(sibling_ref.kind().is_list_item());
            }
            cursor = sibling.entity_id();
        }
        None
    }

    /// 截取单个块在选区内的 Markdown 片段。
    fn markdown_chunk_for_block(
        &self,
        entity: &Entity<Block>,
        range: Range<usize>,
        full_block: bool,
        source: &str,
        mappings: &HashMap<EntityId, SourceMapEntry>,
        cx: &App,
    ) -> String {
        if let Some(mapping) = mappings.get(&entity.entity_id()) {
            if full_block {
                // 映射区间来自序列化推导，理论上与源码等长；这里再归一化到
                // 标量边界并夹取，避免任何上游漂移演变成切片 panic。
                let range = crate::text_offsets::normalize_utf8_range(
                    source,
                    mapping.full_source_range.clone(),
                );
                return source.get(range).unwrap_or_default().to_string();
            }

            let start = self
                .endpoint_source_offset(
                    CrossBlockSelectionEndpoint {
                        entity_id: entity.entity_id(),
                        offset: range.start,
                    },
                    mappings,
                    cx,
                )
                .unwrap_or(mapping.full_source_range.start);
            let end = self
                .endpoint_source_offset(
                    CrossBlockSelectionEndpoint {
                        entity_id: entity.entity_id(),
                        offset: range.end,
                    },
                    mappings,
                    cx,
                )
                .unwrap_or(mapping.full_source_range.end);
            let range = crate::text_offsets::normalize_utf8_range(source, start..end);
            return source.get(range).unwrap_or_default().to_string();
        }

        let block = entity.read(cx);
        if full_block {
            // 空标题的引用自身不占源码行（外层 `> ` 前缀由子块映射补齐）；
            // 按标题重建会凭空多出一行 `> `。
            if block.kind() == BlockKind::Quote
                && block.record.title.plain_text().is_empty()
                && !block.children.is_empty()
            {
                return String::new();
            }
            // 无逐字映射的原子块（表格等）按序列化前缀重建：列表缩进 + 引用前缀。
            // 前缀长度必须与 `tree.rs` / `source_map.rs` 一致，否则复制出的
            // 表格会脱离所在列表或引用。
            let prefix = format!(
                "{}{}",
                " ".repeat(block.source_indent_columns),
                "> ".repeat(block.quote_depth)
            );
            return match block.kind() {
                BlockKind::Table => block
                    .record
                    .table
                    .as_ref()
                    .map(table_to_markdown_lines)
                    .map(|lines| {
                        lines
                            .into_iter()
                            .map(|line| format!("{prefix}{line}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
                _ => block
                    .record
                    .markdown_line(block.source_indent_columns, block.list_ordinal),
            };
        }

        let markdown = block.record.title.serialize_markdown();
        let markdown_range = block.current_range_to_markdown_range(range);
        markdown
            .get(markdown_range)
            .map(ToOwned::to_owned)
            .unwrap_or_default()
    }

    /// 删除跨块选区并把光标留在删除点，成功返回真。
    fn delete_cross_block_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(selection) = self.normalized_cross_block_selection(cx) else {
            return false;
        };
        let Some(source_range) = self.cross_block_source_range_for_normalized(selection, cx) else {
            return false;
        };
        if source_range.is_empty() {
            return false;
        }

        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        let mut source = self.current_document_source(cx);
        let range = crate::text_offsets::normalize_utf8_range(&source, source_range.clone());
        let range = self.expand_lone_separator_delete_range(selection, range, &source, cx);
        let start = range.start;
        source.replace_range(range, "");
        self.cross_block_selection = None;
        self.cross_block_drag = None;

        self.rebuild_after_cross_block_source_edit(source, cx);

        self.apply_selection_snapshot_in_current_mode(
            &SelectionUndoSnapshot {
                range: start..start,
                reversed: false,
            },
            cx,
        );
        self.mark_dirty(cx);
        self.finalize_pending_undo_capture(cx);
        self.sync_table_axis_visuals(cx);
        self.dismiss_contextual_overlays(cx);
        self.sync_cross_block_selection_visuals(cx);
        cx.notify();
        true
    }

    /// 单独删除根层分隔线时，连带吃掉其前面的空行，避免残留一个空段落。
    fn expand_lone_separator_delete_range(
        &self,
        selection: CanonicalCrossBlockSelection,
        range: Range<usize>,
        source: &str,
        cx: &App,
    ) -> Range<usize> {
        if selection.start_index != selection.end_index {
            return range;
        }
        let Some(entity) = self.document.block_entity_by_id(selection.start.entity_id) else {
            return range;
        };
        let block = entity.read(cx);
        if !block.kind().is_separator() || block.render_depth != 0 {
            return range;
        }
        let bytes = source.as_bytes();
        let mut start = range.start;
        let mut end = range.end;
        while start > 0 && bytes[start - 1] == b'\n' {
            start -= 1;
        }
        if start == range.start {
            // 分隔线位于文档开头，没有前置空行可吃，改为吞掉其后的空行。
            while end < bytes.len() && bytes[end] == b'\n' {
                end += 1;
            }
        }
        start..end
    }

    /// 返回当前选区的 Markdown 文本，跨块优先、单块次之，无选中时返回空。
    pub(super) fn selected_markdown_text(&self, cx: &App) -> Option<String> {
        // 优先使用已有的跨块选区。
        if let Some(text) = self.cross_block_selected_markdown(cx)
            && !text.is_empty()
        {
            return Some(text);
        }

        // 回退到含非折叠选区的单个块。
        for visible in self.document.visible_blocks() {
            let block = visible.entity.read(cx);
            if block.selected_range.is_empty() {
                continue;
            }
            let markdown_range =
                block.current_range_to_markdown_range(block.selected_range.clone());
            let full_markdown = block.record.title.serialize_markdown();
            let start = markdown_range.start.min(full_markdown.len());
            let end = markdown_range.end.min(full_markdown.len());
            if start < end {
                return full_markdown.get(start..end).map(ToOwned::to_owned);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use rgpui::{AppContext, Bounds, Context, TestAppContext, point, px, size};

    use super::{CrossBlockSelection, CrossBlockSelectionEndpoint, Editor};
    use crate::components::{BlockKind, Cut, HorizontalSelectMotion, Undo, UndoKind};
    use crate::i18n::I18nManager;

    /// 初始化跨块选区测试应用（测试辅助）。
    fn init_editor_test_app(cx: &mut TestAppContext) {
        cx.update(|cx| {
            I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
    }

    /// 重绘测试窗口并等待空闲（测试辅助）。
    fn redraw(cx: &mut rgpui::VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    /// 按可见块索引设置跨块选区（测试辅助）。
    fn set_selection(
        editor: &mut Editor,
        start_index: usize,
        start_offset: usize,
        end_index: usize,
        end_offset: usize,
        cx: &mut Context<Editor>,
    ) {
        let visible = editor.document.visible_blocks().to_vec();
        let start = visible[start_index].entity.entity_id();
        let end = visible[end_index].entity.entity_id();
        editor.cross_block_selection = Some(CrossBlockSelection {
            anchor: CrossBlockSelectionEndpoint {
                entity_id: start,
                offset: start_offset,
            },
            focus: CrossBlockSelectionEndpoint {
                entity_id: end,
                offset: end_offset,
            },
        });
        editor.sync_cross_block_selection_visuals(cx);
    }

    /// 为可见块补上测试用布局边界（测试辅助）。
    fn assign_visible_block_bounds(editor: &mut Editor, cx: &mut Context<Editor>) {
        for (index, visible) in editor
            .document
            .visible_blocks()
            .to_vec()
            .into_iter()
            .enumerate()
        {
            visible.entity.update(cx, move |block, _cx| {
                block.last_bounds = Some(Bounds::new(
                    point(px(0.0), px(index as f32 * 32.0)),
                    size(px(400.0), px(24.0)),
                ));
            });
        }
    }

    #[test]
    fn mouse_down_starts_cross_block_drag_after_clearing_old_selection() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta\n\ngamma".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            assign_visible_block_bounds(editor, cx);
            set_selection(editor, 0, 0, 2, 2, cx);
            assert!(editor.cross_block_selection.is_some());
            assert!(
                editor
                    .document
                    .visible_blocks()
                    .iter()
                    .any(|visible| visible.entity.read(cx).editor_selection_range.is_some())
            );

            editor.begin_cross_block_drag_at_point(point(px(8.0), px(4.0)), cx);

            assert!(editor.cross_block_selection.is_none());
            assert!(editor.cross_block_drag.is_some());
            assert!(
                editor
                    .document
                    .visible_blocks()
                    .iter()
                    .all(|visible| visible.entity.read(cx).editor_selection_range.is_none())
            );
        });
        cx.quit();
    }

    #[test]
    fn typing_replaces_cross_block_selection_with_plain_text() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta\n\ngamma".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            set_selection(editor, 0, 2, 2, 2, cx);
            assert!(editor.replace_cross_block_selection_with_text(
                "X",
                None,
                false,
                UndoKind::CoalescibleText,
                cx
            ));

            assert_eq!(editor.document.markdown_text(cx), "alXmma");
            assert!(editor.cross_block_selection.is_none());
            assert!(editor.cross_block_drag.is_none());
            let block = editor.document.visible_blocks()[0].entity.read(cx);
            assert_eq!(block.selected_range, 3..3);
            assert!(block.marked_range.is_none());
        });
        cx.quit();
    }

    #[test]
    fn ime_composition_replaces_cross_block_selection_and_marks_inserted_text() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta\n\ngamma".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            set_selection(editor, 0, 2, 2, 2, cx);
            assert!(editor.replace_cross_block_selection_with_text(
                "ni",
                Some(2..2),
                true,
                UndoKind::CoalescibleText,
                cx
            ));

            assert_eq!(editor.document.markdown_text(cx), "alnimma");
            let block = editor.document.visible_blocks()[0].entity.read(cx);
            assert_eq!(block.selected_range, 4..4);
            assert_eq!(block.marked_range, Some(2..4));
            assert!(block.editor_selection_range.is_none());
        });
        cx.quit();
    }

    #[test]
    fn cross_block_selection_marks_visual_ranges_and_copies_markdown() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "alpha **bold**\n\n- item\n\n![alt](image.png)".to_string(),
                None,
            )
        });

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3);
            let end_len = visible[2].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, 2, end_len, cx);

            assert_eq!(
                editor.cross_block_selected_markdown(cx).as_deref(),
                Some("alpha **bold**\n\n- item\n\n![alt](image.png)")
            );
            for visible in visible {
                let block = visible.entity.read(cx);
                assert_eq!(block.editor_selection_range, Some(0..block.plain_len()));
            }
        });
        cx.quit();
    }

    #[test]
    fn cross_block_copy_keeps_literal_intraword_underscore() {
        // 回归：单行复制走可见文本正常，跨块复制取序列化源码时曾把
        // 字面 `_` 写成 `\_`，粘贴后多出一个反斜杠。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let source = "111111_22222222\n\n3333333";
        let editor = cx.new(|cx| Editor::from_markdown(cx, source.to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 2);
            let end_len = visible[1].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, 1, end_len, cx);

            let copied = editor
                .cross_block_selected_markdown(cx)
                .expect("应生成跨块复制文本");
            assert!(
                !copied.contains("\\_"),
                "复制文本不应带冗余转义：{copied:?}"
            );
            assert_eq!(copied, source);
        });
        cx.quit();
    }

    #[test]
    fn cross_block_cut_writes_markdown_deletes_range_and_undo_restores() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let original = "alpha\n\nbeta\n\ngamma";
        let (editor, cx) = cx.add_window_view({
            let original = original.to_string();
            move |_window, cx| Editor::from_markdown(cx, original.clone(), None)
        });
        redraw(cx);

        editor.update(cx, |editor, cx| {
            set_selection(editor, 0, 2, 2, 2, cx);
            assert_eq!(
                editor.cross_block_selected_markdown(cx).as_deref(),
                Some("pha\n\nbeta\n\nga")
            );
        });
        redraw(cx);

        cx.dispatch_action(Cut);
        redraw(cx);

        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("pha\n\nbeta\n\nga")
        );
        assert_eq!(
            editor.read_with(cx, |editor, cx| editor.document.markdown_text(cx)),
            "almma"
        );

        cx.dispatch_action(Undo);
        redraw(cx);

        assert_eq!(
            editor.read_with(cx, |editor, cx| editor.document.markdown_text(cx)),
            original
        );
        editor.read_with(cx, |editor, cx| {
            assert_eq!(
                editor.cross_block_selected_markdown(cx).as_deref(),
                Some("pha\n\nbeta\n\nga")
            );
        });
        cx.quit();
    }

    const TABLE_DOC: &str = "alpha\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\ngamma";

    #[test]
    fn delete_selection_spanning_table_removes_table() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, TABLE_DOC.to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3);
            let end_len = visible[2].entity.read(cx).plain_len();
            // The table sits in the interior of the selection.
            set_selection(editor, 0, 0, 2, end_len, cx);
            assert!(editor.delete_cross_block_selection(cx));

            let text = editor.document.markdown_text(cx);
            assert!(!text.contains('|'), "table should be gone: {text:?}");
            assert!(!text.contains("alpha"), "text: {text}");
            assert!(!text.contains("gamma"), "text: {text}");
        });
        cx.quit();
    }

    #[test]
    fn delete_selection_with_trailing_table_removes_table() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, TABLE_DOC.to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            assert_eq!(editor.document.visible_blocks().len(), 3);
            // Selection ends at the start of the table block: the previously
            // broken case where the trailing atomic block was left behind.
            set_selection(editor, 0, 0, 1, 0, cx);
            assert!(editor.delete_cross_block_selection(cx));

            // The table is removed in full; only `gamma` survives (deleting from
            // the document start leaves the table's trailing blank line, which
            // reparses to leading empty paragraphs, harmless and trimmable).
            let text = editor.document.markdown_text(cx);
            assert!(
                !text.contains('|'),
                "trailing table should be gone: {text:?}"
            );
            assert_eq!(text.trim(), "gamma");
        });
        cx.quit();
    }

    #[test]
    fn delete_selection_of_only_table_removes_just_the_table() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, TABLE_DOC.to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3);
            let alpha_len = visible[0].entity.read(cx).plain_len();
            // Drag from the end of the paragraph above onto the table: only the
            // table is removed, and re-parse normalizes the spacing.
            set_selection(editor, 0, alpha_len, 1, 0, cx);
            assert!(editor.delete_cross_block_selection(cx));

            assert_eq!(editor.document.markdown_text(cx), "alpha\n\ngamma");
        });
        cx.quit();
    }

    #[test]
    fn cut_selection_including_table_serializes_and_deletes_it() {
        // Exercise cut's two halves directly (the clipboard markdown and the
        // deleted source range) rather than dispatching the action, keeping this
        // a focused unit test of the cut logic.
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, TABLE_DOC.to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3);
            let end_len = visible[2].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, 2, end_len, cx);

            // The clipboard markdown serializes the full table, matching what
            // delete removes; otherwise cut would drop it from the clipboard.
            let markdown = editor.cross_block_selected_markdown(cx).unwrap();
            assert!(markdown.contains("| a | b |"), "clipboard: {markdown:?}");
            assert!(markdown.contains("| 1 | 2 |"), "clipboard: {markdown:?}");
            assert!(
                markdown.contains("alpha") && markdown.contains("gamma"),
                "markdown: {markdown}"
            );

            assert!(editor.delete_cross_block_selection(cx));
            assert!(
                !editor.document.markdown_text(cx).contains('|'),
                "document should no longer contain the table"
            );
        });
        cx.quit();
    }

    #[test]
    fn delete_selection_spanning_code_block_removes_it() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        // Code blocks edit their raw text, so they are deletable as an ordinary
        // text range; this documents that plain_len-based behavior.
        let doc = "alpha\n\n```\ncode\n```\n\ngamma";
        let editor = cx.new(|cx| Editor::from_markdown(cx, doc.to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3);
            let end_len = visible[2].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, 2, end_len, cx);
            assert!(editor.delete_cross_block_selection(cx));

            let text = editor.document.markdown_text(cx);
            assert!(
                !text.contains("code"),
                "code block should be gone: {text:?}"
            );
        });
        cx.quit();
    }

    #[test]
    fn delete_selection_ending_on_trailing_empty_paragraph_removes_table() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let doc = "alpha\n\n| a | b |\n| --- | --- |\n| 1 | 2 |";
        let editor = cx.new(|cx| Editor::from_markdown(cx, doc.to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            // Append a trailing empty paragraph, exactly as inserting a table at
            // the end of a document does. Ending the selection on it used to
            // abort deletion because empty roots had no source span.
            let empty =
                Editor::new_block(cx, crate::components::BlockRecord::paragraph(String::new()));
            let index = editor.document.root_count();
            editor
                .document
                .insert_blocks_at(None, index, vec![empty], cx);

            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3);
            let alpha_len = visible[0].entity.read(cx).plain_len();
            // From the end of `alpha` onto the trailing empty paragraph.
            set_selection(editor, 0, alpha_len, 2, 0, cx);
            assert!(editor.delete_cross_block_selection(cx));

            let text = editor.document.markdown_text(cx);
            assert!(!text.contains('|'), "table should be gone: {text:?}");
            assert_eq!(text.trim(), "alpha");
        });
        cx.quit();
    }

    #[test]
    fn delete_selection_starting_on_empty_paragraph_removes_table() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let doc = "| a | b |\n| --- | --- |\n| 1 | 2 |\n\ngamma";
        let editor = cx.new(|cx| Editor::from_markdown(cx, doc.to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            // Prepend a leading empty paragraph; starting the highlight on it used
            // to abort deletion (the user's "drag up from the text below into an
            // empty block above the table" case).
            let empty =
                Editor::new_block(cx, crate::components::BlockRecord::paragraph(String::new()));
            editor.document.insert_blocks_at(None, 0, vec![empty], cx);

            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3);
            // From the empty paragraph (index 0) to the start of `gamma`.
            set_selection(editor, 0, 0, 2, 0, cx);
            assert!(editor.delete_cross_block_selection(cx));

            let text = editor.document.markdown_text(cx);
            assert!(!text.contains('|'), "table should be gone: {text:?}");
            assert_eq!(text.trim(), "gamma");
        });
        cx.quit();
    }

    #[test]
    fn cross_block_copy_and_delete_with_list_group_separator_match_source() {
        // 回归：列表组之间夹空段落时，源码映射的分隔符数曾比实际序列化多 1，
        // 使 [列表, 空段落, 列表] 结构的块区间超出源码长度；全选复制/删除因此
        // 在切片处越界 panic。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "1. aa\n2. bb".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let empty =
                Editor::new_block(cx, crate::components::BlockRecord::paragraph(String::new()));
            editor.document.insert_blocks_at(None, 1, vec![empty], cx);

            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3);
            let source = editor.document.markdown_text(cx);

            let end_len = visible[2].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, 2, end_len, cx);
            // 全选复制必须与权威序列化一致，且不能越界。
            assert_eq!(
                editor.cross_block_selected_markdown(cx).as_deref(),
                Some(source.as_str())
            );
            // 删除走同一套映射区间，同样不能 panic。
            assert!(editor.delete_cross_block_selection(cx));
            let text = editor.document.markdown_text(cx);
            assert!(
                !text.contains("aa") && !text.contains("bb"),
                "selection should be removed: {text:?}"
            );
        });
        cx.quit();
    }

    #[test]
    fn cross_block_selection_with_nested_table_matches_source() {
        // 回归：嵌套在列表项里的表格，tree 序列化曾漏掉列表缩进，与源码映射的
        // 行前缀不一致；产物会脱离列表，且跨块选区取到的表格片段不再带缩进。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "- item\n\n  | a | b |\n  | --- | --- |\n  | 1 | 2 |".to_string(),
                None,
            )
        });

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(
                visible.len(),
                2,
                "fixture should parse to list item + table"
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Table);
            let source = editor.document.markdown_text(cx);
            assert_eq!(
                source, "- item\n  | a | b |\n  | --- | --- |\n  | 1 | 2 |",
                "nested table should stay indented under the list item"
            );

            let end_len = visible[1].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, 1, end_len, cx);
            let selected = editor
                .cross_block_selected_markdown(cx)
                .expect("selection markdown");
            assert!(
                selected.contains("  | a | b |\n  | --- | --- |\n  | 1 | 2 |"),
                "nested table should be selected with list indent: {selected:?}"
            );
        });
        cx.quit();
    }

    #[test]
    fn cross_block_copy_and_delete_match_source_for_footnote_definitions() {
        // 回归：脚注定义的映射曾把头行算两遍（脚注自身映射 + 首段映射都覆盖
        // `[^id]: `），且首段/子块的空行少算容器前缀，导致整块复制错字、偏移
        // 漂移；嵌套在列表-引用里的脚注还会整段被截断。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        for (label, doc) in [
            ("head-only", "[^a]: note\n\ntail"),
            ("para-then-list", "[^a]: note\n\n    - one\n    - two"),
            ("list-only-child", "[^a]:\n\n    - one"),
            (
                "multi-line-first-para",
                "[^a]: line one\n    line two\n\ntail",
            ),
            ("two-paragraphs", "[^a]: first\n\n    second\n\ntail"),
            ("in-quote", "> [^a]: note"),
            ("in-quote-in-list", "- item\n  > [^a]: note"),
            (
                "in-quote-in-list-with-child",
                "- item\n  > [^a]: note\n  > \n  >     - one",
            ),
            ("in-quote-with-child", "> [^a]: note\n> \n>     - one"),
            ("in-callout", "> [!NOTE]\n> [^a]: note\n\ntail"),
            ("in-list-quote", "- item\n  > [^b]: note\n\n    tail"),
            // 同类空行：容器内的空行同样带 `> ` 前缀。
            ("quote-list-para", "> - item\n>\n>   para"),
            ("quote-two-paras", "> a\n>\n> b"),
            ("list-paras", "- item\n\n  para"),
            ("quote-table", "> | A | B |\n> | --- | --- |"),
            ("root-blank-line", "a\n\nb"),
            ("root-two-blank-lines", "a\n\n\nb"),
            ("root-list-blank-list", "- a\n\n- b"),
            ("root-blank-after-list", "- a\n\ntail"),
            (
                "quote-table-in-list",
                "- item\n  > | A | B |\n  > | --- | --- |",
            ),
            (
                "quote-para-then-table",
                "> text\n>\n> | A | B |\n> | --- | --- |",
            ),
        ] {
            let editor = cx.new(|cx| Editor::from_markdown(cx, doc.to_string(), None));
            editor.update(&mut cx, |editor, cx| {
                let source = editor.document.markdown_text(cx);
                let visible = editor.document.visible_blocks().to_vec();
                let last = visible.len() - 1;
                let end_len = visible[last].entity.read(cx).plain_len();
                set_selection(editor, 0, 0, last, end_len, cx);
                let selected = editor
                    .cross_block_selected_markdown(cx)
                    .expect("selection markdown");
                assert_eq!(selected, source, "fixture {label} should round-trip");
                assert!(
                    editor.delete_cross_block_selection(cx),
                    "fixture {label} should delete"
                );
                assert!(
                    editor.document.markdown_text(cx).trim().is_empty(),
                    "fixture {label} should be emptied by delete"
                );
            });
        }
        cx.quit();
    }

    #[test]
    fn cross_block_selection_with_table_inside_quote_in_list_matches_source() {
        // 回归：引用内部的表格既带列表缩进又带引用前缀，整块复制曾丢失引用前缀
        // （产物 `  | a | b |` 会脱离引用）。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let doc = "- item\n  > quote\n  > | a | b |\n  > | --- | --- |\n  > | 1 | 2 |";
        let editor = cx.new(|cx| Editor::from_markdown(cx, doc.to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let last = visible.len() - 1;
            assert_eq!(visible[last].entity.read(cx).kind(), BlockKind::Table);
            let source = editor.document.markdown_text(cx);
            assert_eq!(source, doc, "引用内表格应保持单一缩进");

            let end_len = visible[last].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, last, end_len, cx);
            assert_eq!(
                editor.cross_block_selected_markdown(cx).as_deref(),
                Some(source.as_str())
            );
        });
        cx.quit();
    }

    #[test]
    fn cross_block_selection_with_nested_code_block_matches_source() {
        // 回归：嵌套在列表项里的代码块，源码映射曾漏掉开/闭围栏行的列表缩进，
        // 使该块区间比真实源码短；跨块选区会在代码块处截断（丢失闭围栏）。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "- item with code block\n\n      let x = 1;\n      let y = 2;".to_string(),
                None,
            )
        });

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(
                visible.len(),
                2,
                "fixture should parse to list item + code block"
            );
            let source = editor.document.markdown_text(cx);
            assert!(
                source.contains("  ```"),
                "fixture should keep the fence indented under the list item: {source:?}"
            );

            let end_len = visible[1].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, 1, end_len, cx);
            let selected = editor
                .cross_block_selected_markdown(cx)
                .expect("selection markdown");
            assert!(
                selected.contains("  ```\n  let x = 1;\n  let y = 2;\n  ```"),
                "nested code block should be selected whole: {selected:?}"
            );
        });
        cx.quit();
    }

    #[test]
    fn lone_separator_is_selectable_and_deletable() {
        // 回归：分隔线没有可见文本长度，旧逻辑在「同块同偏移」时判定选区为空，
        // 既选不中也删不掉；现按整块原子处理。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "alpha\n\n---\n\ngamma".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3);
            assert!(visible[1].entity.read(cx).kind().is_separator());

            set_selection(editor, 1, 0, 1, 0, cx);
            assert_eq!(
                editor.cross_block_selected_markdown(cx).as_deref(),
                Some("---")
            );
            assert_eq!(
                visible[1].entity.read(cx).editor_selection_range,
                Some(0..0)
            );

            assert!(editor.delete_cross_block_selection(cx));
            assert_eq!(editor.document.markdown_text(cx), "alpha\n\ngamma");
        });
        cx.quit();
    }

    #[test]
    fn deleting_lone_separator_at_document_edges_leaves_no_blank_block() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);

        // 分隔线位于文档开头。
        let editor = cx.new(|cx| Editor::from_markdown(cx, "---\n\nalpha".to_string(), None));
        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert!(visible[0].entity.read(cx).kind().is_separator());
            set_selection(editor, 0, 0, 0, 0, cx);
            assert!(editor.delete_cross_block_selection(cx));
            assert_eq!(editor.document.markdown_text(cx), "alpha");
        });

        // 分隔线位于文档结尾。
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\n---".to_string(), None));
        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert!(visible[1].entity.read(cx).kind().is_separator());
            set_selection(editor, 1, 0, 1, 0, cx);
            assert!(editor.delete_cross_block_selection(cx));
            assert_eq!(editor.document.markdown_text(cx), "alpha");
        });
        cx.quit();
    }

    #[test]
    fn mouse_down_on_separator_starts_whole_block_selection() {
        // 回归：在分隔线上按下鼠标即整块选中，拖拽不再被「同块不建选区」挡住。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "alpha\n\n---\n\ngamma".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            assign_visible_block_bounds(editor, cx);
            // 第二块（分隔线）的边界在 y=32..56。
            editor.begin_cross_block_drag_at_point(point(px(8.0), px(40.0)), cx);
            assert!(editor.cross_block_selection.is_some());
            assert_eq!(
                editor.cross_block_selected_markdown(cx).as_deref(),
                Some("---")
            );
        });
        cx.quit();
    }

    #[test]
    fn rendered_separator_records_bounds_so_clicks_hit_it() {
        // 回归：分隔线没有文本元素，旧的渲染路径从不记录 last_bounds，
        // 于是「按点命中」永远跳过它，鼠标点上去只会锚到相邻块。
        // 现在渲染时补记边界，点在自己记录到的中心即可整块选中。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let (editor, cx) = cx.add_window_view(|_window, cx| {
            Editor::from_markdown(cx, "alpha\n\n---\n\ngamma".to_string(), None)
        });
        redraw(cx);

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let separator = &visible[1].entity;
            assert!(separator.read(cx).kind().is_separator(), "第二块应是分隔线");
            let bounds = separator
                .read(cx)
                .last_bounds
                .expect("分隔线渲染后应记录块边界");
            assert!(bounds.size.height > px(0.0), "记录到的边界不应退化为零高");

            let center = bounds.center();
            editor.begin_cross_block_drag_at_point(center, cx);
            assert_eq!(
                editor.cross_block_selected_markdown(cx).as_deref(),
                Some("---")
            );
        });
        cx.quit();
    }

    #[test]
    fn clicking_rendered_separator_selects_it_through_real_mouse_events() {
        // 端到端：走真实的「捕获按下 + 块自身按下/松开」链路，确认点分隔线后
        // 选区不会被块内聚焦逻辑清掉。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let (editor, cx) = cx.add_window_view(|_window, cx| {
            Editor::from_markdown(cx, "alpha\n\n---\n\ngamma".to_string(), None)
        });
        redraw(cx);

        let center = editor.read_with(cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert!(
                visible[1].entity.read(cx).kind().is_separator(),
                "第二块应是分隔线"
            );
            visible[1]
                .entity
                .read(cx)
                .last_bounds
                .expect("分隔线渲染后应记录块边界")
                .center()
        });

        cx.simulate_click(center, rgpui::Modifiers::none());
        redraw(cx);

        editor.read_with(cx, |editor, cx| {
            assert_eq!(
                editor.cross_block_selected_markdown(cx).as_deref(),
                Some("---"),
                "点击分隔线后应整块选中"
            );
        });
        cx.quit();
    }

    #[test]
    fn rendered_list_group_separator_row_collapses_to_zero_height() {
        // 回归：相邻列表组之间的空段落渲染时应折叠为零高（对齐 Typora），
        // 而不是占满一个常规段落行高。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let (editor, cx) = cx.add_window_view(|_window, cx| {
            Editor::from_markdown(cx, "- a\n- b\n\n1. c\n2. d".to_string(), None)
        });
        redraw(cx);

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 5, "应为两个列表 + 中间空段落");
            let top = |index: usize| {
                visible[index]
                    .entity
                    .read(cx)
                    .last_bounds
                    .expect("渲染后应记录块边界")
                    .origin
                    .y
            };
            let normal_stride = top(1) - top(0);
            assert!(
                normal_stride > px(20.0),
                "常规行应占据一个行高: {normal_stride:?}"
            );
            let separator_stride = top(3) - top(2);
            assert!(
                separator_stride < px(20.0),
                "列表组分隔空段落应折叠为零高，实际步进 {separator_stride:?}"
            );
        });
        cx.quit();
    }

    #[test]
    fn mouse_down_on_table_frame_starts_whole_block_selection() {
        // 回归：表格没有可见文本，点表格框（非单元格处）应整块选中，便于复制/删除。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "alpha\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\ngamma".to_string(),
                None,
            )
        });

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(visible.len(), 3, "应为段落 + 表格 + 段落");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Table);

            assign_visible_block_bounds(editor, cx);
            // 表格边界在 y=32..56；单元格尚无边界，落点即表格框。
            editor.begin_cross_block_drag_at_point(point(px(8.0), px(40.0)), cx);
            assert!(editor.cross_block_selection.is_some());
            let selected = editor
                .cross_block_selected_markdown(cx)
                .expect("selection markdown");
            assert!(
                selected.contains("| a | b |"),
                "整块选区应包含表格: {selected:?}"
            );
            assert_eq!(
                visible[1].entity.read(cx).editor_selection_range,
                Some(0..0)
            );
        });
        cx.quit();
    }

    #[test]
    fn mouse_down_on_table_cell_does_not_select_whole_table() {
        // 点单元格应进入单元格编辑，而不是整块选中表格。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "alpha\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\ngamma".to_string(),
                None,
            )
        });

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assign_visible_block_bounds(editor, cx);

            // 给表头第一个单元格补上覆盖点击点的边界。
            let header_cell = {
                let table = visible[1].entity.read(cx);
                table.table_runtime.as_ref().expect("表格应有运行时").header[0].clone()
            };
            header_cell.update(cx, |cell, _cx| {
                cell.last_bounds = Some(Bounds::new(
                    point(px(0.0), px(32.0)),
                    size(px(200.0), px(24.0)),
                ));
            });

            editor.begin_cross_block_drag_at_point(point(px(8.0), px(40.0)), cx);
            assert!(
                editor.cross_block_selection.is_none(),
                "点单元格不应整块选中表格"
            );
        });
        cx.quit();
    }

    #[test]
    fn rendered_table_records_bounds_for_point_hit_testing() {
        // 回归：表格不走通用文本路径，旧渲染从不记录 last_bounds，导致按点命中
        // 永远跳过表格。现在渲染时补记边界，表格框点击才能整块选中。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let (editor, cx) = cx.add_window_view(|_window, cx| {
            Editor::from_markdown(
                cx,
                "alpha\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\ngamma".to_string(),
                None,
            )
        });
        redraw(cx);

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let table = &visible[1].entity;
            assert_eq!(table.read(cx).kind(), BlockKind::Table);
            let bounds = table.read(cx).last_bounds.expect("表格渲染后应记录块边界");
            assert!(bounds.size.height > px(0.0), "记录到的边界不应退化为零高");
        });
        cx.quit();
    }

    #[test]
    fn cross_block_selection_keeps_list_child_separator_rules() {
        // 回归：列表项与其子块之间，源码/权威序列化只在「纯段落子块」前加空行；
        // 旧的扁平拼接对所有子块都按根层规则多插一个空行，粘贴后子块会脱离列表。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);

        // 代码块子块：紧贴列表项，无空行。
        let editor = cx
            .new(|cx| Editor::from_markdown(cx, "- item\n  ```\n  code\n  ```".to_string(), None));
        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(
                visible.len(),
                2,
                "fixture should parse to list item + code block"
            );
            let source = editor.document.markdown_text(cx);
            let end_len = visible[1].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, 1, end_len, cx);
            let selected = editor
                .cross_block_selected_markdown(cx)
                .expect("selection markdown");
            assert_eq!(selected, source, "跨块选区应与权威序列化一致");
            assert!(
                !selected.contains("item\n\n"),
                "列表项与其代码块之间不应有空行: {selected:?}"
            );
        });

        // 段落子块：源码里有空行，选区必须保留。
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- item\n\n  para".to_string(), None));
        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            assert_eq!(
                visible.len(),
                2,
                "fixture should parse to list item + paragraph"
            );
            let source = editor.document.markdown_text(cx);
            let end_len = visible[1].entity.read(cx).plain_len();
            set_selection(editor, 0, 0, 1, end_len, cx);
            let selected = editor
                .cross_block_selected_markdown(cx)
                .expect("selection markdown");
            assert_eq!(selected, source, "跨块选区应与权威序列化一致");
            assert!(
                selected.contains("item\n\n  para"),
                "列表项与其段落子块之间应保留空行: {selected:?}"
            );

            // 删除同样依赖映射区间：修正前区间整体前移 1，会漏删末字符。
            assert!(editor.delete_cross_block_selection(cx));
            let remaining = editor.document.markdown_text(cx);
            assert!(
                !remaining.contains("item") && !remaining.contains("para"),
                "删除后不应残留被选内容: {remaining:?}"
            );
        });

        cx.quit();
    }

    #[test]
    fn shift_down_at_last_line_extends_selection_into_next_block() {
        // 未渲染（无布局）的块内无法再往下走，焦点端应接入下一块顶部，
        // 锚点块的原生选区收起为单光标，高亮统一交给跨块选区。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let first_id = visible[0].entity.entity_id();
            let second_id = visible[1].entity.entity_id();
            visible[0].entity.update(cx, |block, _cx| {
                block.selected_range = 2..5;
                block.selection_reversed = false;
            });

            editor.extend_vertical_selection(&visible[0].entity, 1, 0.0, cx);

            let selection = editor
                .cross_block_selection
                .expect("越过块边界后应生成跨块选区");
            assert_eq!(selection.anchor.entity_id, first_id);
            assert_eq!(selection.anchor.offset, 2, "锚点应固定在选区起点");
            assert_eq!(selection.focus.entity_id, second_id);
            assert_eq!(selection.focus.offset, 0, "从上往下进入块时落在首行行首");

            let first = visible[0].entity.read(cx);
            assert_eq!(first.selected_range, 2..2, "锚点块原生选区应收起");
            assert_eq!(first.caret_offset(), 2);
            assert_eq!(visible[1].entity.read(cx).caret_offset(), 0);
            assert_eq!(editor.active_entity_id, Some(second_id));
        });
        cx.quit();
    }

    #[test]
    fn shift_up_from_block_start_extends_selection_into_previous_block() {
        // 反向：从下一块行首往上，焦点端落到上一块末行行尾。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let first_id = visible[0].entity.entity_id();
            let second_id = visible[1].entity.entity_id();
            visible[1].entity.update(cx, |block, _cx| {
                block.selected_range = 0..0;
                block.selection_reversed = false;
            });

            editor.extend_vertical_selection(&visible[1].entity, -1, 0.0, cx);

            let selection = editor
                .cross_block_selection
                .expect("向上越过块边界后应生成跨块选区");
            assert_eq!(selection.anchor.entity_id, second_id);
            assert_eq!(selection.anchor.offset, 0);
            assert_eq!(selection.focus.entity_id, first_id);
            let first_len = visible[0].entity.read(cx).plain_len();
            assert_eq!(
                selection.focus.offset, first_len,
                "从下往上进入块时应落在末行行尾"
            );
            assert_eq!(editor.active_entity_id, Some(first_id));
        });
        cx.quit();
    }

    #[test]
    fn shift_back_into_anchor_block_collapses_cross_block_selection() {
        // 焦点端退回锚点块：应恢复块内选区，不残留单点跨块选区。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let first_id = visible[0].entity.entity_id();
            visible[0].entity.update(cx, |block, _cx| {
                block.selected_range = 2..5;
                block.selection_reversed = false;
            });

            editor.extend_vertical_selection(&visible[0].entity, 1, 0.0, cx);
            assert!(editor.cross_block_selection.is_some());

            editor.extend_vertical_selection(&visible[1].entity, -1, 0.0, cx);

            assert!(
                editor.cross_block_selection.is_none(),
                "退回锚点块后不应保留跨块选区"
            );
            let first = visible[0].entity.read(cx);
            assert_eq!(first.selected_range, 2..5);
            assert_eq!(first.caret_offset(), 5);
            assert_eq!(editor.active_entity_id, Some(first_id));
            assert!(
                editor
                    .document
                    .visible_blocks()
                    .iter()
                    .all(|visible| visible.entity.read(cx).editor_selection_range.is_none())
            );
        });
        cx.quit();
    }

    #[test]
    fn shift_right_moves_cross_block_focus_endpoint() {
        // 跨块选区存在时，Shift+右应移动跨块焦点端点，而不是只改块内被覆盖的原生选区。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let first_id = visible[0].entity.entity_id();
            let second_id = visible[1].entity.entity_id();
            set_selection(editor, 0, 2, 1, 1, cx);

            editor.extend_horizontal_selection(
                &visible[1].entity,
                HorizontalSelectMotion::CharRight,
                0,
                cx,
            );

            let selection = editor.cross_block_selection.expect("跨块选区应保留");
            assert_eq!(selection.anchor.entity_id, first_id);
            assert_eq!(selection.anchor.offset, 2, "锚点不应移动");
            assert_eq!(selection.focus.entity_id, second_id);
            assert_eq!(selection.focus.offset, 2, "焦点端点应向右扩一个字符");
            assert_eq!(
                visible[0].entity.read(cx).editor_selection_range,
                Some(2..5)
            );
            assert_eq!(
                visible[1].entity.read(cx).editor_selection_range,
                Some(0..2)
            );
            assert_eq!(
                visible[1].entity.read(cx).caret_offset(),
                2,
                "光标应跟随焦点端点"
            );
        });
        cx.quit();
    }

    #[test]
    fn shift_right_dispatched_from_anchor_block_moves_focus_endpoint() {
        // 拖拽结束后键盘焦点通常仍在锚点块，按键由锚点块派发也要推动焦点端点。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let second_id = visible[1].entity.entity_id();
            set_selection(editor, 0, 2, 1, 1, cx);

            editor.extend_horizontal_selection(
                &visible[0].entity,
                HorizontalSelectMotion::CharRight,
                0,
                cx,
            );

            let selection = editor.cross_block_selection.expect("跨块选区应保留");
            assert_eq!(selection.anchor.offset, 2);
            assert_eq!(selection.focus.entity_id, second_id);
            assert_eq!(selection.focus.offset, 2, "焦点端点仍应向右扩展");
            assert_eq!(
                visible[1].entity.read(cx).editor_selection_range,
                Some(0..2)
            );
        });
        cx.quit();
    }

    #[test]
    fn shift_left_at_focus_block_start_crosses_to_previous_block_end() {
        // 焦点端在块首继续左移：接入上一可见块末尾，锚点保持不变。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta\n\ngamma".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let first_id = visible[0].entity.entity_id();
            let second_id = visible[1].entity.entity_id();
            set_selection(editor, 0, 2, 2, 0, cx);

            editor.extend_horizontal_selection(
                &visible[2].entity,
                HorizontalSelectMotion::CharLeft,
                0,
                cx,
            );

            let selection = editor.cross_block_selection.expect("跨块选区应保留");
            assert_eq!(selection.anchor.entity_id, first_id);
            assert_eq!(selection.anchor.offset, 2);
            assert_eq!(selection.focus.entity_id, second_id);
            let second_len = visible[1].entity.read(cx).plain_len();
            assert_eq!(selection.focus.offset, second_len, "应落在上一块末尾");
            assert_eq!(
                visible[1].entity.read(cx).editor_selection_range,
                Some(0..second_len)
            );
            assert_eq!(visible[2].entity.read(cx).editor_selection_range, None);
            assert_eq!(editor.active_entity_id, Some(second_id));
        });
        cx.quit();
    }

    #[test]
    fn empty_focus_endpoint_still_marked_as_cross_block_selected() {
        // 焦点端落在块首时区间为空，但仍属于跨块选区：输入路径靠标记判定替换。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            set_selection(editor, 0, 2, 1, 0, cx);

            assert!(visible[0].entity.read(cx).cross_block_selected);
            assert!(visible[1].entity.read(cx).cross_block_selected);
            assert_eq!(
                visible[1].entity.read(cx).editor_selection_range,
                None,
                "空区间不应进入绘制路径"
            );

            editor.clear_cross_block_selection(cx);
            assert!(
                visible
                    .iter()
                    .all(|visible| !visible.entity.read(cx).cross_block_selected),
                "清除跨块选区后标记应复位"
            );
        });
        cx.quit();
    }

    #[test]
    fn navigation_collapses_cross_block_selection_to_matching_endpoint() {
        // 方向键折叠：左/上落到选区起点，右/下落到终点。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let first_id = visible[0].entity.entity_id();
            let second_id = visible[1].entity.entity_id();

            set_selection(editor, 0, 2, 1, 3, cx);
            editor.collapse_cross_block_selection_for_navigation(false, cx);
            assert!(editor.cross_block_selection.is_none());
            assert_eq!(visible[0].entity.read(cx).selected_range, 2..2);
            assert_eq!(visible[0].entity.read(cx).caret_offset(), 2);
            assert_eq!(editor.active_entity_id, Some(first_id));

            set_selection(editor, 0, 2, 1, 3, cx);
            editor.collapse_cross_block_selection_for_navigation(true, cx);
            assert!(editor.cross_block_selection.is_none());
            assert_eq!(visible[1].entity.read(cx).selected_range, 3..3);
            assert_eq!(visible[1].entity.read(cx).caret_offset(), 3);
            assert_eq!(editor.active_entity_id, Some(second_id));
            assert!(
                editor
                    .document
                    .visible_blocks()
                    .iter()
                    .all(|visible| !visible.entity.read(cx).cross_block_selected)
            );
        });
        cx.quit();
    }

    #[test]
    fn shift_left_into_anchor_block_collapses_cross_block_selection() {
        // 焦点端退回锚点块：收敛为块内原生选区，跨块高亮全部清除。
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let first_id = visible[0].entity.entity_id();
            set_selection(editor, 0, 2, 1, 0, cx);

            editor.extend_horizontal_selection(
                &visible[1].entity,
                HorizontalSelectMotion::CharLeft,
                0,
                cx,
            );

            assert!(
                editor.cross_block_selection.is_none(),
                "退回锚点块后不应保留跨块选区"
            );
            let first = visible[0].entity.read(cx);
            assert_eq!(first.selected_range, 2..5);
            assert_eq!(first.caret_offset(), 5);
            assert_eq!(editor.active_entity_id, Some(first_id));
            assert!(
                editor
                    .document
                    .visible_blocks()
                    .iter()
                    .all(|visible| visible.entity.read(cx).editor_selection_range.is_none())
            );
        });
        cx.quit();
    }
}
