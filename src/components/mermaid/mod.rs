//! Mermaid 围栏块解析与 SVG 渲染辅助。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::anyhow;

use crate::components::svg_cache::{load_svg, svg_cache_dir};

const MERMAID_SIMPLE_LINE_LIMIT: usize = 8;
const MERMAID_COMPLEX_TARGET_WIDTH_RATIO: f32 = 0.9;
const MERMAID_MAX_VIEWPORT_WIDTH_RATIO: f32 = 1.15;
const MERMAID_SCALE_PER_EXTRA_LINE: f32 = 0.035;
const MERMAID_MAX_SCALE: f32 = 1.75;

/// Mermaid 围栏代码块的起始围栏元数据。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MermaidFenceSpec {
    /// 围栏标记，反引号或波浪线。
    pub(crate) marker: char,
    /// 起始围栏长度。
    pub(crate) len: usize,
}

/// 从 Markdown 保留的已解析 Mermaid 源码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MermaidSource {
    /// 含起止围栏的完整 Markdown 源码。
    pub(crate) raw: String,
    /// 围栏之间的 Mermaid 图表源码。
    pub(crate) body: String,
    /// 起始围栏后的完整信息串。
    pub(crate) info: String,
}

/// Mermaid 图表渲染为 SVG 缓存文件的结果。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MermaidRender {
    /// 供 GPUI 图片元素消费的 SVG 文件路径。
    pub(crate) path: PathBuf,
    /// SVG 文档内容，供导出路径使用。
    pub(crate) svg: String,
    /// 缓存 SVG 中编码的具体展示宽度。
    pub(crate) display_width: f32,
    /// 缓存 SVG 中编码的具体展示高度。
    pub(crate) display_height: f32,
    /// 相对渲染器固有尺寸的编辑器展示缩放。
    pub(crate) display_scale: f32,
}

/// 展示 SVG 中编码的具体尺寸。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MermaidSize {
    pub(crate) width: f32,
    pub(crate) height: f32,
}

/// 判断围栏代码信息串是否声明 Mermaid 内容。
pub(crate) fn is_mermaid_info_string(info: Option<&str>) -> bool {
    info.and_then(|info| info.split_whitespace().next())
        .is_some_and(|first| {
            first.eq_ignore_ascii_case("mermaid") || first.eq_ignore_ascii_case("mmd")
        })
}

/// 把一行解析为 Mermaid 起始围栏。
pub(crate) fn parse_mermaid_fence_start(line: &str) -> Option<MermaidFenceSpec> {
    let trimmed = strip_fence_indent(line)?.trim_end();
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }

    let len = trimmed.chars().take_while(|ch| *ch == marker).count();
    if len < 3 {
        return None;
    }

    let info = trimmed[marker.len_utf8() * len..].trim();
    if marker == '`' && info.contains('`') {
        return None;
    }

    is_mermaid_info_string((!info.is_empty()).then_some(info))
        .then_some(MermaidFenceSpec { marker, len })
}

/// 判断一行是否为给定 Mermaid 围栏的结束围栏。
pub(crate) fn is_mermaid_closing_fence(line: &str, fence: MermaidFenceSpec) -> bool {
    let Some(trimmed) = strip_fence_indent(line).map(str::trim_end) else {
        return false;
    };
    if !trimmed.starts_with(fence.marker) {
        return false;
    }

    let len = trimmed.chars().take_while(|ch| *ch == fence.marker).count();
    len >= fence.len && trimmed[fence.marker.len_utf8() * len..].trim().is_empty()
}

/// 把原始围栏 Markdown 解析为其中包含的 Mermaid 图表源码。
pub(crate) fn parse_mermaid_fence_source(raw: &str) -> Option<MermaidSource> {
    let raw = raw.trim_matches('\n').to_string();
    let lines = raw.split('\n').collect::<Vec<_>>();
    if lines.len() < 2 {
        return None;
    }

    let opening = strip_fence_indent(lines[0])?.trim_end();
    let fence = parse_mermaid_fence_start(opening)?;
    let info = opening[fence.marker.len_utf8() * fence.len..]
        .trim()
        .to_string();
    if !is_mermaid_closing_fence(lines.last()?, fence) {
        return None;
    }

    let body = lines[1..lines.len() - 1].join("\n");
    Some(MermaidSource { raw, body, info })
}

/// 把 Mermaid 源码渲染为按编辑器展示尺寸的缓存 SVG。
pub(crate) fn render_mermaid_svg_for_display(
    source: &MermaidSource,
    available_width: f32,
    viewport_width: f32,
) -> anyhow::Result<MermaidRender> {
    render_mermaid_svg_for_display_with(source, available_width, viewport_width, render_mermaid_raw)
}

/// 用指定渲染器渲染展示 SVG。
fn render_mermaid_svg_for_display_with(
    source: &MermaidSource,
    available_width: f32,
    viewport_width: f32,
    renderer: MermaidRenderer,
) -> anyhow::Result<MermaidRender> {
    let base_key = mermaid_cache_key(&source.body);
    let base_path = mermaid_base_cache_path(&base_key)?;
    let base_svg = render_mermaid_to_svg_cached_with(&source.body, &base_path, renderer)?;
    let intrinsic = mermaid_svg_intrinsic_size(&base_svg)?;
    let scale = mermaid_display_scale(
        &source.body,
        intrinsic.width,
        intrinsic.height,
        available_width,
        viewport_width,
    );

    let display_key = mermaid_display_cache_key(&source.body, scale);
    let display_path = mermaid_display_cache_path(&display_key)?;
    let svg = load_svg(&display_path, || {
        let (scaled, _) = scale_mermaid_svg_for_display(&base_svg, scale)?;
        Ok(scaled)
    })?;
    let size = mermaid_svg_intrinsic_size(&svg)?;
    Ok(MermaidRender {
        path: display_path,
        svg,
        display_width: size.width,
        display_height: size.height,
        display_scale: scale,
    })
}

type MermaidRenderer = fn(&str) -> anyhow::Result<String>;

/// 带缓存地把源码渲染为基础 SVG。
fn render_mermaid_to_svg_cached_with(
    source: &str,
    path: &Path,
    renderer: MermaidRenderer,
) -> anyhow::Result<String> {
    load_svg(path, || renderer(source))
}

/// 调用底层渲染器渲染原始 SVG。
fn render_mermaid_raw(source: &str) -> anyhow::Result<String> {
    if !looks_like_supported_mermaid_source(source) {
        return Err(anyhow::anyhow!("unsupported Mermaid diagram"));
    }
    let svg = mermaid_rs_renderer::render(source).map_err(|err| anyhow::anyhow!("{err}"))?;
    if svg.contains("class=\"error-text\"") || svg.contains("Syntax error in text") {
        return Err(anyhow::anyhow!("Mermaid syntax error"));
    }
    Ok(svg)
}

/// Mermaid 内容的稳定缓存键。
pub(crate) fn mermaid_cache_key(source: &str) -> String {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 编辑器展示 SVG 内容与缩放的稳定缓存键。
pub(crate) fn mermaid_display_cache_key(source: &str, scale: f32) -> String {
    let mut hasher = DefaultHasher::new();
    mermaid_cache_key(source).hash(&mut hasher);
    // 量化到 0.01：连续 resize 的亚像素抖动不应无限生成缓存文件。
    let quantized_scale = (scale.max(0.1) * 100.0).round() as u64;
    quantized_scale.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 统计对渲染复杂度有实质贡献的图表行数。
pub(crate) fn semantic_mermaid_line_count(source: &str) -> usize {
    let mut in_frontmatter = false;
    source
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return false;
            }
            if trimmed == "---" {
                in_frontmatter = !in_frontmatter;
                return false;
            }
            !(in_frontmatter || trimmed.starts_with("%%"))
        })
        .count()
}

/// 编辑器渲染 Mermaid 图表所用的展示缩放。
pub(crate) fn mermaid_display_scale(
    source: &str,
    intrinsic_width: f32,
    intrinsic_height: f32,
    available_width: f32,
    viewport_width: f32,
) -> f32 {
    let line_count = semantic_mermaid_line_count(source);
    if line_count <= MERMAID_SIMPLE_LINE_LIMIT {
        return 1.0;
    }

    let intrinsic_width = intrinsic_width.max(1.0);
    let intrinsic_height = intrinsic_height.max(1.0);
    let available_width = available_width.max(1.0);
    let viewport_width = viewport_width.max(available_width);
    let extra_lines = line_count.saturating_sub(MERMAID_SIMPLE_LINE_LIMIT) as f32;

    let complexity_scale =
        (1.0 + extra_lines * MERMAID_SCALE_PER_EXTRA_LINE).clamp(1.0, MERMAID_MAX_SCALE);
    let target_column_width = available_width * MERMAID_COMPLEX_TARGET_WIDTH_RATIO;
    let column_fit_scale = if intrinsic_width < target_column_width {
        target_column_width / intrinsic_width
    } else {
        1.0
    };
    let viewport_limit_scale =
        (viewport_width * MERMAID_MAX_VIEWPORT_WIDTH_RATIO / intrinsic_width).max(1.0);
    let height_sanity_scale =
        (viewport_width * MERMAID_MAX_VIEWPORT_WIDTH_RATIO / intrinsic_height).max(1.0);

    let upper_scale = MERMAID_MAX_SCALE
        .min(viewport_limit_scale)
        .min(height_sanity_scale);
    complexity_scale
        .max(column_fit_scale)
        .clamp(1.0, upper_scale)
}

/// 去掉围栏行首缩进。
fn strip_fence_indent(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    (indent <= 3).then_some(&line[indent..])
}

/// 重写根 SVG 元素，使 GPUI 感知预期的固有尺寸。
pub(crate) fn scale_mermaid_svg_for_display(
    svg: &str,
    scale: f32,
) -> anyhow::Result<(String, MermaidSize)> {
    let scale = scale.max(0.1);
    let (start, end) = svg_root_tag_range(svg)?;
    let root_tag = &svg[start..end];
    let base_size = svg_root_size(root_tag)?;
    let size = MermaidSize {
        width: (base_size.width * scale).max(1.0),
        height: (base_size.height * scale).max(1.0),
    };
    let rewritten_root = rewrite_svg_root_tag(root_tag, size)?;
    let mut rewritten = String::with_capacity(svg.len() + 48);
    rewritten.push_str(&svg[..start]);
    rewritten.push_str(&rewritten_root);
    rewritten.push_str(&svg[end..]);
    Ok((rewritten, size))
}

/// 取 SVG 固有尺寸。
fn mermaid_svg_intrinsic_size(svg: &str) -> anyhow::Result<MermaidSize> {
    let (start, end) = svg_root_tag_range(svg)?;
    svg_root_size(&svg[start..end])
}

/// 取 SVG 根标签范围。
fn svg_root_tag_range(svg: &str) -> anyhow::Result<(usize, usize)> {
    let start = svg
        .find("<svg")
        .ok_or_else(|| anyhow!("Mermaid renderer output did not contain an SVG root"))?;
    let bytes = svg.as_bytes();
    let mut quote = None;
    let mut index = start;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            }
        } else if byte == b'"' || byte == b'\'' {
            quote = Some(byte);
        } else if byte == b'>' {
            return Ok((start, index + 1));
        }
        index += 1;
    }
    Err(anyhow!(
        "Mermaid renderer output had an unterminated SVG root tag"
    ))
}

/// 从根标签解析尺寸。
fn svg_root_size(root_tag: &str) -> anyhow::Result<MermaidSize> {
    if let Some(view_box) = svg_root_attr(root_tag, "viewBox")
        && let Some(size) = parse_view_box_size(&view_box)
    {
        return Ok(size);
    }

    let width = svg_root_attr(root_tag, "width")
        .and_then(|value| parse_svg_length(&value))
        .ok_or_else(|| anyhow!("Mermaid SVG root did not expose a usable width"))?;
    let height = svg_root_attr(root_tag, "height")
        .and_then(|value| parse_svg_length(&value))
        .ok_or_else(|| anyhow!("Mermaid SVG root did not expose a usable height"))?;
    Ok(MermaidSize { width, height })
}

/// 解析视图框尺寸。
fn parse_view_box_size(view_box: &str) -> Option<MermaidSize> {
    let values = view_box
        .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
        .filter(|part| !part.is_empty())
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (values.len() == 4 && values[2].is_finite() && values[3].is_finite()).then_some(MermaidSize {
        width: values[2].max(1.0),
        height: values[3].max(1.0),
    })
}

/// 解析 SVG 长度值。
fn parse_svg_length(value: &str) -> Option<f32> {
    let value = value.trim();
    let end = value
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+' | 'e' | 'E'))
        .map(|(index, ch)| index + ch.len_utf8())
        .last()?;
    let parsed = value[..end].parse::<f32>().ok()?;
    (parsed.is_finite() && parsed > 0.0).then_some(parsed)
}

/// 取根标签属性值。
fn svg_root_attr(root_tag: &str, attr_name: &str) -> Option<String> {
    svg_root_attrs(root_tag)
        .into_iter()
        .find(|attr| attr.name.eq_ignore_ascii_case(attr_name))
        .and_then(|attr| attr.value)
}

/// 重写 SVG 根标签尺寸。
fn rewrite_svg_root_tag(root_tag: &str, size: MermaidSize) -> anyhow::Result<String> {
    let attrs = svg_root_attrs(root_tag)
        .into_iter()
        .filter(|attr| {
            !["width", "height", "style"]
                .iter()
                .any(|name| attr.name.eq_ignore_ascii_case(name))
        })
        .map(|attr| attr.raw)
        .collect::<Vec<_>>();

    let mut rewritten = String::from("<svg");
    for attr in attrs {
        rewritten.push(' ');
        rewritten.push_str(attr.trim());
    }
    rewritten.push_str(&format!(
        " width=\"{:.3}\" height=\"{:.3}\">",
        size.width, size.height
    ));
    Ok(rewritten)
}

#[derive(Debug)]
struct SvgRootAttr {
    name: String,
    value: Option<String>,
    raw: String,
}

/// 解析根标签属性列表。
fn svg_root_attrs(root_tag: &str) -> Vec<SvgRootAttr> {
    let Some(mut index) = root_tag.find("<svg").map(|index| index + "<svg".len()) else {
        return Vec::new();
    };
    let end = root_tag.rfind('>').unwrap_or(root_tag.len());
    let bytes = root_tag.as_bytes();
    let mut attrs = Vec::new();

    while index < end {
        while index < end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= end || bytes[index] == b'/' {
            break;
        }

        let attr_start = index;
        while index < end
            && !bytes[index].is_ascii_whitespace()
            && bytes[index] != b'='
            && bytes[index] != b'/'
        {
            index += 1;
        }
        let name = root_tag[attr_start..index].to_string();
        if name.is_empty() {
            break;
        }

        while index < end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }

        let mut value = None;
        if index < end && bytes[index] == b'=' {
            index += 1;
            while index < end && bytes[index].is_ascii_whitespace() {
                index += 1;
            }

            if index < end && (bytes[index] == b'"' || bytes[index] == b'\'') {
                let quote = bytes[index];
                index += 1;
                let value_start = index;
                while index < end && bytes[index] != quote {
                    index += 1;
                }
                value = Some(root_tag[value_start..index].to_string());
                if index < end {
                    index += 1;
                }
            } else {
                let value_start = index;
                while index < end && !bytes[index].is_ascii_whitespace() && bytes[index] != b'/' {
                    index += 1;
                }
                value = Some(root_tag[value_start..index].to_string());
            }
        }

        let raw = root_tag[attr_start..index].trim().to_string();
        attrs.push(SvgRootAttr { name, value, raw });
    }

    attrs
}

/// 取基础缓存文件路径。
fn mermaid_base_cache_path(key: &str) -> anyhow::Result<PathBuf> {
    mermaid_cache_file_path("base", key)
}

/// 取展示缓存文件路径。
fn mermaid_display_cache_path(key: &str) -> anyhow::Result<PathBuf> {
    mermaid_cache_file_path("display", key)
}

/// 拼接缓存文件路径。
fn mermaid_cache_file_path(kind: &str, key: &str) -> anyhow::Result<PathBuf> {
    let dir = svg_cache_dir(&format!("mermaid-svg/{kind}"))?;
    Ok(dir.join(format!("{key}.svg")))
}

/// 判断是否为受支持的 Mermaid 源码。
fn looks_like_supported_mermaid_source(source: &str) -> bool {
    let mut in_frontmatter = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "---" {
            in_frontmatter = !in_frontmatter;
            continue;
        }
        if in_frontmatter || trimmed.starts_with("%%") {
            continue;
        }

        let lower = trimmed.to_ascii_lowercase();
        return [
            "sequencediagram",
            "classdiagram",
            "statediagram",
            "erdiagram",
            "pie",
            "mindmap",
            "journey",
            "timeline",
            "gantt",
            "requirementdiagram",
            "gitgraph",
            "c4",
            "sankey",
            "quadrantchart",
            "zenuml",
            "block",
            "packet",
            "kanban",
            "architecture",
            "radar",
            "treemap",
            "xychart",
            "flowchart",
            "graph",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    static TEST_RENDERER_CALLS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();

    /// 测试用渲染器并统计调用次数（测试辅助）。
    fn test_renderer(source: &str) -> anyhow::Result<String> {
        let calls = TEST_RENDERER_CALLS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut calls = calls.lock().expect("renderer calls mutex poisoned");
        *calls.entry(source.to_string()).or_default() += 1;
        drop(calls);
        render_mermaid_raw(source)
    }

    /// 重置指定源码的渲染器调用计数（测试辅助）。
    fn reset_renderer_calls(source: &str) {
        let calls = TEST_RENDERER_CALLS.get_or_init(|| Mutex::new(HashMap::new()));
        calls
            .lock()
            .expect("renderer calls mutex poisoned")
            .remove(source);
    }

    /// 读取指定源码的渲染器调用次数（测试辅助）。
    fn renderer_calls(source: &str) -> usize {
        let calls = TEST_RENDERER_CALLS.get_or_init(|| Mutex::new(HashMap::new()));
        calls
            .lock()
            .expect("renderer calls mutex poisoned")
            .get(source)
            .copied()
            .unwrap_or(0)
    }

    /// 构造带唯一标签的测试用图表源码（测试辅助）。
    fn unique_mermaid_source(label: &str) -> MermaidSource {
        MermaidSource {
            raw: format!("```mermaid\nflowchart LR\nA[{}] --> B\n```", label),
            body: format!("flowchart LR\nA[{}] --> B", label),
            info: "mermaid".to_string(),
        }
    }

    /// 删除指定的缓存文件（测试辅助）。
    fn remove_cache_file(path: &Path) {
        if path.exists() {
            fs::remove_file(path).expect("remove cache file");
        }
    }

    #[test]
    fn detects_mermaid_info_string() {
        assert!(is_mermaid_info_string(Some("mermaid")));
        assert!(is_mermaid_info_string(Some("MMD title")));
        assert!(!is_mermaid_info_string(Some("rust")));
        assert!(!is_mermaid_info_string(None));
    }

    #[test]
    fn parses_backtick_mermaid_fence() {
        let parsed = parse_mermaid_fence_source("```mermaid\nflowchart LR\nA --> B\n```")
            .expect("mermaid fence");
        assert_eq!(parsed.info, "mermaid");
        assert_eq!(parsed.body, "flowchart LR\nA --> B");
    }

    #[test]
    fn parses_tilde_mmd_fence() {
        let parsed = parse_mermaid_fence_source("~~~MMD\nflowchart LR\nA --> B\n~~~")
            .expect("mermaid fence");
        assert_eq!(parsed.info, "MMD");
        assert_eq!(parsed.body, "flowchart LR\nA --> B");
    }

    #[test]
    fn rejects_unclosed_mermaid_fence() {
        assert!(parse_mermaid_fence_source("```mermaid\nflowchart LR").is_none());
    }

    #[test]
    fn cache_key_changes_with_source() {
        assert_ne!(
            mermaid_cache_key("flowchart LR\nA --> B"),
            mermaid_cache_key("flowchart LR\nA --> C")
        );
    }

    #[test]
    fn semantic_line_count_ignores_comments_blank_lines_and_frontmatter() {
        let source = "---\ntitle: Demo\n---\nflowchart LR\n%% comment\n\nA --> B\nB --> C";
        assert_eq!(semantic_mermaid_line_count(source), 3);
    }

    #[test]
    fn display_scale_uses_intrinsic_size_and_caps_growth() {
        let simple = "flowchart LR\nA --> B\nB --> C";
        assert_eq!(
            mermaid_display_scale(simple, 240.0, 120.0, 720.0, 960.0),
            1.0
        );

        let complex = std::iter::once("flowchart LR".to_string())
            .chain((0..20).map(|index| format!("A{index} --> A{}", index + 1)))
            .collect::<Vec<_>>()
            .join("\n");
        let scale = mermaid_display_scale(&complex, 260.0, 140.0, 720.0, 960.0);
        assert!(scale > 1.0);
        assert!(scale <= MERMAID_MAX_SCALE);
        assert!(260.0 * scale <= 960.0 * MERMAID_MAX_VIEWPORT_WIDTH_RATIO);
    }

    #[test]
    fn display_scale_does_not_overgrow_already_wide_diagrams() {
        let complex = std::iter::once("flowchart LR".to_string())
            .chain((0..30).map(|index| format!("A{index} --> A{}", index + 1)))
            .collect::<Vec<_>>()
            .join("\n");
        let scale = mermaid_display_scale(&complex, 1400.0, 400.0, 720.0, 960.0);

        assert_eq!(scale, 1.0);
    }

    #[test]
    fn display_cache_key_changes_with_scale() {
        let source = "flowchart LR\nA --> B";
        assert_ne!(
            mermaid_display_cache_key(source, 1.0),
            mermaid_display_cache_key(source, 2.0)
        );
    }

    #[test]
    fn display_cache_key_is_stable_for_subpixel_scale_differences() {
        let source = "flowchart LR\nA --> B";
        assert_eq!(
            mermaid_display_cache_key(source, 1.25),
            mermaid_display_cache_key(source, 1.2501),
            "亚像素缩放不应生成新缓存文件"
        );
        assert_ne!(
            mermaid_display_cache_key(source, 1.25),
            mermaid_display_cache_key(source, 1.5)
        );
    }

    #[test]
    fn display_svg_scaling_rewrites_root_dimensions() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50" viewBox="0 0 100 50"><rect width="100" height="50"/></svg>"#;
        let (scaled, size) = scale_mermaid_svg_for_display(svg, 2.0).expect("scaled svg");

        assert_eq!(
            size,
            MermaidSize {
                width: 200.0,
                height: 100.0
            }
        );
        assert!(scaled.contains(r#"width="200.000""#), "scaled: {scaled}");
        assert!(scaled.contains(r#"height="100.000""#), "scaled: {scaled}");
        assert!(
            scaled.contains(r#"viewBox="0 0 100 50""#),
            "scaled: {scaled}"
        );
    }

    #[test]
    fn display_svg_scaling_removes_responsive_root_attrs() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" style="max-width: 240px; aspect-ratio: 2;" viewBox="0 0 120 60"><text>x</text></svg>"#;
        let (scaled, size) = scale_mermaid_svg_for_display(svg, 1.5).expect("scaled svg");

        assert_eq!(
            size,
            MermaidSize {
                width: 180.0,
                height: 90.0
            }
        );
        let root = &scaled[..scaled.find('>').unwrap()];
        assert!(root.contains(r#"width="180.000""#), "root: {root}");
        assert!(root.contains(r#"height="90.000""#), "root: {root}");
        assert!(!root.contains("100%"), "root: {root}");
        assert!(!root.contains("max-width"), "root: {root}");
        assert!(!root.contains("style="), "root: {root}");
    }

    #[test]
    fn renders_basic_flowchart_svg() {
        let svg = render_mermaid_raw("flowchart LR\nA --> B").expect("svg");
        assert!(svg.contains("<svg"), "svg: {svg}");
        assert!(svg.contains("</svg>"), "svg: {svg}");
    }

    #[test]
    fn display_render_uses_scaled_intrinsic_size() {
        let source =
            parse_mermaid_fence_source("```mermaid\nflowchart LR\nA --> B\n```").expect("source");
        let rendered = render_mermaid_svg_for_display(&source, 720.0, 960.0).expect("display svg");

        assert!(rendered.display_width > 1.0);
        assert!(rendered.display_height > 1.0);
        assert!(rendered.display_scale >= 1.0);
        assert!(
            rendered
                .svg
                .contains(&format!("width=\"{:.3}\"", rendered.display_width))
        );
        assert!(
            rendered
                .svg
                .contains(&format!("height=\"{:.3}\"", rendered.display_height))
        );
        assert!(rendered.path.exists());
    }

    #[test]
    fn invalid_mermaid_returns_error() {
        assert!(render_mermaid_raw("not a real mermaid diagram ::::").is_err());
    }

    #[test]
    fn display_cache_hit_does_not_call_renderer_again() {
        let source = unique_mermaid_source("display-cache-hit-does-not-call-renderer-again");
        let base_key = mermaid_cache_key(&source.body);
        let base_path = mermaid_base_cache_path(&base_key).expect("base path");
        remove_cache_file(&base_path);

        reset_renderer_calls(&source.body);
        let first = render_mermaid_svg_for_display_with(&source, 720.0, 960.0, test_renderer)
            .expect("first render");
        assert_eq!(renderer_calls(&source.body), 1);
        let display_path = first.path.clone();

        let second = render_mermaid_svg_for_display_with(&source, 720.0, 960.0, test_renderer)
            .expect("cached render");
        assert_eq!(renderer_calls(&source.body), 1);
        assert_eq!(second.path, display_path);
        assert_eq!(second.display_width, first.display_width);
        assert_eq!(second.display_height, first.display_height);

        remove_cache_file(&display_path);
        remove_cache_file(&base_path);
    }

    #[test]
    fn display_cache_miss_reuses_base_cache() {
        let source = unique_mermaid_source("display-cache-miss-reuses-base-cache");
        let base_key = mermaid_cache_key(&source.body);
        let base_path = mermaid_base_cache_path(&base_key).expect("base path");
        remove_cache_file(&base_path);

        reset_renderer_calls(&source.body);
        let first = render_mermaid_svg_for_display_with(&source, 720.0, 960.0, test_renderer)
            .expect("first render");
        assert_eq!(renderer_calls(&source.body), 1);
        remove_cache_file(&first.path);

        let second = render_mermaid_svg_for_display_with(&source, 720.0, 960.0, test_renderer)
            .expect("display rebuild");
        assert_eq!(renderer_calls(&source.body), 1);
        assert!(second.path.exists());
        assert_eq!(second.display_width, first.display_width);
        assert_eq!(second.display_height, first.display_height);

        remove_cache_file(&second.path);
        remove_cache_file(&base_path);
    }

    /// 磁盘文件被外部清理时，内存副本要能补写回来且不重新渲染。
    #[test]
    fn memory_cache_restores_deleted_display_file_without_rerendering() {
        let source = unique_mermaid_source("memory-cache-restores-deleted-display-file");
        let base_key = mermaid_cache_key(&source.body);
        let base_path = mermaid_base_cache_path(&base_key).expect("base path");
        remove_cache_file(&base_path);

        reset_renderer_calls(&source.body);
        let first = render_mermaid_svg_for_display_with(&source, 720.0, 960.0, test_renderer)
            .expect("first render");
        assert_eq!(renderer_calls(&source.body), 1);
        remove_cache_file(&first.path);

        let second = render_mermaid_svg_for_display_with(&source, 720.0, 960.0, test_renderer)
            .expect("cached render");
        assert_eq!(renderer_calls(&source.body), 1, "不应重新渲染底层图表");
        assert!(second.path.exists(), "内存副本应补写回磁盘");

        remove_cache_file(&second.path);
        remove_cache_file(&base_path);
    }

    #[test]
    fn display_scale_change_reuses_base_cache_with_new_display_file() {
        let source = unique_mermaid_source("display-scale-change-reuses-base-cache");
        let base_key = mermaid_cache_key(&source.body);
        let base_path = mermaid_base_cache_path(&base_key).expect("base path");
        remove_cache_file(&base_path);

        reset_renderer_calls(&source.body);
        let narrow = render_mermaid_svg_for_display_with(&source, 240.0, 320.0, test_renderer)
            .expect("narrow render");
        assert_eq!(renderer_calls(&source.body), 1);

        let wide = render_mermaid_svg_for_display_with(&source, 900.0, 1200.0, test_renderer)
            .expect("wide render");
        assert_eq!(renderer_calls(&source.body), 1);
        assert!(wide.path.exists());

        remove_cache_file(&narrow.path);
        remove_cache_file(&wide.path);
        remove_cache_file(&base_path);
    }
}
