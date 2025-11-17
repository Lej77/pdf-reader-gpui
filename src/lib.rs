pub mod assets;
pub mod elm;
pub mod prompt;
pub mod tabs;

use crate::assets::Assets;
use crate::elm::{MsgSender, Update};
use crate::prompt::{NoDisplayHandle, prompt_load_pdf_file};
use crate::tabs::TabsView;
use gpui::{
    App, AppContext, Application, AsyncWindowContext, Context, Entity, FocusHandle,
    ImageCacheError, ImageSource, InteractiveElement, IntoElement, KeyBinding, ObjectFit,
    ParentElement, Pixels, Render, RenderImage, Resource, ScrollHandle, SharedString, Size, Styled,
    StyledImage, Task, WeakEntity, Window, WindowOptions, div, img, px, size,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::{Scrollbar, ScrollbarAxis, ScrollbarState};
use gpui_component::{Root, StyledExt, VirtualListScrollHandle, v_flex, v_virtual_list};
use hayro::{InterpreterSettings, Pdf, RenderSettings, render};
use hayro_syntax::page::Page;
use image::{Frame, RgbaImage};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Poll, Waker};
use std::time::Duration;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// This type is the same as [`RenderSettings`] except it implements more traits.
#[derive(Clone, Copy, PartialEq)]
pub struct RenderSettings2 {
    /// How much the contents should be scaled into the x direction.
    pub x_scale: f32,
    /// How much the contents should be scaled into the y direction.
    pub y_scale: f32,
    /// The width of the viewport. If this is set to `None`, the width will be chosen
    /// automatically based on the scale factor and the dimensions of the PDF.
    pub width: Option<u16>,
    /// The height of the viewport. If this is set to `None`, the height will be chosen
    /// automatically based on the scale factor and the dimensions of the PDF.
    pub height: Option<u16>,
}
impl Default for RenderSettings2 {
    fn default() -> Self {
        RenderSettings::default().into()
    }
}
impl From<RenderSettings> for RenderSettings2 {
    fn from(value: RenderSettings) -> RenderSettings2 {
        <RenderSettings2 as From<&'_ RenderSettings>>::from(&value)
    }
}
impl From<&'_ RenderSettings> for RenderSettings2 {
    fn from(value: &RenderSettings) -> Self {
        Self {
            x_scale: value.x_scale,
            y_scale: value.y_scale,
            width: value.width,
            height: value.height,
        }
    }
}
impl From<RenderSettings2> for RenderSettings {
    fn from(value: RenderSettings2) -> Self {
        Self {
            x_scale: value.x_scale,
            y_scale: value.y_scale,
            width: value.width,
            height: value.height,
        }
    }
}

/// `true` if both ranges overlap or share an edge.
pub fn range_is_contiguous(a: Range<usize>, b: Range<usize>) -> bool {
    range_union(a.clone(), b.clone()).len() <= a.len() + b.len()
}
/// Get the smallest range that contains both `a` and `b`.
pub fn range_union(a: Range<usize>, b: Range<usize>) -> Range<usize> {
    match (a.len(), b.len()) {
        (0, 0) => 0..0,
        (_, 0) => a,
        (0, _) => b,
        _ => a.start.min(b.start)..a.end.max(b.end),
    }
}
/// Get the largest range that is covered by both `a` and `b`.
pub fn range_intersection(a: Range<usize>, b: Range<usize>) -> Range<usize> {
    let start = a.start.max(b.start);
    start..a.end.min(b.end).max(start)
}

pub struct PdfTabData {
    path: Arc<PathBuf>,
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

    fn full_path(&self) -> Arc<PathBuf> {
        self.path.clone()
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

struct ArcIdentity<T>(Arc<T>);
impl<T> Hash for ArcIdentity<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(Arc::as_ptr(&self.0).addr());
    }
}
impl<T> PartialEq for ArcIdentity<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl<T> Eq for ArcIdentity<T> {}

struct PdfPageCacheMutableState {
    /// Currently cached images of PDF pages. Index of an image is the PDF page's index.
    images: Vec<Option<Arc<RenderImage>>>,
    /// Settings (zoom) that will be used when rendering images.
    render_settings: RenderSettings2,
    /// The parsed PDF file that the background thread will rasterize.
    pdf: Option<Arc<Pdf>>,
    /// Notify/wake the foreground future so that it can request a re-render of the UI with newly
    /// cached images.
    wake_future: Option<Waker>,
    /// All pages in the range should eventually be cached.
    requested_pages: Range<usize>,
    /// The background thread has acknowledged that pages in this range will be rendered.
    acknowledged_pages: Range<usize>,
    /// If `true` then background worker thread and foreground task will exit.
    should_quit: bool,
}
impl PdfPageCacheMutableState {
    pub fn set_new_pdf(&mut self, pdf: Option<Arc<Pdf>>, render_settings: RenderSettings2) {
        self.images.clear(); // <- always clear to ensure all items are None.
        if let Some(pdf) = pdf.as_ref() {
            self.images.resize_with(pdf.pages().len(), || None);
        }
        self.requested_pages = 0..0;
        self.acknowledged_pages = 0..0;
        self.render_settings = render_settings;
        self.pdf = pdf;
    }
}
struct PdfPageCacheSharedState {
    state: Mutex<PdfPageCacheMutableState>,
    wake_worker: Condvar,
}
struct PdfPageCache {
    /// Data shared between background worker thread, frontend async task and [`PdfPages`] view.
    shared: Arc<PdfPageCacheSharedState>,
    /// Dropping this will stop the foreground task.
    _ui_updater: Task<()>,
    /// PDF pages rendered this frame.
    pages_this_frame: Range<usize>,
    /// PDF pages rendered previous frame (keep this in cache).
    pages_last_frame: Range<usize>,
    rendered_images: HashSet<ArcIdentity<RenderImage>>,
}
impl Drop for PdfPageCache {
    fn drop(&mut self) {
        let mut guard = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.should_quit = true;
        let waker = guard.wake_future.take();
        drop(guard);
        if let Some(waker) = waker {
            waker.wake();
        }
        self.shared.wake_worker.notify_all();
    }
}
impl PdfPageCache {
    pub fn new(window: &mut Window, cx: &mut Context<PdfPages>) -> Self {
        let shared = Arc::new(PdfPageCacheSharedState {
            state: Mutex::new(PdfPageCacheMutableState {
                images: Vec::with_capacity(256),
                render_settings: RenderSettings2 {
                    x_scale: 1.,
                    y_scale: 1.,
                    ..Default::default()
                },
                pdf: None,
                wake_future: None,
                requested_pages: 0..0,
                acknowledged_pages: 0..0,
                should_quit: false,
            }),
            wake_worker: Condvar::new(),
        });
        let this = Self {
            shared: shared.clone(),
            _ui_updater: cx.spawn_in(window, {
                let shared = shared.clone();
                async move |parent, window| Self::foreground_work(shared, parent, window).await
            }),
            pages_this_frame: 0..0,
            pages_last_frame: 0..0,
            rendered_images: Default::default(),
        };
        std::thread::Builder::new()
            .name("PDF Rasterizer".to_owned())
            .spawn(move || Self::background_work(shared))
            .expect("failed to spawn background worker thread");

        this
    }

    /// Notify [`PdfPages`] view when new PDF pages have been rendered by the worker thread running
    /// [`Self::background_work`].
    async fn foreground_work(
        shared: Arc<PdfPageCacheSharedState>,
        parent: WeakEntity<PdfPages>,
        window: &mut AsyncWindowContext,
    ) {
        struct WaitForChange<'a> {
            shared: &'a PdfPageCacheSharedState,
            rendered_images: &'a mut Vec<bool>,
        }
        impl<'a> Future for WaitForChange<'a> {
            type Output = bool;

            fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
                let this = self.get_mut();
                let mut guard = this.shared.state.lock().unwrap();

                this.rendered_images
                    .resize_with(guard.images.len(), || false);

                let mut changed_state = false;
                for (cache, is_cached) in guard.images.iter().zip(this.rendered_images.iter_mut()) {
                    if cache.is_some() != *is_cached {
                        *is_cached = cache.is_some();
                        changed_state = true;
                    }
                }

                if guard.should_quit || changed_state {
                    Poll::Ready(guard.should_quit)
                } else {
                    guard.wake_future = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }

        // array of bools (true if an image is known to be cached)
        let mut rendered_images = Vec::with_capacity(256);
        loop {
            let should_quit = WaitForChange {
                shared: &*shared,
                rendered_images: &mut rendered_images,
            }
            .await;
            if should_quit {
                return;
            }

            let result = parent.update(window, |_parent, cx| {
                log::debug!("Notify view about new pdf pages");
                cx.notify();
            });
            if result.is_err() {
                // parent view dropped
                return;
            }
        }
    }

    /// Executed by dedicated worker thread that will rasterize PDF pages as requested by the
    /// [`Self::get_images`] method.
    fn background_work(shared: Arc<PdfPageCacheSharedState>) {
        let mut guard = shared.state.lock().unwrap();
        loop {
            // Check if we need to rasterize another page:
            let mut index_to_render = None;
            {
                let mut wanted_pages = guard.requested_pages.clone();

                // more aggressively cache earlier pages since the virtual list doesn't:
                wanted_pages.start = wanted_pages.start.saturating_sub(1);

                // Chose the page closest to the center of the requested range:
                let mut chose_index_distance = usize::MAX;
                let center = wanted_pages.end.saturating_sub(1 + wanted_pages.len() / 2);

                // We special case caching of the first page since the virtual list always requests it
                let cache_first_image = guard.requested_pages.start <= 1;

                for (index, image) in guard.images.iter_mut().enumerate() {
                    let should_cache = if index == 0 {
                        cache_first_image
                    } else {
                        wanted_pages.contains(&index)
                    };
                    if !should_cache {
                        *image = None;
                    } else if image.is_none() {
                        let distance = index.abs_diff(center);
                        if distance < chose_index_distance {
                            index_to_render = Some(index);
                            chose_index_distance = distance;
                        }
                    }
                }
            }

            log::debug!(
                "Rasterize page {index_to_render:?}, acknowledged_pages={:?}, requested_pages={:?}",
                guard.acknowledged_pages.clone(),
                guard.requested_pages.clone()
            );
            guard.acknowledged_pages = guard.requested_pages.clone();

            if let Some(index) = index_to_render {
                // Copy render inputs:
                let Some(pdf) = guard.pdf.clone() else {
                    continue;
                };
                let render_settings = guard.render_settings;

                // render while not holding the lock:
                drop(guard);
                let new_image = Self::rasterize_pdf_page(
                    &pdf.pages()[index],
                    &RenderSettings::from(render_settings),
                );

                // re-acquire lock and save new image to shared state:
                guard = shared.state.lock().unwrap();
                if guard.render_settings == render_settings
                    && guard
                        .pdf
                        .as_ref()
                        .is_some_and(|new_pdf| Arc::ptr_eq(&pdf, &new_pdf))
                {
                    if let Some(image) = guard.images.get_mut(index) {
                        *image = Some(new_image);
                        log::debug!(
                            "Rasterize image done, index={index}, acknowledged_pages={:?}, wake_frontend={}",
                            guard.acknowledged_pages,
                            guard.wake_future.is_some()
                        );
                        if let Some(waker) = guard.wake_future.take() {
                            waker.wake();
                        }
                    }
                }
            } else {
                // Nothing more to render (ensure range is correct and then wait):
                guard = shared
                    .wake_worker
                    .wait_while(guard, |state| {
                        !state.should_quit && state.acknowledged_pages == state.requested_pages
                    })
                    .unwrap();
            }

            if guard.should_quit {
                return;
            }
        }
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn rasterize_pdf_page(page: &Page, render_settings: &RenderSettings) -> Arc<RenderImage> {
        let interpreter_settings = InterpreterSettings::default();

        let pixmap = render(page, &interpreter_settings, &render_settings);
        // The code below that converts to RenderImage was inspired by code from:
        // <gpui::ImageDecoder as Asset>::load
        //
        // The more "normal" way to convert it would be using:
        // Image::from_bytes(ImageFormat::Png, pixmap.take_png()).to_image_data(renderer)

        let width = u32::from(pixmap.width());
        let height = u32::from(pixmap.height());
        let mut data = pixmap.take_u8();

        // Convert from RGBA to BGRA.
        for pixel in data.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }

        let image_data =
            RgbaImage::from_raw(width, height, data).expect("incorrect image dimensions");
        Arc::new(RenderImage::new([Frame::new(image_data)]))
    }

    pub fn clear(&self) {
        self.set_new_pdf(None, RenderSettings2::default());
    }
    pub fn set_new_pdf(&self, pdf: Option<Arc<Pdf>>, render_settings: RenderSettings2) {
        let mut guard = self.shared.state.lock().unwrap();
        guard.set_new_pdf(pdf, render_settings);
    }

    pub fn frame_start(&mut self, window: &mut Window, _cx: &mut Context<PdfPages>) {
        log::trace!(r"PdfPage render started \\//");
        self.pages_last_frame = self.pages_this_frame.clone();
        self.pages_this_frame = 0..0;

        self.rendered_images.retain(|image| {
            if Arc::strong_count(&image.0) == 1 {
                _ = window.drop_image(image.0.clone());
                false
            } else {
                true
            }
        });
    }

    pub fn get_images(
        &mut self,
        visible_range: Range<usize>,
        _window: &mut Window,
        _cx: &mut Context<PdfPages>,
    ) -> Vec<Option<Arc<RenderImage>>> {
        let mut guard = self.shared.state.lock().unwrap();
        let images = if let Some(images) = guard.images.get(visible_range.clone()) {
            images.to_vec()
        } else {
            vec![None; visible_range.len()]
        };

        self.rendered_images.extend(
            images
                .iter()
                .filter_map(|slot| slot.as_ref())
                .map(|image| ArcIdentity(image.clone())),
        );

        if visible_range.start == 0 && visible_range.end == 1 {
            // Don't track request for only the first page since the virtual list always requests it.
            return images;
        }

        if self.pages_this_frame.len() == 0
            || !range_is_contiguous(self.pages_this_frame.clone(), visible_range.clone())
        {
            // If non-contiguous: ignore previous range this frame, it was likely rendered
            // incorrectly before layout calculations determined that they weren't visible
            self.pages_this_frame = visible_range.clone();
        } else {
            // Keep previously rendered pages cached.
            self.pages_this_frame =
                range_union(self.pages_this_frame.clone(), visible_range.clone());
        }

        // Tell the background thread about the new image range:
        guard.requested_pages =
            range_union(self.pages_this_frame.clone(), self.pages_last_frame.clone());

        if guard.requested_pages != guard.acknowledged_pages {
            if let Some(waker) = guard.wake_future.take() {
                waker.wake();
            }
            drop(guard);
            self.shared.wake_worker.notify_all();
        }

        log::trace!(
            "Rendering pdf pages at visible_range={visible_range:?}, current_images={:?}",
            images
                .iter()
                .map(|image| image.is_some())
                .collect::<Vec<_>>()
        );

        images
    }
}

pub struct PdfPages {
    /// Current scroll position.
    scroll_handle: VirtualListScrollHandle,
    /// State of the scrollbar element.
    scroll_state: ScrollbarState,
    /// Pointer to scroll info inside tab data. Use to save current scroll position before loading a new PDF.
    save_scroll: Arc<Mutex<VirtualListScrollHandle>>,
    /// Sizes of each page in the PDF file.
    item_sizes: Rc<Vec<Size<Pixels>>>,
    /// Cached rasterized PDF pages.
    pdf_page_cache: PdfPageCache,
    /// Used to bypass GPUI's inbuilt image cache.
    disabled_cache: Entity<NoGpuiImageCache>,
}
impl PdfPages {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            scroll_handle: VirtualListScrollHandle::from(ScrollHandle::default()),
            scroll_state: Default::default(),
            save_scroll: Arc::new(Mutex::new(VirtualListScrollHandle::from(
                ScrollHandle::new(),
            ))),
            item_sizes: Rc::new(vec![]),
            pdf_page_cache: PdfPageCache::new(window, cx),
            disabled_cache: cx.new(|_cx| NoGpuiImageCache),
        }
    }
}
impl Render for PdfPages {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        fn weak_image(image: &Arc<RenderImage>) -> ImageSource {
            let image = Arc::downgrade(image);
            ImageSource::Custom(Arc::new(move |_window, _cx| Some(Ok(image.upgrade()?))))
        }

        self.pdf_page_cache.frame_start(window, cx);
        let element = div()
            .relative()
            .size_full()
            .child(
                v_virtual_list(
                    cx.entity().clone(),
                    "pdf-viewer-pages-list",
                    self.item_sizes.clone(),
                    move |view, visible_range, window, cx| {
                        visible_range
                            .clone()
                            .zip(view.pdf_page_cache.get_images(visible_range, window, cx))
                            .map(|(_row_ix, page_image)| {
                                if let Some(page_image) = page_image {
                                    img(weak_image(&page_image))
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
            .into_any_element();

        element
    }
}

pub struct PdfReader {
    focus_handle: FocusHandle,
    tabs: Entity<TabsView<PdfTabData>>,
    pages: Entity<PdfPages>,
    assumed_viewport_size: Size<Pixels>,
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
            assumed_viewport_size: Default::default(),
        }
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn active_pdf_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pages.update(cx, |pages, cx| {
            pages.item_sizes = Rc::new(vec![]); // forget page sizes
            pages.pdf_page_cache.clear(); // clear cache

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
                // no pages
                return;
            }
            let viewport_size = window.viewport_size();

            // Scale to fit window width:
            let max_width = pdf
                .pages()
                .iter()
                .map(|page| page.render_dimensions().0)
                .max_by(f32::total_cmp)
                .expect("there should be at least one page");
            let viewport_width = f32::from(viewport_size.width);
            let scale = viewport_width / max_width;

            let render_settings = RenderSettings {
                x_scale: scale,
                y_scale: scale,
                ..Default::default()
            };

            // Update image rendering:
            pages
                .pdf_page_cache
                .set_new_pdf(Some(pdf.clone()), render_settings.into());

            // Update layout/sizes:
            self.assumed_viewport_size = viewport_size;
            pages.item_sizes = Rc::new(
                pdf.pages()
                    .iter()
                    .map(|page| {
                        // Code adapted from `hayro::render`:
                        let (width, height) = page.render_dimensions();
                        let (width, height) = (width * scale, height * scale);

                        size(px(width.floor()), px(height.floor()))
                    })
                    .collect::<Vec<_>>(),
            );
        });
    }
    fn check_window_size(&mut self, window: &Window, cx: &mut Context<Self>) {
        let mut latest_window_size = window.viewport_size();
        if self.assumed_viewport_size == Size::default() {
            return; // resize already being monitored.
        }
        if latest_window_size == self.assumed_viewport_size {
            return; // no resize
        }
        self.assumed_viewport_size = Size::default();
        let this = cx.weak_entity();

        window
            .spawn(cx, async move |window: &mut AsyncWindowContext| {
                loop {
                    window
                        .background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                    let keep_checking = window.update(|window, cx| {
                        let new_size = window.viewport_size();
                        if new_size != latest_window_size {
                            // still resizing
                            latest_window_size = new_size;
                            true
                        } else {
                            _ = this.update(cx, |this, cx| {
                                this.active_pdf_changed(window, cx);
                            });
                            false
                        }
                    });
                    if !matches!(keep_checking, Ok(true)) {
                        break;
                    }
                }
            })
            .detach();
    }
}
const CONTEXT: &str = "pdf-reader";
impl Render for PdfReader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.check_window_size(window, cx);
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
                        path: Arc::new(path),
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

#[cfg_attr(feature = "hotpath", hotpath::main)]
pub fn start_gui() {
    #[cfg(debug_assertions)]
    {
        if std::env::var_os("RUST_LOG").is_none() {
            unsafe { std::env::set_var("RUST_LOG", "trace") };
        }
    }
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

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
