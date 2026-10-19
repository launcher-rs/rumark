//! 独立图片语法、引用与来源解析。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use rgpui::{Image, ImageFormat, Img, SharedUri, http_client::Uri, img};

use crate::net;

/// 扫描图片引用定义时的活动围栏代码块。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FenceSpec {
    ch: char,
    len: usize,
}

/// 抑制引用定义扫描的 HTML 块起始。
enum HtmlBlockStart {
    /// 以 `<!--` 开头的 HTML 注释。
    Comment,
    /// 关闭行为依赖标签的 HTML 标签块。
    Tag {
        name: String,
        self_closing: bool,
        closes_same_line: bool,
    },
}

/// 已解析的独立图片表达式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageSyntax {
    pub(crate) alt: String,
    pub(crate) target: ImageTarget,
}

/// 仅供原生表格单元格渲染使用的行内图片/文本片段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TableCellInlineImageSegment {
    Text(String),
    Image {
        markdown: String,
        syntax: ImageSyntax,
    },
}

/// 引用解析前的图片目标形式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImageTarget {
    /// 来自 `![alt](src "title")` 的直接图片目标。
    Direct { src: String, title: Option<String> },
    /// 来自 `![alt][label]` 的引用图片目标。
    Reference { label: String },
}

/// 引用式图片标签的全局引用定义。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageReferenceDefinition {
    pub(crate) src: String,
    pub(crate) title: Option<String>,
}

pub(crate) type ImageReferenceDefinitions = HashMap<String, ImageReferenceDefinition>;

/// 可供路径或网址加载的已解析图片目标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedImageTarget {
    pub(crate) src: String,
    pub(crate) title: Option<String>,
}

/// 经本地路径或远端网址分类后的具体图片来源。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImageResolvedSource {
    /// 尽量相对当前文档解析的文件系统路径。
    Local(PathBuf),
    /// 由 GPUI HTTP 客户端处理的 HTTP(S) 图片网址。
    Remote(SharedUri),
    /// 内嵌的 `data:` URI 图片（base64 解码后的字节，渲染时直供图片组件）。
    Embedded { format: ImageFormat, bytes: Vec<u8> },
}

/// 按 MIME 类型映射图片格式（内嵌 `data:` URI 解码与剪贴板图片复用）。
pub(crate) fn image_format_for_mime(mime: &str) -> Option<ImageFormat> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::Webp),
        "image/svg+xml" => Some(ImageFormat::Svg),
        "image/bmp" => Some(ImageFormat::Bmp),
        "image/tiff" => Some(ImageFormat::Tiff),
        "image/x-icon" | "image/vnd.microsoft.icon" => Some(ImageFormat::Ico),
        _ => None,
    }
}

/// 解析 `data:<mime>;base64,<payload>` 内嵌图片，成功返回格式与字节。
pub(crate) fn parse_embedded_data_uri(source: &str) -> Option<(ImageFormat, Vec<u8>)> {
    let payload = source.strip_prefix("data:")?;
    let (meta, data) = payload.split_once(',')?;
    let (mime, encoding) = meta.split_once(';')?;
    if !encoding.trim().eq_ignore_ascii_case("base64") {
        return None;
    }
    let format = image_format_for_mime(mime)?;
    if data.is_empty() {
        return None;
    }
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some((format, bytes))
}

impl ImageSyntax {
    /// 解析图片目标。
    pub(crate) fn resolve_target(
        &self,
        reference_definitions: &ImageReferenceDefinitions,
    ) -> Option<ResolvedImageTarget> {
        match &self.target {
            ImageTarget::Direct { src, title } => Some(ResolvedImageTarget {
                src: src.clone(),
                title: title.clone(),
            }),
            ImageTarget::Reference { label } => {
                let definition = reference_definitions.get(label)?;
                Some(ResolvedImageTarget {
                    src: definition.src.clone(),
                    title: definition.title.clone(),
                })
            }
        }
    }
}

/// 解析图片来源为本地、远端或内嵌 `data:` URI。
pub(crate) fn resolve_image_source(source: &str, base_dir: Option<&Path>) -> ImageResolvedSource {
    if net::is_remote_image_source(source) {
        return ImageResolvedSource::Remote(SharedUri::from(source.to_string()));
    }

    // 内嵌图片优先于路径解析：`data:` 不是合法文件路径，直接解码供渲染直用。
    if source.starts_with("data:")
        && let Some((format, bytes)) = parse_embedded_data_uri(source)
    {
        return ImageResolvedSource::Embedded { format, bytes };
    }

    let path = Path::new(source);
    if path.is_absolute() {
        return ImageResolvedSource::Local(path.to_path_buf());
    }

    let resolved = base_dir
        .map(|dir| dir.join(path))
        .unwrap_or_else(|| path.to_path_buf());
    ImageResolvedSource::Local(resolved)
}

/// 把已分类的图片来源转为图片组件（内嵌字节直供解码，避免走网络）。
pub(crate) fn img_for_resolved_source(source: ImageResolvedSource) -> Img {
    match source {
        ImageResolvedSource::Local(path) => img(path),
        ImageResolvedSource::Remote(uri) => img(uri),
        ImageResolvedSource::Embedded { format, bytes } => {
            img(Arc::new(Image::from_bytes(format, bytes)))
        }
    }
}

/// 解析独立图片语法。
pub(crate) fn parse_standalone_image(markdown: &str) -> Option<ImageSyntax> {
    if markdown.contains('\n') || markdown.contains('\r') {
        return None;
    }
    let markdown = markdown.trim();
    if markdown.is_empty() {
        return None;
    }
    if !markdown.starts_with("![") {
        return None;
    }

    let bytes = markdown.as_bytes();
    let mut alt_end = None;
    for (index, &byte) in bytes.iter().enumerate().skip(2) {
        if byte == b']' && !is_escaped(markdown, index) {
            alt_end = Some(index);
            break;
        }
    }
    let alt_end = alt_end?;

    let alt = unescape_ascii_punctuation(&markdown[2..alt_end]);
    match bytes.get(alt_end + 1) {
        Some(b'(') if markdown.ends_with(')') => {
            let inner = &markdown[alt_end + 2..markdown.len() - 1];
            let (src, title) = parse_image_target(inner)?;
            Some(ImageSyntax {
                alt,
                target: ImageTarget::Direct { src, title },
            })
        }
        Some(b'[') if markdown.ends_with(']') => {
            let raw_label = &markdown[alt_end + 2..markdown.len() - 1];
            let label_source = if raw_label.is_empty() {
                alt.as_str()
            } else {
                raw_label
            };
            let label = normalize_link_label(label_source)?;
            Some(ImageSyntax {
                alt,
                target: ImageTarget::Reference { label },
            })
        }
        None => {
            let label = normalize_link_label(&alt)?;
            Some(ImageSyntax {
                alt,
                target: ImageTarget::Reference { label },
            })
        }
        _ => None,
    }
}

/// 解析表格单元格内的行内图片片段。
pub(crate) fn parse_table_cell_inline_images(markdown: &str) -> Vec<TableCellInlineImageSegment> {
    let mut segments = Vec::new();
    let mut text_start = 0usize;
    let mut cursor = 0usize;
    let mut found_image = false;

    while cursor < markdown.len() {
        if markdown[cursor..].starts_with("![")
            && !is_escaped(markdown, cursor)
            && let Some((image_markdown, syntax, end)) = parse_inline_image_at(markdown, cursor)
        {
            if is_link_wrapped_inline_image(markdown, cursor, end) {
                cursor += markdown[cursor..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(1);
                continue;
            }

            if text_start < cursor {
                segments.push(TableCellInlineImageSegment::Text(
                    markdown[text_start..cursor].to_string(),
                ));
            }
            segments.push(TableCellInlineImageSegment::Image {
                markdown: image_markdown,
                syntax,
            });
            found_image = true;
            cursor = end;
            text_start = cursor;
            continue;
        }

        cursor += markdown[cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }

    if text_start < markdown.len() {
        segments.push(TableCellInlineImageSegment::Text(
            markdown[text_start..].to_string(),
        ));
    }

    if found_image {
        segments
    } else {
        vec![TableCellInlineImageSegment::Text(markdown.to_string())]
    }
}

/// 在指定位置解析行内图片。
fn parse_inline_image_at(markdown: &str, start: usize) -> Option<(String, ImageSyntax, usize)> {
    if !markdown[start..].starts_with("![") {
        return None;
    }

    let alt_end = find_unescaped_char(markdown, start + 2, b']')?;
    let alt = unescape_ascii_punctuation(&markdown[start + 2..alt_end]);
    let next = markdown.as_bytes().get(alt_end + 1).copied();

    match next {
        Some(b'(') => {
            let close = find_unescaped_char(markdown, alt_end + 2, b')')?;
            let inner = &markdown[alt_end + 2..close];
            let (src, title) = parse_image_target(inner)?;
            let end = close + 1;
            Some((
                markdown[start..end].to_string(),
                ImageSyntax {
                    alt,
                    target: ImageTarget::Direct { src, title },
                },
                end,
            ))
        }
        Some(b'[') => {
            let close = find_unescaped_char(markdown, alt_end + 2, b']')?;
            let raw_label = &markdown[alt_end + 2..close];
            let label_source = if raw_label.is_empty() {
                alt.as_str()
            } else {
                raw_label
            };
            let label = normalize_link_label(label_source)?;
            let end = close + 1;
            Some((
                markdown[start..end].to_string(),
                ImageSyntax {
                    alt,
                    target: ImageTarget::Reference { label },
                },
                end,
            ))
        }
        _ => {
            let label = normalize_link_label(&alt)?;
            let end = alt_end + 1;
            Some((
                markdown[start..end].to_string(),
                ImageSyntax {
                    alt,
                    target: ImageTarget::Reference { label },
                },
                end,
            ))
        }
    }
}

/// 判断行内图片是否被链接包裹。
fn is_link_wrapped_inline_image(markdown: &str, start: usize, end: usize) -> bool {
    let mut cursor = 0usize;
    let mut open_label = None;
    while cursor < start {
        let byte = markdown.as_bytes()[cursor];
        if byte == b'[' && !is_escaped(markdown, cursor) {
            open_label = Some(cursor);
        } else if byte == b']' && !is_escaped(markdown, cursor) {
            open_label = None;
        }
        cursor += markdown[cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }

    open_label.is_some() && markdown[end..].starts_with("](")
}

/// 解析文档中的图片引用定义。
pub(crate) fn parse_image_reference_definitions(markdown: &str) -> ImageReferenceDefinitions {
    let lines = markdown.split('\n').collect::<Vec<_>>();
    let normalized_lines = lines
        .iter()
        .map(|line| strip_reference_scan_container_prefixes(line).to_string())
        .collect::<Vec<_>>();
    let normalized_refs = normalized_lines
        .iter()
        .map(|line| line.as_str())
        .collect::<Vec<_>>();
    let mut definitions = ImageReferenceDefinitions::new();
    let mut index = 0usize;
    let mut active_fence = None;
    let mut active_html_tag: Option<String> = None;
    let mut active_html_comment = false;

    while index < lines.len() {
        let line = normalized_refs[index];

        if let Some(fence) = active_fence {
            if is_reference_scan_closing_fence(line, fence) {
                active_fence = None;
            }
            index += 1;
            continue;
        }

        if active_html_comment {
            if line.contains("-->") || line.trim().is_empty() {
                active_html_comment = false;
            }
            index += 1;
            continue;
        }

        if let Some(tag_name) = active_html_tag.clone() {
            if line.trim().is_empty()
                || parse_reference_scan_html_close_tag_name(line).as_deref() == Some(&tag_name)
            {
                active_html_tag = None;
            }
            index += 1;
            continue;
        }

        if let Some(fence) = parse_reference_scan_opening_fence(line) {
            if !is_reference_scan_closing_fence(line, fence) {
                active_fence = Some(fence);
            }
            index += 1;
            continue;
        }

        if let Some(html_start) = parse_reference_scan_html_block_start(line) {
            match html_start {
                HtmlBlockStart::Comment => {
                    if !line.contains("-->") {
                        active_html_comment = true;
                    }
                }
                HtmlBlockStart::Tag {
                    name,
                    self_closing,
                    closes_same_line,
                } => {
                    if !self_closing && !closes_same_line {
                        active_html_tag = Some(name);
                    }
                }
            }
            index += 1;
            continue;
        }

        let Some((label, definition, consumed)) =
            parse_image_reference_definition(&normalized_refs, index)
        else {
            index += 1;
            continue;
        };

        definitions.entry(label).or_insert(definition);
        index += consumed;
    }

    definitions
}

/// 规范化引用标签。
pub(crate) fn normalize_link_label(label: &str) -> Option<String> {
    // 单次拼接：遍历词一次，首词后补分隔符写入输出。避免先 `split_whitespace().collect::<Vec<_>>()` 分配中间数组再 `join` 二次拷贝。
    let mut normalized = String::with_capacity(label.len());
    for word in label.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_lowercase())
    }
}

/// 解析单条图片引用定义。
fn parse_image_reference_definition(
    lines: &[&str],
    start: usize,
) -> Option<(String, ImageReferenceDefinition, usize)> {
    let line = lines.get(start)?;
    let trimmed_end = line.trim_end();
    let leading_spaces = trimmed_end.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > 3 {
        return None;
    }

    let rest = &trimmed_end[leading_spaces..];
    if !rest.starts_with('[') {
        return None;
    }

    let label_end = find_unescaped_char(rest, 1, b']')?;
    if rest.as_bytes().get(label_end + 1) != Some(&b':') {
        return None;
    }

    let label = normalize_link_label(&rest[1..label_end])?;
    let mut target = rest[label_end + 2..].trim_start().to_string();
    let mut consumed = 1usize;

    if let Some(next_line) = lines.get(start + 1)
        && is_reference_definition_title_continuation(next_line)
    {
        if !target.is_empty() {
            target.push(' ');
        }
        target.push_str(next_line.trim());
        consumed += 1;
    }

    let (src, title) = parse_image_target(&target)?;
    Some((label, ImageReferenceDefinition { src, title }, consumed))
}

/// 去掉引用扫描中的容器前缀。
fn strip_reference_scan_container_prefixes(mut line: &str) -> &str {
    loop {
        let original = line;
        if let Some(rest) = strip_reference_scan_quote_prefix(line) {
            line = rest;
            continue;
        }
        if let Some(rest) = strip_reference_scan_list_marker(line) {
            line = rest;
            continue;
        }
        if line == original {
            return line;
        }
    }
}

/// 去掉引用扫描中的引用前缀。
fn strip_reference_scan_quote_prefix(line: &str) -> Option<&str> {
    let leading_spaces = line.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > 3 {
        return None;
    }

    let rest = &line[leading_spaces..];
    if !rest.starts_with('>') {
        return None;
    }

    Some(rest[1..].strip_prefix(' ').unwrap_or(&rest[1..]))
}

/// 去掉引用扫描中的列表标记。
fn strip_reference_scan_list_marker(line: &str) -> Option<&str> {
    let indent_bytes = line
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .map(char::len_utf8)
        .sum::<usize>();
    let rest = &line[indent_bytes..];

    if let Some(marker) = rest.chars().next()
        && matches!(marker, '-' | '*' | '+')
    {
        let after_marker = &rest[marker.len_utf8()..];
        return after_marker
            .strip_prefix(' ')
            .or_else(|| after_marker.strip_prefix('\t'));
    }

    let digit_len = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if !(1..=9).contains(&digit_len) {
        return None;
    }

    let marker = *rest.as_bytes().get(digit_len)?;
    if !matches!(marker, b'.' | b')') {
        return None;
    }

    let separator = *rest.as_bytes().get(digit_len + 1)?;
    if !matches!(separator, b' ' | b'\t') {
        return None;
    }

    Some(&rest[digit_len + 2..])
}

/// 解析引用扫描中的起始围栏。
fn parse_reference_scan_opening_fence(line: &str) -> Option<FenceSpec> {
    let trimmed = line.trim_end();
    let ch = trimmed.chars().next()?;
    if !matches!(ch, '`' | '~') {
        return None;
    }
    let len = trimmed.chars().take_while(|current| *current == ch).count();
    let rest = &trimmed[ch.len_utf8() * len..];
    if ch == '`' && rest.contains('`') {
        return None;
    }
    (len >= 3).then_some(FenceSpec { ch, len })
}

/// 判断是否为引用扫描中的结束围栏。
fn is_reference_scan_closing_fence(line: &str, opener: FenceSpec) -> bool {
    let trimmed = line.trim_end();
    if !trimmed.starts_with(opener.ch) {
        return false;
    }

    let run_len = trimmed
        .chars()
        .take_while(|current| *current == opener.ch)
        .count();
    run_len == opener.len && trimmed[opener.ch.len_utf8() * run_len..].trim().is_empty()
}

/// 解析引用扫描中的 HTML 块起始。
fn parse_reference_scan_html_block_start(line: &str) -> Option<HtmlBlockStart> {
    let rest = line.trim_start().trim_end();
    if rest.starts_with("<!--") {
        return Some(HtmlBlockStart::Comment);
    }

    let tagged = rest.strip_prefix('<')?;
    if tagged.starts_with('/') {
        return None;
    }

    let name_len = tagged
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .count();
    if name_len == 0 {
        return None;
    }

    let name = &tagged[..name_len];
    let suffix = &tagged[name_len..];
    let next = suffix.chars().next()?;
    if !matches!(next, '>' | ' ' | '\t' | '/') {
        return None;
    }

    Some(HtmlBlockStart::Tag {
        name: name.to_string(),
        self_closing: rest.ends_with("/>"),
        closes_same_line: rest.contains(&format!("</{name}>")),
    })
}

/// 解析引用扫描中的 HTML 结束标签名。
fn parse_reference_scan_html_close_tag_name(line: &str) -> Option<String> {
    let rest = line.trim_start().trim_end();
    let tagged = rest.strip_prefix("</")?;
    let name_len = tagged
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .count();
    if name_len == 0 {
        return None;
    }

    let name = &tagged[..name_len];
    let suffix = &tagged[name_len..];
    let next = suffix.chars().next()?;
    if !matches!(next, '>' | ' ' | '\t') {
        return None;
    }

    Some(name.to_string())
}

/// 解析图片目标与标题。
fn parse_image_target(inner: &str) -> Option<(String, Option<String>)> {
    if inner.is_empty() {
        return None;
    }

    if inner.ends_with('"') {
        let close_quote = inner.len() - 1;
        if !is_escaped(inner, close_quote)
            && let Some(open_quote) = find_open_title_quote(inner, close_quote)
        {
            let src = inner[..open_quote.saturating_sub(1)].trim_end();
            let title = inner[open_quote + 1..close_quote].to_string();
            if src.is_empty() {
                return None;
            }
            return Some((normalize_image_source(src), Some(title)));
        }
    }

    Some((normalize_image_source(inner), None))
}

/// 判断是否为引用定义的标题续行。
fn is_reference_definition_title_continuation(line: &str) -> bool {
    let indent_bytes = line
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .map(char::len_utf8)
        .sum::<usize>();
    if indent_bytes == 0 {
        return false;
    }

    let trimmed = line[indent_bytes..].trim();
    (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        || (trimmed.starts_with('(') && trimmed.ends_with(')'))
}

/// 查找标题起始引号。
fn find_open_title_quote(input: &str, close_quote: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    (0..close_quote).rev().find(|&index| {
        bytes[index] == b'"'
            && !is_escaped(input, index)
            && index > 0
            && bytes[index - 1].is_ascii_whitespace()
    })
}

/// 规范化图片来源。
fn normalize_image_source(source: &str) -> String {
    let source = unescape_ascii_punctuation(source);
    if source.starts_with('<')
        && source.ends_with('>')
        && Uri::from_str(&source[1..source.len() - 1]).is_ok()
    {
        source[1..source.len() - 1].to_string()
    } else {
        source
    }
}

/// 去掉 ASCII 标点转义。
fn unescape_ascii_punctuation(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek().is_some_and(|next| next.is_ascii_punctuation()) {
            output.push(chars.next().expect("peeked punctuation must exist"));
        } else {
            output.push(ch);
        }
    }
    output
}

/// 查找未转义字符。
fn find_unescaped_char(input: &str, start: usize, target: u8) -> Option<usize> {
    let bytes = input.as_bytes();
    (start..bytes.len()).find(|&index| bytes[index] == target && !is_escaped(input, index))
}

/// 判断指定索引是否被转义。
fn is_escaped(input: &str, index: usize) -> bool {
    if index == 0 {
        return false;
    }

    let bytes = input.as_bytes();
    let mut backslashes = 0usize;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        if bytes[cursor] == b'\\' {
            backslashes += 1;
        } else {
            break;
        }
    }
    backslashes % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::{
        ImageReferenceDefinition, ImageResolvedSource, ImageSyntax, ImageTarget,
        TableCellInlineImageSegment, normalize_link_label, parse_embedded_data_uri,
        parse_image_reference_definitions, parse_standalone_image, parse_table_cell_inline_images,
        resolve_image_source,
    };
    use std::path::Path;

    #[test]
    fn parses_standalone_image_without_title() {
        let parsed = parse_standalone_image("![alt](./img.png)").expect("image syntax");
        assert_eq!(parsed.alt, "alt");
        assert_eq!(
            parsed.target,
            ImageTarget::Direct {
                src: "./img.png".to_string(),
                title: None,
            }
        );
    }

    #[test]
    fn parses_standalone_image_with_surrounding_whitespace() {
        let three_space =
            parse_standalone_image("   ![alt](https://example.com/a.png)").expect("image syntax");
        assert_eq!(three_space.alt, "alt");
        assert_eq!(
            three_space.target,
            ImageTarget::Direct {
                src: "https://example.com/a.png".to_string(),
                title: None,
            }
        );

        let deeply_indented =
            parse_standalone_image("        ![alt](https://example.com/a.png)   ")
                .expect("image syntax");
        assert_eq!(deeply_indented, three_space);
        assert!(parse_standalone_image("   text ![alt](x)").is_none());
        assert!(parse_standalone_image("   ![alt](x)\n").is_none());
    }

    #[test]
    fn parses_image_target_with_escaped_punctuation_in_source() {
        let parsed = parse_standalone_image("![alt](https://example.com/typera\\_picgo/img.png)")
            .expect("image syntax");
        assert_eq!(
            parsed.target,
            ImageTarget::Direct {
                src: "https://example.com/typera_picgo/img.png".to_string(),
                title: None,
            }
        );
    }

    #[test]
    fn parses_standalone_image_with_underscores_in_alt_and_source() {
        let parsed = parse_standalone_image(
            "![1.1_进制转换例子](./NetworkEngineerSummer.assets/1.1_进制转换例子.jpg)",
        )
        .expect("image syntax");

        assert_eq!(parsed.alt, "1.1_进制转换例子");
        assert_eq!(
            parsed.target,
            ImageTarget::Direct {
                src: "./NetworkEngineerSummer.assets/1.1_进制转换例子.jpg".to_string(),
                title: None,
            }
        );
    }

    #[test]
    fn parses_standalone_image_with_title() {
        let parsed =
            parse_standalone_image("![alt](./img.png \"caption text\")").expect("image syntax");
        assert_eq!(parsed.alt, "alt");
        assert_eq!(
            parsed.target,
            ImageTarget::Direct {
                src: "./img.png".to_string(),
                title: Some("caption text".to_string()),
            }
        );
    }

    #[test]
    fn parses_reference_style_standalone_image() {
        let parsed =
            parse_standalone_image("![reference image][ref-image]").expect("reference image");
        assert_eq!(parsed.alt, "reference image");
        assert_eq!(
            parsed.target,
            ImageTarget::Reference {
                label: "ref-image".to_string(),
            }
        );
    }

    #[test]
    fn parses_collapsed_reference_style_standalone_image() {
        let parsed =
            parse_standalone_image("![collapsed image][]").expect("collapsed reference image");
        assert_eq!(parsed.alt, "collapsed image");
        assert_eq!(
            parsed.target,
            ImageTarget::Reference {
                label: "collapsed image".to_string(),
            }
        );
    }

    #[test]
    fn parses_shortcut_reference_style_standalone_image() {
        let parsed = parse_standalone_image("![shortcut image]").expect("shortcut reference image");
        assert_eq!(parsed.alt, "shortcut image");
        assert_eq!(
            parsed.target,
            ImageTarget::Reference {
                label: "shortcut image".to_string(),
            }
        );
    }

    #[test]
    fn rejects_mixed_or_wrapped_image_syntax() {
        assert!(parse_standalone_image("text ![alt](./img.png)").is_none());
        assert!(parse_standalone_image("[![alt](./img.png)](https://example.com)").is_none());
        assert!(parse_standalone_image("![][]").is_none());
        assert!(parse_standalone_image("![]").is_none());
    }

    #[test]
    fn parses_table_cell_inline_image_segments() {
        let segments = parse_table_cell_inline_images("image ![alt](https://example.com/x.png)");
        assert_eq!(
            segments,
            vec![
                TableCellInlineImageSegment::Text("image ".to_string()),
                TableCellInlineImageSegment::Image {
                    markdown: "![alt](https://example.com/x.png)".to_string(),
                    syntax: ImageSyntax {
                        alt: "alt".to_string(),
                        target: ImageTarget::Direct {
                            src: "https://example.com/x.png".to_string(),
                            title: None,
                        },
                    },
                },
            ]
        );
    }

    #[test]
    fn parses_multiple_table_cell_inline_images() {
        let segments = parse_table_cell_inline_images("![a](x.png) and ![b](y.png)");
        assert_eq!(segments.len(), 3);
        assert!(matches!(
            &segments[0],
            TableCellInlineImageSegment::Image { syntax, .. } if syntax.alt == "a"
        ));
        assert_eq!(
            segments[1],
            TableCellInlineImageSegment::Text(" and ".to_string())
        );
        assert!(matches!(
            &segments[2],
            TableCellInlineImageSegment::Image { syntax, .. } if syntax.alt == "b"
        ));
    }

    #[test]
    fn table_cell_inline_image_segments_keep_escaped_wrapped_and_broken_text() {
        assert_eq!(
            parse_table_cell_inline_images(r"\![alt](x.png)"),
            vec![TableCellInlineImageSegment::Text(
                r"\![alt](x.png)".to_string()
            )]
        );
        assert_eq!(
            parse_table_cell_inline_images("[![alt](x.png)](https://example.com)"),
            vec![TableCellInlineImageSegment::Text(
                "[![alt](x.png)](https://example.com)".to_string()
            )]
        );
        assert_eq!(
            parse_table_cell_inline_images("broken ![alt](x.png"),
            vec![TableCellInlineImageSegment::Text(
                "broken ![alt](x.png".to_string()
            )]
        );
    }

    #[test]
    fn table_cell_inline_reference_images_resolve() {
        let definitions = parse_image_reference_definitions(
            "[ref]: ./ref.png\n[collapsed]: ./collapsed.png\n[shortcut]: ./shortcut.png",
        );
        let segments = parse_table_cell_inline_images("![full][ref] ![collapsed][] ![shortcut]");
        let resolved = segments
            .iter()
            .filter_map(|segment| match segment {
                TableCellInlineImageSegment::Image { syntax, .. } => {
                    syntax.resolve_target(&definitions).map(|target| target.src)
                }
                TableCellInlineImageSegment::Text(_) => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            resolved,
            vec!["./ref.png", "./collapsed.png", "./shortcut.png"]
        );
    }

    #[test]
    fn parses_image_reference_definitions_with_title_and_first_wins() {
        let definitions = parse_image_reference_definitions(
            "[Ref Image]: ./first.png \"Caption\"\n[ref image]: ./second.png".trim(),
        );
        assert_eq!(
            definitions.get("ref image"),
            Some(&ImageReferenceDefinition {
                src: "./first.png".to_string(),
                title: Some("Caption".to_string()),
            })
        );
    }

    #[test]
    fn normalizes_reference_labels_case_and_whitespace_insensitively() {
        assert_eq!(
            normalize_link_label("  Ref\t Image  "),
            Some("ref image".to_string())
        );
    }

    #[test]
    fn resolves_reference_targets() {
        let syntax = ImageSyntax {
            alt: "alt".to_string(),
            target: ImageTarget::Reference {
                label: "ref-image".to_string(),
            },
        };
        let definitions = parse_image_reference_definitions("[ref-image]: ./img.png \"Caption\"");
        let resolved = syntax
            .resolve_target(&definitions)
            .expect("resolved target");
        assert_eq!(resolved.src, "./img.png");
        assert_eq!(resolved.title.as_deref(), Some("Caption"));
    }

    #[test]
    fn resolves_collapsed_and_shortcut_reference_images() {
        let definitions = parse_image_reference_definitions(
            "[collapsed image]: ./collapsed.png\n[shortcut image]: ./shortcut.png",
        );

        let collapsed = parse_standalone_image("![collapsed image][]")
            .expect("collapsed reference image")
            .resolve_target(&definitions)
            .expect("resolved collapsed image");
        assert_eq!(collapsed.src, "./collapsed.png");

        let shortcut = parse_standalone_image("![shortcut image]")
            .expect("shortcut reference image")
            .resolve_target(&definitions)
            .expect("resolved shortcut image");
        assert_eq!(shortcut.src, "./shortcut.png");
    }

    #[test]
    fn unresolved_reference_target_returns_none() {
        let syntax = ImageSyntax {
            alt: "alt".to_string(),
            target: ImageTarget::Reference {
                label: "missing".to_string(),
            },
        };
        assert!(
            syntax
                .resolve_target(&parse_image_reference_definitions("[ref]: ./img.png"))
                .is_none()
        );
    }

    #[test]
    fn resolves_relative_and_remote_sources() {
        let local = resolve_image_source("images/pic.png", Some(Path::new("D:/docs")));
        assert_eq!(
            local,
            ImageResolvedSource::Local(Path::new("D:/docs").join("images/pic.png"))
        );

        let remote = resolve_image_source("https://example.com/img.gif", None);
        match remote {
            ImageResolvedSource::Remote(uri) => {
                assert_eq!(uri.to_string(), "https://example.com/img.gif");
            }
            other => panic!("expected remote source, got {other:?}"),
        }
    }

    #[test]
    fn parses_container_scoped_reference_definitions_in_source_order() {
        let definitions = parse_image_reference_definitions(
            [
                "> [quoted ref]: ./quoted.png \"Quoted\"",
                "- [list ref]: ./list.png",
                "1) [ordered ref]: ./ordered.png",
                "> > [quoted ref]: ./ignored.png",
            ]
            .join("\n")
            .as_str(),
        );

        assert_eq!(
            definitions.get("quoted ref"),
            Some(&ImageReferenceDefinition {
                src: "./quoted.png".to_string(),
                title: Some("Quoted".to_string()),
            })
        );
        assert_eq!(
            definitions.get("list ref"),
            Some(&ImageReferenceDefinition {
                src: "./list.png".to_string(),
                title: None,
            })
        );
        assert_eq!(
            definitions.get("ordered ref"),
            Some(&ImageReferenceDefinition {
                src: "./ordered.png".to_string(),
                title: None,
            })
        );
    }

    #[test]
    fn ignores_reference_definitions_inside_code_fences_and_html_blocks() {
        let definitions = parse_image_reference_definitions(
            [
                "> ```md",
                "> [code ref]: ./ignored-code.png",
                "> ```",
                "",
                "<div>",
                "[html ref]: ./ignored-html.png",
                "</div>",
                "",
                "> [live ref]: ./real.png",
            ]
            .join("\n")
            .as_str(),
        );

        assert!(!definitions.contains_key("code ref"));
        assert!(!definitions.contains_key("html ref"));
        assert_eq!(
            definitions.get("live ref"),
            Some(&ImageReferenceDefinition {
                src: "./real.png".to_string(),
                title: None,
            })
        );
    }

    #[test]
    fn embedded_data_uri_decodes_to_bytes() {
        // 1x1 透明 PNG 的最小载荷。
        let uri = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let (format, bytes) = parse_embedded_data_uri(uri).expect("data uri");
        assert_eq!(format, rgpui::ImageFormat::Png);
        assert!(!bytes.is_empty());
        match resolve_image_source(uri, None) {
            ImageResolvedSource::Embedded { format, bytes } => {
                assert_eq!(format, rgpui::ImageFormat::Png);
                assert!(!bytes.is_empty());
            }
            other => panic!("expected embedded source, got {other:?}"),
        }
        assert!(parse_embedded_data_uri("data:image/png;base64,").is_none());
        assert!(parse_embedded_data_uri("./img.png").is_none());
    }
}
