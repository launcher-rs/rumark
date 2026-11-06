//! Markdown 原生 HTML 块的原生安全分类。
//!
//! 解析器以原始源码为序列化真相，仅对可在 GPUI 中安全渲染的标签构建保守语义树。风险、未知、畸形或歧义内容一律回退为纯文本。

use std::ops::Range;

use cssparser::color::{parse_hash_color, parse_named_color};

#[cfg(feature = "html-native")]
use tree_sitter::Parser;

/// HTML 片段的安全分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HtmlSafetyClass {
    /// 片段含至少一个安全语义节点。
    Semantic,
    /// 整个片段须按纯原始文本展示与存储。
    RawTextBlock,
}

/// 已解析 HTML 节点的宽泛渲染类别。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HtmlNodeKind {
    /// 可用文本跑表示的安全行内标签或文本。
    InlineSemantic,
    /// 映射为原生块级 GPUI 元素的安全块标签。
    BlockSemantic,
    /// 不得按 HTML 解释的不透明原始源码。
    RawTextBlock,
}

/// 来自 HTML 标签的一条源码属性。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HtmlAttr {
    /// 用于安全检查的小写属性名。
    pub(crate) name: String,
    /// 去掉两侧引号的解析后属性值。
    pub(crate) value: Option<String>,
    /// 精确的属性源码文本。
    pub(crate) raw_source: String,
}

/// 从安全行内 `style` 属性解析出的 CSS 颜色值。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum HtmlCssColor {
    /// CSS `currentColor` 关键字。
    CurrentColor,
    /// 带透明度的 sRGB 颜色。
    Rgba(HtmlCssRgba),
}

/// 归一化到可供 GPUI 渲染与导出 CSS 共用的 RGBA 通道。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HtmlCssRgba {
    pub(crate) red: u8,
    pub(crate) green: u8,
    pub(crate) blue: u8,
    pub(crate) alpha: f32,
}

/// 从安全行内 `style` 属性解析出的 CSS 字号值。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum HtmlCssFontSize {
    Px(f32),
    Em(f32),
    Rem(f32),
    Percent(f32),
    Keyword(HtmlCssFontSizeKeyword),
}

/// 渲染 HTML 支持的 CSS 绝对与相对字号关键字。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HtmlCssFontSizeKeyword {
    XxSmall,
    XSmall,
    Small,
    Medium,
    Large,
    XLarge,
    XxLarge,
    Smaller,
    Larger,
}

/// 从安全 HTML `style` 属性解析出的白名单视觉 CSS。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct HtmlInlineStyle {
    pub(crate) color: Option<HtmlCssColor>,
    pub(crate) background_color: Option<HtmlCssColor>,
    pub(crate) font_size: Option<HtmlCssFontSize>,
}

impl Eq for HtmlInlineStyle {}

/// 从独立 HTML `<img>` 块提取的安全数据。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HtmlImageBlock {
    pub(crate) src: String,
    pub(crate) alt: String,
    /// 可选标题（`title` 属性）。
    pub(crate) title: Option<String>,
    pub(crate) zoom: f32,
}

impl HtmlImageBlock {
    /// 取缩放系数。
    pub(crate) fn zoom_factor(&self) -> f32 {
        self.zoom.clamp(0.1, 3.0)
    }
}

/// 经分类的 HTML 节点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HtmlNode {
    /// 安全策略选定的渲染类别。
    pub(crate) kind: HtmlNodeKind,
    /// 小写标签名，文本节点为 `#text`。
    pub(crate) tag_name: String,
    /// 保留为语义数据的安全属性。
    pub(crate) attrs: Vec<HtmlAttr>,
    /// 经分类的子节点。原始文本节点为空。
    pub(crate) children: Vec<HtmlNode>,
    /// 本节点覆盖的精确源码文本。
    pub(crate) raw_source: String,
    /// 原始 HTML 片段中的字节范围。
    pub(crate) source_range: Range<usize>,
}

/// 经分类的 HTML 片段与其保留的源码文本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HtmlDocument {
    /// 用于序列化与原始编辑的精确源码串。
    pub(crate) raw_source: String,
    /// 根级经分类节点。
    pub(crate) nodes: Vec<HtmlNode>,
    /// 整个片段的安全性。
    pub(crate) safety: HtmlSafetyClass,
}

impl HtmlDocument {
    /// 构造纯原始文档。
    pub(crate) fn raw(raw_source: impl Into<String>) -> Self {
        let raw_source = raw_source.into();
        Self {
            nodes: vec![raw_node(&raw_source, 0..raw_source.len())],
            safety: HtmlSafetyClass::RawTextBlock,
            raw_source,
        }
    }

    /// 判断是否为语义文档。
    pub(crate) fn is_semantic(&self) -> bool {
        self.safety == HtmlSafetyClass::Semantic
    }
}

impl HtmlCssColor {
    /// 转为 CSS 串。
    pub(crate) fn to_css(self) -> String {
        match self {
            Self::CurrentColor => "currentColor".to_string(),
            Self::Rgba(color) => format!(
                "rgba({},{},{},{:.3})",
                color.red,
                color.green,
                color.blue,
                color.alpha.clamp(0.0, 1.0)
            ),
        }
    }
}

impl HtmlCssFontSize {
    /// 解析为像素值。
    /// 按父字号与根字号解析。
    pub(crate) fn resolve(self, parent_px: f32, root_px: f32) -> f32 {
        let resolved = match self {
            Self::Px(value) => value,
            Self::Em(value) => parent_px * value,
            Self::Rem(value) => root_px * value,
            Self::Percent(value) => parent_px * value / 100.0,
            Self::Keyword(keyword) => match keyword {
                HtmlCssFontSizeKeyword::XxSmall => root_px * 0.6,
                HtmlCssFontSizeKeyword::XSmall => root_px * 0.75,
                HtmlCssFontSizeKeyword::Small => root_px * 0.875,
                HtmlCssFontSizeKeyword::Medium => root_px,
                HtmlCssFontSizeKeyword::Large => root_px * 1.125,
                HtmlCssFontSizeKeyword::XLarge => root_px * 1.5,
                HtmlCssFontSizeKeyword::XxLarge => root_px * 2.0,
                HtmlCssFontSizeKeyword::Smaller => parent_px * 0.833,
                HtmlCssFontSizeKeyword::Larger => parent_px * 1.2,
            },
        };

        if resolved.is_finite() {
            resolved.clamp(6.0, 96.0)
        } else {
            parent_px
        }
    }

    /// 转为 CSS 串。
    pub(crate) fn to_css(self) -> String {
        match self {
            Self::Px(value) => format!("{}px", css_number(value)),
            Self::Em(value) => format!("{}em", css_number(value)),
            Self::Rem(value) => format!("{}rem", css_number(value)),
            Self::Percent(value) => format!("{}%", css_number(value)),
            Self::Keyword(keyword) => match keyword {
                HtmlCssFontSizeKeyword::XxSmall => "xx-small",
                HtmlCssFontSizeKeyword::XSmall => "x-small",
                HtmlCssFontSizeKeyword::Small => "small",
                HtmlCssFontSizeKeyword::Medium => "medium",
                HtmlCssFontSizeKeyword::Large => "large",
                HtmlCssFontSizeKeyword::XLarge => "x-large",
                HtmlCssFontSizeKeyword::XxLarge => "xx-large",
                HtmlCssFontSizeKeyword::Smaller => "smaller",
                HtmlCssFontSizeKeyword::Larger => "larger",
            }
            .to_string(),
        }
    }
}

impl HtmlInlineStyle {
    /// 判断是否为空。
    pub(crate) fn is_empty(&self) -> bool {
        self.color.is_none() && self.background_color.is_none() && self.font_size.is_none()
    }

    /// 转为 CSS 串，无声明时返回空。
    pub(crate) fn to_css(self) -> Option<String> {
        if self.is_empty() {
            return None;
        }

        let mut declarations = Vec::new();
        if let Some(color) = self.color {
            declarations.push(format!("color: {}", color.to_css()));
        }
        if let Some(color) = self.background_color {
            declarations.push(format!("background-color: {}", color.to_css()));
        }
        if let Some(font_size) = self.font_size {
            declarations.push(format!("font-size: {}", font_size.to_css()));
        }
        Some(format!("{};", declarations.join("; ")))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TagKind {
    Open,
    Close,
    CommentLike,
}

#[derive(Clone, Debug)]
struct HtmlTagToken {
    kind: TagKind,
    name: String,
    attrs: Vec<HtmlAttr>,
    self_closing: bool,
    source_range: Range<usize>,
}

/// 解析并分类原始 HTML 片段。即使语义解析成功，返回文档仍原样保留 `raw_source`。
pub(crate) fn parse_html_document(raw_source: &str) -> HtmlDocument {
    if raw_source.trim().is_empty() {
        return HtmlDocument::raw(raw_source);
    }

    if tree_sitter_reports_error(raw_source) {
        return HtmlDocument::raw(raw_source);
    }

    let (nodes, index, ok) = parse_nodes(raw_source, 0, None);
    if !ok || index < raw_source.len() || nodes.is_empty() {
        return HtmlDocument::raw(raw_source);
    }

    if nodes
        .iter()
        .all(|node| matches!(node.kind, HtmlNodeKind::RawTextBlock))
    {
        return HtmlDocument::raw(raw_source);
    }

    HtmlDocument {
        raw_source: raw_source.to_string(),
        nodes,
        safety: HtmlSafetyClass::Semantic,
    }
}

/// 解析语义节点 `style` 属性的安全视觉子集。
pub(crate) fn style_for_node(node: &HtmlNode) -> HtmlInlineStyle {
    if node.kind == HtmlNodeKind::RawTextBlock {
        return HtmlInlineStyle::default();
    }

    let Some(style) = attr_value(node, "style") else {
        return HtmlInlineStyle::default();
    };

    parse_inline_style(style)
}

/// 解析节点列表。
fn parse_nodes(
    raw: &str,
    mut index: usize,
    closing_tag: Option<&str>,
) -> (Vec<HtmlNode>, usize, bool) {
    let mut nodes = Vec::new();
    while index < raw.len() {
        let Some(tag_start_relative) = raw[index..].find('<') else {
            push_text_node(raw, index..raw.len(), &mut nodes);
            return (nodes, raw.len(), closing_tag.is_none());
        };

        let tag_start = index + tag_start_relative;
        if tag_start > index {
            push_text_node(raw, index..tag_start, &mut nodes);
        }

        let Some(token) = parse_tag_token(raw, tag_start) else {
            push_text_node(raw, tag_start..tag_start + 1, &mut nodes);
            index = tag_start + 1;
            continue;
        };

        match token.kind {
            TagKind::Close => {
                if closing_tag == Some(token.name.as_str()) {
                    return (nodes, token.source_range.end, true);
                }
                nodes.push(raw_node(raw, token.source_range.clone()));
                index = token.source_range.end;
            }
            TagKind::CommentLike => {
                nodes.push(raw_node(raw, token.source_range.clone()));
                index = token.source_range.end;
            }
            TagKind::Open => {
                let class = classify_open_tag(&token);
                if class == HtmlSafetyClass::RawTextBlock {
                    let raw_end = raw_region_end(raw, &token).unwrap_or(token.source_range.end);
                    nodes.push(raw_node(raw, token.source_range.start..raw_end));
                    index = raw_end;
                    continue;
                }

                if token.self_closing || is_void_tag(&token.name) {
                    nodes.push(semantic_node(raw, token, Vec::new()));
                    index = nodes
                        .last()
                        .map(|node| node.source_range.end)
                        .unwrap_or(index);
                    continue;
                }

                let (children, child_end, closed) =
                    parse_nodes(raw, token.source_range.end, Some(&token.name));
                if !closed {
                    nodes.push(raw_node(raw, token.source_range.start..raw.len()));
                    return (nodes, raw.len(), closing_tag.is_none());
                }

                let mut node = semantic_node(raw, token, children);
                node.source_range.end = child_end;
                node.raw_source = raw[node.source_range.clone()].to_string();
                nodes.push(node);
                index = child_end;
            }
        }
    }

    (nodes, index, closing_tag.is_none())
}

/// 解析标签词元。
fn parse_tag_token(raw: &str, start: usize) -> Option<HtmlTagToken> {
    let rest = raw.get(start..)?;
    if !rest.starts_with('<') {
        return None;
    }

    if rest.starts_with("<!--") {
        let end = rest.find("-->").map(|offset| start + offset + 3)?;
        return Some(HtmlTagToken {
            kind: TagKind::CommentLike,
            name: "#comment".into(),
            attrs: Vec::new(),
            self_closing: true,
            source_range: start..end,
        });
    }

    if rest.starts_with("<!") || rest.starts_with("<?") {
        let end = rest.find('>').map(|offset| start + offset + 1)?;
        return Some(HtmlTagToken {
            kind: TagKind::CommentLike,
            name: "#raw".into(),
            attrs: Vec::new(),
            self_closing: true,
            source_range: start..end,
        });
    }

    let bytes = raw.as_bytes();
    let mut index = start + 1;
    let closing = bytes.get(index) == Some(&b'/');
    if closing {
        index += 1;
    }

    let name_start = index;
    while index < raw.len() {
        let ch = raw[index..].chars().next()?;
        if ch.is_ascii_alphanumeric() || ch == '-' {
            index += ch.len_utf8();
        } else {
            break;
        }
    }
    if index == name_start {
        return None;
    }

    let name = raw[name_start..index].to_ascii_lowercase();
    let attrs_start = index;
    let mut quote: Option<char> = None;
    while index < raw.len() {
        let ch = raw[index..].chars().next()?;
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            index += ch.len_utf8();
            continue;
        }

        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            index += ch.len_utf8();
            continue;
        }

        if ch == '>' {
            let source_range = start..index + 1;
            let attrs_source = &raw[attrs_start..index];
            let self_closing = attrs_source.trim_end().ends_with('/');
            return Some(HtmlTagToken {
                kind: if closing {
                    TagKind::Close
                } else {
                    TagKind::Open
                },
                name,
                attrs: if closing {
                    Vec::new()
                } else {
                    parse_html_attrs(attrs_source)
                },
                self_closing,
                source_range,
            });
        }

        index += ch.len_utf8();
    }

    None
}

/// 查看 `index` 处下一个字符但不推进。遇到文末返回 `None`。
#[inline]
fn peek_char(source: &str, index: usize) -> Option<char> {
    source[index..].chars().next()
}

/// 把 `index` 推过下一个字符并返回它。遇到文末返回 `None`。
/// 封装字节下标与 UTF-8 边界的不变式，不需要字符值的调用方不会因手动累加 `index` 而偏离边界 panic。需要判字符的循环应先用 [`peek_char`] 查看取值，再用 `index += ch.len_utf8()` 推进（见 [`parse_html_attrs`]），保证每轮只读一次字符。
#[inline]
fn advance_char(source: &str, index: &mut usize) -> Option<char> {
    let ch = source[*index..].chars().next()?;
    *index += ch.len_utf8();
    Some(ch)
}

/// 解析 HTML 属性列表。
pub(crate) fn parse_html_attrs(source: &str) -> Vec<HtmlAttr> {
    let mut attrs = Vec::new();
    let mut index = 0usize;
    while index < source.len() {
        while let Some(ch) = peek_char(source, index).filter(|c| c.is_whitespace() || *c == '/') {
            index += ch.len_utf8();
        }
        if index >= source.len() {
            break;
        }

        let start = index;
        while let Some(ch) = peek_char(source, index) {
            if ch.is_whitespace() || ch == '=' || ch == '/' {
                break;
            }
            index += ch.len_utf8();
        }
        let name_end = index;
        if name_end == start {
            // Lone separator we couldn't classify — consume one char and retry.
            advance_char(source, &mut index);
            continue;
        }

        while let Some(ch) = peek_char(source, index).filter(|c| c.is_whitespace()) {
            index += ch.len_utf8();
        }

        let mut value = None;
        if source[index..].starts_with('=') {
            index += 1;
            while let Some(ch) = peek_char(source, index).filter(|c| c.is_whitespace()) {
                index += ch.len_utf8();
            }

            if let Some(quote) = peek_char(source, index).filter(|c| *c == '"' || *c == '\'') {
                index += quote.len_utf8();
                let value_start = index;
                while let Some(ch) = peek_char(source, index) {
                    if ch == quote {
                        break;
                    }
                    index += ch.len_utf8();
                }
                value = Some(source[value_start..index].to_string());
                if index < source.len() {
                    index += quote.len_utf8();
                }
            } else if peek_char(source, index).is_some() {
                let value_start = index;
                while let Some(ch) = peek_char(source, index) {
                    if ch.is_whitespace() || ch == '/' {
                        break;
                    }
                    index += ch.len_utf8();
                }
                value = Some(source[value_start..index].to_string());
            }
        }

        attrs.push(HtmlAttr {
            name: source[start..name_end].to_ascii_lowercase(),
            value,
            raw_source: source[start..index].to_string(),
        });
    }

    attrs
}

/// 分类开始标签。
fn classify_open_tag(token: &HtmlTagToken) -> HtmlSafetyClass {
    if !is_safe_tag(&token.name) || has_dangerous_attrs(&token.attrs) {
        HtmlSafetyClass::RawTextBlock
    } else {
        HtmlSafetyClass::Semantic
    }
}

/// 构造语义节点。
fn semantic_node(raw: &str, token: HtmlTagToken, children: Vec<HtmlNode>) -> HtmlNode {
    HtmlNode {
        kind: if is_inline_tag(&token.name) {
            HtmlNodeKind::InlineSemantic
        } else {
            HtmlNodeKind::BlockSemantic
        },
        tag_name: token.name,
        attrs: token.attrs,
        children,
        raw_source: raw[token.source_range.clone()].to_string(),
        source_range: token.source_range,
    }
}

/// 追加文本节点。
fn push_text_node(raw: &str, range: Range<usize>, nodes: &mut Vec<HtmlNode>) {
    if range.is_empty() {
        return;
    }
    nodes.push(HtmlNode {
        kind: HtmlNodeKind::InlineSemantic,
        tag_name: "#text".into(),
        attrs: Vec::new(),
        children: Vec::new(),
        raw_source: raw[range.clone()].to_string(),
        source_range: range,
    });
}

/// 构造原始节点。
fn raw_node(raw: &str, range: Range<usize>) -> HtmlNode {
    HtmlNode {
        kind: HtmlNodeKind::RawTextBlock,
        tag_name: "#raw".into(),
        attrs: Vec::new(),
        children: Vec::new(),
        raw_source: raw[range.clone()].to_string(),
        source_range: range,
    }
}

/// 取原始区域结束位置。
fn raw_region_end(raw: &str, token: &HtmlTagToken) -> Option<usize> {
    if token.self_closing || is_void_tag(&token.name) {
        return Some(token.source_range.end);
    }

    let close = format!("</{}>", token.name);
    let close_upper = close.to_ascii_uppercase();
    let rest = &raw[token.source_range.end..];
    let lower = rest.to_ascii_lowercase();
    let upper = rest.to_ascii_uppercase();
    lower
        .find(&close)
        .or_else(|| upper.find(&close_upper))
        .map(|offset| token.source_range.end + offset + close.len())
        .or(Some(raw.len()))
}

/// 判断属性是否含危险内容。
pub(crate) fn has_dangerous_attrs(attrs: &[HtmlAttr]) -> bool {
    attrs.iter().any(|attr| {
        attr.name.starts_with("on")
            || attr.value.as_deref().is_some_and(|value| {
                let normalized = value
                    .chars()
                    .filter(|ch| !ch.is_whitespace() && *ch != '\0')
                    .collect::<String>()
                    .to_ascii_lowercase();
                matches!(
                    attr.name.as_str(),
                    "href" | "src" | "action" | "formaction" | "xlink:href"
                ) && normalized.starts_with("javascript:")
            })
    })
}

/// 取节点属性值。
pub(crate) fn attr_value<'a>(node: &'a HtmlNode, name: &str) -> Option<&'a str> {
    node.attrs
        .iter()
        .find(|attr| attr.name == name)
        .and_then(|attr| attr.value.as_deref())
}

/// 解析独立 HTML 图片块。
pub(crate) fn parse_html_image_block(raw_source: &str) -> Option<HtmlImageBlock> {
    let trimmed = raw_source.trim();
    if trimmed.is_empty() {
        return None;
    }

    let token = parse_tag_token(trimmed, 0)?;
    if token.kind != TagKind::Open
        || token.name != "img"
        || token.source_range != (0..trimmed.len())
    {
        return None;
    }
    if has_dangerous_attrs(&token.attrs) {
        return None;
    }

    let src = attr_value_in_attrs(&token.attrs, "src")?.trim().to_string();
    if src.is_empty() {
        return None;
    }

    let alt = attr_value_in_attrs(&token.attrs, "alt")
        .unwrap_or_default()
        .to_string();
    let title = attr_value_in_attrs(&token.attrs, "title")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let zoom = attr_value_in_attrs(&token.attrs, "style")
        .and_then(parse_html_zoom)
        .unwrap_or(1.0);

    Some(HtmlImageBlock {
        src,
        alt,
        title,
        zoom,
    })
}

/// 在属性列表中取值。
fn attr_value_in_attrs<'a>(attrs: &'a [HtmlAttr], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|attr| attr.name == name)
        .and_then(|attr| attr.value.as_deref())
}

/// 解析 HTML 图片缩放。
pub(crate) fn parse_html_zoom(style: &str) -> Option<f32> {
    for declaration in style.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        if !property.trim().eq_ignore_ascii_case("zoom") {
            continue;
        }

        let value = value.trim();
        let parsed = if let Some(percent) = value.strip_suffix('%') {
            parse_css_number(percent)? / 100.0
        } else {
            parse_css_number(value)?
        };
        return Some(parsed.clamp(0.1, 3.0));
    }
    None
}

/// 解析行内样式。
pub(crate) fn parse_inline_style(style: &str) -> HtmlInlineStyle {
    let mut parsed = HtmlInlineStyle::default();
    for declaration in style.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let value = value.trim();
        match property.as_str() {
            "color" => {
                if let Some(color) = parse_css_color(value) {
                    parsed.color = Some(color);
                }
            }
            "background-color" => {
                if let Some(color) = parse_css_color(value) {
                    parsed.background_color = Some(color);
                }
            }
            "font-size" => {
                if let Some(size) = parse_css_font_size(value) {
                    parsed.font_size = Some(size);
                }
            }
            _ => {}
        }
    }
    parsed
}

/// 解析 CSS 颜色。
fn parse_css_color(value: &str) -> Option<HtmlCssColor> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("currentcolor") {
        return Some(HtmlCssColor::CurrentColor);
    }
    if value.eq_ignore_ascii_case("transparent") {
        return Some(HtmlCssColor::Rgba(HtmlCssRgba {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0.0,
        }));
    }
    if let Some(hex) = value.strip_prefix('#')
        && let Ok((red, green, blue, alpha)) = parse_hash_color(hex.as_bytes())
    {
        return Some(HtmlCssColor::Rgba(HtmlCssRgba {
            red,
            green,
            blue,
            alpha,
        }));
    }
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphabetic() || ch == '-')
        && let Ok((red, green, blue)) = parse_named_color(value)
    {
        return Some(HtmlCssColor::Rgba(HtmlCssRgba {
            red,
            green,
            blue,
            alpha: 1.0,
        }));
    }
    parse_rgb_color(value).or_else(|| parse_hsl_color(value))
}

/// 解析 RGB 颜色。
fn parse_rgb_color(value: &str) -> Option<HtmlCssColor> {
    let args = css_function_args(value, &["rgb", "rgba"])?;
    let parts = css_function_parts(args);
    if parts.len() < 3 {
        return None;
    }

    let red = parse_rgb_component(&parts[0])?;
    let green = parse_rgb_component(&parts[1])?;
    let blue = parse_rgb_component(&parts[2])?;
    let alpha = parts
        .get(3)
        .and_then(|part| parse_alpha_component(part))
        .unwrap_or(1.0);
    Some(HtmlCssColor::Rgba(HtmlCssRgba {
        red,
        green,
        blue,
        alpha,
    }))
}

/// 解析 HSL 颜色。
fn parse_hsl_color(value: &str) -> Option<HtmlCssColor> {
    let args = css_function_args(value, &["hsl", "hsla"])?;
    let parts = css_function_parts(args);
    if parts.len() < 3 {
        return None;
    }

    let hue = parse_hue(&parts[0])?;
    let saturation = parse_percent_component(&parts[1])?;
    let lightness = parse_percent_component(&parts[2])?;
    let alpha = parts
        .get(3)
        .and_then(|part| parse_alpha_component(part))
        .unwrap_or(1.0);
    let (red, green, blue) = hsl_to_rgb(hue, saturation, lightness);
    Some(HtmlCssColor::Rgba(HtmlCssRgba {
        red,
        green,
        blue,
        alpha,
    }))
}

/// 取 CSS 函数参数。
fn css_function_args<'a>(value: &'a str, names: &[&str]) -> Option<&'a str> {
    let open = value.find('(')?;
    let close = value.rfind(')')?;
    if close <= open || !value[close + 1..].trim().is_empty() {
        return None;
    }
    let name = value[..open].trim();
    names
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
        .then_some(&value[open + 1..close])
}

/// 切分 CSS 函数参数。
fn css_function_parts(args: &str) -> Vec<String> {
    if args.contains(',') {
        return args
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect();
    }

    let normalized = args.replace('/', " / ");
    normalized
        .split_whitespace()
        .filter(|token| *token != "/")
        .map(str::to_string)
        .collect()
}

/// 解析 RGB 分量。
fn parse_rgb_component(value: &str) -> Option<u8> {
    if let Some(percent) = value.trim().strip_suffix('%') {
        let value = parse_css_number(percent)?;
        return Some((value.clamp(0.0, 100.0) * 255.0 / 100.0).round() as u8);
    }

    let value = parse_css_number(value)?;
    Some(value.clamp(0.0, 255.0).round() as u8)
}

/// 解析百分比分量。
fn parse_percent_component(value: &str) -> Option<f32> {
    let value = value.trim().strip_suffix('%')?;
    Some((parse_css_number(value)? / 100.0).clamp(0.0, 1.0))
}

/// 解析透明度分量。
fn parse_alpha_component(value: &str) -> Option<f32> {
    if let Some(percent) = value.trim().strip_suffix('%') {
        return Some((parse_css_number(percent)? / 100.0).clamp(0.0, 1.0));
    }
    Some(parse_css_number(value)?.clamp(0.0, 1.0))
}

/// 解析色相。
fn parse_hue(value: &str) -> Option<f32> {
    let trimmed = value.trim().to_ascii_lowercase();
    if let Some(value) = trimmed.strip_suffix("deg") {
        return parse_css_number(value);
    }
    if let Some(value) = trimmed.strip_suffix("turn") {
        return Some(parse_css_number(value)? * 360.0);
    }
    if let Some(value) = trimmed.strip_suffix("rad") {
        return Some(parse_css_number(value)? * 180.0 / std::f32::consts::PI);
    }
    parse_css_number(&trimmed)
}

/// HSL 转 RGB。
fn hsl_to_rgb(hue_degrees: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let hue = hue_degrees.rem_euclid(360.0) / 60.0;
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let x = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
    let (red, green, blue) = match hue.floor() as i32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = lightness - chroma / 2.0;
    (
        ((red + m).clamp(0.0, 1.0) * 255.0).round() as u8,
        ((green + m).clamp(0.0, 1.0) * 255.0).round() as u8,
        ((blue + m).clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

/// 解析 CSS 字号。
fn parse_css_font_size(value: &str) -> Option<HtmlCssFontSize> {
    let trimmed = value.trim().to_ascii_lowercase();
    match trimmed.as_str() {
        "xx-small" => return Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::XxSmall)),
        "x-small" => return Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::XSmall)),
        "small" => return Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::Small)),
        "medium" => return Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::Medium)),
        "large" => return Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::Large)),
        "x-large" => return Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::XLarge)),
        "xx-large" => return Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::XxLarge)),
        "smaller" => return Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::Smaller)),
        "larger" => return Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::Larger)),
        _ => {}
    }

    if let Some(value) = trimmed.strip_suffix("rem") {
        return Some(HtmlCssFontSize::Rem(parse_non_negative_css_number(value)?));
    }
    if let Some(value) = trimmed.strip_suffix("em") {
        return Some(HtmlCssFontSize::Em(parse_non_negative_css_number(value)?));
    }
    if let Some(value) = trimmed.strip_suffix("px") {
        return Some(HtmlCssFontSize::Px(parse_non_negative_css_number(value)?));
    }
    if let Some(value) = trimmed.strip_suffix('%') {
        return Some(HtmlCssFontSize::Percent(parse_non_negative_css_number(
            value,
        )?));
    }
    None
}

/// 解析非负 CSS 数字。
fn parse_non_negative_css_number(value: &str) -> Option<f32> {
    let value = parse_css_number(value)?;
    (value >= 0.0).then_some(value)
}

/// 解析 CSS 数字。
fn parse_css_number(value: &str) -> Option<f32> {
    let value = value.trim().parse::<f32>().ok()?;
    value.is_finite().then_some(value)
}

/// 格式化 CSS 数字。
fn css_number(value: f32) -> String {
    let mut formatted = format!("{:.3}", value);
    while formatted.contains('.') && formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

/// 判断是否为安全标签。
fn is_safe_tag(name: &str) -> bool {
    is_inline_tag(name) || is_block_tag(name)
}

/// 判断是否为行内标签。
pub(crate) fn is_inline_tag(name: &str) -> bool {
    matches!(
        name,
        "a" | "strong"
            | "em"
            | "b"
            | "i"
            | "u"
            | "mark"
            | "del"
            | "ins"
            | "code"
            | "kbd"
            | "sup"
            | "sub"
            | "small"
            | "abbr"
            | "dfn"
            | "time"
            | "q"
            | "span"
    )
}

/// 判断是否为块级标签。
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "div"
            | "p"
            | "blockquote"
            | "hr"
            | "br"
            | "details"
            | "summary"
            | "figure"
            | "figcaption"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "th"
            | "td"
            | "img"
            | "pre"
    )
}

/// 判断是否为空元素标签。
fn is_void_tag(name: &str) -> bool {
    matches!(name, "br" | "hr" | "img")
}

/// 清洗原始 HTML 片段，输出只含白名单标签与属性的安全标记。
///
/// 编辑器内 HTML 块按语义渲染，导出与复制则面向浏览器和外部站点：脚本标签、
/// 事件属性与危险协议必须在此拦截。无法识别的标签按文本转义保留可见性，
/// 注释与声明直接丢弃。
pub(crate) fn sanitize_html_fragment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut index = 0usize;

    while index < raw.len() {
        let Some(relative) = raw[index..].find('<') else {
            out.push_str(&escape_html_text(&raw[index..]));
            break;
        };
        let tag_start = index + relative;
        if tag_start > index {
            out.push_str(&escape_html_text(&raw[index..tag_start]));
        }

        let Some(token) = parse_tag_token(raw, tag_start) else {
            out.push_str("&lt;");
            index = tag_start + 1;
            continue;
        };
        index = token.source_range.end;

        match token.kind {
            // 注释与声明没有视觉含义，也不该出现在导出产物里。
            TagKind::CommentLike => {}
            TagKind::Open => push_safe_open_tag(&mut out, raw, &token),
            TagKind::Close => {
                if is_safe_tag(&token.name) {
                    out.push_str("</");
                    out.push_str(&token.name);
                    out.push('>');
                } else {
                    out.push_str(&escape_html_text(&raw[token.source_range.clone()]));
                }
            }
        }
    }

    out
}

/// 输出白名单开始标签；非白名单标签按文本转义。
fn push_safe_open_tag(out: &mut String, raw: &str, token: &HtmlTagToken) {
    if !is_safe_tag(&token.name) {
        out.push_str(&escape_html_text(&raw[token.source_range.clone()]));
        return;
    }

    out.push('<');
    out.push_str(&token.name);
    for attr in &token.attrs {
        let Some(rendered) = sanitize_attr(&token.name, attr) else {
            continue;
        };
        out.push(' ');
        out.push_str(&rendered);
    }
    if token.self_closing {
        out.push_str(" />");
    } else {
        out.push('>');
    }
}

/// 渲染单个白名单属性；不在白名单内或取值不安全时返回 `None` 表示丢弃。
fn sanitize_attr(tag_name: &str, attr: &HtmlAttr) -> Option<String> {
    let name = attr.name.as_str();
    if !is_allowed_attr(tag_name, name) {
        return None;
    }

    let value = attr.value.as_deref().unwrap_or_default();
    match name {
        "style" => {
            // 只保留白名单视觉声明，其余样式（含 `url(...)`）整条丢弃。
            let css = parse_inline_style(value).to_css()?;
            Some(format!("style=\"{css}\""))
        }
        "href" | "src" | "cite" => {
            if !is_safe_url(value) {
                return None;
            }
            Some(format!("{name}=\"{}\"", escape_html_text(value)))
        }
        "target" => {
            let target = value.trim().to_ascii_lowercase();
            if !matches!(target.as_str(), "_blank" | "_self" | "_parent" | "_top") {
                return None;
            }
            Some(format!("target=\"{target}\""))
        }
        "open" | "reversed" => Some(format!("{name}=\"\"")),
        _ => Some(format!("{name}=\"{}\"", escape_html_text(value))),
    }
}

/// 判断属性是否在白名单内。
fn is_allowed_attr(tag_name: &str, name: &str) -> bool {
    match name {
        "class" | "id" | "title" | "dir" | "lang" | "style" => true,
        "href" | "target" | "rel" => tag_name == "a",
        "src" | "alt" | "width" | "height" => tag_name == "img",
        "cite" => tag_name == "blockquote",
        "colspan" | "rowspan" | "align" | "valign" | "scope" | "headers" => {
            matches!(tag_name, "td" | "th")
        }
        "start" | "type" | "reversed" => tag_name == "ol",
        "value" => tag_name == "li",
        "open" => tag_name == "details",
        "span" => matches!(tag_name, "col" | "colgroup"),
        _ => false,
    }
}

/// 判断 URL 是否可安全写入导出产物。
///
/// 协议判定前先解码 HTML 实体并剔除控制字符：浏览器会把
/// `java&#115;cript:`、`javascript&colon;` 之类的写法还原成可执行协议。
fn is_safe_url(url: &str) -> bool {
    let decoded = decode_html_entities(url);
    let normalized: String = decoded
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '\0' && (*ch as u32) > 0x20 && *ch != '\u{7f}')
        .collect();
    let lower = normalized.to_ascii_lowercase();

    if lower.starts_with("data:") {
        return is_safe_data_url(&lower);
    }

    match url_scheme(&lower) {
        None => true,
        Some(scheme) => matches!(scheme, "http" | "https" | "mailto" | "ftp" | "ftps" | "tel"),
    }
}

/// 判断 data URL 是否为安全图片类型。
fn is_safe_data_url(lower_url: &str) -> bool {
    let Some(rest) = lower_url.strip_prefix("data:") else {
        return false;
    };
    let mime = rest.split(';').next().unwrap_or_default();
    matches!(
        mime,
        "image/png"
            | "image/jpeg"
            | "image/jpg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/avif"
    )
}

/// 取 URL 的协议部分；相对地址与片段返回 `None`。
fn url_scheme(lower_url: &str) -> Option<&str> {
    for (index, ch) in lower_url.char_indices() {
        match ch {
            ':' => return Some(&lower_url[..index]),
            '/' | '?' | '#' => return None,
            _ => {}
        }
    }
    None
}

/// 解码常见 HTML 实体引用，用于 URL 协议判定。
fn decode_html_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest.find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        match decode_entity(&rest[1..end]) {
            Some(decoded) => {
                out.push(decoded);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 解码单个实体名（不含首尾 `&`、`;`）。
fn decode_entity(entity: &str) -> Option<char> {
    if let Some(digits) = entity.strip_prefix('#') {
        let code = if let Some(hex) = digits
            .strip_prefix('x')
            .or_else(|| digits.strip_prefix('X'))
        {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            digits.parse::<u32>().ok()?
        };
        return char::from_u32(code);
    }

    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "colon" => Some(':'),
        "sol" => Some('/'),
        "tab" => Some('\t'),
        "newline" => Some('\n'),
        _ => None,
    }
}

/// 转义 HTML 特殊字符。
pub(crate) fn escape_html_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// 用解析器判断源码是否有错。
#[cfg(feature = "html-native")]
fn tree_sitter_reports_error(raw_source: &str) -> bool {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_html::LANGUAGE.into())
        .is_err()
    {
        return true;
    }
    parser
        .parse(raw_source, None)
        .is_none_or(|tree| tree.root_node().has_error())
}

/// 无原生解析时保守报错。
#[cfg(not(feature = "html-native"))]
fn tree_sitter_reports_error(_: &str) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_inline_html_classifies_as_semantic() {
        let doc = parse_html_document("<span style='color:blue;'>Blue</span>");
        assert!(doc.is_semantic());
        assert_eq!(doc.nodes[0].tag_name, "span");
        assert_eq!(doc.raw_source, "<span style='color:blue;'>Blue</span>");
    }

    #[test]
    fn risky_tag_classifies_as_raw_text() {
        let doc = parse_html_document("<script>alert(1)</script>");
        assert_eq!(doc.safety, HtmlSafetyClass::RawTextBlock);
        assert_eq!(doc.nodes[0].raw_source, "<script>alert(1)</script>");
    }

    #[test]
    fn dangerous_attribute_classifies_as_raw_text() {
        let doc = parse_html_document("<a href=\"javascript:alert(1)\">bad</a>");
        assert_eq!(doc.safety, HtmlSafetyClass::RawTextBlock);
    }

    #[test]
    fn parses_standalone_html_image_block() {
        let image = parse_html_image_block(
            "<img src=\"./xxx/abc.png\" alt=\"alt text\" style=\"zoom:80%;\" />",
        )
        .expect("html image");

        assert_eq!(image.src, "./xxx/abc.png");
        assert_eq!(image.alt, "alt text");
        assert_eq!(image.zoom, 0.8);
    }

    #[test]
    fn html_image_zoom_ignores_other_style_declarations() {
        let image = parse_html_image_block(
            "<img src=\"a.png\" alt=\"a\" style=\"color:red; zoom: 120%; width:10px\" />",
        )
        .expect("html image");

        assert_eq!(image.zoom, 1.2);
    }

    #[test]
    fn invalid_html_image_blocks_are_not_images() {
        assert!(parse_html_image_block("<img alt=\"missing src\" />").is_none());
        assert!(parse_html_image_block("<img src=\"\" />").is_none());
        assert!(parse_html_image_block("<span><img src=\"x.png\" /></span>").is_none());
    }

    #[test]
    fn risky_child_is_local_raw_inside_safe_parent() {
        let doc = parse_html_document("<div>safe<script>alert(1)</script>tail</div>");
        assert!(doc.is_semantic());
        let div = &doc.nodes[0];
        assert!(
            div.children
                .iter()
                .any(|child| child.kind == HtmlNodeKind::RawTextBlock)
        );
    }

    #[test]
    fn malformed_html_falls_back_to_raw_text() {
        let doc = parse_html_document("<details><summary>x</details>");
        assert_eq!(doc.safety, HtmlSafetyClass::RawTextBlock);
    }

    #[test]
    fn parses_whitelisted_style_color_background_and_font_size() {
        let doc = parse_html_document(
            "<span style=\"color:blue; background-color:#fff8; font-size:20px\">x</span>",
        );
        let style = style_for_node(&doc.nodes[0]);

        assert_eq!(
            style.color,
            Some(HtmlCssColor::Rgba(HtmlCssRgba {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 1.0,
            }))
        );
        assert_eq!(
            style.background_color,
            Some(HtmlCssColor::Rgba(HtmlCssRgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 0.53333336,
            }))
        );
        assert_eq!(style.font_size, Some(HtmlCssFontSize::Px(20.0)));
    }

    #[test]
    fn parses_rgb_hsl_currentcolor_and_font_size_units() {
        let doc = parse_html_document(
            "<span style=\"color:rgba(255, 0, 0, .5); background-color:hsl(120 100% 50% / 25%); font-size:1.25em\">x</span>",
        );
        let style = style_for_node(&doc.nodes[0]);
        assert_eq!(
            style.color,
            Some(HtmlCssColor::Rgba(HtmlCssRgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 0.5,
            }))
        );
        assert_eq!(
            style.background_color,
            Some(HtmlCssColor::Rgba(HtmlCssRgba {
                red: 0,
                green: 255,
                blue: 0,
                alpha: 0.25,
            }))
        );
        assert_eq!(style.font_size, Some(HtmlCssFontSize::Em(1.25)));

        let doc = parse_html_document(
            "<span style=\"color:currentColor; font-size:120%; background-color:transparent\">x</span>",
        );
        let style = style_for_node(&doc.nodes[0]);
        assert_eq!(style.color, Some(HtmlCssColor::CurrentColor));
        assert_eq!(style.font_size, Some(HtmlCssFontSize::Percent(120.0)));
        assert_eq!(
            style.background_color,
            Some(HtmlCssColor::Rgba(HtmlCssRgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 0.0,
            }))
        );

        let doc = parse_html_document("<span style=\"font-size:large\">x</span>");
        assert_eq!(
            style_for_node(&doc.nodes[0]).font_size,
            Some(HtmlCssFontSize::Keyword(HtmlCssFontSizeKeyword::Large))
        );
    }

    #[test]
    fn ignores_unrecognized_or_invalid_style_declarations() {
        let doc = parse_html_document(
            "<span style=\"background-image:url(javascript:bad); color:not-a-real-color; font-size:-1px\">x</span>",
        );
        let style = style_for_node(&doc.nodes[0]);
        assert_eq!(style, HtmlInlineStyle::default());
        assert!(doc.is_semantic());
    }

    #[test]
    fn style_whitelist_parses_safe_declarations() {
        let doc = parse_html_document(
            "<span style=\"color:blue; background-color:rgb(255 255 0); font-size:120%\">x</span>",
        );
        let style = style_for_node(&doc.nodes[0]);
        let css = style.to_css().expect("safe declarations produce css");
        assert!(css.contains("color: rgba(0,0,255,1.000)"), "css: {css}");
        assert!(css.contains("font-size: 120%"), "css: {css}");
    }

    #[test]
    fn sanitize_keeps_safe_tags_and_attributes() {
        assert_eq!(
            sanitize_html_fragment("<span class=\"a\">文本</span>"),
            "<span class=\"a\">文本</span>"
        );
        assert_eq!(
            sanitize_html_fragment("<img src=\"a.png\" alt=\"图\">"),
            "<img src=\"a.png\" alt=\"图\">"
        );
    }

    #[test]
    fn sanitize_escapes_unsafe_tags() {
        assert_eq!(
            sanitize_html_fragment("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(
            sanitize_html_fragment("<iframe src=\"a\"></iframe>"),
            "&lt;iframe src=&quot;a&quot;&gt;&lt;/iframe&gt;"
        );
    }

    #[test]
    fn sanitize_drops_event_handlers_and_dangerous_urls() {
        assert_eq!(
            sanitize_html_fragment("<img src=\"a.png\" onerror=\"alert(1)\">"),
            "<img src=\"a.png\">"
        );
        assert_eq!(
            sanitize_html_fragment("<a href=\"javascript:alert(1)\">x</a>"),
            "<a>x</a>"
        );
        assert_eq!(
            sanitize_html_fragment("<a href=\"java&#115;cript:alert(1)\">x</a>"),
            "<a>x</a>"
        );
        assert_eq!(
            sanitize_html_fragment("<a href=\"javascript&colon;alert(1)\">x</a>"),
            "<a>x</a>"
        );
        assert_eq!(
            sanitize_html_fragment("<img src=\"data:image/svg+xml;base64,AAA\">"),
            "<img>"
        );
    }

    #[test]
    fn sanitize_keeps_safe_urls_and_relative_paths() {
        assert_eq!(
            sanitize_html_fragment("<a href=\"https://example.com/a?b=1&c=2\">x</a>"),
            "<a href=\"https://example.com/a?b=1&amp;c=2\">x</a>"
        );
        assert_eq!(
            sanitize_html_fragment("<a href=\"#anchor\">x</a>"),
            "<a href=\"#anchor\">x</a>"
        );
        assert_eq!(
            sanitize_html_fragment("<img src=\"./img/a.png\">"),
            "<img src=\"./img/a.png\">"
        );
    }

    #[test]
    fn sanitize_filters_style_to_whitelisted_declarations() {
        assert_eq!(
            sanitize_html_fragment(
                "<span style=\"color:red; background-image:url(javascript:x)\">y</span>"
            ),
            "<span style=\"color: rgba(255,0,0,1.000);\">y</span>"
        );
    }

    #[test]
    fn sanitize_drops_comments_and_keeps_plain_text() {
        assert_eq!(sanitize_html_fragment("<!-- 注释 -->文本"), "文本");
        assert_eq!(sanitize_html_fragment("a < b"), "a &lt; b");
    }
}
