//! 渲染模式外部粘贴的分类辅助。
//!
//! 文件导入保持 CommonMark 段落语义，但粘贴应更贴近用户的视觉预期。纯物理行拆分为独立块，结构化 Markdown 与块级/风险 HTML 交给文档块构建器处理。

use super::html::is_inline_tag;
use super::table::collect_pipeless_table;

/// 判断多行纯文本粘贴是否应拆分为多块。
pub(crate) fn should_split_plain_multiline_paste(lines: &[String]) -> bool {
    // A pipeless GFM table reads cell-by-cell as plain lines, so detect the
    // header-plus-delimiter shape explicitly and leave the whole paste to the
    // block builder instead of splitting it into one paragraph per row.
    if (0..lines.len()).any(|start| collect_pipeless_table(lines, start).is_some()) {
        return false;
    }

    lines.iter().filter(|line| !line.trim().is_empty()).count() >= 2
        && lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .all(|line| is_plain_inline_paste_line(line))
}

/// 判断单行是否为纯行内粘贴行。
fn is_plain_inline_paste_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return true;
    }

    if trimmed.starts_with('<') {
        return is_closed_inline_html_line(trimmed);
    }

    !(trimmed.starts_with("```")
        || trimmed.starts_with("~~~")
        || trimmed.starts_with('>')
        || trimmed.starts_with('|')
        || trimmed.starts_with("$$")
        || trimmed.starts_with('\t')
        || line.starts_with("    ")
        || is_simple_list_marker(trimmed)
        || is_simple_atx_heading(trimmed)
        || is_simple_separator(trimmed)
        || is_setext_underline(trimmed)
        || is_simple_reference_definition(trimmed))
}

/// 匹配 Setext 下划线行（`=====` 或 `-----`）。`-` 形已由 `is_simple_separator` 覆盖，但 `=` 并非主题分隔线，若无此项，`text` + `=====` 会被拆为两个纯段落，而非路由给结构化导入器生成一级标题。
fn is_setext_underline(trimmed: &str) -> bool {
    let core = trimmed.trim_end();
    core.len() >= 3
        && (core.bytes().all(|byte| byte == b'=') || core.bytes().all(|byte| byte == b'-'))
}

/// 判断行首闭合行内 HTML 是否可按纯文本粘贴。
fn is_closed_inline_html_line(trimmed: &str) -> bool {
    let Some(name) = leading_html_tag_name(trimmed) else {
        return false;
    };

    // A closed safe inline tag at column 0 is still paragraph content. Block
    // HTML, risky children, and unclosed tags must keep the conservative path.
    let lower = trimmed.to_ascii_lowercase();
    is_inline_tag(&name)
        && !lower.contains("<script")
        && !lower.contains("<style")
        && lower.contains(&format!("</{name}>"))
}

/// 取行首 HTML 标签名。
fn leading_html_tag_name(trimmed: &str) -> Option<String> {
    let tagged = trimmed.strip_prefix('<')?;
    if tagged.starts_with('/') || tagged.starts_with('!') || tagged.starts_with('?') {
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
    matches!(next, '>' | ' ' | '\t' | '/').then(|| name.to_ascii_lowercase())
}

/// 判断是否为简单列表标记。
fn is_simple_list_marker(trimmed: &str) -> bool {
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'-' | b'*' | b'+') && bytes[1].is_ascii_whitespace()
    {
        return true;
    }

    let Some(marker_end) = trimmed.find(['.', ')']) else {
        return false;
    };
    marker_end > 0
        && marker_end <= 9
        && trimmed[..marker_end]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        && trimmed
            .as_bytes()
            .get(marker_end + 1)
            .is_some_and(|byte| byte.is_ascii_whitespace())
}

/// 判断是否为简单 ATX 标题。
fn is_simple_atx_heading(trimmed: &str) -> bool {
    let marker_count = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    (1..=6).contains(&marker_count)
        && trimmed
            .as_bytes()
            .get(marker_count)
            .is_some_and(|byte| byte.is_ascii_whitespace())
}

/// 判断是否为简单分隔线。
fn is_simple_separator(trimmed: &str) -> bool {
    let without_spaces = trimmed
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    without_spaces.len() >= 3
        && without_spaces.iter().all(|byte| *byte == without_spaces[0])
        && matches!(without_spaces[0], b'-' | b'*' | b'_')
}

/// 判断是否为简单引用定义。
fn is_simple_reference_definition(trimmed: &str) -> bool {
    trimmed.starts_with('[') && trimmed.contains("]:")
}

#[cfg(test)]
mod tests {
    use super::should_split_plain_multiline_paste;

    #[test]
    fn accepts_plain_lines_with_script_syntax() {
        let lines = vec![
            "H~2~O".to_string(),
            "CO<sub>2</sub>".to_string(),
            "x<sup>n</sup>".to_string(),
        ];

        assert!(should_split_plain_multiline_paste(&lines));
    }

    #[test]
    fn accepts_closed_safe_inline_html_at_line_start() {
        let lines = vec![
            "<sub>2</sub>".to_string(),
            "<sup>n</sup>".to_string(),
            "<span style=\"color:red\">x</span>".to_string(),
            "<strong>y</strong>".to_string(),
        ];

        assert!(should_split_plain_multiline_paste(&lines));
    }

    #[test]
    fn rejects_block_or_unclosed_html_at_line_start() {
        let lines = vec!["<div>x</div>".to_string(), "<p>y</p>".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));

        let lines = vec!["<script>x</script>".to_string(), "<sup>n</sup>".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));

        let lines = vec!["<style>x</style>".to_string(), "<sup>n</sup>".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));

        let lines = vec!["<span>x".to_string(), "<sup>n</sup>".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));
    }

    #[test]
    fn rejects_structural_markdown() {
        let lines = vec!["```mermaid".to_string(), "flowchart LR".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));

        let lines = vec!["- item".to_string(), "- next".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));

        let lines = vec!["| A |".to_string(), "| --- |".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));

        let lines = vec![
            "```rust".to_string(),
            "fn main() {}".to_string(),
            "```".to_string(),
        ];
        assert!(!should_split_plain_multiline_paste(&lines));

        let lines = vec!["> quote".to_string(), "> more".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));

        let lines = vec!["# Title".to_string(), "body".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));
    }

    #[test]
    fn rejects_setext_underline_pairs() {
        // "=" underline must route to the structural importer (-> H1), like the
        // "-" underline (-> H2) already did, rather than splitting into two
        // plain paragraphs.
        let lines = vec!["Title".to_string(), "=====".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));

        let lines = vec!["Title".to_string(), "-----".to_string()];
        assert!(!should_split_plain_multiline_paste(&lines));
    }

    #[test]
    fn rejects_pipeless_table() {
        // A pipeless GFM table has no leading `|`, so its rows look like plain
        // lines; the header-plus-delimiter shape must still route to the block
        // builder instead of becoming one paragraph per row.
        let lines = vec![
            "Header 1 | Header 2 | Header 3".to_string(),
            "-------- | -------- | --------".to_string(),
            "Cell 1   | Cell 2   | Cell 3".to_string(),
        ];
        assert!(!should_split_plain_multiline_paste(&lines));

        // Prose with a stray `|` and no delimiter row still splits normally.
        let lines = vec![
            "see foo | bar for details".to_string(),
            "and another | line here".to_string(),
        ];
        assert!(should_split_plain_multiline_paste(&lines));
    }
}
