//! cutlass-shapes: the vector shape vocabulary shared by the CPU and GPU.
//!
//! Shapes come in two families with different realization strategies:
//!
//! - **Parametric shapes** ([`SdfShape`]): rounded rects, ellipses,
//!   polygons/stars, lines, arrows, hearts. These are *described*, not
//!   rasterized — the compositor evaluates their signed-distance function in a
//!   fragment shader, so keyframed geometry (a growing corner radius, a
//!   pulsing star) costs a uniform update per frame instead of a re-raster +
//!   re-upload, and edges stay crisp at any scale. This crate holds the
//!   canonical CPU evaluation ([`sdf::eval`]) of the *same math* the WGSL
//!   implements, plus a reference rasterizer ([`sdf::raster`]); golden tests
//!   in the compositor pin the two together.
//! - **Pen-tool paths** ([`BezierPath`]): arbitrary cubic-bezier outlines.
//!   These have no cheap SDF, so they rasterize on the CPU via `tiny-skia`
//!   ([`PathRaster`]) into straight-alpha [`cutlass_core::RgbaImage`] bitmaps
//!   that ride the compositor's existing RGBA layer path, memoized like
//!   `cutlass-text` rasters. Paths animate through the clip *transform* (a
//!   GPU quad), so the raster is only rebuilt when the path or style is
//!   edited.
//!
//! The crate is pure Rust and GPU-free — like `cutlass-text`, it depends only
//! on `cutlass-core` (plus `tiny-skia` for path filling), so geometry stays
//! unit-testable on any CI box. It also carries the editor-facing geometry
//! queries (bounds, point-in-shape hit tests) a pen tool and selection UI
//! need, operating on the same types the renderer consumes.

pub mod path;
pub mod sdf;

pub use path::{PathRaster, path_bounds, path_hit_test};
pub use sdf::{MAX_STAR_POINTS, SDF_AA, eval, raster};

/// A parametric shape, fully resolved to output pixels — every animatable
/// parameter already sampled at the frame's tick. This is the vocabulary the
/// compositor's SDF pipeline consumes; the resolver maps the serialized
/// `cutlass-models` shape (with its `Param` curves) down to one of these per
/// frame.
///
/// The shape is centered on the origin of a `2*half` pixel box; the SDF is
/// evaluated in that pixel space (+y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SdfShape {
    /// Half extents of the shape's bounding box in pixels.
    pub half: [f32; 2],
    pub params: SdfParams,
}

/// The size-free parameters of an [`SdfShape`] — everything but the box.
/// Placement carries the box (one source of truth for size), so this is what
/// travels in a composite layer next to a placement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SdfParams {
    /// Axis-aligned rectangle with rounded corners (`radius` in pixels,
    /// clamped to half the smaller extent; `0` is a sharp rect).
    RoundedRect { radius: f32 },
    /// Axis-aligned ellipse inscribed in the box.
    Ellipse,
    /// A star with `points` spikes: outer vertices on the box's inscribed
    /// (aspect-stretched) circle, inner vertices at `inner` of it (`0..=1`).
    /// Corners rounded by `round` pixels.
    ///
    /// Regular polygons are the degenerate star whose inner vertices sit on
    /// the edge midpoints — build them with [`SdfParams::polygon`] so that
    /// relation lives in exactly one place.
    Star { points: u32, inner: f32, round: f32 },
    /// A horizontal capsule spanning the box: length `2*half[0]`, thickness
    /// `2*half[1]`, round caps.
    Line,
    /// A right-pointing arrow (triangular head + shaft) filling the box.
    Arrow,
    /// A heart, upright (lobes at the top), fit to the box.
    Heart,
}

impl SdfParams {
    /// A regular `sides`-gon, corners rounded by `round` px.
    ///
    /// Encoded as the [`SdfParams::Star`] whose inner vertices lie exactly on
    /// the edge midpoints (`inner = cos(pi/n)`), which makes the star's spike
    /// edges collinear — i.e. a straight polygon edge. One evaluator serves
    /// both shapes, on the CPU and in WGSL.
    pub fn polygon(sides: u32, round: f32) -> Self {
        let n = sides.max(3);
        SdfParams::Star {
            points: n,
            inner: (std::f32::consts::PI / n as f32).cos(),
            round,
        }
    }

    /// The shape this parameterizes, boxed to `half` extents.
    pub const fn with_half(self, half: [f32; 2]) -> SdfShape {
        SdfShape { half, params: self }
    }
}

impl SdfShape {
    /// True when the point (pixels, origin at shape center) is inside the
    /// filled shape, or within the stroke ring if `stroke_width > 0`.
    pub fn hit_test(&self, p: [f32; 2], stroke_width: f32) -> bool {
        eval(self, p) <= stroke_width * 0.5
    }
}

/// An outline drawn on a shape edge, centered on it (half in, half out) —
/// resolved to pixels, colors straight-alpha RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    pub rgba: [u8; 4],
    pub width: f32,
}

/// Fill + stroke for a shape. `fill: None` draws only the stroke (open pen
/// paths).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeStyle {
    pub fill: Option<[u8; 4]>,
    pub stroke: Option<Stroke>,
}

/// One anchor of a [`BezierPath`], with absolute cubic control handles.
/// `handle_in` shapes the segment *arriving* at the anchor, `handle_out` the
/// segment *leaving* it; a handle equal to its anchor makes that side a
/// straight corner (the pen-tool "click without drag" case).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathPoint {
    pub anchor: [f32; 2],
    pub handle_in: [f32; 2],
    pub handle_out: [f32; 2],
}

impl PathPoint {
    /// A corner point: both handles collapsed onto the anchor.
    pub fn corner(anchor: [f32; 2]) -> Self {
        Self {
            anchor,
            handle_in: anchor,
            handle_out: anchor,
        }
    }
}

/// A pen-tool outline: cubic bezier segments through `points`, optionally
/// closed. Coordinates are shape-local pixels; the raster is centered on the
/// path's tight bounds center, which is what the renderer places at the
/// layer's center.
#[derive(Debug, Clone, PartialEq)]
pub struct BezierPath {
    pub points: Vec<PathPoint>,
    pub closed: bool,
}

impl BezierPath {
    /// True when the path has enough points to draw anything (a single
    /// anchor has no segment).
    pub fn is_drawable(&self) -> bool {
        self.points.len() >= 2
    }
}
