//! 编辑器块树的运行时归属。
//!
//! [`DocumentTree`] 是编辑器内块顺序与父子关系的唯一可变持有者。
//! 它同时维护一份缓存的 [`VisibleTreeSnapshot`]，使热点查询不必在每次
//! 聚焦、滚动或变更时都重跑全量深度遍历。

use std::collections::HashMap;

use rgpui::*;

use super::Editor;
use crate::components::table_to_markdown_lines;
use crate::components::{Block, BlockKind, CalloutVariant, parse_standalone_image};

/// 块及其在当前可见深度遍历顺序中的位置。
#[derive(Clone)]
pub(super) struct VisibleBlock {
    pub entity: Entity<Block>,
}

/// 块在运行时树中的位置。
#[derive(Clone)]
pub(super) struct BlockLocation {
    pub parent: Option<Entity<Block>>,
    pub index: usize,
}

/// 当前运行时树的可见顺序缓存元数据。
#[derive(Default, Clone)]
pub(super) struct VisibleTreeSnapshot {
    visible: Vec<VisibleBlock>,
    visible_index_by_entity: HashMap<EntityId, usize>,
    location_by_entity: HashMap<EntityId, BlockLocation>,
    last_visible_descendant_by_entity: HashMap<EntityId, EntityId>,
}

impl VisibleTreeSnapshot {
    /// 清空快照缓存。
    fn clear(&mut self) {
        self.visible.clear();
        self.visible_index_by_entity.clear();
        self.location_by_entity.clear();
        self.last_visible_descendant_by_entity.clear();
    }
}

/// 深度遍历时向下继承的块上下文（`sync_block_list` 递归下传的一组状态，
/// 打包后递归签名从 13 个参数降到 6 个）。
#[derive(Clone, Copy)]
struct InheritedBlockContext {
    list_depth: usize,
    /// 该层子块行的序列化缩进列数：列表项按自身标记正文列宽累计
    /// （`1. ` 为 3），引用/提示框内部不再累计（由 `> ` 前缀承担）。
    source_indent_columns: usize,
    quote_depth: usize,
    quote_group_anchor: Option<uuid::Uuid>,
    visible_quote_group_anchor: Option<uuid::Uuid>,
    callout_depth: usize,
    callout_anchor: Option<uuid::Uuid>,
    callout_variant: Option<CalloutVariant>,
    footnote_anchor: Option<uuid::Uuid>,
}

impl InheritedBlockContext {
    /// 根层级的空继承上下文。
    fn root() -> Self {
        Self {
            list_depth: 0,
            source_indent_columns: 0,
            quote_depth: 0,
            quote_group_anchor: None,
            visible_quote_group_anchor: None,
            callout_depth: 0,
            callout_anchor: None,
            callout_variant: None,
            footnote_anchor: None,
        }
    }
}

/// 运行时块树的规范持有者。
///
/// Markdown 导入器构造根块与嵌套列表子块后移交给 `DocumentTree`。
/// 此后一切结构编辑都必须经过本类型，以保持运行时树与导入器/
/// 序列化器可重建的 Markdown 子集对齐。
pub(super) struct DocumentTree {
    roots: Vec<Entity<Block>>,
    snapshot: VisibleTreeSnapshot,
    /// 结构修订号：每次重建元数据与可见快照时递增，供派生缓存判定失效。
    revision: u64,
}

impl DocumentTree {
    /// 新建文档树。
    pub(super) fn new(roots: Vec<Entity<Block>>) -> Self {
        Self {
            roots,
            snapshot: VisibleTreeSnapshot::default(),
            revision: 0,
        }
    }

    /// 当前结构修订号。
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    /// 获取首个根块。
    pub(super) fn first_root(&self) -> Option<&Entity<Block>> {
        self.roots.first()
    }

    /// 获取根块切片。
    pub(super) fn root_blocks(&self) -> &[Entity<Block>] {
        &self.roots
    }

    /// 获取根块数量。
    pub(super) fn root_count(&self) -> usize {
        self.roots.len()
    }

    /// 获取可见块切片。
    pub(super) fn visible_blocks(&self) -> &[VisibleBlock] {
        &self.snapshot.visible
    }

    /// 可见块总数。仅需长度时用它，避免让整片借用跨越后续可变借用。
    pub(super) fn visible_block_count(&self) -> usize {
        self.snapshot.visible.len()
    }

    /// 按下标取可见块实体标识，同上：逐块取值不留长借用。
    pub(super) fn visible_block_entity_id(&self, index: usize) -> EntityId {
        self.snapshot.visible[index].entity.entity_id()
    }

    /// 获取当前窗口中聚焦块的标识。
    pub(super) fn focused_block_entity_id(&self, window: &Window, cx: &App) -> Option<EntityId> {
        self.snapshot
            .visible
            .iter()
            .find(|visible| visible.entity.read(cx).focus_handle.is_focused(window))
            .map(|visible| visible.entity.entity_id())
    }

    /// 按标识查询可见序号。
    pub(super) fn visible_index_for_entity_id(&self, entity_id: EntityId) -> Option<usize> {
        self.snapshot
            .visible_index_by_entity
            .get(&entity_id)
            .copied()
    }

    /// 按可见序号取块实体；序号越界返回 `None`。
    pub(super) fn visible_block_at(&self, index: usize) -> Option<Entity<Block>> {
        self.snapshot
            .visible
            .get(index)
            .map(|visible| visible.entity.clone())
    }

    /// 按标识查找块实体。
    pub(super) fn block_entity_by_id(&self, entity_id: EntityId) -> Option<Entity<Block>> {
        self.visible_index_for_entity_id(entity_id)
            .and_then(|index| self.snapshot.visible.get(index))
            .map(|visible| visible.entity.clone())
    }

    /// 按标识查找块在树中的位置。
    pub(super) fn find_block_location(&self, entity_id: EntityId) -> Option<BlockLocation> {
        self.snapshot.location_by_entity.get(&entity_id).cloned()
    }

    /// 返回同一父节点下紧邻的前一个兄弟块（若有）。
    pub(super) fn previous_sibling(&self, entity_id: EntityId, cx: &App) -> Option<Entity<Block>> {
        let location = self.find_block_location(entity_id)?;
        let prev_index = location.index.checked_sub(1)?;
        match &location.parent {
            Some(parent) => parent.read(cx).children.get(prev_index).cloned(),
            None => self.roots.get(prev_index).cloned(),
        }
    }

    /// 获取该块的最后一个可见后代块。
    pub(super) fn last_visible_descendant(&self, entity_id: EntityId) -> Option<Entity<Block>> {
        let descendant_id = self
            .snapshot
            .last_visible_descendant_by_entity
            .get(&entity_id)
            .copied()?;
        self.block_entity_by_id(descendant_id)
    }

    /// 整体替换根块并重建元数据与快照。
    pub(super) fn replace_roots(&mut self, roots: Vec<Entity<Block>>, cx: &mut Context<Editor>) {
        self.roots = roots;
        self.rebuild_metadata_and_snapshot(cx);
    }

    /// 序列化整棵树为 Markdown 文本。
    pub(super) fn markdown_text(&self, cx: &App) -> String {
        let mut lines = Vec::new();
        Self::collect_root_markdown_lines(&self.roots, cx, &mut lines);
        lines.join("\n")
    }

    /// 读取源码模式的原始文本。
    pub(super) fn raw_source_text(&self, cx: &App) -> String {
        self.snapshot
            .visible
            .iter()
            .map(|visible| visible.entity.read(cx).rendered_text().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 在指定父节点与下标处插入一批块。
    pub(super) fn insert_blocks_at(
        &mut self,
        parent: Option<Entity<Block>>,
        index: usize,
        blocks: Vec<Entity<Block>>,
        cx: &mut Context<Editor>,
    ) {
        self.with_structure_mutation(cx, move |tree, cx| {
            tree.insert_blocks_at_raw(parent, index, blocks, cx);
        });
    }

    /// 执行一次树变更，随后恰好重建一次元数据与可见快照。
    pub(super) fn with_structure_mutation<R>(
        &mut self,
        cx: &mut Context<Editor>,
        mutate: impl FnOnce(&mut Self, &mut Context<Editor>) -> R,
    ) -> R {
        let result = mutate(self, cx);
        self.rebuild_metadata_and_snapshot(cx);
        result
    }

    /// 按当前根重建树元数据与可见顺序缓存。
    ///
    /// 先把叶子块上不可能存在的运行时子块上提归位，再做一次深度遍历，
    /// 更新父标识、子标识列表、渲染深度、编号列表序号与可见快照。
    pub(super) fn rebuild_metadata_and_snapshot(&mut self, cx: &mut Context<Editor>) {
        self.revision = self.revision.wrapping_add(1);
        Self::normalize_block_list(&mut self.roots, cx);
        self.snapshot.clear();
        Self::sync_block_list(
            &self.roots.clone(),
            None,
            None,
            &InheritedBlockContext::root(),
            cx,
            &mut self.snapshot,
        );
    }

    /// 取出块的全部子块（原位置置空）。
    pub(super) fn take_children(
        block: &Entity<Block>,
        cx: &mut Context<Editor>,
    ) -> Vec<Entity<Block>> {
        let mut children = Vec::new();
        block.update(cx, |block, _cx| {
            children = std::mem::take(&mut block.children);
        });
        children
    }

    /// 在指定位置插入块（底层实现，不重建快照）。
    pub(super) fn insert_blocks_at_raw(
        &mut self,
        parent: Option<Entity<Block>>,
        index: usize,
        blocks: Vec<Entity<Block>>,
        cx: &mut Context<Editor>,
    ) {
        if blocks.is_empty() {
            return;
        }

        if let Some(parent) = parent {
            parent.update(cx, move |parent, _cx| {
                for (offset, block) in blocks.iter().cloned().enumerate() {
                    parent.children.insert(index + offset, block);
                }
            });
        } else {
            for (offset, block) in blocks.into_iter().enumerate() {
                self.roots.insert(index + offset, block);
            }
        }
    }

    /// 按标识移除块（底层实现，不重建快照），返回被删块与原位置。
    pub(super) fn remove_block_by_id_raw(
        &mut self,
        entity_id: EntityId,
        cx: &mut Context<Editor>,
    ) -> Option<(Entity<Block>, BlockLocation)> {
        let location = self.find_block_location(entity_id)?;
        let removed = if let Some(parent) = location.parent.clone() {
            let mut removed = None;
            parent.update(cx, |parent, _cx| {
                removed = Some(parent.children.remove(location.index));
            });
            removed?
        } else {
            self.roots.remove(location.index)
        };

        Some((removed, location))
    }

    /// 规范化兄弟列表：仅容器类块保留子块。
    ///
    /// 挂在叶子块下的子块会上提到同一父列表中该叶子之后。
    fn normalize_block_list(blocks: &mut Vec<Entity<Block>>, cx: &mut Context<Editor>) {
        let mut index = 0;
        while index < blocks.len() {
            let block = blocks[index].clone();
            let mut children = Self::take_children(&block, cx);
            Self::normalize_block_list(&mut children, cx);

            if block.read(cx).kind().supports_children() {
                // children 已是 take_children 取出的新 Vec，直接 move 进去，
                // 避免为闭包额外克隆一份（两个分支互斥，各自只消费一次）。
                block.update(cx, move |block, _cx| {
                    block.children = children;
                });
            } else if !children.is_empty() {
                blocks.splice(index + 1..index + 1, children);
            }

            index += 1;
        }
    }

    /// 同步一批块的元数据并记入可见快照（深度遍历内部实现）。
    fn sync_block_list(
        blocks: &[Entity<Block>],
        parent_entity: Option<Entity<Block>>,
        parent_id: Option<uuid::Uuid>,
        inherited: &InheritedBlockContext,
        cx: &mut Context<Editor>,
        snapshot: &mut VisibleTreeSnapshot,
    ) {
        let mut numbered_list_ordinal = 0;
        let mut previous_was_list_item = false;
        for (index, block) in blocks.iter().enumerate() {
            let entity_id = block.entity_id();
            let visible_index = snapshot.visible.len();
            snapshot.visible.push(VisibleBlock {
                entity: block.clone(),
            });
            snapshot
                .visible_index_by_entity
                .insert(entity_id, visible_index);
            snapshot.location_by_entity.insert(
                entity_id,
                BlockLocation {
                    parent: parent_entity.clone(),
                    index,
                },
            );

            let (block_id, kind, children, is_empty_paragraph, affects_global_references) = {
                let block_ref = block.read(cx);
                let plain = block_ref.record.title.plain_text();
                (
                    block_ref.record.id,
                    block_ref.kind(),
                    block_ref.children.clone(),
                    block_ref.kind() == BlockKind::Paragraph
                        && plain.is_empty()
                        && block_ref.children.is_empty(),
                    // 引用定义/脚注语法标志随每次树同步刷新：撤销恢复、载入等
                    // 路径重建的块由此获得准确初值，编辑器据此决定是否重建
                    // 全局引用集合（events.rs 的 Changed 分支）。
                    block_ref.kind() == BlockKind::FootnoteDefinition
                        || plain.contains("]:")
                        || plain.contains("[^"),
                )
            };
            let parent_is_list_item = parent_entity
                .as_ref()
                .is_some_and(|parent| parent.read(cx).kind().is_list_item());

            let content = children
                .iter()
                .map(|child| child.read(cx).record.id)
                .collect::<Vec<_>>();
            let list_ordinal = if kind.is_numbered_list_item() {
                if numbered_list_ordinal == 0 {
                    // 新分组：起始号取组内首个条目解析出的原始编号（`3. a` 从 3
                    // 起排）；交互创建的条目没有原始编号，从 1 起排。组内后续
                    // 条目按位置续排（CommonMark 只承认首项起始号）。
                    numbered_list_ordinal = block
                        .read(cx)
                        .record
                        .list_number
                        .map(|number| number as usize)
                        .unwrap_or(1);
                } else {
                    numbered_list_ordinal += 1;
                }
                Some(numbered_list_ordinal)
            } else {
                numbered_list_ordinal = 0;
                None
            };
            let is_quote_container = kind.is_quote_container();
            let own_callout_variant = kind.callout_variant();
            let quote_depth = inherited.quote_depth + usize::from(is_quote_container);
            let quote_group_anchor = if is_quote_container {
                inherited.quote_group_anchor.or(Some(block_id))
            } else {
                inherited.quote_group_anchor
            };
            let callout_depth =
                inherited.callout_depth + usize::from(own_callout_variant.is_some());
            let callout_anchor = if own_callout_variant.is_some() {
                Some(block_id)
            } else {
                inherited.callout_anchor
            };
            let callout_variant = own_callout_variant.or(inherited.callout_variant);
            let visible_quote_depth = quote_depth.saturating_sub(callout_depth);
            let visible_quote_group_anchor = match kind {
                BlockKind::Quote => inherited.visible_quote_group_anchor.or(Some(block_id)),
                BlockKind::Callout(_) => None,
                _ if visible_quote_depth == 0 => None,
                _ => inherited.visible_quote_group_anchor,
            };
            let child_visible_quote_group_anchor = if own_callout_variant.is_some() {
                None
            } else {
                visible_quote_group_anchor
            };
            let footnote_anchor = if kind.is_footnote_definition() {
                Some(block_id)
            } else {
                inherited.footnote_anchor
            };
            let child_list_depth = inherited.list_depth + usize::from(kind.is_list_item());
            // 子块行的缩进列宽：列表项按自身标记正文列宽累计（`1. ` 为 3，
            // CommonMark 子块缩进语义）；引用/提示框内部不累计，`> ` 前缀已承担
            // 嵌套层级（与序列化 quote 分支的深度归零约定一致）。
            let child_source_indent_columns = if kind.is_quote_container() {
                inherited.source_indent_columns
            } else if kind.is_footnote_definition() {
                inherited.source_indent_columns + 4
            } else {
                inherited.source_indent_columns
                    + kind.marker_content_columns(list_ordinal) * usize::from(kind.is_list_item())
            };
            let list_group_separator_candidate = is_empty_paragraph && previous_was_list_item;

            block.update(cx, move |block, _cx| {
                block.record.parent = parent_id;
                block.record.content = content.clone();
                block.render_depth = inherited.list_depth;
                block.source_indent_columns = inherited.source_indent_columns;
                block.quote_depth = quote_depth;
                block.quote_group_anchor = quote_group_anchor;
                block.visible_quote_depth = visible_quote_depth;
                block.visible_quote_group_anchor = visible_quote_group_anchor;
                block.callout_depth = callout_depth;
                block.callout_anchor = callout_anchor;
                block.callout_variant = callout_variant;
                block.footnote_anchor = footnote_anchor;
                block.parent_is_list_item = parent_is_list_item;
                block.list_ordinal = list_ordinal;
                block.may_affect_global_references = affects_global_references;
                block.list_group_separator_candidate = list_group_separator_candidate;
            });

            let last_descendant_id = if children.is_empty() {
                entity_id
            } else {
                Self::sync_block_list(
                    &children,
                    Some(block.clone()),
                    Some(block_id),
                    &InheritedBlockContext {
                        list_depth: child_list_depth,
                        source_indent_columns: child_source_indent_columns,
                        quote_depth,
                        quote_group_anchor,
                        visible_quote_group_anchor: child_visible_quote_group_anchor,
                        callout_depth,
                        callout_anchor,
                        callout_variant,
                        footnote_anchor,
                    },
                    cx,
                    snapshot,
                );
                snapshot
                    .last_visible_descendant_by_entity
                    .get(&children.last().expect("children checked").entity_id())
                    .copied()
                    .unwrap_or_else(|| children.last().expect("children checked").entity_id())
            };

            snapshot
                .last_visible_descendant_by_entity
                .insert(entity_id, last_descendant_id);
            previous_was_list_item = kind.is_list_item();
        }
    }

    /// 判断是否为无内容的根空段落。
    fn is_empty_root_paragraph(block: &Block) -> bool {
        block.kind() == BlockKind::Paragraph
            && block.record.title.plain_text().is_empty()
            && block.children.is_empty()
    }

    /// 收集根块的 Markdown 行（含空段落分隔规则）。
    fn collect_root_markdown_lines(blocks: &[Entity<Block>], cx: &App, lines: &mut Vec<String>) {
        let mut pending_empty_roots = 0usize;
        let mut wrote_non_empty_root = false;
        let mut previous_was_list_item = false;

        for block in blocks {
            let block_ref = block.read(cx);
            if Self::is_empty_root_paragraph(block_ref) {
                pending_empty_roots += 1;
                continue;
            }

            let current_is_list_item = block_ref.kind().is_list_item();
            if wrote_non_empty_root {
                let separator_count = if previous_was_list_item && current_is_list_item {
                    pending_empty_roots
                } else {
                    pending_empty_roots + 1
                };
                lines.extend(std::iter::repeat_n(String::new(), separator_count));
            } else if pending_empty_roots > 0 {
                lines.extend(std::iter::repeat_n(String::new(), pending_empty_roots));
            }

            Self::collect_single_block_markdown_lines(block_ref, 0, cx, lines);
            wrote_non_empty_root = true;
            pending_empty_roots = 0;
            previous_was_list_item = current_is_list_item;
        }

        if wrote_non_empty_root {
            if pending_empty_roots > 0 {
                lines.extend(std::iter::repeat_n(String::new(), pending_empty_roots + 1));
            }
        } else if pending_empty_roots > 1 {
            lines.extend(std::iter::repeat_n(String::new(), pending_empty_roots));
        }
    }

    /// 收集单个块及其子块的 Markdown 行。
    ///
    /// `indent_columns` 为该块自身行的缩进列数（非层级数）：列表项子块按父项
    /// 标记正文列宽累计（`1. ` 为 3），引用/提示框内部归零由 `> ` 前缀承担。
    /// 与 `source_map.rs` 的映射遍历、`selection.rs` 的复制回退共用同一约定。
    fn collect_single_block_markdown_lines(
        block_ref: &Block,
        indent_columns: usize,
        cx: &App,
        lines: &mut Vec<String>,
    ) {
        match block_ref.kind() {
            BlockKind::Table => {
                if let Some(table) = block_ref.record.table.as_ref() {
                    // 与代码块、Raw 分支一致：嵌套在列表项里的表格行也要带缩进，
                    // 否则产物会脱离列表（且与源码映射的前缀长度不一致）。
                    let indentation = " ".repeat(indent_columns);
                    for line in table_to_markdown_lines(table) {
                        if indentation.is_empty() {
                            lines.push(line);
                        } else {
                            lines.push(format!("{indentation}{line}"));
                        }
                    }
                }
            }
            BlockKind::CodeBlock { language } => {
                let indentation = " ".repeat(indent_columns);
                let lang_str = language.as_ref().map(|s| s.as_ref()).unwrap_or("");
                let fence = super::persistence::code_fence_markdown(
                    &block_ref.record.title.plain_text(),
                    language.as_ref().map(|language| language.as_ref()),
                    block_ref.record.code_fence,
                );
                lines.push(format!("{indentation}{fence}{lang_str}"));
                let content = block_ref.record.title.plain_text();
                for code_line in content.split('\n') {
                    lines.push(format!("{indentation}{code_line}"));
                }
                lines.push(format!("{indentation}{fence}"));
            }
            BlockKind::Quote => {
                let title_to_markdown =
                    CalloutVariant::escape_quote_header(&block_ref.record.title_to_markdown());
                let indentation = " ".repeat(indent_columns);
                if !title_to_markdown.is_empty() || block_ref.children.is_empty() {
                    for line in title_to_markdown.split('\n') {
                        lines.push(format!("{indentation}> {line}"));
                    }
                }

                if !block_ref.children.is_empty() {
                    let mut child_lines = Vec::new();
                    // 引用内部子块的列表层级从 0 重新起算：引用前缀 `> ` 已经提供
                    // 了嵌套层级，若继续叠加外层列表缩进，表格/代码块等自带缩进的
                    // 子块会多出两格（保存后与打开时不一致）。
                    Self::collect_markdown_lines(
                        &block_ref.children,
                        0,
                        cx,
                        &mut child_lines,
                        false,
                    );
                    lines.extend(
                        child_lines
                            .into_iter()
                            .map(|line| format!("{indentation}> {line}")),
                    );
                }
            }
            BlockKind::Callout(variant) => {
                let indentation = " ".repeat(indent_columns);
                lines.push(format!(
                    "{indentation}> {}",
                    variant.heading_markdown(&block_ref.record.title_to_markdown())
                ));
                if !block_ref.children.is_empty() {
                    let mut child_lines = Vec::new();
                    // 与引用分支同理：提示框内部子块从 0 层重新起算。
                    Self::collect_markdown_lines(
                        &block_ref.children,
                        0,
                        cx,
                        &mut child_lines,
                        false,
                    );
                    lines.extend(
                        child_lines
                            .into_iter()
                            .map(|line| format!("{indentation}> {line}")),
                    );
                }
            }
            BlockKind::FootnoteDefinition => {
                let indentation = " ".repeat(indent_columns);
                let id = block_ref.record.title.plain_text();
                if block_ref.children.is_empty() {
                    lines.push(format!("{indentation}[^{}]:", id));
                    return;
                }

                let first_child = block_ref.children.first().cloned().expect("checked");
                let first_is_paragraph = first_child.read(cx).kind() == BlockKind::Paragraph;
                if first_is_paragraph {
                    let first_title = first_child.read(cx).record.title_to_markdown();
                    let mut first_lines = first_title.split('\n');
                    let first_line = first_lines.next().unwrap_or_default();
                    lines.push(format!("{indentation}[^{}]: {}", id, first_line));
                    for line in first_lines {
                        if line.is_empty() {
                            lines.push(String::new());
                        } else {
                            lines.push(format!("{indentation}    {line}"));
                        }
                    }

                    if block_ref.children.len() > 1 {
                        lines.push(String::new());
                        Self::collect_markdown_lines(&block_ref.children[1..], 4, cx, lines, true);
                    }
                } else {
                    lines.push(format!("{indentation}[^{}]:", id));
                    Self::collect_markdown_lines(&block_ref.children, 4, cx, lines, true);
                }
            }
            BlockKind::RawMarkdown
            | BlockKind::Comment
            | BlockKind::HtmlBlock
            | BlockKind::MathBlock
            | BlockKind::MermaidBlock => {
                let indentation = " ".repeat(indent_columns);
                let raw_markdown = block_ref
                    .record
                    .raw_fallback
                    .clone()
                    .unwrap_or_else(|| block_ref.record.title_to_markdown());
                for line in raw_markdown.split('\n') {
                    if indentation.is_empty() {
                        lines.push(line.to_string());
                    } else {
                        lines.push(format!("{indentation}{line}"));
                    }
                }
            }
            BlockKind::BulletedListItem
            | BlockKind::TaskListItem { .. }
            | BlockKind::NumberedListItem => {
                lines.push(
                    block_ref
                        .record
                        .markdown_line(indent_columns, block_ref.list_ordinal),
                );
                // 子块缩进到本项正文列：`- ` 为 2 列，`1. ` 为 3 列、`10. ` 为 4 列
                // （CommonMark 子块缩进语义，跨渲染器结构一致）。
                let child_indent_columns = indent_columns
                    + block_ref
                        .kind()
                        .marker_content_columns(block_ref.list_ordinal);
                for child in &block_ref.children {
                    let child_ref = child.read(cx);
                    if Self::list_child_requires_leading_blank_line(child_ref) {
                        lines.push(String::new());
                    }
                    Self::collect_single_block_markdown_lines(
                        child_ref,
                        child_indent_columns,
                        cx,
                        lines,
                    );
                }
            }
            _ => {
                lines.push(
                    block_ref
                        .record
                        .markdown_line(indent_columns, block_ref.list_ordinal),
                );
                let child_indent_columns = indent_columns
                    + block_ref
                        .kind()
                        .marker_content_columns(block_ref.list_ordinal)
                        * usize::from(block_ref.kind().is_list_item());
                Self::collect_markdown_lines(
                    &block_ref.children,
                    child_indent_columns,
                    cx,
                    lines,
                    false,
                );
            }
        }
    }

    /// 判断列表子段落前是否需要空行分隔（排除独立图片）。
    pub(super) fn list_child_requires_leading_blank_line(block_ref: &Block) -> bool {
        if block_ref.kind() != BlockKind::Paragraph || !block_ref.children.is_empty() {
            return false;
        }

        let markdown = block_ref.record.title_to_markdown();
        !markdown.is_empty() && parse_standalone_image(&markdown).is_none()
    }

    /// 收集一批兄弟块的 Markdown 行（可选兄弟间空行）。
    fn collect_markdown_lines(
        blocks: &[Entity<Block>],
        indent_columns: usize,
        cx: &App,
        lines: &mut Vec<String>,
        blank_line_between_siblings: bool,
    ) {
        let mut first = true;
        let mut previous_was_list_item = false;
        for block in blocks {
            let current_is_list_item = block.read(cx).kind().is_list_item();
            if !first
                && blank_line_between_siblings
                && !(previous_was_list_item && current_is_list_item)
            {
                lines.push(String::new());
            }
            first = false;

            let block_ref = block.read(cx);
            Self::collect_single_block_markdown_lines(block_ref, indent_columns, cx, lines);
            previous_was_list_item = current_is_list_item;
        }
    }
}

#[cfg(test)]
mod tests {
    use rgpui::{AppContext, TestAppContext};

    use crate::components::{BlockKind, BlockRecord, Editor};

    #[rgpui::test]
    async fn snapshot_tracks_nested_visible_order(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "- a\n  - b\n    - c\n- d".to_string(), None));

        editor.update(cx, |editor, _cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let a = visible[0].entity.clone();
            let b = visible[1].entity.clone();
            let c = visible[2].entity.clone();
            let d = visible[3].entity.clone();

            assert_eq!(
                editor.document.visible_index_for_entity_id(a.entity_id()),
                Some(0)
            );
            assert_eq!(
                editor.document.visible_index_for_entity_id(b.entity_id()),
                Some(1)
            );
            assert_eq!(
                editor.document.visible_index_for_entity_id(c.entity_id()),
                Some(2)
            );
            assert_eq!(
                editor.document.visible_index_for_entity_id(d.entity_id()),
                Some(3)
            );

            let c_location = editor
                .document
                .find_block_location(c.entity_id())
                .expect("location");
            assert_eq!(
                c_location.parent.expect("nested parent").entity_id(),
                b.entity_id()
            );
            assert_eq!(c_location.index, 0);

            assert_eq!(
                editor
                    .document
                    .last_visible_descendant(a.entity_id())
                    .expect("descendant")
                    .entity_id(),
                c.entity_id()
            );
        });
    }

    #[rgpui::test]
    async fn rebuild_hoists_children_from_leaf_blocks(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let root = editor.document.first_root().expect("root").clone();
            let child = Editor::new_block(cx, BlockRecord::paragraph("child"));

            root.update(cx, {
                let child = child.clone();
                move |root, _cx| {
                    root.children.push(child.clone());
                }
            });

            editor.document.rebuild_metadata_and_snapshot(cx);

            assert!(root.read(cx).children.is_empty());
            let visible_ids = editor
                .document
                .visible_blocks()
                .iter()
                .map(|visible| visible.entity.entity_id())
                .collect::<Vec<_>>();
            assert_eq!(visible_ids, vec![root.entity_id(), child.entity_id()]);

            let location = editor
                .document
                .find_block_location(child.entity_id())
                .expect("child location");
            assert!(location.parent.is_none());
            assert_eq!(location.index, 1);
        });
    }

    #[rgpui::test]
    async fn code_block_language_edit_serializes_to_opening_fence(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "```rust\nfn main() {}\n```".into(), None));

        editor.update(cx, |editor, cx| {
            let block = editor.document.first_root().expect("code block").clone();
            block.update(cx, |block, cx| {
                let range = 0..block.code_language_text().len();
                block.replace_code_language_text_in_range(range, "unknown-lang", None, false, cx);
            });

            assert_eq!(
                editor.document.markdown_text(cx),
                "```unknown-lang\nfn main() {}\n```"
            );
        });
    }

    #[rgpui::test]
    async fn code_block_language_with_backtick_round_trips_with_tilde_fence(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "```rust\nbody\n```".into(), None));

        let markdown = editor.update(cx, |editor, cx| {
            let block = editor.document.first_root().expect("code block").clone();
            block.update(cx, |block, cx| {
                let range = 0..block.code_language_text().len();
                block.replace_code_language_text_in_range(range, "we`rd", None, false, cx);
            });
            editor.document.markdown_text(cx)
        });

        assert_eq!(markdown, "~~~we`rd\nbody\n~~~");

        let round_tripped = cx.new(|cx| Editor::from_markdown(cx, markdown, None));
        round_tripped.update(cx, |editor, cx| {
            let block = editor.document.first_root().expect("code block");
            assert_eq!(block.read(cx).code_language_text(), "we`rd");
            assert!(matches!(block.read(cx).kind(), BlockKind::CodeBlock { .. }));
        });
    }

    #[rgpui::test]
    async fn structure_mutation_rebuilds_snapshot_after_relocation(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- a\n- b\n- c".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let a = visible[0].entity.clone();
            let b = visible[1].entity.clone();
            let c = visible[2].entity.clone();

            editor.document.with_structure_mutation(cx, |document, cx| {
                let moved = document
                    .remove_block_by_id_raw(c.entity_id(), cx)
                    .expect("remove c")
                    .0;
                document.insert_blocks_at_raw(
                    Some(a.clone()),
                    a.read(cx).children.len(),
                    vec![moved],
                    cx,
                );
            });

            assert_eq!(
                editor.document.visible_index_for_entity_id(a.entity_id()),
                Some(0)
            );
            assert_eq!(
                editor.document.visible_index_for_entity_id(c.entity_id()),
                Some(1)
            );
            assert_eq!(
                editor.document.visible_index_for_entity_id(b.entity_id()),
                Some(2)
            );

            let c_location = editor
                .document
                .find_block_location(c.entity_id())
                .expect("c location");
            assert_eq!(
                c_location.parent.expect("nested parent").entity_id(),
                a.entity_id()
            );
            assert_eq!(c_location.index, 0);

            assert_eq!(
                editor
                    .document
                    .last_visible_descendant(a.entity_id())
                    .expect("descendant")
                    .entity_id(),
                c.entity_id()
            );
        });
    }
}
