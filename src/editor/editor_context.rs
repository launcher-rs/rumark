//! 块运行时上下文同步：引用定义、图片、脚注与焦点查询。

use super::*;
use crate::components::{parse_image_reference_definitions, parse_link_reference_definitions};

impl Editor {
    /// 从编辑器状态推导当前编辑目标块标识。
    pub(super) fn current_edit_target_entity_id_from_state(&self, cx: &App) -> Option<EntityId> {
        self.active_entity_id
            .filter(|entity_id| self.focusable_entity_by_id(*entity_id).is_some())
            .or_else(|| {
                self.pending_focus
                    .filter(|entity_id| self.focusable_entity_by_id(*entity_id).is_some())
            })
            .or_else(|| self.first_focusable_entity_id(cx))
    }

    /// 从编辑器状态获取当前编辑目标块实体。
    pub(super) fn current_edit_target_from_state(&self, cx: &App) -> Option<Entity<Block>> {
        self.current_edit_target_entity_id_from_state(cx)
            .and_then(|entity_id| self.focusable_entity_by_id(entity_id))
    }

    /// 结束所有块的指针选区会话（内部实现，可选是否通知）。
    fn end_block_pointer_selection_sessions_inner(
        &mut self,
        cx: &mut Context<Self>,
        notify: bool,
    ) -> bool {
        let mut changed = false;

        if let Some(target) = self.current_edit_target_from_state(cx) {
            target.update(cx, |block, _cx| {
                changed |= block.end_pointer_selection_session();
            });
        }

        for visible in self.document.visible_blocks().to_vec() {
            visible.entity.update(cx, |block, _cx| {
                changed |= block.end_pointer_selection_session();
            });
        }

        // 只收集单元格实体句柄，而非整个绑定结构体（内含实体与位置）。
        // 先收集再遍历是为了释放借用；克隆实体只是引用计数增加，
        // 跳过外层结构体克隆后，每帧开销与单元格数成正比。
        let cells: Vec<Entity<Block>> = self
            .table_cells
            .values()
            .map(|binding| binding.cell.clone())
            .collect();
        for cell in cells {
            cell.update(cx, |block, _cx| {
                changed |= block.end_pointer_selection_session();
            });
        }

        if changed && notify {
            cx.notify();
        }
        changed
    }

    /// 结束所有块的指针选区会话。
    pub(super) fn end_block_pointer_selection_sessions(&mut self, cx: &mut Context<Self>) -> bool {
        self.end_block_pointer_selection_sessions_inner(cx, true)
    }

    /// 创建新块实体并订阅它的事件流。
    pub(super) fn new_block(cx: &mut Context<Self>, record: BlockRecord) -> Entity<Block> {
        let block = cx.new(|cx| Block::with_record(cx, record));
        cx.subscribe(&block, Self::on_block_event).detach();
        block
    }

    /// 创建表格单元格块并设为单元格模式。
    pub(super) fn new_table_cell_block(
        cx: &mut Context<Self>,
        title: InlineTextTree,
        position: CellPosition,
        alignment: TableColumnAlignment,
    ) -> Entity<Block> {
        let block = Self::new_block(cx, BlockRecord::new(BlockKind::Paragraph, title));
        block.update(cx, |block, _cx| {
            block.set_table_cell_mode(position, alignment);
        });
        block
    }

    /// 获取图片相对路径的基准目录（文档所在目录或当前目录）。
    pub(super) fn image_base_dir(&self) -> Option<PathBuf> {
        self.file_path
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
    }

    /// 同步单个块的运行时上下文（图片基准目录与引用注册表）。
    pub(super) fn sync_runtime_context_for_block(
        &self,
        block: &Entity<Block>,
        base_dir: Option<&Path>,
        cx: &mut Context<Self>,
    ) {
        self.sync_runtime_context_for_block_inner(block, base_dir, true, cx);
    }

    /// 同步单个块的运行时上下文，`notify` 控制是否请求重绘。
    ///
    /// 大体量文档首次载入时，这些块刚被创建、尚未挂载，也不可能有观察者，
    /// 逐块 `cx.notify()` 只会产生与块数成正比的重绘调度开销；载入完成时
    /// 编辑器会统一 `notify` 一次，因此这里可以安静地写入上下文。
    fn sync_runtime_context_for_block_inner(
        &self,
        block: &Entity<Block>,
        base_dir: Option<&Path>,
        notify: bool,
        cx: &mut Context<Self>,
    ) {
        let next_base_dir = base_dir.map(Path::to_path_buf);
        let image_reference_definitions = self.image_reference_definitions.clone();
        let link_reference_definitions = self.link_reference_definitions.clone();
        let footnote_registry = self.footnote_registry.clone();
        block.update(cx, move |block, cx| {
            block.set_runtime_context(
                next_base_dir.clone(),
                image_reference_definitions.clone(),
                link_reference_definitions.clone(),
                footnote_registry.clone(),
            );
            if notify {
                cx.notify();
            }
        });
    }

    /// 按可见顺序重建脚注注册表（定义绑定与引用序号）。
    pub(super) fn rebuild_footnote_registry(&mut self, cx: &App) {
        let mut definitions = HashMap::new();
        let visible = self.document.visible_blocks().to_vec();
        for visible_block in &visible {
            let block = visible_block.entity.read(cx);
            if block.kind() != BlockKind::FootnoteDefinition {
                continue;
            }

            let allow_definition = self
                .document
                .find_block_location(visible_block.entity.entity_id())
                .is_some_and(|location| {
                    location.parent.is_none()
                        || location
                            .parent
                            .as_ref()
                            .is_some_and(|parent| parent.read(cx).kind().is_quote_container())
                });
            if !allow_definition {
                continue;
            }

            definitions
                .entry(block.record.title.plain_text().to_string())
                .or_insert(visible_block.entity.entity_id());
        }

        let mut bindings = HashMap::<String, FootnoteDefinitionBinding>::new();
        for (id, entity_id) in definitions {
            bindings.insert(
                id,
                FootnoteDefinitionBinding {
                    ordinal: None,
                    definition_entity_id: entity_id,
                    first_reference: None,
                },
            );
        }

        let mut next_ordinal = 1usize;
        let mut occurrence_index = 0usize;
        let mut block_occurrences = HashMap::<uuid::Uuid, Vec<FootnoteResolvedOccurrence>>::new();
        for visible_block in visible {
            let block = visible_block.entity.read(cx);
            let block_id = block.record.id;
            for fragment in &block.record.title.fragments {
                let Some(footnote) = fragment.footnote.as_ref() else {
                    continue;
                };
                let ordinal = if let Some(binding) = bindings.get_mut(&footnote.id) {
                    if binding.ordinal.is_none() {
                        binding.ordinal = Some(next_ordinal);
                        next_ordinal += 1;
                    }
                    if binding.first_reference.is_none() {
                        binding.first_reference = Some(FootnoteReferenceLocation {
                            entity_id: visible_block.entity.entity_id(),
                            occurrence_index,
                        });
                    }
                    binding.ordinal
                } else {
                    None
                };
                block_occurrences
                    .entry(block_id)
                    .or_default()
                    .push(FootnoteResolvedOccurrence {
                        id: footnote.id.clone(),
                        ordinal,
                        occurrence_index,
                    });
                if ordinal.is_none() {
                    occurrence_index += 1;
                    continue;
                }
                occurrence_index += 1;
            }
        }

        self.footnote_registry = Arc::new(FootnoteRegistry {
            bindings,
            block_occurrences,
        });
    }

    /// 重建图片/链接引用定义并同步所有可见块的运行时上下文。
    pub(super) fn rebuild_image_runtimes(&mut self, cx: &mut Context<Self>) {
        let derived = self.derived_document(cx);
        self.image_reference_definitions = derived.image_references.clone();
        self.link_reference_definitions = derived.link_references.clone();
        self.sync_runtime_contexts(cx);
    }

    /// 从载入时已有的源码提取引用定义，避免首帧前生成整篇来源映射。
    ///
    /// 大型文档首次载入专用：所有块刚创建、尚未挂载，逐块通知没有观察者，
    /// 因此安静写入运行时上下文，由载入完成处的一次统一通知负责重绘。
    pub(super) fn rebuild_image_runtimes_from_source(
        &mut self,
        source: &str,
        cx: &mut Context<Self>,
    ) {
        self.image_reference_definitions = Arc::new(parse_image_reference_definitions(source));
        self.link_reference_definitions = Arc::new(parse_link_reference_definitions(source));
        self.sync_runtime_contexts_inner(false, cx);
    }

    /// 同步当前引用定义与脚注注册表到全部运行时块。
    fn sync_runtime_contexts(&mut self, cx: &mut Context<Self>) {
        self.sync_runtime_contexts_inner(true, cx);
    }

    /// 同步全部运行时块，`notify` 控制逐块是否请求重绘。
    ///
    /// 安静模式只用于大型文档首次载入：整批块尚未挂载，逐步通知没有意义，
    /// 载入完成处已有一次统一通知。
    fn sync_runtime_contexts_inner(&mut self, notify: bool, cx: &mut Context<Self>) {
        let base_dir = self.image_base_dir();
        self.rebuild_footnote_registry(cx);
        let visible = self.document.visible_blocks().to_vec();
        for visible_block in visible {
            self.sync_runtime_context_for_block_inner(
                &visible_block.entity,
                base_dir.as_deref(),
                notify,
                cx,
            );
            if visible_block.entity.read(cx).kind() != BlockKind::Table {
                continue;
            }
            let Some(runtime) = visible_block.entity.read(cx).table_runtime.clone() else {
                continue;
            };
            for cell in runtime.header {
                self.sync_runtime_context_for_block_inner(&cell, base_dir.as_deref(), notify, cx);
            }
            for row in runtime.rows {
                for cell in row {
                    self.sync_runtime_context_for_block_inner(
                        &cell,
                        base_dir.as_deref(),
                        notify,
                        cx,
                    );
                }
            }
        }
    }

    /// 按标识查找可聚焦实体（含表格单元格）。
    pub(super) fn focusable_entity_by_id(&self, entity_id: EntityId) -> Option<Entity<Block>> {
        self.document.block_entity_by_id(entity_id).or_else(|| {
            self.table_cells
                .get(&entity_id)
                .map(|binding| binding.cell.clone())
        })
    }

    /// 获取首个可聚焦实体的标识（表格首块落到首个表头单元格）。
    pub(super) fn first_focusable_entity_id(&self, cx: &App) -> Option<EntityId> {
        let first_root = self.document.first_root()?.clone();
        if first_root.read(cx).kind() == BlockKind::Table {
            return first_root
                .read(cx)
                .table_runtime
                .as_ref()
                .and_then(|runtime| runtime.header.first())
                .map(|cell| cell.entity_id())
                .or_else(|| Some(first_root.entity_id()));
        }
        Some(first_root.entity_id())
    }

    /// 获取当前窗口中实际聚焦的编辑目标标识。
    pub(super) fn focused_edit_target_entity_id(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<EntityId> {
        self.document
            .focused_block_entity_id(window, cx)
            .or_else(|| {
                self.table_cells
                    .values()
                    .find(|binding| binding.cell.read(cx).focus_handle.is_focused(window))
                    .map(|binding| binding.cell.entity_id())
            })
    }

    /// 获取当前窗口中实际聚焦的编辑目标块。
    pub(super) fn focused_edit_target(&self, window: &Window, cx: &App) -> Option<Entity<Block>> {
        self.focused_edit_target_entity_id(window, cx)
            .and_then(|entity_id| self.focusable_entity_by_id(entity_id))
    }

    /// 按单元格标识查询其表格绑定。
    pub(super) fn table_cell_binding(&self, entity_id: EntityId) -> Option<CellBinding> {
        self.table_cells.get(&entity_id).cloned()
    }

    /// 按标识查找表格块（非表格返回空）。
    pub(super) fn table_block_by_id(&self, entity_id: EntityId, cx: &App) -> Option<Entity<Block>> {
        self.document
            .block_entity_by_id(entity_id)
            .filter(|block| block.read(cx).kind() == BlockKind::Table)
    }
}
