//! 可编辑块运行时与块本地状态转换。

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rgpui::*;
use unicode_segmentation::*;

mod code;
mod image;
mod projection;
mod table;

use self::projection::{
    ExpandedInlineSegmentKind, InlineExpansionProjection, InlineExpansionSegment, LinkExpansionRun,
    LinkSelectionSnapshot,
};
use super::{
    BlockEvent, BlockKind, BlockRecord, CalloutVariant, FootnoteRegistry, InlineFootnoteHit,
    TableAxisMenuState, UndoKind,
};
use super::{HighlightResult, highlight_code_block};
use super::{
    ImageReferenceDefinitions, ImageResolvedSource, ImageSyntax, LinkReferenceDefinitions,
    parse_standalone_image, resolve_image_source,
};
use crate::components::markdown::inline::{
    InlineFragment, InlineInsertionAttributes, InlineLinkHit, InlineRenderCache, InlineSpan,
    InlineStyle, InlineStyleFlag, InlineTextTree,
};
use crate::components::{
    AxisMarker, CellPosition, TableAxisHighlight, TableColumnAlignment, TableRuntime,
    update_entity_forgiving,
};

/// 鼠标按下点击次数决定的拖拽选区粒度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectExtendMode {
    /// 单击：按字符扩展。
    Char,
    /// 双击：按词扩展。
    Word,
    /// 三击：按行扩展。
    Line,
}

/// 编辑器动作下发的行内格式命令。
#[derive(Clone, Copy)]
pub(crate) enum InlineFormat {
    /// 切换加粗。
    Bold,
    /// 切换斜体。
    Italic,
    /// 切换下划线。
    Underline,
    /// 切换行内代码。
    Code,
}

/// 当前块的编辑语义。
///
/// 富文本块编辑基于属性的文本树，源码模式与代码块编辑无行内 Markdown 归一化的纯文本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditMode {
    /// 普通渲染块基于属性的富文本编辑。
    RenderedRich,
    /// 源码模式与纯回退块的纯 Markdown 编辑。
    SourceRaw,
    /// 围栏代码块内容的纯文本编辑。
    CodeBlockRaw,
}

impl EditMode {
    /// 按块类型推导编辑模式。
    fn for_kind(kind: &BlockKind) -> Self {
        if kind.is_code_block() {
            Self::CodeBlockRaw
        } else if matches!(
            kind,
            BlockKind::RawMarkdown
                | BlockKind::Comment
                | BlockKind::HtmlBlock
                | BlockKind::MathBlock
                | BlockKind::MermaidBlock
        ) {
            Self::SourceRaw
        } else {
            Self::RenderedRich
        }
    }

    /// 判断是否使用纯文本编辑。
    fn uses_raw_text_editing(self) -> bool {
        matches!(self, Self::SourceRaw | Self::CodeBlockRaw)
    }

    /// 判断是否支持行内投影。
    fn supports_inline_projection(self) -> bool {
        matches!(self, Self::RenderedRich)
    }
}

impl EventEmitter<BlockEvent> for Block {}

/// 文档树中的单个可编辑块。
///
/// 每个块持有含持久数据（类型、标题、UUID）的 [`BlockRecord`] 与用于键盘路由的 [`FocusHandle`]。选区、光标闪烁、布局缓存等运行时状态存于本结构体。
///
/// 块经 `BlockEvent` 事件把结构操作（拆分、合并、缩进、删除）委托给父编辑器。
pub struct Block {
    pub record: BlockRecord,
    pub(crate) render_cache: InlineRenderCache,
    code_highlight: Option<HighlightResult>,
    /// 当前高亮结果对应的失效键（语言 + 块内容版本号），未变时直接复用。
    code_highlight_key: Option<(Option<SharedString>, u64)>,
    /// 已在后台计算、尚未回填的高亮键；避免滚动帧重复派发任务。
    code_highlight_pending_key: Option<(Option<SharedString>, u64)>,
    /// 后台高亮任务句柄。
    code_highlight_task: Option<Task<()>>,
    pub children: Vec<Entity<Block>>,
    pub focus_handle: FocusHandle,
    pub(crate) code_language_focus_handle: FocusHandle,
    pub(crate) code_language_selected_range: Range<usize>,
    pub(crate) code_language_selection_reversed: bool,
    pub(crate) code_language_marked_range: Option<Range<usize>>,
    pub(crate) code_language_last_layout: Option<ShapedLine>,
    pub(crate) code_language_last_bounds: Option<Bounds<Pixels>>,
    pub(crate) code_language_is_selecting: bool,
    pub selected_range: Range<usize>,
    pub selection_reversed: bool,
    pub(crate) editor_selection_range: Option<Range<usize>>,
    /// 是否处于编辑器跨块选区范围内（含端点为空区间的块）。
    /// 与 `editor_selection_range` 不同，空区间不会丢失该标记，供输入路径判断是否替换跨块选区。
    pub(crate) cross_block_selected: bool,
    pub marked_range: Option<Range<usize>>,
    pub last_layout: Option<Vec<WrappedLine>>,
    pub last_bounds: Option<Bounds<Pixels>>,
    /// 最近一次绘制出的整块外壳边界（含内边距）。用于将明显落在本块之外的误投递鼠标按下挡掉。
    pub(crate) last_shell_bounds: Option<Bounds<Pixels>>,
    pub last_line_height: Pixels,
    pub render_depth: usize,
    pub quote_depth: usize,
    pub(crate) quote_group_anchor: Option<uuid::Uuid>,
    pub(crate) visible_quote_depth: usize,
    pub(crate) visible_quote_group_anchor: Option<uuid::Uuid>,
    pub(crate) callout_depth: usize,
    pub(crate) callout_anchor: Option<uuid::Uuid>,
    pub(crate) callout_variant: Option<CalloutVariant>,
    pub(crate) footnote_anchor: Option<uuid::Uuid>,
    pub(crate) parent_is_list_item: bool,
    pub list_ordinal: Option<usize>,
    pub is_selecting: bool,
    /// 拖拽扩展选区的吸附粒度：单击按字符，双击按词，三击按行。
    /// mouse_down 时设定，mouse_move 消费，mouse_up 复位。
    pub(super) select_extend_mode: SelectExtendMode,
    pub cursor_blink_epoch: Instant,
    pub vertical_motion_x: Option<Pixels>,
    pub(super) cursor_blink_task: Option<Task<()>>,
    /// 缓存的投影，用于展示当前触及行内区间可编辑的分隔符。
    pub(crate) projection: Option<InlineExpansionProjection>,
    /// 产生当前 `projection` 的输入。当下次
    /// `refresh_inline_projection_for_focus` 算出相同输入时跳过重建，
    /// 省去每渲染帧一次全量遍历（含光标闪烁与方向键）。
    projection_cache_key: Option<(bool, Range<usize>, Option<Range<usize>>)>,
    /// 以共享字符串持有的展示文本，渲染时克隆引用计数指针即可，
    /// 避免逐帧重新分配。在 `refresh_render_cache`、
    /// `rebuild_inline_projection` 与 `reset_inline_projection` 中刷新。
    cached_display_text: SharedString,
    collapsed_caret_affinity: CaretAffinity,
    /// 为真时屏蔽块级快捷键与行内格式，块以纯文本存储源码模式编辑内容。
    pub(crate) edit_mode: EditMode,
    show_source_line_numbers: bool,
    pub(crate) table_runtime: Option<TableRuntime>,
    pub(crate) table_cell_position: Option<CellPosition>,
    pub(crate) table_cell_alignment: Option<TableColumnAlignment>,
    pub(crate) table_axis_preview: Option<AxisMarker>,
    pub(crate) table_axis_selection: Option<AxisMarker>,
    pub(crate) table_axis_highlight: TableAxisHighlight,
    /// 表格轴右键菜单状态（框架弹出菜单宿主，一次一个）。
    pub(crate) table_axis_menu: Option<TableAxisMenuState>,
    pub(crate) table_append_column_edge_hovered: bool,
    pub(crate) table_append_column_hovered: bool,
    pub(crate) table_append_column_zone_hovered: bool,
    pub(crate) table_append_column_button_hovered: bool,
    pub(crate) table_append_column_close_task: Option<Task<()>>,
    pub(crate) table_append_row_edge_hovered: bool,
    pub(crate) table_append_row_hovered: bool,
    pub(crate) table_append_row_zone_hovered: bool,
    pub(crate) table_append_row_button_hovered: bool,
    pub(crate) table_append_row_close_task: Option<Task<()>>,
    image_runtime: Option<ImageRuntime>,
    image_edit_expanded: bool,
    image_expand_requested: bool,
    pub(crate) html_details_open: bool,
    image_base_dir: Option<PathBuf>,
    image_reference_definitions: Arc<ImageReferenceDefinitions>,
    link_reference_definitions: Arc<LinkReferenceDefinitions>,
    footnote_registry: Arc<FootnoteRegistry>,
    pub(crate) list_group_separator_candidate: bool,
    numbered_list_restart_requested: bool,
    quote_reparse_requested: bool,
}

/// 块的缓存独立图片呈现状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageRuntime {
    pub(crate) alt: String,
    pub(crate) src: String,
    pub(crate) title: Option<String>,
    pub(crate) resolved_source: ImageResolvedSource,
}

/// 行内投影边界处折叠光标的样式继承方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CaretAffinity {
    /// 使用常规插入属性查找。
    #[default]
    Default,
    /// 视为紧贴起始分隔符外侧。
    OuterStart,
    /// 视为紧贴结束分隔符外侧。
    OuterEnd,
}

impl Block {
    /// 用记录新建块。
    pub fn with_record(cx: &mut Context<Self>, record: BlockRecord) -> Self {
        let edit_mode = EditMode::for_kind(&record.kind);
        let render_cache = record.title.render_cache();
        let defer_code_highlight = record.defer_code_highlight;
        let mut block = Self {
            record,
            render_cache,
            code_highlight: None,
            code_highlight_key: None,
            code_highlight_pending_key: None,
            code_highlight_task: None,
            children: Vec::new(),
            focus_handle: cx.focus_handle(),
            code_language_focus_handle: cx.focus_handle(),
            code_language_selected_range: 0..0,
            code_language_selection_reversed: false,
            code_language_marked_range: None,
            code_language_last_layout: None,
            code_language_last_bounds: None,
            code_language_is_selecting: false,
            selected_range: 0..0,
            selection_reversed: false,
            editor_selection_range: None,
            cross_block_selected: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            last_shell_bounds: None,
            last_line_height: px(20.0),
            render_depth: 0,
            quote_depth: 0,
            quote_group_anchor: None,
            visible_quote_depth: 0,
            visible_quote_group_anchor: None,
            callout_depth: 0,
            callout_anchor: None,
            callout_variant: None,
            footnote_anchor: None,
            parent_is_list_item: false,
            list_ordinal: None,
            is_selecting: false,
            select_extend_mode: SelectExtendMode::Char,
            cursor_blink_epoch: Instant::now(),
            vertical_motion_x: None,
            cursor_blink_task: None,
            projection: None,
            projection_cache_key: None,
            cached_display_text: SharedString::default(),
            collapsed_caret_affinity: CaretAffinity::Default,
            edit_mode,
            show_source_line_numbers: false,
            table_runtime: None,
            table_cell_position: None,
            table_cell_alignment: None,
            table_axis_preview: None,
            table_axis_selection: None,
            table_axis_highlight: TableAxisHighlight::None,
            table_axis_menu: None,
            table_append_column_edge_hovered: false,
            table_append_column_hovered: false,
            table_append_column_zone_hovered: false,
            table_append_column_button_hovered: false,
            table_append_column_close_task: None,
            table_append_row_edge_hovered: false,
            table_append_row_hovered: false,
            table_append_row_zone_hovered: false,
            table_append_row_button_hovered: false,
            table_append_row_close_task: None,
            image_runtime: None,
            image_edit_expanded: false,
            image_expand_requested: false,
            html_details_open: false,
            image_base_dir: None,
            image_reference_definitions: Arc::default(),
            link_reference_definitions: Arc::default(),
            footnote_registry: Arc::default(),
            list_group_separator_candidate: false,
            numbered_list_restart_requested: false,
            quote_reparse_requested: false,
        };
        if !defer_code_highlight {
            block.sync_code_highlight();
        }
        block.refresh_cached_display_text();
        block
    }

    /// 取块类型。
    pub fn kind(&self) -> BlockKind {
        self.record.kind.clone()
    }

    /// 是否为源码纯文本模式。
    pub(crate) fn is_source_raw_mode(&self) -> bool {
        self.edit_mode == EditMode::SourceRaw
    }

    /// 是否为链接/图片引用定义块（`[label]: url`），渲染视图隐藏。
    pub(crate) fn is_reference_definition(&self) -> bool {
        self.record.reference_definition
    }

    /// 该空段落是否应在渲染时折叠为零高。
    ///
    /// 相邻列表组之间的空段落只用于在源码里分隔两组列表，渲染时对齐 Typora
    /// 不显示多余空行；聚焦时仍恢复常规高度以显示光标。
    pub(crate) fn collapses_as_list_group_separator(&self, focused: bool) -> bool {
        self.list_group_separator_candidate && !focused && !self.is_source_raw_mode()
    }

    /// 是否展示源码行号。
    pub(crate) fn show_source_line_numbers(&self) -> bool {
        self.show_source_line_numbers
    }

    /// 取走引用重解析请求。
    pub(crate) fn take_quote_reparse(&mut self) -> bool {
        let requested = self.quote_reparse_requested;
        self.quote_reparse_requested = false;
        requested
    }

    /// 取走有序列表重启请求。
    pub(crate) fn take_numbered_list_restart(&mut self) -> bool {
        let requested = self.numbered_list_restart_requested;
        self.numbered_list_restart_requested = false;
        requested
    }

    /// 设置运行时上下文。
    pub(crate) fn set_runtime_context(
        &mut self,
        base_dir: Option<PathBuf>,
        image_reference_definitions: Arc<ImageReferenceDefinitions>,
        link_reference_definitions: Arc<LinkReferenceDefinitions>,
        footnote_registry: Arc<FootnoteRegistry>,
    ) {
        if self.image_base_dir != base_dir {
            self.image_base_dir = base_dir;
        }
        if !Arc::ptr_eq(
            &self.image_reference_definitions,
            &image_reference_definitions,
        ) && self.image_reference_definitions != image_reference_definitions
        {
            self.image_reference_definitions = image_reference_definitions;
        }
        self.sync_link_reference_definitions(link_reference_definitions);
        self.sync_footnote_registry(footnote_registry);
        self.sync_image_runtime();
    }

    /// 是否使用纯文本编辑。
    pub(crate) fn uses_raw_text_editing(&self) -> bool {
        self.edit_mode.uses_raw_text_editing()
    }

    /// 进入源码纯文本模式。
    pub(crate) fn enable_source_raw_mode(&mut self) {
        self.reset_inline_projection();
        self.edit_mode = EditMode::SourceRaw;
        self.show_source_line_numbers = false;
    }

    /// 进入源码文档模式。
    pub(crate) fn enable_source_document_mode(&mut self) {
        self.enable_source_raw_mode();
        self.show_source_line_numbers = true;
    }

    /// 按类型同步编辑模式。
    pub(crate) fn sync_edit_mode_from_kind(&mut self) {
        if self.table_cell_position.is_some() {
            self.edit_mode = EditMode::RenderedRich;
            self.show_source_line_numbers = false;
            return;
        }
        if self.kind().is_code_block() {
            self.reset_inline_projection();
        }
        self.edit_mode = EditMode::for_kind(&self.record.kind);
        self.show_source_line_numbers = false;
    }

    /// 取展示文本。
    pub fn rendered_text(&self) -> &str {
        self.current_cache().plain_text()
    }

    /// 以 `SharedString` 低成本克隆当前展示文本（引用计数加一），
    /// 避免每帧重新分配字符串。底层文本可能变化时，
    /// 由 [`Self::refresh_cached_display_text`] 刷新缓存值。
    pub(crate) fn shared_display_text(&self) -> SharedString {
        self.cached_display_text.clone()
    }

    /// 刷新缓存的展示文本。
    fn refresh_cached_display_text(&mut self) {
        let current = self.current_cache().plain_text();
        if self.cached_display_text.as_ref() != current {
            self.cached_display_text = SharedString::from(current.to_string());
        }
    }

    /// 以带上下文方式从 Markdown 解析行内树。
    pub(crate) fn inline_tree_from_markdown_with_context(&self, markdown: &str) -> InlineTextTree {
        InlineTextTree::from_markdown_with_link_references(
            markdown,
            &self.link_reference_definitions,
        )
    }

    /// 取行内片段。
    pub fn inline_spans(&self) -> &[InlineSpan] {
        self.current_cache().spans()
    }

    #[allow(dead_code)]
    /// 取指定偏移的行内样式。
    pub fn inline_style_at(&self, offset: usize) -> InlineStyle {
        self.current_cache().style_at(offset)
    }

    #[allow(dead_code)]
    /// 取指定偏移的行内 HTML 样式。
    pub(crate) fn inline_html_style_at(
        &self,
        offset: usize,
    ) -> Option<crate::components::HtmlInlineStyle> {
        self.current_cache().html_style_at(offset)
    }

    #[allow(dead_code)]
    /// 取指定偏移的链接目标。
    pub(crate) fn inline_link_at(&self, offset: usize) -> Option<&str> {
        self.current_cache().link_at(offset)
    }

    #[allow(dead_code)]
    /// 取指定偏移的链接命中。
    pub(crate) fn inline_link_hit_at(&self, offset: usize) -> Option<&InlineLinkHit> {
        self.current_cache().link_hit_at(offset)
    }

    #[allow(dead_code)]
    /// 取指定偏移的脚注命中。
    pub(crate) fn inline_footnote_hit_at(&self, offset: usize) -> Option<&InlineFootnoteHit> {
        self.current_cache().footnote_hit(offset)
    }

    /// 是否含混合行内视觉。
    pub(crate) fn has_mixed_inline_visuals(&self) -> bool {
        self.record.title.has_mixed_inline_visuals()
    }

    /// 取脚注定义标识。
    pub(crate) fn footnote_definition_id(&self) -> Option<String> {
        self.kind()
            .is_footnote_definition()
            .then(|| self.record.title.plain_text())
    }

    /// 取脚注定义序号。
    pub(crate) fn footnote_definition_ordinal(&self) -> Option<usize> {
        self.footnote_definition_id()
            .as_deref()
            .and_then(|id| self.footnote_registry.ordinal(id))
    }

    /// 脚注定义是否有回链。
    pub(crate) fn footnote_definition_has_backref(&self) -> bool {
        self.footnote_definition_id().as_deref().is_some_and(|id| {
            self.footnote_registry
                .binding(id)
                .and_then(|binding| binding.first_reference.as_ref())
                .is_some()
        })
    }

    /// 取脚注出现处的当前范围。
    pub(crate) fn current_range_for_footnote_occurrence(
        &self,
        occurrence_index: usize,
    ) -> Option<Range<usize>> {
        let mut clean_offset = 0usize;
        for fragment in &self.record.title.fragments {
            let len = fragment.text.len();
            if fragment
                .footnote
                .as_ref()
                .is_some_and(|footnote| footnote.occurrence_index == occurrence_index)
            {
                return Some(self.clean_to_current_range(clean_offset..clean_offset + len));
            }
            clean_offset += len;
        }
        None
    }

    /// 判断是否为空。
    pub fn is_empty(&self) -> bool {
        self.rendered_text().is_empty()
    }

    /// 是否为列表直接子块。
    pub fn is_direct_list_child(&self) -> bool {
        self.parent_is_list_item && !self.kind().is_list_item()
    }

    /// 是否为嵌套列表项。
    pub fn is_nested_list_item(&self) -> bool {
        self.parent_is_list_item && self.kind().is_list_item()
    }

    /// 能否调整列表嵌套。
    pub fn can_change_list_nesting(&self) -> bool {
        (self.kind().is_list_item() || self.parent_is_list_item) && !self.kind().is_code_block()
    }

    /// 能否减少列表嵌套。
    pub fn can_unindent_list_nesting(&self) -> bool {
        self.kind().is_list_item() || self.parent_is_list_item
    }

    /// 取可见长度。
    pub(crate) fn plain_len(&self) -> usize {
        self.current_cache().plain_len()
    }

    /// 按偏移切分标题。
    pub(crate) fn split_title(&self, offset: usize) -> (InlineTextTree, InlineTextTree) {
        self.record
            .title
            .split_at(self.current_to_clean_offset(offset))
    }

    /// 清除纵向移动记忆。
    fn clear_vertical_motion(&mut self) {
        self.vertical_motion_x = None;
    }

    /// 同步渲染缓存。
    pub(crate) fn refresh_render_cache(&mut self) {
        let clean_selected = self.current_to_clean_range(self.selected_range.clone());
        let clean_marked = self
            .marked_range
            .clone()
            .map(|range| self.current_to_clean_range(range));
        let (clean_anchor, clean_focus) = self.clean_selection_anchor_focus();
        let (anchor_affinity, focus_affinity) = self.selection_endpoint_affinities();
        let collapsed_affinity = self.current_collapsed_caret_affinity();
        let keep_projection =
            self.projection.is_some() && self.edit_mode.supports_inline_projection();
        self.render_cache = self.record.title.render_cache();
        self.sync_code_highlight();
        self.sync_image_runtime();
        self.projection = None;
        self.projection_cache_key = None;
        if keep_projection {
            self.rebuild_inline_projection(clean_selected.clone(), clean_marked.clone());
            if clean_selected.is_empty() {
                let offset = self.clean_to_current_cursor_offset_with_affinity(
                    clean_selected.start,
                    collapsed_affinity,
                );
                self.assign_collapsed_selection_offset(offset, collapsed_affinity, None);
            } else {
                self.set_selection_from_clean_anchor_focus(
                    clean_anchor,
                    clean_focus,
                    anchor_affinity,
                    focus_affinity,
                );
                self.collapsed_caret_affinity = CaretAffinity::Default;
            }
            self.marked_range = clean_marked.map(|range| self.clean_to_current_range(range));
        } else {
            self.set_selection_from_anchor_focus(clean_anchor, clean_focus);
            self.marked_range = clean_marked;
            self.collapsed_caret_affinity = CaretAffinity::Default;
        }
        self.refresh_cached_display_text();
    }

    /// 同步链接引用定义。
    fn sync_link_reference_definitions(
        &mut self,
        link_reference_definitions: Arc<LinkReferenceDefinitions>,
    ) {
        // 先走指针短路：编辑器侧保证内容未变时 Arc 指针稳定，万级块文档的
        // 全量同步在稳态下都是 O(1)，不做定义表深度比较。
        if Arc::ptr_eq(
            &self.link_reference_definitions,
            &link_reference_definitions,
        ) {
            return;
        }
        if self.link_reference_definitions == link_reference_definitions {
            // 内容一致但指针不同：收下新 Arc 让指针收敛，后续同步走指针短路。
            self.link_reference_definitions = link_reference_definitions;
            return;
        }

        // 先保存新定义，供后续编辑解析使用；但只有含引用候选的块才需要
        // 把标题序列化再全量重解析。绝大多数块没有 `[`，可跳过这段重活。
        self.link_reference_definitions = link_reference_definitions;
        if self.uses_raw_text_editing() || !self.record.title.has_link_reference_candidate() {
            return;
        }

        let selected_markdown = (!self.uses_raw_text_editing())
            .then(|| self.current_range_to_markdown_range(self.selected_range.clone()));
        let marked_markdown = (!self.uses_raw_text_editing())
            .then(|| {
                self.marked_range
                    .clone()
                    .map(|range| self.current_range_to_markdown_range(range))
            })
            .flatten();
        let selection_reversed = self.selection_reversed;
        let collapsed_affinity = self.current_collapsed_caret_affinity();
        let had_projection = self.projection.is_some();

        let markdown = self.record.title.serialize_markdown();
        let next_title = InlineTextTree::from_markdown_with_link_references(
            &markdown,
            &self.link_reference_definitions,
        );
        if self.record.title == next_title {
            return;
        }

        self.record.set_title(next_title);
        self.sync_edit_mode_from_kind();
        self.refresh_render_cache();

        if let Some(selected_markdown) = selected_markdown {
            let restored = self.markdown_range_to_current_range(selected_markdown);
            if restored.is_empty() {
                self.assign_collapsed_selection_offset(
                    restored.start,
                    collapsed_affinity,
                    self.vertical_motion_x,
                );
            } else {
                self.selected_range = restored;
                self.selection_reversed = selection_reversed;
                self.collapsed_caret_affinity = CaretAffinity::Default;
            }
        }

        self.marked_range =
            marked_markdown.map(|range| self.markdown_range_to_current_range(range));

        if had_projection {
            self.refresh_inline_projection_for_focus(true);
        }
    }

    /// 同步脚注注册表。
    fn sync_footnote_registry(&mut self, footnote_registry: Arc<FootnoteRegistry>) {
        // 指针短路同链接定义：内容一致时由编辑器侧保留旧 Arc，稳态同步 O(1)。
        if Arc::ptr_eq(&self.footnote_registry, &footnote_registry) {
            return;
        }
        if self.footnote_registry == footnote_registry {
            self.footnote_registry = footnote_registry;
            return;
        }

        let selected_markdown = (!self.uses_raw_text_editing())
            .then(|| self.current_range_to_markdown_range(self.selected_range.clone()));
        let marked_markdown = (!self.uses_raw_text_editing())
            .then(|| {
                self.marked_range
                    .clone()
                    .map(|range| self.current_range_to_markdown_range(range))
            })
            .flatten();
        let selection_reversed = self.selection_reversed;
        let collapsed_affinity = self.current_collapsed_caret_affinity();
        let had_projection = self.projection.is_some();

        self.footnote_registry = footnote_registry;
        if self.uses_raw_text_editing() || !self.record.title.has_footnote_references() {
            return;
        }

        let mut next_title = self.record.title.clone();
        let mut occurrence_iter = self
            .footnote_registry
            .occurrences_for_block(self.record.id)
            .unwrap_or(&[])
            .iter();
        next_title.apply_footnote_reference_state(|id| {
            let occurrence = occurrence_iter.next()?;
            if occurrence.id != id {
                return None;
            }
            Some((occurrence.ordinal?, occurrence.occurrence_index))
        });
        if self.record.title == next_title {
            return;
        }

        self.record.set_title(next_title);
        self.sync_edit_mode_from_kind();
        self.refresh_render_cache();

        if let Some(selected_markdown) = selected_markdown {
            let restored = self.markdown_range_to_current_range(selected_markdown);
            if restored.is_empty() {
                self.assign_collapsed_selection_offset(
                    restored.start,
                    collapsed_affinity,
                    self.vertical_motion_x,
                );
            } else {
                self.selected_range = restored;
                self.selection_reversed = selection_reversed;
                self.collapsed_caret_affinity = CaretAffinity::Default;
            }
        }

        self.marked_range =
            marked_markdown.map(|range| self.markdown_range_to_current_range(range));

        if had_projection {
            self.refresh_inline_projection_for_focus(true);
        }
    }

    /// 判断是否走源码空格链接编辑。
    fn should_use_markdown_space_link_edit(&self) -> bool {
        !self.uses_raw_text_editing() && self.record.title.has_source_preserving_links()
    }

    /// 在源码空格上应用标题编辑。
    fn apply_markdown_space_title_edit(
        &mut self,
        visible_range: Range<usize>,
        new_text: &str,
        selected_range_relative: Option<Range<usize>>,
        mark_inserted_text: bool,
        cx: &mut Context<Self>,
    ) {
        let old_visible_len = self.record.title.plain_text().len();
        let mut markdown = self.record.title.serialize_markdown();
        // 即使上游映射已归一化，这里仍夹一次源码标量边界，杜绝切片/替换 panic。
        let markdown_range = crate::text_offsets::normalize_utf8_range(
            &markdown,
            self.current_range_to_markdown_range(visible_range.clone()),
        );
        let replaced_text = markdown[markdown_range.clone()].to_string();
        markdown.replace_range(markdown_range.clone(), new_text);

        let next_title = InlineTextTree::from_markdown_with_link_references(
            &markdown,
            &self.link_reference_definitions,
        );
        let map = next_title.markdown_offset_map();
        let selected_markdown = selected_range_relative.as_ref().map(|relative| {
            markdown_range.start + relative.start..markdown_range.start + relative.end
        });
        let cursor_markdown = selected_markdown
            .as_ref()
            .map(|range| range.end)
            .unwrap_or(markdown_range.start + new_text.len());
        let marked_markdown = if mark_inserted_text && !new_text.is_empty() {
            Some(markdown_range.start..markdown_range.start + new_text.len())
        } else {
            None
        };
        let selected_clean = selected_markdown
            .as_ref()
            .map(|range| map.markdown_to_visible_range(range.clone()));
        let marked_clean = marked_markdown
            .as_ref()
            .map(|range| map.markdown_to_visible_range(range.clone()));
        let cursor_clean = map.markdown_to_visible_offset(cursor_markdown);

        let quote_structure_edit = self.quote_depth > 0
            && (new_text.contains('\n')
                || replaced_text.contains('\n')
                || (self.kind() == BlockKind::Quote
                    && Self::multiline_quote_edit_requires_reparse(&next_title.plain_text())));
        if quote_structure_edit {
            self.quote_reparse_requested = true;
        }

        // 键入结束标记（如补全链接的 `)`）会被吸收为样式区间，
        // 此时纯文本增长少于插入文本，标记后把光标放在新结束符之后，
        // 而不是落入区间内部。
        let caret_may_have_closed_span = !new_text.is_empty()
            && !mark_inserted_text
            && next_title.plain_text().len() < old_visible_len + new_text.len();

        self.apply_title_edit(
            next_title,
            cursor_clean,
            marked_clean,
            selected_clean.clone(),
            selected_clean
                .as_ref()
                .and_then(|range| (!range.is_empty()).then_some(false)),
            caret_may_have_closed_span,
            cx,
        );
    }

    /// 取当前渲染缓存。
    pub(crate) fn current_cache(&self) -> &InlineRenderCache {
        self.projection
            .as_ref()
            .map(|projection| &projection.cache)
            .unwrap_or(&self.render_cache)
    }

    /// 按聚焦同步行内投影。
    pub(crate) fn refresh_inline_projection_for_focus(&mut self, focused: bool) {
        let supports_projection = self.edit_mode.supports_inline_projection();
        if !focused || !supports_projection {
            self.reset_inline_projection();
            return;
        }

        let projected_link_selection = self.projection.as_ref().and_then(|projection| {
            projection
                .link_run_fully_covering_range(&self.selected_range)
                .map(|run| LinkSelectionSnapshot {
                    clean_range: run.clean_range.clone(),
                    display_relative_range: self
                        .selected_range
                        .start
                        .saturating_sub(run.display_range.start)
                        ..self
                            .selected_range
                            .end
                            .saturating_sub(run.display_range.start),
                    selection_reversed: self.selection_reversed,
                })
        });
        let clean_selected = self.current_to_clean_range(self.selected_range.clone());
        let clean_marked = self
            .marked_range
            .clone()
            .map(|range| self.current_to_clean_range(range));
        if self.projection_cache_key.as_ref()
            == Some(&(
                supports_projection,
                clean_selected.clone(),
                clean_marked.clone(),
            ))
        {
            return;
        }
        let (clean_anchor, clean_focus) = self.clean_selection_anchor_focus();
        let (anchor_affinity, focus_affinity) = self.selection_endpoint_affinities();
        let collapsed_affinity = self.current_collapsed_caret_affinity();
        self.rebuild_inline_projection(clean_selected.clone(), clean_marked.clone());
        if let Some(snapshot) = projected_link_selection
            && let Some(run) = self
                .projection
                .as_ref()
                .and_then(|projection| projection.link_run_for_clean_range(&snapshot.clean_range))
        {
            let start = run.display_range.start
                + snapshot
                    .display_relative_range
                    .start
                    .min(run.display_range.len());
            let end = run.display_range.start
                + snapshot
                    .display_relative_range
                    .end
                    .min(run.display_range.len());
            self.selected_range =
                crate::text_offsets::normalize_utf8_range(self.rendered_text(), start..end);
            self.selection_reversed = snapshot.selection_reversed;
            self.collapsed_caret_affinity = CaretAffinity::Default;
        } else if clean_selected.is_empty() {
            let offset = self.clean_to_current_cursor_offset_with_affinity(
                clean_selected.start,
                collapsed_affinity,
            );
            self.assign_collapsed_selection_offset(offset, collapsed_affinity, None);
        } else {
            self.set_selection_from_clean_anchor_focus(
                clean_anchor,
                clean_focus,
                anchor_affinity,
                focus_affinity,
            );
            self.collapsed_caret_affinity = CaretAffinity::Default;
        }
        self.marked_range = clean_marked.map(|range| self.clean_to_current_range(range));
    }

    /// 清空行内投影。
    pub(crate) fn reset_inline_projection(&mut self) {
        if self.projection.is_none() {
            self.projection_cache_key = None;
            return;
        }

        let clean_marked = self
            .marked_range
            .clone()
            .map(|range| self.current_to_clean_range(range));
        let (clean_anchor, clean_focus) = self.clean_selection_anchor_focus();
        self.projection = None;
        self.projection_cache_key = None;
        self.set_selection_from_anchor_focus(clean_anchor, clean_focus);
        self.marked_range = clean_marked;
        self.collapsed_caret_affinity = CaretAffinity::Default;
        self.refresh_cached_display_text();
    }

    /// 重建行内投影。
    fn rebuild_inline_projection(
        &mut self,
        clean_selected: Range<usize>,
        clean_marked: Option<Range<usize>>,
    ) {
        self.projection_cache_key = Some((
            self.edit_mode.supports_inline_projection(),
            clean_selected.clone(),
            clean_marked.clone(),
        ));
        self.projection = InlineExpansionProjection::build(
            &self.record.title.fragments,
            clean_selected,
            clean_marked,
        );
        self.refresh_cached_display_text();
    }

    /// 取投影片段列表。
    fn projection_segments(&self) -> &[InlineExpansionSegment] {
        self.projection
            .as_ref()
            .map(|projection| projection.segments.as_slice())
            .unwrap_or(&[])
    }

    /// 取完全覆盖范围的链接投影。
    fn projected_link_run_fully_covering_range(
        &self,
        range: &Range<usize>,
    ) -> Option<&LinkExpansionRun> {
        self.projection
            .as_ref()
            .and_then(|projection| projection.link_run_fully_covering_range(range))
    }

    /// 取折叠光标的归属。
    fn collapsed_caret_affinity_for_display_offset(&self, offset: usize) -> CaretAffinity {
        self.projection
            .as_ref()
            .map(|projection| projection.collapsed_affinity_for_display_offset(offset))
            .unwrap_or(CaretAffinity::Default)
    }

    /// 当前选区锚点与焦点的归属，重建投影时用于准确恢复各端点。
    fn selection_endpoint_affinities(&self) -> (CaretAffinity, CaretAffinity) {
        let (anchor, focus) = self.selection_anchor_focus();
        (
            self.collapsed_caret_affinity_for_display_offset(anchor),
            self.collapsed_caret_affinity_for_display_offset(focus),
        )
    }

    /// 取当前折叠光标归属。
    fn current_collapsed_caret_affinity(&self) -> CaretAffinity {
        if !self.selected_range.is_empty() {
            return CaretAffinity::Default;
        }

        self.projection
            .as_ref()
            .map(|projection| projection.collapsed_affinity_for_display_offset(self.caret_offset()))
            .unwrap_or(self.collapsed_caret_affinity)
    }

    /// 同步折叠光标归属。
    fn sync_collapsed_caret_affinity(&mut self) {
        self.collapsed_caret_affinity = if self.selected_range.is_empty() {
            self.projection
                .as_ref()
                .map(|projection| {
                    projection.collapsed_affinity_for_display_offset(self.caret_offset())
                })
                .unwrap_or(CaretAffinity::Default)
        } else {
            CaretAffinity::Default
        };
    }

    /// 设置折叠选区偏移。
    pub(crate) fn assign_collapsed_selection_offset(
        &mut self,
        offset: usize,
        affinity: CaretAffinity,
        preferred_x: Option<Pixels>,
    ) {
        let clamped_offset = crate::text_offsets::floor_char_boundary(self.rendered_text(), offset);
        self.selected_range = clamped_offset..clamped_offset;
        self.selection_reversed = false;
        self.vertical_motion_x = preferred_x;
        self.collapsed_caret_affinity = affinity;
        self.sync_collapsed_caret_affinity();
    }

    /// 纯偏移转当前光标偏移。
    fn clean_to_current_cursor_offset(&self, clean: usize) -> usize {
        let Some(projection) = &self.projection else {
            return clean;
        };
        projection
            .clean_to_display_cursor
            .get(clean.min(projection.clean_to_display_cursor.len().saturating_sub(1)))
            .copied()
            .unwrap_or(clean)
    }

    /// 按归属换算光标偏移。
    fn clean_to_current_cursor_offset_with_affinity(
        &self,
        clean: usize,
        affinity: CaretAffinity,
    ) -> usize {
        let Some(projection) = &self.projection else {
            return clean;
        };
        projection
            .display_offset_for_clean_cursor(clean, affinity)
            .unwrap_or_else(|| self.clean_to_current_cursor_offset(clean))
    }

    /// 换算范围起始偏移。
    fn clean_to_current_range_start(&self, clean: usize) -> usize {
        self.clean_to_current_cursor_offset(clean)
    }

    /// 换算范围结束偏移。
    fn clean_to_current_range_end(&self, clean: usize) -> usize {
        self.clean_to_current_cursor_offset(clean)
    }

    /// 纯范围转当前范围（两端按目标文本做标量边界归一化）。
    pub(crate) fn clean_to_current_range(&self, range: Range<usize>) -> Range<usize> {
        let range =
            crate::text_offsets::normalize_utf8_range(self.render_cache.plain_text(), range);
        if range.is_empty() {
            let offset = self.clean_to_current_cursor_offset(range.start);
            let offset = crate::text_offsets::floor_char_boundary(self.rendered_text(), offset);
            offset..offset
        } else {
            crate::text_offsets::normalize_utf8_range(
                self.rendered_text(),
                self.clean_to_current_range_start(range.start)
                    ..self.clean_to_current_range_end(range.end),
            )
        }
    }

    /// 当前范围转纯范围（两端按各自文本做标量边界归一化）。
    pub(crate) fn current_to_clean_range(&self, range: Range<usize>) -> Range<usize> {
        let range = crate::text_offsets::normalize_utf8_range(self.rendered_text(), range);
        crate::text_offsets::normalize_utf8_range(
            self.render_cache.plain_text(),
            self.current_to_clean_offset(range.start)..self.current_to_clean_offset(range.end),
        )
    }

    /// 当前偏移转纯偏移。
    pub(crate) fn current_to_clean_offset(&self, offset: usize) -> usize {
        let offset = crate::text_offsets::floor_char_boundary(self.rendered_text(), offset);
        crate::text_offsets::floor_char_boundary(
            self.render_cache.plain_text(),
            self.unexpand_offset(offset),
        )
    }

    #[allow(dead_code)]
    /// 取指针目标偏移。
    pub(crate) fn pointer_target_offset(&self, offset: usize) -> usize {
        self.projection
            .as_ref()
            .map(|projection| projection.pointer_target_offset(offset))
            .unwrap_or(offset)
    }

    /// 取投影左移目标。
    pub(crate) fn projected_move_left_target(
        &self,
        offset: usize,
    ) -> Option<(usize, CaretAffinity)> {
        self.projection
            .as_ref()
            .and_then(|projection| projection.move_left_target(offset))
    }

    /// 取投影右移目标。
    pub(crate) fn projected_move_right_target(
        &self,
        offset: usize,
    ) -> Option<(usize, CaretAffinity)> {
        self.projection
            .as_ref()
            .and_then(|projection| projection.move_right_target(offset))
    }

    /// 取选区纯范围。
    pub(crate) fn selection_clean_range(&self) -> Range<usize> {
        self.current_to_clean_range(self.selected_range.clone())
    }

    /// 当前范围转 Markdown 范围。
    pub(crate) fn current_range_to_markdown_range(&self, range: Range<usize>) -> Range<usize> {
        if self.uses_raw_text_editing() || self.kind().is_code_block() {
            return crate::text_offsets::normalize_utf8_range(self.rendered_text(), range);
        }

        if let Some(link_run) = self.projected_link_run_fully_covering_range(&range) {
            let map = self.record.title.markdown_offset_map();
            let label_markdown_start = map.visible_to_markdown_offset(link_run.clean_range.start);
            let run_markdown_start =
                label_markdown_start.saturating_sub(link_run.link.open_marker().len());
            let start = run_markdown_start
                + range
                    .start
                    .saturating_sub(link_run.display_range.start)
                    .min(link_run.display_range.len());
            let end = run_markdown_start
                + range
                    .end
                    .saturating_sub(link_run.display_range.start)
                    .min(link_run.display_range.len());
            // 展示态与源码的字节布局不必然一致（引用式链接、转义、多字节
            // 标签），按展示字节增量推出的源码偏移可能停在字符中间，
            // 返回前先落到源码文本的标量边界，调用方切片才不会 panic。
            let markdown = self.record.title.serialize_markdown();
            return crate::text_offsets::normalize_utf8_range(&markdown, start..end);
        }

        if let Some(footnote_run) = self
            .projection
            .as_ref()
            .and_then(|projection| projection.footnote_run_fully_covering_range(&range))
        {
            let raw = footnote_run.footnote.raw_markdown();
            let raw_len = raw.len();
            let local_start = range
                .start
                .saturating_sub(footnote_run.display_range.start)
                .min(footnote_run.display_range.len());
            let local_end = range
                .end
                .saturating_sub(footnote_run.display_range.start)
                .min(footnote_run.display_range.len());
            let mapped_start = (raw_len * local_start) / footnote_run.display_range.len().max(1);
            let mapped_end = (raw_len * local_end) / footnote_run.display_range.len().max(1);
            let map = self.record.title.markdown_offset_map();
            let run_markdown_start = map.visible_to_markdown_offset(footnote_run.clean_range.start);
            let mapped = crate::text_offsets::normalize_utf8_range(&raw, mapped_start..mapped_end);
            return run_markdown_start + mapped.start..run_markdown_start + mapped.end;
        }

        let clean_range = self.current_to_clean_range(range);
        let mapped = self
            .record
            .title
            .markdown_offset_map()
            .visible_to_markdown_range(clean_range);
        // 展示态与源码的字节布局不必然一致（脚注比例映射、转义等），映射结果
        // 可能停在源码字符中间；调用方会直接切片/替换，故先落到源码标量边界。
        crate::text_offsets::normalize_utf8_range(&self.record.title.serialize_markdown(), mapped)
    }

    /// Markdown 范围转当前范围。
    pub(crate) fn markdown_range_to_current_range(&self, range: Range<usize>) -> Range<usize> {
        if self.uses_raw_text_editing() || self.kind().is_code_block() {
            return crate::text_offsets::normalize_utf8_range(self.rendered_text(), range);
        }

        let clean_range = self
            .record
            .title
            .markdown_offset_map()
            .markdown_to_visible_range(range);
        self.clean_to_current_range(clean_range)
    }

    /// Markdown 偏移转当前偏移。
    pub(crate) fn markdown_offset_to_current_offset(&self, offset: usize) -> usize {
        self.markdown_range_to_current_range(offset..offset).start
    }

    /// 预捕获撤销。
    pub(crate) fn prepare_undo_capture(&self, kind: UndoKind, cx: &mut Context<Self>) {
        cx.emit(BlockEvent::PrepareUndo { kind });
    }

    /// 文本内字节偏移转 UTF-16 代码单元偏移。
    pub(super) fn utf8_to_utf16_in(text: &str, offset: usize) -> usize {
        crate::text_offsets::utf8_to_utf16(text, offset)
    }

    /// 文本内 UTF-16 代码单元范围转字节范围（两端落到标量边界）。
    pub(super) fn utf16_range_to_utf8_in(text: &str, range_utf16: &Range<usize>) -> Range<usize> {
        crate::text_offsets::utf16_range_to_utf8(text, range_utf16)
    }

    /// 文本内字节范围转 UTF-16 代码单元范围。
    pub(super) fn utf8_range_to_utf16_in(text: &str, range: &Range<usize>) -> Range<usize> {
        crate::text_offsets::utf8_range_to_utf16(text, range)
    }

    /// 检测编辑后标题中的 Markdown 快捷前缀，并据此转换块类型
    ///（如 `"- "` 转无序列表项）。
    /// 仅当当前类型为 [`BlockKind::Paragraph`] 时触发。
    /// 返回可能更新的类型、去掉前缀的标题、新光标偏移及去掉的前缀长度。
    fn normalize_after_title_edit(
        &self,
        mut next_title: InlineTextTree,
        cursor: usize,
    ) -> (BlockKind, InlineTextTree, usize, usize) {
        if self.is_table_cell() {
            return (self.kind(), next_title, cursor, 0);
        }

        if !self.uses_raw_text_editing() && self.kind() == BlockKind::Paragraph {
            let plain_text = next_title.plain_text();
            if let Some((kind, prefix_len)) = BlockKind::detect_block_shortcut(&plain_text) {
                next_title.strip_visible_prefix(prefix_len);
                return (
                    kind,
                    next_title,
                    cursor.saturating_sub(prefix_len),
                    prefix_len,
                );
            }
        }

        if !self.uses_raw_text_editing() && self.kind() == BlockKind::BulletedListItem {
            let plain_text = next_title.plain_text();
            if let Some((checked, prefix_len)) = BlockKind::parse_task_prefix(&plain_text) {
                next_title.strip_visible_prefix(prefix_len);
                return (
                    BlockKind::TaskListItem { checked },
                    next_title,
                    cursor.saturating_sub(prefix_len),
                    prefix_len,
                );
            }
        }

        (self.kind(), next_title, cursor, 0)
    }

    /// 判断引用行是否开启块语法。
    fn quote_line_starts_block_syntax(line: &str) -> bool {
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
    }

    /// 判断多行引用编辑是否需重解析。
    fn multiline_quote_edit_requires_reparse(text: &str) -> bool {
        text.split('\n')
            .skip(1)
            .any(Self::quote_line_starts_block_syntax)
    }

    /// 按快捷前缀调整范围。
    fn adjust_range_for_shortcut(range: &Range<usize>, removed_prefix_len: usize) -> Range<usize> {
        range.start.saturating_sub(removed_prefix_len)..range.end.saturating_sub(removed_prefix_len)
    }

    /// 收集触及范围的投影样式。
    fn projected_styles_touching_display_range(
        &self,
        display_range: &Range<usize>,
    ) -> Vec<(usize, InlineStyleFlag)> {
        let mut targets = Vec::new();
        for segment in self.projection_segments() {
            let touches = display_range.start < segment.display_range.end
                && segment.display_range.start < display_range.end;
            if touches
                && matches!(
                    segment.kind,
                    ExpandedInlineSegmentKind::OpeningDelimiter(_)
                        | ExpandedInlineSegmentKind::ClosingDelimiter(_)
                )
            {
                let kind = match segment.kind {
                    ExpandedInlineSegmentKind::OpeningDelimiter(kind)
                    | ExpandedInlineSegmentKind::ClosingDelimiter(kind) => kind,
                    _ => continue,
                };
                if let Some(flag) = kind.style_flag() {
                    let target = (segment.fragment_index, flag);
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
            }
        }
        targets
    }

    /// 取片段前的纯偏移。
    fn clean_offset_before_fragment_index(fragments: &[InlineFragment], index: usize) -> usize {
        fragments
            .iter()
            .take(index)
            .map(|fragment| fragment.text.len())
            .sum()
    }

    /// 判断替换是否为纯链接串。
    fn replacement_is_pure_link_run(fragments: &[InlineFragment]) -> bool {
        let Some(first_link) = fragments
            .first()
            .and_then(|fragment| fragment.link.as_ref())
        else {
            return false;
        };

        fragments
            .iter()
            .all(|fragment| fragment.link.as_ref() == Some(first_link))
    }

    /// 应用链接投影编辑。
    fn apply_link_projection_edit(
        &mut self,
        link_run: &LinkExpansionRun,
        visible_range: Range<usize>,
        new_text: &str,
        selected_range_relative: Option<Range<usize>>,
        mark_inserted_text: bool,
        cx: &mut Context<Self>,
    ) {
        let local_visible_range = visible_range.start - link_run.display_range.start
            ..visible_range.end - link_run.display_range.start;
        let local_display_text = self.rendered_text()[link_run.display_range.clone()].to_string();
        let local_tree = InlineTextTree::plain(local_display_text);
        let local_result = local_tree.replace_plain_range_with_link_references(
            local_visible_range.clone(),
            new_text,
            InlineInsertionAttributes::default(),
            &self.link_reference_definitions,
        );
        let replacement_fragments = local_result.tree.fragments.clone();

        let replacement_start = link_run.start_fragment_index;
        let replacement_clean_start = Self::clean_offset_before_fragment_index(
            &self.record.title.fragments,
            replacement_start,
        );
        let mut next_title = self.record.title.clone();
        next_title.replace_fragment_range(
            link_run.start_fragment_index..link_run.end_fragment_index,
            replacement_fragments.clone(),
        );

        if Self::replacement_is_pure_link_run(&replacement_fragments) {
            let old_kind = self.record.kind.clone();
            let old_title = self.record.title.clone();
            self.record.set_title(next_title.clone());
            self.sync_edit_mode_from_kind();
            self.refresh_render_cache();

            let replacement_visible_len = replacement_fragments
                .iter()
                .map(|fragment| fragment.text.len())
                .sum::<usize>();
            let selected_clean =
                replacement_clean_start..replacement_clean_start + replacement_visible_len;
            self.rebuild_inline_projection(selected_clean.clone(), None);

            let local_selected = selected_range_relative.clone().unwrap_or_else(|| {
                let cursor = local_visible_range.start + new_text.len();
                cursor..cursor
            });
            if let Some(projected_link_run) = self.projection.as_ref().and_then(|projection| {
                projection
                    .link_runs
                    .iter()
                    .find(|run| run.clean_range == selected_clean)
            }) {
                let start = projected_link_run.display_range.start
                    + local_selected
                        .start
                        .min(projected_link_run.display_range.len());
                let end = projected_link_run.display_range.start
                    + local_selected
                        .end
                        .min(projected_link_run.display_range.len());
                self.selected_range =
                    crate::text_offsets::normalize_utf8_range(self.rendered_text(), start..end);
                self.selection_reversed = false;
                self.marked_range = if mark_inserted_text && !new_text.is_empty() {
                    Some(self.selected_range.clone())
                } else {
                    None
                };
                self.collapsed_caret_affinity = CaretAffinity::Default;
                self.cursor_blink_epoch = Instant::now();
                self.clear_vertical_motion();
                if self.record.kind != old_kind || self.record.title != old_title {
                    cx.emit(BlockEvent::Changed);
                }
                cx.notify();
                return;
            }
        }

        let local_selected = selected_range_relative.as_ref().map(|relative| {
            let absolute = local_visible_range.start + relative.start
                ..local_visible_range.start + relative.end;
            local_result.map_range(&absolute)
        });
        let cursor = local_selected
            .as_ref()
            .map(|range| range.end)
            .unwrap_or_else(|| local_result.map_offset(local_visible_range.start + new_text.len()));
        let prefix = replacement_clean_start;
        let selected_clean = local_selected.map(|range| prefix + range.start..prefix + range.end);
        let marked_clean = if mark_inserted_text && !new_text.is_empty() {
            let inserted_range =
                local_visible_range.start..local_visible_range.start + new_text.len();
            let mapped = local_result.map_range(&inserted_range);
            Some(prefix + mapped.start..prefix + mapped.end)
        } else {
            None
        };
        self.apply_title_edit(
            next_title,
            prefix + cursor,
            marked_clean,
            selected_clean.clone(),
            selected_clean
                .as_ref()
                .and_then(|range| (!range.is_empty()).then_some(false)),
            false,
            cx,
        );
    }

    /// 取当前偏移的插入属性。
    fn insertion_attributes_for_current_offset(
        &self,
        current_offset: usize,
    ) -> InlineInsertionAttributes {
        if self.uses_raw_text_editing() {
            return InlineInsertionAttributes::default();
        }

        if self.projection.is_none() {
            return self.record.title.insertion_attributes_at(current_offset);
        }

        for segment in self.projection_segments() {
            match segment.kind {
                ExpandedInlineSegmentKind::StyledText
                    if current_offset >= segment.display_range.start
                        && current_offset <= segment.display_range.end =>
                {
                    let fragment = &self.record.title.fragments[segment.fragment_index];
                    return InlineInsertionAttributes {
                        style: fragment.style,
                        html_style: fragment.html_style,
                        link: fragment.link.clone(),
                        footnote: fragment.footnote.clone(),
                        math: None,
                    };
                }
                ExpandedInlineSegmentKind::OpeningDelimiter(_)
                    if current_offset == segment.display_range.end =>
                {
                    let fragment = &self.record.title.fragments[segment.fragment_index];
                    return InlineInsertionAttributes {
                        style: fragment.style,
                        html_style: fragment.html_style,
                        link: fragment.link.clone(),
                        footnote: fragment.footnote.clone(),
                        math: None,
                    };
                }
                ExpandedInlineSegmentKind::ClosingDelimiter(_)
                    if current_offset == segment.display_range.start =>
                {
                    let fragment = &self.record.title.fragments[segment.fragment_index];
                    return InlineInsertionAttributes {
                        style: fragment.style,
                        html_style: fragment.html_style,
                        link: fragment.link.clone(),
                        footnote: fragment.footnote.clone(),
                        math: None,
                    };
                }
                // 光标紧贴区间外侧（结束符后或起始符前）时插入纯文本，
                // 避免被吸回区间内，与代码和删除线行为一致。
                ExpandedInlineSegmentKind::ClosingDelimiter(_)
                    if current_offset == segment.display_range.end =>
                {
                    return InlineInsertionAttributes::default();
                }
                ExpandedInlineSegmentKind::OpeningDelimiter(_)
                    if current_offset == segment.display_range.start =>
                {
                    return InlineInsertionAttributes::default();
                }
                ExpandedInlineSegmentKind::LinkTargetText => {
                    if let Some(link_group) = segment.link_group
                        && let Some(link_run) = self
                            .projection
                            .as_ref()
                            .and_then(|projection| projection.link_runs.get(link_group))
                        && current_offset >= link_run.target_display_range.start
                        && current_offset <= link_run.target_display_range.end
                    {
                        return InlineInsertionAttributes::default();
                    }
                }
                _ => {}
            }
        }

        self.record
            .title
            .insertion_attributes_at(self.current_to_clean_offset(current_offset))
    }

    /// 取单片段的插入属性。
    fn attributes_for_fragment(fragment: &InlineFragment) -> InlineInsertionAttributes {
        InlineInsertionAttributes {
            style: fragment.style,
            html_style: fragment.html_style,
            link: fragment.link.clone(),
            footnote: fragment.footnote.clone(),
            math: None,
        }
    }

    /// 取可见范围的替换属性。
    fn replacement_attributes_for_visible_range(
        &self,
        visible_range: &Range<usize>,
    ) -> InlineInsertionAttributes {
        if self.uses_raw_text_editing() {
            return InlineInsertionAttributes::default();
        }

        if visible_range.is_empty() {
            return self.insertion_attributes_for_current_offset(visible_range.start);
        }

        if self.projection.is_some() {
            return self
                .projected_replacement_attributes_for_visible_range(visible_range)
                .unwrap_or_default();
        }

        self.fragment_attributes_for_clean_range(self.current_to_clean_range(visible_range.clone()))
            .unwrap_or_default()
    }

    /// 取投影下可见范围的替换属性。
    fn projected_replacement_attributes_for_visible_range(
        &self,
        visible_range: &Range<usize>,
    ) -> Option<InlineInsertionAttributes> {
        self.projection_segments().iter().find_map(|segment| {
            (segment.kind == ExpandedInlineSegmentKind::StyledText
                && segment.display_range.start <= visible_range.start
                && visible_range.end <= segment.display_range.end)
                .then(|| {
                    self.record
                        .title
                        .fragments
                        .get(segment.fragment_index)
                        .map(Self::attributes_for_fragment)
                })
                .flatten()
        })
    }

    /// 取纯范围的片段属性。
    fn fragment_attributes_for_clean_range(
        &self,
        clean_range: Range<usize>,
    ) -> Option<InlineInsertionAttributes> {
        if clean_range.is_empty() {
            return None;
        }

        let mut cursor = 0usize;
        for fragment in &self.record.title.fragments {
            let fragment_start = cursor;
            let fragment_end = fragment_start + fragment.text.len();
            if fragment_start <= clean_range.start && clean_range.end <= fragment_end {
                return Some(Self::attributes_for_fragment(fragment));
            }
            cursor = fragment_end;
        }

        None
    }

    /// 为块应用新标题，执行快捷检测并更新渲染缓存、光标与选区状态。
    /// 若类型或标题实际变化则发出 [`BlockEvent::Changed`]。
    // 编辑管线按扁平传参约定透传光标与选区快照，打包结构体反而引入生命周期复杂度。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_title_edit(
        &mut self,
        next_title: InlineTextTree,
        cursor_clean: usize,
        marked_range_clean: Option<Range<usize>>,
        selected_range_clean: Option<Range<usize>>,
        selected_range_reversed: Option<bool>,
        caret_may_have_closed_span: bool,
        cx: &mut Context<Self>,
    ) {
        let old_kind = self.record.kind.clone();
        let old_title = self.record.title.clone();
        let old_title_was_empty = old_title.plain_text().is_empty();
        let mut collapsed_affinity = self.current_collapsed_caret_affinity();
        let keep_projection =
            self.projection.is_some() && self.edit_mode.supports_inline_projection();

        let (next_kind, normalized_title, adjusted_cursor, shortcut_removed_len) =
            self.normalize_after_title_edit(next_title, cursor_clean);
        let should_restart_numbered_list = old_kind == BlockKind::Paragraph
            && old_title_was_empty
            && self.list_group_separator_candidate
            && next_kind == BlockKind::NumberedListItem;

        let next_marked_clean = marked_range_clean
            .as_ref()
            .map(|range| Self::adjust_range_for_shortcut(range, shortcut_removed_len));
        let next_selected_clean = selected_range_clean
            .as_ref()
            .map(|range| Self::adjust_range_for_shortcut(range, shortcut_removed_len))
            .unwrap_or_else(|| adjusted_cursor..adjusted_cursor);

        self.record.set_kind(next_kind);
        self.record.set_title(normalized_title);
        self.numbered_list_restart_requested = should_restart_numbered_list;
        self.sync_edit_mode_from_kind();
        self.refresh_render_cache();
        // 已有投影或本次编辑可能闭合分隔符（产生需投影的新区间）时重建。
        if self.edit_mode.supports_inline_projection()
            && (keep_projection || caret_may_have_closed_span)
        {
            self.rebuild_inline_projection(next_selected_clean.clone(), next_marked_clean.clone());
        }

        // 若编辑闭合了区间（分隔符被吸收），把光标放在新结束标记后，使后续输入为纯文本。
        if caret_may_have_closed_span
            && next_selected_clean.is_empty()
            && self.projection.as_ref().is_some_and(|projection| {
                projection.caret_closes_span_at_clean(next_selected_clean.start)
            })
        {
            collapsed_affinity = CaretAffinity::OuterEnd;
        }

        self.marked_range = next_marked_clean
            .clone()
            .map(|range| self.clean_to_current_range(range));
        if next_selected_clean.is_empty() {
            let offset = self.clean_to_current_cursor_offset_with_affinity(
                next_selected_clean.start,
                collapsed_affinity,
            );
            self.assign_collapsed_selection_offset(offset, collapsed_affinity, None);
        } else {
            self.selected_range = self.clean_to_current_range(next_selected_clean);
            self.selection_reversed = selected_range_reversed.unwrap_or(self.selection_reversed);
            self.collapsed_caret_affinity = CaretAffinity::Default;
        }
        self.cursor_blink_epoch = Instant::now();
        self.clear_vertical_motion();

        if self.record.kind != old_kind || self.record.title != old_title {
            cx.emit(BlockEvent::Changed);
        }
        cx.notify();
    }

    /// 按可见坐标替换文本：把 `new_text` 拼入标题的 `visible_range`，
    /// 重解析行内标记并更新光标状态。
    /// `mark_inserted_text` 为真时插入文本成为输入法组合区间。
    ///
    /// 块处于编辑展开模式（代码段显示反引号分隔符）时，
    /// 先把 `visible_range` 映射回原树的偏移空间。
    pub(crate) fn replace_text_in_visible_range(
        &mut self,
        visible_range: Range<usize>,
        new_text: &str,
        selected_range_relative: Option<Range<usize>>,
        mark_inserted_text: bool,
        cx: &mut Context<Self>,
    ) {
        if self.kind().is_separator() && !self.uses_raw_text_editing() {
            return;
        }

        // 进入编辑的区间可能来自 IME 的 UTF-16 换算或外部调用，先按标量边界
        // 归一化，后续切片与投影映射才不会落在多字节字符中间。
        let visible_range =
            crate::text_offsets::normalize_utf8_range(self.rendered_text(), visible_range);
        let selected_range_relative = selected_range_relative
            .map(|range| crate::text_offsets::normalize_utf8_range(new_text, range));

        let inserted_attributes = self.replacement_attributes_for_visible_range(&visible_range);

        // 行内 `[label](url)` 链接经投影源码往返，即使块其余部分保留源码
        //（如同时含行内公式），也走链接投影编辑，使各块锚文本编辑一致；
        // 引用与自动链接仍走下方的源码空格路径，以保留原始拼写。
        if !self.uses_raw_text_editing()
            && let Some(link_run) = self
                .projected_link_run_fully_covering_range(&visible_range)
                .filter(|run| !run.link.is_source_preserving())
                .cloned()
        {
            self.apply_link_projection_edit(
                &link_run,
                visible_range,
                new_text,
                selected_range_relative,
                mark_inserted_text,
                cx,
            );
            return;
        }

        if self.should_use_markdown_space_link_edit() {
            self.apply_markdown_space_title_edit(
                visible_range,
                new_text,
                selected_range_relative,
                mark_inserted_text,
                cx,
            );
            return;
        }

        // 在行内链接区间外编辑时，若由折叠后的可见文本重建树，
        // 会因丢失 `[label](url)` 标记而静默丢链，故走源码空格编辑以保往返。
        if !self.uses_raw_text_editing() && self.record.title.has_inline_links() {
            self.apply_markdown_space_title_edit(
                visible_range,
                new_text,
                selected_range_relative,
                mark_inserted_text,
                cx,
            );
            return;
        }

        let clean_range = self.current_to_clean_range(visible_range.clone());
        let mut base_title = self.record.title.clone();
        let overlaps_delimiters = self.projection.is_some() && !self.uses_raw_text_editing();
        if overlaps_delimiters {
            let touched_styles = self.projected_styles_touching_display_range(&visible_range);
            if !touched_styles.is_empty() {
                base_title.strip_styles_on_fragments(&touched_styles);
            }
        }

        let base_visible_len = base_title.plain_text().len();
        let replaced_text = self.rendered_text()[visible_range.clone()].to_string();
        let result = if self.uses_raw_text_editing() {
            base_title.replace_plain_range_raw(
                clean_range.clone(),
                new_text,
                InlineInsertionAttributes::default(),
            )
        } else {
            base_title.replace_plain_range_with_link_references(
                clean_range.clone(),
                new_text,
                inserted_attributes,
                &self.link_reference_definitions,
            )
        };

        // 重解析把分隔符吸收为样式即闭合了区间，纯文本比预期短，跳过输入法与删除。
        let expected_visible_len =
            base_visible_len.saturating_sub(clean_range.len()) + new_text.len();
        let caret_may_have_closed_span = !self.uses_raw_text_editing()
            && !new_text.is_empty()
            && !mark_inserted_text
            && result.tree.plain_text().len() < expected_visible_len;
        let quote_structure_edit = !self.uses_raw_text_editing()
            && self.quote_depth > 0
            && (new_text.contains('\n')
                || replaced_text.contains('\n')
                || (self.kind() == BlockKind::Quote
                    && Self::multiline_quote_edit_requires_reparse(&result.tree.plain_text())));
        if quote_structure_edit {
            self.quote_reparse_requested = true;
        }
        let inserted_range = clean_range.start..clean_range.start + new_text.len();
        let marked_range = if mark_inserted_text && !new_text.is_empty() {
            Some(result.map_range(&inserted_range))
        } else {
            None
        };
        let selected_range = selected_range_relative.as_ref().map(|relative| {
            let absolute = clean_range.start + relative.start..clean_range.start + relative.end;
            result.map_range(&absolute)
        });
        let cursor = selected_range
            .as_ref()
            .map(|range| range.end)
            .unwrap_or_else(|| result.map_offset(clean_range.start + new_text.len()));

        self.apply_title_edit(
            result.tree,
            cursor,
            marked_range,
            selected_range.clone(),
            selected_range
                .as_ref()
                .and_then(|range| (!range.is_empty()).then_some(false)),
            caret_may_have_closed_span,
            cx,
        );
    }

    /// 标记块已修改。
    pub(super) fn mark_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_edit_mode_from_kind();
        self.refresh_render_cache();
        self.cursor_blink_epoch = Instant::now();
        self.clear_vertical_motion();
        cx.emit(BlockEvent::Changed);
        cx.notify();
    }

    /// 转为段落。
    pub(crate) fn make_paragraph(&mut self, cx: &mut Context<Self>) {
        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        self.record.set_kind(BlockKind::Paragraph);
        self.quote_reparse_requested = false;
        self.mark_changed(cx);
    }

    /// 转为指定级别的标题（文本保留在标题中，与转为段落同流程）。
    pub(crate) fn convert_to_heading(&mut self, level: u8, cx: &mut Context<Self>) {
        let level = level.clamp(1, 6);
        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        self.record.set_kind(BlockKind::Heading { level });
        self.quote_reparse_requested = false;
        self.mark_changed(cx);
    }

    /// 转为分隔线。
    pub(crate) fn convert_to_separator(&mut self, cx: &mut Context<Self>) {
        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        self.make_separator();
        cx.emit(BlockEvent::Changed);
        cx.notify();
    }

    /// 原地把本块转为分隔线，不发事件也不捕获撤销，供已管理这些的上层流程复用。
    pub(crate) fn make_separator(&mut self) {
        self.reset_inline_projection();
        self.record.set_kind(BlockKind::Separator);
        self.record.set_title(InlineTextTree::plain(String::new()));
        self.quote_reparse_requested = false;
        self.sync_edit_mode_from_kind();
        self.refresh_render_cache();
        self.assign_collapsed_selection_offset(0, CaretAffinity::Default, None);
        self.marked_range = None;
        self.cursor_blink_epoch = Instant::now();
        self.clear_vertical_motion();
    }

    /// 进入代码块。
    pub(crate) fn open_code_block(
        &mut self,
        language: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        self.reset_inline_projection();
        self.record.set_kind(BlockKind::CodeBlock { language });
        self.record.set_title(InlineTextTree::plain(String::new()));
        self.quote_reparse_requested = false;
        self.sync_edit_mode_from_kind();
        self.refresh_render_cache();
        self.assign_collapsed_selection_offset(0, CaretAffinity::Default, None);
        self.marked_range = None;
        self.cursor_blink_epoch = Instant::now();
        self.clear_vertical_motion();
        cx.emit(BlockEvent::Changed);
        cx.notify();
    }

    /// 把当前段落转为展示公式块。`body` 为围栏间公式源码（全新 `$$` 块为空），光标落到正文行首。
    pub(crate) fn open_math_block(&mut self, body: &str, cx: &mut Context<Self>) {
        let source = format!("$$\n{body}\n$$");
        let cursor = "$$\n".len();

        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        self.reset_inline_projection();
        self.record.set_kind(BlockKind::MathBlock);
        self.record.set_title(InlineTextTree::plain(source));
        self.quote_reparse_requested = false;
        self.sync_edit_mode_from_kind();
        self.refresh_render_cache();
        self.assign_collapsed_selection_offset(cursor, CaretAffinity::Default, None);
        self.marked_range = None;
        self.cursor_blink_epoch = Instant::now();
        self.clear_vertical_motion();
        cx.emit(BlockEvent::Changed);
        cx.notify();
    }

    /// 直接在片段树上切换样式标志，全程不操作原始标记字符，
    /// 由选区决定翻转哪些片段的 [`InlineStyle`] 标志，
    /// 导出时再由序列化器转回标记。
    pub(crate) fn toggle_span_format(&mut self, format: InlineFormat, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() || self.uses_raw_text_editing() {
            return;
        }

        let mut next_title = self.record.title.clone();
        let selection = self.selection_clean_range();
        let changed = match format {
            InlineFormat::Bold => next_title.toggle_bold(selection.clone()),
            InlineFormat::Italic => next_title.toggle_italic(selection.clone()),
            InlineFormat::Underline => next_title.toggle_underline(selection.clone()),
            InlineFormat::Code => next_title.toggle_code(selection.clone()),
        };
        if !changed {
            return;
        }

        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        self.apply_title_edit(
            next_title,
            selection.end,
            None,
            Some(selection),
            Some(self.selection_reversed),
            false,
            cx,
        );
    }

    /// 无选中时插入成对格式标记（加粗 `****`、行内代码 `` ` `` 等），
    /// 光标落入标记中间方便直接输入；与 [`Self::toggle_span_format`] 互补。
    ///
    /// 标记以源码形态存入（跳过标记规范化），用户后续输入会走正常
    /// 输入链路重新解析，如 `**hello**` 会转为真正的加粗。源码编辑块
    /// 同样适用（源码即原文）；分隔线块与已有选中的情况直接返回。
    pub(crate) fn insert_format_markers(&mut self, format: InlineFormat, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() || self.kind().is_separator() {
            return;
        }
        let (open, close) = match format {
            InlineFormat::Bold => ("**", "**"),
            InlineFormat::Italic => ("*", "*"),
            InlineFormat::Underline => ("<u>", "</u>"),
            InlineFormat::Code => ("`", "`"),
        };
        let caret = self.selection_clean_range().start;
        let base_title = self.record.title.clone();
        let result = base_title.replace_plain_range_raw(
            caret..caret,
            &format!("{open}{close}"),
            InlineInsertionAttributes::default(),
        );
        // 源码形态插入不做规范化，偏移线性后移：光标落在首尾标记之间。
        let middle = caret + open.len();
        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        self.apply_title_edit(
            result.tree,
            middle,
            None,
            Some(middle..middle),
            Some(false),
            false,
            cx,
        );
    }

    /// 取当前行布局与偏移。
    fn current_line_layout_and_offset(&self) -> Option<(&WrappedLine, usize)> {
        let lines = self.last_layout.as_ref()?;
        let text = self.rendered_text();
        let ranges = super::element::hard_line_ranges(text);
        let (line_idx, offset_in_line) =
            super::element::line_index_for_offset(&ranges, self.caret_offset());
        Some((lines.get(line_idx)?, offset_in_line))
    }

    /// 取垂直移动锚点横坐标。
    pub(super) fn vertical_anchor_x(&self) -> Pixels {
        self.vertical_motion_x
            .or_else(|| {
                self.current_line_layout_and_offset()
                    .and_then(|(layout, offset_in_line)| {
                        super::element::position_for_offset(
                            layout,
                            offset_in_line,
                            self.last_line_height,
                            true,
                        )
                        .map(|position| position.x)
                    })
            })
            .unwrap_or(px(0.0))
    }

    /// 尝试在当前块内上下移动一个视觉行，方向小于零为上。
    /// 若已在首末行则返回假，以便编辑器转移焦点。
    pub(super) fn move_cursor_vertically(
        &mut self,
        direction: i32,
        preferred_x: Pixels,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.vertical_move_target(direction, preferred_x) else {
            return false;
        };
        self.move_to_with_preferred_x(target, Some(preferred_x), cx);
        true
    }

    /// Shift+上/下：按视觉行扩展选区（焦点端移动、锚点端固定）。
    /// 已在首末行时返回假，由编辑器接续到相邻块。
    pub(crate) fn select_vertically(
        &mut self,
        direction: i32,
        preferred_x: Pixels,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.vertical_move_target(direction, preferred_x) else {
            return false;
        };
        self.select_to(target, cx);
        // `select_to` 会清掉纵向移动锚点，跨多行时按本行重新记下期望横坐标。
        self.vertical_motion_x = Some(preferred_x);
        cx.notify();
        true
    }

    /// 计算上下移动一个视觉行后的落点偏移；已在首末行时返回空。
    pub(crate) fn vertical_move_target(
        &self,
        direction: i32,
        preferred_x: Pixels,
    ) -> Option<usize> {
        let lines = self.last_layout.as_ref()?;
        let text = self.rendered_text();
        let ranges = super::element::hard_line_ranges(text);
        let (current_line_idx, offset_in_line) =
            super::element::line_index_for_offset(&ranges, self.caret_offset());
        let current_layout = lines.get(current_line_idx)?;
        let current_position = super::element::position_for_offset(
            current_layout,
            offset_in_line,
            self.last_line_height,
            true,
        )?;

        let current_y =
            super::element::wrapped_line_top(lines, self.last_line_height, current_line_idx)
                + current_position.y;
        let target_y = if direction < 0 {
            current_y - self.last_line_height + self.last_line_height / 2.0
        } else {
            current_y + self.last_line_height + self.last_line_height / 2.0
        };
        if target_y < px(0.0) {
            return None;
        }

        let total_height = lines.iter().fold(px(0.0), |height, line| {
            height + super::element::wrapped_line_height(line, self.last_line_height)
        });
        if target_y >= total_height {
            return None;
        }

        let (target_line_idx, target_y_in_line) =
            super::element::wrapped_line_for_y(lines, self.last_line_height, target_y)?;
        let target_layout = &lines[target_line_idx];
        let target_point = point(preferred_x, target_y_in_line);
        let target_offset_in_line =
            match target_layout.closest_index_for_position(target_point, self.last_line_height) {
                Ok(idx) | Err(idx) => idx,
            };
        Some(ranges[target_line_idx].start + target_offset_in_line)
    }

    /// 计算焦点从上方或下方进入本块时光标落点，
    /// 用纵向移动锚点在不同高度块间保持横向位置。
    pub fn vertical_focus_offset(
        &self,
        prefer_last_line: bool,
        preferred_x: Option<Pixels>,
    ) -> usize {
        let Some(lines) = self.last_layout.as_ref() else {
            return if prefer_last_line {
                self.plain_len()
            } else {
                0
            };
        };

        let text = self.rendered_text();
        let ranges = super::element::hard_line_ranges(text);
        let target_line_idx = if prefer_last_line { lines.len() - 1 } else { 0 };
        let target_layout = &lines[target_line_idx];
        let target_x = preferred_x.unwrap_or(px(0.0));
        let target_y = if prefer_last_line {
            super::element::wrapped_line_height(target_layout, self.last_line_height)
                - self.last_line_height / 2.0
        } else {
            self.last_line_height / 2.0
        };

        let offset_in_line = match target_layout
            .closest_index_for_position(point(target_x, target_y), self.last_line_height)
        {
            Ok(idx) | Err(idx) => idx,
        };
        ranges[target_line_idx].start + offset_in_line
    }

    /// 带期望横坐标移动。
    pub fn move_to_with_preferred_x(
        &mut self,
        offset: usize,
        preferred_x: Option<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.assign_collapsed_selection_offset(offset, CaretAffinity::Default, preferred_x);
        self.cursor_blink_epoch = Instant::now();
        cx.notify();
    }

    /// 启动光标闪烁循环：闪烁阶段每三十三毫秒的后台定时器调用 `cx.notify()` 重绘光标，
    /// 仅在光标透明度实际变化时重绘。每次重置 `cursor_blink_epoch`
    ///（方向键/输入触发）后的前半秒透明度固定为一，此时直接睡到该时刻：
    /// 打字期间逐 33ms 醒来只会做一次无可见变化的判定，纯属空转。
    ///
    /// 块失焦时自动取消闪烁任务（句柄在 [`Block::render`] 中丢弃）。
    pub(super) fn start_cursor_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_blink_epoch = Instant::now();
        self.cursor_blink_task = Some(cx.spawn(
            async |this: WeakEntity<Block>, cx: &mut AsyncApp| {
                /// 透明度恒为一的保持时长（秒），与 [`Block::caret_opacity`] 一致。
                const HOLD: f32 = 0.5;
                loop {
                    let elapsed = update_entity_forgiving(&this, cx, |this: &mut Block, _cx| {
                        this.cursor_blink_epoch.elapsed().as_secs_f32()
                    });
                    match elapsed {
                        // 实体已释放时结束闪烁循环；借用冲突（标题栏拖拽等嵌套派发）
                        // 仅跳过本拍，下拍重试（见 `update_entity_forgiving`）。
                        Some(Err(_)) => break,
                        Some(Ok(elapsed)) if elapsed < HOLD => {
                            cx.background_executor()
                                .timer(Duration::from_secs_f32((HOLD - elapsed).max(0.005)))
                                .await;
                            continue;
                        }
                        _ => {}
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(33))
                        .await;
                    let _ = update_entity_forgiving(
                        &this,
                        cx,
                        |this: &mut Block, cx: &mut Context<Block>| {
                            if this.cursor_blink_epoch.elapsed().as_secs_f32() >= HOLD {
                                cx.notify();
                            }
                        },
                    );
                }
            },
        ));
    }

    /// 基于余弦的平滑闪烁：前半秒完全不透明，之后以约一秒为周期振荡。
    pub fn caret_opacity(&self) -> f32 {
        let elapsed = self.cursor_blink_epoch.elapsed().as_secs_f32();
        if elapsed < 0.5 {
            return 1.0;
        }
        let t = elapsed - 0.5;
        (f32::cos(t * std::f32::consts::TAU) + 1.0) / 2.0
    }

    /// 取光标偏移。
    pub fn caret_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    /// 结束指针选区会话。
    pub(crate) fn end_pointer_selection_session(&mut self) -> bool {
        let changed = self.is_selecting || self.code_language_is_selecting;
        self.is_selecting = false;
        self.code_language_is_selecting = false;
        changed
    }

    /// 取选区锚点与焦点。
    pub(crate) fn selection_anchor_focus(&self) -> (usize, usize) {
        if self.selection_reversed {
            (self.selected_range.end, self.selected_range.start)
        } else {
            (self.selected_range.start, self.selected_range.end)
        }
    }

    /// 取纯选区锚点与焦点。
    fn clean_selection_anchor_focus(&self) -> (usize, usize) {
        let (anchor, focus) = self.selection_anchor_focus();
        (
            self.current_to_clean_offset(anchor),
            self.current_to_clean_offset(focus),
        )
    }

    /// 按锚点焦点设置选区（端点向下取整到字符边界）。
    fn set_selection_from_anchor_focus(&mut self, anchor: usize, focus: usize) {
        let text = self.rendered_text();
        let clamped_anchor = crate::text_offsets::floor_char_boundary(text, anchor);
        let clamped_focus = crate::text_offsets::floor_char_boundary(text, focus);
        self.selected_range = clamped_anchor.min(clamped_focus)..clamped_anchor.max(clamped_focus);
        self.selection_reversed = !self.selected_range.is_empty() && clamped_focus < clamped_anchor;
    }

    /// 按纯锚点焦点设置选区。
    fn set_selection_from_clean_anchor_focus(
        &mut self,
        anchor: usize,
        focus: usize,
        anchor_affinity: CaretAffinity,
        focus_affinity: CaretAffinity,
    ) {
        // 各端点按自身归属映射回去。多个展示位置可共享同一纯偏移
        //（行尾链接的 `](url)` 均塌缩到锚文本末）， plain 映射会把
        // 结束符后的端点拉回内部，保留捕获的归属可在重建后保持位置。
        self.set_selection_from_anchor_focus(
            self.clean_to_current_cursor_offset_with_affinity(anchor, anchor_affinity),
            self.clean_to_current_cursor_offset_with_affinity(focus, focus_affinity),
        );
    }

    /// 移动光标。
    pub fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.move_to_with_preferred_x(offset, None, cx);
    }

    /// 扩展选区到指定偏移（向下取整到字符边界）。
    pub fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let clamped_offset = crate::text_offsets::floor_char_boundary(self.rendered_text(), offset);
        if self.selection_reversed {
            self.selected_range.start = clamped_offset;
        } else {
            self.selected_range.end = clamped_offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.cursor_blink_epoch = Instant::now();
        self.clear_vertical_motion();
        self.sync_collapsed_caret_affinity();
        cx.notify();
    }
    /// 双击选词：在展示文本上按 UAX#29 词边界选中光标所在词。
    ///
    /// 偏移以展示文本字节计；落在词尾边界时归入后一个词，越界钳入文本；
    /// 多字节字符中间的偏移向下取整到字符边界。中日韩字符按 UAX#29
    /// 逐字成词（与主流编辑器一致）。
    pub fn select_word_at_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some(range) = self.word_range_at(offset) else {
            return;
        };
        self.apply_absolute_selection(range, cx);
    }

    /// 三击选整行：选中光标所在硬换行，附带末尾换行符（末行除外），
    /// 与主流编辑器的三击行为一致。
    pub fn select_line_at_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some(range) = self.line_range_at(offset) else {
            return;
        };
        self.apply_absolute_selection(range, cx);
    }

    /// 双击后按住拖拽：活动端按词吸附，锚点端固定。
    ///
    /// 锚点取自当前选区的固定端（正向取 start，反向取 end），拖过锚点
    /// 后自动翻转，与字符拖拽的方向语义一致。
    pub fn extend_word_selection_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some(word) = self.word_range_at(offset) else {
            return;
        };
        self.extend_toward(offset, word.start, word.end, cx);
    }

    /// 三击后按住拖拽：活动端按行吸附，锚点端固定。
    pub fn extend_line_selection_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some(line) = self.line_range_at(offset) else {
            return;
        };
        self.extend_toward(offset, line.start, line.end, cx);
    }

    /// 拖拽扩展的通用落点：偏移在锚点之后取 `(锚点, 吸附尾)` 正向，
    /// 之前取 `(吸附首, 锚点)` 反向。
    fn extend_toward(
        &mut self,
        offset: usize,
        snap_start: usize,
        snap_end: usize,
        cx: &mut Context<Self>,
    ) {
        let anchor = if self.selection_reversed {
            self.selected_range.end
        } else {
            self.selected_range.start
        };
        if offset >= anchor {
            self.apply_absolute_selection(anchor..snap_end, cx);
        } else {
            self.selected_range = snap_start..anchor;
            self.selection_reversed = true;
            self.marked_range = None;
            self.cursor_blink_epoch = Instant::now();
            self.clear_vertical_motion();
            self.sync_collapsed_caret_affinity();
            cx.notify();
        }
    }

    /// 计算偏移所在的词范围（字节偏移，UAX#29）。
    fn word_range_at(&self, offset: usize) -> Option<Range<usize>> {
        let text = self.rendered_text();
        if text.is_empty() {
            return None;
        }
        let mut offset = offset.min(text.len());
        while offset > 0 && !text.is_char_boundary(offset) {
            offset -= 1;
        }
        for (start, word) in text.split_word_bound_indices() {
            let end = start + word.len();
            if start <= offset && offset < end {
                return Some(start..end);
            }
        }
        None
    }

    /// 计算偏移所在的硬换行范围（含末尾换行符，末行除外）。
    fn line_range_at(&self, offset: usize) -> Option<Range<usize>> {
        let text = self.rendered_text();
        if text.is_empty() {
            return None;
        }
        let ranges = super::element::hard_line_ranges(text);
        let (line_idx, _) = super::element::line_index_for_offset(&ranges, offset.min(text.len()));
        let line = ranges.get(line_idx)?;
        let end = if line.end < text.len() {
            line.end + 1
        } else {
            line.end
        };
        Some(line.start..end)
    }

    /// 直接设置一块选区（词/行选择入口共用）：清空标记、复位方向，
    /// 并走与拖拽选择一致的光标刷新链路。
    fn apply_absolute_selection(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.selected_range = range;
        self.selection_reversed = false;
        self.marked_range = None;
        self.cursor_blink_epoch = Instant::now();
        self.clear_vertical_motion();
        self.sync_collapsed_caret_affinity();
        cx.notify();
    }

    /// 范围转 UTF-16。
    pub(super) fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        Self::utf8_range_to_utf16_in(self.rendered_text(), range)
    }

    /// 范围从 UTF-16 转回。
    pub(super) fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        Self::utf16_range_to_utf8_in(self.rendered_text(), range_utf16)
    }

    /// 取前一字形边界。
    pub fn previous_boundary(&self, offset: usize) -> usize {
        let text = self.rendered_text();
        let mut cursor = GraphemeCursor::new(
            crate::text_offsets::floor_char_boundary(text, offset),
            text.len(),
            true,
        );
        cursor.prev_boundary(text, 0).ok().flatten().unwrap_or(0)
    }

    /// 取后一字形边界。
    pub fn next_boundary(&self, offset: usize) -> usize {
        let text = self.rendered_text();
        let mut cursor = GraphemeCursor::new(
            crate::text_offsets::floor_char_boundary(text, offset),
            text.len(),
            true,
        );
        cursor
            .next_boundary(text, 0)
            .ok()
            .flatten()
            .unwrap_or(text.len())
    }

    /// 取 `offset` 前一个词的起始偏移，无词时为零。
    pub fn prev_word_boundary(&self, offset: usize) -> usize {
        let text = self.rendered_text();
        let offset = offset.min(text.len());
        text.unicode_word_indices()
            .map(|(start, _)| start)
            .take_while(|start| *start < offset)
            .last()
            .unwrap_or(0)
    }

    /// 取 `offset` 后一个词的起始偏移，无词时为文本长度。
    pub fn next_word_boundary(&self, offset: usize) -> usize {
        let text = self.rendered_text();
        let offset = offset.min(text.len());
        text.unicode_word_indices()
            .map(|(start, _)| start)
            .find(|start| *start > offset)
            .unwrap_or(text.len())
    }

    /// `display_offset` 的逆操作，把展开后的展示偏移映射回纯树偏移。
    fn unexpand_offset(&self, expanded: usize) -> usize {
        let Some(projection) = &self.projection else {
            return expanded;
        };
        projection
            .display_to_clean
            .get(expanded.min(projection.display_to_clean.len().saturating_sub(1)))
            .copied()
            .unwrap_or(expanded)
    }

    /// 按鼠标位置取字符索引。
    pub fn index_at_mouse(&self, position: Point<Pixels>) -> usize {
        if self.rendered_text().is_empty() {
            return 0;
        }

        let (Some(bounds), Some(lines)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };

        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.plain_len();
        }

        let text = self.rendered_text();
        let ranges = super::element::hard_line_ranges(text);
        let relative_y = position.y - bounds.top();
        let Some((line_idx, y_in_line)) =
            super::element::wrapped_line_for_y(lines, self.last_line_height, relative_y)
        else {
            return 0;
        };
        let (Some(layout), Some(range)) = (lines.get(line_idx), ranges.get(line_idx)) else {
            return 0;
        };
        let origin_x = super::element::aligned_line_left(layout, *bounds, self.text_align());

        let offset_in_line = match layout.closest_index_for_position(
            point(position.x - origin_x, y_in_line),
            self.last_line_height,
        ) {
            Ok(idx) | Err(idx) => idx,
        };
        crate::text_offsets::floor_char_boundary(
            text,
            range.start + offset_in_line.min(range.len()),
        )
    }

    /// 取活动范围或光标边界。
    pub(crate) fn active_selection_or_cursor_bounds(&self) -> Option<Bounds<Pixels>> {
        let bounds = self.last_bounds?;
        let lines = self.last_layout.as_ref()?;
        let line_height = self.last_line_height;
        let text = self.rendered_text();
        let active_range = self
            .marked_range
            .clone()
            .unwrap_or_else(|| self.selected_range.clone());

        if active_range.is_empty() {
            return super::element::cursor_bounds_for_offset(
                lines,
                bounds,
                line_height,
                text,
                self.caret_offset(),
                self.text_align(),
                px(1.0),
            );
        }

        super::element::range_bounds(
            lines,
            bounds,
            line_height,
            text,
            active_range,
            self.text_align(),
        )
    }
}

#[cfg(test)]
mod tests;
