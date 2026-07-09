//! Lottie vector animation: parse once, rasterize frames on demand.
//!
//! Deliberately *not* the sticker pipeline ([`crate::animation`]): stickers
//! pre-decode every frame because embedded assets are tiny; a Lottie file is
//! arbitrarily long and pre-rendering blows up (10 s × 30 fps × 512 px ≈
//! 300 MB of RGBA). Instead this module gives the renderer a parsed,
//! reusable [`LottieAnimation`] that samples time on a capped-fps grid and
//! rasterizes single frames on request; caching rendered frames (LRU) is
//! the caller's job. Design: `docs/lottie-design.md`.
//!
//! Backend: [`velato`] parses Lottie JSON and replays draw calls into a
//! [`velato::RenderSink`]; the sink here is a [`vello_cpu::RenderContext`]
//! (pure Rust, SIMD, no GPU dependency — the compositor owns the GPU).
//!
//! velato's importer `todo!()`s on some unsupported features instead of
//! returning `Err`, so parse *and* render run under `catch_unwind`: a
//! hostile or over-fancy file is a failed load / blank frame, never a
//! crashed editor.

use std::panic::{AssertUnwindSafe, catch_unwind};

use cutlass_core::{DecodeError, RgbaImage};
use kurbo::{Affine, Stroke};
use peniko::{BlendMode, Brush};
use vello_cpu::{Pixmap, RenderContext, RenderMode};

/// Longest side a rasterized Lottie frame may have. Compositions larger
/// than this render downscaled (vector data, so no quality cliff); the
/// compositor samples the bitmap like any other layer.
pub const LOTTIE_MAX_DIMENSION: u32 = 512;

/// Sampling fps cap: requested times quantize to this grid so scrubbing and
/// playback revisit the same frames (cache hits) instead of rasterizing a
/// fresh frame per composite. Decorative stickers look fine at 20 fps.
pub const LOTTIE_MAX_SAMPLE_FPS: f64 = 20.0;

/// Curve flattening tolerance for `kurbo::Shape::to_path`, in output
/// pixels. 0.1 px is invisible at sticker sizes.
const PATH_TOLERANCE: f64 = 0.1;

/// A parsed Lottie composition plus the rasterizer state to render any
/// frame of it. Frames sample on a capped-fps grid (see
/// [`LOTTIE_MAX_SAMPLE_FPS`]) and loop over the intrinsic duration.
pub struct LottieAnimation {
    composition: velato::Composition,
    renderer: velato::Renderer,
    /// Rasterization size: intrinsic size capped to
    /// [`LOTTIE_MAX_DIMENSION`] on the long side, aspect preserved.
    render_width: u32,
    render_height: u32,
    /// Distinct sampled frames per loop (≥ 1).
    frame_count: usize,
    /// Sampled frames per second (`min(intrinsic fps, cap)`).
    sample_fps: f64,
    /// Intrinsic duration of one loop, in seconds.
    duration: f64,
}

impl LottieAnimation {
    /// Parse a Lottie JSON document from a string.
    pub fn parse(source: &str) -> Result<Self, DecodeError> {
        // catch_unwind: velato's importer panics (todo!) on some
        // unsupported features rather than erroring.
        let composition = catch_unwind(|| source.parse::<velato::Composition>())
            .map_err(|_| {
                DecodeError::Decode("lottie: file uses an unsupported feature".to_string())
            })?
            .map_err(|e| DecodeError::Open(format!("lottie: {e}")))?;

        let (iw, ih) = (composition.width as u32, composition.height as u32);
        if iw == 0 || ih == 0 {
            return Err(DecodeError::Decode(
                "lottie: composition has zero dimensions".to_string(),
            ));
        }
        let (render_width, render_height) = capped_size(iw, ih);

        let frame_rate = composition.frame_rate;
        let frame_span = composition.frames.end - composition.frames.start;
        if !frame_rate.is_finite() || frame_rate <= 0.0 || !frame_span.is_finite() {
            return Err(DecodeError::Decode(
                "lottie: composition has a degenerate timebase".to_string(),
            ));
        }
        let duration = (frame_span / frame_rate).max(0.0);
        let sample_fps = frame_rate.min(LOTTIE_MAX_SAMPLE_FPS);
        let frame_count = ((duration * sample_fps).ceil() as usize).max(1);

        Ok(Self {
            composition,
            renderer: velato::Renderer::new(),
            render_width,
            render_height,
            frame_count,
            sample_fps,
            duration,
        })
    }

    /// Parse a Lottie file from disk.
    pub fn load(path: &std::path::Path) -> Result<Self, DecodeError> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| DecodeError::Io(format!("lottie {}: {e}", path.display())))?;
        Self::parse(&source)
    }

    /// The composition's intrinsic size in its own pixels (what placement
    /// math should use; rendered frames may be smaller, see
    /// [`LOTTIE_MAX_DIMENSION`]).
    pub fn intrinsic_size(&self) -> (u32, u32) {
        (
            self.composition.width as u32,
            self.composition.height as u32,
        )
    }

    /// Duration of one loop in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.duration
    }

    /// Number of distinct sampled frames per loop.
    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    /// The sampled-frame index on screen at `local_time` seconds, looping
    /// over the intrinsic duration. Stable across calls — this is the
    /// caller's cache key.
    pub fn frame_index_at(&self, local_time: f64) -> usize {
        if self.frame_count <= 1 || self.duration <= 0.0 {
            return 0;
        }
        let t = local_time.max(0.0) % self.duration;
        ((t * self.sample_fps) as usize).min(self.frame_count - 1)
    }

    /// Rasterize sampled frame `index` (see [`frame_index_at`]) to a
    /// straight-alpha RGBA bitmap of the capped render size.
    ///
    /// [`frame_index_at`]: Self::frame_index_at
    pub fn render_frame(&mut self, index: usize) -> Result<RgbaImage, DecodeError> {
        let index = index.min(self.frame_count.saturating_sub(1));
        let frame = self.composition.frames.start
            + index as f64 / self.sample_fps * self.composition.frame_rate;
        // Never exceed the last frame (float accumulation at loop ends).
        let frame = frame.min(self.composition.frames.end);

        let (iw, _) = self.intrinsic_size();
        let scale = f64::from(self.render_width) / f64::from(iw);

        let mut sink = CpuSink {
            ctx: RenderContext::new(self.render_width as u16, self.render_height as u16),
        };
        let renderer = &mut self.renderer;
        let composition = &self.composition;
        let render = catch_unwind(AssertUnwindSafe(|| {
            renderer.append(composition, frame, Affine::scale(scale), 1.0, &mut sink);
        }));
        if render.is_err() {
            // The renderer's internal state is suspect after a panic;
            // rebuild it so later frames start clean.
            self.renderer = velato::Renderer::new();
            return Err(DecodeError::Decode(
                "lottie: frame uses an unsupported feature".to_string(),
            ));
        }

        let mut pixmap = Pixmap::new(self.render_width as u16, self.render_height as u16);
        let mut resources = vello_cpu::Resources::new();
        sink.ctx.flush();
        sink.ctx.render_to_buffer(
            &mut resources,
            pixmap.data_as_u8_slice_mut(),
            self.render_width as u16,
            self.render_height as u16,
            RenderMode::OptimizeSpeed,
        );

        let pixels: Vec<u8> = pixmap
            .take_unpremultiplied()
            .into_iter()
            .flat_map(|px| [px.r, px.g, px.b, px.a])
            .collect();
        Ok(RgbaImage::new(
            self.render_width,
            self.render_height,
            pixels,
        ))
    }
}

/// Intrinsic size capped to [`LOTTIE_MAX_DIMENSION`] on the long side,
/// aspect preserved, never zero.
fn capped_size(width: u32, height: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= LOTTIE_MAX_DIMENSION {
        return (width, height);
    }
    let scale = f64::from(LOTTIE_MAX_DIMENSION) / f64::from(long);
    (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    )
}

/// velato → vello_cpu adapter: replays velato's draw calls into a CPU
/// render context.
struct CpuSink {
    ctx: RenderContext,
}

impl velato::RenderSink for CpuSink {
    fn push_layer(
        &mut self,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        shape: &impl kurbo::Shape,
    ) {
        self.ctx.set_transform(transform);
        let path = shape.to_path(PATH_TOLERANCE);
        self.ctx
            .push_layer(Some(&path), Some(blend.into()), Some(alpha), None, None);
    }

    fn push_clip_layer(&mut self, transform: Affine, shape: &impl kurbo::Shape) {
        self.ctx.set_transform(transform);
        self.ctx.push_clip_layer(&shape.to_path(PATH_TOLERANCE));
    }

    fn pop_layer(&mut self) {
        self.ctx.pop_layer();
    }

    fn draw(
        &mut self,
        stroke: Option<&Stroke>,
        transform: Affine,
        brush: &Brush,
        shape: &impl kurbo::Shape,
    ) {
        self.ctx.set_transform(transform);
        let paint: vello_cpu::PaintType = match brush {
            Brush::Solid(color) => (*color).into(),
            Brush::Gradient(gradient) => gradient.clone().into(),
            // velato never emits image brushes (image embedding is
            // unsupported upstream); skip rather than mis-paint.
            Brush::Image(_) => return,
        };
        self.ctx.set_paint(paint);
        let path = shape.to_path(PATH_TOLERANCE);
        match stroke {
            Some(stroke) => {
                self.ctx.set_stroke(stroke.clone());
                self.ctx.stroke_path(&path);
            }
            None => self.ctx.fill_path(&path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid Lottie: 100×100, 30 fps, 60 frames, one shape layer
    /// with a centered 80×80 red rectangle.
    const RED_RECT: &str = r#"{
      "v": "5.7.0", "fr": 30, "ip": 0, "op": 60, "w": 100, "h": 100,
      "layers": [{
        "ty": 4, "ip": 0, "op": 60, "st": 0,
        "ks": {"o": {"a": 0, "k": 100}, "p": {"a": 0, "k": [50, 50]},
               "a": {"a": 0, "k": [0, 0, 0]}, "s": {"a": 0, "k": [100, 100, 100]},
               "r": {"a": 0, "k": 0}},
        "shapes": [
          {"ty": "rc", "p": {"a": 0, "k": [0, 0]}, "s": {"a": 0, "k": [80, 80]}, "r": {"a": 0, "k": 0}},
          {"ty": "fl", "c": {"a": 0, "k": [1, 0, 0, 1]}, "o": {"a": 0, "k": 100}}
        ]
      }]
    }"#;

    fn pixel(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * image.width + x) * 4) as usize;
        image.pixels[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn parses_and_renders_the_red_rect() {
        let mut anim = LottieAnimation::parse(RED_RECT).unwrap();
        assert_eq!(anim.intrinsic_size(), (100, 100));
        assert!((anim.duration_seconds() - 2.0).abs() < 1e-9);
        // 2 s at the 20 fps cap (native 30 fps is above the cap).
        assert_eq!(anim.frame_count(), 40);

        let frame = anim.render_frame(0).unwrap();
        assert_eq!((frame.width, frame.height), (100, 100));
        let center = pixel(&frame, 50, 50);
        assert!(center[0] > 200 && center[3] > 200, "center: {center:?}");
        assert_eq!(pixel(&frame, 2, 2)[3], 0, "corner should be transparent");
    }

    #[test]
    fn frame_indices_quantize_and_loop() {
        let anim = LottieAnimation::parse(RED_RECT).unwrap();
        assert_eq!(anim.frame_index_at(0.0), 0);
        assert_eq!(anim.frame_index_at(0.049), 0);
        assert_eq!(anim.frame_index_at(0.051), 1);
        assert_eq!(anim.frame_index_at(1.999), 39);
        // Loops: 2.1 s into a 2 s animation = 0.1 s.
        assert_eq!(anim.frame_index_at(2.1), 2);
        assert_eq!(anim.frame_index_at(-1.0), 0);
    }

    #[test]
    fn oversized_compositions_render_capped() {
        let big = RED_RECT.replace(r#""w": 100, "h": 100"#, r#""w": 2000, "h": 1000"#);
        let mut anim = LottieAnimation::parse(&big).unwrap();
        assert_eq!(anim.intrinsic_size(), (2000, 1000));
        let frame = anim.render_frame(0).unwrap();
        assert_eq!((frame.width, frame.height), (512, 256));
    }

    #[test]
    fn garbage_and_non_lottie_json_error() {
        assert!(LottieAnimation::parse("not json").is_err());
        assert!(LottieAnimation::parse("{}").is_err());
    }

    #[test]
    fn unsupported_features_error_instead_of_panicking() {
        // Split rotation ("rx"/"ry") hits a todo!() in velato's importer.
        let split = RED_RECT.replace(
            r#""r": {"a": 0, "k": 0}"#,
            r#""rx": {"a": 0, "k": 0}, "ry": {"a": 0, "k": 0}"#,
        );
        assert!(LottieAnimation::parse(&split).is_err());
    }
}
