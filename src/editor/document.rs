//! Markdown 到编辑器树的反序列化。
//!
//! 原生 Markdown 会被解析为拾墨可安全编辑的原生块结构子集；
//! 超出当前运行时模型的语法会保留为源码块，确保往返无损。

use rgpui::*;

use super::Editor;
use crate::components::{
    BlockKind, BlockRecord, CalloutVariant, FenceOpening, InlineTextTree,
    parse_footnote_definition_head,
};
use crate::components::{HtmlSafetyClass, parse_html_document};
use crate::components::{
    collect_pipeless_table, collect_root_table_candidate, collect_table_candidate,
    is_root_table_candidate_line, is_table_candidate_line, parse_root_table_region,
    parse_standalone_image, parse_table_region,
};
use crate::components::{is_mermaid_info_string, parse_display_math_source};

/// 解析出的围栏代码块起始标记元数据。
///
/// 起始围栏同时记录标记字符与其连续长度，
/// 只有匹配的结束围栏才能终结该代码块。
type FenceSpec = FenceOpening;

/// Markdown 导入器识别的 HTML 块起始形态。
enum HtmlBlockStart {
    /// 以 `<!--` 开头的 HTML 注释区。
    Comment,
    /// HTML 标签块，结束行为取决于标签形态。
    Tag {
        name: String,
        self_closing: bool,
        closes_same_line: bool,
    },
}

/// 从单行源码解析出的有序/无序列表标记。
#[derive(Clone)]
struct ListMarker {
    kind: BlockKind,
    indent_columns: usize,
    content_indent_columns: usize,
    text: String,
}

/// 去掉围栏行最多三空格的缩进，超限则不是围栏。
fn strip_fence_indent(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    (indent <= 3).then_some(&line[indent..])
}

/// 从起始行向后收集到空行处的下标。
fn collect_until_blank_line(lines: &[String], start: usize) -> usize {
    let mut index = start + 1;
    while index < lines.len() && !lines[index].trim().is_empty() {
        index += 1;
    }
    index
}

/// 收集 HTML 回退区的结束下标（遇空行或新块起点停止）。
fn collect_html_fallback_region(lines: &[String], start: usize) -> usize {
    let mut index = start + 1;
    while index < lines.len() {
        if lines[index].trim().is_empty()
            || looks_like_root_block_start(lines, index)
            || parse_standalone_image(&lines[index]).is_some()
        {
            break;
        }
        index += 1;
    }
    index
}

/// 检测文本中未闭合的行内代码反引号串长度。
fn pending_inline_code_run_len(markdown: &str) -> Option<usize> {
    let mut open_run_len = None;
    let mut chars = markdown.char_indices().peekable();

    while let Some((_, ch)) = chars.next() {
        if open_run_len.is_none() && ch == '\\' {
            let _ = chars.next();
            continue;
        }

        if ch != '`' {
            continue;
        }

        let mut run_len = 1usize;
        while chars.peek().is_some_and(|(_, ch)| *ch == '`') {
            let _ = chars.next();
            run_len += 1;
        }

        if open_run_len == Some(run_len) {
            open_run_len = None;
        } else if open_run_len.is_none() {
            open_run_len = Some(run_len);
        }
    }

    open_run_len
}

/// 判断行内是否含指定长度的反引号串。
fn line_contains_matching_backtick_run(line: &str, run_len: usize) -> bool {
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '`' {
            continue;
        }

        let mut current_run_len = 1usize;
        while chars.peek().is_some_and(|ch| *ch == '`') {
            let _ = chars.next();
            current_run_len += 1;
        }

        if current_run_len == run_len {
            return true;
        }
    }

    false
}

/// 判断段落能否跨越空行边界延续（未闭合行内代码可延续）。
fn paragraph_can_continue_through_boundary(
    paragraph_lines: &[String],
    lines: &[String],
    boundary_index: usize,
) -> bool {
    let Some(run_len) = pending_inline_code_run_len(&paragraph_lines.join("\n")) else {
        return false;
    };

    lines[boundary_index..]
        .iter()
        .any(|line| line_contains_matching_backtick_run(line, run_len))
}

/// 解析行首的围栏代码块起始标记。
fn parse_opening_fence(line: &str) -> Option<FenceSpec> {
    BlockKind::parse_fence_opening(strip_fence_indent(line)?.trim_end())
}

/// 判断该行是否为匹配的围栏结束标记。
fn is_closing_fence(line: &str, opener: &FenceSpec) -> bool {
    let Some(trimmed) = strip_fence_indent(line).map(str::trim_end) else {
        return false;
    };
    if !trimmed.starts_with(opener.ch) {
        return false;
    }
    let run_len = trimmed.chars().take_while(|&c| c == opener.ch).count();
    if run_len != opener.len {
        return false;
    }
    trimmed[opener.ch.len_utf8() * run_len..].trim().is_empty()
}

/// 向后查找与起始围栏匹配的结束围栏下标。
fn find_matching_closing_fence(
    lines: &[String],
    start_index: usize,
    opener: &FenceSpec,
) -> Option<usize> {
    for (index, line) in lines.iter().enumerate().skip(start_index + 1) {
        // 围栏块在首个匹配围栏处结束（与 CommonMark 一致）。
        // 若向后找更晚的围栏（之前的行为），起始围栏会吞掉后续
        // 以裸围栏结尾的块并在往返时损坏它们（issue #58）。裸结束
        // 围栏与空起始围栏无法区分，因此首个匹配是唯一的明确规则。
        if is_closing_fence(line, opener) {
            return Some(index);
        }

        // 带信息串的起始围栏不可能是结束围栏，因此先遇到它
        // 说明该块从未闭合，保持未匹配状态。
        if parse_opening_fence(line)
            .as_ref()
            .and_then(|fence| fence.language.as_ref())
            .is_some()
        {
            break;
        }
    }

    None
}

/// 计算行首缩进的列数与字节数（制表符按四列折算）。
fn leading_indent_columns_and_bytes(line: &str) -> (usize, usize) {
    let mut columns = 0usize;
    let mut bytes = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' => {
                columns += 1;
                bytes += 1;
            }
            '\t' => {
                columns += 4 - (columns % 4);
                bytes += 1;
            }
            _ => break,
        }
    }
    (columns, bytes)
}

/// 去掉缩进代码块的一级缩进前缀。
fn strip_indented_code_prefix(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix('\t') {
        Some(rest)
    } else {
        line.strip_prefix("    ")
    }
}

/// 计算字符串的显示列宽（制表符按四列折算）。
fn display_columns(value: &str) -> usize {
    let mut columns = 0usize;
    for ch in value.chars() {
        match ch {
            '\t' => columns += 4 - (columns % 4),
            _ => columns += 1,
        }
    }
    columns
}

/// 去掉行首指定列数的缩进，列数不足则返回空。
fn strip_leading_columns(line: &str, columns: usize) -> Option<&str> {
    if columns == 0 {
        return Some(line);
    }
    if line.trim().is_empty() {
        return Some("");
    }

    let mut consumed_columns = 0usize;
    for (idx, ch) in line.char_indices() {
        let bytes_after_char = idx + ch.len_utf8();
        match ch {
            ' ' => {
                consumed_columns += 1;
            }
            '\t' => {
                consumed_columns += 4 - (consumed_columns % 4);
            }
            _ => break,
        }

        if consumed_columns >= columns {
            return Some(&line[bytes_after_char..]);
        }
    }

    None
}

/// 对多行统一去掉指定列数的缩进。
fn dedent_lines(lines: &[String], columns: usize) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            strip_leading_columns(line, columns)
                .unwrap_or(line.as_str())
                .to_string()
        })
        .collect()
}

/// 解析单行的列表标记（含任务列表勾选状态）。
fn parse_list_marker(line: &str) -> Option<ListMarker> {
    let (indent_columns, indent_bytes) = leading_indent_columns_and_bytes(line);
    let rest = &line[indent_bytes..];

    if let Some(marker) = rest.chars().next()
        && matches!(marker, '-' | '*' | '+')
    {
        let after_marker = &rest[marker.len_utf8()..];
        let separator_len = after_marker
            .chars()
            .next()
            .filter(|ch| matches!(ch, ' ' | '\t'))
            .map(char::len_utf8)?;
        let text = after_marker
            .strip_prefix(' ')
            .or_else(|| after_marker.strip_prefix('\t'))?;
        let (kind, text) = if let Some((checked, prefix_len)) = BlockKind::parse_task_prefix(text) {
            (
                BlockKind::TaskListItem { checked },
                text[prefix_len..].to_string(),
            )
        } else {
            (BlockKind::BulletedListItem, text.to_string())
        };
        return Some(ListMarker {
            kind,
            indent_columns,
            content_indent_columns: display_columns(
                &line[..indent_bytes + marker.len_utf8() + separator_len],
            ),
            text,
        });
    }

    let (digit_len, marker_len, text) = parse_ordered_list_marker(rest)?;
    Some(ListMarker {
        kind: BlockKind::NumberedListItem,
        indent_columns,
        content_indent_columns: display_columns(&line[..indent_bytes + digit_len + marker_len]),
        text: text.to_string(),
    })
}

/// 解析有序列表标记的数字长度、符号长度与正文。
fn parse_ordered_list_marker(rest: &str) -> Option<(usize, usize, &str)> {
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

    Some((digit_len, 2, &rest[digit_len + 2..]))
}

/// 去掉引用行的一层 `>` 前缀，非法则返回空。
fn strip_one_quote_level(line: &str) -> Option<String> {
    let leading_spaces = line.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > 3 {
        return None;
    }

    let rest = &line[leading_spaces..];
    if !rest.starts_with('>') {
        return None;
    }

    Some(
        rest[1..]
            .strip_prefix(' ')
            .unwrap_or(&rest[1..])
            .to_string(),
    )
}

/// 判断该行是否为引用块起始行。
fn is_quote_start(line: &str) -> bool {
    let trimmed_end = line.trim_end();
    let leading_spaces = trimmed_end.bytes().take_while(|b| *b == b' ').count();
    leading_spaces <= 3 && trimmed_end[leading_spaces..].starts_with('>')
}

/// 判断该行是否为引用定义起始行。
fn is_reference_definition_start(line: &str) -> bool {
    let trimmed_end = line.trim_end();
    let leading_spaces = trimmed_end.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > 3 {
        return false;
    }

    let rest = &trimmed_end[leading_spaces..];
    let Some(label_end) = rest.find("]:") else {
        return false;
    };
    rest.starts_with('[') && label_end > 1
}

/// 判断该行是否为脚注定义起始行。
fn is_footnote_definition_start(line: &str) -> bool {
    let trimmed_end = line.trim_end();
    let leading_spaces = trimmed_end.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > 3 {
        return false;
    }

    let rest = &trimmed_end[leading_spaces..];
    let Some(label_end) = rest.find("]:") else {
        return false;
    };
    rest.starts_with("[^") && label_end > 2
}

/// 判断该行是否为引用定义标题的续行。
fn is_reference_definition_title_continuation(line: &str) -> bool {
    let (_, indent_bytes) = leading_indent_columns_and_bytes(line);
    if indent_bytes == 0 {
        return false;
    }

    let trimmed = line[indent_bytes..].trim();
    (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        || (trimmed.starts_with('(') && trimmed.ends_with(')'))
}

/// 判断该行是否为块级 HTML 起始行。
fn is_block_html_start(line: &str) -> bool {
    parse_html_block_start(line).is_some()
}

/// 收集闭合 HTML 注释区的结束下标，未闭合则返回空。
fn collect_closed_html_comment_region(lines: &[String], start: usize) -> Option<usize> {
    match parse_html_block_start(&lines[start])? {
        HtmlBlockStart::Comment => {}
        HtmlBlockStart::Tag { .. } => return None,
    }

    if lines[start].contains("-->") {
        return Some(start + 1);
    }

    let mut index = start + 1;
    while index < lines.len() {
        if lines[index].contains("-->") {
            return Some(index + 1);
        }
        index += 1;
    }

    None
}

/// 收集块级 HTML 区的结束下标（含标签配对与回退）。
fn collect_block_html_region(lines: &[String], start: usize) -> usize {
    match parse_html_block_start(&lines[start]) {
        Some(HtmlBlockStart::Comment) => collect_closed_html_comment_region(lines, start)
            .unwrap_or_else(|| collect_html_fallback_region(lines, start)),
        Some(HtmlBlockStart::Tag {
            name,
            self_closing,
            closes_same_line,
        }) => {
            if self_closing || closes_same_line {
                return start + 1;
            }

            let mut depth = 1usize;
            let mut index = start + 1;
            while index < lines.len() {
                if let Some(HtmlBlockStart::Tag {
                    name: nested_name,
                    self_closing,
                    closes_same_line,
                }) = parse_html_block_start(&lines[index])
                    && nested_name == name
                    && !self_closing
                    && !closes_same_line
                {
                    depth += 1;
                }

                if let Some(close_name) = parse_html_close_tag_name(&lines[index])
                    && close_name == name
                {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return index + 1;
                    }
                }

                index += 1;
            }
            collect_html_fallback_region(lines, start)
        }
        None => collect_until_blank_line(lines, start),
    }
}

/// 收集引用定义区（含标题续行）的结束下标。
fn collect_reference_definition_region(lines: &[String], start: usize) -> usize {
    let mut index = start + 1;
    while index < lines.len() && is_reference_definition_title_continuation(&lines[index]) {
        index += 1;
    }
    index
}

/// 收集脚注定义区的结束下标。
fn collect_footnote_definition_region(lines: &[String], start: usize) -> usize {
    let mut index = start + 1;
    while index < lines.len() {
        let line = &lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }

        let (indent_columns, _) = leading_indent_columns_and_bytes(line);
        if indent_columns > 0 {
            index += 1;
            continue;
        }

        break;
    }
    index
}

/// 判断该行是否为块级数学公式起始行。
fn is_display_math_start(line: &str) -> bool {
    strip_fence_indent(line)
        .map(str::trim_end)
        .is_some_and(|rest| rest.starts_with("$$"))
}

/// 收集块级数学公式区的结束下标。
fn collect_display_math_region(lines: &[String], start: usize) -> usize {
    let opener = strip_fence_indent(&lines[start])
        .map(str::trim_end)
        .unwrap_or_default();
    if opener != "$$" && opener[2..].contains("$$") {
        return start + 1;
    }

    let mut index = start + 1;
    while index < lines.len() {
        if lines[index].trim() == "$$" {
            return index + 1;
        }

        if lines[index].trim().is_empty() {
            let mut lookahead = index + 1;
            while lookahead < lines.len() && lines[lookahead].trim().is_empty() {
                lookahead += 1;
            }

            if lookahead >= lines.len() || looks_like_root_block_start(lines, lookahead) {
                return lookahead;
            }
        }

        index += 1;
    }

    lines.len()
}

/// 解析块级 HTML 起始标记的形态。
fn parse_html_block_start(line: &str) -> Option<HtmlBlockStart> {
    let rest = strip_fence_indent(line)?.trim_end();
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
        self_closing: rest.ends_with("/>") || is_html_void_block_tag(name),
        closes_same_line: rest.contains(&format!("</{name}>")),
    })
}

/// 判断标签是否为自闭合的空元素标签。
fn is_html_void_block_tag(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "br" | "hr" | "img")
}

/// 解析行内 HTML 结束标签的标签名。
fn parse_html_close_tag_name(line: &str) -> Option<String> {
    let rest = strip_fence_indent(line)?.trim_end();
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

/// 收集连续引用行的原始区域结束下标。
fn collect_quote_raw_region(lines: &[String], start: usize) -> usize {
    let mut index = start;
    while index < lines.len() {
        let line = &lines[index];
        if line.trim().is_empty() || !is_quote_start(line) {
            break;
        }
        index += 1;
    }
    index
}

/// 判断引用内该行是否为原生模型不支持的结构起始。
fn quote_content_starts_unsupported(lines: &[String], index: usize) -> bool {
    let line = &lines[index];
    is_block_html_start(line)
        || is_footnote_definition_start(line)
        || is_reference_definition_start(line)
        || is_root_table_candidate_line(line)
        || is_display_math_start(line)
        || BlockKind::parse_separator(line)
        || lines
            .get(index + 1)
            .and_then(|next| BlockKind::parse_setext_rule(next))
            .is_some()
}

/// 收集引用内不支持结构的区域结束下标。
fn collect_unsupported_quote_region(lines: &[String], start: usize) -> Option<usize> {
    if start >= lines.len() {
        return None;
    }

    let line = &lines[start];
    if is_block_html_start(line) {
        return Some(collect_block_html_region(lines, start));
    }
    if is_footnote_definition_start(line) {
        return Some(collect_footnote_definition_region(lines, start));
    }
    if is_reference_definition_start(line) {
        return Some(collect_reference_definition_region(lines, start));
    }
    if is_root_table_candidate_line(line) {
        return Some(collect_root_table_candidate(lines, start));
    }
    if is_display_math_start(line) {
        return Some(collect_display_math_region(lines, start));
    }
    if BlockKind::parse_separator(line) {
        return Some(start + 1);
    }
    if lines
        .get(start + 1)
        .and_then(|next| BlockKind::parse_setext_rule(next))
        .is_some()
    {
        return Some((start + 2).min(lines.len()));
    }

    None
}

/// 收集单个列表项覆盖的行区域结束下标。
fn collect_list_item_region(lines: &[String], start: usize, marker_indent_columns: usize) -> usize {
    let mut index = start + 1;
    let mut pending_blank_lines = 0usize;
    while index < lines.len() {
        let line = &lines[index];
        if line.trim().is_empty() {
            pending_blank_lines += 1;
            index += 1;
            continue;
        }

        if parse_list_marker(line)
            .is_some_and(|marker| marker.indent_columns <= marker_indent_columns)
        {
            return index.saturating_sub(pending_blank_lines);
        }

        if parse_list_marker(line).is_some() {
            pending_blank_lines = 0;
            index += 1;
            continue;
        }

        let (indent_columns, _) = leading_indent_columns_and_bytes(line);
        if indent_columns > marker_indent_columns || pending_blank_lines == 0 {
            pending_blank_lines = 0;
            index += 1;
            continue;
        }

        return index.saturating_sub(pending_blank_lines);
    }
    index
}

/// 判断该行是否像根块的起始行。
fn looks_like_root_block_start(lines: &[String], index: usize) -> bool {
    let line = &lines[index];
    if line.trim().is_empty() {
        return true;
    }

    parse_opening_fence(line).is_some()
        || is_block_html_start(line)
        || is_footnote_definition_start(line)
        || is_reference_definition_start(line)
        || strip_indented_code_prefix(line).is_some()
        || parse_list_marker(line).is_some()
        || is_quote_start(line)
        || BlockKind::parse_heading_line(line).is_some()
        || BlockKind::parse_separator(line)
        || lines
            .get(index + 1)
            .and_then(|next| BlockKind::parse_setext_rule(next))
            .is_some()
        || is_root_table_candidate_line(line)
        || is_display_math_start(line)
}

/// 把子块挂到父块的 `children` 末尾。
fn attach_child_blocks(
    parent: &Entity<super::Block>,
    children: Vec<Entity<super::Block>>,
    cx: &mut Context<Editor>,
) {
    if children.is_empty() {
        return;
    }

    parent.update(cx, move |parent, _cx| {
        parent.children.extend(children);
    });
}

/// 构造指定语言与内容的代码块实体。
fn build_code_block(
    cx: &mut Context<Editor>,
    language: Option<SharedString>,
    content: String,
) -> Entity<super::Block> {
    Editor::new_block(
        cx,
        BlockRecord::new(
            BlockKind::CodeBlock { language },
            InlineTextTree::plain(content),
        ),
    )
}

/// 收集围栏代码块并返回块与下一行下标。
fn collect_fenced_code_block(
    cx: &mut Context<Editor>,
    lines: &[String],
    start: usize,
) -> Option<(Entity<super::Block>, usize)> {
    let fence = parse_opening_fence(&lines[start])?;
    let closing_index = find_matching_closing_fence(lines, start, &fence)?;
    if is_mermaid_info_string(fence.language.as_ref().map(|language| language.as_ref())) {
        let raw = lines[start..=closing_index].join("\n");
        return Some((
            Editor::new_block(cx, BlockRecord::mermaid(raw)),
            closing_index + 1,
        ));
    }

    // 长度已知：closing_index - (start + 1)。slice.to_vec()
    // 一次分配恰好容量，而 Vec::new() + 循环 push
    // 会让非平凡代码块的缓冲翻倍 2-3 次。
    let code_lines = lines[start + 1..closing_index].to_vec();

    Some((
        build_code_block(cx, fence.language.clone(), code_lines.join("\n")),
        closing_index + 1,
    ))
}

/// 收集缩进代码块并返回块与下一行下标。
fn collect_indented_code_block(
    cx: &mut Context<Editor>,
    lines: &[String],
    start: usize,
) -> Option<(Entity<super::Block>, usize)> {
    let stripped = strip_indented_code_prefix(&lines[start])?;
    let mut code_lines = vec![stripped.to_string()];
    let mut code_index = start + 1;
    while code_index < lines.len() {
        if let Some(stripped) = strip_indented_code_prefix(&lines[code_index]) {
            code_lines.push(stripped.to_string());
            code_index += 1;
        } else if lines[code_index].trim().is_empty() {
            code_lines.push(String::new());
            code_index += 1;
        } else {
            break;
        }
    }

    Some((
        build_code_block(cx, None, code_lines.join("\n")),
        code_index,
    ))
}

/// 构造原样保留的源码块实体。
fn raw_block(cx: &mut Context<Editor>, markdown: String) -> Entity<super::Block> {
    Editor::new_block(cx, BlockRecord::raw_markdown(markdown))
}

/// 构造 HTML 注释块实体。
fn comment_block(cx: &mut Context<Editor>, markdown: String) -> Entity<super::Block> {
    Editor::new_block(cx, BlockRecord::comment(markdown))
}

/// 构造链接/图片引用定义块实体（渲染视图隐藏）。
fn reference_definition_block(cx: &mut Context<Editor>, markdown: String) -> Entity<super::Block> {
    Editor::new_block(cx, BlockRecord::reference_definition(markdown))
}

/// 语义 HTML 落为原生块，否则回退为源码块。
fn html_or_raw_block(cx: &mut Context<Editor>, markdown: String) -> Entity<super::Block> {
    let document = parse_html_document(&markdown);
    if document.safety == HtmlSafetyClass::Semantic {
        let mut record = BlockRecord::html(markdown);
        record.html = Some(document);
        Editor::new_block(cx, record)
    } else {
        raw_block(cx, markdown)
    }
}

/// 可解析公式落为数学块，否则回退为源码块。
fn math_or_raw_block(cx: &mut Context<Editor>, markdown: String) -> Entity<super::Block> {
    if parse_display_math_source(&markdown).is_some() {
        Editor::new_block(cx, BlockRecord::math(markdown))
    } else {
        raw_block(cx, markdown)
    }
}

/// 收集闭合注释块并返回块与下一行下标。
fn collect_comment_block(
    cx: &mut Context<Editor>,
    lines: &[String],
    start: usize,
) -> Option<(Entity<super::Block>, usize)> {
    let end = collect_closed_html_comment_region(lines, start)?;
    Some((comment_block(cx, lines[start..end].join("\n")), end))
}

/// 构造指定类型的原生行内块实体。
fn native_block(
    cx: &mut Context<Editor>,
    kind: BlockKind,
    markdown: String,
) -> Entity<super::Block> {
    Editor::new_block(
        cx,
        BlockRecord::new(kind, InlineTextTree::from_markdown(&markdown)),
    )
}

/// 构造独立图片段落块实体。
fn standalone_image_block(cx: &mut Context<Editor>, markdown: String) -> Entity<super::Block> {
    Editor::new_block(cx, BlockRecord::paragraph(markdown.trim().to_string()))
}

/// 判断多行是否为单行独立图片段落。
fn is_standalone_image_paragraph(lines: &[String]) -> bool {
    lines.len() == 1 && parse_standalone_image(&lines[0]).is_some()
}

/// 判断后续行能否承接独立图片子段落。
fn starts_with_standalone_image_child_paragraph(lines: &[String]) -> bool {
    if lines.is_empty() || !is_standalone_image_paragraph(&lines[..1]) {
        return false;
    }

    lines.get(1).is_none_or(|next| {
        next.trim().is_empty()
            || parse_list_marker(next).is_some()
            || is_quote_start(next)
            || parse_opening_fence(next).is_some()
            || strip_indented_code_prefix(next).is_some()
            || is_block_html_start(next)
            || is_footnote_definition_start(next)
            || is_reference_definition_start(next)
            || is_root_table_candidate_line(next)
            || is_display_math_start(next)
    })
}

/// 把 Markdown 片段追加到块标题末尾。
fn append_markdown_to_block(
    block: &Entity<super::Block>,
    separator: &str,
    markdown: &str,
    cx: &mut Context<Editor>,
) {
    block.update(cx, |block, _cx| {
        let mut title = block.record.title.clone();
        if !separator.is_empty() {
            title.push_tree(InlineTextTree::plain(separator.to_string()));
        }
        title.push_tree(InlineTextTree::from_markdown(markdown));
        block.record.set_title(title);
        block.sync_edit_mode_from_kind();
        block.refresh_render_cache();
    });
}

/// 构造纯文本段落块实体。
fn plain_text_paragraph_block(cx: &mut Context<Editor>, text: String) -> Entity<super::Block> {
    Editor::new_block(cx, BlockRecord::paragraph(text))
}

/// 按空行数补入引用内的分隔空段落。
fn append_quote_separator_children(
    children: &mut Vec<Entity<super::Block>>,
    count: usize,
    cx: &mut Context<Editor>,
) {
    for _ in 0..count {
        children.push(native_block(cx, BlockKind::Paragraph, String::new()));
    }
}

/// 构造原生脚注定义块（含子块）。
fn build_native_footnote_definition_block(
    cx: &mut Context<Editor>,
    lines: &[String],
) -> Option<Entity<super::Block>> {
    let (id, first_line) = parse_footnote_definition_head(lines.first()?)?;
    let mut body_lines = Vec::new();
    if !first_line.is_empty() {
        body_lines.push(first_line);
    }

    for line in lines.iter().skip(1) {
        if line.trim().is_empty() {
            body_lines.push(String::new());
        } else {
            body_lines.push(
                strip_leading_columns(line, 4)
                    .unwrap_or(line.as_str())
                    .to_string(),
            );
        }
    }

    let children = Editor::build_blocks_from_lines_internal(cx, &body_lines, false);
    let block = Editor::new_block(
        cx,
        BlockRecord::new(BlockKind::FootnoteDefinition, InlineTextTree::plain(id)),
    );
    attach_child_blocks(&block, children, cx);
    Some(block)
}

impl Editor {
    /// 从完整 Markdown 文本构建根块列表。
    pub(super) fn build_root_blocks_from_markdown(
        cx: &mut Context<Self>,
        markdown: &str,
    ) -> Vec<Entity<super::Block>> {
        let lines = markdown
            .split('\n')
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        Self::build_blocks_from_lines_internal(cx, &lines, true)
    }

    /// 从指定行游标分批构造根块，返回新块与下一批起点。
    pub(super) fn build_root_blocks_chunk(
        cx: &mut Context<Self>,
        lines: &[String],
        start: usize,
        max_root_blocks: usize,
    ) -> (Vec<Entity<super::Block>>, usize) {
        Self::build_blocks_from_lines_limited(cx, lines, start, true, max_root_blocks)
    }

    /// 用运行时块按行构建文档（仅运行时可安全编辑的语法落为原生块）。
    ///
    /// 运行时编辑器可安全编辑的语法才创建原生块；更复杂的合法
    /// Markdown 区域回退为 [`BlockKind::RawMarkdown`]，保存时原样保留。
    pub(super) fn build_blocks_from_lines(
        cx: &mut Context<Self>,
        lines: &[String],
    ) -> Vec<Entity<super::Block>> {
        Self::build_blocks_from_lines_internal(cx, lines, true)
    }

    /// 按行构建块列表的内部实现（含脚注位置开关）。
    fn build_blocks_from_lines_internal(
        cx: &mut Context<Self>,
        lines: &[String],
        allow_root_footnote_definitions: bool,
    ) -> Vec<Entity<super::Block>> {
        Self::build_blocks_from_lines_limited(
            cx,
            lines,
            0,
            allow_root_footnote_definitions,
            usize::MAX,
        )
        .0
    }

    /// 按有限根块数量构造文档片段，列表只在完整项边界暂停。
    fn build_blocks_from_lines_limited(
        cx: &mut Context<Self>,
        lines: &[String],
        start: usize,
        allow_root_footnote_definitions: bool,
        max_root_blocks: usize,
    ) -> (Vec<Entity<super::Block>>, usize) {
        let mut roots = Vec::new();
        let mut index = start;

        while index < lines.len() && roots.len() < max_root_blocks {
            let line = &lines[index];
            if line.trim().is_empty() {
                let blank_start = index;
                while index < lines.len() && lines[index].trim().is_empty() {
                    index += 1;
                }

                let blank_run_len = index - blank_start;
                let previous_root_is_list_item = roots
                    .last()
                    .map(|block: &Entity<super::Block>| block.read(cx).kind().is_list_item())
                    .unwrap_or(false);
                let next_root_is_list_item = lines
                    .get(index)
                    .is_some_and(|line| parse_list_marker(line).is_some());
                let preserved_empty_blocks =
                    if roots.is_empty() || (previous_root_is_list_item && next_root_is_list_item) {
                        blank_run_len
                    } else {
                        blank_run_len.saturating_sub(1)
                    };

                for _ in 0..preserved_empty_blocks {
                    roots.push(native_block(cx, BlockKind::Paragraph, String::new()));
                }
                continue;
            }

            if parse_opening_fence(line).is_some() {
                let Some((block, next_index)) = collect_fenced_code_block(cx, lines, index) else {
                    let paragraph = Self::collect_paragraph_block(cx, lines, index);
                    roots.push(paragraph.0);
                    index = paragraph.1;
                    continue;
                };

                roots.push(block);
                index = next_index;
                continue;
            }

            if let Some((block, end)) = collect_comment_block(cx, lines, index) {
                roots.push(block);
                index = end;
                continue;
            }

            if is_block_html_start(line) {
                let end = collect_block_html_region(lines, index);
                roots.push(html_or_raw_block(cx, lines[index..end].join("\n")));
                index = end;
                continue;
            }

            if is_footnote_definition_start(line) {
                let end = collect_footnote_definition_region(lines, index);
                if allow_root_footnote_definitions {
                    if let Some(block) =
                        build_native_footnote_definition_block(cx, &lines[index..end])
                    {
                        roots.push(block);
                    } else {
                        roots.push(raw_block(cx, lines[index..end].join("\n")));
                    }
                } else {
                    roots.push(raw_block(cx, lines[index..end].join("\n")));
                }
                index = end;
                continue;
            }

            if is_reference_definition_start(line) {
                let end = collect_reference_definition_region(lines, index);
                roots.push(reference_definition_block(cx, lines[index..end].join("\n")));
                index = end;
                continue;
            }

            // 紧贴段落的 `---`/`===` 按 CommonMark 解析成 Setext 标题（与 Typora 一致），
            // 不是分隔线；分隔线需要与上一段之间有空行。
            if let Some(level) = lines
                .get(index + 1)
                .and_then(|next| BlockKind::parse_setext_rule(next))
            {
                roots.push(native_block(
                    cx,
                    BlockKind::Heading { level },
                    line.trim_end().to_string(),
                ));
                index += 2;
                continue;
            }

            if parse_standalone_image(line).is_some() {
                roots.push(standalone_image_block(cx, line.to_string()));
                index += 1;
                continue;
            }

            if strip_indented_code_prefix(line).is_some() {
                let Some((block, next_index)) = collect_indented_code_block(cx, lines, index)
                else {
                    unreachable!("indented code prefix disappeared after detection");
                };

                roots.push(block);
                index = next_index;
                continue;
            }

            if parse_list_marker(line).is_some() {
                let remaining = max_root_blocks.saturating_sub(roots.len());
                let (blocks, next_index) = Self::collect_list_blocks(cx, lines, index, remaining);
                roots.extend(blocks);
                index = next_index;
                continue;
            }

            if is_quote_start(line) {
                let (block, next_index) = Self::collect_quote_block(cx, lines, index);
                roots.push(block);
                index = next_index;
                continue;
            }

            if let Some((level, content)) = BlockKind::parse_heading_line(line) {
                roots.push(native_block(cx, BlockKind::Heading { level }, content));
                index += 1;
                continue;
            }

            if BlockKind::parse_separator(line) {
                roots.push(Self::new_block(
                    cx,
                    BlockRecord::new(BlockKind::Separator, InlineTextTree::plain(String::new())),
                ));
                index += 1;
                continue;
            }

            if is_root_table_candidate_line(line) {
                let end = collect_root_table_candidate(lines, index);
                let region = &lines[index..end];
                if let Some(table) = parse_root_table_region(region) {
                    roots.push(Self::new_block(cx, BlockRecord::table(table)));
                } else {
                    roots.extend(
                        region
                            .iter()
                            .cloned()
                            .map(|line| plain_text_paragraph_block(cx, line)),
                    );
                }
                index = end;
                continue;
            }

            if let Some(end) = collect_pipeless_table(lines, index)
                && let Some(table) = parse_root_table_region(&lines[index..end])
            {
                roots.push(Self::new_block(cx, BlockRecord::table(table)));
                index = end;
                continue;
            }

            if is_display_math_start(line) {
                let end = collect_display_math_region(lines, index);
                roots.push(math_or_raw_block(cx, lines[index..end].join("\n")));
                index = end;
                continue;
            }

            let paragraph = Self::collect_paragraph_block(cx, lines, index);
            roots.push(paragraph.0);
            index = paragraph.1;
        }

        (roots, index)
    }

    /// 收集连续行组成的段落块与下一行下标。
    fn collect_paragraph_block(
        cx: &mut Context<Self>,
        lines: &[String],
        start: usize,
    ) -> (Entity<super::Block>, usize) {
        let mut paragraph_lines = vec![lines[start].to_string()];
        let mut index = start + 1;
        while index < lines.len() {
            if (lines[index].trim().is_empty() || looks_like_root_block_start(lines, index))
                && !paragraph_can_continue_through_boundary(&paragraph_lines, lines, index)
            {
                break;
            }
            paragraph_lines.push(lines[index].to_string());
            index += 1;
        }

        (
            native_block(cx, BlockKind::Paragraph, paragraph_lines.join("\n")),
            index,
        )
    }

    /// 收集引用块与下一行下标（非法则回退为源码块）。
    fn collect_quote_block(
        cx: &mut Context<Self>,
        lines: &[String],
        start: usize,
    ) -> (Entity<super::Block>, usize) {
        let end = collect_quote_raw_region(lines, start);
        let region = &lines[start..end];
        let mut dequoted = Vec::with_capacity(region.len());
        for line in region {
            if line.trim().is_empty() {
                dequoted.push(String::new());
                continue;
            }

            let Some(content) = strip_one_quote_level(line) else {
                return (raw_block(cx, region.join("\n")), end);
            };
            dequoted.push(content);
        }

        let Some(block) = Self::build_native_quote_block(cx, &dequoted) else {
            return (raw_block(cx, region.join("\n")), end);
        };

        (block, end)
    }

    /// 构造原生引用块（含提示框与嵌套子块）。
    fn build_native_quote_block(
        cx: &mut Context<Self>,
        lines: &[String],
    ) -> Option<Entity<super::Block>> {
        if let Some(header_index) = lines.iter().position(|line| !line.trim().is_empty())
            && let Some((variant, title)) = CalloutVariant::parse_heading(&lines[header_index])
        {
            return Self::build_native_callout_block(
                cx,
                &lines[header_index + 1..],
                variant,
                title,
            );
        }

        let mut title_to_markdown = String::new();
        let mut children = Vec::new();
        let mut index = 0usize;
        let mut pending_blank_lines = 0usize;
        let mut saw_child = false;

        while index < lines.len() {
            let line = &lines[index];
            if line.trim().is_empty() {
                pending_blank_lines += 1;
                index += 1;
                continue;
            }

            if is_table_candidate_line(line) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                let table_end = collect_table_candidate(lines, index);
                let table_region = &lines[index..table_end];
                if let Some(table) = parse_table_region(table_region) {
                    children.push(Self::new_block(cx, BlockRecord::table(table)));
                } else {
                    children.push(raw_block(cx, table_region.join("\n")));
                }
                saw_child = true;
                pending_blank_lines = 0;
                index = table_end;
                continue;
            }

            if is_footnote_definition_start(line) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                let footnote_end = collect_footnote_definition_region(lines, index);
                if let Some(footnote) =
                    build_native_footnote_definition_block(cx, &lines[index..footnote_end])
                {
                    children.push(footnote);
                    saw_child = true;
                    pending_blank_lines = 0;
                    index = footnote_end;
                    continue;
                }
            }

            if let Some((comment, consumed)) = collect_comment_block(cx, lines, index) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                children.push(comment);
                saw_child = true;
                pending_blank_lines = 0;
                index = consumed;
                continue;
            }

            if is_block_html_start(line) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                let html_end = collect_block_html_region(lines, index);
                children.push(html_or_raw_block(cx, lines[index..html_end].join("\n")));
                saw_child = true;
                pending_blank_lines = 0;
                index = html_end;
                continue;
            }

            if is_display_math_start(line) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                let math_end = collect_display_math_region(lines, index);
                children.push(math_or_raw_block(cx, lines[index..math_end].join("\n")));
                saw_child = true;
                pending_blank_lines = 0;
                index = math_end;
                continue;
            }

            if let Some((level, content)) = BlockKind::parse_heading_line(line) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                children.push(native_block(cx, BlockKind::Heading { level }, content));
                saw_child = true;
                pending_blank_lines = 0;
                index += 1;
                continue;
            }

            if let Some(unsupported_end) = collect_unsupported_quote_region(lines, index) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                children.push(raw_block(cx, lines[index..unsupported_end].join("\n")));
                saw_child = true;
                pending_blank_lines = 0;
                index = unsupported_end;
                continue;
            }

            if is_quote_start(line) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                let (quote, consumed) = Self::collect_quote_block(cx, lines, index);
                if quote.read(cx).kind() == BlockKind::RawMarkdown {
                    return None;
                }
                children.push(quote);
                saw_child = true;
                pending_blank_lines = 0;
                index = consumed;
                continue;
            }

            if parse_list_marker(line).is_some() {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                let (list_blocks, consumed) =
                    Self::collect_list_blocks(cx, lines, index, usize::MAX);
                if list_blocks
                    .iter()
                    .any(|block| block.read(cx).kind() == BlockKind::RawMarkdown)
                {
                    return None;
                }
                children.extend(list_blocks);
                saw_child = true;
                pending_blank_lines = 0;
                index = consumed;
                continue;
            }

            if parse_opening_fence(line).is_some()
                && let Some((code_block, consumed)) = collect_fenced_code_block(cx, lines, index)
            {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                children.push(code_block);
                saw_child = true;
                pending_blank_lines = 0;
                index = consumed;
                continue;
            }

            if starts_with_standalone_image_child_paragraph(&lines[index..]) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                children.push(standalone_image_block(cx, line.to_string()));
                saw_child = true;
                pending_blank_lines = 0;
                index += 1;
                continue;
            }

            if strip_indented_code_prefix(line).is_some()
                && let Some((code_block, consumed)) = collect_indented_code_block(cx, lines, index)
            {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                children.push(code_block);
                saw_child = true;
                pending_blank_lines = 0;
                index = consumed;
                continue;
            }

            let mut paragraph_lines = vec![line.clone()];
            index += 1;
            while index < lines.len() {
                let next = &lines[index];
                if next.trim().is_empty()
                    || is_quote_start(next)
                    || parse_list_marker(next).is_some()
                    || parse_opening_fence(next).is_some()
                    || strip_indented_code_prefix(next).is_some()
                    || BlockKind::parse_heading_line(next).is_some()
                    || quote_content_starts_unsupported(lines, index)
                {
                    break;
                }

                paragraph_lines.push(next.clone());
                index += 1;
            }

            if is_standalone_image_paragraph(&paragraph_lines) {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                children.push(standalone_image_block(cx, paragraph_lines.join("\n")));
                saw_child = true;
                pending_blank_lines = 0;
                continue;
            }

            if saw_child {
                if pending_blank_lines > 0
                    && (!title_to_markdown.is_empty() || !children.is_empty())
                {
                    append_quote_separator_children(&mut children, pending_blank_lines, cx);
                }
                children.push(native_block(
                    cx,
                    BlockKind::Paragraph,
                    paragraph_lines.join("\n"),
                ));
                pending_blank_lines = 0;
                continue;
            }

            if !title_to_markdown.is_empty() {
                title_to_markdown.push_str(if pending_blank_lines > 0 {
                    "\n\n"
                } else {
                    "\n"
                });
            }
            title_to_markdown.push_str(&paragraph_lines.join("\n"));
            pending_blank_lines = 0;
        }

        if pending_blank_lines > 0 && (!title_to_markdown.is_empty() || !children.is_empty()) {
            append_quote_separator_children(&mut children, pending_blank_lines, cx);
        }

        let block = native_block(cx, BlockKind::Quote, title_to_markdown);
        attach_child_blocks(&block, children, cx);
        Some(block)
    }

    /// 构造原生提示框块（含子块）。
    fn build_native_callout_block(
        cx: &mut Context<Self>,
        lines: &[String],
        variant: CalloutVariant,
        title: String,
    ) -> Option<Entity<super::Block>> {
        let mut children = Vec::new();
        let mut index = 0usize;
        let mut pending_blank_lines = 0usize;

        while index < lines.len() {
            let line = &lines[index];
            if line.trim().is_empty() {
                pending_blank_lines += 1;
                index += 1;
                continue;
            }

            if pending_blank_lines > 0 {
                append_quote_separator_children(&mut children, pending_blank_lines, cx);
                pending_blank_lines = 0;
            }

            if is_table_candidate_line(line) {
                let table_end = collect_table_candidate(lines, index);
                let table_region = &lines[index..table_end];
                if let Some(table) = parse_table_region(table_region) {
                    children.push(Self::new_block(cx, BlockRecord::table(table)));
                } else {
                    children.push(raw_block(cx, table_region.join("\n")));
                }
                index = table_end;
                continue;
            }

            if is_footnote_definition_start(line) {
                let footnote_end = collect_footnote_definition_region(lines, index);
                if let Some(footnote) =
                    build_native_footnote_definition_block(cx, &lines[index..footnote_end])
                {
                    children.push(footnote);
                    index = footnote_end;
                    continue;
                }
            }

            if let Some((comment, consumed)) = collect_comment_block(cx, lines, index) {
                children.push(comment);
                index = consumed;
                continue;
            }

            if is_block_html_start(line) {
                let html_end = collect_block_html_region(lines, index);
                children.push(html_or_raw_block(cx, lines[index..html_end].join("\n")));
                index = html_end;
                continue;
            }

            if is_display_math_start(line) {
                let math_end = collect_display_math_region(lines, index);
                children.push(math_or_raw_block(cx, lines[index..math_end].join("\n")));
                index = math_end;
                continue;
            }

            if let Some((level, content)) = BlockKind::parse_heading_line(line) {
                children.push(native_block(cx, BlockKind::Heading { level }, content));
                index += 1;
                continue;
            }

            if let Some(unsupported_end) = collect_unsupported_quote_region(lines, index) {
                children.push(raw_block(cx, lines[index..unsupported_end].join("\n")));
                index = unsupported_end;
                continue;
            }

            if is_quote_start(line) {
                let (quote, consumed) = Self::collect_quote_block(cx, lines, index);
                if quote.read(cx).kind() == BlockKind::RawMarkdown {
                    return None;
                }
                children.push(quote);
                index = consumed;
                continue;
            }

            if parse_list_marker(line).is_some() {
                let (list_blocks, consumed) =
                    Self::collect_list_blocks(cx, lines, index, usize::MAX);
                if list_blocks
                    .iter()
                    .any(|block| block.read(cx).kind() == BlockKind::RawMarkdown)
                {
                    return None;
                }
                children.extend(list_blocks);
                index = consumed;
                continue;
            }

            if parse_opening_fence(line).is_some()
                && let Some((code_block, consumed)) = collect_fenced_code_block(cx, lines, index)
            {
                children.push(code_block);
                index = consumed;
                continue;
            }

            if starts_with_standalone_image_child_paragraph(&lines[index..]) {
                children.push(standalone_image_block(cx, line.to_string()));
                index += 1;
                continue;
            }

            if strip_indented_code_prefix(line).is_some()
                && let Some((code_block, consumed)) = collect_indented_code_block(cx, lines, index)
            {
                children.push(code_block);
                index = consumed;
                continue;
            }

            let mut paragraph_lines = vec![line.clone()];
            index += 1;
            while index < lines.len() {
                let next = &lines[index];
                if next.trim().is_empty()
                    || is_quote_start(next)
                    || parse_list_marker(next).is_some()
                    || parse_opening_fence(next).is_some()
                    || strip_indented_code_prefix(next).is_some()
                    || BlockKind::parse_heading_line(next).is_some()
                    || quote_content_starts_unsupported(lines, index)
                {
                    break;
                }

                paragraph_lines.push(next.clone());
                index += 1;
            }

            children.push(native_block(
                cx,
                BlockKind::Paragraph,
                paragraph_lines.join("\n"),
            ));
        }

        if pending_blank_lines > 0 {
            append_quote_separator_children(&mut children, pending_blank_lines, cx);
        }

        let block = Editor::new_block(
            cx,
            BlockRecord::new(
                BlockKind::Callout(variant),
                InlineTextTree::from_markdown(&title),
            ),
        );
        attach_child_blocks(&block, children, cx);
        Some(block)
    }

    /// 收集连续列表项块与下一行下标。
    fn collect_list_blocks(
        cx: &mut Context<Self>,
        lines: &[String],
        start: usize,
        max_items: usize,
    ) -> (Vec<Entity<super::Block>>, usize) {
        let mut roots = Vec::new();
        let mut index = start;

        while index < lines.len() && roots.len() < max_items {
            let Some(marker) = parse_list_marker(&lines[index]) else {
                break;
            };

            let item_end = collect_list_item_region(lines, index, marker.indent_columns);
            let block = native_block(cx, marker.kind.clone(), marker.text);
            let mut body_index = index + 1;
            let mut pending_blank_lines = 0usize;
            let mut fallback_raw = false;
            let mut saw_child = false;

            while body_index < item_end {
                let line = &lines[body_index];
                if line.trim().is_empty() {
                    pending_blank_lines += 1;
                    body_index += 1;
                    continue;
                }

                let (line_indent_columns, _) = leading_indent_columns_and_bytes(line);
                if line_indent_columns > marker.indent_columns {
                    let anchor_dedented =
                        dedent_lines(&lines[body_index..item_end], line_indent_columns);

                    if parse_list_marker(&anchor_dedented[0]).is_some() {
                        let (children, consumed) =
                            Self::collect_list_blocks(cx, &anchor_dedented, 0, usize::MAX);
                        attach_child_blocks(&block, children, cx);
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    if is_quote_start(&anchor_dedented[0]) {
                        let (quote, consumed) = Self::collect_quote_block(cx, &anchor_dedented, 0);
                        if quote.read(cx).kind() == BlockKind::RawMarkdown {
                            fallback_raw = true;
                            break;
                        }

                        attach_child_blocks(&block, vec![quote], cx);
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    if parse_opening_fence(&anchor_dedented[0]).is_some()
                        && let Some((code_block, consumed)) =
                            collect_fenced_code_block(cx, &anchor_dedented, 0)
                    {
                        attach_child_blocks(&block, vec![code_block], cx);
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    if is_root_table_candidate_line(&anchor_dedented[0]) {
                        let table_end = collect_root_table_candidate(&anchor_dedented, 0);
                        let table_region = &anchor_dedented[..table_end];
                        let child = if let Some(table) = parse_root_table_region(table_region) {
                            Self::new_block(cx, BlockRecord::table(table))
                        } else {
                            raw_block(cx, table_region.join("\n"))
                        };
                        attach_child_blocks(&block, vec![child], cx);
                        body_index += table_end;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    if starts_with_standalone_image_child_paragraph(&anchor_dedented) {
                        attach_child_blocks(
                            &block,
                            vec![standalone_image_block(cx, anchor_dedented[0].clone())],
                            cx,
                        );
                        body_index += 1;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    if line_indent_columns >= marker.content_indent_columns {
                        let content_dedented = dedent_lines(
                            &lines[body_index..item_end],
                            marker.content_indent_columns,
                        );
                        if strip_indented_code_prefix(&content_dedented[0]).is_some() {
                            let Some((code_block, consumed)) =
                                collect_indented_code_block(cx, &content_dedented, 0)
                            else {
                                unreachable!(
                                    "indented code prefix disappeared after child detection"
                                );
                            };

                            attach_child_blocks(&block, vec![code_block], cx);
                            body_index += consumed;
                            pending_blank_lines = 0;
                            saw_child = true;
                            continue;
                        }
                    }

                    if is_reference_definition_start(&anchor_dedented[0]) {
                        let consumed = collect_reference_definition_region(&anchor_dedented, 0);
                        attach_child_blocks(
                            &block,
                            vec![reference_definition_block(
                                cx,
                                anchor_dedented[..consumed].join("\n"),
                            )],
                            cx,
                        );
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    if let Some((comment, consumed)) =
                        collect_comment_block(cx, &anchor_dedented, 0)
                    {
                        attach_child_blocks(&block, vec![comment], cx);
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    if is_block_html_start(&anchor_dedented[0]) {
                        let consumed = collect_block_html_region(&anchor_dedented, 0);
                        attach_child_blocks(
                            &block,
                            vec![html_or_raw_block(
                                cx,
                                anchor_dedented[..consumed].join("\n"),
                            )],
                            cx,
                        );
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    if is_footnote_definition_start(&anchor_dedented[0]) {
                        let consumed = collect_footnote_definition_region(&anchor_dedented, 0);
                        attach_child_blocks(
                            &block,
                            vec![raw_block(cx, anchor_dedented[..consumed].join("\n"))],
                            cx,
                        );
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    if is_display_math_start(&anchor_dedented[0]) {
                        let consumed = collect_display_math_region(&anchor_dedented, 0);
                        attach_child_blocks(
                            &block,
                            vec![math_or_raw_block(
                                cx,
                                anchor_dedented[..consumed].join("\n"),
                            )],
                            cx,
                        );
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }

                    let should_promote_plain_child = pending_blank_lines > 0
                        || saw_child
                        || block.read(cx).rendered_text().is_empty()
                        || parse_standalone_image(&block.read(cx).record.title_to_markdown())
                            .is_some();
                    if should_promote_plain_child {
                        let (paragraph, consumed) =
                            Self::collect_paragraph_block(cx, &anchor_dedented, 0);
                        attach_child_blocks(&block, vec![paragraph], cx);
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }
                }

                if line_indent_columns >= marker.content_indent_columns {
                    let content_dedented =
                        dedent_lines(&lines[body_index..item_end], marker.content_indent_columns);
                    if strip_indented_code_prefix(&content_dedented[0]).is_some() {
                        let Some((code_block, consumed)) =
                            collect_indented_code_block(cx, &content_dedented, 0)
                        else {
                            unreachable!("indented code prefix disappeared after detection");
                        };

                        attach_child_blocks(&block, vec![code_block], cx);
                        body_index += consumed;
                        pending_blank_lines = 0;
                        saw_child = true;
                        continue;
                    }
                }

                let trimmed = line.trim_start_matches([' ', '\t']);
                append_markdown_to_block(
                    &block,
                    if pending_blank_lines > 0 {
                        "\n\n"
                    } else {
                        "\n"
                    },
                    trimmed,
                    cx,
                );
                pending_blank_lines = 0;
                body_index += 1;
            }

            if fallback_raw {
                roots.push(raw_block(cx, lines[index..item_end].join("\n")));
            } else {
                roots.push(block);
            }
            index = item_end;
        }

        (roots, index)
    }
}

#[cfg(test)]
mod tests {
    use rgpui::{AppContext, TestAppContext};

    use super::{
        collect_block_html_region, find_matching_closing_fence, is_closing_fence,
        is_reference_definition_start, parse_list_marker, parse_opening_fence,
        strip_indented_code_prefix, strip_one_quote_level,
    };
    use crate::components::{BlockKind, CalloutVariant, Editor, HtmlCssColor};

    #[test]
    fn closing_fence_must_match_exact_opening_run_length() {
        let opener = parse_opening_fence("````rust").expect("opening fence");

        assert!(is_closing_fence("````", &opener));
        assert!(is_closing_fence("  ````   ", &opener));
        assert!(!is_closing_fence("```", &opener));
        assert!(!is_closing_fence("`````", &opener));
    }

    #[test]
    fn fence_detection_rejects_indent_beyond_three_spaces() {
        assert!(parse_opening_fence("    ```rust").is_none());

        let opener = parse_opening_fence("```rust").expect("opening fence");
        assert!(!is_closing_fence("    ```", &opener));
    }

    #[test]
    fn unmatched_opening_fence_does_not_form_code_block() {
        let lines = vec![
            "```rust".to_string(),
            "fn main() {}".to_string(),
            "plain tail".to_string(),
        ];
        let opener = parse_opening_fence(&lines[0]).expect("opening fence");
        assert_eq!(find_matching_closing_fence(&lines, 0, &opener), None);
    }

    #[test]
    fn matching_closing_fence_can_skip_inner_non_closing_backtick_runs() {
        let lines = vec![
            "```rust".to_string(),
            "````".to_string(),
            "body".to_string(),
            "```".to_string(),
        ];
        let opener = parse_opening_fence(&lines[0]).expect("opening fence");
        assert_eq!(find_matching_closing_fence(&lines, 0, &opener), Some(3));
    }

    #[test]
    fn fence_closes_at_first_match_even_before_a_later_opener() {
        // 首个结束围栏即终结该块；后面的围栏属于后续内容，
        // 而不属于本块（issue #58）。
        let lines = vec![
            "```rust".to_string(),
            "```".to_string(),
            "body".to_string(),
            "```".to_string(),
            "```ts".to_string(),
        ];
        let opener = parse_opening_fence(&lines[0]).expect("opening fence");
        assert_eq!(find_matching_closing_fence(&lines, 0, &opener), Some(1));
    }

    #[test]
    fn empty_language_fence_closes_at_first_match() {
        // 相邻的无语言块必须保持独立，而不能让首个块
        // 把第二个块的围栏当作正文吞掉（issue #58）。
        let lines = vec![
            "```".to_string(),
            "first".to_string(),
            "```".to_string(),
            "```".to_string(),
            "second".to_string(),
            "```".to_string(),
        ];
        let opener = parse_opening_fence(&lines[0]).expect("opening fence");
        assert_eq!(find_matching_closing_fence(&lines, 0, &opener), Some(2));
    }

    #[test]
    fn info_tagged_fence_does_not_absorb_following_empty_blocks() {
        // 带信息串的起始围栏仍应在自己的围栏处结束，而不是
        // 吞掉后续的无语言块（issue #58）。
        let lines = vec![
            "```bash".to_string(),
            "git clone url".to_string(),
            "```".to_string(),
            "```".to_string(),
            "cargo build".to_string(),
            "```".to_string(),
        ];
        let opener = parse_opening_fence(&lines[0]).expect("opening fence");
        assert_eq!(find_matching_closing_fence(&lines, 0, &opener), Some(2));
    }

    #[test]
    fn next_opening_without_prior_closing_leaves_fence_unmatched() {
        let lines = vec![
            "```rust".to_string(),
            "body".to_string(),
            "```ts".to_string(),
            "```".to_string(),
        ];
        let opener = parse_opening_fence(&lines[0]).expect("opening fence");
        assert_eq!(find_matching_closing_fence(&lines, 0, &opener), None);
    }

    #[test]
    fn parses_indented_code_blocks() {
        assert_eq!(strip_indented_code_prefix("    code"), Some("code"));
        assert_eq!(strip_indented_code_prefix("\tcode"), Some("code"));
        assert_eq!(strip_indented_code_prefix("  code"), None);
    }

    #[test]
    fn parses_original_unordered_list_markers() {
        assert_eq!(
            parse_list_marker("- item").unwrap().kind,
            BlockKind::BulletedListItem
        );
        assert_eq!(
            parse_list_marker("* item").unwrap().kind,
            BlockKind::BulletedListItem
        );
        assert_eq!(
            parse_list_marker("+ item").unwrap().kind,
            BlockKind::BulletedListItem
        );
        assert_eq!(
            parse_list_marker("- [ ] item").unwrap().kind,
            BlockKind::TaskListItem { checked: false }
        );
        assert_eq!(
            parse_list_marker("* [x] item").unwrap().kind,
            BlockKind::TaskListItem { checked: true }
        );
        assert_eq!(
            parse_list_marker("+ [X] item").unwrap().kind,
            BlockKind::TaskListItem { checked: true }
        );
    }

    #[test]
    fn parses_commonmark_ordered_list_markers() {
        let dot = parse_list_marker("1. item").expect("dot marker");
        assert_eq!(dot.kind, BlockKind::NumberedListItem);
        assert_eq!(dot.text, "item");
        assert_eq!(dot.content_indent_columns, 3);

        let paren = parse_list_marker("12) item").expect("paren marker");
        assert_eq!(paren.kind, BlockKind::NumberedListItem);
        assert_eq!(paren.text, "item");
        assert_eq!(paren.content_indent_columns, 4);

        let tab = parse_list_marker("1)\titem").expect("tab separator");
        assert_eq!(tab.kind, BlockKind::NumberedListItem);
        assert_eq!(tab.text, "item");
        assert_eq!(tab.content_indent_columns, 4);

        assert!(parse_list_marker("1)item").is_none());
        assert!(parse_list_marker("1234567890) item").is_none());
    }

    #[test]
    fn strips_one_quote_level_per_line() {
        assert_eq!(strip_one_quote_level("> quote"), Some("quote".to_string()));
        assert_eq!(
            strip_one_quote_level("   > quote"),
            Some("quote".to_string())
        );
        assert_eq!(
            strip_one_quote_level(">> nested"),
            Some("> nested".to_string())
        );
    }

    #[test]
    fn recognizes_reference_definition_lines() {
        assert!(is_reference_definition_start("[id]: http://example.com"));
        assert!(is_reference_definition_start(
            "   [id]: <http://example.com/>"
        ));
        assert!(!is_reference_definition_start("[id] http://example.com"));
    }

    #[test]
    fn block_html_region_runs_until_blank_line() {
        let lines = vec![
            "<table>".to_string(),
            "<tr><td>x</td></tr>".to_string(),
            "</table>".to_string(),
            "".to_string(),
            "tail".to_string(),
        ];
        assert_eq!(collect_block_html_region(&lines, 0), 3);
    }

    #[rgpui::test]
    async fn imports_setext_headings_and_grouped_paragraphs(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "Heading\n-------\n\nfirst line\nsecond line".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Heading { level: 2 }
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "Heading");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "first line second line"
            );
            assert_eq!(
                editor.document.markdown_text(cx),
                "## Heading\n\nfirst line second line"
            );
        });
    }

    #[rgpui::test]
    async fn imports_indented_code_blocks_and_serializes_fenced(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "    let x = 1;\n    println!(\"hi\");".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert!(visible[0].entity.read(cx).kind().is_code_block());
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "let x = 1;\nprintln!(\"hi\");"
            );
            assert_eq!(
                editor.document.markdown_text(cx),
                "```\nlet x = 1;\nprintln!(\"hi\");\n```"
            );
        });
    }

    #[rgpui::test]
    async fn imports_consecutive_code_blocks_without_merging(cx: &mut TestAppContext) {
        // 带信息串的块后跟无语言块：每个都必须解析为独立
        // 代码块而不能合并（issue #58）。
        let source = "```bash\ngit clone url\n```\n\n```\ncargo build\n```\n\n```\nmake\n```";
        let editor = cx.new(|cx| Editor::from_markdown(cx, source.to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            let code_blocks: Vec<_> = visible
                .iter()
                .filter(|block| block.entity.read(cx).kind().is_code_block())
                .collect();
            assert_eq!(code_blocks.len(), 3);
            assert_eq!(
                code_blocks[0].entity.read(cx).rendered_text(),
                "git clone url"
            );
            assert_eq!(
                code_blocks[1].entity.read(cx).rendered_text(),
                "cargo build"
            );
            assert_eq!(code_blocks[2].entity.read(cx).rendered_text(), "make");
        });
    }

    #[rgpui::test]
    async fn preserves_hard_break_spaces_in_paragraph_round_trip(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha  \nbeta".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "alpha  \nbeta");
            assert_eq!(editor.document.markdown_text(cx), "alpha  \nbeta");

            editor.toggle_view_mode(cx);
            editor.toggle_view_mode(cx);

            let visible = editor.document.visible_blocks();
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "alpha  \nbeta");
            assert_eq!(editor.document.markdown_text(cx), "alpha  \nbeta");
        });
    }

    #[rgpui::test]
    async fn preserves_tibetan_spaces_in_paragraph_round_trip(cx: &mut TestAppContext) {
        let tibetan = "༄༅།།དཔལ་ལྡན་རྩ་བའི་བླ་མ་རིན་པོ་ཆེ།། བདག་གི་སྤྱི་བོར་པདྨའི་གདན་བཞུགས་ནས།། ";
        let editor = cx.new(|cx| Editor::from_markdown(cx, tibetan.to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), tibetan);
            assert!(
                visible[0]
                    .entity
                    .read(cx)
                    .rendered_text()
                    .contains("།། བདག")
            );
            assert!(visible[0].entity.read(cx).rendered_text().ends_with(' '));
            assert_eq!(editor.document.markdown_text(cx), tibetan);

            editor.toggle_view_mode(cx);
            editor.toggle_view_mode(cx);

            let visible = editor.document.visible_blocks();
            assert_eq!(visible[0].entity.read(cx).rendered_text(), tibetan);
            assert_eq!(editor.document.markdown_text(cx), tibetan);
        });
    }

    #[rgpui::test]
    async fn preserves_chinese_spaces_in_paragraph_round_trip(cx: &mut TestAppContext) {
        let chinese = "中文 文本 ";
        let editor = cx.new(|cx| Editor::from_markdown(cx, chinese.to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), chinese);
            assert_eq!(editor.document.markdown_text(cx), chinese);
        });
    }

    #[rgpui::test]
    async fn preserves_hard_break_spaces_in_simple_quote(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> alpha  \n> beta".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "alpha  \nbeta");
            assert_eq!(editor.document.markdown_text(cx), "> alpha  \n> beta");
        });
    }

    #[rgpui::test]
    async fn preserves_hard_break_spaces_in_list_item_continuation(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- alpha  \n  beta".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "alpha  \nbeta");
            assert_eq!(editor.document.markdown_text(cx), "- alpha  \n  beta");
        });
    }

    #[rgpui::test]
    async fn imports_nested_list_children_as_native_blocks(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "- parent\n  - nested bullet\n  - [x] nested task".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "parent");
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "nested bullet");
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::TaskListItem { checked: true }
            );
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "nested task");
        });
    }

    #[rgpui::test]
    async fn imports_deeply_nested_list_levels(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "- 深层嵌套：\n  1. 第一层有序\n     - 第二层无序\n       - 第三层无序\n         1. 第四层有序"
                    .to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            let kinds = visible
                .iter()
                .map(|block| block.entity.read(cx).kind().clone())
                .collect::<Vec<_>>();
            assert_eq!(
                kinds,
                vec![
                    BlockKind::BulletedListItem,
                    BlockKind::NumberedListItem,
                    BlockKind::BulletedListItem,
                    BlockKind::BulletedListItem,
                    BlockKind::NumberedListItem,
                ]
            );
            let depths = visible
                .iter()
                .map(|block| block.entity.read(cx).render_depth)
                .collect::<Vec<_>>();
            assert_eq!(depths, vec![0, 1, 2, 3, 4]);
        });
    }

    #[rgpui::test]
    async fn imports_indented_code_block_as_native_list_child(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "- item with code block\n\n      let x = 1;\n      let y = 2;".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::CodeBlock { language: None }
            );
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "let x = 1;\nlet y = 2;"
            );
            assert_eq!(
                editor.document.markdown_text(cx),
                "- item with code block\n  ```\n  let x = 1;\n  let y = 2;\n  ```"
            );

            editor.toggle_view_mode(cx);
            editor.toggle_view_mode(cx);

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::CodeBlock { language: None }
            );
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "let x = 1;\nlet y = 2;"
            );
        });
    }

    #[rgpui::test]
    async fn imports_fenced_code_block_as_native_list_child(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "- item with fenced code\n  ```rust\n  fn main() {}\n  ```".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::CodeBlock {
                    language: Some("rust".into())
                }
            );
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "fn main() {}");
        });
    }

    #[rgpui::test]
    async fn imports_simple_quote_as_native_list_child(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "1. item with nested quote\n\n   > quoted text\n   >\n   > quoted paragraph two"
                    .to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "item with nested quote"
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "quoted text\n\nquoted paragraph two"
            );
            assert_eq!(
                editor.document.markdown_text(cx),
                "1. item with nested quote\n  > quoted text\n  > \n  > quoted paragraph two"
            );

            editor.toggle_view_mode(cx);
            editor.toggle_view_mode(cx);

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "quoted text\n\nquoted paragraph two"
            );
        });
    }

    #[rgpui::test]
    async fn separated_numbered_list_runs_restart_at_one_after_blank_line(cx: &mut TestAppContext) {
        let editor = cx
            .new(|cx| Editor::from_markdown(cx, "1. aa\n2. bb\n3. cc\n\n1. dd".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 5);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(visible[0].entity.read(cx).list_ordinal, Some(1));
            assert_eq!(visible[1].entity.read(cx).list_ordinal, Some(2));
            assert_eq!(visible[2].entity.read(cx).list_ordinal, Some(3));
            assert_eq!(visible[3].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[3].entity.read(cx).rendered_text(), "");
            assert_eq!(
                visible[4].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(visible[4].entity.read(cx).rendered_text(), "dd");
            assert_eq!(visible[4].entity.read(cx).list_ordinal, Some(1));
            assert_eq!(
                editor.document.markdown_text(cx),
                "1. aa\n2. bb\n3. cc\n\n1. dd"
            );
        });
    }

    #[rgpui::test]
    async fn list_group_separator_paragraph_collapses_when_unfocused(cx: &mut TestAppContext) {
        // 相邻列表组之间的空段落只用于在源码里分隔两组列表；渲染时应折叠为零高
        // （对齐 Typora：列表之间不显示多余空行），聚焦时恢复常规高度。
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "- a\n- b\n\n1. c\n2. d".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 5, "应为两个列表 + 中间空段落");
            let separator = visible[2].entity.read(cx);
            assert_eq!(separator.kind(), BlockKind::Paragraph);
            assert_eq!(separator.rendered_text(), "");
            assert!(separator.list_group_separator_candidate);
            assert!(separator.collapses_as_list_group_separator(false));
            assert!(!separator.collapses_as_list_group_separator(true));
        });
    }

    #[rgpui::test]
    async fn imports_parenthesized_ordered_lists_and_serializes_canonical_dot_markers(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "1) one\n2) two".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "one");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "two");
            assert_eq!(visible[0].entity.read(cx).list_ordinal, Some(1));
            assert_eq!(visible[1].entity.read(cx).list_ordinal, Some(2));
            assert_eq!(editor.document.markdown_text(cx), "1. one\n2. two");
        });
    }

    #[rgpui::test]
    async fn imports_nested_parenthesized_ordered_list_children(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "1) parent\n   1) child".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "parent");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "child");
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "1. parent\n  1. child");
        });
    }

    #[rgpui::test]
    async fn imports_nested_quotes_as_native_blocks(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(cx, "> level1\n>> level2\n>>> level3".to_string(), None)
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "level1");
            assert_eq!(visible[0].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "level2");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 2);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "level3");
            assert_eq!(visible[2].entity.read(cx).quote_depth, 3);
            assert_eq!(
                editor.document.markdown_text(cx),
                "> level1\n> > level2\n> > > level3"
            );
        });
    }

    #[rgpui::test]
    async fn literal_blank_line_splits_quote_groups(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "> first\n\n> second".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "first");
            assert_eq!(visible[0].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "second");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "> first\n\n> second");
        });
    }

    #[rgpui::test]
    async fn quoted_blank_line_stays_inside_same_quote_group(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "> first\n>\n> second".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "first\n\nsecond"
            );
            assert_eq!(editor.document.markdown_text(cx), "> first\n> \n> second");
        });
    }

    #[rgpui::test]
    async fn imports_quote_with_list_children(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "> Quote with list:\n> - item 1\n> - [ ] task item".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "Quote with list:"
            );
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "item 1");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::TaskListItem { checked: false }
            );
            assert_eq!(visible[2].entity.read(cx).quote_depth, 1);
            assert_eq!(
                editor.document.markdown_text(cx),
                "> Quote with list:\n> - item 1\n> - [ ] task item"
            );
        });
    }

    #[rgpui::test]
    async fn imports_quote_with_code_block_child(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "> Quote with code block:\n>\n>     fn main() {\n>         println!(\"hi\");\n>     }"
                    .to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "Quote with code block:"
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::CodeBlock { language: None }
            );
            assert_eq!(visible[2].entity.read(cx).quote_depth, 1);
            assert_eq!(
                visible[2].entity.read(cx).rendered_text(),
                "fn main() {\n    println!(\"hi\");\n}"
            );
            assert_eq!(
                editor.document.markdown_text(cx),
                "> Quote with code block:\n> \n> ```\n> fn main() {\n>     println!(\"hi\");\n> }\n> ```"
            );
        });
    }

    #[rgpui::test]
    async fn imports_quote_with_heading_child(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "> Quote with heading:\n>\n> #### Inner heading".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "Quote with heading:"
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::Heading { level: 4 }
            );
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "Inner heading");
            assert_eq!(visible[2].entity.read(cx).quote_depth, 1);
            assert_eq!(
                editor.document.markdown_text(cx),
                "> Quote with heading:\n> \n> #### Inner heading"
            );
        });
    }

    #[rgpui::test]
    async fn imports_quote_with_standalone_image_child(cx: &mut TestAppContext) {
        let markdown = "> ![alt](./img.png)".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_bulleted_list_item_with_standalone_image_title(cx: &mut TestAppContext) {
        let markdown = "- ![alt](./img.png)".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert!(visible[0].entity.read(cx).children.is_empty());
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_list_item_with_standalone_image_child(cx: &mut TestAppContext) {
        let markdown = "- item\n  ![alt](./img.png)".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "item");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_list_image_title_with_native_child_paragraph(cx: &mut TestAppContext) {
        let markdown = "- ![alt](./img.png)\n  child text".to_string();
        let canonical_markdown = "- ![alt](./img.png)\n\n  child text";
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "child text");
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), canonical_markdown);
        });
    }

    #[rgpui::test]
    async fn imports_quote_with_numbered_list_image_item(cx: &mut TestAppContext) {
        let markdown = "> 1. ![alt](./img.png)".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::NumberedListItem
            );
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_callout_with_task_list_image_item_and_child(cx: &mut TestAppContext) {
        let markdown = "> [!NOTE]\n> - [ ] ![cover][img]\n>   ![detail](./detail.png)\n>\n> [img]: ./cover.png".to_string();
        let canonical_markdown = "> [!NOTE]\n> - [ ] ![cover][img]\n>   ![detail](./detail.png)\n> \n> [img]: ./cover.png";
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Note)
            );
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::TaskListItem { checked: false }
            );
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[1].entity.read(cx).callout_depth, 1);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).render_depth, 1);
            assert_eq!(visible[2].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[2].entity.read(cx).callout_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), canonical_markdown);
        });
    }

    #[rgpui::test]
    async fn imports_callout_with_standalone_image_child(cx: &mut TestAppContext) {
        let markdown = "> [!NOTE]\n> ![alt](./img.png)".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Note)
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[1].entity.read(cx).callout_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_quote_list_item_with_native_child_paragraph(cx: &mut TestAppContext) {
        let markdown = "> - item\n>\n>     child text".to_string();
        let canonical_markdown = "> - item\n> \n>   child text";
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "child text");
            assert_eq!(visible[2].entity.read(cx).render_depth, 1);
            assert_eq!(visible[2].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), canonical_markdown);
        });
    }

    #[rgpui::test]
    async fn imports_callout_list_item_with_native_child_paragraph(cx: &mut TestAppContext) {
        let markdown = "> [!NOTE]\n> - item\n>\n>     child text".to_string();
        let canonical_markdown = "> [!NOTE]\n> - item\n> \n>   child text";
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Note)
            );
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[1].entity.read(cx).callout_depth, 1);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "child text");
            assert_eq!(visible[2].entity.read(cx).render_depth, 1);
            assert_eq!(visible[2].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[2].entity.read(cx).callout_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), canonical_markdown);
        });
    }

    #[rgpui::test]
    async fn quote_does_not_promote_multiline_image_paragraph_to_child(cx: &mut TestAppContext) {
        let markdown = "> ![alt](./img.png)  \n> tail".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_callout_from_quote_header(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> [!NOTE]".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Note)
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "");
            assert!(visible[0].entity.read(cx).children.is_empty());
            assert_eq!(visible[0].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "> [!NOTE]");
        });
    }

    #[rgpui::test]
    async fn imports_important_callout_case_insensitively(cx: &mut TestAppContext) {
        let editor = cx
            .new(|cx| Editor::from_markdown(cx, "> [!important] Optional title".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Important)
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "Optional title");
            assert_eq!(
                editor.document.markdown_text(cx),
                "> [!IMPORTANT] Optional title"
            );
        });
    }

    #[rgpui::test]
    async fn imports_callout_title_and_nested_quote_child(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "> [!WARNING] Custom title\n> body\n> > nested".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Warning)
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "Custom title");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "body");
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "nested");
            assert_eq!(visible[2].entity.read(cx).quote_depth, 2);
            assert_eq!(
                editor.document.markdown_text(cx),
                "> [!WARNING] Custom title\n> body\n> > nested"
            );
        });
    }

    #[rgpui::test]
    async fn imports_callout_with_multiline_nested_quote_child(cx: &mut TestAppContext) {
        let markdown = [
            "> [!WARNING] Custom title",
            "> body",
            "> > inner one",
            "> >",
            "> > inner two",
            "> after",
        ]
        .join("\n");
        let canonical_markdown = [
            "> [!WARNING] Custom title",
            "> body",
            "> > inner one",
            "> > ",
            "> > inner two",
            "> after",
        ]
        .join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 4);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Warning)
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "body");
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[2].entity.read(cx).rendered_text(),
                "inner one\n\ninner two"
            );
            assert_eq!(visible[2].entity.read(cx).quote_depth, 2);
            assert!(
                visible[2]
                    .entity
                    .read(cx)
                    .visible_quote_group_anchor
                    .is_some()
            );
            assert_eq!(visible[3].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[3].entity.read(cx).rendered_text(), "after");
            assert_eq!(visible[3].entity.read(cx).quote_depth, 1);
            assert!(
                visible[3]
                    .entity
                    .read(cx)
                    .visible_quote_group_anchor
                    .is_none()
            );
            assert_eq!(editor.document.markdown_text(cx), canonical_markdown);
        });
    }

    #[rgpui::test]
    async fn unknown_callout_marker_stays_plain_quote(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "> [!UNKNOWN]".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "[!UNKNOWN]");
            assert_eq!(editor.document.markdown_text(cx), "> [!UNKNOWN]");
        });
    }

    #[rgpui::test]
    async fn preserves_separator_between_quote_title_and_nested_child(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "> outer\n>\n>> inner".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "outer");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "inner");
            assert_eq!(visible[2].entity.read(cx).quote_depth, 2);
            assert_eq!(editor.document.markdown_text(cx), "> outer\n> \n> > inner");
        });
    }

    #[rgpui::test]
    async fn imports_quote_with_native_table_child(cx: &mut TestAppContext) {
        let markdown = "> Quote with table:\n> | A | B |\n> | --- | --- |\n> | 1 | 2 |".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "Quote with table:"
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Table);
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            let table = visible[1]
                .entity
                .read(cx)
                .record
                .table
                .as_ref()
                .expect("native nested table");
            assert_eq!(table.header.len(), 2);
            assert_eq!(table.rows.len(), 1);
            assert_eq!(table.rows[0].len(), 2);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn quote_child_blocks_inside_list_items_keep_single_indent(cx: &mut TestAppContext) {
        // 回归：引用内部的子块（表格、代码块）曾在外层列表缩进之上再叠加自身
        // 缩进，产物多出两格（`  >   | A | B |`）；打开后保存即被改写。
        let table = "- item\n  > quote\n  > | A | B |\n  > | --- | --- |\n  > | 1 | 2 |";
        let editor = cx.new(|cx| Editor::from_markdown(cx, table.to_string(), None));
        editor.update(cx, |editor, cx| {
            assert_eq!(
                editor.document.markdown_text(cx),
                table,
                "引用内表格不应叠加列表缩进"
            );
        });

        let code = "- item\n  > quote\n  > ```\n  > code\n  > ```";
        let editor = cx.new(|cx| Editor::from_markdown(cx, code.to_string(), None));
        editor.update(cx, |editor, cx| {
            assert_eq!(
                editor.document.markdown_text(cx),
                code,
                "引用内代码块不应叠加列表缩进"
            );
        });
    }

    #[rgpui::test]
    async fn invalid_table_inside_quote_preserves_outer_quote_and_raw_child(
        cx: &mut TestAppContext,
    ) {
        let markdown = "> Quote with broken table:\n> | A |\n> | --- | --- |\n> | 1 |".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "Quote with broken table:"
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::RawMarkdown);
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "| A |\n| --- | --- |\n| 1 |"
            );
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn final_mixed_mega_block_preserves_important_callout_with_native_table_and_native_footnote(
        cx: &mut TestAppContext,
    ) {
        let markdown = "> [!IMPORTANT]\n> Final mixed block that combines:\n>\n> - **bold**\n> - *italic*\n> - `inline code`\n> - [link](https://example.com)\n> - ![image](https://example.com/image.png)\n> - ~~strike~~\n>\n> And a table:\n>\n> | k | v |\n> | --- | --- |\n> | a | 1 |\n> | b | 2 |\n>\n> And a fenced code block:\n>\n> ```ts\n> export const answer = 42;\n> ```\n>\n> And a footnote reference.[^final]\n>\n> [^final]: Final footnote text with nested list:\n>   - one\n>   - two".to_string();
        let canonical_markdown = "> [!IMPORTANT]\n> Final mixed block that combines:\n> \n> - **bold**\n> - *italic*\n> - `inline code`\n> - [link](https://example.com)\n> - ![image](https://example.com/image.png)\n> - ~~strike~~\n> \n> And a table:\n> \n> | k | v |\n> | --- | --- |\n> | a | 1 |\n> | b | 2 |\n> \n> And a fenced code block:\n> \n> ```ts\n> export const answer = 42;\n> ```\n> \n> And a footnote reference.[^final]\n> \n> [^final]: Final footnote text with nested list:\n> \n>     - one\n>     - two";
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            // 失败时打印「类型 + 引用深度」画像，any(...) 本身的表达式看不出漏了哪一层。
            let profile = visible
                .iter()
                .map(|visible| {
                    let block = visible.entity.read(cx);
                    (block.kind(), block.quote_depth)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Important)
            );
            assert!(
                visible.iter().any(|visible| {
                    let block = visible.entity.read(cx);
                    block.kind() == BlockKind::BulletedListItem && block.quote_depth == 1
                }),
                "missing a depth-1 list item; profile: {profile:?}"
            );
            assert!(
                visible.iter().any(|visible| {
                    let block = visible.entity.read(cx);
                    block.kind()
                        == BlockKind::CodeBlock {
                            language: Some("ts".into()),
                        }
                        && block.rendered_text().contains("export const answer = 42;")
                }),
                "missing the ts code block; profile: {profile:?}"
            );
            assert!(
                visible.iter().any(|visible| {
                    let block = visible.entity.read(cx);
                    block.kind() == BlockKind::Table
                        && block.quote_depth == 1
                        && block.record.table.as_ref().is_some_and(|table| {
                            table.header.len() == 2
                                && table.rows.len() == 2
                                && table.header[0].serialize_markdown() == "k"
                                && table.rows[1][1].serialize_markdown() == "2"
                        })
                }),
                "missing the depth-1 2x2 table; profile: {profile:?}"
            );
            assert!(
                visible.iter().any(|visible| {
                    let block = visible.entity.read(cx);
                    block.kind() == BlockKind::Paragraph
                        && block.rendered_text().contains("And a table:")
                        && block.quote_depth == 1
                }),
                "missing the depth-1 intro paragraph; profile: {profile:?}"
            );
            assert!(
                visible.iter().any(|visible| {
                    let block = visible.entity.read(cx);
                    block.kind() == BlockKind::FootnoteDefinition
                        && block.rendered_text() == "final"
                        && block.quote_depth == 1
                }),
                "missing the depth-1 footnote definition; profile: {profile:?}"
            );
            assert!(
                visible.iter().any(|visible| {
                    let block = visible.entity.read(cx);
                    block.kind() == BlockKind::Paragraph
                        && block.rendered_text() == "Final footnote text with nested list:"
                        && block.footnote_anchor.is_some()
                        && block.quote_depth == 1
                }),
                "missing the footnote body paragraph; profile: {profile:?}"
            );
            let nested_footnote_items = visible
                .iter()
                .filter(|visible| {
                    let block = visible.entity.read(cx);
                    block.kind() == BlockKind::BulletedListItem
                        && block.footnote_anchor.is_some()
                        && block.quote_depth == 1
                })
                .count();
            assert!(
                nested_footnote_items >= 2,
                "expected 2 nested footnote list items, got {nested_footnote_items}; profile: {profile:?}"
            );
            assert_eq!(editor.document.markdown_text(cx), canonical_markdown);
        });
    }

    #[rgpui::test]
    async fn unsupported_nested_block_preserves_native_list_item_with_raw_child(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "- native before\n- raw item\n  <div>\n  inner\n  </div>\n- native after"
                    .to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 4);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "native before");
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "raw item");
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::HtmlBlock);
            assert!(
                visible[2].entity.read(cx).rendered_text().contains("<div>"),
                "html block text: {:?}",
                visible[2].entity.read(cx).rendered_text()
            );
            assert_eq!(
                visible[3].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[3].entity.read(cx).rendered_text(), "native after");
        });
    }

    #[rgpui::test]
    async fn imports_and_canonicalizes_task_lists(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "- [ ] todo\n* [x] done\n+ [X] shipped".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::TaskListItem { checked: false }
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "todo");
            assert_eq!(
                visible[1].entity.read(cx).kind(),
                BlockKind::TaskListItem { checked: true }
            );
            assert_eq!(
                editor.document.markdown_text(cx),
                "- [ ] todo\n- [x] done\n- [x] shipped"
            );
        });
    }

    #[rgpui::test]
    async fn parses_root_level_pipe_table_as_native_table(cx: &mut TestAppContext) {
        let markdown = "| A | B |\n| --- | --- |\n| 1 | 2 |".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Table);
            let table = visible[0]
                .entity
                .read(cx)
                .record
                .table
                .as_ref()
                .expect("native table data");
            assert_eq!(table.header.len(), 2);
            assert_eq!(table.rows.len(), 1);
            assert_eq!(table.rows[0].len(), 2);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn broken_root_level_table_degrades_to_plain_text_lines(cx: &mut TestAppContext) {
        let markdown = "| A | B |\n| nope | --- |\n| 1 | 2 |".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "| A | B |");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "| nope | --- |");
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "| 1 | 2 |");
            assert_eq!(
                editor.document.markdown_text(cx),
                "| A | B |\n\n| nope | --- |\n\n| 1 | 2 |"
            );
        });
    }

    #[rgpui::test]
    async fn imports_display_math_block_as_native_math_block(cx: &mut TestAppContext) {
        let markdown = "$$\n\\int_0^1 x^2 dx\n$$".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::MathBlock);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), markdown);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_single_line_display_math_between_paragraphs(cx: &mut TestAppContext) {
        let markdown = "before\n$$x^2$$\nafter".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::MathBlock);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "$$x^2$$");
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(
                editor.document.markdown_text(cx),
                "before\n\n$$x^2$$\n\nafter"
            );
        });
    }

    #[rgpui::test]
    async fn unclosed_display_math_stays_raw(cx: &mut TestAppContext) {
        let markdown = "$$\n\\int_0^1 x^2 dx".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::RawMarkdown);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_mermaid_fence_as_native_mermaid_block(cx: &mut TestAppContext) {
        let markdown = "before\n```mermaid\nflowchart LR\nA --> B\n```\nafter".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::MermaidBlock);
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "```mermaid\nflowchart LR\nA --> B\n```"
            );
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(
                editor.document.markdown_text(cx),
                "before\n\n```mermaid\nflowchart LR\nA --> B\n```\n\nafter"
            );
        });
    }

    #[rgpui::test]
    async fn imports_tilde_mmd_fence_as_native_mermaid_block(cx: &mut TestAppContext) {
        let markdown = "~~~MMD\nflowchart LR\nA --> B\n~~~".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::MermaidBlock);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn regular_fenced_code_is_not_mermaid(cx: &mut TestAppContext) {
        let markdown = "```rust\nfn main() {}\n```".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert!(matches!(
                visible[0].entity.read(cx).kind(),
                BlockKind::CodeBlock { .. }
            ));
        });
    }

    #[rgpui::test]
    async fn imports_details_html_block_with_blank_lines_as_native_html_block(
        cx: &mut TestAppContext,
    ) {
        let markdown =
            "<details>\n<summary>Title</summary>\n\nHidden content with `code`.\n\n</details>"
                .to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::HtmlBlock);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), markdown);
            assert!(
                visible[0]
                    .entity
                    .read(cx)
                    .record
                    .html
                    .as_ref()
                    .is_some_and(|html| html.is_semantic())
            );
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_safe_inline_html_line_as_native_html_block(cx: &mut TestAppContext) {
        let markdown = "<span style='color:blue;'>Anaconda</span>: https://example.com".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            let block = visible[0].entity.read(cx);
            assert_eq!(block.kind(), BlockKind::HtmlBlock);
            assert_eq!(block.rendered_text(), markdown);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_standalone_html_image_as_native_html_block(cx: &mut TestAppContext) {
        let markdown =
            "<img src=\"./assets/pic.png\" alt=\"alt text\" style=\"zoom:80%;\" />".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            let block = visible[0].entity.read(cx);
            assert_eq!(block.kind(), BlockKind::HtmlBlock);
            assert_eq!(block.rendered_text(), markdown);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_list_items_with_inline_span_style_as_text_not_links(cx: &mut TestAppContext) {
        let markdown = [
            "- Anaconda的安装需要留意<span style='color:blue;'>磁盘预留空间、系统环境变量</span>等问题",
            "- Pycharm的安装需要留意<span style='color:blue;'>专业版破解、python解释器关联</span>等问题",
            "- GPU版本的 Pytorch v1.5.0安装需要留意本机<span style='color:blue;'>英伟达驱动`CUDA+cuDNN`</span>",
        ]
        .join("\n");
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            for block in visible {
                assert_eq!(block.entity.read(cx).kind(), BlockKind::BulletedListItem);
            }

            let first = visible[0].entity.read(cx);
            let span_start = "Anaconda的安装需要留意".len();
            assert_eq!(first.inline_link_at(span_start), None);
            assert!(matches!(
                first
                    .inline_html_style_at(span_start)
                    .and_then(|style| style.color),
                Some(HtmlCssColor::Rgba(color))
                    if color.red == 0 && color.green == 0 && color.blue == 255
            ));
            assert_eq!(
                first.rendered_text(),
                "Anaconda的安装需要留意磁盘预留空间、系统环境变量等问题"
            );

            let third = visible[2].entity.read(cx);
            let code_start = "GPU版本的 Pytorch v1.5.0安装需要留意本机英伟达驱动".len();
            assert!(third.inline_style_at(code_start).code);
            assert_eq!(third.inline_link_at(code_start), None);
            assert!(third.inline_html_style_at(code_start).is_some());
        });
    }

    #[rgpui::test]
    async fn risky_html_tag_stays_raw_markdown(cx: &mut TestAppContext) {
        let markdown = "<script>alert(1)</script>".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::RawMarkdown);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), markdown);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn safe_html_with_risky_child_uses_html_block_and_preserves_source(
        cx: &mut TestAppContext,
    ) {
        let markdown = "<div>safe<script>alert(1)</script>tail</div>".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            let block = visible[0].entity.read(cx);
            assert_eq!(block.kind(), BlockKind::HtmlBlock);
            assert!(
                block
                    .record
                    .html
                    .as_ref()
                    .is_some_and(|html| html.is_semantic())
            );
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_closed_html_comment_as_native_comment_block(cx: &mut TestAppContext) {
        let markdown = "<!--\n xxx \n-->".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Comment);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), markdown);
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn imports_reference_definitions_as_hidden_raw_blocks(cx: &mut TestAppContext) {
        let markdown =
            "[ref-link]: https://example.com\n\n[ref-image]: https://example.com/image.png \"标题\""
                .to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert!(visible[0].entity.read(cx).is_reference_definition());
            assert!(visible[1].entity.read(cx).is_reference_definition());
            assert_eq!(editor.document.markdown_text(cx), markdown);
        });
    }

    #[rgpui::test]
    async fn html_comment_closes_at_first_marker_and_resumes_block_parsing(
        cx: &mut TestAppContext,
    ) {
        let markdown = "before\n<!--\na\n--> trailing\n# after".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Comment);
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "<!--\na\n--> trailing"
            );
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::Heading { level: 1 }
            );
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "after");
            assert_eq!(
                editor.document.markdown_text(cx),
                "before\n\n<!--\na\n--> trailing\n\n# after"
            );
        });
    }

    #[rgpui::test]
    async fn unclosed_html_comment_stays_raw_and_does_not_absorb_following_paragraph(
        cx: &mut TestAppContext,
    ) {
        let markdown = "<!--\na\n\nparagraph".to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::RawMarkdown);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "<!--\na");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "paragraph");
            assert_eq!(editor.document.markdown_text(cx), "<!--\na\n\nparagraph");
        });
    }

    #[rgpui::test]
    async fn imports_comment_blocks_inside_list_quote_and_callout(cx: &mut TestAppContext) {
        let list_editor =
            cx.new(|cx| Editor::from_markdown(cx, "- item\n  <!--\n  list\n  -->".into(), None));
        list_editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Comment);
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "<!--\nlist\n-->"
            );
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(
                editor.document.markdown_text(cx),
                "- item\n  <!--\n  list\n  -->"
            );
        });

        let quote_editor = cx.new(|cx| {
            Editor::from_markdown(cx, "> quote\n>\n> <!--\n> quoted\n> -->".into(), None)
        });
        quote_editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Comment);
            assert_eq!(
                visible[2].entity.read(cx).rendered_text(),
                "<!--\nquoted\n-->"
            );
            assert_eq!(visible[2].entity.read(cx).quote_depth, 1);
            assert_eq!(
                editor.document.markdown_text(cx),
                "> quote\n> \n> <!--\n> quoted\n> -->"
            );
        });

        let callout_editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "> [!NOTE] Title\n>\n> <!--\n> callout\n> -->".into(),
                None,
            )
        });
        callout_editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::Callout(CalloutVariant::Note)
            );
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Comment);
            assert_eq!(
                visible[2].entity.read(cx).rendered_text(),
                "<!--\ncallout\n-->"
            );
            assert_eq!(visible[2].entity.read(cx).callout_depth, 1);
            assert_eq!(
                editor.document.markdown_text(cx),
                "> [!NOTE] Title\n> \n> <!--\n> callout\n> -->"
            );
        });
    }

    #[rgpui::test]
    async fn parses_multiline_root_footnote_definition_as_native_block(cx: &mut TestAppContext) {
        let markdown = "[^note]: Footnote text with **bold**\n    - item 1\n    - item 2\n\n    Second paragraph.".to_string();
        let canonical_markdown = "[^note]: Footnote text with **bold**\n\n    - item 1\n    - item 2\n\n    Second paragraph.";
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 5);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::FootnoteDefinition
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "note");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(
                visible[1].entity.read(cx).rendered_text(),
                "Footnote text with bold"
            );
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "item 1");
            assert_eq!(
                visible[3].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[3].entity.read(cx).rendered_text(), "item 2");
            assert_eq!(visible[4].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(
                visible[4].entity.read(cx).rendered_text(),
                "Second paragraph."
            );
            assert_eq!(editor.document.markdown_text(cx), canonical_markdown);
        });
    }

    #[rgpui::test]
    async fn nested_quote_footnote_definition_upgrades_to_native_block(cx: &mut TestAppContext) {
        let markdown = "> outer\n>\n> [^note]: nested footnote".to_string();
        let canonical_markdown = "> outer\n> \n> [^note]: nested footnote";
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 4);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "outer");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).quote_depth, 1);
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::FootnoteDefinition
            );
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "note");
            assert_eq!(visible[2].entity.read(cx).quote_depth, 1);
            assert_eq!(visible[3].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(
                visible[3].entity.read(cx).rendered_text(),
                "nested footnote"
            );
            assert_eq!(visible[3].entity.read(cx).quote_depth, 1);
            assert!(visible[3].entity.read(cx).footnote_anchor.is_some());
            assert_eq!(editor.document.markdown_text(cx), canonical_markdown);
        });
    }

    #[rgpui::test]
    async fn test_md_fixture_keeps_mixed_supported_and_raw_sections_visible(
        cx: &mut TestAppContext,
    ) {
        let markdown = include_str!("../../docs/render-sample.md").to_string();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert!(visible.len() > 40, "visible blocks: {}", visible.len());
            // 失败时打印实际解析出的块类型序列，否则 any(...) 的表达式本身看不出漏了哪一块。
            let kinds = visible
                .iter()
                .map(|block| block.entity.read(cx).kind())
                .collect::<Vec<_>>();

            assert!(
                visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind() == BlockKind::Heading { level: 1 }
                        && block.rendered_text() == "Markdown 渲染测试文档"
                }),
                "missing the level 1 title; kinds: {kinds:?}"
            );

            assert!(
                visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind() == BlockKind::Quote
                        && block.rendered_text().contains("引用段落一。")
                }),
                "missing the quote block; kinds: {kinds:?}"
            );

            assert!(
                visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind().is_code_block()
                        && block.rendered_text().contains("println!(\"围栏代码块\");")
                }),
                "missing the fenced code block; kinds: {kinds:?}"
            );

            assert!(
                visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind() == BlockKind::TaskListItem { checked: false }
                        && block.rendered_text().contains("未勾选任务")
                }),
                "missing the unchecked task; kinds: {kinds:?}"
            );

            assert!(
                visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind() == BlockKind::BulletedListItem
                        && block.rendered_text() == "混合列表项"
                }),
                "missing the mixed list item; kinds: {kinds:?}"
            );

            assert!(
                visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind().is_code_block() && block.rendered_text().contains("let x = 1;")
                }),
                "missing the indented code block; kinds: {kinds:?}"
            );

            let multiline_code = visible
                .iter()
                .find(|block| {
                    block
                        .entity
                        .read(cx)
                        .rendered_text()
                        .starts_with("跨行的行内代码：")
                })
                .expect("multiline inline code sample")
                .entity
                .read(cx);
            assert!(
                multiline_code.rendered_text().contains("第一行 第二行"),
                "code text: {:?}",
                multiline_code.rendered_text()
            );
            let multiline_prefix = "跨行的行内代码：\n".len();
            assert!(
                multiline_code.inline_spans().iter().any(|span| {
                    span.style.code
                        && span.range
                            == (multiline_prefix..multiline_prefix + "第一行 第二行".len())
                }),
                "spans: {:?}",
                multiline_code
                    .inline_spans()
                    .iter()
                    .map(|span| (span.range.clone(), span.style.code))
                    .collect::<Vec<_>>()
            );

            let backtick_sample = visible
                .iter()
                .find(|block| {
                    block
                        .entity
                        .read(cx)
                        .rendered_text()
                        .starts_with("正文中的反引号：")
                })
                .expect("literal backtick sample")
                .entity
                .read(cx);
            assert_eq!(
                backtick_sample.rendered_text(),
                "正文中的反引号：` 以及 `` 以及 ```"
            );
            let backtick_prefix = "正文中的反引号：".len();
            let separator = " 以及 ".len();
            let second_start = backtick_prefix + 1 + separator;
            let third_start = second_start + 2 + separator;
            let expected_code_ranges = vec![
                backtick_prefix..backtick_prefix + 1,
                second_start..second_start + 2,
                third_start..third_start + 3,
            ];
            let actual_code_ranges = backtick_sample
                .inline_spans()
                .iter()
                .filter(|span| span.style.code)
                .map(|span| span.range.clone())
                .collect::<Vec<_>>();
            assert_eq!(actual_code_ranges, expected_code_ranges);
            assert!(!backtick_sample.inline_style_at(backtick_prefix + 1).code);
            assert!(!backtick_sample.inline_style_at(second_start + 2).code);

            assert!(
                visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind() == BlockKind::Quote && block.rendered_text().contains("引用段落二")
                }),
                "missing the second quote; kinds: {kinds:?}"
            );

            assert!(
                visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind() == BlockKind::Table
                        && block
                            .record
                            .table
                            .as_ref()
                            .is_some_and(|table| table.header.len() == 3 && table.rows.len() >= 2)
                }),
                "missing the 3-column table; kinds: {kinds:?}"
            );

            assert!(
                visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind() == BlockKind::HtmlBlock
                        && block.rendered_text().contains("<details>")
                }),
                "missing the details html block; kinds: {kinds:?}"
            );

            assert!(
                !visible.iter().any(|block| {
                    let block = block.entity.read(cx);
                    block.kind() == BlockKind::RawMarkdown
                        && block.rendered_text().contains("- 混合列表项")
                }),
                "list item should not degrade to raw markdown; kinds: {kinds:?}"
            );
        });
    }

    #[rgpui::test]
    async fn list_followed_by_blank_line_and_root_paragraph_stays_separate(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- item\n\ntext".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "item");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "text");
            assert_eq!(visible[1].entity.read(cx).render_depth, 0);
        });
    }

    #[rgpui::test]
    async fn mode_switch_preserves_root_paragraph_after_list(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- item\n\ntext".to_string(), None));

        editor.update(cx, |editor, cx| {
            editor.toggle_view_mode(cx);
            assert!(matches!(editor.view_mode, super::super::ViewMode::Source));
            editor.toggle_view_mode(cx);
            assert!(matches!(editor.view_mode, super::super::ViewMode::Rendered));

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "text");
            assert_eq!(visible[1].entity.read(cx).render_depth, 0);
        });
    }

    #[rgpui::test]
    async fn list_empty_root_and_following_paragraph_stay_outside_list(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "- item\n\n\ntext".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).render_depth, 0);
            assert_eq!(visible[2].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "text");
            assert_eq!(visible[2].entity.read(cx).render_depth, 0);
        });
    }

    #[rgpui::test]
    async fn blank_line_then_indented_text_upgrades_to_native_list_child_paragraph(
        cx: &mut TestAppContext,
    ) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "- item\n\n    child text".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(
                visible[0].entity.read(cx).kind(),
                BlockKind::BulletedListItem
            );
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "item");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "child text");
            assert_eq!(visible[1].entity.read(cx).render_depth, 1);
            assert_eq!(editor.document.markdown_text(cx), "- item\n\n  child text");
        });
    }

    #[rgpui::test]
    async fn preserves_reference_definitions_and_stops_quote_at_first_non_quoted_line(
        cx: &mut TestAppContext,
    ) {
        let reference_editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "[id]: http://example.com/\n    \"Title\"".to_string(),
                None,
            )
        });
        reference_editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::RawMarkdown);
            assert_eq!(
                editor.document.markdown_text(cx),
                "[id]: http://example.com/\n    \"Title\""
            );
        });

        let quote_editor =
            cx.new(|cx| Editor::from_markdown(cx, "> quoted\ncontinued".to_string(), None));
        quote_editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "quoted");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "continued");
            assert_eq!(editor.document.markdown_text(cx), "> quoted\n\ncontinued");
        });
    }

    #[rgpui::test]
    async fn simple_quote_does_not_consume_following_root_blocks(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(
                cx,
                "> quoted line\n> second line\n\n---\n\n## Next".to_string(),
                None,
            )
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "quoted line second line"
            );
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Separator);
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::Heading { level: 2 }
            );
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "Next");
        });
    }

    #[rgpui::test]
    async fn non_quoted_line_after_quote_becomes_plain_paragraph_before_heading(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(cx, "> quoted\ncontinued\n\n## Next".to_string(), None)
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Quote);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "quoted");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "continued");
            assert_eq!(
                visible[2].entity.read(cx).kind(),
                BlockKind::Heading { level: 2 }
            );
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "Next");
        });
    }

    #[rgpui::test]
    async fn preserves_empty_root_blocks_across_round_trip(cx: &mut TestAppContext) {
        let editor =
            cx.new(|cx| Editor::from_markdown(cx, "alpha\n\n\nbeta\n\n".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 4);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "alpha");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "beta");
            assert_eq!(visible[3].entity.read(cx).rendered_text(), "");
            assert_eq!(editor.document.markdown_text(cx), "alpha\n\n\nbeta\n\n");
        });

        editor.update(cx, |editor, cx| {
            editor.toggle_view_mode(cx);
            assert!(matches!(editor.view_mode, super::super::ViewMode::Source));
            editor.toggle_view_mode(cx);
            assert!(matches!(editor.view_mode, super::super::ViewMode::Rendered));

            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 4);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "alpha");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "beta");
            assert_eq!(visible[3].entity.read(cx).rendered_text(), "");
        });
    }

    #[rgpui::test]
    async fn imports_blank_line_inside_inline_code_as_single_paragraph(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "`line 1\n\nline 2`".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            let block = visible[0].entity.read(cx);
            let text = "line 1  line 2";
            assert_eq!(block.kind(), BlockKind::Paragraph);
            assert_eq!(block.rendered_text(), text);
            assert!(
                block
                    .inline_spans()
                    .iter()
                    .any(|span| { span.style.code && span.range == (0..text.len()) })
            );
            assert_eq!(editor.document.markdown_text(cx), "`line 1  line 2`");
        });
    }

    #[rgpui::test]
    async fn unclosed_inline_code_does_not_absorb_blank_line_paragraph(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "`line 1\n\nline 2".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 2);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "`line 1");
            assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "line 2");
        });
    }

    #[rgpui::test]
    async fn preserves_multiple_leading_blank_lines_as_empty_blocks(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "\n\nalpha".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "alpha");
            assert_eq!(editor.document.markdown_text(cx), "\n\nalpha");
        });
    }

    #[rgpui::test]
    async fn preserves_multiple_trailing_blank_lines_as_empty_blocks(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\n\n".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 3);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "alpha");
            assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
            assert_eq!(visible[2].entity.read(cx).rendered_text(), "");
            assert_eq!(editor.document.markdown_text(cx), "alpha\n\n\n");
        });
    }

    #[rgpui::test]
    async fn single_trailing_newline_does_not_create_visible_empty_block(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n".to_string(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "alpha");
            assert_eq!(editor.document.markdown_text(cx), "alpha");
        });
    }

    #[rgpui::test]
    async fn empty_document_keeps_single_editable_empty_block(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Paragraph);
            assert_eq!(visible[0].entity.read(cx).rendered_text(), "");
            assert_eq!(editor.document.markdown_text(cx), "");
        });
    }

    #[rgpui::test]
    async fn multiline_paragraph_collapses_soft_break_to_single_line(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            Editor::from_markdown(cx, "跨行的行内代码：\n`第一行\n第二行`".to_string(), None)
        });

        editor.update(cx, |editor, cx| {
            let visible = editor.document.visible_blocks();
            assert_eq!(visible.len(), 1);
            assert_eq!(
                visible[0].entity.read(cx).rendered_text(),
                "跨行的行内代码： 第一行 第二行"
            );
            assert_eq!(
                editor.document.markdown_text(cx),
                "跨行的行内代码： `第一行 第二行`"
            );
        });
    }
}
