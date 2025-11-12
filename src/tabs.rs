use gpui::{
    AlignItems, Context, Empty, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Pixels, Point, Render, ScrollHandle, ScrollWheelEvent, SharedString,
    StyleRefinement, Styled, Window, div, px,
};
use gpui_component::button::Button;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{Icon, IconName, StyledExt};
use std::cmp::Ordering;

#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]
#[action(namespace = tabs)]
pub struct CloseTab;

#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]
#[action(namespace = tabs)]
pub struct CreateTab;

#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]
#[action(namespace = tabs)]
pub struct NextTab;

#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]
#[action(namespace = tabs)]
pub struct PrevTab;

pub trait TabData: 'static {
    fn label(&self) -> SharedString;
}

pub struct TabsView<T: 'static> {
    active_tab: usize,
    tabs: Vec<Option<T>>,
    scroll_handle: ScrollHandle,
    latest_scroll_offset: Point<Pixels>,
    on_tab_changed: Box<dyn Fn(&mut Window, &mut Context<Self>) + 'static>,
}
impl<T> TabsView<T> {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            active_tab: 0,
            tabs: vec![None],
            scroll_handle: ScrollHandle::new(),
            latest_scroll_offset: Point::default(),
            on_tab_changed: Box::new(|_window, _cx| {}),
        }
    }
    pub fn on_tab_changed(&mut self, handler: impl Fn(&mut Window, &mut Context<Self>) + 'static) {
        self.on_tab_changed = Box::new(handler);
    }

    pub fn create_tab(&mut self, data: Option<T>, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab = self.tabs.len();
        self.tabs.push(data);
        (self.on_tab_changed)(window, cx);
    }
    pub fn remove_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.len() {
            0 => return,
            1 => {
                self.tabs[0] = None;
                (self.on_tab_changed)(window, cx);
            }
            _ => {
                self.tabs.remove(index);
                if self.active_tab == index {
                    self.set_active_tab(index, window, cx);
                }
            }
        }
    }
    pub fn set_active_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab = index.min(self.tabs.len().saturating_sub(1));
        (self.on_tab_changed)(window, cx);
    }

    pub fn scroll_to_active_tab(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        if self.active_tab == 0 {
            self.scroll_handle.set_offset(Point::default());
        } else {
            self.scroll_handle.scroll_to_item(self.active_tab);
        }
        self.save_latest_scroll();
    }
    fn save_latest_scroll(&mut self) {
        self.latest_scroll_offset = self.scroll_handle.offset();
    }
    pub fn active_tab(&self) -> usize {
        self.active_tab
    }
    pub fn tabs_data(&self) -> &[Option<T>] {
        self.tabs.as_slice()
    }
    pub fn tabs_data_mut(&mut self) -> &mut [Option<T>] {
        self.tabs.as_mut_slice()
    }
    pub fn active_tab_data(&self) -> Option<&T> {
        self.tabs.get(self.active_tab)?.as_ref()
    }
    pub fn active_tab_data_mut(&mut self) -> Option<&mut Option<T>> {
        self.tabs.get_mut(self.active_tab)
    }
}
impl<T> TabsView<T> {
    pub fn on_action_close_tab(
        &mut self,
        _: &CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remove_tab(self.active_tab, window, cx);
        self.scroll_to_active_tab(window, cx);
        cx.notify();
    }
    pub fn on_action_create_tab(
        &mut self,
        _: &CreateTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.create_tab(None, window, cx);
        self.scroll_to_active_tab(window, cx);
        cx.notify();
    }
    pub fn on_action_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.set_active_tab(self.active_tab + 1, window, cx);
        self.scroll_to_active_tab(window, cx);
        cx.notify();
    }
    pub fn on_action_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.set_active_tab(self.active_tab.saturating_sub(1), window, cx);
        self.scroll_to_active_tab(window, cx);
        cx.notify();
    }
}
impl<T: TabData> Render for TabsView<T> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let new_tab_button = if self.tabs.is_empty() {
            Empty.into_any_element()
        } else {
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .child(
                    Button::new("button-new-tab-pdf")
                        .icon(Icon::new(IconName::Plus))
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.create_tab(None, window, cx);
                            view.scroll_to_active_tab(window, cx);
                        })),
                )
                .flex_shrink_0()
                .into_any_element()
        };
        let tab_bar = TabBar::new("dynamic-tabs-with-pdf-files")
            .with_menu(self.tabs.len() > 1)
            .selected_index(self.active_tab)
            .track_scroll(&self.scroll_handle)
            .on_click(cx.listener(|view, index, window, cx| {
                view.set_active_tab(*index, window, cx);
                view.scroll_to_active_tab(window, cx);
            }))
            .children(self.tabs.iter().enumerate().map(|(tab_index, tab_data)| {
                Tab::new(if let Some(tab_data) = tab_data {
                    tab_data.label()
                } else {
                    "New tab".into()
                })
                .on_any_mouse_down(cx.listener(move |view, event, window, cx| {
                    if let MouseDownEvent {
                        button: MouseButton::Middle,
                        ..
                    } = event
                    {
                        view.remove_tab(tab_index, window, cx);
                        view.scroll_to_active_tab(window, cx);
                    }
                }))
            }));

        div()
            .flex()
            .flex_row()
            .w_full()
            .min_h(px(33.))
            .refine_style(&StyleRefinement {
                align_items: Some(AlignItems::Stretch),
                ..Default::default()
            })
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_8()
                    .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, window, cx| {
                        let prev_offset = view.latest_scroll_offset;
                        view.save_latest_scroll();

                        if event.control {
                            view.scroll_handle.set_offset(prev_offset);
                            view.save_latest_scroll();

                            match event.delta.pixel_delta(px(1.0)).y.cmp(&px(0.)) {
                                Ordering::Greater => {
                                    view.set_active_tab(
                                        view.active_tab.saturating_sub(1),
                                        window,
                                        cx,
                                    );
                                    view.scroll_to_active_tab(window, cx);
                                }
                                Ordering::Equal => {}
                                Ordering::Less => {
                                    view.set_active_tab(
                                        view.active_tab.saturating_add(1),
                                        window,
                                        cx,
                                    );
                                    view.scroll_to_active_tab(window, cx);
                                }
                            }
                        }
                    }))
                    .child(tab_bar),
            )
            .child(new_tab_button)
    }
}
