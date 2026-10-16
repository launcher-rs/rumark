//! 表格轴右键菜单：框架 `PopupMenu` 的 Block 层宿主。
//!
//! 手柄右键链路保留 `stop_propagation`（防行级插入菜单误开），打开的菜单由
//! 表格块自持（一次一个，重复右键替换旧菜单），渲染侧经
//! `deferred + anchored` 挂载（照抄框架 `ContextMenu` 的挂载模式）。
//! 菜单项点击经 [`BlockEvent::RequestTableAxisOp`] 走既有块订阅路由到编辑器执行。

use rgpui::menu::{PopupMenu, PopupMenuItem};
use rgpui::*;

use super::{Block, BlockEvent};
use crate::components::{TableAxisKind, TableAxisMenuOp};
use crate::i18n::{I18nManager, I18nSnapshot};

/// 表格块自持的轴菜单状态（随 `DismissEvent` 清除，菜单实体随状态释放）。
pub(crate) struct TableAxisMenuState {
    /// 菜单锚定坐标（右键点击位置）。
    position: Point<Pixels>,
    /// 框架弹出菜单实体。
    menu: Entity<PopupMenu>,
    /// 菜单关闭订阅（持有以保活，同框架 `ContextMenu` 模式）。
    _subscription: Subscription,
}

/// 在右键坐标打开指定轴的框架弹出菜单（同时只开一个）。
pub(crate) fn open_table_axis_menu(
    block: &Entity<Block>,
    kind: TableAxisKind,
    index: usize,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let i18n = cx.global::<I18nManager>().snapshot();
    let menu = PopupMenu::build(window, cx, |menu, _window, cx| {
        build_table_axis_popup(menu, block, kind, index, &i18n, cx)
    });
    // 关闭时清除块持有的菜单状态（菜单实体随状态释放）。
    let weak = block.downgrade();
    let subscription = window.subscribe(&menu, cx, move |_, _: &DismissEvent, _, cx| {
        if let Some(block) = weak.upgrade() {
            block.update(cx, |block, cx| {
                block.table_axis_menu = None;
                cx.notify();
            });
        }
    });
    block.update(cx, |block, cx| {
        block.table_axis_menu = Some(TableAxisMenuState {
            position,
            menu: menu.clone(),
            _subscription: subscription,
        });
        cx.notify();
    });
    // 聚焦菜单以支持键盘导航（同框架 `ContextMenu`）。
    menu.focus_handle(cx).focus(window, cx);
    window.refresh();
}

/// 渲染块自持的轴菜单宿主（无菜单时返回 `None`）。
pub(crate) fn render_table_axis_menu(
    block: &Block,
    window: &mut Window,
    cx: &mut Context<Block>,
) -> Option<AnyElement> {
    let state = block.table_axis_menu.as_ref()?;
    let menu = state.menu.clone();
    let position = state.position;
    // 打开后聚焦菜单以支持键盘导航（同框架 `ContextMenu`）。
    if !menu.focus_handle(cx).contains_focused(window, cx) {
        menu.focus_handle(cx).focus(window, cx);
    }
    Some(
        deferred(
            anchored()
                .position(position)
                .snap_to_window_with_margin(px(8.0))
                .child(menu),
        )
        .with_priority(1)
        .into_any(),
    )
}

/// 构造单个轴菜单项（点击经块事件发往编辑器执行）。
fn axis_item(
    block: &Entity<Block>,
    kind: TableAxisKind,
    index: usize,
    op: TableAxisMenuOp,
    label: String,
    enabled: bool,
) -> PopupMenuItem {
    let block = block.clone();
    PopupMenuItem::new(label)
        .disabled(!enabled)
        .on_click(move |_, _, cx| {
            block.update(cx, |_, cx| {
                cx.emit(BlockEvent::RequestTableAxisOp { kind, index, op });
            });
        })
}

/// 构造轴菜单内容（构建时读块最新表格数据，启用态即时计算）。
fn build_table_axis_popup(
    menu: PopupMenu,
    block: &Entity<Block>,
    kind: TableAxisKind,
    index: usize,
    i18n: &I18nSnapshot,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    match kind {
        TableAxisKind::Column => {
            let column_count = block
                .read(cx)
                .record
                .table
                .as_ref()
                .map(|table| table.column_count())
                .unwrap_or(0);
            menu.item(axis_item(
                block,
                kind,
                index,
                TableAxisMenuOp::AlignColumnLeft,
                i18n.t("table_column_align_left", &[]),
                true,
            ))
            .item(axis_item(
                block,
                kind,
                index,
                TableAxisMenuOp::AlignColumnCenter,
                i18n.t("table_column_align_center", &[]),
                true,
            ))
            .item(axis_item(
                block,
                kind,
                index,
                TableAxisMenuOp::AlignColumnRight,
                i18n.t("table_column_align_right", &[]),
                true,
            ))
            .separator()
            .item(axis_item(
                block,
                kind,
                index,
                TableAxisMenuOp::MoveColumnLeft,
                i18n.t("table_column_move_left", &[]),
                index > 0,
            ))
            .item(axis_item(
                block,
                kind,
                index,
                TableAxisMenuOp::MoveColumnRight,
                i18n.t("table_column_move_right", &[]),
                index + 1 < column_count,
            ))
            .separator()
            .item(
                axis_item(
                    block,
                    kind,
                    index,
                    TableAxisMenuOp::DeleteColumn,
                    i18n.t("table_column_delete", &[]),
                    true,
                )
                .danger(true),
            )
        }
        TableAxisKind::Row => {
            let row_count = block
                .read(cx)
                .record
                .table
                .as_ref()
                .map(|table| table.rows.len())
                .unwrap_or(0);
            let mut menu = menu;
            // 表头行（可视序号 0）共用普通行菜单，顶部额外附带表头行样式开关。
            if index == 0 {
                let headers_shown = crate::config::EditorSettings::table_headers_visible(cx);
                let block = block.clone();
                menu = menu
                    .item(
                        PopupMenuItem::new(i18n.t("table_header_row", &[]))
                            .checked(headers_shown)
                            .on_click(move |_, _, cx| {
                                block.update(cx, |_, cx| {
                                    cx.emit(BlockEvent::RequestTableAxisOp {
                                        kind,
                                        index,
                                        op: TableAxisMenuOp::ToggleHeaders,
                                    });
                                });
                            }),
                    )
                    .separator();
            }
            menu.item(axis_item(
                block,
                kind,
                index,
                TableAxisMenuOp::MoveRowUp,
                i18n.t("table_row_move_up", &[]),
                index > 0,
            ))
            .item(axis_item(
                block,
                kind,
                index,
                TableAxisMenuOp::MoveRowDown,
                i18n.t("table_row_move_down", &[]),
                index < row_count,
            ))
            .separator()
            .item(
                axis_item(
                    block,
                    kind,
                    index,
                    TableAxisMenuOp::DeleteRow,
                    i18n.t("table_row_delete", &[]),
                    true,
                )
                .danger(true),
            )
        }
    }
}
