//! 纯 Rust 的 Markdown 到 PDF 渲染（`export-pdf` feature 门控）。
//!
//! 不依赖浏览器：经共享块模型手排 A4 版面，中文经内嵌子集中文字体呈现。
//! 字体来源按序为：构建时 `assets/fonts/` 自备字体（build.rs 命中即内嵌，
//! OFL 的 Noto Sans SC 优先）→ 各平台系统字体目录 → 纯英文文档回退内置
//! Helvetica。含中文但找不到中文字体时返回可操作错误，而非输出乱码方框。

use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use anyhow::{Context as _, anyhow};
use printpdf::{
    BuiltinFont, Color, Line, LinePoint, Mm, Op, PaintMode, ParsedFont, PdfDocument, PdfFontHandle,
    PdfPage, PdfSaveOptions, Point, Pt, RawImage, RawImageData, RawImageFormat, Rect, Rgb,
    TextItem, XObjectTransform,
};

use super::{Align, Block, ExportOptions, Inline, collect_headings, inline_text, parse_blocks};

/// A4 纸张（毫米）。
const PAGE_WIDTH_MM: f32 = 210.0;
const PAGE_HEIGHT_MM: f32 = 297.0;
/// 版心边距（毫米）。
const MARGIN_LEFT_MM: f32 = 20.0;
const MARGIN_RIGHT_MM: f32 = 20.0;
const MARGIN_TOP_MM: f32 = 22.0;
const MARGIN_BOTTOM_MM: f32 = 20.0;
/// 正文与各级标题字号（pt）。
const BODY_PT: f32 = 10.5;
const CODE_PT: f32 = 9.0;
const SMALL_PT: f32 = 9.5;

/// 毫米与 PDF 点换算：1pt ≈ 0.3528mm（1 英寸 = 25.4mm = 72pt）。
/// 注意区别于 96dpi 屏幕像素的 mm/px ≈ 0.2646，二者混用会让版心宽度偏差约 1.33 倍。
const MM_PER_PT: f32 = 0.3528;
const PT_PER_MM: f32 = 2.8346;

/// 把 Markdown 渲染为完整 PDF 字节（含中文子集字体）。
pub(crate) fn render_pdf_bytes(markdown: &str, options: &ExportOptions) -> anyhow::Result<Vec<u8>> {
    let blocks = parse_blocks(markdown);
    let fonts = Fonts::load(markdown)?;
    // 外嵌字体在文档级注册一次，各行复用句柄（子集化在保存时自动完成）。
    let mut doc = PdfDocument::new(&options.title);
    let font = match &fonts.parsed {
        Some(parsed) => PdfFontHandle::External(doc.add_font(parsed)),
        None => PdfFontHandle::Builtin(BuiltinFont::Helvetica),
    };
    let mut layout = Layout::new(&fonts, font, &mut doc, options.base_dir.clone());
    if options.include_toc {
        layout.push_toc(&collect_headings(&blocks));
    }
    layout.push_blocks(&blocks, 0);
    layout.finish()
}

/// 缺中文字体标记错误（调用方据此切换 i18n 文案，而非解析字符串）。
#[derive(Debug)]
pub(crate) struct MissingCjkFont;

impl std::fmt::Display for MissingCjkFont {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "missing CJK font")
    }
}

impl std::error::Error for MissingCjkFont {}

/// 是否缺中文字体错误（中文 PDF 特有，调用方映射为本地化提示）。
pub(crate) fn is_missing_cjk_font(error: &anyhow::Error) -> bool {
    error.downcast_ref::<MissingCjkFont>().is_some()
}

/// 渲染用字体：中文外嵌字体缺席且文档含中文时直接报错。
struct Fonts {
    parsed: Option<Arc<ParsedFont>>,
    units_per_em: f32,
    space_width_units: f32,
}

/// 进程内共享的已解析字体。CJK 字体动辄数十 MB，每次导出重新读盘并解析
/// 会把「点一次导出」变成数百毫秒的纯 CPU 开销；解析结果只读，可安全复用。
#[derive(Clone)]
struct CachedCjkFont {
    parsed: Arc<ParsedFont>,
    units_per_em: f32,
    space_width_units: f32,
}

/// 只缓存解析成功的字体；「找不到字体」与解析失败都不缓存，下次导出会
/// 重新探测，这样用户补齐/修复字体后无需重启应用即可恢复。
static CJK_FONT_CACHE: Mutex<Option<CachedCjkFont>> = Mutex::new(None);
/// 字体探测/解析次数（测试用：缓存预热后连续多次导出不应再增加）。
static CJK_FONT_PARSES: AtomicUsize = AtomicUsize::new(0);

fn cached_cjk_font() -> Result<Option<CachedCjkFont>, String> {
    // 冷缓存时在锁内探测并解析：多窗口同时导出只解析一次，不重复读盘/解析
    // 数十 MB 字体；锁毒化时取回内部值继续，避免一次 panic 让导出永久失效。
    let mut cache = CJK_FONT_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(font) = cache.as_ref() {
        return Ok(Some(font.clone()));
    }
    // 缓存未命中（或上次失败/没有字体）：重新探测并解析。
    CJK_FONT_PARSES.fetch_add(1, Ordering::Relaxed);
    let Some((bytes, index, origin)) = bundled_cjk_font().or_else(system_cjk_font) else {
        return Ok(None);
    };
    let mut warnings = Vec::new();
    let parsed = ParsedFont::from_bytes(&bytes, index, &mut warnings)
        .with_context(|| format!("中文字体解析失败：{origin}"))
        .map_err(|err| format!("{err:#}"))?;
    let metrics = parsed.get_font_metrics();
    let units_per_em = metrics.units_per_em.max(1) as f32;
    let space_width_units = parsed
        .lookup_glyph_index(' ' as u32)
        .and_then(|gid| parsed.get_glyph_width_internal(gid))
        .map(|width| width as f32)
        .unwrap_or(units_per_em / 4.0);
    let cached = CachedCjkFont {
        parsed: Arc::new(parsed),
        units_per_em,
        space_width_units,
    };
    *cache = Some(cached.clone());
    Ok(Some(cached))
}

impl Fonts {
    /// 按内嵌 → 系统 → 内置的顺序加载字体，命中进程级缓存时零解析。
    fn load(markdown: &str) -> anyhow::Result<Self> {
        match cached_cjk_font() {
            Ok(Some(font)) => Ok(Self {
                parsed: Some(font.parsed.clone()),
                units_per_em: font.units_per_em,
                space_width_units: font.space_width_units,
            }),
            Ok(None) if contains_cjk(markdown) => Err(MissingCjkFont.into()),
            // 纯英文文档回退内置 Helvetica 的既有度量。
            Ok(None) => Ok(Self {
                parsed: None,
                units_per_em: 1000.0,
                space_width_units: 278.0,
            }),
            Err(message) => Err(anyhow!("{message}")),
        }
    }

    /// 字符串排印宽度（pt），按字形 advance 累加，缺字按半角空格回退。
    fn text_width_pt(&self, text: &str, size_pt: f32) -> f32 {
        let mut units = 0.0f32;
        for ch in text.chars() {
            let gid = self
                .parsed
                .as_ref()
                .and_then(|font| font.lookup_glyph_index(ch as u32));
            let width = gid
                .and_then(|gid| self.parsed.as_ref()?.get_glyph_width_internal(gid))
                .map(|width| width as f32)
                .unwrap_or(self.space_width_units);
            units += width;
        }
        units / self.units_per_em * size_pt
    }
}

/// 构建时内嵌的中文字体（`assets/fonts/` 有字体文件时由 build.rs 注入）。
fn bundled_cjk_font() -> Option<(Vec<u8>, usize, String)> {
    #[cfg(rumark_bundled_cjk)]
    {
        let bytes = include_bytes!(concat!("../../", env!("RUMARK_BUNDLED_CJK_PATH"))).to_vec();
        return Some((bytes, 0, "内嵌字体".to_string()));
    }
    #[cfg(not(rumark_bundled_cjk))]
    {
        None
    }
}

/// 在系统字体目录中查找中文字体，返回（字节，集合下标，来源说明）。
fn system_cjk_font() -> Option<(Vec<u8>, usize, String)> {
    for (path, index) in system_cjk_candidates() {
        if let Ok(bytes) = std::fs::read(&path) {
            return Some((bytes, index, path));
        }
    }
    None
}

/// 各平台系统目录的中文字体候选（OFL 的 Noto 优先，避免商用字体嵌入争议）。
fn system_cjk_candidates() -> Vec<(String, usize)> {
    #[cfg(target_os = "windows")]
    {
        let dir = std::env::var("SystemRoot")
            .map(|root| format!("{root}\\Fonts"))
            .unwrap_or_else(|_| "C:\\Windows\\Fonts".to_string());
        [
            ("NotoSansSC-Regular.ttf", 0),
            ("NotoSansSC-Regular.otf", 0),
            ("NotoSansSC-VF.ttf", 0),
            ("msyh.ttc", 0),
            ("simsun.ttc", 0),
            ("simhei.ttf", 0),
        ]
        .into_iter()
        .map(|(name, index)| (format!("{dir}\\{name}"), index))
        .collect()
    }
    #[cfg(target_os = "macos")]
    {
        [
            ("/System/Library/Fonts/PingFang.ttc", 0),
            ("/System/Library/Fonts/Supplemental/Songti.ttc", 0),
            ("/Library/Fonts/NotoSansSC-Regular.otf", 0),
            ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
        ]
        .into_iter()
        .map(|(path, index)| (path.to_string(), index))
        .collect()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        [
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/truetype/noto/NotoSansSC-Regular.ttf", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/wenquanyi/wqy-microhei.ttc", 0),
        ]
        .into_iter()
        .map(|(path, index)| (path.to_string(), index))
        .collect()
    }
}

/// 文档是否含中文字符（含全角标点与日文假名，排版同按等宽处理）。
fn contains_cjk(text: &str) -> bool {
    // 注：`\u{3000}` 全角空格与假名区落在 `\u{2E80}`..=`\u{9FFF}` 内，不再单列。
    text.chars().any(|ch| {
        matches!(ch,
            '\u{2E80}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{20000}'..='\u{2A6DF}'
            | '\u{FF00}'..='\u{FFEF}')
    })
}

/// 版面装配：把块流转成逐页操作序列（文本节按行独立开关，保证混排合法）。
struct Layout<'a> {
    fonts: &'a Fonts,
    font: PdfFontHandle,
    /// 图片 XObject 需在文档级注册，装配期直接持有文档引用。
    doc: &'a mut PdfDocument,
    base_dir: Option<PathBuf>,
    pages: Vec<Vec<Op>>,
    cursor_mm: f32,
}

/// 正文灰与点缀色（浅底打印友好）。
fn ink() -> Color {
    gray(0.12)
}

/// 次级文字灰。
fn secondary_ink() -> Color {
    gray(0.38)
}

/// 代码块底灰。
fn code_fill() -> Color {
    gray(0.95)
}

/// 表头底灰。
fn head_fill() -> Color {
    gray(0.93)
}

/// 引用竖线蓝。
fn quote_bar() -> Color {
    Color::Rgb(Rgb {
        r: 0.04,
        g: 0.41,
        b: 0.85,
        icc_profile: None,
    })
}

/// 网格线灰。
fn grid_ink() -> Color {
    gray(0.72)
}

/// 灰度色构造（0 黑 ~ 1 白）。
fn gray(value: f32) -> Color {
    Color::Rgb(Rgb {
        r: value,
        g: value,
        b: value,
        icc_profile: None,
    })
}

impl<'a> Layout<'a> {
    /// 新建装配器（首页光标置于上边距）。
    fn new(
        fonts: &'a Fonts,
        font: PdfFontHandle,
        doc: &'a mut PdfDocument,
        base_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            fonts,
            font,
            doc,
            base_dir,
            pages: vec![Vec::new()],
            cursor_mm: MARGIN_TOP_MM,
        }
    }

    /// 版心宽度（毫米）。
    fn content_width_mm(&self) -> f32 {
        PAGE_WIDTH_MM - MARGIN_LEFT_MM - MARGIN_RIGHT_MM
    }

    /// 正文区底部边界（距页顶毫米）。
    fn bottom_mm(&self) -> f32 {
        PAGE_HEIGHT_MM - MARGIN_BOTTOM_MM
    }

    /// 距页顶毫米转 PDF 坐标纵轴（原点左下）。
    fn pdf_y_mm(from_top_mm: f32) -> Mm {
        Mm(PAGE_HEIGHT_MM - from_top_mm)
    }

    /// 确保后续 `need_mm` 高度能容纳，否则换页（段前避寡）。
    fn ensure_space(&mut self, need_mm: f32) {
        if self.cursor_mm + need_mm > self.bottom_mm() {
            self.pages.push(Vec::new());
            self.cursor_mm = MARGIN_TOP_MM;
        }
    }

    /// 换页（显式分页或超长内容兜底）。
    fn new_page(&mut self) {
        self.pages.push(Vec::new());
        self.cursor_mm = MARGIN_TOP_MM;
    }

    /// 纵向留白（跨页时钳到页顶）。
    fn gap(&mut self, mm: f32) {
        self.cursor_mm += mm;
        if self.cursor_mm > self.bottom_mm() {
            self.new_page();
        }
    }

    /// 输出一行文本（独立文本节；`x_mm` 为行左缘）。
    fn text_line(
        &mut self,
        x_mm: f32,
        text: &str,
        size_pt: f32,
        color: Color,
        font: PdfFontHandle,
    ) {
        if text.is_empty() {
            return;
        }
        let line_mm = size_pt * MM_PER_PT;
        self.ensure_space(line_mm + 1.0);
        let y = Self::pdf_y_mm(self.cursor_mm + line_mm * 0.8);
        self.pages.last_mut().expect("至少一页").extend([
            Op::StartTextSection,
            Op::SetTextCursor {
                pos: Point::new(Mm(x_mm), y),
            },
            Op::SetFont {
                font,
                size: Pt(size_pt),
            },
            Op::SetLineHeight { lh: Pt(size_pt) },
            Op::SetFillColor { col: color },
            Op::ShowText {
                items: vec![TextItem::Text(text.to_string())],
            },
            Op::EndTextSection,
        ]);
        self.cursor_mm += line_mm;
    }

    /// 输出横线（分隔线/表格网格共用）。
    fn rule_line(&mut self, x_mm: f32, width_mm: f32, color: Color) {
        self.ensure_space(3.0);
        let y = Self::pdf_y_mm(self.cursor_mm + 1.0);
        self.pages.last_mut().expect("至少一页").extend([
            Op::SetOutlineColor { col: color },
            Op::DrawLine {
                line: Line {
                    points: vec![
                        LinePoint {
                            p: Point::new(Mm(x_mm), y),
                            bezier: false,
                        },
                        LinePoint {
                            p: Point::new(Mm(x_mm + width_mm), y),
                            bezier: false,
                        },
                    ],
                    is_closed: false,
                },
            },
        ]);
        self.cursor_mm += 3.0;
    }

    /// 输出填充矩形（代码底/表头底/引用竖线共用，y 为矩形顶）。
    fn fill_rect(&mut self, x_mm: f32, top_mm: f32, width_mm: f32, height_mm: f32, color: Color) {
        self.pages.last_mut().expect("至少一页").extend([
            Op::SetFillColor { col: color },
            Op::DrawRectangle {
                rectangle: Rect {
                    x: Pt(x_mm * PT_PER_MM),
                    y: Pt((PAGE_HEIGHT_MM - top_mm - height_mm) * PT_PER_MM),
                    width: Pt(width_mm * PT_PER_MM),
                    height: Pt(height_mm * PT_PER_MM),
                    mode: Some(PaintMode::Fill),
                    winding_order: None,
                },
            },
        ]);
    }

    /// 插入目录页（无标题时跳过；条目按层级缩进）。
    fn push_toc(&mut self, headings: &[(u8, String)]) {
        if headings.is_empty() {
            return;
        }
        self.gap(2.0);
        // 目录标题行用稍大字号，并与正文拉开间距。
        self.ensure_space(12.0);
        // 目录自身不计入后续标题收集（直接操作块外）。
        let ops_font = self.font_handle();
        self.text_line(MARGIN_LEFT_MM, "目录", 15.0, ink(), ops_font);
        self.gap(3.0);
        for (level, label) in headings {
            let indent = ((*level).clamp(1, 6) as f32 - 1.0) * 7.0;
            let width_mm = self.content_width_mm() - indent;
            for line in self.wrap_text(label, SMALL_PT + 1.0, width_mm) {
                let ops_font = self.font_handle();
                self.text_line(
                    MARGIN_LEFT_MM + indent,
                    &line,
                    SMALL_PT + 1.0,
                    ink(),
                    ops_font,
                );
            }
            self.gap(0.8);
        }
        self.gap(4.0);
    }

    /// 当前字体句柄（文档级注册一次，各行克隆复用）。
    fn font_handle(&self) -> PdfFontHandle {
        self.font.clone()
    }

    /// 顶层块流装配（含层级缩进）。
    fn push_blocks(&mut self, blocks: &[Block], depth: u32) {
        for block in blocks {
            self.push_block(block, depth);
        }
    }

    /// 单个块装配。
    fn push_block(&mut self, block: &Block, depth: u32) {
        let indent = (depth as f32 * 6.0).min(24.0);
        let x = MARGIN_LEFT_MM + indent;
        let width = self.content_width_mm() - indent;
        match block {
            Block::Heading { level, inline } => {
                let size = match (*level).clamp(1, 6) {
                    1 => 20.0,
                    2 => 17.0,
                    3 => 14.5,
                    4 => 12.5,
                    _ => 11.5,
                };
                let text = inline_text(inline);
                if text.trim().is_empty() {
                    return;
                }
                self.gap(if *level <= 2 { 5.0 } else { 3.5 });
                for line in self.wrap_text(&text, size, width) {
                    let font = self.font_handle();
                    self.text_line(x, &line, size, ink(), font);
                }
                self.gap(2.0);
            }
            Block::Paragraph(inline) => {
                let segments = split_inline_flow(inline);
                if !segments.iter().any(FlowSegment::has_content) {
                    self.gap(2.0);
                    return;
                }
                self.gap(1.5);
                self.push_segments(segments, x, width, None);
                self.gap(1.5);
            }
            Block::Code { code, .. } => {
                let trimmed = code.trim_end_matches('\n');
                if trimmed.is_empty() {
                    return;
                }
                self.gap(2.0);
                let line_mm = CODE_PT * MM_PER_PT + 1.6;
                for line in trimmed.split('\n') {
                    // 空行仍占底块高度，保持代码块视觉连续。
                    let display = if line.is_empty() { " " } else { line };
                    for piece in self.wrap_text(display, CODE_PT, width - 4.0) {
                        self.ensure_space(line_mm);
                        let top = self.cursor_mm;
                        self.fill_rect(x, top, width, line_mm, code_fill());
                        let font = self.font_handle();
                        self.text_line(x + 2.0, &piece, CODE_PT, ink(), font);
                        self.cursor_mm += 1.6;
                    }
                }
                self.gap(2.5);
            }
            Block::Mermaid(source) => {
                let png = crate::export::mermaid::render_svg(source)
                    .and_then(|svg| crate::export::mermaid::svg_to_png_bytes(&svg, 2.0));
                if let Some(png) = png {
                    self.gap(2.0);
                    if self.push_picture_bytes(&png, x, width) {
                        return;
                    }
                }
                // 渲染失败回退等宽源码，保证内容不丢失。
                self.push_block(
                    &Block::Code {
                        lang: Some("mermaid".to_string()),
                        code: source.clone(),
                    },
                    depth,
                );
            }
            Block::Math(math) => {
                let text = math.trim();
                if text.is_empty() {
                    return;
                }
                self.push_block(
                    &Block::Code {
                        lang: Some("math".to_string()),
                        code: text.to_string(),
                    },
                    depth,
                );
            }
            Block::Quote(inner) => {
                if inner.is_empty() {
                    return;
                }
                self.gap(2.0);
                let start_page = self.pages.len();
                let top = self.cursor_mm.max(MARGIN_TOP_MM);
                self.push_blocks(inner, depth + 1);
                let bottom = self.cursor_mm;
                // 跨页时竖线只画当前页部分，避免坐标跨页错位。
                let bar_top = if self.pages.len() != start_page {
                    MARGIN_TOP_MM
                } else {
                    top
                };
                if bottom > bar_top {
                    self.fill_rect(x - 3.0, bar_top, 1.0, bottom - bar_top, quote_bar());
                }
                self.gap(2.0);
            }
            Block::List {
                ordered,
                start,
                items,
            } => {
                self.gap(1.5);
                for (index, item) in items.iter().enumerate() {
                    let marker = list_marker(*ordered, start + index as u64, item.checked, depth);
                    let item_indent = 5.0;
                    let first = format!("{marker} ");
                    // 首行带编号/符号，其余行对齐到文本起始。
                    let mut first_line = true;
                    for block in &item.blocks {
                        match block {
                            Block::Paragraph(inline) => {
                                let segments = split_inline_flow(inline);
                                let prefix = if first_line {
                                    first_line = false;
                                    Some(first.as_str())
                                } else {
                                    None
                                };
                                let offset = item_indent;
                                self.push_segments(segments, x + offset, width - offset, prefix);
                            }
                            _ => self.push_block(block, depth + 1),
                        }
                    }
                    if first_line {
                        let font = self.font_handle();
                        self.text_line(x + item_indent, &first, BODY_PT, ink(), font);
                    }
                    self.gap(1.0);
                }
                self.gap(1.5);
            }
            Block::Table {
                headers,
                aligns,
                rows,
            } => self.push_table(headers, aligns, rows, x, width),
            Block::Rule => {
                self.gap(3.0);
                self.rule_line(x, width, grid_ink());
                self.gap(3.0);
            }
            Block::Html(raw) => {
                let text = raw.trim();
                if text.is_empty() {
                    return;
                }
                self.gap(1.5);
                for line in self.wrap_text(text, BODY_PT, width) {
                    let font = self.font_handle();
                    self.text_line(x, &line, BODY_PT, secondary_ink(), font);
                }
                self.gap(1.5);
            }
        }
    }

    /// 富文本行输出：行内拼成字符流后按宽度切行（单字体无真粗斜体，v1 统一样式）。
    fn push_rich_lines(
        &mut self,
        runs: &[(String, RunKind)],
        x_mm: f32,
        width_mm: f32,
        size_pt: f32,
        color: Color,
    ) {
        let full: String = runs.iter().map(|(text, _)| text.as_str()).collect();
        for line in self.wrap_text(&full, size_pt, width_mm) {
            let font = self.font_handle();
            self.text_line(x_mm, &line, size_pt, color.clone(), font);
        }
    }

    /// 段落内容装配：文本段走富文本行，图片段嵌图（失败回退占位文本）。
    ///
    /// `prefix` 为列表首段的编号/符号前缀，并入首个文本段。
    fn push_segments(
        &mut self,
        mut segments: Vec<FlowSegment>,
        x_mm: f32,
        width_mm: f32,
        prefix: Option<&str>,
    ) {
        if let Some(prefix) = prefix {
            let prefix_run = (prefix.to_string(), RunKind::Plain);
            match segments.first_mut() {
                Some(FlowSegment::Runs(runs)) => runs.insert(0, prefix_run),
                _ => segments.insert(0, FlowSegment::Runs(vec![prefix_run])),
            }
        }
        for segment in segments {
            match segment {
                FlowSegment::Runs(runs) => {
                    if runs.iter().all(|(text, _)| text.trim().is_empty()) {
                        continue;
                    }
                    self.push_rich_lines(&runs, x_mm, width_mm, BODY_PT, ink());
                }
                FlowSegment::Image { alt, url } => {
                    if !self.push_picture(&url, x_mm, width_mm) {
                        let font = self.font_handle();
                        self.text_line(
                            x_mm,
                            &format!("[图片：{alt}]"),
                            BODY_PT,
                            secondary_ink(),
                            font,
                        );
                    }
                }
            }
        }
    }

    /// 嵌入一张图片：按版心宽等比缩放（小图不放大、超高钳到页高六成），
    /// 解码失败或文件缺失返回 `false` 由调用方回退占位。
    fn push_picture(&mut self, url: &str, x_mm: f32, width_mm: f32) -> bool {
        let Some(bytes) = super::image::load_image_bytes(url, self.base_dir.as_deref()) else {
            return false;
        };
        self.push_picture_bytes(&bytes, x_mm, width_mm)
    }

    /// 嵌入图片字节：文档图片与 Mermaid 渲染出的 PNG 共用同一套缩放与跨页逻辑。
    fn push_picture_bytes(&mut self, bytes: &[u8], x_mm: f32, width_mm: f32) -> bool {
        let Some(decoded) = super::image::decode_image(bytes) else {
            return false;
        };
        let (px_w, px_h) = (decoded.width(), decoded.height());
        // 96dpi 屏幕像素的自然毫米尺寸（区别于正文 pt 换算的 0.3528）。
        let natural_w_mm = px_w as f32 * 0.2646;
        let natural_h_mm = px_h as f32 * 0.2646;
        let max_h_mm = (self.bottom_mm() - MARGIN_TOP_MM) * 0.6;
        let scale = (width_mm / natural_w_mm)
            .min(max_h_mm / natural_h_mm)
            .min(1.0);
        let w_mm = natural_w_mm * scale;
        let h_mm = natural_h_mm * scale;
        self.ensure_space(h_mm + 2.0);
        let top = self.cursor_mm;
        let raw = RawImage {
            pixels: RawImageData::U8(decoded.to_rgba8().into_raw()),
            width: px_w as usize,
            height: px_h as usize,
            data_format: RawImageFormat::RGBA8,
            tag: Vec::new(),
        };
        let id = self.doc.add_image(&raw);
        // dpi=72 时自动缩放把 1px 映射为 1pt，再按目标尺寸二次缩放。
        self.pages
            .last_mut()
            .expect("至少一页")
            .push(Op::UseXobject {
                id,
                transform: XObjectTransform {
                    translate_x: Some(Pt(x_mm * PT_PER_MM)),
                    translate_y: Some(Pt((PAGE_HEIGHT_MM - top - h_mm) * PT_PER_MM)),
                    dpi: Some(72.0),
                    scale_x: Some(w_mm * PT_PER_MM / px_w as f32),
                    scale_y: Some(h_mm * PT_PER_MM / px_h as f32),
                    ..Default::default()
                },
            });
        self.cursor_mm = top + h_mm + 2.0;
        true
    }

    /// 按宽度换行（返回至少一行；超长单词按字硬断）。
    fn wrap_text(&self, text: &str, size_pt: f32, width_mm: f32) -> Vec<String> {
        // 1pt ≈ 0.3528mm（与 `text_line` 的字号换算一致）。这里曾误用 0.2646
        // （那是 96dpi 下的 mm/px），把版心当成约 1.33 倍宽，正文会溢出右边距。
        let width_pt = width_mm * PT_PER_MM;
        let mut lines = Vec::new();
        // 保留原有显式换行，段内再按宽度折行。
        for paragraph in text.split('\n') {
            let mut current = String::new();
            let mut current_pt = 0.0f32;
            // 按“词（英文）/字（中文）”切分，中文字符独立成段保证任意处可断。
            for piece in split_wrap_units(paragraph) {
                let piece_pt = self.fonts.text_width_pt(&piece, size_pt);
                let breakable = current.is_empty()
                    || current.ends_with(' ')
                    || current.ends_with('\u{3000}')
                    || is_cjk_boundary(&piece);
                // 单元自身超出版心（如行首长 URL）时必须按字硬断：原先
                // `current.is_empty()` 分支会把这种超长词整段塞入且不折行，
                // 导致行首长 URL 溢出版心右缘。
                let needs_hard_break = piece_pt > width_pt
                    || (current_pt + piece_pt > width_pt && !current.is_empty() && !breakable);
                if needs_hard_break {
                    // 超长词：按字硬断。
                    for ch in piece.chars() {
                        let ch_string = ch.to_string();
                        let ch_pt = self.fonts.text_width_pt(&ch_string, size_pt);
                        if current_pt + ch_pt > width_pt && !current.is_empty() {
                            lines.push(std::mem::take(&mut current));
                            current_pt = 0.0;
                        }
                        current.push(ch);
                        current_pt += ch_pt;
                    }
                } else if current_pt + piece_pt <= width_pt || current.is_empty() {
                    current.push_str(&piece);
                    current_pt += piece_pt;
                } else if breakable {
                    lines.push(std::mem::take(&mut current));
                    // 行首空格不保留，避免缩进漂移。
                    let trimmed = piece.trim_start_matches([' ', '\u{3000}']);
                    current.push_str(trimmed);
                    current_pt = self.fonts.text_width_pt(trimmed, size_pt);
                }
            }
            lines.push(std::mem::take(&mut current));
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        lines
    }

    /// 表格装配：列宽按内容度量分配，行高按换行数取齐，超页逐行换页。
    fn push_table(
        &mut self,
        headers: &[Vec<Inline>],
        aligns: &[Align],
        rows: &[Vec<Vec<Inline>>],
        x_mm: f32,
        width_mm: f32,
    ) {
        let columns = headers.len().max(1);
        let size = SMALL_PT;
        // 列宽：先按内容最大宽度占比分配，再钳到页内。
        // `text_width_pt` 返回 pt，乘 0.3528 得 mm（曾误用 0.2646，列宽整体偏小）。
        let mut natural = vec![10.0f32; columns];
        let cell_text = |cell: &[Inline]| inline_text(cell);
        for (index, head) in headers.iter().enumerate().take(columns) {
            natural[index] = natural[index]
                .max(self.fonts.text_width_pt(&cell_text(head), size) * MM_PER_PT + 4.0);
        }
        for row in rows {
            for (index, cell) in row.iter().enumerate().take(columns) {
                // 长单元格按 30 字估算自然宽，避免单列撑爆版心。
                let sample: String = cell_text(cell).chars().take(30).collect();
                natural[index] =
                    natural[index].max(self.fonts.text_width_pt(&sample, size) * MM_PER_PT + 4.0);
            }
        }
        let total: f32 = natural.iter().sum();
        let widths: Vec<f32> = natural
            .iter()
            .map(|width| (width / total * width_mm).clamp(12.0, width_mm / 2.0))
            .collect();
        // 按比例归一，保证恰好占满版心。
        let sum: f32 = widths.iter().sum();
        let widths: Vec<f32> = widths.iter().map(|width| width / sum * width_mm).collect();

        self.gap(2.5);
        let header_cells: Vec<String> = headers.iter().map(|cell| cell_text(cell)).collect();
        self.push_table_row(&header_cells, &widths, x_mm, size, true, aligns);
        for row in rows {
            let cells: Vec<String> = (0..columns)
                .map(|index| {
                    row.get(index)
                        .map(|cell| cell_text(cell))
                        .unwrap_or_default()
                })
                .collect();
            self.push_table_row(&cells, &widths, x_mm, size, false, aligns);
        }
        self.gap(2.5);
    }

    /// 单行表格输出（含表头底色与网格线）。
    fn push_table_row(
        &mut self,
        cells: &[String],
        widths: &[f32],
        x_mm: f32,
        size: f32,
        header: bool,
        aligns: &[Align],
    ) {
        let line_mm = size * MM_PER_PT + 3.2;
        // 每列先换行，行高取最大行数。
        let wrapped: Vec<Vec<String>> = cells
            .iter()
            .zip(widths.iter())
            .map(|(cell, width)| self.wrap_text(cell, size, *width - 3.0))
            .collect();
        let height_lines = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let row_mm = line_mm * height_lines as f32;
        self.ensure_space(row_mm + 1.0);
        let top = self.cursor_mm;
        if header {
            self.fill_rect(x_mm, top, widths.iter().sum(), row_mm, head_fill());
        }
        let font = self.font_handle();
        let color = ink();
        let mut x = x_mm;
        for (index, column) in wrapped.iter().enumerate() {
            let width = widths[index];
            for (line_index, line) in column.iter().enumerate() {
                let line_top = top + line_mm * line_index as f32;
                let y = Self::pdf_y_mm(line_top + size * MM_PER_PT * 0.8 + 1.6);
                // 对齐：居中/右对齐按行宽补偏移。
                let offset = match aligns.get(index).copied().unwrap_or(Align::None) {
                    Align::Center => {
                        (width - 3.0 - self.fonts.text_width_pt(line, size) * MM_PER_PT).max(0.0)
                            / 2.0
                    }
                    Align::Right => {
                        (width - 3.0 - self.fonts.text_width_pt(line, size) * MM_PER_PT).max(0.0)
                    }
                    _ => 0.0,
                };
                self.pages.last_mut().expect("至少一页").extend([
                    Op::StartTextSection,
                    Op::SetTextCursor {
                        pos: Point::new(Mm(x + 1.5 + offset), y),
                    },
                    Op::SetFont {
                        font: font.clone(),
                        size: Pt(size),
                    },
                    Op::SetLineHeight { lh: Pt(size) },
                    Op::SetFillColor { col: color.clone() },
                    Op::ShowText {
                        items: vec![TextItem::Text(line.clone())],
                    },
                    Op::EndTextSection,
                ]);
            }
            // 列竖线。
            let y_top = Self::pdf_y_mm(top);
            let y_bottom = Self::pdf_y_mm(top + row_mm);
            self.pages.last_mut().expect("至少一页").extend([
                Op::SetOutlineColor { col: grid_ink() },
                Op::DrawLine {
                    line: Line {
                        points: vec![
                            LinePoint {
                                p: Point::new(Mm(x + width), y_top),
                                bezier: false,
                            },
                            LinePoint {
                                p: Point::new(Mm(x + width), y_bottom),
                                bezier: false,
                            },
                        ],
                        is_closed: false,
                    },
                },
            ]);
            x += width;
        }
        // 行上下横线与左框线。
        let total_width: f32 = widths.iter().sum();
        self.rule_line_at(x_mm, top, total_width);
        self.rule_line_at(x_mm, top + row_mm, total_width);
        let y_top = Self::pdf_y_mm(top);
        let y_bottom = Self::pdf_y_mm(top + row_mm);
        self.pages.last_mut().expect("至少一页").extend([
            Op::SetOutlineColor { col: grid_ink() },
            Op::DrawLine {
                line: Line {
                    points: vec![
                        LinePoint {
                            p: Point::new(Mm(x_mm), y_top),
                            bezier: false,
                        },
                        LinePoint {
                            p: Point::new(Mm(x_mm), y_bottom),
                            bezier: false,
                        },
                    ],
                    is_closed: false,
                },
            },
        ]);
        self.cursor_mm = top + row_mm;
    }

    /// 在指定纵位置画表格横线（不推进光标）。
    fn rule_line_at(&mut self, x_mm: f32, top_mm: f32, width_mm: f32) {
        let y = Self::pdf_y_mm(top_mm);
        self.pages.last_mut().expect("至少一页").extend([
            Op::SetOutlineColor { col: grid_ink() },
            Op::DrawLine {
                line: Line {
                    points: vec![
                        LinePoint {
                            p: Point::new(Mm(x_mm), y),
                            bezier: false,
                        },
                        LinePoint {
                            p: Point::new(Mm(x_mm + width_mm), y),
                            bezier: false,
                        },
                    ],
                    is_closed: false,
                },
            },
        ]);
    }

    /// 落盘：逐页装配后保存（字体子集化由保存选项自动完成）。
    fn finish(self) -> anyhow::Result<Vec<u8>> {
        let pages: Vec<PdfPage> = self
            .pages
            .into_iter()
            .map(|ops| PdfPage::new(Mm(PAGE_WIDTH_MM), Mm(PAGE_HEIGHT_MM), ops))
            .collect();
        if pages.is_empty() {
            return Err(anyhow!("文档为空，无法生成 PDF"));
        }
        Ok(self
            .doc
            .with_pages(pages)
            .save(&PdfSaveOptions::default(), &mut Vec::new()))
    }
}

/// 行内展平：嵌套样式拍平为（文本，种类）序列（PDF 单字体无真粗斜体，种类仅保留代码底色区分）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Plain,
    Code,
}

/// 段落内容片段：连续文本游程或一张图片（图片打断文本流独立成段）。
enum FlowSegment {
    Runs(Vec<(String, RunKind)>),
    Image { alt: String, url: String },
}

impl FlowSegment {
    /// 是否有可见内容（空白文本段跳过，图片段恒有内容）。
    fn has_content(&self) -> bool {
        match self {
            Self::Runs(runs) => runs.iter().any(|(text, _)| !text.trim().is_empty()),
            Self::Image { .. } => true,
        }
    }
}

/// 行内树切分为文本/图片片段序列（样式嵌套拍平，图片保留目标地址）。
fn split_inline_flow(inline: &[Inline]) -> Vec<FlowSegment> {
    fn walk(
        inline: &[Inline],
        parent: RunKind,
        runs: &mut Vec<(String, RunKind)>,
        segments: &mut Vec<FlowSegment>,
    ) {
        for node in inline {
            match node {
                Inline::Text(text) => push_run(runs, text.clone(), parent),
                Inline::Strong(inner) | Inline::Emphasis(inner) | Inline::Strike(inner) => {
                    walk(inner, parent, runs, segments);
                }
                Inline::Code(code) => push_run(runs, code.clone(), RunKind::Code),
                Inline::Math(math) => push_run(runs, math.clone(), RunKind::Code),
                Inline::Link { label, .. } => push_run(runs, label.clone(), parent),
                Inline::Image { alt, url } => {
                    if !runs.is_empty() {
                        segments.push(FlowSegment::Runs(std::mem::take(runs)));
                    }
                    segments.push(FlowSegment::Image {
                        alt: alt.clone(),
                        url: url.clone(),
                    });
                }
                Inline::FootnoteRef(label) => push_run(runs, format!("[^{label}]"), parent),
                Inline::Break => push_run(runs, "\n".to_string(), parent),
            }
        }
    }

    let mut segments = Vec::new();
    let mut runs = Vec::new();
    walk(inline, RunKind::Plain, &mut runs, &mut segments);
    if !runs.is_empty() {
        segments.push(FlowSegment::Runs(runs));
    }
    segments
}

/// 合并相邻同类游程，避免碎片化换行计算。
fn push_run(runs: &mut Vec<(String, RunKind)>, text: String, kind: RunKind) {
    if text.is_empty() {
        return;
    }
    if let Some((last, last_kind)) = runs.last_mut()
        && *last_kind == kind
    {
        last.push_str(&text);
        return;
    }
    runs.push((text, kind));
}

/// 列表标记：有序用序号，无序按层级轮换符号，任务列表用 ASCII 勾选框（缺字安全）。
fn list_marker(ordered: bool, number: u64, checked: Option<bool>, depth: u32) -> String {
    if let Some(checked) = checked {
        return if checked {
            "[x]".to_string()
        } else {
            "[ ]".to_string()
        };
    }
    if ordered {
        return format!("{number}.");
    }
    match depth % 3 {
        0 => "•".to_string(),
        1 => "◦".to_string(),
        _ => "▪".to_string(),
    }
}

/// 换行切分单元：中文逐字、英文数字按词、空格独立成段。
fn split_wrap_units(text: &str) -> Vec<String> {
    let mut units = Vec::new();
    let mut word = String::new();
    for ch in text.chars() {
        // 空白与中文字符独立成段：前者保留为空白段，后者保证任意处可断。
        if ch == ' ' || ch == '\u{3000}' || ch == '\t' || is_cjk_char(ch) {
            if !word.is_empty() {
                units.push(std::mem::take(&mut word));
            }
            units.push(ch.to_string());
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        units.push(word);
    }
    units
}

/// 是否中文字符（含全角标点与假名）。
fn is_cjk_char(ch: char) -> bool {
    matches!(ch,
        '\u{2E80}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{20000}'..='\u{2A6DF}'
        | '\u{FF00}'..='\u{FFEF}')
}

/// 切分单元是否适合作为断行点（中文字符或空白起首）。
fn is_cjk_boundary(piece: &str) -> bool {
    piece.chars().next().is_some_and(is_cjk_char)
}

#[cfg(test)]
mod tests {
    use super::{
        CJK_FONT_PARSES, Fonts, Layout, MM_PER_PT, PT_PER_MM, contains_cjk, is_cjk_char,
        list_marker, render_pdf_bytes, split_wrap_units,
    };
    use crate::export::{ExportOptions, parse_blocks};
    use printpdf::{BuiltinFont, PdfFontHandle};
    use std::sync::Arc;

    /// 测试选项构造。
    fn test_options() -> ExportOptions {
        ExportOptions {
            title: "测试文档".to_string(),
            include_toc: true,
            base_dir: None,
        }
    }

    #[test]
    fn pdf_bytes_start_with_pdf_header() {
        let bytes = render_pdf_bytes("# 你好\n\n世界 Hello\n", &test_options()).expect("渲染成功");
        assert!(bytes.starts_with(b"%PDF"), "前八字节：{:?}", &bytes[..8]);
        assert!(bytes.len() > 1000, "含子集字体应大于 1KB：{}", bytes.len());
    }

    #[test]
    fn cjk_font_is_parsed_once_across_exports() {
        use std::sync::atomic::Ordering;

        // 先导出一份含中文的文档把解析结果写进进程级缓存（别的测试也可能已预热）。
        render_pdf_bytes("# 预热\n\n中文段落。\n", &test_options()).expect("预热导出成功");

        // 缓存已预热后再统计：后续导出不应新增任何探测/解析。
        let before = CJK_FONT_PARSES.load(Ordering::Relaxed);
        render_pdf_bytes("# 第一次\n\n中文段落。\n", &test_options()).expect("首次导出成功");
        render_pdf_bytes("# 第二次\n\n中文段落。\n", &test_options()).expect("再次导出成功");
        assert_eq!(
            CJK_FONT_PARSES.load(Ordering::Relaxed) - before,
            0,
            "缓存预热后连续导出不应重复解析字体"
        );

        // 两次导出拿到同一份解析结果（Arc 指向同一分配）。
        let first = Fonts::load("中文").expect("字体可用");
        let second = Fonts::load("中文").expect("字体可用");
        if let (Some(a), Some(b)) = (first.parsed, second.parsed) {
            assert!(Arc::ptr_eq(&a, &b), "应共享同一份字体解析结果");
        }
    }

    #[test]
    fn pdf_renders_full_document_structure() {
        let markdown = "# 一级\n\n## 二级\n\n正文**加粗**与`代码`。\n\n- 甲\n- [x] 乙\n\n1. 有序一\n2. 有序二\n\n> 引用块\n\n| 甲 | 乙 |\n| --- | --- |\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n\n---\n\n$$x^2$$\n";
        let bytes = render_pdf_bytes(markdown, &test_options()).expect("渲染成功");
        assert!(bytes.starts_with(b"%PDF"));
        // 子集化后仍应保持小体积（全量嵌入约数 MB）。
        assert!(bytes.len() < 600_000, "子集化失效？体积：{}", bytes.len());
    }

    #[test]
    fn pdf_without_toc_and_ascii_only_renders() {
        let options = ExportOptions {
            title: "plain".to_string(),
            include_toc: false,
            base_dir: None,
        };
        let bytes = render_pdf_bytes("Hello *world*.\n\n- a\n- b\n", &options).expect("渲染成功");
        assert!(bytes.starts_with(b"%PDF"));
    }

    #[test]
    fn cjk_detection_covers_punctuation_and_kana() {
        assert!(contains_cjk("，。！"));
        assert!(contains_cjk("あア"));
        assert!(contains_cjk("中文"));
        assert!(!contains_cjk("Hello, world! 123"));
        assert!(is_cjk_char('中'));
        assert!(!is_cjk_char('A'));
    }

    #[test]
    fn wrap_units_split_cjk_by_char_and_latin_by_word() {
        assert_eq!(
            split_wrap_units("中文ab cd"),
            vec!["中", "文", "ab", " ", "cd"]
        );
        assert_eq!(split_wrap_units("a  b"), vec!["a", " ", " ", "b"]);
    }

    #[test]
    fn wrap_text_uses_point_based_content_width() {
        // 用确定性的假字体（parsed=None，每个字宽 = size_pt）验证换行是按 pt
        // 换算版心宽度。此处曾误用 0.2646（96dpi 的 mm/px），每行会多容纳约
        // 1.33 倍字符，导致正文溢出右边距。
        let fonts = Fonts {
            parsed: None,
            units_per_em: 1000.0,
            space_width_units: 1000.0,
        };
        let mut doc = printpdf::PdfDocument::new("wrap-test");
        let layout = Layout::new(
            &fonts,
            PdfFontHandle::Builtin(BuiltinFont::Helvetica),
            &mut doc,
            None,
        );
        let lines = layout.wrap_text(&"中".repeat(30), 10.0, 25.0);
        // 25mm × 2.8346 ≈ 70.87pt，单字 10pt → 每行 7 字。
        assert_eq!(lines[0].chars().count(), 7, "首行字数错误：{lines:?}");
        assert!(lines.iter().all(|line| line.chars().count() <= 7));
    }

    #[test]
    fn wrap_text_breaks_leading_long_url_within_content_width() {
        // 行首超长 URL（单个无空格词）必须按字硬断，不能整段塞入导致溢出版心。
        let fonts = Fonts {
            parsed: None,
            units_per_em: 1000.0,
            space_width_units: 1000.0,
        };
        let mut doc = printpdf::PdfDocument::new("wrap-url-test");
        let layout = Layout::new(
            &fonts,
            PdfFontHandle::Builtin(BuiltinFont::Helvetica),
            &mut doc,
            None,
        );
        let url = format!("https://example.com/{}", "a".repeat(40));
        let lines = layout.wrap_text(&url, 10.0, 25.0);
        // 假字体每字 10pt、版心约 70.87pt → 每行最多 7 字符。
        assert!(lines.len() > 1, "长 URL 应折行：{lines:?}");
        assert!(
            lines.iter().all(|line| line.chars().count() <= 7),
            "每行不得超出版心：{lines:?}"
        );
        assert_eq!(lines.concat(), url, "折行不应丢字符");
    }

    #[test]
    fn millimetre_point_constants_match_72pt_per_inch() {
        // 1 英寸 = 25.4mm = 72pt；锁定常量，防止再次混入 0.2646。
        assert!((25.4 / MM_PER_PT - 72.0).abs() < 0.01);
        assert!((72.0 / PT_PER_MM - 25.4).abs() < 0.01);
    }

    #[test]
    fn list_markers_match_depth_and_task_state() {
        assert_eq!(list_marker(true, 3, None, 0), "3.");
        assert_eq!(list_marker(false, 1, None, 0), "•");
        assert_eq!(list_marker(false, 1, None, 1), "◦");
        assert_eq!(list_marker(false, 1, Some(true), 0), "[x]");
        assert_eq!(list_marker(false, 1, Some(false), 0), "[ ]");
    }

    #[test]
    fn pdf_embeds_local_image_as_xobject() {
        let root = std::env::temp_dir().join(format!("rumark-pdf-img-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("temp dir");
        let fixture = image::RgbaImage::from_pixel(6, 4, image::Rgba([30, 120, 200, 255]));
        fixture
            .save(root.join("pic.png"))
            .expect("write fixture png");

        let options = ExportOptions {
            title: "嵌图".to_string(),
            include_toc: false,
            base_dir: Some(root.clone()),
        };
        let markdown = "前文\n\n![图](./pic.png)\n\n后文\n\n- 列表项\n\n  ![列表图](./pic.png)\n";
        let bytes = render_pdf_bytes(markdown, &options).expect("渲染成功");
        assert!(bytes.starts_with(b"%PDF"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Subtype/Image"), "应含图片 XObject");
        assert!(text.contains("/XObject"), "应注册 XObject 资源");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pdf_renders_mermaid_as_image_not_source() {
        let options = ExportOptions {
            title: "mermaid".to_string(),
            include_toc: false,
            base_dir: None,
        };
        let bytes = render_pdf_bytes("```mermaid\nflowchart TD\n  A --> B\n```\n", &options)
            .expect("渲染成功");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Subtype/Image"), "应含图片 XObject");
        assert!(!text.contains("flowchart TD"), "源码不应写进 PDF：{text}");
    }

    #[test]
    fn pdf_missing_image_falls_back_without_error() {
        let bytes = render_pdf_bytes("![缺图](./missing.png)", &test_options()).expect("渲染成功");
        assert!(bytes.starts_with(b"%PDF"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("/Subtype/Image"), "缺图不应产生图片对象");
    }

    #[test]
    fn pdf_renders_repo_regression_fixture() {
        // docs/render-sample.md 为手动渲染回归夹具（含 CommonMark + GFM 边界用例），
        // 导出同样全量走一遍，保证解析器无 panic、无内容丢失级错误。
        let markdown = include_str!("../../docs/render-sample.md");
        let bytes = render_pdf_bytes(markdown, &test_options()).expect("渲染成功");
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes.len() < 2_000_000, "子集化失效？体积：{}", bytes.len());
    }

    #[test]
    fn font_measures_cjk_wider_than_latin() {
        let fonts = Fonts::load("中文测试 Hello").expect("系统或内嵌字体可用");
        let cjk = fonts.text_width_pt("中", 10.0);
        let latin = fonts.text_width_pt("A", 10.0);
        assert!(cjk > 0.0 && latin > 0.0);
        assert!(cjk > latin, "中文宽度 {cjk} 应大于英文 {latin}");
        // 换行在窄版心下必须切分中文长串。
        let blocks = parse_blocks("中".repeat(200).as_str());
        assert!(!blocks.is_empty());
    }
}
