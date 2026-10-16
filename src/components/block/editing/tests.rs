use std::ops::Range;
use std::sync::Arc;

use super::projection::{
    expanded_display_cursor_offset_for_clean, expanded_display_offset_for_clean,
};
use crate::components::markdown::highlight::HighlightLanguage;
use crate::components::markdown::inline::{
    InlineFragment, InlineInsertionAttributes, InlineLinkHit, InlineScript, InlineStyle,
    InlineTextTree,
};
use crate::components::markdown::link::parse_link_reference_definitions;
use crate::components::{
    Block, BlockKind, BlockRecord, CellPosition, DeleteBack, IndentBlock, Newline,
};
use crate::i18n::I18nManager;
use rgpui::{
    AppContext, EntityInputHandler, Modifiers, MouseButton, MouseMoveEvent, MouseUpEvent,
    TestAppContext, point, px,
};

fn assert_only_code_range(block: &Block, expected: Range<usize>) {
    let code_ranges = block
        .inline_spans()
        .iter()
        .filter(|span| span.style.code)
        .map(|span| span.range.clone())
        .collect::<Vec<_>>();
    assert_eq!(code_ranges, vec![expected]);
}

#[rgpui::test]
async fn unicode_selection_and_replacement_stay_on_scalar_boundaries(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("*测试")));

    cx.update(|_window, cx| {
        block.update(cx, |block, block_cx| {
            for offset in 0..=block.rendered_text().len() + 1 {
                block.move_to(offset, block_cx);
                assert!(block.rendered_text().is_char_boundary(block.caret_offset()));
                assert!(
                    block
                        .rendered_text()
                        .is_char_boundary(block.previous_boundary(offset))
                );
                assert!(
                    block
                        .rendered_text()
                        .is_char_boundary(block.next_boundary(offset))
                );
                block.select_to(offset, block_cx);
                assert!(
                    block
                        .rendered_text()
                        .get(block.selected_range.clone())
                        .is_some()
                );
            }
            block.replace_text_in_visible_range(2..5, "X", None, false, block_cx);
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "*X");
        assert!(block.rendered_text().is_char_boundary(block.caret_offset()));
    });
}

#[rgpui::test]
async fn unicode_ime_range_conversion_preserves_surrogate_and_combining_edges(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("a😀e\u{301}")));

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.range_from_utf16(&(2..2)), 1..1);
        assert_eq!(block.range_from_utf16(&(1..2)), 1..5);
        assert_eq!(block.range_from_utf16(&(4..5)), 6..8);
        assert_eq!(block.range_to_utf16(&(6..8)), 4..5);
    });
}

#[rgpui::test]
async fn ime_cancel_removes_only_marked_text_after_cjk_content(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("前文")));

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.move_to("前文".len(), block_cx);
            <Block as EntityInputHandler>::replace_and_mark_text_in_range(
                block,
                None,
                "ni",
                Some(2..2),
                window,
                block_cx,
            );
            assert_eq!(block.rendered_text(), "前文ni");
            assert_eq!(block.marked_range, Some(6..8));
            <Block as EntityInputHandler>::replace_text_in_range(block, None, "", window, block_cx);
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "前文");
        assert_eq!(block.selected_range, 6..6);
        assert_eq!(block.marked_range, None);
    });
}

#[rgpui::test]
async fn projected_unicode_footnote_maps_only_to_source_boundaries(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let mut title = InlineTextTree::from_markdown("[^测试]");
    title.apply_footnote_reference_state(|_| Some((1, 0)));
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::new(BlockKind::Paragraph, title)));

    cx.update(|_window, cx| {
        block.update(cx, |block, block_cx| {
            block.move_to(0, block_cx);
            block.refresh_inline_projection_for_focus(true);
            assert_eq!(block.rendered_text(), "[^测试]");
            let markdown = block.record.title.serialize_markdown();
            for offset in 0..=block.rendered_text().len() {
                let clean = block.current_to_clean_offset(offset);
                assert!(block.render_cache.plain_text().is_char_boundary(clean));
                let source_range = block.current_range_to_markdown_range(offset..offset);
                assert!(markdown.get(source_range).is_some());
            }
        });
    });
}

#[rgpui::test]
async fn footnote_range_mapping_stays_on_source_boundaries(cx: &mut TestAppContext) {
    // 回归：id 含多字节字符、序号为两位上标（≥12）时，脚注的整数比例映射会把
    // 展示偏移映到汉字内部；选区跨过脚注（不整段覆盖，走一般分支）须归一化到
    // 源码字符边界，否则调用方按源码切片会 panic。
    let cx = cx.add_empty_window();
    let mut title = InlineTextTree::from_markdown("前[^中]后");
    title.apply_footnote_reference_state(|_| Some((12, 0)));
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::new(BlockKind::Paragraph, title)));

    cx.update(|_window, cx| {
        block.update(cx, |block, block_cx| {
            block.move_to(0, block_cx);
            block.refresh_inline_projection_for_focus(true);
            let markdown = block.record.title.serialize_markdown();
            let display = block.rendered_text().to_string();
            for start in 0..=display.len() {
                if !display.is_char_boundary(start) {
                    continue;
                }
                for end in start..=display.len() {
                    if !display.is_char_boundary(end) {
                        continue;
                    }
                    let mapped = block.current_range_to_markdown_range(start..end);
                    assert!(
                        markdown.get(mapped.clone()).is_some(),
                        "展示 {start}..{end} → 源码 {mapped:?} 不在字符边界"
                    );
                }
            }
        });
    });
}

#[rgpui::test]
async fn projected_unicode_link_maps_only_to_source_boundaries(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("前[中文标签😀](https://例え.com/路径)后"),
            ),
        )
    });

    cx.update(|_window, cx| {
        block.update(cx, |block, block_cx| {
            let mut saw_expanded_link = false;
            let clean_len = block.render_cache.plain_text().len();
            for caret in 0..=clean_len {
                block.move_to(caret, block_cx);
                block.refresh_inline_projection_for_focus(true);
                let markdown = block.record.title.serialize_markdown();
                let display = block.rendered_text().to_string();
                saw_expanded_link |= display.contains("https://例え.com/路径");
                for start in 0..=display.len() {
                    for end in start..=display.len() {
                        let mapped = block.current_range_to_markdown_range(start..end);
                        assert!(
                            markdown.get(mapped.clone()).is_some(),
                            "光标 {caret}，展示 {start}..{end} 映射到源码 {mapped:?} 不在字符边界"
                        );
                    }
                }
                for start in 0..=markdown.len() {
                    for end in start..=markdown.len() {
                        let mapped = block.markdown_range_to_current_range(start..end);
                        assert!(
                            display.get(mapped.clone()).is_some(),
                            "光标 {caret}，源码 {start}..{end} 映射到展示 {mapped:?} 不在字符边界"
                        );
                    }
                }
            }
            assert!(saw_expanded_link, "链接投影从未展开，测试未覆盖目标分支");
        });
    });
}

#[rgpui::test]
async fn inline_projection_mapping_stays_on_scalar_boundaries_for_unicode_constructs(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();
    const FIXTURES: &[&str] = &[
        "前[中文标签😀](https://例え.com/路径)后",
        "文本 `code😀` 尾",
        "![图片😀](https://例え.com/i.png) 尾",
        "$E = mc^2$ 与 中文😀",
        "粗体 **重点😀** 与 *斜体*",
        "转义 \\* 与 ~~删除😀~~",
        "自动链接 <https://例え.com/a> 尾",
        "==高亮😀== 与 ^上标^",
    ];
    let mut saw_expanded_link = false;
    for fixture in FIXTURES {
        let block = cx.new(|cx| {
            Block::with_record(
                cx,
                BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown(fixture)),
            )
        });
        cx.update(|_window, cx| {
            block.update(cx, |block, block_cx| {
                let clean_len = block.render_cache.plain_text().len();
                for caret in 0..=clean_len {
                    block.move_to(caret, block_cx);
                    block.refresh_inline_projection_for_focus(true);
                    let markdown = block.record.title.serialize_markdown();
                    let display = block.rendered_text().to_string();
                    saw_expanded_link |= display.contains("https://例え.com/路径");
                    for start in 0..=display.len() {
                        if !display.is_char_boundary(start) {
                            continue;
                        }
                        for end in start..=display.len() {
                            if !display.is_char_boundary(end) {
                                continue;
                            }
                            let mapped = block.current_range_to_markdown_range(start..end);
                            assert!(
                                markdown.get(mapped.clone()).is_some(),
                                "{fixture:?} 光标 {caret}，展示 {start}..{end} → 源码 {mapped:?} 不在字符边界"
                            );
                        }
                    }
                    for start in 0..=markdown.len() {
                        if !markdown.is_char_boundary(start) {
                            continue;
                        }
                        for end in start..=markdown.len() {
                            if !markdown.is_char_boundary(end) {
                                continue;
                            }
                            let mapped = block.markdown_range_to_current_range(start..end);
                            assert!(
                                display.get(mapped.clone()).is_some(),
                                "{fixture:?} 光标 {caret}，源码 {start}..{end} → 展示 {mapped:?} 不在字符边界"
                            );
                        }
                    }
                }
            });
        });
    }
    assert!(saw_expanded_link, "链接投影从未展开，扫描未覆盖目标分支");
}

#[rgpui::test]
async fn tab_inserts_character_in_paragraph(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("ab")));

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.move_to(1, block_cx);
            block.on_indent_block(&IndentBlock, window, block_cx);
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "a    b");
        assert_eq!(block.selected_range, 5..5);
    });
}

#[rgpui::test]
async fn tab_inserts_character_in_code_block(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::with_plain_text(BlockKind::CodeBlock { language: None }, "ab"),
        )
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.move_to(1, block_cx);
            block.on_indent_block(&IndentBlock, window, block_cx);
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "a    b");
        assert_eq!(block.selected_range, 5..5);
    });
}

#[test]
fn expanded_code_cursor_offset_stays_before_closing_backtick() {
    let fragments = vec![InlineFragment {
        text: "123".to_string(),
        style: InlineStyle {
            code: true,
            ..InlineStyle::default()
        },
        html_style: None,
        link: None,
        footnote: None,
        math: None,
    }];

    assert_eq!(expanded_display_offset_for_clean(&fragments, 0), 1);
    assert_eq!(expanded_display_offset_for_clean(&fragments, 3), 5);
    assert_eq!(expanded_display_cursor_offset_for_clean(&fragments, 0), 1);
    assert_eq!(expanded_display_cursor_offset_for_clean(&fragments, 3), 4);
}

#[test]
fn expanded_code_cursor_offset_keeps_plain_text_boundaries() {
    let fragments = vec![
        InlineFragment {
            text: "a".to_string(),
            style: InlineStyle::default(),
            html_style: None,
            link: None,
            footnote: None,
            math: None,
        },
        InlineFragment {
            text: "bc".to_string(),
            style: InlineStyle {
                code: true,
                ..InlineStyle::default()
            },
            html_style: None,
            link: None,
            footnote: None,
            math: None,
        },
    ];

    assert_eq!(expanded_display_cursor_offset_for_clean(&fragments, 1), 1);
    assert_eq!(expanded_display_cursor_offset_for_clean(&fragments, 3), 4);
}

#[test]
fn typing_inside_manual_backticks_keeps_cursor_inside_code_span() {
    let tree = InlineTextTree::plain("``");
    let result = tree.replace_plain_range(1..1, "1", InlineInsertionAttributes::default());

    assert_eq!(result.tree.plain_text(), "1");
    assert_eq!(
        result.tree.fragments,
        vec![InlineFragment {
            text: "1".to_string(),
            style: InlineStyle {
                code: true,
                ..InlineStyle::default()
            },
            html_style: None,
            link: None,
            footnote: None,
            math: None,
        }]
    );

    let clean_cursor = result.map_offset(2);
    assert_eq!(clean_cursor, 1);
    assert_eq!(
        expanded_display_cursor_offset_for_clean(&result.tree.fragments, clean_cursor),
        2
    );
}

#[rgpui::test]
async fn enter_inside_multiline_inline_code_inserts_hard_line_without_splitting(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("`line 1\nline 2`"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        let offset = "line 1\n".len();
        block.selected_range = offset..offset;
        cx.notify();
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.on_newline(&Newline, window, block_cx);
        });
    });

    block.read_with(cx, |block, _cx| {
        let text = "line 1\n\nline 2";
        assert_eq!(block.kind(), BlockKind::Paragraph);
        assert_eq!(block.rendered_text(), text);
        assert_eq!(block.selected_range, "line 1\n\n".len().."line 1\n\n".len());
        assert!(
            block
                .inline_spans()
                .iter()
                .any(|span| { span.style.code && span.range == (0..text.len()) })
        );
    });
}

#[rgpui::test]
async fn inline_math_focus_stays_rendered_rich_and_keeps_links(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("**bold** $x^2$ [repo](https://example.com)"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        // The math source is shown inline (`$x^2$`) while bold and the link stay
        // collapsed; the block never falls back to raw Markdown editing.
        assert_eq!(block.rendered_text(), "bold $x^2$ repo");

        // Focusing with the caret inside the math keeps the rendered-rich
        // projection rather than dumping the whole block to raw source, so the
        // link in the same block keeps its link attribute.
        let caret = "bold $".len();
        block.move_to(caret, cx);
        block.refresh_inline_projection_for_focus(true);
        assert!(!block.uses_raw_text_editing());
        assert!(block.record.title.has_mixed_inline_visuals());
        assert!(block.record.title.has_inline_links());
        assert!(
            block.inline_spans().iter().any(|span| span.link.is_some()),
            "link must stay styled while editing the math in the same block"
        );
        assert_eq!(
            block.record.title.serialize_markdown(),
            "**bold** $x^2$ [repo](https://example.com)"
        );
    });
}

#[rgpui::test]
async fn script_spans_focus_stay_rendered_rich(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("x^2^ and H~2~O"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "x2 and H2O");
        assert_eq!(block.inline_spans()[0].style.script, InlineScript::Normal);
        assert_eq!(
            block.inline_spans()[1].style.script,
            InlineScript::Superscript
        );
        assert!(!block.uses_raw_text_editing());
        assert_eq!(block.rendered_text(), "x2 and H2O");
        assert_eq!(block.record.title.serialize_markdown(), "x^2^ and H~2~O");
    });
}

#[rgpui::test]
async fn link_anchor_emphasis_delimiters_are_revealed_when_caret_inside(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("[**bold**](https://example.com)"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        // Collapsed, only the styled anchor text is shown.
        assert_eq!(block.rendered_text(), "bold");

        // With the caret inside the bold anchor text, the projection reveals both
        // the link syntax and the anchor's own `**` emphasis markers, so they can
        // be edited instead of staying invisible.
        block.move_to(2, cx);
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "[**bold**](https://example.com)");
    });
}

#[rgpui::test]
async fn mermaid_block_uses_raw_text_editing(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let markdown = "```mermaid\nflowchart LR\nA --> B\n```";
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::mermaid(markdown)));

    block.update(cx, |block, _cx| {
        assert_eq!(block.kind(), BlockKind::MermaidBlock);
        assert!(block.uses_raw_text_editing());
        assert_eq!(block.rendered_text(), markdown);
        assert_eq!(block.record.markdown_line(0, None), markdown);
    });
}

#[rgpui::test]
async fn enter_inside_projected_inline_code_inserts_hard_line_without_splitting(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("`line 1\nline 2`"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        let offset = "line 1\n".len();
        block.selected_range = offset..offset;
        block.refresh_inline_projection_for_focus(true);
        cx.notify();
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.on_newline(&Newline, window, block_cx);
        });
    });

    block.read_with(cx, |block, _cx| {
        let text = "line 1\n\nline 2";
        assert_eq!(block.kind(), BlockKind::Paragraph);
        assert_eq!(block.record.title.plain_text(), text);
        assert!(
            block
                .record
                .title
                .render_cache()
                .spans()
                .iter()
                .any(|span| span.style.code && span.range == (0..text.len()))
        );
    });
}

#[rgpui::test]
async fn enter_outside_inline_code_still_splits_paragraph(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("alpha beta"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = "alpha".len().."alpha".len();
        cx.notify();
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.on_newline(&Newline, window, block_cx);
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.kind(), BlockKind::Paragraph);
        assert_eq!(block.rendered_text(), "alpha");
        assert_eq!(block.selected_range, "alpha".len().."alpha".len());
    });
}

#[rgpui::test]
async fn enter_inside_comment_block_inserts_hard_line_without_splitting(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::comment("<!--\n**not bold** [not link](https://example.com)\n-->"),
        )
    });

    block.update(cx, |block, cx| {
        let offset = "<!--\n".len();
        block.selected_range = offset..offset;
        cx.notify();
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.on_newline(&Newline, window, block_cx);
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.kind(), BlockKind::Comment);
        assert_eq!(
            block.rendered_text(),
            "<!--\n\n**not bold** [not link](https://example.com)\n-->"
        );
        assert_eq!(block.inline_spans().len(), 1);
        assert_eq!(
            block.inline_spans()[0].range,
            0..block.rendered_text().len()
        );
        assert_eq!(block.inline_spans()[0].style, InlineStyle::default());
    });
}

#[rgpui::test]
async fn paragraph_shortcut_creates_task_item_directly(cx: &mut TestAppContext) {
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph(String::new())));

    block.update(cx, |block, cx| {
        block.apply_title_edit(
            InlineTextTree::plain("- [x] task"),
            10,
            None,
            None,
            None,
            false,
            cx,
        );
    });

    let kind = block.read_with(cx, |block, _cx| block.kind());
    let text = block.read_with(cx, |block, _cx| block.rendered_text().to_string());
    assert_eq!(kind, BlockKind::TaskListItem { checked: true });
    assert_eq!(text, "task");
}

#[rgpui::test]
async fn paragraph_shortcut_creates_parenthesized_numbered_list_directly(cx: &mut TestAppContext) {
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph(String::new())));

    block.update(cx, |block, cx| {
        block.apply_title_edit(
            InlineTextTree::plain("1) item"),
            7,
            None,
            None,
            None,
            false,
            cx,
        );
    });

    let kind = block.read_with(cx, |block, _cx| block.kind());
    let text = block.read_with(cx, |block, _cx| block.rendered_text().to_string());
    assert_eq!(kind, BlockKind::NumberedListItem);
    assert_eq!(text, "item");
}

#[rgpui::test]
async fn bullet_shortcut_upgrades_to_task_item_after_box_prefix(cx: &mut TestAppContext) {
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph(String::new())));

    block.update(cx, |block, cx| {
        block.apply_title_edit(InlineTextTree::plain("- "), 2, None, None, None, false, cx);
    });
    let kind = block.read_with(cx, |block, _cx| block.kind());
    assert_eq!(kind, BlockKind::BulletedListItem);

    block.update(cx, |block, cx| {
        block.apply_title_edit(
            InlineTextTree::plain("[ ] "),
            4,
            None,
            None,
            None,
            false,
            cx,
        );
    });

    let kind = block.read_with(cx, |block, _cx| block.kind());
    let text = block.read_with(cx, |block, _cx| block.rendered_text().to_string());
    assert_eq!(kind, BlockKind::TaskListItem { checked: false });
    assert_eq!(text, "");
}

#[rgpui::test]
async fn inline_code_projection_only_expands_touched_span(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a `code` b"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a code b"
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a `code` b"
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 9..9;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a code b"
    );
}

#[rgpui::test]
async fn inline_code_projection_expands_only_the_selected_code_span(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("`one` and `two`"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 1..1;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "`one` and two"
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 10..10;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "one and `two`"
    );
}

#[rgpui::test]
async fn bold_projection_only_expands_touched_span(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a **bold** b"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a bold b"
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a **bold** b"
    );
}

#[rgpui::test]
async fn bold_projection_expands_only_the_selected_bold_span(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("**one** and **two**"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 1..1;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "**one** and two"
    );

    block.update(cx, |block, _cx| {
        block.reset_inline_projection();
        block.selected_range = "one and ".len().."one and ".len();
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "one and **two**"
    );
}

#[rgpui::test]
async fn bold_projection_expands_selected_range_and_html_strong(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a **bold** b"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 2..6;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a **bold** b"
    );

    let html_block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("<strong>bold</strong>"),
            ),
        )
    });

    html_block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        html_block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "**bold**"
    );
}

#[rgpui::test]
async fn bold_projection_marker_edit_unwraps_bold_style(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("**bold**"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "**bold**");
        block.replace_text_in_visible_range(0..2, "", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "bold");
        assert_eq!(block.record.title.serialize_markdown(), "bold");
        assert!(
            block
                .record
                .title
                .render_cache()
                .spans()
                .iter()
                .all(|span| !span.style.bold)
        );
    });
}

#[rgpui::test]
async fn bold_projection_insertion_inside_span_preserves_bold_style(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("**bold**"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "**bold**");
        block.replace_text_in_visible_range(3..3, "X", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "**bXold**");
        assert_eq!(block.record.title.serialize_markdown(), "**bXold**");
        assert!(block.record.title.render_cache().spans()[0].style.bold);
    });
}

#[rgpui::test]
async fn italic_projection_only_expands_touched_span(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a *italic* b"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a italic b"
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a *italic* b"
    );
}

#[rgpui::test]
async fn italic_projection_marker_edit_unwraps_italic_style(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown("*it*")),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "*it*");
        block.replace_text_in_visible_range(0..1, "", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "it");
        assert_eq!(block.record.title.serialize_markdown(), "it");
        assert!(
            block
                .record
                .title
                .render_cache()
                .spans()
                .iter()
                .all(|span| !span.style.italic)
        );
    });
}

#[rgpui::test]
async fn typing_closing_italic_marker_places_caret_after_marker(cx: &mut TestAppContext) {
    // `*italic` is literal until the closing `*` is typed; afterwards the caret
    // must land *after* the closing marker so further typing stays plain.
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("*italic"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 7..7;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "*italic");
        block.replace_text_in_visible_range(7..7, "*", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "*italic*");
        assert_eq!(block.caret_offset(), "*italic*".len());
        assert_eq!(
            block.collapsed_caret_affinity,
            super::CaretAffinity::OuterEnd
        );
    });
}

#[rgpui::test]
async fn typing_closing_bold_marker_places_caret_after_marker(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("**bold*"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 7..7;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "**bold*");
        block.replace_text_in_visible_range(7..7, "*", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "**bold**");
        assert_eq!(block.caret_offset(), "**bold**".len());
        assert_eq!(
            block.collapsed_caret_affinity,
            super::CaretAffinity::OuterEnd
        );
    });
}

#[rgpui::test]
async fn typing_inside_span_keeps_default_affinity(cx: &mut TestAppContext) {
    // Inserting an ordinary character inside a bold span must not jump the
    // caret outside the span.
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("**bold**"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "**bold**");
        // Insert "X" inside the bold word (display offset 3 = after "**b").
        block.replace_text_in_visible_range(3..3, "X", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "**bXold**");
        assert_eq!(
            block.collapsed_caret_affinity,
            super::CaretAffinity::Default
        );
    });
}

#[rgpui::test]
async fn typing_bold_markers_char_by_char_produces_bold_not_italic(cx: &mut TestAppContext) {
    // Typing `**bold**` one character at a time must yield bold, not italic.
    // The clean parse is committed on each keystroke, so the intermediate
    // `**bold*` must not collapse to a literal `*` plus an italic `bold`.
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown("")),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        for ch in "**bold**".chars() {
            let caret = block.caret_offset();
            block.replace_text_in_visible_range(caret..caret, &ch.to_string(), None, false, cx);
        }
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.record.title.plain_text(), "bold");
        assert_eq!(block.record.title.serialize_markdown(), "**bold**");
        assert!(
            block
                .record
                .title
                .render_cache()
                .spans()
                .iter()
                .all(|span| span.style.bold && !span.style.italic),
            "typed `**bold**` must be bold, not italic"
        );
    });
}

#[rgpui::test]
async fn typing_after_closing_italic_marker_inserts_plain_text(cx: &mut TestAppContext) {
    // After typing `*italic*` the caret sits after the closing `*`, so further
    // typing must be plain text rather than being absorbed back into the span.
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown("")),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        for ch in "*italic* x".chars() {
            let caret = block.caret_offset();
            block.replace_text_in_visible_range(caret..caret, &ch.to_string(), None, false, cx);
        }
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.record.title.plain_text(), "italic x");
        assert_eq!(block.record.title.serialize_markdown(), "*italic* x");
        // The trailing " x" must be a plain (non-italic) fragment.
        let trailing_is_italic = block
            .record
            .title
            .fragments
            .iter()
            .any(|fragment| fragment.text.contains('x') && fragment.style.italic);
        assert!(
            !trailing_is_italic,
            "text after closing `*` must not be italic"
        );
    });
}

#[rgpui::test]
async fn typing_after_closing_bold_marker_inserts_plain_text(cx: &mut TestAppContext) {
    // Same as above for bold: typing past the closing `**` must be plain.
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown("")),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        for ch in "**bold** more".chars() {
            let caret = block.caret_offset();
            block.replace_text_in_visible_range(caret..caret, &ch.to_string(), None, false, cx);
        }
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.record.title.plain_text(), "bold more");
        assert_eq!(block.record.title.serialize_markdown(), "**bold** more");
        let trailing_is_bold = block
            .record
            .title
            .fragments
            .iter()
            .any(|fragment| fragment.text.contains("more") && fragment.style.bold);
        assert!(
            !trailing_is_bold,
            "text after closing `**` must not be bold"
        );
    });
}

#[rgpui::test]
async fn strikethrough_projection_only_expands_touched_span(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a ~~gone~~ b"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a gone b"
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a ~~gone~~ b"
    );
}

#[rgpui::test]
async fn script_projection_expands_only_touched_span(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("x^2^ and H~2~O"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "x2 and H2O"
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 1..1;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "x^2^ and H2O"
    );

    block.update(cx, |block, _cx| {
        block.reset_inline_projection();
        block.selected_range = "x2 and H".len().."x2 and H".len();
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "x2 and H~2~O"
    );
}

#[rgpui::test]
async fn standalone_script_projection_uses_html_marker_fallback(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("<sup>2</sup> and <sub>n</sub>"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "<sup>2</sup> and n"
    );

    block.update(cx, |block, _cx| {
        block.reset_inline_projection();
        block.selected_range = "2 and ".len().."2 and ".len();
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "2 and <sub>n</sub>"
    );
}

#[rgpui::test]
async fn script_projection_marker_edit_unwraps_script_style(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown("x^2^")),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 1..1;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "x^2^");
        block.replace_text_in_visible_range(1..2, "", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "x2");
        assert_eq!(block.record.title.serialize_markdown(), "x2");
        assert!(
            block
                .inline_spans()
                .iter()
                .all(|span| span.style.script == InlineScript::Normal)
        );
    });
}

#[rgpui::test]
async fn subscript_projection_marker_edit_unwraps_script_style(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown("H~2~O")),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 1..1;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "H~2~O");
        block.replace_text_in_visible_range(1..2, "", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "H2O");
        assert_eq!(block.record.title.serialize_markdown(), "H2O");
        assert!(
            block
                .record
                .title
                .render_cache()
                .spans()
                .iter()
                .all(|span| span.style.script == InlineScript::Normal)
        );
    });
}

#[rgpui::test]
async fn script_projection_insertion_inside_span_preserves_script_style(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown("x^2^")),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 1..1;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "x^2^");
        block.replace_text_in_visible_range(3..3, "3", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "x^23^");
        assert_eq!(block.record.title.serialize_markdown(), "x^23^");
        assert_eq!(
            block.record.title.render_cache().spans()[1].style.script,
            InlineScript::Superscript
        );
    });
}

#[rgpui::test]
async fn inline_code_projection_right_escape_stays_outside_after_rebuild(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a `123` b"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 5..5;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 6);

    block.update(cx, |block, _cx| {
        let (target, affinity) = block
            .projected_move_right_target(block.caret_offset())
            .expect("inner end should jump to outer end");
        block.assign_collapsed_selection_offset(target, affinity, None);
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a `123` b"
    );
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 7);

    block.update(cx, |block, _cx| {
        let target = block.next_boundary(block.caret_offset());
        block.move_to_with_preferred_x(target, None, _cx);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 8);
}

#[rgpui::test]
async fn inline_code_projection_left_escape_stays_outside_after_rebuild(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a `123` b"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 3);

    block.update(cx, |block, _cx| {
        let (target, affinity) = block
            .projected_move_left_target(block.caret_offset())
            .expect("inner start should jump to outer start");
        block.assign_collapsed_selection_offset(target, affinity, None);
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 2);

    block.update(cx, |block, _cx| {
        let target = block.previous_boundary(block.caret_offset());
        block.move_to_with_preferred_x(target, None, _cx);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 1);
}

#[rgpui::test]
async fn strikethrough_projection_right_escape_stays_outside_after_rebuild(
    cx: &mut TestAppContext,
) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a ~~123~~ b"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 5..5;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 7);

    block.update(cx, |block, _cx| {
        let (target, affinity) = block
            .projected_move_right_target(block.caret_offset())
            .expect("inner end should jump to outer end");
        block.assign_collapsed_selection_offset(target, affinity, None);
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 9);

    block.update(cx, |block, _cx| {
        let target = block.next_boundary(block.caret_offset());
        block.move_to_with_preferred_x(target, None, _cx);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 10);
}

#[rgpui::test]
async fn strikethrough_projection_left_escape_stays_outside_after_rebuild(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a ~~bc~~ d"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 4);

    block.update(cx, |block, _cx| {
        let (target, affinity) = block
            .projected_move_left_target(block.caret_offset())
            .expect("expected projected move left target");
        block.assign_collapsed_selection_offset(target, affinity, None);
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 2);

    block.update(cx, |block, _cx| {
        let target = block.previous_boundary(block.caret_offset());
        block.move_to_with_preferred_x(target, None, _cx);
    });
    assert_eq!(block.read_with(cx, |block, _cx| block.caret_offset()), 1);
}

#[rgpui::test]
async fn word_start_boundaries_step_over_whole_words(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("hello world foo"),
            ),
        )
    });

    block.read_with(cx, |block, _cx| {
        // Word starts are at offsets 0 ("hello"), 6 ("world"), 12 ("foo").
        assert_eq!(block.next_word_boundary(0), 6);
        assert_eq!(block.next_word_boundary(3), 6);
        assert_eq!(block.next_word_boundary(6), 12);
        assert_eq!(block.next_word_boundary(12), 15);

        assert_eq!(block.prev_word_boundary(15), 12);
        assert_eq!(block.prev_word_boundary(12), 6);
        assert_eq!(block.prev_word_boundary(7), 6);
        assert_eq!(block.prev_word_boundary(6), 0);
        assert_eq!(block.prev_word_boundary(0), 0);
    });
}

#[rgpui::test]
async fn inline_link_projection_only_expands_touched_span(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a [link](https://example.com) b"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a link b"
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a [link](https://example.com) b"
    );
}

#[rgpui::test]
async fn reference_style_link_resolves_and_expands_preserving_raw_syntax(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("[reference link][ref-link]"),
            ),
        );
        block.set_runtime_context(
            None,
            Arc::default(),
            Arc::new(parse_link_reference_definitions(
                "[ref-link]: https://example.com",
            )),
            Arc::default(),
        );
        block
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "reference link"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.inline_link_at(0).map(str::to_string)),
        Some("https://example.com".to_string())
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "[reference link][ref-link]"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.record.title.serialize_markdown()),
        "[reference link][ref-link]"
    );
}

#[rgpui::test]
async fn reference_style_link_hit_exposes_raw_prompt_and_resolved_open_target(
    cx: &mut TestAppContext,
) {
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("[reference link][ref-links]"),
            ),
        );
        block.set_runtime_context(
            None,
            Arc::default(),
            Arc::new(parse_link_reference_definitions(
                "[ref-links]: https://example.com",
            )),
            Arc::default(),
        );
        block
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.inline_link_hit_at(0).cloned()),
        Some(InlineLinkHit {
            prompt_target: "ref-links".to_string(),
            open_target: "https://example.com".to_string(),
        })
    );
}

#[rgpui::test]
async fn inline_link_with_title_expands_title_but_opens_destination(cx: &mut TestAppContext) {
    let markdown = "[ABC](https://abc.com \"https://abc.com\")";
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown(markdown),
            ),
        )
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.inline_link_hit_at(0).cloned()),
        Some(InlineLinkHit {
            prompt_target: "https://abc.com".to_string(),
            open_target: "https://abc.com".to_string(),
        })
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        markdown
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.record.title.serialize_markdown()),
        markdown
    );
}

#[rgpui::test]
async fn autolink_expands_with_angle_brackets_when_touched(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("<https://example.com>"),
            ),
        )
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "https://example.com"
    );

    block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "<https://example.com>"
    );
}

#[rgpui::test]
async fn projected_reference_target_stays_link_hit_testable(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("[reference link][ref-link]"),
            ),
        );
        block.set_runtime_context(
            None,
            Arc::default(),
            Arc::new(parse_link_reference_definitions(
                "[ref-link]: https://example.com",
            )),
            Arc::default(),
        );
        block
    });

    let target_offset = block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        block
            .rendered_text()
            .find("ref-link")
            .expect("projection should expose reference target")
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block
            .inline_link_hit_at(target_offset)
            .cloned()),
        Some(InlineLinkHit {
            prompt_target: "ref-link".to_string(),
            open_target: "https://example.com".to_string(),
        })
    );
}

#[rgpui::test]
async fn projected_reference_syntax_maps_full_delimiter_range_back_to_markdown(
    cx: &mut TestAppContext,
) {
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("[reference link][ref-link]"),
            ),
        );
        block.set_runtime_context(
            None,
            Arc::default(),
            Arc::new(parse_link_reference_definitions(
                "[ref-link]: https://example.com",
            )),
            Arc::default(),
        );
        block
    });

    let display_len = block.update(cx, |block, _cx| {
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        block.rendered_text().len()
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| {
            block.current_range_to_markdown_range(0..display_len)
        }),
        0.."[reference link][ref-link]".len()
    );
}

#[rgpui::test]
async fn editing_link_destination_inside_projection_preserves_link(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a [link](https://example.com) b"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
        let expanded = block.rendered_text().to_string();
        let insert_at = expanded
            .find("example.com")
            .expect("expanded link should expose its destination");
        block.replace_text_in_visible_range(insert_at..insert_at, "docs.", None, false, cx);
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.record.title.serialize_markdown()),
        "a [link](https://docs.example.com) b"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "a [link](https://docs.example.com) b"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.inline_link_at(3).map(str::to_string)),
        Some("https://docs.example.com".to_string())
    );
}

#[rgpui::test]
async fn typing_after_inline_link_preserves_link(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("[Link](https://example.com/) x"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        // Place the caret past the link (in the trailing text) so the edit does
        // not touch the link's projected run, then type. This is the case that
        // previously re-parsed from collapsed text and dropped the link.
        block.selected_range = 0..0;
        block.refresh_inline_projection_for_focus(true);
        let end = block.rendered_text().len();
        block.selected_range = end..end;
        block.refresh_inline_projection_for_focus(true);
        block.replace_text_in_visible_range(end..end, "y", None, false, cx);
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.record.title.serialize_markdown()),
        "[Link](https://example.com/) xy"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.inline_link_at(0).map(str::to_string)),
        Some("https://example.com/".to_string())
    );
}

#[rgpui::test]
async fn deleting_adjacent_text_preserves_reference_style_link_syntax(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("[ref][ref-link]a"),
            ),
        );
        block.set_runtime_context(
            None,
            Arc::default(),
            Arc::new(parse_link_reference_definitions(
                "[ref-link]: https://example.com",
            )),
            Arc::default(),
        );
        block
    });

    block.update(cx, |block, cx| {
        block.replace_text_in_visible_range(3..4, "", None, false, cx);
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.record.title.serialize_markdown()),
        "[ref][ref-link]"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "ref"
    );
}

#[rgpui::test]
async fn deleting_adjacent_text_preserves_autolink_syntax(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("<ref2>a"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.replace_text_in_visible_range(4..5, "", None, false, cx);
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.record.title.serialize_markdown()),
        "<ref2>"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "ref2"
    );
}

#[rgpui::test]
async fn link_projection_preserves_cursor_inside_destination_after_rebuild(
    cx: &mut TestAppContext,
) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a [link](https://example.com) b"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
        let expanded = block.rendered_text().to_string();
        let destination_offset = expanded
            .find("example.com")
            .expect("expanded link should expose destination text");
        block.move_to_with_preferred_x(destination_offset, None, cx);
        block.refresh_inline_projection_for_focus(true);
    });

    let destination_offset = block.read_with(cx, |block, _cx| {
        block
            .rendered_text()
            .find("example.com")
            .expect("expanded link should expose destination text")
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.caret_offset()),
        destination_offset
    );
}

#[rgpui::test]
async fn link_projection_preserves_selection_inside_destination_after_rebuild(
    cx: &mut TestAppContext,
) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a [link](https://example.com) b"),
            ),
        )
    });

    let selected_range = block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
        let expanded = block.rendered_text().to_string();
        let destination_offset = expanded
            .find("example.com")
            .expect("expanded link should expose destination text");
        let selected_range = destination_offset..destination_offset + "example".len();
        block.selected_range = selected_range.clone();
        block.selection_reversed = false;
        block.refresh_inline_projection_for_focus(true);
        selected_range
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        selected_range
    );
}

#[rgpui::test]
async fn link_middle_delimiter_click_snaps_to_destination_start(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a [link](https://example.com) b"),
            ),
        )
    });

    let destination_offset = block.update(cx, |block, cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
        let expanded = block.rendered_text().to_string();
        let middle = expanded
            .find("](")
            .expect("expanded link should expose middle delimiter");
        let destination_offset = expanded
            .find("https://")
            .expect("expanded link should expose destination start");
        let click_target = block.pointer_target_offset(middle + 1);
        block.move_to_with_preferred_x(click_target, None, cx);
        block.refresh_inline_projection_for_focus(true);
        destination_offset
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.caret_offset()),
        destination_offset
    );
}

#[rgpui::test]
async fn reversed_selection_survives_projection_focus_refresh(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("alpha beta"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 1..7;
        block.selection_reversed = true;
        block.refresh_inline_projection_for_focus(true);
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        1..7
    );
    assert!(block.read_with(cx, |block, _cx| block.selection_reversed));
}

#[rgpui::test]
async fn reversed_selection_survives_render_cache_refresh(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("alpha beta"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 1..7;
        block.selection_reversed = true;
        block.refresh_render_cache();
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        1..7
    );
    assert!(block.read_with(cx, |block, _cx| block.selection_reversed));
}

#[rgpui::test]
async fn reversed_selection_survives_clear_inline_projection(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("`code`"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
        block.selected_range = 1..5;
        block.selection_reversed = true;
        block.reset_inline_projection();
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "code"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        0..4
    );
    assert!(block.read_with(cx, |block, _cx| block.selection_reversed));
}

#[rgpui::test]
async fn reversed_selection_inside_link_destination_survives_focus_refresh(
    cx: &mut TestAppContext,
) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("a [link](https://example.com) b"),
            ),
        )
    });

    let expected = block.update(cx, |block, _cx| {
        block.selected_range = 2..2;
        block.refresh_inline_projection_for_focus(true);
        let expanded = block.rendered_text().to_string();
        let destination_offset = expanded
            .find("example.com")
            .expect("expanded link should expose destination text");
        let expected = destination_offset..destination_offset + "example".len();
        block.selected_range = expected.clone();
        block.selection_reversed = true;
        block.refresh_inline_projection_for_focus(true);
        expected
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        expected
    );
    assert!(block.read_with(cx, |block, _cx| block.selection_reversed));
}

#[rgpui::test]
async fn ime_selected_text_range_reports_reversed_for_right_to_left_selection(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("alpha beta"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 1..7;
        block.selection_reversed = true;
    });

    let selection = cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            <Block as EntityInputHandler>::selected_text_range(block, false, window, block_cx)
                .expect("selection")
        })
    });

    assert_eq!(selection.range, 1..7);
    assert!(selection.reversed);
}

#[rgpui::test]
async fn ime_replace_text_replaces_right_to_left_selection_in_source_raw_mode(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("alpha beta"),
            ),
        );
        block.enable_source_raw_mode();
        block
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 1..7;
        block.selection_reversed = true;
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            <Block as EntityInputHandler>::replace_text_in_range(
                block, None, "Z", window, block_cx,
            );
        });
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "aZeta"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        2..2
    );
    assert!(!block.read_with(cx, |block, _cx| block.selection_reversed));
}

#[rgpui::test]
async fn source_document_mode_enables_line_numbers(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::plain("a\nb")),
        );
        block.enable_source_document_mode();
        block
    });

    block.read_with(cx, |block, _cx| {
        assert!(block.is_source_raw_mode());
        assert!(block.show_source_line_numbers());
    });
}

#[rgpui::test]
async fn source_raw_mode_does_not_enable_line_numbers(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::plain("raw")),
        );
        block.enable_source_raw_mode();
        block
    });

    block.read_with(cx, |block, _cx| {
        assert!(block.is_source_raw_mode());
        assert!(!block.show_source_line_numbers());
    });
}

#[rgpui::test]
async fn ime_replace_and_mark_text_replaces_right_to_left_selection_in_table_cell(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::from_markdown("alpha")),
        );
        block.set_table_cell_mode(
            CellPosition { row: 0, column: 0 },
            crate::components::TableColumnAlignment::Left,
        );
        block
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 1..4;
        block.selection_reversed = true;
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            <Block as EntityInputHandler>::replace_and_mark_text_in_range(
                block,
                None,
                "XY",
                Some(0..1),
                window,
                block_cx,
            );
        });
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "aXYa"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        1..2
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.marked_range.clone()),
        Some(1..3)
    );
    assert!(!block.read_with(cx, |block, _cx| block.selection_reversed));
}

#[rgpui::test]
async fn ime_commit_inside_inline_code_preserves_code_style(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("aaa`hello world`aaa"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        let cursor = "aaahello".len();
        block.selected_range = cursor..cursor;
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            <Block as EntityInputHandler>::replace_and_mark_text_in_range(
                block,
                None,
                "ni",
                Some(2..2),
                window,
                block_cx,
            );
            <Block as EntityInputHandler>::replace_text_in_range(
                block, None, "你", window, block_cx,
            );
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "aaahello你 worldaaa");
        assert_eq!(
            block.record.title.serialize_markdown(),
            "aaa`hello你 world`aaa"
        );
        assert_only_code_range(block, "aaa".len().."aaahello你 world".len());
    });
}

#[rgpui::test]
async fn ime_commit_inside_projected_inline_code_preserves_code_style(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("aaa`hello world`aaa"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        let cursor = "aaahello".len();
        block.selected_range = cursor..cursor;
        block.refresh_inline_projection_for_focus(true);
        assert_eq!(block.rendered_text(), "aaa`hello world`aaa");
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            <Block as EntityInputHandler>::replace_and_mark_text_in_range(
                block,
                None,
                "ni",
                Some(2..2),
                window,
                block_cx,
            );
            <Block as EntityInputHandler>::replace_text_in_range(
                block, None, "你", window, block_cx,
            );
        });
    });

    block.update(cx, |block, _cx| {
        assert_eq!(
            block.record.title.serialize_markdown(),
            "aaa`hello你 world`aaa"
        );
        block.reset_inline_projection();
        assert_eq!(block.rendered_text(), "aaahello你 worldaaa");
        assert_only_code_range(block, "aaa".len().."aaahello你 world".len());
    });
}

#[rgpui::test]
async fn replacing_selection_inside_inline_code_preserves_code_style(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("aaa`hello world`aaa"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        let start = "aaahello ".len();
        let end = "aaahello world".len();
        block.selected_range = start..end;
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            <Block as EntityInputHandler>::replace_text_in_range(
                block, None, "你", window, block_cx,
            );
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "aaahello 你aaa");
        assert_eq!(block.record.title.serialize_markdown(), "aaa`hello 你`aaa");
        assert_only_code_range(block, "aaa".len().."aaahello 你".len());
    });
}

#[rgpui::test]
async fn replacing_selection_across_inline_code_boundary_stays_plain(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("aaa`hello`bbb"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = "aaahel".len().."aaahellobb".len();
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            <Block as EntityInputHandler>::replace_text_in_range(
                block, None, "你", window, block_cx,
            );
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.rendered_text(), "aaahel你b");
        assert_eq!(block.record.title.serialize_markdown(), "aaa`hel`你b");
        assert_only_code_range(block, "aaa".len().."aaahel".len());
    });
}

#[test]
fn ime_utf16_ranges_keep_multilingual_boundaries() {
    let text = "中文😀かな";
    let emoji_utf8 = "中文".len().."中文😀".len();
    assert_eq!(Block::utf16_range_to_utf8_in(text, &(2..4)), emoji_utf8);
    assert_eq!(Block::utf8_range_to_utf16_in(text, &emoji_utf8), 2..4);
}

#[rgpui::test]
async fn ime_replace_text_handles_cjk_and_emoji_utf16_ranges(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        let mut block = Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::plain("中文😀かな".to_string()),
            ),
        );
        block.enable_source_raw_mode();
        block
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            <Block as EntityInputHandler>::replace_text_in_range(
                block,
                Some(2..4),
                "語",
                window,
                block_cx,
            );
        });
    });

    assert_eq!(
        block.read_with(cx, |block, _cx| block.rendered_text().to_string()),
        "中文語かな"
    );
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        "中文語".len().."中文語".len()
    );
}

#[rgpui::test]
async fn ime_selection_ignores_editor_external_selection(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("alpha beta"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.selected_range = 1..1;
        block.editor_selection_range = Some(0..block.plain_len());
    });

    let selection = cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            <Block as EntityInputHandler>::selected_text_range(block, false, window, block_cx)
                .expect("selection")
        })
    });

    assert_eq!(selection.range, 1..1);
    assert!(!selection.reversed);
}

#[rgpui::test]
async fn focusing_rendered_image_does_not_auto_expand(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("![diagram](./assets/diagram.png)"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.refresh_render_cache();
        assert!(block.showing_rendered_image());
        assert!(!block.image_edit_expanded);

        assert!(!block.sync_image_focus_state(true));
        assert!(block.showing_rendered_image());
        assert!(!block.image_edit_expanded);
    });
}

#[rgpui::test]
async fn requested_rendered_image_expansion_enters_raw_markdown_editing(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("![diagram](./assets/diagram.png)"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.refresh_render_cache();
        block.request_image_edit_expansion();
        assert!(block.sync_image_focus_state(true));
        assert!(block.image_edit_expanded);
        assert!(!block.showing_rendered_image());
        assert_eq!(block.caret_offset(), block.plain_len());
    });
}

#[rgpui::test]
async fn expanded_image_keeps_render_and_exposes_source(cx: &mut TestAppContext) {
    // 点击图片后：源码行与渲染图同屏（对齐 Typora），而不是源码替换图片。
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("![diagram](./assets/diagram.png)"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.refresh_render_cache();
        assert!(block.showing_rendered_image());
        assert!(!block.showing_image_source_with_render());

        block.request_image_edit_expansion();
        assert!(block.sync_image_focus_state(true));
        assert!(block.showing_image_source_with_render());
        // 渲染图仍在（不再因进入源码态而消失）。
        assert!(block.image_runtime().is_some());

        assert!(block.sync_image_focus_state(false));
        assert!(!block.showing_image_source_with_render());
        assert!(block.showing_rendered_image());
    });
}

#[rgpui::test]
async fn blurred_valid_rendered_image_recovers_image_presentation(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("![diagram](./assets/diagram.png)"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.refresh_render_cache();
        block.request_image_edit_expansion();
        assert!(block.sync_image_focus_state(true));
        assert!(block.image_edit_expanded);

        assert!(block.sync_image_focus_state(false));
        assert!(!block.image_edit_expanded);
        assert!(block.showing_rendered_image());
    });
}

#[rgpui::test]
async fn broken_rendered_image_syntax_blurs_back_to_plain_text(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("![diagram](./assets/diagram.png)"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.refresh_render_cache();
        block.request_image_edit_expansion();
        assert!(block.sync_image_focus_state(true));

        block
            .record
            .set_title(InlineTextTree::from_markdown("not an image anymore"));
        block.refresh_render_cache();
        assert!(!block.sync_image_focus_state(false));
        assert!(block.image_runtime().is_none());
        assert!(!block.image_edit_expanded);
        assert!(!block.showing_rendered_image());
        assert_eq!(block.rendered_text(), "not an image anymore");
    });
}

#[rgpui::test]
async fn code_block_cache_builds_rust_highlight_spans(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("fn main() {\n    let value: i32 = 42;\n}\n"),
            ),
        )
    });

    let highlight = block
        .read_with(cx, |block, _cx| block.code_highlight_result().cloned())
        .expect("code block should cache a highlight result");
    assert_eq!(highlight.language, HighlightLanguage::Rust);
    assert!(!highlight.spans.is_empty());
}

#[rgpui::test]
async fn code_block_cache_updates_when_language_changes(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("fn main() {\n    let value = 42;\n}\n"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.record.kind = BlockKind::CodeBlock {
            language: Some("text".into()),
        };
        block.refresh_render_cache();
    });

    let highlight = block
        .read_with(cx, |block, _cx| block.code_highlight_result().cloned())
        .expect("known plain fallback should still cache a result");
    assert_eq!(highlight.language, HighlightLanguage::PlainText);
    assert!(highlight.spans.is_empty());
}

#[rgpui::test]
async fn code_block_language_setter_updates_highlight_without_changing_content(
    cx: &mut TestAppContext,
) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("print('hello')"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        let range = 0..block.code_language_text().len();
        block.replace_code_language_text_in_range(range, "python", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.code_language_text(), "python");
        assert_eq!(block.rendered_text(), "print('hello')");
        assert_eq!(
            block
                .code_highlight_result()
                .expect("python should highlight")
                .language,
            HighlightLanguage::Python
        );
    });
}

#[rgpui::test]
async fn code_block_language_accepts_unknown_language_as_plain_rendering(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("fn main() {}"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        let range = 0..block.code_language_text().len();
        block.replace_code_language_text_in_range(range, "unknown-lang", None, false, cx);
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.code_language_text(), "unknown-lang");
        assert!(block.code_highlight_result().is_none());
    });
}

#[rgpui::test]
async fn code_language_input_uses_ime_path_without_touching_code_content(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("fn main() {}"),
            ),
        )
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.code_language_focus_handle.focus(window, block_cx);
            block.code_language_selected_range = 0..block.code_language_text().len();
            block.selected_range = 3..3;
            <Block as EntityInputHandler>::replace_text_in_range(
                block, None, "python", window, block_cx,
            );
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.code_language_text(), "python");
        assert_eq!(block.rendered_text(), "fn main() {}");
        assert_eq!(block.selected_range, 3..3);
        assert_eq!(block.code_language_selected_range, 6..6);
    });
}

#[rgpui::test]
async fn code_language_input_handles_utf16_ranges(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("zh😀kana".into()),
                },
                InlineTextTree::plain("body"),
            ),
        )
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.code_language_focus_handle.focus(window, block_cx);
            <Block as EntityInputHandler>::replace_text_in_range(
                block,
                Some(2..4),
                "py",
                window,
                block_cx,
            );
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.code_language_text(), "zhpykana");
        assert_eq!(block.rendered_text(), "body");
    });
}

#[rgpui::test]
async fn code_language_ime_selection_tracks_collapsed_crlf(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("body"),
            ),
        )
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.code_language_focus_handle.focus(window, block_cx);
            block.code_language_selected_range = 0..4;
            <Block as EntityInputHandler>::replace_and_mark_text_in_range(
                block,
                None,
                "a\r\n😀",
                Some(3..5),
                window,
                block_cx,
            );
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.code_language_text(), "a 😀");
        assert_eq!(block.code_language_selected_range, 2..6);
        assert_eq!(block.code_language_marked_range, Some(0..6));
        assert_eq!(block.rendered_text(), "body");
    });
}

#[rgpui::test]
async fn code_language_input_clears_language_when_empty(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("body"),
            ),
        )
    });

    cx.update(|window, cx| {
        block.update(cx, |block, block_cx| {
            block.code_language_focus_handle.focus(window, block_cx);
            block.code_language_selected_range = 0..block.code_language_text().len();
            <Block as EntityInputHandler>::replace_text_in_range(block, None, "", window, block_cx);
        });
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(block.code_language_text(), "");
        assert!(matches!(
            block.kind(),
            BlockKind::CodeBlock { language: None }
        ));
        assert!(block.code_highlight_result().is_none());
    });
}

#[rgpui::test]
async fn ending_pointer_selection_session_preserves_text_state(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("fn main() {}"),
            ),
        )
    });

    block.update(cx, |block, _cx| {
        block.is_selecting = true;
        block.code_language_is_selecting = true;
        block.selected_range = 3..7;
        block.marked_range = Some(4..6);
        block.code_language_selected_range = 1..3;
        block.code_language_marked_range = Some(1..2);

        assert!(block.end_pointer_selection_session());
        assert!(!block.is_selecting);
        assert!(!block.code_language_is_selecting);
        assert_eq!(block.selected_range, 3..7);
        assert_eq!(block.marked_range, Some(4..6));
        assert_eq!(block.code_language_selected_range, 1..3);
        assert_eq!(block.code_language_marked_range, Some(1..2));

        assert!(!block.end_pointer_selection_session());
    });
}

#[rgpui::test]
async fn non_dragging_mouse_move_ends_stale_text_selection(cx: &mut TestAppContext) {
    cx.update(|cx| {
        I18nManager::init_with_language_id(cx, "en-US");
        rgpui::theme::init(cx);
    });
    let (block, cx) = cx.add_window_view(|_window, cx| {
        Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::plain("hello world")),
        )
    });

    let event = MouseMoveEvent {
        position: point(px(8.0), px(8.0)),
        pressed_button: None,
        modifiers: Modifiers::default(),
    };
    cx.update(|window, cx| {
        block.update(cx, |block, cx| {
            block.is_selecting = true;
            block.selected_range = 3..7;
            block.marked_range = Some(4..6);

            block.on_mouse_move(&event, window, cx);

            assert!(!block.is_selecting);
            assert_eq!(block.selected_range, 3..7);
            assert_eq!(block.marked_range, Some(4..6));
        });
    });
}

#[rgpui::test]
async fn dragging_mouse_move_keeps_text_selection_session_active(cx: &mut TestAppContext) {
    cx.update(|cx| {
        I18nManager::init_with_language_id(cx, "en-US");
        rgpui::theme::init(cx);
    });
    let (block, cx) = cx.add_window_view(|_window, cx| {
        Block::with_record(
            cx,
            BlockRecord::new(BlockKind::Paragraph, InlineTextTree::plain("hello world")),
        )
    });

    let event = MouseMoveEvent {
        position: point(px(8.0), px(8.0)),
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    };
    cx.update(|window, cx| {
        block.update(cx, |block, cx| {
            block.is_selecting = true;
            block.on_mouse_move(&event, window, cx);
            assert!(block.is_selecting);
        });
    });
}

#[rgpui::test]
async fn non_dragging_mouse_move_ends_stale_code_language_selection(cx: &mut TestAppContext) {
    cx.update(|cx| {
        I18nManager::init_with_language_id(cx, "en-US");
        rgpui::theme::init(cx);
    });
    let (block, cx) = cx.add_window_view(|_window, cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("fn main() {}"),
            ),
        )
    });

    let event = MouseMoveEvent {
        position: point(px(8.0), px(8.0)),
        pressed_button: None,
        modifiers: Modifiers::default(),
    };
    cx.update(|window, cx| {
        block.update(cx, |block, cx| {
            block.code_language_is_selecting = true;
            block.code_language_selected_range = 1..3;
            block.code_language_marked_range = Some(1..2);

            block.on_code_language_mouse_move(&event, window, cx);

            assert!(!block.code_language_is_selecting);
            assert_eq!(block.code_language_selected_range, 1..3);
            assert_eq!(block.code_language_marked_range, Some(1..2));
        });
    });
}

#[rgpui::test]
async fn code_language_mouse_up_out_ends_selection_without_clearing_text_state(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        I18nManager::init_with_language_id(cx, "en-US");
        rgpui::theme::init(cx);
    });
    let (block, cx) = cx.add_window_view(|_window, cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock {
                    language: Some("rust".into()),
                },
                InlineTextTree::plain("fn main() {}"),
            ),
        )
    });

    let event = MouseUpEvent {
        position: point(px(200.0), px(200.0)),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 1,
    };
    cx.update(|window, cx| {
        block.update(cx, |block, cx| {
            block.is_selecting = true;
            block.code_language_is_selecting = true;
            block.selected_range = 3..7;
            block.marked_range = Some(4..6);
            block.code_language_selected_range = 1..3;
            block.code_language_marked_range = Some(1..2);

            block.on_code_language_mouse_up_out(&event, window, cx);

            assert!(block.is_selecting);
            assert!(!block.code_language_is_selecting);
            assert_eq!(block.selected_range, 3..7);
            assert_eq!(block.marked_range, Some(4..6));
            assert_eq!(block.code_language_selected_range, 1..3);
            assert_eq!(block.code_language_marked_range, Some(1..2));
        });
    });
}

#[rgpui::test]
async fn code_block_without_language_keeps_plain_rendering(cx: &mut TestAppContext) {
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::CodeBlock { language: None },
                InlineTextTree::plain("no highlighting here"),
            ),
        )
    });

    assert!(block.read_with(cx, |block, _cx| block.code_highlight_result().is_none()));
}

#[rgpui::test]
async fn editing_link_anchor_in_math_block_matches_plain_paragraph(cx: &mut TestAppContext) {
    // A block mixing inline math with a link is "source preserving", which used
    // to route its link edits through the markdown-space path. That path assumed
    // the anchor label began right after `[`, so the anchor's own emphasis
    // markers shifted the mapping and edits landed on the wrong character. Inline
    // links now edit through the link projection in every block, so deleting a
    // revealed anchor delimiter touches the delimiter, not a label character.
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("$x^2$ [**bold**](https://e.com)"),
            ),
        )
    });

    cx.update(|window, cx| {
        block.update(cx, |block, cx| {
            block.move_to("$x^2$ bo".len(), cx);
            block.refresh_inline_projection_for_focus(true);

            // Caret just past the revealed opening `**` of the bold anchor.
            let projected = block.rendered_text().to_string();
            assert_eq!(projected, "$x^2$ [**bold**](https://e.com)");
            let after_open = projected.find("[**").unwrap() + "[**".len();
            block.selected_range = after_open..after_open;

            block.on_delete_back(&DeleteBack, window, cx);

            let markdown = block.record.title.serialize_markdown();
            assert!(
                markdown.starts_with("$x^2$ "),
                "math source preserved: {markdown:?}"
            );
            assert!(
                markdown.contains("bold"),
                "anchor label must stay intact, only the delimiter is edited: {markdown:?}"
            );
        });
    });
}

#[rgpui::test]
async fn completing_link_in_math_block_places_caret_after_closing_paren(cx: &mut TestAppContext) {
    // A block mixing math with a link edits in markdown space. Typing the closing
    // `)` completes the link, and the caret must land just past it (like a plain
    // paragraph) rather than inside the anchor before `]`.
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("$x$ [link](google.com"),
            ),
        )
    });

    cx.update(|window, cx| {
        block.update(cx, |block, cx| {
            block.move_to(block.plain_len(), cx);
            block.refresh_inline_projection_for_focus(true);
            block.replace_text_in_range(None, ")", window, cx);
            block.refresh_inline_projection_for_focus(true);

            assert_eq!(
                block.record.title.serialize_markdown(),
                "$x$ [link](google.com)"
            );
            assert_eq!(block.rendered_text(), "$x$ [link](google.com)");
            let end = block.plain_len();
            assert_eq!(block.selected_range, end..end);
        });
    });
}

#[rgpui::test]
async fn rtl_selection_across_trailing_link_keeps_block_end_anchor(cx: &mut TestAppContext) {
    // A link sitting at the very end of a block that also contains inline math
    // stays expanded while the projection is rebuilt on every render. Dragging a
    // selection right-to-left from the block end across the link used to collapse
    // the anchor onto the closing `]` of the anchor text, because the trailing
    // `](url)` delimiters all share one clean offset and the remap snapped back
    // to the inner cursor position. The anchor must stay at the block end.
    let cx = cx.add_empty_window();
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("$x$ [link](google.com)"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        block.move_to(block.plain_len(), cx);
        block.refresh_inline_projection_for_focus(true);
        let end = block.plain_len();
        assert_eq!(block.rendered_text(), "$x$ [link](google.com)");

        // Start an RTL selection at the block end and drag the head left,
        // re-syncing the projection after each move like the render loop does.
        block.move_to(end, cx);
        block.refresh_inline_projection_for_focus(true);
        for target in (0..end).rev() {
            block.select_to(target, cx);
            block.refresh_inline_projection_for_focus(true);
            assert_eq!(
                block.selected_range,
                target..end,
                "RTL selection anchor must stay at the block end (head {target})"
            );
            assert!(block.selection_reversed);
        }
    });
}

#[rgpui::test]
async fn typing_destination_into_empty_link_parens_keeps_caret_inside(cx: &mut TestAppContext) {
    // Fixes an edge case where batched auto-pair macro `()+Left` caused first character typed
    // into `()` of link to snap the caret past `)` with rest of URL landing outside the link.
    let block = cx.new(|cx| {
        Block::with_record(
            cx,
            BlockRecord::new(
                BlockKind::Paragraph,
                InlineTextTree::from_markdown("[GitHub]"),
            ),
        )
    });

    block.update(cx, |block, cx| {
        // Auto-pair the `()` after the label, then drop the caret between them.
        block.selected_range = 8..8;
        block.refresh_inline_projection_for_focus(true);
        block.replace_text_in_visible_range(8..8, "()", None, false, cx);
        block.refresh_inline_projection_for_focus(true);
        let between = block.rendered_text().find(')').expect("closing paren");
        block.selected_range = between..between;
        block.refresh_inline_projection_for_focus(true);
        for ch in "https://github.com".chars() {
            let at = block.selected_range.clone();
            block.replace_text_in_visible_range(at, &ch.to_string(), None, false, cx);
            block.refresh_inline_projection_for_focus(true);
        }
    });

    block.read_with(cx, |block, _cx| {
        assert_eq!(
            block.record.title.serialize_markdown(),
            "[GitHub](https://github.com)"
        );
        assert_eq!(
            block.inline_link_at(1).map(str::to_string),
            Some("https://github.com".to_string())
        );
        // Caret stays inside `()`, just before the closing `)`.
        let close = block.rendered_text().find(')').expect("closing paren");
        assert_eq!(block.selected_range, close..close);
    });
}

#[rgpui::test]
async fn double_click_selects_word_at_offset(cx: &mut TestAppContext) {
    let block =
        cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("hello world rust".to_string())));

    // 词中、词首、词尾空格分别选中对应片段。
    for (offset, expected) in [(7, 6..11), (6, 6..11), (11, 11..12), (15, 12..16)] {
        block.update(cx, |block, cx| {
            block.select_word_at_offset(offset, cx);
        });
        assert_eq!(
            block.read_with(cx, |block, _cx| block.selected_range.clone()),
            expected,
            "offset {offset}"
        );
    }
}

#[rgpui::test]
async fn double_click_on_cjk_selects_single_character(cx: &mut TestAppContext) {
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("你好世界".to_string())));

    // 多字节字符中间的偏移向下取整到字符边界（UAX#29 下中文逐字成词）。
    block.update(cx, |block, cx| {
        block.select_word_at_offset(1, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        0..3
    );
}

#[rgpui::test]
async fn triple_click_selects_line_with_break(cx: &mut TestAppContext) {
    let block =
        cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("aa\nbbbb\ncc".to_string())));

    // 中间行附带换行符，末行不带。
    block.update(cx, |block, cx| {
        block.select_line_at_offset(5, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        3..8
    );

    block.update(cx, |block, cx| {
        block.select_line_at_offset(9, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        8..10
    );
}

#[rgpui::test]
async fn word_and_line_selection_noop_on_empty_block(cx: &mut TestAppContext) {
    let block = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph(String::new())));

    block.update(cx, |block, cx| {
        block.select_word_at_offset(0, cx);
        block.select_line_at_offset(0, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        0..0
    );
}

#[rgpui::test]
async fn double_click_drag_extends_by_word(cx: &mut TestAppContext) {
    let block =
        cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("hello world rust".to_string())));

    // 双击选中 world。
    block.update(cx, |block, cx| {
        block.select_word_at_offset(7, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        6..11
    );

    // 向右拖到 rust 中部：活动端吸附到词尾，锚点不动。
    block.update(cx, |block, cx| {
        block.extend_word_selection_to(13, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        6..16
    );
    assert!(!block.read_with(cx, |block, _cx| block.selection_reversed));

    // 向左拖过锚点：翻转，活动端吸附到词首。
    block.update(cx, |block, cx| {
        block.extend_word_selection_to(2, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        0..6
    );
    assert!(block.read_with(cx, |block, _cx| block.selection_reversed));

    // 再向右拖过锚点：翻回正向。
    block.update(cx, |block, cx| {
        block.extend_word_selection_to(14, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        6..16
    );
    assert!(!block.read_with(cx, |block, _cx| block.selection_reversed));
}

#[rgpui::test]
async fn triple_click_drag_extends_by_line(cx: &mut TestAppContext) {
    let block =
        cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("aa\nbbbb\ncc".to_string())));

    // 三击选中中间行（含换行符）。
    block.update(cx, |block, cx| {
        block.select_line_at_offset(5, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        3..8
    );

    // 向下拖到末行：扩展到末行尾。
    block.update(cx, |block, cx| {
        block.extend_line_selection_to(9, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        3..10
    );

    // 向上拖过锚点：翻转到首行。
    block.update(cx, |block, cx| {
        block.extend_line_selection_to(1, cx);
    });
    assert_eq!(
        block.read_with(cx, |block, _cx| block.selected_range.clone()),
        0..3
    );
    assert!(block.read_with(cx, |block, _cx| block.selection_reversed));
}
