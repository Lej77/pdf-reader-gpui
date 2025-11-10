use gpui::{Axis, Entity, FocusHandle, InteractiveElement, KeyBinding, SharedString, StyledImage};
use std::path::PathBuf;
pub mod assets;
pub mod elm;
pub mod prompt;
pub mod tabs;

use crate::assets::Assets;
use crate::elm::{MsgSender, Update};
use crate::prompt::{NoDisplayHandle, prompt_load_pdf_file};
use crate::tabs::TabsView;
use gpui::{
    App, AppContext, Application, Context, Image, ImageFormat, IntoElement, ObjectFit,
    ParentElement, Render, Size, Styled, Window, WindowOptions, div, img, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Root, StyledExt, v_flex};
use hayro::{InterpreterSettings, Pdf, RenderSettings, render};
use std::sync::Arc;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub struct PdfTabData {
    path: PathBuf,
    pdf_data: Arc<Vec<u8>>,
}
impl tabs::TabData for PdfTabData {
    fn label(&self) -> SharedString {
        if let Some(name) = self.path.file_name() {
            name.to_string_lossy().into_owned().into()
        } else {
            "<invalid path>".into()
        }
    }
}

pub struct PdfReader {
    focus_handle: FocusHandle,
    tabs: Entity<TabsView<PdfTabData>>,
    images: Vec<Arc<Image>>,
}
impl PdfReader {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
            KeyBinding::new("ctrl-w", tabs::CloseTab, Some(CONTEXT)),
            KeyBinding::new("ctrl-t", tabs::CreateTab, Some(CONTEXT)),
            KeyBinding::new("ctrl-tab", tabs::NextTab, Some(CONTEXT)),
            KeyBinding::new("ctrl-shift-tab", tabs::PrevTab, Some(CONTEXT)),
        ]);
        // dbg!(&cx.key_bindings().borrow().bindings().collect::<Vec<_>>());

        Self {
            focus_handle: cx.focus_handle(),
            tabs: {
                let sender = MsgSender::from_cx(window, cx);
                cx.new(move |cx| {
                    let mut tabs = TabsView::new(window, cx);
                    tabs.on_tab_changed(move |_window, _cx| {
                        sender
                            .spawn(async move |_window, mut sender| {
                                sender.send(PdfCommand::ChangedTab);
                            })
                            .detach();
                    });
                    tabs
                })
            },
            images: Vec::new(),
        }
    }
    fn update_images(&mut self, cx: &mut Context<Self>) {
        let Some(tab_data) = self.tabs.read(cx).active_tab_data() else {
            self.images = Vec::new();
            return;
        };
        let pdf = Pdf::new(tab_data.pdf_data.clone()).unwrap();

        let interpreter_settings = InterpreterSettings::default();

        let render_settings = RenderSettings {
            x_scale: 1.,
            y_scale: 1.,
            ..Default::default()
        };

        self.images = pdf
            .pages()
            .iter()
            .map(|page| render(page, &interpreter_settings, &render_settings).take_png())
            .map(|png_data| Arc::new(Image::from_bytes(ImageFormat::Png, png_data)))
            .collect();
    }
}
const CONTEXT: &str = "pdf-reader";
impl Render for PdfReader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .id("pdf-reader")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(window.listener_for(&self.tabs, TabsView::on_action_close_tab))
            .on_action(window.listener_for(&self.tabs, TabsView::on_action_create_tab))
            .on_action(window.listener_for(&self.tabs, TabsView::on_action_next_tab))
            .on_action(window.listener_for(&self.tabs, TabsView::on_action_prev_tab))
            // Tab bar:
            .child(self.tabs.clone())
            // Content:
            .child(if !self.images.is_empty() {
                v_flex()
                    .size_full()
                    .max_w_full()
                    .items_center()
                    .justify_center()
                    .children(
                        self.images
                            .iter()
                            .cloned()
                            .map(|image| img(image).object_fit(ObjectFit::Contain)),
                    )
                    .scrollable(Axis::Vertical)
                    .into_any_element()
            } else {
                div()
                    .v_flex()
                    .gap_2()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(
                        Button::new("ok")
                            .primary()
                            .label("Select a PDF file")
                            .on_click({
                                let sender = MsgSender::from_cx(window, cx);
                                move |_, window, _cx| {
                                    let prompt =
                                        prompt_load_pdf_file(Some(&NoDisplayHandle(window)));
                                    sender
                                        .spawn(async move |_window, mut sender| {
                                            if let Some(data) = prompt.await {
                                                sender.send(PdfCommand::LoadedData(
                                                    data.path().to_owned(),
                                                    data.read().await,
                                                ))
                                            }
                                        })
                                        .detach();
                                }
                            }),
                    )
                    .into_any_element()
            })
    }
}

pub enum PdfCommand {
    LoadedData(PathBuf, Vec<u8>),
    ChangedTab,
}
impl Update<PdfCommand> for PdfReader {
    fn update(&mut self, _window: &mut Window, cx: &mut Context<Self>, msg: PdfCommand) {
        match msg {
            PdfCommand::LoadedData(path, pdf_data) => {
                if let Some(tab_data) = self.tabs.as_mut(cx).active_tab_data_mut() {
                    *tab_data = Some(PdfTabData {
                        path,
                        pdf_data: Arc::new(pdf_data),
                    });
                }
                self.update_images(cx);
            }
            PdfCommand::ChangedTab => {
                self.update_images(cx);
            }
        }
    }
}

pub fn start_gui() {
    // let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
    // let _rt_guard = rt.enter();

    Application::new().with_assets(Assets).run(|cx: &mut App| {
        cx.new(|cx: &mut Context<'_, ()>| {
            // This must be called before using any GPUI Component features.
            gpui_component::init(cx);

            cx.open_window(
                WindowOptions {
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("GPUI PDF Reader".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(Size::new(px(400.), px(400.))),
                    ..Default::default()
                },
                |window: &mut Window, cx: &mut App| {
                    // Uncomment next line to test a specific theme instead of using the system theme:
                    // gpui_component::Theme::change(gpui_component::ThemeMode::Light, Some(window), cx);

                    let main_ui = cx.new(|cx: &mut Context<'_, _>| PdfReader::new(window, cx));
                    cx.new(|cx| Root::new(main_ui.into(), window, cx))
                },
            )
            .expect("Failed to build and open window");
        });
    });
}
