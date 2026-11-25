//! 检查更新的 UI 驱动：偏好 → 组件调用 → 更新对话框 → 状态栏下载进度。
//!
//! 版本比较 / 镜像回退 / 下载解压全部由 `rgpui-extra` 组件负责，
//! 本文件只做应用侧编排（i18n 文案、对话框、进度回传）。

use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

use rgpui::*;
use rgpui_extra::updater::ui::{
    UpdateChoice, UpdateDialogContent, UpdateDialogStrings, UpdatePhase, UpdateStatus,
    render_update_dialog,
};

use crate::config::{EditorSettings, RumarkConfigDirs};
use crate::i18n::{I18nManager, I18nSnapshot};
use crate::update as updater;

/// 更新对话框需要的数据（下载时据此选择资产）。
#[derive(Clone, Debug)]
pub(crate) struct UpdateDialogState {
    pub(crate) current: String,
    pub(crate) remote: String,
    pub(crate) published: String,
    pub(crate) notes: String,
    pub(crate) asset: Option<updater::ReleaseAsset>,
    /// 本地已暂存且版本匹配的更新版本号；`Some` 时可直接安装，无需重新下载。
    pub(crate) staged_version: Option<String>,
}

impl super::Editor {
    /// 从菜单 / 偏好设置触发的手动检查更新。
    pub(crate) fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_status.phase != UpdatePhase::Idle {
            return;
        }
        self.update_status = UpdateStatus::checking();
        cx.notify();

        let configs = update_configs(cx);
        let staged_config = configs[0].clone();
        let weak = cx.entity().downgrade();
        let window_handle = window.window_handle();

        cx.spawn(async move |_this, cx: &mut AsyncApp| {
            let result = updater::run_blocking(move || {
                updater::try_configs(configs, |config| {
                    updater::check(config, updater::CURRENT_VERSION)
                })
            })
            .await;
            // 结果已拿到：提示框会等待用户点击，先收起状态栏进度，
            // 避免「已是最新版本」弹窗旁边还挂着「正在检查更新…」。
            let _ = weak.update(cx, |editor, cx| {
                editor.update_status = UpdateStatus::default();
                cx.notify();
            });
            let i18n = cx.update(|cx| cx.global::<I18nManager>().snapshot());
            let ok_label = i18n.t("dialog_ok", &[]);

            match result {
                Ok(check) if check.newer => {
                    let remote_version = check.remote.version_string();
                    // 若上次下载后点了「稍后」，本地仍留着对应版本的暂存文件。
                    // 检测到就直接提供「安装已下载」入口，避免重复下载。
                    let staged_version = cx.update(|_cx| {
                        let dirs = RumarkConfigDirs::from_system().ok()?;
                        updater::load_staged_update(
                            &dirs.updates_dir(),
                            &staged_config.executable_name,
                            &remote_version,
                        )?;
                        Some(remote_version.clone())
                    });
                    let state = UpdateDialogState {
                        current: check.current.version_string(),
                        remote: remote_version,
                        published: if check.release.published_at.is_empty() {
                            "-".to_string()
                        } else {
                            check.release.published_at.clone()
                        },
                        notes: check.release.body.clone(),
                        asset: check.asset,
                        staged_version,
                    };
                    let _ = weak.update(cx, |editor, cx| {
                        editor.update_dialog = Some(state);
                        cx.notify();
                    });
                }
                Ok(_) => {
                    let title = i18n.t("update_none_message", &[]);
                    let detail = i18n.t(
                        "update_current_version",
                        &[("version", updater::CURRENT_VERSION)],
                    );
                    show_prompt(cx, window_handle, &title, &detail, &[&ok_label]).await;
                }
                Err(err) => {
                    let title = i18n.t("update_error", &[]);
                    show_prompt(cx, window_handle, &title, &err.to_string(), &[&ok_label]).await;
                }
            }
        })
        .detach();
    }

    /// 更新对话框按钮回调：稍后 / 打开发布页 / 下载。
    pub(crate) fn on_update_choice(
        &mut self,
        choice: UpdateChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.update_dialog.take() else {
            return;
        };
        cx.notify();

        match choice {
            UpdateChoice::Later => {}
            UpdateChoice::OpenRelease => {
                let config = updater::updater_config(Vec::new(), false);
                cx.spawn(async move |_this, _cx: &mut AsyncApp| {
                    let _ =
                        updater::run_blocking(move || updater::open_release_page(&config)).await;
                })
                .detach();
            }
            UpdateChoice::Download => {
                // 若本地已有与最新版本一致的暂存文件（上次点「稍后」留下），
                // 直接走安装确认，跳过下载，避免重复下载浪费时间。
                let staged = dialog.staged_version.as_deref().and_then(|version| {
                    let dirs = RumarkConfigDirs::from_system().ok()?;
                    let config = updater::updater_config(Vec::new(), false);
                    updater::load_staged_update(
                        &dirs.updates_dir(),
                        &config.executable_name,
                        version,
                    )
                });
                if let Some(staged) = staged {
                    let weak = cx.entity().downgrade();
                    let window_handle = window.window_handle();
                    cx.spawn(async move |_this, cx: &mut AsyncApp| {
                        let installed = confirm_and_install(staged.path, window_handle, cx).await;
                        if !installed {
                            let _ = weak.update(cx, |editor, cx| {
                                editor.update_status = UpdateStatus::default();
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                    return;
                }

                let Some(asset) = dialog.asset else {
                    let i18n = cx.global::<I18nManager>().snapshot();
                    let title = i18n.t("update_error", &[]);
                    let detail = format!("Release 中未找到当前平台的资产 {}", config_asset_name());
                    let ok = i18n.t("dialog_ok", &[]);
                    std::mem::drop(window.prompt(
                        PromptLevel::Info,
                        &title,
                        Some(&detail),
                        &[ok.as_str()],
                        cx,
                    ));
                    return;
                };

                self.start_download(asset, dialog.remote.clone(), window, cx);
            }
        }
    }

    /// 后台下载，并把进度回传给状态栏。
    fn start_download(
        &mut self,
        asset: updater::ReleaseAsset,
        version: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_status = UpdateStatus::downloading();
        cx.notify();

        let configs = update_configs(cx);
        let (tx, rx) = mpsc::channel::<updater::DownloadProgress>();

        // 进度泵：后台线程只做 mpsc::send，这里定时切回 UI 线程更新实体。
        // 关键：每次唤醒要把队列里积压的进度全部取完，只保留最新一条。若每
        // 次只取一条，下载线程 send 的速度远高于 60ms 一次的消费速度，状态栏
        // 会一直显示几十秒前的旧进度，甚至在下载早已完成后仍显示"正在下载"。
        let pump_weak = cx.entity().downgrade();
        cx.spawn(async move |_this, cx: &mut AsyncApp| {
            loop {
                let (latest, disconnected) = drain_latest_progress(&rx);
                if let Some(progress) = latest {
                    let _ = pump_weak.update(cx, |editor, cx| {
                        // 下载已结束（Downloaded/Default）后丢弃迟到的进度，
                        // 避免覆盖"下载完成"状态。
                        if editor.update_status.phase == UpdatePhase::Downloading {
                            editor.update_status.progress = Some(progress);
                            cx.notify();
                        }
                    });
                }
                if disconnected {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
            }
        })
        .detach();

        let finish_weak = cx.entity().downgrade();
        let window_handle = window.window_handle();
        let send_tx = tx.clone();
        let job_tx = send_tx.clone();

        cx.spawn(async move |_this, cx: &mut AsyncApp| {
            let dest = cx.update(|_cx| {
                RumarkConfigDirs::from_system()
                    .map(|dirs| dirs.updates_dir())
                    .unwrap_or_else(|_| std::env::temp_dir())
            });
            let result = updater::run_blocking(move || {
                updater::try_configs(configs, |config| {
                    updater::download_and_stage(config, &asset, &version, &dest, |progress| {
                        let _ = job_tx.send(progress);
                    })
                })
            })
            .await;
            drop(send_tx);

            let i18n = cx.update(|cx| cx.global::<I18nManager>().snapshot());
            let ok_label = i18n.t("dialog_ok", &[]);
            match result {
                Ok(path) => {
                    // 下载已完成：立刻把状态栏从"正在下载"切到"下载完成"，
                    // 再弹确认框，避免状态栏与弹窗文案矛盾。
                    let _ = finish_weak.update(cx, |editor, cx| {
                        editor.update_status = UpdateStatus::downloaded();
                        cx.notify();
                    });

                    // 与「安装已下载」入口共用同一套确认/替换/重启逻辑。
                    // 用户取消时保留 .new 文件，下次检查更新可直接复用。
                    let _ = confirm_and_install(path, window_handle, cx).await;
                }
                Err(err) => {
                    let title = i18n.t("update_download_failed", &[]);
                    show_prompt(cx, window_handle, &title, &err.to_string(), &[&ok_label]).await;
                }
            }

            let _ = finish_weak.update(cx, |editor, cx| {
                editor.update_status = UpdateStatus::default();
                cx.notify();
            });
        })
        .detach();
    }

    /// 渲染更新对话框覆盖层；无待处理对话框时返回 `None`。
    pub(crate) fn render_update_dialog_overlay(
        &self,
        i18n: &I18nSnapshot,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.update_dialog.as_ref()?;
        let strings = UpdateDialogStrings {
            title: i18n.t("update_available_title", &[]),
            // 这几条是「标签」，值由对话框组件单独拼在后面显示，因此传空值把
            // 文案里的 {version}/{date} 占位符去掉，避免出现「{version} <版本号>」。
            current_label: i18n.t("update_current_version", &[("version", "")]),
            new_label: i18n.t("update_new_version", &[("version", "")]),
            published_label: i18n.t("update_published_at", &[("date", "")]),
            notes_label: i18n.t("update_notes", &[]),
            // 本地已有匹配版本的暂存文件时，主按钮改为直接安装，避免重复下载。
            download: if dialog.staged_version.is_some() {
                i18n.t("update_install_downloaded", &[])
            } else {
                i18n.t("update_download", &[])
            },
            later: i18n.t("update_later", &[]),
            open_release: i18n.t("update_open_release", &[]),
            empty_notes: "-".to_string(),
        };
        let content = UpdateDialogContent {
            current: dialog.current.clone(),
            remote: dialog.remote.clone(),
            published: dialog.published.clone(),
            notes: dialog.notes.clone(),
            notes_max_height: 220.0,
        };
        Some(render_update_dialog(
            cx,
            &strings,
            &content,
            Self::on_update_choice,
        ))
    }
}

/// 从当前偏好组装候选配置：官方直连优先，镜像只在开启回退且直连失败时使用。
fn update_configs(cx: &App) -> Vec<rgpui_extra::updater::Config> {
    updater::fallback_configs(
        EditorSettings::custom_mirrors(cx),
        EditorSettings::use_mirrors(cx),
    )
}

/// 资产名与偏好无关，始终用直连配置读取即可。
fn config_asset_name() -> String {
    updater::updater_config(Vec::new(), false).asset_name()
}

/// 弹出简单提示并等待用户点击，返回按钮索引。
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

/// 弹出安装确认框；确认后启动 helper 并走标准退出流程。
///
/// `path` 可以是本次刚下载的暂存文件，也可以是上次点「稍后」留下的缓存。
/// 返回 `true` 表示安装已启动（主程序即将退出）；`false` 表示用户取消或启动失败。
async fn confirm_and_install(
    path: PathBuf,
    window_handle: AnyWindowHandle,
    cx: &mut AsyncApp,
) -> bool {
    let i18n = cx.update(|cx| cx.global::<I18nManager>().snapshot());
    let title = i18n.t("update_install_title", &[]);
    let detail = i18n.t("update_install_confirm", &[]);
    let install = i18n.t("update_install_now", &[]);
    let cancel = i18n.t("update_install_cancel", &[]);
    // 把"稍后"放在索引 0：show_prompt 在对话框被关闭/异常时兜底返回 0，
    // 这样默认行为是"不安装"，避免误替换正在运行的程序。
    let answer = show_prompt(cx, window_handle, &title, &detail, &[&cancel, &install]).await;
    if answer != 1 {
        return false;
    }

    let current_exe =
        cx.update(|_| std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rumark")));
    let options = updater::InstallOptions {
        current_exe,
        staged_exe: path,
        process_id: std::process::id(),
        // 更新后按正常的空文档启动，不重放中文文件路径或试图重建多个窗口。
        restart_args: restart_args_after_update(),
    };

    match updater::spawn_install_helper(&options) {
        Ok(_) => {
            // helper 已在后台等待；走标准退出流程（含未保存检查）。
            // 若检查取消，helper 会等待超时后自行退出，不会误替换。
            cx.update(crate::app_menu::request_quit_application);
            true
        }
        Err(err) => {
            let title = i18n.t("update_install_failed", &[]);
            let ok = i18n.t("dialog_ok", &[]);
            show_prompt(cx, window_handle, &title, &err.to_string(), &[&ok]).await;
            false
        }
    }
}

/// 排空进度 channel，返回 (最新进度, 发送端是否已断开)。
///
/// 后台下载线程 send 的速度远高于 UI 每 60ms 一次的消费，若每次只取一条，
/// 状态栏会一直落后于真实进度。这里把积压消息全部取完只留最后一条，
/// 让状态栏始终显示最新快照。
fn drain_latest_progress(
    rx: &mpsc::Receiver<updater::DownloadProgress>,
) -> (Option<updater::DownloadProgress>, bool) {
    let mut latest = None;
    let mut disconnected = false;
    loop {
        match rx.try_recv() {
            Ok(progress) => latest = Some(progress),
            Err(TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
            Err(TryRecvError::Empty) => break,
        }
    }
    (latest, disconnected)
}

/// 更新后启动新实例时不重放文件路径，避免 Windows Unicode 参数损坏并统一窗口状态。
fn restart_args_after_update() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::{drain_latest_progress, restart_args_after_update};
    use crate::update::DownloadProgress;

    fn progress(downloaded: u64) -> DownloadProgress {
        DownloadProgress {
            downloaded,
            total: Some(100),
        }
    }

    #[test]
    fn drain_keeps_only_latest_progress() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(progress(10)).unwrap();
        tx.send(progress(50)).unwrap();
        tx.send(progress(90)).unwrap();
        let (latest, disconnected) = drain_latest_progress(&rx);
        assert_eq!(latest.map(|p| p.downloaded), Some(90));
        assert!(!disconnected);
    }

    #[test]
    fn drain_reports_disconnect_after_final_progress() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(progress(70)).unwrap();
        tx.send(progress(100)).unwrap();
        drop(tx);
        let (latest, disconnected) = drain_latest_progress(&rx);
        assert_eq!(latest.map(|p| p.downloaded), Some(100));
        assert!(disconnected);
    }

    #[test]
    fn drain_on_empty_channel_reports_no_update() {
        let (_tx, rx) = std::sync::mpsc::channel::<DownloadProgress>();
        let (latest, disconnected) = drain_latest_progress(&rx);
        assert!(latest.is_none());
        assert!(!disconnected);
    }

    #[test]
    fn update_restart_does_not_replay_document_paths() {
        assert!(restart_args_after_update().is_empty());
    }
}
