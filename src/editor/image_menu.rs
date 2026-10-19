//! 图片右键菜单：定位、复制、缩放与 base64 内嵌/还原等图片专属操作。
//!
//! 仅在右键目标块渲染为独立图片（Markdown 图片或孤立 `<img>` HTML 块）时挂载，
//! 条目追加在通用块菜单最前面。图片本身仍是文档文本的一部分，缩放与内嵌/还原
//! 都通过改写该块文本实现，因此严格保持 Markdown 源码的可编辑与可往返。

use std::path::Path;

use anyhow::Context as _;
use rgpui::menu::{PopupMenu, PopupMenuItem};
use rgpui::*;

use super::Editor;
use crate::components::{
    Block, BlockKind, ImageResolvedSource, InlineTextTree, PastedImageSource,
    image_format_for_mime, parse_html_image_block, resolve_image_source,
};
use crate::i18n::I18nManager;

/// 缩放图片子菜单的可选档位（含 100% 原图）。
const ZOOM_OPTIONS: [f32; 8] = [0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.8, 1.0, 1.5, 2.0];

/// 「复制为」子菜单的目标格式。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CopyAsFormat {
    Markdown,
    Html,
    Url,
}

/// 右键图片块时收集的只读快照。
#[derive(Clone)]
pub(super) struct ImageMenuData {
    /// 被右键的图片块（改写文本的作用目标）。
    pub(super) block: Entity<Block>,
    /// 图片的原始源码文本（Markdown 图片串或 HTML 串）。
    pub(super) source_markdown: String,
    pub(super) alt: String,
    pub(super) title: Option<String>,
    /// 原始 `src`（未加 Markdown 转义）。
    pub(super) src: String,
    pub(super) resolved: ImageResolvedSource,
    /// 当前显示缩放（1.0 为原图）。
    pub(super) zoom: f32,
    /// 是否来自 HTML `<img>` 块（否则为 Markdown 图片）。
    pub(super) from_html: bool,
}

impl ImageMenuData {
    /// 是否为本地文件图片（可定位、可导出、可内嵌）。
    fn is_local(&self) -> bool {
        matches!(self.resolved, ImageResolvedSource::Local(_))
    }

    /// 是否为内嵌 `data:` URI 图片（可导出为本地文件）。
    fn is_embedded(&self) -> bool {
        matches!(self.resolved, ImageResolvedSource::Embedded { .. })
    }

    /// 是否为远端图片（无法直接读字节与定位）。
    fn is_remote(&self) -> bool {
        matches!(self.resolved, ImageResolvedSource::Remote(_))
    }

    /// 本地文件路径（仅本地图片有）。
    fn local_path(&self) -> Option<&Path> {
        match &self.resolved {
            ImageResolvedSource::Local(path) => Some(path),
            _ => None,
        }
    }

    /// 读取图片字节与格式（本地文件或内嵌 `data:` URI 图片）。
    fn load_bytes(&self) -> anyhow::Result<(ImageFormat, Vec<u8>)> {
        match &self.resolved {
            ImageResolvedSource::Local(path) => {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("failed to read '{}'", path.display()))?;
                let format = image_format_for_mime(Editor::embedded_mime_for_extension(path))
                    .unwrap_or(ImageFormat::Png);
                Ok((format, bytes))
            }
            ImageResolvedSource::Embedded { format, bytes } => Ok((*format, bytes.clone())),
            ImageResolvedSource::Remote(_) => {
                anyhow::bail!("远端图片需先下载，暂不读取本地字节")
            }
        }
    }

    /// 以 Markdown 图片语法生成文本（`target` 需为已转义的目标串）。
    fn markdown_with_target(&self, target: &str) -> String {
        match &self.title {
            Some(title) => format!(
                "![{}]({} \"{}\")",
                escape_markdown_alt(&self.alt),
                target,
                escape_markdown_title(title)
            ),
            None => format!("![{}]({})", escape_markdown_alt(&self.alt), target),
        }
    }

    /// 还原为规范 Markdown 图片文本。
    fn markdown_form(&self) -> String {
        self.markdown_with_target(&Editor::markdown_image_target(&self.src))
    }

    /// 生成带缩放的 HTML `<img>` 文本（Typora 式 `style="zoom:.."`）。
    fn html_form(&self, zoom: f32) -> String {
        let mut html = format!("<img src=\"{}\"", escape_html_attr(&self.src));
        if !self.alt.is_empty() {
            html.push_str(&format!(" alt=\"{}\"", escape_html_attr(&self.alt)));
        }
        if let Some(title) = &self.title {
            html.push_str(&format!(" title=\"{}\"", escape_html_attr(title)));
        }
        let percent = (zoom * 100.0).round() as i64;
        html.push_str(&format!(" style=\"zoom:{percent}%;\" />"));
        html
    }

    /// 复制为目标格式的文本。
    fn copy_text(&self, format: CopyAsFormat) -> String {
        match format {
            CopyAsFormat::Markdown => {
                if self.from_html {
                    self.markdown_form()
                } else {
                    self.source_markdown.clone()
                }
            }
            CopyAsFormat::Html => {
                if self.from_html {
                    self.source_markdown.clone()
                } else {
                    self.html_form(self.zoom)
                }
            }
            CopyAsFormat::Url => self.src.clone(),
        }
    }
}

/// 转义 Markdown 图片替代文本中的 `\` 与 `]`。
fn escape_markdown_alt(alt: &str) -> String {
    alt.chars()
        .flat_map(|ch| match ch {
            '\\' | ']' => ['\\', ch].into_iter().collect::<Vec<_>>(),
            _ => [ch].into_iter().collect(),
        })
        .collect()
}

/// 转义 Markdown 标题中的引号与控制字符。
fn escape_markdown_title(title: &str) -> String {
    title
        .chars()
        .flat_map(|ch| match ch {
            '"' | '\\' => ['\\', ch].into_iter().collect::<Vec<_>>(),
            _ => [ch].into_iter().collect(),
        })
        .collect()
}

/// 转义 HTML 属性值。
fn escape_html_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 判断文本是否为需按 HTML 块承载的 `<img>` 串。
fn is_html_img(markdown: &str) -> bool {
    markdown.trim_start().starts_with("<img")
}

impl Editor {
    /// 判断右键目标是否为可操作的图片块，并收集快照。
    pub(super) fn describe_image_menu(
        &self,
        block_id: Option<EntityId>,
        cx: &App,
    ) -> Option<ImageMenuData> {
        let id = block_id.or(self.active_entity_id)?;
        let block = self.focusable_entity_by_id(id)?;
        let base_dir = self.image_base_dir();
        let (alt, title, src, resolved, zoom, from_html, source_markdown) = {
            let block_ref = block.read(cx);
            if let Some(runtime) = block_ref.image_runtime() {
                (
                    runtime.alt.clone(),
                    runtime.title.clone(),
                    runtime.src.clone(),
                    runtime.resolved_source.clone(),
                    1.0,
                    false,
                    block_ref.rendered_text().to_string(),
                )
            } else {
                let raw = block_ref.rendered_text();
                let image = parse_html_image_block(raw)?;
                (
                    image.alt.clone(),
                    image.title.clone(),
                    image.src.clone(),
                    resolve_image_source(&image.src, base_dir.as_deref()),
                    image.zoom,
                    true,
                    raw.to_string(),
                )
            }
        };
        Some(ImageMenuData {
            block,
            source_markdown,
            alt,
            title,
            src,
            resolved,
            zoom,
            from_html,
        })
    }

    /// 把图片专属条目追加到弹出菜单最前面。
    pub(super) fn build_image_menu_items(
        menu: PopupMenu,
        editor: &Entity<Editor>,
        data: &ImageMenuData,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let strings = cx.global::<I18nManager>().snapshot();
        let mut menu = menu
            .item(Self::image_item(
                strings.t("image_open_location", &[]),
                data.is_local(),
                false,
                editor,
                data,
                Self::on_image_open_location,
            ))
            .item(Self::image_item(
                strings.t("image_copy_to", &[]),
                !data.is_remote(),
                false,
                editor,
                data,
                Self::on_image_copy_to,
            ))
            .item(Self::image_item(
                strings.t("image_copy_bits", &[]),
                !data.is_remote(),
                false,
                editor,
                data,
                Self::on_image_copy_bits,
            ))
            .item(Self::image_item(
                strings.t("image_copy_path", &[]),
                data.is_local(),
                false,
                editor,
                data,
                Self::on_image_copy_path,
            ))
            .separator();

        menu = menu.submenu(strings.t("image_resize", &[]), window, cx, {
            let editor = editor.clone();
            let data = data.clone();
            move |sub, _window, _cx| {
                let mut sub = sub;
                for zoom in ZOOM_OPTIONS {
                    let label = format!("{}%", (zoom * 100.0).round() as i64);
                    let checked = (data.zoom - zoom).abs() < 1e-3;
                    sub = sub.item(Self::image_item(
                        label,
                        true,
                        checked,
                        &editor,
                        &data,
                        move |editor, data, window, cx| {
                            Self::on_image_zoom(editor, data, zoom, window, cx)
                        },
                    ));
                }
                sub
            }
        });

        menu = menu.submenu(strings.t("image_copy_as", &[]), window, cx, {
            let editor = editor.clone();
            let data = data.clone();
            let strings = strings.clone();
            move |sub, _window, _cx| {
                sub.item(Self::image_item(
                    strings.t("image_copy_as_markdown", &[]),
                    true,
                    false,
                    &editor,
                    &data,
                    |editor, data, window, cx| {
                        Self::on_image_copy_as(editor, data, CopyAsFormat::Markdown, window, cx)
                    },
                ))
                .item(Self::image_item(
                    strings.t("image_copy_as_html", &[]),
                    true,
                    false,
                    &editor,
                    &data,
                    |editor, data, window, cx| {
                        Self::on_image_copy_as(editor, data, CopyAsFormat::Html, window, cx)
                    },
                ))
                .item(Self::image_item(
                    strings.t("image_copy_as_url", &[]),
                    true,
                    false,
                    &editor,
                    &data,
                    |editor, data, window, cx| {
                        Self::on_image_copy_as(editor, data, CopyAsFormat::Url, window, cx)
                    },
                ))
            }
        });

        menu.separator()
            .item(Self::image_item(
                strings.t("image_embed_base64", &[]),
                data.is_local(),
                false,
                editor,
                data,
                Self::on_image_embed_base64,
            ))
            .item(Self::image_item(
                strings.t("image_extract_file", &[]),
                data.is_embedded(),
                false,
                editor,
                data,
                Self::on_image_extract_file,
            ))
    }

    /// 构造单个图片菜单项。
    fn image_item(
        label: String,
        enabled: bool,
        checked: bool,
        editor: &Entity<Editor>,
        data: &ImageMenuData,
        op: impl Fn(&mut Editor, &ImageMenuData, &mut Window, &mut Context<Editor>)
        + Send
        + Sync
        + 'static,
    ) -> PopupMenuItem {
        PopupMenuItem::new(label)
            .disabled(!enabled)
            .checked(checked)
            .on_click(Self::image_click_handler(editor.clone(), data.clone(), op))
    }

    /// 构造图片菜单点击闭包：把快照交给操作处理器。
    fn image_click_handler(
        editor: Entity<Editor>,
        data: ImageMenuData,
        op: impl Fn(&mut Editor, &ImageMenuData, &mut Window, &mut Context<Editor>)
        + Send
        + Sync
        + 'static,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + Send + Sync + 'static {
        move |_event: &ClickEvent, window: &mut Window, cx: &mut App| {
            editor.update(cx, |editor, cx| op(editor, &data, window, cx));
        }
    }

    /// 打开图片所在位置（在系统文件管理器中定位本地文件）。
    pub(super) fn on_image_open_location(
        editor: &mut Editor,
        data: &ImageMenuData,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor.close_context_menu(cx);
        if let Some(path) = data.local_path() {
            cx.reveal_path(path);
        }
    }

    /// 把图片另存到用户选择的目录。
    pub(super) fn on_image_copy_to(
        editor: &mut Editor,
        data: &ImageMenuData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor.close_context_menu(cx);
        let (_, bytes) = match data.load_bytes() {
            Ok(loaded) => loaded,
            Err(err) => return Self::show_image_action_error(err, cx),
        };
        let preferred_name = data
            .local_path()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("image.png")
            .to_string();
        let prompt_title = cx
            .global::<I18nManager>()
            .snapshot()
            .t("dialog_choose_image_folder", &[]);
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt_title.into()),
        });
        let window_handle = window.window_handle();
        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let directory = match prompt.await {
                Ok(Ok(Some(mut paths))) if !paths.is_empty() => paths.remove(0),
                Ok(Ok(_)) | Err(_) => return,
                Ok(Err(err)) => {
                    show_image_action_error_in_window(window_handle, err.to_string(), cx).await;
                    return;
                }
            };
            let target = Editor::unique_file_path(&directory, &preferred_name);
            if let Err(err) = crate::atomic_file::write_bytes(&target, &bytes) {
                show_image_action_error_in_window(window_handle, err.to_string(), cx).await;
            }
        })
        .detach();
    }

    /// 把图片位图复制到剪贴板。
    pub(super) fn on_image_copy_bits(
        editor: &mut Editor,
        data: &ImageMenuData,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor.close_context_menu(cx);
        let (format, bytes) = match data.load_bytes() {
            Ok(loaded) => loaded,
            Err(err) => return Self::show_image_action_error(err, cx),
        };
        let image = Image::from_bytes(format, bytes);
        cx.write_to_clipboard(ClipboardItem::new_image(&image));
    }

    /// 把图片的本地路径作为文件剪贴板项复制。
    pub(super) fn on_image_copy_path(
        editor: &mut Editor,
        data: &ImageMenuData,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor.close_context_menu(cx);
        let Some(path) = data.local_path() else {
            return;
        };
        let mut paths = ExternalPaths::default();
        paths.0.push(path.to_path_buf());
        cx.write_to_clipboard(ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(paths)],
        });
    }

    /// 设置图片显示缩放（100% 时还原为纯 Markdown 图片）。
    pub(super) fn on_image_zoom(
        editor: &mut Editor,
        data: &ImageMenuData,
        zoom: f32,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor.close_context_menu(cx);
        let markdown = if (zoom - 1.0).abs() < 1e-3 {
            data.markdown_form()
        } else {
            data.html_form(zoom)
        };
        Self::rewrite_image_block(editor, data, markdown, cx);
    }

    /// 复制图片为 Markdown / HTML / URL 文本。
    pub(super) fn on_image_copy_as(
        editor: &mut Editor,
        data: &ImageMenuData,
        format: CopyAsFormat,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor.close_context_menu(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(data.copy_text(format)));
    }

    /// 把本地图片改写为内嵌 base64 `data:` URI。
    pub(super) fn on_image_embed_base64(
        editor: &mut Editor,
        data: &ImageMenuData,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor.close_context_menu(cx);
        let Some(path) = data.local_path().map(Path::to_path_buf) else {
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                return Self::show_image_action_error(
                    anyhow::Error::new(err).context(format!("failed to read '{}'", path.display())),
                    cx,
                );
            }
        };
        if let Err(err) = Self::check_embedded_size(bytes.len()) {
            return Self::show_image_action_error(err, cx);
        }
        let mime = Self::embedded_mime_for_extension(&path);
        let uri = Self::embedded_data_uri(mime, &bytes);
        let markdown = data.markdown_with_target(&Self::markdown_image_target(&uri));
        Self::rewrite_image_block(editor, data, markdown, cx);
    }

    /// 把内嵌 `data:` URI 图片还原为磁盘文件并改回相对路径引用。
    pub(super) fn on_image_extract_file(
        editor: &mut Editor,
        data: &ImageMenuData,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor.close_context_menu(cx);
        let (format, bytes) = match data.load_bytes() {
            Ok(loaded) => loaded,
            Err(err) => return Self::show_image_action_error(err, cx),
        };
        let source = PastedImageSource::ClipboardImage(Image::from_bytes(format, bytes));
        let (path, relative) = match editor.materialize_pasted_image(&source) {
            Ok(materialized) => materialized,
            Err(err) => return Self::show_image_action_error(err, cx),
        };
        let target = if relative {
            let Ok(root_dir) = editor.image_paste_root_dir() else {
                return;
            };
            let Some(relative_path) = Self::relative_markdown_path(&root_dir, &path) else {
                return;
            };
            Self::markdown_image_target(&relative_path)
        } else {
            Self::markdown_image_target(&Self::markdown_path_string(&path))
        };
        let markdown = data.markdown_with_target(&target);
        Self::rewrite_image_block(editor, data, markdown, cx);
    }

    /// 用新的图片文本替换块内容（HTML 串落到 HTML 块，否则为段落）。
    pub(super) fn rewrite_image_block(
        editor: &mut Editor,
        data: &ImageMenuData,
        markdown: String,
        cx: &mut Context<Self>,
    ) {
        let kind = if is_html_img(&markdown) {
            BlockKind::HtmlBlock
        } else {
            BlockKind::Paragraph
        };
        let cursor = markdown.len();
        Self::set_block_title_and_kind(
            &data.block,
            kind,
            InlineTextTree::plain(markdown),
            cursor,
            cx,
        );
        editor.focus_block(data.block.entity_id());
        editor.mark_dirty(cx);
        editor.finalize_pending_undo_capture(cx);
        editor.rebuild_image_runtimes(cx);
        cx.notify();
    }

    /// 用提示框展示图片操作失败原因。
    fn show_image_action_error(err: anyhow::Error, cx: &mut Context<Self>) {
        let i18n = cx.global::<I18nManager>().snapshot();
        let title = i18n.t("message_image_action_failed", &[]);
        let detail = err.to_string();
        if let Some(window) = cx.active_window() {
            let ok = i18n.t("dialog_ok", &[]);
            let _ = window.update(cx, |_view, window, cx| {
                let buttons = [ok.as_str()];
                let prompt =
                    window.prompt(PromptLevel::Critical, &title, Some(&detail), &buttons, cx);
                cx.spawn(async move |_| {
                    let _ = prompt.await;
                })
                .detach();
            });
        } else {
            eprintln!("{title}: {err}");
        }
    }
}

/// 在指定窗口异步展示图片操作失败提示。
async fn show_image_action_error_in_window(
    window_handle: AnyWindowHandle,
    detail: String,
    cx: &mut AsyncApp,
) {
    let _ = cx.update_window(
        window_handle,
        move |_view: AnyView, window: &mut Window, cx: &mut App| {
            let i18n = cx.global::<I18nManager>().snapshot();
            let ok = i18n.t("dialog_ok", &[]);
            let title = i18n.t("message_image_action_failed", &[]);
            let buttons = [ok.as_str()];
            let prompt = window.prompt(PromptLevel::Critical, &title, Some(&detail), &buttons, cx);
            cx.spawn(async move |_| {
                let _ = prompt.await;
            })
            .detach();
        },
    );
}
