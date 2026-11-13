//! 规范 Markdown 与渲染块之间的源码偏移映射。

use std::ops::Range;

use super::*;

impl Editor {
    /// 按当前视图模式读取文档源码文本。
    pub(super) fn current_document_source(&self, cx: &App) -> String {
        let derived = self.derived_document(cx);
        match self.view_mode {
            ViewMode::Rendered => derived.markdown.to_string(),
            ViewMode::Source => derived.raw_source.to_string(),
        }
    }

    /// 判断是否为无内容的空段落分隔块。
    pub(super) fn is_empty_paragraph_separator(block: &Block) -> bool {
        block.kind() == BlockKind::Paragraph
            && block.record.title.plain_text().is_empty()
            && block.children.is_empty()
    }

    /// 判断是否为无内容的根空段落块。
    pub(super) fn is_empty_root_paragraph(block: &Block) -> bool {
        Self::is_empty_paragraph_separator(block)
    }

    /// 为带行首缀的内容构建源码映射（含首行与续行前缀）。
    pub(super) fn build_prefixed_content_mapping(
        content: &str,
        first_prefix: &str,
        continuation_prefix: &str,
    ) -> (String, Vec<usize>, Vec<usize>) {
        let mut full = String::new();
        let mut content_to_source = vec![0; content.len() + 1];
        let mut source_to_content = vec![0];

        full.push_str(first_prefix);
        source_to_content.resize(full.len() + 1, 0);

        let mut content_offset = 0usize;
        while content_offset < content.len() {
            content_to_source[content_offset] = full.len();
            let ch = content[content_offset..]
                .chars()
                .next()
                .expect("content offset should stay on char boundaries");
            let start = full.len();
            full.push(ch);
            source_to_content.resize(full.len() + 1, content_offset);
            source_to_content[start..=full.len()].fill(content_offset);
            content_offset += ch.len_utf8();
            if ch == '\n' {
                let prefix_start = full.len();
                full.push_str(continuation_prefix);
                source_to_content.resize(full.len() + 1, content_offset);
                source_to_content[prefix_start..=full.len()].fill(content_offset);
            }
        }
        content_to_source[content.len()] = full.len();
        source_to_content[full.len()] = content.len();

        (full, content_to_source, source_to_content)
    }

    /// 为代码块内容构建源码映射（含围栏与缩进）。
    pub(super) fn build_code_block_content_mapping(
        content: &str,
        indentation: &str,
        language: Option<&SharedString>,
    ) -> (String, Vec<usize>, Vec<usize>) {
        let fence = self::persistence::safe_code_fence_with_info(
            content,
            language.map(|language| language.as_ref()),
        );
        let mut full = String::new();
        let mut content_to_source = vec![0; content.len() + 1];
        let mut source_to_content = vec![0];

        // 开围栏行也要带列表缩进，与 `tree.rs::collect_single_block_markdown_lines`
        // 逐行输出保持一致（否则嵌套列表内代码块的映射会比真实源码短）。
        full.push_str(indentation);
        full.push_str(&fence);
        if let Some(language) = language {
            full.push_str(language);
        }
        full.push('\n');
        source_to_content.resize(full.len() + 1, 0);

        let prefix_start = full.len();
        full.push_str(indentation);
        source_to_content.resize(full.len() + 1, 0);
        source_to_content[prefix_start..=full.len()].fill(0);

        let mut content_offset = 0usize;
        while content_offset < content.len() {
            content_to_source[content_offset] = full.len();
            let ch = content[content_offset..]
                .chars()
                .next()
                .expect("content offset should stay on char boundaries");
            let start = full.len();
            full.push(ch);
            source_to_content.resize(full.len() + 1, content_offset);
            source_to_content[start..=full.len()].fill(content_offset);
            content_offset += ch.len_utf8();
            if ch == '\n' {
                let line_prefix_start = full.len();
                full.push_str(indentation);
                source_to_content.resize(full.len() + 1, content_offset);
                source_to_content[line_prefix_start..=full.len()].fill(content_offset);
            }
        }
        content_to_source[content.len()] = full.len();
        source_to_content[full.len()] = content.len();

        full.push('\n');
        source_to_content.resize(full.len() + 1, content.len());
        // 闭围栏行同样带缩进。
        full.push_str(indentation);
        source_to_content.resize(full.len() + 1, content.len());
        full.push_str(&fence);
        source_to_content.resize(full.len() + 1, content.len());
        source_to_content[full.len()] = content.len();

        (full, content_to_source, source_to_content)
    }

    /// 记录行内块的源码映射并返回其源码长度。
    // 映射收集管线按扁平传参约定透传前缀与累加器，打包结构体反而引入生命周期复杂度。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_inline_block_mapping(
        &self,
        block: &Entity<Block>,
        content_markdown: String,
        first_prefix: String,
        continuation_prefix: String,
        quote_depth: usize,
        absolute_start: usize,
        mappings: &mut Vec<SourceMapEntry>,
    ) -> usize {
        let (full_text, content_to_source, source_to_content) =
            Self::build_prefixed_content_mapping(
                &content_markdown,
                &first_prefix,
                &continuation_prefix,
            );
        let (full_text, content_to_source, source_to_content) =
            Self::wrap_source_mapping_with_quotes(
                full_text,
                content_to_source,
                source_to_content,
                quote_depth,
            );
        mappings.push(SourceMapEntry {
            entity: block.clone(),
            full_source_range: absolute_start..absolute_start + full_text.len(),
            content_to_source: Arc::new(content_to_source),
            source_to_content: Arc::new(source_to_content),
        });
        full_text.len()
    }

    /// 记录脚注定义头部的源码映射并返回其源码长度。
    pub(super) fn push_footnote_definition_head_mapping(
        block: &Entity<Block>,
        footnote_id: &str,
        include_trailing_space: bool,
        indentation: &str,
        quote_depth: usize,
        absolute_start: usize,
        mappings: &mut Vec<SourceMapEntry>,
    ) -> usize {
        // 头行前缀是「列表缩进 + `> `×引用层数」：`> ` 由下方包裹补在外侧，
        // 两者长度相加与 tree.rs 输出的真实行一致（否则嵌套在列表里的脚注
        // 映射会短一截，整块复制被截断）。
        let mut full_text = format!("{indentation}[^{footnote_id}]:");
        if include_trailing_space {
            full_text.push(' ');
        }

        let mut content_to_source = vec![0; footnote_id.len() + 1];
        let mut source_to_content = vec![0; full_text.len() + 1];
        let id_start = indentation.len() + 2;
        for (offset, slot) in content_to_source
            .iter_mut()
            .enumerate()
            .take(footnote_id.len() + 1)
        {
            *slot = id_start + offset;
        }
        for (source_offset, slot) in source_to_content
            .iter_mut()
            .enumerate()
            .take(full_text.len() + 1)
        {
            *slot = if source_offset <= id_start {
                0
            } else if source_offset >= id_start + footnote_id.len() {
                footnote_id.len()
            } else {
                source_offset - id_start
            };
        }

        let (full_text, content_to_source, source_to_content) =
            Self::wrap_source_mapping_with_quotes(
                full_text,
                content_to_source,
                source_to_content,
                quote_depth,
            );
        mappings.push(SourceMapEntry {
            entity: block.clone(),
            full_source_range: absolute_start..absolute_start + full_text.len(),
            content_to_source: Arc::new(content_to_source),
            source_to_content: Arc::new(source_to_content),
        });
        full_text.len()
    }

    /// 记录原样保留块的源码映射并返回其源码长度。
    pub(super) fn push_raw_block_mapping(
        &self,
        block: &Entity<Block>,
        quote_depth: usize,
        absolute_start: usize,
        mappings: &mut Vec<SourceMapEntry>,
        cx: &App,
    ) -> usize {
        let (content, indentation) = {
            let block_ref = block.read(cx);
            (
                block_ref.rendered_text().to_string(),
                if block_ref.render_depth == 0 {
                    String::new()
                } else {
                    "  ".repeat(block_ref.render_depth)
                },
            )
        };
        let (full_text, content_to_source, source_to_content) =
            Self::build_prefixed_content_mapping(&content, &indentation, &indentation);
        let (full_text, content_to_source, source_to_content) =
            Self::wrap_source_mapping_with_quotes(
                full_text,
                content_to_source,
                source_to_content,
                quote_depth,
            );
        mappings.push(SourceMapEntry {
            entity: block.clone(),
            full_source_range: absolute_start..absolute_start + full_text.len(),
            content_to_source: Arc::new(content_to_source),
            source_to_content: Arc::new(source_to_content),
        });
        full_text.len()
    }

    /// 记录代码块的源码映射并返回其源码长度。
    pub(super) fn push_code_block_mapping(
        &self,
        block: &Entity<Block>,
        quote_depth: usize,
        absolute_start: usize,
        mappings: &mut Vec<SourceMapEntry>,
        cx: &App,
    ) -> usize {
        let (language, indentation, content) = {
            let block_ref = block.read(cx);
            (
                match block_ref.kind() {
                    BlockKind::CodeBlock { language } => language.clone(),
                    _ => None,
                },
                "  ".repeat(block_ref.render_depth),
                block_ref.rendered_text().to_string(),
            )
        };

        let (full_text, content_to_source, source_to_content) =
            Self::build_code_block_content_mapping(&content, &indentation, language.as_ref());
        let (full_text, content_to_source, source_to_content) =
            Self::wrap_source_mapping_with_quotes(
                full_text,
                content_to_source,
                source_to_content,
                quote_depth,
            );
        mappings.push(SourceMapEntry {
            entity: block.clone(),
            full_source_range: absolute_start..absolute_start + full_text.len(),
            content_to_source: Arc::new(content_to_source),
            source_to_content: Arc::new(source_to_content),
        });
        full_text.len()
    }

    /// 按引用层数为源码映射包裹引用前缀。
    pub(super) fn wrap_source_mapping_with_quotes(
        mut full_text: String,
        mut content_to_source: Vec<usize>,
        mut source_to_content: Vec<usize>,
        quote_depth: usize,
    ) -> (String, Vec<usize>, Vec<usize>) {
        for _ in 0..quote_depth {
            let (wrapped_text, inner_to_wrapped, wrapped_to_inner) =
                Self::build_prefixed_content_mapping(&full_text, "> ", "> ");
            let max_inner_to_wrapped = inner_to_wrapped.len().saturating_sub(1);
            let max_source_to_content = source_to_content.len().saturating_sub(1);

            let wrapped_content_to_source = content_to_source
                .iter()
                .map(|offset| inner_to_wrapped[(*offset).min(max_inner_to_wrapped)])
                .collect::<Vec<_>>();
            let wrapped_source_to_content = wrapped_to_inner
                .iter()
                .map(|offset| source_to_content[(*offset).min(max_source_to_content)])
                .collect::<Vec<_>>();

            full_text = wrapped_text;
            content_to_source = wrapped_content_to_source;
            source_to_content = wrapped_source_to_content;
        }

        (full_text, content_to_source, source_to_content)
    }

    /// 记录表格各单元格的源码映射并返回表格源码总长。
    pub(super) fn push_table_mappings(
        &self,
        block: &Entity<Block>,
        list_depth: usize,
        quote_depth: usize,
        absolute_start: usize,
        mappings: &mut Vec<SourceMapEntry>,
        cx: &App,
    ) -> usize {
        let Some(table) = block.read(cx).record.table.clone() else {
            return 0;
        };
        let Some(runtime) = block.read(cx).table_runtime.clone() else {
            return 0;
        };

        let lines = crate::components::table_to_markdown_lines(&table);
        let indentation = "  ".repeat(list_depth);
        let quote_prefix = "> ".repeat(quote_depth);
        let line_prefix_len = indentation.len() + quote_prefix.len();
        let mut line_start = absolute_start;

        if let Some(header_line) = lines.first() {
            let mut line_cursor = line_prefix_len + 2usize;
            for (column, cell) in runtime.header.iter().enumerate() {
                let Some(tree) = table.header.get(column) else {
                    continue;
                };
                let cell_markdown = cell_to_markdown(tree);
                let start = line_start + line_cursor;
                let len = cell_markdown.len();
                mappings.push(SourceMapEntry {
                    entity: cell.clone(),
                    full_source_range: start..start + len,
                    content_to_source: Arc::new((0..=len).collect()),
                    source_to_content: Arc::new((0..=len).collect()),
                });
                line_cursor += len + 3;
            }
            line_start += line_prefix_len + header_line.len() + 1;
        }

        if lines.len() > 1 {
            line_start += line_prefix_len + lines[1].len() + 1;
        }

        for (body_row_index, row) in runtime.rows.iter().enumerate() {
            let Some(row_line) = lines.get(body_row_index + 2) else {
                break;
            };
            let mut line_cursor = line_prefix_len + 2usize;
            for (column, cell) in row.iter().enumerate() {
                let Some(tree) = table
                    .rows
                    .get(body_row_index)
                    .and_then(|table_row| table_row.get(column))
                else {
                    continue;
                };
                let cell_markdown = cell_to_markdown(tree);
                let start = line_start + line_cursor;
                let len = cell_markdown.len();
                mappings.push(SourceMapEntry {
                    entity: cell.clone(),
                    full_source_range: start..start + len,
                    content_to_source: Arc::new((0..=len).collect()),
                    source_to_content: Arc::new((0..=len).collect()),
                });
                line_cursor += len + 3;
            }
            line_start += line_prefix_len + row_line.len() + 1;
        }

        lines
            .iter()
            .map(|line| line_prefix_len + line.len())
            .sum::<usize>()
            + lines.len().saturating_sub(1)
    }

    /// 收集单个块及其子块的源码映射并返回其源码总长。
    // 映射收集管线按扁平传参约定透传前缀与累加器，打包结构体反而引入生命周期复杂度。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn collect_single_block_source_mappings(
        &self,
        block: &Entity<Block>,
        list_depth: usize,
        quote_depth: usize,
        absolute_start: usize,
        mappings: &mut Vec<SourceMapEntry>,
        block_ranges: &mut HashMap<EntityId, Range<usize>>,
        cx: &App,
    ) -> usize {
        let (kind, list_ordinal, title, children) = {
            let block_ref = block.read(cx);
            let kind = block_ref.kind();
            let title = (!matches!(
                kind,
                BlockKind::Table
                    | BlockKind::CodeBlock { .. }
                    | BlockKind::Comment
                    | BlockKind::HtmlBlock
                    | BlockKind::MathBlock
                    | BlockKind::MermaidBlock
                    | BlockKind::RawMarkdown
                    | BlockKind::Separator
            ))
            .then(|| block_ref.record.title.markdown_offset_map());
            (
                kind,
                block_ref.list_ordinal,
                title,
                block_ref.children.clone(),
            )
        };

        let own_len = match kind {
            BlockKind::Table => self.push_table_mappings(
                block,
                list_depth,
                quote_depth,
                absolute_start,
                mappings,
                cx,
            ),
            BlockKind::CodeBlock { .. } => {
                self.push_code_block_mapping(block, quote_depth, absolute_start, mappings, cx)
            }
            BlockKind::RawMarkdown
            | BlockKind::Comment
            | BlockKind::HtmlBlock
            | BlockKind::MathBlock
            | BlockKind::MermaidBlock => {
                self.push_raw_block_mapping(block, quote_depth, absolute_start, mappings, cx)
            }
            BlockKind::Separator => {
                let line = block
                    .read(cx)
                    .record
                    .markdown_line(list_depth, list_ordinal);
                if quote_depth == 0 {
                    line.len()
                } else {
                    Self::wrap_source_mapping_with_quotes(
                        line.clone(),
                        (0..=line.len()).collect(),
                        (0..=line.len()).collect(),
                        quote_depth,
                    )
                    .0
                    .len()
                }
            }
            BlockKind::Heading { level } => self.push_inline_block_mapping(
                block,
                title.expect("heading title").markdown().to_string(),
                format!("{}{} ", "  ".repeat(list_depth), "#".repeat(level as usize)),
                String::new(),
                quote_depth,
                absolute_start,
                mappings,
            ),
            BlockKind::Paragraph => {
                let indentation = "  ".repeat(list_depth);
                self.push_inline_block_mapping(
                    block,
                    title.expect("paragraph title").markdown().to_string(),
                    indentation.clone(),
                    indentation,
                    quote_depth,
                    absolute_start,
                    mappings,
                )
            }
            BlockKind::BulletedListItem => {
                let indentation = "  ".repeat(list_depth);
                self.push_inline_block_mapping(
                    block,
                    title.expect("bullet title").markdown().to_string(),
                    format!("{indentation}- "),
                    format!("{indentation}  "),
                    quote_depth,
                    absolute_start,
                    mappings,
                )
            }
            BlockKind::TaskListItem { checked } => {
                let indentation = "  ".repeat(list_depth);
                self.push_inline_block_mapping(
                    block,
                    title.expect("task title").markdown().to_string(),
                    format!("{indentation}- [{}] ", if checked { "x" } else { " " }),
                    format!("{indentation}      "),
                    quote_depth,
                    absolute_start,
                    mappings,
                )
            }
            BlockKind::NumberedListItem => {
                let indentation = "  ".repeat(list_depth);
                let ordinal = list_ordinal.unwrap_or(1);
                self.push_inline_block_mapping(
                    block,
                    title.expect("numbered title").markdown().to_string(),
                    format!("{indentation}{ordinal}. "),
                    format!("{indentation}   "),
                    quote_depth,
                    absolute_start,
                    mappings,
                )
            }
            BlockKind::Quote => {
                let title = title.expect("quote title").markdown().to_string();
                if title.is_empty() && !children.is_empty() {
                    0
                } else {
                    // 引用行的真实前缀是「列表缩进 + `> `」；`push_inline_block_mapping`
                    // 把 `> ` 包在缩进外侧，两者长度相加与原行一致，映射偏移才对齐。
                    let indentation = "  ".repeat(list_depth);
                    self.push_inline_block_mapping(
                        block,
                        title,
                        indentation.clone(),
                        indentation,
                        quote_depth + 1,
                        absolute_start,
                        mappings,
                    )
                }
            }
            BlockKind::Callout(variant) => {
                let title_to_markdown = title.expect("callout title").markdown().to_string();
                let indentation = "  ".repeat(list_depth);
                if title_to_markdown.is_empty() {
                    let head = format!("{indentation}[!{}]", variant.marker());
                    let full_text = Self::wrap_source_mapping_with_quotes(
                        head.clone(),
                        vec![0],
                        vec![0; head.len() + 1],
                        quote_depth + 1,
                    )
                    .0;
                    mappings.push(SourceMapEntry {
                        entity: block.clone(),
                        full_source_range: absolute_start..absolute_start + full_text.len(),
                        content_to_source: Arc::new(vec![full_text.len()]),
                        source_to_content: Arc::new(vec![0; full_text.len() + 1]),
                    });
                    full_text.len()
                } else {
                    self.push_inline_block_mapping(
                        block,
                        title_to_markdown,
                        format!("{indentation}[!{}] ", variant.marker()),
                        indentation,
                        quote_depth + 1,
                        absolute_start,
                        mappings,
                    )
                }
            }
            BlockKind::FootnoteDefinition => {
                let footnote_id = title.expect("footnote id").markdown().to_string();
                let first_child = children.first().cloned();
                let first_is_paragraph = first_child
                    .as_ref()
                    .is_some_and(|child| child.read(cx).kind() == BlockKind::Paragraph);
                Self::push_footnote_definition_head_mapping(
                    block,
                    &footnote_id,
                    first_is_paragraph,
                    &"  ".repeat(list_depth),
                    quote_depth,
                    absolute_start,
                    mappings,
                )
            }
        };

        if kind == BlockKind::FootnoteDefinition {
            let mut total_len = own_len;
            let mut child_index = 0usize;
            if let Some(first_child) = children.first()
                && first_child.read(cx).kind() == BlockKind::Paragraph
            {
                // 头行 `[^id]: ` 已由脚注自身的映射覆盖，首段只映射正文，
                // 否则整块复制会把头行算两遍；首行接在头行同一行上、自身不带
                // 前缀，续行前缀才是「列表缩进 + `> `×引用层数 + 4 空格」
                // （与 tree.rs 脚注分支逐行输出一致），故不用逐行引用包裹。
                let mut continuation = "  ".repeat(list_depth);
                continuation.push_str(&"> ".repeat(quote_depth));
                continuation.push_str("    ");
                total_len += self.push_inline_block_mapping(
                    first_child,
                    first_child
                        .read(cx)
                        .record
                        .title
                        .markdown_offset_map()
                        .markdown()
                        .to_string(),
                    String::new(),
                    continuation,
                    0,
                    absolute_start + total_len,
                    mappings,
                );
                child_index = 1;
            }

            let mut previous_kind = if child_index > 0 {
                Some(BlockKind::Paragraph)
            } else {
                None
            };
            // 脚注头行与其后子块之间的空行落在引用/列表容器内时同样带容器
            // 逐行前缀（tree.rs 只 push 空串，前缀由外层引用补齐），只按一个
            // 换行记长会让其后所有块的映射整体前移。
            let blank_line_prefix = "  ".repeat(list_depth) + &"> ".repeat(quote_depth);
            for child in children.iter().skip(child_index) {
                let current_kind = child.read(cx).kind();
                if total_len > 0 {
                    total_len += if previous_kind.is_none()
                        || previous_kind.as_ref().is_some_and(|previous| {
                            previous.is_list_item() && current_kind.is_list_item()
                        }) {
                        1
                    } else {
                        2 + blank_line_prefix.len()
                    };
                }
                total_len += self.collect_single_block_source_mappings(
                    child,
                    list_depth + 2,
                    quote_depth,
                    absolute_start + total_len,
                    mappings,
                    block_ranges,
                    cx,
                );
                previous_kind = Some(current_kind);
            }
            block_ranges.insert(
                block.entity_id(),
                absolute_start..absolute_start + total_len,
            );
            return total_len;
        }

        let child_list_depth = list_depth + usize::from(kind.is_list_item());
        let child_quote_depth = quote_depth + usize::from(kind.is_quote_container());
        // 同理：子块前的空行若落在引用/列表容器内，需要加上容器逐行前缀。
        let blank_line_prefix = "  ".repeat(list_depth) + &"> ".repeat(quote_depth);
        let mut total_len = own_len;
        for child in children {
            if total_len > 0 {
                // 列表项子块：只有纯段落子块前补空行（与 tree.rs
                // `collect_single_block_markdown_lines` 的列表分支一致），
                // 代码块/表格/子列表紧贴父项。少补这一空行会让该子块及其
                // 之后所有块的映射整体前移 1，跨块复制会丢字、删除会切错。
                total_len += if kind.is_list_item()
                    && super::DocumentTree::list_child_requires_leading_blank_line(child.read(cx))
                {
                    2 + blank_line_prefix.len()
                } else {
                    1
                };
            }
            total_len += self.collect_single_block_source_mappings(
                &child,
                child_list_depth,
                child_quote_depth,
                absolute_start + total_len,
                mappings,
                block_ranges,
                cx,
            );
        }

        block_ranges.insert(
            block.entity_id(),
            absolute_start..absolute_start + total_len,
        );
        total_len
    }

    /// 构建全文档的源码与渲染块双向映射（按内容指纹缓存）。
    pub(super) fn build_source_target_mappings(&self, cx: &App) -> Vec<SourceMapEntry> {
        self.derived_document(cx).mappings.as_ref().clone()
    }

    /// 与 [`Self::build_source_target_mappings`] 相同，额外返回每块按实体标识索引的源码区间。
    /// 原子块（如表格）没有逐字符文本映射，只能通过该区间恢复其完整源码范围，供选区与删除使用。
    pub(super) fn build_source_target_mappings_with_block_ranges(
        &self,
        cx: &App,
    ) -> (Vec<SourceMapEntry>, HashMap<EntityId, Range<usize>>) {
        let derived = self.derived_document(cx);
        (
            derived.mappings.as_ref().clone(),
            derived.block_ranges.as_ref().clone(),
        )
    }

    /// 全量重算源码映射与块区间，只由派生缓存调用。
    pub(super) fn compute_source_target_mappings_with_block_ranges(
        &self,
        cx: &App,
    ) -> (Vec<SourceMapEntry>, HashMap<EntityId, Range<usize>>) {
        let mut mappings = Vec::new();
        let mut block_ranges = HashMap::new();
        let mut absolute = 0usize;
        let mut pending_empty_roots = 0usize;
        let mut wrote_non_empty_root = false;
        let mut previous_was_list_item = false;

        for block in self.document.root_blocks() {
            let (is_empty_root, current_is_list_item) = {
                let block_ref = block.read(cx);
                (
                    Self::is_empty_root_paragraph(block_ref),
                    block_ref.kind().is_list_item(),
                )
            };
            if is_empty_root {
                // Empty roots carry no text mapping, but they still need a source
                // span so a cross-block selection whose boundary lands on one can
                // be resolved (otherwise deletion of the selection aborts). A
                // zero-width anchor at the current cursor is the right position:
                // 0 for a leading empty root, source end for a trailing one.
                block_ranges.insert(block.entity_id(), absolute..absolute);
                pending_empty_roots += 1;
                continue;
            }

            if wrote_non_empty_root {
                // 与 `DocumentTree::collect_root_markdown_lines`（tree.rs）及
                // `cross_block_selected_markdown`（selection.rs）的分隔规则严格一致：
                // 相邻列表项之间只补空段落数本身，其余情况补“空段落数 + 1”。
                // 这里曾对相邻列表项多算 1，导致映射比真实源码长，全选复制/删除越界 panic。
                let separator_count = if previous_was_list_item && current_is_list_item {
                    pending_empty_roots
                } else {
                    pending_empty_roots + 1
                };
                absolute += separator_count;
            } else if pending_empty_roots > 0 {
                absolute += pending_empty_roots;
            }

            absolute += self.collect_single_block_source_mappings(
                block,
                0,
                0,
                absolute,
                &mut mappings,
                &mut block_ranges,
                cx,
            );

            wrote_non_empty_root = true;
            pending_empty_roots = 0;
            previous_was_list_item = current_is_list_item;
            absolute += 1;
        }

        (mappings, block_ranges)
    }
}

#[cfg(test)]
mod tests {
    use rgpui::{AppContext, TestAppContext};

    use super::Editor;

    /// 初始化测试应用（测试辅助）。
    fn init_source_map_test_app(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::i18n::I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
    }

    #[test]
    fn prefixed_content_mapping_tracks_offsets_across_prefixes() {
        let (full, content_to_source, source_to_content) =
            Editor::build_prefixed_content_mapping("ab\ncd", "> ", "> ");

        assert_eq!(full, "> ab\n> cd");
        // 每个内容偏移都指向源码中对应的字符。
        assert_eq!(content_to_source.len(), 6);
        assert_eq!(&full[content_to_source[0]..content_to_source[0] + 1], "a");
        assert_eq!(&full[content_to_source[1]..content_to_source[1] + 1], "b");
        assert_eq!(&full[content_to_source[2]..content_to_source[2] + 1], "\n");
        assert_eq!(&full[content_to_source[3]..content_to_source[3] + 1], "c");
        assert_eq!(&full[content_to_source[4]..content_to_source[4] + 1], "d");
        // 末尾偏移落在全文长度上。
        assert_eq!(content_to_source[5], full.len());

        // 反向映射：源码中的每个偏移都回指到内容偏移。
        assert_eq!(source_to_content.len(), full.len() + 1);
        assert_eq!(source_to_content[0], 0);
        assert_eq!(source_to_content[full.len()], 5);
        for (source_offset, &content_offset) in source_to_content[..full.len()].iter().enumerate() {
            assert!(
                content_offset <= 5,
                "source offset {source_offset} mapped out of range: {content_offset}"
            );
        }
    }

    #[test]
    fn code_block_content_mapping_wraps_content_in_fence() {
        let (full, content_to_source, source_to_content) =
            Editor::build_code_block_content_mapping("a\nb", "", None);

        assert_eq!(full, "```\na\nb\n```");
        assert_eq!(content_to_source.len(), 4);
        assert_eq!(&full[content_to_source[0]..content_to_source[0] + 1], "a");
        assert_eq!(&full[content_to_source[1]..content_to_source[1] + 1], "\n");
        assert_eq!(&full[content_to_source[2]..content_to_source[2] + 1], "b");
        assert_eq!(content_to_source[3], full.len() - 4);
        assert_eq!(source_to_content.len(), full.len() + 1);
        assert_eq!(source_to_content[full.len()], 3);
    }

    #[test]
    fn code_block_content_mapping_keeps_indentation_on_every_line() {
        let (full, _, _) = Editor::build_code_block_content_mapping("x\ny", "  ", None);

        assert_eq!(full, "  ```\n  x\n  y\n  ```");
    }

    #[test]
    fn quote_wrapping_prefixes_each_line() {
        let (full, content_to_source, source_to_content) = Editor::wrap_source_mapping_with_quotes(
            "a\nb".to_string(),
            vec![0, 1, 2],
            vec![0, 1, 2],
            1,
        );

        assert_eq!(full, "> a\n> b");
        assert_eq!(content_to_source.len(), 3);
        assert_eq!(&full[content_to_source[0]..content_to_source[0] + 1], "a");
        assert_eq!(&full[content_to_source[2]..content_to_source[2] + 1], "b");
        assert_eq!(source_to_content.len(), full.len() + 1);
    }

    #[test]
    fn quote_wrapping_applies_prefix_once_per_depth() {
        let (full, _, _) =
            Editor::wrap_source_mapping_with_quotes("a".to_string(), vec![0, 1], vec![0, 1], 2);

        assert_eq!(full, "> > a");
    }

    #[rgpui::test]
    async fn source_mappings_cover_document_source(cx: &mut TestAppContext) {
        init_source_map_test_app(cx);
        let markdown = "# 标题\n\n段落一\n\n- 列表项\n\n```rust\nfn main() {}\n```\n".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        cx.update(|cx| {
            let editor = editor.read(cx);
            let mappings = editor.build_source_target_mappings(cx);
            assert!(!mappings.is_empty(), "mappings should not be empty");

            // 每条映射的源码区间都落在文档文本内。
            for mapping in &mappings {
                assert!(
                    mapping.full_source_range.end <= markdown.len(),
                    "mapping range {:?} exceeds source length {}",
                    mapping.full_source_range,
                    markdown.len()
                );
            }

            // 各映射区间互不重叠且按顺序排列。
            for pair in mappings.windows(2) {
                assert!(
                    pair[0].full_source_range.end <= pair[1].full_source_range.start,
                    "mappings overlap: {:?} vs {:?}",
                    pair[0].full_source_range,
                    pair[1].full_source_range
                );
            }
        });
    }

    #[rgpui::test]
    async fn source_mapping_of_heading_points_at_heading_text(cx: &mut TestAppContext) {
        init_source_map_test_app(cx);
        let markdown = "# 标题\n\n正文\n".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        cx.update(|cx| {
            let editor = editor.read(cx);
            let mappings = editor.build_source_target_mappings(cx);
            let heading = mappings.first().expect("heading should have a mapping");
            let text = &markdown[heading.full_source_range.clone()];
            assert!(text.starts_with("# 标题"), "heading source: {text:?}");
        });
    }
}
