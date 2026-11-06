use super::{
    EmphasisDelimiter, HtmlInlineStyle, InlineFragment, InlineScript, InlineStyle,
    MarkdownOffsetMap,
};

pub(super) fn serialize_fragment_run_markdown_with_offset_map(
    fragments: &[InlineFragment],
) -> MarkdownOffsetMap {
    if fragments.is_empty() {
        return MarkdownOffsetMap {
            markdown: String::new(),
            visible_to_markdown: vec![0],
            markdown_to_visible: vec![0],
        };
    }

    let stacks = choose_fragment_stacks(fragments);
    let mut output = String::new();
    let total_visible_len = fragments
        .iter()
        .map(|fragment| fragment.text.len())
        .sum::<usize>();
    let mut visible_to_markdown = vec![0; total_visible_len + 1];
    let mut markdown_to_visible = vec![0];
    let mut current_stack: Vec<EmphasisDelimiter> = Vec::new();
    let mut current_html_style: Option<HtmlInlineStyle> = None;
    let mut visible_cursor = 0usize;

    for (fragment, next_stack) in fragments.iter().zip(stacks.iter()) {
        if current_html_style != fragment.html_style {
            let transition = stack_transition_string(&current_stack, &[]);
            push_markdown_marker(
                &mut output,
                &mut markdown_to_visible,
                visible_cursor,
                &transition,
            );
            current_stack.clear();

            if current_html_style.is_some() {
                push_markdown_marker(
                    &mut output,
                    &mut markdown_to_visible,
                    visible_cursor,
                    "</span>",
                );
            }
            if let Some(style) = fragment.html_style
                && let Some(marker) = html_style_open_marker(style)
            {
                push_markdown_marker(
                    &mut output,
                    &mut markdown_to_visible,
                    visible_cursor,
                    &marker,
                );
            }
            current_html_style = fragment.html_style;
        }

        let transition = stack_transition_string(&current_stack, next_stack);
        let transition_start = output.len();
        output.push_str(&transition);
        markdown_to_visible.resize(output.len() + 1, visible_cursor);
        for local in 0..=transition.len() {
            markdown_to_visible[transition_start + local] = visible_cursor;
        }

        let escaped = if let Some(math) = fragment.math.as_ref() {
            identity_text_with_offset_map(&math.source)
        } else if fragment.style.code {
            escape_code_span_text_with_offset_map(&fragment.text)
        } else {
            escape_literal_text_with_offset_map(&fragment.text)
        };
        let escaped_start = output.len();
        output.push_str(escaped.markdown());
        for local_visible in 0..=fragment.text.len() {
            visible_to_markdown[visible_cursor + local_visible] =
                escaped_start + escaped.visible_to_markdown_offset(local_visible);
        }
        markdown_to_visible.resize(output.len() + 1, visible_cursor);
        for local_markdown in 0..=escaped.markdown().len() {
            markdown_to_visible[escaped_start + local_markdown] =
                visible_cursor + escaped.markdown_to_visible_offset(local_markdown);
        }
        visible_cursor += fragment.text.len();
        current_stack = next_stack.clone();
    }

    let transition = stack_transition_string(&current_stack, &[]);
    push_markdown_marker(
        &mut output,
        &mut markdown_to_visible,
        visible_cursor,
        &transition,
    );
    if current_html_style.is_some() {
        push_markdown_marker(
            &mut output,
            &mut markdown_to_visible,
            visible_cursor,
            "</span>",
        );
    }

    MarkdownOffsetMap {
        markdown: output,
        visible_to_markdown,
        markdown_to_visible,
    }
}

fn push_markdown_marker(
    output: &mut String,
    markdown_to_visible: &mut Vec<usize>,
    visible_cursor: usize,
    marker: &str,
) {
    if marker.is_empty() {
        return;
    }
    let marker_start = output.len();
    output.push_str(marker);
    markdown_to_visible.resize(output.len() + 1, visible_cursor);
    for local in 0..=marker.len() {
        markdown_to_visible[marker_start + local] = visible_cursor;
    }
}

fn identity_text_with_offset_map(text: &str) -> MarkdownOffsetMap {
    MarkdownOffsetMap {
        markdown: text.to_string(),
        visible_to_markdown: (0..=text.len()).collect(),
        markdown_to_visible: (0..=text.len()).collect(),
    }
}

fn html_style_open_marker(style: HtmlInlineStyle) -> Option<String> {
    style
        .to_css()
        .map(|css| format!("<span style=\"{}\">", escape_html_attr(&css)))
}

fn escape_html_attr(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// 转义普通文本并建映射。
fn escape_literal_text_with_offset_map(text: &str) -> MarkdownOffsetMap {
    let mut escaped = String::new();
    let mut visible_to_markdown = vec![0; text.len() + 1];
    let mut markdown_to_visible = vec![0];
    let mut index = 0;

    while index < text.len() {
        visible_to_markdown[index] = escaped.len();
        if text[index..].starts_with("</strong>") {
            let start = escaped.len();
            escaped.push('\\');
            escaped.push_str("</strong>");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 9;
            continue;
        }

        if text[index..].starts_with("<strong>") {
            let start = escaped.len();
            escaped.push('\\');
            escaped.push_str("<strong>");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 8;
            continue;
        }

        if text[index..].starts_with("</em>") {
            let start = escaped.len();
            escaped.push('\\');
            escaped.push_str("</em>");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 5;
            continue;
        }

        if text[index..].starts_with("<em>") {
            let start = escaped.len();
            escaped.push('\\');
            escaped.push_str("<em>");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 4;
            continue;
        }

        if text[index..].starts_with("</u>") {
            let start = escaped.len();
            escaped.push('\\');
            escaped.push_str("</u>");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 4;
            continue;
        }

        if text[index..].starts_with("<u>") {
            let start = escaped.len();
            escaped.push('\\');
            escaped.push_str("<u>");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 3;
            continue;
        }

        if text[index..].starts_with('\\') {
            let start = escaped.len();
            escaped.push_str("\\\\");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 1;
            continue;
        }

        if text[index..].starts_with('*') {
            let start = escaped.len();
            escaped.push_str("\\*");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 1;
            continue;
        }

        if text[index..].starts_with('_') {
            let start = escaped.len();
            escaped.push_str("\\_");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 1;
            continue;
        }

        if text[index..].starts_with('~') {
            let start = escaped.len();
            escaped.push_str("\\~");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 1;
            continue;
        }

        if text[index..].starts_with('^') {
            let start = escaped.len();
            escaped.push_str("\\^");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 1;
            continue;
        }

        if text[index..].starts_with('`') {
            let start = escaped.len();
            escaped.push_str("\\`");
            markdown_to_visible.resize(escaped.len() + 1, index);
            for local in 0..=escaped.len() - start {
                markdown_to_visible[start + local] = index;
            }
            index += 1;
            continue;
        }

        let ch = text[index..].chars().next().unwrap();
        let start = escaped.len();
        escaped.push(ch);
        markdown_to_visible.resize(escaped.len() + 1, index);
        for local in 0..=escaped.len() - start {
            markdown_to_visible[start + local] = index;
        }
        index += ch.len_utf8();
    }
    visible_to_markdown[text.len()] = escaped.len();
    markdown_to_visible[escaped.len()] = text.len();

    MarkdownOffsetMap {
        markdown: escaped,
        visible_to_markdown,
        markdown_to_visible,
    }
}

/// 转义代码段文本并建映射。
fn escape_code_span_text_with_offset_map(text: &str) -> MarkdownOffsetMap {
    let needs_padding = !text.is_empty()
        && !text.chars().all(|ch| ch == ' ')
        && (text.starts_with([' ', '`']) || text.ends_with([' ', '`']));
    let leading_padding = usize::from(needs_padding);

    let mut markdown = String::new();
    if needs_padding {
        markdown.push(' ');
    }
    markdown.push_str(text);
    if needs_padding {
        markdown.push(' ');
    }

    let mut visible_to_markdown = vec![0; text.len() + 1];
    for (visible, markdown_offset) in visible_to_markdown.iter_mut().enumerate() {
        *markdown_offset = leading_padding + visible;
    }

    let content_start = leading_padding;
    let content_end = leading_padding + text.len();
    let mut markdown_to_visible = vec![0; markdown.len() + 1];
    for (markdown_offset, visible) in markdown_to_visible.iter_mut().enumerate() {
        *visible = if markdown_offset <= content_start {
            0
        } else if markdown_offset >= content_end {
            text.len()
        } else {
            markdown_offset - content_start
        };
    }

    MarkdownOffsetMap {
        markdown,
        visible_to_markdown,
        markdown_to_visible,
    }
}

/// 类维特比动态规划，为每个片段选择最优分隔符栈。
/// 每个片段的样式可用 Markdown 或超文本分隔符表达，
/// 优化目标为分隔符字符总数加超文本惩罚，
/// 当切换会产生 4 个以上连续 `*`（Markdown 歧义）时加大惩罚。
fn choose_fragment_stacks(fragments: &[InlineFragment]) -> Vec<Vec<EmphasisDelimiter>> {
    // 为每个片段样式枚举一到两种可能的分隔符栈。
    let variants = fragments
        .iter()
        .enumerate()
        .map(|(index, fragment)| {
            stack_variants(
                fragment,
                index.checked_sub(1).and_then(|i| fragments.get(i)),
            )
        })
        .collect::<Vec<_>>();

    // 动态规划表：costs[片段下标][方案下标]。
    let mut costs: Vec<Vec<usize>> = variants
        .iter()
        .map(|choices| vec![usize::MAX; choices.len()])
        .collect();
    let mut previous_choice: Vec<Vec<Option<usize>>> = variants
        .iter()
        .map(|choices| vec![None; choices.len()])
        .collect();

    // 首片段：由空栈到各方案的代价。
    for (choice_index, stack) in variants[0].iter().enumerate() {
        costs[0][choice_index] = stack_transition_cost(&[], stack) + stack_variant_penalty(stack);
    }

    // 前向：计算每个片段各方案的最小代价。
    for fragment_index in 1..variants.len() {
        for (choice_index, stack) in variants[fragment_index].iter().enumerate() {
            for (prev_index, prev_stack) in variants[fragment_index - 1].iter().enumerate() {
                let prev_cost = costs[fragment_index - 1][prev_index];
                if prev_cost == usize::MAX {
                    continue;
                }

                let cost = prev_cost
                    + stack_transition_cost(prev_stack, stack)
                    + stack_variant_penalty(stack);
                if cost < costs[fragment_index][choice_index] {
                    costs[fragment_index][choice_index] = cost;
                    previous_choice[fragment_index][choice_index] = Some(prev_index);
                }
            }
        }
    }

    // 回溯：选择最优终栈并沿动态规划表回追踪。
    let last_fragment_index = variants.len() - 1;
    let (mut best_choice, _) = variants[last_fragment_index]
        .iter()
        .enumerate()
        .map(|(choice_index, stack)| {
            (
                choice_index,
                costs[last_fragment_index][choice_index] + stack_transition_cost(stack, &[]),
            )
        })
        .min_by(|(left_index, left_cost), (right_index, right_cost)| {
            left_cost.cmp(right_cost).then_with(|| {
                stack_preference_key(&variants[last_fragment_index][*left_index]).cmp(
                    &stack_preference_key(&variants[last_fragment_index][*right_index]),
                )
            })
        })
        .unwrap_or((0, 0));

    let mut chosen = vec![Vec::new(); variants.len()];
    for fragment_index in (0..variants.len()).rev() {
        chosen[fragment_index] = variants[fragment_index][best_choice].clone();
        if let Some(prev_index) = previous_choice[fragment_index][best_choice] {
            best_choice = prev_index;
        }
    }

    chosen
}

/// 枚举分隔符栈方案。
fn stack_variants(
    fragment: &InlineFragment,
    previous_fragment: Option<&InlineFragment>,
) -> Vec<Vec<EmphasisDelimiter>> {
    let style = fragment.style;
    let code_run_len = style.code.then(|| code_delimiter_run_len(&fragment.text));
    let mut markdown_stack = Vec::new();
    if style.bold {
        markdown_stack.push(EmphasisDelimiter::BoldMarkdown { marker: '*' });
    }
    if style.underline {
        markdown_stack.push(EmphasisDelimiter::Underline);
    }
    if style.strikethrough {
        markdown_stack.push(EmphasisDelimiter::StrikethroughMarkdown);
    }
    match style.script {
        InlineScript::Normal => {}
        InlineScript::Superscript
            if can_use_markdown_script_delimiters(previous_fragment, fragment) =>
        {
            markdown_stack.push(EmphasisDelimiter::SuperscriptMarkdown)
        }
        InlineScript::Superscript => markdown_stack.push(EmphasisDelimiter::SuperscriptHtml),
        InlineScript::Subscript
            if style.strikethrough
                || !can_use_markdown_script_delimiters(previous_fragment, fragment) =>
        {
            markdown_stack.push(EmphasisDelimiter::SubscriptHtml)
        }
        InlineScript::Subscript => markdown_stack.push(EmphasisDelimiter::SubscriptMarkdown),
    }
    if style.italic {
        markdown_stack.push(EmphasisDelimiter::ItalicMarkdown { marker: '*' });
    }
    // 代码恒为最内层分隔符，以便嵌套在强调之内。
    if let Some(run_len) = code_run_len {
        markdown_stack.push(EmphasisDelimiter::CodeMarkdown { run_len });
    }

    let has_emphasis = style.bold || style.italic;
    if !has_emphasis {
        return vec![markdown_stack];
    }

    let mut html_stack = Vec::new();
    if style.bold {
        html_stack.push(EmphasisDelimiter::BoldHtml);
    }
    if style.underline {
        html_stack.push(EmphasisDelimiter::Underline);
    }
    if style.strikethrough {
        html_stack.push(EmphasisDelimiter::StrikethroughMarkdown);
    }
    match style.script {
        InlineScript::Normal => {}
        InlineScript::Superscript => html_stack.push(EmphasisDelimiter::SuperscriptHtml),
        InlineScript::Subscript => html_stack.push(EmphasisDelimiter::SubscriptHtml),
    }
    if style.italic {
        html_stack.push(EmphasisDelimiter::ItalicHtml);
    }
    if let Some(run_len) = code_run_len {
        html_stack.push(EmphasisDelimiter::CodeMarkdown { run_len });
    }

    vec![markdown_stack, html_stack]
}

/// 判断能否用简写上下标符。
pub(crate) fn can_use_markdown_script_delimiters(
    previous_fragment: Option<&InlineFragment>,
    fragment: &InlineFragment,
) -> bool {
    // 该保护由序列化与行内投影共用。Markdown 上下标标记要求其前紧邻
    // 普通 ASCII 属主，否则回退到 <sup>/<sub>，使下次解析看到相同的样式边界。
    let Some(previous) = previous_fragment else {
        return false;
    };
    if previous.style.has_script() {
        return false;
    }
    previous
        .text
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_ascii_alphanumeric())
        && previous.html_style == fragment.html_style
        && previous.link == fragment.link
        && previous.footnote.is_none()
        && fragment.footnote.is_none()
        && previous.math.is_none()
        && fragment.math.is_none()
        && styles_match_ignoring_script(previous.style, fragment.style)
}

/// 忽略上下标比较样式。
fn styles_match_ignoring_script(left: InlineStyle, right: InlineStyle) -> bool {
    left.bold == right.bold
        && left.italic == right.italic
        && left.underline == right.underline
        && left.strikethrough == right.strikethrough
        && left.code == right.code
}

/// 计算代码分隔符长度。
fn code_delimiter_run_len(text: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in text.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest + 1
}

/// 计算栈切换长度。
fn stack_transition_len(from: &[EmphasisDelimiter], to: &[EmphasisDelimiter]) -> usize {
    let common = common_prefix_len(from, to);
    let close_len = from[common..]
        .iter()
        .rev()
        .map(|delimiter| delimiter.close().len())
        .sum::<usize>();
    let open_len = to[common..]
        .iter()
        .map(|delimiter| delimiter.open().len())
        .sum::<usize>();
    close_len + open_len
}

/// 依次关闭 `from` 再开启 `to` 分隔符的代价。
/// 若结果串含 4 个以上连续 `*`（Markdown 解析有歧义），则加重惩罚。
fn stack_transition_cost(from: &[EmphasisDelimiter], to: &[EmphasisDelimiter]) -> usize {
    let marker_len = stack_transition_len(from, to);
    let marker_string = stack_transition_string(from, to);
    let ambiguity_penalty =
        if !from.is_empty() && !to.is_empty() && longest_star_run(&marker_string) >= 4 {
            1_000
        } else {
            0
        };
    marker_len + ambiguity_penalty
}

/// 计算栈方案惩罚。
fn stack_variant_penalty(stack: &[EmphasisDelimiter]) -> usize {
    if stack.iter().any(|delimiter| delimiter.is_html()) {
        64
    } else {
        0
    }
}

/// 写出栈切换标记。
fn write_stack_transition(
    output: &mut String,
    from: &[EmphasisDelimiter],
    to: &[EmphasisDelimiter],
) {
    let common = common_prefix_len(from, to);
    for delimiter in from[common..].iter().rev() {
        output.push_str(&delimiter.close());
    }
    for delimiter in &to[common..] {
        output.push_str(&delimiter.open());
    }
}

/// 生成栈切换字符串。
fn stack_transition_string(from: &[EmphasisDelimiter], to: &[EmphasisDelimiter]) -> String {
    let mut output = String::new();
    write_stack_transition(&mut output, from, to);
    output
}

/// 计算栈公共前缀长。
fn common_prefix_len(left: &[EmphasisDelimiter], right: &[EmphasisDelimiter]) -> usize {
    let mut index = 0;
    while index < left.len() && index < right.len() && left[index] == right[index] {
        index += 1;
    }
    index
}

/// 取栈偏好键。
fn stack_preference_key(stack: &[EmphasisDelimiter]) -> Vec<u8> {
    stack
        .iter()
        .map(|delimiter| delimiter.preference_rank())
        .collect()
}

/// 取最长星号连写。
fn longest_star_run(text: &str) -> usize {
    let mut max_run = 0;
    let mut current_run = 0;
    for ch in text.chars() {
        if ch == '*' {
            current_run += 1;
            max_run = max_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    max_run
}
