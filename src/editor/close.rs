//! 未保存修改对话框与窗口关闭拦截。
//!
//! 文档为脏时，`Editor::on_window_should_close` 返回
//! 假并弹出框架确认框，提供三种选择：保存后关闭、
//! 放弃并关闭、继续编辑。对话框关闭且未关闭窗口时，
//! 焦点恢复到之前活动的块。

use rgpui::*;

use super::Editor;

impl Editor {
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
            window.remove_window();
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
        self.pending_close_after_save = false;
        self.hide_unsaved_changes_dialog(cx);
        window.close_dialog(cx);
        self.restore_focus_after_close_dialog(cx);
    }

    /// 点击放弃并关闭：不保存直接移除窗口。
    pub(crate) fn on_discard_and_close(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_close_after_save = false;
        self.close_dialog_restore_focus = None;
        self.hide_unsaved_changes_dialog(cx);
        window.remove_window();
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
}
