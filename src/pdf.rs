use gpui::RenderImage;
use hayro::{RenderSettings, render};
use hayro_interpret::font::Glyph;
use hayro_interpret::{
    ClipPath, Context, Device, FillRule, GlyphDrawMode, Image, InterpreterSettings, Paint,
    PathDrawMode, SoftMask, interpret,
};
use hayro_syntax::content::ops::TypedInstruction;
use hayro_syntax::object::Rect;
use hayro_syntax::page::Page;
use image::{Frame, RgbaImage};
use kurbo::{Affine, BezPath, Shape};
use std::cell::Cell;
use std::sync::Arc;

/// Rasterize a PDF page and convert the result from a [`hayro::Pixmap`] to a [`gpui::RenderImage`].
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub fn rasterize_pdf_page(
    page: &Page,
    render_settings: &RenderSettings,
    interpreter_settings: &InterpreterSettings,
) -> Arc<RenderImage> {
    let pixmap = render(page, interpreter_settings, render_settings);
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

    let image_data = RgbaImage::from_raw(width, height, data).expect("incorrect image dimensions");
    Arc::new(RenderImage::new([Frame::new(image_data)]))
}

pub struct PdfFeature {}

pub fn extract_features(
    page: &Page,
    render_settings: &RenderSettings,
    interpreter_settings: &InterpreterSettings,
) {
    // Adapted from `hayro::render` but the device was changed to `FeatureExtractor` and some rendering code was removed.

    let (x_scale, y_scale) = (render_settings.x_scale, render_settings.y_scale);
    let (width, height) = page.render_dimensions();
    let (scaled_width, scaled_height) = ((width * x_scale) as f64, (height * y_scale) as f64);
    let initial_transform =
        Affine::scale_non_uniform(x_scale as f64, y_scale as f64) * page.initial_transform(true);

    let (pix_width, pix_height) = (
        render_settings.width.unwrap_or(scaled_width.floor() as u16),
        render_settings
            .height
            .unwrap_or(scaled_height.floor() as u16),
    );
    let mut state = Context::new(
        initial_transform,
        Rect::new(0.0, 0.0, pix_width as f64, pix_height as f64),
        page.xref(),
        interpreter_settings.clone(),
    );

    let current_op = Cell::new(None);
    let mut device = FeatureExtractor {
        _current_op: &current_op,
    };

    device.push_clip_path(&ClipPath {
        path: initial_transform * page.intersected_crop_box().to_path(0.1),
        fill: FillRule::NonZero,
    });

    let resources = page.resources();
    let mut ops = page.typed_operations();
    interpret(
        std::iter::from_fn(|| {
            let op = ops.next();
            current_op.set(op.clone());
            op
        }),
        resources,
        &mut state,
        &mut device,
    );

    device.pop_clip_path();
}

/// A [`hayro_interpret::Device`] that is used as an "output" for PDF rendering.
///
/// See
/// [`hayro-interpret/examples/extract_images.rs`](https://github.com/LaurenzV/hayro/blob/e08071f8602c3e28000b4d114be41d08ee82b86b/hayro-interpret/examples/extract_images.rs)
/// for a simpler example.
struct FeatureExtractor<'out, 'pdf> {
    _current_op: &'out Cell<Option<TypedInstruction<'pdf>>>,
}
impl<'a, 'out, 'pdf> Device<'a> for FeatureExtractor<'out, 'pdf> {
    fn set_soft_mask(&mut self, _mask: Option<SoftMask<'a>>) {}

    fn draw_path(
        &mut self,
        _path: &BezPath,
        _transform: Affine,
        _paint: &Paint<'a>,
        _draw_mode: &PathDrawMode,
    ) {
    }

    fn push_clip_path(&mut self, _clip_path: &ClipPath) {}

    fn push_transparency_group(&mut self, _opacity: f32, _mask: Option<SoftMask<'a>>) {}

    fn draw_glyph(
        &mut self,
        _glyph: &Glyph<'a>,
        _transform: Affine,
        _glyph_transform: Affine,
        _paint: &Paint<'a>,
        _draw_mode: &GlyphDrawMode,
    ) {
        // TODO: extract text location and symbol (check self.current_op)
    }

    fn draw_image(&mut self, _image: Image<'a, '_>, _transform: Affine) {
        // TODO: extract image (see example linked in struct's doc-comment above)
    }

    fn pop_clip_path(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}
