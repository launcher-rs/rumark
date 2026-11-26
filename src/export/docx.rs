//! Markdown 到 DOCX 渲染（`export-docx` feature 门控）。
//!
//! 经共享块模型直写原生 Word 文档：标题样式、真列表编号、网格表格、
//! 目录域与中英文字体分区（西文 Calibri/Consolas、中文微软雅黑），
//! 全程纯 Rust、无外部进程依赖。公式回退等宽原文，Mermaid 图渲染为图片嵌入，
//! 段落内图片解码转 PNG 嵌入（加载失败回退替代文本占位）。

use std::path::PathBuf;

use docx_rs::{
    AbstractNumbering, AlignmentType, Docx, Hyperlink, HyperlinkType, IndentLevel, Level, LevelJc,
    LevelText, LineSpacing, LineSpacingType, NumberFormat, Numbering, NumberingId, Paragraph,
    ParagraphBorder, ParagraphBorderPosition, ParagraphBorders, Pic, Run, RunFonts, Shading, Start,
    Style as DocxStyle, StyleType, Table, TableAlignmentType, TableBorder, TableBorderPosition,
    TableBorders, TableCell, TableLayoutType, TableOfContents, TableRow, WidthType,
};

use super::{Align, Block, ExportOptions, Inline, collect_headings, inline_text, parse_blocks};
use crate::theme::ThemeTypography;
use rgpui::hsla;

/// 无序列表的抽象编号 ID（实现内保留，与有序列表区分）。
const BULLET_ABSTRACT_ID: usize = 101;
/// 有序列表的抽象编号 ID。
const DECIMAL_ABSTRACT_ID: usize = 102;
/// 正文行高对应的磅值（docx-rs 字号以半磅计）。
const BODY_HALF_POINTS: usize = 21;
/// 代码与表格的较小字号（半磅）。
const SMALL_HALF_POINTS: usize = 18;
/// 版心宽度（twips）：A4 宽 11906 减左右边距各 1701。
const CONTENT_WIDTH_TWIPS: usize = 11906 - 1701 * 2;
/// 版心宽度（EMU）：1 twip = 635 EMU。
const CONTENT_WIDTH_EMU: u32 = (CONTENT_WIDTH_TWIPS as u32) * 635;
/// 图片高度上限（EMU）：约版心高六成，避免单图撑满整页。
const MAX_IMAGE_HEIGHT_EMU: u32 = 5_000_000;
/// 1 像素对应的 EMU（96dpi）。
const EMU_PER_PX: f64 = 9525.0;
/// 代码块与行内代码的底纹。
const CODE_SHADE: &str = "F2F2F2";
/// 表头底纹。
const TABLE_HEAD_SHADE: &str = "EDEDED";
/// 表格与分隔线颜色。
const BORDER_COLOR: &str = "BFBFBF";
/// 代码块与表格的上下留白（twips，6pt）。
const BLOCK_SPACING_TWIPS: i32 = 120;
/// 表格单元格内边距（twips）：上下 4pt、左右 6pt。
const TABLE_CELL_PAD_Y: usize = 80;
const TABLE_CELL_PAD_X: usize = 120;

/// 固定高度的空段落，用作表格前后的视觉留白。
fn spacer_paragraph() -> Paragraph {
    Paragraph::new().line_spacing(
        LineSpacing::new()
            .line(BLOCK_SPACING_TWIPS)
            .line_rule(LineSpacingType::Exact),
    )
}

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
        // 文档级默认值：WPS/Word 对「无样式」段落有自己的默认字体与行距，
        // 不显式写清就会各处不一致。这里统一正文字体、字号与 1.15 倍行距。
        .default_fonts(body_fonts())
        .default_size(BODY_HALF_POINTS)
        .default_line_spacing(
            LineSpacing::new()
                .line(276)
                .line_rule(LineSpacingType::Auto),
        )
        .add_abstract_numbering(bullet_abstract_numbering())
        .add_numbering(Numbering::new(BULLET_ABSTRACT_ID, BULLET_ABSTRACT_ID));
    // docx-rs 默认只写 Normal 样式；文档里引用的 HeadingN/Title 若不定义，
    // Word 会按正文渲染，标题完全没有层级效果。
    for style in document_styles() {
        doc = doc.add_style(style);
    }
    if options.include_toc && !headings.is_empty() {
        // 目录标题用 Title 样式而非 Heading1：TOC 域按 Heading1-3 收集条目，
        // 若自身也用 Heading1，Word 刷新域时会把「目录」一节收进目录里。
        doc = doc
            .add_paragraph(Paragraph::new().style("Title").add_run(body_run("目录")))
            .add_table_of_contents(
                TableOfContents::new()
                    .heading_styles_range(1, 3)
                    .alias(options.title.as_str())
                    .auto(),
            );
    }
    let mut writer = DocxWriter {
        depth: 0,
        quote_style_active: false,
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

/// 导出用到的样式定义。docx-rs 只内置 `Normal`，标题样式必须显式注册，
/// 否则 Word 打开时会把 `Heading1..6` 当成未知样式、按正文显示。
fn document_styles() -> Vec<DocxStyle> {
    // 半磅单位：一级最大，逐级收敛但仍明显大于正文（正文 21）。
    let heading_sizes = [40, 34, 30, 27, 24, 22];
    let mut styles = Vec::with_capacity(7);
    for (index, size) in heading_sizes.into_iter().enumerate() {
        let level = index + 1;
        styles.push(
            DocxStyle::new(format!("Heading{level}"), StyleType::Paragraph)
                // Word 内置样式名是小写 `heading N`，TOC 域据此收集条目。
                .name(format!("heading {level}"))
                .based_on("Normal")
                .next("Normal")
                .q_format(true)
                .ui_priority(9)
                .outline_lvl(index)
                .bold()
                .size(size)
                .fonts(body_fonts())
                // 标题沿用 Markdown 的黑色，不引入主题色。
                .color("000000"),
        );
    }
    styles.push(
        DocxStyle::new("Title", StyleType::Paragraph)
            .name("Title")
            .based_on("Normal")
            .next("Normal")
            .q_format(true)
            .bold()
            .size(44)
            .fonts(body_fonts()),
    );
    // 代码块、引用块使用具名样式：WPS/Word 打开时能识别为独立段落类型，
    // 而不是一串没有语义的普通段落。
    let mut code_style = DocxStyle::new("CodeBlock", StyleType::Paragraph)
        .name("Code Block")
        .based_on("Normal")
        .next("Normal")
        .fonts(mono_fonts())
        .size(SMALL_HALF_POINTS)
        .indent(Some(360), None, None, None);
    code_style.paragraph_property = code_style
        .paragraph_property
        .shading(Shading::new().fill(CODE_SHADE));
    styles.push(code_style);
    styles.push(
        DocxStyle::new("Quote", StyleType::Paragraph)
            .name("Quote")
            .based_on("Normal")
            .next("Normal")
            .indent(Some(360), None, None, None)
            .color("595959"),
    );
    styles
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

/// Emoji 字体：正文的 Calibri / 雅黑都不含彩色 emoji，必须单独指定，
/// 否则 Word / WPS 会显示成空白或方框。
fn emoji_fonts() -> RunFonts {
    RunFonts::new()
        .ascii("Segoe UI Emoji")
        .hi_ansi("Segoe UI Emoji")
        .east_asia("Segoe UI Emoji")
        .cs("Segoe UI Emoji")
}

/// 是否按 emoji 处理该码点（含变体选择符、ZWJ、区域指示符等组合标记）。
fn is_emoji_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1F000..=0x1FAFF
            | 0x2600..=0x27BF
            | 0x2194..=0x21AA
            | 0x231A..=0x231B
            | 0x23E9..=0x23F3
            | 0x23F8..=0x23FA
            | 0x25FD..=0x25FE
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x200D
            | 0x20E3
            | 0xFE0E..=0xFE0F
    )
}

/// 按 emoji / 非 emoji 切分文本，分别构造游程（emoji 用专用字体）。
fn split_emoji_runs(text: &str, mut build: impl FnMut(&str, bool) -> Run) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut segment = String::new();
    let mut segment_is_emoji = false;
    let mut started = false;
    for ch in text.chars() {
        let is_emoji = is_emoji_char(ch);
        if started && is_emoji != segment_is_emoji {
            runs.push(build(&segment, segment_is_emoji));
            segment.clear();
        }
        segment_is_emoji = is_emoji;
        started = true;
        segment.push(ch);
    }
    if started {
        runs.push(build(&segment, segment_is_emoji));
    }
    runs
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

/// 公式 SVG 转 DOCX 图片：按逻辑像素尺寸嵌入，不放大到版心宽。
fn math_pic(svg: &str, scale: f32) -> Option<Pic> {
    let png = crate::export::mermaid::svg_to_png_bytes(svg, scale)?;
    let decoded = crate::export::image::decode_image(&png)?;
    let (width, height) = (decoded.width(), decoded.height());
    let mut bytes = Vec::new();
    decoded
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .ok()?;
    let to_emu = |px: u32| ((px as f64 / scale as f64) * EMU_PER_PX).max(1.0) as u32;
    Some(Pic::new_with_dimensions(bytes, width, height).size(to_emu(width), to_emu(height)))
}

/// 行内公式图片（失败返回 `None`，由调用方回退原文）。
fn inline_math_pic(latex: &str) -> Option<Pic> {
    let font_size = crate::components::inline_math_font_size(ThemeTypography::DEFAULT.text_size);
    let svg = crate::components::render_inline_math_svg(latex, hsla(0.0, 0.0, 0.0, 1.0), font_size)
        .ok()?;
    math_pic(&svg.svg, 2.0)
}

/// 展示公式图片（markdown 事件里已是纯 LaTeX 正文，不含 `$$`）。
fn display_math_pic(latex: &str) -> Option<Pic> {
    let font_size = crate::components::display_math_font_size(ThemeTypography::DEFAULT.text_size);
    let svg = crate::components::render_inline_math_svg(latex, hsla(0.0, 0.0, 0.0, 1.0), font_size)
        .ok()?;
    math_pic(&svg.svg, 2.0)
}

/// 块装配器（`depth` 为列表嵌套深度，决定编号层级）。
struct DocxWriter {
    depth: u32,
    /// 是否正在引用块内：给其中的段落挂 `Quote` 样式。
    quote_style_active: bool,
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
                for run in self.heading_runs(inline) {
                    paragraph = paragraph.add_run(run);
                }
                doc.add_paragraph(paragraph)
            }
            Block::Paragraph(inline) => {
                if inline.is_empty() {
                    return doc;
                }
                let mut paragraph = Paragraph::new();
                if self.quote_style_active {
                    paragraph = paragraph.style("Quote");
                }
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
                let lines: Vec<&str> = trimmed.split('\n').collect();
                // 上下各加一条同底纹的窄空行作为内边距（段前/段后间距不会撑开底纹）。
                let pad = LineSpacing::new()
                    .line(BLOCK_SPACING_TWIPS)
                    .line_rule(LineSpacingType::Exact);
                doc = doc.add_paragraph(
                    Paragraph::new()
                        .style("CodeBlock")
                        .keep_next(true)
                        .line_spacing(pad.clone().before(BLOCK_SPACING_TWIPS as u32)),
                );
                for line in lines {
                    // 行首两个空格作为左右内边距，避免文字贴着底纹边缘。
                    let line = format!("  {}", line);
                    doc = doc.add_paragraph(
                        Paragraph::new()
                            .style("CodeBlock")
                            .keep_next(true)
                            .add_run(mono_run(&line)),
                    );
                }
                doc.add_paragraph(
                    Paragraph::new()
                        .style("CodeBlock")
                        .line_spacing(pad.after(BLOCK_SPACING_TWIPS as u32)),
                )
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
                if let Some(pic) = display_math_pic(text) {
                    return doc.add_paragraph(
                        Paragraph::new()
                            .align(AlignmentType::Center)
                            .add_run(Run::new().add_image(pic)),
                    );
                }
                // 渲染失败回退原文，保证内容不丢失。
                doc.add_paragraph(Paragraph::new().add_run(mono_run(text)))
            }
            Block::Quote(inner) => {
                let mut doc = doc;
                let was_quote = self.quote_style_active;
                self.quote_style_active = true;
                self.depth += 1;
                for block in inner {
                    doc = self.push_block(doc, block);
                }
                self.depth -= 1;
                self.quote_style_active = was_quote;
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
            // 分隔线用空段落的下边框画真实横线，不再塞一行星号文字。
            Block::Rule => {
                // 必须从空白边框集起，否则 docx-rs 的默认实现会补上四条边，
                // Word 里横线就变成了一个矩形框。
                let borders = ParagraphBorders::with_empty().set(
                    ParagraphBorder::new(ParagraphBorderPosition::Bottom)
                        .size(6)
                        .color(BORDER_COLOR),
                );
                let mut paragraph = Paragraph::new();
                paragraph.property = paragraph.property.set_borders(borders);
                doc.add_paragraph(paragraph)
            }
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
        // 均匀列宽 + 固定布局，避免 Word 按内容自行分配导致宽窄失衡。
        let column_twips = CONTENT_WIDTH_TWIPS / columns;
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
        // 单元格内边距：Word 默认单元格几乎贴着文字，表格显得很局促。
        let make_cell = |inline: &[Inline], header: bool, index: usize| {
            let mut cell = TableCell::new().width(column_twips, WidthType::Dxa);
            if header {
                cell = cell.shading(Shading::new().fill(TABLE_HEAD_SHADE));
            }
            cell.property = cell
                .property
                .margin_top(TABLE_CELL_PAD_Y, WidthType::Dxa)
                .margin_bottom(TABLE_CELL_PAD_Y, WidthType::Dxa)
                .margin_left(TABLE_CELL_PAD_X, WidthType::Dxa)
                .margin_right(TABLE_CELL_PAD_X, WidthType::Dxa);
            cell.add_paragraph(cell_paragraph(inline, header, index))
        };
        if !headers.iter().all(Vec::is_empty) {
            let cells = (0..columns)
                .map(|index| {
                    make_cell(
                        headers.get(index).map(Vec::as_slice).unwrap_or(&[]),
                        true,
                        index,
                    )
                })
                .collect();
            table_rows.push(TableRow::new(cells));
        }
        for row in rows {
            let cells = (0..columns)
                .map(|index| {
                    make_cell(
                        row.get(index).map(Vec::as_slice).unwrap_or(&[]),
                        false,
                        index,
                    )
                })
                .collect();
            table_rows.push(TableRow::new(cells));
        }
        if table_rows.is_empty() {
            return doc;
        }
        // docx-rs 不内置 TableGrid 样式，直接写边框才可靠显示网格。
        let grid_border = |position| TableBorder::new(position).size(6).color(BORDER_COLOR);
        let borders = TableBorders::new()
            .set(grid_border(TableBorderPosition::Top))
            .set(grid_border(TableBorderPosition::Left))
            .set(grid_border(TableBorderPosition::Bottom))
            .set(grid_border(TableBorderPosition::Right))
            .set(grid_border(TableBorderPosition::InsideH))
            .set(grid_border(TableBorderPosition::InsideV));
        // 表格本身没有段前/段后属性，用固定高度的空段落做上下留白。
        doc.add_paragraph(spacer_paragraph())
            .add_table(
                Table::new(table_rows)
                    .set_grid(vec![column_twips; columns])
                    .width(CONTENT_WIDTH_TWIPS, WidthType::Dxa)
                    .layout(TableLayoutType::Fixed)
                    .set_borders(borders)
                    .align(TableAlignmentType::Center),
            )
            .add_paragraph(spacer_paragraph())
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
                Inline::Math(math) => {
                    if let Some(pic) = inline_math_pic(math) {
                        pieces.push(Piece::Run(Run::new().add_image(pic)));
                    } else {
                        push_text_piece(pieces, math.clone(), style.mono());
                    }
                }
                Inline::Link { label, url } => {
                    let mut link = Hyperlink::new(url, HyperlinkType::External);
                    for run in styled_runs(label, style.link()) {
                        link = link.add_run(run);
                    }
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
                Inline::Math(math) => {
                    if let Some(pic) = inline_math_pic(math) {
                        runs.push(Run::new().add_image(pic));
                    } else {
                        push_text_run(runs, math.clone(), style.mono(), small);
                    }
                }
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

    /// 标题游程只保留行内强调，不写死字号与字体，交由 `HeadingN` 样式控制：
    /// 直接格式化优先级高于样式，写死正文号会把标题压成正文外观。
    fn heading_runs(&self, inline: &[Inline]) -> Vec<Run> {
        let mut runs = Vec::new();
        self.collect_heading_runs(inline, Style::default(), &mut runs);
        runs
    }

    /// 标题语境的行内树转游程。
    fn collect_heading_runs(&self, inline: &[Inline], style: Style, runs: &mut Vec<Run>) {
        for node in inline {
            match node {
                Inline::Text(text) => push_heading_run(runs, text.clone(), style),
                Inline::Strong(inner) => self.collect_heading_runs(inner, style.bold(), runs),
                Inline::Emphasis(inner) => self.collect_heading_runs(inner, style.italic(), runs),
                Inline::Strike(inner) => self.collect_heading_runs(inner, style.strike(), runs),
                Inline::Code(code) => push_heading_run(runs, code.clone(), style.mono()),
                Inline::Math(math) => {
                    if let Some(pic) = inline_math_pic(math) {
                        runs.push(Run::new().add_image(pic));
                    } else {
                        push_heading_run(runs, math.clone(), style.mono());
                    }
                }
                Inline::Link { label, .. } => push_heading_run(runs, label.clone(), style.link()),
                Inline::Image { alt, .. } => {
                    push_heading_run(runs, format!("[图片：{alt}]"), style.gray());
                }
                Inline::FootnoteRef(label) => {
                    push_heading_run(runs, format!("[^{label}]"), style);
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

/// 按样式构造游程（含字体分区与字号），emoji 自动换专用字体。
fn styled_runs(text: &str, style: Style) -> Vec<Run> {
    styled_runs_sized(text, style, BODY_HALF_POINTS)
}

/// 按样式与字号构造游程。
fn styled_runs_sized(text: &str, style: Style, size: usize) -> Vec<Run> {
    split_emoji_runs(text, |part, is_emoji| {
        let mut run = Run::new()
            .add_text(part)
            .fonts(if is_emoji {
                emoji_fonts()
            } else if style.mono {
                mono_fonts()
            } else {
                body_fonts()
            })
            .size(size);
        // 行内代码加浅灰底纹，反引号内容在 Word 里才有可辨识的代码外观。
        if style.mono && !is_emoji {
            run = run.shading(Shading::new().fill(CODE_SHADE));
        }
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
    })
}

/// 文本片段追加（空串跳过）。
fn push_text_piece(pieces: &mut Vec<Piece>, text: String, style: Style) {
    if text.is_empty() {
        return;
    }
    pieces.extend(styled_runs(&text, style).into_iter().map(Piece::Run));
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
    runs.extend(styled_runs_sized(&text, style, size));
}

/// 标题游程追加：不写死字号/字体，让 `HeadingN` 样式生效；emoji 单独换字体。
fn push_heading_run(runs: &mut Vec<Run>, text: String, style: Style) {
    if text.is_empty() {
        return;
    }
    runs.extend(split_emoji_runs(&text, |part, is_emoji| {
        let mut run = Run::new().add_text(part);
        if is_emoji {
            run = run.fonts(emoji_fonts());
        }
        if style.bold {
            run = run.bold();
        }
        if style.italic {
            run = run.italic();
        }
        if style.strike {
            run = run.strike();
        }
        if style.mono && !is_emoji {
            run = run
                .fonts(mono_fonts())
                .shading(Shading::new().fill(CODE_SHADE));
        }
        if style.link {
            run = run.color("0563C1").underline("single");
        }
        if style.gray {
            run = run.color("595959");
        }
        run
    }));
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

    #[test]
    fn docx_registers_heading_styles() {
        // docx-rs 默认只写 Normal；标题样式必须显式注册，Word 才会呈现层级。
        let bytes =
            render_docx_bytes("# 一级标题\n\n## 二级标题\n", &test_options()).expect("渲染成功");
        let styles = unpack_docx_part(&bytes, "word/styles.xml");
        assert!(
            styles.contains("w:styleId=\"Heading1\""),
            "styles: {styles}"
        );
        assert!(
            styles.contains("w:styleId=\"Heading6\""),
            "styles: {styles}"
        );
        assert!(
            styles.contains("w:val=\"heading 1\""),
            "内置样式名可供 TOC 收集"
        );
        let document = unpack_docx_part(&bytes, "word/document.xml");
        assert!(document.contains("Heading1") && document.contains("Heading2"));
    }

    #[test]
    fn docx_table_has_visible_borders_and_fixed_grid() {
        let markdown = "| 甲 | 乙 |\n| --- | --- |\n| 1 | 2 |\n";
        let bytes = render_docx_bytes(markdown, &test_options()).expect("渲染成功");
        let document = unpack_docx_part(&bytes, "word/document.xml");
        assert!(document.contains("<w:tblBorders"), "表格边框：{document}");
        assert!(document.contains("<w:gridCol"), "表格列宽网格：{document}");
        assert!(
            document.contains("<w:tblLayout w:type=\"fixed\""),
            "固定列宽"
        );
        assert!(document.contains("w:type=\"dxa\""), "twips 列宽单位");
        assert!(document.contains("w:fill=\"EDEDED\""), "表头底纹");
        // 单元格内边距：上下 4pt、左右 6pt。
        assert!(
            document.contains("<w:tcMar>") && document.contains("w:w=\"80\""),
            "单元格内边距：{document}"
        );
        // 表格前后各有固定高度的空段落做留白。
        assert!(
            document
                .matches("w:line=\"120\" w:lineRule=\"exact\"")
                .count()
                >= 2,
            "表格上下留白：{document}"
        );
    }

    #[test]
    fn docx_rule_is_real_line_and_code_has_shading() {
        let markdown = "---\n\n```rust\nlet x = 1;\n```\n\n行内 `code` 代码。\n";
        let bytes = render_docx_bytes(markdown, &test_options()).expect("渲染成功");
        let document = unpack_docx_part(&bytes, "word/document.xml");
        // 分隔线是空段落的下边框，且只能有下边框（docx-rs 默认会补四条边变成矩形）。
        let rule_borders = document
            .split("<w:pBdr>")
            .find(|part| part.starts_with("<w:bottom"))
            .expect("分隔线应有下边框");
        assert!(
            !rule_borders.contains("<w:left")
                && !rule_borders.contains("<w:right")
                && !rule_borders.contains("<w:top"),
            "横线不应是矩形：{rule_borders}"
        );
        assert!(!document.contains("＊"), "不应再输出星号占位");
        // 代码块上下留白与底纹内的内边距空行。
        assert!(document.contains("w:before=\"120\""), "代码块上方留白");
        assert!(document.contains("w:after=\"120\""), "代码块下方留白");
        assert!(
            document
                .matches("w:line=\"120\" w:lineRule=\"exact\"")
                .count()
                >= 2,
            "代码块上下内边距：{document}"
        );
        // 行内代码底纹仍写在游程上；代码块底纹已移到 CodeBlock 样式里。
        assert!(
            document.contains("w:fill=\"F2F2F2\""),
            "行内代码底纹：{document}"
        );
        let styles = unpack_docx_part(&bytes, "word/styles.xml");
        assert!(
            styles.contains("w:fill=\"F2F2F2\""),
            "代码块样式底纹：{styles}"
        );
    }

    #[test]
    fn docx_emoji_uses_emoji_font_without_changing_cjk() {
        let bytes = render_docx_bytes("你好 😀 世界", &test_options()).expect("渲染成功");
        let document = unpack_docx_part(&bytes, "word/document.xml");
        assert!(document.contains("😀"), "emoji 正文应写入：{document}");
        assert!(
            document.contains("Segoe UI Emoji"),
            "emoji 必须换专用字体：{document}"
        );
        assert!(
            document.contains("Microsoft YaHei"),
            "中文仍走雅黑字体：{document}"
        );
    }

    #[test]
    fn docx_heading_styles_use_black() {
        let bytes = render_docx_bytes("# 标题\n", &test_options()).expect("渲染成功");
        let styles = unpack_docx_part(&bytes, "word/styles.xml");
        assert!(
            styles.contains("w:color w:val=\"000000\"") || !styles.contains("2F5496"),
            "标题应为黑色：{styles}"
        );
        assert!(!styles.contains("2F5496"), "不应残留蓝色：{styles}");
    }

    #[test]
    fn docx_registers_block_styles_and_document_defaults() {
        let markdown = "> 引用段落\n\n```\nlet x = 1;\n```\n";
        let bytes = render_docx_bytes(markdown, &test_options()).expect("渲染成功");
        let styles = unpack_docx_part(&bytes, "word/styles.xml");
        assert!(
            styles.contains("w:styleId=\"CodeBlock\""),
            "styles: {styles}"
        );
        assert!(styles.contains("w:styleId=\"Quote\""), "styles: {styles}");
        assert!(styles.contains("Microsoft YaHei"), "文档默认字体：{styles}");
        // 代码块底纹写在样式里，而不是每行的直接格式。
        assert!(
            styles.contains("w:fill=\"F2F2F2\""),
            "代码样式底纹：{styles}"
        );
        let document = unpack_docx_part(&bytes, "word/document.xml");
        assert!(document.contains("CodeBlock"), "代码块引用样式：{document}");
        assert!(document.contains("Quote"), "引用段落引用样式：{document}");
    }

    #[test]
    fn docx_renders_math_as_embedded_images() {
        let markdown = "行内 $E = mc^2$\n\n$$\n\\int_0^1 x^2 \\, dx = \\frac{1}{3}\n$$\n";
        let bytes = render_docx_bytes(markdown, &test_options()).expect("渲染成功");
        let archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("合法 zip");
        assert!(
            archive
                .file_names()
                .any(|name| name.starts_with("word/media/")),
            "公式应栅格化为图片内嵌"
        );
        let document = unpack_docx_part(&bytes, "word/document.xml");
        assert!(document.contains("blip"), "公式图片引用：{document}");
    }
}
