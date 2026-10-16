//! 行内投影引擎，用于可编辑的 Markdown 分隔符。

use std::ops::Range;

use crate::components::InlineFootnoteReference;
use crate::components::markdown::inline::{
    InlineFragment, InlineLink, InlineRenderCache, InlineScript, InlineStyle, InlineStyleFlag,
    InlineTextTree, can_use_markdown_script_delimiters,
};

use super::CaretAffinity;

/// 展开行内投影中的一个展示片段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct InlineExpansionSegment {
    pub(super) display_range: Range<usize>,
    pub(super) clean_range: Range<usize>,
    pub(super) fragment_index: usize,
    pub(super) link_group: Option<usize>,
    pub(super) kind: ExpandedInlineSegmentKind,
}

/// Markdown 分隔符可投影编辑的行内构造。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InlineExpansionKind {
    /// 链接标签与目标语法。
    Link,
    /// 粗体 Markdown 分隔符。
    BoldMarkdown,
    /// 斜体 Markdown 分隔符。
    ItalicMarkdown,
    /// 删除线分隔符。
    Strikethrough,
    /// 代码段反引号分隔符。
    Code,
    /// 上标 Markdown 分隔符。
    SuperscriptMarkdown,
    /// 上标 HTML 分隔符。
    SuperscriptHtml,
    /// 下标 Markdown 分隔符。
    SubscriptMarkdown,
    /// 下标 HTML 分隔符。
    SubscriptHtml,
}

impl InlineExpansionKind {
    /// 判断是否适用于指定样式。
    fn applies_to(self, style: InlineStyle) -> bool {
        match self {
            Self::Link => false,
            Self::BoldMarkdown => style.bold,
            Self::ItalicMarkdown => style.italic,
            Self::Strikethrough => style.strikethrough,
            Self::Code => style.code,
            Self::SuperscriptMarkdown | Self::SuperscriptHtml => {
                style.script == InlineScript::Superscript
            }
            Self::SubscriptMarkdown | Self::SubscriptHtml => {
                style.script == InlineScript::Subscript
            }
        }
    }

    /// 取起始标记。
    fn open_marker(self) -> &'static str {
        match self {
            Self::Link => "[",
            Self::BoldMarkdown => "**",
            Self::ItalicMarkdown => "*",
            Self::Strikethrough => "~~",
            Self::Code => "`",
            Self::SuperscriptMarkdown => "^",
            Self::SuperscriptHtml => "<sup>",
            Self::SubscriptMarkdown => "~",
            Self::SubscriptHtml => "<sub>",
        }
    }

    /// 取结束标记。
    fn close_marker(self) -> &'static str {
        match self {
            Self::Link => ")",
            Self::SuperscriptHtml => "</sup>",
            Self::SubscriptHtml => "</sub>",
            _ => self.open_marker(),
        }
    }

    /// 取样式标记。
    pub(super) fn style_flag(self) -> Option<InlineStyleFlag> {
        match self {
            Self::Link => None,
            Self::BoldMarkdown => Some(InlineStyleFlag::Bold),
            Self::ItalicMarkdown => Some(InlineStyleFlag::Italic),
            Self::Strikethrough => Some(InlineStyleFlag::Strikethrough),
            Self::Code => Some(InlineStyleFlag::Code),
            Self::SuperscriptMarkdown | Self::SuperscriptHtml => Some(InlineStyleFlag::Superscript),
            Self::SubscriptMarkdown | Self::SubscriptHtml => Some(InlineStyleFlag::Subscript),
        }
    }

    /// 取投影优先级。
    fn projection_rank(self) -> u8 {
        match self {
            Self::Link => 0,
            Self::BoldMarkdown => 1,
            Self::Strikethrough => 2,
            Self::SuperscriptMarkdown
            | Self::SuperscriptHtml
            | Self::SubscriptMarkdown
            | Self::SubscriptHtml => 3,
            Self::ItalicMarkdown => 4,
            Self::Code => 5,
        }
    }
}

/// 单个投影行内片段的展示角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExpandedInlineSegmentKind {
    /// 无投影行内语法的文本。
    PlainText,
    /// 携带投影样式的文本。
    StyledText,
    /// 起始分隔符如 `[` 或反引号。
    OpeningDelimiter(InlineExpansionKind),
    /// 中部分隔符如链接的 `](`。
    MiddleDelimiter(InlineExpansionKind),
    /// 可编辑的链接目标文本。
    LinkTargetText,
    /// 可编辑的脚注标识文本。
    FootnoteIdText,
    /// 结束分隔符如 `)` 或反引号。
    ClosingDelimiter(InlineExpansionKind),
}

/// 横跨一个或多个行内片段的投影链接游程。
#[derive(Clone, Debug)]
pub(super) struct LinkExpansionRun {
    pub(super) link: InlineLink,
    pub(super) start_fragment_index: usize,
    pub(super) end_fragment_index: usize,
    pub(super) clean_range: Range<usize>,
    pub(super) display_range: Range<usize>,
    pub(super) target_display_range: Range<usize>,
}

/// 单个投影脚注引用游程。
#[derive(Clone, Debug)]
pub(super) struct FootnoteExpansionRun {
    pub(super) footnote: InlineFootnoteReference,
    pub(super) clean_range: Range<usize>,
    pub(super) display_range: Range<usize>,
}

/// 平移到展开链接展示范围的选区快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LinkSelectionSnapshot {
    pub(super) clean_range: Range<usize>,
    pub(super) display_relative_range: Range<usize>,
    pub(super) selection_reversed: bool,
}

/// 展开行内投影的渲染缓存与偏移映射。
#[derive(Clone, Debug)]
pub(crate) struct InlineExpansionProjection {
    pub(super) cache: InlineRenderCache,
    pub(super) segments: Vec<InlineExpansionSegment>,
    pub(super) clean_to_display_cursor: Vec<usize>,
    pub(super) display_to_clean: Vec<usize>,
    pub(super) link_runs: Vec<LinkExpansionRun>,
    pub(super) footnote_runs: Vec<FootnoteExpansionRun>,
}

#[cfg(test)]
pub(super) fn expanded_display_offset_for_clean(
    fragments: &[InlineFragment],
    clean: usize,
) -> usize {
    let mut display = 0usize;
    let mut clean_cursor = 0usize;
    for fragment in fragments {
        let clean_len = fragment.text.len();
        let clean_end = clean_cursor + clean_len;
        if clean <= clean_end {
            let off = clean.saturating_sub(clean_cursor);
            if (fragment.style.code || fragment.style.script != InlineScript::Normal)
                && clean_len > 0
            {
                if off == clean_len {
                    return display + clean_len + 2;
                }
                return display + 1 + off;
            }
            return display + off;
        }
        clean_cursor = clean_end;
        display += if (fragment.style.code || fragment.style.script != InlineScript::Normal)
            && clean_len > 0
        {
            clean_len + 2
        } else {
            clean_len
        };
    }
    display
}

#[cfg(test)]
pub(super) fn expanded_display_cursor_offset_for_clean(
    fragments: &[InlineFragment],
    clean: usize,
) -> usize {
    let mut display = 0usize;
    let mut clean_cursor = 0usize;
    for fragment in fragments {
        let clean_len = fragment.text.len();
        let clean_end = clean_cursor + clean_len;
        if clean <= clean_end {
            let off = clean.saturating_sub(clean_cursor);
            if (fragment.style.code || fragment.style.script != InlineScript::Normal)
                && clean_len > 0
            {
                if off == 0 {
                    return display + 1;
                }
                if off >= clean_len {
                    return display + clean_len + 1;
                }
                return display + 1 + off;
            }
            return display + off;
        }
        clean_cursor = clean_end;
        display += if (fragment.style.code || fragment.style.script != InlineScript::Normal)
            && clean_len > 0
        {
            clean_len + 2
        } else {
            clean_len
        };
    }
    display
}

impl InlineExpansionProjection {
    /// 构建投影：仅对光标、选区或输入法标记触及的片段暴露分隔符，同时保留回纯文本偏移的映射。
    pub(super) fn build(
        fragments: &[InlineFragment],
        clean_selected: Range<usize>,
        clean_marked: Option<Range<usize>>,
    ) -> Option<Self> {
        let clean_len = fragments
            .iter()
            .map(|fragment| fragment.text.len())
            .sum::<usize>();
        let mut projected_fragments = Vec::new();
        let mut segments = Vec::new();
        let mut clean_to_display_cursor = vec![0; clean_len + 1];
        let mut display_to_clean = vec![0];
        let mut link_runs = Vec::new();
        let mut footnote_runs = Vec::new();
        let mut clean_cursor = 0usize;
        let mut display_cursor = 0usize;
        let mut any_expanded = false;
        let mut fragment_index = 0usize;

        while fragment_index < fragments.len() {
            let fragment = &fragments[fragment_index];
            let fragment_len = fragment.text.len();
            if fragment_len == 0 {
                fragment_index += 1;
                continue;
            }

            if let Some(footnote) = fragment.footnote.as_ref() {
                let clean_range = clean_cursor..clean_cursor + fragment_len;
                let expand_footnote = Self::fragment_is_touched(
                    clean_range.clone(),
                    &clean_selected,
                    clean_marked.as_ref(),
                );
                let run_display_start = display_cursor;
                if expand_footnote {
                    any_expanded = true;
                    let open_marker = "[^".to_string();
                    let open_len = open_marker.len();
                    projected_fragments.push(InlineFragment {
                        text: open_marker,
                        style: InlineStyle::default(),
                        html_style: None,
                        link: None,
                        footnote: None,
                        math: None,
                    });
                    segments.push(InlineExpansionSegment {
                        display_range: display_cursor..display_cursor + open_len,
                        clean_range: clean_range.start..clean_range.start,
                        fragment_index,
                        link_group: None,
                        kind: ExpandedInlineSegmentKind::OpeningDelimiter(
                            InlineExpansionKind::Link,
                        ),
                    });
                    for _ in 0..open_len {
                        display_to_clean.push(clean_range.start);
                    }
                    display_cursor += open_len;

                    let id_text = footnote.id.clone();
                    let id_len = id_text.len();
                    projected_fragments.push(InlineFragment {
                        text: id_text,
                        style: fragment.style,
                        html_style: fragment.html_style,
                        link: None,
                        footnote: Some(footnote.clone()),
                        math: None,
                    });
                    segments.push(InlineExpansionSegment {
                        display_range: display_cursor..display_cursor + id_len,
                        clean_range: clean_range.clone(),
                        fragment_index,
                        link_group: None,
                        kind: ExpandedInlineSegmentKind::FootnoteIdText,
                    });
                    for offset in 0..=fragment_len {
                        let mapped = (id_len * offset).checked_div(fragment_len).unwrap_or(0);
                        clean_to_display_cursor[clean_range.start + offset] =
                            display_cursor + mapped;
                    }
                    for offset in 1..=id_len {
                        let mapped = (fragment_len * offset).checked_div(id_len).unwrap_or(0);
                        display_to_clean.push(clean_range.start + mapped);
                    }
                    display_cursor += id_len;
                    let close_marker = "]".to_string();
                    let close_len = close_marker.len();
                    projected_fragments.push(InlineFragment {
                        text: close_marker,
                        style: InlineStyle::default(),
                        html_style: None,
                        link: None,
                        footnote: None,
                        math: None,
                    });
                    segments.push(InlineExpansionSegment {
                        display_range: display_cursor..display_cursor + close_len,
                        clean_range: clean_range.end..clean_range.end,
                        fragment_index,
                        link_group: None,
                        kind: ExpandedInlineSegmentKind::ClosingDelimiter(
                            InlineExpansionKind::Link,
                        ),
                    });
                    for _ in 0..close_len {
                        display_to_clean.push(clean_range.end);
                    }
                    display_cursor += close_len;

                    footnote_runs.push(FootnoteExpansionRun {
                        footnote: footnote.clone(),
                        clean_range: clean_range.clone(),
                        display_range: run_display_start..display_cursor,
                    });
                } else {
                    projected_fragments.push(fragment.clone());
                    segments.push(InlineExpansionSegment {
                        display_range: display_cursor..display_cursor + fragment_len,
                        clean_range: clean_range.clone(),
                        fragment_index,
                        link_group: None,
                        kind: ExpandedInlineSegmentKind::PlainText,
                    });
                    for offset in 0..=fragment_len {
                        clean_to_display_cursor[clean_range.start + offset] =
                            display_cursor + offset;
                    }
                    for offset in 1..=fragment_len {
                        display_to_clean.push(clean_range.start + offset);
                    }
                    display_cursor += fragment_len;
                }

                clean_cursor = clean_range.end;
                fragment_index += 1;
                continue;
            }

            if let Some(link) = fragment.link.as_ref() {
                let run_start = fragment_index;
                let run_clean_start = clean_cursor;
                let mut run_end = fragment_index;
                let mut run_clean_end = clean_cursor;
                while run_end < fragments.len() {
                    let run_fragment = &fragments[run_end];
                    if run_fragment.link.as_ref() != Some(link) {
                        break;
                    }
                    run_clean_end += run_fragment.text.len();
                    run_end += 1;
                }

                let run_clean_range = run_clean_start..run_clean_end;
                let expand_link = Self::fragment_is_touched(
                    run_clean_range.clone(),
                    &clean_selected,
                    clean_marked.as_ref(),
                );
                let link_group = expand_link.then_some(link_runs.len());
                let run_display_start = display_cursor;
                if expand_link {
                    any_expanded = true;
                    let open_marker = link.open_marker().to_string();
                    let open_len = open_marker.len();
                    projected_fragments.push(InlineFragment {
                        text: open_marker,
                        style: InlineStyle::default(),
                        html_style: None,
                        link: None,
                        footnote: None,
                        math: None,
                    });
                    segments.push(InlineExpansionSegment {
                        display_range: display_cursor..display_cursor + open_len,
                        clean_range: run_clean_start..run_clean_start,
                        fragment_index: run_start,
                        link_group,
                        kind: ExpandedInlineSegmentKind::OpeningDelimiter(
                            InlineExpansionKind::Link,
                        ),
                    });
                    for _ in 0..open_len {
                        display_to_clean.push(run_clean_start);
                    }
                    display_cursor += open_len;
                }

                let mut local_clean_cursor = run_clean_start;
                for current_index in run_start..run_end {
                    let current_fragment = &fragments[current_index];
                    let current_len = current_fragment.text.len();
                    let current_clean_range = local_clean_cursor..local_clean_cursor + current_len;
                    // While the link is expanded, reveal each label fragment's
                    // own emphasis markers so anchor text edits like ordinary text.
                    let label_kinds = if expand_link {
                        Self::expanded_kinds_for_fragment(
                            fragments,
                            current_index,
                            current_fragment.style,
                            current_clean_range.clone(),
                            &clean_selected,
                            clean_marked.as_ref(),
                        )
                    } else {
                        Vec::new()
                    };
                    push_projected_fragment(
                        current_fragment,
                        current_index,
                        current_clean_range.clone(),
                        &label_kinds,
                        link_group,
                        expand_link,
                        &mut projected_fragments,
                        &mut segments,
                        &mut clean_to_display_cursor,
                        &mut display_to_clean,
                        &mut display_cursor,
                        &mut any_expanded,
                    );
                    local_clean_cursor = current_clean_range.end;
                }
                if expand_link {
                    if let Some(middle_marker) = link.middle_marker() {
                        let middle_len = middle_marker.len();
                        projected_fragments.push(InlineFragment {
                            text: middle_marker.to_string(),
                            style: InlineStyle::default(),
                            html_style: None,
                            link: None,
                            footnote: None,
                            math: None,
                        });
                        segments.push(InlineExpansionSegment {
                            display_range: display_cursor..display_cursor + middle_len,
                            clean_range: run_clean_end..run_clean_end,
                            fragment_index: run_start,
                            link_group,
                            kind: ExpandedInlineSegmentKind::MiddleDelimiter(
                                InlineExpansionKind::Link,
                            ),
                        });
                        for _ in 0..middle_len {
                            display_to_clean.push(run_clean_end);
                        }
                        display_cursor += middle_len;
                    }

                    let target_display_start = display_cursor;
                    if let Some(link_target) = link.editable_text() {
                        let target_len = link_target.len();
                        if target_len > 0 {
                            projected_fragments.push(InlineFragment {
                                text: link_target,
                                style: InlineStyle::default(),
                                html_style: None,
                                link: Some(link.clone()),
                                footnote: None,
                                math: None,
                            });
                            segments.push(InlineExpansionSegment {
                                display_range: display_cursor..display_cursor + target_len,
                                clean_range: run_clean_end..run_clean_end,
                                fragment_index: run_start,
                                link_group,
                                kind: ExpandedInlineSegmentKind::LinkTargetText,
                            });
                            for _ in 0..target_len {
                                display_to_clean.push(run_clean_end);
                            }
                            display_cursor += target_len;
                        }
                    }
                    let target_display_end = display_cursor;

                    let close_marker = link.close_marker().to_string();
                    let close_len = close_marker.len();
                    projected_fragments.push(InlineFragment {
                        text: close_marker,
                        style: InlineStyle::default(),
                        html_style: None,
                        link: None,
                        footnote: None,
                        math: None,
                    });
                    segments.push(InlineExpansionSegment {
                        display_range: display_cursor..display_cursor + close_len,
                        clean_range: run_clean_end..run_clean_end,
                        fragment_index: run_start,
                        link_group,
                        kind: ExpandedInlineSegmentKind::ClosingDelimiter(
                            InlineExpansionKind::Link,
                        ),
                    });
                    for _ in 0..close_len {
                        display_to_clean.push(run_clean_end);
                    }
                    display_cursor += close_len;

                    link_runs.push(LinkExpansionRun {
                        link: link.clone(),
                        start_fragment_index: run_start,
                        end_fragment_index: run_end,
                        clean_range: run_clean_range.clone(),
                        display_range: run_display_start..display_cursor,
                        target_display_range: target_display_start..target_display_end,
                    });
                }

                clean_cursor = run_clean_end;
                fragment_index = run_end;
                continue;
            }

            let clean_range = clean_cursor..clean_cursor + fragment_len;
            let expanded_kinds = Self::expanded_kinds_for_fragment(
                fragments,
                fragment_index,
                fragment.style,
                clean_range.clone(),
                &clean_selected,
                clean_marked.as_ref(),
            );

            push_projected_fragment(
                fragment,
                fragment_index,
                clean_range.clone(),
                &expanded_kinds,
                None,
                false,
                &mut projected_fragments,
                &mut segments,
                &mut clean_to_display_cursor,
                &mut display_to_clean,
                &mut display_cursor,
                &mut any_expanded,
            );

            clean_cursor = clean_range.end;
            fragment_index += 1;
        }

        if any_expanded {
            for segment in &segments {
                match segment.kind {
                    ExpandedInlineSegmentKind::OpeningDelimiter(
                        InlineExpansionKind::BoldMarkdown,
                    )
                    | ExpandedInlineSegmentKind::OpeningDelimiter(
                        InlineExpansionKind::ItalicMarkdown,
                    )
                    | ExpandedInlineSegmentKind::OpeningDelimiter(InlineExpansionKind::Code)
                    | ExpandedInlineSegmentKind::OpeningDelimiter(
                        InlineExpansionKind::Strikethrough,
                    )
                    | ExpandedInlineSegmentKind::OpeningDelimiter(
                        InlineExpansionKind::SuperscriptMarkdown,
                    )
                    | ExpandedInlineSegmentKind::OpeningDelimiter(
                        InlineExpansionKind::SubscriptMarkdown,
                    ) => {
                        clean_to_display_cursor[segment.clean_range.start] =
                            segment.display_range.end;
                    }
                    ExpandedInlineSegmentKind::ClosingDelimiter(
                        InlineExpansionKind::BoldMarkdown,
                    )
                    | ExpandedInlineSegmentKind::ClosingDelimiter(
                        InlineExpansionKind::ItalicMarkdown,
                    )
                    | ExpandedInlineSegmentKind::ClosingDelimiter(InlineExpansionKind::Code)
                    | ExpandedInlineSegmentKind::ClosingDelimiter(
                        InlineExpansionKind::Strikethrough,
                    )
                    | ExpandedInlineSegmentKind::ClosingDelimiter(
                        InlineExpansionKind::SuperscriptMarkdown,
                    )
                    | ExpandedInlineSegmentKind::ClosingDelimiter(
                        InlineExpansionKind::SubscriptMarkdown,
                    ) => {
                        clean_to_display_cursor[segment.clean_range.start] =
                            segment.display_range.start;
                    }
                    _ => {}
                }
            }
        }

        any_expanded.then(|| Self {
            cache: InlineTextTree::from_fragments(projected_fragments).render_cache(),
            segments,
            clean_to_display_cursor,
            display_to_clean,
            link_runs,
            footnote_runs,
        })
    }

    #[allow(dead_code)]
    /// 取指针目标偏移。
    pub(super) fn pointer_target_offset(&self, offset: usize) -> usize {
        for segment in &self.segments {
            match segment.kind {
                ExpandedInlineSegmentKind::OpeningDelimiter(_)
                    if offset >= segment.display_range.start
                        && offset <= segment.display_range.end =>
                {
                    return segment.display_range.end;
                }
                ExpandedInlineSegmentKind::ClosingDelimiter(_)
                    if offset >= segment.display_range.start
                        && offset <= segment.display_range.end =>
                {
                    return segment.display_range.start;
                }
                ExpandedInlineSegmentKind::MiddleDelimiter(InlineExpansionKind::Link)
                    if offset >= segment.display_range.start
                        && offset <= segment.display_range.end =>
                {
                    if let Some(link_group) = segment.link_group
                        && let Some(run) = self.link_runs.get(link_group)
                    {
                        return run.target_display_range.start;
                    }
                    return segment.display_range.end;
                }
                _ => {}
            }
        }
        offset
    }

    /// 取展示偏移的折叠光标亲和。
    pub(super) fn collapsed_affinity_for_display_offset(&self, offset: usize) -> CaretAffinity {
        for segment in &self.segments {
            match segment.kind {
                ExpandedInlineSegmentKind::OpeningDelimiter(_)
                    if offset == segment.display_range.start =>
                {
                    return CaretAffinity::OuterStart;
                }
                ExpandedInlineSegmentKind::ClosingDelimiter(_)
                    if offset == segment.display_range.end =>
                {
                    return CaretAffinity::OuterEnd;
                }
                _ => {}
            }
        }
        CaretAffinity::Default
    }

    /// 判断 `clean` 是否落在投影结束分隔符起点（某样式区间的右边界），用于把刚键入结束标记的光标放到新结束符之后。
    pub(super) fn caret_closes_span_at_clean(&self, clean: usize) -> bool {
        self.segments.iter().any(|segment| {
            matches!(segment.kind, ExpandedInlineSegmentKind::ClosingDelimiter(_))
                && segment.clean_range.start == clean
        })
    }

    /// 取纯偏移对应的展示光标偏移。
    pub(super) fn display_offset_for_clean_cursor(
        &self,
        clean: usize,
        affinity: CaretAffinity,
    ) -> Option<usize> {
        match affinity {
            CaretAffinity::Default => self
                .clean_to_display_cursor
                .get(clean.min(self.clean_to_display_cursor.len().saturating_sub(1)))
                .copied(),
            CaretAffinity::OuterStart => self
                .segments
                .iter()
                .find_map(|segment| match segment.kind {
                    ExpandedInlineSegmentKind::OpeningDelimiter(_)
                        if segment.clean_range.start == clean =>
                    {
                        Some(segment.display_range.start)
                    }
                    _ => None,
                })
                .or_else(|| {
                    self.clean_to_display_cursor
                        .get(clean.min(self.clean_to_display_cursor.len().saturating_sub(1)))
                        .copied()
                }),
            CaretAffinity::OuterEnd => self
                .segments
                .iter()
                .find_map(|segment| match segment.kind {
                    ExpandedInlineSegmentKind::ClosingDelimiter(_)
                        if segment.clean_range.start == clean =>
                    {
                        Some(segment.display_range.end)
                    }
                    _ => None,
                })
                .or_else(|| {
                    self.clean_to_display_cursor
                        .get(clean.min(self.clean_to_display_cursor.len().saturating_sub(1)))
                        .copied()
                }),
        }
    }

    /// 取左移目标。
    pub(super) fn move_left_target(&self, offset: usize) -> Option<(usize, CaretAffinity)> {
        for segment in &self.segments {
            match segment.kind {
                ExpandedInlineSegmentKind::OpeningDelimiter(_)
                    if offset == segment.display_range.end =>
                {
                    return Some((segment.display_range.start, CaretAffinity::OuterStart));
                }
                ExpandedInlineSegmentKind::ClosingDelimiter(_)
                    if offset == segment.display_range.end =>
                {
                    return Some((segment.display_range.start, CaretAffinity::Default));
                }
                _ => {}
            }
        }
        None
    }

    /// 取右移目标。
    pub(super) fn move_right_target(&self, offset: usize) -> Option<(usize, CaretAffinity)> {
        for segment in &self.segments {
            match segment.kind {
                ExpandedInlineSegmentKind::OpeningDelimiter(_)
                    if offset == segment.display_range.start =>
                {
                    return Some((segment.display_range.end, CaretAffinity::Default));
                }
                ExpandedInlineSegmentKind::ClosingDelimiter(_)
                    if offset == segment.display_range.start =>
                {
                    return Some((segment.display_range.end, CaretAffinity::OuterEnd));
                }
                _ => {}
            }
        }
        None
    }

    /// 取片段应展开的种类。
    fn expanded_kinds_for_fragment(
        fragments: &[InlineFragment],
        fragment_index: usize,
        style: InlineStyle,
        fragment_range: Range<usize>,
        clean_selected: &Range<usize>,
        clean_marked: Option<&Range<usize>>,
    ) -> Vec<InlineExpansionKind> {
        let mut kinds = Vec::new();
        let script_kind = Self::script_projection_kind(fragments, fragment_index);
        for kind in [
            Some(InlineExpansionKind::BoldMarkdown),
            Some(InlineExpansionKind::ItalicMarkdown),
            Some(InlineExpansionKind::Strikethrough),
            script_kind,
            Some(InlineExpansionKind::Code),
        ]
        .into_iter()
        .flatten()
        {
            if kind.applies_to(style)
                && Self::fragment_is_touched(fragment_range.clone(), clean_selected, clean_marked)
            {
                kinds.push(kind);
            }
        }
        kinds.sort_by_key(|kind| kind.projection_rank());
        kinds
    }

    /// 取上下标投影种类。
    fn script_projection_kind(
        fragments: &[InlineFragment],
        fragment_index: usize,
    ) -> Option<InlineExpansionKind> {
        let fragment = fragments.get(fragment_index)?;
        match fragment.style.script {
            InlineScript::Normal => None,
            InlineScript::Superscript => {
                // Prefer compact Markdown markers only when serialization can
                // round-trip them safely; standalone script spans use HTML.
                if can_use_markdown_script_delimiters(
                    fragment_index
                        .checked_sub(1)
                        .and_then(|index| fragments.get(index)),
                    fragment,
                ) {
                    Some(InlineExpansionKind::SuperscriptMarkdown)
                } else {
                    Some(InlineExpansionKind::SuperscriptHtml)
                }
            }
            InlineScript::Subscript => {
                // A strikethrough subscript would serialize ambiguously around
                // `~`, so it also uses the HTML marker projection.
                if !fragment.style.strikethrough
                    && can_use_markdown_script_delimiters(
                        fragment_index
                            .checked_sub(1)
                            .and_then(|index| fragments.get(index)),
                        fragment,
                    )
                {
                    Some(InlineExpansionKind::SubscriptMarkdown)
                } else {
                    Some(InlineExpansionKind::SubscriptHtml)
                }
            }
        }
    }

    /// 判断片段是否被触及。
    fn fragment_is_touched(
        fragment_range: Range<usize>,
        clean_selected: &Range<usize>,
        clean_marked: Option<&Range<usize>>,
    ) -> bool {
        if let Some(marked_range) = clean_marked
            && !marked_range.is_empty()
            && Self::ranges_overlap(&fragment_range, marked_range)
        {
            return true;
        }

        if !clean_selected.is_empty() {
            return Self::ranges_overlap(&fragment_range, clean_selected);
        }

        let cursor = clean_selected.start;
        fragment_range.start <= cursor && cursor <= fragment_range.end
    }

    /// 判断两范围是否重叠。
    fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
        left.start < right.end && right.start < left.end
    }

    /// 取完全覆盖范围的链接游程。
    pub(super) fn link_run_fully_covering_range(
        &self,
        range: &Range<usize>,
    ) -> Option<&LinkExpansionRun> {
        self.link_runs.iter().find(|run| {
            run.display_range.start <= range.start && range.end <= run.display_range.end
        })
    }

    /// 按纯范围取链接游程。
    pub(super) fn link_run_for_clean_range(
        &self,
        clean_range: &Range<usize>,
    ) -> Option<&LinkExpansionRun> {
        self.link_runs
            .iter()
            .find(|run| run.clean_range == *clean_range)
    }

    /// 取完全覆盖范围的脚注游程。
    pub(super) fn footnote_run_fully_covering_range(
        &self,
        range: &Range<usize>,
    ) -> Option<&FootnoteExpansionRun> {
        self.footnote_runs.iter().find(|run| {
            run.display_range.start <= range.start && range.end <= run.display_range.end
        })
    }
}

/// 取投影标记样式。
fn marker_style_for_projection(mut style: InlineStyle, kind: InlineExpansionKind) -> InlineStyle {
    if matches!(
        kind,
        InlineExpansionKind::SuperscriptMarkdown
            | InlineExpansionKind::SuperscriptHtml
            | InlineExpansionKind::SubscriptMarkdown
            | InlineExpansionKind::SubscriptHtml
    ) {
        style.script = InlineScript::Normal;
    }
    style
}

/// 输出单个行内片段并包裹其投影强调分隔符，供独立与链接标签片段共用，使锚文本如普通文本般显现加粗/斜体/代码标记。`force_styled` 让无标记片段保持样式（展开链接游程中的链接标签）。
#[allow(clippy::too_many_arguments)]
fn push_projected_fragment(
    fragment: &InlineFragment,
    fragment_index: usize,
    clean_range: Range<usize>,
    kinds: &[InlineExpansionKind],
    link_group: Option<usize>,
    force_styled: bool,
    projected_fragments: &mut Vec<InlineFragment>,
    segments: &mut Vec<InlineExpansionSegment>,
    clean_to_display_cursor: &mut [usize],
    display_to_clean: &mut Vec<usize>,
    display_cursor: &mut usize,
    any_expanded: &mut bool,
) {
    let fragment_len = fragment.text.len();

    for kind in kinds {
        *any_expanded = true;
        let marker = kind.open_marker().to_string();
        let marker_len = marker.len();
        let marker_style = marker_style_for_projection(fragment.style, *kind);
        projected_fragments.push(InlineFragment {
            text: marker,
            style: marker_style,
            html_style: fragment.html_style,
            link: None,
            footnote: None,
            math: None,
        });
        segments.push(InlineExpansionSegment {
            display_range: *display_cursor..*display_cursor + marker_len,
            clean_range: clean_range.start..clean_range.start,
            fragment_index,
            link_group,
            kind: ExpandedInlineSegmentKind::OpeningDelimiter(*kind),
        });
        for _ in 0..marker_len {
            display_to_clean.push(clean_range.start);
        }
        *display_cursor += marker_len;
    }

    let text_segment_kind = if kinds.is_empty() && !force_styled {
        ExpandedInlineSegmentKind::PlainText
    } else {
        ExpandedInlineSegmentKind::StyledText
    };
    projected_fragments.push(fragment.clone());
    segments.push(InlineExpansionSegment {
        display_range: *display_cursor..*display_cursor + fragment_len,
        clean_range: clean_range.clone(),
        fragment_index,
        link_group,
        kind: text_segment_kind,
    });
    for offset in 0..=fragment_len {
        clean_to_display_cursor[clean_range.start + offset] = *display_cursor + offset;
    }
    for offset in 1..=fragment_len {
        display_to_clean.push(clean_range.start + offset);
    }
    *display_cursor += fragment_len;

    for kind in kinds.iter().rev() {
        let marker = kind.close_marker().to_string();
        let marker_len = marker.len();
        let marker_style = marker_style_for_projection(fragment.style, *kind);
        projected_fragments.push(InlineFragment {
            text: marker,
            style: marker_style,
            html_style: fragment.html_style,
            link: None,
            footnote: None,
            math: None,
        });
        segments.push(InlineExpansionSegment {
            display_range: *display_cursor..*display_cursor + marker_len,
            clean_range: clean_range.end..clean_range.end,
            fragment_index,
            link_group,
            kind: ExpandedInlineSegmentKind::ClosingDelimiter(*kind),
        });
        for _ in 0..marker_len {
            display_to_clean.push(clean_range.end);
        }
        *display_cursor += marker_len;
    }
}
