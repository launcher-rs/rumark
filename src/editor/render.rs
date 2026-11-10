//! 编辑器窗口渲染：居中可滚动块列、未保存对话框、自定义滚动条
//! 与延迟操作（焦点、滚动、保存、窗口标题）。

use std::time::{Duration, Instant};

use rgpui::*;

use super::{Editor, MountedRow};
use crate::components::CalloutVariant;
use crate::components::{Block, NoRecentFiles, update_entity_forgiving};
use crate::i18n::{I18nManager, I18nSnapshot};
use crate::theme::{ThemeDimensions, ThemeTypography};
use rgpui::menu::{ContextMenuExt, PopupMenu};
use rgpui::prelude::FluentBuilder as _;
use rgpui::title_bar::{TITLE_BAR_LEFT_PADDING, TitleBar};

pub(crate) const GITHUB_URL: &str = "https://github.com/launcher-rs/rumark";

/// 距视口此像素内的行保持挂载，快速滚动时提前绘制，避免露出空白边缘。
const OVERDRAW_PX: f32 = 800.0;

/// 在浏览器中打开关于页的仓库链接。
pub(crate) fn open_about_github_url(cx: &mut App) {
    cx.open_url(GITHUB_URL);
}

/// 构造编辑器文本字体（含藏文回退字体）。
fn editor_text_font() -> Font {
    // 字体回退表内部为引用计数向量，每进程构建一次、每帧克隆引用，
    // 因 `editor_text_font()` 在每帧渲染中调用。
    static FALLBACKS: std::sync::OnceLock<FontFallbacks> = std::sync::OnceLock::new();
    let fallbacks = FALLBACKS
        .get_or_init(|| {
            FontFallbacks::from_fonts(tibetan_font_fallbacks_for_target_os(std::env::consts::OS))
        })
        .clone();
    let mut font = font(".SystemUIFont");
    font.fallbacks = Some(fallbacks);
    font
}

/// 按目标系统返回藏文回退字体族列表。
fn tibetan_font_fallbacks_for_target_os(target_os: &str) -> Vec<String> {
    let families = match target_os {
        "windows" => &[
            "Microsoft Himalaya",
            "Noto Serif Tibetan",
            "Noto Sans Tibetan",
            "BabelStone Tibetan",
        ][..],
        "macos" => &["Kailasa", "Noto Serif Tibetan", "Noto Sans Tibetan"][..],
        _ => &[
            "Noto Serif Tibetan",
            "Noto Sans Tibetan",
            "Microsoft Himalaya",
            "Kailasa",
            "BabelStone Tibetan",
        ][..],
    };
    families
        .iter()
        .map(|family| (*family).to_string())
        .collect()
}

/// 一行内的分段：普通块，或共用脚注外壳的连续块。
enum RowSegment {
    /// 单个块独占一行。
    Block(usize),
    /// 连续脚注块区间（含头，不含尾）。
    FootnoteGroup { start: usize, end: usize },
}

/// 一个渲染行的分组计划。先完成分组，元素只为视口内的行构建。
enum RowPlan {
    /// 单个块独占一行。
    Single { block: usize, top_gap: f32 },
    /// 连续脚注块共用一个脚注外壳。
    Footnote {
        start: usize,
        end: usize,
        top_gap: f32,
    },
    /// 提示框外壳内的若干分块（其中可再含脚注子组）。
    Callout {
        start: usize,
        top_gap: f32,
        variant: CalloutVariant,
        segments: Vec<RowSegment>,
    },
}

impl RowPlan {
    /// 行首可见块下标，用作焦点行定位与行足迹的缓存键。
    fn start(&self) -> usize {
        match self {
            RowPlan::Single { block, .. } => *block,
            RowPlan::Footnote { start, .. } | RowPlan::Callout { start, .. } => *start,
        }
    }

    /// 行前导边距，占位高度需要减去挂载行自行应用的那一份。
    fn top_gap(&self) -> f32 {
        match self {
            RowPlan::Single { top_gap, .. }
            | RowPlan::Footnote { top_gap, .. }
            | RowPlan::Callout { top_gap, .. } => *top_gap,
        }
    }
}

/// 相邻行元数据，用于折叠视觉组内的间距。

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RowSpacingInfo {
    quote_group_anchor: Option<uuid::Uuid>,
    visible_quote_group_anchor: Option<uuid::Uuid>,
    callout_anchor: Option<uuid::Uuid>,
    callout_variant: Option<CalloutVariant>,
    is_callout_header: bool,
    footnote_anchor: Option<uuid::Uuid>,
    is_footnote_header: bool,
}

impl RowSpacingInfo {
    /// 从块读取相邻行间距所需的分组元数据。
    fn from_block(block: &Block) -> Self {
        Self {
            quote_group_anchor: block.quote_group_anchor,
            visible_quote_group_anchor: block.visible_quote_group_anchor,
            callout_anchor: block.callout_anchor,
            callout_variant: block.callout_variant,
            is_callout_header: block.kind().is_callout(),
            footnote_anchor: block.footnote_anchor,
            is_footnote_header: block.kind().is_footnote_definition(),
        }
    }
}

/// 计算相邻渲染行的顶部间隙（同引用组合并为零）。
fn rendered_row_top_gap(
    previous: Option<RowSpacingInfo>,
    current: RowSpacingInfo,
    default_gap: f32,
) -> f32 {
    let Some(previous) = previous else {
        return 0.0;
    };

    if previous.quote_group_anchor.is_some()
        && previous.quote_group_anchor == current.quote_group_anchor
    {
        0.0
    } else {
        default_gap
    }
}

/// 计算提示框行的顶部间隙（同组折叠，首尾用主题间距）。
fn callout_row_top_gap(
    previous: Option<RowSpacingInfo>,
    current: RowSpacingInfo,
    dimensions: &ThemeDimensions,
) -> f32 {
    let Some(previous) = previous else {
        return 0.0;
    };

    if previous.visible_quote_group_anchor.is_some()
        && previous.visible_quote_group_anchor == current.visible_quote_group_anchor
    {
        return 0.0;
    }

    if previous.is_callout_header {
        dimensions.callout_header_margin_bottom
    } else {
        dimensions.callout_body_gap
    }
}

/// 计算脚注行的顶部间隙（定义头后稍收紧）。
fn footnote_row_top_gap(previous: Option<RowSpacingInfo>, default_gap: f32) -> f32 {
    let Some(previous) = previous else {
        return 0.0;
    };

    if previous.is_footnote_header {
        default_gap * 0.75
    } else {
        default_gap
    }
}

/// 判断目标系统是否支持窗内菜单（除 macOS 外均支持）。
fn supports_in_window_menu_for_target_os(target_os: &str) -> bool {
    target_os != "macos"
}

/// 判断当前系统是否支持窗内菜单。
fn supports_in_window_menu() -> bool {
    supports_in_window_menu_for_target_os(std::env::consts::OS)
}

/// 标题栏开头品牌槽的展示文本（将来有图标时在此槽位拼接图标 + 名称）。
const TITLEBAR_BRAND_LABEL: &str = "拾墨";

/// 把原生菜单项逐项装入框架弹出菜单（递归处理子菜单）。
/// 点击后经窗口标准分发路由到已注册的动作处理。
pub(super) fn push_owned_menu_items(
    mut menu: PopupMenu,
    items: &[OwnedMenuItem],
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for item in items {
        match item {
            OwnedMenuItem::Separator => {
                menu = menu.separator();
            }
            OwnedMenuItem::Action {
                name,
                action,
                checked,
                disabled,
                ..
            } => {
                // 禁用态以数据为准，空历史占位项同样置灰。
                let disabled = *disabled || action.as_ref().as_any().is::<NoRecentFiles>();
                if *checked {
                    menu = menu.menu_with_check_and_disabled(
                        name.clone(),
                        true,
                        action.boxed_clone(),
                        disabled,
                    );
                } else {
                    menu = menu.menu_with_disabled(name.clone(), action.boxed_clone(), disabled);
                }
            }
            OwnedMenuItem::Submenu(submenu) => {
                let label = submenu.name.clone();
                let sub_items = submenu.items.clone();
                menu = menu.submenu(label, window, cx, move |sub, window, cx| {
                    push_owned_menu_items(sub, &sub_items, window, cx)
                });
            }
            // 系统菜单只存在于 macOS 原生栏，窗内菜单不会遇到。
            OwnedMenuItem::SystemMenu(_) => {}
        }
    }
    menu
}

/// 构造脚注分组的外层容器样式。
fn footnote_group_shell(
    children: Vec<AnyElement>,
    theme: &Theme,
    dimensions: &ThemeDimensions,
) -> AnyElement {
    div()
        .w_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(0.0))
        .px(px(dimensions.footnote_padding_x))
        .py(px(dimensions.footnote_padding_y))
        .rounded(px(dimensions.footnote_radius))
        .border(px(1.0))
        .border_color(theme.border)
        .bg(theme.popover)
        .children(children)
        .into_any_element()
}

impl Editor {
    /// 安装窗口关闭拦截（已安装时跳过）。
    pub(crate) fn install_close_guard(&mut self, cx: &mut Context<Self>, window: &mut Window) {
        if self.close_guard_installed {
            return;
        }

        self.force_install_close_guard(cx, window);
    }

    /// 强制安装窗口关闭拦截并标记已安装。
    pub(crate) fn force_install_close_guard(
        &mut self,
        cx: &mut Context<Self>,
        window: &mut Window,
    ) {
        let editor = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            editor
                .update(cx, |this, cx| this.on_window_should_close(window, cx))
                .unwrap_or(true)
        });
        self.close_guard_installed = true;
    }

    /// 应用挂起的焦点请求并聚焦对应块。
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entity_id) = self.pending_focus.take()
            && let Some(block) = self.focusable_entity_by_id(entity_id)
        {
            let focus_handle = block.read(cx).focus_handle.clone();
            focus_handle.focus(window, cx);
        }
    }

    /// 确保聚焦光标可见，必要时滚动视口，成功返回真。
    fn ensure_focused_caret_visible(&mut self, window: &Window, cx: &App) -> bool {
        let Some(focused_block) = self.focused_edit_target(window, cx) else {
            return false;
        };
        let Some(active_bounds) =
            focused_block.read_with(cx, |block, _cx| block.active_selection_or_cursor_bounds())
        else {
            return false;
        };

        let viewport = self.scroll_handle.bounds();
        let padding = px(20.0);
        let top_limit = viewport.top() + padding;
        let bottom_limit = viewport.bottom() - padding;
        let mut offset = self.scroll_handle.offset();
        let mut changed = false;

        if active_bounds.top() < top_limit {
            offset.y += top_limit - active_bounds.top();
            changed = true;
        } else if active_bounds.bottom() > bottom_limit {
            offset.y -= active_bounds.bottom() - bottom_limit;
            changed = true;
        }

        if changed {
            let max_offset_y = self.scroll_handle.max_offset().y.max(px(0.0));
            offset.y = offset.y.min(px(0.0)).max(-max_offset_y);
            self.scroll_handle.set_offset(offset);
        }

        true
    }

    /// 应用挂起的滚动到视口请求（含布局后重试）。
    fn apply_pending_scroll_into_view(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.scrollbar_drag.is_some() {
            return;
        }

        if !self.pending_scroll_active_block_into_view {
            return;
        }

        // 占位间隔会打乱按位置索引子项的滚动，故用聚焦块边界做像素换算，其恒挂载。
        let has_bounds = self.ensure_focused_caret_visible(window, cx);
        if self.pending_scroll_recheck_after_layout {
            self.pending_scroll_recheck_after_layout = false;
            self.schedule_scroll_recheck(cx);
            return;
        }

        if !has_bounds {
            self.schedule_scroll_recheck(cx);
            return;
        }

        self.pending_scroll_active_block_into_view = false;
        self.scroll_recheck_task = None;
    }

    /// 请求下一帧重绘，让待滚动在目标块布局后重试。`render` 内调用 `cx.notify()` 会被吞掉，
    /// 若无此延迟重试，下次刷新要等外部通知（如约半秒后的光标闪烁）。
    fn schedule_scroll_recheck(&mut self, cx: &mut Context<Self>) {
        self.scroll_recheck_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
            // 借用冲突（标题栏拖拽等嵌套派发）时跳过，下次布局/通知再重试。
            let _ = update_entity_forgiving(&this, cx, |_this, cx| cx.notify());
        }));
    }

    /// 同步挂起的保存请求。
    fn sync_pending_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_save {
            self.pending_save = false;
            self.save_document(window, cx);
        }
    }

    /// 同步挂起的另存请求。
    fn sync_pending_save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_save_as {
            self.pending_save_as = false;
            self.save_document_as(window, cx);
        }
    }

    /// 同步挂起的链接跳转确认弹窗。
    fn sync_pending_open_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(link) = self.pending_open_link.take() else {
            return;
        };

        let i18n = cx.global::<I18nManager>().snapshot();
        let open_text = i18n.t("dialog_open_link_open", &[]);
        let cancel_text = i18n.t("dialog_open_link_cancel", &[]);
        let title = i18n.t("dialog_open_link_title", &[]);
        let buttons = [open_text.as_str(), cancel_text.as_str()];
        let prompt = window.prompt(
            PromptLevel::Info,
            &title,
            Some(&link.prompt_target),
            &buttons,
            cx,
        );
        let window_handle = window.window_handle();
        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok(choice) = prompt.await else {
                return;
            };
            if choice == 0 {
                let _ = cx.update_window(window_handle, |_view: AnyView, _window, cx| {
                    cx.open_url(&link.open_target);
                });
            }
        })
        .detach();
    }

    /// 同步窗口的已编辑标记。
    fn sync_window_edited_state(&mut self, window: &mut Window) {
        if self.pending_window_edited {
            self.pending_window_edited = false;
            window.set_window_edited(true);
        }
    }

    /// 同步滚动视口尺寸（变化时请求活动块进入视口）。
    fn sync_scroll_viewport(&mut self, viewport_size: Size<Pixels>, cx: &mut Context<Self>) {
        match self.last_scroll_viewport_size {
            Some(previous) if Self::viewport_size_changed(previous, viewport_size) => {
                self.last_scroll_viewport_size = Some(viewport_size);
                self.request_active_block_scroll_into_view(cx);
            }
            Some(_) => {}
            None => {
                self.last_scroll_viewport_size = Some(viewport_size);
            }
        }
    }

    /// 同步系统窗口标题（有挂起刷新时才执行）。
    fn sync_window_title(&mut self, window: &mut Window, i18n: &I18nSnapshot) {
        if self.pending_window_title_refresh {
            self.pending_window_title_refresh = false;
            let title = Self::window_title(self.file_path.as_deref(), self.document_dirty, i18n);
            window.set_window_title(&title);
        }
    }

    /// 打开未保存修改确认框（框架 `Dialog` 三按钮；`footer` 自定义）。
    ///
    /// 隐式关闭路径全禁（`keyboard`/`overlay_closable`/`close_button`），关闭只走
    /// 三个按钮（与旧自研 overlay“点遮罩/ESC 无反应”同语义）；按钮持有实体
    /// 直连既有 `on_*` 逻辑方法（派发期间窗口正被 take，不能走
    /// `downcast` + `WindowHandle::update`），视觉关闭由各方法内的 `close_dialog` 负责。
    pub(crate) fn open_unsaved_changes_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let i18n = cx.global::<I18nManager>().snapshot();
        let title = i18n.t("dialog_unsaved_title", &[]);
        let message = i18n.t("dialog_unsaved_message", &[]);
        let cancel = i18n.t("dialog_unsaved_cancel", &[]);
        let discard = i18n.t("dialog_unsaved_discard", &[]);
        let save = i18n.t("dialog_unsaved_save", &[]);
        let width = ThemeDimensions::DEFAULT.dialog_width;
        let gap = ThemeDimensions::DEFAULT.dialog_button_gap;
        let editor = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            // 构建闭包是 `Fn`（对话框层每帧重建都会调用），内层闭包捕获先克隆为本轮局部。
            let message = message.clone();
            // 按钮路由用实体直连（见 `dialog_button_click`）。
            let cancel_editor = editor.clone();
            let discard_editor = editor.clone();
            let save_editor = editor.clone();
            dialog
                .width(px(width))
                .title(title.clone())
                .keyboard(false)
                .overlay_closable(false)
                .close_button(false)
                .content(move |content, _, cx| {
                    let theme = cx.theme().clone();
                    let t = &ThemeTypography::DEFAULT;
                    content.child(
                        div()
                            .text_size(px(t.dialog_body_size))
                            .font_weight(t.dialog_button_weight)
                            .line_height(rems(t.text_line_height))
                            .text_color(theme.foreground)
                            .child(message.clone()),
                    )
                })
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(gap))
                        .child(
                            Button::new("cancel-close-dialog")
                                .label(cancel.clone())
                                .secondary()
                                .on_click(Self::dialog_button_click(
                                    cancel_editor,
                                    Self::on_cancel_close_dialog,
                                )),
                        )
                        .child(
                            Button::new("discard-and-close-dialog")
                                .label(discard.clone())
                                .danger()
                                .on_click(Self::dialog_button_click(
                                    discard_editor,
                                    Self::on_discard_and_close,
                                )),
                        )
                        .child(
                            Button::new("save-and-close-dialog")
                                .label(save.clone())
                                .primary()
                                .on_click(Self::dialog_button_click(
                                    save_editor,
                                    Self::on_save_and_close,
                                )),
                        ),
                )
        });
    }

    /// 打开拖放文件替换确认框（框架 `Dialog` 三按钮，约束同未保存确认框）。
    pub(crate) fn open_drop_replace_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let i18n = cx.global::<I18nManager>().snapshot();
        let title = i18n.t("dialog_drop_replace_title", &[]);
        let message = i18n.t("dialog_drop_replace_message", &[]);
        let cancel = i18n.t("dialog_drop_replace_cancel", &[]);
        let discard = i18n.t("dialog_drop_replace_discard", &[]);
        let save = i18n.t("dialog_drop_replace_save", &[]);
        let width = ThemeDimensions::DEFAULT.dialog_width;
        let gap = ThemeDimensions::DEFAULT.dialog_button_gap;
        let editor = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            // 构建闭包是 `Fn`（对话框层每帧重建都会调用），内层闭包捕获先克隆为本轮局部。
            let message = message.clone();
            // 按钮路由用实体直连（见 `dialog_button_click`）。
            let cancel_editor = editor.clone();
            let discard_editor = editor.clone();
            let save_editor = editor.clone();
            dialog
                .width(px(width))
                .title(title.clone())
                .keyboard(false)
                .overlay_closable(false)
                .close_button(false)
                .content(move |content, _, cx| {
                    let theme = cx.theme().clone();
                    let t = &ThemeTypography::DEFAULT;
                    content.child(
                        div()
                            .text_size(px(t.dialog_body_size))
                            .font_weight(t.dialog_button_weight)
                            .line_height(rems(t.text_line_height))
                            .text_color(theme.foreground)
                            .child(message.clone()),
                    )
                })
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(gap))
                        .child(
                            Button::new("cancel-drop-replace-dialog")
                                .label(cancel.clone())
                                .secondary()
                                .on_click(Self::dialog_button_click(
                                    cancel_editor,
                                    Self::on_cancel_drop_replace_dialog,
                                )),
                        )
                        .child(
                            Button::new("discard-and-replace-drop-dialog")
                                .label(discard.clone())
                                .danger()
                                .on_click(Self::dialog_button_click(
                                    discard_editor,
                                    Self::on_discard_and_replace_drop,
                                )),
                        )
                        .child(
                            Button::new("save-and-replace-drop-dialog")
                                .label(save.clone())
                                .primary()
                                .on_click(Self::dialog_button_click(
                                    save_editor,
                                    Self::on_save_and_replace_drop,
                                )),
                        ),
                )
        });
    }

    /// 打开关于对话框（框架 `Dialog` 单确认按钮；内容复刻旧自研浮层）。
    pub(crate) fn open_about_dialog(&self, window: &mut Window, cx: &mut App) {
        // 未保存弹窗打开时不覆盖（与旧自研守卫同语义）。
        if self.show_unsaved_changes_dialog {
            return;
        }
        let i18n = cx.global::<I18nManager>().snapshot();
        let title = i18n.t("dialog_about_title", &[]);
        let version = format!("Rumark {}", env!("CARGO_PKG_VERSION"));
        let message = i18n.t("dialog_about_message", &[]);
        let github_label = i18n.t("dialog_about_github", &[]);
        let star = i18n.t("dialog_about_star", &[]);
        let ok = i18n.t("dialog_ok", &[]);
        let width = ThemeDimensions::DEFAULT.dialog_width;
        window.open_dialog(cx, move |dialog, _, _| {
            // 外层构建闭包是 `Fn`（对话框层每帧重建都会调用），内层
            // `content` 闭包也是 `Fn`，不能直接移出捕获变量，先克隆为本轮局部。
            let version = version.clone();
            let message = message.clone();
            let github_label = github_label.clone();
            let star = star.clone();
            dialog
                .width(px(width))
                .title(title.clone())
                .button_props(DialogButtonProps::default().ok_text(ok.clone()))
                .content(move |content, _, cx| {
                    let theme = cx.theme().clone();
                    let t = &ThemeTypography::DEFAULT;
                    let d = &ThemeDimensions::DEFAULT;
                    let body_row = |text: String| {
                        div()
                            .text_size(px(t.dialog_body_size))
                            .font_weight(t.dialog_button_weight)
                            .text_color(theme.foreground)
                            .child(text)
                    };
                    content.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(d.dialog_gap * 0.5))
                            .child(body_row(version.clone()))
                            .child(body_row(message.clone()))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(4.0))
                                    .text_size(px(t.dialog_body_size))
                                    .text_color(theme.foreground)
                                    .child(format!("{github_label}:"))
                                    .child(
                                        div()
                                            .id("about-github-link")
                                            .cursor_pointer()
                                            .text_color(theme.link)
                                            .underline()
                                            .child(GITHUB_URL)
                                            .on_click(move |_, _, cx| {
                                                open_about_github_url(cx);
                                            }),
                                    ),
                            )
                            .child(body_row(star.clone())),
                    )
                })
        });
    }
}

impl Editor {
    /// 读取某可见行的间距元数据（分组外壳内的相邻间距需要逐块回看）。
    fn row_spacing_at(
        blocks: &[super::tree::VisibleBlock],
        index: usize,
        cx: &mut Context<Self>,
    ) -> RowSpacingInfo {
        blocks[index]
            .entity
            .read_with(cx, |block, _cx| RowSpacingInfo::from_block(block))
    }

    /// 挂载单个块行：稳定 ElementId，渲染/源码双模式都挂右键。
    /// 表格单元格走表格轴菜单，不挂插入菜单；`full_width` 行由外层外壳定宽。
    fn mounted_row_element(
        &self,
        entity: &Entity<Block>,
        full_width: bool,
        gap: f32,
        centered_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = div()
            .id(ElementId::Name(
                format!("editor-row-{:?}", entity.entity_id()).into(),
            ))
            .flex_shrink_0()
            .mt(px(gap))
            .child(entity.clone());
        let row = if full_width {
            row.w_full()
        } else {
            row.w(px(centered_width)).max_w(relative(1.0))
        };
        let entity_id = entity.entity_id();
        if self.table_cell_binding(entity_id).is_some() {
            return row.into_any_element();
        }
        let row_entity = cx.entity().clone();
        row.context_menu(move |menu, window, cx| {
            Self::build_insert_context_popup(menu, &row_entity, Some(entity_id), window, cx)
        })
        .into_any_element()
    }

    /// 连续脚注块的行序列，共用一个脚注外壳。
    fn footnote_rows(
        &self,
        start: usize,
        end: usize,
        blocks: &[super::tree::VisibleBlock],
        centered_width: f32,
        d: &ThemeDimensions,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows = Vec::with_capacity(end - start);
        let mut previous: Option<RowSpacingInfo> = None;
        for index in start..end {
            let spacing = Self::row_spacing_at(blocks, index, cx);
            rows.push(self.mounted_row_element(
                &blocks[index].entity,
                true,
                footnote_row_top_gap(previous, d.block_gap),
                centered_width,
                cx,
            ));
            previous = Some(spacing);
        }
        rows
    }

    /// 按计划构建一行元素。窗口外的行不进入这里，因此每帧的元素分配
    /// 只与视口高度相关，与文档长度无关；块切片按行临时借用，不跨阶段驻留。
    fn build_row_element(
        &self,
        plan: &RowPlan,
        centered_width: f32,
        theme: &Theme,
        d: &ThemeDimensions,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let blocks = self.document.visible_blocks();
        let shell = |children: Vec<AnyElement>, gap: f32| {
            div()
                .w(px(centered_width))
                .max_w(relative(1.0))
                .flex_shrink_0()
                .mt(px(gap))
                .children(children)
        };
        match plan {
            RowPlan::Single { block, top_gap } => self.mounted_row_element(
                &blocks[*block].entity,
                false,
                *top_gap,
                centered_width,
                cx,
            ),
            RowPlan::Footnote {
                start,
                end,
                top_gap,
            } => {
                let children = self.footnote_rows(*start, *end, blocks, centered_width, d, cx);
                shell(
                    vec![footnote_group_shell(children, theme, d).into_any_element()],
                    *top_gap,
                )
                .into_any_element()
            }
            RowPlan::Callout {
                top_gap,
                variant,
                segments,
                ..
            } => {
                let (accent, background) =
                    crate::components::callout_accent_and_background(*variant, theme);
                let mut children: Vec<AnyElement> = Vec::with_capacity(segments.len());
                let mut previous: Option<RowSpacingInfo> = None;
                for segment in segments {
                    match segment {
                        RowSegment::Block(block) => {
                            let spacing = Self::row_spacing_at(blocks, *block, cx);
                            children.push(self.mounted_row_element(
                                &blocks[*block].entity,
                                true,
                                callout_row_top_gap(previous, spacing, d),
                                centered_width,
                                cx,
                            ));
                            previous = Some(spacing);
                        }
                        RowSegment::FootnoteGroup { start, end } => {
                            let first = Self::row_spacing_at(blocks, *start, cx);
                            let inner =
                                self.footnote_rows(*start, *end, blocks, centered_width, d, cx);
                            children.push(
                                div()
                                    .w_full()
                                    .flex_shrink_0()
                                    .mt(px(callout_row_top_gap(previous, first, d)))
                                    .child(footnote_group_shell(inner, theme, d))
                                    .into_any_element(),
                            );
                            previous = Some(Self::row_spacing_at(blocks, *end - 1, cx));
                        }
                    }
                }
                div()
                    .w(px(centered_width))
                    .max_w(relative(1.0))
                    .flex_shrink_0()
                    .mt(px(*top_gap))
                    .flex()
                    .flex_col()
                    .gap(px(0.0))
                    .px(px(d.callout_padding_x))
                    .py(px(d.callout_padding_y))
                    .rounded(px(d.callout_radius))
                    .border_l(px(d.callout_border_width))
                    .border_color(accent)
                    .bg(background)
                    .children(children)
                    .into_any_element()
            }
        }
    }
}

impl Render for Editor {
    /// 渲染编辑器窗口（含菜单栏、文档列、对话框与延迟同步）。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.install_close_guard(cx, window);
        self.apply_pending_focus(window, cx);
        self.apply_pending_scroll_into_view(window, cx);
        self.last_selection_snapshot = self.capture_source_selection_snapshot(cx);
        self.sync_pending_save(window, cx);
        self.sync_pending_save_as(window, cx);
        self.sync_pending_open_link(window, cx);
        self.sync_window_edited_state(window);

        let viewport_bounds = self.scroll_handle.bounds();
        let viewport_size = viewport_bounds.size;
        self.sync_scroll_viewport(viewport_size, cx);

        let theme = rgpui::Theme::global(cx).clone();
        let i18n = cx.global::<I18nManager>().snapshot();
        self.sync_window_title(window, &i18n);

        let d = &ThemeDimensions::DEFAULT;
        let editor = cx.entity().downgrade();
        let scroll_trigger_padding = (d.block_min_height * 0.75).max(16.0);
        let max_scroll_y = f32::from(self.scroll_handle.max_offset().y.max(px(0.0)));
        let viewport_height = f32::from(viewport_bounds.size.height.max(px(1.0)));
        // 末块下方留白，使最末行可滚动到视口中央，而非钉在底边。
        let scroll_beyond_bottom = viewport_height * 0.5;
        let viewport_width = f32::from(viewport_bounds.size.width.max(px(1.0)));
        let has_overflow = max_scroll_y > 0.5;

        let centered_width = Self::centered_column_width(viewport_width, &ThemeDimensions::DEFAULT);
        let current_scroll_y = (-f32::from(self.scroll_handle.offset().y)).clamp(0.0, max_scroll_y);
        let scrollbar_geometry =
            Self::scrollbar_geometry(viewport_height, max_scroll_y, current_scroll_y);
        let track_height = scrollbar_geometry.track_height;
        let thumb_height = scrollbar_geometry.thumb_height;
        let thumb_top = scrollbar_geometry.thumb_top;

        let show_custom_scrollbar = has_overflow
            && (self.scrollbar_drag.is_some()
                || self.scrollbar_hovered
                || Instant::now() <= self.scrollbar_visible_until);

        // 分组与结构快照分阶段短暂借用文档：长文档每帧不再克隆与块数等长的实体句柄。
        let block_count = self.document.visible_block_count();
        // 间距元数据按需读取，而非预收集为与可见块等长的向量，
        // 长文档下每帧可省数十 KB 分配，单块读取仅为廉价不可变锁加七字段拷贝。
        let spacing_for = |index: usize| -> RowSpacingInfo {
            self.document.visible_blocks()[index]
                .entity
                .read_with(cx, |block, _cx| RowSpacingInfo::from_block(block))
        };
        // 每行先只登记分组计划：分组只需逐块间距元数据，元素留到窗口确定后再构建。
        let mut row_plans: Vec<RowPlan> = Vec::new();
        let mut previous_row_spacing: Option<RowSpacingInfo> = None;
        let mut index = 0usize;
        while index < block_count {
            let first_spacing = spacing_for(index);
            let top_gap = rendered_row_top_gap(previous_row_spacing, first_spacing, d.block_gap);

            if let (Some(callout_anchor), Some(callout_variant)) =
                (first_spacing.callout_anchor, first_spacing.callout_variant)
            {
                let mut segments: Vec<RowSegment> = Vec::new();
                let mut group_end = index;
                while group_end < block_count
                    && spacing_for(group_end).callout_anchor == Some(callout_anchor)
                {
                    let row_spacing = spacing_for(group_end);
                    if let Some(footnote_anchor) = row_spacing.footnote_anchor {
                        let footnote_start = group_end;
                        let mut footnote_end = group_end;
                        while footnote_end < block_count
                            && spacing_for(footnote_end).callout_anchor == Some(callout_anchor)
                            && spacing_for(footnote_end).footnote_anchor == Some(footnote_anchor)
                        {
                            footnote_end += 1;
                        }
                        segments.push(RowSegment::FootnoteGroup {
                            start: footnote_start,
                            end: footnote_end,
                        });
                        group_end = footnote_end;
                        continue;
                    }
                    segments.push(RowSegment::Block(group_end));
                    group_end += 1;
                }

                row_plans.push(RowPlan::Callout {
                    start: index,
                    top_gap,
                    variant: callout_variant,
                    segments,
                });
                previous_row_spacing = Some(spacing_for(group_end - 1));
                index = group_end;
                continue;
            }

            if let Some(footnote_anchor) = first_spacing.footnote_anchor {
                let mut group_end = index;
                while group_end < block_count
                    && spacing_for(group_end).footnote_anchor == Some(footnote_anchor)
                {
                    group_end += 1;
                }

                row_plans.push(RowPlan::Footnote {
                    start: index,
                    end: group_end,
                    top_gap,
                });
                previous_row_spacing = Some(spacing_for(group_end - 1));
                index = group_end;
                continue;
            }

            row_plans.push(RowPlan::Single {
                block: index,
                top_gap,
            });
            previous_row_spacing = Some(first_spacing);
            index += 1;
        }
        // 行首块下标与前导边距由计划派生，供焦点行定位与占位高度使用。
        let row_starts: Vec<usize> = row_plans.iter().map(RowPlan::start).collect();
        let row_top_gaps: Vec<f32> = row_plans.iter().map(RowPlan::top_gap).collect();

        // 聚焦行恒挂载以防光标失焦，表格单元格映射到其所属表格块的行。
        let focus_row = self
            .focused_edit_target_entity_id(window, cx)
            .and_then(|id| {
                self.document.visible_index_for_entity_id(id).or_else(|| {
                    self.table_cell_binding(id).and_then(|binding| {
                        self.document
                            .visible_index_for_entity_id(binding.table_block.entity_id())
                    })
                })
            })
            .map(|visible_index| {
                row_starts
                    .partition_point(|&start| start <= visible_index)
                    .saturating_sub(1)
            });

        // 以行首块为键缓存行足迹。
        let row_first_ids: Vec<EntityId> = row_starts
            .iter()
            .map(|&start| self.document.visible_block_entity_id(start))
            .collect();

        // 结构编辑后行号与上帧对不上，跳过下方缓存刷新，块键条目仍有效。
        let structural_change = block_count != self.prev_visible_block_ids.len()
            || (0..block_count.min(self.prev_visible_block_ids.len())).any(|index| {
                self.document.visible_block_entity_id(index) != self.prev_visible_block_ids[index]
            });
        if structural_change {
            self.prev_visible_block_ids = (0..block_count)
                .map(|index| self.document.visible_block_entity_id(index))
                .collect();
        }

        // 足迹仅对其测量时的列宽有效。首帧尚无滚动边界，列宽塌到一像素底线，
        // 每块每行一字换行，保留该测量会使文档尺寸永久错误。
        let width_changed = self.row_stride_width != Some(centered_width);
        if width_changed {
            self.row_stride_cache.clear();
            self.row_stride_width = Some(centered_width);
        }

        // 滚动容器记录每个挂载子项的布局边界，相邻顶部差恰为一行足迹，
        // 缓存差值而非原始位置可保持滚动时窗口稳定。
        if !structural_change
            && !width_changed
            && let Some(prev) = self
                .prev_mounted_run
                .filter(|prev| self.mounted_run_is_addressable(*prev))
        {
            let prev_end = prev.row_end.min(row_first_ids.len());
            let row_end = prev_end.saturating_sub(1);
            for (row, row_id) in row_first_ids
                .iter()
                .enumerate()
                .take(row_end)
                .skip(prev.row_start)
            {
                let child = prev.child_base + row - prev.row_start;
                if let (Some(bounds), Some(next_bounds)) = (
                    self.scroll_handle.bounds_for_item(child),
                    self.scroll_handle.bounds_for_item(child + 1),
                ) {
                    let stride = f32::from(next_bounds.top() - bounds.top());
                    if stride > 0.0 && stride.is_finite() {
                        self.row_stride_cache.insert(*row_id, stride);
                    }
                }
            }
        }

        // 未测量行用最小块高作下界，使窗口多挂载而非落在占位上。
        let estimate = d.block_min_height.max(1.0);
        let strides: Vec<f32> = row_first_ids
            .iter()
            .map(|id| self.row_stride_cache.get(id).copied().unwrap_or(estimate))
            .collect();

        // 仅当缓存超过存活行时，按块更替裁剪缓存。
        if self.row_stride_cache.len() > row_first_ids.len().saturating_mul(2) {
            let live: std::collections::HashSet<EntityId> = row_first_ids.iter().copied().collect();
            self.row_stride_cache.retain(|id, _| live.contains(id));
        }

        let render_window = Self::rendered_window(
            &strides,
            current_scroll_y,
            viewport_height,
            OVERDRAW_PX,
            focus_row,
        );

        let island = render_window.focus_island;
        let island_before_run = island.is_some_and(|island| island.row < render_window.run_start);
        // 挂载行会重应用自身边距，前一段跨度已覆盖，故占位减去其前导行的边距。
        let spacer_before = |row: usize, height: f32| -> f32 {
            match row_top_gaps.get(row) {
                Some(gap) => (height - gap).max(0.0),
                None => height,
            }
        };
        let mut block_rows: Vec<AnyElement> =
            Vec::with_capacity(render_window.run_end - render_window.run_start + 4);
        let push_spacer = |rows: &mut Vec<AnyElement>, height: f32| {
            if height > 0.5 {
                rows.push(
                    div()
                        .w_full()
                        .flex_shrink_0()
                        .h(px(height))
                        .into_any_element(),
                );
            }
        };

        // 元素只为窗口内的行构建，屏外行不再分配 ElementId 字符串与菜单闭包。
        let mut build_row = |rows: &mut Vec<AnyElement>, row: usize| {
            if let Some(plan) = row_plans.get(row) {
                rows.push(self.build_row_element(plan, centered_width, &theme, d, cx));
            }
        };

        if let Some(island) = island.filter(|_| island_before_run) {
            push_spacer(&mut block_rows, spacer_before(island.row, island.lead_h));
            build_row(&mut block_rows, island.row);
        }
        push_spacer(
            &mut block_rows,
            spacer_before(render_window.run_start, render_window.top_h),
        );
        let run_child_base = block_rows.len();
        for row in render_window.run_start..render_window.run_end {
            build_row(&mut block_rows, row);
        }
        if let Some(island) = island.filter(|_| !island_before_run) {
            push_spacer(&mut block_rows, spacer_before(island.row, island.lead_h));
            build_row(&mut block_rows, island.row);
        }
        push_spacer(&mut block_rows, render_window.bottom_h);
        // 下帧按子项下标读回足迹，并先校验子项数再采信。
        self.prev_mounted_run = Some(MountedRow {
            row_start: render_window.run_start,
            row_end: render_window.run_end,
            child_base: run_child_base,
            child_count: block_rows.len(),
        });

        let scroll_content = div()
            .id("editor-scroll-inner")
            .flex()
            .flex_col()
            .flex_grow(1.)
            .h_full()
            .items_center()
            .bg(theme.background)
            .overflow_y_scroll()
            .scrollbar_width(px(0.0))
            .track_scroll(&self.scroll_handle)
            .on_hover(cx.listener(Self::on_editor_hover))
            .capture_any_mouse_down(cx.listener(Self::on_editor_capture_mouse_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_editor_mouse_down))
            .on_mouse_move(cx.listener(Self::on_editor_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_editor_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_editor_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_editor_scroll_wheel))
            .p(px(d.editor_padding))
            .pb(px(d.editor_padding
                + scroll_trigger_padding
                + scroll_beyond_bottom))
            .children(block_rows);
        // 空白处右键：追加语义的插入菜单（框架弹出菜单，定位由组件处理）。
        let empty_menu_entity = cx.entity().clone();
        let scroll_content = scroll_content.context_menu(move |menu, window, cx| {
            Self::build_insert_context_popup(menu, &empty_menu_entity, None, window, cx)
        });

        let content_area = div()
            .id("editor-scroll")
            .w_full()
            .h_full()
            .flex_1()
            .min_w(px(0.0))
            .bg(theme.background)
            .relative()
            .child(scroll_content);

        let content_area = if show_custom_scrollbar {
            let scrollbar_editor = editor.clone();
            let track_origin_y = f32::from(viewport_bounds.origin.y);
            content_area.child(
                div()
                    .id("editor-scrollbar-thumb")
                    .absolute()
                    .occlude()
                    .top(px(thumb_top))
                    .right(px(d.scrollbar_right))
                    .w(px(d.scrollbar_width))
                    .h(px(thumb_height))
                    .rounded(px(999.0))
                    .bg(theme.scrollbar_thumb)
                    .cursor_pointer()
                    .on_hover(cx.listener(Self::on_editor_hover))
                    .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
                        let pointer_offset_y =
                            f32::from(event.position.y) - track_origin_y - thumb_top;
                        let _ = scrollbar_editor.update(cx, |editor, cx| {
                            cx.stop_propagation();
                            editor.start_scrollbar_drag(
                                pointer_offset_y,
                                track_height,
                                thumb_height,
                                max_scroll_y,
                                cx,
                            );
                        });
                    })
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |_thumb_bounds, _, window, _| {
                                window.on_mouse_event({
                                    let editor = editor.clone();
                                    move |_event: &MouseUpEvent, phase, _window, cx| {
                                        if !phase.bubble() {
                                            return;
                                        }
                                        let _ = editor.update(cx, |editor, cx| {
                                            editor.end_scrollbar_drag(cx);
                                        });
                                    }
                                });

                                window.on_mouse_event({
                                    let editor = editor.clone();
                                    move |event: &MouseMoveEvent, phase, _window, cx| {
                                        if !phase.bubble() || !event.dragging() {
                                            return;
                                        }

                                        let pointer_y_in_track =
                                            f32::from(event.position.y) - track_origin_y;
                                        let _ = editor.update(cx, |editor, cx| {
                                            editor.update_scrollbar_drag(pointer_y_in_track, cx);
                                        });
                                    }
                                });
                            },
                        )
                        .size_full(),
                    ),
            )
        } else {
            content_area
        };

        // 跟随修饰键切换时重绘，使悬停链接的手形光标无需移动指针即更新。
        // 修饰键变化沿聚焦元素路径派发到根，本根为各块祖先，
        // 故一处监听覆盖编辑中任意块的链接，仅对次修饰键生效，选区中的换档不重绘。
        let follow_modifier_active = window.modifiers().secondary();

        let base = div()
            .w_full()
            .h_full()
            .flex()
            .flex_col()
            .relative()
            .bg(theme.background)
            .font(editor_text_font())
            .on_modifiers_changed(move |event, window, _| {
                if event.modifiers.secondary() != follow_modifier_active {
                    window.refresh();
                }
            })
            .capture_action(cx.listener(Self::on_copy_capture))
            .capture_action(cx.listener(Self::on_cut_capture))
            .capture_action(cx.listener(Self::on_delete_capture))
            .capture_action(cx.listener(Self::on_delete_back_capture))
            .capture_key_down(cx.listener(Self::on_editor_key_down_capture))
            .can_drop(|dragged, _window, _cx| dragged.is::<ExternalPaths>())
            .on_drop::<ExternalPaths>(cx.listener(Self::on_external_paths_drop))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_save_document))
            .on_action(cx.listener(Self::on_save_document_as))
            .on_action(cx.listener(Self::on_quit_application))
            .on_action(cx.listener(Self::on_close_window))
            .on_action(cx.listener(Self::on_toggle_view_mode_action))
            .on_action(cx.listener(Self::on_toggle_workspace_action))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_jump_to_top))
            .on_action(cx.listener(Self::on_jump_to_bottom))
            .on_action(cx.listener(Self::on_dismiss_transient_ui))
            .on_action(cx.listener(Self::on_install_cli_tool))
            .on_action(cx.listener(Self::on_uninstall_cli_tool))
            .on_action(cx.listener(Self::on_insert_link_action))
            .on_action(cx.listener(Self::on_insert_image_action))
            .on_action(cx.listener(Self::on_insert_table_action))
            .on_action(cx.listener(Self::on_insert_code_block_action))
            .on_action(cx.listener(Self::on_insert_math_block_action))
            .on_action(cx.listener(Self::on_insert_quote_action))
            .on_action(cx.listener(Self::on_insert_bulleted_list_action))
            .on_action(cx.listener(Self::on_insert_numbered_list_action))
            .on_action(cx.listener(Self::on_insert_task_list_action))
            .on_action(cx.listener(Self::on_insert_separator_action))
            .on_action(cx.listener(Self::on_insert_paragraph_above_action))
            .on_action(cx.listener(Self::on_insert_paragraph_below_action))
            .on_action(cx.listener(Self::on_convert_heading_1_action))
            .on_action(cx.listener(Self::on_convert_heading_2_action))
            .on_action(cx.listener(Self::on_convert_heading_3_action))
            .on_action(cx.listener(Self::on_convert_heading_4_action))
            .on_action(cx.listener(Self::on_convert_heading_5_action))
            .on_action(cx.listener(Self::on_convert_heading_6_action))
            .on_action(cx.listener(Self::on_convert_paragraph_action))
            .on_action(cx.listener(Self::on_table_insert_row_above_action))
            .on_action(cx.listener(Self::on_table_insert_row_below_action))
            .on_action(cx.listener(Self::on_table_delete_row_action))
            .on_action(cx.listener(Self::on_table_insert_column_left_action))
            .on_action(cx.listener(Self::on_table_insert_column_right_action))
            .on_action(cx.listener(Self::on_table_delete_column_action))
            .on_action(cx.listener(Self::on_table_move_row_up_action))
            .on_action(cx.listener(Self::on_table_move_row_down_action))
            .on_action(cx.listener(Self::on_table_move_column_left_action))
            .on_action(cx.listener(Self::on_table_move_column_right_action))
            .on_action(cx.listener(Self::on_table_align_column_left_action))
            .on_action(cx.listener(Self::on_table_align_column_center_action))
            .on_action(cx.listener(Self::on_table_align_column_right_action))
            .on_action(cx.listener(Self::on_toggle_table_headers_action))
            .on_action(cx.listener(Self::on_find_action));
        // 标题栏菜单只在非 macOS 绘制（与原生菜单二选一）。
        let menus = supports_in_window_menu()
            .then(|| cx.get_menus())
            .flatten()
            .filter(|m| !m.is_empty());
        // 单行组件标题栏：开头应用名 + 左侧菜单按钮，右侧窗口控制按钮由组件自动追加。
        // 文档标题不在此显示（任务栏标题仍经 `set_window_title` 同步）。
        // 有菜单时清零组件左内边距，改由内层菜单栏按框架左内边距对齐，
        // 与无菜单分支的组件内边距同源（框架改动自动跟随）。
        let titlebar = TitleBar::new();
        let titlebar = if menus.is_none() {
            titlebar
        } else {
            titlebar.pl(px(0.0))
        };
        // 标题栏按钮收敛到 Small 高度（24px）：34px 栏内上下各留约 5px，
        // 文字用标题栏专用 13px 覆盖 Small 默认 14px，比品牌小一档。
        // 展开状态由编辑器统一持有，同一时刻只允许一个菜单展开。
        let titlebar_menu_text_size = d.titlebar_menu_text_size;
        let menu_buttons: Vec<AnyElement> = menus
            .as_ref()
            .map(|list| {
                list.iter()
                    .enumerate()
                    .map(|(index, menu)| {
                        self.render_titlebar_menu(index, menu, titlebar_menu_text_size, cx)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let base = base.child(
            titlebar.child(
                h_flex()
                    .w_full()
                    .h_full()
                    .items_center()
                    .gap(px(d.menu_bar_gap))
                    .pl(TITLE_BAR_LEFT_PADDING)
                    // 开头品牌槽位：将来有标题栏图标时在此拼接图标 + 名称。
                    // 品牌比菜单大一档（14px 加粗），与 13px 菜单形成层级。
                    // 行高与菜单统一为 1.0，避免中英文混排时基线差 1px；
                    // 右侧复用菜单间距再垫 4px，品牌-首菜单 8px、菜单间 4px。
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(d.titlebar_brand_text_size))
                            .line_height(relative(1.0))
                            .font_weight(rgpui::FontWeight::SEMIBOLD)
                            .text_color(theme.button_secondary_foreground)
                            .pr(px(d.menu_bar_gap))
                            .child(TITLEBAR_BRAND_LABEL),
                    )
                    .children(menu_buttons),
            ),
        );
        let workspace_width =
            self.workspace_effective_panel_width(f32::from(window.viewport_size().width));
        let is_resizing_workspace = self.workspace.resize_drag.is_some();
        let mut main_content = div()
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .min_w(px(0.0))
            .on_mouse_move(cx.listener(Self::on_workspace_panel_resize_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(Self::on_workspace_panel_resize_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(Self::on_workspace_panel_resize_up),
            );
        if let Some(workspace_panel) =
            self.render_workspace_panel(&theme, &i18n, workspace_width, window, cx)
        {
            // 侧边栏与正文之间的拖拽分隔条（8px 热区 + 1px 可见线，悬停/拖拽时高亮，双击恢复自动宽度）。
            let resize_handle = div()
                .id("workspace-resize-handle")
                .h_full()
                .w(px(8.0))
                .flex_shrink_0()
                .cursor_col_resize()
                .flex()
                .justify_center()
                .group("workspace-resize-handle")
                .child(
                    div()
                        .w(px(1.0))
                        .h_full()
                        .bg(if is_resizing_workspace {
                            theme.accent
                        } else {
                            theme.border
                        })
                        .group_hover("workspace-resize-handle", |this| this.bg(theme.accent)),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(Self::on_workspace_panel_resize_down),
                )
                .on_double_click(cx.listener(Self::on_workspace_panel_resize_reset));
            main_content = main_content.child(workspace_panel).child(resize_handle);
        }
        let base = base.child(main_content.child(content_area));
        let base = if let Some(find_bar) = self.render_find_bar(&theme, cx) {
            base.child(find_bar)
        } else {
            base
        };
        let base = if self.status_bar_enabled(cx) {
            base.child(self.status_bar_element(&i18n, cx))
        } else {
            base
        };
        let base = if let Some(table_dialog) = self.render_table_insert_dialog_overlay(&theme, cx) {
            base.child(table_dialog)
        } else {
            base
        };
        let base = if let Some(link_dialog) = self.render_link_dialog_overlay(&theme, cx) {
            base.child(link_dialog)
        } else {
            base
        };
        // 未保存/替换确认已迁框架 `Dialog`（由对话框层渲染，此处只挂载）。
        base.when_some(Root::render_dialog_layer(window, cx), |this, layer| {
            this.child(layer)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RowSpacingInfo, callout_row_top_gap, editor_text_font, rendered_row_top_gap,
        supports_in_window_menu_for_target_os, tibetan_font_fallbacks_for_target_os,
    };
    use crate::theme::ThemeDimensions;
    use uuid::Uuid;

    #[test]
    fn contiguous_quote_rows_collapse_inter_row_gap() {
        let group = Uuid::new_v4();
        let gap = rendered_row_top_gap(
            Some(RowSpacingInfo {
                quote_group_anchor: Some(group),
                ..RowSpacingInfo::default()
            }),
            RowSpacingInfo {
                quote_group_anchor: Some(group),
                ..RowSpacingInfo::default()
            },
            4.0,
        );
        assert_eq!(gap, 0.0);
    }

    #[test]
    fn editor_text_font_keeps_system_ui_as_primary_family() {
        assert_eq!(editor_text_font().family.to_string(), ".SystemUIFont");
    }

    #[test]
    fn tibetan_font_fallbacks_prioritize_platform_defaults() {
        assert_eq!(
            tibetan_font_fallbacks_for_target_os("windows")
                .first()
                .map(String::as_str),
            Some("Microsoft Himalaya")
        );
        assert_eq!(
            tibetan_font_fallbacks_for_target_os("macos")
                .first()
                .map(String::as_str),
            Some("Kailasa")
        );
        assert_eq!(
            tibetan_font_fallbacks_for_target_os("linux")
                .first()
                .map(String::as_str),
            Some("Noto Serif Tibetan")
        );
        assert_eq!(
            tibetan_font_fallbacks_for_target_os("unknown")
                .first()
                .map(String::as_str),
            Some("Noto Serif Tibetan")
        );
    }

    #[test]
    fn nested_quote_separator_row_keeps_outer_group_gap_collapsed() {
        let group = Uuid::new_v4();
        let gap = rendered_row_top_gap(
            Some(RowSpacingInfo {
                quote_group_anchor: Some(group),
                ..RowSpacingInfo::default()
            }),
            RowSpacingInfo {
                quote_group_anchor: Some(group),
                ..RowSpacingInfo::default()
            },
            4.0,
        );
        assert_eq!(gap, 0.0);
    }

    #[test]
    fn distinct_quote_groups_keep_default_gap() {
        let gap = rendered_row_top_gap(
            Some(RowSpacingInfo {
                quote_group_anchor: Some(Uuid::new_v4()),
                ..RowSpacingInfo::default()
            }),
            RowSpacingInfo {
                quote_group_anchor: Some(Uuid::new_v4()),
                ..RowSpacingInfo::default()
            },
            4.0,
        );
        assert_eq!(gap, 4.0);
    }

    #[test]
    fn non_quote_rows_keep_default_gap() {
        let gap = rendered_row_top_gap(
            Some(RowSpacingInfo {
                quote_group_anchor: None,
                ..RowSpacingInfo::default()
            }),
            RowSpacingInfo {
                quote_group_anchor: Some(Uuid::new_v4()),
                ..RowSpacingInfo::default()
            },
            4.0,
        );
        assert_eq!(gap, 4.0);
    }

    #[test]
    fn callout_inner_spacing_uses_header_and_body_tokens() {
        let dimensions = &ThemeDimensions::DEFAULT;

        let header_gap = callout_row_top_gap(
            Some(RowSpacingInfo {
                is_callout_header: true,
                ..RowSpacingInfo::default()
            }),
            RowSpacingInfo::default(),
            dimensions,
        );
        let body_gap = callout_row_top_gap(
            Some(RowSpacingInfo {
                is_callout_header: false,
                ..RowSpacingInfo::default()
            }),
            RowSpacingInfo::default(),
            dimensions,
        );

        assert_eq!(header_gap, dimensions.callout_header_margin_bottom);
        assert_eq!(body_gap, dimensions.callout_body_gap);
    }

    #[test]
    fn nested_quote_rows_inside_callout_collapse_body_gap() {
        let dimensions = &ThemeDimensions::DEFAULT;
        let group = Uuid::new_v4();

        let gap = callout_row_top_gap(
            Some(RowSpacingInfo {
                is_callout_header: false,
                visible_quote_group_anchor: Some(group),
                ..RowSpacingInfo::default()
            }),
            RowSpacingInfo {
                visible_quote_group_anchor: Some(group),
                ..RowSpacingInfo::default()
            },
            dimensions,
        );

        assert_eq!(gap, 0.0);
    }

    #[test]
    fn in_window_menu_is_enabled_for_every_target_except_macos() {
        for target_os in [
            "windows",
            "linux",
            "freebsd",
            "openbsd",
            "netbsd",
            "dragonfly",
            "solaris",
            "illumos",
            "android",
            "unknown",
        ] {
            assert!(
                supports_in_window_menu_for_target_os(target_os),
                "{target_os} should use the in-window fallback menu"
            );
        }
        assert!(!supports_in_window_menu_for_target_os("macos"));
    }
}
