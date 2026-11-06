use super::{
    InlineFragment, InlineInsertionAttributes, InlineLinkHit, InlineMathDelimiter, InlineScript,
    InlineStyle, InlineStyleFlag, InlineTextTree, LinkReferenceDefinitions,
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

/// 概括一段 Markdown 解析后的加粗/斜体分布，样式标记紧跟其作用的文本：
/// `foo[I] bar` 表示 `foo` 斜体、` bar` 纯文本，`ab[BI]` 表示既粗又斜。
///
/// 按逐字节样式重新聚合，而不是直接罗列片段：`***x***` 这类两层嵌套会
/// 产出相邻的同样式片段，逐片段输出会把同一个游程拆成两段。
fn emphasis_outline(source: &str) -> String {
    let tree = InlineTextTree::from_markdown(source);
    let text = tree.plain_text();
    let mut bold = vec![false; text.len()];
    let mut italic = vec![false; text.len()];
    for span in tree.render_cache().spans() {
        assert!(
            !span.style.code
                && !span.style.underline
                && !span.style.strikethrough
                && span.style.script == InlineScript::Normal,
            "强调用例只应涉及加粗/斜体：{source:?}"
        );
        for offset in span.range.clone() {
            bold[offset] = span.style.bold;
            italic[offset] = span.style.italic;
        }
    }

    let mut runs = Vec::new();
    let mut index = 0usize;
    while index < text.len() {
        let flags = (bold[index], italic[index]);
        let mut end = index;
        while end < text.len() && (bold[end], italic[end]) == flags {
            end += 1;
        }
        // 样式区间始终按字符边界对齐，同一样式游程不会在 UTF-8 字符中间断开。
        runs.push((text[index..end].to_string(), flags.0, flags.1));
        index = end;
    }
    render_runs(&runs)
}

/// 把「文本 + 加粗/斜体」游程渲染成轮廓字符串，相邻同样式游程合并，
/// 样式标记紧跟其作用的文本。
fn render_runs(runs: &[(String, bool, bool)]) -> String {
    let mut outline = String::new();
    let mut current: Option<(bool, bool)> = None;
    for (text, bold, italic) in runs {
        let flags = (*bold, *italic);
        if current != Some(flags) {
            if let Some(previous) = current {
                outline.push_str(style_marker(previous));
            }
            current = Some(flags);
        }
        outline.push_str(text);
    }
    if let Some(flags) = current {
        outline.push_str(style_marker(flags));
    }
    outline
}

/// 样式游程的轮廓标记。
fn style_marker(flags: (bool, bool)) -> &'static str {
    match flags {
        (true, true) => "[BI]",
        (true, false) => "[B]",
        (false, true) => "[I]",
        (false, false) => "",
    }
}

/// 规范基线：交给 CommonMark 的成熟实现（本仓已依赖的 `pulldown-cmark`）解析，
/// 压成与 `emphasis_outline` 同格式的强调轮廓，这样表驱动测试不必写死期望字符串。
///
/// 用例只放纯文本与强调标记，出现代码、链接、HTML 等其他事件立即失败，
/// 免得基线悄悄变成在比对别的语法。
fn spec_outline(source: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let mut bold = false;
    let mut italic = false;
    let mut runs = Vec::new();
    for event in Parser::new_ext(source, Options::empty()) {
        match event {
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            // 单行用例只会被包成一段，段落本身不带样式。
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {}
            Event::Text(text) => {
                if !text.is_empty() {
                    runs.push((text.to_string(), bold, italic));
                }
            }
            other => panic!("规范基线用例只应涉及强调与纯文本：{source:?} -> {other:?}"),
        }
    }
    render_runs(&runs)
}

#[test]
fn commonmark_emphasis_matches_spec() {
    // CommonMark §6.2「强调与加粗强调」：成对分隔符、侧翼规则、规则 3、词内下划线。
    // 期望值直接取规范实现的解析结果，实现偏差由本表当场暴露。
    for source in [
        // 基本成对形式与长度分档
        "*foo bar*",
        "_foo bar_",
        "**foo bar**",
        "__foo bar__",
        "***foo bar***",
        "___foo bar___",
        // 起始符后紧跟空白、结束符前紧跟空白 → 不是左侧/右侧翼，保持字面量
        // （行首 `* ` 会被规范判成列表，用例一律加前缀避开块级语法）
        "a * foo bar*",
        "*foo bar *",
        "**foo bar **",
        // 词内强调：`*` 允许，`_` 只在整个游程都不贴词内时成立
        "*foo*bar",
        "foo*bar*",
        "foo*bar*baz",
        "_foo_bar_",
        "foo_bar_baz",
        "aa_bb_cc",
        "aa__bb__cc",
        "__foo__bar",
        "foo__bar__baz",
        "___foo__bar",
        // 分隔符长度不一致的成对与未配对
        "*foo**",
        "*foo***",
        "**",
        "*foo",
        "**foo",
        // 嵌套
        "**a *b* c**",
        "*a **b** c*",
        "**(*foo*)**",
        "*_foo_*",
        "_*foo*_",
        // 标点相邻
        "*(foo)*",
        "**(foo)**",
        "\"*foo*\"",
        // 规则 3：开闭长度之和为 3 的倍数且并非全为 3 的倍数 → 不能配对
        "*foo**bar**baz*",
        "***foo****",
        // 空内容
        "**_**",
        "__*__",
        // 连续同标记游程
        "***foo***bar***baz***",
    ] {
        assert_eq!(
            emphasis_outline(source),
            spec_outline(source),
            "source: {source:?}"
        );
    }
}

#[test]
fn commonmark_emphasis_documented_deviations() {
    // 与规范实现的已知差异，逐条写明原因，改动解析器时不得静默扩大这张表。
    // 期望值是实测输出（`emphasis_outline` 的格式），不是规范结果。
    for (source, expected, reason) in [
        (
            "a ** foo bar**",
            "a * foo bar[I]*",
            "`*` 的起始判定是简化的「后一字符非空白」，双游程会被当成单游程 + 字面量",
        ),
        (
            "a*\"foo\"*",
            "a\"foo\"[I]",
            "`*` 的结束判定是简化的「前一字符非空白」，少了规范的右侧翼限制",
        ),
        (
            "**foo*",
            "**foo*",
            "未闭合的多字符起始符整体按字面保留，保证加粗意图不因漏写一半标记而丢失",
        ),
        (
            "***foo**",
            "**foo[I]*",
            "同上：外层 `**` 闭不上时退回字面量，内层按 `*` 成对",
        ),
        (
            "***a *b* c***",
            "a b c[BI]",
            "同标记相邻游程用递归贪心配对，规范的分隔符栈会把尾游程拆成斜体 + 加粗",
        ),
        (
            "*(**foo**)*",
            "(foo)[I]",
            "同上：外层斜体区间内的加粗游程被外层一并吞掉",
        ),
        (
            "__a__b__",
            "__a_b[I]_",
            "下划线游程按 2 + 1 拆分尝试，规范把整个游程当作可拆分的分隔符栈结点",
        ),
        (
            "*(*foo*)*",
            "([I]foo)[I]",
            "同标记嵌套的配对顺序差异，视觉样式相同但样式边界不同",
        ),
    ] {
        assert_eq!(
            emphasis_outline(source),
            expected,
            "source: {source:?} 偏差原因：{reason}"
        );
        assert_ne!(
            emphasis_outline(source),
            spec_outline(source),
            "source: {source:?} 已与规范一致，应从偏差表移入基线表"
        );
    }
}

#[test]
fn unmatched_emphasis_runs_do_not_duplicate_text() {
    // 回归：预扫描曾比递归闭包判定少查侧翼规则，且递归闭不上时外层还会重放
    // 整段 token，两者叠加让未配对的下划线游程写出重复文本，保存即损坏文档。
    for source in [
        "__foo__bar",
        "foo__bar__baz",
        "x__y__z__w",
        "a__b__c",
        "_a__b_c",
        "__a__b__",
        "***foo**",
        "***foo***bar***baz***",
        "*a**b*c",
        "a*b**c*d",
    ] {
        let tree = InlineTextTree::from_markdown(source);
        let plain = tree.plain_text();

        // 纯强调输入的正文只可能去掉成对标记，绝不会凭空变长。
        assert!(
            plain.len() <= source.len(),
            "source: {source:?} plain: {plain:?}"
        );

        // 写出再读回必须得到同一文本：重复内容会在往返里继续膨胀或漂移。
        let escaped = tree.serialize_markdown();
        let round_trip = InlineTextTree::from_markdown(&escaped).plain_text();
        assert_eq!(
            round_trip, plain,
            "source: {source:?} serialize: {escaped:?}"
        );
    }
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
                .all(|span| !span.style.italic && !span.style.bold && !span.style.strikethrough),
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
    let result = tree.replace_plain_range(0..0, "**bold**", InlineInsertionAttributes::default());

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
    let result =
        tree.replace_plain_range_raw(5..5, "**`<u>x</u>`**", InlineInsertionAttributes::default());

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
    let definitions =
        super::super::link::parse_link_reference_definitions("[ref links]: https://example.com");
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
    let markdown = include_str!("../../../../docs/render-sample.md");
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
    let tree = InlineTextTree::from_markdown_with_link_references("![cover][img]", &definitions);

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
