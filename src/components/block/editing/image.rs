//! 渲染后独立图片运行时状态。

use super::*;

impl Block {
    /// 取图片运行时。
    pub(crate) fn image_runtime(&self) -> Option<&ImageRuntime> {
        self.image_runtime.as_ref()
    }

    /// 判断块能否呈现为图片。
    pub(super) fn can_present_as_image(&self) -> bool {
        self.is_table_cell()
            || matches!(
                self.kind(),
                BlockKind::Paragraph
                    | BlockKind::BulletedListItem
                    | BlockKind::NumberedListItem
                    | BlockKind::TaskListItem { .. }
            )
    }

    /// 块文本是否为渲染为独立图片部件的孤立图片。与 `showing_rendered_image` 不同，此处由标题文本推导而非计算后运行时得出，故在图片运行时重建前依然有效。
    pub(crate) fn renders_as_standalone_image(&self) -> bool {
        self.can_present_as_image() && self.standalone_image_markdown_for_runtime().is_some()
    }

    /// 计算图片运行时。
    pub(super) fn compute_image_runtime(
        &self,
        base_dir: Option<&Path>,
        syntax: ImageSyntax,
    ) -> Option<ImageRuntime> {
        let resolved_target = syntax.resolve_target(&self.image_reference_definitions)?;
        self.can_present_as_image().then(|| ImageRuntime {
            alt: syntax.alt.clone(),
            src: resolved_target.src.clone(),
            title: resolved_target.title.clone(),
            resolved_source: resolve_image_source(&resolved_target.src, base_dir),
        })
    }

    /// 按语法取图片运行时。
    pub(crate) fn image_runtime_for_syntax(&self, syntax: ImageSyntax) -> Option<ImageRuntime> {
        self.compute_image_runtime(self.image_base_dir.as_deref(), syntax)
    }

    /// 取图片基准目录。
    pub(crate) fn image_base_dir(&self) -> Option<&Path> {
        self.image_base_dir.as_deref()
    }

    /// 同步图片运行时。
    pub(super) fn sync_image_runtime(&mut self) {
        let next_runtime = if self.can_present_as_image() {
            self.standalone_image_markdown_for_runtime()
                .and_then(|markdown| parse_standalone_image(&markdown))
                .and_then(|syntax| {
                    self.compute_image_runtime(self.image_base_dir.as_deref(), syntax)
                })
        } else {
            None
        };

        if next_runtime.is_none() {
            self.image_edit_expanded = false;
            self.image_expand_requested = false;
        }
        self.image_runtime = next_runtime;
    }

    /// 取用于运行时的独立图片标记。
    fn standalone_image_markdown_for_runtime(&self) -> Option<String> {
        // `parse_standalone_image` 要求文本以 `![` 开头；先按片段检查两个
        // 候选来源的起始标记，绝大多数块都可直接返回，省下可见文本与
        // Markdown 两份字符串分配（大文档每块都会走到这里）。
        if !self.record.title.has_image_marker() {
            return None;
        }

        let visible = self.record.title.plain_text();
        if parse_standalone_image(&visible).is_some() {
            return Some(visible);
        }

        let serialized = self.record.title.serialize_markdown();
        parse_standalone_image(&serialized)
            .is_some()
            .then_some(serialized)
    }

    /// 请求展开图片编辑态。
    pub(crate) fn request_image_edit_expansion(&mut self) {
        if self.image_runtime.is_some() {
            self.image_expand_requested = true;
        }
    }

    /// 消费已请求的图片编辑展开。
    pub(super) fn consume_requested_image_edit_expansion(&mut self) -> bool {
        if self.image_runtime.is_some() && self.image_expand_requested && !self.image_edit_expanded
        {
            self.image_expand_requested = false;
            self.image_edit_expanded = true;
            self.reset_inline_projection();
            self.assign_collapsed_selection_offset(self.plain_len(), CaretAffinity::Default, None);
            self.cursor_blink_epoch = Instant::now();
            self.clear_vertical_motion();
            return true;
        }

        false
    }

    /// 同步图片聚焦状态。
    pub(crate) fn sync_image_focus_state(&mut self, focused: bool) -> bool {
        if self.image_runtime.is_none() {
            if self.image_edit_expanded || self.image_expand_requested {
                self.image_edit_expanded = false;
                self.image_expand_requested = false;
                self.reset_inline_projection();
                return true;
            }
            return false;
        }

        if focused {
            return self.consume_requested_image_edit_expansion();
        }

        if self.image_edit_expanded {
            self.image_edit_expanded = false;
            self.reset_inline_projection();
            return true;
        }

        false
    }

    /// 判断是否正在展示渲染图片。
    pub(crate) fn showing_rendered_image(&self) -> bool {
        self.image_runtime.is_some() && !self.is_source_raw_mode() && !self.image_edit_expanded
    }

    /// 是否为内嵌 `data:` URI 图片。
    ///
    /// 这类图片的源码是超长 base64 串，点击时不展开源码编辑（展开会把
    /// 上万字符的串摊进正文），需要编辑时从右键菜单显式进入。
    pub(crate) fn renders_embedded_image(&self) -> bool {
        self.image_runtime.as_ref().is_some_and(|runtime| {
            matches!(
                runtime.resolved_source,
                ImageResolvedSource::Embedded { .. }
            )
        })
    }

    /// 是否处于「源码行与渲染图同屏」的图片编辑态。
    ///
    /// 点击图片进入该状态：源码行叠加在渲染图上方，图片保持可见（对齐 Typora）。
    pub(crate) fn showing_image_source_with_render(&self) -> bool {
        self.image_runtime.is_some() && !self.is_source_raw_mode() && self.image_edit_expanded
    }
}
