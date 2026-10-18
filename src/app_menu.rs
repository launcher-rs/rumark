//! 原生应用菜单、应用级动作与窗口关闭路由。
//!
//! 本模块负责构造菜单与操作当前编辑器窗口的动作，退出动作会路由到当前窗口，
//! 以便该窗口的未保存更改对话框保持最终决定权。

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rgpui::*;

use crate::app_identity::RUMARK_APP_ID;
use crate::components::{
    AddLanguageConfig, AddThemeConfig, ClearRecentFiles, CloseWindow, CopyHtml, CopyMarkdown,
    ExportDocx, ExportHtml, ExportPdf, InstallCliTool, NewWindow, NoRecentFiles, OpenFile,
    OpenPreferences, OpenRecentFile, QuitApplication, SaveDocument, SaveDocumentAs, SelectLanguage,
    SelectTheme, ShowAbout, ToggleWorkspace, UninstallCliTool,
};
use crate::config::{
    apply_configured_language, apply_configured_theme, clear_recent_files,
    import_language_config_and_select, import_theme_config_and_select, open_preferences_window,
    read_recent_files, record_recent_file, remove_recent_file,
};
use crate::editor::Editor;
use crate::i18n::I18nManager;
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
}

impl Global for MenuState {}

/// 按给定 Markdown 内容与可选路径打开编辑器窗口。
pub(crate) fn open_editor_window(
    cx: &mut App,
    markdown: String,
    file_path: Option<PathBuf>,
) -> WindowHandle<Editor> {
    let bounds = Bounds::centered(None, size(px(1080.), px(720.)), cx);
    let handle = cx
        .open_window(rumark_window_options(bounds), move |_window, cx| {
            cx.new(move |cx| Editor::from_markdown(cx, markdown, file_path))
        })
        .unwrap();

    handle
        .update(cx, |editor, window, cx| {
            window.activate_window();
            editor.force_install_close_guard(cx, window);
        })
        .expect("newly opened editor window should be updateable");

    handle
}

/// 在新窗口中打开指定文件。
pub(crate) fn open_file_in_new_window(cx: &mut App, path: &Path) -> anyhow::Result<()> {
    let markdown = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read '{}'", path.display()))?;
    open_editor_window(cx, markdown, Some(path.to_path_buf()));
    record_recent_file_and_refresh(path, cx);
    Ok(())
}

/// 若当前活动窗口是「用户尚未输入任何内容的空白编辑器」，则把文件就地载入该窗口。
///
/// 返回是否已复用当前窗口（`false` 表示调用方应改为新建窗口）。
/// 读取失败时不会改动窗口状态，交由调用方按新建窗口路径统一报错。
fn try_load_into_active_blank_window(cx: &mut App, path: &Path) -> bool {
    let Some(window) = cx
        .active_window()
        .and_then(|window| window.downcast::<Editor>())
    else {
        return false;
    };
    let blank = window
        .read_with(cx, |editor, cx| editor.is_blank_document(cx))
        .unwrap_or(false);
    if !blank {
        return false;
    }

    let path = path.to_path_buf();
    window
        .update(cx, |editor, window, cx| {
            if editor.replace_document_from_path(&path, cx).is_ok() {
                window.set_window_edited(false);
                window.activate_window();
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
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
                let title = cx
                    .global::<I18nManager>()
                    .snapshot()
                    .t("message_open_failed", &[]);
                show_window_prompt(error_window, &title, &err.to_string(), cx);
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

#[cfg(target_os = "macos")]
/// 安装命令行工具符号链接。
pub(crate) fn install_cli_tool(cx: &mut App) {
    use std::process::Command;

    let bin_link = "/usr/local/bin/rumark";

    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            show_install_cli_error(cx, &format!("Failed to get executable path: {err}"));
            return;
        }
    };

    // 仅允许来自便携式 .app 包（如拖入 /Applications 安装）的调用
    if !current_exe
        .to_string_lossy()
        .contains(".app/Contents/MacOS/")
    {
        show_install_cli_error(
            cx,
            "Command-line tool installation requires running from an .app bundle.\n\n\
             If the app was installed via the `.pkg` installer,\n\
             the CLI command is configured automatically.",
        );
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
                let title = "CLI Command Installed";
                let detail = format!(
                    "Successfully installed! You can now use 'rumark' from the terminal:\n\n\
                     \x1b[1mrumark README.md\x1b[0m\n\
                     \x1b[1mrumark file1.md file2.md\x1b[0m\n\n\
                     Location: {bin_link}\n\n\
                     Note: If you move or delete Rumark.app,\n\
                     the 'rumark' command will stop working\n\
                     automatically (no cleanup needed)."
                );
                show_window_prompt_with_level(
                    cx.active_window(),
                    PromptLevel::Info,
                    title,
                    &detail,
                    cx,
                );
            } else {
                // 用户在管理员密码框点了取消，或链接创建因其他原因失败。
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let detail = if stderr.contains("User canceled") || stderr.contains("(-128)") {
                    "Installation cancelled.".to_string()
                } else {
                    format!("Installation failed: {stderr}")
                };
                show_install_cli_error(cx, &detail);
            }
        }
        Err(err) => {
            show_install_cli_error(cx, &format!("Failed to run installer: {err}"));
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

    if !is_cli_symlink_current_app() {
        show_install_cli_error(cx, "CLI command is not installed for this app.");
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
                let title = "CLI Command Uninstalled";
                let detail = "CLI command has been removed successfully.".to_string();
                show_window_prompt_with_level(
                    cx.active_window(),
                    PromptLevel::Info,
                    title,
                    &detail,
                    cx,
                );
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let detail = if stderr.contains("User canceled") || stderr.contains("(-128)") {
                    "Uninstall cancelled.".to_string()
                } else {
                    format!("Uninstall failed: {stderr}")
                };
                show_install_cli_error(cx, &detail);
            }
        }
        Err(err) => {
            show_install_cli_error(cx, &format!("Failed to run uninstaller: {err}"));
        }
    }
    // 刷新菜单，使安装/卸载标签随之切换。
    install_menus(cx);
}

#[cfg(not(target_os = "macos"))]
/// 安装命令行工具（非 macOS 下仅提示不支持）。
pub(crate) fn install_cli_tool(cx: &mut App) {
    show_install_cli_error(
        cx,
        "Command-line tool installation is only available on macOS.",
    );
}

#[cfg(not(target_os = "macos"))]
/// 卸载命令行工具（非 macOS 下仅提示不支持）。
pub(crate) fn uninstall_cli_tool(cx: &mut App) {
    show_install_cli_error(
        cx,
        "Command-line tool uninstallation is only available on macOS.",
    );
}

/// 显示命令行工具安装失败提示。
fn show_install_cli_error(cx: &mut App, detail: &str) {
    let title = "Install Command-Line Tool Failed";
    show_window_prompt_with_level(cx.active_window(), PromptLevel::Critical, title, detail, cx);
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
    let Some(window) = window.downcast::<Editor>() else {
        return;
    };
    cx.spawn(async move |cx| {
        let _ = window.update(cx, |editor, window, cx| update(editor, window, cx));
    })
    .detach();
}

/// 在当前活动编辑器上打开关于对话框（框架 `Dialog`）。
fn show_about_dialog_on_active_editor(cx: &mut App) {
    with_active_editor(cx, move |editor, window, cx| {
        editor.open_about_dialog(window, cx);
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
    let Some(window) = window.downcast::<Editor>() else {
        return false;
    };

    window
        .update(cx, |editor, window, cx| {
            editor.request_close_current_window(window, cx);
        })
        .is_ok()
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
pub(crate) fn request_quit_application(cx: &mut App) {
    let candidates = current_window_candidates(cx);
    if candidates.is_empty() {
        cx.quit();
        return;
    }

    // 见 `request_close_current_editor_window`：必须延后执行，否则同步 `update`
    // 必失败，导致单窗口下退出/关闭菜单静默失效。
    cx.spawn(async move |cx| {
        for window in candidates {
            let Some(window) = window.downcast::<Editor>() else {
                continue;
            };

            let should_close = window
                .update(cx, |editor, window, cx| {
                    editor.on_window_should_close(window, cx)
                })
                .unwrap_or(false);
            if !should_close {
                return;
            }
        }

        cx.update(|cx| cx.quit());
    })
    .detach();
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
        // 按 CLI 是否已指向当前应用显示不同菜单项，仅便携式拖拽安装的 .app 需要；
        // 经安装包安装的应用由安装脚本维护符号链接。
        let cli_installed = is_cli_symlink_current_app();
        let mut items = vec![];
        if cli_installed {
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
    let help_items = vec![MenuItem::action(
        i18n_manager.t("menu_help_about", &[]),
        ShowAbout,
    )];

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
            name: i18n_manager.t("menu_workspace", &[]).into(),
            items: vec![MenuItem::action(
                i18n_manager.t("menu_workspace_toggle", &[]),
                ToggleWorkspace,
            )],
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

/// 窗口全部关闭后退出应用。
fn handle_window_closed(cx: &mut App, _: WindowId) {
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
    cx.on_action(|_: &ToggleWorkspace, cx| {
        dispatch_menu_action(&ToggleWorkspace, cx);
    });
    cx.on_action(|_: &QuitApplication, cx| {
        dispatch_menu_action(&QuitApplication, cx);
    });
    cx.on_action(|_: &CloseWindow, cx| {
        dispatch_menu_action(&CloseWindow, cx);
    });

    install_menus(cx);
    cx.activate(true);
}

#[cfg(test)]
mod tests {
    use super::{applescript_string_literal, build_menus};
    use crate::components::{
        AddLanguageConfig, AddThemeConfig, ClearRecentFiles, NoRecentFiles, OpenRecentFile,
        SelectLanguage, SelectTheme, ShowAbout,
    };
    use crate::i18n::I18nManager;
    use crate::theme::{THEME_ID_DARK, THEME_ID_LIGHT};
    use rgpui::MenuItem;
    use rgpui::{Bounds, WindowBackgroundAppearance, WindowDecorations, point, px, size};
    use std::path::PathBuf;

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

    // macOS 下菜单栏为：[拾墨应用菜单, File, Language, Theme, Workspace, Help]
    // 其他平台为：      [File, Language, Theme, Workspace, Help]

    #[cfg(target_os = "macos")]
    const LANGUAGE_MENU_INDEX: usize = 2;
    #[cfg(not(target_os = "macos"))]
    const LANGUAGE_MENU_INDEX: usize = 1;

    #[cfg(target_os = "macos")]
    const THEME_MENU_INDEX: usize = 3;
    #[cfg(not(target_os = "macos"))]
    const THEME_MENU_INDEX: usize = 2;

    #[cfg(target_os = "macos")]
    const WORKSPACE_MENU_INDEX: usize = 4;
    #[cfg(not(target_os = "macos"))]
    const WORKSPACE_MENU_INDEX: usize = 3;

    #[cfg(target_os = "macos")]
    const HELP_MENU_INDEX: usize = 5;
    #[cfg(not(target_os = "macos"))]
    const HELP_MENU_INDEX: usize = 4;

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
            vec!["Rumark", "File", "Language", "Theme", "Workspace", "Help"]
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            menu_names,
            vec!["File", "Language", "Theme", "Workspace", "Help"]
        );

        // 新建窗口在 macOS 下归属文件操作，在其他平台仍为文件菜单首项。
        #[cfg(target_os = "macos")]
        assert_eq!(action_name(&menus[1].items[0]), "New Window");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action_name(&menus[0].items[0]), "New Window");

        // 最近文件子菜单位置因平台而异。
        #[cfg(target_os = "macos")]
        assert_eq!(
            submenu(&menus[1].items[3]).name.to_string(),
            "Open Recent File"
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            submenu(&menus[0].items[3]).name.to_string(),
            "Open Recent File"
        );

        // 关闭窗口与新建窗口同在文件菜单中。
        #[cfg(target_os = "macos")]
        assert_eq!(action_name(&menus[1].items[1]), "Close Window");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action_name(&menus[0].items[1]), "Close Window");

        // 偏好设置位置因平台而异。
        #[cfg(target_os = "macos")]
        assert_eq!(action_name(&menus[0].items[0]), "Preferences");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action_name(&menus[0].items[4]), "Preferences");

        assert_eq!(
            action_name(&menus[LANGUAGE_MENU_INDEX].items[0]),
            "简体中文"
        );
        assert_eq!(
            action_name(&menus[LANGUAGE_MENU_INDEX].items[1]),
            "\u{2713} English"
        );
        assert_eq!(
            action_name(&menus[WORKSPACE_MENU_INDEX].items[0]),
            "Toggle Workspace"
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
            submenu(&menus[1].items[3]).name.to_string(),
            i18n_manager.t("menu_file_recent", &[])
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            submenu(&menus[0].items[3]).name.to_string(),
            i18n_manager.t("menu_file_recent", &[])
        );

        let menu_names = menus
            .iter()
            .map(|menu| menu.name.to_string())
            .collect::<Vec<_>>();

        #[cfg(target_os = "macos")]
        assert_eq!(
            menu_names,
            vec!["Rumark", "文件", "语言", "主题", "工作区", "帮助"]
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(menu_names, vec!["文件", "语言", "主题", "工作区", "帮助"]);

        #[cfg(target_os = "macos")]
        assert_eq!(action_name(&menus[1].items[0]), "新建窗口");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(action_name(&menus[0].items[0]), "新建窗口");
        assert_eq!(
            action_name(&menus[LANGUAGE_MENU_INDEX].items[0]),
            "✓ 简体中文"
        );
        assert_eq!(action_name(&menus[LANGUAGE_MENU_INDEX].items[1]), "English");
        assert_eq!(
            action_name(&menus[WORKSPACE_MENU_INDEX].items[0]),
            "切换工作区"
        );
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

        // macOS 下文件菜单序号为 1，其中打开最近文件为第 3 项；其他平台文件菜单序号为 0，同样第 3 项。
        #[cfg(target_os = "macos")]
        let recent_menu = submenu(&menus[1].items[3]);
        #[cfg(not(target_os = "macos"))]
        let recent_menu = submenu(&menus[0].items[3]);

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
        let recent_menu = submenu(&menus[1].items[3]);
        #[cfg(not(target_os = "macos"))]
        let recent_menu = submenu(&menus[0].items[3]);

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
    fn help_menu_first_item_is_about() {
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
                assert!(action.as_any().is::<ShowAbout>());
            }
            _ => panic!("expected about action item"),
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn help_menu_contains_about_only() {
        let i18n_manager = I18nManager::default();
        let menus = build_menus(
            THEME_ID_DARK,
            &crate::theme::bundled_theme_catalog(),
            &i18n_manager,
            &[],
        );
        let help_items = &menus[HELP_MENU_INDEX].items;

        assert_eq!(help_items.len(), 1);
        match &help_items[0] {
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
