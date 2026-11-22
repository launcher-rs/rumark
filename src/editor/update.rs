//! 检查更新的 UI 驱动：手动触发、异步查询、结果提示与下载落地。
//!
//! 全部流程在 `cx.spawn` 后台任务中完成，网络请求经 `run_blocking` 放到独立
//! 线程，不阻塞渲染帧。所有对话框用 `window.prompt`（返回按钮索引）， awaiting
//! 结果后再决定下一步。

use rgpui::PromptLevel;
use rgpui::*;

use crate::config::EditorSettings;
use crate::i18n::I18nManager;
use crate::update as updater;

impl super::Editor {
    /// 从菜单/设置触发的「检查更新」入口（手动）。
    pub(crate) fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_busy {
            return;
        }
        self.update_busy = true;
        cx.notify();

        let mirrors = EditorSettings::custom_mirrors(cx);
        let weak = cx.entity().downgrade();
        let window_handle = window.window_handle();
        let current = updater::SemVer::current();
        let current_text = current.version_string();

        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let fetch_result =
                updater::run_blocking(move || updater::fetch_latest_release(&mirrors)).await;
            let i18n = cx.update(|cx| cx.global::<I18nManager>().snapshot());
            let error_title = i18n.t("update_error", &[]);

            let release = match fetch_result {
                Ok((release, _mirror)) => release,
                Err(err) => {
                    show_prompt(
                        cx,
                        window_handle,
                        &error_title,
                        &err.to_string(),
                        &[&i18n.t("dialog_ok", &[])],
                    )
                    .await;
                    finish(weak, cx);
                    return;
                }
            };

            let remote_version = match updater::parse_version(&release.tag_name) {
                Some(v) => v,
                None => {
                    let detail = format!("无法解析版本号：{}", release.tag_name);
                    show_prompt(
                        cx,
                        window_handle,
                        &error_title,
                        &detail,
                        &[&i18n.t("dialog_ok", &[])],
                    )
                    .await;
                    finish(weak, cx);
                    return;
                }
            };

            if !updater::is_newer(&remote_version, &current) {
                let message = i18n.t("update_none_message", &[]);
                let detail = i18n.t(
                    "update_current_version",
                    &[("version", current_text.as_str())],
                );
                show_prompt(
                    cx,
                    window_handle,
                    &message,
                    &detail,
                    &[&i18n.t("dialog_ok", &[])],
                )
                .await;
                finish(weak, cx);
                return;
            }

            // 发现新版本：展示版本、发布日期与更新说明，三个按钮。
            let title = i18n.t("update_available_title", &[]);
            let current_label = i18n.t(
                "update_current_version",
                &[("version", current_text.as_str())],
            );
            let new_label = i18n.t(
                "update_new_version",
                &[("version", remote_version.version_string().as_str())],
            );
            let published_label = i18n.t(
                "update_published_at",
                &[("date", release.published_at.as_str())],
            );
            let notes_label = i18n.t("update_notes", &[]);
            let detail = format!(
                "{current_label}\n{new_label}\n{published_label}\n\n{notes_label}\n{}",
                if release.body.trim().is_empty() {
                    "-".to_string()
                } else {
                    release.body.clone()
                }
            );
            let download_text = i18n.t("update_download", &[]);
            let later_text = i18n.t("update_later", &[]);
            let open_text = i18n.t("update_open_release", &[]);
            let answers = [
                download_text.as_str(),
                later_text.as_str(),
                open_text.as_str(),
            ];
            let choice = show_prompt(cx, window_handle, &title, &detail, &answers).await;

            match choice {
                0 => {
                    // 用户确认下载。资产名按平台拼出，镜像偏好重新读取。
                    let asset_name = updater::asset_name_for_platform();
                    let asset = match updater::find_asset(&release, asset_name) {
                        Ok(asset) => asset.clone(),
                        Err(err) => {
                            show_prompt(
                                cx,
                                window_handle,
                                &error_title,
                                &err.to_string(),
                                &[&i18n.t("dialog_ok", &[])],
                            )
                            .await;
                            finish(weak.clone(), cx);
                            return;
                        }
                    };
                    let mirrors = mirrors_for_download(cx);
                    let message = i18n.t("update_downloading", &[]);
                    let fail_title = i18n.t("update_download_failed", &[]);
                    let staged = run_stage(cx, asset, mirrors).await;
                    let ok_text = i18n.t("dialog_ok", &[]);
                    match staged {
                        Ok(path) => {
                            let detail = i18n.t(
                                "update_downloaded_restart",
                                &[("path", path.to_string_lossy().as_ref())],
                            );
                            show_prompt(cx, window_handle, &message, &detail, &[ok_text.as_str()])
                                .await;
                        }
                        Err(err) => {
                            show_prompt(
                                cx,
                                window_handle,
                                &fail_title,
                                &err.to_string(),
                                &[ok_text.as_str()],
                            )
                            .await;
                        }
                    }
                    finish(weak, cx);
                }
                2 => {
                    let _ = updater::run_blocking(updater::open_release_page).await;
                    finish(weak, cx);
                }
                _ => finish(weak, cx),
            }
        })
        .detach();
    }
}

/// 弹出提示并等待用户点击，返回按钮索引。
async fn show_prompt(
    cx: &mut AsyncApp,
    window_handle: AnyWindowHandle,
    title: &str,
    detail: &str,
    answers: &[&str],
) -> usize {
    let title = title.to_string();
    let detail = detail.to_string();
    let answers: Vec<String> = answers.iter().map(|s| s.to_string()).collect();
    let receiver = cx.update_window(
        window_handle,
        move |_view: AnyView, window: &mut Window, cx: &mut App| {
            let answer_refs: Vec<&str> = answers.iter().map(|s| s.as_str()).collect();
            window.prompt(PromptLevel::Info, &title, Some(&detail), &answer_refs, cx)
        },
    );
    match receiver {
        Ok(rx) => rx.await.unwrap_or(0),
        Err(_) => 0,
    }
}

/// 读取当前镜像偏好（在 async 上下文里经 App 读 global）。
fn mirrors_for_download(cx: &AsyncApp) -> Vec<String> {
    cx.update(|cx| EditorSettings::custom_mirrors(cx))
}

/// 后台下载并落地到 .new 文件。
async fn run_stage(
    cx: &mut AsyncApp,
    asset: updater::ReleaseAsset,
    mirrors: Vec<String>,
) -> anyhow::Result<std::path::PathBuf> {
    let dest = cx.update(|_cx| {
        crate::config::RumarkConfigDirs::from_system()
            .map(|dirs| dirs.updates_dir())
            .unwrap_or_else(|_| std::env::temp_dir())
    });
    updater::run_blocking(move || updater::download_and_stage(&asset, &mirrors, &dest)).await
}

/// 结束忙碌态。
fn finish(weak: WeakEntity<crate::editor::Editor>, cx: &mut AsyncApp) {
    let _ = weak.update(cx, |editor, cx| {
        editor.update_busy = false;
        cx.notify();
    });
}
