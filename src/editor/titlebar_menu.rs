//! 标题栏下拉菜单的展开状态。
//!
//! 框架的 `Button::dropdown_menu` 让每个按钮各自持有一个独立弹层，依次点击
//! 不同菜单时旧弹层不会收起、按钮也一直保持选中态。这里改为编辑器统一持有
//! 一个打开下标：任意时刻最多展开一个菜单，切换、点击外部与选中菜单项都会
//! 同步收起。

use rgpui::*;

use super::Editor;
use super::render::push_owned_menu_items;

impl Editor {
    /// 构建标题栏中的一个菜单触发器及其展开的菜单层。
    pub(super) fn render_titlebar_menu(
        &self,
        index: usize,
        menu: &OwnedMenu,
        text_size: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_open = self.titlebar_menu_open == Some(index);
        let popup = self.titlebar_menu_popup.clone().filter(|_| is_open);
        let label = menu.name.clone();

        // 触发器在捕获阶段处理按下：菜单层的“点击外部关闭”同样在捕获阶段，
        // 先收起/切换并阻断传播，避免旧菜单的关闭回调把新展开的菜单一起收掉。
        let trigger = div()
            .relative()
            .id(("titlebar-menu", index))
            .capture_any_mouse_down(
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if event.button != MouseButton::Left {
                        return;
                    }
                    cx.stop_propagation();
                    window.prevent_default();
                    this.toggle_titlebar_menu(index, window, cx);
                }),
            )
            .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                if *hovered {
                    this.hover_titlebar_menu(index, window, cx);
                }
            }))
            .child(
                Button::new(("titlebar-menu-button", index))
                    .with_variant(ButtonVariant::Ghost)
                    .with_size(ElementSize::Small)
                    .compact()
                    .selected(is_open)
                    .child(
                        div()
                            .text_size(px(text_size))
                            .line_height(relative(1.0))
                            .child(label),
                    ),
            );

        match popup {
            Some(popup) => trigger
                .child(deferred(
                    anchored()
                        .anchor(Anchor::TopLeft)
                        .snap_to_window_with_margin(px(8.0))
                        .child(div().size_full().occlude().top_1().child(popup)),
                ))
                .into_any_element(),
            None => trigger.into_any_element(),
        }
    }

    /// 切换标题栏菜单：点击已展开的菜单收起，否则展开对应菜单。
    fn toggle_titlebar_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.titlebar_menu_open == Some(index) {
            self.close_titlebar_menu(window, cx);
        } else {
            self.open_titlebar_menu(index, window, cx);
        }
    }

    /// 已展开菜单时，悬停到其它菜单直接切换（与系统菜单栏一致）。
    fn hover_titlebar_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.titlebar_menu_open.is_some() && self.titlebar_menu_open != Some(index) {
            self.open_titlebar_menu(index, window, cx);
        }
    }

    fn open_titlebar_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.titlebar_menu_open == Some(index) {
            return;
        }
        let Some(menu) = cx.get_menus().and_then(|menus| menus.get(index).cloned()) else {
            return;
        };

        // 从文档切到菜单时记下焦点，整组菜单收起后归还。
        if self.titlebar_menu_open.is_none() {
            self.titlebar_menu_restore_focus = window.focused(cx);
        }
        self.release_titlebar_menu();

        let items = menu.items.clone();
        let popup = PopupMenu::build(window, cx, move |popup, window, cx| {
            push_owned_menu_items(popup, &items, window, cx)
        });
        // 菜单的“点击外部关闭”、Esc 与选中项都会销毁实体，订阅其 Dismiss 事件
        // 同步收起标题栏状态。
        self.titlebar_menu_subscription = Some(cx.subscribe_in(
            &popup,
            window,
            |this, _: &Entity<PopupMenu>, _: &DismissEvent, window, cx| {
                this.close_titlebar_menu(window, cx);
            },
        ));
        popup.focus_handle(cx).focus(window, cx);
        self.titlebar_menu_popup = Some(popup);
        self.titlebar_menu_open = Some(index);
        cx.notify();
    }

    fn close_titlebar_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.titlebar_menu_open.is_none() {
            return;
        }
        self.release_titlebar_menu();
        self.titlebar_menu_open = None;
        if let Some(handle) = self.titlebar_menu_restore_focus.take() {
            handle.focus(window, cx);
        }
        cx.notify();
    }

    /// 丢弃菜单实体与关闭订阅（焦点归还由调用方决定）。
    fn release_titlebar_menu(&mut self) {
        self.titlebar_menu_subscription = None;
        self.titlebar_menu_popup = None;
    }
}
