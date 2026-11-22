//! 一窗多文档的容器视图（薄壳 host）。
//!
//! 窗口根视图从「唯一的 Editor」升级为 [`DocumentHost`]：它持有一组 tab，
//! 每个 tab 是一个独立的 [`Editor`](super::Editor) 实体（自带文档树、光标、
//! 滚动、撤销栈与脏标记）。`render` 时只把活动 tab 的 `Entity<Editor>` 挂进
//! 元素树——框架据此调用 `Editor::render`，非活动 tab 不参与渲染但其 Rust
//! 状态完整保留（滚动位置、选区均在实体字段里而非元素树里）。
//!
//! 窗口级职责（标题栏、关闭守卫、动作捕获）仍由活动 Editor 承担：只有活动
//! tab 被挂载，其 `render` 才会执行 `sync_window_title` / `install_close_guard`。
//! 本容器额外注入一条 tab 条到 Editor 的 `top_strip` 插槽（标题栏下方）。
//!
//! standalone 编辑器（测试直接 `cx.new(Editor::from_markdown)`）不经过本容器，
//! 外观与行为完全不变。

use std::path::{Path, PathBuf};

use rgpui::menu::{PopupMenu, PopupMenuItem};
use rgpui::tabs::tab_drag::TabDragData;
use rgpui::tabs::{Tab, TabBar};
use rgpui::title_bar::TITLE_BAR_LEFT_PADDING;
use rgpui::*;

use super::{CloseScope, Editor};
use crate::i18n::I18nManager;

/// 一个文档 tab：核心是编辑器实体，路径从编辑器读取（同一文件可开多个 tab）。
struct TabEntry {
    editor: Entity<Editor>,
}

/// 窗口级多文档容器。
pub(crate) struct DocumentHost {
    tabs: Vec<TabEntry>,
    active: usize,
    /// 注入到每个 Editor 的 tab 条视图（同一窗口共用一条）。
    strip: Entity<TabStrip>,
    /// 关窗/退出时挂起的流程：逐个确认脏 tab 后执行。
    pending_window_close: Option<WindowCloseFlow>,
    /// 批量关闭 tab（右键菜单「关闭其他/右侧」）时挂起的目标实体集合。
    pending_batch_close: Option<Vec<Entity<Editor>>>,
    /// 是否启用多标签页。关闭时只有一个 tab、不显示 tab 条，行为等同单文档窗口。
    multi_tab: bool,
    /// 上一次写到窗口的 edited 状态，避免每帧重复调用平台 API。
    last_window_edited: Option<bool>,
    /// 上一次写到窗口的标题。
    last_window_title: Option<String>,
}

/// 关窗/退出的挂起流程。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowCloseFlow {
    /// 确认完所有脏 tab 后关闭本窗口。
    CloseWindow,
    /// 确认完所有脏 tab 后退出应用。
    Quit,
}

impl DocumentHost {
    /// 创建一个属于本 host 的编辑器：注入宿主弱引用与 tab 条插槽。
    fn create_editor(
        &self,
        markdown: String,
        file_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Entity<Editor> {
        let host = cx.entity().downgrade();
        let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, file_path));
        let strip = self.strip.clone();
        let multi_tab = self.multi_tab;
        editor.update(cx, |editor, cx| {
            editor.set_owner_host(host);
            editor.set_multi_tab_enabled(multi_tab);
            if multi_tab {
                editor.set_top_strip(Some(strip.into()), cx);
            }
        });
        editor
    }

    /// 用单个编辑器建立 host（单 tab 起步）。
    pub(crate) fn new(
        markdown: String,
        file_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = cx.entity().downgrade();
        let strip = cx.new(|_cx| TabStrip {
            host,
            selected: 0,
            labels: Vec::new(),
            menu: None,
        });
        let multi_tab = crate::config::EditorSettings::multi_tab_enabled(cx);
        // `create_editor` 需要 `&self.strip`，先用占位 host 构造再补首个 tab。
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            strip,
            pending_window_close: None,
            pending_batch_close: None,
            multi_tab,
            last_window_edited: None,
            last_window_title: None,
        };
        let editor = this.create_editor(markdown, file_path, cx);
        this.tabs.push(TabEntry { editor });
        this
    }

    /// 当前活动 tab 的编辑器实体。
    pub(crate) fn active_editor(&self) -> Entity<Editor> {
        self.tabs[self.active].editor.clone()
    }

    /// 按「任一 tab 脏」聚合窗口标题与系统 edited 标记。
    /// 标题文件名仍取活动 tab，但脏标记取所有 tab 的并集。
    /// 子 Editor 脏状态变化时只通知 host（不更新 host），避免 update 重入。
    fn sync_window_chrome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let any_dirty = self.any_dirty(cx);
        if self.last_window_edited != Some(any_dirty) {
            window.set_window_edited(any_dirty);
            self.last_window_edited = Some(any_dirty);
        }

        let i18n = cx.global::<I18nManager>().snapshot();
        let title = Editor::window_title(self.active_file_path(cx).as_deref(), any_dirty, &i18n);
        if self.last_window_title.as_deref() != Some(title.as_str()) {
            window.set_window_title(&title);
            self.last_window_title = Some(title);
        }
    }

    /// tab 数量（测试与后续会话持久化用）。
    #[allow(dead_code)]
    pub(crate) fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// 活动 tab 下标（测试与后续会话持久化用）。
    #[allow(dead_code)]
    pub(crate) fn active_index(&self) -> usize {
        self.active
    }

    /// 新增一个文档 tab（不复用空白 tab；复用逻辑在 app_menu 入口层判断）。
    /// 返回新 tab 的下标。
    pub(crate) fn add_tab(
        &mut self,
        markdown: String,
        file_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> usize {
        let editor = self.create_editor(markdown, file_path, cx);
        self.tabs.push(TabEntry { editor });
        self.active = self.tabs.len() - 1;
        self.refresh_strip(cx);
        cx.notify();
        self.active
    }

    /// 切换到指定 tab（下标越界时忽略）。
    pub(crate) fn set_active(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        self.active = index;
        self.refresh_strip(cx);
        self.request_title_refresh(cx);
        cx.notify();
    }

    /// 新建空白 tab 并切到它。单文档模式下不动作。
    pub(crate) fn open_new_tab(&mut self, cx: &mut Context<Self>) {
        if !self.multi_tab {
            return;
        }
        self.add_tab(String::new(), None, cx);
        self.request_title_refresh(cx);
    }

    /// 请求关闭当前活动 tab：走编辑器的未保存检查流程，确认后按 tab 目标关闭。
    /// 单文档模式下退化为关闭窗口（等同旧版「关闭窗口」）。
    pub(crate) fn request_close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scope = if self.multi_tab {
            CloseScope::Tab
        } else {
            CloseScope::Window
        };
        let editor = self.active_editor();
        editor.update(cx, |editor, cx| {
            editor.set_close_scope(scope);
            editor.request_close_current_window(window, cx);
        });
    }

    /// 请求关闭指定下标的 tab，不切换活动 tab。
    /// 与 `request_close_active_tab` 的区别：脏 tab 的确认框直接在该 editor 上弹出，
    /// 关闭完成由 `close_editor_tab` 按实体定位移除，全程不改变 `active`。
    pub(crate) fn request_close_tab_at(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.multi_tab {
            self.request_close_active_tab(window, cx);
            return;
        }
        if index >= self.tabs.len() {
            return;
        }
        let editor = self.tabs[index].editor.clone();
        editor.update(cx, |editor, cx| {
            editor.set_close_scope(CloseScope::Tab);
            editor.request_close_current_window(window, cx);
        });
    }

    /// 编辑器完成关闭确认（保存/放弃）后回调：从 host 移除对应 tab，
    /// 若正有关窗/退出流程挂起则推进到下一个脏 tab。
    pub(crate) fn close_editor_tab(
        &mut self,
        editor: Entity<Editor>,
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.editor == editor) {
            self.close_tab(index, cx);
            self.request_title_refresh(cx);
        }
        if self.pending_window_close.is_some() || self.pending_batch_close.is_some() {
            // 推进需要 `&mut Window` 弹窗；回到窗口的 update 闭包里同时取 window 与 host。
            cx.spawn(async move |_, cx| {
                let _ = window_handle.update(cx, |view: AnyView, window, cx| {
                    if let Ok(host) = view.downcast::<DocumentHost>() {
                        host.update(cx, |host, cx| {
                            host.advance_batch_close(window, cx);
                            host.advance_window_close(window, cx);
                        });
                    }
                });
            })
            .detach();
        }
    }

    /// 取消批量关闭流程（用户点「继续编辑」）。
    pub(crate) fn cancel_batch_close(&mut self, cx: &mut Context<Self>) {
        if self.pending_batch_close.take().is_some() {
            cx.notify();
        }
    }

    /// 请求关闭「除 index 外」的所有 tab（右键菜单项），逐个确认脏 tab。
    pub(crate) fn request_close_other_tabs(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.multi_tab || index >= self.tabs.len() {
            return;
        }
        let targets = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, tab)| tab.editor.clone())
            .collect::<Vec<_>>();
        self.pending_batch_close = Some(targets);
        self.advance_batch_close(window, cx);
    }

    /// 请求关闭 index 右侧的所有 tab（右键菜单项），逐个确认脏 tab。
    pub(crate) fn request_close_tabs_to_right(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.multi_tab || index >= self.tabs.len() {
            return;
        }
        let targets = self.tabs[index + 1..]
            .iter()
            .map(|tab| tab.editor.clone())
            .collect::<Vec<_>>();
        self.pending_batch_close = Some(targets);
        self.advance_batch_close(window, cx);
    }

    /// 在独立窗口打开指定 tab 的文档（右键菜单项）。
    /// 有磁盘路径时用路径打开（与「在新窗口打开文件」一致），否则用当前内容开空白文档窗口。
    pub(crate) fn open_tab_in_new_window(&mut self, index: usize, cx: &mut App) {
        if index >= self.tabs.len() {
            return;
        }
        let (markdown, path) = self.tabs[index].editor.read_with(cx, |editor, cx| {
            (editor.current_document_source(cx), editor.file_path.clone())
        });
        if let Some(path) = path {
            crate::app_menu::open_editor_window(cx, markdown, Some(path));
        } else {
            crate::app_menu::open_editor_window(cx, markdown, None);
        }
    }

    /// 推进批量关闭：找到挂起集合里第一个脏 tab 走确认，干净的直接移除，全部完成清空挂起。
    fn advance_batch_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(targets) = self.pending_batch_close.clone() else {
            return;
        };
        // 已被前面步骤移除的 tab 直接跳过。
        let remaining = targets
            .into_iter()
            .filter(|editor| self.tabs.iter().any(|tab| &tab.editor == editor))
            .collect::<Vec<_>>();
        if remaining.is_empty() {
            self.pending_batch_close = None;
            cx.notify();
            return;
        }

        // 先移除集合里所有干净 tab（无需确认），脏 tab 留在挂起集合等待逐个确认。
        let mut dirty = Vec::new();
        for editor in remaining {
            let Some(index) = self.tabs.iter().position(|tab| tab.editor == editor) else {
                continue;
            };
            if editor.read(cx).document_dirty {
                dirty.push(editor);
            } else {
                self.close_tab(index, cx);
            }
        }
        if dirty.is_empty() {
            self.pending_batch_close = None;
            self.request_title_refresh(cx);
            cx.notify();
            return;
        }
        // 保留全部脏 tab：当前确认的 editor 关闭后，close_editor_tab 会再次推进到下一个。
        self.pending_batch_close = Some(dirty.clone());
        let editor = dirty.remove(0);
        editor.update(cx, |editor, cx| {
            editor.set_close_scope(CloseScope::BatchFlow);
            editor.on_window_should_close(window, cx);
        });
        cx.notify();
    }

    /// 取消关窗/退出流程。
    pub(crate) fn cancel_window_close(&mut self, cx: &mut Context<Self>) {
        if self.pending_window_close.take().is_some() {
            cx.notify();
        }
    }

    /// 请求关闭整个窗口或退出：逐个确认脏 tab，全部通过后执行最终动作。
    pub(crate) fn request_window_close(
        &mut self,
        flow: WindowCloseFlow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_window_close = Some(flow);
        self.advance_window_close(window, cx);
    }

    /// 推进关窗流程：找到第一个脏 tab 走确认，否则执行窗口关闭/退出。
    fn advance_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(flow) = self.pending_window_close else {
            return;
        };
        // 从活动位置起找第一个脏 tab，减少来回切换。
        let len = self.tabs.len();
        let dirty = (0..len)
            .map(|offset| (self.active + offset) % len)
            .find(|&index| self.tabs[index].editor.read(cx).document_dirty);
        if let Some(index) = dirty {
            self.active = index;
            let editor = self.tabs[index].editor.clone();
            editor.update(cx, |editor, cx| {
                editor.set_close_scope(CloseScope::WindowFlow);
                editor.on_window_should_close(window, cx);
            });
            self.refresh_strip(cx);
            return;
        }

        // 无脏 tab：清除挂起态并关闭本窗口。
        // 退出不再在此直接 `cx.quit()`：多窗口混合退出时还存在其他编辑器窗口，
        // 须让它们各自完成未保存确认；最终由 app_menu 的 `handle_window_closed`
        // 在窗口归零时统一退出（`flow` 仅用于区分调用来源，保留供后续扩展）。
        let _ = flow;
        self.pending_window_close = None;
        window.close_dialog(cx);
        window.remove_window();
    }

    /// 切换到下一个 tab（末尾回绕到第一个）。单文档模式下不动作。
    pub(crate) fn select_next_tab(&mut self, cx: &mut Context<Self>) {
        if !self.multi_tab {
            return;
        }
        let next = (self.active + 1) % self.tabs.len();
        self.set_active(next, cx);
    }

    /// 切换到上一个 tab（开头回绕到最后一个）。单文档模式下不动作。
    pub(crate) fn select_previous_tab(&mut self, cx: &mut Context<Self>) {
        if !self.multi_tab {
            return;
        }
        let previous = (self.active + self.tabs.len() - 1) % self.tabs.len();
        self.set_active(previous, cx);
    }

    /// 把 tab 从 `from` 移动到 `to`，并让被拖动的 tab 保持活动。
    /// 用于同窗拖拽重排；越界或原地不动时忽略。
    pub(crate) fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if !self.multi_tab || from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let entry = self.tabs.remove(from);
        self.tabs.insert(to, entry);
        self.active = to;
        self.refresh_strip(cx);
        cx.notify();
    }

    /// 关闭指定 tab，返回是否关闭了活动 tab。
    /// 至少保留一个 tab：最后一个 tab 关闭时清空其内容为空白文档。
    pub(crate) fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            let editor = self.create_editor(String::new(), None, cx);
            self.tabs.push(TabEntry { editor });
            self.active = 0;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if index < self.active {
            self.active -= 1;
        }
        self.refresh_strip(cx);
        cx.notify();
        true
    }

    /// 汇总：任一 tab 脏则为真（窗口 edited 状态与会话持久化用）。
    pub(crate) fn any_dirty(&self, cx: &App) -> bool {
        self.tabs.iter().any(|tab| {
            tab.editor
                .read_with(cx, |editor, _cx| editor.document_dirty)
        })
    }

    /// 活动 tab 的路径（窗口标题与会话持久化用）。
    pub(crate) fn active_file_path(&self, cx: &App) -> Option<PathBuf> {
        self.tabs[self.active]
            .editor
            .read_with(cx, |editor, _cx| editor.file_path.clone())
    }

    /// 在 host 中打开一个文件路径：空白活动 tab 就地载入，否则新建 tab。
    /// 返回是否成功打开（读文件失败返回 false，交调用方提示）。
    pub(crate) fn open_path_in_tab(&mut self, path: &Path, cx: &mut Context<Self>) -> bool {
        let Ok(markdown) = super::read_markdown_file(path) else {
            return false;
        };
        if self.active_is_blank(cx) {
            self.load_active_from_path(path, cx);
        } else {
            self.add_tab(markdown, Some(path.to_path_buf()), cx);
            self.request_title_refresh(cx);
        }
        true
    }

    /// 活动 tab 是否为「用户尚未输入的空白文档」（打开文件复用判断用）。
    pub(crate) fn active_is_blank(&self, cx: &App) -> bool {
        self.tabs[self.active]
            .editor
            .read_with(cx, |editor, cx| editor.is_blank_document(cx))
    }

    /// 把文件载入活动 tab；成功时同步刷新标题与 tab 条标签。
    pub(crate) fn load_active_from_path(&mut self, path: &Path, cx: &mut Context<Self>) -> bool {
        let loaded = self.tabs[self.active].editor.update(cx, |editor, cx| {
            editor.replace_document_from_path(path, cx).is_ok()
        });
        if loaded {
            self.refresh_strip(cx);
            self.request_title_refresh(cx);
            cx.notify();
        }
        loaded
    }

    /// 刷新 tab 条标签与选中态。
    fn refresh_strip(&self, cx: &mut Context<Self>) {
        let labels = self
            .tabs
            .iter()
            .map(|tab| {
                tab.editor.read_with(cx, |editor, cx| {
                    let dirty = editor.document_dirty;
                    let name = match editor.file_path.as_deref() {
                        Some(path) => path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Untitled".to_string()),
                        None => cx.global::<I18nManager>().snapshot().t("win_untitled", &[]),
                    };
                    if dirty { format!("● {name}") } else { name }
                })
            })
            .collect::<Vec<_>>();
        self.strip.update(cx, |strip, cx| {
            strip.selected = self.active;
            strip.labels = labels;
            cx.notify();
        });
    }

    /// 活动 tab 变更后标记窗口标题刷新（下一帧由活动 Editor 的 render 应用）。
    pub(crate) fn request_title_refresh(&self, cx: &mut Context<Self>) {
        self.tabs[self.active].editor.update(cx, |editor, cx| {
            editor.pending_window_title_refresh = true;
            cx.notify();
        });
    }
}

impl Render for DocumentHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 窗口级 chrome 由 host 统一同步：标题取活动 tab，脏标记取所有 tab 的并集。
        // 必须在挂载活动 Editor 之前完成，避免子 Editor 的旧逻辑覆盖。
        self.sync_window_chrome(window, cx);
        // 只有活动 tab 的 Editor 被挂载：框架调用它的 render，其余 tab 保持
        // Rust 状态但不产生元素，因此不消耗布局/绘制成本。
        self.tabs[self.active].editor.clone().into_element()
    }
}

/// tab 条右键菜单状态（自持菜单实体，随 `DismissEvent` 清除）。
struct TabMenuState {
    position: Point<Pixels>,
    menu: Entity<PopupMenu>,
    _subscription: Subscription,
}

/// tab 条视图：持有 host 弱引用，渲染一行 TabBar。
/// 左键切换、中键关闭、× 关闭、右键菜单（关闭/关闭其他/关闭右侧/新窗口打开）、拖拽重排。
pub(crate) struct TabStrip {
    host: WeakEntity<DocumentHost>,
    selected: usize,
    labels: Vec<String>,
    menu: Option<TabMenuState>,
}

/// 在右键坐标打开某个 tab 的框架弹出菜单（同时只开一个）。
fn open_tab_menu(
    strip: &Entity<TabStrip>,
    index: usize,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let host = strip.read(cx).host.clone();
    let i18n = cx.global::<I18nManager>().snapshot();
    let menu = PopupMenu::build(window, cx, |menu, _window, cx| {
        let close =
            PopupMenuItem::new(i18n.t("tab_menu_close", &[])).on_click(move |_, window, cx| {
                let host = host.clone();
                let _ = host.update(cx, |host, cx| host.request_close_tab_at(index, window, cx));
            });
        let host_others = strip.read(cx).host.clone();
        let close_others = PopupMenuItem::new(i18n.t("tab_menu_close_others", &[])).on_click(
            move |_, window, cx| {
                let host = host_others.clone();
                let _ = host.update(cx, |host, cx| {
                    host.request_close_other_tabs(index, window, cx)
                });
            },
        );
        let host_right = strip.read(cx).host.clone();
        let close_right = PopupMenuItem::new(i18n.t("tab_menu_close_right", &[])).on_click(
            move |_, window, cx| {
                let host = host_right.clone();
                let _ = host.update(cx, |host, cx| {
                    host.request_close_tabs_to_right(index, window, cx)
                });
            },
        );
        let host_new_window = strip.read(cx).host.clone();
        let open_new_window = PopupMenuItem::new(i18n.t("tab_menu_open_new_window", &[])).on_click(
            move |_, _window, cx| {
                let host = host_new_window.clone();
                let _ = host.update(cx, |host, cx| host.open_tab_in_new_window(index, cx));
            },
        );
        menu.item(close)
            .item(close_others)
            .item(close_right)
            .separator()
            .item(open_new_window)
    });
    // 关闭时清除 strip 持有的菜单状态（菜单实体随状态释放）。
    let weak = strip.downgrade();
    let subscription = window.subscribe(&menu, cx, move |_, _: &DismissEvent, _, cx| {
        if let Some(strip) = weak.upgrade() {
            strip.update(cx, |strip, cx| {
                strip.menu = None;
                cx.notify();
            });
        }
    });
    strip.update(cx, |strip, cx| {
        strip.menu = Some(TabMenuState {
            position,
            menu: menu.clone(),
            _subscription: subscription,
        });
        cx.notify();
    });
    menu.focus_handle(cx).focus(window, cx);
    window.refresh();
}

impl Render for TabStrip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self.host.clone();
        let host_change = host.clone();
        let strip = cx.entity();
        // TabBar 默认 `px(-1.)` 的左右 padding 会让首个 tab 贴到窗口边缘；
        // 这里补回与标题栏菜单一致的左间距，并留出右间距。
        let tab_bar = TabBar::new("document-tabs")
            .underline()
            .selected_index(self.selected)
            // 用 TabBar 的 on_change 统一处理点击与左右方向键；只挂在单个
            // Tab::on_click 上时，TabBar 内部命中/焦点态与选中态不同步。
            .on_change(move |index, _window, cx| {
                let _ = host_change.update(cx, |host, cx| host.set_active(index, cx));
            })
            .children(
                self.labels
                    .iter()
                    .enumerate()
                    .map(|(index, label)| {
                        let host_close = host.clone();
                        let host_middle = host.clone();
                        let host_drop = host.clone();
                        let strip_menu = strip.clone();
                        let tab_label = label.clone();
                        Tab::new()
                            .label(label.clone())
                            // 中键关闭：不切换活动 tab，直接对该 editor 走未保存确认。
                            .on_mouse_down(MouseButton::Middle, move |_event, window, cx| {
                                cx.stop_propagation();
                                let host = host_middle.clone();
                                let _ = host.update(cx, |host, cx| {
                                    host.request_close_tab_at(index, window, cx)
                                });
                            })
                            // 右键菜单：关闭/关闭其他/关闭右侧/新窗口打开。
                            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                                cx.stop_propagation();
                                open_tab_menu(&strip_menu, index, event.position, window, cx);
                            })
                            // 同窗拖拽重排：on_drop 的落点即当前 tab 下标。
                            .on_drag(
                                TabDragData {
                                    index,
                                    title: tab_label.clone(),
                                    position: Point::default(),
                                },
                                move |data: &TabDragData, position, _window, cx| {
                                    cx.new(|_| TabDragData {
                                        index: data.index,
                                        title: data.title.clone(),
                                        position,
                                    })
                                },
                            )
                            .drag_over::<TabDragData>(|style, _, _, _| {
                                style.bg(hsla(0.58, 0.9, 0.55, 0.25))
                            })
                            .on_drop(move |dragged: &TabDragData, _window, cx| {
                                let from = dragged.index;
                                let host = host_drop.clone();
                                let _ = host.update(cx, |host, cx| host.move_tab(from, index, cx));
                            })
                            .suffix(
                                div()
                                    .id(("tab-close", index))
                                    .on_click(move |event, window, cx| {
                                        // 关闭按钮不再冒泡到 Tab 的选中点击。
                                        cx.stop_propagation();
                                        let _ = event;
                                        let host = host_close.clone();
                                        let _ = host.update(cx, |host, cx| {
                                            host.request_close_tab_at(index, window, cx)
                                        });
                                    })
                                    .child("×"),
                            )
                    })
                    .collect::<Vec<_>>(),
            );
        // 右键菜单宿主：deferred + anchored 挂到右键坐标（同框架 ContextMenu 模式）。
        let menu_element = self.menu.as_ref().map(|state| {
            let menu = state.menu.clone();
            let position = state.position;
            if !menu.focus_handle(cx).contains_focused(window, cx) {
                menu.focus_handle(cx).focus(window, cx);
            }
            deferred(
                anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.0))
                    .child(menu),
            )
            .with_priority(1)
            .into_any()
        });
        div()
            .w_full()
            .pl(TITLE_BAR_LEFT_PADDING)
            .pr(px(12.0))
            .child(tab_bar)
            .children(menu_element)
    }
}

#[cfg(test)]
mod tests {
    use super::DocumentHost;
    use crate::i18n::I18nManager;
    use rgpui::{AppContext, TestAppContext};

    fn init_test_app(cx: &mut TestAppContext) {
        cx.update(|cx| {
            I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
    }

    fn redraw(cx: &mut rgpui::VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    #[rgpui::test]
    async fn host_starts_with_single_tab_and_active_editor(cx: &mut TestAppContext) {
        init_test_app(cx);
        let host = cx.new(|cx| DocumentHost::new("hello".to_string(), None, cx));
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            assert_eq!(host.tab_count(), 1);
            assert_eq!(host.active_index(), 0);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "hello"
            );
        });
    }

    #[rgpui::test]
    async fn add_switch_close_tabs_maintain_active_index(cx: &mut TestAppContext) {
        init_test_app(cx);
        let host = cx.new(|cx| DocumentHost::new("first".to_string(), None, cx));
        cx.run_until_parked();

        host.update(cx, |host, cx| {
            host.add_tab("second".to_string(), None, cx);
        });
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            assert_eq!(host.tab_count(), 2);
            assert_eq!(host.active_index(), 1);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "second"
            );
        });

        host.update(cx, |host, cx| host.set_active(0, cx));
        host.read_with(cx, |host, cx| {
            assert_eq!(host.active_index(), 0);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "first"
            );
        });

        // 关闭非活动 tab，活动索引前移。
        host.update(cx, |host, cx| host.close_tab(0, cx));
        host.read_with(cx, |host, cx| {
            assert_eq!(host.tab_count(), 1);
            assert_eq!(host.active_index(), 0);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "second"
            );
        });
    }

    #[rgpui::test]
    async fn closing_last_tab_keeps_one_blank_tab(cx: &mut TestAppContext) {
        init_test_app(cx);
        let host = cx.new(|cx| DocumentHost::new("only".to_string(), None, cx));
        cx.run_until_parked();
        host.update(cx, |host, cx| host.close_tab(0, cx));
        host.read_with(cx, |host, _cx| {
            assert_eq!(host.tab_count(), 1);
            assert_eq!(host.active_index(), 0);
        });
    }

    #[rgpui::test]
    async fn inactive_tab_keeps_its_document_state(cx: &mut TestAppContext) {
        init_test_app(cx);
        let host = cx.new(|cx| DocumentHost::new("first".to_string(), None, cx));
        host.update(cx, |host, cx| {
            host.add_tab("second".to_string(), None, cx);
        });
        cx.run_until_parked();

        // 切回第一个 tab，文档内容未因不渲染而丢失。
        host.update(cx, |host, cx| host.set_active(0, cx));
        host.read_with(cx, |host, cx| {
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "first"
            );
        });
    }

    #[rgpui::test]
    async fn tab_cycling_wraps_around(cx: &mut TestAppContext) {
        init_test_app(cx);
        let host = cx.new(|cx| DocumentHost::new("first".to_string(), None, cx));
        host.update(cx, |host, cx| {
            host.add_tab("second".to_string(), None, cx);
            host.add_tab("third".to_string(), None, cx);
        });
        cx.run_until_parked();

        // 第三个 tab 上「下一个」回到第一个。
        host.update(cx, |host, cx| host.select_next_tab(cx));
        host.read_with(cx, |host, cx| {
            assert_eq!(host.active_index(), 0);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "first"
            );
        });
        // 第一个 tab 上「上一个」回到最后一个。
        host.update(cx, |host, cx| host.select_previous_tab(cx));
        host.read_with(cx, |host, cx| {
            assert_eq!(host.active_index(), 2);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "third"
            );
        });
    }

    #[rgpui::test]
    async fn open_path_reuses_blank_tab_then_opens_new_tab(cx: &mut TestAppContext) {
        init_test_app(cx);
        let path = std::env::temp_dir().join(format!(
            "rumark-host-open-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "from file").expect("write markdown");

        // 空白 tab 就地载入，不新增 tab。
        let host = cx.new(|cx| DocumentHost::new(String::new(), None, cx));
        host.update(cx, |host, cx| {
            assert!(host.open_path_in_tab(&path, cx));
            assert_eq!(host.tab_count(), 1);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "from file"
            );
        });

        // 有内容的 tab 上再打开，新增一个 tab。
        host.update(cx, |host, cx| {
            assert!(host.open_path_in_tab(&path, cx));
            assert_eq!(host.tab_count(), 2);
            assert_eq!(host.active_index(), 1);
        });

        let _ = std::fs::remove_file(&path);
    }

    #[rgpui::test]
    async fn single_tab_mode_keeps_one_tab_and_ignores_tab_actions(cx: &mut TestAppContext) {
        init_test_app(cx);
        cx.update(|cx| crate::config::EditorSettings::set_multi_tab_enabled(cx, false));
        let host = cx.new(|cx| DocumentHost::new("only".to_string(), None, cx));
        cx.run_until_parked();

        host.update(cx, |host, cx| {
            assert_eq!(host.tab_count(), 1);
            assert!(!host.multi_tab);
            // 多 tab 动作在单文档模式下全部无动作。
            host.open_new_tab(cx);
            host.select_next_tab(cx);
            host.select_previous_tab(cx);
            assert_eq!(host.tab_count(), 1);
            assert_eq!(host.active_index(), 0);
        });
        host.read_with(cx, |host, cx| {
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "only"
            );
        });

        // 恢复默认，避免污染同进程其他测试。
        cx.update(|cx| crate::config::EditorSettings::set_multi_tab_enabled(cx, true));
    }

    #[rgpui::test]
    async fn window_chrome_aggregates_dirty_state_across_tabs(cx: &mut TestAppContext) {
        init_test_app(cx);
        let (host, cx) = cx.add_window_view(|_window, cx| {
            DocumentHost::new(
                "active".to_string(),
                Some(std::path::PathBuf::from("active.md")),
                cx,
            )
        });
        host.update(cx, |host, cx| {
            host.add_tab(
                "other".to_string(),
                Some(std::path::PathBuf::from("other.md")),
                cx,
            );
        });
        cx.run_until_parked();

        // 只有非活动 tab 变脏，窗口标题也必须带脏标记。
        let inactive = host.read_with(cx, |host, _cx| host.tabs[0].editor.clone());
        inactive.update(cx, |editor, cx| editor.mark_dirty(cx));
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            assert!(host.any_dirty(cx));
        });

        cx.update(|window, cx| {
            host.update(cx, |host, cx| host.sync_window_chrome(window, cx));
        });
        host.read_with(cx, |host, cx| {
            assert_eq!(host.last_window_edited, Some(true));
            let marker = cx
                .global::<I18nManager>()
                .snapshot()
                .t("win_dirty_marker", &[]);
            let title = host
                .last_window_title
                .as_ref()
                .expect("title should be set");
            assert!(title.starts_with(&marker), "title: {title}");
            // add_tab 会把新 tab 变为活动 tab；文件名取活动 tab，脏标记来自另一个 tab。
            assert!(title.contains("other.md"), "title: {title}");
        });

        // 所有 tab 干净后，edited 标记与标题脏标记都要清掉。
        host.update(cx, |host, cx| {
            host.tabs[0]
                .editor
                .update(cx, |editor, _cx| editor.document_dirty = false);
            cx.notify();
        });
        cx.update(|window, cx| {
            host.update(cx, |host, cx| host.sync_window_chrome(window, cx));
        });
        host.read_with(cx, |host, cx| {
            assert_eq!(host.last_window_edited, Some(false));
            let marker = cx
                .global::<I18nManager>()
                .snapshot()
                .t("win_dirty_marker", &[]);
            let title = host
                .last_window_title
                .as_ref()
                .expect("title should be set");
            assert!(!title.starts_with(&marker), "title: {title}");
        });
    }

    /// 读取全部 tab 的文档内容（测试辅助）。
    fn tab_sources(host: &DocumentHost, cx: &rgpui::App) -> Vec<String> {
        (0..host.tab_count())
            .map(|i| {
                host.tabs[i]
                    .editor
                    .read_with(cx, |editor, cx| editor.current_document_source(cx))
            })
            .collect()
    }

    #[rgpui::test]
    async fn move_tab_reorders_and_follows_dragged_tab(cx: &mut TestAppContext) {
        init_test_app(cx);
        let host = cx.new(|cx| DocumentHost::new("first".to_string(), None, cx));
        host.update(cx, |host, cx| {
            host.add_tab("second".to_string(), None, cx);
            host.add_tab("third".to_string(), None, cx);
            host.set_active(0, cx);
        });
        cx.run_until_parked();

        // 把第一个 tab 拖到末尾，活动 tab 跟随被拖动的 tab。
        host.update(cx, |host, cx| host.move_tab(0, 2, cx));
        host.read_with(cx, |host, cx| {
            assert_eq!(tab_sources(host, cx), ["second", "third", "first"]);
            assert_eq!(host.active_index(), 2);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "first"
            );
        });

        // 越界与原地拖动均无副作用。
        host.update(cx, |host, cx| {
            host.move_tab(0, 99, cx);
            host.move_tab(1, 1, cx);
        });
        host.read_with(cx, |host, cx| {
            assert_eq!(tab_sources(host, cx), ["second", "third", "first"]);
            assert_eq!(host.active_index(), 2);
        });
    }

    #[rgpui::test]
    async fn closing_inactive_tab_does_not_switch_active(cx: &mut TestAppContext) {
        init_test_app(cx);
        let (host, cx) =
            cx.add_window_view(|_window, cx| DocumentHost::new("first".to_string(), None, cx));
        host.update(cx, |host, cx| {
            host.add_tab("second".to_string(), None, cx);
            host.add_tab("third".to_string(), None, cx);
            host.set_active(1, cx); // 活动 = second
        });
        cx.run_until_parked();

        // 请求关闭非活动的 third（index 2），不切换活动 tab。
        cx.update(|window, cx| {
            host.update(cx, |host, cx| host.request_close_tab_at(2, window, cx));
        });
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            assert_eq!(tab_sources(host, cx), ["first", "second"]);
            assert_eq!(host.active_index(), 1);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "second"
            );
        });
    }

    #[rgpui::test]
    async fn close_other_tabs_keeps_only_target(cx: &mut TestAppContext) {
        init_test_app(cx);
        let (host, cx) =
            cx.add_window_view(|_window, cx| DocumentHost::new("first".to_string(), None, cx));
        host.update(cx, |host, cx| {
            host.add_tab("second".to_string(), None, cx);
            host.add_tab("third".to_string(), None, cx);
        });
        cx.run_until_parked();

        // 关闭除 index 1（second）外的所有 tab。
        cx.update(|window, cx| {
            host.update(cx, |host, cx| host.request_close_other_tabs(1, window, cx));
        });
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            assert_eq!(tab_sources(host, cx), ["second"]);
            assert_eq!(host.active_index(), 0);
            assert!(host.pending_batch_close.is_none());
        });
    }

    #[rgpui::test]
    async fn close_tabs_to_right_keeps_left_and_target(cx: &mut TestAppContext) {
        init_test_app(cx);
        let (host, cx) =
            cx.add_window_view(|_window, cx| DocumentHost::new("first".to_string(), None, cx));
        host.update(cx, |host, cx| {
            host.add_tab("second".to_string(), None, cx);
            host.add_tab("third".to_string(), None, cx);
            host.add_tab("fourth".to_string(), None, cx);
            host.set_active(0, cx);
        });
        cx.run_until_parked();

        // 关闭 index 1（second）右侧的所有 tab。
        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.request_close_tabs_to_right(1, window, cx)
            });
        });
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            assert_eq!(tab_sources(host, cx), ["first", "second"]);
            assert!(host.pending_batch_close.is_none());
        });
    }

    /// 渲染冒烟：3 个 tab 的窗口能完整绘制，tab 条标签与活动文档一致（手动目检的自动化替代）。
    #[rgpui::test]
    async fn three_tab_window_renders_with_correct_strip_labels(cx: &mut TestAppContext) {
        init_test_app(cx);
        let (host, cx) =
            cx.add_window_view(|_window, cx| DocumentHost::new("文档一".to_string(), None, cx));
        cx.update(|_window, cx| {
            host.update(cx, |host, cx| {
                host.add_tab("文档二".to_string(), None, cx);
                host.add_tab("文档三".to_string(), None, cx);
            });
        });
        cx.run_until_parked();
        // 绘制若干帧，覆盖 tab 条、指示器动画与活动 Editor 的渲染路径。
        for _ in 0..3 {
            redraw(cx);
        }
        host.read_with(cx, |host, cx| {
            assert_eq!(host.tab_count(), 3);
            let strip = host.strip.read(cx);
            assert_eq!(strip.labels.len(), 3);
            assert_eq!(strip.selected, 2);
            assert!(
                strip.labels.iter().all(|label| !label.trim().is_empty()),
                "每个 tab 都应有可见标签: {:?}",
                strip.labels
            );
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "文档三"
            );
        });
    }

    #[rgpui::test]
    async fn middle_click_closes_tab_without_switching(cx: &mut TestAppContext) {
        init_test_app(cx);
        let (host, cx) =
            cx.add_window_view(|_window, cx| DocumentHost::new("first".to_string(), None, cx));
        cx.update(|_window, cx| {
            host.update(cx, |host, cx| {
                host.add_tab("second".to_string(), None, cx);
                host.set_active(1, cx); // 活动 = second（index 1）
            });
        });
        cx.update(|window, _cx| window.activate_window());
        cx.run_until_parked();
        redraw(cx);

        // 中键点击第一个 tab（x=30, y=49），关闭非活动的 first，活动仍是 second。
        let point = rgpui::point(rgpui::px(30.0), rgpui::px(49.0));
        cx.simulate_mouse_down(point, rgpui::MouseButton::Middle, rgpui::Modifiers::none());
        cx.run_until_parked();
        redraw(cx);

        host.read_with(cx, |host, cx| {
            assert_eq!(tab_sources(host, cx), ["second"]);
            assert_eq!(host.active_index(), 0);
            assert_eq!(
                host.active_editor().read(cx).current_document_source(cx),
                "second"
            );
        });
    }

    #[rgpui::test]
    async fn right_click_opens_tab_context_menu(cx: &mut TestAppContext) {
        init_test_app(cx);
        let (host, cx) =
            cx.add_window_view(|_window, cx| DocumentHost::new("first".to_string(), None, cx));
        cx.update(|_window, cx| {
            host.update(cx, |host, cx| {
                host.add_tab("second".to_string(), None, cx);
            });
        });
        cx.update(|window, _cx| window.activate_window());
        cx.run_until_parked();
        redraw(cx);

        // 右键点击第一个 tab 的文字区域，应弹出 tab 菜单。
        let point = rgpui::point(rgpui::px(30.0), rgpui::px(49.0));
        cx.simulate_mouse_down(point, rgpui::MouseButton::Right, rgpui::Modifiers::none());
        cx.run_until_parked();
        redraw(cx);

        host.read_with(cx, |host, cx| {
            assert!(
                host.strip.read(cx).menu.is_some(),
                "右键 tab 应弹出上下文菜单"
            );
        });
    }
}
