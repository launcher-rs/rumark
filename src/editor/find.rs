//! 文档内查找栏：全局 `Ctrl+F` 打开，输入即搜，回车跳转，`Esc` 关闭。
//!
//! 匹配按源码偏移计算（大小写不敏感），经当前视图选区快照定位，
//! 渲染/源码双模式复用同一跳转路径。匹配上限截断，避免大文档卡顿。

use std::ops::Range;
use std::time::Duration;

use rgpui::input_ui::{Input, InputEvent, InputState};
use rgpui::*;

use super::Editor;
use crate::components::{FindInDocument, update_entity_forgiving};
use crate::i18n::I18nManager;
use crate::theme::{ThemeDimensions, ThemeTypography};

/// 单次查找返回的最大匹配数（超出截断，计数按实际返回量显示）。
const MAX_FIND_MATCHES: usize = 2000;
/// 输入后延迟重搜的时长：连续逐字输入只算最后一次，避免每键全量扫描。
const FIND_DEBOUNCE: Duration = Duration::from_millis(120);

/// 上次重搜使用的键：文档未变、查询与选项未变时直接复用命中。
type FindSearchKey = (u64, bool, String, bool, bool);

/// 查找栏状态：查询/替换输入框、匹配选项与当前匹配序号。
pub(super) struct FindBarState {
    pub query: Entity<InputState>,
    pub replace: Entity<InputState>,
    pub match_case: bool,
    pub whole_word: bool,
    pub matches: Vec<Range<usize>>,
    pub index: usize,
    /// 未执行的延迟重搜任务；新输入会取消上一条。
    debounce_task: Option<Task<()>>,
    /// 上次重搜的键，用于跳过文档/查询都未变时的全量扫描。
    search_key: Option<FindSearchKey>,
}

impl Editor {
    /// 按当前选中文本播种查询词（单行、截断），打开查找栏并聚焦。
    pub(super) fn open_find_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.link_dialog.is_some() || self.table_insert_dialog.is_some() {
            return;
        }
        let seed = self
            .selected_markdown_text(cx)
            .map(|text| {
                text.lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(64)
                    .collect::<String>()
            })
            .filter(|seed| !seed.is_empty())
            .unwrap_or_default();
        if let Some(bar) = self.find_bar.as_ref() {
            let query = bar.query.clone();
            if !seed.is_empty() {
                query.update(cx, |state, cx| {
                    state.set_value(seed.as_str(), window, cx);
                });
            }
            self.refresh_find_matches(cx);
            query.update(cx, |state, cx| {
                let len = state.value().len();
                state.set_selected_range(0..len, cx);
                state.focus(window, cx);
            });
            cx.notify();
            return;
        }
        let query = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(seed.as_str())
                .submit_on_enter(true)
        });
        let replace = cx.new(|cx| InputState::new(window, cx).submit_on_enter(true));
        cx.subscribe(&query, Self::on_find_input_event).detach();
        cx.subscribe(&replace, Self::on_find_input_event).detach();
        self.find_bar = Some(FindBarState {
            query: query.clone(),
            replace,
            match_case: false,
            whole_word: false,
            matches: Vec::new(),
            index: 0,
            debounce_task: None,
            search_key: None,
        });
        self.refresh_find_matches(cx);
        query.update(cx, |state, cx| {
            let len = state.value().len();
            state.set_selected_range(0..len, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// 关闭查找栏并把焦点还给原聚焦块。
    pub(super) fn close_find_bar(&mut self, cx: &mut Context<Self>) {
        if self.find_bar.take().is_some() {
            self.pending_focus = self.active_entity_id.or(self.pending_focus);
            cx.notify();
        }
    }

    /// 快捷键动作：打开查找栏（已打开时聚焦输入框）。
    pub(super) fn on_find_action(
        &mut self,
        _: &FindInDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find_bar(window, cx);
    }

    /// 输入框事件分发：查询框回车跳转/输入重搜；替换框回车替换（`Shift` 全部替换）。
    fn on_find_input_event(
        &mut self,
        input: Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        let is_query = self
            .find_bar
            .as_ref()
            .is_some_and(|bar| bar.query.entity_id() == input.entity_id());
        match event {
            InputEvent::PressEnter { shift, .. } => {
                if is_query {
                    // 防抖未执行时先同步重算并停在光标处首个匹配，避免「第一次
                    // 回车直接越过它」；已是最新命中则只按方向移动，不再重复扫全篇。
                    if self.ensure_find_matches(cx) {
                        self.jump_to_find_index(cx);
                    } else if *shift {
                        self.goto_find_match(-1, cx);
                    } else {
                        self.goto_find_match(1, cx);
                    }
                } else if *shift {
                    self.replace_all_find_matches(cx);
                } else {
                    self.replace_current_find_match(cx);
                }
            }
            InputEvent::Change if is_query => {
                self.schedule_find_matches_refresh(cx);
            }
            _ => {}
        }
    }

    /// 取当前查询文本与匹配选项（查找栏关闭时返回空）。
    fn find_search_params(&self, cx: &App) -> Option<(String, bool, bool)> {
        self.find_bar.as_ref().map(|bar| {
            (
                bar.query.read(cx).value().to_string(),
                bar.match_case,
                bar.whole_word,
            )
        })
    }

    /// 输入变化后安排一次延迟重搜：连续逐字输入只执行最后一次。
    fn schedule_find_matches_refresh(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = self.find_bar.as_mut() else {
            return;
        };
        // Task 被 drop 即取消，保证只有最后一次输入会触发重搜。
        bar.debounce_task = None;
        let weak = cx.entity().downgrade();
        bar.debounce_task = Some(cx.spawn(
            async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
                cx.background_executor().timer(FIND_DEBOUNCE).await;
                let _ = update_entity_forgiving(&weak, cx, |editor, cx| {
                    editor.refresh_find_matches(cx);
                });
            },
        ));
        cx.notify();
    }

    /// 确保匹配与当前文档/查询一致；返回是否发生了重算。
    ///
    /// 文档指纹、查询与选项都未变时直接复用上次命中：光标移动或重复的输入
    /// 事件不再触发全量扫描。源码视图与渲染视图文本不同，键里带上模式。
    /// 不移动选区/视口，调用方决定是「停在首个匹配」还是「按方向移动」。
    fn ensure_find_matches(&mut self, cx: &mut Context<Self>) -> bool {
        let (query, match_case, whole_word, key) = match self.find_bar.as_ref() {
            Some(bar) => {
                let query = bar.query.read(cx).value().to_string();
                let is_source = self.view_mode == super::ViewMode::Source;
                let key = (
                    self.derived_document(cx).fingerprint(),
                    is_source,
                    query.clone(),
                    bar.match_case,
                    bar.whole_word,
                );
                (query, bar.match_case, bar.whole_word, key)
            }
            None => return false,
        };
        let already_current = self
            .find_bar
            .as_ref()
            .and_then(|bar| bar.search_key.as_ref())
            == Some(&key);
        if let Some(bar) = self.find_bar.as_mut() {
            // 复用与重算都收掉已无意义的防抖任务（drop Task 即取消）。
            bar.debounce_task = None;
            if already_current {
                return false;
            }
        }

        // 只有重算分支才取光标快照，复用路径不做无谓的选区换算。
        let cursor = self.capture_source_selection_snapshot(cx).range.start;
        // 复用派生缓存的共享文本，少一次整篇源码拷贝。
        let source = self.current_document_source_arc(cx);
        let matches = Self::compute_find_matches(source.as_ref(), &query, match_case, whole_word);
        let index = matches
            .iter()
            .position(|range| range.start >= cursor)
            .unwrap_or(0);
        if let Some(bar) = self.find_bar.as_mut() {
            bar.matches = matches;
            bar.index = index;
            bar.search_key = Some(key);
        }
        true
    }

    /// 重算（或复用）匹配并定位到光标处首个匹配。
    fn refresh_find_matches(&mut self, cx: &mut Context<Self>) {
        if self.ensure_find_matches(cx) {
            self.jump_to_find_index(cx);
        }
    }

    /// 按方向移动到上/下一个匹配（循环回绕）。
    ///
    /// 命中未变化时直接复用 `bar.matches`，不再每次回车都扫描整篇文档。
    fn goto_find_match(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.find_bar.is_none() {
            return;
        }
        self.ensure_find_matches(cx);
        let Some(bar) = self.find_bar.as_mut() else {
            return;
        };
        let len = bar.matches.len();
        if len == 0 {
            bar.index = 0;
            cx.notify();
            return;
        }
        let current = bar.index.min(len - 1);
        bar.index = (current as i32 + delta).rem_euclid(len as i32) as usize;
        self.jump_to_find_index(cx);
    }

    /// 跳转到当前序号匹配：选中该区间并滚动到视口。
    fn jump_to_find_index(&mut self, cx: &mut Context<Self>) {
        let range = match self.find_bar.as_ref() {
            Some(bar) => bar.matches.get(bar.index).cloned(),
            None => None,
        };
        let Some(range) = range else {
            cx.notify();
            return;
        };
        self.apply_selection_snapshot_in_current_mode(
            &super::SelectionUndoSnapshot {
                range,
                reversed: false,
            },
            cx,
        );
        self.request_active_block_scroll_into_view(cx);
        cx.notify();
    }

    /// 点击「下一个」：跳到下一个匹配。
    pub(super) fn on_find_next(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.goto_find_match(1, cx);
    }

    /// 点击「上一个」：跳到上一个匹配。
    pub(super) fn on_find_prev(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.goto_find_match(-1, cx);
    }

    /// 点击关闭：关闭查找栏。
    pub(super) fn on_find_close(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_find_bar(cx);
    }

    /// 点击「替换」：替换当前匹配并停留在替换位置。
    pub(super) fn on_find_replace(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_current_find_match(cx);
    }

    /// 点击「全部替换」：一次性替换全部匹配（单次撤销）。
    pub(super) fn on_find_replace_all(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_all_find_matches(cx);
    }

    /// 替换当前匹配：重算匹配后替换序号处区间，锚点留在原地以便连续替换。
    fn replace_current_find_match(&mut self, cx: &mut Context<Self>) {
        // 锚点取自 `bar.matches`：按钮与回车两条路径都可能赶在防抖执行前触发，
        // 未刷新时会替换到旧查询的匹配上，因此入口先确保命中是最新的。
        self.ensure_find_matches(cx);
        let (replacement, anchor) = match self.find_bar.as_ref() {
            Some(bar) => (
                bar.replace.read(cx).value().to_string(),
                bar.matches.get(bar.index).map(|range| range.start),
            ),
            None => return,
        };
        let Some(anchor) = anchor else {
            cx.notify();
            return;
        };
        let source = self.serialized_document_text(cx);
        let Some((query, match_case, whole_word)) = self.find_search_params(cx) else {
            return;
        };
        let matches = Self::compute_find_matches(&source, &query, match_case, whole_word);
        let Some(position) = matches.iter().position(|range| range.start >= anchor) else {
            // 锚点后无匹配则回绕到首个：传空切片不会替换任何内容，与注释意图不符。
            // 无匹配时 `compute_find_matches` 返回空，取首个需先判空。
            if let Some(first) = matches.first() {
                let target = first.clone();
                self.apply_find_replacement(&source, &[target], &replacement, 0, cx);
            } else {
                cx.notify();
            }
            return;
        };
        let target = matches[position].clone();
        self.apply_find_replacement(&source, &[target], &replacement, anchor, cx);
    }

    /// 全部替换：从后向前应用全部匹配，一次撤销可整体还原。
    fn replace_all_find_matches(&mut self, cx: &mut Context<Self>) {
        // 下面会按当前查询重算全量匹配，无需先刷新 `bar.matches`；只需取消
        // 残留的防抖任务，避免替换完成后再触发一次多余的重搜与跳转。
        if let Some(bar) = self.find_bar.as_mut() {
            bar.debounce_task = None;
        }
        let replacement = match self.find_bar.as_ref() {
            Some(bar) => bar.replace.read(cx).value().to_string(),
            None => return,
        };
        let source = self.serialized_document_text(cx);
        let Some((query, match_case, whole_word)) = self.find_search_params(cx) else {
            return;
        };
        let matches = Self::compute_find_matches(&source, &query, match_case, whole_word);
        if matches.is_empty() {
            cx.notify();
            return;
        }
        let anchor = matches[0].start;
        self.apply_find_replacement(&source, &matches, &replacement, anchor, cx);
    }

    /// 把替换应用到文档：单次撤销捕获内重建文档树，刷新匹配并定位到锚点处。
    fn apply_find_replacement(
        &mut self,
        source: &str,
        targets: &[Range<usize>],
        replacement: &str,
        anchor: usize,
        cx: &mut Context<Self>,
    ) {
        use crate::components::UndoKind;

        let next = Self::apply_replacements_to_source(source, targets, replacement);
        self.prepare_undo_capture(UndoKind::NonCoalescible, cx);
        self.rebuild_after_cross_block_source_edit(next, cx);
        self.mark_dirty(cx);
        self.finalize_pending_undo_capture(cx);
        self.sync_table_axis_visuals(cx);
        // 替换后重算匹配（替换词可能仍命中查询），锚点就近定位。
        let Some((query, match_case, whole_word)) = self.find_search_params(cx) else {
            return;
        };
        let fresh = self.serialized_document_text(cx);
        let matches = Self::compute_find_matches(&fresh, &query, match_case, whole_word);
        let index = matches
            .iter()
            .position(|range| range.start >= anchor)
            .unwrap_or(0);
        // 同步写回搜索键：文档已变，不更新会让下一次跳转/替换白扫一遍全篇。
        let key = (
            self.derived_document(cx).fingerprint(),
            self.view_mode == super::ViewMode::Source,
            query,
            match_case,
            whole_word,
        );
        if let Some(bar) = self.find_bar.as_mut() {
            bar.matches = matches;
            bar.index = index;
            bar.search_key = Some(key);
        }
        self.jump_to_find_index(cx);
    }

    /// 纯函数：把多个（升序、互不重叠）区间替换为同一文本，从后向前应用使偏移有效。
    fn apply_replacements_to_source(
        source: &str,
        targets: &[Range<usize>],
        replacement: &str,
    ) -> String {
        let mut next = source.to_string();
        for range in targets.iter().rev() {
            if range.start <= range.end && range.end <= next.len() {
                next.replace_range(range.clone(), replacement);
            }
        }
        next
    }

    /// 渲染底部查找栏（内容区与状态栏之间，无匹配时计数位显示提示）。
    pub(super) fn render_find_bar(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let bar = self.find_bar.as_ref()?;
        let i18n = cx.global::<I18nManager>().snapshot();
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        let query = bar.query.clone();
        let count = if query.read(cx).value().trim().is_empty() {
            String::new()
        } else if bar.matches.is_empty() {
            i18n.t("find_no_results", &[])
        } else {
            let current = (bar.index + 1).to_string();
            let total = bar.matches.len().to_string();
            i18n.t(
                "find_count_template",
                &[("current", current.as_str()), ("total", total.as_str())],
            )
        };
        let prev_label = i18n.t("find_prev", &[]);
        let next_label = i18n.t("find_next", &[]);
        let case_label = i18n.t("find_match_case", &[]);
        let word_label = i18n.t("find_whole_word", &[]);
        let replace_label = i18n.t("find_replace", &[]);
        let replace_all_label = i18n.t("find_replace_all", &[]);
        let close_label = i18n.t("find_close", &[]);
        let replace = bar.replace.clone();
        // 匹配选项开启态用主按钮强调，关闭态用次按钮。
        let case_button = Button::new("find-match-case").label(case_label).compact();
        let case_button = if bar.match_case {
            case_button.primary()
        } else {
            case_button.secondary()
        };
        let word_button = Button::new("find-whole-word").label(word_label).compact();
        let word_button = if bar.whole_word {
            word_button.primary()
        } else {
            word_button.secondary()
        };
        Some(
            div()
                .w_full()
                .flex_shrink_0()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .px(px(d.editor_padding))
                .py(px(6.0))
                .bg(theme.popover)
                .border_t(px(d.dialog_border_width))
                .border_color(theme.border)
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(t.dialog_body_size))
                        .font_weight(t.dialog_button_weight)
                        .text_color(theme.muted_foreground)
                        .child(i18n.t("find_label", &[])),
                )
                .child(div().w(px(220.0)).child(Input::new(&query)))
                .child(
                    div()
                        .min_w(px(56.0))
                        .text_size(px(t.dialog_body_size))
                        .text_color(theme.muted_foreground)
                        .child(count),
                )
                .child(
                    Button::new("find-prev")
                        .label(prev_label)
                        .secondary()
                        .compact()
                        .on_click(cx.listener(Self::on_find_prev)),
                )
                .child(
                    Button::new("find-next")
                        .label(next_label)
                        .secondary()
                        .compact()
                        .on_click(cx.listener(Self::on_find_next)),
                )
                .child(case_button.on_click(cx.listener(Self::on_find_toggle_case)))
                .child(word_button.on_click(cx.listener(Self::on_find_toggle_word)))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(t.dialog_body_size))
                        .font_weight(t.dialog_button_weight)
                        .text_color(theme.muted_foreground)
                        .child(i18n.t("find_replace_label", &[])),
                )
                .child(div().w(px(160.0)).child(Input::new(&replace)))
                .child(
                    Button::new("find-replace")
                        .label(replace_label)
                        .secondary()
                        .compact()
                        .on_click(cx.listener(Self::on_find_replace)),
                )
                .child(
                    Button::new("find-replace-all")
                        .label(replace_all_label)
                        .secondary()
                        .compact()
                        .on_click(cx.listener(Self::on_find_replace_all)),
                )
                .child(
                    Button::new("find-close")
                        .label(close_label)
                        .secondary()
                        .compact()
                        .on_click(cx.listener(Self::on_find_close)),
                )
                .into_any_element(),
        )
    }

    /// 计算查询在源码中的全部匹配区间（上限截断）。
    ///
    /// 默认大小写不敏感；`match_case` 为真时逐字精确比较。逐字符窗口比较
    /// 天然与 `to_lowercase` 的字节长度变化无关，不会 panic，只做有效边界切片。
    /// `whole_word` 为真时要求匹配两侧均非词字符（字母数字或下划线，含中文）。
    pub(super) fn compute_find_matches(
        source: &str,
        query: &str,
        match_case: bool,
        whole_word: bool,
    ) -> Vec<Range<usize>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let query_cmp = if match_case {
            trimmed.to_string()
        } else {
            trimmed.to_lowercase()
        };
        let query_chars: Vec<char> = trimmed.chars().collect();
        if query_chars.is_empty() {
            return Vec::new();
        }
        let src_chars: Vec<char> = source.chars().collect();
        // 原文字符序号到字节偏移的映射（末尾哨兵为文本长度）。
        let mut char_bytes: Vec<usize> = Vec::with_capacity(src_chars.len() + 1);
        for (byte, _) in source.char_indices() {
            char_bytes.push(byte);
        }
        char_bytes.push(source.len());
        if query_chars.len() > src_chars.len() {
            return Vec::new();
        }
        let first = query_cmp.chars().next().expect("query non-empty");
        let mut matches = Vec::new();
        // 候选窗口复用同一缓冲区；`str::to_lowercase` 等价于逐字符
        // `char::to_lowercase` 展开，逐字符比较可再省掉一次 String 分配。
        let mut window = String::new();
        let mut index = 0;
        while index + query_chars.len() <= src_chars.len() && matches.len() < MAX_FIND_MATCHES {
            // 首字符快速过滤，避免每个位置都构造窗口。
            let head_matches = if match_case {
                src_chars[index] == query_chars[0]
            } else {
                src_chars[index] == query_chars[0]
                    || src_chars[index].to_lowercase().next() == Some(first)
            };
            if head_matches {
                window.clear();
                window.extend(src_chars[index..index + query_chars.len()].iter());
                // 字符边界来自 `char_indices` 推导，必为有效边界。
                let matched = if match_case {
                    window == query_cmp
                } else {
                    window
                        .chars()
                        .flat_map(char::to_lowercase)
                        .eq(query_cmp.chars())
                };
                if matched
                    && (!whole_word
                        || Self::find_match_has_word_boundaries(
                            &src_chars,
                            index,
                            query_chars.len(),
                        ))
                {
                    matches.push(char_bytes[index]..char_bytes[index + query_chars.len()]);
                    index += query_chars.len();
                    continue;
                }
            }
            index += 1;
        }
        matches
    }

    /// 判断候选匹配两侧是否为词边界（字母数字与下划线为词字符，含中文）。
    fn find_match_has_word_boundaries(src_chars: &[char], start: usize, len: usize) -> bool {
        let left_ok = start
            .checked_sub(1)
            .and_then(|prev| src_chars.get(prev))
            .is_none_or(|ch| !is_find_word_char(*ch));
        let right_ok = src_chars
            .get(start + len)
            .is_none_or(|ch| !is_find_word_char(*ch));
        left_ok && right_ok
    }

    /// 点击匹配选项：翻转后即时重搜。
    fn toggle_find_option(&mut self, case: bool, cx: &mut Context<Self>) {
        {
            let Some(bar) = self.find_bar.as_mut() else {
                return;
            };
            if case {
                bar.match_case = !bar.match_case;
            } else {
                bar.whole_word = !bar.whole_word;
            }
        }
        self.refresh_find_matches(cx);
    }

    /// 点击「区分大小写」开关。
    pub(super) fn on_find_toggle_case(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_find_option(true, cx);
    }

    /// 点击「全词匹配」开关。
    pub(super) fn on_find_toggle_word(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_find_option(false, cx);
    }
}

/// 判断字符是否为全词匹配意义下的词字符（字母数字与下划线，含中文）。
fn is_find_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::Editor;
    use rgpui::{AppContext as _, TestAppContext};

    /// 与 `editor::tests` 一致的测试应用初始化。
    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::i18n::I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
    }

    #[test]
    fn find_matches_keep_unicode_case_folding_semantics() {
        // 逐字符 lowercase 比较应与旧的 `String::to_lowercase` 完全一致。
        let matches = Editor::compute_find_matches("Straße STRASSE", "straße", false, false);
        assert_eq!(matches, vec![0..7]);
    }

    #[rgpui::test]
    async fn find_input_is_debounced_and_reuses_matches(cx: &mut TestAppContext) {
        init(cx);
        let (editor, cx) = cx.add_window_view(|window, cx| {
            let mut editor = Editor::from_markdown(cx, "alpha beta alpha".to_string(), None);
            let query = cx.new(|cx| {
                rgpui::input_ui::InputState::new(window, cx)
                    .default_value("alpha")
                    .submit_on_enter(true)
            });
            let replace = cx.new(|cx| rgpui::input_ui::InputState::new(window, cx));
            editor.find_bar = Some(super::FindBarState {
                query,
                replace,
                match_case: false,
                whole_word: false,
                matches: Vec::new(),
                index: 0,
                debounce_task: None,
                search_key: None,
            });
            editor
        });

        // 模拟 `InputEvent::Change`：只排队，不立即全量重搜。
        editor.update(cx, |editor, cx| {
            editor.schedule_find_matches_refresh(cx);
            let bar = editor.find_bar.as_ref().expect("查找栏应已打开");
            assert!(bar.debounce_task.is_some(), "应存在未执行的防抖任务");
            assert!(bar.matches.is_empty(), "防抖期内不应更新命中");
        });
        // 连续输入再排一次，旧任务应被取消而不是叠加。
        editor.update(cx, |editor, cx| {
            editor.schedule_find_matches_refresh(cx);
            assert!(editor.find_bar.as_ref().unwrap().debounce_task.is_some());
        });

        // 时钟推进后执行最后一次重搜。
        cx.executor().advance_clock(super::FIND_DEBOUNCE);
        cx.run_until_parked();
        editor.update(cx, |editor, _cx| {
            let bar = editor.find_bar.as_ref().expect("查找栏应仍打开");
            assert_eq!(bar.matches.len(), 2, "应命中两处 alpha");
            assert!(bar.debounce_task.is_none(), "重搜完成后任务应被清空");
            assert!(bar.search_key.is_some(), "应记录本次重搜的键");
        });

        // 文档、查询、选项都未变时直接复用命中，不再触发全量扫描。
        editor.update(cx, |editor, cx| {
            let before = editor.find_bar.as_ref().unwrap().matches.clone();
            editor.refresh_find_matches(cx);
            assert_eq!(editor.find_bar.as_ref().unwrap().matches, before);
        });

        cx.quit();
    }

    #[rgpui::test]
    async fn find_enter_stops_at_first_match_and_replace_refreshes_search_key(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let (editor, cx) = cx.add_window_view(|window, cx| {
            let mut editor = Editor::from_markdown(cx, "alpha beta alpha".to_string(), None);
            let query =
                cx.new(|cx| rgpui::input_ui::InputState::new(window, cx).default_value("alpha"));
            let replace =
                cx.new(|cx| rgpui::input_ui::InputState::new(window, cx).default_value("x"));
            editor.find_bar = Some(super::FindBarState {
                query,
                replace,
                match_case: false,
                whole_word: false,
                matches: Vec::new(),
                index: 0,
                debounce_task: None,
                search_key: None,
            });
            editor
        });

        // 防抖未执行时按下回车：ensure 先收敛命中，并停在光标处首个匹配。
        editor.update(cx, |editor, cx| {
            assert!(editor.ensure_find_matches(cx), "首次应收敛并重算");
            let bar = editor.find_bar.as_ref().expect("查找栏应已打开");
            assert_eq!(bar.matches.len(), 2);
            assert_eq!(bar.index, 0, "应停在首个匹配，而不是越过它");
        });

        // 命中未变化时只复用，索引保持不动。
        editor.update(cx, |editor, cx| {
            assert!(!editor.ensure_find_matches(cx), "命中未变不应重算");
            assert_eq!(editor.find_bar.as_ref().unwrap().index, 0);
        });

        // 替换当前匹配后必须刷新搜索键，否则下一次交互会白扫一遍全篇。
        editor.update(cx, |editor, cx| {
            editor.replace_current_find_match(cx);
            let bar = editor.find_bar.as_ref().expect("查找栏应已打开");
            assert!(bar.search_key.is_some(), "替换后应写回新的搜索键");
            assert_eq!(bar.matches.len(), 1, "替换后应剩一处 alpha");
        });
        editor.update(cx, |editor, cx| {
            assert!(!editor.ensure_find_matches(cx), "替换后不应再全量重扫");
        });

        cx.quit();
    }

    #[test]
    fn find_matches_are_case_insensitive_and_bounded() {
        let matches = Editor::compute_find_matches("Hello hello HELLO", "hello", false, false);
        assert_eq!(matches, vec![0..5, 6..11, 12..17]);
    }

    #[test]
    fn find_matches_support_cjk_and_empty_query() {
        let matches = Editor::compute_find_matches("你好世界你好", "你好", false, false);
        assert_eq!(matches.len(), 2);
        assert!(Editor::compute_find_matches("alpha", "  ", false, false).is_empty());
        assert!(Editor::compute_find_matches("a", "abcdef", false, false).is_empty());
    }

    #[test]
    fn find_matches_do_not_overlap() {
        let matches = Editor::compute_find_matches("aaa", "aa", false, false);
        assert_eq!(matches, vec![0..2]);
    }

    #[test]
    fn find_match_case_option_restricts_to_exact_case() {
        let sensitive = Editor::compute_find_matches("Hello hello", "hello", true, false);
        assert_eq!(sensitive, vec![6..11]);
        let insensitive = Editor::compute_find_matches("Hello hello", "hello", false, false);
        assert_eq!(insensitive, vec![0..5, 6..11]);
    }

    #[test]
    fn find_whole_word_option_skips_partial_words() {
        let matches = Editor::compute_find_matches("he hello the", "he", false, true);
        assert_eq!(matches, vec![0..2]);
        // 中文按字成词：相邻汉字视为词字符，不单独命中。
        let cjk = Editor::compute_find_matches("你好世界", "你好", false, true);
        assert!(cjk.is_empty());
        let cjk_single = Editor::compute_find_matches("你好", "你好", false, true);
        assert_eq!(cjk_single, vec![0.."你好".len()]);
    }

    #[test]
    fn replacements_apply_from_back_to_keep_offsets_valid() {
        let source = "foo bar foo";
        let matches = Editor::compute_find_matches(source, "foo", false, false);
        assert_eq!(matches, vec![0..3, 8..11]);
        assert_eq!(
            Editor::apply_replacements_to_source(source, &matches, "baz"),
            "baz bar baz"
        );
        assert_eq!(
            Editor::apply_replacements_to_source(source, &matches[..1], "baz"),
            "baz bar foo"
        );
        // 越界区间被忽略，不 panic。
        let out_of_bounds = 0..99;
        assert_eq!(
            Editor::apply_replacements_to_source(source, std::slice::from_ref(&out_of_bounds), "x"),
            source
        );
    }
}
