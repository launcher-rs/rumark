//! 顶层编辑器控制器与窗口状态。
//!
//! [`Editor`] 负责窗口级事务，如视图模式、保存/关闭流程、
//! 滚动状态与焦点延迟。运行时块树本身存放在
//! [`DocumentTree`] 中，由它集中处理结构变更并缓存可见
//! 顺序元数据。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rgpui::tree::TreeState;
use rgpui::*;

use self::context_menu::{ContextMenuState, LinkDialogState, TableInsertDialogState};
use self::find::FindBarState;
use self::tree::DocumentTree;
use crate::components::{
    AxisMarker, CellPosition, TableAxisHighlight, TableAxisKind, TableColumnAlignment, TableModel,
    TableRuntime, UndoKind, cell_to_markdown,
};
use crate::components::{
    Block, BlockKind, BlockRecord, FootnoteDefinitionBinding, FootnoteReferenceLocation,
    FootnoteRegistry, FootnoteResolvedOccurrence, ImageReferenceDefinitions, InlineTextTree,
    LinkReferenceDefinitions,
};
mod close;
mod context_menu;
mod derive;
mod document;
mod editor_context;
mod events;
mod file_drop;
mod find;
mod history;
mod image_cache;
mod image_menu;
mod persistence;
/// 文档读取入口：启动、菜单打开与拖放打开共用同一套 BOM / 编码处理。
pub(crate) use persistence::{read_error_detail, read_markdown_file};
#[cfg(test)]
mod perf;
mod render;
mod selection;
mod source_map;
mod status_bar;
mod table_edit;
mod titlebar_menu;
/// 测试用：统计右键弹出菜单的构造次数，用于回归“单次右键只弹一层菜单”。
#[cfg(test)]
pub(crate) static MENU_BUILD_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
mod tests;
mod tree;
mod window_layout;
mod workspace;

/// 每个载入批次构建的根块数量。
///
/// 保持较小批次以让出 UI；实测把批次放大到 256 并不会降低 rgpui 的
/// 实体更新总成本，反而会增加单批工作。
pub(crate) const ROOT_BLOCKS_PER_CHUNK: usize = 32;

/// 单次载入 tick 的时间预算（毫秒）。
///
/// Windows 系统定时器默认约 15.6ms 才传递一次过期，逐批 sleep 会让分批载入
/// 被放大近一个数量级；每个 tick 连续处理批次直到耗尽该预算再让出主线程，
/// 既显著减少 tick 总数，也保证单次更新远小于一帧的预算以维持输入响应。
pub(crate) const DOCUMENT_LOAD_BUDGET_MS: u64 = 8;

use self::workspace::WorkspaceState;

/// 延迟到有 `Window` 可用时再执行的链接跳转请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingLinkOpen {
    pub(crate) prompt_target: String,
    pub(crate) open_target: String,
}

/// 正在分批构建的大型文档；块实体在载入完成前不挂入可编辑树。
struct PendingDocumentLoad {
    lines: Vec<String>,
    next_line: usize,
    root_blocks: Vec<Entity<Block>>,
    /// 已构建内容中最后一个根块是否为列表项。
    ///
    /// 批次边界的空行保留规则依赖上一根块的类型；不携带该上下文会让分批
    /// 构建出的块树与一次完整构建不一致（列表组分隔空行数量漂移）。
    previous_root_is_list_item: bool,
    file_path: Option<PathBuf>,
}

/// 顶层控制器，持有编辑器级状态并把树变更委托给 [`DocumentTree`]。
///
/// 编辑器订阅每个子块发出的 [`BlockEvent`](crate::components::BlockEvent)。
/// 结构变更集中处理，以保持焦点、滚动、脏标记与序列化同步。
pub struct Editor {
    document: DocumentTree,
    table_cells: HashMap<EntityId, CellBinding>,
    /// 编辑器当前呈现的视图。
    pub(crate) view_mode: ViewMode,
    /// 有 [`Window`] 可用时在渲染阶段应用的延迟焦点目标。
    pending_focus: Option<EntityId>,
    active_entity_id: Option<EntityId>,
    pending_scroll_active_block_into_view: bool,
    pending_scroll_recheck_after_layout: bool,
    pending_save: bool,
    pending_save_as: bool,
    pending_open_link: Option<PendingLinkOpen>,
    pending_document_load: Option<PendingDocumentLoad>,
    document_loading: bool,
    pending_window_edited: bool,
    pending_window_title_refresh: bool,
    document_dirty: bool,
    file_path: Option<PathBuf>,
    scroll_handle: ScrollHandle,
    last_scroll_viewport_size: Option<Size<Pixels>>,
    /// 上一帧文档树修订号，用于只在结构变化后刷新高度缓存。
    prev_render_tree_revision: Option<u64>,
    /// 按行缓存的占用高度（行高加尾部 gap），以行首块为键。与滚动无关，
    /// 不像原始绘制位置那样随滚动变化，因此按其累加和做窗口化始终正确。
    /// 绘制行时填充；未知行用最小高度估计。
    row_stride_cache: HashMap<EntityId, f32>,
    /// 行高前缀索引，滚动帧只更新新测量行并对数查询窗口边界。
    row_stride_index: window_layout::StrideIndex,
    /// 可复用的渲染行计划；滚动帧不再逐块读取元数据重新分组。
    render_rows_cache: Option<Arc<render::RenderRowsCache>>,
    /// 缓存占用高度时的内容列宽。行在列宽变化时会重排，因此丢弃异宽条目。
    row_stride_width: Option<f32>,
    /// 上一帧挂载区间在滚动容器子节点中的位置。
    prev_mounted_run: Option<MountedRow>,
    close_guard_installed: bool,
    show_unsaved_changes_dialog: bool,
    /// 为真时，下次保存成功后关闭窗口。
    pending_close_after_save: bool,
    /// 关闭对话框取消时要恢复的焦点目标。
    close_dialog_restore_focus: Option<EntityId>,
    pending_drop_replace_path: Option<PathBuf>,
    show_drop_replace_dialog: bool,
    pending_drop_replace_after_save: bool,
    drop_replace_restore_focus: Option<EntityId>,
    workspace: WorkspaceState,
    /// 工作区文件树 tab 的框架树状态（文件列表渲染与交互由此承载）。
    file_tree_state: Entity<TreeState>,
    /// 工作区大纲 tab 的框架树状态（标题树渲染与交互由此承载）。
    outline_tree_state: Entity<TreeState>,
    /// 框架文件树点击文件的延迟打开（事件无 `Window`，在渲染侧消费）。
    pending_workspace_open: Option<PathBuf>,
    context_menu: Option<ContextMenuState>,
    /// 标题栏当前展开的菜单下标（同一时刻只允许一个）。
    titlebar_menu_open: Option<usize>,
    /// 展开中的标题栏下拉菜单实体。
    titlebar_menu_popup: Option<Entity<PopupMenu>>,
    /// 标题栏菜单的关闭订阅（丢弃菜单实体前释放）。
    titlebar_menu_subscription: Option<Subscription>,
    /// 展开标题栏菜单前的焦点，关闭后归还给文档。
    titlebar_menu_restore_focus: Option<FocusHandle>,
    table_insert_dialog: Option<TableInsertDialogState>,
    link_dialog: Option<LinkDialogState>,
    find_bar: Option<FindBarState>,
    table_axis_preview: Option<AxisSelection>,
    table_axis_selection: Option<AxisSelection>,
    cross_block_selection: Option<CrossBlockSelection>,
    cross_block_drag: Option<CrossBlockDrag>,
    /// 拖拽选区的当前指针位置（窗口坐标），供越界自动滚动使用。
    drag_pointer: Option<Point<Pixels>>,
    /// 拖拽期间的自动滚动循环任务（拖拽结束后释放）。
    drag_autoscroll_task: Option<Task<()>>,
    scrollbar_hovered: bool,
    scrollbar_visible_until: Instant,
    scrollbar_fade_task: Option<Task<()>>,
    /// 挂起的滚动进入视口重试完成后短暂强制重绘（目标块尚无实测
    /// 边界时），让滚动在下一帧落定，而不必等光标闪烁。
    scroll_recheck_task: Option<Task<()>>,
    scrollbar_drag: Option<ScrollbarDrag>,
    undo_history: Vec<UndoEntry>,
    redo_history: Vec<UndoEntry>,
    pending_undo_capture: Option<UndoCapture>,
    last_selection_snapshot: SelectionUndoSnapshot,
    last_stable_source_text: Arc<str>,
    /// 大文档文字输入期间延后刷新全篇历史快照与来源映射。
    stable_snapshot_stale: bool,
    /// 输入停顿后的空闲快照刷新任务：触发时若仍在输入则跳过并等下一帧重排。
    snapshot_refresh_task: Option<Task<()>>,
    last_document_edit: Option<Instant>,
    history_restore_in_progress: bool,
    /// 上次为引用相关块重建树元数据时的树修订号：纯文本编辑不改变引用元数据
    /// （深度/锚点来自树结构），修订号未变时跳过每键全树重建。
    quote_metadata_revision: u64,
    image_reference_definitions: Arc<ImageReferenceDefinitions>,
    link_reference_definitions: Arc<LinkReferenceDefinitions>,
    footnote_registry: Arc<FootnoteRegistry>,
    /// 序列化文本/源码映射/引用定义的派生缓存，按内容指纹失效。
    derive_cache: RefCell<Option<derive::DerivedDocument>>,
    /// 内容指纹快路：缓存 (全局内容纪元, 树修订号, 指纹)，二者未变时无需逐块扫描。
    fingerprint_cache: Cell<Option<(u64, u64, u64)>>,
    /// 状态栏统计缓存：字数按内容指纹、行列按光标偏移、选中字数按选区指纹失效。
    status_stats_cache: RefCell<Option<status_bar::StatusStats>>,
    /// 本窗口的图片缓存：成功结果按 LRU 限量，失败结果到期后自动重试。
    image_cache: Entity<image_cache::BoundedImageCache>,
}

/// 表格块与某个单元格编辑器之间的运行时绑定。
#[derive(Clone)]
struct CellBinding {
    table_block: Entity<Block>,
    cell: Entity<Block>,
    position: CellPosition,
}

/// 渲染后原生表格中选中的行或列。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AxisSelection {
    table_block_id: EntityId,
    kind: TableAxisKind,
    index: usize,
}

/// 自定义编辑器滚动条的像素几何。
#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollbarMetrics {
    track_height: f32,
    thumb_height: f32,
    thumb_top: f32,
    max_scroll_y: f32,
}

/// 窗口化结果：要挂载的行区间，以及代替裁掉行的占位高度。
/// `top_h` 是区间上方的占位，`bottom_h` 是文档末尾的占位。
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewportWindow {
    run_start: usize,
    run_end: usize,
    top_h: f32,
    bottom_h: f32,
    focus_island: Option<FocusHighlight>,
}

/// 一帧挂载区间在滚动容器子节点中的位置，供下一帧按下标读回
/// 实测边界。`child_count` 用于校验映射而非默认可信。
#[derive(Clone, Copy, Debug, PartialEq)]
struct MountedRow {
    row_start: usize,
    row_end: usize,
    child_base: usize,
    child_count: usize,
}

/// 单独挂载的聚焦行（不在主区间内）。它相对主区间的位置由 `row`
/// 决定，`lead_h` 是其上方的占位高度。
#[derive(Clone, Copy, Debug, PartialEq)]
struct FocusHighlight {
    row: usize,
    lead_h: f32,
}

/// 自定义滚动条滑块的活动拖拽会话。
#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollbarDrag {
    pointer_offset_y: f32,
    track_height: f32,
    thumb_height: f32,
    max_scroll_y: f32,
}

/// 随撤销历史保存的源码模式选区快照。
#[derive(Clone, Debug, PartialEq, Eq)]
struct SelectionUndoSnapshot {
    range: std::ops::Range<usize>,
    reversed: bool,
}

/// 单条撤销历史：源码文本与选区状态。
#[derive(Clone, Debug)]
struct UndoEntry {
    /// 整篇源码快照，内容未变时与派生缓存及其他快照共享同一份内存。
    source_text: Arc<str>,
    selection: SelectionUndoSnapshot,
    timestamp: Instant,
    kind: UndoKind,
}

/// 挂起的撤销捕获，用于合并相邻输入编辑。
#[derive(Clone, Debug)]
struct UndoCapture {
    snapshot: UndoEntry,
    block_version: Option<(EntityId, u64)>,
}

/// 可见块顺序下的跨块选区端点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CrossBlockSelectionEndpoint {
    pub(super) entity_id: EntityId,
    pub(super) offset: usize,
}

/// 横跨两个可见块端点的编辑器级选区。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CrossBlockSelection {
    pub(super) anchor: CrossBlockSelectionEndpoint,
    pub(super) focus: CrossBlockSelectionEndpoint,
}

/// 创建或扩展跨块选区时的拖拽状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CrossBlockDrag {
    pub(super) anchor: CrossBlockSelectionEndpoint,
}

/// 单个可见块文本区间到规范 Markdown 偏移的映射。
/// 逐字符偏移表用 `Arc` 共享，使整份映射可以廉价克隆进缓存。
#[derive(Clone)]
pub(super) struct SourceMapEntry {
    entity: Entity<Block>,
    full_source_range: std::ops::Range<usize>,
    content_to_source: Arc<Vec<usize>>,
    source_to_content: Arc<Vec<usize>>,
}

/// 编辑器可呈现的两种编辑视图。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    /// 富文本渲染视图：按语义类型为每个块设置样式。
    Rendered,
    /// 纯源码视图：整个 Markdown 文档作为单个原始缓冲区编辑。
    Source,
}

impl Editor {
    const UNDO_LIMIT: usize = 200;
    /// 单条历史栈（撤销/重做各自）最多保留的源码字节总量。
    const HISTORY_BYTES_LIMIT: usize = 64 * 1024 * 1024;
    const UNDO_COALESCE_WINDOW: Duration = Duration::from_millis(1_000);

    /// 对话框按钮统一路由：持有实体直接更新。
    ///
    /// 框架 `Dialog` 的构建闭包只有 `&mut App`，拿不到实体上下文；
    /// 但按钮点击发生在事件派发中，此时窗口正被 `take`，
    /// `downcast` + `WindowHandle::update` 必失败（app_menu 的 F12 教训），
    /// 故调用方在有 `Context` 处取出实体并在此直连更新。
    pub(super) fn dialog_button_click(
        editor: Entity<Self>,
        op: fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>),
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + Send + Sync + 'static {
        move |event, window, cx| {
            editor.update(cx, |editor, cx| {
                op(editor, event, window, cx);
            });
        }
    }

    /// 从 Markdown 文本构造编辑器（含文件路径）。
    pub fn from_markdown(
        cx: &mut Context<Self>,
        markdown: String,
        file_path: Option<PathBuf>,
    ) -> Self {
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        let mut roots = Self::build_root_blocks_from_markdown(cx, &normalized);
        if roots.is_empty() {
            roots.push(Self::new_block(cx, BlockRecord::paragraph(String::new())));
        }

        let file_tree_state = cx.new(|cx| TreeState::new(cx));
        let outline_tree_state = cx.new(|cx| TreeState::new(cx));
        let image_cache = cx.new(|_cx| image_cache::BoundedImageCache::new());

        let mut document = DocumentTree::new(roots);
        document.rebuild_metadata_and_snapshot(cx);
        let mut editor = Self {
            document,
            table_cells: HashMap::new(),
            view_mode: ViewMode::Rendered,
            pending_focus: None,
            active_entity_id: None,
            pending_scroll_active_block_into_view: true,
            pending_scroll_recheck_after_layout: true,
            pending_save: false,
            pending_save_as: false,
            pending_open_link: None,
            pending_document_load: None,
            document_loading: false,
            pending_window_edited: false,
            pending_window_title_refresh: false,
            document_dirty: false,
            file_path,
            scroll_handle: ScrollHandle::new(),
            last_scroll_viewport_size: None,
            prev_render_tree_revision: None,
            row_stride_cache: HashMap::new(),
            row_stride_index: window_layout::StrideIndex::default(),
            render_rows_cache: None,
            row_stride_width: None,
            prev_mounted_run: None,
            close_guard_installed: false,
            show_unsaved_changes_dialog: false,
            pending_close_after_save: false,
            close_dialog_restore_focus: None,
            pending_drop_replace_path: None,
            show_drop_replace_dialog: false,
            pending_drop_replace_after_save: false,
            drop_replace_restore_focus: None,
            workspace: WorkspaceState::default(),
            file_tree_state: file_tree_state.clone(),
            outline_tree_state: outline_tree_state.clone(),
            pending_workspace_open: None,
            context_menu: None,
            titlebar_menu_open: None,
            titlebar_menu_popup: None,
            titlebar_menu_subscription: None,
            titlebar_menu_restore_focus: None,
            table_insert_dialog: None,
            link_dialog: None,
            find_bar: None,
            table_axis_preview: None,
            table_axis_selection: None,
            cross_block_selection: None,
            cross_block_drag: None,
            drag_pointer: None,
            drag_autoscroll_task: None,
            scrollbar_hovered: false,
            scrollbar_visible_until: Instant::now(),
            scrollbar_fade_task: None,
            scroll_recheck_task: None,
            scrollbar_drag: None,
            undo_history: Vec::new(),
            redo_history: Vec::new(),
            pending_undo_capture: None,
            last_selection_snapshot: Self::empty_selection_snapshot(),
            last_stable_source_text: Arc::from(normalized),
            stable_snapshot_stale: false,
            snapshot_refresh_task: None,
            last_document_edit: None,
            history_restore_in_progress: false,
            quote_metadata_revision: 0,
            image_reference_definitions: Arc::default(),
            link_reference_definitions: Arc::default(),
            footnote_registry: Arc::default(),
            derive_cache: RefCell::new(None),
            fingerprint_cache: Cell::new(None),
            status_stats_cache: RefCell::new(None),
            image_cache,
        };
        editor.rebuild_table_runtimes(cx);
        editor.rebuild_image_runtimes(cx);
        cx.subscribe(&file_tree_state, Self::on_workspace_tree_event)
            .detach();
        cx.subscribe(&outline_tree_state, Self::on_outline_tree_event)
            .detach();
        // 文档以 Mermaid 图开头时先展示图表预览，避免启动后自动进入源码编辑态。
        editor.pending_focus = editor.first_focusable_entity_id(cx).filter(|entity_id| {
            editor
                .focusable_entity_by_id(*entity_id)
                .is_some_and(|block| block.read(cx).kind() != BlockKind::MermaidBlock)
        });
        editor.active_entity_id = editor.pending_focus;
        // 常规构造保留稳定快照初始化；大文档窗口先构造轻量占位符，成本很低。
        editor.refresh_stable_document_snapshot(cx);
        editor
    }

    /// 延后载入大型文档，先让窗口显示可见的加载提示并完成首帧。
    pub(crate) fn defer_large_document_load(
        &mut self,
        markdown: String,
        file_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.document_loading = true;
        let editor = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if editor
                .update(cx, move |this, cx| {
                    this.begin_pending_document_load(markdown, file_path, cx);
                })
                .is_err()
            {
                return;
            }

            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(2))
                    .await;
                let finished = editor
                    .update(cx, |this, cx| {
                        // Windows 默认定时器粒度约 15.6ms，逐批等待会把 2ms 批间隔
                        // 放大近一个数量级（万级块文档载入实测 4.4s）。改为按时间
                        // 预算在一次更新里连续处理多个批次再让出主线程，tick 数
                        // 随预算成倍减少，UI 仍保持可响应。
                        let budget_start = Instant::now();
                        let mut finished = false;
                        while !finished
                            && budget_start.elapsed()
                                < Duration::from_millis(DOCUMENT_LOAD_BUDGET_MS)
                        {
                            finished = this.load_next_document_chunk(cx);
                        }
                        finished
                    })
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        })
        .detach();
    }

    /// 切分大型文档并开始分批构造块；未完成前只展示加载占位块。
    fn begin_pending_document_load(
        &mut self,
        markdown: String,
        file_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        self.pending_document_load = Some(PendingDocumentLoad {
            lines: normalized.split('\n').map(ToOwned::to_owned).collect(),
            next_line: 0,
            root_blocks: Vec::new(),
            previous_root_is_list_item: false,
            file_path,
        });
        self.pending_focus = None;
        self.active_entity_id = None;
        cx.notify();
    }

    /// 在一次 UI 更新中解析有限数量的根块，返回是否已全部载入。
    fn load_next_document_chunk(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(pending) = self.pending_document_load.as_ref() else {
            return true;
        };
        let (blocks, next_line) = Self::build_root_blocks_chunk(
            cx,
            &pending.lines,
            pending.next_line,
            ROOT_BLOCKS_PER_CHUNK,
            pending.previous_root_is_list_item,
        );
        let is_complete = next_line >= pending.lines.len();

        let pending = self
            .pending_document_load
            .as_mut()
            .expect("载入任务在同一实体更新中保持存在");
        pending.root_blocks.extend(blocks);
        pending.next_line = next_line;
        if let Some(last) = pending.root_blocks.last() {
            pending.previous_root_is_list_item = last.read(cx).kind().is_list_item();
        }
        if !is_complete {
            return false;
        }

        let pending = self
            .pending_document_load
            .take()
            .expect("已完成任务应仍存在");
        let source = pending.lines.join("\n");
        self.replace_document_from_root_blocks(
            pending.root_blocks,
            pending.file_path,
            Arc::from(source),
            cx,
        );
        true
    }
}
