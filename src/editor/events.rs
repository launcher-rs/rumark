//! 编辑器侧处理子块发出的 [`BlockEvent`] 事件。
//!
//! 这里是拆分、合并、缩进、退格、删除、多行粘贴、焦点转移与脏标记
//! 的中央变更引擎。运行时树变更委托给 [`DocumentTree`](super::tree::DocumentTree），
//! 使可见顺序元数据与每次编辑保持同步。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use rgpui::*;

use super::Editor;
use crate::atomic_file::{copy_file, write_bytes};
use crate::components::{
    BlockEvent, BlockKind, BlockRecord, CaretAffinity, CellPosition, IndentBlock, InlineTextTree,
    OutdentBlock, PastedImageSource, is_table_row_candidate, parse_root_table_region,
    parse_table_body_row, update_entity_forgiving,
};
use crate::config::{ImagePasteBehavior, read_app_preferences};

impl Editor {
    /// 找到当前实际聚焦且可处理 Tab 键的块。
    fn focused_block_for_tab_key(
        &self,
        window: &mut Window,
        cx: &App,
    ) -> Option<Entity<super::Block>> {
        let is_focused = |block: &Entity<super::Block>| {
            let block = block.read(cx);
            block.focus_handle.is_focused(window)
                || block.code_language_focus_handle.is_focused(window)
        };

        if let Some(block) = self
            .active_entity_id
            .and_then(|entity_id| self.focusable_entity_by_id(entity_id))
            .filter(is_focused)
        {
            return Some(block);
        }

        for binding in self.table_cells.values() {
            if is_focused(&binding.cell) {
                return Some(binding.cell.clone());
            }
        }

        self.document
            .visible_blocks()
            .iter()
            .find_map(|visible| is_focused(&visible.entity).then(|| visible.entity.clone()))
    }

    /// 捕获 Tab 键并转发为缩进/退格（仅处理支持的块）。
    pub(crate) fn on_editor_key_down_capture(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key != "tab" {
            return;
        }

        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt || modifiers.function {
            return;
        }

        // 链接弹窗打开时 Tab 在文本/URL 框之间切换，避免被输入框缩进或块缩进吞掉。
        if let Some(dialog) = self.link_dialog.as_ref() {
            let label_focused = dialog.label.read(cx).is_focused(window);
            let url_focused = dialog.url.read(cx).is_focused(window);
            if label_focused || url_focused {
                let next = if modifiers.shift {
                    if url_focused {
                        dialog.label.clone()
                    } else {
                        dialog.url.clone()
                    }
                } else if label_focused {
                    dialog.url.clone()
                } else {
                    dialog.label.clone()
                };
                next.update(cx, |state, cx| {
                    state.focus(window, cx);
                });
                cx.stop_propagation();
                return;
            }
        }

        // 查找栏打开时 Tab 在查询/替换框之间切换。
        if let Some(bar) = self.find_bar.as_ref() {
            let query_focused = bar.query.read(cx).is_focused(window);
            let replace_focused = bar.replace.read(cx).is_focused(window);
            if query_focused || replace_focused {
                let next = if replace_focused {
                    bar.query.clone()
                } else {
                    bar.replace.clone()
                };
                next.update(cx, |state, cx| {
                    state.focus(window, cx);
                });
                cx.stop_propagation();
                return;
            }
        }

        let Some(target) = self.focused_block_for_tab_key(window, cx) else {
            return;
        };

        let handles_tab = {
            let block = target.read(cx);
            if block.code_language_focus_handle.is_focused(window) {
                cx.stop_propagation();
                return;
            }
            block.is_table_cell()
                || block.kind().is_list_item()
                || block.kind() == BlockKind::Paragraph
                || block.kind().is_code_block()
        };

        if !handles_tab {
            return;
        }

        if modifiers.shift {
            target.update(cx, |block, block_cx| {
                block.on_outdent_block(&OutdentBlock, window, block_cx);
            });
        } else {
            target.update(cx, |block, block_cx| {
                block.on_indent_block(&IndentBlock, window, block_cx);
            });
        }
        cx.stop_propagation();
    }

    /// 从纯文本行构建粘贴用的段落块列表。
    fn build_plain_paste_blocks_from_lines(
        cx: &mut Context<Self>,
        lines: &[String],
    ) -> Vec<Entity<super::Block>> {
        let mut blocks = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                Self::new_block(
                    cx,
                    BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown(line)),
                )
            })
            .collect::<Vec<_>>();

        if blocks.is_empty() && !lines.is_empty() {
            blocks.push(Self::new_block(
                cx,
                BlockRecord::new(BlockKind::Paragraph, InlineTextTree::plain(String::new())),
            ));
        }

        blocks
    }

    /// 判断块是否与渲染模式引用结构相关。
    fn block_is_quote_structure_related(&self, block: &Entity<super::Block>, cx: &App) -> bool {
        if self.view_mode != super::ViewMode::Rendered {
            return false;
        }

        let block_ref = block.read(cx);
        block_ref.kind().is_quote_container()
            || block_ref.quote_depth > 0
            || block_ref.quote_group_anchor.is_some()
    }

    /// 引用结构相关时刷新渲染模式的引用元数据。
    fn refresh_rendered_quote_metadata_if_needed(
        &mut self,
        block: &Entity<super::Block>,
        cx: &mut Context<Self>,
    ) {
        if !self.block_is_quote_structure_related(block, cx) {
            return;
        }

        self.document.rebuild_metadata_and_snapshot(cx);
    }

    /// 判断渲染模式引用文本是否需要重新解析。
    fn rendered_quote_text_requires_reparse(block: &Entity<super::Block>, cx: &App) -> bool {
        let block_ref = block.read(cx);
        if block_ref.quote_depth == 0 && !block_ref.kind().is_quote_container() {
            return false;
        }

        let text = block_ref.rendered_text();
        if !text.contains('\n') {
            return false;
        }

        text.split('\n').skip(1).any(|line| {
            let trimmed_end = line.trim_end();
            if trimmed_end.is_empty() {
                return false;
            }

            let leading_spaces = trimmed_end.bytes().take_while(|b| *b == b' ').count();
            if leading_spaces >= 4 {
                return true;
            }

            BlockKind::detect_block_shortcut(&format!("{trimmed_end} "))
                .is_some_and(|(kind, _)| kind != BlockKind::Paragraph)
                || BlockKind::parse_fence_opening(trimmed_end).is_some()
                || BlockKind::parse_separator(trimmed_end)
                || BlockKind::parse_heading_line(trimmed_end).is_some()
        })
    }

    /// 判断该块事件是否应清除跨块选区。
    fn block_event_clears_cross_block_selection(event: &BlockEvent) -> bool {
        matches!(
            event,
            BlockEvent::Changed
                | BlockEvent::RequestNewline { .. }
                | BlockEvent::RequestEnterCalloutBody
                | BlockEvent::RequestQuoteBreak
                | BlockEvent::RequestCalloutBreak
                | BlockEvent::RequestMergeIntoPrev { .. }
                | BlockEvent::RequestPasteMultiline { .. }
                | BlockEvent::RequestPasteImage { .. }
                | BlockEvent::RequestIndent
                | BlockEvent::RequestOutdent
                | BlockEvent::RequestDowngradeNestedListItemToChildParagraph
                | BlockEvent::ToggleTaskChecked
                | BlockEvent::RequestAppendTableColumn
                | BlockEvent::RequestAppendTableRow
                | BlockEvent::RequestDelete
        )
    }

    /// 聚焦指定块并排队滚动到视口内。
    pub(crate) fn focus_block(&mut self, entity_id: EntityId) {
        self.pending_focus = Some(entity_id);
        self.active_entity_id = Some(entity_id);
        self.pending_scroll_active_block_into_view = true;
    }

    /// 重置块光标到指定偏移并清除选区状态。
    fn reset_block_cursor(block: &Entity<super::Block>, cursor: usize, cx: &mut Context<Self>) {
        block.update(cx, move |block, cx| {
            block.selected_range = cursor..cursor;
            block.selection_reversed = false;
            block.marked_range = None;
            block.vertical_motion_x = None;
            block.cursor_blink_epoch = Instant::now();
            cx.notify();
        });
    }

    /// 聚焦块并设置其选中区间。
    fn focus_block_range(
        &mut self,
        block: &Entity<super::Block>,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) {
        block.update(cx, move |block, cx| {
            block.selected_range = range.clone();
            block.selection_reversed = false;
            block.marked_range = None;
            block.vertical_motion_x = None;
            block.cursor_blink_epoch = Instant::now();
            cx.notify();
        });
        self.focus_block(block.entity_id());
    }

    /// 读取当前图片粘贴行为偏好。
    fn current_image_paste_behavior() -> ImagePasteBehavior {
        read_app_preferences()
            .map(|preferences| preferences.image_paste_behavior)
            .unwrap_or(ImagePasteBehavior::None)
    }

    /// 获取图片粘贴的基准目录（文档所在目录或当前目录）。
    fn image_paste_root_dir(&self) -> anyhow::Result<PathBuf> {
        if let Some(parent) = self.file_path.as_ref().and_then(|path| path.parent()) {
            return Ok(parent.to_path_buf());
        }
        std::env::current_dir().context("failed to resolve current working directory")
    }

    /// 按剪贴板图片格式返回文件扩展名。
    fn clipboard_image_extension(format: ImageFormat) -> &'static str {
        match format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
            ImageFormat::Gif => "gif",
            ImageFormat::Svg => "svg",
            ImageFormat::Bmp => "bmp",
            ImageFormat::Tiff => "tiff",
            ImageFormat::Ico => "ico",
            ImageFormat::Pnm => "pnm",
        }
    }

    /// 按粘贴偏好计算图片的目标目录。
    fn image_target_dir(
        &self,
        behavior: ImagePasteBehavior,
        root_dir: &Path,
        source: &PastedImageSource,
    ) -> anyhow::Result<PathBuf> {
        match behavior {
            ImagePasteBehavior::None
            | ImagePasteBehavior::CopyToDocumentFolder
            | ImagePasteBehavior::EmbedAsDataUri => Ok(root_dir.to_path_buf()),
            ImagePasteBehavior::CopyToAssetsFolder => Ok(root_dir.join("assets")),
            ImagePasteBehavior::CopyToNamedAssetsFolder => {
                let base = self
                    .file_path
                    .as_ref()
                    .and_then(|path| path.file_stem())
                    .and_then(|stem| stem.to_str())
                    .filter(|stem| !stem.trim().is_empty())
                    .unwrap_or("untitle");
                if self.file_path.is_some() {
                    return Ok(root_dir.join(format!("{base}.assets")));
                }

                for index in 0.. {
                    let folder = if index == 0 {
                        "untitle.assets".to_string()
                    } else {
                        format!("untitle{index}.assets")
                    };
                    let path = root_dir.join(folder);
                    if !path.exists() {
                        return Ok(path);
                    }
                    if matches!(source, PastedImageSource::LocalPath(_)) {
                        continue;
                    }
                }
                unreachable!("unbounded search should always return");
            }
        }
    }

    /// 在目录内生成不冲突的文件名（含序号后缀）。
    fn unique_file_path(dir: &Path, preferred_name: &str) -> PathBuf {
        let preferred = Path::new(preferred_name);
        let stem = preferred
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("image");
        let extension = preferred.extension().and_then(|ext| ext.to_str());
        for index in 0.. {
            let file_name = if index == 0 {
                preferred_name.to_string()
            } else if let Some(extension) = extension {
                format!("{stem}{index}.{extension}")
            } else {
                format!("{stem}{index}")
            };
            let candidate = dir.join(file_name);
            if !candidate.exists() {
                return candidate;
            }
        }
        unreachable!("unbounded search should always return");
    }

    /// 判断左侧路径的父目录与右侧路径是否相同。
    fn path_parent_eq(left: &Path, right: &Path) -> bool {
        let Some(parent) = left.parent() else {
            return false;
        };
        let left = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
        left == right
    }

    /// 落盘粘贴的图片并返回路径与是否相对引用。
    fn materialize_pasted_image(
        &self,
        source: &PastedImageSource,
    ) -> anyhow::Result<(PathBuf, bool)> {
        let behavior = Self::current_image_paste_behavior();
        let root_dir = self.image_paste_root_dir()?;

        if matches!(behavior, ImagePasteBehavior::None)
            && let PastedImageSource::LocalPath(path) = source
        {
            return Ok((path.clone(), false));
        }

        let target_dir = self.image_target_dir(behavior, &root_dir, source)?;
        fs::create_dir_all(&target_dir)
            .with_context(|| format!("failed to create '{}'", target_dir.display()))?;

        match source {
            PastedImageSource::LocalPath(path) => {
                if Self::path_parent_eq(path, &target_dir) {
                    return Ok((path.clone(), behavior != ImagePasteBehavior::None));
                }
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("image");
                let target = Self::unique_file_path(&target_dir, file_name);
                // 与粘贴图片一样走原子写：`fs::copy` 写到一半中断会留下半截文件，
                // 而文档里可能已经引用它。
                copy_file(path, &target).with_context(|| {
                    format!(
                        "failed to copy '{}' to '{}'",
                        path.display(),
                        target.display()
                    )
                })?;
                Ok((target, behavior != ImagePasteBehavior::None))
            }
            PastedImageSource::ClipboardImage(image) => {
                let file_name = format!(
                    "pasted-image.{}",
                    Self::clipboard_image_extension(image.format)
                );
                let target = Self::unique_file_path(&target_dir, &file_name);
                write_bytes(&target, &image.bytes)
                    .with_context(|| format!("failed to write '{}'", target.display()))?;
                Ok((target, behavior != ImagePasteBehavior::None))
            }
        }
    }

    /// 把路径转为 Markdown 可用的斜杠形式。
    fn markdown_path_string(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    /// 转义图片目标路径中的特殊字符。
    fn markdown_image_target(path: &str) -> String {
        path.chars()
            .flat_map(|ch| match ch {
                '\\' | '(' | ')' | '"' => ['\\', ch].into_iter().collect::<Vec<_>>(),
                _ => [ch].into_iter().collect::<Vec<_>>(),
            })
            .collect()
    }

    /// 按文件名生成转义后的图片替代文本。
    fn markdown_image_alt(path: &Path) -> String {
        let alt = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("image");
        alt.chars()
            .flat_map(|ch| match ch {
                '\\' | ']' => ['\\', ch].into_iter().collect::<Vec<_>>(),
                _ => [ch].into_iter().collect::<Vec<_>>(),
            })
            .collect()
    }

    /// 计算相对基准目录的 `./` 开头图片路径。
    fn relative_markdown_path(root_dir: &Path, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(root_dir).ok()?;
        Some(format!("./{}", Self::markdown_path_string(relative)))
    }

    /// 按文件扩展名推断内嵌 `data:` URI 的 MIME 类型。
    fn embedded_mime_for_extension(path: &Path) -> &'static str {
        match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref()
        {
            Some("png") => "image/png",
            Some("jpg") | Some("jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            Some("svg") => "image/svg+xml",
            Some("bmp") => "image/bmp",
            Some("tif") | Some("tiff") => "image/tiff",
            Some("ico") => "image/x-icon",
            _ => "image/png",
        }
    }

    /// 按剪贴板图片格式推断内嵌 `data:` URI 的 MIME 类型。
    fn embedded_mime_for_format(format: ImageFormat) -> &'static str {
        match format {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Webp => "image/webp",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Svg => "image/svg+xml",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Tiff => "image/tiff",
            ImageFormat::Ico => "image/x-icon",
            ImageFormat::Pnm => "image/png",
        }
    }

    /// 把字节编码为内嵌 `data:` URI（base64 单文件便携）。
    fn embedded_data_uri(mime: &str, bytes: &[u8]) -> String {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        format!("data:{mime};base64,{encoded}")
    }

    /// 内嵌体积上限：base64 膨胀约 1/3，5MB 原图即约 6.8MB 文本，文档会明显卡顿。
    const MAX_EMBED_BYTES: usize = 5 * 1024 * 1024;

    /// 校验内嵌字节体积，超限返回中文友好错误（经粘贴错误弹窗展示）。
    fn check_embedded_size(bytes_len: usize) -> anyhow::Result<()> {
        if bytes_len > Self::MAX_EMBED_BYTES {
            anyhow::bail!(
                "图片约 {:.1}MB，超过内嵌上限 5MB，内嵌会导致文档膨胀卡顿，建议改用“复制图片到附件目录”偏好",
                bytes_len as f64 / (1024.0 * 1024.0)
            );
        }
        Ok(())
    }

    /// 内嵌模式：直接把图片字节转为 Markdown 图片串，不落盘。
    fn embedded_image_markdown(&self, source: &PastedImageSource) -> anyhow::Result<String> {
        match source {
            PastedImageSource::LocalPath(path) => {
                let bytes = fs::read(path)
                    .with_context(|| format!("failed to read '{}'", path.display()))?;
                Self::check_embedded_size(bytes.len())?;
                let mime = Self::embedded_mime_for_extension(path);
                let uri = Self::embedded_data_uri(mime, &bytes);
                Ok(format!(
                    "![{}]({})",
                    Self::markdown_image_alt(path),
                    Self::markdown_image_target(&uri)
                ))
            }
            PastedImageSource::ClipboardImage(image) => {
                Self::check_embedded_size(image.bytes.len())?;
                let mime = Self::embedded_mime_for_format(image.format);
                let uri = Self::embedded_data_uri(mime, &image.bytes);
                // 剪贴板图片无文件名，替代文本固定为 image。
                Ok(format!("![image]({uri})"))
            }
        }
    }

    /// 为粘贴的图片来源生成 Markdown 图片串。
    fn pasted_image_markdown(&self, source: &PastedImageSource) -> anyhow::Result<String> {
        if matches!(
            Self::current_image_paste_behavior(),
            ImagePasteBehavior::EmbedAsDataUri
        ) {
            return self.embedded_image_markdown(source);
        }
        let root_dir = self.image_paste_root_dir()?;
        let (path, relative) = self.materialize_pasted_image(source)?;
        let path_text = if relative {
            Self::relative_markdown_path(&root_dir, &path)
                .ok_or_else(|| anyhow!("failed to create a relative image path"))?
        } else {
            Self::markdown_path_string(&path)
        };
        Ok(format!(
            "![{}]({})",
            Self::markdown_image_alt(&path),
            Self::markdown_image_target(&path_text)
        ))
    }

    /// 本地图片经粘贴管线落盘/定位后，仅返回填入对话框 URL 栏的目标串。
    ///
    /// 复用用户的图片粘贴偏好（原地引用/复制到文档目录或附件目录），
    /// 与粘贴图片的落点语义完全一致。
    pub(super) fn local_image_url_for_path(&self, path: &Path) -> anyhow::Result<String> {
        // 内嵌偏好下浏览本地文件直接编码为 `data:` URI，不落盘不改名。
        if matches!(
            Self::current_image_paste_behavior(),
            ImagePasteBehavior::EmbedAsDataUri
        ) {
            let bytes =
                fs::read(path).with_context(|| format!("failed to read '{}'", path.display()))?;
            Self::check_embedded_size(bytes.len())?;
            let mime = Self::embedded_mime_for_extension(path);
            let uri = Self::embedded_data_uri(mime, &bytes);
            return Ok(Self::markdown_image_target(&uri));
        }
        let root_dir = self.image_paste_root_dir()?;
        let source = PastedImageSource::LocalPath(path.to_path_buf());
        let (final_path, relative) = self.materialize_pasted_image(&source)?;
        if relative {
            Self::relative_markdown_path(&root_dir, &final_path)
                .ok_or_else(|| anyhow!("failed to create a relative image path"))
                .map(|path_text| Self::markdown_image_target(&path_text))
        } else {
            Ok(Self::markdown_image_target(&Self::markdown_path_string(
                &final_path,
            )))
        }
    }

    /// 显示图片粘贴失败的提示框。
    pub(super) fn show_image_paste_error(&self, err: anyhow::Error, cx: &mut Context<Self>) {
        let i18n = cx.global::<crate::i18n::I18nManager>().snapshot();
        let ok = i18n.t("dialog_ok", &[]);
        let title = i18n.t("message_image_paste_failed", &[]);
        if let Some(window) = cx.active_window() {
            let detail = err.to_string();
            let _ = window.update(cx, |_view, window, cx| {
                // 对话框 future 必须轮询才会弹出：转交后台任务等待用户响应。
                let title = title.clone();
                let detail = detail.clone();
                let ok = ok.clone();
                let buttons = [ok.as_str()];
                let prompt =
                    window.prompt(PromptLevel::Critical, &title, Some(&detail), &buttons, cx);
                cx.spawn(async move |_| {
                    let _ = prompt.await;
                })
                .detach();
            });
        } else {
            eprintln!("{}: {err}", i18n.t("message_image_paste_failed", &[]));
        }
    }

    /// 按块类型构造插入图片的行内文本树。
    fn inserted_image_tree_for_block(block: &super::Block, markdown: &str) -> InlineTextTree {
        if block.uses_raw_text_editing() || block.kind().is_code_block() {
            InlineTextTree::plain(markdown.to_string())
        } else {
            InlineTextTree::from_markdown(markdown)
        }
    }

    /// 用图片文本替换当前块的选中区。
    fn replace_current_block_selection_with_image_text(
        &mut self,
        block: &Entity<super::Block>,
        leading: &InlineTextTree,
        markdown: &str,
        trailing: &InlineTextTree,
        cx: &mut Context<Self>,
    ) {
        let (kind, title, cursor) = block.read_with(cx, |block, _cx| {
            let mut title = leading.clone();
            title.push_tree(Self::inserted_image_tree_for_block(block, markdown));
            let cursor = title.plain_len();
            title.push_tree(trailing.clone());
            (block.kind(), title, cursor)
        });
        Self::set_block_title_and_kind(block, kind, title, cursor, cx);
        if let Some(binding) = self.table_cell_binding(block.entity_id()) {
            self.sync_table_record_from_runtime(&binding.table_block, cx);
        }
        self.focus_block(block.entity_id());
        self.rebuild_image_runtimes(cx);
    }

    /// 在段落后拆分出独立的图片块（含前后段落）。
    fn insert_image_block_after_paragraph(
        &mut self,
        block: &Entity<super::Block>,
        leading: &InlineTextTree,
        markdown: &str,
        trailing: &InlineTextTree,
        cx: &mut Context<Self>,
    ) {
        let Some(location) = self.document.find_block_location(block.entity_id()) else {
            return;
        };
        let leading_empty = leading.plain_len() == 0;
        let trailing_empty = trailing.plain_len() == 0;

        if leading_empty {
            Self::set_block_title_and_kind(
                block,
                BlockKind::Paragraph,
                InlineTextTree::plain(markdown.to_string()),
                markdown.len(),
                cx,
            );
            let image_block = block.clone();
            if !trailing_empty {
                let trailing_block =
                    Self::new_block(cx, BlockRecord::new(BlockKind::Paragraph, trailing.clone()));
                self.document.insert_blocks_at(
                    location.parent,
                    location.index + 1,
                    vec![trailing_block],
                    cx,
                );
            }
            self.focus_block(image_block.entity_id());
            self.rebuild_image_runtimes(cx);
            return;
        }

        Self::set_block_title_and_kind(
            block,
            BlockKind::Paragraph,
            leading.clone(),
            leading.plain_len(),
            cx,
        );
        let image_block = Self::new_block(cx, BlockRecord::paragraph(markdown.to_string()));
        let mut inserted = vec![image_block.clone()];
        if !trailing_empty {
            inserted.push(Self::new_block(
                cx,
                BlockRecord::new(BlockKind::Paragraph, trailing.clone()),
            ));
        }
        self.document
            .insert_blocks_at(location.parent, location.index + 1, inserted, cx);
        self.focus_block(image_block.entity_id());
        self.rebuild_image_runtimes(cx);
    }

    /// 处理粘贴图片请求（落盘后按模式插入）。
    fn handle_paste_image_request(
        &mut self,
        block: Entity<super::Block>,
        leading: &InlineTextTree,
        source: &PastedImageSource,
        trailing: &InlineTextTree,
        cx: &mut Context<Self>,
    ) {
        let markdown = match self.pasted_image_markdown(source) {
            Ok(markdown) => markdown,
            Err(err) => {
                self.show_image_paste_error(err, cx);
                return;
            }
        };

        if self.replace_cross_block_selection_with_text(
            &markdown,
            None,
            false,
            crate::components::UndoKind::NonCoalescible,
            cx,
        ) {
            return;
        }

        self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
        let can_insert_image_block = self.view_mode == super::ViewMode::Rendered
            && block.read(cx).kind() == BlockKind::Paragraph
            && self.table_cell_binding(block.entity_id()).is_none()
            && !block.read(cx).uses_raw_text_editing();

        if can_insert_image_block {
            self.insert_image_block_after_paragraph(&block, leading, &markdown, trailing, cx);
        } else {
            self.replace_current_block_selection_with_image_text(
                &block, leading, &markdown, trailing, cx,
            );
        }

        self.mark_dirty(cx);
        self.finalize_pending_undo_capture(cx);
        cx.notify();
    }

    /// 跳转到脚注定义块并聚焦行首。
    fn jump_to_footnote_definition(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(binding) = self.footnote_registry.binding(id) else {
            return false;
        };
        let Some(block) = self.focusable_entity_by_id(binding.definition_entity_id) else {
            return false;
        };
        self.focus_block_range(&block, 0..0, cx);
        true
    }

    /// 跳转到脚注首个引用处并选中该引用。
    fn jump_to_footnote_backref(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(binding) = self.footnote_registry.binding(id) else {
            return false;
        };
        let Some(first_reference) = binding.first_reference.as_ref() else {
            return false;
        };
        let Some(block) = self.focusable_entity_by_id(first_reference.entity_id) else {
            return false;
        };
        let range = block
            .read(cx)
            .current_range_for_footnote_occurrence(first_reference.occurrence_index)
            .unwrap_or(0..0);
        self.focus_block_range(&block, range, cx);
        true
    }

    /// 在列表项前插入空段落以分隔编号列表分组。
    fn insert_list_group_separator_before(
        &mut self,
        entity_id: EntityId,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(location) = self.document.find_block_location(entity_id) else {
            return false;
        };

        let separator = Self::new_block(cx, BlockRecord::paragraph(String::new()));
        self.document
            .insert_blocks_at(location.parent, location.index, vec![separator], cx);
        true
    }

    /// 设置块标题与类型（含快捷写法展开与光标定位）。
    fn set_block_title_and_kind(
        block: &Entity<super::Block>,
        kind: BlockKind,
        title: InlineTextTree,
        cursor: usize,
        cx: &mut Context<Self>,
    ) {
        let (kind, title, cursor) = Self::apply_paragraph_shortcuts(kind, title, cursor);
        block.update(cx, move |block, cx| {
            block.record.kind = kind;
            block.record.set_title(title.clone());
            block.sync_edit_mode_from_kind();
            block.refresh_render_cache();
            let clean_cursor = cursor.min(block.record.title.plain_len());
            block.selected_range = block.clean_to_current_range(clean_cursor..clean_cursor);
            block.selection_reversed = false;
            block.marked_range = None;
            block.vertical_motion_x = None;
            block.cursor_blink_epoch = Instant::now();
            cx.notify();
        });
    }

    /// 可被下划线提升为标题的块：非空单行无子块纯段落。
    fn is_setext_heading_target(block: &Entity<super::Block>, cx: &App) -> bool {
        let block = block.read(cx);
        if block.kind() != BlockKind::Paragraph || !block.children.is_empty() {
            return false;
        }
        let text = block.record.title.plain_text();
        !text.trim().is_empty() && !text.contains('\n')
    }

    /// 处理纯下划线段落上的回车：有匹配段落则合并为标题，
    /// 孤立短横线回退为分隔线；消费回车时返回真。
    fn try_form_setext_heading_on_newline(
        &mut self,
        block: &Entity<super::Block>,
        cx: &mut Context<Self>,
    ) -> bool {
        let text = block.read(cx).rendered_text().to_string();
        let Some(level) = BlockKind::parse_setext_rule(&text) else {
            return false;
        };
        if block.read(cx).kind() != BlockKind::Paragraph {
            return false;
        }
        let Some(location) = self.document.find_block_location(block.entity_id()) else {
            return false;
        };

        // 只有根段落会自动成标题；嵌套上下文（引用、
        // 列表）保持原有的换行行为。
        let target = if location.parent.is_none() {
            self.document
                .previous_sibling(block.entity_id(), cx)
                .filter(|prev| Self::is_setext_heading_target(prev, cx))
        } else {
            None
        };

        // 无标题目标的 `=` 下划线是普通文本，走常规换行拆分；
        // 短横线串仍需变为分隔线。
        if target.is_none() && !BlockKind::parse_separator(&text) {
            return false;
        }

        // 换行自带的捕获已由块的 Changed 事件收尾（当时尚无变化），
        // 因此这里开启一次覆盖标题/分隔线转换的新捕获；已有挂起时 prepare 为空操作。
        self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);

        if let Some(prev) = target {
            let heading_title = prev.read(cx).record.title.clone();
            let cursor = heading_title.plain_len();
            let removed_id = block.entity_id();
            let new_paragraph = Self::new_block(cx, BlockRecord::paragraph(String::new()));

            Self::set_block_title_and_kind(
                &prev,
                BlockKind::Heading { level },
                heading_title,
                cursor,
                cx,
            );
            self.document.with_structure_mutation(cx, |document, cx| {
                let _ = document.remove_block_by_id_raw(removed_id, cx);
            });
            if let Some(heading_location) = self.document.find_block_location(prev.entity_id()) {
                self.document.insert_blocks_at(
                    heading_location.parent,
                    heading_location.index + 1,
                    vec![new_paragraph.clone()],
                    cx,
                );
            }
            self.focus_block(new_paragraph.entity_id());
        } else {
            block.update(cx, |block, _cx| block.make_separator());
            let new_paragraph = Self::new_block(cx, BlockRecord::paragraph(String::new()));
            self.document.insert_blocks_at(
                location.parent,
                location.index + 1,
                vec![new_paragraph.clone()],
                cx,
            );
            self.focus_block(new_paragraph.entity_id());
        }

        self.rebuild_image_runtimes(cx);
        self.mark_dirty(cx);
        self.finalize_pending_undo_capture(cx);
        cx.notify();
        true
    }

    /// 处理管道表格行上的回车：分隔行组表成表、表下行并入表，
    /// 之后光标落到表下新段落；消费回车时返回真。
    fn try_form_or_extend_table_on_newline(
        &mut self,
        block: &Entity<super::Block>,
        cx: &mut Context<Self>,
    ) -> bool {
        let text = block.read(cx).rendered_text().to_string();
        if block.read(cx).kind() != BlockKind::Paragraph || !is_table_row_candidate(&text) {
            return false;
        }
        let Some(location) = self.document.find_block_location(block.entity_id()) else {
            return false;
        };
        if location.parent.is_some() {
            return false;
        }
        let Some(prev) = self.document.previous_sibling(block.entity_id(), cx) else {
            return false;
        };

        if prev.read(cx).kind() == BlockKind::Table {
            // 紧贴表格手输的多列行即为表格行，与粘贴行同样按
            // 表头宽度对不齐的格数补齐或截断后并入表格。
            return self.extend_table_with_typed_row(&prev, block, &text, cx);
        }

        if prev.read(cx).kind() != BlockKind::Paragraph {
            return false;
        }
        let header_text = prev.read(cx).rendered_text().to_string();
        if !is_table_row_candidate(&header_text) {
            return false;
        }
        let Some(table) = parse_root_table_region(&[header_text, text]) else {
            return false;
        };

        self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
        // 先删下方（分隔线）块以稳定表头下标。
        let header_index = location.index - 1;
        let removed_delimiter = block.entity_id();
        let removed_header = prev.entity_id();
        let table_block = Self::new_table_block(cx, table);
        let new_paragraph = Self::new_block(cx, BlockRecord::paragraph(String::new()));
        self.document.with_structure_mutation(cx, |document, cx| {
            let _ = document.remove_block_by_id_raw(removed_delimiter, cx);
            let _ = document.remove_block_by_id_raw(removed_header, cx);
        });
        self.document.insert_blocks_at(
            None,
            header_index,
            vec![table_block.clone(), new_paragraph.clone()],
            cx,
        );
        self.rebuild_table_runtimes(cx);
        self.focus_block(new_paragraph.entity_id());
        self.mark_dirty(cx);
        self.finalize_pending_undo_capture(cx);
        cx.notify();
        true
    }

    /// 把手输的表格行并入已有的原生表格。
    fn extend_table_with_typed_row(
        &mut self,
        table_block: &Entity<super::Block>,
        row_block: &Entity<super::Block>,
        text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        // 变更记录前先同步进行中的单元格编辑。
        self.sync_table_record_from_runtime(table_block, cx);
        let Some(mut table) = table_block.read(cx).record.table.clone() else {
            return false;
        };
        let Some(row) = parse_table_body_row(text, table.column_count()) else {
            return false;
        };

        self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
        table.rows.push(row);
        table_block.update(cx, |block, cx| {
            block.record.table = Some(table);
            cx.notify();
        });

        let removed_id = row_block.entity_id();
        self.document.with_structure_mutation(cx, |document, cx| {
            let _ = document.remove_block_by_id_raw(removed_id, cx);
        });
        let new_paragraph = Self::new_block(cx, BlockRecord::paragraph(String::new()));
        if let Some(table_location) = self.document.find_block_location(table_block.entity_id()) {
            self.document.insert_blocks_at(
                table_location.parent,
                table_location.index + 1,
                vec![new_paragraph.clone()],
                cx,
            );
        }
        self.rebuild_table_runtimes(cx);
        self.focus_block(new_paragraph.entity_id());
        self.mark_dirty(cx);
        self.finalize_pending_undo_capture(cx);
        cx.notify();
        true
    }

    /// 结构块末尾补空段落，避免渲染模式下光标无处可去。
    ///
    /// 当块为表格、代码、公式、分隔线、引用、提示框、脚注定义、
    /// 独立图片等光标无法越过的自包含结构，且其容器内暂无后继时，
    /// 在其后补一个空段落，保证渲染优先的用户能继续输入。
    /// 已有后继或非滞留结构时不做任何事。
    pub(super) fn ensure_trailing_paragraph_after_structural(
        &mut self,
        block: &Entity<super::Block>,
        cx: &mut Context<Self>,
    ) {
        let strands = {
            let block = block.read(cx);
            let kind = block.kind();
            kind.is_atomic_block()
                || kind.is_quote_container()
                || kind.is_footnote_definition()
                || block.renders_as_standalone_image()
        };
        if !strands {
            return;
        }
        let Some(location) = self.document.find_block_location(block.entity_id()) else {
            return;
        };
        let sibling_count = match location.parent.as_ref() {
            Some(parent) => parent.read(cx).children.len(),
            None => self.document.root_count(),
        };
        if location.index + 1 < sibling_count {
            return;
        }
        let trailing = Self::new_block(cx, BlockRecord::paragraph(String::new()));
        self.document
            .insert_blocks_at(location.parent, location.index + 1, vec![trailing], cx);
    }

    /// 展开段落快捷写法（如列表标记、标题标记）。
    fn apply_paragraph_shortcuts(
        kind: BlockKind,
        mut title: InlineTextTree,
        cursor: usize,
    ) -> (BlockKind, InlineTextTree, usize) {
        if kind == BlockKind::Paragraph {
            let plain_text = title.plain_text();
            if let Some((detected_kind, prefix_len)) = BlockKind::detect_block_shortcut(&plain_text)
            {
                title.strip_visible_prefix(prefix_len);
                return (detected_kind, title, cursor.saturating_sub(prefix_len));
            }
        }

        (kind, title, cursor)
    }

    /// 点亮滚动条并排队短暂后自动淡出。
    pub(crate) fn reveal_scrollbar(&mut self, cx: &mut Context<Self>) {
        let duration = Duration::from_millis(900);
        self.scrollbar_visible_until = Instant::now() + duration;

        let weak_editor = cx.entity().downgrade();
        self.scrollbar_fade_task = Some(cx.spawn(
            async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
                cx.background_executor()
                    .timer(duration + Duration::from_millis(50))
                    .await;
                // 借用冲突（标题栏拖拽等嵌套派发）时跳过，滚动条下次点亮再淡出。
                let _ = update_entity_forgiving(&weak_editor, cx, |this, cx| {
                    this.scrollbar_fade_task = None;
                    cx.notify();
                });
            },
        ));

        cx.notify();
    }

    /// 鼠标悬停编辑器时点亮滚动条。
    pub(crate) fn on_editor_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scrollbar_hovered = *hovered;
        if *hovered {
            self.reveal_scrollbar(cx);
        } else {
            cx.notify();
        }
    }

    /// 鼠标按下编辑器正文时关闭菜单与行列选中。
    pub(crate) fn on_editor_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_table_axis_preview(cx);
        self.clear_table_axis_selection(cx);
    }

    /// 滚轮滚动时点亮滚动条。
    pub(crate) fn on_editor_scroll_wheel(
        &mut self,
        _event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal_scrollbar(cx);
    }

    /// 向上翻一页（视口高度）。
    pub(crate) fn on_page_up(
        &mut self,
        _: &crate::components::PageUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = self.scroll_handle.bounds().size.height;
        self.scroll_viewport_by(page, cx);
    }

    /// 向下翻一页（视口高度）。
    pub(crate) fn on_page_down(
        &mut self,
        _: &crate::components::PageDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = self.scroll_handle.bounds().size.height;
        self.scroll_viewport_by(-page, cx);
    }

    /// 跳转到文档顶部。
    pub(crate) fn on_jump_to_top(
        &mut self,
        _: &crate::components::JumpToTop,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_vertical_scroll_offset(px(0.0), cx);
    }

    /// 跳转到文档底部。
    pub(crate) fn on_jump_to_bottom(
        &mut self,
        _: &crate::components::JumpToBottom,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let max_offset_y = self.scroll_handle.max_offset().y.max(px(0.0));
        self.set_vertical_scroll_offset(-max_offset_y, cx);
    }

    /// 按增量纵向滚动视口：正值趋向文档开头，负值趋向末尾。
    /// 一页即当前视口高度，因此步长随窗口大小变化。
    fn scroll_viewport_by(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        let target = self.scroll_handle.offset().y + delta;
        self.set_vertical_scroll_offset(target, cx);
    }

    /// 应用绝对纵向滚动偏移并夹取到可滚动范围。
    /// 偏移顶部为 0，底部为 `-max_offset`。
    fn set_vertical_scroll_offset(&mut self, target_y: Pixels, cx: &mut Context<Self>) {
        let max_offset_y = self.scroll_handle.max_offset().y.max(px(0.0));
        let mut offset = self.scroll_handle.offset();
        offset.y = target_y.min(px(0.0)).max(-max_offset_y);
        self.scroll_handle.set_offset(offset);
        // 直接滚动视口后应保持位置，因此取消下一帧
        // 会把活动块重新居中的排队滚动。
        self.pending_scroll_active_block_into_view = false;
        self.pending_scroll_recheck_after_layout = false;
        self.reveal_scrollbar(cx);
        cx.notify();
    }

    /// 开始拖拽自定义滚动条滑块。
    pub(crate) fn start_scrollbar_drag(
        &mut self,
        pointer_offset_y: f32,
        track_height: f32,
        thumb_height: f32,
        max_scroll_y: f32,
        cx: &mut Context<Self>,
    ) {
        self.scrollbar_drag = Some(super::ScrollbarDrag {
            pointer_offset_y: pointer_offset_y.clamp(0.0, thumb_height.max(0.0)),
            track_height,
            thumb_height,
            max_scroll_y,
        });
        self.pending_scroll_active_block_into_view = false;
        self.pending_scroll_recheck_after_layout = false;
        self.reveal_scrollbar(cx);
        cx.notify();
    }

    /// 更新滚动条拖拽位置并同步视口偏移。
    pub(crate) fn update_scrollbar_drag(
        &mut self,
        pointer_y_in_track: f32,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.scrollbar_drag else {
            return;
        };

        let travel = (drag.track_height - drag.thumb_height).max(0.0);
        let thumb_top = (pointer_y_in_track - drag.pointer_offset_y).clamp(0.0, travel);
        let scroll_y = Self::scroll_offset_for_thumb_top(
            thumb_top,
            drag.track_height,
            drag.thumb_height,
            drag.max_scroll_y,
        );

        let mut offset = self.scroll_handle.offset();
        offset.y = -px(scroll_y);
        self.scroll_handle.set_offset(offset);
        self.reveal_scrollbar(cx);
        cx.notify();
    }

    /// 结束滚动条拖拽会话。
    pub(crate) fn end_scrollbar_drag(&mut self, cx: &mut Context<Self>) {
        if self.scrollbar_drag.take().is_some() {
            self.reveal_scrollbar(cx);
            cx.notify();
        }
    }

    /// 聚焦表格中指定位置的单元格。
    pub(super) fn focus_table_cell_position(
        &mut self,
        table_block: &Entity<super::Block>,
        position: CellPosition,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(cell) = table_block
            .read(cx)
            .table_runtime
            .as_ref()
            .and_then(|runtime| runtime.cell(position))
        else {
            return false;
        };
        self.focus_block(cell.entity_id());
        cx.notify();
        true
    }

    /// 键盘从相邻块进入表格时聚焦入口单元格：从上进入落到
    /// 首个表头单元格，从下进入落到末个正文行首格，无正文行
    /// 时回落到表头。
    fn focus_table_entry_cell(
        &mut self,
        table_block: &Entity<super::Block>,
        from_top: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(runtime) = table_block.read(cx).table_runtime.clone() else {
            return false;
        };
        let cell = if from_top {
            runtime.header.first().cloned()
        } else {
            runtime
                .rows
                .last()
                .and_then(|row| row.first())
                .cloned()
                .or_else(|| runtime.header.first().cloned())
        };
        let Some(cell) = cell else {
            return false;
        };
        self.focus_block(cell.entity_id());
        cx.notify();
        true
    }

    /// 把焦点从表格边缘移到紧邻的上（delta < 0）下（delta > 0）块，
    /// 行为与普通块光标离首末行时转移焦点一致。相邻为表格时直接
    /// 进入其单元格，避免落在不可编辑的表格容器上。`to_block_start`
    /// 为真则落到相邻块行首（块上下语义），否则落到最近边缘
    /// （上下移动语义）。
    fn focus_block_adjacent_to_table(
        &mut self,
        table_block: &Entity<super::Block>,
        delta: i32,
        to_block_start: bool,
        cx: &mut Context<Self>,
    ) {
        let visible = self.document.flatten_visible_blocks();
        let Some(index) = visible
            .iter()
            .position(|visible| visible.entity.entity_id() == table_block.entity_id())
        else {
            return;
        };
        let target_index = if delta < 0 {
            index.checked_sub(1)
        } else {
            Some(index + 1)
        };
        let Some(target) = target_index
            .and_then(|target_index| visible.get(target_index))
            .map(|visible| visible.entity.clone())
        else {
            return;
        };
        if target.read(cx).kind() == BlockKind::Table
            && self.focus_table_entry_cell(&target, delta > 0, cx)
        {
            return;
        }
        self.focus_block(target.entity_id());
        if to_block_start {
            target.update(cx, |target, cx| target.move_to(0, cx));
        } else {
            let prefer_last_line = delta < 0;
            let offset = target
                .read(cx)
                .vertical_focus_offset(prefer_last_line, None);
            target.update(cx, move |target, cx| {
                target.move_to_with_preferred_x(offset, None, cx);
            });
        }
        cx.notify();
    }

    /// 聚焦同行左右相邻的表格单元格。
    fn focus_table_cell_horizontal_neighbor(
        &mut self,
        table_block: &Entity<super::Block>,
        position: CellPosition,
        delta: i32,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = table_block.read(cx).table_runtime.clone() else {
            return;
        };
        let columns = runtime.header.len();
        let total_rows = 1 + runtime.rows.len();
        if columns == 0 || total_rows == 0 {
            return;
        }

        let linear = position.row * columns + position.column;
        let next = if delta < 0 {
            linear.checked_sub(delta.unsigned_abs() as usize)
        } else {
            linear.checked_add(delta as usize)
        };
        let Some(next) = next else {
            return;
        };
        if next >= total_rows * columns {
            return;
        }

        let next_position = CellPosition {
            row: next / columns,
            column: next % columns,
        };
        let _ = self.focus_table_cell_position(table_block, next_position, cx);
    }

    /// 聚焦上下相邻的表格单元格，越界则离表到相邻块。
    fn focus_table_cell_vertical_neighbor(
        &mut self,
        table_block: &Entity<super::Block>,
        position: CellPosition,
        delta: i32,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = table_block.read(cx).table_runtime.clone() else {
            return;
        };
        let max_row = runtime.rows.len();
        let next_row = if delta < 0 {
            position.row.checked_sub(delta.unsigned_abs() as usize)
        } else {
            position.row.checked_add(delta as usize)
        };
        // 移出首末行则离开表格到相邻块，而不是停在边缘。
        let Some(next_row) = next_row.filter(|row| *row <= max_row) else {
            self.focus_block_adjacent_to_table(table_block, delta, false, cx);
            return;
        };

        let next_position = CellPosition {
            row: next_row,
            column: position.column.min(runtime.header.len().saturating_sub(1)),
        };
        let _ = self.focus_table_cell_position(table_block, next_position, cx);
    }

    /// 处理表格单元格块发出的块事件。
    fn on_table_cell_event(
        &mut self,
        binding: super::CellBinding,
        event: &BlockEvent,
        cx: &mut Context<Self>,
    ) {
        if Self::block_event_clears_cross_block_selection(event) {
            self.rendered_select_all_cycle = None;
            self.clear_cross_block_selection(cx);
        }

        match event {
            BlockEvent::Changed => {
                self.sync_table_record_from_runtime(&binding.table_block, cx);
                self.rebuild_image_runtimes(cx);
                self.mark_dirty(cx);
                self.request_active_block_scroll_into_view(cx);
                self.finalize_pending_undo_capture(cx);
            }
            BlockEvent::RequestOpenLink {
                prompt_target,
                open_target,
            } => {
                self.request_open_link_prompt(prompt_target.clone(), open_target.clone(), cx);
            }
            BlockEvent::RequestJumpToFootnoteDefinition { id, .. } => {
                let _ = self.jump_to_footnote_definition(id, cx);
            }
            BlockEvent::RequestJumpToFootnoteBackref { id } => {
                let _ = self.jump_to_footnote_backref(id, cx);
            }
            BlockEvent::RequestTableCellMoveHorizontal { delta } => {
                self.focus_table_cell_horizontal_neighbor(
                    &binding.table_block,
                    binding.position,
                    *delta,
                    cx,
                );
            }
            BlockEvent::RequestTableCellMoveVertical { delta } => {
                self.focus_table_cell_vertical_neighbor(
                    &binding.table_block,
                    binding.position,
                    *delta,
                    cx,
                );
            }
            BlockEvent::RequestNewline { .. } => {
                let Some(location) = self
                    .document
                    .find_block_location(binding.table_block.entity_id())
                else {
                    return;
                };
                self.clear_table_axis_preview(cx);
                self.clear_table_axis_selection(cx);
                self.sync_table_record_from_runtime(&binding.table_block, cx);
                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
                let new_block = Self::new_block(cx, BlockRecord::paragraph(String::new()));
                self.document.insert_blocks_at(
                    location.parent,
                    location.index + 1,
                    vec![new_block.clone()],
                    cx,
                );
                self.rebuild_image_runtimes(cx);
                self.focus_block(new_block.entity_id());
                self.mark_dirty(cx);
                self.request_active_block_scroll_into_view(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestFocus => {
                self.clear_table_axis_preview(cx);
                self.clear_table_axis_selection(cx);
                self.focus_block(binding.cell.entity_id());
                cx.notify();
            }
            BlockEvent::RequestFocusPrev { .. } => {
                self.focus_table_cell_vertical_neighbor(
                    &binding.table_block,
                    binding.position,
                    -1,
                    cx,
                );
            }
            BlockEvent::RequestFocusNext { .. } => {
                self.focus_table_cell_vertical_neighbor(
                    &binding.table_block,
                    binding.position,
                    1,
                    cx,
                );
            }
            // 块上下把表格视为整体：直接离表到上下块，而不按单元格步进。
            BlockEvent::RequestBlockUp => {
                self.focus_block_adjacent_to_table(&binding.table_block, -1, true, cx);
            }
            BlockEvent::RequestBlockDown => {
                self.focus_block_adjacent_to_table(&binding.table_block, 1, true, cx);
            }
            _ => {}
        }
    }

    /// 查找最近的引用容器祖先块。
    fn nearest_quote_ancestor(
        &self,
        entity_id: EntityId,
        cx: &App,
    ) -> Option<Entity<super::Block>> {
        let mut current = self.focusable_entity_by_id(entity_id)?;
        loop {
            if current.read(cx).kind().is_quote_container() {
                return Some(current);
            }
            let location = self.document.find_block_location(current.entity_id())?;
            current = location.parent?;
        }
    }

    /// 查找最顶层的引用容器祖先块。
    fn topmost_quote_ancestor(
        &self,
        entity_id: EntityId,
        cx: &App,
    ) -> Option<Entity<super::Block>> {
        let mut current = self.nearest_quote_ancestor(entity_id, cx)?;
        loop {
            let Some(location) = self.document.find_block_location(current.entity_id()) else {
                break;
            };
            let Some(parent) = location.parent.clone() else {
                break;
            };
            if !parent.read(cx).kind().is_quote_container() {
                break;
            }
            current = parent;
        }
        Some(current)
    }

    /// 计算引用打断后的新块插入位置。
    fn quote_break_insertion_target(
        &self,
        entity_id: EntityId,
        cx: &App,
    ) -> Option<(Option<Entity<super::Block>>, usize)> {
        let quote_block = self.nearest_quote_ancestor(entity_id, cx)?;
        let location = self.document.find_block_location(quote_block.entity_id())?;
        Some((location.parent.clone(), location.index + 1))
    }

    /// 计算提示框打断后的新块插入位置。
    fn callout_break_insertion_target(
        &self,
        entity_id: EntityId,
        cx: &App,
    ) -> Option<(Option<Entity<super::Block>>, usize)> {
        let callout_root = self.topmost_quote_ancestor(entity_id, cx)?;
        let location = self
            .document
            .find_block_location(callout_root.entity_id())?;
        Some((location.parent.clone(), location.index + 1))
    }

    /// 确保提示框有正文入口块，无则新建。
    fn ensure_callout_body_entry(
        &mut self,
        callout: &Entity<super::Block>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<super::Block>> {
        if !matches!(callout.read(cx).kind(), BlockKind::Callout(_)) {
            return None;
        }

        if let Some(first_child) = callout.read(cx).children.first().cloned() {
            return Some(first_child);
        }

        let body = Self::new_block(cx, BlockRecord::paragraph(String::new()));
        self.document
            .insert_blocks_at(Some(callout.clone()), 0, vec![body.clone()], cx);
        Some(body)
    }

    /// 空引用输入提示框快捷写法时转为提示框并返回正文块标识。
    fn materialize_empty_callout_shortcut(
        &mut self,
        block: &Entity<super::Block>,
        cx: &mut Context<Self>,
    ) -> Option<EntityId> {
        if self.view_mode != super::ViewMode::Rendered {
            return None;
        }

        let (kind, title_to_markdown, has_children) = block.read_with(cx, |block, _cx| {
            (
                block.kind(),
                block.record.title.serialize_markdown(),
                !block.children.is_empty(),
            )
        });
        if kind != BlockKind::Quote || has_children {
            return None;
        }

        let (variant, title) =
            crate::components::CalloutVariant::parse_heading(&title_to_markdown)?;

        block.update(cx, |block, cx| {
            block.record.kind = BlockKind::Callout(variant);
            block
                .record
                .set_title(InlineTextTree::from_markdown(&title));
            block.sync_edit_mode_from_kind();
            block.refresh_render_cache();
            block.cursor_blink_epoch = Instant::now();
            cx.notify();
        });
        let body = self.ensure_callout_body_entry(block, cx)?;
        Some(body.entity_id())
    }

    /// 空提示框正文删除时降级回引用，消费删除时返回真。
    fn downgrade_empty_callout_body_to_quote(
        &mut self,
        block: &Entity<super::Block>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(location) = self.document.find_block_location(block.entity_id()) else {
            return false;
        };
        let Some(parent) = location.parent.clone() else {
            return false;
        };

        let (heading_markdown, only_child, block_is_empty_leaf) = {
            let parent_ref = parent.read(cx);
            let Some(variant) = parent_ref.kind().callout_variant() else {
                return false;
            };
            let block_ref = block.read(cx);
            (
                variant.heading_markdown(&parent_ref.record.title.serialize_markdown()),
                parent_ref.children.len() == 1,
                block_ref.kind() == BlockKind::Paragraph
                    && block_ref.rendered_text().is_empty()
                    && block_ref.children.is_empty(),
            )
        };
        if !only_child || !block_is_empty_leaf {
            return false;
        }

        self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
        self.document.with_structure_mutation(cx, |document, cx| {
            let _ = document.remove_block_by_id_raw(block.entity_id(), cx);
            parent.update(cx, |parent, cx| {
                parent.record.kind = BlockKind::Quote;
                parent
                    .record
                    .set_title(InlineTextTree::from_markdown(&heading_markdown));
                parent.sync_edit_mode_from_kind();
                parent.refresh_render_cache();
                parent.assign_collapsed_selection_offset(0, CaretAffinity::Default, None);
                parent.marked_range = None;
                parent.cursor_blink_epoch = Instant::now();
                cx.notify();
            });
        });
        self.focus_block(parent.entity_id());
        self.rebuild_image_runtimes(cx);
        self.mark_dirty(cx);
        self.finalize_pending_undo_capture(cx);
        cx.notify();
        true
    }

    /// 按当前可见顺序快照处理所有块来源的编辑器事件。
    pub(crate) fn on_block_event(
        &mut self,
        block: Entity<super::Block>,
        event: &BlockEvent,
        cx: &mut Context<Self>,
    ) {
        if let BlockEvent::PrepareUndo { kind } = event {
            self.prepare_undo_capture_from_stable_snapshot(*kind);
            return;
        }

        if let BlockEvent::RequestReplaceCrossBlockSelection {
            text,
            selected_range_relative,
            mark_inserted_text,
            undo_kind,
        } = event
            && self.replace_cross_block_selection_with_text(
                text,
                selected_range_relative.clone(),
                *mark_inserted_text,
                *undo_kind,
                cx,
            )
        {
            return;
        }

        if matches!(event, BlockEvent::RequestRenderedSelectAll) {
            self.on_rendered_select_all_press(block, cx);
            return;
        }

        if let BlockEvent::RequestPasteImage {
            leading,
            source,
            trailing,
        } = event
        {
            self.handle_paste_image_request(block, leading, source, trailing, cx);
            return;
        }

        if let Some(binding) = self.table_cell_binding(block.entity_id()) {
            self.on_table_cell_event(binding, event, cx);
            return;
        }

        if Self::block_event_clears_cross_block_selection(event) {
            self.rendered_select_all_cycle = None;
            self.clear_cross_block_selection(cx);
        }

        let visible_before = self.document.flatten_visible_blocks();
        let current_visible_index = visible_before
            .iter()
            .position(|visible| visible.entity.entity_id() == block.entity_id())
            .unwrap_or(0);

        match event {
            BlockEvent::Changed => {
                let should_restart_numbered_list =
                    block.update(cx, |block, _cx| block.take_numbered_list_restart());
                if should_restart_numbered_list {
                    self.insert_list_group_separator_before(block.entity_id(), cx);
                }

                let callout_focus_target = self.materialize_empty_callout_shortcut(&block, cx);

                let should_normalize_quote =
                    block.update(cx, |block, _cx| {
                        let requested = block.take_quote_reparse();
                        requested && block.marked_range.is_none()
                    }) || Self::rendered_quote_text_requires_reparse(&block, cx);

                self.refresh_rendered_quote_metadata_if_needed(&block, cx);
                if should_normalize_quote {
                    self.normalize_rendered_quote_structure(cx);
                } else {
                    self.rebuild_image_runtimes(cx);
                }
                if let Some(focus_id) = callout_focus_target {
                    self.focus_block(focus_id);
                }
                self.mark_dirty(cx);
                self.request_active_block_scroll_into_view(cx);
                self.finalize_pending_undo_capture(cx);
            }
            BlockEvent::RequestNewline {
                trailing,
                source_already_mutated,
            } => {
                // 在段落下输入 setext 下划线（`=====`/`-----`）后回车，
                // 该段落即成标题，与导入器处理相邻两行的逻辑一致。
                if self.try_form_setext_heading_on_newline(&block, cx) {
                    return;
                }
                // 在表头下输入分隔行则组表，表格下继续输入管道行则并入表。
                if self.try_form_or_extend_table_on_newline(&block, cx) {
                    return;
                }
                let Some(location) = self.document.find_block_location(block.entity_id()) else {
                    return;
                };
                if !source_already_mutated {
                    self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
                }
                let current_kind = block.read(cx).kind();
                let new_block = Self::new_block(
                    cx,
                    BlockRecord::new(current_kind.sibling_kind_after_newline(), trailing.clone()),
                );
                if self.view_mode == super::ViewMode::Source {
                    new_block.update(cx, |block, _cx| block.enable_source_document_mode());
                }
                self.document.insert_blocks_at(
                    location.parent,
                    location.index + 1,
                    vec![new_block.clone()],
                    cx,
                );
                self.rebuild_image_runtimes(cx);
                self.focus_block(new_block.entity_id());
                if current_kind.is_quote_container() {
                    self.normalize_rendered_quote_structure(cx);
                }
                self.mark_dirty(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestEnterCalloutBody => {
                let needs_body = block.read(cx).children.is_empty();
                if needs_body {
                    self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
                }
                let created = self.ensure_callout_body_entry(&block, cx);
                if let Some(body) = created {
                    self.focus_block(body.entity_id());
                    self.rebuild_image_runtimes(cx);
                    if needs_body {
                        self.mark_dirty(cx);
                        self.finalize_pending_undo_capture(cx);
                    }
                    cx.notify();
                }
            }
            BlockEvent::RequestQuoteBreak => {
                let Some((parent, insert_index)) =
                    self.quote_break_insertion_target(block.entity_id(), cx)
                else {
                    return;
                };

                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);

                let new_quote = Self::new_block(
                    cx,
                    BlockRecord::new(BlockKind::Quote, InlineTextTree::plain(String::new())),
                );
                let blocks = if parent.is_none() {
                    vec![new_quote.clone()]
                } else {
                    vec![
                        Self::new_block(cx, BlockRecord::paragraph(String::new())),
                        new_quote.clone(),
                    ]
                };
                self.document
                    .insert_blocks_at(parent, insert_index, blocks, cx);
                self.focus_block(new_quote.entity_id());
                self.normalize_rendered_quote_structure(cx);
                self.mark_dirty(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestCalloutBreak => {
                let Some((parent, insert_index)) =
                    self.callout_break_insertion_target(block.entity_id(), cx)
                else {
                    return;
                };

                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
                let plain = Self::new_block(cx, BlockRecord::paragraph(String::new()));
                let blocks = if parent.is_none() {
                    vec![plain.clone()]
                } else {
                    vec![
                        Self::new_block(cx, BlockRecord::paragraph(String::new())),
                        plain.clone(),
                    ]
                };
                self.document
                    .insert_blocks_at(parent, insert_index, blocks, cx);
                self.focus_block(plain.entity_id());
                self.rebuild_image_runtimes(cx);
                self.mark_dirty(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestMergeIntoPrev { content } => {
                if current_visible_index == 0 {
                    return;
                }
                let prev = visible_before[current_visible_index - 1].entity.clone();
                let quote_related = self.block_is_quote_structure_related(&block, cx)
                    || self.block_is_quote_structure_related(&prev, cx);
                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);

                let cursor_pos = prev.read(cx).rendered_text().len();
                let adopted_children = super::tree::DocumentTree::take_children(&block, cx);
                let removed_entity_id = block.entity_id();

                self.document.with_structure_mutation(cx, |document, cx| {
                    prev.update(cx, {
                        let content = content.clone();
                        let adopted_children = adopted_children.clone();
                        move |prev, cx| {
                            let mut next_title = prev.record.title.clone();
                            next_title.push_tree(content.clone());
                            prev.record.set_title(next_title);
                            prev.refresh_render_cache();
                            prev.children.extend(adopted_children.clone());
                            prev.selected_range = cursor_pos..cursor_pos;
                            prev.selection_reversed = false;
                            prev.marked_range = None;
                            prev.vertical_motion_x = None;
                            prev.cursor_blink_epoch = Instant::now();
                            cx.notify();
                        }
                    });
                    let _ = document.remove_block_by_id_raw(removed_entity_id, cx);
                });

                self.focus_block(prev.entity_id());
                if quote_related {
                    self.normalize_rendered_quote_structure(cx);
                } else {
                    self.rebuild_image_runtimes(cx);
                }
                self.mark_dirty(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestPasteMultiline {
                leading,
                lines,
                trailing,
                split_physical_lines,
            } => {
                if lines.is_empty() {
                    return;
                }
                let quote_related = self.block_is_quote_structure_related(&block, cx);
                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);

                let current_kind = block.read(cx).kind();
                // 结构化 Markdown（表格、围栏、容器）必须按整块解析。
                // 纯文本路径会把首个粘贴行并入当前段落，从而剥掉表格表头行，
                // 因此结构化粘贴把所有行交给导入器，并保留光标前的文本。
                let structural = !*split_physical_lines;
                let leading_empty = leading.plain_len() == 0;
                let (mut first_title, tail_lines) = if structural {
                    (leading.clone(), lines.clone())
                } else {
                    let mut first_title = leading.clone();
                    first_title.push_tree(InlineTextTree::from_markdown(&lines[0]));
                    (first_title, lines[1..].to_vec())
                };
                if tail_lines.is_empty() {
                    first_title.push_tree(trailing.clone());
                    let cursor = first_title.plain_len();
                    Self::set_block_title_and_kind(&block, current_kind, first_title, cursor, cx);
                    self.focus_block(block.entity_id());
                    if quote_related {
                        self.normalize_rendered_quote_structure(cx);
                    } else {
                        self.rebuild_image_runtimes(cx);
                    }
                    self.mark_dirty(cx);
                    self.finalize_pending_undo_capture(cx);
                    cx.notify();
                    return;
                }

                let cursor = first_title.plain_len();
                Self::set_block_title_and_kind(&block, current_kind, first_title, cursor, cx);

                let Some(location) = self.document.find_block_location(block.entity_id()) else {
                    return;
                };

                // 物理行粘贴用于纯渲染文本片段。若分类器看到结构化
                // Markdown，则把尾部交给常规导入器，使表格、围栏与容器
                // 保持完整而不退化为段落。
                let mut inserted_roots = if *split_physical_lines {
                    Self::build_plain_paste_blocks_from_lines(cx, &tail_lines)
                } else {
                    Self::build_blocks_from_lines(cx, &tail_lines)
                };
                if structural && trailing.plain_len() > 0 {
                    inserted_roots.push(Self::new_block(cx, BlockRecord::paragraph(String::new())));
                }
                self.document.insert_blocks_at(
                    location.parent,
                    location.index + 1,
                    inserted_roots.clone(),
                    cx,
                );
                self.rebuild_table_runtimes(cx);

                // 粘贴在文档末尾的结构块下方无行；先记住位置，
                // 待粘贴（及引用规范化）落定后再补尾段落。
                let inserted_at_doc_end = inserted_roots.last().is_some_and(|last| {
                    self.document
                        .find_block_location(last.entity_id())
                        .is_some_and(|location| {
                            location.parent.is_none()
                                && location.index + 1 >= self.document.root_count()
                        })
                });

                if let Some(last_root) = inserted_roots.last() {
                    let focus_block = if last_root.read(cx).kind() == BlockKind::Table {
                        last_root
                            .read(cx)
                            .table_runtime
                            .as_ref()
                            .and_then(|runtime| {
                                runtime
                                    .rows
                                    .last()
                                    .and_then(|row| row.last())
                                    .cloned()
                                    .or_else(|| runtime.header.last().cloned())
                            })
                    } else {
                        self.document.last_visible_descendant(last_root.entity_id())
                    };
                    let Some(focus_block) = focus_block else {
                        return;
                    };
                    focus_block.update(cx, {
                        let trailing = trailing.clone();
                        move |focus_block, cx| {
                            let mut next_title = focus_block.record.title.clone();
                            next_title.push_tree(trailing.clone());
                            focus_block.record.set_title(next_title);
                            focus_block.refresh_render_cache();
                            focus_block.cursor_blink_epoch = Instant::now();
                            cx.notify();
                        }
                    });
                    let cursor = focus_block.read(cx).rendered_text().len();
                    Self::reset_block_cursor(&focus_block, cursor, cx);
                    self.rebuild_image_runtimes(cx);
                    if let Some(binding) = self.table_cell_binding(focus_block.entity_id()) {
                        self.sync_table_record_from_runtime(&binding.table_block, cx);
                    }
                    self.focus_block(focus_block.entity_id());
                }

                // 结构化内容粘贴到空行时没有光标前文本可保留，因此删掉
                // 已变空的段落，避免粘贴块上方留下空行。
                if structural && leading_empty {
                    self.document.with_structure_mutation(cx, |document, cx| {
                        document.remove_block_by_id_raw(block.entity_id(), cx);
                    });
                }

                if quote_related {
                    self.normalize_rendered_quote_structure(cx);
                }

                // 引用规范化会从 Markdown 重建根，因此从实时树
                // 解析落点块，而不用粘贴时的旧句柄。
                if inserted_at_doc_end
                    && let Some(last_root) = self.document.root_blocks().last().cloned()
                {
                    self.ensure_trailing_paragraph_after_structural(&last_root, cx);
                }
                self.mark_dirty(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestPasteImage { .. }
            | BlockEvent::RequestReplaceCrossBlockSelection { .. } => {}
            BlockEvent::RequestIndent => {
                if current_visible_index == 0 {
                    return;
                }

                let Some(location) = self.document.find_block_location(block.entity_id()) else {
                    return;
                };
                let current_kind = block.read(cx).kind();
                let target_parent = visible_before[current_visible_index - 1].entity.clone();
                if !current_kind.nests_under(&target_parent.read(cx).kind()) {
                    return;
                }
                if location
                    .parent
                    .as_ref()
                    .is_some_and(|parent| parent.entity_id() == target_parent.entity_id())
                {
                    return;
                }
                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);

                let moved = self.document.with_structure_mutation(cx, |document, cx| {
                    let moved = document.remove_block_by_id_raw(block.entity_id(), cx)?.0;
                    let child_index = target_parent.read(cx).children.len();
                    document.insert_blocks_at_raw(
                        Some(target_parent.clone()),
                        child_index,
                        vec![moved.clone()],
                        cx,
                    );
                    Some(moved)
                });

                let Some(moved) = moved else {
                    return;
                };

                self.focus_block(moved.entity_id());
                self.mark_dirty(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestOutdent => {
                let Some(location) = self.document.find_block_location(block.entity_id()) else {
                    return;
                };
                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);

                if let Some(parent) = location.parent.clone() {
                    let Some(parent_location) =
                        self.document.find_block_location(parent.entity_id())
                    else {
                        return;
                    };

                    let moved = self.document.with_structure_mutation(cx, |document, cx| {
                        let moved = document.remove_block_by_id_raw(block.entity_id(), cx)?.0;
                        document.insert_blocks_at_raw(
                            parent_location.parent,
                            parent_location.index + 1,
                            vec![moved.clone()],
                            cx,
                        );
                        Some(moved)
                    });

                    let Some(moved) = moved else {
                        return;
                    };
                    self.focus_block(moved.entity_id());
                } else {
                    block.update(cx, |block, cx| block.make_paragraph(cx));
                    self.focus_block(block.entity_id());
                }

                self.mark_dirty(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestDowngradeNestedListItemToChildParagraph => {
                let Some(location) = self.document.find_block_location(block.entity_id()) else {
                    return;
                };
                let Some(parent) = location.parent.clone() else {
                    return;
                };
                if !block.read(cx).kind().is_list_item() || !parent.read(cx).kind().is_list_item() {
                    return;
                }

                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);

                let downgraded = self.document.with_structure_mutation(cx, |document, cx| {
                    let (moved, removed_location) =
                        document.remove_block_by_id_raw(block.entity_id(), cx)?;
                    moved.update(cx, |block, cx| {
                        block.record.kind = BlockKind::Paragraph;
                        block.record.raw_fallback = None;
                        block.sync_edit_mode_from_kind();
                        block.refresh_render_cache();
                        block.cursor_blink_epoch = Instant::now();
                        cx.notify();
                    });
                    document.insert_blocks_at_raw(
                        Some(parent.clone()),
                        removed_location.index,
                        vec![moved.clone()],
                        cx,
                    );
                    Some(moved)
                });

                let Some(downgraded) = downgraded else {
                    return;
                };

                self.focus_block(downgraded.entity_id());
                self.rebuild_image_runtimes(cx);
                self.mark_dirty(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::ToggleTaskChecked => {
                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
                block.update(cx, |block, cx| {
                    let checked = match block.kind() {
                        BlockKind::TaskListItem { checked } => checked,
                        _ => return,
                    };
                    block.record.kind = BlockKind::TaskListItem { checked: !checked };
                    block.sync_edit_mode_from_kind();
                    block.refresh_render_cache();
                    block.cursor_blink_epoch = Instant::now();
                    cx.notify();
                });
                self.mark_dirty(cx);
                self.request_active_block_scroll_into_view(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestOpenLink {
                prompt_target,
                open_target,
            } => {
                self.request_open_link_prompt(prompt_target.clone(), open_target.clone(), cx);
            }
            BlockEvent::RequestJumpToFootnoteDefinition { id, .. } => {
                let _ = self.jump_to_footnote_definition(id, cx);
                cx.notify();
            }
            BlockEvent::RequestJumpToFootnoteBackref { id } => {
                let _ = self.jump_to_footnote_backref(id, cx);
                cx.notify();
            }
            BlockEvent::RequestAppendTableColumn => {
                if block.read(cx).kind() == BlockKind::Table {
                    // 表格框上的按下会先建立整块选区；追加行列属于表格内部操作，先清掉。
                    self.clear_cross_block_selection(cx);
                    self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
                    self.append_table_column(&block, cx);
                    self.finalize_pending_undo_capture(cx);
                }
            }
            BlockEvent::RequestAppendTableRow => {
                if block.read(cx).kind() == BlockKind::Table {
                    self.clear_cross_block_selection(cx);
                    self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
                    self.append_table_row(&block, cx);
                    self.finalize_pending_undo_capture(cx);
                }
            }
            BlockEvent::RequestTableAxisPreview {
                kind,
                index,
                hovered,
            } => {
                if block.read(cx).kind() == BlockKind::Table {
                    self.preview_table_axis(block.entity_id(), *kind, *index, *hovered, cx);
                }
            }
            BlockEvent::RequestSelectTableAxis { kind, index } => {
                if block.read(cx).kind() == BlockKind::Table {
                    // 轴带位于表格框内，捕获阶段可能已建立整块选区；选中行列时清掉。
                    self.clear_cross_block_selection(cx);
                    self.select_table_axis(block.entity_id(), *kind, *index, cx);
                }
            }
            BlockEvent::RequestTableAxisOp { kind, index, op } => {
                if block.read(cx).kind() == BlockKind::Table {
                    self.apply_table_axis_op(&block, *kind, *index, *op, cx);
                }
            }
            BlockEvent::RequestTableCellMoveHorizontal { .. }
            | BlockEvent::RequestTableCellMoveVertical { .. } => {}
            BlockEvent::RequestFocusPrev { preferred_x } => {
                if current_visible_index == 0 {
                    return;
                }

                let target = visible_before[current_visible_index - 1].entity.clone();
                // 从下方进入表格时落在正文单元格，而非不可编辑的表格容器。
                if target.read(cx).kind() == BlockKind::Table
                    && self.focus_table_entry_cell(&target, false, cx)
                {
                    return;
                }
                let target_x = preferred_x.map(px);
                let offset = target.read(cx).vertical_focus_offset(true, target_x);
                self.focus_block(target.entity_id());
                target.update(cx, move |target, cx| {
                    target.move_to_with_preferred_x(offset, target_x, cx);
                });
                cx.notify();
            }
            BlockEvent::RequestFocusNext { preferred_x } => {
                if current_visible_index + 1 >= visible_before.len() {
                    // 末尾多行块（代码、公式等）下方无处可去，因此补一个
                    // 段落承接并聚焦，与末尾表格的行为一致。
                    if block.read(cx).kind().is_multiline_text_block() {
                        self.ensure_trailing_paragraph_after_structural(&block, cx);
                        let visible = self.document.flatten_visible_blocks();
                        if let Some(landing) = visible
                            .iter()
                            .position(|v| v.entity.entity_id() == block.entity_id())
                            .and_then(|index| visible.get(index + 1))
                            .map(|v| v.entity.clone())
                        {
                            self.focus_block(landing.entity_id());
                            landing.update(cx, |landing, cx| landing.move_to(0, cx));
                            cx.notify();
                        }
                    }
                    return;
                }

                let target = visible_before[current_visible_index + 1].entity.clone();
                // 从上方进入表格时落在表头单元格，而非不可编辑的表格容器。
                if target.read(cx).kind() == BlockKind::Table
                    && self.focus_table_entry_cell(&target, true, cx)
                {
                    return;
                }
                let target_x = preferred_x.map(px);
                let offset = target.read(cx).vertical_focus_offset(false, target_x);
                self.focus_block(target.entity_id());
                target.update(cx, move |target, cx| {
                    target.move_to_with_preferred_x(offset, target_x, cx);
                });
                cx.notify();
            }
            BlockEvent::RequestBlockUp => {
                if current_visible_index == 0 {
                    return;
                }

                let target = visible_before[current_visible_index - 1].entity.clone();
                if target.read(cx).kind() == BlockKind::Table
                    && self.focus_table_entry_cell(&target, false, cx)
                {
                    return;
                }
                self.focus_block(target.entity_id());
                target.update(cx, |target, cx| target.move_to(0, cx));
                cx.notify();
            }
            BlockEvent::RequestBlockDown => {
                if current_visible_index + 1 >= visible_before.len() {
                    return;
                }

                let target = visible_before[current_visible_index + 1].entity.clone();
                if target.read(cx).kind() == BlockKind::Table
                    && self.focus_table_entry_cell(&target, true, cx)
                {
                    return;
                }
                self.focus_block(target.entity_id());
                target.update(cx, |target, cx| target.move_to(0, cx));
                cx.notify();
            }
            BlockEvent::RequestDelete => {
                if self.downgrade_empty_callout_body_to_quote(&block, cx) {
                    return;
                }
                let quote_related = self.block_is_quote_structure_related(&block, cx);
                let is_last_visible_leaf =
                    visible_before.len() == 1 && block.read(cx).children.is_empty();
                if is_last_visible_leaf {
                    if block.read(cx).kind() == BlockKind::Paragraph {
                        Self::reset_block_cursor(&block, 0, cx);
                    } else {
                        block.update(cx, |block, cx| block.make_paragraph(cx));
                    }
                    self.focus_block(block.entity_id());
                    cx.notify();
                    return;
                }
                self.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);

                let visible_before_ids = visible_before
                    .iter()
                    .map(|visible| visible.entity.entity_id())
                    .collect::<Vec<_>>();
                let focus_candidate = if current_visible_index > 0 {
                    Some(visible_before_ids[current_visible_index - 1])
                } else {
                    visible_before_ids.get(current_visible_index + 1).copied()
                };

                let adopted_children = super::tree::DocumentTree::take_children(&block, cx);
                let removed = self.document.with_structure_mutation(cx, |document, cx| {
                    let (_, location) = document.remove_block_by_id_raw(block.entity_id(), cx)?;
                    if !adopted_children.is_empty() {
                        document.insert_blocks_at_raw(
                            location.parent.clone(),
                            location.index,
                            adopted_children.clone(),
                            cx,
                        );
                    }
                    Some(location)
                });

                if removed.is_none() {
                    return;
                }

                if let Some(focus_id) = focus_candidate {
                    self.focus_block(focus_id);
                } else if let Some(first_root) = self.document.first_root() {
                    self.focus_block(first_root.entity_id());
                }

                if quote_related {
                    self.normalize_rendered_quote_structure(cx);
                }
                self.mark_dirty(cx);
                self.finalize_pending_undo_capture(cx);
                cx.notify();
            }
            BlockEvent::RequestFocus => {
                self.clear_table_axis_preview(cx);
                self.clear_table_axis_selection(cx);
                self.focus_block(block.entity_id());
                for visible in self.document.flatten_visible_blocks() {
                    visible.entity.update(cx, |_, cx| cx.notify());
                }
                cx.notify();
            }
            BlockEvent::RequestRenderedSelectAll => {}
            BlockEvent::PrepareUndo { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;
    use crate::components::{
        Block, BlockEvent, BlockKind, BlockRecord, CalloutVariant, Delete, DeleteBack,
        ExitCodeBlock, InlineTextTree, Newline,
    };
    use rgpui::{App, AppContext, Entity, TestAppContext};

    #[rgpui::test]
    async fn request_quote_break_creates_new_root_leaf_quote_group(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> first".to_string(), None));

        editor.update(cx, |editor, cx| {
            let quote = editor.document.first_root().expect("root quote").clone();
            editor.on_block_event(quote, &BlockEvent::RequestQuoteBreak, cx);

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "first");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "> first\n\n> ");
            assert_eq!(editor.pending_focus, Some(visible[1].entity.entity_id()));
        });
    }

    #[rgpui::test]
    async fn typing_quote_shortcut_immediately_refreshes_rendered_quote_metadata(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let paragraph = editor
                .document
                .first_root()
                .expect("root paragraph")
                .clone();
            paragraph.update(cx, |block, cx| {
                block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
                block.replace_text_in_visible_range(0..0, "> ", None, false, cx);
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[0].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "> ");
        });
    }

    #[rgpui::test]
    async fn footnote_reference_jump_and_backref_follow_in_place_definition(
        cx: &mut TestAppContext,
    ) {
        let markdown = "alpha[^note]\n\n[^note]: Footnote body".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let paragraph = editor
                .document
                .first_root()
                .expect("reference paragraph")
                .clone();
            let definition = editor
                .document
                .visible_blocks()
                .iter()
                .find(|visible| visible.entity.read(cx).kind() == BlockKind::FootnoteDefinition)
                .expect("footnote definition block")
                .entity
                .clone();

            editor.on_block_event(
                paragraph.clone(),
                &BlockEvent::RequestJumpToFootnoteDefinition {
                    id: "note".to_string(),
                },
                cx,
            );
            assert_eq!(editor.pending_focus, Some(definition.entity_id()));
            assert_eq!(definition.read(cx).selected_range, 0..0);

            let expected_backref_range = paragraph
                .read(cx)
                .current_range_for_footnote_occurrence(0)
                .expect("resolved footnote occurrence");
            editor.on_block_event(
                definition.clone(),
                &BlockEvent::RequestJumpToFootnoteBackref {
                    id: "note".to_string(),
                },
                cx,
            );
            assert_eq!(editor.pending_focus, Some(paragraph.entity_id()));
            assert_eq!(paragraph.read(cx).selected_range, expected_backref_range);
        });
    }

    #[rgpui::test]
    async fn image_block_insert_preserves_surrounding_paragraph_text(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "beforeafter".to_string(), None));

        editor.update(cx, |editor, cx| {
            let paragraph = editor.document.first_root().expect("paragraph").clone();
            editor.insert_image_block_after_paragraph(
                &paragraph,
                &InlineTextTree::plain("before"),
                "![image](./assets/image.png)",
                &InlineTextTree::plain("after"),
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "before");
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "![image](./assets/image.png)"
            );
            assert!(visible[1].entity.read(cx).image_runtime().is_some());
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "after");
        });
    }

    #[rgpui::test]
    async fn image_paste_text_in_code_block_stays_inside_block(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "```\nbeforeafter\n```".to_string(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.first_root().expect("code block").clone();
            editor.replace_current_block_selection_with_image_text(
                &block,
                &InlineTextTree::plain("before"),
                "![image](./assets/image.png)",
                &InlineTextTree::plain("after"),
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::CodeBlock { language: None }
            );
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "before![image](./assets/image.png)after"
            );
        });
    }

    #[rgpui::test]
    async fn typing_callout_shortcut_materializes_body_and_focuses_it(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let paragraph = editor
                .document
                .first_root()
                .expect("root paragraph")
                .clone();
            paragraph.update(cx, |block, cx| {
                block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
                block.replace_text_in_visible_range(0..0, "> [!NOTE]", None, false, cx);
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Note)
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "> [!NOTE]\n> ");
            assert_eq!(editor.pending_focus, Some(visible[1].entity.entity_id()));
        });
    }

    #[rgpui::test]
    async fn typing_numbered_list_shortcut_after_separator_preserves_group_boundary(
        cx: &mut TestAppContext,
    ) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "1. aa\n2. bb\n3. cc".to_string(), None));

        let separator_id = editor.update(cx, |editor, cx| {
            let separator = Editor::new_block(cx, BlockRecord::paragraph(String::new()));
            editor.document.insert_blocks_at(
                None,
                editor.document.root_count(),
                vec![separator.clone()],
                cx,
            );
            separator.entity_id()
        });

        editor.update(cx, |editor, cx| {
            let separator = editor
                .document
                .block_entity_by_id(separator_id)
                .expect("separator paragraph");
            assert!(separator.read(cx).list_group_separator_candidate);
            separator.update(cx, |block, cx| {
                block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
                block.replace_text_in_visible_range(0..0, "1. ", None, false, cx);
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 5);
            assert_eq!(visible[0].entity.read(cx).list_ordinal, Some(1));
            assert_eq!(visible[1].entity.read(cx).list_ordinal, Some(2));
            assert_eq!(visible[2].entity.read(cx).list_ordinal, Some(3));
            assert_eq!(visible[3].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[3].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[4].entity.entity_id(), separator_id);
            assert_eq!(
                visible[4].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(visible[4].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[4].entity.read(cx).list_ordinal, Some(1));
            assert_eq!(
                editor.document.markdown_text(cx),
                "1. aa\n2. bb\n3. cc\n\n1. "
            );
        });
    }

    #[rgpui::test]
    async fn request_indent_nests_non_empty_list_item(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- a\n- b".to_string(), None));

        editor.update(cx, |editor, cx| {
            let second = editor.document.visible_blocks()[1].entity.clone();
            editor.on_block_event(second, &BlockEvent::RequestIndent, cx);

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "- a\n  - b");
        });
    }

    #[rgpui::test]
    async fn request_outdent_lifts_list_child_paragraph_after_parent(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "- item\n\n  child text".to_string(), None));

        let child_id = editor.update(cx, |editor, cx| {
            let child = editor.document.visible_blocks()[1].entity.clone();
            editor.on_block_event(child.clone(), &BlockEvent::RequestOutdent, cx);
            child.entity_id()
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "item");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "child text");
            assert_eq!(visible[1].entity.read(cx).render_depth, 0);
            assert_eq!(visible[1].entity.entity_id(), child_id);
            assert_eq!(editor.document.markdown_text(cx), "- item\n\nchild text");
        });
    }

    #[rgpui::test]
    async fn empty_list_child_paragraph_backspace_outdents_to_root(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- item\n\n  child".to_string(), None));

        let child_id = editor.update(cx, |editor, _cx| {
            editor.document.visible_blocks()[1].entity.entity_id()
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let child = editor.document.visible_blocks()[1].entity.clone();
                child.update(cx, |block, block_cx| {
                    block.prepare_undo_capture(
                        crate::components::UndoKind::NonCoalescible,
                        block_cx,
                    );
                    block.replace_text_in_visible_range(
                        0..block.plain_len(),
                        "",
                        None,
                        false,
                        block_cx,
                    );
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.entity_id(), child_id);
            assert_eq!(visible[1].entity.read(cx).render_depth, 0);
            assert_eq!(editor.document.markdown_text(cx), "- item\n\n");
        });
    }

    #[rgpui::test]
    async fn empty_list_child_paragraph_enter_continues_same_level(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- item\n\n  child".to_string(), None));

        let child_id = editor.update(cx, |editor, _cx| {
            editor.document.visible_blocks()[1].entity.entity_id()
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let child = editor.document.visible_blocks()[1].entity.clone();
                child.update(cx, |block, block_cx| {
                    block.prepare_undo_capture(
                        crate::components::UndoKind::NonCoalescible,
                        block_cx,
                    );
                    block.replace_text_in_visible_range(
                        0..block.plain_len(),
                        "",
                        None,
                        false,
                        block_cx,
                    );
                    block.move_to(0, block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.entity_id(), child_id);
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[2].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "- item\n  \n  ");
        });
    }

    #[rgpui::test]
    async fn enter_inside_script_paragraph_creates_new_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "H~2~O".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "H2O");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(editor.document.markdown_text(cx), "H~2~O\n\n");
        });
    }

    #[rgpui::test]
    async fn enter_inside_inline_math_paragraph_creates_new_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "$n^2$".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "$n^2$");
            assert!(!visible[0].entity.read(cx).uses_raw_text_editing());
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(editor.document.markdown_text(cx), "$n^2$\n\n");
        });
    }

    #[rgpui::test]
    async fn trailing_fence_line_enter_closes_code_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "```rust\nlet x = 1;\n```".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    // Type a closing fence on a fresh last line, then Enter.
                    let end = block.plain_len();
                    block.replace_text_in_visible_range(end..end, "\n```", None, false, block_cx);
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::CodeBlock {
                    language: Some("rust".into())
                }
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "let x = 1;");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(
                editor.document.markdown_text(cx),
                "```rust\nlet x = 1;\n```\n\n"
            );
        });
    }

    #[rgpui::test]
    async fn setext_equals_underline_enter_promotes_previous_paragraph_to_h1(
        cx: &mut TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "Title\n\n=====".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let underline = editor.document.visible_blocks()[1].entity.clone();
                underline.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Heading { level: 1 }
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "Title");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(editor.document.markdown_text(cx), "# Title\n\n");
        });

        // Reversible: undo restores the two original paragraphs.
        editor.update(cx, |editor, cx| {
            editor.undo_document(cx);
            assert_eq!(editor.document.markdown_text(cx), "Title\n\n=====");
        });
    }

    #[rgpui::test]
    async fn setext_dash_underline_enter_promotes_previous_paragraph_to_h2(
        cx: &mut TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        // A bare "-----" in source parses as a thematic break, so simulate the
        // user typing the underline into the paragraph below the title instead.
        let editor = cx.new(|cx| Editor::from_markdown(cx, "Title\n\nx".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let underline = editor.document.visible_blocks()[1].entity.clone();
                underline.update(cx, |block, block_cx| {
                    let end = block.plain_len();
                    block.replace_text_in_visible_range(0..end, "-----", None, false, block_cx);
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Heading { level: 2 }
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "Title");
            assert_eq!(editor.document.markdown_text(cx), "## Title\n\n");
        });
    }

    #[rgpui::test]
    async fn dash_underline_without_heading_target_stays_a_separator(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    block.replace_text_in_visible_range(0..0, "-----", None, false, block_cx);
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Separator);
        });
    }

    #[rgpui::test]
    async fn equals_underline_without_heading_target_stays_a_paragraph(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    block.replace_text_in_visible_range(0..0, "=====", None, false, block_cx);
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "=====");
        });
    }

    #[rgpui::test]
    async fn delimiter_row_enter_forms_native_table(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| {
            Editor::from_markdown(cx, "| Name | Score |\n\n| --- | --- |".to_string(), None)
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let delimiter = editor.document.root_blocks()[1].clone();
                delimiter.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let roots = editor.document.root_blocks();
            assert_eq!(roots.len(), 2);
            assert_eq!(roots[0].read(cx).kind(), BlockKind::Table);
            let table = roots[0].read(cx).record.table.clone().expect("table");
            assert_eq!(table.header.len(), 2);
            assert_eq!(table.header[0].serialize_markdown(), "Name");
            assert!(table.rows.is_empty());
            assert_eq!(roots[1].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(
                editor.document.markdown_text(cx),
                "| Name | Score |\n| --- | --- |\n\n"
            );
        });

        // Reversible in one step back to the two source paragraphs.
        editor.update(cx, |editor, cx| {
            editor.undo_document(cx);
            assert_eq!(
                editor.document.markdown_text(cx),
                "| Name | Score |\n\n| --- | --- |"
            );
        });
    }

    #[rgpui::test]
    async fn pipe_row_below_table_is_absorbed_as_a_row(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| {
            Editor::from_markdown(cx, "| Name | Score |\n\n| --- | --- |".to_string(), None)
        });

        // Form the table.
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let delimiter = editor.document.root_blocks()[1].clone();
                delimiter.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        // Type a body row into the paragraph below the table and press Enter.
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let row = editor.document.root_blocks()[1].clone();
                row.update(cx, |block, block_cx| {
                    block.replace_text_in_visible_range(
                        0..0,
                        "| Alice | 10 |",
                        None,
                        false,
                        block_cx,
                    );
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let roots = editor.document.root_blocks();
            assert_eq!(roots[0].read(cx).kind(), BlockKind::Table);
            let table = roots[0].read(cx).record.table.clone().expect("table");
            assert_eq!(table.rows.len(), 1);
            assert_eq!(table.rows[0][0].serialize_markdown(), "Alice");
            assert_eq!(table.rows[0][1].serialize_markdown(), "10");
            assert_eq!(roots[1].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(roots[1].read(cx).rendered_text(), "");
        });
    }

    #[rgpui::test]
    async fn pipeless_delimiter_row_enter_forms_native_table(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "Name | Score\n\n---- | ----".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let delimiter = editor.document.root_blocks()[1].clone();
                delimiter.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let roots = editor.document.root_blocks();
            assert_eq!(roots.len(), 2);
            assert_eq!(roots[0].read(cx).kind(), BlockKind::Table);
            let table = roots[0].read(cx).record.table.clone().expect("table");
            assert_eq!(table.header.len(), 2);
            assert_eq!(table.header[0].serialize_markdown(), "Name");
            assert_eq!(table.header[1].serialize_markdown(), "Score");
            assert!(table.rows.is_empty());
            assert_eq!(roots[1].read(cx).kind(), BlockKind::Paragraph);
        });
    }

    #[rgpui::test]
    async fn pipeless_row_below_table_is_absorbed_as_a_row(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "Name | Score\n\n---- | ----".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let delimiter = editor.document.root_blocks()[1].clone();
                delimiter.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        // A pipeless body row with the table's column count is absorbed.
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let row = editor.document.root_blocks()[1].clone();
                row.update(cx, |block, block_cx| {
                    block.replace_text_in_visible_range(0..0, "Alice | 10", None, false, block_cx);
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let roots = editor.document.root_blocks();
            assert_eq!(roots[0].read(cx).kind(), BlockKind::Table);
            let table = roots[0].read(cx).record.table.clone().expect("table");
            assert_eq!(table.rows.len(), 1);
            assert_eq!(table.rows[0][0].serialize_markdown(), "Alice");
            assert_eq!(table.rows[0][1].serialize_markdown(), "10");
            assert_eq!(roots[1].read(cx).kind(), BlockKind::Paragraph);
        });
    }

    #[rgpui::test]
    async fn ragged_pipeless_row_below_table_is_padded_to_width(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx
            .new(|cx| Editor::from_markdown(cx, "A | B | C\n\n--- | --- | ---".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let delimiter = editor.document.root_blocks()[1].clone();
                delimiter.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        // Two cells typed under a three-column table: absorbed as a row and
        // padded to the header width, matching how pasted ragged rows behave.
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let row = editor.document.root_blocks()[1].clone();
                row.update(cx, |block, block_cx| {
                    block.replace_text_in_visible_range(0..0, "one | two", None, false, block_cx);
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let table = editor.document.root_blocks()[0]
                .read(cx)
                .record
                .table
                .clone()
                .expect("table");
            assert_eq!(table.rows.len(), 1);
            assert_eq!(table.rows[0].len(), 3);
            assert_eq!(table.rows[0][0].serialize_markdown(), "one");
            assert_eq!(table.rows[0][1].serialize_markdown(), "two");
            assert_eq!(table.rows[0][2].serialize_markdown(), "");
        });
    }

    #[rgpui::test]
    async fn lone_pipe_row_without_table_context_stays_a_paragraph(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.root_blocks()[0].clone();
                block.update(cx, |block, block_cx| {
                    block.replace_text_in_visible_range(0..0, "| a | b |", None, false, block_cx);
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let roots = editor.document.root_blocks();
            assert_eq!(roots[0].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(roots[0].read(cx).rendered_text(), "| a | b |");
        });
    }

    #[rgpui::test]
    async fn math_block_exit_shortcut_creates_plain_text_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "$$n^2$$".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    block.on_exit_code_block(&ExitCodeBlock, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::MathBlock);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "$$n^2$$");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(editor.document.markdown_text(cx), "$$n^2$$\n\n");
        });
    }

    #[rgpui::test]
    async fn dollar_dollar_enter_creates_editable_math_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    block.replace_text_in_visible_range(
                        0..block.plain_len(),
                        "$$",
                        None,
                        false,
                        block_cx,
                    );
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            let block = visible[0].entity.read(cx);
            assert_eq!(block.kind(), BlockKind::MathBlock);
            assert_eq!(block.rendered_text(), "$$\n\n$$");
            assert_eq!(block.selected_range, 3..3);
            assert!(block.uses_raw_text_editing());
            assert_eq!(editor.document.markdown_text(cx), "$$\n\n$$");
        });
    }

    #[rgpui::test]
    async fn dollar_dollar_prefix_then_enter_wraps_existing_line(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "E = mc^2".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    // Home, type the fence in front of the formula, then Enter.
                    block.move_to(0, block_cx);
                    block.replace_text_in_visible_range(0..0, "$$", None, false, block_cx);
                    block.move_to("$$".len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            let block = visible[0].entity.read(cx);
            assert_eq!(block.kind(), BlockKind::MathBlock);
            // The pre-existing text is kept as the formula body.
            assert_eq!(block.rendered_text(), "$$\nE = mc^2\n$$");
            assert_eq!(block.selected_range, "$$\n".len().."$$\n".len());
            assert_eq!(editor.document.markdown_text(cx), "$$\nE = mc^2\n$$");
        });
    }

    #[rgpui::test]
    async fn enter_inside_math_block_keeps_local_formula_editing(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "$$n^2$$".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    block.move_to(3, block_cx);
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::MathBlock);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "$$n\n^2$$");
            assert_eq!(editor.document.markdown_text(cx), "$$n\n^2$$");
        });
    }

    #[rgpui::test]
    async fn auto_created_math_block_exit_shortcut_creates_plain_text_block(
        cx: &mut TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let block = editor.document.visible_blocks()[0].entity.clone();
                block.update(cx, |block, block_cx| {
                    block.replace_text_in_visible_range(
                        0..block.plain_len(),
                        "$$",
                        None,
                        false,
                        block_cx,
                    );
                    block.move_to(block.plain_len(), block_cx);
                    block.on_newline(&Newline, window, block_cx);
                    block.on_exit_code_block(&ExitCodeBlock, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::MathBlock);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "$$\n\n$$");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(editor.document.markdown_text(cx), "$$\n\n$$\n\n");
        });
    }

    #[rgpui::test]
    async fn raw_like_block_exit_shortcut_creates_plain_text_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let cases = [
            (
                BlockRecord::html("<div>\ncontent\n</div>"),
                BlockKind::HtmlBlock,
                "<div>\ncontent\n</div>",
            ),
            (
                BlockRecord::mermaid("```mermaid\nflowchart LR\nA-->B\n```"),
                BlockKind::MermaidBlock,
                "```mermaid\nflowchart LR\nA-->B\n```",
            ),
            (
                BlockRecord::raw_markdown("::: custom\ncontent\n:::"),
                BlockKind::RawMarkdown,
                "::: custom\ncontent\n:::",
            ),
            (
                BlockRecord::comment("<!--\ncomment\n-->"),
                BlockKind::Comment,
                "<!--\ncomment\n-->",
            ),
        ];

        for (record, kind, text) in cases {
            let editor = cx.new(|cx| {
                let mut editor = Editor::from_markdown(cx, String::new(), None);
                let block = Editor::new_block(cx, record.clone());
                editor.document.replace_roots(vec![block], cx);
                editor
            });

            cx.update(|window, cx| {
                editor.update(cx, |editor, cx| {
                    let block = editor.document.visible_blocks()[0].entity.clone();
                    block.update(cx, |block, block_cx| {
                        block.on_exit_code_block(&ExitCodeBlock, window, block_cx);
                    });
                });
            });

            editor.update(cx, |editor, cx| {
                let visible = editor.document.visible_blocks();
                assert_eq!(visible.len(), 2);
                assert_eq!(visible[0].entity.read(cx).kind(), kind);
                assert_eq!(visible[0].entity.read(cx).rendered_text(), text);
                assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
                assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            });
        }
    }

    #[rgpui::test]
    async fn table_cell_enter_still_moves_to_next_row(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |", "| 3 | 4 |"].join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        let mut next_cell_id = None;
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let table = editor.document.first_root().expect("table root").clone();
                let (cell, expected_next_cell_id) = {
                    let table = table.read(cx);
                    let runtime = table.table_runtime.as_ref().expect("table runtime");
                    (runtime.rows[0][0].clone(), runtime.rows[1][0].entity_id())
                };
                next_cell_id = Some(expected_next_cell_id);
                cell.update(cx, |block, block_cx| {
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, _cx| {
            assert_eq!(editor.document.visible_blocks().len(), 1);
            assert_eq!(editor.pending_focus, next_cell_id);
        });
    }

    #[rgpui::test]
    async fn table_cell_exit_shortcut_inserts_sibling_after_table(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let markdown = ["> [!NOTE]", "> | A | B |", "> | --- | --- |", "> | 1 | 2 |"].join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let callout = editor.document.first_root().expect("callout root").clone();
                let table = callout
                    .read(cx)
                    .children
                    .iter()
                    .find(|child| child.read(cx).kind() == BlockKind::Table)
                    .expect("nested table")
                    .clone();
                let cell = table
                    .read(cx)
                    .table_runtime
                    .as_ref()
                    .expect("table runtime")
                    .rows[0][0]
                    .clone();
                cell.update(cx, |block, block_cx| {
                    block.on_exit_code_block(&ExitCodeBlock, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let callout = editor.document.first_root().expect("callout root").clone();
            let children = callout.read(cx).children.clone();
            assert_eq!(children.len(), 2);
            assert_eq!(children[0].read(cx).kind(), BlockKind::Table);
            assert_eq!(children[1].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(children[1].read(cx).rendered_text(), "");
            assert_eq!(editor.pending_focus, Some(children[1].entity_id()));
        });
    }

    /// 取文档中的表格根块（测试辅助）。
    fn table_root(editor: &Editor, cx: &App) -> Entity<Block> {
        editor
            .document
            .visible_blocks()
            .iter()
            .map(|visible| visible.entity.clone())
            .find(|block| block.read(cx).kind() == BlockKind::Table)
            .expect("table root")
    }

    #[rgpui::test]
    async fn arrow_down_from_last_row_exits_table_to_following_block(cx: &mut TestAppContext) {
        let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |", "", "after"].join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let table = table_root(editor, cx);
            let cell = table
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .rows
                .last()
                .and_then(|row| row.first())
                .cloned()
                .expect("last row cell");
            editor.on_block_event(
                cell,
                &BlockEvent::RequestTableCellMoveVertical { delta: 1 },
                cx,
            );

            let following = editor.document.visible_blocks()[1].entity.clone();
            assert_eq!(following.read(cx).rendered_text(), "after");
            assert_eq!(editor.pending_focus, Some(following.entity_id()));
        });
    }

    #[rgpui::test]
    async fn arrow_up_from_header_exits_table_to_preceding_block(cx: &mut TestAppContext) {
        let markdown = ["before", "", "| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let table = table_root(editor, cx);
            let cell = table
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .header
                .first()
                .cloned()
                .expect("header cell");
            editor.on_block_event(
                cell,
                &BlockEvent::RequestTableCellMoveVertical { delta: -1 },
                cx,
            );

            let preceding = editor.document.visible_blocks()[0].entity.clone();
            assert_eq!(preceding.read(cx).rendered_text(), "before");
            assert_eq!(editor.pending_focus, Some(preceding.entity_id()));
        });
    }

    #[rgpui::test]
    async fn arrow_down_into_table_focuses_header_cell(cx: &mut TestAppContext) {
        let markdown = ["before", "", "| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let paragraph = editor
                .document
                .first_root()
                .expect("paragraph root")
                .clone();
            editor.on_block_event(
                paragraph,
                &BlockEvent::RequestFocusNext { preferred_x: None },
                cx,
            );

            let header_cell = table_root(editor, cx)
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .header
                .first()
                .map(|cell| cell.entity_id());
            assert_eq!(editor.pending_focus, header_cell);
        });
    }

    #[rgpui::test]
    async fn arrow_up_into_table_focuses_last_row_cell(cx: &mut TestAppContext) {
        let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |", "", "after"].join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let paragraph = editor.document.visible_blocks()[1].entity.clone();
            assert_eq!(paragraph.read(cx).rendered_text(), "after");
            editor.on_block_event(
                paragraph,
                &BlockEvent::RequestFocusPrev { preferred_x: None },
                cx,
            );

            let last_row_cell = table_root(editor, cx)
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .rows
                .last()
                .and_then(|row| row.first())
                .map(|cell| cell.entity_id());
            assert_eq!(editor.pending_focus, last_row_cell);
        });
    }

    #[rgpui::test]
    async fn block_up_from_table_cell_exits_to_preceding_block(cx: &mut TestAppContext) {
        let markdown = ["before", "", "| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            // Start from a body cell, not the header, to confirm Block Up leaves
            // the whole table instead of stepping to the cell above.
            let cell = table_root(editor, cx)
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .rows
                .last()
                .and_then(|row| row.first())
                .cloned()
                .expect("body cell");
            editor.on_block_event(cell, &BlockEvent::RequestBlockUp, cx);

            let preceding = editor.document.visible_blocks()[0].entity.clone();
            assert_eq!(preceding.read(cx).rendered_text(), "before");
            assert_eq!(editor.pending_focus, Some(preceding.entity_id()));
        });
    }

    #[rgpui::test]
    async fn block_down_into_table_focuses_header_cell(cx: &mut TestAppContext) {
        let markdown = ["before", "", "| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let paragraph = editor
                .document
                .first_root()
                .expect("paragraph root")
                .clone();
            editor.on_block_event(paragraph, &BlockEvent::RequestBlockDown, cx);

            let header_cell = table_root(editor, cx)
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .header
                .first()
                .map(|cell| cell.entity_id());
            assert_eq!(editor.pending_focus, header_cell);
        });
    }

    #[rgpui::test]
    async fn down_out_of_code_block_focuses_following_block(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "```rust\nab\n```\n\nafter".to_string(), None));

        editor.update(cx, |editor, cx| {
            let code = editor.document.first_root().expect("code root").clone();
            assert!(code.read(cx).kind().is_code_block());
            // Down from the language field emits RequestFocusNext; with a block
            // below, focus lands there rather than creating anything.
            editor.on_block_event(
                code,
                &BlockEvent::RequestFocusNext { preferred_x: None },
                cx,
            );

            let following = editor.document.visible_blocks()[1].entity.clone();
            assert_eq!(following.read(cx).rendered_text(), "after");
            assert_eq!(editor.document.root_count(), 2);
            assert_eq!(editor.pending_focus, Some(following.entity_id()));
        });
    }

    #[rgpui::test]
    async fn down_out_of_trailing_code_block_creates_and_focuses_paragraph(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "```rust\nab\n```".to_string(), None));

        editor.update(cx, |editor, cx| {
            let code = editor.document.first_root().expect("code root").clone();
            assert_eq!(editor.document.root_count(), 1);
            editor.on_block_event(
                code,
                &BlockEvent::RequestFocusNext { preferred_x: None },
                cx,
            );

            let roots = editor.document.root_blocks();
            assert_eq!(roots.len(), 2);
            assert_eq!(roots[1].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(roots[1].read(cx).rendered_text(), "");
            assert_eq!(editor.pending_focus, Some(roots[1].entity_id()));
        });
    }

    #[rgpui::test]
    async fn down_out_of_trailing_math_block_creates_and_focuses_paragraph(
        cx: &mut TestAppContext,
    ) {
        // Same miss as code blocks, one of the other multi-line widget blocks.
        let editor = cx.new(|cx| Editor::from_markdown(cx, "$$\nx^2\n$$".to_string(), None));

        editor.update(cx, |editor, cx| {
            let math = editor.document.first_root().expect("math root").clone();
            assert_eq!(math.read(cx).kind(), BlockKind::MathBlock);
            editor.on_block_event(
                math,
                &BlockEvent::RequestFocusNext { preferred_x: None },
                cx,
            );

            let roots = editor.document.root_blocks();
            assert_eq!(roots.len(), 2);
            assert_eq!(roots[1].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(editor.pending_focus, Some(roots[1].entity_id()));
        });
    }

    #[rgpui::test]
    async fn down_at_end_of_trailing_paragraph_creates_nothing(cx: &mut TestAppContext) {
        // Regression guard: ordinary text blocks must not sprout a paragraph.
        let editor = cx.new(|cx| Editor::from_markdown(cx, "hello".to_string(), None));

        editor.update(cx, |editor, cx| {
            let paragraph = editor.document.first_root().expect("paragraph").clone();
            editor.on_block_event(
                paragraph,
                &BlockEvent::RequestFocusNext { preferred_x: None },
                cx,
            );

            // No trailing paragraph is invented for an ordinary text block.
            assert_eq!(editor.document.root_count(), 1);
        });
    }

    #[rgpui::test]
    async fn plain_multiline_paste_with_scripts_splits_physical_lines(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain(String::new()),
                    lines: vec![
                        "H~2~O".to_string(),
                        "CO<sub>2</sub>".to_string(),
                        "x<sup>n</sup>".to_string(),
                    ],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: true,
                },
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "H2O");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "CO2");
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "xn");
            assert_eq!(editor.document.markdown_text(cx), "H~2~O\n\nCO~2~\n\nx^n^");
        });
    }

    #[rgpui::test]
    async fn structural_paste_of_table_renders_native_table(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain(String::new()),
                    lines: vec![
                        "| A | B |".to_string(),
                        "| --- | --- |".to_string(),
                        "| 1 | 2 |".to_string(),
                    ],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: false,
                },
                cx,
            );

            // The header row must survive: previously the first pasted line was
            // folded into the paragraph, leaving the alignment row to masquerade
            // as the header. The empty paste target is also dropped, and a
            // trailing paragraph is added so the document does not end on the
            // table with no line below it.
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            let table = visible[0].entity.read(cx);
            assert_eq!(table.kind(), BlockKind::Table);
            let data = table.record.table.as_ref().expect("table data");
            assert_eq!(data.header[0].serialize_markdown(), "A");
            assert_eq!(data.header[1].serialize_markdown(), "B");
            assert_eq!(data.rows.len(), 1);
            assert_eq!(data.rows[0][0].serialize_markdown(), "1");
            assert_eq!(data.rows[0][1].serialize_markdown(), "2");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
        });
    }

    #[rgpui::test]
    async fn structural_paste_of_code_block_renders_native_code_block(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain(String::new()),
                    lines: vec![
                        "```rust".to_string(),
                        "fn main() {}".to_string(),
                        "```".to_string(),
                    ],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: false,
                },
                cx,
            );

            // The fence is structural, so the whole paste goes through the block
            // importer rather than the plain-text path: the opening ```rust line is
            // no longer folded into a paragraph, and the empty paste target is
            // dropped. A trailing paragraph is added so the document does not end
            // on the code block with no line below it.
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            let code = visible[0].entity.read(cx);
            assert_eq!(
                code.kind(),
                BlockKind::CodeBlock {
                    language: Some("rust".into())
                }
            );
            assert_eq!(code.rendered_text(), "fn main() {}");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(
                editor.document.markdown_text(cx),
                "```rust\nfn main() {}\n```\n\n"
            );
        });
    }

    #[rgpui::test]
    async fn structural_paste_of_table_preserves_surrounding_text(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "beforeafter".into(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain("before"),
                    lines: vec![
                        "| A | B |".to_string(),
                        "| --- | --- |".to_string(),
                        "| 1 | 2 |".to_string(),
                    ],
                    trailing: InlineTextTree::plain("after"),
                    split_physical_lines: false,
                },
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "before");

            let table = visible[1].entity.read(cx);
            assert_eq!(table.kind(), BlockKind::Table);
            let data = table.record.table.as_ref().expect("table data");
            assert_eq!(data.header[0].serialize_markdown(), "A");
            assert_eq!(data.rows[0][0].serialize_markdown(), "1");

            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "after");
        });
    }

    #[rgpui::test]
    async fn structural_paste_of_code_block_preserves_surrounding_text(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "beforeafter".into(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain("before"),
                    lines: vec![
                        "```rust".to_string(),
                        "fn main() {}".to_string(),
                        "```".to_string(),
                    ],
                    trailing: InlineTextTree::plain("after"),
                    split_physical_lines: false,
                },
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "before");
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::CodeBlock {
                    language: Some("rust".into())
                }
            );
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "fn main() {}");
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "after");
            // Text already follows the code block, so no extra trailing
            // paragraph is added mid-document.
        });
    }

    #[rgpui::test]
    async fn structural_paste_at_document_end_adds_one_trailing_paragraph(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "intro".into(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            block.update(cx, |block, _cx| {
                block.selected_range = block.plain_len()..block.plain_len();
            });
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain("intro"),
                    lines: vec!["***".to_string()],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: false,
                },
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "intro");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Separator);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "");
        });
    }

    #[rgpui::test]
    async fn structural_paste_of_quote_at_document_end_adds_trailing_paragraph(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "intro".into(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            block.update(cx, |block, _cx| {
                block.selected_range = block.plain_len()..block.plain_len();
            });
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain("intro"),
                    lines: vec!["> quoted".to_string()],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: false,
                },
                cx,
            );

            // The quote container cannot hold the caret below it, so a trailing
            // paragraph is added even though quote normalization re-parses the
            // whole document on the way.
            let roots = editor.document.root_blocks();
            assert_eq!(roots.len(), 3);
            assert_eq!(roots[0].read(cx).rendered_text(), "intro");
            assert_eq!(roots[1].read(cx).kind(), BlockKind::Quote);
            assert_eq!(roots[2].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(roots[2].read(cx).rendered_text(), "");
        });
    }

    #[rgpui::test]
    async fn structural_paste_of_callout_at_document_end_adds_trailing_paragraph(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "intro".into(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            block.update(cx, |block, _cx| {
                block.selected_range = block.plain_len()..block.plain_len();
            });
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain("intro"),
                    lines: vec!["> [!NOTE]".to_string(), "> body".to_string()],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: false,
                },
                cx,
            );

            let roots = editor.document.root_blocks();
            assert_eq!(roots.len(), 3);
            assert_eq!(
                roots[1].read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Note)
            );
            assert_eq!(roots[2].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(roots[2].read(cx).rendered_text(), "");
        });
    }

    #[rgpui::test]
    async fn structural_paste_of_footnote_definition_at_document_end_adds_trailing_paragraph(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "intro".into(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            block.update(cx, |block, _cx| {
                block.selected_range = block.plain_len()..block.plain_len();
            });
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain("intro"),
                    lines: vec!["[^note]: definition body".to_string()],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: false,
                },
                cx,
            );

            let roots = editor.document.root_blocks();
            assert_eq!(roots.len(), 3);
            assert_eq!(roots[1].read(cx).kind(), BlockKind::FootnoteDefinition);
            assert_eq!(roots[2].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(roots[2].read(cx).rendered_text(), "");
        });
    }

    #[rgpui::test]
    async fn structural_paste_of_standalone_image_at_document_end_adds_trailing_paragraph(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "intro".into(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            block.update(cx, |block, _cx| {
                block.selected_range = block.plain_len()..block.plain_len();
            });
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain("intro"),
                    lines: vec!["![alt](pic.png)".to_string()],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: false,
                },
                cx,
            );

            // A lone image renders as a self-contained widget, so it gets the
            // same trailing paragraph even though it is a paragraph block.
            let roots = editor.document.root_blocks();
            assert_eq!(roots.len(), 3);
            assert!(roots[1].read(cx).renders_as_standalone_image());
            assert_eq!(roots[2].read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(roots[2].read(cx).rendered_text(), "");
        });
    }

    #[rgpui::test]
    async fn plain_multiline_paste_with_blank_script_lines_skips_separator_blanks(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain(String::new()),
                    lines: vec![
                        "H~2~O".to_string(),
                        String::new(),
                        "CO<sub>2</sub>".to_string(),
                        String::new(),
                        "x<sup>n</sup>".to_string(),
                        String::new(),
                    ],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: true,
                },
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "H2O");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "CO2");
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "xn");
        });
    }

    #[rgpui::test]
    async fn plain_multiline_paste_with_leading_inline_html_splits_physical_lines(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain(String::new()),
                    lines: vec![
                        "<sub>2</sub>".to_string(),
                        "<sup>n</sup>".to_string(),
                        "<span style=\"color:red\">x</span>".to_string(),
                    ],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: true,
                },
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "2");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "n");
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "x");
            assert_eq!(
                editor.document.markdown_text(cx),
                "<sub>2</sub>\n\n<sup>n</sup>\n\n<span style=\"color: rgba(255,0,0,1.000);\">x</span>"
            );
        });
    }

    #[rgpui::test]
    async fn plain_paste_preserves_tibetan_spaces(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));
        let tibetan = "༄༅།།དཔལ་ལྡན་རྩ་བའི་བླ་མ་རིན་པོ་ཆེ།། བདག་གི་སྤྱི་བོར་པདྨའི་གདན་བཞུགས་ནས།། ";

        editor.update(cx, |editor, cx| {
            let block = editor.document.visible_blocks()[0].entity.clone();
            editor.on_block_event(
                block,
                &BlockEvent::RequestPasteMultiline {
                    leading: InlineTextTree::plain(String::new()),
                    lines: vec![tibetan.to_string()],
                    trailing: InlineTextTree::plain(String::new()),
                    split_physical_lines: true,
                },
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), tibetan);
            assert!(
                visible[0]
                    .entity
                    .read(cx)
                    .rendered_text()
                    .contains("།། བདག")
            );
            assert!(visible[0].entity.read(cx).rendered_text().ends_with(' '));
            assert_eq!(editor.document.markdown_text(cx), tibetan);
        });
    }

    #[rgpui::test]
    async fn nested_list_item_backspace_downgrades_to_direct_list_child(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- a\n  - b".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let nested = editor.document.visible_blocks()[1].entity.clone();
                nested.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "b");
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "- a\n\n  b");
        });
    }

    #[rgpui::test]
    async fn empty_nested_list_item_backspace_twice_exits_to_outer_paragraph(
        cx: &mut TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- a\n  - ".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let nested = editor.document.visible_blocks()[1].entity.clone();
                nested.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "- a\n  ");
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let child = editor.document.visible_blocks()[1].entity.clone();
                child.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).render_depth, 0);
            assert_eq!(editor.document.markdown_text(cx), "- a\n\n");
        });
    }

    #[rgpui::test]
    async fn nested_list_item_downgrade_hoists_children_after_paragraph(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "- a\n  - b\n    - c\n  - d".to_string(), None));

        editor.update(cx, |editor, cx| {
            let nested = editor.document.visible_blocks()[1].entity.clone();
            editor.on_block_event(
                nested,
                &BlockEvent::RequestDowngradeNestedListItemToChildParagraph,
                cx,
            );

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 4);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "b");
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "c");
            assert_eq!(visible[2].entity.read(cx).render_depth, 1);
            assert_eq!(
                visible[3].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[3].entity.read(cx).rendered_text(), "d");
            assert_eq!(visible[3].entity.read(cx).render_depth, 1);
            assert_eq!(
                editor.document.markdown_text(cx),
                "- a\n\n  b\n  - c\n  - d"
            );
        });
    }

    #[rgpui::test]
    async fn nested_numbered_and_task_items_backspace_downgrade_to_list_child(
        cx: &mut TestAppContext,
    ) {
        let cx = cx.add_empty_window();

        let numbered = cx.new(|cx| Editor::from_markdown(cx, "1. a\n  1. b".to_string(), None));
        cx.update(|window, cx| {
            numbered.update(cx, |editor, cx| {
                let nested = editor.document.visible_blocks()[1].entity.clone();
                nested.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });
        numbered.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "b");
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "1. a\n\n  b");
        });

        let task = cx.new(|cx| Editor::from_markdown(cx, "- [ ] a\n  - [ ] b".to_string(), None));
        cx.update(|window, cx| {
            task.update(cx, |editor, cx| {
                let nested = editor.document.visible_blocks()[1].entity.clone();
                nested.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });
        task.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "b");
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "- [ ] a\n\n  b");
        });
    }

    #[rgpui::test]
    async fn request_quote_break_creates_nested_leaf_quote_group(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> outer\n>> inner".to_string(), None));

        editor.update(cx, |editor, cx| {
            let nested_quote = editor.document.visible_blocks()[1].entity.clone();
            editor.on_block_event(nested_quote, &BlockEvent::RequestQuoteBreak, cx);

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 4);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "outer");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "inner");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 2);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[2].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[3].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[3].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[3].entity.read(cx).quote_depth, 2);
            assert_eq!(
                editor.document.markdown_text(cx),
                "> outer\n> > inner\n> \n> > "
            );
            assert_eq!(editor.pending_focus, Some(visible[3].entity.entity_id()));
        });
    }

    #[rgpui::test]
    async fn imported_leaf_quote_backspace_twice_downgrades_to_text_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> a".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let quote = editor.document.first_root().expect("root quote").clone();
                quote.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[0].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "> ");
        });

        let empty_quote_id = editor.update(cx, |editor, _cx| {
            editor
                .document
                .first_root()
                .expect("empty quote")
                .entity_id()
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let quote = editor.document.first_root().expect("empty quote").clone();
                quote.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[0].entity.read(cx).quote_depth, 0);
            assert_eq!(visible[0].entity.entity_id(), empty_quote_id);
            assert_eq!(editor.document.markdown_text(cx), "");
        });
    }

    #[rgpui::test]
    async fn shortcut_created_leaf_quote_backspace_twice_downgrades_to_text_block(
        cx: &mut TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let paragraph = editor
                .document
                .first_root()
                .expect("root paragraph")
                .clone();
            paragraph.update(cx, |block, cx| {
                block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
                block.replace_text_in_visible_range(0..0, "> ", None, false, cx);
                block.replace_text_in_visible_range(0..0, "a", None, false, cx);
            });
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let quote = editor
                    .document
                    .first_root()
                    .expect("shortcut quote")
                    .clone();
                quote.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let quote = editor.document.first_root().expect("empty shortcut quote");
            assert_eq!(quote.read(cx).kind(), BlockKind::Quote);
            assert_eq!(quote.read(cx).rendered_text(), "");
            assert_eq!(quote.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "> ");
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let quote = editor
                    .document
                    .first_root()
                    .expect("empty shortcut quote")
                    .clone();
                quote.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let paragraph = editor
                .document
                .first_root()
                .expect("text block after downgrade");
            assert_eq!(paragraph.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(paragraph.read(cx).rendered_text(), "");
            assert_eq!(editor.document.markdown_text(cx), "");
        });
    }

    #[rgpui::test]
    async fn root_quote_break_then_backspace_keeps_text_block_slot_after_group(
        cx: &mut TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> side\n>\n> 1234".to_string(), None));

        let new_leaf_id = editor.update(cx, |editor, cx| {
            let quote = editor.document.first_root().expect("group quote").clone();
            editor.on_block_event(quote, &BlockEvent::RequestQuoteBreak, cx);
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            visible[1].entity.entity_id()
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let new_leaf = editor.document.visible_blocks()[1].entity.clone();
                new_leaf.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "side\n\n1234");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.entity_id(), new_leaf_id);
            assert_eq!(visible[1].entity.read(cx).quote_depth, 0);
            assert_eq!(editor.document.markdown_text(cx), "> side\n> \n> 1234\n\n");
        });
    }

    #[rgpui::test]
    async fn empty_callout_body_backspace_downgrades_parent_to_quote(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> [!NOTE]\n> ".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let body = editor.document.visible_blocks()[1].entity.clone();
                body.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "[!NOTE]");
            assert_eq!(editor.document.markdown_text(cx), "> \\[!NOTE]");
        });
    }

    #[rgpui::test]
    async fn callout_exit_break_creates_plain_text_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> [!TIP]\n> body".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let body = editor.document.visible_blocks()[1].entity.clone();
                body.update(cx, |block, block_cx| {
                    block.on_exit_code_block(&ExitCodeBlock, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Tip)
            );
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[2].entity.read(cx).quote_depth, 0);
            assert_eq!(editor.document.markdown_text(cx), "> [!TIP]\n> body\n\n");
            assert_eq!(editor.pending_focus, Some(visible[2].entity.entity_id()));
        });
    }

    #[rgpui::test]
    async fn delete_on_empty_leaf_quote_downgrades_to_text_block(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> ".to_string(), None));

        let empty_quote_id = editor.update(cx, |editor, _cx| {
            editor
                .document
                .first_root()
                .expect("empty quote")
                .entity_id()
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let quote = editor.document.first_root().expect("empty quote").clone();
                quote.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete(&Delete, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[0].entity.entity_id(), empty_quote_id);
            assert_eq!(editor.document.markdown_text(cx), "");
        });
    }

    #[rgpui::test]
    async fn quote_container_with_children_does_not_collapse_from_leaf_exit_path(
        cx: &mut TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, ">\n> - item".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let quote = editor
                    .document
                    .first_root()
                    .expect("container quote")
                    .clone();
                quote.update(cx, |block, block_cx| {
                    block.move_to(0, block_cx);
                    block.on_delete_back(&DeleteBack, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[0].entity.read(cx).quote_depth, 1);
            assert!(!visible[0].entity.read(cx).children.is_empty());
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(editor.document.markdown_text(cx), "> - item");
        });
    }

    #[rgpui::test]
    async fn quote_newline_inside_title_stays_in_one_source_authoritative_group(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> firstsecond".to_string(), None));

        editor.update(cx, |editor, cx| {
            let quote = editor.document.first_root().expect("root quote").clone();
            quote.update(cx, |block, cx| {
                block.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
                block.replace_text_in_visible_range(5..5, "\n", None, false, cx);
            });

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "first\nsecond");
            assert_eq!(editor.document.markdown_text(cx), "> first\n> second");
        });
    }

    #[rgpui::test]
    async fn root_quote_enter_stays_in_same_group(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> first".to_string(), None));

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                let quote = editor.document.first_root().expect("root quote").clone();
                quote.update(cx, |block, block_cx| {
                    block.move_to(block.plain_len(), block_cx);
                });
                quote.update(cx, |block, block_cx| {
                    block.on_newline(&Newline, window, block_cx);
                });
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "first");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "> first\n> ");
        });
    }

    #[rgpui::test]
    async fn multiline_edit_inside_quote_reparses_into_child_blocks(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> first".to_string(), None));

        editor.update(cx, |editor, cx| {
            let quote = editor.document.first_root().expect("root quote").clone();
            quote.update(cx, |block, cx| {
                block.prepare_undo_capture(crate::components::UndoKind::NonCoalescible, cx);
                block.replace_text_in_visible_range(5..5, "\n- item", None, false, cx);
            });
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "first");
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "item");
            assert_eq!(editor.document.markdown_text(cx), "> first\n> - item");
        });
    }
}
