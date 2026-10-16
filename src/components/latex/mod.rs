//! LaTeX 展示公式解析与 RaTeX SVG 渲染辅助。

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use anyhow::{Context as _, anyhow};
use directories::ProjectDirs;
use rgpui::{Hsla, Rgba};

const DISPLAY_MATH_SCALE: f32 = 1.25;
const INLINE_MATH_SCALE: f32 = 1.12;

/// 从 Markdown 保留的已解析展示公式源码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DisplayMathExpression {
    /// 含 `$$` 分隔符的完整 Markdown 源码。
    pub(crate) raw: String,
    /// 展示分隔符之间的 LaTeX 正文。
    pub(crate) body: String,
}

/// 展示公式渲染为 SVG 缓存文件的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LatexRender {
    /// 供 GPUI 图片元素消费的 SVG 文件路径。
    pub(crate) path: PathBuf,
    /// SVG 文档内容，供导出路径使用。
    pub(crate) svg: String,
}

/// 把原始 `$$...$$` Markdown 块解析为其中包含的 LaTeX 正文。
pub(crate) fn parse_display_math_source(raw: &str) -> Option<DisplayMathExpression> {
    let raw = raw.trim_matches('\n').to_string();
    let lines = raw.split('\n').collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }

    if lines.len() == 1 {
        let line = strip_display_indent(lines[0])?.trim_end();
        let body_and_close = line.strip_prefix("$$")?;
        let close = body_and_close.find("$$")?;
        let body = body_and_close[..close].trim().to_string();
        return Some(DisplayMathExpression { raw, body });
    }

    let opener = strip_display_indent(lines[0])?.trim_end();
    let closer = lines.last()?.trim();
    if opener != "$$" || closer != "$$" {
        return None;
    }

    let body = lines[1..lines.len() - 1].join("\n");
    Some(DisplayMathExpression { raw, body })
}

/// 用于渲染展示公式块的展示字号。
pub(crate) fn display_math_font_size(base_font_size: f32) -> f32 {
    base_font_size * DISPLAY_MATH_SCALE
}

/// 用于渲染行内公式的展示字号。
pub(crate) fn inline_math_font_size(base_font_size: f32) -> f32 {
    base_font_size * INLINE_MATH_SCALE
}

/// 把展示公式源码渲染为缓存 SVG 文件。
pub(crate) fn render_display_math_svg(
    source: &DisplayMathExpression,
    text_color: Hsla,
    font_size: f32,
) -> anyhow::Result<LatexRender> {
    render_latex_svg_to_cache(&source.body, text_color, font_size)
}

/// 把行内 LaTeX 正文渲染为缓存 SVG 文件。
pub(crate) fn render_inline_math_svg(
    latex: &str,
    text_color: Hsla,
    font_size: f32,
) -> anyhow::Result<LatexRender> {
    render_latex_svg_to_cache(latex, text_color, font_size)
}

/// 渲染 LaTeX 并写入 SVG 缓存。
fn render_latex_svg_to_cache(
    latex: &str,
    text_color: Hsla,
    font_size: f32,
) -> anyhow::Result<LatexRender> {
    let svg = render_latex_to_svg(latex, text_color, font_size)?;
    let key = latex_cache_key(latex, text_color, font_size);
    let path = latex_cache_dir()?.join(format!("{key}.svg"));
    if !path.exists() {
        crate::atomic_file::write_text(&path, &svg)
            .with_context(|| format!("failed to write LaTeX SVG cache '{}'", path.display()))?;
    }
    Ok(LatexRender { path, svg })
}

/// 渲染 LaTeX 表达式为自包含 SVG 文本。
pub(crate) fn render_latex_to_svg(
    latex: &str,
    text_color: Hsla,
    font_size: f32,
) -> anyhow::Result<String> {
    let parsed = ratex_parser::parse(latex).map_err(|err| anyhow!("{err}"))?;
    let layout = ratex_layout::layout(&parsed, &ratex_layout::LayoutOptions::default());
    let display_list = ratex_layout::to_display_list(&layout);
    let mut svg = ratex_svg::render_to_svg(
        &display_list,
        &ratex_svg::SvgOptions {
            font_size: f64::from(font_size.max(1.0)),
            padding: f64::from((font_size * 0.35).max(4.0)),
            embed_glyphs: true,
            ..ratex_svg::SvgOptions::default()
        },
    );
    svg = recolor_default_black(&svg, &svg_color(text_color));
    Ok(svg)
}

/// 公式内容与视觉参数的稳定缓存键。
pub(crate) fn latex_cache_key(latex: &str, text_color: Hsla, font_size: f32) -> String {
    let mut hasher = DefaultHasher::new();
    latex.hash(&mut hasher);
    svg_color(text_color).hash(&mut hasher);
    font_size.to_bits().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 去掉展示公式行首缩进。
fn strip_display_indent(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    (indent <= 3).then_some(&line[indent..])
}

/// 取 LaTeX SVG 缓存目录。
fn latex_cache_dir() -> anyhow::Result<PathBuf> {
    let root = ProjectDirs::from("rs", "launcher", "Rumark")
        .map(|dirs| dirs.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("Rumark"));
    let dir = root.join("latex-svg");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create LaTeX SVG cache '{}'", dir.display()))?;
    Ok(dir)
}

/// 把主题颜色转为 SVG 颜色串。
fn svg_color(color: Hsla) -> String {
    let color = Rgba::from(color);
    format!(
        "rgba({},{},{},{})",
        color_channel(color.r),
        color_channel(color.g),
        color_channel(color.b),
        trim_float(f64::from(color.a.clamp(0.0, 1.0)))
    )
}

/// 取颜色通道字节值。
fn color_channel(channel: f32) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// 精简浮点数字符串。
fn trim_float(value: f64) -> String {
    let formatted = format!("{value:.3}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// 把默认黑色重着为主题色。
fn recolor_default_black(svg: &str, color: &str) -> String {
    svg.replace("rgba(0,0,0,1)", color)
        .replace("rgba(0, 0, 0, 1)", color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgpui::rgba;

    #[test]
    fn parses_single_line_display_math() {
        let parsed = parse_display_math_source("$$x^2$$").expect("display math");
        assert_eq!(parsed.body, "x^2");
        assert_eq!(parsed.raw, "$$x^2$$");
    }

    #[test]
    fn parses_multiline_display_math() {
        let parsed = parse_display_math_source("$$\n\\int_0^1 x^2 dx\n$$").expect("display math");
        assert_eq!(parsed.body, "\\int_0^1 x^2 dx");
    }

    #[test]
    fn rejects_unclosed_display_math() {
        assert!(parse_display_math_source("$$\n\\frac{1}{2}").is_none());
    }

    #[test]
    fn cache_key_changes_with_theme_inputs() {
        let first = latex_cache_key("\\frac{1}{2}", Hsla::from(rgba(0xffffffff)), 18.0);
        let second = latex_cache_key("\\frac{1}{2}", Hsla::from(rgba(0x000000ff)), 18.0);
        assert_ne!(first, second);
    }

    #[test]
    fn display_math_font_size_scales_base_text_size() {
        assert_eq!(display_math_font_size(20.0), 25.0);
    }

    #[test]
    fn inline_math_font_size_scales_base_text_size() {
        assert!((inline_math_font_size(20.0) - 22.4).abs() < 0.001);
    }

    #[test]
    fn renders_basic_formula_svg() {
        let svg =
            render_latex_to_svg("\\frac{1}{2}", Hsla::from(rgba(0xffffffff)), 18.0).expect("svg");
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn invalid_latex_returns_error() {
        assert!(render_latex_to_svg("\\frac{a}", Hsla::from(rgba(0xffffffff)), 18.0).is_err());
    }
}
