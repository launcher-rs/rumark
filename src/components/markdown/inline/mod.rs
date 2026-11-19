//! 块标题与表格单元格用的基于属性的行内 Markdown 树。
//!
//! 运行时模型仅存文本片段与格式属性。Markdown 标记在输入输出边界解析，保存时重新生成，使编辑操作聚焦文本范围而非原始分隔符串。

use std::ops::Range;

use super::footnote::{InlineFootnoteHit, InlineFootnoteReference, superscript_ordinal};
use super::html::HtmlInlineStyle;
use super::link::LinkReferenceDefinitions;

mod parse;
mod serialize;

#[cfg(test)]
mod tests;

use parse::normalize_inline_fragments;
pub(crate) use serialize::can_use_markdown_script_delimiters;
use serialize::serialize_fragment_run_markdown_with_offset_map;

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
        #[cfg(test)]
        let parse_started = std::time::Instant::now();

        // 绝大多数段落、标题和通知文本不含任何行内标记。完整归一化会为每个
        // 字符分配 token、逐边界写映射并递归检查分隔符，是大文档 parse 阶段
        // 的主要开销。若不存在任何可能的行内语法触发符，plain 树与完整解析
        // 结果等价，可直接走快路；软换行折叠仍按原语义执行。
        let fast_path = !has_inline_syntax_trigger(markdown);
        let tree = if fast_path {
            let mut tree = Self::plain(markdown);
            tree.normalize_soft_breaks();
            tree
        } else {
            let mut tree = Self::plain(markdown)
                .normalize_inline_syntax_with_link_references(reference_definitions)
                .tree;
            tree.normalize_code_spans();
            tree.normalize_soft_breaks();
            tree
        };
        #[cfg(test)]
        record_inline_parse(parse_started.elapsed(), fast_path);
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

    /// 是否可能受链接引用定义变化影响。
    ///
    /// 引用式链接（完整 `[标签][ref]`、折叠 `[标签][]`、快捷 `[标签]`）
    /// 在未解析时都含字面 `[`；解析后则带 [`InlineLink::Reference`] 标记。
    /// 两种情况任一命中即为候选；都不命中的块可安全跳过昂贵重解析。
    pub(crate) fn has_link_reference_candidate(&self) -> bool {
        self.fragments.iter().any(|fragment| {
            fragment.text.contains('[')
                || matches!(fragment.link, Some(InlineLink::Reference { .. }))
        })
    }

    /// 片段中是否出现独立图片标记 `![`。
    ///
    /// `parse_standalone_image` 要求文本以 `![` 开头，因此不含该标记的块
    /// 不可能解析出独立图片；用它做保守预筛，避免每块都分配可见文本与
    /// Markdown 两份字符串。快捷图片引用的 `![` 来自图片语法本身，同样
    /// 落在可见文本里，故不会漏判。
    pub(crate) fn has_image_marker(&self) -> bool {
        self.fragments
            .iter()
            .any(|fragment| fragment.text.contains("!["))
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

/// 是否存在任何可能被行内解析器消费的语法触发符。
///
/// 这里的集合故意保守：只要可能出现的语法就回退到完整解析，宁可少走快路，
/// 也不能把 `**粗体**`、`[链接]`、HTML 实体、Emoji 短代码等误判为纯文本。
fn has_inline_syntax_trigger(markdown: &str) -> bool {
    let bytes = markdown.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' | b'*' | b'_' | b'~' | b'^' | b'`' | b'<' | b'[' | b'$' | b'&' => return true,
            b':' if has_emoji_shortcode_candidate(bytes, index) => return true,
            _ => {}
        }
        index += 1;
    }
    false
}

/// 判断 `:` 处是否形如 `:shortcode:`，用于保守识别 Emoji 短代码。
///
/// 短代码名只包含 ASCII 字母数字、`_`、`-`、`+`。普通文本里的 `http://`、
/// `Note: message` 不会命中，这样带 URL 的许可证段落仍能走纯文本快路。
fn has_emoji_shortcode_candidate(bytes: &[u8], colon_index: usize) -> bool {
    let mut cursor = colon_index + 1;
    while cursor < bytes.len()
        && (bytes[cursor].is_ascii_alphanumeric() || matches!(bytes[cursor], b'_' | b'-' | b'+'))
    {
        cursor += 1;
    }
    cursor > colon_index + 1 && bytes.get(cursor) == Some(&b':')
}

// 性能归因探针：只在测试构建编译，用于把大文档 parse 阶段拆成自有 inline 解析
// 与其他建树开销。生产构建没有这些计数器和计时调用。
#[cfg(test)]
thread_local! {
    static INLINE_PARSE_NS: std::cell::Cell<u128> = const { std::cell::Cell::new(0) };
    static INLINE_PARSE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static INLINE_FAST_PATHS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_inline_parse(elapsed: std::time::Duration, fast_path: bool) {
    INLINE_PARSE_NS.with(|ns| ns.set(ns.get() + elapsed.as_nanos()));
    INLINE_PARSE_CALLS.with(|calls| calls.set(calls.get() + 1));
    if fast_path {
        INLINE_FAST_PATHS.with(|fast| fast.set(fast.get() + 1));
    }
}

/// 读取并清零 inline 解析统计：`(调用次数, 快路次数, 累计毫秒)`。
#[cfg(test)]
pub(crate) fn take_inline_parse_stats() -> (usize, usize, f64) {
    let ns = INLINE_PARSE_NS.with(|ns| ns.replace(0));
    let calls = INLINE_PARSE_CALLS.with(|calls| calls.replace(0));
    let fast = INLINE_FAST_PATHS.with(|fast| fast.replace(0));
    (calls, fast, ns as f64 / 1_000_000.0)
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
        normalize_inline_fragments(&self.fragments, reference_definitions)
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
