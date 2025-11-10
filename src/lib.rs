use gpui::{
    Entity, FocusHandle, ImageCacheError, InteractiveElement, KeyBinding, Pixels, RenderImage,
    Resource, ScrollHandle, SharedString, StyledImage, size,
};
use std::path::PathBuf;
use std::rc::Rc;

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
use gpui_component::scroll::{Scrollbar, ScrollbarAxis, ScrollbarState};
use gpui_component::{Root, StyledExt, VirtualListScrollHandle, v_flex, v_virtual_list};
use hayro::{InterpreterSettings, Pdf, RenderSettings, render};
use std::sync::{Arc, Mutex};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub struct PdfTabData {
    path: PathBuf,
    pdf_data: Arc<Vec<u8>>,
    scroll: Arc<Mutex<VirtualListScrollHandle>>,
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

pub struct NoGpuiImageCache;
impl gpui::ImageCache for NoGpuiImageCache {
    fn load(
        &mut self,
        _resource: &Resource,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        None
    }
}

struct ImageCacheMutableState {
    used: Vec<Option<Arc<RenderImage>>>,
    unused: Vec<Option<Arc<RenderImage>>>,
    in_progress: Vec<bool>,
    gc_counter: u32,
}
impl ImageCacheMutableState {
    fn mark_as_used(&mut self, index: usize, data: Arc<RenderImage>) {
        if self.used.len() <= index {
            self.used.resize_with(index + 1, || None);
        }
        self.used[index] = Some(data);
        self.set_in_progress(index, false);
    }
    fn set_in_progress(&mut self, index: usize, value: bool) {
        if self.in_progress.len() <= index {
            self.in_progress.resize(index + 1, false);
        }
        self.in_progress[index] = value;
    }
}
struct ImageCache {
    state: Mutex<ImageCacheMutableState>,
    render_settings: RenderSettings,
    pdf: Option<Arc<Pdf>>,
}
impl ImageCache {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ImageCacheMutableState {
                used: Vec::with_capacity(256),
                unused: Vec::with_capacity(256),
                in_progress: Vec::with_capacity(256),
                gc_counter: 0,
            }),
            render_settings: RenderSettings {
                x_scale: 1.,
                y_scale: 1.,
                ..Default::default()
            },
            pdf: None,
        }
    }
    pub fn gc(&self) {
        let mut guard = self.state.lock().unwrap();
        let this = &mut *guard;

        let total_size = this.used.len() + this.unused.len();
        this.gc_counter += 1;
        if this.gc_counter >= 3 || total_size > 6 {
            this.unused.clear();
            std::mem::swap(&mut this.used, &mut this.unused);
            this.gc_counter = 0;
        }
    }
    pub fn get_image(
        self: &Arc<Self>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<PdfPages>,
    ) -> Option<Arc<RenderImage>> {
        let mut guard = self.state.lock().unwrap();
        if let Some(image) = guard.used.get(index).cloned().flatten() {
            eprintln!("Cache hit for page {index} (in used)");
            return Some(image);
        } else if let Some(image) = guard.unused.get_mut(index).and_then(|slot| slot.take()) {
            eprintln!("Cache hit for page {index} (in unused)");
            guard.mark_as_used(index, image.clone());
            Some(image)
        } else {
            eprintln!("Cache miss for page {index}");
            if matches!(guard.in_progress.get(index), Some(true)) {
                return None; // already rendering in background.
            }
            let Some(pdf) = self.pdf.clone() else {
                return None;
            };
            guard.set_in_progress(index, true);
            drop(guard);

            let window = window.to_async(cx);
            let this = self.clone();
            let renderer = cx.svg_renderer();
            let background = cx.background_spawn(async move {
                let interpreter_settings = InterpreterSettings::default();

                let pixmap = render(
                    &pdf.pages()[index],
                    &interpreter_settings,
                    &this.render_settings,
                );
                let image = Image::from_bytes(ImageFormat::Png, pixmap.take_png());

                // Code from: <gpui::ImageDecoder as Asset>::load
                let result = image.to_image_data(renderer);
                {
                    let mut guard = this.state.lock().unwrap();
                    if let Ok(render_image) = result {
                        guard.mark_as_used(index, render_image);
                    } else {
                        // TODO: log error
                    }
                    guard.set_in_progress(index, false);
                }
            });
            let parent = cx.weak_entity();
            window
                .spawn(async move |window| {
                    background.await;

                    if let Some(parent) = parent.upgrade() {
                        _ = window.update_entity(&parent, |_view, cx: &mut Context<PdfPages>| {
                            cx.notify();
                        });
                    }
                })
                .detach();
            None
        }
    }
}

pub struct PdfPages {
    scroll_handle: VirtualListScrollHandle,
    scroll_state: ScrollbarState,
    save_scroll: Arc<Mutex<VirtualListScrollHandle>>,
    item_sizes: Rc<Vec<Size<Pixels>>>,
    images: Arc<ImageCache>,
    /// Used to bypass GPUI's inbuilt image cache.
    disabled_cache: Entity<NoGpuiImageCache>,
}
impl PdfPages {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            scroll_handle: VirtualListScrollHandle::from(ScrollHandle::default()),
            scroll_state: Default::default(),
            save_scroll: Arc::new(Mutex::new(VirtualListScrollHandle::from(
                ScrollHandle::new(),
            ))),
            item_sizes: Rc::new(vec![]),
            images: Arc::new(ImageCache::new()),
            disabled_cache: cx.new(|_cx| NoGpuiImageCache),
        }
    }
}
impl Render for PdfPages {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.images.gc();
        div()
            .relative()
            .size_full()
            .child(
                v_virtual_list(
                    cx.entity().clone(),
                    "pdf-viewer-pages-list",
                    self.item_sizes.clone(),
                    move |view, visible_range, window, cx| {
                        visible_range
                            .map(|row_ix| {
                                let page_image = view.images.get_image(row_ix, window, cx);
                                if let Some(page_image) = page_image {
                                    img(page_image)
                                        .object_fit(ObjectFit::Cover)
                                        .max_w(window.viewport_size().width)
                                        .image_cache(&view.disabled_cache)
                                        //.w(px(page.media_box().width() as f32))
                                        //.h(px(page.media_box().height() as f32))
                                        .into_any_element()
                                } else {
                                    //  Loading or errored
                                    div().into_any_element()
                                }
                            })
                            .collect()
                    },
                )
                .track_scroll(&self.scroll_handle),
            )
            .child(
                // Add scrollbars
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(
                        Scrollbar::both(&self.scroll_state, &self.scroll_handle)
                            .axis(ScrollbarAxis::Vertical),
                    ),
            )
            .into_any_element()
    }
}

pub struct PdfReader {
    focus_handle: FocusHandle,
    tabs: Entity<TabsView<PdfTabData>>,
    pages: Entity<PdfPages>,
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
                cx.new(|cx| {
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
            pages: cx.new(|cx| PdfPages::new(window, cx)),
        }
    }
    fn active_pdf_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pages.update(cx, |pages, cx| {
            pages.item_sizes = Rc::new(vec![]); // forget page sizes
            pages.images = Arc::new(ImageCache::new()); // clear cache

            *pages.save_scroll.lock().unwrap() = pages.scroll_handle.clone(); // save scroll
            pages.scroll_handle = VirtualListScrollHandle::from(ScrollHandle::default()); // reset scroll

            let Some(tab_data) = self.tabs.read(cx).active_tab_data() else {
                return;
            };
            pages.scroll_handle = tab_data.scroll.lock().unwrap().clone(); // restore scroll
            let Ok(pdf) = Pdf::new(tab_data.pdf_data.clone()) else {
                return;
            };
            let pdf = Arc::new(pdf);
            if pdf.pages().is_empty() {
                return;
            }

            // Scale to fit window width:
            let base_width = pdf
                .pages()
                .iter()
                .map(|page| page.media_box().width() as f32)
                .max_by(f32::total_cmp)
                .expect("more than one page");
            let viewport_width = f32::from(window.viewport_size().width);
            let scale_x = viewport_width / base_width;

            let render_settings = RenderSettings {
                x_scale: scale_x,
                y_scale: scale_x,
                ..Default::default()
            };

            let mut cache = ImageCache::new();
            cache.render_settings = render_settings;
            cache.pdf = Some(pdf.clone());
            pages.images = Arc::new(cache);

            pages.item_sizes = Rc::new(
                pdf.pages()
                    .iter()
                    .map(|page| {
                        let width = page.media_box().width() as f32 * scale_x;
                        let height = page.media_box().height() as f32 * scale_x;
                        size(px(width), px(height))
                    })
                    .collect::<Vec<_>>(),
            );
        });
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
            .child(
                if let Some(tab_data) = self.tabs.read(cx).active_tab_data() {
                    match Pdf::new(tab_data.pdf_data.clone()) {
                        Ok(_) => self.pages.clone().into_any_element(),
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
    fn update(&mut self, window: &mut Window, cx: &mut Context<Self>, msg: PdfCommand) {
        match msg {
            PdfCommand::LoadedData(path, pdf_data) => {
                if let Some(tab_data) = self.tabs.as_mut(cx).active_tab_data_mut() {
                    *tab_data = Some(PdfTabData {
                        path,
                        pdf_data: Arc::new(pdf_data),
                        scroll: Arc::new(Mutex::new(VirtualListScrollHandle::from(
                            ScrollHandle::new(),
                        ))),
                    });
                }
                self.active_pdf_changed(window, cx);
            }
            PdfCommand::ChangedTab => {
                self.active_pdf_changed(window, cx);
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
