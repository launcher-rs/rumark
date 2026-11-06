//! 外部 Markdown 文件拖放：替换当前编辑器窗口的文档。

use std::path::{Path, PathBuf};

use anyhow::Result;
use rgpui::*;

use super::{Editor, ViewMode};
use crate::components::BlockRecord;
use crate::i18n::I18nManager;

impl Editor {
    /// 判断路径是否为 Markdown 文件（`md`/`markdown` 后缀）。
    pub(super) fn is_markdown_file_path(path: &Path) -> bool {
        path.is_file()
            && path.extension().is_some_and(|extension| {
                extension.to_string_lossy().eq_ignore_ascii_case("md")
                    || extension.to_string_lossy().eq_ignore_ascii_case("markdown")
            })
    }

    /// 从拖放路径列表中取出第一个 Markdown 文件路径。
    pub(super) fn first_dropped_markdown_path(paths: &[PathBuf]) -> Option<PathBuf> {
        paths
            .iter()
            .find(|path| Self::is_markdown_file_path(path))
            .cloned()
    }

    /// 处理外部路径拖放：非 Markdown 提示，Markdown 则请求替换文档。
    pub(crate) fn on_external_paths_drop(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = Self::first_dropped_markdown_path(paths.paths()) else {
            let message = cx
                .global::<I18nManager>()
                .t("dialog_drop_replace_non_markdown", &[]);
            self.show_drop_open_failed_prompt(message, window, cx);
            return;
        };

        self.request_dropped_markdown_replace(path, window, cx);
    }

    /// 请求用拖放的 Markdown 文件替换当前文档（脏文档先弹窗确认）。
    pub(crate) fn request_dropped_markdown_replace(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 确认类对话框已开时不关栈顶（避免关掉自己的确认框），直接层叠新框。
        if !self.show_unsaved_changes_dialog && !self.show_drop_replace_dialog {
            // 替换确认弹窗打开前，先关掉可能开着的框架对话框（如关于）。
            window.close_dialog(cx);
        }
        self.dismiss_contextual_overlays(cx);

        if self.document_dirty {
            self.pending_drop_replace_path = Some(path);
            self.pending_drop_replace_after_save = false;
            if !self.show_drop_replace_dialog {
                self.drop_replace_restore_focus = self.document.focused_block_entity_id(window, cx);
                self.show_drop_replace_dialog = true;
                window.blur();
                self.open_drop_replace_dialog(window, cx);
            }
            cx.notify();
            return;
        }

        match self.replace_document_from_path(&path, cx) {
            Ok(()) => window.set_window_edited(false),
            Err(err) => self.show_drop_open_failed_prompt(
                Self::open_error_detail(&err, &path, cx),
                window,
                cx,
            ),
        }
    }

    /// 判断窗口是否为「用户尚未输入任何内容的空白文档」。
    ///
    /// 条件：未关联文件路径、无未保存修改，且文档仅含一个空根段落。
    /// 供「打开文件」优先复用当前空窗口，避免再新建一个与旧窗口完全重叠的窗口。
    pub(crate) fn is_blank_document(&self, cx: &App) -> bool {
        if self.file_path.is_some() || self.document_dirty {
            return false;
        }
        match self.document.root_blocks() {
            [] => true,
            [only] => Self::is_empty_root_paragraph(only.read(cx)),
            _ => false,
        }
    }

    /// 从指定路径读取文件并替换当前文档。
    pub(crate) fn replace_document_from_path(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let markdown = super::read_markdown_file(path)?;
        self.replace_document_from_markdown(markdown, Some(path.to_path_buf()), cx);
        crate::app_menu::record_recent_file_from_editor(path, cx);
        Ok(())
    }

    /// 用给定 Markdown 文本重建文档并重置编辑器状态。
    pub(super) fn replace_document_from_markdown(
        &mut self,
        markdown: String,
        file_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        let mut roots = Self::build_root_blocks_from_markdown(cx, &normalized);
        if roots.is_empty() {
            roots.push(Self::new_block(cx, BlockRecord::paragraph(String::new())));
        }

        self.file_path = file_path;
        self.view_mode = ViewMode::Rendered;
        self.document.replace_roots(roots, cx);
        self.table_cells.clear();
        self.rebuild_table_runtimes(cx);
        self.rebuild_image_runtimes(cx);

        self.document_dirty = false;
        self.pending_window_edited = false;
        self.pending_window_title_refresh = true;
        self.pending_save = false;
        self.pending_save_as = false;
        self.pending_open_link = None;
        self.pending_close_after_save = false;
        self.close_dialog_restore_focus = None;
        self.show_unsaved_changes_dialog = false;
        self.clear_pending_drop_replace_state(cx);
        self.dismiss_contextual_overlays(cx);
        self.table_axis_preview = None;
        self.table_axis_selection = None;
        self.sync_table_axis_visuals(cx);
        self.clear_cross_block_selection(cx);

        self.pending_scroll_active_block_into_view = true;
        self.pending_scroll_recheck_after_layout = true;
        self.last_scroll_viewport_size = None;
        self.scroll_handle.set_offset(point(px(0.0), px(0.0)));
        self.pending_focus = self.first_focusable_entity_id(cx);
        self.active_entity_id = self.pending_focus;

        self.undo_history.clear();
        self.redo_history.clear();
        self.pending_undo_capture = None;
        self.last_selection_snapshot = Self::empty_selection_snapshot();
        self.last_stable_source_text = normalized;
        self.history_restore_in_progress = false;
        self.refresh_stable_document_snapshot(cx);
        self.sync_workspace_after_document_path_change(cx);
        cx.notify();
    }

    /// 取消拖放替换对话框并恢复焦点。
    pub(crate) fn cancel_drop_replace_dialog(&mut self, cx: &mut Context<Self>) {
        let restore_focus = self.drop_replace_restore_focus.take();
        self.clear_pending_drop_replace_state(cx);
        if let Some(focus_id) = restore_focus {
            self.pending_focus = Some(focus_id);
            self.pending_scroll_active_block_into_view = true;
        }
        cx.notify();
    }

    /// 放弃当前修改并用拖放文件替换文档。
    pub(crate) fn discard_pending_drop_replace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.pending_drop_replace_path.take() else {
            self.clear_pending_drop_replace_state(cx);
            return;
        };

        self.clear_pending_drop_replace_state(cx);
        match self.replace_document_from_path(&path, cx) {
            Ok(()) => window.set_window_edited(false),
            Err(err) => self.show_drop_open_failed_prompt(
                Self::open_error_detail(&err, &path, cx),
                window,
                cx,
            ),
        }
    }

    /// 先保存当前文档再替换为拖放的文件。
    pub(crate) fn save_and_replace_pending_drop(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_drop_replace_path.is_none() {
            self.clear_pending_drop_replace_state(cx);
            return;
        }

        self.show_drop_replace_dialog = false;
        self.pending_drop_replace_after_save = true;

        if let Some(path) = self.file_path.clone() {
            if self.save_to_existing_path(&path, window, cx) {
                self.replace_after_successful_save(window, cx);
            } else {
                self.abort_pending_drop_replace_after_save(cx);
            }
            return;
        }

        self.save_via_prompt_then_replace_drop(window, cx);
        cx.notify();
    }

    /// 点击取消拖放替换对话框。
    pub(crate) fn on_cancel_drop_replace_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_drop_replace_dialog(cx);
        window.close_dialog(cx);
    }

    /// 点击放弃并替换：丢弃当前修改后载入拖放文件。
    pub(crate) fn on_discard_and_replace_drop(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.discard_pending_drop_replace(window, cx);
        window.close_dialog(cx);
    }

    /// 点击保存并替换：保存后载入拖放文件。
    pub(crate) fn on_save_and_replace_drop(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_and_replace_pending_drop(window, cx);
        window.close_dialog(cx);
    }

    /// 保存成功后执行挂起的拖放替换。
    fn replace_after_successful_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drop_path) = self.pending_drop_replace_path.take() else {
            self.clear_pending_drop_replace_state(cx);
            return;
        };

        self.clear_pending_drop_replace_state(cx);
        match self.replace_document_from_path(&drop_path, cx) {
            Ok(()) => window.set_window_edited(false),
            Err(err) => self.show_drop_open_failed_prompt(
                Self::open_error_detail(&err, &drop_path, cx),
                window,
                cx,
            ),
        }
    }

    /// 无路径时弹框另存当前文档，保存成功后再替换拖放文件。
    fn save_via_prompt_then_replace_drop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drop_path) = self.pending_drop_replace_path.clone() else {
            self.clear_pending_drop_replace_state(cx);
            return;
        };
        let markdown = self.serialized_document_text(cx);
        let (default_dir, suggested_name) = self.save_dialog_defaults();
        let prompt = cx.prompt_for_new_path(&default_dir, suggested_name.as_deref());
        let weak_editor = cx.entity().downgrade();
        let weak_editor_for_cancel = weak_editor.clone();
        let weak_editor_for_error = weak_editor.clone();
        let weak_editor_for_write_error = weak_editor.clone();
        let window_handle = window.window_handle();

        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut save_path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => {
                    let _ = weak_editor_for_cancel.update(cx, |this, cx| {
                        this.abort_pending_drop_replace_after_save(cx);
                    });
                    return;
                }
                Ok(Err(err)) => {
                    let _ = weak_editor_for_error.update(cx, |this, cx| {
                        this.abort_pending_drop_replace_after_save(cx);
                    });
                    let detail = err.to_string();
                    let _ = cx.update_window(
                        window_handle,
                        move |_view: AnyView, window: &mut Window, cx: &mut App| {
                            let i18n = cx.global::<I18nManager>().snapshot();
                            let ok = i18n.t("dialog_ok", &[]);
                            let title = i18n.t("message_save_failed", &[]);
                            let buttons = [ok.as_str()];
                            // 对话框 future 必须轮询才会弹出：转交后台任务等待用户响应。
                            let prompt = window.prompt(
                                PromptLevel::Critical,
                                &title,
                                Some(&detail),
                                &buttons,
                                cx,
                            );
                            cx.spawn(async move |_| {
                                let _ = prompt.await;
                            })
                            .detach();
                        },
                    );
                    return;
                }
            };

            if save_path.extension().is_none() {
                save_path.set_extension("md");
            }

            if let Err(err) = crate::atomic_file::write_text(&save_path, &markdown) {
                let _ = weak_editor_for_write_error.update(cx, |this, cx| {
                    this.abort_pending_drop_replace_after_save(cx);
                });
                let detail = err.to_string();
                let _ = cx.update_window(
                    window_handle,
                    move |_view: AnyView, window: &mut Window, cx: &mut App| {
                        let i18n = cx.global::<I18nManager>().snapshot();
                        let ok = i18n.t("dialog_ok", &[]);
                        let title = i18n.t("message_save_failed", &[]);
                        let buttons = [ok.as_str()];
                        // 对话框 future 必须轮询才会弹出：转交后台任务等待用户响应。
                        let prompt = window.prompt(
                            PromptLevel::Critical,
                            &title,
                            Some(&detail),
                            &buttons,
                            cx,
                        );
                        cx.spawn(async move |_| {
                            let _ = prompt.await;
                        })
                        .detach();
                    },
                );
                return;
            }

            let saved_path = save_path.clone();
            let prompted_path = drop_path.clone();
            let replace_result = weak_editor.update(cx, move |this, cx| {
                this.apply_successful_save(saved_path, cx);
                this.pending_drop_replace_path = Some(drop_path);
                this.replace_after_successful_save_async(cx)
            });
            let _ = cx.update_window(
                window_handle,
                move |_view: AnyView, window: &mut Window, cx: &mut App| match replace_result {
                    Ok(Ok(())) => window.set_window_edited(false),
                    Ok(Err(err)) => {
                        let i18n = cx.global::<I18nManager>().snapshot();
                        let ok = i18n.t("dialog_ok", &[]);
                        let title = i18n.t("message_open_failed", &[]);
                        let detail = super::read_error_detail(&err, &prompted_path, &i18n);
                        let buttons = [ok.as_str()];
                        // 对话框 future 必须轮询才会弹出：转交后台任务等待用户响应。
                        let prompt = window.prompt(
                            PromptLevel::Critical,
                            &title,
                            Some(&detail),
                            &buttons,
                            cx,
                        );
                        cx.spawn(async move |_| {
                            let _ = prompt.await;
                        })
                        .detach();
                    }
                    Err(_) => {}
                },
            );
        })
        .detach();
    }

    /// 异步保存成功回调中执行挂起的拖放替换。
    fn replace_after_successful_save_async(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let Some(drop_path) = self.pending_drop_replace_path.take() else {
            self.clear_pending_drop_replace_state(cx);
            return Ok(());
        };

        self.clear_pending_drop_replace_state(cx);
        self.replace_document_from_path(&drop_path, cx)
    }

    /// 中止保存后替换流程并恢复焦点。
    fn abort_pending_drop_replace_after_save(&mut self, cx: &mut Context<Self>) {
        self.pending_drop_replace_after_save = false;
        self.show_drop_replace_dialog = false;
        self.pending_drop_replace_path = None;
        let restore_focus = self.drop_replace_restore_focus.take();
        if let Some(focus_id) = restore_focus {
            self.pending_focus = Some(focus_id);
            self.pending_scroll_active_block_into_view = true;
        }
        cx.notify();
    }

    /// 清理拖放替换相关的挂起状态。
    fn clear_pending_drop_replace_state(&mut self, cx: &mut Context<Self>) {
        let had_path = self.pending_drop_replace_path.take().is_some();
        let had_dialog = self.show_drop_replace_dialog;
        let had_after_save = self.pending_drop_replace_after_save;
        let had_restore_focus = self.drop_replace_restore_focus.take().is_some();
        let had_state = had_path || had_dialog || had_after_save || had_restore_focus;
        self.show_drop_replace_dialog = false;
        self.pending_drop_replace_after_save = false;
        if had_state {
            cx.notify();
        }
    }

    /// 弹出拖放文件打开失败的提示框。
    fn show_drop_open_failed_prompt(
        &self,
        detail: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let i18n = cx.global::<I18nManager>().snapshot();
        let ok = i18n.t("dialog_ok", &[]);
        let title = i18n.t("message_open_failed", &[]);
        let buttons = [ok.as_str()];
        // 对话框 future 必须轮询才会弹出：转交后台任务等待用户响应。
        let prompt = window.prompt(PromptLevel::Critical, &title, Some(&detail), &buttons, cx);
        cx.spawn(async move |_, _| {
            let _ = prompt.await;
        })
        .detach();
    }

    /// 拖放读取失败的展示文案：非 UTF-8 换成编码提示，其余沿用原始错误信息。
    fn open_error_detail(err: &anyhow::Error, path: &Path, cx: &App) -> String {
        let i18n = cx.global::<I18nManager>().snapshot();
        super::read_error_detail(err, path, &i18n)
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;
    use std::path::{Path, PathBuf};

    /// 在独立临时目录里创建若干文件并返回其路径（测试辅助）。
    fn temp_markdown_dir(files: &[(&str, &str)]) -> Vec<PathBuf> {
        let root = std::env::temp_dir().join(format!("rumark-file-drop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("temp dir should be created");
        files
            .iter()
            .map(|(name, content)| {
                let path = root.join(name);
                std::fs::write(&path, content).expect("temp file should be written");
                path
            })
            .collect()
    }

    #[test]
    fn markdown_suffixes_are_recognized_case_insensitively() {
        let paths = temp_markdown_dir(&[
            ("note.md", "# 标题"),
            ("NOTE.MARKDOWN", "正文"),
            ("notes.txt", "正文"),
        ]);

        assert!(Editor::is_markdown_file_path(&paths[0]));
        assert!(Editor::is_markdown_file_path(&paths[1]));
        assert!(!Editor::is_markdown_file_path(&paths[2]));
    }

    #[test]
    fn missing_paths_and_directories_are_not_markdown_files() {
        let root =
            std::env::temp_dir().join(format!("rumark-file-drop-dir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("temp dir should be created");

        // 目录即使带 .md 后缀也不算 Markdown 文件。
        let as_dir = root.join("package.md");
        std::fs::create_dir_all(&as_dir).expect("nested dir should be created");
        assert!(!Editor::is_markdown_file_path(&as_dir));

        let missing = Path::new(&root).join("missing.md");
        assert!(!Editor::is_markdown_file_path(&missing));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn first_dropped_markdown_path_skips_non_markdown_entries() {
        let paths = temp_markdown_dir(&[
            ("image.png", "二进制内容"),
            ("readme.txt", "说明"),
            ("doc.md", "# 文档"),
            ("other.markdown", "## 另一篇"),
        ]);

        let picked = Editor::first_dropped_markdown_path(&paths);
        assert_eq!(picked, Some(paths[2].clone()));
    }

    #[test]
    fn first_dropped_markdown_path_returns_none_without_markdown() {
        let paths = temp_markdown_dir(&[("a.png", "x"), ("b.txt", "y")]);

        assert_eq!(Editor::first_dropped_markdown_path(&paths), None);
        assert_eq!(Editor::first_dropped_markdown_path(&[]), None);
    }
}
