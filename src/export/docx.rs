//! Markdown 到 DOCX 渲染（`export-docx` feature 门控）。
//!
//! 经共享块模型直写原生 Word 文档：标题样式、真列表编号、网格表格、
//! 目录域与中英文字体分区（西文 Calibri/Consolas、中文微软雅黑），
//! 全程纯 Rust、无外部进程依赖。公式回退等宽原文，Mermaid 图渲染为图片嵌入，
//! 段落内图片解码转 PNG 嵌入（加载失败回退替代文本占位）。

use std::path::PathBuf;

use docx_rs::{
    AbstractNumbering, AlignmentType, Docx, Hyperlink, HyperlinkType, IndentLevel, Level, LevelJc,
    LevelText, NumberFormat, Numbering, NumberingId, Paragraph, Pic, Run, RunFonts, Start, Table,
    TableAlignmentType, TableCell, TableOfContents, TableRow,
};

use super::{Align, Block, ExportOptions, Inline, collect_headings, inline_text, parse_blocks};

/// 无序列表的抽象编号 ID（实现内保留，与有序列表区分）。
const BULLET_ABSTRACT_ID: usize = 101;
/// 有序列表的抽象编号 ID。
const DECIMAL_ABSTRACT_ID: usize = 102;
/// 正文行高对应的磅值（docx-rs 字号以半磅计）。
const BODY_HALF_POINTS: usize = 21;
/// 代码与表格的较小字号（半磅）。
const SMALL_HALF_POINTS: usize = 18;
/// 版心宽度（EMU）：A4 宽 11906 twips 减左右边距各 1701，1 twip = 635 EMU。
const CONTENT_WIDTH_EMU: u32 = (11906 - 1701 * 2) * 635;
/// 图片高度上限（EMU）：约版心高六成，避免单图撑满整页。
const MAX_IMAGE_HEIGHT_EMU: u32 = 5_000_000;
/// 1 像素对应的 EMU（96dpi）。
const EMU_PER_PX: f64 = 9525.0;

/// 把 Markdown 渲染为完整 DOCX 字节（ZIP 容器，Word 可直接打开）。
pub(crate) fn render_docx_bytes(
    markdown: &str,
    options: &ExportOptions,
) -> anyhow::Result<Vec<u8>> {
    let blocks = parse_blocks(markdown);
    let headings = collect_headings(&blocks);
    // 有序列表不再共用一个编号实例：每个列表在渲染时注册独立的抽象编号
    // （起始号取列表声明的 `start`），否则 Word 会把多个列表接续编号。
    let mut doc = Docx::new()
        .add_abstract_numbering(bullet_abstract_numbering())
        .add_numbering(Numbering::new(BULLET_ABSTRACT_ID, BULLET_ABSTRACT_ID));
    if options.include_toc && !headings.is_empty() {
        doc = doc
            .add_paragraph(Paragraph::new().style("Heading1").add_run(body_run("目录")))
            .add_table_of_contents(
                TableOfContents::new()
                    .heading_styles_range(1, 3)
                    .alias(options.title.as_str())
                    .auto(),
            );
    }
    let mut writer = DocxWriter {
        depth: 0,
        base_dir: options.base_dir.clone(),
        next_ordered_list_index: 0,
        ordered_numbering_stack: Vec::new(),
    };
    for block in &blocks {
        doc = writer.push_block(doc, block);
    }
    let mut bytes = Vec::new();
    doc.build()
        .pack(std::io::Cursor::new(&mut bytes))
        .map_err(|err| anyhow::anyhow!("DOCX 打包失败：{err}"))?;
    Ok(bytes)
}

/// 无序列表编号定义（9 级，符号按层级轮换）。
fn bullet_abstract_numbering() -> AbstractNumbering {
    let symbols = ["•", "◦", "▪"];
    let mut numbering = AbstractNumbering::new(BULLET_ABSTRACT_ID);
    for level in 0..9 {
        numbering = numbering.add_level(
            Level::new(
                level,
                Start::new(1),
                NumberFormat::new("bullet"),
                LevelText::new(symbols[level % symbols.len()]),
                LevelJc::new("left"),
            )
            .indent(Some(720 + level as i32 * 360), None, None, None),
        );
    }
    numbering
}

/// 有序列表编号定义（9 级，十进制 `1.` 样式）。
///
/// 每个有序列表独立一组：`abstract_id` 唯一，顶层起始号取列表声明的
/// `start`，子层级仍从 1 起排。
fn decimal_abstract_numbering(abstract_id: usize, start: u64) -> AbstractNumbering {
    let mut numbering = AbstractNumbering::new(abstract_id);
    for level in 0..9 {
        numbering = numbering.add_level(
            Level::new(
                level,
                Start::new(if level == 0 { start as usize } else { 1 }),
                NumberFormat::new("decimal"),
                LevelText::new("%1."),
                LevelJc::new("left"),
            )
            .indent(Some(720 + level as i32 * 360), None, None, None),
        );
    }
    numbering
}

/// 正文西文/中文分区字体（中文微软雅黑，缺失时 Word 自动回退）。
fn body_fonts() -> RunFonts {
    RunFonts::new()
        .ascii("Calibri")
        .hi_ansi("Calibri")
        .east_asia("Microsoft YaHei")
        .cs("Calibri")
}

/// 等宽字体（代码与公式，西文 Consolas + 中文雅黑）。
fn mono_fonts() -> RunFonts {
    RunFonts::new()
        .ascii("Consolas")
        .hi_ansi("Consolas")
        .east_asia("Microsoft YaHei")
        .cs("Consolas")
}

/// 正文游程（分区字体 + 正文大小）。
fn body_run(text: &str) -> Run {
    Run::new()
        .add_text(text)
        .fonts(body_fonts())
        .size(BODY_HALF_POINTS)
}

/// 等宽游程（代码/公式原文）。
fn mono_run(text: &str) -> Run {
    Run::new()
        .add_text(text)
        .fonts(mono_fonts())
        .size(SMALL_HALF_POINTS)
}

/// 块装配器（`depth` 为列表嵌套深度，决定编号层级）。
struct DocxWriter {
    depth: u32,
    base_dir: Option<PathBuf>,
    /// 下一个有序列表的编号定义下标（每个列表一组独立的抽象/具体编号）。
    next_ordered_list_index: usize,
    /// 当前渲染中的有序列表编号 id 栈：嵌套列表进入时压栈、退出时弹栈，
    /// 外层列表的后续条目继续引用自己的编号定义。
    ordered_numbering_stack: Vec<usize>,
}

impl DocxWriter {
    /// 加载图片并转为按版心缩放的 PNG 内嵌对象（失败返回 `None` 走占位）。
    fn image_pic(&self, url: &str) -> Option<Pic> {
        let bytes = super::image::load_image_bytes(url, self.base_dir.as_deref())?;
        self.image_pic_bytes(&bytes)
    }

    /// 把图片字节转为按版心缩放的 PNG 内嵌对象（文档图片与 Mermaid 共用）。
    fn image_pic_bytes(&self, bytes: &[u8]) -> Option<Pic> {
        let decoded = super::image::decode_image(bytes)?;
        let (width, height) = (decoded.width(), decoded.height());
        let mut png = Vec::new();
        decoded
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .ok()?;
        // 小图不放大，大图等比钳到版心宽与页高上限。
        let scale = (CONTENT_WIDTH_EMU as f64 / (width as f64 * EMU_PER_PX))
            .min(MAX_IMAGE_HEIGHT_EMU as f64 / (height as f64 * EMU_PER_PX))
            .min(1.0);
        let width_emu = ((width as f64 * EMU_PER_PX * scale) as u32).max(1);
        let height_emu = ((height as f64 * EMU_PER_PX * scale) as u32).max(1);
        Some(Pic::new_with_dimensions(png, width, height).size(width_emu, height_emu))
    }
    /// 单个块装配（消费并返回文档，适配消费式 builder）。
    fn push_block(&mut self, doc: Docx, block: &Block) -> Docx {
        match block {
            Block::Heading { level, inline } => {
                let text = inline_text(inline);
                if text.trim().is_empty() {
                    return doc;
                }
                let style_id = format!("Heading{}", (*level).clamp(1, 6));
                let mut paragraph = Paragraph::new().style(style_id.as_str());
                for run in self.inline_runs(inline, false) {
                    paragraph = paragraph.add_run(run);
                }
                doc.add_paragraph(paragraph)
            }
            Block::Paragraph(inline) => {
                if inline.is_empty() {
                    return doc;
                }
                let mut paragraph = Paragraph::new();
                for piece in self.paragraph_pieces(inline) {
                    paragraph = match piece {
                        Piece::Run(run) => paragraph.add_run(run),
                        Piece::Hyperlink(link) => paragraph.add_hyperlink(*link),
                    };
                }
                doc.add_paragraph(paragraph)
            }
            Block::Code { code, .. } => {
                let trimmed = code.trim_end_matches('\n');
                if trimmed.is_empty() {
                    return doc;
                }
                let mut doc = doc;
                for line in trimmed.split('\n') {
                    let line = if line.is_empty() { " " } else { line };
                    doc = doc.add_paragraph(Paragraph::new().add_run(mono_run(line)));
                }
                doc
            }
            Block::Mermaid(source) => {
                let pic = crate::export::mermaid::render_svg(source)
                    .and_then(|svg| crate::export::mermaid::svg_to_png_bytes(&svg, 2.0))
                    .and_then(|png| self.image_pic_bytes(&png));
                if let Some(pic) = pic {
                    return doc.add_paragraph(
                        Paragraph::new()
                            .align(AlignmentType::Center)
                            .add_run(Run::new().add_image(pic)),
                    );
                }
                // 渲染失败回退等宽源码，保证内容不丢失。
                self.push_block(
                    doc,
                    &Block::Code {
                        lang: Some("mermaid".to_string()),
                        code: source.clone(),
                    },
                )
            }
            Block::Math(math) => {
                let text = math.trim();
                if text.is_empty() {
                    return doc;
                }
                doc.add_paragraph(Paragraph::new().add_run(mono_run(text)))
            }
            Block::Quote(inner) => {
                let mut doc = doc;
                self.depth += 1;
                for block in inner {
                    doc = self.push_block(doc, block);
                }
                self.depth -= 1;
                doc
            }
            Block::List {
                ordered,
                start,
                items,
            } => {
                let mut doc = doc;
                if *ordered {
                    // 每个有序列表注册一组独立编号（起始号取列表声明的 start），
                    // 避免多个列表在 Word 中接续编号；嵌套列表压栈/弹栈，
                    // 外层列表的后续条目继续引用自己的编号定义。
                    let abstract_id = DECIMAL_ABSTRACT_ID + 1 + self.next_ordered_list_index;
                    self.next_ordered_list_index += 1;
                    doc = doc
                        .add_abstract_numbering(decimal_abstract_numbering(abstract_id, *start))
                        .add_numbering(Numbering::new(abstract_id, abstract_id));
                    self.ordered_numbering_stack.push(abstract_id);
                }
                for item in items {
                    doc = self.push_list_item(doc, *ordered, item);
                }
                if *ordered {
                    self.ordered_numbering_stack.pop();
                }
                doc
            }
            Block::Table {
                headers,
                aligns,
                rows,
            } => self.push_table(doc, headers, aligns, rows),
            Block::Rule => doc.add_paragraph(
                Paragraph::new().align(AlignmentType::Center).add_run(
                    Run::new()
                        .add_text("＊ ＊ ＊")
                        .fonts(body_fonts())
                        .size(BODY_HALF_POINTS)
                        .color("808080"),
                ),
            ),
            Block::Html(raw) => {
                let text = raw.trim();
                if text.is_empty() {
                    return doc;
                }
                doc.add_paragraph(
                    Paragraph::new().add_run(
                        Run::new()
                            .add_text(text)
                            .fonts(body_fonts())
                            .size(BODY_HALF_POINTS)
                            .color("595959"),
                    ),
                )
            }
        }
    }

    /// 列表项装配（任务项用勾选前缀，普通项挂真编号）。
    fn push_list_item(&mut self, doc: Docx, ordered: bool, item: &super::ListItem) -> Docx {
        let level = self.depth.min(8) as usize;
        let mut doc = doc;
        if let Some(checked) = item.checked {
            let marker = if checked { "[x] " } else { "[ ] " };
            for block in &item.blocks {
                match block {
                    Block::Paragraph(inline) => {
                        let mut paragraph = Paragraph::new()
                            .indent(Some(720 + level as i32 * 360), None, None, None)
                            .add_run(body_run(marker));
                        for piece in self.paragraph_pieces(inline) {
                            paragraph = match piece {
                                Piece::Run(run) => paragraph.add_run(run),
                                Piece::Hyperlink(link) => paragraph.add_hyperlink(*link),
                            };
                        }
                        doc = doc.add_paragraph(paragraph);
                    }
                    other => {
                        doc = self.push_block(doc, other);
                    }
                }
            }
            return doc;
        }
        for block in &item.blocks {
            match block {
                Block::Paragraph(inline) => {
                    let numbering = if ordered {
                        // 有序列表条目引用当前列表自己的编号定义（见 List 分支）。
                        let list_numbering_id = self
                            .ordered_numbering_stack
                            .last()
                            .copied()
                            .expect("有序列表条目渲染时编号定义应已注册");
                        NumberingId::new(list_numbering_id)
                    } else {
                        NumberingId::new(BULLET_ABSTRACT_ID)
                    };
                    let mut paragraph =
                        Paragraph::new().numbering(numbering, IndentLevel::new(level));
                    for piece in self.paragraph_pieces(inline) {
                        paragraph = match piece {
                            Piece::Run(run) => paragraph.add_run(run),
                            Piece::Hyperlink(link) => paragraph.add_hyperlink(*link),
                        };
                    }
                    doc = doc.add_paragraph(paragraph);
                }
                Block::List { .. } => {
                    self.depth += 1;
                    doc = self.push_block(doc, block);
                    self.depth -= 1;
                }
                other => {
                    doc = self.push_block(doc, other);
                }
            }
        }
        doc
    }

    /// 表格装配（首行为表头加粗，列对齐映射到单元格段落）。
    fn push_table(
        &self,
        doc: Docx,
        headers: &[Vec<Inline>],
        aligns: &[Align],
        rows: &[Vec<Vec<Inline>>],
    ) -> Docx {
        let columns = headers.len().max(1).max(aligns.len().max(1));
        let mut table_rows = Vec::new();
        let align_of = |index: usize| match aligns.get(index).copied().unwrap_or(Align::None) {
            Align::Left => AlignmentType::Left,
            Align::Center => AlignmentType::Center,
            Align::Right => AlignmentType::Right,
            Align::None => AlignmentType::Left,
        };
        let cell_paragraph = |inline: &[Inline], header: bool, index: usize| {
            let mut paragraph = Paragraph::new().align(align_of(index));
            for run in self.inline_runs(inline, true) {
                let run = if header { run.bold() } else { run };
                paragraph = paragraph.add_run(run);
            }
            paragraph
        };
        if !headers.iter().all(Vec::is_empty) {
            let cells = (0..columns)
                .map(|index| {
                    TableCell::new().add_paragraph(cell_paragraph(
                        headers.get(index).map(Vec::as_slice).unwrap_or(&[]),
                        true,
                        index,
                    ))
                })
                .collect();
            table_rows.push(TableRow::new(cells));
        }
        for row in rows {
            let cells = (0..columns)
                .map(|index| {
                    TableCell::new().add_paragraph(cell_paragraph(
                        row.get(index).map(Vec::as_slice).unwrap_or(&[]),
                        false,
                        index,
                    ))
                })
                .collect();
            table_rows.push(TableRow::new(cells));
        }
        if table_rows.is_empty() {
            return doc;
        }
        doc.add_table(
            Table::new(table_rows)
                .style("TableGrid")
                .align(TableAlignmentType::Center),
        )
    }

    /// 段落内容转游程/超链接片段。
    fn paragraph_pieces(&self, inline: &[Inline]) -> Vec<Piece> {
        let mut pieces = Vec::new();
        self.collect_pieces(inline, Style::default(), &mut pieces);
        pieces
    }

    /// 行内树转片段（含超链接提升为段落级节点）。
    fn collect_pieces(&self, inline: &[Inline], style: Style, pieces: &mut Vec<Piece>) {
        for node in inline {
            match node {
                Inline::Text(text) => push_text_piece(pieces, text.clone(), style),
                Inline::Strong(inner) => self.collect_pieces(inner, style.bold(), pieces),
                Inline::Emphasis(inner) => self.collect_pieces(inner, style.italic(), pieces),
                Inline::Strike(inner) => self.collect_pieces(inner, style.strike(), pieces),
                Inline::Code(code) => push_text_piece(pieces, code.clone(), style.mono()),
                Inline::Math(math) => push_text_piece(pieces, math.clone(), style.mono()),
                Inline::Link { label, url } => {
                    let link = Hyperlink::new(url, HyperlinkType::External)
                        .add_run(styled_run(label, style.link()));
                    pieces.push(Piece::Hyperlink(Box::new(link)));
                }
                Inline::Image { alt, url } => match self.image_pic(url) {
                    Some(pic) => pieces.push(Piece::Run(Run::new().add_image(pic))),
                    None => push_text_piece(pieces, format!("[图片：{alt}]"), style.gray()),
                },
                Inline::FootnoteRef(label) => {
                    push_text_piece(pieces, format!("[^{label}]"), style);
                }
                Inline::Break => {
                    pieces.push(Piece::Run(
                        Run::new().add_break(docx_rs::BreakType::TextWrapping),
                    ));
                }
            }
        }
    }

    /// 标题/表格语境的行内游程（small 控制小字号）。
    fn inline_runs(&self, inline: &[Inline], small: bool) -> Vec<Run> {
        let mut runs = Vec::new();
        self.collect_runs(inline, Style::default(), small, &mut runs);
        runs
    }

    /// 行内树转游程（超链接在标题/表格中降级为带下划线的普通文本）。
    fn collect_runs(&self, inline: &[Inline], style: Style, small: bool, runs: &mut Vec<Run>) {
        for node in inline {
            match node {
                Inline::Text(text) => push_text_run(runs, text.clone(), style, small),
                Inline::Strong(inner) => self.collect_runs(inner, style.bold(), small, runs),
                Inline::Emphasis(inner) => self.collect_runs(inner, style.italic(), small, runs),
                Inline::Strike(inner) => self.collect_runs(inner, style.strike(), small, runs),
                Inline::Code(code) => push_text_run(runs, code.clone(), style.mono(), small),
                Inline::Math(math) => push_text_run(runs, math.clone(), style.mono(), small),
                Inline::Link { label, .. } => {
                    push_text_run(runs, label.clone(), style.link(), small);
                }
                Inline::Image { alt, .. } => {
                    push_text_run(runs, format!("[图片：{alt}]"), style.gray(), small);
                }
                Inline::FootnoteRef(label) => {
                    push_text_run(runs, format!("[^{label}]"), style, small);
                }
                Inline::Break => runs.push(Run::new().add_break(docx_rs::BreakType::TextWrapping)),
            }
        }
    }
}

/// 段落级片段：普通游程或超链接（后者需挂段落子节点）。
// `Run` 本体约 736 字节，但片段缓冲只在段落装配期短暂存活、
// 数量少，装箱反而增加调用点负担，此处豁免变体大小检查。
#[allow(clippy::large_enum_variant)]
enum Piece {
    Run(Run),
    Hyperlink(Box<Hyperlink>),
}

/// 行内样式累积（嵌套叠加，最终一次性落到游程属性）。
#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    mono: bool,
    link: bool,
    gray: bool,
}

impl Style {
    /// 叠加加粗。
    fn bold(self) -> Self {
        Self { bold: true, ..self }
    }

    /// 叠加斜体。
    fn italic(self) -> Self {
        Self {
            italic: true,
            ..self
        }
    }

    /// 叠加删除线。
    fn strike(self) -> Self {
        Self {
            strike: true,
            ..self
        }
    }

    /// 叠加等宽。
    fn mono(self) -> Self {
        Self { mono: true, ..self }
    }

    /// 叠加链接（蓝色下划线）。
    fn link(self) -> Self {
        Self { link: true, ..self }
    }

    /// 叠加灰色（占位文本）。
    fn gray(self) -> Self {
        Self { gray: true, ..self }
    }
}

/// 按样式构造游程（含字体分区与字号）。
fn styled_run(text: &str, style: Style) -> Run {
    styled_run_sized(text, style, BODY_HALF_POINTS)
}

/// 按样式与字号构造游程。
fn styled_run_sized(text: &str, style: Style, size: usize) -> Run {
    let mut run = Run::new()
        .add_text(text)
        .fonts(if style.mono {
            mono_fonts()
        } else {
            body_fonts()
        })
        .size(size);
    if style.bold {
        run = run.bold();
    }
    if style.italic {
        run = run.italic();
    }
    if style.strike {
        run = run.strike();
    }
    if style.link {
        run = run.color("0563C1").underline("single");
    }
    if style.gray {
        run = run.color("595959");
    }
    run
}

/// 文本片段追加（空串跳过）。
fn push_text_piece(pieces: &mut Vec<Piece>, text: String, style: Style) {
    if text.is_empty() {
        return;
    }
    pieces.push(Piece::Run(styled_run(&text, style)));
}

/// 游程追加（small 语境用小字号，空串跳过）。
fn push_text_run(runs: &mut Vec<Run>, text: String, style: Style, small: bool) {
    if text.is_empty() {
        return;
    }
    let size = if small {
        SMALL_HALF_POINTS
    } else {
        BODY_HALF_POINTS
    };
    runs.push(styled_run_sized(&text, style, size));
}

#[cfg(test)]
mod tests {
    use super::render_docx_bytes;
    use crate::export::ExportOptions;

    /// 测试选项构造。
    fn test_options() -> ExportOptions {
        ExportOptions {
            title: "测试文档".to_string(),
            include_toc: true,
            base_dir: None,
        }
    }

    #[test]
    fn docx_bytes_are_zip_container() {
        let bytes = render_docx_bytes("# 你好\n\n世界 Hello\n", &test_options()).expect("渲染成功");
        assert!(bytes.starts_with(b"PK"), "前两字节：{:?}", &bytes[..2]);
        assert!(bytes.len() > 1000, "体积：{}", bytes.len());
    }

    #[test]
    fn docx_renders_full_document_structure() {
        let markdown = "# 一级\n\n## 二级\n\n正文**加粗**与*斜体*与`代码`与[链接](https://x.com)。\n\n- 甲\n- [x] 乙\n\n1. 有序一\n2. 有序二\n\n> 引用块\n\n| 甲 | 乙 |\n| --- | --- |\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n\n---\n\n$$x^2$$\n\n![图](a.png)\n";
        let bytes = render_docx_bytes(markdown, &test_options()).expect("渲染成功");
        assert!(bytes.starts_with(b"PK"));
    }

    #[test]
    fn docx_without_toc_renders() {
        let options = ExportOptions {
            title: "plain".to_string(),
            include_toc: false,
            base_dir: None,
        };
        let bytes = render_docx_bytes("Hello *world*.\n", &options).expect("渲染成功");
        assert!(bytes.starts_with(b"PK"));
    }

    #[test]
    fn docx_renders_repo_regression_fixture() {
        let markdown = include_str!("../../docs/render-sample.md");
        let bytes = render_docx_bytes(markdown, &test_options()).expect("渲染成功");
        assert!(bytes.starts_with(b"PK"));
    }

    #[test]
    fn docx_empty_document_renders() {
        let bytes = render_docx_bytes("", &test_options()).expect("渲染成功");
        assert!(bytes.starts_with(b"PK"));
    }

    /// 解包生成的文档部件原文（测试专用解包）。
    #[cfg(test)]
    fn unpack_docx_part(bytes: &[u8], name: &str) -> String {
        use std::io::Read as _;
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor).expect("合法 zip 容器");
        let mut part = archive.by_name(name).expect("部件存在");
        let mut text = String::new();
        part.read_to_string(&mut text).expect("部件为 UTF-8");
        text
    }

    #[test]
    fn docx_embeds_local_image_into_media() {
        let root = std::env::temp_dir().join(format!("rumark-docx-img-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("temp dir");
        let fixture = image::RgbImage::from_pixel(4, 3, image::Rgb([200, 30, 30]));
        fixture
            .save(root.join("pic.png"))
            .expect("write fixture png");

        let options = ExportOptions {
            title: "嵌图".to_string(),
            include_toc: false,
            base_dir: Some(root.clone()),
        };
        let bytes =
            render_docx_bytes("前文\n\n![图](./pic.png)\n\n后文", &options).expect("渲染成功");
        let archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("合法 zip");
        assert!(
            archive
                .file_names()
                .any(|name| name.starts_with("word/media/")),
            "应内嵌图片部件：{:?}",
            archive.file_names().collect::<Vec<_>>()
        );
        let document = unpack_docx_part(&bytes, "word/document.xml");
        assert!(document.contains("blip"), "图片引用落盘：{document}");
        assert!(document.contains("前文") && document.contains("后文"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn docx_renders_mermaid_as_image_not_source() {
        let options = ExportOptions {
            title: "mermaid".to_string(),
            include_toc: false,
            base_dir: None,
        };
        let bytes = render_docx_bytes("```mermaid\nflowchart TD\n  A --> B\n```\n", &options)
            .expect("渲染成功");
        let archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("合法 zip");
        assert!(
            archive
                .file_names()
                .any(|name| name.starts_with("word/media/")),
            "应内嵌图片"
        );
        let document = unpack_docx_part(&bytes, "word/document.xml");
        assert!(document.contains("blip"), "图片引用落盘");
        assert!(!document.contains("flowchart TD"), "源码不应写进正文");
    }

    #[test]
    fn docx_missing_image_falls_back_to_alt_text() {
        let bytes = render_docx_bytes("![缺图](./missing.png)", &test_options()).expect("渲染成功");
        let document = unpack_docx_part(&bytes, "word/document.xml");
        assert!(document.contains("[图片：缺图]"), "占位文本：{document}");
    }

    #[test]
    fn docx_document_xml_carries_headings_lists_and_toc() {
        let markdown = "# 一级标题\n\n正文**加粗**。\n\n- 甲\n- 乙\n\n3. 丙\n4. 丁\n";
        let bytes = render_docx_bytes(markdown, &test_options()).expect("渲染成功");
        let document = unpack_docx_part(&bytes, "word/document.xml");
        assert!(document.contains("一级标题"), "标题文本落盘");
        assert!(document.contains("Heading1"), "标题样式引用");
        assert!(document.contains("加粗"), "加粗文本落盘");
        assert!(document.contains("numId"), "列表编号引用");
        assert!(
            document.contains("Table of Contents") || document.contains("TOC"),
            "目录域"
        );
        let numbering = unpack_docx_part(&bytes, "word/numbering.xml");
        assert!(numbering.contains("101"), "无序列表编号定义");
        // 有序列表按列表注册独立编号定义（首个列表 id = 103），起始号 3 落盘，
        // 避免 Word 把多个列表接续编号。
        assert!(numbering.contains("103"), "有序列表独立编号定义");
        assert!(
            numbering.contains("w:start w:val=\"3\""),
            "有序列表起始号落盘：{numbering}"
        );
        assert!(
            document.contains("\"103\""),
            "有序列表引用独立编号：{document}"
        );
    }
}
