pub mod assets;
pub mod elm;
pub mod prompt;
pub mod tabs;

use crate::assets::Assets;
use crate::elm::{MsgSender, Update};
use crate::prompt::{NoDisplayHandle, prompt_load_pdf_file};
use crate::tabs::TabsView;
use gpui::{
    App, AppContext, Application, Context, Image, ImageFormat, ImageSource, IntoElement, ObjectFit,
    ParentElement, Render, RenderImage, Size, Styled, Window, WindowOptions, div, img, px,
};
use gpui::{Axis, Entity, FocusHandle, InteractiveElement, KeyBinding, SharedString, StyledImage};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Root, StyledExt, v_flex};
use hayro::{InterpreterSettings, Pdf, RenderSettings, render};
use hayro_syntax::page::Page;
use std::cell::RefCell;
use std::path::PathBuf;
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

struct ImageCacheMutableState {
    used: Vec<Option<Arc<RenderImage>>>,
    unused: Vec<Option<Arc<RenderImage>>>,
}
struct ImageCache {
    state: RefCell<ImageCacheMutableState>,
    render_settings: RenderSettings,
}
impl ImageCache {
    pub fn new() -> Self {
        Self {
            state: RefCell::new(ImageCacheMutableState {
                used: Vec::with_capacity(256),
                unused: Vec::with_capacity(256),
            }),
            render_settings: RenderSettings {
                x_scale: 1.,
                y_scale: 1.,
                ..Default::default()
            },
        }
    }
    pub fn clear(&self) {
        let mut guard = self.state.borrow_mut();
        guard.used.clear();
        guard.unused.clear();
    }
    pub fn gc(&self) {
        let mut guard = self.state.borrow_mut();
        eprintln!("GC {}", guard.used.len());
        let this = &mut *guard;
        this.unused.clear();
        std::mem::swap(&mut this.used, &mut this.unused);
    }
    pub fn get_image(&self, index: usize, page: &Page, cx: &mut App) -> Option<Arc<RenderImage>> {
        let mut guard = self.state.borrow_mut();
        if let Some(image) = guard.used.get(index).cloned().flatten() {
            eprintln!("Cache hit for page {index} (in used)");
            return Some(image);
        }
        let render_image =
            if let Some(image) = guard.unused.get_mut(index).and_then(|slot| slot.take()) {
                eprintln!("Cache hit for page {index} (in unused)");
                image
            } else {
                eprintln!("Cache miss for page {index}");
                let interpreter_settings = InterpreterSettings::default();

                let pixmap = render(page, &interpreter_settings, &self.render_settings);
                let image = Image::from_bytes(ImageFormat::Png, pixmap.take_png());

                // Code from: <gpui::ImageDecoder as Asset>::load
                let renderer = cx.svg_renderer();
                // TODO: log error
                image.to_image_data(renderer).ok()?
            };

        // Cache it:
        if guard.used.len() <= index {
            guard.used.resize_with(index + 1, || None);
        }
        guard.used[index] = Some(render_image.clone());
        Some(render_image)
    }
}

pub struct PdfReader {
    focus_handle: FocusHandle,
    tabs: Entity<TabsView<PdfTabData>>,
    images: Arc<ImageCache>,
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
            images: Arc::new(ImageCache::new()),
        }
    }
}
const CONTEXT: &str = "pdf-reader";
impl Render for PdfReader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.images.gc();

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
            .child(
                if let Some(PdfTabData { pdf_data, .. }) = self.tabs.read(cx).active_tab_data() {
                    match Pdf::new(pdf_data.clone()) {
                        Ok(pdf) => v_flex()
                            .size_full()
                            .max_w_full()
                            .items_center()
                            .justify_center()
                            .children({
                                let pdf = Arc::new(pdf);
                                pdf.pages()
                                    .iter()
                                    .enumerate()
                                    .map(|(index, page)| {
                                        let images = self.images.clone();
                                        let pdf = pdf.clone();
                                        let source = ImageSource::from(
                                            move |_window: &mut Window, cx: &mut App| {
                                                Some(Ok(images.get_image(
                                                    index,
                                                    &pdf.pages()[index],
                                                    cx,
                                                )?))
                                            },
                                        );
                                        (source, page)
                                    })
                                    .map(|(image, page)| {
                                        div()
                                            .child(img(image).object_fit(ObjectFit::Contain))
                                            .w(px(dbg!(page.media_box()).width() as f32))
                                            .h(px(page.media_box().height() as f32))
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .scrollable(Axis::Vertical)
                            .into_any_element(),
                        Err(e) => v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .child(format!("Failed to load PDF:\n{e:?}"))
                            .into_any_element(),
                    }
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
                },
            )
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
                self.images.clear();
            }
            PdfCommand::ChangedTab => {
                self.images.clear();
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
