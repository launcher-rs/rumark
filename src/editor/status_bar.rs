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
        if prefs.show_cursor_position {
            let (line, col) = self.compute_cursor_position(cx);
            right_items.push(StatusBarItem::new(format!("{line} : {col}")).id("cursor-position"));
        }

        if prefs.show_word_count {
            let text = self.serialized_document_text(cx);
            let total_count = word_count(&text);
            let suffix = i18n.t("status_word_count_suffix", &[]);
            let label = match self.selected_markdown_text(cx).as_deref().map(word_count) {
                Some(selection_count) => {
                    format!("{selection_count} / {total_count} {suffix}")
                }
                None => format!("{total_count} {suffix}"),
            };
            right_items.push(StatusBarItem::new(label).id("word-count").muted(true));
        }

        for button in &prefs.custom_buttons {
            right_items.push(StatusBarItem::new(button.label.clone()).id(button.id.clone()));
        }

        StatusBar::new().left(left_items).right(right_items)
    }

    /// 返回当前光标的行列号（均为从 1 起）：源码模式读原始缓冲区，
    /// 渲染模式经选区快照换算为源码偏移后同样按源码行列展示。
    fn compute_cursor_position(&self, cx: &App) -> (usize, usize) {
        use unicode_segmentation::UnicodeSegmentation;

        let (text, caret_offset) = if self.view_mode == super::ViewMode::Source {
            let snapshot = self.capture_source_selection_snapshot(cx);
            (self.document.raw_source_text(cx), snapshot.range.end)
        } else {
            let snapshot = self.capture_source_selection_snapshot(cx);
            (self.serialized_document_text(cx), snapshot.range.end)
        };
        let clamped = floor_char_boundary(&text, caret_offset);

        let line = text[..clamped].matches('\n').count() + 1;
        let last_newline = text[..clamped].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = text[last_newline..clamped].graphemes(true).count() + 1;
        (line, col)
    }
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
    use super::{floor_char_boundary, word_count};

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
}
