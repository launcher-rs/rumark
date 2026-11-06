//! 拾墨 Rumark：基于 rgpui 的块式 Markdown 编辑器。
//!
//! 从命令行参数读取文件路径，每个文件打开一个窗口；无参数时创建单个空窗口。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[cfg(target_os = "macos")]
use futures::{StreamExt, channel::mpsc};
use rgpui::*;

mod app_identity;
mod app_menu;
mod atomic_file;
mod components;
mod config;
mod editor;
/// PDF/DOCX 全关时共享块模型暂无调用方，条件放行死代码警告。
#[cfg_attr(
    not(any(feature = "export-pdf", feature = "export-docx")),
    allow(dead_code)
)]
mod export;
#[cfg(any(target_os = "macos", test))]
mod file_url;
mod i18n;
mod net;
mod text_offsets;
mod theme;

use app_menu::{init as init_app_menu, open_editor_window};
use components::init_with_keybindings as init_editor;
#[cfg(target_os = "macos")]
use file_url::parse_file_url;
use i18n::I18nManager;

actions!(inspector_demo, [ToggleInspector]);

/// 按启动偏好打开初始窗口。
fn open_startup_window(cx: &mut App, startup_open: config::StartupOpenPreference) {
    if startup_open == config::StartupOpenPreference::LastOpenedFile
        && let Some(path) = config::first_existing_recent_markdown_file()
    {
        match std::fs::read_to_string(&path) {
            Ok(markdown) => {
                open_editor_window(cx, markdown, Some(path));
                return;
            }
            Err(err) => {
                eprintln!(
                    "failed to read last opened file '{}': {err}",
                    path.display()
                );
            }
        }
    }

    open_editor_window(cx, String::new(), None);
}

/// 解析命令行参数并启动应用主循环。
fn main() {
    // 框架及其平台后端的诊断日志：不调用则 `log` 输出全部静默丢弃。
    // 默认只留 error，`RUST_LOG=debug` 等可提高；release 下 Windows 无
    // 控制台，需重定向 stderr 或从终端启动才看得到。
    init_logging();

    let args: Vec<String> = std::env::args().collect();

    // 解析命令行参数
    let mut detach = false;
    let mut input_paths = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-v" => {
                println!("rumark {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                println!(
                    "rumark {} - A block-based Markdown editor",
                    env!("CARGO_PKG_VERSION")
                );
                println!();
                println!("USAGE:");
                println!("    rumark [OPTIONS] [FILES...]");
                println!();
                println!("OPTIONS:");
                println!("    -v, --version    Print version information");
                println!("    -h, --help       Print this help message");
                println!("    -d, --detach     Launch in background (non-blocking)");
                println!();
                println!("FILES:");
                println!("    One or more markdown files to open. If no files are specified,");
                println!("    opens an empty document.");
                return;
            }
            "--detach" | "-d" => {
                detach = true;
            }
            option if option.starts_with('-') => {
                eprintln!("Unknown option: {}", option);
                std::process::exit(1);
            }
            path => {
                input_paths.push(PathBuf::from(path));
            }
        }
        i += 1;
    }

    #[cfg(not(target_os = "macos"))]
    let _ = detach;

    // macOS 下按需脱离终端后台运行
    // 待办：其他平台可能也需要适配
    #[cfg(target_os = "macos")]
    if detach {
        use std::process::Command;

        // 以去掉 --detach 参数的方式在后台重新启动应用
        let exe_path = std::env::current_exe().expect("Failed to get executable path");
        let non_detach_args: Vec<String> = args
            .iter()
            .filter(|arg| *arg != "--detach" && *arg != "-d")
            .cloned()
            .collect();

        Command::new(exe_path)
            .args(&non_detach_args[1..])
            .spawn()
            .expect("Failed to detach process");

        return;
    }

    #[cfg(target_os = "macos")]
    let (open_file_tx, mut open_file_rx) = mpsc::unbounded::<PathBuf>();
    #[cfg(target_os = "macos")]
    let open_file_requested = Arc::new(AtomicBool::new(false));

    let app = rgpui_platform::application();

    #[cfg(target_os = "macos")]
    {
        let open_file_requested_for_callback = open_file_requested.clone();
        app.on_open_urls(move |urls| {
            for url in urls {
                let Some(path) = parse_file_url(&url) else {
                    continue;
                };
                open_file_requested_for_callback.store(true, Ordering::SeqCst);
                let _ = open_file_tx.unbounded_send(path);
            }
        });
    }

    app.run(move |cx: &mut App| {
        let (preferences, recovery) =
            config::load_or_create_app_preferences().unwrap_or_else(|err| {
                eprintln!("failed to initialize app preferences: {err}");
                (Default::default(), None)
            });
        // 配置损坏时已备份原文件并重建默认配置：登记提示，由首个窗口弹窗告知用户。
        if let Some(recovery) = recovery {
            config::PendingConfigRecovery::set(cx, recovery);
        }
        I18nManager::init_with_language_id(cx, &preferences.default_language_id);
        crate::theme::init(cx, &preferences.default_theme_id);
        config::EditorSettings::init(cx, preferences.table_headers_visible);
        net::install_http_client(cx);
        init_editor(cx, &preferences.keybindings);
        // 工作区文件树的键盘导航（上/下/左/右）绑定。
        rgpui::tree::init(cx);
        init_app_menu(cx);

        // 一行启用默认检查器面板（含 Div 布局展示注册）。
        // 下面整个块仅调试版本编译：release 下无检查器代码、无 F12 绑定。
        // F12 用全局动作 helper（全局绑定 + 打活动窗口 + spawn 延后更新），
        // 不依赖视图焦点链。

        cx.enable_default_inspector();

        cx.on_global_action(ToggleInspector, Some("f12"), |window, cx| {
            window.toggle_inspector(cx);
        });

        #[cfg(target_os = "macos")]
        cx.spawn(async move |cx| {
            while let Some(path) = open_file_rx.next().await {
                let _ = cx.update(move |cx| {
                    if let Err(err) = app_menu::open_file_in_new_window(cx, &path) {
                        eprintln!("failed to open '{}': {err}", path.display());
                    }
                });
            }
        })
        .detach();

        if input_paths.is_empty() {
            #[cfg(target_os = "macos")]
            {
                let startup_open = preferences.startup_open;
                let open_file_requested = open_file_requested.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(150))
                        .await;
                    if !open_file_requested.load(Ordering::SeqCst) {
                        let _ = cx.update(move |cx| open_startup_window(cx, startup_open));
                    }
                })
                .detach();
            }

            #[cfg(not(target_os = "macos"))]
            open_startup_window(cx, preferences.startup_open);

            return;
        }

        for path in &input_paths {
            let absolute_path = if path.is_absolute() {
                path.clone()
            } else {
                match std::env::current_dir() {
                    Ok(cwd) => cwd.join(path),
                    Err(_) => path.clone(),
                }
            };

            match std::fs::read_to_string(&absolute_path) {
                Ok(markdown) => {
                    if let Err(err) = config::record_recent_file(&absolute_path) {
                        eprintln!("failed to update recent file history: {err}");
                    }
                    open_editor_window(cx, markdown, Some(absolute_path));
                }
                Err(err) => {
                    eprintln!(
                        "failed to read '{}': {err}. opened as an untitled empty document.",
                        absolute_path.display()
                    );
                    // 读取失败即不关联原路径：否则用户按保存会把空文档覆盖回原文件。
                    open_editor_window(cx, String::new(), None);
                }
            }
        }
        app_menu::install_menus(cx);
        cx.refresh_windows();
    });
}
