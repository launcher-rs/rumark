//! 原生应用菜单、应用级动作与窗口关闭路由。
//!
//! 本模块负责构造菜单与操作当前编辑器窗口的动作，退出动作会路由到当前窗口，
//! 以便该窗口的未保存更改对话框保持最终决定权。

use std::path::{Path, PathBuf};

use rgpui::*;

use crate::app_identity::RUMARK_APP_ID;
use crate::components::{
    AddLanguageConfig, AddThemeConfig, CheckForUpdates, ClearRecentFiles, CloseTab, CloseWindow,
    CopyHtml, CopyMarkdown, ExportDocx, ExportHtml, ExportPdf, InstallCliTool, NewTab, NewWindow,
    NextTab, NoRecentFiles, OpenFile, OpenPreferences, OpenRecentFile, PreviousTab,
    QuitApplication, SaveDocument, SaveDocumentAs, SelectLanguage, SelectTheme, ShowAbout,
    ToggleWorkspace, UninstallCliTool,
};
use crate::config::{
    PendingConfigRecovery, apply_configured_language, apply_configured_theme, clear_recent_files,
    import_language_config_and_select, import_theme_config_and_select, open_preferences_window,
    read_recent_files, record_recent_file, remove_recent_file,
};
use crate::editor::{DocumentHost, Editor, WindowCloseFlow, read_error_detail, read_markdown_file};
use crate::i18n::I18nManager;
#[cfg(any(target_os = "macos", test))]
use crate::i18n::I18nSnapshot;
use rgpui::title_bar::TitleBar;
use rgpui::{WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowOptions};

/// 所有窗口共用的选项基座：`TitleBar` 组件基座（透明标题栏 + 组件接管拖拽），
/// 叠加应用标识、窗口几何、不透明背景与客户端装饰。
/// 任务栏标题不在选项里带（组件基座默认无标题），运行时由各窗口经
/// `set_window_title` 设置。
/// macOS 后端忽略装饰选项，故常置 Client 全平台统一。
pub(crate) fn rumark_window_options(bounds: Bounds<Pixels>) -> WindowOptions {
    let mut options = TitleBar::window_options();
    options.app_id = Some(RUMARK_APP_ID.to_string());
    options.window_bounds = Some(WindowBounds::Windowed(bounds));
    options.window_background = WindowBackgroundAppearance::Opaque;
    options.window_decorations = Some(WindowDecorations::Client);
    options.titlebar = Some(TitleBar::title_bar_options());
    options
}

/// 平台菜单生命周期钩子共用的应用菜单状态。
#[derive(Default)]
pub(crate) struct MenuState {
    window_closed_subscription: Option<Subscription>,
    /// 退出流程待关闭的编辑器窗口队列（逐窗确认未保存，全部关闭后归零退出）。
    quit_remaining: Vec<AnyWindowHandle>,
    /// 是否处于退出流程（防止普通关窗误触发队列推进）。
    quitting: bool,
}

impl Global for MenuState {}

/// 确保 `MenuState` 已注册（测试 app 不一定调用过 `init`，退出/取消路径也要可用）。
fn ensure_menu_state(cx: &mut App) {
    if cx.try_global::<MenuState>().is_none() {
        cx.set_global(MenuState::default());
    }
}

/// 按给定 Markdown 内容与可选路径打开编辑器窗口。
///
/// 窗口根视图是多文档容器 [`DocumentHost`]：它内含首个 tab（编辑器）。关闭守卫
/// 由活动 Editor 首次渲染时自行安装（`install_close_guard`），切 tab 后新 Editor
/// 渲染会重装，因此这里不再显式安装。
pub(crate) fn open_editor_window(
    cx: &mut App,
    markdown: String,
    file_path: Option<PathBuf>,
) -> WindowHandle<DocumentHost> {
    const DEFERRED_LOAD_THRESHOLD: usize = 128 * 1024;
    let (initial_markdown, deferred_markdown) = if markdown.len() >= DEFERRED_LOAD_THRESHOLD {
        (
            cx.global::<I18nManager>()
                .snapshot()
                .t("message_loading_document", &[]),
            Some(markdown),
        )
    } else {
        (markdown, None)
    };
    let deferred = deferred_markdown.is_some();
    let initial_file_path = if deferred { None } else { file_path.clone() };
    let deferred_path = file_path;
    let bounds = Bounds::centered(None, size(px(1080.), px(720.)), cx);
    let handle = cx
        .open_window(rumark_window_options(bounds), move |_window, cx| {
            cx.new(move |cx| DocumentHost::new(initial_markdown, initial_file_path, cx))
        })
        .unwrap();

    handle
        .update(cx, move |host, window, cx| {
            window.activate_window();
            show_pending_config_recovery(cx, window);
            if let Some(markdown) = deferred_markdown {
                let editor = host.active_editor();
                editor.update(cx, |editor, cx| {
                    editor.defer_large_document_load(markdown, deferred_path, cx);
                });
            }
        })
        .expect("newly opened editor window should be updateable");

    handle
}

/// 展示启动时的配置恢复提示。
///
/// 配置文件损坏时启动流程已备份原文件并重建默认配置，此处告知用户备份位置，
/// 否则用户只会看到键位、主题与语言"没生效"，而不知原配置已被移走。
fn show_pending_config_recovery(cx: &mut App, window: &mut Window) {
    let Some(recovery) = PendingConfigRecovery::take(cx) else {
        return;
    };
    let i18n = cx.global::<I18nManager>().snapshot();
    let title = i18n.t("message_config_damaged_title", &[]);
    let damaged_path = recovery.damaged_path.display().to_string();
    let backup_path = recovery.backup_path.display().to_string();
    let detail = i18n.t(
        "message_config_damaged_message",
        &[("path", &damaged_path), ("backup", &backup_path)],
    );
    let ok = i18n.t("dialog_ok", &[]);
    let buttons = [ok.as_str()];
    let prompt = window.prompt(PromptLevel::Critical, &title, Some(&detail), &buttons, cx);
    // 这里拿到的是 `App`（而非某个实体的 Context），spawn 闭包只有 cx 一个参数。
    cx.spawn(async move |_cx| {
        let _ = prompt.await;
    })
    .detach();
}

/// 在新窗口中打开指定文件。
pub(crate) fn open_file_in_new_window(cx: &mut App, path: &Path) -> anyhow::Result<()> {
    let markdown = read_markdown_file(path)?;
    open_editor_window(cx, markdown, Some(path.to_path_buf()));
    record_recent_file_and_refresh(path, cx);
    Ok(())
}

/// 把文档读取失败提示到指定窗口。
///
/// release 版 Windows 没有控制台，只 `eprintln!` 的话用户只会看到「打开后是空的」，
/// 不知道是编码不支持还是文件被占用。
pub(crate) fn show_read_failure_prompt(
    window: Option<AnyWindowHandle>,
    path: &Path,
    err: &anyhow::Error,
    cx: &mut App,
) {
    let i18n = cx.global::<I18nManager>().snapshot();
    let title = i18n.t("message_open_failed", &[]);
    let detail = read_error_detail(err, path, &i18n);
    eprintln!("{title}: {detail}");
    show_window_prompt(window, &title, &detail, cx);
}

/// 若当前活动窗口是「用户尚未输入任何内容的空白编辑器」，则把文件就地载入该窗口。
///
/// 返回是否已复用当前窗口（`false` 表示调用方应改为新建窗口）。
/// 读取失败时不会改动窗口状态，交由调用方按新建窗口路径统一报错。
fn try_load_into_active_blank_window(cx: &mut App, path: &Path) -> bool {
    let Some(window) = cx.active_window() else {
        return false;
    };
    let path = path.to_path_buf();

    // standalone 编辑器窗口（测试直接以 Editor 为根）：保持原有路径。
    if let Some(editor_window) = window.downcast::<Editor>() {
        let blank = editor_window
            .read_with(cx, |editor, cx| editor.is_blank_document(cx))
            .unwrap_or(false);
        if !blank {
            return false;
        }
        return editor_window
            .update(cx, |editor, window, cx| {
                if editor.replace_document_from_path(&path, cx).is_ok() {
                    window.set_window_edited(false);
                    window.activate_window();
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
    }

    // 多文档窗口：复用活动 tab 的空白编辑器，不改变 tab 数量。
    if let Some(host_window) = window.downcast::<DocumentHost>() {
        let blank = host_window
            .read_with(cx, |host, cx| host.active_is_blank(cx))
            .unwrap_or(false);
        if !blank {
            return false;
        }
        return host_window
            .update(cx, |host, window, cx| {
                if host.load_active_from_path(&path, cx) {
                    window.set_window_edited(false);
                    window.activate_window();
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
    }

    false
}

/// 打开单个文件：优先把内容载入当前空白窗口，否则新建窗口。
///
/// 载入经 `spawn` 延后，避免菜单动作分发期间活动窗口被 take 导致同步
/// `update` 失败（与 `with_active_editor` 同款处理）；错误提示定位到
/// 调用时记录的目标窗口。
pub(crate) fn open_file_preferring_blank_window(
    cx: &mut App,
    path: PathBuf,
    error_window: Option<AnyWindowHandle>,
) {
    cx.spawn(async move |cx| {
        cx.update(move |cx| {
            if try_load_into_active_blank_window(cx, &path) {
                return;
            }
            if let Err(err) = open_file_in_new_window(cx, &path) {
                let i18n = cx.global::<I18nManager>().snapshot();
                let title = i18n.t("message_open_failed", &[]);
                let detail = read_error_detail(&err, &path, &i18n);
                show_window_prompt(error_window, &title, &detail, cx);
            }
        });
    })
    .detach();
}

/// 记录最近文件并刷新菜单与窗口。
fn record_recent_file_and_refresh(path: &Path, cx: &mut App) {
    if let Err(err) = record_recent_file(path) {
        eprintln!("failed to update recent file history: {err}");
        return;
    }
    install_menus(cx);
    cx.refresh_windows();
}

/// 清空最近文件历史并刷新菜单与窗口。
fn clear_recent_files_and_refresh(cx: &mut App) {
    if let Err(err) = clear_recent_files() {
        eprintln!("failed to clear recent file history: {err}");
        return;
    }
    install_menus(cx);
    cx.refresh_windows();
}

#[cfg(target_os = "macos")]
/// 检查 `/usr/local/bin/rumark` 是否正确指向当前应用。
///
/// 仅当该符号链接存在**且**（直接或经一层规范化后）解析到
/// 当前正在运行的可执行文件时返回 `true`。
fn is_cli_symlink_current_app() -> bool {
    let link = std::path::Path::new("/usr/local/bin/rumark");
    let Ok(target) = std::fs::read_link(link) else {
        return false; // 不存在或不是符号链接
    };
    let resolved = if target.is_absolute() {
        // 规范化目标本身（悬空时可能失败）。
        std::fs::canonicalize(&target).unwrap_or(target)
    } else {
        // 相对路径——从符号链接所在父目录解析。
        link.parent()
            .unwrap_or(std::path::Path::new("/"))
            .join(&target)
            .canonicalize()
            .unwrap_or(target)
    };
    match std::env::current_exe() {
        Ok(exe) => resolved == exe,
        Err(_) => false,
    }
}

#[cfg(any(target_os = "macos", test))]
/// 将路径转义为 AppleScript 字符串字面量。
fn applescript_string_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(any(target_os = "macos", test))]
/// 安装成功提示正文：带上链接位置与终端用法示例。
fn cli_installed_detail(i18n: &I18nSnapshot, bin_link: &str) -> String {
    i18n.t("message_cli_installed_detail", &[("location", bin_link)])
}

#[cfg(any(target_os = "macos", test))]
/// 安装失败提示正文：用户在管理员密码框点了取消，与真实失败分开表述。
fn cli_install_failure_detail(i18n: &I18nSnapshot, stderr: &str) -> String {
    if stderr.contains("User canceled") || stderr.contains("(-128)") {
        i18n.t("message_cli_install_cancelled", &[])
    } else {
        i18n.t("message_cli_install_failed", &[("error", stderr)])
    }
}

#[cfg(any(target_os = "macos", test))]
/// 卸载失败提示正文：同样区分取消与失败。
fn cli_uninstall_failure_detail(i18n: &I18nSnapshot, stderr: &str) -> String {
    if stderr.contains("User canceled") || stderr.contains("(-128)") {
        i18n.t("message_cli_uninstall_cancelled", &[])
    } else {
        i18n.t("message_cli_uninstall_failed", &[("error", stderr)])
    }
}

#[cfg(target_os = "macos")]
/// 安装命令行工具符号链接。
pub(crate) fn install_cli_tool(cx: &mut App) {
    use std::process::Command;

    let bin_link = "/usr/local/bin/rumark";
    let i18n = cx.global::<I18nManager>().snapshot();

    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            let detail = i18n.t(
                "message_cli_exe_path_failed",
                &[("error", &err.to_string())],
            );
            show_install_cli_error(cx, "message_cli_install_failed_title", &detail);
            return;
        }
    };

    // 仅允许来自便携式 .app 包（如拖入 /Applications 安装）的调用
    if !current_exe
        .to_string_lossy()
        .contains(".app/Contents/MacOS/")
    {
        let detail = i18n.t("message_cli_requires_bundle", &[]);
        show_install_cli_error(cx, "message_cli_install_failed_title", &detail);
        return;
    }

    let exe_path = applescript_string_literal(&current_exe.to_string_lossy());
    let link_path = applescript_string_literal(bin_link);
    let script = format!(
        r#"set exePath to {exe_path}
set linkPath to {link_path}
do shell script "rm -f " & quoted form of linkPath & linefeed & "ln -s " & quoted form of exePath & space & quoted form of linkPath with administrator privileges"#
    );

    match Command::new("osascript").arg("-e").arg(&script).output() {
        Ok(output) => {
            if output.status.success() {
                let title = i18n.t("message_cli_installed_title", &[]);
                let detail = cli_installed_detail(&i18n, bin_link);
                show_window_prompt_with_level(
                    cx.active_window(),
                    PromptLevel::Info,
                    &title,
                    &detail,
                    cx,
                );
            } else {
                // 用户在管理员密码框点了取消，或链接创建因其他原因失败。
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let detail = cli_install_failure_detail(&i18n, &stderr);
                show_install_cli_error(cx, "message_cli_install_failed_title", &detail);
            }
        }
        Err(err) => {
            let detail = i18n.t(
                "message_cli_installer_run_failed",
                &[("error", &err.to_string())],
            );
            show_install_cli_error(cx, "message_cli_install_failed_title", &detail);
        }
    }
    // 刷新菜单，使安装/卸载标签随之切换。
    install_menus(cx);
}

#[cfg(target_os = "macos")]
/// 卸载命令行工具符号链接。
pub(crate) fn uninstall_cli_tool(cx: &mut App) {
    use std::process::Command;

    let bin_link = "/usr/local/bin/rumark";
    let i18n = cx.global::<I18nManager>().snapshot();

    if !is_cli_symlink_current_app() {
        let detail = i18n.t("message_cli_not_installed", &[]);
        show_install_cli_error(cx, "message_cli_uninstall_failed_title", &detail);
        return;
    }

    let link_path = applescript_string_literal(bin_link);
    let script = format!(
        r#"set linkPath to {link_path}
do shell script "rm -f " & quoted form of linkPath with administrator privileges"#
    );

    match Command::new("osascript").arg("-e").arg(&script).output() {
        Ok(output) => {
            if output.status.success() {
                let title = i18n.t("message_cli_uninstalled_title", &[]);
                let detail = i18n.t("message_cli_uninstalled_detail", &[]);
                show_window_prompt_with_level(
                    cx.active_window(),
                    PromptLevel::Info,
                    &title,
                    &detail,
                    cx,
                );
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let detail = cli_uninstall_failure_detail(&i18n, &stderr);
                show_install_cli_error(cx, "message_cli_uninstall_failed_title", &detail);
            }
        }
        Err(err) => {
            let detail = i18n.t(
                "message_cli_uninstaller_run_failed",
                &[("error", &err.to_string())],
            );
            show_install_cli_error(cx, "message_cli_uninstall_failed_title", &detail);
        }
    }
    // 刷新菜单，使安装/卸载标签随之切换。
    install_menus(cx);
}

#[cfg(not(target_os = "macos"))]
/// 安装命令行工具（非 macOS 下仅提示不支持）。
pub(crate) fn install_cli_tool(cx: &mut App) {
    let detail = cx
        .global::<I18nManager>()
        .snapshot()
        .t("message_cli_install_macos_only", &[]);
    show_install_cli_error(cx, "message_cli_install_failed_title", &detail);
}

#[cfg(not(target_os = "macos"))]
/// 卸载命令行工具（非 macOS 下仅提示不支持）。
pub(crate) fn uninstall_cli_tool(cx: &mut App) {
    let detail = cx
        .global::<I18nManager>()
        .snapshot()
        .t("message_cli_uninstall_macos_only", &[]);
    show_install_cli_error(cx, "message_cli_uninstall_failed_title", &detail);
}

/// 显示命令行工具安装失败提示。
///
/// 标题按当前语言取键值，正文由调用方生成（可能是本地化的，也可能是系统错误原文）。
fn show_install_cli_error(cx: &mut App, title_key: &str, detail: &str) {
    let title = cx.global::<I18nManager>().snapshot().t(title_key, &[]);
    show_window_prompt_with_level(
        cx.active_window(),
        PromptLevel::Critical,
        &title,
        detail,
        cx,
    );
}

/// 从编辑器侧记录最近文件。
pub(crate) fn record_recent_file_from_editor(path: &Path, cx: &mut App) {
    record_recent_file_and_refresh(path, cx);
}

/// 在指定窗口弹出提示框；无窗口时输出到标准错误。
///
/// 动作分发期间活动窗口被占用，同步 `update` 必失败，故统一经 `spawn`
/// 延后到下一次循环执行（与 `with_active_editor` 同款处理）。对话框级别
/// 可指定，便于信息类对话框复用。
fn show_window_prompt_with_level(
    window: Option<AnyWindowHandle>,
    level: PromptLevel,
    title: &str,
    detail: &str,
    cx: &mut App,
) {
    let Some(window) = window else {
        eprintln!("{title}: {detail}");
        return;
    };
    let ok = cx.global::<I18nManager>().snapshot().t("dialog_ok", &[]);
    let title = title.to_string();
    let detail = detail.to_string();
    cx.spawn(async move |cx| {
        let _ = window.update(cx, |_view, window, cx| {
            // 对话框 future 必须轮询才会弹出：转交后台任务等待用户响应。
            let buttons = [ok.as_str()];
            let prompt = window.prompt(level, &title, Some(&detail), &buttons, cx);
            cx.spawn(async move |_| {
                let _ = prompt.await;
            })
            .detach();
        });
    })
    .detach();
}

/// 在指定窗口显示错误提示，无窗口时输出到标准错误。
fn show_window_prompt(window: Option<AnyWindowHandle>, title: &str, detail: &str, cx: &mut App) {
    show_window_prompt_with_level(window, PromptLevel::Critical, title, detail, cx);
}

/// 对当前活动编辑器执行一次更新操作。
///
/// 调用方处于动作分发中（窗口正被 take），同步 `update` 必失败
/// （F12 同款教训），故此处只同步解析活动窗口句柄（只读安全），
/// 再经 `spawn` 延后更新；`let _` 吞掉的是窗口已关闭的紧凑型失败。
fn with_active_editor(
    cx: &mut App,
    update: impl FnOnce(&mut Editor, &mut Window, &mut Context<Editor>) + 'static,
) {
    let Some(window) = cx.active_window() else {
        return;
    };
    // 双路径：standalone 编辑器（测试等直接以 Editor 为窗口根）与多文档容器。
    // 两者都必须经 spawn 延后，动作分发期间同步 update 活动窗口必失败（F12 教训）。
    if let Some(editor_window) = window.downcast::<Editor>() {
        cx.spawn(async move |cx| {
            let _ = editor_window.update(cx, |editor, window, cx| update(editor, window, cx));
        })
        .detach();
    } else if let Some(host_window) = window.downcast::<DocumentHost>() {
        cx.spawn(async move |cx| {
            let _ = host_window.update(cx, |host, window, cx| {
                let editor = host.active_editor();
                // Context<DocumentHost> 解引用为 App，可直接 update 子 Editor；
                // window 由外层闭包捕获并传给回调，借用互不冲突。
                editor.update(cx, |editor, editor_cx| {
                    update(editor, window, editor_cx);
                });
            });
        })
        .detach();
    }
}

/// 对当前活动的多文档容器执行一次更新操作（tab 级动作入口）。
///
/// 与 [`with_active_editor`] 同款处理：动作分发期间活动窗口被 take，同步
/// `update` 必失败，故统一经 `spawn` 延后；standalone Editor 窗口（测试等）
/// 没有 host，直接忽略。
fn with_active_host(
    cx: &mut App,
    update: impl FnOnce(&mut DocumentHost, &mut Window, &mut Context<DocumentHost>) + 'static,
) {
    let Some(window) = cx.active_window() else {
        return;
    };
    let Some(host_window) = window.downcast::<DocumentHost>() else {
        return;
    };
    cx.spawn(async move |cx| {
        let _ = host_window.update(cx, |host, window, cx| update(host, window, cx));
    })
    .detach();
}

/// 在当前活动编辑器上打开关于对话框（框架 `Dialog`）。
fn show_about_dialog_on_active_editor(cx: &mut App) {
    with_active_editor(cx, move |editor, window, cx| {
        editor.open_about_dialog(window, cx);
    });
}

/// 在当前活动编辑器上手动触发检查更新。
fn check_updates_on_active_editor(cx: &mut App) {
    with_active_editor(cx, move |editor, window, cx| {
        editor.check_for_updates(window, cx);
    });
}

/// 读取供菜单展示的最近文件列表。
fn recent_files_for_menu() -> Vec<PathBuf> {
    match read_recent_files() {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("failed to read recent file history: {err}");
            Vec::new()
        }
    }
}

/// 打开最近文件（错误提示定位到当前活动窗口）。
fn open_recent_file(cx: &mut App, path: PathBuf) {
    let error_window = cx.active_window();
    open_recent_file_with_error_window(cx, path, error_window);
}

/// 打开最近文件并把错误提示定位到指定窗口。
fn open_recent_file_with_error_window(
    cx: &mut App,
    path: PathBuf,
    error_window: Option<AnyWindowHandle>,
) {
    if !path.is_file() {
        if let Err(err) = remove_recent_file(&path) {
            eprintln!("failed to remove missing recent file: {err}");
        }
        install_menus(cx);
        cx.refresh_windows();
        let detail = cx.global::<I18nManager>().snapshot().t(
            "message_recent_missing_message",
            &[("path", &path.to_string_lossy())],
        );
        let title = cx
            .global::<I18nManager>()
            .snapshot()
            .t("message_recent_missing_title", &[]);
        show_window_prompt(error_window, &title, &detail, cx);
        return;
    }

    open_file_preferring_blank_window(cx, path, error_window);
}

/// 收集关闭与退出时需依次确认的窗口候选。
fn current_window_candidates(cx: &mut App) -> Vec<AnyWindowHandle> {
    let mut candidates = Vec::new();
    let mut push_unique = |window: AnyWindowHandle| {
        if candidates
            .iter()
            .all(|candidate: &AnyWindowHandle| candidate.window_id() != window.window_id())
        {
            candidates.push(window);
        }
    };

    if let Some(window) = cx.active_window() {
        push_unique(window);
    }
    if let Some(windows) = cx.window_stack() {
        for window in windows {
            push_unique(window);
        }
    }
    for window in cx.windows() {
        push_unique(window);
    }

    candidates
}

/// 请求关闭指定编辑器窗口（含未保存确认）。
///
/// 泛化到任意 `AppContext`，以便既能在同步路径调用，也能在 `spawn` 出
/// 的异步任务（`AsyncApp`）中调用。
fn request_close_editor_window(window: AnyWindowHandle, cx: &mut impl AppContext) -> bool {
    if let Some(window) = window.downcast::<Editor>() {
        return window
            .update(cx, |editor, window, cx| {
                editor.request_close_current_window(window, cx);
            })
            .is_ok();
    }
    if let Some(window) = window.downcast::<DocumentHost>() {
        return window
            .update(cx, |host, window, cx| {
                host.request_window_close(WindowCloseFlow::CloseWindow, window, cx);
            })
            .is_ok();
    }
    false
}

/// 请求关闭当前编辑器窗口。
fn request_close_current_editor_window(cx: &mut App) {
    let candidates = current_window_candidates(cx);
    if candidates.is_empty() {
        cx.quit();
        return;
    }

    // 本函数由动作分发触发（原生菜单经 `App::dispatch_action` 先 take 活动窗口，
    // 键盘快捷键则在窗口 `update` 内），同步对活动窗口 `update` 必失败
    // （与 F12 同款的教训）。故经 `spawn` 延后到下一次循环，此时窗口已归位；
    // 多窗口下依次尝试，首个成功者即当前活动窗口。
    cx.spawn(async move |cx| {
        for window in candidates {
            if request_close_editor_window(window, cx) {
                return;
            }
        }
    })
    .detach();
}

/// 逐窗确认后请求退出应用。
///
/// 退出是「逐窗关闭」：把所有编辑器窗口放进队列，每次只请求关闭一个；
/// 该窗口的未保存确认由自身负责，确认后 `window.remove_window()` 触发
/// `handle_window_closed` 推进下一个；全部窗口归零时统一 `cx.quit()`。
/// 这样混合场景（host 窗口 + standalone 编辑器窗口）里每个窗口的未保存内容
/// 都会得到提示，不再因首个 host 命中就直接退出而漏掉其他窗口。
pub(crate) fn request_quit_application(cx: &mut App) {
    ensure_menu_state(cx);
    let candidates = current_window_candidates(cx);
    if candidates.is_empty() {
        cx.quit();
        return;
    }

    // 只保留编辑器窗口（host 或 standalone Editor）；偏好设置等非编辑器窗口
    // 不参与未保存确认队列，最后随应用退出一并消失。
    let editor_windows = candidates
        .into_iter()
        .filter(|window| {
            window.downcast::<DocumentHost>().is_some() || window.downcast::<Editor>().is_some()
        })
        .collect::<Vec<_>>();
    {
        let state = cx.global_mut::<MenuState>();
        state.quitting = true;
        state.quit_remaining = editor_windows;
    }

    // 见 `request_close_current_editor_window`：动作分发期间同步 `update` 活动
    // 窗口必失败，必须延后到下一次循环。
    cx.spawn(async move |cx| {
        cx.update(advance_quit_window);
    })
    .detach();
}

/// 推进退出流程：找到第一个「有未保存修改」的窗口并对它弹确认框，其余窗口保持原样；
/// 所有窗口都干净时统一退出。脏窗口确认关闭后会触发 `handle_window_closed`，
/// 再次进入本函数检查剩余窗口——如此逐窗提示，直到全部干净才退出。
fn advance_quit_window(cx: &mut App) {
    ensure_menu_state(cx);
    let candidates = cx.global::<MenuState>().quit_remaining.clone();
    for window in candidates {
        // 先判断该窗口是否有未保存内容（窗口已关闭则 update 失败，自动跳过）。
        let dirty = if let Some(host_window) = window.downcast::<DocumentHost>() {
            host_window
                .update(cx, |host, _, cx| host.any_dirty(cx))
                .unwrap_or(false)
        } else if let Some(editor_window) = window.downcast::<Editor>() {
            editor_window
                .update(cx, |editor, _, _| editor.is_document_dirty())
                .unwrap_or(false)
        } else {
            false
        };
        if !dirty {
            continue;
        }
        // 只对这个脏窗口弹确认框，不关闭其他任何窗口（遇脏即停，兼容既有语义）。
        if let Some(host_window) = window.downcast::<DocumentHost>() {
            // host 内部逐 tab 确认；确认完成后由 handle_window_closed 推进下一轮。
            let _ = host_window.update(cx, |host, window, cx| {
                host.request_window_close(WindowCloseFlow::Quit, window, cx);
            });
        } else if let Some(editor_window) = window.downcast::<Editor>() {
            let _ = editor_window.update(cx, |editor, window, cx| {
                editor.on_window_should_close(window, cx);
            });
        }
        return;
    }

    // 走到这里说明所有编辑器窗口都干净：清掉退出态并退出（quit 会关闭全部窗口）。
    cx.global_mut::<MenuState>().quitting = false;
    cx.global_mut::<MenuState>().quit_remaining.clear();
    cx.quit();
}

/// 用户取消退出中的某窗口确认：清空退出队列，中止整个退出流程。
pub(crate) fn cancel_quit_flow(cx: &mut App) {
    ensure_menu_state(cx);
    let state = cx.global_mut::<MenuState>();
    if state.quitting {
        state.quitting = false;
        state.quit_remaining.clear();
    }
}

/// 按当前应用状态执行应用菜单动作。
pub(crate) fn dispatch_menu_action(action: &dyn Action, cx: &mut App) {
    if action.as_any().is::<NewWindow>() {
        open_editor_window(cx, String::new(), None);
    } else if action.as_any().is::<OpenFile>() {
        prompt_and_open_files(cx);
    } else if action.as_any().is::<OpenPreferences>() {
        open_preferences_window(cx);
    } else if let Some(action) = action.as_any().downcast_ref::<OpenRecentFile>() {
        open_recent_file(cx, PathBuf::from(&action.path));
    } else if action.as_any().is::<NoRecentFiles>() {
    } else if action.as_any().is::<ClearRecentFiles>() {
        clear_recent_files_and_refresh(cx);
    } else if action.as_any().is::<AddLanguageConfig>() {
        prompt_and_import_language_config(cx);
    } else if action.as_any().is::<AddThemeConfig>() {
        prompt_and_import_theme_config(cx);
    } else if action.as_any().is::<SaveDocument>() {
        with_active_editor(cx, |editor, window, cx| editor.save_document(window, cx));
    } else if action.as_any().is::<SaveDocumentAs>() {
        with_active_editor(cx, |editor, window, cx| editor.save_document_as(window, cx));
    } else if action.as_any().is::<ExportHtml>() {
        with_active_editor(cx, |editor, window, cx| {
            editor.export_document_as_html(window, cx)
        });
    } else if action.as_any().is::<CopyHtml>() {
        with_active_editor(cx, |editor, window, cx| {
            editor.copy_html_to_clipboard(window, cx)
        });
    } else if action.as_any().is::<CopyMarkdown>() {
        with_active_editor(cx, |editor, window, cx| {
            editor.copy_markdown_to_clipboard(window, cx)
        });
    } else if action.as_any().is::<ExportPdf>() {
        #[cfg(feature = "export-pdf")]
        with_active_editor(cx, |editor, window, cx| {
            editor.export_document_with_format(crate::export::ExportFormat::Pdf, window, cx)
        });
    } else if action.as_any().is::<ExportDocx>() {
        #[cfg(feature = "export-docx")]
        with_active_editor(cx, |editor, window, cx| {
            editor.export_document_with_format(crate::export::ExportFormat::Docx, window, cx)
        });
    } else if let Some(action) = action.as_any().downcast_ref::<SelectTheme>() {
        match apply_configured_theme(cx, &action.theme_id) {
            Ok(changed) => {
                if changed {
                    install_menus(cx);
                    cx.refresh_windows();
                }
            }
            Err(err) => {
                let title = cx
                    .global::<I18nManager>()
                    .snapshot()
                    .t("message_save_preferences_failed", &[]);
                show_window_prompt(cx.active_window(), &title, &err.to_string(), cx);
            }
        }
    } else if let Some(action) = action.as_any().downcast_ref::<SelectLanguage>() {
        match apply_configured_language(cx, &action.language_id) {
            Ok(changed) => {
                if changed {
                    install_menus(cx);
                    cx.refresh_windows();
                }
            }
            Err(err) => {
                let title = cx
                    .global::<I18nManager>()
                    .snapshot()
                    .t("message_save_preferences_failed", &[]);
                show_window_prompt(cx.active_window(), &title, &err.to_string(), cx);
            }
        }
    } else if action.as_any().is::<ShowAbout>() {
        show_about_dialog_on_active_editor(cx);
    } else if action.as_any().is::<CheckForUpdates>() {
        check_updates_on_active_editor(cx);
    } else if action.as_any().is::<InstallCliTool>() {
        install_cli_tool(cx);
    } else if action.as_any().is::<UninstallCliTool>() {
        uninstall_cli_tool(cx);
    } else if action.as_any().is::<ToggleWorkspace>() {
        with_active_editor(cx, |editor, window, cx| {
            editor.toggle_workspace_drawer(window, cx);
        });
    } else if action.as_any().is::<QuitApplication>() {
        request_quit_application(cx);
    } else if action.as_any().is::<CloseWindow>() {
        request_close_current_editor_window(cx);
    } else if action.as_any().is::<NewTab>() {
        with_active_host(cx, |host, _window, cx| host.open_new_tab(cx));
    } else if action.as_any().is::<CloseTab>() {
        with_active_host(cx, |host, window, cx| {
            host.request_close_active_tab(window, cx)
        });
    } else if action.as_any().is::<NextTab>() {
        with_active_host(cx, |host, _window, cx| host.select_next_tab(cx));
    } else if action.as_any().is::<PreviousTab>() {
        with_active_host(cx, |host, _window, cx| host.select_previous_tab(cx));
    }
}

/// 按主题、语言与最近文件构造全部菜单。
fn build_menus(
    active_theme_id: &str,
    theme_entries: &[crate::theme::ThemeCatalogEntry],
    i18n_manager: &I18nManager,
    recent_files: &[PathBuf],
) -> Vec<Menu> {
    let mut theme_items = theme_entries
        .iter()
        .map(|entry| {
            let label = if entry.id.as_str() == active_theme_id {
                format!("\u{2713} {}", entry.name)
            } else {
                entry.name.to_string()
            };
            MenuItem::action(
                label,
                SelectTheme {
                    theme_id: entry.id.to_string(),
                },
            )
        })
        .collect::<Vec<_>>();
    theme_items.push(MenuItem::separator());
    theme_items.push(MenuItem::action(
        i18n_manager.t("menu_theme_add_config", &[]),
        AddThemeConfig,
    ));

    let active_language_id = i18n_manager.active_language_id().to_string();
    let mut language_items = i18n_manager
        .listed_languages()
        .iter()
        .map(|entry| {
            let name = entry.name.to_string();
            let label = if entry.id.as_str() == active_language_id {
                format!("\u{2713} {name}")
            } else {
                name
            };
            MenuItem::action(
                label,
                SelectLanguage {
                    language_id: entry.id.to_string(),
                },
            )
        })
        .collect::<Vec<_>>();
    language_items.push(MenuItem::separator());
    language_items.push(MenuItem::action(
        i18n_manager.t("menu_language_add_config", &[]),
        AddLanguageConfig,
    ));

    let recent_items = if recent_files.is_empty() {
        vec![MenuItem::action(
            i18n_manager.t("menu_file_recent_empty", &[]),
            NoRecentFiles,
        )]
    } else {
        let mut items = recent_files
            .iter()
            .map(|path| {
                // `into_owned` 在常见的有效 UTF-8 路径下复用 `Cow::Owned` 不再拷贝，
                // 仅在兜底转码时分配；此前的写法每次都会分配。
                let label = path.to_string_lossy().into_owned();
                MenuItem::action(label.clone(), OpenRecentFile { path: label })
            })
            .collect::<Vec<_>>();
        items.push(MenuItem::separator());
        items.push(MenuItem::action(
            i18n_manager.t("menu_file_recent_clear", &[]),
            ClearRecentFiles,
        ));
        items
    };

    #[cfg(target_os = "macos")]
    let initial_menus = {
        // macOS 下首个菜单为应用菜单（系统会用应用名覆盖标题），文件操作单独放在
        // “文件”菜单以符合 macOS 惯例。
        vec![
            Menu {
                disabled: false,
                name: "Rumark".into(),
                items: vec![
                    MenuItem::action(
                        i18n_manager.t("menu_file_preferences", &[]),
                        OpenPreferences,
                    ),
                    MenuItem::separator(),
                    MenuItem::action(i18n_manager.t("menu_file_quit", &[]), QuitApplication),
                ],
            },
            Menu {
                disabled: false,
                name: i18n_manager.t("menu_file", &[]).into(),
                items: vec![
                    MenuItem::action(i18n_manager.t("menu_file_new_window", &[]), NewWindow),
                    MenuItem::action(i18n_manager.t("menu_file_new_tab", &[]), NewTab),
                    MenuItem::action(i18n_manager.t("menu_file_close_tab", &[]), CloseTab),
                    MenuItem::action(i18n_manager.t("menu_file_close_window", &[]), CloseWindow),
                    MenuItem::action(i18n_manager.t("menu_file_open", &[]), OpenFile),
                    MenuItem::submenu(Menu {
                        disabled: false,
                        name: i18n_manager.t("menu_file_recent", &[]).into(),
                        items: recent_items,
                    }),
                    MenuItem::separator(),
                    MenuItem::action(i18n_manager.t("menu_file_save", &[]), SaveDocument),
                    MenuItem::action(i18n_manager.t("menu_file_save_as", &[]), SaveDocumentAs),
                    MenuItem::submenu(Menu {
                        disabled: false,
                        name: i18n_manager.t("menu_export", &[]).into(),
                        items: vec![
                            MenuItem::action(i18n_manager.t("menu_export_html", &[]), ExportHtml),
                            #[cfg(feature = "export-pdf")]
                            MenuItem::action(i18n_manager.t("menu_export_pdf", &[]), ExportPdf),
                            #[cfg(feature = "export-docx")]
                            MenuItem::action(i18n_manager.t("menu_export_docx", &[]), ExportDocx),
                            MenuItem::action(
                                i18n_manager.t("menu_export_copy_html", &[]),
                                CopyHtml,
                            ),
                            MenuItem::action(
                                i18n_manager.t("menu_export_copy_markdown", &[]),
                                CopyMarkdown,
                            ),
                        ],
                    }),
                ],
            },
        ]
    };

    #[cfg(not(target_os = "macos"))]
    let initial_menus = {
        vec![Menu {
            disabled: false,
            name: i18n_manager.t("menu_file", &[]).into(),
            items: vec![
                MenuItem::action(i18n_manager.t("menu_file_new_window", &[]), NewWindow),
                MenuItem::action(i18n_manager.t("menu_file_new_tab", &[]), NewTab),
                MenuItem::action(i18n_manager.t("menu_file_close_tab", &[]), CloseTab),
                MenuItem::action(i18n_manager.t("menu_file_close_window", &[]), CloseWindow),
                MenuItem::action(i18n_manager.t("menu_file_open", &[]), OpenFile),
                MenuItem::submenu(Menu {
                    disabled: false,
                    name: i18n_manager.t("menu_file_recent", &[]).into(),
                    items: recent_items,
                }),
                MenuItem::action(
                    i18n_manager.t("menu_file_preferences", &[]),
                    OpenPreferences,
                ),
                MenuItem::separator(),
                MenuItem::action(i18n_manager.t("menu_file_save", &[]), SaveDocument),
                MenuItem::action(i18n_manager.t("menu_file_save_as", &[]), SaveDocumentAs),
                MenuItem::submenu(Menu {
                    disabled: false,
                    name: i18n_manager.t("menu_export", &[]).into(),
                    items: vec![
                        MenuItem::action(i18n_manager.t("menu_export_html", &[]), ExportHtml),
                        #[cfg(feature = "export-pdf")]
                        MenuItem::action(i18n_manager.t("menu_export_pdf", &[]), ExportPdf),
                        #[cfg(feature = "export-docx")]
                        MenuItem::action(i18n_manager.t("menu_export_docx", &[]), ExportDocx),
                        MenuItem::action(i18n_manager.t("menu_export_copy_html", &[]), CopyHtml),
                        MenuItem::action(
                            i18n_manager.t("menu_export_copy_markdown", &[]),
                            CopyMarkdown,
                        ),
                    ],
                }),
                MenuItem::separator(),
                MenuItem::action(i18n_manager.t("menu_file_quit", &[]), QuitApplication),
            ],
        }]
    };

    #[cfg(target_os = "macos")]
    let help_items = {
        // 顺序与下方 mac 测试一致：检查更新 / 分隔符 / 安装或卸载 CLI / 分隔符 / 关于。
        let mut items = vec![MenuItem::action(
            i18n_manager.t("menu_help_check_updates", &[]),
            CheckForUpdates,
        )];
        items.push(MenuItem::separator());
        // 按 CLI 是否已指向当前应用显示不同菜单项，仅便携式拖拽安装的 .app 需要；
        // 经安装包安装的应用由安装脚本维护符号链接。
        if is_cli_symlink_current_app() {
            items.push(MenuItem::action(
                i18n_manager.t("menu_help_uninstall_cli", &[]),
                UninstallCliTool,
            ));
        } else {
            items.push(MenuItem::action(
                i18n_manager.t("menu_help_install_cli", &[]),
                InstallCliTool,
            ));
        }
        items.push(MenuItem::separator());
        items.push(MenuItem::action(
            i18n_manager.t("menu_help_about", &[]),
            ShowAbout,
        ));
        items
    };
    #[cfg(not(target_os = "macos"))]
    let help_items = vec![
        MenuItem::action(
            i18n_manager.t("menu_help_check_updates", &[]),
            CheckForUpdates,
        ),
        MenuItem::action(i18n_manager.t("menu_help_about", &[]), ShowAbout),
    ];

    let mut menus = initial_menus;
    menus.extend([
        Menu {
            disabled: false,
            name: i18n_manager.t("menu_language", &[]).into(),
            items: language_items,
        },
        Menu {
            disabled: false,
            name: i18n_manager.t("menu_theme", &[]).into(),
            items: theme_items,
        },
        Menu {
            disabled: false,
            name: i18n_manager.t("menu_help", &[]).into(),
            items: help_items,
        },
    ]);
    menus
}

/// 安装原生菜单。
pub(crate) fn install_menus(cx: &mut App) {
    let recent_files = recent_files_for_menu();
    let theme_entries = crate::theme::available_theme_catalog(cx);
    let menus = build_menus(
        &crate::theme::active_theme_id(cx),
        &theme_entries,
        cx.global::<I18nManager>(),
        &recent_files,
    );
    cx.set_menus(menus);
}

/// 弹出文件选择框并打开所选文件。
fn prompt_and_open_files(cx: &mut App) {
    let error_window = cx.active_window();
    prompt_and_open_files_with_error_window(cx, error_window);
}

/// 弹出文件选择框并把错误提示定位到指定窗口。
fn prompt_and_open_files_with_error_window(cx: &mut App, error_window: Option<AnyWindowHandle>) {
    let prompt_title = cx.global::<I18nManager>().t("dialog_open_file_prompt", &[]);
    let prompt = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: Some(prompt_title.into()),
    });

    cx.spawn(async move |cx| match prompt.await {
        Ok(Ok(Some(paths))) => {
            cx.update(move |cx| {
                for path in paths {
                    open_file_preferring_blank_window(cx, path, error_window);
                }
            });
        }
        Ok(Err(err)) => {
            let detail = err.to_string();
            cx.update(move |cx| {
                let title = cx
                    .global::<I18nManager>()
                    .snapshot()
                    .t("message_open_failed", &[]);
                show_window_prompt(error_window, &title, &detail, cx);
            });
        }
        Ok(Ok(None)) | Err(_) => {}
    })
    .detach();
}

/// 弹出语言包选择框并导入所选配置。
fn prompt_and_import_language_config(cx: &mut App) {
    let error_window = cx.active_window();
    prompt_and_import_language_config_with_error_window(cx, error_window);
}

/// 弹出语言包选择框并把错误定位到指定窗口。
fn prompt_and_import_language_config_with_error_window(
    cx: &mut App,
    error_window: Option<AnyWindowHandle>,
) {
    let prompt_title = cx
        .global::<I18nManager>()
        .t("dialog_add_language_prompt", &[]);
    let prompt = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(prompt_title.into()),
    });

    cx.spawn(async move |cx| match prompt.await {
        Ok(Ok(Some(paths))) => {
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            cx.update(move |cx| {
                let result = import_language_config_and_select(cx, &path);
                match result {
                    Ok(_) => {
                        install_menus(cx);
                        cx.refresh_windows();
                    }
                    Err(err) => {
                        let title = cx
                            .global::<I18nManager>()
                            .t("message_config_import_failed", &[]);
                        show_window_prompt(error_window, &title, &err.to_string(), cx);
                    }
                }
            });
        }
        Ok(Err(err)) => {
            let detail = err.to_string();
            cx.update(move |cx| {
                let title = cx
                    .global::<I18nManager>()
                    .snapshot()
                    .t("message_config_import_failed", &[]);
                show_window_prompt(error_window, &title, &detail, cx);
            });
        }
        Ok(Ok(None)) | Err(_) => {}
    })
    .detach();
}

/// 弹出主题包选择框并导入所选配置。
fn prompt_and_import_theme_config(cx: &mut App) {
    let error_window = cx.active_window();
    prompt_and_import_theme_config_with_error_window(cx, error_window);
}

/// 弹出主题包选择框并把错误定位到指定窗口。
fn prompt_and_import_theme_config_with_error_window(
    cx: &mut App,
    error_window: Option<AnyWindowHandle>,
) {
    let prompt_title = cx.global::<I18nManager>().t("dialog_add_theme_prompt", &[]);
    let prompt = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(prompt_title.into()),
    });

    cx.spawn(async move |cx| match prompt.await {
        Ok(Ok(Some(paths))) => {
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            cx.update(move |cx| {
                let result = import_theme_config_and_select(cx, &path);
                match result {
                    Ok(_) => {
                        install_menus(cx);
                        cx.refresh_windows();
                    }
                    Err(err) => {
                        let title = cx
                            .global::<I18nManager>()
                            .t("message_config_import_failed", &[]);
                        show_window_prompt(error_window, &title, &err.to_string(), cx);
                    }
                }
            });
        }
        Ok(Err(err)) => {
            let detail = err.to_string();
            cx.update(move |cx| {
                let title = cx
                    .global::<I18nManager>()
                    .snapshot()
                    .t("message_config_import_failed", &[]);
                show_window_prompt(error_window, &title, &detail, cx);
            });
        }
        Ok(Ok(None)) | Err(_) => {}
    })
    .detach();
}

/// 窗口关闭后的调度：退出流程中重新检查剩余窗口的未保存内容，普通关窗归零时退出应用。
fn handle_window_closed(cx: &mut App, _: WindowId) {
    ensure_menu_state(cx);
    if cx.global::<MenuState>().quitting {
        // 延后重新检查，避免在窗口关闭回调里同步 update 其他窗口。
        cx.spawn(async move |cx| {
            cx.update(advance_quit_window);
        })
        .detach();
        return;
    }
    if cx.windows().is_empty() {
        cx.quit();
    }
}

/// 安装菜单状态、动作处理与原生菜单栏。
pub(crate) fn init(cx: &mut App) {
    cx.set_global(MenuState::default());
    let subscription = cx.on_window_closed(handle_window_closed);
    cx.global_mut::<MenuState>().window_closed_subscription = Some(subscription);

    cx.on_action(|_: &NewWindow, cx| {
        dispatch_menu_action(&NewWindow, cx);
    });
    cx.on_action(|_: &OpenFile, cx| {
        dispatch_menu_action(&OpenFile, cx);
    });
    cx.on_action(|_: &OpenPreferences, cx| {
        dispatch_menu_action(&OpenPreferences, cx);
    });
    cx.on_action(|action: &OpenRecentFile, cx| {
        dispatch_menu_action(action, cx);
    });
    cx.on_action(|_: &NoRecentFiles, cx| {
        dispatch_menu_action(&NoRecentFiles, cx);
    });
    cx.on_action(|_: &ClearRecentFiles, cx| {
        dispatch_menu_action(&ClearRecentFiles, cx);
    });
    cx.on_action(|_: &AddLanguageConfig, cx| {
        dispatch_menu_action(&AddLanguageConfig, cx);
    });
    cx.on_action(|_: &AddThemeConfig, cx| {
        dispatch_menu_action(&AddThemeConfig, cx);
    });
    cx.on_action(|_: &SaveDocument, cx| {
        dispatch_menu_action(&SaveDocument, cx);
    });
    cx.on_action(|_: &SaveDocumentAs, cx| {
        dispatch_menu_action(&SaveDocumentAs, cx);
    });
    cx.on_action(|_: &ExportHtml, cx| {
        dispatch_menu_action(&ExportHtml, cx);
    });
    cx.on_action(|_: &ExportPdf, cx| {
        dispatch_menu_action(&ExportPdf, cx);
    });
    cx.on_action(|_: &ExportDocx, cx| {
        dispatch_menu_action(&ExportDocx, cx);
    });
    cx.on_action(|_: &CopyHtml, cx| {
        dispatch_menu_action(&CopyHtml, cx);
    });
    cx.on_action(|_: &CopyMarkdown, cx| {
        dispatch_menu_action(&CopyMarkdown, cx);
    });
    cx.on_action(|action: &SelectTheme, cx| {
        dispatch_menu_action(action, cx);
    });
    cx.on_action(|action: &SelectLanguage, cx| {
        dispatch_menu_action(action, cx);
    });
    cx.on_action(|_: &ShowAbout, cx| {
        dispatch_menu_action(&ShowAbout, cx);
    });
    cx.on_action(|_: &CheckForUpdates, cx| {
        dispatch_menu_action(&CheckForUpdates, cx);
    });
    cx.on_action(|_: &ToggleWorkspace, cx| {
        dispatch_menu_action(&ToggleWorkspace, cx);
    });
    cx.on_action(|_: &QuitApplication, cx| {
        dispatch_menu_action(&QuitApplication, cx);
    });
    cx.on_action(|_: &CloseWindow, cx| {
        dispatch_menu_action(&CloseWindow, cx);
    });
    cx.on_action(|_: &NewTab, cx| {
        dispatch_menu_action(&NewTab, cx);
    });
    cx.on_action(|_: &CloseTab, cx| {
        dispatch_menu_action(&CloseTab, cx);
    });
    cx.on_action(|_: &NextTab, cx| {
        dispatch_menu_action(&NextTab, cx);
    });
    cx.on_action(|_: &PreviousTab, cx| {
        dispatch_menu_action(&PreviousTab, cx);
    });

    install_menus(cx);
    cx.activate(true);
}

#[cfg(test)]
mod tests {
    use super::{applescript_string_literal, build_menus};
    use crate::components::{
        AddLanguageConfig, AddThemeConfig, CheckForUpdates, ClearRecentFiles, NoRecentFiles,
        OpenRecentFile, SelectLanguage, SelectTheme, ShowAbout,
    };
    use crate::i18n::I18nManager;
    use crate::theme::{THEME_ID_DARK, THEME_ID_LIGHT};
    use rgpui::MenuItem;
    use rgpui::{Bounds, WindowBackgroundAppearance, WindowDecorations, point, px, size};
    use std::path::PathBuf;

    /// 命令行工具提示文案随语言切换，且占位符都要填上。
    #[test]
    fn cli_tool_messages_are_localized() {
        let zh = I18nManager::new_with_language_id("zh-CN").snapshot();
        let en = I18nManager::new_with_language_id("en-US").snapshot();

        let detail = super::cli_installed_detail(&zh, "/usr/local/bin/rumark");
        assert!(detail.contains("/usr/local/bin/rumark"), "zh: {detail}");
        assert!(
            !detail.contains('{'),
            "zh still has a placeholder: {detail}"
        );

        // 用户取消与真实失败走不同键值。
        let cancelled = super::cli_install_failure_detail(&zh, "User canceled (-128)");
        assert_eq!(cancelled, zh.t("message_cli_install_cancelled", &[]));
        let failed = super::cli_install_failure_detail(&en, "permission denied");
        assert!(failed.contains("permission denied"), "en: {failed}");
        assert_eq!(
            failed,
            en.t(
                "message_cli_install_failed",
                &[("error", "permission denied")]
            )
        );

        let uninstall_failed = super::cli_uninstall_failure_detail(&en, "Operation not permitted");
        assert!(
            uninstall_failed.contains("Operation not permitted"),
            "en: {uninstall_failed}"
        );
    }

    #[test]
    fn window_options_use_transparent_component_titlebar() {
        let bounds = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(800.0), px(600.0)),
        };
        let options = super::rumark_window_options(bounds);
        // Windows 无边框只认该标记：丢了就会冒出原生标题栏。
        assert!(options.app_owns_titlebar_drag);
        assert_eq!(options.window_decorations, Some(WindowDecorations::Client));
        assert_eq!(
            options.window_background,
            WindowBackgroundAppearance::Opaque
        );
        let titlebar = options.titlebar.expect("titlebar options");
        assert!(titlebar.appears_transparent);
        // 任务栏标题不在选项里带（组件基座默认无标题），运行时由各窗口
        // 经 `set_window_title` 设置——此处只锁定该约定。
        assert!(titlebar.title.is_none());
    }

    /// 取动作菜单项的展示名称（测试辅助）。
    fn action_name(item: &MenuItem) -> &str {
        match item {
            MenuItem::Action { name, .. } => name.as_ref(),
            _ => panic!("expected action menu item"),
        }
    }

    /// 取子菜单项中的子菜单（测试辅助）。
    fn submenu(item: &MenuItem) -> &rgpui::Menu {
        match item {
            MenuItem::Submenu(menu) => menu,
            _ => panic!("expected submenu item"),
        }
    }

    #[test]
    fn applescript_string_literal_escapes_special_characters() {
        assert_eq!(
            applescript_string_literal(r#"/Applications/Rumark "Test".app/Contents/MacOS/rumark"#),
            r#""/Applications/Rumark \"Test\".app/Contents/MacOS/rumark""#
        );
        assert_eq!(
            applescript_string_literal(r#"/Applications/O'Brien\Rumark.app"#),
            r#""/Applications/O'Brien\\Rumark.app""#
        );
    }

    // macOS 下菜单栏为：[拾墨应用菜单, 文件, 语言, 主题, 帮助]
    // 其他平台为：      [文件, 语言, 主题, 帮助]

    #[cfg(target_os = "macos")]
    const LANGUAGE_MENU_INDEX: usize = 2;
    #[cfg(not(target_os = "macos"))]
    const LANGUAGE_MENU_INDEX: usize = 1;

    #[cfg(target_os = "macos")]
    const THEME_MENU_INDEX: usize = 3;
    #[cfg(not(target_os = "macos"))]
    const THEME_MENU_INDEX: usize = 2;

    #[cfg(target_os = "macos")]
    const HELP_MENU_INDEX: usize = 4;
    #[cfg(not(target_os = "macos"))]
    const HELP_MENU_INDEX: usize = 3;

    #[test]
    fn build_menus_uses_english_fallback_by_default() {
        let i18n_manager = I18nManager::default();
        let menus = build_menus(
            THEME_ID_DARK,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );

        let menu_names = menus
            .iter()
            .map(|menu| menu.name.to_string())
            .collect::<Vec<_>>();

        #[cfg(target_os = "macos")]
        assert_eq!(
            menu_names,
            vec!["Rumark", "File", "Language", "Theme", "Help"]
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(menu_names, vec!["File", "Language", "Theme", "Help"]);

        // 新建窗口在 macOS 下归属文件操作，在其他平台仍为文件菜单首项。
        #[cfg(target_os = "macos")]
        assert_eq!(action_name(&menus[1].items[0]), "New Window");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action_name(&menus[0].items[0]), "New Window");

        // 新建/关闭标签页紧随新建窗口之后。
        #[cfg(target_os = "macos")]
        {
            assert_eq!(action_name(&menus[1].items[1]), "New Tab");
            assert_eq!(action_name(&menus[1].items[2]), "Close Tab");
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(action_name(&menus[0].items[1]), "New Tab");
            assert_eq!(action_name(&menus[0].items[2]), "Close Tab");
        }

        // 最近文件子菜单位置因平台而异。
        #[cfg(target_os = "macos")]
        assert_eq!(
            submenu(&menus[1].items[5]).name.to_string(),
            "Open Recent File"
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            submenu(&menus[0].items[5]).name.to_string(),
            "Open Recent File"
        );

        // 关闭窗口与新建窗口同在文件菜单中。
        #[cfg(target_os = "macos")]
        assert_eq!(action_name(&menus[1].items[3]), "Close Window");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action_name(&menus[0].items[3]), "Close Window");

        // 偏好设置位置因平台而异。
        #[cfg(target_os = "macos")]
        assert_eq!(action_name(&menus[0].items[0]), "Preferences");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action_name(&menus[0].items[6]), "Preferences");

        assert_eq!(
            action_name(&menus[LANGUAGE_MENU_INDEX].items[0]),
            "简体中文"
        );
        assert_eq!(
            action_name(&menus[LANGUAGE_MENU_INDEX].items[1]),
            "\u{2713} English"
        );
    }

    #[test]
    fn build_menus_uses_chinese_language_when_selected() {
        let i18n_manager = I18nManager::new_with_language_id("zh-CN");
        let menus = build_menus(
            THEME_ID_DARK,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );

        #[cfg(target_os = "macos")]
        assert_eq!(
            submenu(&menus[1].items[5]).name.to_string(),
            i18n_manager.t("menu_file_recent", &[])
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            submenu(&menus[0].items[5]).name.to_string(),
            i18n_manager.t("menu_file_recent", &[])
        );

        let menu_names = menus
            .iter()
            .map(|menu| menu.name.to_string())
            .collect::<Vec<_>>();

        #[cfg(target_os = "macos")]
        assert_eq!(menu_names, vec!["Rumark", "文件", "语言", "主题", "帮助"]);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(menu_names, vec!["文件", "语言", "主题", "帮助"]);

        #[cfg(target_os = "macos")]
        assert_eq!(action_name(&menus[1].items[0]), "新建窗口");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action_name(&menus[0].items[0]), "新建窗口");
        assert_eq!(
            action_name(&menus[LANGUAGE_MENU_INDEX].items[0]),
            "✓ 简体中文"
        );
        assert_eq!(action_name(&menus[LANGUAGE_MENU_INDEX].items[1]), "English");
    }

    #[test]
    fn language_menu_items_dispatch_select_language_actions() {
        let i18n_manager = I18nManager::default();
        let menus = build_menus(
            THEME_ID_DARK,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );

        match &menus[LANGUAGE_MENU_INDEX].items[0] {
            MenuItem::Action { action, .. } => {
                let action = action
                    .as_any()
                    .downcast_ref::<SelectLanguage>()
                    .expect("language item should dispatch SelectLanguage");
                assert_eq!(action.language_id, "zh-CN");
            }
            _ => panic!("expected language action item"),
        }
    }

    #[test]
    fn recent_files_submenu_uses_empty_state_when_history_is_empty() {
        let i18n_manager = I18nManager::default();
        let menus = build_menus(
            THEME_ID_DARK,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );

        // macOS 下文件菜单序号为 1，其他平台为 0；新建/关闭标签页插入两项后，打开最近文件为第 5 项。
        #[cfg(target_os = "macos")]
        let recent_menu = submenu(&menus[1].items[5]);
        #[cfg(not(target_os = "macos"))]
        let recent_menu = submenu(&menus[0].items[5]);

        assert_eq!(recent_menu.name.to_string(), "Open Recent File");
        assert_eq!(recent_menu.items.len(), 1);
        assert_eq!(action_name(&recent_menu.items[0]), "No Recent Files");
        match &recent_menu.items[0] {
            MenuItem::Action { action, .. } => {
                assert!(action.as_any().is::<NoRecentFiles>());
            }
            _ => panic!("expected empty recent-file action item"),
        }
    }

    #[test]
    fn recent_files_submenu_dispatches_path_actions() {
        let i18n_manager = I18nManager::default();
        let recent_files = vec![
            PathBuf::from(r"C:\docs\one.md"),
            PathBuf::from(r"D:\notes\two.markdown"),
        ];
        let menus = build_menus(
            "rumark",
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &recent_files,
        );

        #[cfg(target_os = "macos")]
        let recent_menu = submenu(&menus[1].items[5]);
        #[cfg(not(target_os = "macos"))]
        let recent_menu = submenu(&menus[0].items[5]);

        assert_eq!(recent_menu.items.len(), 4);
        assert_eq!(action_name(&recent_menu.items[0]), r"C:\docs\one.md");
        match &recent_menu.items[0] {
            MenuItem::Action { action, .. } => {
                let action = action
                    .as_any()
                    .downcast_ref::<OpenRecentFile>()
                    .expect("recent file should dispatch OpenRecentFile");
                assert_eq!(action.path, r"C:\docs\one.md");
            }
            _ => panic!("expected recent-file action item"),
        }
        assert_eq!(action_name(&recent_menu.items[1]), r"D:\notes\two.markdown");
        assert!(matches!(recent_menu.items[2], MenuItem::Separator));
        assert_eq!(action_name(&recent_menu.items[3]), "Clear Recent Files");
        match &recent_menu.items[3] {
            MenuItem::Action { action, .. } => {
                assert!(action.as_any().is::<ClearRecentFiles>());
            }
            _ => panic!("expected clear-recent-files action item"),
        }
    }

    #[test]
    fn config_import_items_are_bottom_menu_actions() {
        let i18n_manager = I18nManager::default();
        let menus = build_menus(
            THEME_ID_DARK,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );

        let language_items = &menus[LANGUAGE_MENU_INDEX].items;
        assert!(matches!(
            language_items[language_items.len() - 2],
            MenuItem::Separator
        ));
        assert_eq!(
            action_name(&language_items[language_items.len() - 1]),
            "Add Language Config"
        );
        match &language_items[language_items.len() - 1] {
            MenuItem::Action { action, .. } => {
                assert!(action.as_any().is::<AddLanguageConfig>());
            }
            _ => panic!("expected add language config action item"),
        }

        let theme_items = &menus[THEME_MENU_INDEX].items;
        assert_eq!(theme_items.len(), 4);
        assert_eq!(action_name(&theme_items[0]), "\u{2713} Rumark");
        assert_eq!(action_name(&theme_items[1]), "Rumark Light");
        match &theme_items[0] {
            MenuItem::Action { action, .. } => {
                assert!(action.as_any().is::<SelectTheme>());
            }
            _ => panic!("expected select theme action item"),
        }
        assert!(matches!(theme_items[2], MenuItem::Separator));
        assert_eq!(action_name(&theme_items[3]), "Add Theme Config");
        match &theme_items[3] {
            MenuItem::Action { action, .. } => {
                assert!(action.as_any().is::<AddThemeConfig>());
            }
            _ => panic!("expected add theme config action item"),
        }
    }

    #[test]
    fn theme_menu_marks_selected_builtin_light_theme() {
        let i18n_manager = I18nManager::default();
        let menus = build_menus(
            THEME_ID_LIGHT,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );
        let theme_items = &menus[THEME_MENU_INDEX].items;

        assert_eq!(action_name(&theme_items[0]), "Rumark");
        assert_eq!(action_name(&theme_items[1]), "\u{2713} Rumark Light");
        match &theme_items[1] {
            MenuItem::Action { action, .. } => {
                let action = action
                    .as_any()
                    .downcast_ref::<SelectTheme>()
                    .expect("light theme item should dispatch SelectTheme");
                assert_eq!(action.theme_id, "rumark-light");
            }
            _ => panic!("expected light theme action item"),
        }
    }

    #[test]
    fn help_menu_first_item_is_check_updates() {
        let i18n_manager = I18nManager::default();
        let menus = build_menus(
            THEME_ID_DARK,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );
        let help_items = &menus[HELP_MENU_INDEX].items;

        match &help_items[0] {
            MenuItem::Action { action, .. } => {
                assert!(action.as_any().is::<CheckForUpdates>());
            }
            _ => panic!("expected check updates action item"),
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn help_menu_contains_check_updates_and_about() {
        let i18n_manager = I18nManager::default();
        let menus = build_menus(
            THEME_ID_DARK,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );
        let help_items = &menus[HELP_MENU_INDEX].items;

        assert_eq!(help_items.len(), 2);
        match &help_items[0] {
            MenuItem::Action { action, .. } => {
                assert!(action.as_any().is::<CheckForUpdates>());
            }
            _ => panic!("expected check updates action item"),
        }
        match &help_items[1] {
            MenuItem::Action { action, .. } => {
                assert!(action.as_any().is::<ShowAbout>());
            }
            _ => panic!("expected about action item"),
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn help_menu_contains_cli_and_about_on_macos() {
        let i18n_manager = I18nManager::default();
        let menus = build_menus(
            THEME_ID_DARK,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );
        let help_items = &menus[HELP_MENU_INDEX].items;

        // 依次为检查更新、分隔符、安装/卸载命令行工具、分隔符、关于。
        assert_eq!(help_items.len(), 5);
        assert!(matches!(help_items[3], MenuItem::Separator));
        match &help_items[4] {
            MenuItem::Action { action, .. } => {
                assert!(action.as_any().is::<ShowAbout>());
            }
            _ => panic!("expected about action item"),
        }
    }
}
