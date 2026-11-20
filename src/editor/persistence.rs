//! 文档保存与读取操作。
//!
//! 渲染模式把语义块树序列化回规范 Markdown；
//! 源码模式直接写原始缓冲区，以保留字面分隔符。
//! 读取侧统一走 [`read_markdown_file`]，负责 BOM 与非 UTF-8 判定。

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rgpui::*;

use super::Editor;
#[cfg(any(feature = "export-pdf", feature = "export-docx"))]
use crate::export::ExportFormat;
use crate::i18n::{I18nManager, I18nSnapshot};

/// 统计文本中指定标记的最长连续长度。
fn longest_marker_run(text: &str, marker: char) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;

    for ch in text.chars() {
        if ch == marker {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }

    longest
}

/// 选取不会与内容冲突的安全代码围栏（反引号或波浪线）。
pub(super) fn safe_code_fence(content: &str) -> String {
    let longest_backticks = longest_marker_run(content, '`');
    if longest_backticks < 3 {
        return "```".to_string();
    }

    let longest_tildes = longest_marker_run(content, '~');
    "~".repeat(longest_tildes.max(2) + 1)
}

/// 带语言信息时选取安全围栏：信息串含反引号则用波浪线。
pub(super) fn safe_code_fence_with_info(content: &str, info: Option<&str>) -> String {
    if info.is_some_and(|info| info.contains('`')) {
        let longest_tildes = longest_marker_run(content, '~');
        return "~".repeat(longest_tildes.max(2) + 1);
    }

    safe_code_fence(content)
}

/// 计算代码块序列化围栏：优先沿用解析时记录的原文围栏变体（`~~~`、更长的
/// 围栏串），保证保存往返不改写用户写法；变体对当前内容不再安全闭合
/// （内容中出现长度达到围栏的同类标记串，或反引号围栏配含反引号的信息串）
/// 时回退到按内容重算。
pub(super) fn code_fence_markdown(
    content: &str,
    info: Option<&str>,
    variant: Option<(char, usize)>,
) -> String {
    let computed = safe_code_fence_with_info(content, info);
    if let Some((ch, len)) = variant {
        let info_has_backtick = info.is_some_and(|info| info.contains('`'));
        let closes_safely =
            !(ch == '`' && info_has_backtick) && longest_marker_run(content, ch) < len;
        if closes_safely {
            return ch.to_string().repeat(len);
        }
    }
    computed
}

/// UTF-8 字节序标记：仅出现在文件开头时剥离。
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// 非 UTF-8 文档错误：展示方据此换成「编码不支持」文案，而不是原始解码错误串。
#[derive(Debug)]
pub(crate) struct NonUtf8Document;

impl std::fmt::Display for NonUtf8Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("file is not valid UTF-8")
    }
}

impl std::error::Error for NonUtf8Document {}

/// 读取 Markdown 文档文件：剥掉开头 BOM，非 UTF-8 内容返回 [`NonUtf8Document`]。
///
/// 直接 `read_to_string` 会把 BOM 留在正文首块里显示成一个不可见字符；
/// 而非 UTF-8 文件只能拿到 `stream did not contain valid UTF-8` 这类系统串，
/// 用户看不出是编码问题，故此处用类型化错误让展示方能换成可读文案。
pub(crate) fn read_markdown_file(path: &Path) -> anyhow::Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    let bytes = bytes.strip_prefix(UTF8_BOM).unwrap_or(&bytes);
    String::from_utf8(bytes.to_vec()).map_err(|_| anyhow::anyhow!(NonUtf8Document))
}

/// 读取失败的用户可读文案：非 UTF-8 换成编码提示，其余沿用原始错误信息。
pub(crate) fn read_error_detail(err: &anyhow::Error, path: &Path, i18n: &I18nSnapshot) -> String {
    if err.downcast_ref::<NonUtf8Document>().is_some() {
        return i18n.t(
            "message_file_not_utf8",
            &[("path", &path.display().to_string())],
        );
    }
    err.to_string()
}

impl Editor {
    /// 按当前视图模式序列化文档文本。
    pub(super) fn serialized_document_text(&self, cx: &App) -> String {
        let derived = self.derived_document(cx);
        if self.view_mode == super::ViewMode::Source {
            derived.raw_source.to_string()
        } else {
            derived.markdown.to_string()
        }
    }

    /// 计算另存对话框的默认目录与建议文件名。
    pub(super) fn save_dialog_defaults(&self) -> (PathBuf, Option<String>) {
        if let Some(path) = self.file_path.as_ref() {
            let directory = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let suggested_name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string());
            (directory, suggested_name)
        } else {
            (
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                Some("untitled.md".to_string()),
            )
        }
    }

    /// 应用保存成功后的状态（路径、脏标记、标题刷新）。
    pub(super) fn apply_successful_save(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.file_path = Some(path);
        self.document_dirty = false;
        self.pending_window_edited = false;
        self.pending_window_title_refresh = true;
        self.pending_close_after_save = false;
        self.close_dialog_restore_focus = None;
        self.sync_workspace_after_document_path_change(cx);
        cx.notify();
    }

    /// 保存到已有路径：成功返回真，失败弹窗并返回假。
    pub(super) fn save_to_existing_path(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let markdown = self.serialized_document_text(cx);
        match crate::atomic_file::write_text(path, &markdown) {
            Ok(_) => {
                self.apply_successful_save(path.to_path_buf(), cx);
                window.set_window_edited(false);
                true
            }
            Err(err) => {
                let detail = err.to_string();
                let i18n = cx.global::<I18nManager>().snapshot();
                let ok = i18n.t("dialog_ok", &[]);
                let title = i18n.t("message_save_failed", &[]);
                let buttons = [ok.as_str()];
                // 对话框 future 必须轮询才会弹出：转交后台任务等待用户响应。
                let prompt =
                    window.prompt(PromptLevel::Critical, &title, Some(&detail), &buttons, cx);
                cx.spawn(async move |_, _| {
                    let _ = prompt.await;
                })
                .detach();
                false
            }
        }
    }

    /// 弹框让用户选择路径后异步保存文档。
    ///
    /// 不在弹框前序列化：对话框期间文档可能继续被编辑，写盘前重新取当前
    /// 文本才能避免「保存旧内容却清掉脏标记」的静默丢数据。
    fn save_document_via_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (default_dir, suggested_name) = self.save_dialog_defaults();
        let prompt = cx.prompt_for_new_path(&default_dir, suggested_name.as_deref());
        let weak_editor = cx.entity().downgrade();
        let weak_editor_for_cancel = weak_editor.clone();
        let weak_editor_for_error = weak_editor.clone();
        let weak_editor_for_write_error = weak_editor.clone();
        let window_handle = window.window_handle();
        let should_close_after_save = self.pending_close_after_save;

        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => {
                    if should_close_after_save {
                        let _ = weak_editor_for_cancel
                            .update(cx, |this, cx| this.discard_pending_close_after_save(cx));
                    }
                    return;
                }
                Ok(Err(err)) => {
                    if should_close_after_save {
                        let _ = weak_editor_for_error
                            .update(cx, |this, cx| this.discard_pending_close_after_save(cx));
                    }
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

            if path.extension().is_none() {
                path.set_extension("md");
            }

            // 对话框结束后取「此刻」的文档，而不是弹框前的快照。
            let Ok(markdown) = weak_editor.update(cx, |this, cx| this.serialized_document_text(cx))
            else {
                return;
            };

            if let Err(err) = crate::atomic_file::write_text(&path, &markdown) {
                if should_close_after_save {
                    let _ = weak_editor_for_write_error
                        .update(cx, |this, cx| this.discard_pending_close_after_save(cx));
                }
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

            let path_for_state = path.clone();
            let written = markdown.clone();
            let _ = weak_editor.update(cx, move |this, cx| {
                this.apply_successful_save(path_for_state, cx);
                // 写盘期间若又发生编辑，保留脏标记，让下一次保存覆盖新内容。
                if this.serialized_document_text(cx) != written {
                    this.document_dirty = true;
                    this.pending_window_edited = true;
                }
            });
            let _ = cx.update_window(
                window_handle,
                move |_view: AnyView, window: &mut Window, _cx: &mut App| {
                    window.set_window_edited(false);
                    if should_close_after_save {
                        window.remove_window();
                    }
                },
            );
        })
        .detach();
    }

    /// 保存文档：有路径直接写，无路径走另存弹框。
    pub(crate) fn save_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_loading {
            return;
        }
        if let Some(path) = self.file_path.clone() {
            let should_close_after_save = self.pending_close_after_save;
            if self.save_to_existing_path(&path, window, cx) {
                if should_close_after_save {
                    window.remove_window();
                }
            } else if should_close_after_save {
                self.discard_pending_close_after_save(cx);
            }
            return;
        }

        self.save_document_via_prompt(window, cx);
    }

    /// 另存文档：始终弹框选择路径。
    pub(crate) fn save_document_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_loading {
            return;
        }
        self.save_document_via_prompt(window, cx);
    }

    /// 导出当前文档为单文件 HTML（含内嵌图片直显，无需网络）。
    ///
    /// 经 `pulldown-cmark`（GFM 全开）转正文，套极简样式外壳后经另存弹框落盘，
    /// 不引入浏览器/PDF 重型依赖。
    pub(crate) fn export_document_as_html(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let markdown = self.serialized_document_text(cx);
        let title = self
            .file_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or("untitled")
            .to_string();
        let toc_title = cx
            .global::<I18nManager>()
            .snapshot()
            .t("export_toc_title", &[]);
        let (include_toc, dark) = export_html_options(cx);
        let base_dir = self.image_paste_root_dir().ok();
        let html = markdown_to_standalone_html(
            &markdown,
            &title,
            dark,
            include_toc,
            &toc_title,
            base_dir.as_deref(),
        );
        let (default_dir, _) = self.save_dialog_defaults();
        let suggested_name = format!("{title}.html");
        let prompt = cx.prompt_for_new_path(&default_dir, Some(&suggested_name));
        let window_handle = window.window_handle();
        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(err)) => {
                    show_export_error(window_handle, err.to_string(), cx).await;
                    return;
                }
            };
            if path.extension().is_none() {
                path.set_extension("html");
            }
            if let Err(err) = crate::atomic_file::write_text(&path, &html) {
                show_export_error(window_handle, err.to_string(), cx).await;
            }
        })
        .detach();
    }

    /// 复制当前文档的单文件 HTML 到剪贴板（与文件导出同模板，免落盘快发博客/邮件）。
    pub(crate) fn copy_html_to_clipboard(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let markdown = self.serialized_document_text(cx);
        let title = self
            .file_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or("untitled")
            .to_string();
        let toc_title = cx
            .global::<I18nManager>()
            .snapshot()
            .t("export_toc_title", &[]);
        let (include_toc, dark) = export_html_options(cx);
        let base_dir = self.image_paste_root_dir().ok();
        let html = markdown_to_standalone_html(
            &markdown,
            &title,
            dark,
            include_toc,
            &toc_title,
            base_dir.as_deref(),
        );
        cx.write_to_clipboard(ClipboardItem::new_string(html));
    }

    /// 复制当前文档的 Markdown 全文到剪贴板（渲染/源码模式各自按序列化文本复制）。
    pub(crate) fn copy_markdown_to_clipboard(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let markdown = self.serialized_document_text(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(markdown));
    }

    /// 按格式导出文档（PDF/DOCX）：另存弹框选定路径后，排版渲染在后台线程完成再落盘。
    ///
    /// 渲染失败（如中文 PDF 缺中文字体）弹窗说明；渲染不再占用 UI 线程，
    /// 长文档导出期间窗口仍可交互。
    #[cfg(any(feature = "export-pdf", feature = "export-docx"))]
    pub(crate) fn export_document_with_format(
        &mut self,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let markdown = self.serialized_document_text(cx);
        let title = self
            .file_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or("untitled")
            .to_string();
        let (include_toc, _) = export_html_options(cx);
        let options = crate::export::ExportOptions {
            title: title.clone(),
            include_toc,
            base_dir: self.image_paste_root_dir().ok(),
        };
        // 缺字体的提示文案要在主线程取好：后台任务只产出字节或错误，不碰 i18n 全局。
        let cjk_font_hint = cx
            .global::<I18nManager>()
            .snapshot()
            .t("message_export_missing_cjk_font", &[]);
        let (default_dir, _) = self.save_dialog_defaults();
        let suggested_name = format!("{title}.{}", format.extension());
        let prompt = cx.prompt_for_new_path(&default_dir, Some(&suggested_name));
        let window_handle = window.window_handle();
        let extension = format.extension();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(err)) => {
                    show_export_error(window_handle, err.to_string(), cx).await;
                    return;
                }
            };
            if path.extension().is_none() {
                path.set_extension(extension);
            }
            // 渲染丢给后台执行器：这段排版在 UI 线程上会让窗口卡住。
            let rendered = executor
                .spawn(async move { render_export_bytes(format, &markdown, &options) })
                .await;
            let bytes = match rendered {
                Ok(bytes) => bytes,
                Err(err) => {
                    let detail = export_error_detail(format, &err, &cjk_font_hint);
                    show_export_error(window_handle, detail, cx).await;
                    return;
                }
            };
            if let Err(err) = crate::atomic_file::write_bytes(&path, &bytes) {
                show_export_error(window_handle, err.to_string(), cx).await;
            }
        })
        .detach();
    }
}

/// 后台渲染错误的用户可读文案：仅 PDF 会因缺中文字体失败，
/// 提示文案由主线程预先取好后传入（后台线程不碰 i18n 全局）。
#[cfg(feature = "export-pdf")]
fn export_error_detail(format: ExportFormat, err: &anyhow::Error, cjk_font_hint: &str) -> String {
    if matches!(format, ExportFormat::Pdf) && crate::export::pdf::is_missing_cjk_font(err) {
        return cjk_font_hint.to_string();
    }
    err.to_string()
}

/// 无 PDF 功能时只有普通错误文案。
#[cfg(all(not(feature = "export-pdf"), feature = "export-docx"))]
fn export_error_detail(_format: ExportFormat, err: &anyhow::Error, _cjk_font_hint: &str) -> String {
    err.to_string()
}

/// 在后台线程渲染导出字节（纯函数，不触碰任何 UI 上下文）。
#[cfg(any(feature = "export-pdf", feature = "export-docx"))]
fn render_export_bytes(
    format: ExportFormat,
    markdown: &str,
    options: &crate::export::ExportOptions,
) -> anyhow::Result<Vec<u8>> {
    match format {
        #[cfg(feature = "export-pdf")]
        ExportFormat::Pdf => crate::export::pdf::render_pdf_bytes(markdown, options),
        #[cfg(feature = "export-docx")]
        ExportFormat::Docx => crate::export::docx::render_docx_bytes(markdown, options),
    }
}

/// 导出目录条目：源码行号、标题级别、展示文本与锚点。
struct ExportHeading {
    line: usize,
    level: u8,
    label: String,
    slug: String,
}

/// 标题转锚点：保留字母数字（含中文）与 `-`/`_`，空白转 `-`，其余丢弃；空时回退 `section`。
fn slugify_heading(label: &str) -> String {
    let mut slug = String::new();
    for ch in label.trim().chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            for lower in ch.to_lowercase() {
                slug.push(lower);
            }
        } else if ch.is_whitespace() {
            slug.push('-');
        }
    }
    // 合并连续 `-` 并去首尾。
    let mut compact = String::with_capacity(slug.len());
    let mut last_dash = true;
    for ch in slug.chars() {
        if ch == '-' {
            if !last_dash {
                compact.push(ch);
            }
            last_dash = true;
        } else {
            compact.push(ch);
            last_dash = false;
        }
    }
    let compact = compact.trim_matches('-').to_string();
    if compact.is_empty() {
        "section".to_string()
    } else {
        compact
    }
}

/// 去掉行内 Markdown 标记，取目录展示文本（代码/强调/链接取其文本）。
fn strip_inline_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '`' => {}
            '\\' => {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '!' if chars.peek() == Some(&'[') => {
                chars.next();
                // 图片整体丢弃（替代文本不进目录）。
                let _ = take_bracketed(&mut chars);
                skip_parenthesized(&mut chars);
            }
            '[' => {
                let label = take_bracketed(&mut chars);
                // 链接 `[文本](目标)` 只保留文本。
                skip_parenthesized(&mut chars);
                out.push_str(&label);
            }
            '*' | '_' | '~' => {}
            _ => out.push(ch),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 跳过紧随其后的 `(...)` 目标（调用方保证尚未消费 `(`）。
fn skip_parenthesized(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    if chars.peek() != Some(&'(') {
        return;
    }
    chars.next();
    let mut depth = 1;
    for next in chars.by_ref() {
        if next == '(' {
            depth += 1;
        } else if next == ')' {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
    }
}

/// 取方括号内文本（调用方保证已消费 `[`）。
fn take_bracketed(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut label = String::new();
    let mut depth = 1;
    for ch in chars.by_ref() {
        if ch == '[' {
            depth += 1;
            label.push(ch);
        } else if ch == ']' {
            depth -= 1;
            if depth == 0 {
                break;
            }
            label.push(ch);
        } else {
            label.push(ch);
        }
    }
    label
}

/// 收集 Markdown 中的 ATX 标题（跳过围栏代码块），锚点按序去重。
fn collect_export_headings(markdown: &str) -> Vec<ExportHeading> {
    use std::collections::HashMap;

    let mut headings = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut used: HashMap<String, usize> = HashMap::new();
    for (line_idx, line) in markdown.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some((marker, len)) = fence {
            let run = trimmed.chars().take_while(|&c| c == marker).count();
            if run >= len && trimmed[marker.len_utf8() * run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if let Some(opening) = crate::components::BlockKind::parse_fence_opening(trimmed.trim_end())
        {
            fence = Some((opening.ch, opening.len));
            continue;
        }
        let Some((level, title)) = crate::components::BlockKind::parse_heading_line(line) else {
            continue;
        };
        let label = strip_inline_markdown(&title);
        let base = slugify_heading(&label);
        let count = used.entry(base.clone()).or_insert(0);
        let slug = if *count == 0 {
            base
        } else {
            format!("{base}-{}", *count)
        };
        *count += 1;
        headings.push(ExportHeading {
            line: line_idx,
            level,
            label,
            slug,
        });
    }
    headings
}

/// 给标题行注入 `{#锚点}`（已有显式属性的行跳过），返回带锚点的新源码。
fn inject_heading_ids(markdown: &str, headings: &[ExportHeading]) -> String {
    if headings.is_empty() {
        return markdown.to_string();
    }
    let by_line: std::collections::HashMap<usize, &ExportHeading> = headings
        .iter()
        .map(|heading| (heading.line, heading))
        .collect();
    markdown
        .lines()
        .enumerate()
        .map(|(idx, line)| {
            let Some(heading) = by_line.get(&idx) else {
                return line.to_string();
            };
            if line.contains("{#") {
                return line.to_string();
            }
            format!("{} {{#{}}}", line.trim_end(), heading.slug)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 渲染嵌套目录导航（无标题时返回空）。
fn render_export_toc(headings: &[ExportHeading], toc_title: &str) -> String {
    if headings.is_empty() {
        return String::new();
    }
    let mut toc = String::from("<nav class=\"toc\">\n<strong>");
    toc.push_str(&html_escape(toc_title));
    toc.push_str("</strong>\n");
    let mut depth = 0;
    for heading in headings {
        let level = heading.level.clamp(1, 6) as usize;
        while depth < level {
            toc.push_str("<ul>\n");
            depth += 1;
        }
        while depth > level {
            toc.push_str("</ul>\n");
            depth -= 1;
        }
        toc.push_str("<li><a href=\"#");
        toc.push_str(&html_escape(&heading.slug));
        toc.push_str("\">");
        toc.push_str(&html_escape(&heading.label));
        toc.push_str("</a></li>\n");
    }
    while depth > 0 {
        toc.push_str("</ul>\n");
        depth -= 1;
    }
    toc.push_str("</nav>\n");
    toc
}

/// 读取 HTML 导出选项（目录开关 + 配色），偏好缺失时回退默认值。
fn export_html_options(cx: &App) -> (bool, bool) {
    use crate::config::ExportThemeMode;

    let (include_toc, theme) = crate::config::read_app_preferences()
        .map(|preferences| (preferences.export_include_toc, preferences.export_theme))
        .unwrap_or((true, ExportThemeMode::FollowEditor));
    let dark = match theme {
        ExportThemeMode::Light => false,
        ExportThemeMode::Dark => true,
        ExportThemeMode::FollowEditor => {
            crate::theme::theme_mode_for_id(&crate::theme::active_theme_id(cx))
                == rgpui::ThemeMode::Dark
        }
    };
    (include_toc, dark)
}

/// 把 Markdown 转为单文件 HTML（GFM 表格/删除线/任务列表/脚注全开，明暗跟随编辑器主题）。
///
/// 有标题且 `include_toc` 为真时在正文前插入嵌套目录（标题行注入 `{#锚点}` 供跳转）。
/// 本地图片按 `base_dir` 解析后内嵌为 `data:` URI，导出文件脱离文档目录也能直显；
/// 远程图片保留原地址（单文件不承诺离线显示网络图）。
fn markdown_to_standalone_html(
    markdown: &str,
    title: &str,
    dark: bool,
    include_toc: bool,
    toc_title: &str,
    base_dir: Option<&Path>,
) -> String {
    use pulldown_cmark::{Options, Parser, html};
    let headings = collect_export_headings(markdown);
    // 目录关闭时不注入锚点，保持正文干净。
    let anchored = if include_toc {
        inject_heading_ids(markdown, &headings)
    } else {
        markdown.to_string()
    };
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    let parser = Parser::new_ext(&anchored, options);
    let mut body = String::new();
    html::push_html(
        &mut body,
        sanitize_export_events(embed_local_image_events(parser, base_dir)),
    );
    let toc = if include_toc {
        render_export_toc(&headings, toc_title)
    } else {
        String::new()
    };
    let escaped_title = html_escape(title);
    // 明暗两套极简样式：浅色沿用 GitHub 浅灰，暗色用编辑器暗底 + 浅字，对比度优先可读。
    let (page_bg, text, pre_bg, border, quote_fg, link) = if dark {
        (
            "#0d1117", "#e6edf3", "#161b22", "#30363d", "#8b949e", "#79c0ff",
        )
    } else {
        (
            "#ffffff", "#24292f", "#f6f8fa", "#d0d7de", "#57606a", "#0969da",
        )
    };
    format!(
        "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n\
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
        <meta name=\"color-scheme\" content=\"{}\">\n\
        <title>{escaped_title}</title>\n<style>\n\
        body{{max-width:860px;margin:2rem auto;padding:0 1.25rem;line-height:1.7;background:{page_bg};\
        font-family:system-ui,-apple-system,\"Segoe UI\",Roboto,\"Noto Sans\",\
        \"PingFang SC\",\"Microsoft YaHei\",sans-serif;color:{text};}}\n\
        a{{color:{link};}}\n\
        pre{{background:{pre_bg};padding:1rem;overflow:auto;border-radius:6px;}}\n\
        code{{font-family:ui-monospace,Consolas,monospace;}}\n\
        table{{border-collapse:collapse;width:100%;}}\n\
        th,td{{border:1px solid {border};padding:6px 12px;text-align:left;}}\n\
        img{{max-width:100%;}}\nblockquote{{border-left:4px solid {border};\
        margin:0;padding:0 1rem;color:{quote_fg};}}\n.toc{{background:{pre_bg};\
        border:1px solid {border};border-radius:6px;padding:.5rem 1rem;margin-bottom:1.5rem;}}\n\
        .toc ul{{margin:.25rem 0;padding-left:1.2em;}}\n.toc a{{text-decoration:none;}}\n\
        </style>\n</head>\n\
        <body>\n{toc}{body}\n</body>\n</html>\n",
        if dark { "dark" } else { "light" },
    )
}

/// 把事件流中的本地图片目标替换为内嵌 `data:` URI（读取失败保留原地址）。
fn embed_local_image_events<'a>(
    events: impl Iterator<Item = pulldown_cmark::Event<'a>>,
    base_dir: Option<&Path>,
) -> impl Iterator<Item = pulldown_cmark::Event<'a>> {
    events.map(move |event| match event {
        pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            let embedded =
                crate::export::image::load_local_image_bytes(&dest_url, base_dir).map(|bytes| {
                    let path = crate::export::image::resolve_local_path(&dest_url, base_dir);
                    let mime = Editor::embedded_mime_for_extension(&path);
                    Editor::embedded_data_uri(mime, &bytes)
                });
            let dest_url = match embedded {
                Some(uri) => pulldown_cmark::CowStr::from(uri),
                None => dest_url,
            };
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            })
        }
        other => other,
    })
}

/// 把 Markdown 事件流里的原始 HTML 片段替换为清洗后的安全标记。
///
/// `pulldown-cmark` 默认把原始 HTML 原样写出，导出的 HTML 与剪贴板内容
/// 因此会带上可执行脚本。清洗按标签逐个进行，块级与行内片段同样适用。
fn sanitize_export_events<'a>(
    events: impl Iterator<Item = pulldown_cmark::Event<'a>>,
) -> impl Iterator<Item = pulldown_cmark::Event<'a>> {
    events.map(|event| match event {
        pulldown_cmark::Event::Html(raw) | pulldown_cmark::Event::InlineHtml(raw) => {
            pulldown_cmark::Event::Html(pulldown_cmark::CowStr::from(
                crate::components::markdown::html::sanitize_html_fragment(&raw),
            ))
        }
        other => other,
    })
}

/// 转义 HTML 标题中的特殊字符。
fn html_escape(text: &str) -> String {
    crate::components::markdown::html::escape_html_text(text)
}

/// 在导出窗口上显示失败提示。
async fn show_export_error(window_handle: AnyWindowHandle, detail: String, cx: &mut AsyncApp) {
    let _ = cx.update_window(
        window_handle,
        move |_view: AnyView, window: &mut Window, cx: &mut App| {
            let i18n = cx.global::<I18nManager>().snapshot();
            let ok = i18n.t("dialog_ok", &[]);
            let title = i18n.t("message_export_failed", &[]);
            let buttons = [ok.as_str()];
            let prompt = window.prompt(PromptLevel::Critical, &title, Some(&detail), &buttons, cx);
            cx.spawn(async move |_| {
                let _ = prompt.await;
            })
            .detach();
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{
        NonUtf8Document, code_fence_markdown, collect_export_headings, html_escape,
        markdown_to_standalone_html, read_error_detail, read_markdown_file, safe_code_fence,
        safe_code_fence_with_info, slugify_heading, strip_inline_markdown,
    };

    #[test]
    fn safe_code_fence_is_longer_than_any_inner_backtick_run() {
        assert_eq!(safe_code_fence("plain code"), "```");
        assert_eq!(safe_code_fence("```\ncode"), "~~~");
        assert_eq!(safe_code_fence("value = `````"), "~~~");
        assert_eq!(safe_code_fence("```\n~~~"), "~~~~");
    }

    #[test]
    fn safe_code_fence_with_info_uses_tildes_when_info_contains_backticks() {
        assert_eq!(
            safe_code_fence_with_info("plain code", Some("we`rd")),
            "~~~"
        );
        assert_eq!(
            safe_code_fence_with_info("plain\n~~~\ncode", Some("we`rd")),
            "~~~~"
        );
        assert_eq!(safe_code_fence_with_info("plain code", Some("rust")), "```");
    }

    #[test]
    fn code_fence_markdown_reuses_variant_when_safe() {
        // 无冲突：原样保留原文变体。
        assert_eq!(code_fence_markdown("a\nb", None, Some(('~', 3))), "~~~");
        assert_eq!(code_fence_markdown("a\nb", None, Some(('`', 4))), "````");
        assert_eq!(
            code_fence_markdown("a\nb", Some("rust"), Some(('~', 3))),
            "~~~"
        );
        // 原文波浪线而内容含 ``` 行：重算切到波浪线，字符匹配且长度足够，
        // 原样保留（内容中波浪线串长度 0 < 3，闭合安全）。
        assert_eq!(
            code_fence_markdown("```\nx\n```", None, Some(('~', 3))),
            "~~~"
        );
        // 原文反引号而内容含 ``` 行：重算换字符，回退到重算结果。
        assert_eq!(
            code_fence_markdown("```\nx\n```", None, Some(('`', 3))),
            "~~~"
        );
        // 原文波浪线而内容含波浪线围栏行：重算换回反引号（可安全闭合），回退。
        assert_eq!(
            code_fence_markdown("plain\n~~~~\ncode", None, Some(('~', 3))),
            "```"
        );
        // 无变体（交互创建）：按内容重算。
        assert_eq!(code_fence_markdown("plain code", None, None), "```");
    }

    #[test]
    fn standalone_html_keeps_gfm_table_and_escapes_title() {
        let html = markdown_to_standalone_html(
            "| a |\n| --- |\n| 1 |",
            "A&B <T>",
            false,
            true,
            "目录",
            None,
        );
        assert!(html.contains("<table>"), "html: {html:?}");
        assert!(
            html.contains("<title>A&amp;B &lt;T&gt;</title>"),
            "html: {html:?}"
        );
        assert!(html.contains("meta charset"), "html: {html:?}");
        assert!(html.contains("color-scheme"), "html: {html:?}");
        // 无标题时不生成目录。
        assert!(!html.contains("<nav class=\"toc\">"), "html: {html:?}");
    }

    #[test]
    fn standalone_html_dark_mode_uses_dark_palette() {
        let light = markdown_to_standalone_html("# T", "T", false, true, "目录", None);
        let dark = markdown_to_standalone_html("# T", "T", true, true, "目录", None);
        assert!(light.contains("#ffffff"), "html: {light:?}");
        assert!(dark.contains("#0d1117"), "html: {dark:?}");
        assert!(
            dark.contains("color-scheme\" content=\"dark\""),
            "html: {dark:?}"
        );
    }

    #[test]
    fn standalone_html_toc_can_be_disabled() {
        let html = markdown_to_standalone_html("# A\n\ntext", "T", false, false, "目录", None);
        assert!(!html.contains("<nav class=\"toc\">"), "html: {html:?}");
        assert!(!html.contains("{#a}"), "html: {html:?}");
        assert!(html.contains("<h1>A</h1>"), "html: {html:?}");
    }

    #[test]
    fn export_toc_collects_headings_skips_fences_and_dedupes_slugs() {
        let markdown = "# 快速开始\n\n```md\n# 忽略\n```\n\n## 快速开始\n\n### 安装指南\n";
        let headings = collect_export_headings(markdown);
        assert_eq!(headings.len(), 3);
        assert_eq!(headings[0].slug, "快速开始");
        assert_eq!(headings[1].slug, "快速开始-1");
        assert_eq!(headings[2].slug, "安装指南");
        assert_eq!(headings[2].level, 3);
    }

    #[test]
    fn export_toc_renders_nested_nav_with_anchors() {
        let markdown = "# A\n\n## B\n\n# A\n";
        let html = markdown_to_standalone_html(markdown, "T", false, true, "Contents", None);
        assert!(html.contains("<nav class=\"toc\">"), "html: {html:?}");
        assert!(html.contains("<strong>Contents</strong>"), "html: {html:?}");
        assert!(html.contains("href=\"#a\""), "html: {html:?}");
        assert!(html.contains("href=\"#a-1\""), "html: {html:?}");
        assert!(html.contains("id=\"a\""), "html: {html:?}");
    }

    #[test]
    fn slugify_and_strip_cover_cjk_and_links() {
        assert_eq!(slugify_heading("快速 开始！"), "快速-开始");
        assert_eq!(slugify_heading("  "), "section");
        assert_eq!(
            strip_inline_markdown("**加粗**与[链接](https://x.com)"),
            "加粗与链接"
        );
        assert_eq!(strip_inline_markdown("`代码` ![图](./x.png)"), "代码");
    }

    #[test]
    fn html_escape_covers_special_characters() {
        assert_eq!(html_escape("A&B<>\"'"), "A&amp;B&lt;&gt;&quot;&#39;");
    }

    #[test]
    fn standalone_html_escapes_script_blocks() {
        let html = markdown_to_standalone_html(
            "<script>alert(1)</script>\n\n正文",
            "T",
            false,
            false,
            "目录",
            None,
        );
        assert!(!html.contains("<script>"), "html: {html:?}");
        assert!(html.contains("&lt;script&gt;"), "html: {html:?}");
        assert!(html.contains("正文"), "html: {html:?}");
    }

    #[test]
    fn standalone_html_drops_event_handlers_and_javascript_urls() {
        let html = markdown_to_standalone_html(
            "<img src=\"a.png\" onerror=\"alert(1)\">\n\n<a href=\"javascript:alert(1)\">x</a>",
            "T",
            false,
            false,
            "目录",
            None,
        );
        assert!(!html.contains("onerror"), "html: {html:?}");
        assert!(!html.contains("javascript:"), "html: {html:?}");
        assert!(html.contains("<img src=\"a.png\">"), "html: {html:?}");
    }

    #[test]
    fn standalone_html_keeps_safe_html_blocks() {
        let html = markdown_to_standalone_html(
            "<details open>\n<summary>标题</summary>\n正文\n</details>",
            "T",
            false,
            false,
            "目录",
            None,
        );
        assert!(html.contains("<details open=\"\">"), "html: {html:?}");
        assert!(html.contains("<summary>标题</summary>"), "html: {html:?}");
        assert!(html.contains("</details>"), "html: {html:?}");
    }

    #[test]
    fn standalone_html_embeds_local_images_as_data_uri() {
        let root = std::env::temp_dir().join(format!("rumark-html-img-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("assets")).expect("temp dir");
        std::fs::write(root.join("assets/pic.png"), b"fake-png-bytes").expect("write image");

        let markdown = "![图](./assets/pic.png)\n\n![远程](https://example.com/a.png)\n\n![缺图](./missing.png)";
        let html = markdown_to_standalone_html(markdown, "T", false, false, "目录", Some(&root));
        assert!(html.contains("data:image/png;base64,"), "html: {html:?}");
        assert!(!html.contains("./assets/pic.png"), "html: {html:?}");
        // 远程地址保留原样，缺失文件不内嵌也不报错。
        assert!(html.contains("https://example.com/a.png"), "html: {html:?}");
        assert!(html.contains("./missing.png"), "html: {html:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_markdown_file_strips_leading_bom_and_keeps_inner_one() {
        let root = std::env::temp_dir().join(format!("rumark-read-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("temp dir");

        let bom_path = root.join("bom.md");
        std::fs::write(
            &bom_path,
            [&[0xEF, 0xBB, 0xBF][..], "# 标题".as_bytes()].concat(),
        )
        .expect("write bom file");
        assert_eq!(read_markdown_file(&bom_path).unwrap(), "# 标题");

        // 正文中间的 BOM 不是文件开头标记，原样保留。
        let inner_path = root.join("inner.md");
        std::fs::write(&inner_path, "a\u{FEFF}b").expect("write inner file");
        assert_eq!(read_markdown_file(&inner_path).unwrap(), "a\u{FEFF}b");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_error_detail_separates_encoding_from_io_failures() {
        let root = std::env::temp_dir().join(format!("rumark-read-err-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("temp dir");

        let gbk_path = root.join("gbk.md");
        std::fs::write(&gbk_path, [0xC4, 0xE3, 0xBA, 0xC3]).expect("write gbk file");
        let encoding_err = read_markdown_file(&gbk_path).expect_err("non UTF-8 should fail");
        assert!(encoding_err.downcast_ref::<NonUtf8Document>().is_some());

        let missing_path = root.join("missing.md");
        let io_err = read_markdown_file(&missing_path).expect_err("missing file should fail");
        assert!(io_err.downcast_ref::<NonUtf8Document>().is_none());

        let zh = crate::i18n::I18nManager::new_with_language_id("zh-CN").snapshot();
        let detail = read_error_detail(&encoding_err, &gbk_path, &zh);
        assert!(detail.contains("UTF-8"), "zh encoding detail: {detail}");
        assert!(detail.contains(&gbk_path.display().to_string()));
        // 非编码类错误沿用原始信息，不套编码文案。
        let detail = read_error_detail(&io_err, &missing_path, &zh);
        assert!(detail.contains("failed to read"), "zh io detail: {detail}");
        assert!(!detail.contains("UTF-8"), "zh io detail: {detail}");

        let en = crate::i18n::I18nManager::new_with_language_id("en-US").snapshot();
        let detail = read_error_detail(&encoding_err, &gbk_path, &en);
        assert!(detail.contains("UTF-8"), "en encoding detail: {detail}");

        let _ = std::fs::remove_dir_all(root);
    }
}
