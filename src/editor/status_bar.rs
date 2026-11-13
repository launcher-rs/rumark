//! 底部状态栏条目组装：把偏好与实时状态转为框架状态栏的左右条目。
//!
//! 框架 `StatusBar` 为纯值驱动（每帧直接收条目，无实体同步），
//! 本模块只负责按偏好组装条目。字数统计（含单测）保留在此。

use rgpui::components::{StatusBar, StatusBarItem};
use rgpui::*;

use super::Editor;
use crate::config::preferences::StatusBarPreferences;
use crate::i18n::I18nSnapshot;

impl Editor {
    /// 状态栏是否启用（偏好门控）。
    pub(super) fn status_bar_enabled(&self, cx: &App) -> bool {
        self.status_bar_preferences(cx).enabled
    }

    /// 读取当前状态栏偏好设置。
    fn status_bar_preferences(&self, cx: &App) -> StatusBarPreferences {
        crate::config::preferences::EditorSettings::status_bar_preferences(cx)
    }

    /// 按偏好与实时状态组装框架状态栏（纯值驱动，每帧直接构造）。
    pub(super) fn status_bar_element(
        &self,
        i18n: &I18nSnapshot,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let prefs = self.status_bar_preferences(cx);
        let editor = cx.entity().downgrade();

        let mut left_items = Vec::new();
        if prefs.show_sidebar_toggle {
            let editor = editor.clone();
            left_items.push(
                StatusBarItem::new(i18n.t("status_files", &[]))
                    .id("sidebar-toggle")
                    .on_click(move |_, window, cx| {
                        if let Some(editor) = editor.upgrade() {
                            editor.update(cx, |editor, cx| {
                                editor.toggle_workspace_drawer(window, cx);
                            });
                        }
                    }),
            );
        }

        if prefs.show_mode_switch {
            let label = match self.view_mode {
                super::ViewMode::Source => i18n.t("status_mode_rendered", &[]),
                super::ViewMode::Rendered => i18n.t("status_mode_source", &[]),
            };
            let editor = editor.clone();
            left_items.push(StatusBarItem::new(label).id("mode-switch").on_click(
                move |_, _, cx| {
                    if let Some(editor) = editor.upgrade() {
                        editor.update(cx, |editor, cx| editor.toggle_view_mode_from_ui(cx));
                    }
                },
            ));
        }

        let mut right_items = Vec::new();
        if prefs.show_cursor_position || prefs.show_word_count {
            let stats = self.status_stats(cx);
            if prefs.show_cursor_position {
                let (line, col) = stats.cursor_position;
                right_items
                    .push(StatusBarItem::new(format!("{line} : {col}")).id("cursor-position"));
            }

            if prefs.show_word_count {
                let suffix = i18n.t("status_word_count_suffix", &[]);
                let label = match stats.selection_words {
                    Some(selection_count) => {
                        format!("{selection_count} / {} {suffix}", stats.total_words)
                    }
                    None => format!("{} {suffix}", stats.total_words),
                };
                right_items.push(StatusBarItem::new(label).id("word-count").muted(true));
            }
        }

        for button in &prefs.custom_buttons {
            right_items.push(StatusBarItem::new(button.label.clone()).id(button.id.clone()));
        }

        StatusBar::new().left(left_items).right(right_items)
    }

    /// 取状态栏统计：字数只在内容指纹变化时重算，行列只在指纹或光标偏移
    /// 变化时重算，选中字数只在指纹或选区状态变化时重算。空闲帧（仅光标
    /// 闪烁）命中缓存后不做任何全文扫描。
    pub(super) fn status_stats(&self, cx: &App) -> StatusStats {
        let fingerprint = self.document_fingerprint(cx);
        let caret_offset = self.capture_source_selection_snapshot(cx).range.end;
        let selection_key = self.selection_fingerprint(cx);

        let mut cached = self.status_stats_cache.borrow().clone();
        let reusable = cached.as_ref().is_some_and(|stats| {
            stats.fingerprint == fingerprint && stats.view_mode == self.view_mode
        });
        if reusable {
            let stats = cached.as_mut().expect("reusable 蕴含 Some");
            if stats.cursor_offset != caret_offset {
                let derived = self.derived_document(cx);
                stats.cursor_offset = caret_offset;
                stats.cursor_position = cursor_position_in(self.view_text(&derived), caret_offset);
            }
            if stats.selection_key != selection_key {
                stats.selection_key = selection_key;
                stats.selection_words = self.selected_markdown_text(cx).as_deref().map(word_count);
            }
        } else {
            let derived = self.derived_document(cx);
            let text = self.view_text(&derived);
            count_total_word_count_run();
            cached = Some(StatusStats {
                fingerprint,
                view_mode: self.view_mode,
                total_words: word_count(text),
                cursor_offset: caret_offset,
                cursor_position: cursor_position_in(text, caret_offset),
                selection_key,
                selection_words: self.selected_markdown_text(cx).as_deref().map(word_count),
            });
        }

        let stats = cached.expect("两条分支都会写入缓存值");
        *self.status_stats_cache.borrow_mut() = Some(stats.clone());
        stats
    }

    /// 当前视图模式对应的派生文本（源码模式读原始缓冲区）。
    fn view_text<'a>(&self, derived: &'a super::derive::DerivedDocument) -> &'a str {
        match self.view_mode {
            super::ViewMode::Source => &derived.raw_source,
            super::ViewMode::Rendered => &derived.markdown,
        }
    }

    /// 选区状态指纹：覆盖 `selected_markdown_text` 的全部输入（跨块选区
    /// 端点或各块非空选区），供选中字数缓存失效；与内容指纹相互独立。
    fn selection_fingerprint(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(selection) = &self.cross_block_selection {
            true.hash(&mut hasher);
            for endpoint in [&selection.anchor, &selection.focus] {
                endpoint.entity_id.hash(&mut hasher);
                endpoint.offset.hash(&mut hasher);
            }
        } else {
            false.hash(&mut hasher);
            for visible in self.document.visible_blocks() {
                let block = visible.entity.read(cx);
                if block.selected_range.is_empty() {
                    continue;
                }
                visible.entity.entity_id().hash(&mut hasher);
                block.selected_range.start.hash(&mut hasher);
                block.selected_range.end.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

/// 状态栏统计缓存值。三个失效键相互独立：内容指纹管字数，光标偏移管行列，
/// 选区指纹管选中字数，避免仅移动光标时重算全文字数。
#[derive(Clone)]
pub(super) struct StatusStats {
    fingerprint: u64,
    view_mode: super::ViewMode,
    pub(super) total_words: usize,
    cursor_offset: usize,
    pub(super) cursor_position: (usize, usize),
    selection_key: u64,
    pub(super) selection_words: Option<usize>,
}

/// 按源码文本与光标偏移计算行列号（均从 1 起）。
fn cursor_position_in(text: &str, caret_offset: usize) -> (usize, usize) {
    use unicode_segmentation::UnicodeSegmentation;

    let clamped = floor_char_boundary(text, caret_offset);
    let line = text[..clamped].matches('\n').count() + 1;
    let last_newline = text[..clamped].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = text[last_newline..clamped].graphemes(true).count() + 1;
    (line, col)
}

// 测试观测点：全文字数重算次数（生产构建不编译）。
#[cfg(test)]
thread_local! {
    static TOTAL_WORD_COUNT_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_total_word_count_run() {
    TOTAL_WORD_COUNT_RUNS.with(|runs| runs.set(runs.get() + 1));
}

#[cfg(not(test))]
fn count_total_word_count_run() {}

/// 读取并清零全文字数重算计数（测试辅助）。
#[cfg(test)]
pub(super) fn take_total_word_count_runs() -> usize {
    TOTAL_WORD_COUNT_RUNS.with(|runs| runs.replace(0))
}

/// 把字节下标收缩到不超过文本长度、且落在字符边界上的安全下标。
///
/// 光标偏移可能来自与当前文本不同源的快照（如渲染模式经源码映射换算），
/// 直接按字节切片可能落进多字节字符中间导致 panic，这里回退到最近的边界。
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 统计中拉混排文本的字数。
///
/// 每个中日韩字符计一词，拉丁词按空白切分。
pub fn word_count(text: &str) -> usize {
    let mut count = 0;
    let mut in_latin_word = false;

    for ch in text.chars() {
        if is_cjk_char(ch) {
            if in_latin_word {
                count += 1;
                in_latin_word = false;
            }
            count += 1;
        } else if ch.is_whitespace() {
            if in_latin_word {
                count += 1;
                in_latin_word = false;
            }
        } else {
            in_latin_word = true;
        }
    }
    if in_latin_word {
        count += 1;
    }
    count
}

/// 判断字符是否为中日韩字符。
fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch as u32,
        // 中日韩统一表意文字
        0x4E00..=0x9FFF
        // 中日韩统一表意文字扩展区 A
        | 0x3400..=0x4DBF
        // 中日韩统一表意文字扩展区 B
        | 0x20000..=0x2A6DF
        // 中日韩兼容表意文字
        | 0xF900..=0xFAFF
        // 中日韩部首补充／康熙部首
        | 0x2E80..=0x2EFF
        | 0x2F00..=0x2FDF
        // 平假名／片假名（日文）
        | 0x3040..=0x309F
        | 0x30A0..=0x30FF
        // 谚文音节（韩文）
        | 0xAC00..=0xD7AF
    )
}

#[cfg(test)]
mod tests {
    use super::{floor_char_boundary, take_total_word_count_runs, word_count};
    use rgpui::{AppContext, TestAppContext};

    use super::super::{CrossBlockSelection, CrossBlockSelectionEndpoint, Editor};
    use crate::components::InlineTextTree;
    use crate::i18n::I18nManager;

    /// 初始化状态栏测试应用（测试辅助）。
    fn init_editor_test_app(cx: &mut TestAppContext) {
        cx.update(|cx| {
            I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
    }

    #[test]
    fn empty_text_has_zero_words() {
        assert_eq!(word_count(""), 0);
    }

    #[test]
    fn english_words_are_counted() {
        assert_eq!(word_count("hello world"), 2);
        assert_eq!(word_count("one two three four"), 4);
    }

    #[test]
    fn cjk_characters_are_counted_individually() {
        assert_eq!(word_count("你好世界"), 4);
        assert_eq!(word_count("中文"), 2);
    }

    #[test]
    fn mixed_cjk_and_english() {
        assert_eq!(word_count("hello 世界"), 3);
        assert_eq!(word_count("你好 world foo"), 4);
    }

    #[test]
    fn whitespace_handling() {
        assert_eq!(word_count("  hello   world  "), 2);
        assert_eq!(word_count("   "), 0);
    }

    #[test]
    fn floor_char_boundary_clamps_to_char_edges() {
        // "嵌" 占 3 字节（1..4），下标落在其中间时回退到字符起点。
        let text = "a嵌b";
        assert_eq!(floor_char_boundary(text, 1), 1);
        assert_eq!(floor_char_boundary(text, 2), 1);
        assert_eq!(floor_char_boundary(text, 3), 1);
        assert_eq!(floor_char_boundary(text, 4), 4);
        assert_eq!(floor_char_boundary(text, 5), 5);
        // 超出长度时收缩到末尾边界。
        assert_eq!(floor_char_boundary(text, 999), text.len());
        assert_eq!(floor_char_boundary("", 5), 0);
    }

    #[test]
    fn status_stats_word_count_updates_after_edit() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let before = editor.status_stats(cx);
            assert_eq!(before.total_words, 2);
            assert_eq!(before.selection_words, None);

            let first = editor.document.visible_blocks()[0].entity.clone();
            first.update(cx, |block, _cx| {
                block
                    .record
                    .set_title(InlineTextTree::plain("changed gamma"));
            });
            let after = editor.status_stats(cx);
            assert_eq!(after.total_words, 3, "输入后字数统计应更新");
        });
        cx.quit();
    }

    #[test]
    fn status_stats_caret_move_skips_word_count() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            // 源码模式的光标直接落在原始缓冲区上，便于精确移动。
            editor.toggle_view_mode(cx);
            let root = editor.document.first_root().expect("源码模式应有根块");
            root.update(cx, |block, _cx| {
                block.selected_range = 2..2;
                block.selection_reversed = false;
            });
            take_total_word_count_runs();
            let first = editor.status_stats(cx);
            assert_eq!(take_total_word_count_runs(), 1);
            assert_eq!(first.cursor_position, (1, 3));

            root.update(cx, |block, _cx| {
                block.selected_range = 4..4;
            });
            let second = editor.status_stats(cx);
            assert_eq!(
                take_total_word_count_runs(),
                0,
                "仅移动光标不应重算全文字数"
            );
            assert_eq!(second.cursor_position, (1, 5), "行列应跟随光标");
            assert_eq!(second.total_words, first.total_words);
        });
        cx.quit();
    }

    #[test]
    fn status_stats_selection_words_follow_selection_only() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let visible = editor.document.visible_blocks().to_vec();
            let anchor = CrossBlockSelectionEndpoint {
                entity_id: visible[0].entity.entity_id(),
                offset: 0,
            };
            let focus = CrossBlockSelectionEndpoint {
                entity_id: visible[1].entity.entity_id(),
                offset: 2,
            };
            editor.cross_block_selection = Some(CrossBlockSelection { anchor, focus });
            take_total_word_count_runs();
            let selected = editor.status_stats(cx);
            assert_eq!(selected.selection_words, Some(2), "选中 alpha 与 be 共两词");
            assert_eq!(take_total_word_count_runs(), 1);

            editor.cross_block_selection = None;
            let cleared = editor.status_stats(cx);
            assert_eq!(cleared.selection_words, None);
            assert_eq!(take_total_word_count_runs(), 0, "选区变化不应重算全文字数");
        });
        cx.quit();
    }
}
