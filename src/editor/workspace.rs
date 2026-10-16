//! 轻量工作区面板状态、文件树扫描与大纲解析。

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context as _, Result};
use rgpui::list::ListItem;
use rgpui::prelude::FluentBuilder as _;
use rgpui::tabs::{Tabs, TabsItem};
use rgpui::tree::{Tree, TreeEntry, TreeEvent, TreeItem, TreeState};
use rgpui::*;

use super::{BlockKind, Editor};
use crate::i18n::I18nSnapshot;
use crate::theme::{ThemeDimensions, ThemeTypography};

const WORKSPACE_TARGET_RATIO: f32 = 0.15;
const WORKSPACE_MIN_WIDTH: f32 = 240.0;
const WORKSPACE_MAX_WIDTH: f32 = 360.0;
/// 用户拖拽时允许的宽度范围（比自动档更宽，窄屏/宽屏都可用）。
pub(super) const WORKSPACE_PANEL_RESIZE_MIN_WIDTH: f32 = 180.0;
pub(super) const WORKSPACE_PANEL_RESIZE_MAX_WIDTH: f32 = 560.0;
const WORKSPACE_ROW_HEIGHT: f32 = 28.0;
const WORKSPACE_ROW_INDENT: f32 = 18.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum WorkspaceTab {
    #[default]
    Files,
    Outline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceTreeKind {
    Directory(PathBuf),
    MarkdownFile(PathBuf),
    Heading { line: usize, level: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WorkspaceTreeNode {
    id: String,
    label: String,
    kind: WorkspaceTreeKind,
    children: Vec<WorkspaceTreeNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkspaceSelection {
    File(PathBuf),
    Outline(String),
}

#[derive(Default)]
pub(super) struct WorkspaceState {
    pub(super) is_open: bool,
    active_tab: WorkspaceTab,
    root: Option<PathBuf>,
    file_tree: Option<WorkspaceTreeNode>,
    file_error: Option<String>,
    /// 文件节点标识到种类的映射（框架树行渲染时查图标与点击行为）。
    file_kind_map: Rc<BTreeMap<String, WorkspaceTreeKind>>,
    outline_tree: Vec<WorkspaceTreeNode>,
    outline_source: Option<String>,
    expanded: HashSet<String>,
    selected: Option<WorkspaceSelection>,
    /// 用户拖拽后的侧边栏宽度（`None` 表示跟随视口自动计算）。
    pub(super) panel_width: Option<f32>,
    /// 侧边栏拖拽会话（按下时的指针横坐标与起始宽度）。
    pub(super) resize_drag: Option<WorkspacePanelResizeDrag>,
}

/// 侧边栏宽度拖拽会话。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct WorkspacePanelResizeDrag {
    start_x: f32,
    start_width: f32,
}

impl Editor {
    /// 开关工作区抽屉（打开时同步文件树与大纲）。
    pub(crate) fn toggle_workspace_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.is_open {
            self.workspace.is_open = false;
            self.pending_workspace_open = None;
        } else {
            self.dismiss_contextual_overlays(cx);
            self.workspace.is_open = true;
            self.sync_workspace_models(cx);
            window.activate_window();
        }
        cx.notify();
    }

    /// 响应开关工作区动作。
    pub(crate) fn on_toggle_workspace_action(
        &mut self,
        _: &crate::components::ToggleWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_workspace_drawer(window, cx);
    }

    /// 文档路径变化后重置工作区模型（打开时即时同步）。
    pub(super) fn sync_workspace_after_document_path_change(&mut self, cx: &mut Context<Self>) {
        self.workspace.root = None;
        self.workspace.file_tree = None;
        self.workspace.file_error = None;
        self.workspace.outline_source = None;
        if self.workspace.is_open {
            self.sync_workspace_models(cx);
        }
    }

    /// 同步工作区文件树与大纲模型。
    fn sync_workspace_models(&mut self, cx: &mut Context<Self>) {
        self.sync_workspace_file_tree(cx);
        self.sync_workspace_outline(cx);
    }

    /// 获取当前文件所在目录作为工作区根目录。
    fn workspace_root_for_current_file(&self) -> Option<PathBuf> {
        self.file_path.as_ref()?.parent().map(Path::to_path_buf)
    }

    /// 同步工作区文件树（根目录不变时仅更新选中项）。
    fn sync_workspace_file_tree(&mut self, cx: &mut Context<Self>) {
        let next_root = self.workspace_root_for_current_file();
        if self.workspace.root == next_root && self.workspace.file_tree.is_some() {
            self.workspace.selected = self
                .file_path
                .as_ref()
                .map(|path| WorkspaceSelection::File(path.clone()));
            self.sync_file_tree_selection(cx);
            return;
        }

        self.workspace.root = next_root.clone();
        self.workspace.file_tree = None;
        self.workspace.file_error = None;

        let Some(root) = next_root else {
            self.workspace.selected = None;
            self.sync_file_tree_state(cx);
            return;
        };

        // 校验根路径是否有效。
        if root.as_os_str().is_empty() {
            self.workspace.file_error = Some("Invalid workspace path: empty path".to_string());
            self.workspace.selected = None;
            self.sync_file_tree_state(cx);
            return;
        }

        match scan_workspace_dir(&root) {
            Ok(tree) => {
                self.workspace.expanded.insert(tree.id.clone());
                self.workspace.file_tree = Some(tree);
                self.workspace.selected = self
                    .file_path
                    .as_ref()
                    .map(|path| WorkspaceSelection::File(path.clone()));
            }
            Err(err) => {
                self.workspace.file_error = Some(err.to_string());
            }
        }
        self.sync_file_tree_state(cx);
    }

    /// 由文件树模型重建框架树（含种类映射；恢复展开与选中）。
    fn sync_file_tree_state(&mut self, cx: &mut Context<Self>) {
        let items = self
            .workspace
            .file_tree
            .as_ref()
            .map(|root| vec![file_tree_item(root, &self.workspace.expanded)])
            .unwrap_or_default();
        let mut kinds = BTreeMap::new();
        if let Some(root) = self.workspace.file_tree.as_ref() {
            collect_file_kinds(root, &mut kinds);
        }
        self.workspace.file_kind_map = Rc::new(kinds);
        let selected_id = match &self.workspace.selected {
            Some(WorkspaceSelection::File(path)) => Some(file_node_id(path)),
            _ => None,
        };
        self.file_tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
            if let Some(id) = selected_id {
                state.set_selected_item(Some(&TreeItem::new(id, "")), cx);
            }
        });
    }

    /// 文件未变时同步框架树选中（跨文件导航跟随高亮；大纲选中时清空）。
    fn sync_file_tree_selection(&mut self, cx: &mut Context<Self>) {
        if self.workspace.active_tab != WorkspaceTab::Files {
            return;
        }
        let expected = match &self.workspace.selected {
            Some(WorkspaceSelection::File(path)) => Some(file_node_id(path)),
            _ => None,
        };
        let current = self
            .file_tree_state
            .read(cx)
            .selected_item()
            .map(|item| item.id.to_string());
        if current == expected {
            return;
        }
        if let Some(id) = expected {
            self.file_tree_state.update(cx, |state, cx| {
                state.set_selected_item(Some(&TreeItem::new(id, "")), cx);
            });
        } else {
            self.file_tree_state.update(cx, |state, cx| {
                state.set_selected_index(None, cx);
            });
        }
    }

    /// 框架文件树事件：展开/折叠回写 `expanded`，点击/回车确认文件延迟打开（目录仅切换展开）。
    pub(super) fn on_workspace_tree_event(
        &mut self,
        _state: Entity<TreeState>,
        event: &TreeEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            TreeEvent::Expanded(id) => {
                self.workspace.expanded.insert(id.to_string());
            }
            TreeEvent::Collapsed(id) => {
                self.workspace.expanded.remove(id.as_str());
            }
            TreeEvent::Selected(id) | TreeEvent::Confirmed(id) => {
                let kind = self.workspace.file_kind_map.get(id.as_str()).cloned();
                // 事件无 `Window`，文件打开延迟到渲染侧消费。
                if let Some(WorkspaceTreeKind::MarkdownFile(path)) = kind {
                    self.workspace.selected = Some(WorkspaceSelection::File(path.clone()));
                    self.pending_workspace_open = Some(path);
                    // 文件选中即清掉大纲高亮（旧自研大纲按 `selected` 派生高亮，框架树需显式同步）。
                    self.sync_outline_tree_selection(cx);
                    cx.notify();
                }
            }
        }
    }

    /// 按当前文档源码同步大纲树（源码未变时跳过）。
    fn sync_workspace_outline(&mut self, cx: &mut Context<Self>) {
        let source = self.serialized_document_text(cx);
        if self.workspace.outline_source.as_deref() == Some(source.as_str()) {
            return;
        }

        let outline = build_outline_tree(&source);
        prune_outline_state(&mut self.workspace, &outline);
        self.workspace.outline_tree = outline;
        self.workspace.outline_source = Some(source);
        self.sync_outline_tree_state(cx);
    }

    /// 由大纲模型重建框架树（含恢复展开与选中）。
    fn sync_outline_tree_state(&mut self, cx: &mut Context<Self>) {
        let items = self
            .workspace
            .outline_tree
            .iter()
            .map(|node| outline_tree_item(node, &self.workspace.expanded))
            .collect::<Vec<_>>();
        let selected_id = match &self.workspace.selected {
            Some(WorkspaceSelection::Outline(id)) => Some(id.clone()),
            _ => None,
        };
        self.outline_tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
            match selected_id {
                Some(id) => state.set_selected_item(Some(&TreeItem::new(id, "")), cx),
                None => state.set_selected_index(None, cx),
            }
        });
    }

    /// 同步框架大纲树选中（与 `workspace.selected` 对齐，不一致时才写）。
    fn sync_outline_tree_selection(&mut self, cx: &mut Context<Self>) {
        let expected = match &self.workspace.selected {
            Some(WorkspaceSelection::Outline(id)) => Some(id.clone()),
            _ => None,
        };
        let current = self
            .outline_tree_state
            .read(cx)
            .selected_item()
            .map(|item| item.id.to_string());
        if current == expected {
            return;
        }
        match expected {
            Some(id) => self.outline_tree_state.update(cx, |state, cx| {
                state.set_selected_item(Some(&TreeItem::new(id, "")), cx);
            }),
            None => self.outline_tree_state.update(cx, |state, cx| {
                state.set_selected_index(None, cx);
            }),
        }
    }

    /// 框架大纲树事件：展开/折叠回写 `expanded`，点击/回车确认标题选中并跳转到正文。
    pub(super) fn on_outline_tree_event(
        &mut self,
        _state: Entity<TreeState>,
        event: &TreeEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            TreeEvent::Expanded(id) => {
                self.workspace.expanded.insert(id.to_string());
            }
            TreeEvent::Collapsed(id) => {
                self.workspace.expanded.remove(id.as_str());
            }
            TreeEvent::Selected(id) | TreeEvent::Confirmed(id) => {
                self.select_outline_node(id.to_string(), cx);
            }
        }
    }

    /// 切换工作区页签（文件/大纲）并同步模型。
    fn set_workspace_tab(&mut self, tab: WorkspaceTab, cx: &mut Context<Self>) {
        if self.workspace.active_tab != tab {
            self.workspace.active_tab = tab;
            self.sync_workspace_models(cx);
            cx.notify();
        }
    }

    /// 选中大纲节点（同时清空两棵框架树的对方选中，保持旧“单选”语义），并跳转到对应标题。
    fn select_outline_node(&mut self, id: String, cx: &mut Context<Self>) {
        let line = find_outline_line(&self.workspace.outline_tree, &id);
        self.workspace.selected = Some(WorkspaceSelection::Outline(id));
        self.file_tree_state.update(cx, |state, cx| {
            state.set_selected_index(None, cx);
        });
        self.sync_outline_tree_selection(cx);
        // 点击即跳转：把源码行换算为偏移，经当前视图快照定位并滚动到视口。
        if let Some(line) = line {
            let source = self.serialized_document_text(cx);
            let mut offset = 0usize;
            for (index, text) in source.split_inclusive('\n').enumerate() {
                if index == line {
                    break;
                }
                offset += text.len();
            }
            offset = offset.min(source.len());
            self.apply_selection_snapshot_in_current_mode(
                &super::SelectionUndoSnapshot {
                    range: offset..offset,
                    reversed: false,
                },
                cx,
            );
            self.request_active_block_scroll_into_view(cx);
        }
        cx.notify();
    }

    /// 打开工作区中的文件（脏文档先弹窗确认）。
    fn open_workspace_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.selected = Some(WorkspaceSelection::File(path.clone()));
        self.request_dropped_markdown_replace(path, window, cx);
    }

    /// 获取侧边栏当前生效宽度（用户拖过用拖拽值，否则跟随视口自动计算）。
    pub(super) fn workspace_effective_panel_width(&self, viewport_width: f32) -> f32 {
        match self.workspace.panel_width {
            Some(width) => clamp_workspace_panel_resize_width(width),
            None => workspace_panel_width_for_viewport(viewport_width),
        }
    }

    /// 开始侧边栏拖拽（记录按下时的指针横坐标与起始宽度）。
    pub(super) fn start_workspace_panel_resize(
        &mut self,
        start_x: f32,
        start_width: f32,
        cx: &mut Context<Self>,
    ) {
        self.workspace.resize_drag = Some(WorkspacePanelResizeDrag {
            start_x,
            start_width,
        });
        cx.notify();
    }

    /// 更新侧边栏拖拽宽度（超出范围时夹取）。
    pub(super) fn update_workspace_panel_resize(&mut self, current_x: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.workspace.resize_drag else {
            return;
        };
        self.workspace.panel_width = Some(clamp_workspace_panel_resize_width(
            drag.start_width + (current_x - drag.start_x),
        ));
        cx.notify();
    }

    /// 结束侧边栏拖拽会话。
    pub(super) fn end_workspace_panel_resize(&mut self, cx: &mut Context<Self>) {
        if self.workspace.resize_drag.take().is_some() {
            cx.notify();
        }
    }

    /// 重置侧边栏宽度为跟随视口自动计算（双击分隔条时调用）。
    pub(super) fn reset_workspace_panel_width(&mut self, cx: &mut Context<Self>) {
        self.workspace.panel_width = None;
        self.workspace.resize_drag = None;
        cx.notify();
    }

    /// 按下分隔条开始拖拽（记录指针横坐标与当前生效宽度）。
    pub(super) fn on_workspace_panel_resize_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport_width = f32::from(window.viewport_size().width);
        let start_width = self.workspace_effective_panel_width(viewport_width);
        self.start_workspace_panel_resize(f32::from(event.position.x), start_width, cx);
        cx.stop_propagation();
    }

    /// 拖拽中更新侧边栏宽度（非拖拽移动直接忽略）。
    pub(super) fn on_workspace_panel_resize_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.resize_drag.is_none() || !event.dragging() {
            return;
        }
        self.update_workspace_panel_resize(f32::from(event.position.x), cx);
    }

    /// 松开鼠标结束侧边栏拖拽。
    pub(super) fn on_workspace_panel_resize_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_workspace_panel_resize(cx);
    }

    /// 双击分隔条恢复自动宽度。
    pub(super) fn on_workspace_panel_resize_reset(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset_workspace_panel_width(cx);
    }

    /// 渲染工作区面板（含页签与文件/大纲正文）。
    pub(super) fn render_workspace_panel(
        &mut self,
        theme: &Theme,
        i18n: &I18nSnapshot,
        panel_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.workspace.is_open {
            return None;
        }

        self.sync_workspace_models(cx);
        // 框架树点击文件的延迟打开（树事件无 `Window`，在此消费）。
        if let Some(path) = self.pending_workspace_open.take() {
            self.open_workspace_file(path, window, cx);
        }
        let editor = cx.entity().downgrade();
        let d = &ThemeDimensions::DEFAULT;

        // 文件与大纲两静态页签由框架值驱动 `Tabs` 承载（零同步代码）。
        let active_tab_id: SharedString = match self.workspace.active_tab {
            WorkspaceTab::Files => "files".into(),
            WorkspaceTab::Outline => "outline".into(),
        };
        let tab_editor = editor.clone();
        let tabs = Tabs::new(vec![
            TabsItem::new("files", i18n.t("workspace_tab_files", &[])),
            TabsItem::new("outline", i18n.t("workspace_tab_outline", &[])),
        ])
        .active(Some(active_tab_id))
        .on_change(move |id: SharedString, _, cx| {
            let tab = match id.as_str() {
                "outline" => WorkspaceTab::Outline,
                _ => WorkspaceTab::Files,
            };
            let _ = tab_editor.update(cx, |editor, cx| {
                editor.set_workspace_tab(tab, cx);
            });
        });

        // 文件与大纲两 tab 均由框架树承载（含虚拟滚动与方向键导航）。
        let body = match self.workspace.active_tab {
            WorkspaceTab::Files => self.render_workspace_files_tree(theme, i18n),
            WorkspaceTab::Outline => self.render_workspace_outline_tree(theme, i18n),
        };

        Some(
            div()
                .id("workspace-panel")
                .h_full()
                .w(px(panel_width))
                .flex()
                .flex_col()
                .flex_shrink_0()
                .bg(theme.popover)
                // 右侧分隔线由拖拽手柄的 1px 线承担，此处不再画边框，避免出现双竖线。
                .child(
                    div()
                        .px(px(12.0))
                        .pt(px(12.0))
                        .pb(px(8.0))
                        .flex()
                        .border_b(px(d.dialog_border_width))
                        .border_color(theme.border)
                        .child(tabs),
                )
                .child(body)
                .into_any_element(),
        )
    }

    /// 渲染工作区文件树（含空状态与报错状态；列表由框架树承载）。
    fn render_workspace_files_tree(&self, theme: &Theme, i18n: &I18nSnapshot) -> AnyElement {
        if self.workspace.root.is_none() {
            let title = i18n.t("workspace_no_file_title", &[]);
            let message = i18n.t("workspace_no_file_message", &[]);
            return self.render_workspace_empty_state(title.as_str(), message.as_str(), theme);
        }

        if let Some(error) = self.workspace.file_error.as_ref() {
            let title = i18n.t("workspace_scan_failed", &[]);
            return self.render_workspace_empty_state(title.as_str(), error, theme);
        }

        if self.workspace.file_tree.is_none() {
            let message = i18n.t("workspace_empty_files", &[]);
            return self.render_workspace_empty_state("", message.as_str(), theme);
        }

        let kinds = self.workspace.file_kind_map.clone();
        div()
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .px(px(8.0))
            .py(px(10.0))
            .child(Tree::new(
                &self.file_tree_state,
                move |ix, entry, selected, _, cx| {
                    render_file_tree_row(ix, entry, selected, &kinds, cx)
                },
            ))
            .into_any_element()
    }

    /// 渲染工作区大纲树（为空时显示空状态；列表由框架树承载）。
    fn render_workspace_outline_tree(&self, theme: &Theme, i18n: &I18nSnapshot) -> AnyElement {
        if self.workspace.outline_tree.is_empty() {
            let message = i18n.t("workspace_empty_outline", &[]);
            return self.render_workspace_empty_state("", message.as_str(), theme);
        }

        div()
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .px(px(8.0))
            .py(px(10.0))
            .child(Tree::new(
                &self.outline_tree_state,
                move |ix, entry, selected, _, cx| render_outline_tree_row(ix, entry, selected, cx),
            ))
            .into_any_element()
    }

    /// 渲染工作区空状态（窄栏内居中换行，不再用框架 `EmptyState` 以免定宽内边距撑出横向溢出）。
    fn render_workspace_empty_state(
        &self,
        title: &str,
        message: &str,
        theme: &Theme,
    ) -> AnyElement {
        // 无标题的调用点把说明文字提升为标题。
        let (title, description) = if title.is_empty() {
            (message.to_string(), None)
        } else {
            (title.to_string(), Some(message.to_string()))
        };
        let t = &ThemeTypography::DEFAULT;
        div()
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .px(px(16.0))
            .py(px(24.0))
            .child(
                div()
                    .w_full()
                    .max_w_full()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .w_full()
                            .max_w_full()
                            .text_align(TextAlign::Center)
                            .text_size(px(14.0))
                            .line_height(px(t.text_size * t.text_line_height))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(title),
                    )
                    .when_some(description, |this, description| {
                        this.child(
                            div()
                                .w_full()
                                .max_w_full()
                                .text_align(TextAlign::Center)
                                .text_size(px(12.0))
                                .line_height(px(12.0 * 1.6))
                                .text_color(theme.muted_foreground)
                                .child(description),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// 由大纲模型构造框架树节点（展开态取自工作区 `expanded` 集合）。
fn outline_tree_item(node: &WorkspaceTreeNode, expanded: &HashSet<String>) -> TreeItem {
    let has_children = !node.children.is_empty();
    TreeItem::new(node.id.clone(), node.label.clone())
        .expanded(has_children && expanded.contains(&node.id))
        .children(
            node.children
                .iter()
                .map(|child| outline_tree_item(child, expanded)),
        )
}

/// 渲染框架大纲树的一行（箭头 + 标签；标题节点无图标，选中走框架 `list` 样式）。
fn render_outline_tree_row(ix: usize, entry: &TreeEntry, selected: bool, cx: &mut App) -> ListItem {
    let theme = Theme::global(cx).clone();
    let t = &ThemeTypography::DEFAULT;
    let arrow = if entry.is_folder() {
        if entry.is_expanded() { "v" } else { ">" }
    } else {
        ""
    };
    ListItem::new(ix)
        .h(px(WORKSPACE_ROW_HEIGHT))
        .pl(px(8.0 + entry.depth() as f32 * WORKSPACE_ROW_INDENT))
        .rounded(px(6.0))
        .selected(selected)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .w(px(14.0))
                        .h(px(18.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.0))
                        .text_color(theme.muted_foreground)
                        .child(arrow),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .truncate()
                        .text_size(px(t.text_size * 0.9))
                        .line_height(px(t.text_size * t.text_line_height))
                        .text_color(if selected {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(entry.item().label.clone()),
                ),
        )
}
/// 空目录按叶子处理（无箭头），与旧自研渲染的 `has_children` 语义一致。
fn file_tree_item(node: &WorkspaceTreeNode, expanded: &HashSet<String>) -> TreeItem {
    let has_children = !node.children.is_empty();
    TreeItem::new(node.id.clone(), node.label.clone())
        .expanded(has_children && expanded.contains(&node.id))
        .children(
            node.children
                .iter()
                .map(|child| file_tree_item(child, expanded)),
        )
}

/// 收集文件节点标识到种类的映射（框架树行渲染时查图标；空目录靠此正确显示文件夹图标）。
fn collect_file_kinds(node: &WorkspaceTreeNode, kinds: &mut BTreeMap<String, WorkspaceTreeKind>) {
    kinds.insert(node.id.clone(), node.kind.clone());
    for child in &node.children {
        collect_file_kinds(child, kinds);
    }
}

/// 渲染框架文件树的一行（箭头 + 图标 + 标签；复刻旧自研行的视觉）。
fn render_file_tree_row(
    ix: usize,
    entry: &TreeEntry,
    selected: bool,
    kinds: &BTreeMap<String, WorkspaceTreeKind>,
    cx: &mut App,
) -> ListItem {
    let theme = Theme::global(cx).clone();
    let t = &ThemeTypography::DEFAULT;
    let arrow = if entry.is_folder() {
        if entry.is_expanded() { "v" } else { ">" }
    } else {
        ""
    };
    let folder_color = Hsla::from(rgba(0xf59e0bff));
    let (icon_path, icon_data, icon_color) = match kinds.get(entry.item().id.as_str()) {
        Some(WorkspaceTreeKind::Directory(_)) | None if entry.is_folder() => (
            IconName::Folder.path(),
            Some(IconName::Folder.bytes()),
            folder_color,
        ),
        Some(WorkspaceTreeKind::MarkdownFile(_)) | None => (
            IconName::File.path(),
            Some(IconName::File.bytes()),
            Hsla::from(rgba(0x2563ebff)),
        ),
        _ => (
            IconName::Folder.path(),
            Some(IconName::Folder.bytes()),
            folder_color,
        ),
    };
    ListItem::new(ix)
        .h(px(WORKSPACE_ROW_HEIGHT))
        .pl(px(8.0 + entry.depth() as f32 * WORKSPACE_ROW_INDENT))
        .rounded(px(6.0))
        .selected(selected)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .w(px(14.0))
                        .h(px(18.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.0))
                        .text_color(theme.muted_foreground)
                        .child(arrow),
                )
                .child(
                    svg()
                        .path(icon_path)
                        .when_some(icon_data, |svg, data| svg.data(data))
                        .size(px(16.0))
                        .flex_shrink_0()
                        .text_color(icon_color)
                        .into_any_element(),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .truncate()
                        .text_size(px(t.text_size * 0.9))
                        .line_height(px(t.text_size * t.text_line_height))
                        .text_color(if selected {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(entry.item().label.clone()),
                ),
        )
}

/// 判断路径是否为 Markdown 文件（`md` 后缀）。
fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("md"))
}

/// 扫描目录生成工作区文件树（仅目录与 Markdown 文件）。
fn scan_workspace_dir(path: &Path) -> Result<WorkspaceTreeNode> {
    let mut children = Vec::new();
    for entry in
        fs::read_dir(path).with_context(|| format!("failed to read '{}'", path.display()))?
    {
        let entry = entry?;
        let entry_path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            children.push(scan_workspace_dir(&entry_path)?);
        } else if file_type.is_file() && is_markdown_file(&entry_path) {
            children.push(WorkspaceTreeNode {
                id: file_node_id(&entry_path),
                label: file_label(&entry_path),
                kind: WorkspaceTreeKind::MarkdownFile(entry_path),
                children: Vec::new(),
            });
        }
    }

    children.sort_by(|left, right| {
        let left_dir = matches!(left.kind, WorkspaceTreeKind::Directory(_));
        let right_dir = matches!(right.kind, WorkspaceTreeKind::Directory(_));
        right_dir
            .cmp(&left_dir)
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
    });

    Ok(WorkspaceTreeNode {
        id: file_node_id(path),
        label: file_label(path),
        kind: WorkspaceTreeKind::Directory(path.to_path_buf()),
        children,
    })
}

/// 取路径的文件名作为节点展示标签。
fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// 按路径生成文件节点的稳定标识。
fn file_node_id(path: &Path) -> String {
    format!("file:{}", path.to_string_lossy())
}

/// 按视口宽度计算工作区面板宽度（含上下限夹取）。
pub(super) fn workspace_panel_width_for_viewport(viewport_width: f32) -> f32 {
    let target = viewport_width * WORKSPACE_TARGET_RATIO;
    target.clamp(WORKSPACE_MIN_WIDTH, WORKSPACE_MAX_WIDTH)
}

/// 把用户拖拽宽度夹取到允许的手动范围内。
pub(super) fn clamp_workspace_panel_resize_width(width: f32) -> f32 {
    width.clamp(
        WORKSPACE_PANEL_RESIZE_MIN_WIDTH,
        WORKSPACE_PANEL_RESIZE_MAX_WIDTH,
    )
}

/// 裁剪大纲中已不存在节点的展开与选中状态。
fn prune_outline_state(workspace: &mut WorkspaceState, outline: &[WorkspaceTreeNode]) {
    let mut current_ids = HashSet::new();
    collect_node_ids(outline, &mut current_ids);
    workspace
        .expanded
        .retain(|id| !is_outline_node_id(id) || current_ids.contains(id));

    if matches!(
        &workspace.selected,
        Some(WorkspaceSelection::Outline(id)) if !current_ids.contains(id)
    ) {
        workspace.selected = None;
    }
}

/// 收集大纲树中全部节点的标识。
fn collect_node_ids(nodes: &[WorkspaceTreeNode], ids: &mut HashSet<String>) {
    for node in nodes {
        ids.insert(node.id.clone());
        collect_node_ids(&node.children, ids);
    }
}

/// 判断标识是否为大纲节点标识。
fn is_outline_node_id(id: &str) -> bool {
    id.starts_with("outline:")
}

/// 在大纲树中按标识查找标题源码行号。
fn find_outline_line(nodes: &[WorkspaceTreeNode], id: &str) -> Option<usize> {
    for node in nodes {
        if node.id == id {
            if let WorkspaceTreeKind::Heading { line, .. } = node.kind {
                return Some(line);
            }
            return None;
        }
        if let Some(line) = find_outline_line(&node.children, id) {
            return Some(line);
        }
    }
    None
}

/// 从 Markdown 源码解析标题大纲树（跳过围栏代码块内标题）。
fn build_outline_tree(markdown: &str) -> Vec<WorkspaceTreeNode> {
    let mut roots = Vec::new();
    let mut stack: Vec<(u8, Vec<usize>)> = Vec::new();
    let mut fence: Option<(char, usize)> = None;

    for (line_index, line) in markdown.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some((marker, len)) = fence {
            if is_closing_fence(trimmed, marker, len) {
                fence = None;
            }
            continue;
        }

        if let Some(next_fence) = opening_fence(trimmed) {
            fence = Some(next_fence);
            continue;
        }

        let Some((level, title)) = BlockKind::parse_heading_line(line) else {
            continue;
        };

        while stack
            .last()
            .is_some_and(|(parent_level, _)| *parent_level >= level)
        {
            stack.pop();
        }

        let node = WorkspaceTreeNode {
            id: format!("outline:{line_index}"),
            label: title,
            kind: WorkspaceTreeKind::Heading {
                line: line_index,
                level,
            },
            children: Vec::new(),
        };

        let siblings = if let Some((_, parent_path)) = stack.last() {
            children_at_path_mut(&mut roots, parent_path)
        } else {
            &mut roots
        };
        siblings.push(node);

        let mut node_path = stack
            .last()
            .map(|(_, path)| path.clone())
            .unwrap_or_default();
        node_path.push(siblings.len() - 1);
        stack.push((level, node_path));
    }

    roots
}

/// 按下标路径取大纲树中某节点的子节点列表。
fn children_at_path_mut<'a>(
    nodes: &'a mut Vec<WorkspaceTreeNode>,
    path: &[usize],
) -> &'a mut Vec<WorkspaceTreeNode> {
    let mut current = nodes;
    for &index in path {
        current = &mut current[index].children;
    }
    current
}

/// 解析行首的围栏起始标记（至少三个反引号或波浪线）。
fn opening_fence(trimmed: &str) -> Option<(char, usize)> {
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let len = trimmed.chars().take_while(|ch| *ch == marker).count();
    (len >= 3).then_some((marker, len))
}

/// 判断该行是否为匹配的围栏结束标记。
fn is_closing_fence(trimmed: &str, marker: char, len: usize) -> bool {
    let count = trimmed.chars().take_while(|ch| *ch == marker).count();
    count >= len && trimmed[count..].trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::{
        WORKSPACE_PANEL_RESIZE_MAX_WIDTH, WORKSPACE_PANEL_RESIZE_MIN_WIDTH, WorkspaceSelection,
        WorkspaceState, WorkspaceTreeKind, WorkspaceTreeNode, build_outline_tree,
        clamp_workspace_panel_resize_width, collect_file_kinds, file_tree_item, find_outline_line,
        outline_tree_item, prune_outline_state, scan_workspace_dir,
        workspace_panel_width_for_viewport,
    };
    use std::collections::{BTreeMap, HashSet};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn workspace_scan_keeps_dirs_and_md_files_only() {
        let root =
            std::env::temp_dir().join(format!("rumark-workspace-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("nested")).expect("create dirs");
        fs::write(root.join("a.md"), "a").expect("write md");
        fs::write(root.join("a.txt"), "ignored").expect("write txt");
        fs::write(root.join("nested").join("b.md"), "b").expect("write nested md");

        let tree = scan_workspace_dir(&root).expect("scan tree");
        let labels = tree
            .children
            .iter()
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["nested", "a.md"]);
        assert!(matches!(
            tree.children[0].kind,
            WorkspaceTreeKind::Directory(_)
        ));
        assert!(matches!(
            tree.children[1].kind,
            WorkspaceTreeKind::MarkdownFile(_)
        ));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn outline_tree_skips_headings_inside_fenced_code() {
        let outline = build_outline_tree(
            "# Root\n\n```md\n# ignored\n```\n\n## Child\n\n### Grandchild\n\n# Next",
        );

        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0].label, "Root");
        assert_eq!(outline[0].children[0].label, "Child");
        assert_eq!(outline[0].children[0].children[0].label, "Grandchild");
        assert_eq!(outline[1].label, "Next");
    }

    #[test]
    fn outline_expansion_state_is_not_auto_populated_and_prunes_stale_ids() {
        let outline = build_outline_tree("# Root\n\n## Child\n\n# Next");
        let mut fresh = WorkspaceState::default();
        prune_outline_state(&mut fresh, &outline);
        assert!(fresh.expanded.is_empty());

        let mut existing = WorkspaceState::default();
        existing.expanded.insert("outline:0".to_string());
        existing.expanded.insert("outline:999".to_string());
        existing
            .expanded
            .insert("workspace-dir:C:/docs".to_string());
        existing.selected = Some(WorkspaceSelection::Outline("outline:999".to_string()));

        prune_outline_state(&mut existing, &outline);

        assert!(existing.expanded.contains("outline:0"));
        assert!(existing.expanded.contains("workspace-dir:C:/docs"));
        assert!(!existing.expanded.contains("outline:999"));
        assert_eq!(existing.selected, None);
    }

    #[test]
    fn outline_tree_items_carry_expanded_flags() {
        let outline = build_outline_tree("# Root\n\n## Child\n\n# Next");
        assert_eq!(outline.len(), 2);

        let mut expanded = HashSet::new();
        expanded.insert("outline:0".to_string());

        let items = outline
            .iter()
            .map(|node| outline_tree_item(node, &expanded))
            .collect::<Vec<_>>();
        assert_eq!(items.len(), 2);
        // 有子节点的标题按文件夹处理（含箭头），展开态来自工作区集合。
        assert!(items[0].is_folder());
        assert!(items[0].is_expanded());
        assert_eq!(items[0].children.len(), 1);
        // 叶子标题无箭头、未展开。
        assert!(!items[1].is_folder());
        assert!(!items[1].is_expanded());
        assert_eq!(items[0].children[0].label, "Child");
    }

    #[test]
    fn find_outline_line_resolves_nested_heading_ids() {
        let outline = build_outline_tree("# Root\n\n## Child\n\n# Next");
        assert_eq!(find_outline_line(&outline, "outline:0"), Some(0));
        assert_eq!(find_outline_line(&outline, "outline:2"), Some(2));
        assert_eq!(find_outline_line(&outline, "outline:4"), Some(4));
        assert_eq!(find_outline_line(&outline, "outline:999"), None);
    }

    #[test]
    fn workspace_panel_width_uses_ratio_with_bounds() {
        assert_eq!(workspace_panel_width_for_viewport(1000.0), 240.0);
        assert_eq!(workspace_panel_width_for_viewport(2000.0), 300.0);
        assert_eq!(workspace_panel_width_for_viewport(4000.0), 360.0);
    }

    #[test]
    fn workspace_panel_resize_width_clamps_to_manual_range() {
        assert_eq!(
            clamp_workspace_panel_resize_width(10.0),
            WORKSPACE_PANEL_RESIZE_MIN_WIDTH
        );
        assert_eq!(
            clamp_workspace_panel_resize_width(10000.0),
            WORKSPACE_PANEL_RESIZE_MAX_WIDTH
        );
        assert_eq!(clamp_workspace_panel_resize_width(300.0), 300.0);
    }

    #[test]
    fn workspace_panel_resize_drag_delta_applies_from_start() {
        // 拖拽增量 = 起始宽度 + 指针位移，再夹取到手动范围。
        let start_width = 280.0;
        let start_x = 280.0;
        let dragged = clamp_workspace_panel_resize_width(start_width + (340.0 - start_x));
        assert_eq!(dragged, 340.0);
        let dragged_left = clamp_workspace_panel_resize_width(start_width + (100.0 - start_x));
        assert_eq!(dragged_left, WORKSPACE_PANEL_RESIZE_MIN_WIDTH);
    }

    #[test]
    fn file_tree_items_carry_expanded_flags_and_kind_map() {
        let root = WorkspaceTreeNode {
            id: "file:/docs".to_string(),
            label: "docs".to_string(),
            kind: WorkspaceTreeKind::Directory(PathBuf::from("/docs")),
            children: vec![
                WorkspaceTreeNode {
                    id: "file:/docs/empty".to_string(),
                    label: "empty".to_string(),
                    kind: WorkspaceTreeKind::Directory(PathBuf::from("/docs/empty")),
                    children: Vec::new(),
                },
                WorkspaceTreeNode {
                    id: "file:/docs/a.md".to_string(),
                    label: "a.md".to_string(),
                    kind: WorkspaceTreeKind::MarkdownFile(PathBuf::from("/docs/a.md")),
                    children: Vec::new(),
                },
            ],
        };
        let mut expanded = HashSet::new();
        expanded.insert("file:/docs".to_string());

        let item = file_tree_item(&root, &expanded);
        assert!(item.is_expanded());
        assert!(item.is_folder());
        assert_eq!(item.children.len(), 2);
        // 空目录无子节点即无箭头（与旧自研渲染一致），未展开。
        assert!(!item.children[0].is_expanded());
        assert!(!item.children[0].is_folder());

        let mut kinds = BTreeMap::new();
        collect_file_kinds(&root, &mut kinds);
        assert_eq!(kinds.len(), 3);
        // 空目录靠种类映射正确识别（框架按子节点判空会误判为文件）。
        assert!(matches!(
            kinds["file:/docs/empty"],
            WorkspaceTreeKind::Directory(_)
        ));
        assert!(matches!(
            kinds["file:/docs/a.md"],
            WorkspaceTreeKind::MarkdownFile(_)
        ));
    }
}
