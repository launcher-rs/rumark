//! 块标题与表格单元格用的基于属性的行内 Markdown 树。
//!
//! 运行时模型仅存文本片段与格式属性。Markdown 标记在输入输出边界解析，保存时重新生成，使编辑操作聚焦文本范围而非原始分隔符串。

use std::ops::Range;

use super::footnote::{
    InlineFootnoteHit, InlineFootnoteReference, parse_inline_footnote_reference,
    superscript_ordinal,
};
use super::html::{
    HtmlAttr, HtmlInlineStyle, HtmlNode, HtmlNodeKind, has_dangerous_attrs, is_inline_tag,
    parse_html_attrs, style_for_node,
};
use super::link::{LinkReferenceDefinition, LinkReferenceDefinitions, parse_link_target};

/// 一段文本生效的行内格式标志位。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub script: InlineScript,
}

/// 简单 Markdown 扩展语法的垂直上下标样式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InlineScript {
    #[default]
    Normal,
    Superscript,
    Subscript,
}

impl InlineStyle {
    /// 取加粗样式。
    pub fn with_bold(self) -> Self {
        Self { bold: true, ..self }
    }

    /// 取斜体样式。
    pub fn with_italic(self) -> Self {
        Self {
            italic: true,
            ..self
        }
    }

    /// 取下划线样式。
    pub fn with_underline(self) -> Self {
        Self {
            underline: true,
            ..self
        }
    }

    /// 取删除线样式。
    pub fn with_strikethrough(self) -> Self {
        Self {
            strikethrough: true,
            ..self
        }
    }

    /// 取行内代码样式。
    pub fn with_code(self) -> Self {
        Self { code: true, ..self }
    }

    /// 取上标样式。
    pub fn with_superscript(self) -> Self {
        Self {
            script: InlineScript::Superscript,
            ..self
        }
    }

    /// 取下标样式。
    pub fn with_subscript(self) -> Self {
        Self {
            script: InlineScript::Subscript,
            ..self
        }
    }

    /// 是否含上下标。
    pub fn has_script(self) -> bool {
        self.script != InlineScript::Normal
    }

    /// 应用分隔符样式。
    fn apply(self, delimiter: EmphasisDelimiter) -> Self {
        match delimiter {
            EmphasisDelimiter::BoldMarkdown { .. } | EmphasisDelimiter::BoldHtml => {
                self.with_bold()
            }
            EmphasisDelimiter::ItalicMarkdown { .. } | EmphasisDelimiter::ItalicHtml => {
                self.with_italic()
            }
            EmphasisDelimiter::Underline => self.with_underline(),
            EmphasisDelimiter::StrikethroughMarkdown => self.with_strikethrough(),
            EmphasisDelimiter::CodeMarkdown { .. } => self.with_code(),
            EmphasisDelimiter::SuperscriptMarkdown | EmphasisDelimiter::SuperscriptHtml => {
                self.with_superscript()
            }
            EmphasisDelimiter::SubscriptMarkdown | EmphasisDelimiter::SubscriptHtml => {
                self.with_subscript()
            }
        }
    }
}

/// 具有统一 [`InlineStyle`] 的连续文本游程。
///
/// [`InlineTextTree`] 即 `Vec<InlineFragment>`，归一化时合并相邻同样式片段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineFragment {
    pub text: String,
    pub style: InlineStyle,
    pub html_style: Option<HtmlInlineStyle>,
    pub link: Option<InlineLink>,
    pub footnote: Option<InlineFootnoteReference>,
    pub math: Option<InlineMath>,
}

/// 追加片段：与末尾片段的渲染属性完全一致时并入其中，否则另起一段。
///
/// 公式片段各自带源码与定界符，永不参与合并。
fn push_merged_fragment(fragments: &mut Vec<InlineFragment>, fragment: InlineFragment) {
    if let Some(last) = fragments.last_mut()
        && last.style == fragment.style
        && last.html_style == fragment.html_style
        && last.link == fragment.link
        && last.footnote == fragment.footnote
        && last.math.is_none()
        && fragment.math.is_none()
    {
        last.text.push_str(&fragment.text);
        return;
    }

    fragments.push(fragment);
}

/// 源码保留的行内 LaTeX 公式元数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineMath {
    /// 完整 Markdown 源码，含 `$...$` 或 `\(...\)` 分隔符。
    pub source: String,
    /// 行内公式分隔符之间的 LaTeX 正文。
    pub body: String,
    /// 源码使用的分隔符形式。
    pub delimiter: InlineMathDelimiter,
}

/// 支持的行内公式分隔符语法。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineMathDelimiter {
    /// 美元分隔的行内公式：`$...$`。
    Dollar,
    /// 括号分隔的行内公式：`\(...\)`。
    Paren,
}

/// 附在格式化行内文本片段上的链接元数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineLink {
    /// 来自 `[label](destination "title")` 的行内目标与可选标题。
    Inline {
        destination: String,
        title: Option<String>,
    },
    /// 引用式链接，由 `[label][ref]` 语法解析。
    Reference { label: String, destination: String },
    /// 自动链接目标，来自 `<scheme:target>` 或类邮箱语法。
    Autolink { target: String },
}

/// 命中测试与打开链接提示用的链接目标对。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineLinkHit {
    pub prompt_target: String,
    pub open_target: String,
}

impl InlineLink {
    /// 取打开目标。
    pub fn open_target(&self) -> &str {
        match self {
            Self::Inline { destination, .. } | Self::Reference { destination, .. } => destination,
            Self::Autolink { target } => target,
        }
    }

    /// 取原始目标。
    pub fn raw_target(&self) -> &str {
        match self {
            Self::Inline { destination, .. } => destination,
            Self::Reference { label, .. } => label,
            Self::Autolink { target } => target,
        }
    }

    /// 取链接命中信息。
    pub(crate) fn hit(&self) -> InlineLinkHit {
        InlineLinkHit {
            prompt_target: self.raw_target().to_string(),
            open_target: self.open_target().to_string(),
        }
    }

    /// 判断是否为保留源码的链接。
    pub(crate) fn is_source_preserving(&self) -> bool {
        matches!(self, Self::Reference { .. } | Self::Autolink { .. })
    }

    /// 取链接起始标记。
    pub(crate) fn open_marker(&self) -> &'static str {
        match self {
            Self::Autolink { .. } => "<",
            Self::Inline { .. } | Self::Reference { .. } => "[",
        }
    }

    /// 取链接中间标记。
    pub(crate) fn middle_marker(&self) -> Option<&'static str> {
        match self {
            Self::Inline { .. } => Some("]("),
            Self::Reference { .. } => Some("]["),
            Self::Autolink { .. } => None,
        }
    }

    /// 取链接可编辑文本。
    pub(crate) fn editable_text(&self) -> Option<String> {
        match self {
            Self::Inline { destination, title } => {
                Some(format_inline_link_target(destination, title.as_deref()))
            }
            Self::Reference { label, .. } => Some(label.clone()),
            Self::Autolink { .. } => None,
        }
    }

    /// 取链接结束标记。
    pub(crate) fn close_marker(&self) -> &'static str {
        match self {
            Self::Inline { .. } => ")",
            Self::Reference { .. } => "]",
            Self::Autolink { .. } => ">",
        }
    }
}

/// 格式化行内链接目标。
fn format_inline_link_target(destination: &str, title: Option<&str>) -> String {
    match title {
        Some(title) => format!("{destination} \"{}\"", escape_link_title(title)),
        None => destination.to_string(),
    }
}

/// 转义链接标题。
fn escape_link_title(title: &str) -> String {
    let mut escaped = String::with_capacity(title.len());
    for ch in title.chars() {
        if matches!(ch, '\\' | '"') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// 带 [`InlineStyle`] 的可见文本范围，供渲染缓存为文本系统构建样式跑。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineSpan {
    pub range: Range<usize>,
    pub style: InlineStyle,
    pub html_style: Option<HtmlInlineStyle>,
    pub link: Option<InlineLinkHit>,
    pub footnote: Option<InlineFootnoteHit>,
    pub math: Option<InlineMath>,
}

/// 光标处插入文本继承的片段属性。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineInsertionAttributes {
    pub style: InlineStyle,
    pub html_style: Option<HtmlInlineStyle>,
    pub link: Option<InlineLink>,
    pub footnote: Option<InlineFootnoteReference>,
    pub math: Option<InlineMath>,
}

/// 为渲染优化的 [`InlineTextTree`] 预计算视图。
///
/// 把片段树拍平为可见文本串与 [`InlineSpan`] 列表，供文本系统按样式跑构建布局。
#[derive(Clone, Debug, Default)]
pub struct InlineRenderCache {
    plain_text: String,
    spans: Vec<InlineSpan>,
}

/// 源码 Markdown 与可见行内文本的双向偏移映射。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkdownOffsetMap {
    markdown: String,
    visible_to_markdown: Vec<usize>,
    markdown_to_visible: Vec<usize>,
}

impl MarkdownOffsetMap {
    /// 取源码字符串。
    pub(crate) fn markdown(&self) -> &str {
        &self.markdown
    }

    /// 可见偏移转源码偏移。
    pub(crate) fn visible_to_markdown_offset(&self, offset: usize) -> usize {
        self.visible_to_markdown
            .get(offset.min(self.visible_to_markdown.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    }

    /// 可见范围转源码范围。
    pub(crate) fn visible_to_markdown_range(&self, range: Range<usize>) -> Range<usize> {
        self.visible_to_markdown_offset(range.start)..self.visible_to_markdown_offset(range.end)
    }

    /// 源码偏移转可见偏移。
    pub(crate) fn markdown_to_visible_offset(&self, offset: usize) -> usize {
        self.markdown_to_visible
            .get(offset.min(self.markdown_to_visible.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    }

    /// 源码范围转可见范围。
    pub(crate) fn markdown_to_visible_range(&self, range: Range<usize>) -> Range<usize> {
        self.markdown_to_visible_offset(range.start)..self.markdown_to_visible_offset(range.end)
    }
}

impl InlineRenderCache {
    /// 由片段树构建渲染缓存。
    pub fn from_tree(tree: &InlineTextTree) -> Self {
        let mut plain_text = String::new();
        let mut spans = Vec::new();
        let mut visible_offset = 0;

        for fragment in tree.fragments.iter() {
            let fragment_start = visible_offset;
            plain_text.push_str(&fragment.text);
            let fragment_len = fragment.text.len();
            if fragment_len > 0 {
                spans.push(InlineSpan {
                    range: fragment_start..fragment_start + fragment_len,
                    style: fragment.style,
                    html_style: fragment.html_style,
                    link: fragment.link.as_ref().map(InlineLink::hit),
                    footnote: fragment
                        .footnote
                        .as_ref()
                        .and_then(InlineFootnoteReference::hit),
                    math: fragment.math.clone(),
                });
            }

            visible_offset += fragment_len;
        }

        Self { plain_text, spans }
    }

    /// 取可见文本。
    pub fn plain_text(&self) -> &str {
        &self.plain_text
    }

    /// 取行内样式片段。
    pub fn spans(&self) -> &[InlineSpan] {
        &self.spans
    }

    /// 取可见长度。
    pub fn plain_len(&self) -> usize {
        self.plain_text.len()
    }

    /// 取指定偏移的样式。
    pub fn style_at(&self, offset: usize) -> InlineStyle {
        self.spans
            .iter()
            .find(|span| span.range.start <= offset && offset < span.range.end)
            .map(|span| span.style)
            .unwrap_or_default()
    }

    #[allow(dead_code)]
    /// 取指定偏移的超文本样式。
    pub fn html_style_at(&self, offset: usize) -> Option<HtmlInlineStyle> {
        self.spans
            .iter()
            .find(|span| span.range.start <= offset && offset < span.range.end)
            .and_then(|span| span.html_style)
    }

    #[allow(dead_code)]
    /// 取指定偏移的链接目标。
    pub fn link_at(&self, offset: usize) -> Option<&str> {
        self.link_hit_at(offset).map(|hit| hit.open_target.as_str())
    }

    /// 取指定偏移的链接命中。
    pub fn link_hit_at(&self, offset: usize) -> Option<&InlineLinkHit> {
        self.spans
            .iter()
            .find(|span| span.range.start <= offset && offset < span.range.end)
            .and_then(|span| span.link.as_ref())
    }

    #[allow(dead_code)]
    /// 取指定偏移的脚注命中。
    pub fn footnote_hit(&self, offset: usize) -> Option<&InlineFootnoteHit> {
        self.spans
            .iter()
            .find(|span| span.range.start <= offset && offset < span.range.end)
            .and_then(|span| span.footnote.as_ref())
    }

    #[allow(dead_code)]
    /// 取指定偏移的行内公式。
    pub fn inline_math_at(&self, offset: usize) -> Option<&InlineMath> {
        self.spans
            .iter()
            .find(|span| span.range.start <= offset && offset < span.range.end)
            .and_then(|span| span.math.as_ref())
    }
}

/// 表示行内格式文本的 [`InlineFragment`] 序列。
///
/// 这是块标题的核心数据结构，支持：
/// - 由原始 Markdown 构建（自动解析加粗/斜体/下划线标记）
/// - 带最优分隔符选择的双向 Markdown 序列化
/// - 按任意字节偏移切分（用于回车、粘贴）
/// - 在任意范围切换行内样式
///
/// 序列化使用类维特比动态规划，在 Markdown 与 HTML 分隔符变体之间选择，
/// 避免产生有歧义的 `****` 连写。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineTextTree {
    pub(crate) fragments: Vec<InlineFragment>,
}

impl InlineTextTree {
    /// 构造纯文本树。
    pub fn plain(text: impl Into<String>) -> Self {
        Self::from_fragments(vec![InlineFragment {
            text: text.into(),
            style: InlineStyle::default(),
            html_style: None,
            link: None,
            footnote: None,
            math: None,
        }])
    }

    /// 把带标记的 Markdown 解析为内部片段表示。
    ///
    /// 标记（`**`、`*`、`<u>`、`<strong>`、`<em>`）会被消费并转为
    /// 相邻片段上的 [`InlineStyle`] 标志，标记本身不存储，树中仅保留文本
    /// 内容与样式属性。
    pub fn from_markdown(markdown: &str) -> Self {
        Self::from_markdown_with_link_references(markdown, &LinkReferenceDefinitions::default())
    }

    /// 按引用定义解析 Markdown。
    pub fn from_markdown_with_link_references(
        markdown: &str,
        reference_definitions: &LinkReferenceDefinitions,
    ) -> Self {
        let mut tree = Self::plain(markdown)
            .normalize_inline_syntax_with_link_references(reference_definitions)
            .tree;
        tree.normalize_code_spans();
        tree.normalize_soft_breaks();
        tree
    }

    /// 规范化代码段内容（对齐 CommonMark / GitHub）：
    /// - 换行统一转为单个空格，使行内代码始终渲染为一行。
    /// - 非全空且首尾各有一个空格时，去掉首尾空格。
    fn normalize_code_spans(&mut self) {
        for fragment in &mut self.fragments {
            if fragment.style.code && !fragment.text.is_empty() {
                let mut s = fragment
                    .text
                    .replace("\r\n", " ")
                    .replace(['\r', '\n'], " ");
                let all_space = s.chars().all(|c| c == ' ');
                if !all_space && s.starts_with(' ') && s.ends_with(' ') {
                    s.remove(0);
                    s.pop();
                }
                fragment.text = s;
            }
        }
        self.normalize_fragments();
    }

    /// 规范化段落内软换行（对齐 CommonMark / GitHub README）：
    /// 段内裸换行折叠为空格，仅硬换行（行尾两空格或反斜杠，序列化为 `"  \n"`）
    /// 与空行（段落分隔）保留换行。
    fn normalize_soft_breaks(&mut self) {
        let text = self.plain_text();
        if !text.contains(['\n', '\r']) {
            return;
        }
        let bytes = text.as_bytes();
        let len = bytes.len();
        let mut collapse = vec![false; len];
        let mut line_start = 0usize;
        let mut index = 0usize;
        while index < len {
            if bytes[index] != b'\n' && bytes[index] != b'\r' {
                index += 1;
                continue;
            }
            let prev_blank = text[line_start..index].trim().is_empty();
            let next_start = index + 1;
            let next_end = text[next_start..]
                .find(['\n', '\r'])
                .map(|offset| next_start + offset)
                .unwrap_or(len);
            let next_blank = text[next_start..next_end].trim().is_empty();
            let hard =
                index >= line_start + 2 && bytes[index - 1] == b' ' && bytes[index - 2] == b' ';
            if !prev_blank && !next_blank && !hard {
                collapse[index] = true;
            }
            line_start = next_start;
            index = next_start;
        }

        let mut base = 0usize;
        for fragment in &mut self.fragments {
            let fragment_len = fragment.text.len();
            if !fragment.style.code && fragment.text.contains(['\n', '\r']) {
                let mut out = String::with_capacity(fragment_len);
                for (offset, ch) in fragment.text.char_indices() {
                    out.push(if collapse[base + offset] { ' ' } else { ch });
                }
                fragment.text = out;
            }
            base += fragment_len;
        }
        self.normalize_fragments();
    }

    /// 由片段构造并归一化。
    pub fn from_fragments(fragments: Vec<InlineFragment>) -> Self {
        let mut tree = Self { fragments };
        tree.normalize_fragments();
        tree
    }

    /// 取树可见文本。
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        for fragment in &self.fragments {
            text.push_str(&fragment.text);
        }
        text
    }

    /// 取树可见长度。
    pub fn plain_len(&self) -> usize {
        self.fragments
            .iter()
            .map(|fragment| fragment.text.len())
            .sum()
    }

    /// 是否含保留源码的链接。
    pub(crate) fn has_source_preserving_links(&self) -> bool {
        self.fragments.iter().any(|fragment| {
            fragment
                .link
                .as_ref()
                .is_some_and(InlineLink::is_source_preserving)
                || fragment.footnote.is_some()
                || fragment.math.is_some()
        })
    }

    /// 是否有片段携带行内 `[label](url)` 链接。与引用/自动链接不同，
    /// 这类链接不算“保留源码”，但其 `[...](...)` 标记已从片段文本剥离，
    /// 若仅由可见文本重建树会丢失它们。
    pub(crate) fn has_inline_links(&self) -> bool {
        self.fragments
            .iter()
            .any(|fragment| matches!(fragment.link, Some(InlineLink::Inline { .. })))
    }

    /// 是否含混合行内视觉。
    pub(crate) fn has_mixed_inline_visuals(&self) -> bool {
        self.fragments
            .iter()
            .any(|fragment| fragment.math.is_some() || fragment.style.has_script())
    }

    /// 是否含脚注引用。
    pub(crate) fn has_footnote_references(&self) -> bool {
        self.fragments
            .iter()
            .any(|fragment| fragment.footnote.is_some())
    }

    /// 应用脚注引用状态。
    pub(crate) fn apply_footnote_reference_state(
        &mut self,
        mut resolve: impl FnMut(&str) -> Option<(usize, usize)>,
    ) {
        for fragment in &mut self.fragments {
            let Some(footnote) = fragment.footnote.as_mut() else {
                continue;
            };
            if let Some((ordinal, occurrence_index)) = resolve(&footnote.id) {
                footnote.ordinal = Some(ordinal);
                footnote.occurrence_index = occurrence_index;
                fragment.text = superscript_ordinal(ordinal);
            } else {
                footnote.ordinal = None;
                footnote.occurrence_index = 0;
                fragment.text = footnote.raw_markdown();
            }
        }
        self.normalize_fragments();
    }

    /// 构建渲染缓存。
    pub fn render_cache(&self) -> InlineRenderCache {
        InlineRenderCache::from_tree(self)
    }

    /// 以最优分隔符选择把片段序列化回 Markdown 文本。
    ///
    /// 每个片段的样式标志决定其文本两侧的标记。
    /// 这是输入输出边界的导出侧，内部片段表示从不存储原始标记字符。
    pub fn serialize_markdown(&self) -> String {
        self.markdown_offset_map().markdown
    }

    /// 构建 Markdown 偏移映射。
    pub(crate) fn markdown_offset_map(&self) -> MarkdownOffsetMap {
        if self.fragments.is_empty() {
            return MarkdownOffsetMap {
                markdown: String::new(),
                visible_to_markdown: vec![0],
                markdown_to_visible: vec![0],
            };
        }

        let mut output = String::new();
        let mut visible_to_markdown = vec![0; self.plain_len() + 1];
        let mut markdown_to_visible = vec![0];
        let mut visible_cursor = 0usize;
        let mut index = 0usize;
        while index < self.fragments.len() {
            if let Some(footnote) = self.fragments[index].footnote.clone() {
                let raw_markdown = footnote.raw_markdown();
                let raw_len = raw_markdown.len();
                let run_visible_len = self.fragments[index].text.len();
                let run_start = output.len();
                output.push_str(&raw_markdown);
                let run_end = output.len();

                for local_visible in 0..=run_visible_len {
                    let mapped = (raw_len * local_visible)
                        .checked_div(run_visible_len)
                        .unwrap_or(0);
                    visible_to_markdown[visible_cursor + local_visible] = run_start + mapped;
                }

                markdown_to_visible.resize(run_end + 1, visible_cursor);
                for local_markdown in 0..=raw_len {
                    let mapped = (run_visible_len * local_markdown)
                        .checked_div(raw_len)
                        .unwrap_or(0);
                    markdown_to_visible[run_start + local_markdown] = visible_cursor + mapped;
                }

                visible_cursor += run_visible_len;
                index += 1;
                continue;
            }

            if let Some(math) = self.fragments[index].math.clone() {
                let raw_markdown = math.source;
                let raw_len = raw_markdown.len();
                let run_visible_len = self.fragments[index].text.len();
                let run_start = output.len();
                output.push_str(&raw_markdown);
                let run_end = output.len();

                for local_visible in 0..=run_visible_len {
                    visible_to_markdown[visible_cursor + local_visible] =
                        run_start + local_visible.min(raw_len);
                }

                markdown_to_visible.resize(run_end + 1, visible_cursor);
                for local_markdown in 0..=raw_len {
                    markdown_to_visible[run_start + local_markdown] =
                        visible_cursor + local_markdown.min(run_visible_len);
                }

                visible_cursor += run_visible_len;
                index += 1;
                continue;
            }

            let link = self.fragments[index].link.clone();
            let mut end = index + 1;
            while end < self.fragments.len()
                && self.fragments[end].link == link
                && self.fragments[end].footnote.is_none()
                && self.fragments[end].math.is_none()
            {
                end += 1;
            }

            let run_map =
                serialize_fragment_run_markdown_with_offset_map(&self.fragments[index..end]);
            if let Some(link) = link {
                let run_visible_len = run_map.visible_to_markdown.len().saturating_sub(1);
                let link_start = output.len();
                let editable_text = link.editable_text();
                output.push_str(link.open_marker());
                output.push_str(run_map.markdown());
                if let Some(middle_marker) = link.middle_marker() {
                    output.push_str(middle_marker);
                }
                if let Some(editable_text) = editable_text.as_deref() {
                    output.push_str(editable_text);
                }
                output.push_str(link.close_marker());
                let link_end = output.len();
                let label_markdown_start = link_start + link.open_marker().len();

                for local_visible in 0..=run_visible_len {
                    visible_to_markdown[visible_cursor + local_visible] =
                        label_markdown_start + run_map.visible_to_markdown_offset(local_visible);
                }

                markdown_to_visible.resize(link_end + 1, visible_cursor);
                for local in 0..=link.open_marker().len() {
                    markdown_to_visible[link_start + local] = visible_cursor;
                }
                for local_markdown in 0..run_map.markdown().len() {
                    markdown_to_visible[label_markdown_start + local_markdown] =
                        visible_cursor + run_map.markdown_to_visible_offset(local_markdown);
                }

                let label_markdown_end = label_markdown_start + run_map.markdown().len();
                markdown_to_visible[label_markdown_end] = visible_cursor + run_visible_len;

                let suffix_start = label_markdown_end;
                let suffix_len = link.middle_marker().map(str::len).unwrap_or(0)
                    + editable_text.as_ref().map(String::len).unwrap_or(0)
                    + link.close_marker().len();
                for local in 0..=suffix_len {
                    markdown_to_visible[suffix_start + local] = visible_cursor + run_visible_len;
                }
                visible_cursor += run_visible_len;
            } else {
                let run_start = output.len();
                output.push_str(run_map.markdown());
                let run_end = output.len();

                let run_visible_len = run_map.visible_to_markdown.len().saturating_sub(1);
                for local_visible in 0..=run_visible_len {
                    visible_to_markdown[visible_cursor + local_visible] =
                        run_start + run_map.visible_to_markdown_offset(local_visible);
                }

                markdown_to_visible.resize(run_end + 1, visible_cursor);
                for local_markdown in 0..=run_map.markdown().len() {
                    markdown_to_visible[run_start + local_markdown] =
                        visible_cursor + run_map.markdown_to_visible_offset(local_markdown);
                }
                visible_cursor += run_visible_len;
            }

            index = end;
        }

        MarkdownOffsetMap {
            markdown: output,
            visible_to_markdown,
            markdown_to_visible,
        }
    }
}

/// 序列化片段并建偏移映射。
fn serialize_fragment_run_markdown_with_offset_map(
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

/// 追加 Markdown 标记。
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

/// 构造恒等偏移映射。
fn identity_text_with_offset_map(text: &str) -> MarkdownOffsetMap {
    MarkdownOffsetMap {
        markdown: text.to_string(),
        visible_to_markdown: (0..=text.len()).collect(),
        markdown_to_visible: (0..=text.len()).collect(),
    }
}

/// 生成 HTML 样式起始标记。
fn html_style_open_marker(style: HtmlInlineStyle) -> Option<String> {
    style
        .to_css()
        .map(|css| format!("<span style=\"{}\">", escape_html_attr(&css)))
}

/// 转义 HTML 属性。
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

impl InlineTextTree {
    /// 按偏移切分树。
    pub fn split_at(&self, offset: usize) -> (Self, Self) {
        let clamped = offset.min(self.plain_len());
        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut consumed = 0;

        for fragment in &self.fragments {
            let fragment_len = fragment.text.len();
            let fragment_start = consumed;
            let fragment_end = fragment_start + fragment_len;

            if clamped <= fragment_start {
                right.push(fragment.clone());
            } else if clamped >= fragment_end {
                left.push(fragment.clone());
            } else {
                let split_offset = crate::text_offsets::floor_char_boundary(
                    &fragment.text,
                    clamped - fragment_start,
                );
                if split_offset > 0 {
                    left.push(InlineFragment {
                        text: fragment.text[..split_offset].to_string(),
                        style: fragment.style,
                        html_style: fragment.html_style,
                        link: fragment.link.clone(),
                        footnote: fragment.footnote.clone(),
                        math: None,
                    });
                }
                if split_offset < fragment_len {
                    right.push(InlineFragment {
                        text: fragment.text[split_offset..].to_string(),
                        style: fragment.style,
                        html_style: fragment.html_style,
                        link: fragment.link.clone(),
                        footnote: fragment.footnote.clone(),
                        math: None,
                    });
                }
            }

            consumed = fragment_end;
        }

        (Self::from_fragments(left), Self::from_fragments(right))
    }

    /// 追加另一棵树。
    pub fn push_tree(&mut self, other: Self) {
        self.fragments.extend(other.fragments);
        self.normalize_fragments();
    }

    /// 替换片段区间。
    pub(crate) fn replace_fragment_range(
        &mut self,
        range: Range<usize>,
        replacement: Vec<InlineFragment>,
    ) {
        self.fragments.splice(range, replacement);
        self.normalize_fragments();
    }

    /// 删除可见前缀。
    pub fn strip_visible_prefix(&mut self, prefix_len: usize) {
        let (_, tail) = self.split_at(prefix_len);
        *self = tail;
    }

    /// 取插入点继承属性。
    pub fn insertion_attributes_at(&self, offset: usize) -> InlineInsertionAttributes {
        if self.fragments.is_empty() {
            return InlineInsertionAttributes::default();
        }

        let clamped = offset.min(self.plain_len());
        let mut consumed = 0;

        for (index, fragment) in self.fragments.iter().enumerate() {
            let fragment_len = fragment.text.len();
            let fragment_start = consumed;
            let fragment_end = fragment_start + fragment_len;

            if fragment_start < clamped && clamped < fragment_end {
                return InlineInsertionAttributes {
                    style: fragment.style,
                    html_style: fragment.html_style,
                    link: fragment.link.clone(),
                    footnote: fragment.footnote.clone(),
                    math: None,
                };
            }

            // 在带分隔符片段边界处输入应产生纯文本，光标在外侧时
            // 不把样式扩展到可见的结束/起始标记之外。
            if clamped == fragment_end && index + 1 == self.fragments.len() {
                return if fragment.style.code || fragment.style.strikethrough {
                    InlineInsertionAttributes::default()
                } else {
                    InlineInsertionAttributes {
                        style: fragment.style,
                        html_style: fragment.html_style,
                        link: fragment.link.clone(),
                        footnote: fragment.footnote.clone(),
                        math: None,
                    }
                };
            }

            if clamped == fragment_start && index == 0 {
                return if fragment.style.code || fragment.style.strikethrough {
                    InlineInsertionAttributes::default()
                } else {
                    InlineInsertionAttributes {
                        style: fragment.style,
                        html_style: fragment.html_style,
                        link: fragment.link.clone(),
                        footnote: fragment.footnote.clone(),
                        math: None,
                    }
                };
            }

            consumed = fragment_end;
        }

        InlineInsertionAttributes::default()
    }

    /// 切换加粗。
    pub fn toggle_bold(&mut self, range: Range<usize>) -> bool {
        self.toggle_style(range, InlineStyleFlag::Bold)
    }

    /// 切换斜体。
    pub fn toggle_italic(&mut self, range: Range<usize>) -> bool {
        self.toggle_style(range, InlineStyleFlag::Italic)
    }

    /// 切换下划线。
    pub fn toggle_underline(&mut self, range: Range<usize>) -> bool {
        self.toggle_style(range, InlineStyleFlag::Underline)
    }

    #[allow(dead_code)]
    /// 切换删除线。
    pub fn toggle_strikethrough(&mut self, range: Range<usize>) -> bool {
        self.toggle_style(range, InlineStyleFlag::Strikethrough)
    }

    /// 切换行内代码。
    pub fn toggle_code(&mut self, range: Range<usize>) -> bool {
        self.toggle_style(range, InlineStyleFlag::Code)
    }

    /// 清除片段指定样式。
    pub fn strip_styles_on_fragments(&mut self, targets: &[(usize, InlineStyleFlag)]) {
        if targets.is_empty() {
            return;
        }

        for (fragment_index, flag) in targets {
            if let Some(fragment) = self.fragments.get_mut(*fragment_index) {
                fragment.style = set_style_flag(fragment.style, *flag, false);
            }
        }
        self.normalize_fragments();
    }

    #[allow(dead_code)]
    /// 替换可见范围。
    pub fn replace_plain_range(
        &self,
        range: Range<usize>,
        new_text: &str,
        inserted_attributes: InlineInsertionAttributes,
    ) -> InlineEditOutcome {
        self.replace_plain_range_with_link_references(
            range,
            new_text,
            inserted_attributes,
            &LinkReferenceDefinitions::default(),
        )
    }

    /// 按引用定义替换可见范围。
    pub fn replace_plain_range_with_link_references(
        &self,
        range: Range<usize>,
        new_text: &str,
        inserted_attributes: InlineInsertionAttributes,
        reference_definitions: &LinkReferenceDefinitions,
    ) -> InlineEditOutcome {
        let clamped_start = range.start.min(self.plain_len());
        let clamped_end = range.end.min(self.plain_len());
        let (before, tail) = self.split_at(clamped_start);
        let (_, after) = tail.split_at(clamped_end.saturating_sub(clamped_start));

        let mut temp = before;
        if !new_text.is_empty() {
            temp.fragments.push(InlineFragment {
                text: new_text.to_string(),
                style: inserted_attributes.style,
                html_style: inserted_attributes.html_style,
                link: inserted_attributes.link,
                footnote: inserted_attributes.footnote,
                math: inserted_attributes.math,
            });
        }
        temp.push_tree(after);
        temp.normalize_fragments();
        temp.normalize_inline_syntax_with_link_references(reference_definitions)
    }

    /// 与 `replace_plain_range` 类似，但跳过标记归一化，
    /// 反引号、星号等分隔符按原样存储。
    /// 用于源码模式编辑，此时文本必须保持原样。
    pub fn replace_plain_range_raw(
        &self,
        range: Range<usize>,
        new_text: &str,
        inserted_attributes: InlineInsertionAttributes,
    ) -> InlineEditOutcome {
        let clamped_start = range.start.min(self.plain_len());
        let clamped_end = range.end.min(self.plain_len());
        let (before, tail) = self.split_at(clamped_start);
        let (_, after) = tail.split_at(clamped_end.saturating_sub(clamped_start));

        let mut temp = before;
        if !new_text.is_empty() {
            temp.fragments.push(InlineFragment {
                text: new_text.to_string(),
                style: inserted_attributes.style,
                html_style: inserted_attributes.html_style,
                link: inserted_attributes.link,
                footnote: inserted_attributes.footnote,
                math: inserted_attributes.math,
            });
        }
        temp.push_tree(after);
        temp.normalize_fragments();
        let len = temp.plain_len();
        InlineEditOutcome {
            tree: InlineTextTree::from_fragments(temp.fragments),
            visible_to_normalized: (0..=len).collect(),
        }
    }

    /// 按引用定义归一化行内语法。
    pub fn normalize_inline_syntax_with_link_references(
        &self,
        reference_definitions: &LinkReferenceDefinitions,
    ) -> InlineEditOutcome {
        let plain_text = self.plain_text();
        let tokens = flatten_tokens(&self.fragments);
        let mut builder = InlineNormalizer::new(plain_text.len());
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

    /// 切换指定样式。
    fn toggle_style(&mut self, range: Range<usize>, flag: InlineStyleFlag) -> bool {
        if range.is_empty() {
            return false;
        }

        let clamped_start = range.start.min(self.plain_len());
        let clamped_end = range.end.min(self.plain_len());
        if clamped_start >= clamped_end {
            return false;
        }

        let (before, tail) = self.split_at(clamped_start);
        let (mut middle, after) = tail.split_at(clamped_end - clamped_start);
        let should_remove = middle
            .fragments
            .iter()
            .all(|fragment| style_flag_enabled(fragment.style, flag));

        for fragment in &mut middle.fragments {
            fragment.style = set_style_flag(fragment.style, flag, !should_remove);
        }
        middle.normalize_fragments();

        let mut next = before;
        next.push_tree(middle);
        next.push_tree(after);
        *self = next;
        true
    }

    /// 归一化合并片段。
    fn normalize_fragments(&mut self) {
        let mut normalized: Vec<InlineFragment> = Vec::new();
        for fragment in self.fragments.drain(..) {
            if fragment.text.is_empty() {
                continue;
            }

            push_merged_fragment(&mut normalized, fragment);
        }
        self.fragments = normalized;
    }
}

/// 可见文本替换操作的结果，包含归一化后的树，
/// 以及编辑前可见偏移到编辑后树偏移的映射。
#[derive(Clone, Debug)]
pub struct InlineEditOutcome {
    pub tree: InlineTextTree,
    visible_to_normalized: Vec<usize>,
}

impl InlineEditOutcome {
    /// 映射偏移。
    pub fn map_offset(&self, offset: usize) -> usize {
        self.visible_to_normalized
            .get(offset.min(self.visible_to_normalized.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    }

    /// 映射范围。
    pub fn map_range(&self, range: &Range<usize>) -> Range<usize> {
        self.map_offset(range.start)..self.map_offset(range.end)
    }
}

/// 动态规划序列化器使用的分隔符变体偏好顺序。
/// 等级越低越优先，Markdown 分隔符因更短、更常用而优先于超文本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmphasisDelimiter {
    /// Markdown 加粗标记，可用 `*` 或 `_`。
    BoldMarkdown { marker: char },
    /// Markdown 斜体标记，可用 `*` 或 `_`。
    ItalicMarkdown { marker: char },
    /// Markdown 删除线标记 `~~`。
    StrikethroughMarkdown,
    /// Markdown 上标标记 `^`。
    SuperscriptMarkdown,
    /// Markdown 下标标记 `~`。
    SubscriptMarkdown,
    /// 超文本下划线标记 `<u>`。
    Underline,
    /// 超文本上标标记 `<sup>`。
    SuperscriptHtml,
    /// 超文本下标标记 `<sub>`。
    SubscriptHtml,
    /// 超文本加粗标记 `<strong>`。
    BoldHtml,
    /// 超文本斜体标记 `<em>`。
    ItalicHtml,
    /// Markdown 代码段标记，使用选定的反引号长度。
    CodeMarkdown { run_len: usize },
}

impl EmphasisDelimiter {
    /// 返回起始标记字符串。代码段为 `run_len` 个反引号，
    /// 强调为 `**`、`*`、`<u>` 等。
    fn open(self) -> String {
        match self {
            Self::BoldMarkdown { marker } => marker.to_string().repeat(2),
            Self::ItalicMarkdown { marker } => marker.to_string(),
            Self::StrikethroughMarkdown => "~~".into(),
            Self::SuperscriptMarkdown => "^".into(),
            Self::SubscriptMarkdown => "~".into(),
            Self::Underline => "<u>".into(),
            Self::SuperscriptHtml => "<sup>".into(),
            Self::SubscriptHtml => "<sub>".into(),
            Self::BoldHtml => "<strong>".into(),
            Self::ItalicHtml => "<em>".into(),
            Self::CodeMarkdown { run_len } => "`".repeat(run_len),
        }
    }

    /// 取结束标记。
    fn close(self) -> String {
        match self {
            Self::BoldMarkdown { marker } => marker.to_string().repeat(2),
            Self::ItalicMarkdown { marker } => marker.to_string(),
            Self::StrikethroughMarkdown => "~~".into(),
            Self::SuperscriptMarkdown => "^".into(),
            Self::SubscriptMarkdown => "~".into(),
            Self::Underline => "</u>".into(),
            Self::SuperscriptHtml => "</sup>".into(),
            Self::SubscriptHtml => "</sub>".into(),
            Self::BoldHtml => "</strong>".into(),
            Self::ItalicHtml => "</em>".into(),
            Self::CodeMarkdown { run_len } => "`".repeat(run_len),
        }
    }

    /// 取标记长度。
    fn token_len(self) -> usize {
        match self {
            Self::CodeMarkdown { run_len } => run_len,
            other => other.open().chars().count(),
        }
    }

    /// 取偏好等级。
    fn preference_rank(self) -> u8 {
        match self {
            Self::BoldMarkdown { .. } => 0,
            Self::Underline => 1,
            Self::StrikethroughMarkdown => 2,
            Self::SuperscriptMarkdown | Self::SubscriptMarkdown => 3,
            Self::ItalicMarkdown { .. } => 4,
            Self::SuperscriptHtml | Self::SubscriptHtml => 5,
            Self::BoldHtml => 6,
            Self::ItalicHtml => 7,
            Self::CodeMarkdown { .. } => 8,
        }
    }

    /// 判断是否为超文本分隔符。
    fn is_html(self) -> bool {
        matches!(
            self,
            Self::BoldHtml | Self::ItalicHtml | Self::SuperscriptHtml | Self::SubscriptHtml
        )
    }

    /// 取 Markdown 强调分隔符的标记字符，超文本与代码段返回 `None`。
    fn emphasis_marker(self) -> Option<char> {
        match self {
            Self::BoldMarkdown { marker } | Self::ItalicMarkdown { marker } => Some(marker),
            Self::StrikethroughMarkdown => Some('~'),
            _ => None,
        }
    }

    /// 取 Markdown 强调分隔符的标记长度。
    fn emphasis_len(self) -> usize {
        match self {
            Self::BoldMarkdown { .. } => 2,
            _ => 1,
        }
    }
}

/// 编辑命令可寻址的行内样式标志。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InlineStyleFlag {
    /// 加粗文本。
    Bold,
    /// 斜体文本。
    Italic,
    /// 下划线文本。
    Underline,
    /// 删除线文本。
    Strikethrough,
    /// 行内代码文本。
    Code,
    /// 上标文本。
    Superscript,
    /// 下标文本。
    Subscript,
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

impl InlineNormalizer {
    /// 新建归一化构造器。
    fn new(input_len: usize) -> Self {
        Self {
            fragments: Vec::new(),
            visible_to_normalized: vec![0; input_len + 1],
            normalized_len: 0,
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
        if let Some(ref end_delim) = end_delimiter {
            let mut closed = match end_delim {
                EmphasisDelimiter::CodeMarkdown { run_len } => {
                    tokens[index].ch == '`' && backtick_run_len(tokens, index) == *run_len
                }
                EmphasisDelimiter::SuperscriptMarkdown => {
                    tokens[index].ch == '^' && can_close_emphasis(tokens, index, *end_delim)
                }
                EmphasisDelimiter::SubscriptMarkdown => {
                    is_single_tilde_delimiter(tokens, index)
                        && can_close_emphasis(tokens, index, *end_delim)
                }
                EmphasisDelimiter::StrikethroughMarkdown => {
                    is_exactly_two_tildes(tokens, index)
                        && can_close_emphasis(tokens, index, *end_delim)
                }
                _ => {
                    matches_sequence(tokens, index, &end_delim.close())
                        && can_close_emphasis(tokens, index, *end_delim)
                        && !emphasis_pair_blocked(tokens, opener_index, *end_delim, index)
                }
            };

            // 强调区间必须包含至少一个字符，紧贴正文起始的结束符视为字面文本。
            if closed && index == body_start && emphasis_requires_body(*end_delim) {
                closed = false;
            }

            if closed {
                let close_len = end_delim.close().chars().count();
                for token in &tokens[index..index + close_len] {
                    builder.drop_token(token);
                }
                return InlineParseResult {
                    next_index: index + close_len,
                    closed: true,
                };
            }
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
                if has_closing_delimiter(tokens, index, delimiter) {
                    for token in &tokens[index..index + delimiter.token_len()] {
                        builder.drop_token(token);
                    }
                    let inner_start = index + delimiter.token_len();
                    let is_code_delim = matches!(delimiter, EmphasisDelimiter::CodeMarkdown { .. });
                    let parsed = parse_until(
                        tokens,
                        inner_start,
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
                } else if delimiter.token_len() > 1 {
                    // 未闭合的多字符起始符（`**`、`__`、`~~`、反引号串）整体按字面保留，
                    // 若只输出首字符，剩余部分会开启更短的区间并在每次按键提交，
                    // 从而丢失原本的加粗意图。
                    for token in &tokens[index..index + delimiter.token_len()] {
                        builder.emit_token(token, extra_style, extra_html_style);
                    }
                    index += delimiter.token_len();
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
            let normalized_label = super::image::normalize_link_label(&link_label)?;
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
            let normalized_label = super::image::normalize_link_label(&raw_label)?;
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
    let close_str = delimiter.close();

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
    let requires_body = emphasis_requires_body(delimiter);
    let mut cursor = body_start;
    while cursor < tokens.len() {
        if tokens[cursor].ch == '\\'
            && let Some(escaped_len) = escaped_sequence_token_len(tokens, cursor)
        {
            cursor += 1 + escaped_len;
            continue;
        }

        if matches_sequence(tokens, cursor, &close_str) {
            // 强调区间必须非空，紧贴起始符的结束符视为空区间，按字面处理。
            if requires_body && cursor == body_start {
                cursor += 1;
                continue;
            }
            // 规则 3：跳过因长度组合无法与开启符配对的结束符。
            if emphasis_pair_blocked(tokens, index, delimiter, cursor) {
                cursor += 1;
                continue;
            }
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

/// 判断样式标志是否启用。
fn style_flag_enabled(style: InlineStyle, flag: InlineStyleFlag) -> bool {
    match flag {
        InlineStyleFlag::Bold => style.bold,
        InlineStyleFlag::Italic => style.italic,
        InlineStyleFlag::Underline => style.underline,
        InlineStyleFlag::Strikethrough => style.strikethrough,
        InlineStyleFlag::Code => style.code,
        InlineStyleFlag::Superscript => style.script == InlineScript::Superscript,
        InlineStyleFlag::Subscript => style.script == InlineScript::Subscript,
    }
}

/// 设置样式标志。
fn set_style_flag(mut style: InlineStyle, flag: InlineStyleFlag, enabled: bool) -> InlineStyle {
    match flag {
        InlineStyleFlag::Bold => style.bold = enabled,
        InlineStyleFlag::Italic => style.italic = enabled,
        InlineStyleFlag::Underline => style.underline = enabled,
        InlineStyleFlag::Strikethrough => style.strikethrough = enabled,
        InlineStyleFlag::Code => style.code = enabled,
        InlineStyleFlag::Superscript => {
            style.script = if enabled {
                InlineScript::Superscript
            } else if style.script == InlineScript::Superscript {
                InlineScript::Normal
            } else {
                style.script
            }
        }
        InlineStyleFlag::Subscript => {
            style.script = if enabled {
                InlineScript::Subscript
            } else if style.script == InlineScript::Subscript {
                InlineScript::Normal
            } else {
                style.script
            }
        }
    }
    style
}

/// 判断能否开启强调。
///
/// `_` 采用 CommonMark 侧翼（flanking）规则，单词内部的下划线不作为强调起始；
/// `*` 保持原有的简化判定以避免影响既有行为。
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
/// `_` 采用 CommonMark 侧翼规则，单词内部的下划线不作为强调结束。
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

#[cfg(test)]
mod tests {
    use super::{
        InlineFragment, InlineInsertionAttributes, InlineLinkHit, InlineMathDelimiter,
        InlineScript, InlineStyle, InlineStyleFlag, InlineTextTree, LinkReferenceDefinitions,
    };
    use crate::components::HtmlCssColor;

    #[test]
    fn parses_supported_styles_and_serializes_canonically() {
        let tree = InlineTextTree::from_markdown("1**23**4*56*7<u>89</u>0***ab***<u>*cd*</u>");
        let serialized = tree.serialize_markdown();
        let reparsed = InlineTextTree::from_markdown(&serialized);

        assert_eq!(tree.plain_text(), "1234567890abcd");
        assert_eq!(reparsed.plain_text(), tree.plain_text());
        assert_eq!(reparsed.render_cache().spans(), tree.render_cache().spans());
    }

    #[test]
    fn parses_underscore_emphasis_and_canonicalizes_to_asterisks() {
        let tree = InlineTextTree::from_markdown("_a_ __b__");

        assert_eq!(tree.plain_text(), "a b");
        assert_eq!(tree.serialize_markdown(), "*a* **b**");
    }

    #[test]
    fn emphasis_delimiters_surrounded_by_spaces_stay_literal() {
        let tree = InlineTextTree::from_markdown("* a * _ b _");

        assert_eq!(tree.plain_text(), "* a * _ b _");
        assert_eq!(tree.serialize_markdown(), "\\* a \\* \\_ b \\_");
    }

    #[test]
    fn intraword_underscore_does_not_emphasize() {
        let tree = InlineTextTree::from_markdown("foo_bar_baz");

        assert_eq!(tree.plain_text(), "foo_bar_baz");
        assert!(
            tree.render_cache()
                .spans()
                .iter()
                .all(|span| !span.style.italic && !span.style.bold),
            "intraword underscores must stay literal"
        );
    }

    #[test]
    fn triple_tilde_is_not_strikethrough() {
        let tree = InlineTextTree::from_markdown("~~~三个波浪号~~~");

        assert_eq!(tree.plain_text(), "~~~三个波浪号~~~");
        assert!(
            tree.render_cache()
                .spans()
                .iter()
                .all(|span| !span.style.strikethrough),
            "a run of three tildes must not open strikethrough"
        );
    }

    #[test]
    fn preserves_unclosed_markers_as_literal_text() {
        let tree = InlineTextTree::from_markdown("1**234");

        assert_eq!(tree.plain_text(), "1**234");
        assert_eq!(tree.serialize_markdown(), "1\\*\\*234");
    }

    #[test]
    fn empty_emphasis_spans_stay_literal() {
        // `**`, `* *`, or `**word` must not be swallowed as an empty emphasis
        // span; the markers stay literal until a non-empty body is closed.
        for input in ["*", "**", "***", "****", "~~~~", "__"] {
            let tree = InlineTextTree::from_markdown(input);
            assert_eq!(tree.plain_text(), input, "input {input:?} lost markers");
        }

        let leading = InlineTextTree::from_markdown("**word");
        assert_eq!(leading.plain_text(), "**word");
        assert_eq!(leading.serialize_markdown(), "\\*\\*word");

        let trailing = InlineTextTree::from_markdown("**word*");
        assert_eq!(trailing.plain_text(), "**word*");
    }

    #[test]
    fn nested_bold_inside_italic_keeps_bold() {
        // 内层 `**` 紧跟非空白字符时，不应被误判为外层 `*` 的结束符（CommonMark 规则 3）。
        let tree = InlineTextTree::from_markdown("*斜体里放**加粗**文本*");
        assert_eq!(tree.plain_text(), "斜体里放加粗文本");

        let spans = tree.render_cache();
        let text = tree.plain_text();
        assert!(
            spans
                .spans()
                .iter()
                .any(|span| { span.style.bold && &text[span.range.clone()] == "加粗" }),
            "内层加粗必须保留"
        );
        assert!(
            spans.spans().iter().any(|span| {
                span.style.italic && !span.style.bold && &text[span.range.clone()] == "斜体里放"
            }),
            "外层斜体必须保留"
        );

        let compact = InlineTextTree::from_markdown("*a**b**c*");
        assert_eq!(compact.plain_text(), "abc");
        let compact_text = compact.plain_text();
        assert!(
            compact
                .render_cache()
                .spans()
                .iter()
                .any(|span| { span.style.bold && &compact_text[span.range.clone()] == "b" })
        );

        // 往返后语义保持一致。
        let serialized = tree.serialize_markdown();
        let reparsed = InlineTextTree::from_markdown(&serialized);
        assert_eq!(reparsed.plain_text(), tree.plain_text());
        assert_eq!(reparsed.render_cache().spans(), tree.render_cache().spans());
    }

    #[test]
    fn non_empty_emphasis_still_parses_after_empty_guard() {
        let bold = InlineTextTree::from_markdown("**word**");
        assert_eq!(bold.plain_text(), "word");
        assert_eq!(bold.serialize_markdown(), "**word**");

        let italic = InlineTextTree::from_markdown("*a*");
        assert_eq!(italic.plain_text(), "a");
        assert_eq!(italic.serialize_markdown(), "*a*");

        let single_char_bold = InlineTextTree::from_markdown("**a**");
        assert_eq!(single_char_bold.plain_text(), "a");
        assert_eq!(single_char_bold.serialize_markdown(), "**a**");

        let bold_italic = InlineTextTree::from_markdown("***x***");
        assert_eq!(bold_italic.plain_text(), "x");
        let spans = bold_italic.render_cache();
        assert!(
            spans
                .spans()
                .iter()
                .all(|span| span.style.bold && span.style.italic)
        );
    }

    #[test]
    fn unclosed_multichar_opener_stays_fully_literal() {
        // While typing `**bold**`, the intermediate `**bold*` must stay literal;
        // otherwise the second `*` opens an italic span and the bold is lost.
        let partial = InlineTextTree::from_markdown("**bold*");
        assert_eq!(partial.plain_text(), "**bold*");
        assert!(
            partial
                .render_cache()
                .spans()
                .iter()
                .all(|span| !span.style.italic && !span.style.bold),
            "`**bold*` must be plain literal, not italic"
        );

        // The completed marker still resolves to bold (not italic).
        let complete = InlineTextTree::from_markdown("**bold**");
        assert_eq!(complete.plain_text(), "bold");
        assert!(
            complete
                .render_cache()
                .spans()
                .iter()
                .all(|span| span.style.bold && !span.style.italic),
            "`**bold**` must be bold, not italic"
        );

        // A genuine single-`*` italic opener is unaffected by the multi-char rule.
        let italic = InlineTextTree::from_markdown("*word*");
        assert_eq!(italic.plain_text(), "word");
        assert!(
            italic
                .render_cache()
                .spans()
                .iter()
                .all(|span| span.style.italic && !span.style.bold),
            "`*word*` must stay italic"
        );

        // Other unclosed multi-char openers stay literal as a unit too.
        for input in ["__bold_", "~~strike~"] {
            let tree = InlineTextTree::from_markdown(input);
            assert_eq!(tree.plain_text(), input, "input {input:?} lost markers");
            assert!(
                tree.render_cache()
                    .spans()
                    .iter()
                    .all(|span| !span.style.italic
                        && !span.style.bold
                        && !span.style.strikethrough),
                "input {input:?} should be plain literal"
            );
        }
    }

    #[test]
    fn empty_code_span_is_unaffected_by_emphasis_guard() {
        // The empty-emphasis guard must not touch code spans. `*` inside a code
        // span stays literal and the span round-trips.
        let tree = InlineTextTree::from_markdown("`*`");
        assert_eq!(tree.plain_text(), "*");
        assert_eq!(tree.serialize_markdown(), "`*`");
    }

    #[test]
    fn preserves_escaped_marker_sequences_as_literal_text() {
        let tree = InlineTextTree::from_markdown("\\*\\*\\<u>text\\</u>\\\\");

        assert_eq!(tree.plain_text(), "**<u>text</u>\\");
        assert_eq!(tree.serialize_markdown(), "\\*\\*\\<u>text\\</u>\\\\");
    }

    #[test]
    fn backslash_escapes_any_ascii_punctuation_to_literal() {
        let tree = InlineTextTree::from_markdown("\\# \\> \\- \\. \\! \\&");

        assert_eq!(tree.plain_text(), "# > - . ! &");
        assert!(
            tree.render_cache()
                .spans()
                .iter()
                .all(|span| !span.style.bold && !span.style.italic && !span.style.strikethrough)
        );
    }

    #[test]
    fn decodes_named_and_numeric_html_entities() {
        let tree =
            InlineTextTree::from_markdown("实体：&copy; &amp; &lt; &gt; &quot; &hellip; &mdash;");
        assert_eq!(tree.plain_text(), "实体：© & < > \" … —");

        let numeric = InlineTextTree::from_markdown("&#x2764; &#10084;");
        assert_eq!(numeric.plain_text(), "❤ ❤");
    }

    #[test]
    fn unknown_entity_names_stay_literal() {
        let tree = InlineTextTree::from_markdown("&notarealentity;");
        assert_eq!(tree.plain_text(), "&notarealentity;");
    }

    #[test]
    fn decodes_emoji_shortcodes() {
        let tree = InlineTextTree::from_markdown(":smile: :rocket: :warning: :thumbsup:");
        assert_eq!(tree.plain_text(), "😄 🚀 ⚠️ 👍");

        let literal = InlineTextTree::from_markdown(":notanemoji:");
        assert_eq!(literal.plain_text(), ":notanemoji:");
    }

    #[test]
    fn trailing_backslash_becomes_hard_break_without_visible_backslash() {
        let tree = InlineTextTree::from_markdown("硬换行。\\\n下一行。");

        assert_eq!(tree.plain_text(), "硬换行。  \n下一行。");
        assert!(!tree.plain_text().contains('\\'));
        assert_eq!(tree.serialize_markdown(), "硬换行。  \n下一行。");
    }

    #[test]
    fn soft_break_collapses_to_space() {
        let tree = InlineTextTree::from_markdown("first line\nsecond line");

        assert_eq!(tree.plain_text(), "first line second line");
        assert_eq!(tree.serialize_markdown(), "first line second line");
    }

    #[test]
    fn blank_line_and_hard_break_keep_newline() {
        let blank = InlineTextTree::from_markdown("first\n\nsecond");
        assert_eq!(blank.plain_text(), "first\n\nsecond");

        let spaced = InlineTextTree::from_markdown("first  \nsecond");
        assert_eq!(spaced.plain_text(), "first  \nsecond");
        assert_eq!(spaced.serialize_markdown(), "first  \nsecond");

        let backslash = InlineTextTree::from_markdown("first\\\nsecond");
        assert_eq!(backslash.plain_text(), "first  \nsecond");
    }

    #[test]
    fn escaped_block_markers_render_as_literal_text() {
        for (source, expected) in [
            ("\\* 转义星号 \\*", "* 转义星号 *"),
            ("\\_ 转义下划线 \\_", "_ 转义下划线 _"),
            ("\\` 转义反引号 \\`", "` 转义反引号 `"),
            ("\\[ 转义方括号 \\]", "[ 转义方括号 ]"),
            ("\\# 转义标题符号", "# 转义标题符号"),
            ("\\> 转义引用符号", "> 转义引用符号"),
            ("\\- 转义列表符号", "- 转义列表符号"),
            ("\\. 转义有序列表点", ". 转义有序列表点"),
            ("\\! 转义感叹号", "! 转义感叹号"),
            ("\\& 转义与号", "& 转义与号"),
        ] {
            assert_eq!(
                InlineTextTree::from_markdown(source).plain_text(),
                expected,
                "source: {source}"
            );
        }
    }

    #[test]
    fn lone_markers_and_ampersands_stay_literal() {
        let lone = "孤立标记符：* 星号、_ 下划线、~ 波浪号、` 反引号。";
        assert_eq!(InlineTextTree::from_markdown(lone).plain_text(), lone);

        assert_eq!(InlineTextTree::from_markdown("AT&T").plain_text(), "AT&T");
        assert_eq!(InlineTextTree::from_markdown("a & b").plain_text(), "a & b");
    }

    #[test]
    fn preserves_tibetan_spaces_through_inline_round_trip() {
        let markdown = "༄༅།།དཔལ་ལྡན་རྩ་བའི་བླ་མ་རིན་པོ་ཆེ།། བདག་གི་སྤྱི་བོར་པདྨའི་གདན་བཞུགས་ནས།། ";
        let tree = InlineTextTree::from_markdown(markdown);
        let serialized = tree.serialize_markdown();

        assert_eq!(tree.plain_text(), markdown);
        assert!(
            tree.plain_text().contains("།། བདག"),
            "plain: {:?}",
            tree.plain_text()
        );
        assert!(tree.plain_text().ends_with(' '));
        assert_eq!(serialized, markdown);
        assert_eq!(
            InlineTextTree::from_markdown(&serialized).plain_text(),
            markdown
        );
    }

    #[test]
    fn preserves_chinese_spaces_through_inline_round_trip() {
        let markdown = "中文 文本 ";
        let tree = InlineTextTree::from_markdown(markdown);

        assert_eq!(tree.plain_text(), markdown);
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn toggle_style_operates_on_selected_slice_only() {
        let mut tree = InlineTextTree::plain("123");
        assert!(tree.toggle_bold(1..3));
        assert_eq!(tree.serialize_markdown(), "1**23**");

        assert!(tree.toggle_bold(2..3));
        assert_eq!(tree.serialize_markdown(), "1**2**3");
    }

    #[test]
    fn replaces_visible_range_and_normalizes_manual_markdown_input() {
        let tree = InlineTextTree::plain(String::new());
        let result =
            tree.replace_plain_range(0..0, "**bold**", InlineInsertionAttributes::default());

        assert_eq!(result.tree.plain_text(), "bold");
        assert_eq!(result.map_offset(8), 4);
        assert_eq!(result.tree.serialize_markdown(), "**bold**");
    }

    #[test]
    fn renders_nested_marks_without_storing_markers_in_text() {
        let tree = InlineTextTree::from_markdown("**<u>*TEST*</u>**");
        let cache = tree.render_cache();

        assert_eq!(cache.plain_text(), "TEST");
        assert_eq!(
            cache.style_at(0),
            InlineStyle {
                bold: true,
                italic: true,
                underline: true,
                strikethrough: false,
                code: false,
                script: InlineScript::Normal,
            }
        );
    }

    #[test]
    fn replace_visible_range_raw_preserves_markers_as_literal_text() {
        let tree = InlineTextTree::plain("alpha");
        let result = tree.replace_plain_range_raw(
            5..5,
            "**`<u>x</u>`**",
            InlineInsertionAttributes::default(),
        );

        assert_eq!(result.tree.plain_text(), "alpha**`<u>x</u>`**");
        assert_eq!(
            result.tree.serialize_markdown(),
            "alpha\\*\\*\\`\\<u>x\\</u>\\`\\*\\*"
        );
    }

    #[test]
    fn unwrap_code_fragments_keeps_text_and_removes_code_style() {
        let mut tree = InlineTextTree::from_markdown("before `code` after");
        tree.strip_styles_on_fragments(&[(1, InlineStyleFlag::Code)]);

        assert_eq!(tree.plain_text(), "before code after");
        let cache = tree.render_cache();
        assert!(!cache.style_at(7).code);
        assert_eq!(tree.serialize_markdown(), "before code after");
    }

    #[test]
    fn parses_and_serializes_strikethrough() {
        let tree = InlineTextTree::from_markdown("~~text~~");
        let cache = tree.render_cache();

        assert_eq!(tree.plain_text(), "text");
        assert!(cache.style_at(0).strikethrough);
        assert_eq!(tree.serialize_markdown(), "~~text~~");
    }

    #[test]
    fn parses_and_serializes_superscript() {
        let tree = InlineTextTree::from_markdown("x^2^");
        let cache = tree.render_cache();

        assert_eq!(tree.plain_text(), "x2");
        assert_eq!(cache.style_at(1).script, InlineScript::Superscript);
        assert_eq!(tree.serialize_markdown(), "x^2^");
    }

    #[test]
    fn parses_and_serializes_subscript_without_conflicting_with_strikethrough() {
        let tree = InlineTextTree::from_markdown("H~2~O and ~~old~~");
        let cache = tree.render_cache();

        assert_eq!(tree.plain_text(), "H2O and old");
        assert_eq!(cache.style_at(1).script, InlineScript::Subscript);
        assert!(cache.style_at("H2O and ".len()).strikethrough);
        assert_eq!(tree.serialize_markdown(), "H~2~O and ~~old~~");
    }

    #[test]
    fn script_markers_require_ascii_context_and_ascii_body() {
        for markdown in ["\\^2^", "\\~2~", "汉^2^", "H~二~O", "`x^2^ H~2~O`"] {
            let tree = InlineTextTree::from_markdown(markdown);
            assert!(
                tree.render_cache()
                    .spans()
                    .iter()
                    .all(|span| span.style.script == InlineScript::Normal),
                "{markdown} should not produce script spans"
            );
        }
    }

    #[test]
    fn inline_html_sup_and_sub_map_to_script_style() {
        let tree = InlineTextTree::from_markdown("x<sup>2</sup> and H<sub>2</sub>O");
        let cache = tree.render_cache();

        assert_eq!(tree.plain_text(), "x2 and H2O");
        assert_eq!(cache.style_at(1).script, InlineScript::Superscript);
        assert_eq!(
            cache.style_at("x2 and H".len()).script,
            InlineScript::Subscript
        );
        assert_eq!(tree.serialize_markdown(), "x^2^ and H~2~O");

        let standalone = InlineTextTree::from_markdown("<sup>2</sup>");
        assert_eq!(standalone.serialize_markdown(), "<sup>2</sup>");
    }

    #[test]
    fn unmatched_strikethrough_markers_stay_literal() {
        let tree = InlineTextTree::from_markdown("~~text");
        assert_eq!(tree.plain_text(), "~~text");
        assert_eq!(tree.serialize_markdown(), "\\~\\~text");
    }

    #[test]
    fn toggle_strikethrough_operates_on_selected_slice_only() {
        let mut tree = InlineTextTree::plain("1234");
        assert!(tree.toggle_strikethrough(1..4));
        assert!(tree.toggle_strikethrough(2..4));

        let serialized = tree.serialize_markdown();
        let reparsed = InlineTextTree::from_markdown(&serialized);

        assert_eq!(serialized, "1~~2~~34");
        assert_eq!(tree, reparsed);
    }

    #[test]
    fn insertion_at_outer_end_of_terminal_strikethrough_is_plain_text() {
        let tree = InlineTextTree::from_markdown("~~123~~");
        let result = tree.replace_plain_range(
            tree.plain_len()..tree.plain_len(),
            "456",
            tree.insertion_attributes_at(tree.plain_len()),
        );
        assert_eq!(result.tree.serialize_markdown(), "~~123~~456");
    }

    #[test]
    fn insertion_at_outer_start_of_terminal_strikethrough_is_plain_text() {
        let tree = InlineTextTree::from_markdown("~~123~~");
        let result = tree.replace_plain_range(0..0, "0", tree.insertion_attributes_at(0));
        assert_eq!(result.tree.serialize_markdown(), "0~~123~~");
    }

    #[test]
    fn serializes_partial_underline_removal_without_ambiguous_star_runs() {
        let mut tree = InlineTextTree::plain("1234");
        assert!(tree.toggle_bold(1..4));
        assert!(tree.toggle_underline(1..4));
        assert!(tree.toggle_italic(1..4));
        assert!(tree.toggle_underline(2..4));

        let serialized = tree.serialize_markdown();
        let reparsed = InlineTextTree::from_markdown(&serialized);

        assert_eq!(serialized, "1**<u>*2*</u>*34***");
        assert!(!serialized.contains("*****34"), "serialized: {serialized}");
        assert_eq!(reparsed.plain_text(), "1234");
        assert_eq!(reparsed.render_cache().spans(), tree.render_cache().spans());
    }

    #[test]
    fn parses_inline_links_autolinks_and_preserves_other_unsupported_inline_syntax() {
        let markdown =
            "[link](http://example.com) ![alt](/img.png) <http://example.com/> <span>x</span>";
        let tree = InlineTextTree::from_markdown(markdown);

        assert_eq!(
            tree.plain_text(),
            "link ![alt](/img.png) http://example.com/ <span>x</span>"
        );
        assert_eq!(tree.render_cache().link_at(0), Some("http://example.com"));
        assert_eq!(
            tree.render_cache().link_at("link ![alt](/img.png) ".len()),
            Some("http://example.com/")
        );
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn parses_dollar_inline_math_as_source_preserving_fragment() {
        let markdown = "before $x^2$ after";
        let tree = InlineTextTree::from_markdown(markdown);
        let cache = tree.render_cache();
        let math_start = "before ".len();
        let math = cache
            .inline_math_at(math_start)
            .expect("inline math span should be recorded");

        assert_eq!(tree.plain_text(), markdown);
        assert_eq!(math.source, "$x^2$");
        assert_eq!(math.body, "x^2");
        assert_eq!(math.delimiter, InlineMathDelimiter::Dollar);
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn parses_paren_inline_math_as_source_preserving_fragment() {
        let markdown = "before \\(\\frac{1}{2}\\) after";
        let tree = InlineTextTree::from_markdown(markdown);
        let cache = tree.render_cache();
        let math_start = "before ".len();
        let math = cache
            .inline_math_at(math_start)
            .expect("inline math span should be recorded");

        assert_eq!(tree.plain_text(), markdown);
        assert_eq!(math.source, "\\(\\frac{1}{2}\\)");
        assert_eq!(math.body, "\\frac{1}{2}");
        assert_eq!(math.delimiter, InlineMathDelimiter::Paren);
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn rejects_conservative_inline_math_cases() {
        for markdown in ["\\$x$", "$ x $", "$", "$x\ny$", "cost $12$"] {
            let tree = InlineTextTree::from_markdown(markdown);
            assert!(
                tree.render_cache()
                    .spans()
                    .iter()
                    .all(|span| span.math.is_none()),
                "{markdown:?} should stay plain text"
            );
        }
    }

    #[test]
    fn inline_math_does_not_parse_inside_code_spans() {
        let tree = InlineTextTree::from_markdown("`$x$` and $y$");
        let cache = tree.render_cache();

        assert!(cache.style_at(0).code);
        assert!(cache.inline_math_at(0).is_none());
        assert!(cache.inline_math_at("$x$ and ".len()).is_some());
        assert_eq!(tree.serialize_markdown(), "`$x$` and $y$");
    }

    #[test]
    fn parses_inline_link_title_without_polluting_open_target() {
        let markdown = "[ABC](https://abc.com \"https://abc.com\")";
        let tree = InlineTextTree::from_markdown(markdown);

        assert_eq!(tree.plain_text(), "ABC");
        assert_eq!(
            tree.render_cache().link_hit_at(0),
            Some(&InlineLinkHit {
                prompt_target: "https://abc.com".to_string(),
                open_target: "https://abc.com".to_string(),
            })
        );
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn parses_span_style_as_inline_html_not_link() {
        let markdown = "留意<span style='color:blue;'>磁盘预留空间、系统环境变量</span>等问题";
        let tree = InlineTextTree::from_markdown(markdown);
        let cache = tree.render_cache();
        let span_start = "留意".len();

        assert_eq!(tree.plain_text(), "留意磁盘预留空间、系统环境变量等问题");
        assert_eq!(cache.link_at(span_start), None);
        assert!(matches!(
            cache.html_style_at(span_start).and_then(|style| style.color),
            Some(HtmlCssColor::Rgba(color))
                if color.red == 0 && color.green == 0 && color.blue == 255
        ));
        assert_eq!(cache.html_style_at(0), None);
        assert_eq!(
            tree.serialize_markdown(),
            "留意<span style=\"color: rgba(0,0,255,1.000);\">磁盘预留空间、系统环境变量</span>等问题"
        );
    }

    #[test]
    fn inline_span_style_allows_nested_markdown_code() {
        let markdown = "<span style='color:blue;'>英伟达驱动`CUDA+cuDNN`</span>";
        let tree = InlineTextTree::from_markdown(markdown);
        let cache = tree.render_cache();
        let code_start = "英伟达驱动".len();

        assert_eq!(tree.plain_text(), "英伟达驱动CUDA+cuDNN");
        assert!(cache.style_at(code_start).code);
        assert!(matches!(
            cache.html_style_at(code_start).and_then(|style| style.color),
            Some(HtmlCssColor::Rgba(color))
                if color.red == 0 && color.green == 0 && color.blue == 255
        ));

        let reparsed = InlineTextTree::from_markdown(&tree.serialize_markdown());
        assert_eq!(reparsed.plain_text(), tree.plain_text());
        assert_eq!(reparsed.render_cache().spans(), tree.render_cache().spans());
    }

    #[test]
    fn html_like_tags_are_not_autolinks_when_unsafe_or_unclosed() {
        let unclosed = InlineTextTree::from_markdown("<span style='color:blue;'>x");
        assert_eq!(unclosed.plain_text(), "<span style='color:blue;'>x");
        assert_eq!(unclosed.render_cache().link_at(0), None);

        let script = InlineTextTree::from_markdown("<script>alert(1)</script>");
        assert_eq!(script.plain_text(), "<script>alert(1)</script>");
        assert_eq!(script.render_cache().link_at(0), None);
    }

    #[test]
    fn parses_reference_style_links_with_definitions_and_preserves_syntax() {
        let markdown = "[reference link][ref-link]";
        let definitions =
            super::super::link::parse_link_reference_definitions("[ref-link]: https://example.com");
        let tree = InlineTextTree::from_markdown_with_link_references(markdown, &definitions);

        assert_eq!(tree.plain_text(), "reference link");
        assert_eq!(tree.render_cache().link_at(0), Some("https://example.com"));
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn parses_reference_style_links_with_generic_normalized_labels() {
        let markdown = "[reference link][Ref   Links]";
        let definitions = super::super::link::parse_link_reference_definitions(
            "[ref links]: https://example.com",
        );
        let tree = InlineTextTree::from_markdown_with_link_references(markdown, &definitions);

        assert_eq!(tree.plain_text(), "reference link");
        assert_eq!(tree.render_cache().link_at(0), Some("https://example.com"));
        assert_eq!(
            tree.render_cache().link_hit_at(0),
            Some(&InlineLinkHit {
                prompt_target: "Ref   Links".to_string(),
                open_target: "https://example.com".to_string(),
            })
        );
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn parses_collapsed_reference_style_links_with_definitions() {
        let markdown = "[collapsed reference][]";
        let definitions = super::super::link::parse_link_reference_definitions(
            "[collapsed reference]: https://example.org",
        );
        let tree = InlineTextTree::from_markdown_with_link_references(markdown, &definitions);

        assert_eq!(tree.plain_text(), "collapsed reference");
        assert_eq!(tree.render_cache().link_at(0), Some("https://example.org"));
        assert_eq!(
            tree.serialize_markdown(),
            "[collapsed reference][collapsed reference]"
        );
    }

    #[test]
    fn parses_shortcut_reference_style_links_with_definitions() {
        let markdown = "[shortcut reference]";
        let definitions = super::super::link::parse_link_reference_definitions(
            "[shortcut reference]: https://example.net",
        );
        let tree = InlineTextTree::from_markdown_with_link_references(markdown, &definitions);

        assert_eq!(tree.plain_text(), "shortcut reference");
        assert_eq!(tree.render_cache().link_at(0), Some("https://example.net"));
        assert_eq!(
            tree.serialize_markdown(),
            "[shortcut reference][shortcut reference]"
        );
    }

    #[test]
    fn resolves_reference_link_examples_from_test_markdown() {
        let markdown = include_str!("../../../docs/render-sample.md");
        let definitions = super::super::link::parse_link_reference_definitions(markdown);
        let tree = InlineTextTree::from_markdown_with_link_references(
            "[引用链接][ref-link] [折叠引用][] [快捷引用]",
            &definitions,
        );

        assert_eq!(tree.plain_text(), "引用链接 折叠引用 快捷引用");
        assert_eq!(tree.render_cache().link_at(0), Some("https://example.com"));
        assert_eq!(
            tree.render_cache().link_at("引用链接 ".len()),
            Some("https://example.org")
        );
        assert_eq!(
            tree.render_cache().link_at("引用链接 折叠引用 ".len()),
            Some("https://example.net")
        );
    }

    #[test]
    fn unresolved_reference_style_links_remain_literal_text() {
        let markdown = "[reference link][missing]";
        let tree = InlineTextTree::from_markdown_with_link_references(
            markdown,
            &LinkReferenceDefinitions::default(),
        );

        assert_eq!(tree.plain_text(), markdown);
        assert_eq!(tree.render_cache().link_at(0), None);
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn unresolved_shortcut_reference_links_remain_literal_text() {
        let markdown = "[shortcut reference]";
        let tree = InlineTextTree::from_markdown_with_link_references(
            markdown,
            &LinkReferenceDefinitions::default(),
        );

        assert_eq!(tree.plain_text(), markdown);
        assert_eq!(tree.render_cache().link_at(0), None);
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn shortcut_reference_detection_does_not_consume_images_as_links() {
        let definitions = super::super::link::parse_link_reference_definitions(
            "[alt]: https://example.com/not-an-image-link",
        );
        let tree = InlineTextTree::from_markdown_with_link_references("![alt]", &definitions);

        assert_eq!(tree.plain_text(), "![alt]");
        assert_eq!(tree.render_cache().link_at(0), None);
        assert_eq!(tree.serialize_markdown(), "![alt]");
    }

    #[test]
    fn shortcut_reference_detection_does_not_rewrite_reference_images() {
        let definitions = super::super::link::parse_link_reference_definitions(
            "[img]: https://example.com/image.png",
        );
        let tree =
            InlineTextTree::from_markdown_with_link_references("![cover][img]", &definitions);

        assert_eq!(tree.plain_text(), "![cover][img]");
        assert_eq!(tree.render_cache().link_at(0), None);
        assert_eq!(tree.serialize_markdown(), "![cover][img]");
    }

    #[test]
    fn parses_mailto_autolinks_and_preserves_syntax() {
        let markdown = "<mailto:test@example.com>";
        let tree = InlineTextTree::from_markdown(markdown);

        assert_eq!(tree.plain_text(), "mailto:test@example.com");
        assert_eq!(
            tree.render_cache().link_at(0),
            Some("mailto:test@example.com")
        );
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn parses_any_standalone_autolink_and_preserves_syntax() {
        let markdown = "<ref2>";
        let tree = InlineTextTree::from_markdown(markdown);

        assert_eq!(tree.plain_text(), "ref2");
        assert_eq!(tree.render_cache().link_at(0), Some("ref2"));
        assert_eq!(
            tree.render_cache().link_hit_at(0),
            Some(&InlineLinkHit {
                prompt_target: "ref2".to_string(),
                open_target: "ref2".to_string(),
            })
        );
        assert_eq!(tree.serialize_markdown(), markdown);
    }

    #[test]
    fn parses_nested_inline_marks_inside_link_label() {
        let tree = InlineTextTree::from_markdown("[**go** now](https://example.com)");
        let cache = tree.render_cache();

        assert_eq!(tree.plain_text(), "go now");
        assert_eq!(cache.link_at(0), Some("https://example.com"));
        assert!(cache.style_at(0).bold);
        assert_eq!(
            tree.serialize_markdown(),
            "[**go** now](https://example.com)"
        );
    }

    #[test]
    fn serializes_partial_bold_removal_without_ambiguous_star_runs() {
        let mut tree = InlineTextTree::plain("1234");
        assert!(tree.toggle_bold(1..4));
        assert!(tree.toggle_italic(1..4));
        assert!(tree.toggle_bold(2..4));

        let serialized = tree.serialize_markdown();
        let reparsed = InlineTextTree::from_markdown(&serialized);

        assert_eq!(serialized, "1***2***<em>34</em>");
        assert_eq!(reparsed.plain_text(), "1234");
        assert_eq!(reparsed.render_cache().spans(), tree.render_cache().spans());
    }

    // --- inline code tests ---

    #[test]
    fn parses_backtick_as_code_style() {
        let tree = InlineTextTree::from_markdown("a `code` b");
        let cache = tree.render_cache();

        assert_eq!(cache.plain_text(), "a code b");
        // "code" at offset 2 should have code style
        let style = cache.style_at(2);
        assert!(style.code, "expected code=true at offset 2");
        assert!(!style.bold);
    }

    #[test]
    fn backtick_content_preserves_markers_as_literal() {
        // Inside a code span, ** and * are literal, not parsed as bold/italic.
        let tree = InlineTextTree::from_markdown("`**not bold**`");
        let cache = tree.render_cache();

        assert_eq!(cache.plain_text(), "**not bold**");
        let style = cache.style_at(0);
        assert!(style.code);
        assert!(!style.bold);
        assert!(!style.italic);
    }

    #[test]
    fn unclosed_backtick_is_literal() {
        let tree = InlineTextTree::from_markdown("a `b");
        assert_eq!(tree.plain_text(), "a `b");
        assert_eq!(tree.serialize_markdown(), "a \\`b");
    }

    #[test]
    fn toggle_code_on_selection() {
        let mut tree = InlineTextTree::plain("hello world");
        assert!(tree.toggle_code(0..5)); // "hello"
        assert_eq!(tree.serialize_markdown(), "`hello` world");
    }

    #[test]
    fn toggle_code_twice_removes_code() {
        let mut tree = InlineTextTree::plain("hello world");
        assert!(tree.toggle_code(0..5));
        assert!(tree.toggle_code(0..5)); // toggle back
        assert_eq!(tree.serialize_markdown(), "hello world");
    }

    #[test]
    fn code_round_trips_through_serialization() {
        let tree = InlineTextTree::from_markdown("a `code` b");
        let serialized = tree.serialize_markdown();
        let reparsed = InlineTextTree::from_markdown(&serialized);

        assert_eq!(serialized, "a `code` b");
        assert_eq!(reparsed.plain_text(), "a code b");
        assert_eq!(reparsed.render_cache().spans(), tree.render_cache().spans());
    }

    #[test]
    fn code_inside_bold_text() {
        // `**bold `code` more**` — bold wraps around a code span.
        let tree = InlineTextTree::from_markdown("**bold `code` more**");
        let serialized = tree.serialize_markdown();
        let reparsed = InlineTextTree::from_markdown(&serialized);

        assert_eq!(tree.plain_text(), "bold code more");
        assert_eq!(reparsed.plain_text(), tree.plain_text());
        assert_eq!(reparsed.render_cache().spans(), tree.render_cache().spans());
    }

    #[test]
    fn consecutive_backticks_treated_as_literal() {
        // Per CommonMark: a backtick run that has no matching closing run
        // is treated as literal text.
        let tree = InlineTextTree::from_markdown("``");
        // Two backticks with no closing -> literal (run_len=2, no matching close).
        assert_eq!(tree.plain_text(), "``");
        assert!(!tree.render_cache().style_at(0).code);
    }

    #[test]
    fn variable_length_backtick_run() {
        // `` `` `x` ``` `` (run_len=1 with 'x', matching close of run_len=1)
        let tree = InlineTextTree::from_markdown("`x`");
        assert_eq!(tree.plain_text(), "x");
        assert!(tree.render_cache().style_at(0).code);

        // ``` `` `` `` `` (run_len=2, content "a", run_len=2 close)
        let tree2 = InlineTextTree::from_markdown("``a``");
        assert_eq!(tree2.plain_text(), "a");
        assert!(tree2.render_cache().style_at(0).code);
    }

    #[test]
    fn code_span_content_normalization() {
        // Leading/trailing single space is stripped.
        let tree = InlineTextTree::from_markdown("` hello `");
        assert_eq!(tree.plain_text(), "hello");
        assert!(tree.render_cache().style_at(0).code);

        // All-space content is preserved (no stripping per spec).
        let tree2 = InlineTextTree::from_markdown("`   `");
        assert_eq!(tree2.plain_text(), "   ");
    }

    #[test]
    fn code_span_newline_becomes_space() {
        // CommonMark/GitHub：代码段内换行转为空格，始终渲染为一行。
        let tree = InlineTextTree::from_markdown("`a\nb`");
        assert_eq!(tree.plain_text(), "a b");

        let cache = tree.render_cache();
        assert_eq!(cache.spans().len(), 1);
        assert_eq!(cache.spans()[0].range, 0..3);
        assert!(cache.spans()[0].style.code);
        assert_eq!(tree.serialize_markdown(), "`a b`");
    }

    #[test]
    fn code_span_blank_line_stays_inside_single_code_span() {
        let tree = InlineTextTree::from_markdown("`line 1\n\nline 2`");
        assert_eq!(tree.plain_text(), "line 1  line 2");

        let cache = tree.render_cache();
        assert_eq!(cache.spans().len(), 1);
        assert_eq!(cache.spans()[0].range, 0.."line 1  line 2".len());
        assert!(cache.spans()[0].style.code);
        assert_eq!(tree.serialize_markdown(), "`line 1  line 2`");
    }

    #[test]
    fn code_span_content_keeps_inline_markers_literal() {
        let tree = InlineTextTree::from_markdown("`*[x] [link](x) \\\\`");

        assert_eq!(tree.plain_text(), "*[x] [link](x) \\\\");
        let cache = tree.render_cache();
        assert_eq!(cache.spans().len(), 1);
        assert!(cache.spans()[0].style.code);
        assert!(cache.spans()[0].link.is_none());
        assert!(!cache.spans()[0].style.bold);
        assert!(!cache.spans()[0].style.italic);
    }

    #[test]
    fn parses_literal_backtick_runs_with_unambiguous_delimiters() {
        let markdown = "`` ` `` and ``` `` ``` and ```` ``` ````";
        let tree = InlineTextTree::from_markdown(markdown);
        let cache = tree.render_cache();
        let code_ranges = cache
            .spans()
            .iter()
            .filter(|span| span.style.code)
            .map(|span| span.range.clone())
            .collect::<Vec<_>>();

        assert_eq!(tree.plain_text(), "` and `` and ```");
        assert_eq!(code_ranges, vec![0..1, 6..8, 13..16]);
        assert!(!cache.style_at("` ".len()).code);
        assert!(!cache.style_at("` and `` ".len()).code);

        let serialized = tree.serialize_markdown();
        let reparsed = InlineTextTree::from_markdown(&serialized);
        assert_eq!(reparsed.plain_text(), tree.plain_text());
        assert_eq!(reparsed.render_cache().spans(), cache.spans());
    }

    #[test]
    fn serializes_code_spans_with_safe_backtick_delimiters_and_padding() {
        for text in [" leading", "trailing ", "`tick", "tick`", "`", "``", "   "] {
            let tree = InlineTextTree::from_fragments(vec![InlineFragment {
                text: text.to_string(),
                style: InlineStyle {
                    code: true,
                    ..InlineStyle::default()
                },
                html_style: None,
                link: None,
                footnote: None,
                math: None,
            }]);
            let serialized = tree.serialize_markdown();
            let reparsed = InlineTextTree::from_markdown(&serialized);

            assert_eq!(reparsed.plain_text(), text, "serialized as {serialized:?}");
            assert_eq!(reparsed.render_cache().spans(), tree.render_cache().spans());
        }
    }

    #[test]
    fn source_to_rendered_round_trip_preserves_code_span() {
        // Simulate Source -> Rendered: raw markdown -> from_markdown parses it.
        let raw = "`123`";
        let tree = InlineTextTree::from_markdown(raw);
        assert_eq!(tree.plain_text(), "123");
        assert!(tree.render_cache().style_at(0).code);

        // Serialize back: must produce valid markdown.
        let serialized = tree.serialize_markdown();
        assert_eq!(serialized, "`123`");

        // Re-parse: must produce same result.
        let reparsed = InlineTextTree::from_markdown(&serialized);
        assert_eq!(reparsed.plain_text(), "123");
        assert!(reparsed.render_cache().style_at(0).code);
    }

    #[test]
    fn raw_text_with_backticks_not_double_escaped() {
        // Simulate the Source block's rendered_text() path.
        let raw = "`123`";
        // rendered_text() returns raw text as-is; from_markdown re-parses.
        let parsed = InlineTextTree::from_markdown(raw);
        assert_eq!(parsed.plain_text(), "123");

        // A second round-trip should NOT escape or double the backticks.
        let serialized = parsed.serialize_markdown();
        assert_eq!(serialized, "`123`");
        let reparsed = InlineTextTree::from_markdown(&serialized);
        assert_eq!(reparsed.plain_text(), "123");
    }

    #[test]
    fn escaped_backtick_in_code() {
        let tree = InlineTextTree::from_markdown("\\`not code\\`");
        assert_eq!(tree.plain_text(), "`not code`");
        // Escaped backticks are literal, not code delimiters.
        let cache = tree.render_cache();
        assert!(!cache.style_at(0).code);
        assert_eq!(tree.serialize_markdown(), "\\`not code\\`");
    }
}
