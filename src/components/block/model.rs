//! 块语义状态与块级 Markdown 解析辅助。
//!
//! 本模块定义序列化到 Markdown 并从中恢复的持久块记录。块级解析保持收窄：仅把运行时树可重建的语法解析为结构化块。

use std::ops::Range;
use std::path::PathBuf;

use rgpui::{Image, SharedString};
use uuid::Uuid;

use crate::components::markdown::html::{HtmlDocument, parse_html_document};
use crate::components::markdown::image::parse_standalone_image;
use crate::components::markdown::inline::InlineTextTree;
use crate::components::{TableAxisKind, TableAxisMenuOp, TableModel, parse_mermaid_fence_source};

/// 从 `[!TYPE]` 引用头解析出的提示框变体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalloutVariant {
    /// 信息提示框。
    Note,
    /// 实用技巧提示框。
    Tip,
    /// 高强调的重要提示框。
    Important,
    /// 风险或意外内容的警告提示框。
    Warning,
    /// 潜在危险操作的注意提示框。
    Caution,
}

impl CalloutVariant {
    /// 返回标记。
    pub fn marker(self) -> &'static str {
        match self {
            Self::Note => "NOTE",
            Self::Tip => "TIP",
            Self::Important => "IMPORTANT",
            Self::Warning => "WARNING",
            Self::Caution => "CAUTION",
        }
    }

    /// 返回显示标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }

    /// 返回图标。
    pub fn icon(self) -> &'static str {
        match self {
            Self::Note => "i",
            Self::Tip => "+",
            Self::Important => "*",
            Self::Warning => "!",
            Self::Caution => "x",
        }
    }

    /// 解析提示框头行。
    pub fn parse_heading(line: &str) -> Option<(Self, String)> {
        let trimmed = line.trim_start();
        let rest = trimmed.strip_prefix("[!")?;
        let marker_end = rest.find(']')?;
        let marker = &rest[..marker_end];
        let variant = match marker.to_ascii_uppercase().as_str() {
            "NOTE" => Self::Note,
            "TIP" => Self::Tip,
            "IMPORTANT" => Self::Important,
            "WARNING" => Self::Warning,
            "CAUTION" => Self::Caution,
            _ => return None,
        };
        let title = rest[marker_end + 1..].trim_start().to_string();
        Some((variant, title))
    }

    /// 生成提示框头 Markdown。
    pub fn heading_markdown(self, title_to_markdown: &str) -> String {
        if title_to_markdown.trim().is_empty() {
            format!("[!{}]", self.marker())
        } else {
            format!("[!{}] {}", self.marker(), title_to_markdown)
        }
    }

    /// 转义普通引用头。
    pub fn escape_quote_header(title_to_markdown: &str) -> String {
        let mut lines = title_to_markdown.splitn(2, '\n');
        let first = lines.next().unwrap_or_default();
        let rest = lines.next();
        let escaped_first = if Self::parse_heading(first).is_some() {
            format!("\\{first}")
        } else {
            first.to_string()
        };
        match rest {
            Some(rest) => format!("{escaped_first}\n{rest}"),
            None => escaped_first,
        }
    }
}

/// 块的语义类型，决定其 Markdown 语法与视觉渲染。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    /// 带行内格式的普通段落。
    Paragraph,
    /// 水平分隔线。
    Separator,
    /// ATX 或 Setext 标题，带 CommonMark 标题级别。
    Heading { level: u8 },
    /// 无序列表项。
    BulletedListItem,
    /// 任务列表项，带勾选状态。
    TaskListItem { checked: bool },
    /// 有序列表项；序列化使用规范点标记。
    NumberedListItem,
    /// 引用块容器。
    Quote,
    /// GitHub 风格的提醒/提示框容器。
    Callout(CalloutVariant),
    /// 脚注定义容器。
    FootnoteDefinition,
    /// 原生管道表格块。
    Table,
    /// 围栏代码块，带可选语言信息串。
    CodeBlock { language: Option<SharedString> },
    /// 可见 HTML 注释块，按原始注释文本保留。
    Comment,
    /// 经原生界面语义元素渲染的安全行内 HTML。
    HtmlBlock,
    /// 用 LaTeX 管线渲染的展示数学块。
    MathBlock,
    /// 渲染为 SVG 的 Mermaid 围栏块。
    MermaidBlock,
    /// 原生运行时子集之外的语法的纯 Markdown 回退。
    RawMarkdown,
}

/// 从围栏代码块解析出的起始围栏。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceOpening {
    /// 围栏字符，反引号或波浪线。
    pub ch: char,
    /// 起始围栏长度。
    pub len: usize,
    /// 起始围栏后的可选语言/信息串。
    pub language: Option<SharedString>,
}

impl BlockKind {
    /// 当前运行时树中该类型块是否可拥有子块。
    pub fn supports_children(&self) -> bool {
        self.is_list_item() || self.is_quote_container() || self.is_footnote_definition()
    }

    /// 是否为列表项。
    pub fn is_list_item(&self) -> bool {
        matches!(
            self,
            Self::BulletedListItem | Self::TaskListItem { .. } | Self::NumberedListItem
        )
    }

    /// 是否为有序列表项。
    pub fn is_numbered_list_item(&self) -> bool {
        matches!(self, Self::NumberedListItem)
    }

    /// 是否为任务列表项。
    pub fn is_task_list_item(&self) -> bool {
        matches!(self, Self::TaskListItem { .. })
    }

    /// 是否为代码块。
    pub fn is_code_block(&self) -> bool {
        matches!(self, Self::CodeBlock { .. })
    }

    /// 以该类型块为目标时右键“插入表格”是否合理。原子/结构块（表格、代码、数学公式等）渲染为自包含部件，在其中插入表格没有意义，因此排除。
    pub fn allows_table_insert_from_context(&self) -> bool {
        !matches!(
            self,
            Self::Table
                | Self::CodeBlock { .. }
                | Self::MathBlock
                | Self::MermaidBlock
                | Self::HtmlBlock
                | Self::Comment
                | Self::RawMarkdown
        )
    }

    /// 是否为引用容器。
    pub fn is_quote_container(&self) -> bool {
        matches!(self, Self::Quote | Self::Callout(_))
    }

    /// 列表项标记的正文列宽：`- ` 为 2；`1. ` 为数字位数 + 2（`10. ` 为 4）。
    /// 嵌套子块的序列化缩进以此为步长（CommonMark：子块需缩进到父项正文列）。
    pub fn marker_content_columns(&self, ordinal: Option<usize>) -> usize {
        match self {
            Self::NumberedListItem => ordinal.unwrap_or(1).to_string().len() + 2,
            _ => 2,
        }
    }

    /// 渲染为自包含部件且其后无光标位置的块。在渲染文档末尾需要补一个尾段落，让以渲染为主的用户能继续输入，而不必切到源码模式。
    pub fn is_atomic_block(&self) -> bool {
        matches!(
            self,
            Self::Separator
                | Self::Table
                | Self::CodeBlock { .. }
                | Self::MathBlock
                | Self::MermaidBlock
                | Self::HtmlBlock
                | Self::Comment
                | Self::RawMarkdown
        )
    }

    /// 是否为提示框。
    pub fn is_callout(&self) -> bool {
        matches!(self, Self::Callout(_))
    }

    /// 按多行纯文本编辑并渲染为自包含部件的块（代码、数学公式、HTML、mermaid、注释、纯 Markdown）。用 `Down` 或 `Ctrl/Cmd+Enter` 向下退出时下方需要一行落点。
    pub fn is_multiline_text_block(&self) -> bool {
        self.is_code_block()
            || matches!(
                self,
                Self::MathBlock
                    | Self::HtmlBlock
                    | Self::MermaidBlock
                    | Self::Comment
                    | Self::RawMarkdown
            )
    }

    /// 是否为脚注定义。
    pub fn is_footnote_definition(&self) -> bool {
        matches!(self, Self::FootnoteDefinition)
    }

    /// 返回提示框变体。
    pub fn callout_variant(&self) -> Option<CalloutVariant> {
        match self {
            Self::Callout(variant) => Some(*variant),
            _ => None,
        }
    }

    /// 是否为分隔线。
    pub fn is_separator(&self) -> bool {
        matches!(self, Self::Separator)
    }

    /// 是否可嵌套在指定父块下。
    pub fn nests_under(&self, parent: &Self) -> bool {
        if !parent.is_list_item() {
            return false;
        }

        self.is_list_item()
            || matches!(
                self,
                Self::Paragraph
                    | Self::Quote
                    | Self::Callout(_)
                    | Self::FootnoteDefinition
                    | Self::Table
                    | Self::CodeBlock { .. }
                    | Self::Comment
                    | Self::HtmlBlock
                    | Self::MathBlock
                    | Self::MermaidBlock
                    | Self::RawMarkdown
            )
    }

    /// 返回换行时的兄弟块类型。
    pub fn sibling_kind_after_newline(&self) -> Self {
        if matches!(self, Self::TaskListItem { .. }) {
            Self::TaskListItem { checked: false }
        } else if self.is_list_item() || self.is_quote_container() {
            self.clone()
        } else {
            Self::Paragraph
        }
    }

    /// 从用户输入实时识别 Markdown 前缀，返回对应的 [`BlockKind`] 与应剥离的前缀字符数。
    pub fn detect_block_shortcut(value: &str) -> Option<(Self, usize)> {
        if value.starts_with("###### ") {
            Some((Self::Heading { level: 6 }, 7))
        } else if value.starts_with("##### ") {
            Some((Self::Heading { level: 5 }, 6))
        } else if value.starts_with("#### ") {
            Some((Self::Heading { level: 4 }, 5))
        } else if value.starts_with("### ") {
            Some((Self::Heading { level: 3 }, 4))
        } else if value.starts_with("## ") {
            Some((Self::Heading { level: 2 }, 3))
        } else if value.starts_with("# ") {
            Some((Self::Heading { level: 1 }, 2))
        } else if let Some((checked, prefix_len)) = Self::parse_task_list_shortcut(value) {
            Some((Self::TaskListItem { checked }, prefix_len))
        } else if value.starts_with("* ") || value.starts_with("+ ") || value.starts_with("- ") {
            Some((Self::BulletedListItem, 2))
        } else if let Some(prefix_len) = Self::numbered_list_shortcut_prefix_len(value) {
            Some((Self::NumberedListItem, prefix_len))
        } else if value.starts_with("> ") {
            Some((Self::Quote, 2))
        } else {
            None
        }
    }

    /// 解析 ATX 标题行。
    pub fn parse_heading_line(line: &str) -> Option<(u8, String)> {
        let trimmed_end = line.trim_end();
        let leading_spaces = trimmed_end.bytes().take_while(|b| *b == b' ').count();
        if leading_spaces > 3 {
            return None;
        }

        let rest = &trimmed_end[leading_spaces..];
        let level = rest.bytes().take_while(|b| *b == b'#').count();
        if !(1..=6).contains(&level) {
            return None;
        }

        let content = rest[level..].strip_prefix(' ')?;
        let mut content = content.trim_end().to_string();
        if let Some(closing_hash_start) = content.rfind(' ')
            && content[closing_hash_start + 1..]
                .chars()
                .all(|ch| ch == '#')
        {
            content.truncate(closing_hash_start);
            content = content.trim_end().to_string();
        }

        Some((level as u8, content))
    }

    /// 解析 Setext 下划线。
    pub fn parse_setext_rule(line: &str) -> Option<u8> {
        let trimmed_end = line.trim_end();
        let leading_spaces = trimmed_end.bytes().take_while(|b| *b == b' ').count();
        if leading_spaces > 3 {
            return None;
        }

        let rest = &trimmed_end[leading_spaces..];
        if rest.len() < 3 {
            return None;
        }

        if rest.bytes().all(|b| b == b'=') {
            Some(1)
        } else if rest.bytes().all(|b| b == b'-') {
            Some(2)
        } else {
            None
        }
    }

    /// 解析代码围栏起始行。
    pub fn parse_fence_opening(value: &str) -> Option<FenceOpening> {
        let trimmed = value.trim_end();
        let ch = trimmed.chars().next()?;
        if ch != '`' && ch != '~' {
            return None;
        }

        let len = trimmed.chars().take_while(|&c| c == ch).count();
        if len < 3 {
            return None;
        }

        let rest = &trimmed[ch.len_utf8() * len..];
        if ch == '`' && rest.contains('`') {
            return None;
        }

        let language = rest.trim();
        Some(FenceOpening {
            ch,
            len,
            language: if language.is_empty() {
                None
            } else {
                Some(language.to_string().into())
            },
        })
    }

    /// 解析分隔线。
    pub fn parse_separator(value: &str) -> bool {
        let trimmed_end = value.trim_end();
        let leading_spaces = trimmed_end.bytes().take_while(|b| *b == b' ').count();
        if leading_spaces > 3 {
            return false;
        }

        let rest = &trimmed_end[leading_spaces..];
        let mut marker = None;
        let mut marker_count = 0usize;
        for ch in rest.chars() {
            if ch == ' ' {
                continue;
            }
            if !matches!(ch, '-' | '*' | '_') {
                return false;
            }
            if let Some(existing) = marker {
                if existing != ch {
                    return false;
                }
            } else {
                marker = Some(ch);
            }
            marker_count += 1;
        }

        marker_count >= 3
    }

    /// 解析列表项内容开头的任务列表标记。
    ///
    /// 接受 `[ ]`、`[x]`、`[X]` 形式，项文本前可跟空格或制表符，空标题同样合法。
    pub fn parse_task_prefix(value: &str) -> Option<(bool, usize)> {
        let bytes = value.as_bytes();
        if bytes.len() < 3 || bytes[0] != b'[' || bytes[2] != b']' {
            return None;
        }

        let checked = match bytes[1] {
            b' ' => false,
            b'x' | b'X' => true,
            _ => return None,
        };

        if bytes.len() == 3 {
            return Some((checked, 3));
        }

        if matches!(bytes[3], b' ' | b'\t') {
            Some((checked, 4))
        } else {
            None
        }
    }

    /// 解析任务列表快捷前缀。
    fn parse_task_list_shortcut(value: &str) -> Option<(bool, usize)> {
        let rest = value.strip_prefix("- ")?;
        let (checked, prefix_len) = Self::parse_task_prefix(rest)?;
        Some((checked, 2 + prefix_len))
    }

    /// 解析有序列表快捷前缀长度。
    fn numbered_list_shortcut_prefix_len(value: &str) -> Option<usize> {
        let digit_len = value.bytes().take_while(|b| b.is_ascii_digit()).count();
        if !(1..=9).contains(&digit_len) {
            return None;
        }

        let marker = *value.as_bytes().get(digit_len)?;
        if !matches!(marker, b'.' | b')') {
            return None;
        }

        let separator = *value.as_bytes().get(digit_len + 1)?;
        matches!(separator, b' ' | b'\t').then_some(digit_len + 2)
    }
}

/// 与编辑器运行时无关的块持久数据。
///
/// 保存块标识、类型、带行内格式的标题与树引用（经 UUID 的父子关系）。按原文保留的 Markdown 把原始来源存入 `raw_fallback`，以便保存/加载无损往返。
#[derive(Debug, Clone)]
pub struct BlockRecord {
    pub id: Uuid,
    pub kind: BlockKind,
    pub title: InlineTextTree,
    pub table: Option<TableModel>,
    pub html: Option<HtmlDocument>,
    pub parent: Option<Uuid>,
    pub content: Vec<Uuid>,
    pub raw_fallback: Option<String>,
    /// 是否为链接/图片引用定义块（`[label]: url`）。渲染视图隐藏，源码视图保留。
    pub reference_definition: bool,
    /// 有序列表项在源码中声明的原始编号（如 `3.`）；交互创建的条目为 `None`，
    /// 由运行时按组内位置从分组起始号续排（起始号同样取自组内首个条目）。
    pub list_number: Option<u64>,
    /// 围栏代码块解析时记录的原文围栏变体（围栏字符与长度，如 `('~', 3)` 代表
    /// `~~~`）。序列化时在仍能安全闭合的前提下原样保留；交互创建的代码块为
    /// `None`，按内容重算标准围栏。
    pub code_fence: Option<(char, usize)>,
    /// 大文档载入时延迟代码高亮：建块阶段不跑 tree-sitter，进入视口时再算。
    pub(crate) defer_code_highlight: bool,
    /// 内容修订号：任何影响序列化结果的变更都递增，供编辑器派生缓存判定失效。
    /// 红线：内容变更必须走 `set_title`/`set_kind`/`set_table`，直接赋值字段会让缓存陈旧。
    content_version: u64,
}

impl BlockRecord {
    /// 创建块记录。
    pub fn new(kind: BlockKind, title: InlineTextTree) -> Self {
        let mut record = Self {
            id: Uuid::new_v4(),
            kind,
            title,
            table: None,
            html: None,
            parent: None,
            content: Vec::new(),
            raw_fallback: None,
            reference_definition: false,
            list_number: None,
            code_fence: None,
            defer_code_highlight: false,
            content_version: 0,
        };
        record.sync_raw_fallback();
        record
    }

    /// 设置有序列表项的原始编号（仅解析载入路径使用）。
    pub fn set_list_number(&mut self, number: Option<u64>) {
        if self.list_number != number {
            self.list_number = number;
            self.content_version = self.content_version.wrapping_add(1);
            crate::components::bump_document_content_epoch();
        }
    }

    /// 创建延迟高亮的代码块记录，用于大文档分批建树。
    pub(crate) fn with_deferred_code_highlight(kind: BlockKind, title: InlineTextTree) -> Self {
        let mut record = Self::new(kind, title);
        record.defer_code_highlight = true;
        record
    }

    /// 当前内容修订号。
    pub fn content_version(&self) -> u64 {
        self.content_version
    }

    /// 用纯文本创建块记录。
    pub fn with_plain_text(kind: BlockKind, text: impl Into<String>) -> Self {
        Self::new(kind, InlineTextTree::plain(text.into()))
    }

    /// 创建段落块。
    pub fn paragraph(text: impl Into<String>) -> Self {
        Self::with_plain_text(BlockKind::Paragraph, text)
    }

    /// 创建纯 Markdown 回退块。
    pub fn raw_markdown(markdown: impl Into<String>) -> Self {
        let markdown = markdown.into();
        let mut record = Self::with_plain_text(BlockKind::RawMarkdown, markdown.clone());
        record.raw_fallback = Some(markdown);
        record
    }

    /// 创建注释块。
    pub fn comment(markdown: impl Into<String>) -> Self {
        let markdown = markdown.into();
        let mut record = Self::with_plain_text(BlockKind::Comment, markdown.clone());
        record.raw_fallback = Some(markdown);
        record
    }

    /// 创建链接/图片引用定义块。
    ///
    /// 这类块不产生可见输出（对齐 GitHub/Typora），但仍按原始 Markdown 保留，
    /// 在源码视图与保存时无损往返。
    pub fn reference_definition(markdown: impl Into<String>) -> Self {
        let markdown = markdown.into();
        let mut record = Self::with_plain_text(BlockKind::RawMarkdown, markdown.clone());
        record.raw_fallback = Some(markdown);
        record.reference_definition = true;
        record
    }

    /// 创建 HTML 块。
    pub fn html(markdown: impl Into<String>) -> Self {
        let markdown = markdown.into();
        let html = parse_html_document(&markdown);
        let mut record = Self::with_plain_text(BlockKind::HtmlBlock, markdown.clone());
        record.html = Some(html);
        record.raw_fallback = Some(markdown);
        record
    }

    /// 创建数学公式块。
    pub fn math(markdown: impl Into<String>) -> Self {
        let markdown = markdown.into();
        let mut record = Self::with_plain_text(BlockKind::MathBlock, markdown.clone());
        record.raw_fallback = Some(markdown);
        record
    }

    /// 创建 Mermaid 块。
    ///
    /// title 只存围栏之间的图表正文（聚焦编辑时展示的文本，与代码块一致），
    /// raw_fallback 保留含围栏的完整原文，未编辑过的块保存时无损往返。
    pub fn mermaid(markdown: impl Into<String>) -> Self {
        let markdown = markdown.into();
        let body = parse_mermaid_fence_source(&markdown)
            .map(|source| source.body)
            .unwrap_or_else(|| markdown.clone());
        let mut record = Self::with_plain_text(BlockKind::MermaidBlock, body);
        record.raw_fallback = Some(markdown);
        record
    }

    /// 创建表格块。
    pub fn table(table: TableModel) -> Self {
        let mut record = Self::new(BlockKind::Table, InlineTextTree::plain(String::new()));
        record.table = Some(table);
        record
    }

    /// 设置块标题。
    ///
    /// 所有内容变更都必须经这三个 setter：它们同时递增块版本号与全局纪元，
    /// 否则派生数据缓存会读到旧内容（见 `editor/derive.rs`）。
    pub fn set_title(&mut self, title: InlineTextTree) {
        self.title = title;
        self.sync_raw_fallback();
        self.content_version = self.content_version.wrapping_add(1);
        crate::components::bump_document_content_epoch();
    }

    /// 设置块类型，并同步原始回退文本与 HTML 解析结果。
    pub fn set_kind(&mut self, kind: BlockKind) {
        self.kind = kind;
        self.sync_raw_fallback();
        self.content_version = self.content_version.wrapping_add(1);
        crate::components::bump_document_content_epoch();
    }

    /// 设置表格模型。
    pub fn set_table(&mut self, table: Option<TableModel>) {
        self.table = table;
        self.content_version = self.content_version.wrapping_add(1);
        crate::components::bump_document_content_epoch();
    }

    /// 把块标题导出为 Markdown：片段样式标记经 [`InlineTextTree::serialize_markdown`] 序列化回分隔符。
    pub fn title_to_markdown(&self) -> String {
        self.title.serialize_markdown()
    }

    /// 该类型块是否把原始文本保存在 `raw_fallback` 中，因其按不透明 Markdown 保留。
    pub fn kind_falls_back_to_raw(&self) -> bool {
        matches!(
            self.kind,
            BlockKind::RawMarkdown
                | BlockKind::Comment
                | BlockKind::HtmlBlock
                | BlockKind::MathBlock
                | BlockKind::MermaidBlock
        )
    }

    /// 把块序列化为单行 Markdown，含嵌套缩进与有序列表序号。按原文保留的块在
    /// 列宽为 0 时输出回退文本。
    ///
    /// `indent_columns` 为该块自身行的缩进列数（不是层级数）：列表项子块的
    /// 步长按父项标记正文列宽累计（见 [`BlockKind::marker_content_columns`]）。
    pub fn markdown_line(&self, indent_columns: usize, list_ordinal: Option<usize>) -> String {
        let indentation = " ".repeat(indent_columns);
        let title_to_markdown = self.title_markdown_for_output();
        match self.kind {
            BlockKind::Paragraph => indent_multiline(
                &escape_leading_block_marker(&title_to_markdown),
                &indentation,
            ),
            BlockKind::Separator => "---".to_string(),
            BlockKind::Heading { level } => {
                format!(
                    "{indentation}{} {title_to_markdown}",
                    "#".repeat(level as usize)
                )
            }
            BlockKind::BulletedListItem => prefixed_multiline(
                &title_to_markdown,
                &format!("{indentation}- "),
                &format!("{indentation}  "),
            ),
            BlockKind::TaskListItem { checked } => prefixed_multiline(
                &title_to_markdown,
                &format!("{indentation}- [{}] ", if checked { "x" } else { " " }),
                &format!("{indentation}      "),
            ),
            BlockKind::NumberedListItem => {
                // 运行时序号（含分组起始号）优先；直连序列化（如复制回退）退回
                // 解析时捕获的原始编号，再退到 1。
                let ordinal = list_ordinal
                    .map(|value| value as u64)
                    .or(self.list_number)
                    .unwrap_or(1);
                prefixed_multiline(
                    &title_to_markdown,
                    &format!("{indentation}{ordinal}. "),
                    &format!("{indentation}   "),
                )
            }
            BlockKind::Quote => prefixed_multiline(
                &CalloutVariant::escape_quote_header(&title_to_markdown),
                &format!("{indentation}> "),
                &format!("{indentation}> "),
            ),
            BlockKind::Callout(variant) => format!(
                "{indentation}> {}",
                variant.heading_markdown(&title_to_markdown)
            ),
            BlockKind::FootnoteDefinition => {
                format!("{indentation}[^{}]: ", self.title.plain_text())
            }
            BlockKind::Table => String::new(),
            BlockKind::CodeBlock { .. } => title_to_markdown,
            BlockKind::RawMarkdown
            | BlockKind::Comment
            | BlockKind::HtmlBlock
            | BlockKind::MathBlock
            | BlockKind::MermaidBlock => {
                if indent_columns == 0 {
                    self.raw_fallback.clone().unwrap_or(title_to_markdown)
                } else {
                    indent_multiline(
                        &self.raw_fallback.clone().unwrap_or(title_to_markdown),
                        &indentation,
                    )
                }
            }
        }
    }

    /// 返回用于输出的标题 Markdown。
    fn title_markdown_for_output(&self) -> String {
        let visible = self.title.plain_text();
        if self.can_present_title_as_standalone_image()
            && parse_standalone_image(&visible).is_some()
        {
            return visible;
        }

        self.title_to_markdown()
    }

    /// 是否可把标题呈现为独立图片。
    fn can_present_title_as_standalone_image(&self) -> bool {
        matches!(
            self.kind,
            BlockKind::Paragraph
                | BlockKind::BulletedListItem
                | BlockKind::NumberedListItem
                | BlockKind::TaskListItem { .. }
        )
    }

    /// 同步原始回退文本。
    fn sync_raw_fallback(&mut self) {
        if self.kind_falls_back_to_raw() {
            self.raw_fallback = if self.kind == BlockKind::MermaidBlock {
                Some(self.rebuilt_mermaid_fallback())
            } else {
                Some(self.title.plain_text().to_string())
            };
            if self.kind == BlockKind::HtmlBlock {
                self.html = self
                    .raw_fallback
                    .as_ref()
                    .map(|raw| parse_html_document(raw));
            }
        } else {
            self.raw_fallback = None;
            self.html = None;
        }
    }

    /// 重建 Mermaid 块的围栏原文：正文取块标题文本，围栏沿用上一份原文的
    /// 起止行（保留 `~~~`、围栏长度与大小写 info 等变体），旧原文解析不出
    /// 围栏时回退标准 ```` ```mermaid ````。仅编辑后经 setter 走到这里；
    /// 构造时由 `BlockRecord::mermaid` 直接写入原始围栏。
    fn rebuilt_mermaid_fallback(&self) -> String {
        let body = self.title.plain_text();
        if let Some(raw) = self.raw_fallback.as_deref()
            && parse_mermaid_fence_source(raw).is_some()
        {
            let open_line = raw.split('\n').next().unwrap_or_default();
            let close_line = raw.split('\n').next_back().unwrap_or_default();
            return format!("{open_line}\n{body}\n{close_line}");
        }
        format!("```mermaid\n{body}\n```")
    }
}

/// 缩进多行文本。
fn indent_multiline(content: &str, indentation: &str) -> String {
    content
        .split('\n')
        .map(|line| format!("{indentation}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 为段落首行转义会被误判为块级标记的首字符。
///
/// 段落文本可能因反斜杠转义而以 `#`、`>`、`-`、`+` 或有序列表标记开头，
/// 序列化时若不还原转义，重新解析会变成标题/引用/列表等块，破坏往返。
fn escape_leading_block_marker(content: &str) -> String {
    match leading_block_marker_escape_index(content) {
        Some(index) => format!("{}\\{}", &content[..index], &content[index..]),
        None => content.to_string(),
    }
}

/// 返回需要在首行插入反斜杠的字节下标（有序列表标记插入在 `.`/`)` 之前）。
fn leading_block_marker_escape_index(content: &str) -> Option<usize> {
    let first = content.chars().next()?;
    if matches!(first, '#' | '>' | '-' | '+') {
        return Some(0);
    }

    let digit_len = content.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digit_len) {
        let marker = *content.as_bytes().get(digit_len)?;
        if matches!(marker, b'.' | b')') {
            let separator = content.as_bytes().get(digit_len + 1);
            if separator.is_none_or(|byte| matches!(byte, b' ' | b'\t')) {
                return Some(digit_len);
            }
        }
    }

    None
}

/// 为多行文本添加首行与后续行前缀。
fn prefixed_multiline(content: &str, first_prefix: &str, continuation_prefix: &str) -> String {
    let mut lines = content.split('\n');
    let mut rendered = String::new();
    if let Some(first) = lines.next() {
        rendered.push_str(first_prefix);
        rendered.push_str(first);
    }

    for line in lines {
        rendered.push('\n');
        rendered.push_str(continuation_prefix);
        rendered.push_str(line);
    }

    rendered
}

/// 从界面剪贴板抽象中提取的图片载荷。
///
/// 文件管理器的复制通常表现为本地路径，图片编辑器或浏览器的位图复制则为编码后的图片字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PastedImageSource {
    ClipboardImage(Image),
    LocalPath(PathBuf),
}

/// 块发给父编辑器的事件，用于请求块自身无法处理的结构变更或焦点转移。
///
/// 编辑器经 `cx.subscribe(&block, Self::on_block_event)` 订阅每个块的这些事件。
#[derive(Debug, Clone)]
pub enum BlockEvent {
    /// 在变更前捕获当前文档状态。
    PrepareUndo { kind: UndoKind },
    /// 块内容或类型变化；编辑器应标记文档为脏并按需滚动以保持块可见。
    Changed,
    /// 用户按下回车；应在该块后用给定尾文本新建块。
    RequestNewline {
        trailing: InlineTextTree,
        source_already_mutated: bool,
    },
    /// 用户在提示框头按下回车；编辑器应确保提示框拥有正文项并把焦点移入其中。
    RequestEnterCalloutBody,
    /// 用户请求在当前引用深度打断引用组。编辑器应在当前深度插入新的空引用组，并按该层 Markdown 要求补齐分隔结构。
    RequestQuoteBreak,
    /// 用户请求退出当前提示框到纯文本块。编辑器应插入结束外层引用组所需的分隔结构，再把焦点放到下方纯段落项。
    RequestCalloutBreak,
    /// 用户在块首按下退格；应把整块内容追加到前一块。
    RequestMergeIntoPrev { content: InlineTextTree },
    /// 检测到多行粘贴；编辑器须把粘贴行拆成独立块，并把首尾文本接到正确位置。
    RequestPasteMultiline {
        leading: InlineTextTree,
        lines: Vec<String>,
        trailing: InlineTextTree,
        split_physical_lines: bool,
    },
    /// 粘贴了图片类剪贴板载荷。编辑器按存储偏好解析，插入图片块或图片文本。
    RequestPasteImage {
        leading: InlineTextTree,
        source: PastedImageSource,
        trailing: InlineTextTree,
    },
    /// 用经聚焦块输入处理器提交的文本替换编辑器级跨块选区。
    RequestReplaceCrossBlockSelection {
        text: String,
        selected_range_relative: Option<Range<usize>>,
        mark_inserted_text: bool,
        undo_kind: UndoKind,
    },
    /// 在渲染编辑中按下 Ctrl/Cmd+A。由编辑器决定本次选中聚焦块还是升级为全部渲染块。
    RequestRenderedSelectAll,
    /// 在列表上下文中按下 Tab；当前一块可收养时增加其嵌套层级。
    RequestIndent,
    /// 在列表上下文中按下 Shift-Tab；把当前块提升一层。
    RequestOutdent,
    /// 嵌套列表项上的退格应先去掉其标记，把它降级为同深度的直接列表子段落。
    RequestDowngradeNestedListItemToChildParagraph,
    /// 切换任务列表项的勾选状态。
    ToggleTaskChecked,
    /// 提示打开被点击的行内链接目标。`prompt_target` 保留展示给用户的原始语法目标，`open_target` 为实际打开的解析后目标。
    RequestOpenLink {
        prompt_target: String,
        open_target: String,
    },
    /// 从渲染的脚注引用跳转到对应的就地脚注定义块。
    RequestJumpToFootnoteDefinition { id: String },
    /// 从就地脚注定义跳回首个引用处。
    RequestJumpToFootnoteBackref { id: String },
    /// 在原生表格单元格间水平移动焦点。
    RequestTableCellMoveHorizontal { delta: i32 },
    /// 在原生表格单元格间垂直移动焦点。
    RequestTableCellMoveVertical { delta: i32 },
    /// 为原生表格追加一空列。
    RequestAppendTableColumn,
    /// 为原生表格追加一空正文行。
    RequestAppendTableRow,
    /// 原生表格轴手柄被指针进入或离开。`hovered` 区分两者，编辑器可忽略相邻手柄接管预览后到达的离开事件。
    RequestTableAxisPreview {
        kind: TableAxisKind,
        index: usize,
        hovered: bool,
    },
    /// 选中原生表格的一行或一列以便批量操作。
    RequestSelectTableAxis { kind: TableAxisKind, index: usize },
    /// 表格轴右键菜单项被点击；编辑器对该表的指定轴执行操作。
    RequestTableAxisOp {
        kind: TableAxisKind,
        index: usize,
        op: TableAxisMenuOp,
    },
    /// 光标到达块顶；把焦点移到上一可见块，并保留期望水平位置。
    RequestFocusPrev { preferred_x: Option<f32> },
    /// 光标到达块底；把焦点移到下一可见块，并保留期望水平位置。
    RequestFocusNext { preferred_x: Option<f32> },
    /// Shift+上/下：按视觉行扩展选区，越过块边界时由编辑器接续。
    RequestSelectVertical { delta: i32, preferred_x: f32 },
    /// Shift+左/右、Shift+Home/End：扩展选区；已有跨块选区时由编辑器接续焦点端点。
    RequestSelectHorizontal {
        motion: HorizontalSelectMotion,
        /// 块内计算出的落点；没有跨块选区时按此就地扩选。
        target: usize,
    },
    /// 把焦点移到上一可见块开头。
    RequestBlockUp,
    /// 把焦点移到下一可见块开头。
    RequestBlockDown,
    /// 删除该块（为空且按下退格/删除键）。
    RequestDelete,
    /// 用户点击该块；通知兄弟块以展示模式重渲染。
    RequestFocus,
}

/// 块内横向扩选动作的语义，供编辑器区分越界方向与按词粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalSelectMotion {
    /// 按字符左移。
    CharLeft,
    /// 按字符右移。
    CharRight,
    /// 按词左移。
    WordLeft,
    /// 按词右移。
    WordRight,
    /// 到块首。
    Home,
    /// 到块尾。
    End,
}

/// 变更前捕获的撤销合并类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoKind {
    /// 可在合并窗口内与相邻输入合并的文本编辑。
    CoalescibleText,
    /// 结构或离散编辑，总是形成独立撤销项。
    NonCoalescible,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_table_insert_excludes_atomic_blocks() {
        assert!(BlockKind::Paragraph.allows_table_insert_from_context());
        assert!(BlockKind::Heading { level: 1 }.allows_table_insert_from_context());
        assert!(BlockKind::Quote.allows_table_insert_from_context());
        assert!(!BlockKind::Table.allows_table_insert_from_context());
        assert!(!BlockKind::CodeBlock { language: None }.allows_table_insert_from_context());
        assert!(!BlockKind::MathBlock.allows_table_insert_from_context());
        assert!(!BlockKind::MermaidBlock.allows_table_insert_from_context());
    }

    #[test]
    fn marker_content_columns_follow_ordinal_width() {
        assert_eq!(BlockKind::BulletedListItem.marker_content_columns(None), 2);
        assert_eq!(
            BlockKind::TaskListItem { checked: false }.marker_content_columns(None),
            2
        );
        assert_eq!(
            BlockKind::NumberedListItem.marker_content_columns(Some(1)),
            3
        );
        assert_eq!(
            BlockKind::NumberedListItem.marker_content_columns(Some(9)),
            3
        );
        assert_eq!(
            BlockKind::NumberedListItem.marker_content_columns(Some(10)),
            4
        );
        assert_eq!(BlockKind::Paragraph.marker_content_columns(None), 2);
    }

    #[test]
    fn mermaid_record_edits_body_and_rebuilds_fence() {
        let record = BlockRecord::mermaid("```mermaid\nflowchart LR\nA --> B\n```");
        // title 只存正文，未编辑时序列化原样保留围栏原文。
        assert_eq!(record.title.plain_text(), "flowchart LR\nA --> B");
        assert_eq!(
            record.markdown_line(0, None),
            "```mermaid\nflowchart LR\nA --> B\n```"
        );

        let mut record = record;
        record.set_title(InlineTextTree::plain("flowchart LR\nX --> Y"));
        assert_eq!(
            record.markdown_line(0, None),
            "```mermaid\nflowchart LR\nX --> Y\n```"
        );
    }

    #[test]
    fn mermaid_edit_preserves_fence_variants() {
        // `~~~` 围栏与大小写 info 等变体在编辑后仍按原样式重建。
        let mut record = BlockRecord::mermaid("~~~MMD\nflowchart LR\nA --> B\n~~~");
        assert_eq!(
            record.markdown_line(0, None),
            "~~~MMD\nflowchart LR\nA --> B\n~~~"
        );
        record.set_title(InlineTextTree::plain("pie\ntitle 占比"));
        assert_eq!(
            record.markdown_line(0, None),
            "~~~MMD\npie\ntitle 占比\n~~~"
        );
    }

    #[test]
    fn paragraph_serialization_escapes_leading_block_markers() {
        assert_eq!(
            BlockRecord::paragraph("# 标题符号").markdown_line(0, None),
            "\\# 标题符号"
        );
        assert_eq!(
            BlockRecord::paragraph("> 引用符号").markdown_line(0, None),
            "\\> 引用符号"
        );
        assert_eq!(
            BlockRecord::paragraph("- 列表符号").markdown_line(0, None),
            "\\- 列表符号"
        );
        assert_eq!(
            BlockRecord::paragraph("1. 有序项").markdown_line(0, None),
            "1\\. 有序项"
        );
        // 普通文本与 10 位以上数字开头不受影响。
        assert_eq!(
            BlockRecord::paragraph("普通段落").markdown_line(0, None),
            "普通段落"
        );
        assert_eq!(
            BlockRecord::paragraph("1234567890. 文本").markdown_line(0, None),
            "1234567890. 文本"
        );
    }

    #[test]
    fn detects_markdown_shortcuts() {
        assert_eq!(
            BlockKind::detect_block_shortcut("- item"),
            Some((BlockKind::BulletedListItem, 2))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("1. item"),
            Some((BlockKind::NumberedListItem, 3))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("12. item"),
            Some((BlockKind::NumberedListItem, 4))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("1) item"),
            Some((BlockKind::NumberedListItem, 3))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("12)\titem"),
            Some((BlockKind::NumberedListItem, 4))
        );
        assert_eq!(BlockKind::detect_block_shortcut("1234567890) item"), None);
        assert_eq!(
            BlockKind::detect_block_shortcut("# heading"),
            Some((BlockKind::Heading { level: 1 }, 2))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("## heading"),
            Some((BlockKind::Heading { level: 2 }, 3))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("### heading"),
            Some((BlockKind::Heading { level: 3 }, 4))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("#### heading"),
            Some((BlockKind::Heading { level: 4 }, 5))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("##### heading"),
            Some((BlockKind::Heading { level: 5 }, 6))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("###### heading"),
            Some((BlockKind::Heading { level: 6 }, 7))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("- [ ] task"),
            Some((BlockKind::TaskListItem { checked: false }, 6))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("- [x] task"),
            Some((BlockKind::TaskListItem { checked: true }, 6))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("* item"),
            Some((BlockKind::BulletedListItem, 2))
        );
        assert_eq!(
            BlockKind::detect_block_shortcut("+ item"),
            Some((BlockKind::BulletedListItem, 2))
        );
        assert_eq!(BlockKind::detect_block_shortcut("#no-space"), None);
        assert_eq!(BlockKind::detect_block_shortcut("```"), None);
        assert_eq!(
            BlockKind::detect_block_shortcut("> quote"),
            Some((BlockKind::Quote, 2))
        );
        assert_eq!(BlockKind::detect_block_shortcut(">no-space"), None);
    }

    #[test]
    fn parses_separator_lines() {
        assert!(BlockKind::parse_separator("---"));
        assert!(BlockKind::parse_separator("----"));
        assert!(BlockKind::parse_separator("***"));
        assert!(BlockKind::parse_separator("_ _ _"));
        assert!(BlockKind::parse_separator(" - - - "));
        assert!(!BlockKind::parse_separator("--"));
        assert!(BlockKind::parse_separator(" ---"));
        assert!(!BlockKind::parse_separator("---x"));
    }

    #[test]
    fn parses_code_fence_openings() {
        assert_eq!(
            BlockKind::parse_fence_opening("```rust"),
            Some(FenceOpening {
                ch: '`',
                len: 3,
                language: Some("rust".into()),
            })
        );
        assert_eq!(
            BlockKind::parse_fence_opening("~~~ts"),
            Some(FenceOpening {
                ch: '~',
                len: 3,
                language: Some("ts".into()),
            })
        );
        assert_eq!(
            BlockKind::parse_fence_opening("```"),
            Some(FenceOpening {
                ch: '`',
                len: 3,
                language: None,
            })
        );
        assert_eq!(BlockKind::parse_fence_opening("``"), None);
        assert_eq!(BlockKind::parse_fence_opening("```ru`st"), None);
    }

    #[test]
    fn parses_task_list_item_prefixes() {
        assert_eq!(BlockKind::parse_task_prefix("[ ] a"), Some((false, 4)));
        assert_eq!(BlockKind::parse_task_prefix("[x] a"), Some((true, 4)));
        assert_eq!(BlockKind::parse_task_prefix("[X] a"), Some((true, 4)));
        assert_eq!(BlockKind::parse_task_prefix("[a] a"), None);
    }

    #[test]
    fn serializes_supported_block_kinds() {
        let list = BlockRecord::new(
            BlockKind::BulletedListItem,
            InlineTextTree::from_markdown("*item*"),
        );
        let numbered = BlockRecord::new(
            BlockKind::NumberedListItem,
            InlineTextTree::from_markdown("step"),
        );
        let task = BlockRecord::new(
            BlockKind::TaskListItem { checked: true },
            InlineTextTree::from_markdown("done"),
        );
        let heading = BlockRecord::new(
            BlockKind::Heading { level: 2 },
            InlineTextTree::from_markdown("**title**"),
        );
        let quote = BlockRecord::new(BlockKind::Quote, InlineTextTree::plain("quoted text"));
        let paragraph = BlockRecord::paragraph("plain");
        let comment = BlockRecord::comment("<!--\ncomment\n-->");

        assert_eq!(list.markdown_line(0, None), "- *item*");
        assert_eq!(list.markdown_line(4, None), "    - *item*");
        assert_eq!(task.markdown_line(0, None), "- [x] done");
        assert_eq!(task.markdown_line(4, None), "    - [x] done");
        assert_eq!(numbered.markdown_line(0, Some(3)), "3. step");
        assert_eq!(numbered.markdown_line(4, Some(12)), "    12. step");
        assert_eq!(heading.markdown_line(0, None), "## **title**");
        assert_eq!(quote.markdown_line(0, None), "> quoted text");
        assert_eq!(quote.markdown_line(4, None), "    > quoted text");
        assert_eq!(paragraph.markdown_line(2, None), "  plain");
        assert_eq!(comment.markdown_line(0, None), "<!--\ncomment\n-->");
        assert_eq!(comment.markdown_line(2, None), "  <!--\n  comment\n  -->");
    }

    #[test]
    fn standalone_image_markdown_line_preserves_underscores() {
        let markdown = "![1.1_进制转换例子](./NetworkEngineerSummer.assets/1.1_进制转换例子.jpg)";
        let paragraph = BlockRecord::paragraph(markdown);

        assert_eq!(paragraph.markdown_line(0, None), markdown);
    }

    #[test]
    fn quote_serializes_back_to_markdown() {
        let record = BlockRecord::new(BlockKind::Quote, InlineTextTree::plain("text"));
        let line = record.markdown_line(0, None);
        assert_eq!(line, "> text");
    }

    #[test]
    fn parses_h2_and_h3_lines_with_correct_levels() {
        let h2 = BlockKind::parse_heading_line("## hello");
        assert_eq!(h2, Some((2, "hello".to_string())));

        let h3 = BlockKind::parse_heading_line("### hello");
        assert_eq!(h3, Some((3, "hello".to_string())));
    }

    #[test]
    fn parses_atx_headings_with_closing_hashes_and_setext_underlines() {
        let atx = BlockKind::parse_heading_line("  ### title ######");
        assert_eq!(atx, Some((3, "title".to_string())));

        assert_eq!(BlockKind::parse_setext_rule("==="), Some(1));
        assert_eq!(BlockKind::parse_setext_rule("---"), Some(2));
        assert_eq!(BlockKind::parse_setext_rule("- - -"), None);
    }

    #[test]
    fn code_block_kind_stores_language() {
        let kind = BlockKind::CodeBlock {
            language: Some(SharedString::from("rust")),
        };
        assert!(kind.is_code_block());
        assert!(!kind.is_list_item());

        let no_lang = BlockKind::CodeBlock { language: None };
        assert!(no_lang.is_code_block());
    }

    #[test]
    fn code_block_markdown_line_returns_plain_content() {
        let record = BlockRecord::new(
            BlockKind::CodeBlock {
                language: Some("rust".into()),
            },
            InlineTextTree::plain("let x = 1;\nprintln!(\"hi\");"),
        );
        // markdown_line 仅返回裸内容；围栏由持久层补上。
        let line = record.markdown_line(0, None);
        assert_eq!(line, "let x = 1;\nprintln!(\"hi\");");
    }

    #[test]
    fn separator_markdown_line_round_trips() {
        let record = BlockRecord::new(BlockKind::Separator, InlineTextTree::plain(String::new()));
        assert_eq!(record.markdown_line(0, None), "---");
        assert!(BlockKind::parse_separator("---"));
    }

    #[test]
    fn task_list_serializes_canonical_markdown() {
        let record = BlockRecord::new(
            BlockKind::TaskListItem { checked: false },
            InlineTextTree::plain("todo"),
        );
        assert_eq!(record.markdown_line(0, None), "- [ ] todo");
    }
}
