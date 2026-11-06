use std::ops::Range;

use super::super::footnote::{InlineFootnoteReference, parse_inline_footnote_reference};
use super::super::html::{
    HtmlAttr, HtmlInlineStyle, HtmlNode, HtmlNodeKind, has_dangerous_attrs, is_inline_tag,
    parse_html_attrs, style_for_node,
};
use super::super::link::{LinkReferenceDefinition, LinkReferenceDefinitions, parse_link_target};
use super::{
    EmphasisDelimiter, InlineEditOutcome, InlineFragment, InlineLink, InlineMath,
    InlineMathDelimiter, InlineStyle, InlineTextTree, push_merged_fragment,
};

pub(super) fn normalize_inline_fragments(
    fragments: &[InlineFragment],
    reference_definitions: &LinkReferenceDefinitions,
) -> InlineEditOutcome {
    let input_len = fragments.iter().map(|fragment| fragment.text.len()).sum();
    let tokens = flatten_tokens(fragments);
    let mut builder = InlineNormalizer::new(input_len);
    let _ = parse_until(
        &tokens,
        0,
        None,
        InlineStyle::default(),
        None,
        &mut builder,
        false,
        reference_definitions,
    );
    InlineEditOutcome {
        tree: InlineTextTree::from_fragments(builder.fragments),
        visible_to_normalized: builder.visible_to_normalized,
    }
}

/// 行内解析用的源码字符、样式与字节范围。
#[derive(Clone)]
struct InlineCharToken {
    ch: char,
    style: InlineStyle,
    html_style: Option<HtmlInlineStyle>,
    source_range: Range<usize>,
}

/// 解析带分隔符行内区域的结果。
struct InlineParseResult {
    next_index: usize,
    closed: bool,
}

/// 归一化（标记解析）期间构建输出片段。
/// 维护可见偏移到归一化偏移的映射，使选区与光标可映射到归一化树。
struct InlineNormalizer {
    fragments: Vec<InlineFragment>,
    visible_to_normalized: Vec<usize>,
    normalized_len: usize,
}

/// 解析一段区间前的构造器快照，区间最终未闭合时用它丢弃已写出的内容。
struct NormalizerCheckpoint {
    /// 快照时的片段数量。
    fragment_count: usize,
    /// 快照时末尾游程的文本长度；同游程的合并写入会延长它，回退时需要一并截断。
    tail_text_len: usize,
    /// 快照时的归一化长度。
    normalized_len: usize,
    /// 回退时映射退回「未输出」的源码起点（起始符自身的位置）。
    source_start: usize,
}

impl InlineNormalizer {
    /// 新建归一化构造器。
    fn new(input_len: usize) -> Self {
        Self {
            fragments: Vec::new(),
            visible_to_normalized: vec![0; input_len + 1],
            normalized_len: 0,
        }
    }

    /// 记录当前输出状态，供 `rollback` 使用。
    fn checkpoint(&self, source_start: usize) -> NormalizerCheckpoint {
        NormalizerCheckpoint {
            fragment_count: self.fragments.len(),
            tail_text_len: self
                .fragments
                .last()
                .map(|fragment| fragment.text.len())
                .unwrap_or(0),
            normalized_len: self.normalized_len,
            source_start,
        }
    }

    /// 回退到快照：删掉其间写出的片段与文本，起点之后的源码映射先退回未输出，
    /// 再由外层按字面量重新写出。
    fn rollback(&mut self, checkpoint: NormalizerCheckpoint) {
        self.fragments.truncate(checkpoint.fragment_count);
        if let Some(last) = self.fragments.last_mut() {
            last.text.truncate(checkpoint.tail_text_len);
        }
        self.normalized_len = checkpoint.normalized_len;
        for boundary in checkpoint.source_start..self.visible_to_normalized.len() {
            self.visible_to_normalized[boundary] = checkpoint.normalized_len;
        }
    }

    /// 丢弃标记字符。
    fn drop_token(&mut self, token: &InlineCharToken) {
        for boundary in token.source_range.start..=token.source_range.end {
            self.visible_to_normalized[boundary] = self.normalized_len;
        }
    }

    /// 输出字符为片段。
    fn emit_token(
        &mut self,
        token: &InlineCharToken,
        extra_style: InlineStyle,
        html_style: Option<HtmlInlineStyle>,
    ) {
        let mut style = token.style;
        if extra_style.bold {
            style.bold = true;
        }
        if extra_style.italic {
            style.italic = true;
        }
        if extra_style.underline {
            style.underline = true;
        }
        if extra_style.strikethrough {
            style.strikethrough = true;
        }
        if extra_style.code {
            style.code = true;
        }
        if extra_style.has_script() {
            style.script = extra_style.script;
        }
        let html_style = merge_html_styles(html_style, token.html_style);

        let text = token.ch.to_string();
        let start = self.normalized_len;
        for boundary in token.source_range.start..=token.source_range.end {
            self.visible_to_normalized[boundary] = start + (boundary - token.source_range.start);
        }
        self.normalized_len += text.len();

        if let Some(last) = self.fragments.last_mut()
            && last.style == style
            && last.html_style == html_style
            && last.link.is_none()
            && last.footnote.is_none()
            && last.math.is_none()
        {
            last.text.push_str(&text);
            return;
        }

        self.fragments.push(InlineFragment {
            text,
            style,
            html_style,
            link: None,
            footnote: None,
            math: None,
        });
    }

    /// 输出一段解码后的文本（如 HTML 实体），并把其源码范围映射回归一化偏移。
    fn emit_decoded(
        &mut self,
        source_range: Range<usize>,
        text: &str,
        style: InlineStyle,
        html_style: Option<HtmlInlineStyle>,
    ) {
        let normalized_start = self.normalized_len;
        let source_len = source_range.len().max(1);
        for boundary in source_range.start..=source_range.end {
            let mapped = text.len() * (boundary - source_range.start) / source_len;
            self.visible_to_normalized[boundary] = normalized_start + mapped;
        }
        self.normalized_len += text.len();

        if let Some(last) = self.fragments.last_mut()
            && last.style == style
            && last.html_style == html_style
            && last.link.is_none()
            && last.footnote.is_none()
            && last.math.is_none()
        {
            last.text.push_str(text);
            return;
        }

        self.fragments.push(InlineFragment {
            text: text.to_string(),
            style,
            html_style,
            link: None,
            footnote: None,
            math: None,
        });
    }

    /// 输出行内公式片段。
    fn emit_inline_math(
        &mut self,
        tokens: &[InlineCharToken],
        math: InlineMath,
        extra_style: InlineStyle,
        extra_html_style: Option<HtmlInlineStyle>,
    ) {
        let source_start = tokens
            .first()
            .map(|token| token.source_range.start)
            .unwrap_or(0);
        let normalized_start = self.normalized_len;
        let source = math.source.clone();
        let plain_len = source.len();

        for token in tokens {
            let token_len = token.source_range.len();
            for delta in 0..=token_len {
                self.visible_to_normalized[token.source_range.start + delta] =
                    normalized_start + (token.source_range.start + delta - source_start);
            }
        }

        self.normalized_len += plain_len;
        self.fragments.push(InlineFragment {
            text: source,
            style: extra_style,
            html_style: extra_html_style,
            link: None,
            footnote: None,
            math: Some(math),
        });
    }
}

/// 拍平片段为字符流。
fn flatten_tokens(fragments: &[InlineFragment]) -> Vec<InlineCharToken> {
    let mut tokens = Vec::new();
    let mut visible_offset = 0;

    for fragment in fragments {
        for ch in fragment.text.chars() {
            let len = ch.len_utf8();
            tokens.push(InlineCharToken {
                ch,
                style: fragment.style,
                html_style: fragment.html_style,
                source_range: visible_offset..visible_offset + len,
            });
            visible_offset += len;
        }
    }

    tokens
}

/// 递归下降解析器，消费 [`InlineCharToken`] 并重建归一化行内树。
/// 配对的分隔符会被消费（丢弃），未配对的按字面文本输出，
/// 嵌套样式由累积 `extra_style` 的递归调用处理。
// 解析管线按扁平传参约定透传词法上下文，打包结构体反而引入生命周期复杂度。
#[allow(clippy::too_many_arguments)]
fn parse_until(
    tokens: &[InlineCharToken],
    mut index: usize,
    end_delimiter: Option<EmphasisDelimiter>,
    extra_style: InlineStyle,
    extra_html_style: Option<HtmlInlineStyle>,
    builder: &mut InlineNormalizer,
    inside_code: bool,
    reference_definitions: &LinkReferenceDefinitions,
) -> InlineParseResult {
    let body_start = index;
    let opener_index = end_delimiter
        .map(|delimiter| body_start.saturating_sub(delimiter.emphasis_len()))
        .unwrap_or(0);
    while index < tokens.len() {
        // 检查结束分隔符。
        if let Some(end_delim) = end_delimiter
            && emphasis_close_at(tokens, body_start, opener_index, end_delim, index)
        {
            let close_len = end_delim.close().chars().count();
            for token in &tokens[index..index + close_len] {
                builder.drop_token(token);
            }
            return InlineParseResult {
                next_index: index + close_len,
                closed: true,
            };
        }

        if !inside_code
            && let Some(next_index) =
                parse_inline_math(tokens, index, extra_style, extra_html_style, builder)
        {
            index = next_index;
            continue;
        }

        if !inside_code
            && tokens[index].ch == '\\'
            && let Some(escaped_len) = escaped_sequence_token_len(tokens, index)
        {
            builder.drop_token(&tokens[index]);
            let escaped_start = index + 1;
            let escaped_end = escaped_start + escaped_len;
            for token in &tokens[escaped_start..escaped_end] {
                builder.emit_token(token, extra_style, extra_html_style);
            }
            index = escaped_end;
            continue;
        }

        if !inside_code
            && tokens[index].ch == '\\'
            && let Some(next) = tokens.get(index + 1)
            && matches!(next.ch, '\n' | '\r')
        {
            // 行尾反斜杠为硬换行：转为两空格硬换行，隐藏反斜杠且保持往返。
            let source_range = tokens[index].source_range.start..next.source_range.end;
            let replacement = format!("  {}", next.ch);
            builder.emit_decoded(source_range, &replacement, extra_style, extra_html_style);
            index += 2;
            continue;
        }

        if !inside_code
            && tokens[index].ch == '&'
            && let Some((consumed, decoded)) = decode_html_entity(tokens, index)
        {
            let source_range =
                tokens[index].source_range.start..tokens[index + consumed - 1].source_range.end;
            builder.emit_decoded(source_range, &decoded, extra_style, extra_html_style);
            index += consumed;
            continue;
        }

        if !inside_code
            && tokens[index].ch == ':'
            && let Some((consumed, emoji)) = decode_emoji_shortcode(tokens, index)
        {
            let source_range =
                tokens[index].source_range.start..tokens[index + consumed - 1].source_range.end;
            builder.emit_decoded(source_range, emoji, extra_style, extra_html_style);
            index += consumed;
            continue;
        }

        // 代码段内所有文本（含标记）均按字面处理。
        if !inside_code {
            if tokens[index].ch == '['
                && let Some(next_index) =
                    parse_footnote_reference(tokens, index, extra_style, extra_html_style, builder)
            {
                index = next_index;
                continue;
            }

            if let Some(next_index) = parse_inline_link(
                tokens,
                index,
                extra_style,
                extra_html_style,
                builder,
                reference_definitions,
            ) {
                index = next_index;
                continue;
            }

            if tokens[index].ch == '<'
                && let Some(next_index) = parse_inline_html_container(
                    tokens,
                    index,
                    extra_style,
                    extra_html_style,
                    builder,
                    reference_definitions,
                )
            {
                index = next_index;
                continue;
            }

            if tokens[index].ch == '<'
                && let Some(next_index) = parse_autolink(
                    tokens,
                    index,
                    extra_style,
                    extra_html_style,
                    builder,
                    reference_definitions,
                )
            {
                index = next_index;
                continue;
            }

            if let Some(delimiter) = match_open_delimiter(tokens, index) {
                let literal_run = delimiter.token_len();
                if has_closing_delimiter(tokens, index, delimiter) {
                    let checkpoint = builder.checkpoint(tokens[index].source_range.start);
                    for token in &tokens[index..index + literal_run] {
                        builder.drop_token(token);
                    }
                    let is_code_delim = matches!(delimiter, EmphasisDelimiter::CodeMarkdown { .. });
                    let parsed = parse_until(
                        tokens,
                        index + literal_run,
                        Some(delimiter),
                        extra_style.apply(delimiter),
                        extra_html_style,
                        builder,
                        is_code_delim,
                        reference_definitions,
                    );
                    if parsed.closed {
                        index = parsed.next_index;
                        continue;
                    }

                    // 递归最终没闭上：预扫描是扁平的，遇到区间内更短的嵌套分隔符会判得偏乐观。
                    // 回退递归已经写出的正文，否则会和外层重放的 token 叠加成重复文本。
                    builder.rollback(checkpoint);
                }

                // 未闭合的多字符起始符（`**`、`__`、`~~`、反引号串）整体按字面保留，
                // 若只输出首字符，剩余部分会开启更短的区间并在每次按键提交，
                // 从而丢失原本的加粗意图。
                if literal_run > 1 {
                    for token in &tokens[index..index + literal_run] {
                        builder.emit_token(token, extra_style, extra_html_style);
                    }
                    index += literal_run;
                    continue;
                }
            }
        }

        builder.emit_token(&tokens[index], extra_style, extra_html_style);
        index += 1;
    }

    InlineParseResult {
        next_index: tokens.len(),
        closed: false,
    }
}

/// 判断 `index` 处能否作为 `end_delim` 的结束符。
///
/// `parse_until` 的闭合判定与 `has_closing_delimiter` 的预扫描共用本函数，
/// 两处口径一致，预扫描才不会在有嵌套分隔符的区间上误报「一定能闭合」。
/// 即便误报，`parse_until` 也会在递归前打快照、闭不上时回滚，正文不会被写两遍。
fn emphasis_close_at(
    tokens: &[InlineCharToken],
    body_start: usize,
    opener_index: usize,
    end_delim: EmphasisDelimiter,
    index: usize,
) -> bool {
    let closed = match end_delim {
        EmphasisDelimiter::CodeMarkdown { run_len } => {
            tokens[index].ch == '`' && backtick_run_len(tokens, index) == run_len
        }
        EmphasisDelimiter::SuperscriptMarkdown => {
            tokens[index].ch == '^' && can_close_emphasis(tokens, index, end_delim)
        }
        EmphasisDelimiter::SubscriptMarkdown => {
            is_single_tilde_delimiter(tokens, index) && can_close_emphasis(tokens, index, end_delim)
        }
        EmphasisDelimiter::StrikethroughMarkdown => {
            is_exactly_two_tildes(tokens, index) && can_close_emphasis(tokens, index, end_delim)
        }
        _ => {
            matches_sequence(tokens, index, &end_delim.close())
                && can_close_emphasis(tokens, index, end_delim)
                && !emphasis_pair_blocked(tokens, opener_index, end_delim, index)
        }
    };

    // 强调区间必须包含至少一个字符，紧贴正文起始的结束符视为字面文本。
    closed && !(index == body_start && emphasis_requires_body(end_delim))
}

/// 解析行内公式。
fn parse_inline_math(
    tokens: &[InlineCharToken],
    index: usize,
    extra_style: InlineStyle,
    extra_html_style: Option<HtmlInlineStyle>,
    builder: &mut InlineNormalizer,
) -> Option<usize> {
    let (body_start, close_start, close_end, delimiter) = if tokens.get(index)?.ch == '$' {
        if matches_sequence(tokens, index, "$$") || token_is_backslash_escaped(tokens, index) {
            return None;
        }
        let close = locate_inline_dollar_math_close(tokens, index + 1)?;
        (index + 1, close, close, InlineMathDelimiter::Dollar)
    } else if matches_sequence(tokens, index, "\\(") {
        let close = locate_inline_paren_math_close(tokens, index + 2)?;
        (index + 2, close, close + 1, InlineMathDelimiter::Paren)
    } else {
        return None;
    };

    if body_start >= close_start {
        return None;
    }
    if tokens[body_start..close_start]
        .iter()
        .any(|token| token.ch == '\n' || token.ch == '\r')
    {
        return None;
    }
    if tokens[body_start].ch.is_whitespace() || tokens[close_start - 1].ch.is_whitespace() {
        return None;
    }

    let source = tokens_to_string(&tokens[index..=close_end]);
    let body = tokens_to_string(&tokens[body_start..close_start]);
    if looks_like_obvious_currency(tokens, index, close_end, &body) {
        return None;
    }

    let math = InlineMath {
        source,
        body,
        delimiter,
    };
    builder.emit_inline_math(
        &tokens[index..=close_end],
        math,
        extra_style,
        extra_html_style,
    );
    Some(close_end + 1)
}

/// 定位美元公式结尾。
fn locate_inline_dollar_math_close(tokens: &[InlineCharToken], mut cursor: usize) -> Option<usize> {
    while cursor < tokens.len() {
        let token = &tokens[cursor];
        if token.ch == '\n' || token.ch == '\r' {
            return None;
        }
        if token.ch == '$'
            && !token_is_backslash_escaped(tokens, cursor)
            && !matches_sequence(tokens, cursor, "$$")
        {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

/// 定位括号公式结尾。
fn locate_inline_paren_math_close(tokens: &[InlineCharToken], mut cursor: usize) -> Option<usize> {
    while cursor + 1 < tokens.len() {
        if tokens[cursor].ch == '\n' || tokens[cursor].ch == '\r' {
            return None;
        }
        if matches_sequence(tokens, cursor, "\\)") {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

/// 判断是否被反斜杠转义。
fn token_is_backslash_escaped(tokens: &[InlineCharToken], index: usize) -> bool {
    if index == 0 {
        return false;
    }
    let mut cursor = index;
    let mut slash_count = 0usize;
    while cursor > 0 && tokens[cursor - 1].ch == '\\' {
        slash_count += 1;
        cursor -= 1;
    }
    slash_count % 2 == 1
}

/// 判断是否为金额而非公式。
fn looks_like_obvious_currency(
    tokens: &[InlineCharToken],
    open_index: usize,
    close_index: usize,
    body: &str,
) -> bool {
    let prev_is_digit = open_index
        .checked_sub(1)
        .and_then(|idx| tokens.get(idx))
        .is_some_and(|token| token.ch.is_ascii_digit());
    let next_is_digit = tokens
        .get(close_index + 1)
        .is_some_and(|token| token.ch.is_ascii_digit());
    if prev_is_digit || next_is_digit {
        return true;
    }

    body.chars()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | ',' | '_'))
        && body.chars().any(|ch| ch.is_ascii_digit())
        && body.len() > 1
}

/// 解析脚注引用。
fn parse_footnote_reference(
    tokens: &[InlineCharToken],
    index: usize,
    extra_style: InlineStyle,
    extra_html_style: Option<HtmlInlineStyle>,
    builder: &mut InlineNormalizer,
) -> Option<usize> {
    if tokens.get(index)?.ch != '[' || tokens.get(index + 1)?.ch != '^' {
        return None;
    }

    let mut cursor = index + 2;
    let end_index = loop {
        let token = tokens.get(cursor)?;
        if token.ch == '\\' {
            cursor += 2;
            continue;
        }
        if token.ch == ']' {
            break cursor;
        }
        cursor += 1;
    };

    let raw_markdown = tokens_to_string(&tokens[index..=end_index]);
    let id = parse_inline_footnote_reference(&raw_markdown)?;
    let fragments = vec![InlineFragment {
        text: raw_markdown.clone(),
        style: extra_style,
        html_style: extra_html_style,
        link: None,
        footnote: Some(InlineFootnoteReference {
            id,
            ordinal: None,
            occurrence_index: 0,
        }),
        math: None,
    }];

    let normalized_start = builder.normalized_len;
    let plain_len = raw_markdown.len();
    let normalized_end = normalized_start + plain_len;
    for token in &tokens[index..=end_index] {
        let token_len = token.source_range.len();
        for delta in 0..=token_len {
            builder.visible_to_normalized[token.source_range.start + delta] = normalized_start
                + (token.source_range.start + delta - tokens[index].source_range.start);
        }
    }

    for fragment in fragments {
        builder.normalized_len += fragment.text.len();
        push_merged_fragment(&mut builder.fragments, fragment);
    }

    for boundary in tokens[end_index].source_range.end..=tokens[end_index].source_range.end {
        builder.visible_to_normalized[boundary] = normalized_end;
    }

    Some(end_index + 1)
}

/// 解析行内链接。
fn parse_inline_link(
    tokens: &[InlineCharToken],
    index: usize,
    extra_style: InlineStyle,
    extra_html_style: Option<HtmlInlineStyle>,
    builder: &mut InlineNormalizer,
    reference_definitions: &LinkReferenceDefinitions,
) -> Option<usize> {
    let located = locate_inline_link(tokens, index, reference_definitions)?;
    let label_end = located.label_end;
    let label_tokens = &tokens[index + 1..label_end];
    let label_markdown = tokens_to_string(label_tokens);
    let mut label_result = InlineTextTree::plain(label_markdown)
        .normalize_inline_syntax_with_link_references(reference_definitions);
    apply_extra_style_to_fragments(
        &mut label_result.tree.fragments,
        extra_style,
        extra_html_style,
    );
    let link = located.link;

    let normalized_start = builder.normalized_len;
    let label_len = label_result.tree.plain_len();

    for boundary in tokens[index].source_range.start..=tokens[index].source_range.end {
        builder.visible_to_normalized[boundary] = normalized_start;
    }

    let mut local_boundary = 0usize;
    for token in label_tokens {
        let token_len = token.source_range.len();
        for delta in 0..=token_len {
            builder.visible_to_normalized[token.source_range.start + delta] =
                normalized_start + label_result.visible_to_normalized[local_boundary + delta];
        }
        local_boundary += token_len;
    }

    let normalized_end = normalized_start + label_len;
    for token in &tokens[label_end..=located.end_index] {
        for boundary in token.source_range.start..=token.source_range.end {
            builder.visible_to_normalized[boundary] = normalized_end;
        }
    }

    for mut fragment in label_result.tree.fragments {
        fragment.link = Some(link.clone());
        fragment.footnote = None;
        fragment.math = None;
        builder.normalized_len += fragment.text.len();
        push_merged_fragment(&mut builder.fragments, fragment);
    }

    Some(located.end_index + 1)
}

/// 解析自动链接。
fn parse_autolink(
    tokens: &[InlineCharToken],
    index: usize,
    extra_style: InlineStyle,
    extra_html_style: Option<HtmlInlineStyle>,
    builder: &mut InlineNormalizer,
    _reference_definitions: &LinkReferenceDefinitions,
) -> Option<usize> {
    let end_index = locate_autolink(tokens, index)?;
    let target_tokens = &tokens[index + 1..end_index];
    let target = tokens_to_string(target_tokens);
    let fragments = vec![InlineFragment {
        text: target.clone(),
        style: extra_style,
        html_style: extra_html_style,
        link: Some(InlineLink::Autolink {
            target: target.clone(),
        }),
        footnote: None,
        math: None,
    }];

    let normalized_start = builder.normalized_len;
    let target_len = target.len();

    for boundary in tokens[index].source_range.start..=tokens[index].source_range.end {
        builder.visible_to_normalized[boundary] = normalized_start;
    }

    let mut local_boundary = 0usize;
    for token in target_tokens {
        let token_len = token.source_range.len();
        for delta in 0..=token_len {
            builder.visible_to_normalized[token.source_range.start + delta] =
                normalized_start + local_boundary + delta;
        }
        local_boundary += token_len;
    }

    let normalized_end = normalized_start + target_len;
    for boundary in tokens[end_index].source_range.start..=tokens[end_index].source_range.end {
        builder.visible_to_normalized[boundary] = normalized_end;
    }

    for fragment in fragments {
        builder.normalized_len += fragment.text.len();
        push_merged_fragment(&mut builder.fragments, fragment);
    }

    Some(end_index + 1)
}

#[derive(Clone, Debug)]
struct InlineHtmlTag {
    name: String,
    attrs: Vec<HtmlAttr>,
    end_index: usize,
    self_closing: bool,
}

/// 解析行内容器。
fn parse_inline_html_container(
    tokens: &[InlineCharToken],
    index: usize,
    extra_style: InlineStyle,
    extra_html_style: Option<HtmlInlineStyle>,
    builder: &mut InlineNormalizer,
    reference_definitions: &LinkReferenceDefinitions,
) -> Option<usize> {
    let tag = locate_inline_html_open_tag(tokens, index)?;
    if tag.self_closing || !is_inline_tag(&tag.name) || has_dangerous_attrs(&tag.attrs) {
        return None;
    }

    let (close_start, close_end) =
        locate_matching_inline_html_close(tokens, tag.end_index + 1, &tag.name)?;
    let tag_style = inline_html_semantic_style(&tag.name, extra_style);
    let html_style = merge_html_styles(extra_html_style, inline_html_style(&tag));
    if tag_style == extra_style && html_style == extra_html_style {
        return None;
    }

    for token in &tokens[index..=tag.end_index] {
        builder.drop_token(token);
    }
    let _ = parse_until(
        &tokens[tag.end_index + 1..close_start],
        0,
        None,
        tag_style,
        html_style,
        builder,
        false,
        reference_definitions,
    );
    for token in &tokens[close_start..=close_end] {
        builder.drop_token(token);
    }

    Some(close_end + 1)
}

/// 取标签语义样式。
fn inline_html_semantic_style(name: &str, style: InlineStyle) -> InlineStyle {
    match name {
        "strong" | "b" => style.with_bold(),
        "em" | "i" => style.with_italic(),
        "u" | "ins" => style.with_underline(),
        "del" => style.with_strikethrough(),
        "code" | "kbd" => style.with_code(),
        "sup" => style.with_superscript(),
        "sub" => style.with_subscript(),
        _ => style,
    }
}

/// 取标签行内样式。
fn inline_html_style(tag: &InlineHtmlTag) -> Option<HtmlInlineStyle> {
    let node = HtmlNode {
        kind: HtmlNodeKind::InlineSemantic,
        tag_name: tag.name.clone(),
        attrs: tag.attrs.clone(),
        children: Vec::new(),
        raw_source: String::new(),
        source_range: 0..0,
    };
    let style = style_for_node(&node);
    (!style.is_empty()).then_some(style)
}

/// 合并行内样式。
fn merge_html_styles(
    parent: Option<HtmlInlineStyle>,
    child: Option<HtmlInlineStyle>,
) -> Option<HtmlInlineStyle> {
    let mut merged = parent.unwrap_or_default();
    if let Some(child) = child {
        if child.color.is_some() {
            merged.color = child.color;
        }
        if child.background_color.is_some() {
            merged.background_color = child.background_color;
        }
        if child.font_size.is_some() {
            merged.font_size = child.font_size;
        }
    }

    (!merged.is_empty()).then_some(merged)
}

/// 在字符流中定位到的行内链接语法。
#[derive(Clone)]
struct InlineLinkLocation {
    label_end: usize,
    end_index: usize,
    link: InlineLink,
}

/// 定位行内链接。
fn locate_inline_link(
    tokens: &[InlineCharToken],
    index: usize,
    reference_definitions: &LinkReferenceDefinitions,
) -> Option<InlineLinkLocation> {
    if tokens.get(index)?.ch != '[' {
        return None;
    }
    if index > 0 && matches!(tokens[index - 1].ch, '!' | ']') {
        return None;
    }

    let mut label_depth = 0usize;
    let mut cursor = index + 1;
    let label_end = loop {
        let token = tokens.get(cursor)?;
        if token.ch == '\\' {
            cursor += 2;
            continue;
        }

        match token.ch {
            '[' => label_depth += 1,
            ']' if label_depth == 0 => break cursor,
            ']' => label_depth = label_depth.saturating_sub(1),
            _ => {}
        }
        cursor += 1;
    };

    match tokens.get(label_end + 1).map(|token| token.ch) {
        Some('(') => {
            let url_start = label_end + 2;
            let mut paren_depth = 0usize;
            cursor = url_start;
            let url_end = loop {
                let token = tokens.get(cursor)?;
                if token.ch == '\\' {
                    cursor += 2;
                    continue;
                }

                match token.ch {
                    '(' => paren_depth += 1,
                    ')' if paren_depth == 0 => break cursor,
                    ')' => paren_depth = paren_depth.saturating_sub(1),
                    _ => {}
                }
                cursor += 1;
            };

            // `[label]()` 这类空目标是合法链接，但目标解析器会拒绝空串，
            // 识别它可让光标在补全目标期间停留在投影链接内。
            let (destination, title) = if url_start == url_end {
                (String::new(), None)
            } else {
                parse_link_target(&tokens_to_string(&tokens[url_start..url_end]))?
            };
            Some(InlineLinkLocation {
                label_end,
                end_index: url_end,
                link: InlineLink::Inline { destination, title },
            })
        }
        Some('[') => {
            let reference_start = label_end + 2;
            cursor = reference_start;
            let reference_end = loop {
                let token = tokens.get(cursor)?;
                if token.ch == '\\' {
                    cursor += 2;
                    continue;
                }
                if token.ch == ']' {
                    break cursor;
                }
                cursor += 1;
            };

            let raw_label = tokens_to_string(&tokens[reference_start..reference_end]);
            let link_label = if raw_label.is_empty() {
                tokens_to_string(&tokens[index + 1..label_end])
            } else {
                raw_label
            };
            let normalized_label =
                crate::components::markdown::image::normalize_link_label(&link_label)?;
            let LinkReferenceDefinition { destination, .. } =
                reference_definitions.get(&normalized_label)?.clone();
            Some(InlineLinkLocation {
                label_end,
                end_index: reference_end,
                link: InlineLink::Reference {
                    label: link_label,
                    destination,
                },
            })
        }
        _ => {
            let raw_label = tokens_to_string(&tokens[index + 1..label_end]);
            let normalized_label =
                crate::components::markdown::image::normalize_link_label(&raw_label)?;
            let LinkReferenceDefinition { destination, .. } =
                reference_definitions.get(&normalized_label)?.clone();
            Some(InlineLinkLocation {
                label_end,
                end_index: label_end,
                link: InlineLink::Reference {
                    label: raw_label,
                    destination,
                },
            })
        }
    }
}

/// 定位自动链接。
fn locate_autolink(tokens: &[InlineCharToken], index: usize) -> Option<usize> {
    if tokens.get(index)?.ch != '<' {
        return None;
    }

    let mut cursor = index + 1;
    let end_index = loop {
        let token = tokens.get(cursor)?;
        if token.ch == '\\' {
            cursor += 2;
            continue;
        }
        if token.ch == '>' {
            break cursor;
        }
        cursor += 1;
    };

    let target = tokens_to_string(&tokens[index + 1..end_index]);
    (!target.is_empty() && !looks_like_non_autolink_html_tag(tokens, end_index, &target))
        .then_some(end_index)
}

/// 把字符流拼为字符串。
fn tokens_to_string(tokens: &[InlineCharToken]) -> String {
    tokens.iter().map(|token| token.ch).collect()
}

/// 定位行内开始标签。
fn locate_inline_html_open_tag(tokens: &[InlineCharToken], index: usize) -> Option<InlineHtmlTag> {
    if tokens.get(index)?.ch != '<' {
        return None;
    }

    let mut cursor = index + 1;
    if !tokens.get(cursor)?.ch.is_ascii_alphabetic() {
        return None;
    }
    let name_start = cursor;
    while cursor < tokens.len() && is_html_tag_name_char(tokens[cursor].ch) {
        cursor += 1;
    }
    let name = tokens_to_string(&tokens[name_start..cursor]).to_ascii_lowercase();

    match tokens.get(cursor).map(|token| token.ch) {
        Some(ch) if ch.is_whitespace() || ch == '>' || ch == '/' => {}
        _ => return None,
    }

    let attrs_start = cursor;
    let mut quote = None;
    while cursor < tokens.len() {
        let ch = tokens[cursor].ch;
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            cursor += 1;
            continue;
        }

        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            cursor += 1;
            continue;
        }

        if ch == '>' {
            let attrs_source = tokens_to_string(&tokens[attrs_start..cursor]);
            let self_closing = attrs_source.trim_end().ends_with('/');
            return Some(InlineHtmlTag {
                name,
                attrs: parse_html_attrs(&attrs_source),
                end_index: cursor,
                self_closing,
            });
        }

        cursor += 1;
    }

    None
}

/// 定位行内结束标签。
fn locate_inline_html_close_tag(
    tokens: &[InlineCharToken],
    index: usize,
    expected_name: &str,
) -> Option<usize> {
    if tokens.get(index)?.ch != '<' || tokens.get(index + 1)?.ch != '/' {
        return None;
    }

    let mut cursor = index + 2;
    while tokens
        .get(cursor)
        .is_some_and(|token| token.ch.is_whitespace())
    {
        cursor += 1;
    }
    let name_start = cursor;
    while cursor < tokens.len() && is_html_tag_name_char(tokens[cursor].ch) {
        cursor += 1;
    }
    if name_start == cursor {
        return None;
    }
    let name = tokens_to_string(&tokens[name_start..cursor]).to_ascii_lowercase();
    if name != expected_name {
        return None;
    }
    while tokens
        .get(cursor)
        .is_some_and(|token| token.ch.is_whitespace())
    {
        cursor += 1;
    }
    (tokens.get(cursor)?.ch == '>').then_some(cursor)
}

/// 定位配对结束标签。
fn locate_matching_inline_html_close(
    tokens: &[InlineCharToken],
    mut cursor: usize,
    name: &str,
) -> Option<(usize, usize)> {
    let mut depth = 1usize;
    while cursor < tokens.len() {
        if tokens[cursor].ch != '<' {
            cursor += 1;
            continue;
        }

        if let Some(close_end) = locate_inline_html_close_tag(tokens, cursor, name) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some((cursor, close_end));
            }
            cursor = close_end + 1;
            continue;
        }

        if let Some(open) = locate_inline_html_open_tag(tokens, cursor) {
            if open.name == name && !open.self_closing {
                depth += 1;
            }
            cursor = open.end_index + 1;
            continue;
        }

        cursor += 1;
    }

    None
}

/// 判断是否为非自动链接标签。
fn looks_like_non_autolink_html_tag(
    tokens: &[InlineCharToken],
    end_index: usize,
    target: &str,
) -> bool {
    let target = target.trim();
    if target.starts_with('/') {
        let rest = target.trim_start_matches('/').trim();
        return html_tag_name_with_attrs(rest).is_some();
    }

    if let Some((_tag_name, has_attrs_or_slash)) = html_tag_name_with_attrs(target)
        && has_attrs_or_slash
    {
        return true;
    }

    let Some((tag_name, _)) = html_tag_name_with_attrs(target) else {
        return false;
    };
    let rest = tokens_to_string(&tokens[end_index + 1..]).to_ascii_lowercase();
    let tag_name = tag_name.to_ascii_lowercase();
    rest.contains(&format!("</{tag_name}>"))
}

/// 解析标签名与属性。
fn html_tag_name_with_attrs(target: &str) -> Option<(&str, bool)> {
    if target.is_empty() {
        return None;
    }

    let first = target.as_bytes()[0];
    if !first.is_ascii_alphabetic() {
        return None;
    }

    let mut end = 0usize;
    for (index, ch) in target.char_indices() {
        if is_html_tag_name_char(ch) {
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }

    let raw_rest = &target[end..];
    let rest = raw_rest.trim();
    if rest.is_empty() {
        return Some((&target[..end], false));
    }
    (raw_rest.chars().next().is_some_and(|ch| ch.is_whitespace())
        || rest == "/"
        || rest.starts_with('/'))
    .then_some((&target[..end], true))
}

/// 判断是否为标签名字符。
fn is_html_tag_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')
}

/// 为片段附加额外样式。
fn apply_extra_style_to_fragments(
    fragments: &mut [InlineFragment],
    extra_style: InlineStyle,
    extra_html_style: Option<HtmlInlineStyle>,
) {
    for fragment in fragments {
        if extra_style.bold {
            fragment.style.bold = true;
        }
        if extra_style.italic {
            fragment.style.italic = true;
        }
        if extra_style.underline {
            fragment.style.underline = true;
        }
        if extra_style.strikethrough {
            fragment.style.strikethrough = true;
        }
        if extra_style.code {
            fragment.style.code = true;
        }
        if extra_style.has_script() {
            fragment.style.script = extra_style.script;
        }
        fragment.html_style = merge_html_styles(extra_html_style, fragment.html_style);
    }
}

/// 匹配起始分隔符。
fn match_open_delimiter(tokens: &[InlineCharToken], index: usize) -> Option<EmphasisDelimiter> {
    if matches_sequence(tokens, index, "<strong>") {
        Some(EmphasisDelimiter::BoldHtml)
    } else if matches_sequence(tokens, index, "<em>") {
        Some(EmphasisDelimiter::ItalicHtml)
    } else if matches_sequence(tokens, index, "<u>") {
        Some(EmphasisDelimiter::Underline)
    } else if matches_sequence(tokens, index, "~~") && is_exactly_two_tildes(tokens, index) {
        Some(EmphasisDelimiter::StrikethroughMarkdown)
    } else if matches_sequence(tokens, index, "^") && can_open_script(tokens, index, '^') {
        Some(EmphasisDelimiter::SuperscriptMarkdown)
    } else if is_single_tilde_delimiter(tokens, index) && can_open_script(tokens, index, '~') {
        Some(EmphasisDelimiter::SubscriptMarkdown)
    } else if matches_sequence(tokens, index, "**") && can_open_emphasis(tokens, index, 2, '*') {
        Some(EmphasisDelimiter::BoldMarkdown { marker: '*' })
    } else if matches_sequence(tokens, index, "__") && can_open_emphasis(tokens, index, 2, '_') {
        Some(EmphasisDelimiter::BoldMarkdown { marker: '_' })
    } else if matches_sequence(tokens, index, "*") && can_open_emphasis(tokens, index, 1, '*') {
        Some(EmphasisDelimiter::ItalicMarkdown { marker: '*' })
    } else if matches_sequence(tokens, index, "_") && can_open_emphasis(tokens, index, 1, '_') {
        Some(EmphasisDelimiter::ItalicMarkdown { marker: '_' })
    } else if tokens[index].ch == '`' {
        // 统计连续反引号的长度。
        let run_len = backtick_run_len(tokens, index);
        // 仅当其后不紧跟反引号时，反引号串才是有效起始符，避免重复计数。
        if run_len > 0 {
            Some(EmphasisDelimiter::CodeMarkdown { run_len })
        } else {
            None
        }
    } else {
        None
    }
}

/// 返回自 `index` 起连续反引号的长度。
fn backtick_run_len(tokens: &[InlineCharToken], index: usize) -> usize {
    let mut len = 0;
    while index + len < tokens.len() && tokens[index + len].ch == '`' {
        len += 1;
    }
    // 仅当其前不紧邻反引号时，反引号串才有效，即必须起于 `index`。
    if index > 0 && tokens[index - 1].ch == '`' {
        return 0;
    }
    len
}

/// 判断是否有配对结束符。
fn has_closing_delimiter(
    tokens: &[InlineCharToken],
    index: usize,
    delimiter: EmphasisDelimiter,
) -> bool {
    let skip = delimiter.token_len();

    // 代码段查找等长反引号串，强调只需扫描结束串。
    if let EmphasisDelimiter::CodeMarkdown { .. } = delimiter {
        let mut cursor = index + skip;
        while cursor < tokens.len() {
            if tokens[cursor].ch == '\\'
                && let Some(escaped_len) = escaped_sequence_token_len(tokens, cursor)
            {
                cursor += 1 + escaped_len;
                continue;
            }

            if tokens[cursor].ch == '`' && backtick_run_len(tokens, cursor) == skip {
                return true;
            }

            cursor += 1;
        }
        return false;
    }

    if matches!(
        delimiter,
        EmphasisDelimiter::SuperscriptMarkdown | EmphasisDelimiter::SubscriptMarkdown
    ) {
        let marker = match delimiter {
            EmphasisDelimiter::SuperscriptMarkdown => '^',
            EmphasisDelimiter::SubscriptMarkdown => '~',
            _ => unreachable!(),
        };
        return locate_script_close(tokens, index + skip, marker).is_some();
    }

    let body_start = index + skip;
    let mut cursor = body_start;
    while cursor < tokens.len() {
        if tokens[cursor].ch == '\\'
            && let Some(escaped_len) = escaped_sequence_token_len(tokens, cursor)
        {
            cursor += 1 + escaped_len;
            continue;
        }

        // 与递归解析同一套闭合判定；本扫描是扁平的，区间内若有更短的嵌套分隔符仍可能偏乐观。
        if emphasis_close_at(tokens, body_start, index, delimiter, cursor) {
            return true;
        }

        cursor += 1;
    }

    false
}

/// `delimiter` 是否要求正文非空。强调与删除线必须包含至少一个字符，
/// 代码段可为空，上下标正文已在别处约束。
fn emphasis_requires_body(delimiter: EmphasisDelimiter) -> bool {
    matches!(
        delimiter,
        EmphasisDelimiter::BoldMarkdown { .. }
            | EmphasisDelimiter::ItalicMarkdown { .. }
            | EmphasisDelimiter::StrikethroughMarkdown
            | EmphasisDelimiter::BoldHtml
            | EmphasisDelimiter::ItalicHtml
            | EmphasisDelimiter::Underline
    )
}

/// 定位上下标结尾。
fn locate_script_close(
    tokens: &[InlineCharToken],
    mut cursor: usize,
    marker: char,
) -> Option<usize> {
    let body_start = cursor;
    while cursor < tokens.len() {
        if tokens[cursor].ch == '\\'
            && let Some(escaped_len) = escaped_sequence_token_len(tokens, cursor)
        {
            cursor += 1 + escaped_len;
            continue;
        }

        let is_close = if marker == '~' {
            is_single_tilde_delimiter(tokens, cursor)
        } else {
            tokens[cursor].ch == marker
        };
        if is_close {
            return valid_script_body(tokens, body_start, cursor).then_some(cursor);
        }

        cursor += 1;
    }

    None
}

/// 校验上下标正文。
fn valid_script_body(tokens: &[InlineCharToken], start: usize, end: usize) -> bool {
    start < end
        && tokens[start..end]
            .iter()
            .all(|token| token.ch.is_ascii_alphanumeric())
}

/// 判断是否为单波浪分隔符。
fn is_single_tilde_delimiter(tokens: &[InlineCharToken], index: usize) -> bool {
    tokens.get(index).is_some_and(|token| token.ch == '~')
        && index
            .checked_sub(1)
            .and_then(|prev| tokens.get(prev))
            .is_none_or(|token| token.ch != '~')
        && tokens.get(index + 1).is_none_or(|token| token.ch != '~')
}

/// 判断是否恰为两个波浪号（删除线分隔符不允许夹在更长的波浪号串中）。
fn is_exactly_two_tildes(tokens: &[InlineCharToken], index: usize) -> bool {
    matches_sequence(tokens, index, "~~")
        && index
            .checked_sub(1)
            .and_then(|prev| tokens.get(prev))
            .is_none_or(|token| token.ch != '~')
        && tokens.get(index + 2).is_none_or(|token| token.ch != '~')
}

/// 判断是否匹配字符序列。
fn matches_sequence(tokens: &[InlineCharToken], index: usize, sequence: &str) -> bool {
    sequence
        .chars()
        .enumerate()
        .all(|(offset, ch)| tokens.get(index + offset).is_some_and(|t| t.ch == ch))
}

/// 取转义序列长度。
fn escaped_sequence_token_len(tokens: &[InlineCharToken], index: usize) -> Option<usize> {
    let next_index = index + 1;
    if next_index >= tokens.len() {
        return None;
    }

    if matches_sequence(tokens, next_index, "</strong>") {
        Some(9)
    } else if matches_sequence(tokens, next_index, "<strong>") {
        Some(8)
    } else if matches_sequence(tokens, next_index, "</em>") {
        Some(5)
    } else if matches_sequence(tokens, next_index, "<em>")
        || matches_sequence(tokens, next_index, "</u>")
    {
        Some(4)
    } else if matches_sequence(tokens, next_index, "<u>") {
        Some(3)
    } else if tokens[next_index].ch.is_ascii_punctuation() {
        // CommonMark：反斜杠可转义任意 ASCII 标点，转义后该字符按字面处理。
        Some(1)
    } else {
        None
    }
}

/// 解析从 `&` 起的 HTML 实体，返回消费的字符数与解码结果。
fn decode_html_entity(tokens: &[InlineCharToken], index: usize) -> Option<(usize, String)> {
    let mut cursor = index + 1;
    if tokens.get(cursor)?.ch != '#' {
        // 具名实体：`&name;`
        let start = cursor;
        while tokens
            .get(cursor)
            .is_some_and(|token| token.ch.is_ascii_alphanumeric())
        {
            cursor += 1;
        }
        if cursor == start || tokens.get(cursor)?.ch != ';' {
            return None;
        }
        let name: String = tokens[start..cursor].iter().map(|token| token.ch).collect();
        let decoded = named_entity(&name)?;
        return Some((cursor - index + 1, decoded.to_string()));
    }

    // 数字实体：`&#123;` 或 `&#x1F600;`
    cursor += 1;
    let hexadecimal = tokens
        .get(cursor)
        .is_some_and(|token| matches!(token.ch, 'x' | 'X'));
    if hexadecimal {
        cursor += 1;
    }
    let start = cursor;
    while tokens.get(cursor).is_some_and(|token| {
        if hexadecimal {
            token.ch.is_ascii_hexdigit()
        } else {
            token.ch.is_ascii_digit()
        }
    }) {
        cursor += 1;
    }
    if cursor == start || tokens.get(cursor)?.ch != ';' {
        return None;
    }

    let digits: String = tokens[start..cursor].iter().map(|token| token.ch).collect();
    let code = if hexadecimal {
        u32::from_str_radix(&digits, 16).ok()?
    } else {
        digits.parse::<u32>().ok()?
    };
    // 无效码点（0、代理区、越界）按 CommonMark 替换为 U+FFFD。
    let decoded = if code == 0 || (0xD800..=0xDFFF).contains(&code) {
        '\u{FFFD}'
    } else {
        char::from_u32(code).unwrap_or('\u{FFFD}')
    };
    Some((cursor - index + 1, decoded.to_string()))
}

/// HTML5 具名实体的常用子集。
fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "nbsp" => '\u{00A0}',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "laquo" => '«',
        "raquo" => '»',
        "ldquo" => '“',
        "rdquo" => '”',
        "lsquo" => '‘',
        "rsquo" => '’',
        "bull" => '•',
        "middot" => '·',
        "dagger" => '†',
        "Dagger" => '‡',
        "sect" => '§',
        "para" => '¶',
        "permil" => '‰',
        "prime" => '′',
        "Prime" => '″',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "minus" => '−',
        "lowast" => '∗',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "sup1" => '¹',
        "sup2" => '²',
        "sup3" => '³',
        "micro" => 'µ',
        "pound" => '£',
        "euro" => '€',
        "yen" => '¥',
        "cent" => '¢',
        "curren" => '¤',
        "larr" => '←',
        "uarr" => '↑',
        "rarr" => '→',
        "darr" => '↓',
        "harr" => '↔',
        "infin" => '∞',
        "ne" => '≠',
        "le" => '≤',
        "ge" => '≥',
        "asymp" => '≈',
        "equiv" => '≡',
        "sum" => '∑',
        "prod" => '∏',
        "radic" => '√',
        "int" => '∫',
        "part" => '∂',
        "nabla" => '∇',
        "and" => '∧',
        "or" => '∨',
        "cap" => '∩',
        "cup" => '∪',
        "isin" => '∈',
        "notin" => '∉',
        "ni" => '∋',
        "empty" => '∅',
        "forall" => '∀',
        "exist" => '∃',
        "there4" => '∴',
        "ang" => '∠',
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "epsilon" => 'ε',
        "theta" => 'θ',
        "lambda" => 'λ',
        "mu" => 'μ',
        "pi" => 'π',
        "sigma" => 'σ',
        "phi" => 'φ',
        "omega" => 'ω',
        "spades" => '♠',
        "clubs" => '♣',
        "hearts" => '♥',
        "diams" => '♦',
        "star" => '☆',
        "starf" => '★',
        "check" => '✓',
        "verbar" => '|',
        "lowbar" => '_',
        "sol" => '/',
        _ => return None,
    })
}

/// 解析从 `:` 起的 Emoji 短代码，返回消费的字符数与替换文本。
fn decode_emoji_shortcode(
    tokens: &[InlineCharToken],
    index: usize,
) -> Option<(usize, &'static str)> {
    if tokens.get(index)?.ch != ':' {
        return None;
    }

    let mut cursor = index + 1;
    let start = cursor;
    while tokens.get(cursor).is_some_and(|token| {
        token.ch.is_ascii_alphanumeric() || matches!(token.ch, '_' | '-' | '+')
    }) {
        cursor += 1;
    }
    if cursor == start || tokens.get(cursor)?.ch != ':' {
        return None;
    }

    let name: String = tokens[start..cursor].iter().map(|token| token.ch).collect();
    let emoji = emoji_shortcode(&name)?;
    Some((cursor - index + 1, emoji))
}

/// GitHub Emoji 短代码的常用子集。
fn emoji_shortcode(name: &str) -> Option<&'static str> {
    Some(match name {
        "smile" => "😄",
        "grin" => "😁",
        "laughing" | "satisfied" => "😆",
        "blush" => "😊",
        "heart_eyes" => "😍",
        "kissing_heart" => "😘",
        "wink" => "😉",
        "smirk" => "😏",
        "joy" => "😂",
        "cry" => "😢",
        "sob" => "😭",
        "angry" => "😠",
        "rage" => "😡",
        "scream" => "😱",
        "sunglasses" => "😎",
        "thinking" => "🤔",
        "neutral_face" => "😐",
        "expressionless" => "😑",
        "sleeping" => "😴",
        "mask" => "😷",
        "nerd_face" => "🤓",
        "triumph" => "😤",
        "sunglasses_face" => "😎",
        "heart" => "❤️",
        "broken_heart" => "💔",
        "two_hearts" => "💕",
        "sparkling_heart" => "💖",
        "heartpulse" => "💗",
        "blue_heart" => "💙",
        "green_heart" => "💚",
        "yellow_heart" => "💛",
        "purple_heart" => "💜",
        "black_heart" => "🖤",
        "orange_heart" => "🧡",
        "thumbsup" | "+1" => "👍",
        "thumbsdown" | "-1" => "👎",
        "ok_hand" => "👌",
        "clap" => "👏",
        "pray" => "🙏",
        "muscle" => "💪",
        "wave" => "👋",
        "point_up" => "☝️",
        "point_right" => "👉",
        "point_left" => "👈",
        "point_down" => "👇",
        "raised_hands" => "🙌",
        "handshake" => "🤝",
        "eyes" => "👀",
        "fire" => "🔥",
        "rocket" => "🚀",
        "star" => "⭐",
        "star2" => "🌟",
        "sparkles" => "✨",
        "zap" => "⚡",
        "boom" | "collision" => "💥",
        "warning" => "⚠️",
        "exclamation" => "❗",
        "question" => "❓",
        "heavy_check_mark" => "✔️",
        "white_check_mark" => "✅",
        "x" => "❌",
        "heavy_multiplication_x" => "✖️",
        "bangbang" => "‼️",
        "bulb" => "💡",
        "gear" => "⚙️",
        "wrench" => "🔧",
        "hammer" => "🔨",
        "lock" => "🔒",
        "unlock" => "🔓",
        "key" => "🔑",
        "mag" => "🔍",
        "bell" => "🔔",
        "calendar" | "date" => "📅",
        "hourglass" => "⌛",
        "alarm_clock" => "⏰",
        "gift" => "🎁",
        "trophy" => "🏆",
        "medal_sports" => "🏅",
        "soccer" => "⚽",
        "basketball" => "🏀",
        "football" => "🏈",
        "baseball" => "⚾",
        "tennis" => "🎾",
        "golf" => "⛳",
        "dart" => "🎯",
        "video_game" => "🎮",
        "musical_note" => "🎵",
        "notes" => "🎶",
        "book" => "📖",
        "books" => "📚",
        "memo" => "📝",
        "pencil2" => "✏️",
        "paperclip" => "📎",
        "pushpin" => "📌",
        "round_pushpin" => "📍",
        "scissors" => "✂️",
        "email" => "✉️",
        "phone" => "☎️",
        "computer" => "💻",
        "iphone" => "📱",
        "camera" => "📷",
        "video_camera" => "📹",
        "tv" => "📺",
        "floppy_disk" => "💾",
        "package" => "📦",
        "mailbox" => "📫",
        "moneybag" => "💰",
        "dollar" => "💵",
        "credit_card" => "💳",
        "chart_with_upwards_trend" => "📈",
        "chart_with_downwards_trend" => "📉",
        "bar_chart" => "📊",
        "clipboard" => "📋",
        "coffee" => "☕",
        "beer" => "🍺",
        "beers" => "🍻",
        "wine_glass" => "🍷",
        "cocktail" => "🍸",
        "pizza" => "🍕",
        "hamburger" => "🍔",
        "fries" => "🍟",
        "apple" => "🍎",
        "banana" => "🍌",
        "cake" => "🍰",
        "birthday" => "🎂",
        "icecream" => "🍦",
        "sun" | "sunny" => "☀️",
        "cloud" => "☁️",
        "umbrella" => "☔",
        "snowflake" => "❄️",
        "snowman" => "⛄",
        "rainbow" => "🌈",
        "ocean" => "🌊",
        "earth_asia" => "🌏",
        "globe_with_meridians" => "🌐",
        "moon" => "🌙",
        "sunrise" => "🌅",
        "dog" => "🐶",
        "cat" => "🐱",
        "mouse" => "🐭",
        "hamster" => "🐹",
        "rabbit" => "🐰",
        "bear" => "🐻",
        "panda_face" => "🐼",
        "penguin" => "🐧",
        "bird" => "🐦",
        "bug" => "🐛",
        "bee" => "🐝",
        "ant" => "🐜",
        "snake" => "🐍",
        "turtle" => "🐢",
        "whale" => "🐳",
        "dolphin" => "🐬",
        "fish" => "🐟",
        "dragon" => "🐉",
        "horse" => "🐴",
        "cow" => "🐮",
        "monkey" => "🐒",
        "skull" => "💀",
        "ghost" => "👻",
        "alien" => "👽",
        "robot" => "🤖",
        "poop" | "hankey" => "💩",
        "100" => "💯",
        "ok" => "🆗",
        "new" => "🆕",
        "free" => "🆓",
        "top" => "🔝",
        "soon" => "🔜",
        "no_entry" => "⛔",
        "heavy_plus_sign" => "➕",
        "heavy_minus_sign" => "➖",
        "heavy_division_sign" => "➗",
        "arrow_up" => "⬆️",
        "arrow_down" => "⬇️",
        "arrow_left" => "⬅️",
        "arrow_right" => "➡️",
        "recycle" => "♻️",
        "anchor" => "⚓",
        "dizzy" => "💫",
        "thought_balloon" => "💭",
        "speech_balloon" => "💬",
        "crown" => "👑",
        "gem" => "💎",
        "ring" => "💍",
        "tada" => "🎉",
        "confetti_ball" => "🎊",
        "balloon" => "🎈",
        "checkered_flag" => "🏁",
        _ => return None,
    })
}
/// 判断能否开启强调。
///
/// `_` 按 CommonMark 的左右侧翼规则判定（词内下划线不作起始）；
/// `*` 沿用简化的「后一字符非空白」判定：Toggle 加粗/斜体与局部取消样式的
/// 序列化行为都建立在它之上，改成完整侧翼规则会让 `***ab***`、`1*2*34` 一类
/// 既有用例回退成字面量，属于独立的改造，不在本轮处理。
fn can_open_emphasis(tokens: &[InlineCharToken], index: usize, len: usize, marker: char) -> bool {
    if marker == '_' {
        let (left, right) = delimiter_flanking(tokens, index, len);
        let before_punct = delimiter_char_before(tokens, index).is_some_and(is_ascii_punctuation);
        return left && (!right || before_punct);
    }

    tokens
        .get(index + len)
        .map(|token| !token.ch.is_whitespace())
        .unwrap_or(false)
}

/// 判断能否结束强调。
///
/// `_` 按 CommonMark 侧翼规则判定，词内下划线不作结束；`*` 与其他标记
/// （删除线、上下标、超文本）沿用「结束符前非空白」的简化判定，理由同
/// [`can_open_emphasis`]。
fn can_close_emphasis(
    tokens: &[InlineCharToken],
    index: usize,
    delimiter: EmphasisDelimiter,
) -> bool {
    if delimiter.emphasis_marker() == Some('_') {
        let len = delimiter.emphasis_len();
        let (left, right) = delimiter_flanking(tokens, index, len);
        let after_punct = tokens
            .get(index + len)
            .is_some_and(|token| is_ascii_punctuation(token.ch));
        return right && (!left || after_punct);
    }

    index > 0 && !tokens[index - 1].ch.is_whitespace()
}

/// 判断能否开启上下标。
fn can_open_script(tokens: &[InlineCharToken], index: usize, marker: char) -> bool {
    if token_is_backslash_escaped(tokens, index) {
        return false;
    }

    if marker == '~' && !is_single_tilde_delimiter(tokens, index) {
        return false;
    }

    index > 0
        && tokens[index - 1].ch.is_ascii_alphanumeric()
        && tokens
            .get(index + 1)
            .is_some_and(|token| token.ch.is_ascii_alphanumeric())
}

/// 取指定位置之前的字符。
fn delimiter_char_before(tokens: &[InlineCharToken], index: usize) -> Option<char> {
    index
        .checked_sub(1)
        .and_then(|prev| tokens.get(prev))
        .map(|token| token.ch)
}

/// 判断字符是否为 ASCII 标点（CommonMark 标点集合的近似）。
fn is_ascii_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation()
}

/// 计算强调分隔符的左右侧翼状态，返回 `(left_flanking, right_flanking)`。
fn delimiter_flanking(tokens: &[InlineCharToken], index: usize, len: usize) -> (bool, bool) {
    let before = delimiter_char_before(tokens, index);
    let after = tokens.get(index + len).map(|token| token.ch);

    let before_whitespace = before.is_none_or(char::is_whitespace);
    let after_whitespace = after.is_none_or(char::is_whitespace);
    let before_punct = before.is_some_and(is_ascii_punctuation);
    let after_punct = after.is_some_and(is_ascii_punctuation);

    let left = !after_whitespace && (!after_punct || before_whitespace || before_punct);
    let right = !before_whitespace && (!before_punct || after_whitespace || after_punct);
    (left, right)
}

/// 取自 `index` 起连续 `marker` 的长度。
fn marker_run_len(tokens: &[InlineCharToken], index: usize, marker: char) -> usize {
    let mut len = 0;
    while tokens
        .get(index + len)
        .is_some_and(|token| token.ch == marker)
    {
        len += 1;
    }
    len
}

/// CommonMark「规则 3」：当开/闭分隔符之一两侧翼皆可（既可开启又可结束）时，
/// 若两者长度之和为 3 的倍数且二者长度不全为 3 的倍数，则该对不能配对。
///
/// 该规则避免 `*斜体**加粗**文本*` 中的内层 `**` 被误判为外层 `*` 的结束符。
fn emphasis_pair_blocked(
    tokens: &[InlineCharToken],
    opener_index: usize,
    delimiter: EmphasisDelimiter,
    closer_index: usize,
) -> bool {
    let (marker, opener_len) = match delimiter {
        EmphasisDelimiter::BoldMarkdown { marker } => (marker, 2),
        EmphasisDelimiter::ItalicMarkdown { marker } => (marker, 1),
        _ => return false,
    };
    let closer_len = marker_run_len(tokens, closer_index, marker);
    if closer_len == 0 {
        return false;
    }

    let (open_left, open_right) = delimiter_flanking(tokens, opener_index, opener_len);
    let (close_left, close_right) = delimiter_flanking(tokens, closer_index, closer_len);
    (open_left && open_right || close_left && close_right)
        && (opener_len + closer_len).is_multiple_of(3)
        && !(opener_len.is_multiple_of(3) && closer_len.is_multiple_of(3))
}
