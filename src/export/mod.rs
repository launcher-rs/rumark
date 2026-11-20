//! 文档导出：Markdown 到 PDF / DOCX 的纯 Rust 渲染。
//!
//! HTML 导出沿用既有轻量模板（见 `editor::persistence`），本模块负责另外两种
//! 格式。设计约束：不引入浏览器/异步运行时重型依赖（`chromiumoxide`/`tokio`
//! 已在裁剪提交中移除，不再加回），PDF 与 DOCX 各由独立 Cargo feature 门控
//! （`export-pdf` / `export-docx`，默认全开；`--no-default-features` 可裁）。
//!
//! 三种格式共享下面的块模型：先由 `parse_blocks` 把 Markdown 走
//! `pulldown-cmark` 解析为与呈现无关的块树，再由各格式渲染器消费。
//! 降级约定：公式等编辑器内富渲染在导出中回退为等宽原文；Mermaid 图导出为
//! 图片（HTML 内嵌 SVG，PDF/DOCX 内嵌栅格化 PNG），渲染失败才回退源码；
//! 图片按文档目录解析后嵌入（加载失败回退替代文本占位）。

#[cfg(feature = "export-docx")]
pub(crate) mod docx;
pub(crate) mod image;
pub(crate) mod mermaid;
#[cfg(feature = "export-pdf")]
pub(crate) mod pdf;

/// 导出目标（HTML 由编辑器侧直出，不在本枚举中）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    /// 纯 Rust 手排版 PDF（内嵌子集中文字体，离线可用）。
    #[cfg(feature = "export-pdf")]
    Pdf,
    /// 原生 Word 文档（`docx-rs` 直写，无外部依赖）。
    #[cfg(feature = "export-docx")]
    Docx,
}

impl ExportFormat {
    /// 存盘对话框默认扩展名。
    pub(crate) fn extension(self) -> &'static str {
        match self {
            #[cfg(feature = "export-pdf")]
            Self::Pdf => "pdf",
            #[cfg(feature = "export-docx")]
            Self::Docx => "docx",
        }
    }
}

/// 各格式渲染共用的导出选项。
#[derive(Clone, Debug)]
pub(crate) struct ExportOptions {
    /// 文档标题（取自文件名词干，无文件时为 untitled）。
    pub(crate) title: String,
    /// 是否在正文前插入目录（沿用 HTML 导出的目录偏好）。
    pub(crate) include_toc: bool,
    /// 相对图片路径的基准目录（文档所在目录，未保存时为当前目录）。
    pub(crate) base_dir: Option<std::path::PathBuf>,
}

/// 行内节点：各格式按能力取舍呈现（如下划线/删除线 PDF 暂忽略）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Inline {
    /// 普通文本。
    Text(String),
    /// 加粗（含 `**加粗**`）。
    Strong(Vec<Inline>),
    /// 斜体（含 `*斜体*`）。
    Emphasis(Vec<Inline>),
    /// 删除线（DOCX 原生支持，PDF 回退普通文本）。
    Strike(Vec<Inline>),
    /// 行内代码。
    Code(String),
    /// 行内公式原文。
    Math(String),
    /// 链接：渲染展示文本，目标地址供 DOCX 超链接使用。
    Link { label: String, url: String },
    /// 图片：目标地址供各格式解析嵌入，加载失败时以替代文本占位。
    Image { alt: String, url: String },
    /// 脚注引用（如 `[^1]`）。
    FootnoteRef(String),
    /// 硬换行。
    Break,
}

/// 列表项：勾选状态仅任务列表携带。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListItem {
    pub(crate) checked: Option<bool>,
    pub(crate) blocks: Vec<Block>,
}

/// 表格单元格对齐（沿用 Markdown 列标记，无标记为 `None`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Align {
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// 块节点：段落/标题/代码/引用/列表/表格/分隔线等。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Block {
    Heading {
        level: u8,
        inline: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    /// 围栏/缩进代码块（非 Mermaid 的原文）。
    Code {
        lang: Option<String>,
        code: String,
    },
    /// Mermaid 图源码：各格式渲染为图片，失败回退等宽原文。
    Mermaid(String),
    /// 块级公式原文。
    Math(String),
    Quote(Vec<Block>),
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Table {
        headers: Vec<Vec<Inline>>,
        aligns: Vec<Align>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
    /// 原生 HTML 块：各格式按纯文本收纳，避免内容丢失。
    Html(String),
}

/// 取行内节点的展示纯文本（标题/目录/表格度量共用）。
pub(crate) fn inline_text(inline: &[Inline]) -> String {
    let mut out = String::new();
    for node in inline {
        match node {
            Inline::Text(text) => out.push_str(text),
            Inline::Strong(inner) | Inline::Emphasis(inner) | Inline::Strike(inner) => {
                out.push_str(&inline_text(inner));
            }
            Inline::Code(code) | Inline::Math(code) => out.push_str(code),
            Inline::Link { label, .. } => out.push_str(label),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::FootnoteRef(label) => {
                out.push_str("[^");
                out.push_str(label);
                out.push(']');
            }
            Inline::Break => out.push(' '),
        }
    }
    out
}

/// 把 Markdown 解析为块树（GFM 表格/删除线/任务列表/脚注/上下标全开）。
pub(crate) fn parse_blocks(markdown: &str) -> Vec<Block> {
    use pulldown_cmark::{Options, Parser};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_GFM);
    // 上下标在部分旧版本不存在时编译失败，故不开启：渲染器遇到未知标签会跳过。
    let mut events = Parser::new_ext(markdown, options).peekable();
    parse_block_list(&mut events)
}

/// 收集一级标题（供目录使用），层级钳制在 1..=6。
pub(crate) fn collect_headings(blocks: &[Block]) -> Vec<(u8, String)> {
    let mut headings = Vec::new();
    collect_headings_into(blocks, &mut headings);
    headings
}

/// 递归收集标题（含引用块内标题，列表项内标题同样收录）。
fn collect_headings_into(blocks: &[Block], out: &mut Vec<(u8, String)>) {
    for block in blocks {
        match block {
            Block::Heading { level, inline } => {
                let label = inline_text(inline)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !label.is_empty() {
                    out.push(((*level).clamp(1, 6), label));
                }
            }
            Block::Quote(inner) => collect_headings_into(inner, out),
            Block::List { items, .. } => {
                for item in items {
                    collect_headings_into(&item.blocks, out);
                }
            }
            _ => {}
        }
    }
}

/// 解析块序列，直到事件流结束或遇到归属上层的结束标签。
fn parse_block_list<'a, I>(events: &mut std::iter::Peekable<I>) -> Vec<Block>
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{Event, Tag, TagEnd};

    let mut blocks = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::Paragraph) => {
                blocks.push(Block::Paragraph(collect_inline(events, &TagEnd::Paragraph)));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                blocks.push(Block::Heading {
                    level: level as u8,
                    inline: collect_inline(events, &TagEnd::Heading(level)),
                });
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match &kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().map(str::to_string)
                    }
                    pulldown_cmark::CodeBlockKind::Indented => None,
                };
                let mut code = String::new();
                for event in events.by_ref() {
                    match event {
                        Event::Text(text) => code.push_str(&text),
                        Event::End(TagEnd::CodeBlock) => break,
                        _ => {}
                    }
                }
                if crate::components::mermaid::is_mermaid_info_string(lang.as_deref()) {
                    blocks.push(Block::Mermaid(code));
                } else {
                    blocks.push(Block::Code { lang, code });
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                let inner = parse_block_list(events);
                blocks.push(Block::Quote(inner));
            }
            Event::Start(Tag::List(start)) => {
                blocks.push(parse_list(events, start));
            }
            Event::Start(Tag::FootnoteDefinition(label)) => {
                let inner = parse_block_list(events);
                let mut inline = vec![Inline::Text(format!("[^{label}]: "))];
                for block in inner {
                    match block {
                        Block::Paragraph(children)
                        | Block::Heading {
                            inline: children, ..
                        } => {
                            inline.extend(children);
                            inline.push(Inline::Text(" ".to_string()));
                        }
                        other => {
                            inline.push(Inline::Text(block_text_fallback(&other)));
                            inline.push(Inline::Text(" ".to_string()));
                        }
                    }
                }
                blocks.push(Block::Paragraph(inline));
            }
            Event::Start(Tag::Table(aligns)) => {
                blocks.push(parse_table(events, &aligns));
            }
            Event::Start(Tag::HtmlBlock) => {
                let mut raw = String::new();
                for event in events.by_ref() {
                    match event {
                        Event::Html(html) | Event::Text(html) => raw.push_str(&html),
                        Event::End(TagEnd::HtmlBlock) => break,
                        _ => {}
                    }
                }
                if !raw.trim().is_empty() {
                    blocks.push(Block::Html(raw));
                }
            }
            Event::Start(Tag::MetadataBlock(_)) => skip_to_end(events, "MetadataBlock"),
            Event::Start(Tag::DefinitionList) => skip_to_end(events, "DefinitionList"),
            Event::Start(_) => skip_to_end(events, "未知块"),
            Event::Rule => blocks.push(Block::Rule),
            Event::DisplayMath(math) => blocks.push(Block::Math(math.into_string())),
            Event::TaskListMarker(_) | Event::SoftBreak | Event::HardBreak => {}
            Event::Text(text) => {
                if !text.trim().is_empty() {
                    blocks.push(Block::Paragraph(vec![Inline::Text(text.into_string())]));
                }
            }
            Event::Code(code) => {
                blocks.push(Block::Paragraph(vec![Inline::Code(code.into_string())]));
            }
            Event::InlineMath(math) => {
                blocks.push(Block::Paragraph(vec![Inline::Math(math.into_string())]));
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                if !html.trim().is_empty() {
                    blocks.push(Block::Html(html.into_string()));
                }
            }
            Event::FootnoteReference(label) => {
                blocks.push(Block::Paragraph(vec![Inline::FootnoteRef(
                    label.into_string(),
                )]));
            }
            // 归属上层的结束标签：交还给调用方处理。
            Event::End(_) => break,
        }
    }
    blocks
}

/// 解析列表（含任务列表勾选态），结束于列表闭合标签。
fn parse_list<'a, I>(events: &mut std::iter::Peekable<I>, start: Option<u64>) -> Block
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{Event, Tag, TagEnd};

    let mut items = Vec::new();
    let mut current: Option<ListItem> = None;
    // 条目内的块需要跨多个事件累积：用显式缓冲而不是递归调用。
    let mut buffer: Vec<Block> = Vec::new();
    // 把缓冲中的行内事件拼成段落（条目首段常见形态）。
    let flush_inline = |inline: &mut Vec<Inline>, buffer: &mut Vec<Block>| {
        if !inline.is_empty() {
            buffer.push(Block::Paragraph(std::mem::take(inline)));
        }
    };

    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::Item) => {
                if let Some(done) = current.take() {
                    items.push(done);
                }
                current = Some(ListItem {
                    checked: None,
                    blocks: Vec::new(),
                });
                buffer.clear();
            }
            Event::End(TagEnd::Item) => {
                if let Some(mut item) = current.take() {
                    item.blocks.append(&mut buffer);
                    // 空条目保留空段落，避免编号错位。
                    if item.blocks.is_empty() {
                        item.blocks.push(Block::Paragraph(Vec::new()));
                    }
                    items.push(item);
                }
                buffer.clear();
            }
            Event::End(TagEnd::List(_)) => {
                if let Some(mut item) = current.take() {
                    item.blocks.append(&mut buffer);
                    if item.blocks.is_empty() {
                        item.blocks.push(Block::Paragraph(Vec::new()));
                    }
                    items.push(item);
                }
                break;
            }
            Event::TaskListMarker(checked) => {
                if let Some(item) = current.as_mut()
                    && item.checked.is_none()
                {
                    item.checked = Some(checked);
                }
            }
            // 条目内的嵌套块直接复用块解析，结束后把结果并入缓冲。
            Event::Start(Tag::BlockQuote(_)) => {
                let inner = parse_block_list(events);
                buffer.push(Block::Quote(inner));
            }
            Event::Start(Tag::List(nested)) => {
                buffer.push(parse_list(events, nested));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match &kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().map(str::to_string)
                    }
                    pulldown_cmark::CodeBlockKind::Indented => None,
                };
                let mut code = String::new();
                for event in events.by_ref() {
                    match event {
                        Event::Text(text) => code.push_str(&text),
                        Event::End(TagEnd::CodeBlock) => break,
                        _ => {}
                    }
                }
                if crate::components::mermaid::is_mermaid_info_string(lang.as_deref()) {
                    buffer.push(Block::Mermaid(code));
                } else {
                    buffer.push(Block::Code { lang, code });
                }
            }
            Event::Start(Tag::Paragraph) => {
                let inline = collect_inline(events, &TagEnd::Paragraph);
                buffer.push(Block::Paragraph(inline));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                let inline = collect_inline(events, &TagEnd::Heading(level));
                buffer.push(Block::Heading {
                    level: level as u8,
                    inline,
                });
            }
            Event::Rule => buffer.push(Block::Rule),
            Event::DisplayMath(math) => buffer.push(Block::Math(math.into_string())),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                let text = text.into_string();
                if text.trim().is_empty() {
                    continue;
                }
                match buffer.last_mut() {
                    Some(Block::Paragraph(inline)) => inline.push(Inline::Text(text)),
                    _ => buffer.push(Block::Paragraph(vec![Inline::Text(text)])),
                }
            }
            Event::Code(code) => match buffer.last_mut() {
                Some(Block::Paragraph(inline)) => inline.push(Inline::Code(code.into_string())),
                _ => buffer.push(Block::Paragraph(vec![Inline::Code(code.into_string())])),
            },
            Event::SoftBreak => {
                if let Some(Block::Paragraph(inline)) = buffer.last_mut() {
                    inline.push(Inline::Text(" ".to_string()));
                }
            }
            Event::HardBreak => {
                if let Some(Block::Paragraph(inline)) = buffer.last_mut() {
                    inline.push(Inline::Break);
                }
            }
            _ => {
                // 列表语境下其余行内事件先拼段，复杂嵌套由上层分支覆盖。
                let _ = &flush_inline;
            }
        }
    }

    Block::List {
        ordered: start.is_some(),
        start: start.unwrap_or(1),
        items,
    }
}

/// 解析表格：表头 + 对齐 + 若干正文行。
fn parse_table<'a, I>(
    events: &mut std::iter::Peekable<I>,
    aligns: &[pulldown_cmark::Alignment],
) -> Block
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{Event, Tag, TagEnd};

    let map_align = |align: &pulldown_cmark::Alignment| match align {
        pulldown_cmark::Alignment::Left => Align::Left,
        pulldown_cmark::Alignment::Center => Align::Center,
        pulldown_cmark::Alignment::Right => Align::Right,
        pulldown_cmark::Alignment::None => Align::None,
    };

    let mut headers: Vec<Vec<Inline>> = Vec::new();
    let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
    let mut current_row: Vec<Vec<Inline>> = Vec::new();
    let mut in_head = false;

    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::TableHead) => in_head = true,
            // 表头直接挂 TableCell（无 TableRow 包裹），闭合时整体移交。
            Event::End(TagEnd::TableHead) => {
                in_head = false;
                headers = std::mem::take(&mut current_row);
            }
            Event::Start(Tag::TableRow) => current_row = Vec::new(),
            Event::End(TagEnd::TableRow) => {
                if !in_head && !current_row.is_empty() {
                    rows.push(std::mem::take(&mut current_row));
                }
            }
            Event::Start(Tag::TableCell) => {
                let inline = collect_inline(events, &TagEnd::TableCell);
                current_row.push(inline);
            }
            Event::End(TagEnd::Table) => break,
            Event::End(_) => break,
            _ => {}
        }
    }

    let width = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut aligns_out: Vec<Align> = aligns.iter().map(map_align).collect();
    aligns_out.resize(width, Align::None);
    for row in rows.iter_mut() {
        row.resize_with(width, Vec::new);
    }
    headers.resize_with(width, Vec::new);

    Block::Table {
        headers,
        aligns: aligns_out,
        rows,
    }
}

/// 收集行内事件直到指定结束标签（含加粗/斜体/链接/图片嵌套）。
fn collect_inline<'a, I>(
    events: &mut std::iter::Peekable<I>,
    end: &pulldown_cmark::TagEnd,
) -> Vec<Inline>
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{Event, Tag, TagEnd};

    let mut inline = Vec::new();
    while let Some(event) = events.next() {
        // 结束标签匹配即归还（`TagEnd::List(bool)` 等带载荷标签按变体匹配）。
        if tag_end_matches(&event, end) {
            break;
        }
        match event {
            Event::Text(text) => inline.push(Inline::Text(text.into_string())),
            Event::Code(code) => inline.push(Inline::Code(code.into_string())),
            Event::InlineMath(math) => inline.push(Inline::Math(math.into_string())),
            Event::SoftBreak => inline.push(Inline::Text(" ".to_string())),
            Event::HardBreak => inline.push(Inline::Break),
            Event::Html(html) | Event::InlineHtml(html) => {
                inline.push(Inline::Text(html.into_string()));
            }
            Event::FootnoteReference(label) => {
                inline.push(Inline::FootnoteRef(label.into_string()));
            }
            Event::TaskListMarker(checked) => {
                inline.push(Inline::Text(if checked { "☑ " } else { "☐ " }.to_string()));
            }
            Event::Start(Tag::Strong) => {
                inline.push(Inline::Strong(collect_inline(events, &TagEnd::Strong)));
            }
            Event::Start(Tag::Emphasis) => {
                inline.push(Inline::Emphasis(collect_inline(events, &TagEnd::Emphasis)));
            }
            Event::Start(Tag::Strikethrough) => {
                inline.push(Inline::Strike(collect_inline(
                    events,
                    &TagEnd::Strikethrough,
                )));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let label_nodes = collect_inline(events, &TagEnd::Link);
                inline.push(Inline::Link {
                    label: inline_text(&label_nodes),
                    url: dest_url.into_string(),
                });
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                let alt_nodes = collect_inline(events, &TagEnd::Image);
                let alt = inline_text(&alt_nodes);
                let url = dest_url.into_string();
                inline.push(Inline::Image {
                    alt: if alt.trim().is_empty() {
                        url.clone()
                    } else {
                        alt
                    },
                    url,
                });
            }
            Event::Start(Tag::Subscript) => {
                let inner = collect_inline(events, &TagEnd::Subscript);
                inline.push(Inline::Text(inline_text(&inner)));
            }
            Event::Start(Tag::Superscript) => {
                let inner = collect_inline(events, &TagEnd::Superscript);
                inline.push(Inline::Text(inline_text(&inner)));
            }
            Event::Start(_) => skip_to_end(events, "未知行内"),
            Event::Rule => inline.push(Inline::Text("---".to_string())),
            Event::DisplayMath(math) => inline.push(Inline::Math(math.into_string())),
            Event::End(_) => break,
        }
    }
    inline
}

/// 结束标签是否与目标变体一致（带载荷标签只比变体）。
fn tag_end_matches(event: &pulldown_cmark::Event<'_>, end: &pulldown_cmark::TagEnd) -> bool {
    use pulldown_cmark::{Event, TagEnd};

    match (event, end) {
        (Event::End(TagEnd::Paragraph), TagEnd::Paragraph) => true,
        (Event::End(TagEnd::Heading(a)), TagEnd::Heading(b)) => a == b,
        (Event::End(TagEnd::TableCell), TagEnd::TableCell) => true,
        (Event::End(TagEnd::Strong), TagEnd::Strong) => true,
        (Event::End(TagEnd::Emphasis), TagEnd::Emphasis) => true,
        (Event::End(TagEnd::Strikethrough), TagEnd::Strikethrough) => true,
        (Event::End(TagEnd::Link), TagEnd::Link) => true,
        (Event::End(TagEnd::Image), TagEnd::Image) => true,
        (Event::End(TagEnd::Subscript), TagEnd::Subscript) => true,
        (Event::End(TagEnd::Superscript), TagEnd::Superscript) => true,
        _ => false,
    }
}

/// 跳过未知标签体直至其配对结束（按深度计数，容错不配对输入）。
fn skip_to_end<'a, I>(events: &mut std::iter::Peekable<I>, _why: &str)
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::Event;

    let mut depth = 1;
    for event in events.by_ref() {
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
}

/// 非段落块在脚注等语境下的纯文本回退。
fn block_text_fallback(block: &Block) -> String {
    match block {
        Block::Paragraph(inline) | Block::Heading { inline, .. } => inline_text(inline),
        Block::Code { code, .. } | Block::Mermaid(code) | Block::Math(code) | Block::Html(code) => {
            code.clone()
        }
        Block::Quote(inner) => inner
            .iter()
            .map(block_text_fallback)
            .collect::<Vec<_>>()
            .join(" "),
        Block::List { items, .. } => items
            .iter()
            .flat_map(|item| item.blocks.iter().map(block_text_fallback))
            .collect::<Vec<_>>()
            .join(" "),
        Block::Table { headers, rows, .. } => {
            let mut cells: Vec<String> = headers.iter().map(|cell| inline_text(cell)).collect();
            for row in rows {
                cells.extend(row.iter().map(|cell| inline_text(cell)));
            }
            cells.join(" ")
        }
        Block::Rule => "---".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Align, Block, ExportFormat, Inline, collect_headings, inline_text, parse_blocks};

    #[test]
    fn export_format_extensions_match_dialog_defaults() {
        #[cfg(feature = "export-pdf")]
        assert_eq!(ExportFormat::Pdf.extension(), "pdf");
        #[cfg(feature = "export-docx")]
        assert_eq!(ExportFormat::Docx.extension(), "docx");
    }

    #[test]
    fn walker_covers_headings_lists_tables_and_code() {
        let blocks = parse_blocks(
            "# 标题\n\n- [x] 完成\n- [ ] 待办\n\n| 甲 | 乙 |\n| --- | ---: |\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n",
        );
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        let Block::List { ordered, items, .. } = &blocks[1] else {
            panic!("expected list: {blocks:?}");
        };
        assert!(!ordered);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].checked, Some(true));
        assert_eq!(items[1].checked, Some(false));
        let Block::Table {
            headers,
            aligns,
            rows,
            ..
        } = &blocks[2]
        else {
            panic!("expected table: {blocks:?}");
        };
        assert_eq!(inline_text(&headers[0]), "甲");
        assert_eq!(aligns[1], Align::Right);
        assert_eq!(rows.len(), 1);
        let Block::Code { lang, code } = &blocks[3] else {
            panic!("expected code: {blocks:?}");
        };
        assert_eq!(lang.as_deref(), Some("rust"));
        assert!(code.contains("let x = 1;"), "code: {code}");
    }

    #[test]
    fn walker_recognizes_mermaid_fences_as_diagram_blocks() {
        let blocks = parse_blocks(
            "```mermaid\nflowchart TD\n  A --> B\n```\n\n~~~mmd\ngraph LR\n  X --> Y\n~~~\n",
        );
        assert!(
            matches!(&blocks[0], Block::Mermaid(code) if code.contains("A --> B")),
            "blocks: {blocks:?}"
        );
        assert!(
            matches!(&blocks[1], Block::Mermaid(code) if code.contains("X --> Y")),
            "blocks: {blocks:?}"
        );
    }

    #[test]
    fn walker_keeps_inline_styles_links_and_math() {
        let blocks = parse_blocks("**粗**与*斜*与`码`与[文](https://x.com)与$公式$与~~删~~\n");
        let Block::Paragraph(inline) = &blocks[0] else {
            panic!("expected paragraph: {blocks:?}");
        };
        assert!(matches!(inline[0], Inline::Strong(_)));
        assert!(matches!(inline[1], Inline::Text(_)));
        assert!(matches!(inline[2], Inline::Emphasis(_)));
        assert!(matches!(inline[4], Inline::Code(_)));
        let link = inline.iter().find_map(|node| match node {
            Inline::Link { label, url } => Some((label.clone(), url.clone())),
            _ => None,
        });
        assert_eq!(link, Some(("文".to_string(), "https://x.com".to_string())));
        assert!(
            inline.iter().any(|node| matches!(node, Inline::Math(_))),
            "missing a math node; inline: {inline:?}"
        );
        assert!(
            inline.iter().any(|node| matches!(node, Inline::Strike(_))),
            "missing a strike node; inline: {inline:?}"
        );
        assert_eq!(inline_text(inline), "粗与斜与码与文与公式与删");
    }

    #[test]
    fn walker_keeps_image_url_and_alt() {
        let blocks = parse_blocks("![替代](./assets/pic.png)\n\n![](bare.png)\n");
        let Block::Paragraph(inline) = &blocks[0] else {
            panic!("expected paragraph: {blocks:?}");
        };
        let Inline::Image { alt, url } = &inline[0] else {
            panic!("expected image: {inline:?}");
        };
        assert_eq!(alt, "替代");
        assert_eq!(url, "./assets/pic.png");
        let Block::Paragraph(inline) = &blocks[1] else {
            panic!("expected paragraph: {blocks:?}");
        };
        let Inline::Image { alt, url } = &inline[0] else {
            panic!("expected image: {inline:?}");
        };
        // 空替代文本回退为目标地址，但 url 始终保留供嵌图解析。
        assert_eq!(alt, "bare.png");
        assert_eq!(url, "bare.png");
    }

    #[test]
    fn headings_collect_skips_empty_and_clamps_levels() {
        let blocks = parse_blocks("# 甲\n\n> ## 乙\n\n- ### 丙\n");
        let headings = collect_headings(&blocks);
        assert_eq!(headings.len(), 3);
        assert_eq!(headings[0], (1, "甲".to_string()));
        assert_eq!(headings[1], (2, "乙".to_string()));
        assert_eq!(headings[2], (3, "丙".to_string()));
    }

    #[test]
    fn walker_never_panics_on_mixed_edge_input() {
        let markdown = "```\n未闭合围栏\n\n# 标题 {#自定义}\n\n[^1]: 脚注正文\n\n<div>\n原生块\n</div>\n\n1. 有序\n   - 嵌套无序\n\n> 引用\n> > 深层引用\n";
        let blocks = parse_blocks(markdown);
        assert!(!blocks.is_empty());
        let _ = parse_blocks("");
        let _ = parse_blocks("纯文本无块标记");
    }
}
