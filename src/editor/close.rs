//! 未保存修改对话框与窗口关闭拦截。
//!
//! 文档为脏时，`Editor::on_window_should_close` 返回
//! 假并弹出框架确认框，提供三种选择：保存后关闭、
//! 放弃并关闭、继续编辑。对话框关闭且未关闭窗口时，
//! 焦点恢复到之前活动的块。

use rgpui::*;

use super::{CloseScope, DocumentHost, Editor};

impl Editor {
    /// 绑定所属的多文档容器（由 host 创建 tab 时调用）。
    pub(crate) fn set_owner_host(&mut self, host: WeakEntity<DocumentHost>) {
        self.owner_host = Some(host);
    }

    /// 通知所属 host 在下一帧重算窗口标题与 edited 标记。
    /// 直接按实体 ID 通知，不 `update` host，避免编辑器 update 期间重入同一个 host。
    pub(crate) fn notify_host_window_chrome_refresh(&self, cx: &mut App) {
        if let Some(host) = &self.owner_host {
            cx.notify(host.entity_id());
        }
    }

    /// 设置宿主是否启用多标签页（单文档模式下拖入文件走替换当前文档）。
    pub(crate) fn set_multi_tab_enabled(&mut self, enabled: bool) {
        self.multi_tab_enabled = enabled;
    }

    /// 设置本次关闭的目标（窗口或所属 tab）。
    pub(crate) fn set_close_scope(&mut self, scope: CloseScope) {
        self.close_scope = scope;
    }

    /// 请求关闭当前窗口（先走未保存检查流程）。
    pub(crate) fn request_close_current_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 确认框已开时保持现状（与旧自研守卫同语义；此时挂起状态必为空）。
        if self.show_unsaved_changes_dialog {
            return;
        }
        // 先关掉可能开着的框架对话框（如关于），再走未保存检查流程。
        window.close_dialog(cx);
        self.pending_close_after_save = false;

        if self.on_window_should_close(window, cx) {
            self.close_dialog_restore_focus = None;
            self.finish_close(window, cx);
        }
    }

    /// 关闭对话框后恢复之前记录的焦点块。
    pub(crate) fn restore_focus_after_close_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(focus_id) = self.close_dialog_restore_focus.take() {
            self.pending_focus = Some(focus_id);
            self.pending_scroll_active_block_into_view = true;
            cx.notify();
        }
    }

    /// 隐藏未保存修改对话框。
    pub(crate) fn hide_unsaved_changes_dialog(&mut self, cx: &mut Context<Self>) {
        if self.show_unsaved_changes_dialog {
            self.show_unsaved_changes_dialog = false;
            cx.notify();
        }
    }

    /// 中止保存后关闭的挂起状态并恢复焦点。
    pub(crate) fn discard_pending_close_after_save(&mut self, cx: &mut Context<Self>) {
        let had_pending_close = self.pending_close_after_save;
        self.pending_close_after_save = false;
        self.hide_unsaved_changes_dialog(cx);
        if had_pending_close {
            self.restore_focus_after_close_dialog(cx);
        } else {
            self.close_dialog_restore_focus = None;
        }
    }

    /// 窗口关闭前检查：脏文档弹窗拦截，干净文档直接放行。
    pub(crate) fn on_window_should_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.document_dirty {
            return true;
        }

        // 确认框已开时保持现状，不重复打开。
        if self.show_unsaved_changes_dialog {
            return false;
        }

        // 未保存弹窗打开前，先关掉可能开着的框架对话框（如关于）。
        window.close_dialog(cx);
        self.close_dialog_restore_focus = self.document.focused_block_entity_id(window, cx);
        self.show_unsaved_changes_dialog = true;
        window.blur();
        self.open_unsaved_changes_dialog(window, cx);
        cx.notify();

        false
    }

    /// 点击取消关闭对话框：关闭弹窗并恢复焦点。
    pub(crate) fn on_cancel_close_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 用户选择「继续编辑」：若正处于退出流程，中止整个逐窗退出。
        crate::app_menu::cancel_quit_flow(cx);
        let flow_scope = self.close_scope;
        self.pending_close_after_save = false;
        self.close_scope = CloseScope::Window;
        self.hide_unsaved_changes_dialog(cx);
        window.close_dialog(cx);
        self.restore_focus_after_close_dialog(cx);
        // 关窗/退出或批量关闭流程被用户取消：通知宿主丢弃挂起流程。
        if flow_scope != CloseScope::Window
            && let Some(host) = self.owner_host.clone()
        {
            let _ = host.update(cx, move |host, cx| {
                if flow_scope == CloseScope::BatchFlow {
                    host.cancel_batch_close(cx);
                } else {
                    host.cancel_window_close(cx);
                }
            });
        }
    }

    /// 点击放弃并关闭：不保存直接移除窗口或所属 tab。
    pub(crate) fn on_discard_and_close(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_close_after_save = false;
        self.close_dialog_restore_focus = None;
        self.hide_unsaved_changes_dialog(cx);
        self.finish_close(window, cx);
    }

    /// 点击保存并关闭：标记挂起保存等待保存完成后关闭。
    pub(crate) fn on_save_and_close(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_close_after_save = true;
        self.hide_unsaved_changes_dialog(cx);
        window.close_dialog(cx);
        self.pending_save = true;
        cx.notify();
    }

    /// 按当前关闭目标结束：窗口直接移除，tab 则通知宿主移除本编辑器。
    ///
    /// standalone 编辑器没有宿主，`close_scope` 恒为 `Window`，语义与改造前一致。
    pub(crate) fn finish_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.close_scope, CloseScope::Tab | CloseScope::WindowFlow) {
            let host = self.owner_host.clone();
            if let Some(host) = host {
                let editor = cx.entity();
                let scope = self.close_scope;
                let window_handle = window.window_handle();
                // 关闭 tab 的请求本身往往在 host.update 内发起（窗口级动作），
                // 此处若同步再 update host 会重入同一实体而 panic，故延后一拍。
                cx.spawn(async move |_this, cx| {
                    let _ = host.update(cx, move |host, cx| {
                        host.close_editor_tab(editor, window_handle, cx);
                        // 主动关 tab 前清掉可能残留的关窗流程（如上次取消）。
                        if scope == CloseScope::Tab {
                            host.cancel_window_close(cx);
                        }
                    });
                    let _ = window_handle.update(cx, |_view, window, cx| {
                        window.close_dialog(cx);
                    });
                })
                .detach();
                return;
            }
        }
        window.remove_window();
    }
}
