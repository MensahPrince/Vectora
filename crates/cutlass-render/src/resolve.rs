//! The pure timeline → [`Scene`] resolver.
//!
//! Given a [`Project`] and a timeline instant, [`resolve`] walks the visual
//! track stack bottom-to-top, finds the clip active on each lane, and turns it
//! into a placed [`SceneLayer`]: canvas geometry, transform, crop/mirror, and a
//! classified pixel source. It decodes nothing and touches no GPU, so the
//! geometry is deterministic and unit-testable on any platform.
//!
//! ## Coverage (v1)
//!
//! - **Media**: video sources and still images (both aspect-fit into the
//!   canvas, then scaled; stills place one cached frame for the clip's whole
//!   extent).
//! - **Generators**: text, solid fills, and every shape kind — parametric
//!   shapes resolve to sampled SDF layers (animated geometry/colors are
//!   sampled per instant here, evaluated on the GPU), pen paths to CPU-raster
//!   layers.
//! - **Deferred**: stickers, effects, filters, and adjustment layers are
//!   skipped (they produce no layer) rather than rendered wrong.

use cutlass_core::{RationalTime, resample};
use cutlass_models::{
    ClipId, ClipSource, ClipTransform, Generator, MediaKind, Param, Project, Shape, ShapePath,
    ShapeStroke, TextAlignH, TextStyle as ModelTextStyle,
};
use cutlass_shapes::{BezierPath, PathPoint, SDF_AA, SdfParams, Stroke};
use cutlass_text::{FontFamily, TextAlign, TextStyle};

use crate::scene::{LayerSource, Scene, SceneLayer, SizeSpec};

/// Vertical reference height that a generator's reference-pixel sizes (text
/// `size`, shape `width`/`height`) are authored against. Matches the model's
/// `canvas_height / 1080` convention.
const REFERENCE_HEIGHT: f32 = 1080.0;

/// Fallback canvas size when `Auto` aspect can't find any video media.
const DEFAULT_CANVAS: (u32, u32) = (1920, 1080);

/// Live-preview substitutions resolved in place of committed clip state.
///
/// A drag/scale/rotate gesture overrides one clip's transform; a live
/// inspector edit (font-size slider, shape color) overrides one clip's
/// generator. Both are session-side only: the project, history, and export
/// never see them — release commits one real edit and clears the override.
#[derive(Debug, Default, Clone, Copy)]
pub struct ResolveOverrides<'a> {
    pub transform: Option<(ClipId, ClipTransform)>,
    pub generator: Option<(ClipId, &'a Generator)>,
}

/// Resolve `project` at timeline instant `t` into a [`Scene`].
///
/// `t` is interpreted at the timeline frame rate (it is resampled to it first),
/// so callers may pass a tick at any rate.
pub fn resolve(project: &Project, t: RationalTime) -> Result<Scene, cutlass_models::ModelError> {
    resolve_with(project, t, ResolveOverrides::default())
}

/// [`resolve`] with live-preview [`ResolveOverrides`] applied.
pub fn resolve_with(
    project: &Project,
    t: RationalTime,
    overrides: ResolveOverrides<'_>,
) -> Result<Scene, cutlass_models::ModelError> {
    let timeline = project.timeline();
    let rate = timeline.frame_rate;
    let t = resample(t, rate);

    let (width, height) = canvas_size(project);
    let bg = timeline.canvas().background;
    let mut scene = Scene::empty(width, height, [bg[0], bg[1], bg[2], 255]);

    let cw = width as f32;
    let ch = height as f32;

    for track in timeline.tracks_ordered() {
        if !track.kind.is_visual() || !track.enabled {
            continue;
        }
        let Some(clip) = track.clip_at(t)? else {
            continue;
        };
        if let Some(layer) = resolve_clip(project, clip, t, cw, ch, overrides)? {
            scene.layers.push(layer);
        }
    }

    Ok(scene)
}

/// Canvas pixel size for `project`: fixed presets resolve to a 1080-baseline
/// box on the longer side; `Auto` follows the largest video media (falling back
/// to 1920×1080 when there is none).
pub fn canvas_size(project: &Project) -> (u32, u32) {
    match project.timeline().canvas().aspect.ratio() {
        Some((rw, rh)) => ratio_to_pixels(rw, rh),
        None => auto_canvas_size(project),
    }
}

/// Largest visible dimension box for a `w:h` ratio, even-rounded for encoders.
fn ratio_to_pixels(rw: u32, rh: u32) -> (u32, u32) {
    const BASE: f32 = REFERENCE_HEIGHT;
    let (rw, rh) = (rw as f32, rh as f32);
    let (w, h) = if rw >= rh {
        ((BASE * rw / rh).round(), BASE)
    } else {
        (BASE, (BASE * rh / rw).round())
    };
    (even(w as u32), even(h as u32))
}

/// The largest video media used anywhere on the timeline, or the default.
fn auto_canvas_size(project: &Project) -> (u32, u32) {
    let mut best: Option<(u32, u32)> = None;
    for track in project.timeline().tracks_ordered() {
        if !track.kind.is_visual() {
            continue;
        }
        for clip in track.clips() {
            let Some(id) = clip.media() else { continue };
            let Some(media) = project.media(id) else {
                continue;
            };
            if media.kind() != MediaKind::Video {
                continue;
            }
            let area = u64::from(media.width) * u64::from(media.height);
            if best.is_none_or(|(bw, bh)| area > u64::from(bw) * u64::from(bh)) {
                best = Some((media.width, media.height));
            }
        }
    }
    best.map_or(DEFAULT_CANVAS, |(w, h)| (even(w), even(h)))
}

/// Round `v` down to the nearest even value (≥ 2): H.264 needs even dimensions.
fn even(v: u32) -> u32 {
    (v & !1).max(2)
}

fn resolve_clip(
    project: &Project,
    clip: &cutlass_models::Clip,
    t: RationalTime,
    cw: f32,
    ch: f32,
    overrides: ResolveOverrides<'_>,
) -> Result<Option<SceneLayer>, cutlass_models::ModelError> {
    // Clip-relative tick at the timeline rate (both `t` and the clip start are
    // expressed at it), which is what animated transforms key against.
    let local_tick = t.value - clip.timeline.start.value;
    // A live gesture replaces the whole sampled transform for its clip.
    let xf = match overrides.transform {
        Some((id, xf)) if id == clip.id => xf,
        _ => clip.transform.sample(local_tick),
    };

    // `position` is the anchor's offset from the canvas center, as a fraction of
    // canvas width/height. With the default center anchor (the common case) this
    // is the placed quad's center; non-center anchors are a follow-up.
    let center = [cw * (0.5 + xf.position[0]), ch * (0.5 + xf.position[1])];
    let rotation = xf.rotation.to_radians();
    let opacity = xf.opacity.clamp(0.0, 1.0);
    let uv = crop_flip_uv(clip);

    match &clip.content {
        ClipSource::Media { media, .. } => {
            let Some(src) = project.media(*media) else {
                return Ok(None);
            };
            // Both picture kinds aspect-fit into the canvas at their probed
            // size; audio-only media places nothing.
            let source = match src.kind() {
                MediaKind::Video => {
                    let Some(source_time) = clip.source_time_at(t)? else {
                        return Ok(None);
                    };
                    LayerSource::Media {
                        media: *media,
                        source_time,
                    }
                }
                // One frame for the clip's whole extent: no source time, and
                // retime/reverse are irrelevant by construction.
                MediaKind::Image => LayerSource::Still { media: *media },
                MediaKind::Audio => return Ok(None),
            };
            let fit = fit_scale(src.width as f32, src.height as f32, cw, ch);
            let size = SizeSpec::Fixed([
                src.width as f32 * fit * xf.scale,
                src.height as f32 * fit * xf.scale,
            ]);
            Ok(Some(SceneLayer {
                source,
                center,
                size,
                rotation,
                opacity,
                uv,
            }))
        }
        ClipSource::Generated(generator) => {
            // A live inspector edit replaces the clip's generator content.
            let generator = match overrides.generator {
                Some((id, live)) if id == clip.id => live,
                _ => generator,
            };
            Ok(resolve_generator(
                generator, center, rotation, opacity, uv, cw, ch, xf.scale, local_tick,
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_generator(
    generator: &Generator,
    center: [f32; 2],
    rotation: f32,
    opacity: f32,
    uv: [f32; 4],
    cw: f32,
    ch: f32,
    scale: f32,
    tick: i64,
) -> Option<SceneLayer> {
    let ref_scale = ch / REFERENCE_HEIGHT;
    match generator {
        Generator::Text { content, style } => {
            let text = style.case.apply(content);
            if text.trim().is_empty() {
                return None;
            }
            Some(SceneLayer {
                source: LayerSource::Text {
                    content: text,
                    style: map_text_style(style, cw, ch),
                },
                center,
                size: SizeSpec::BitmapScaled(scale),
                rotation,
                opacity,
                uv,
            })
        }
        Generator::SolidColor { rgba } => Some(SceneLayer {
            source: LayerSource::Solid(*rgba),
            center,
            size: SizeSpec::Fixed([cw * scale, ch * scale]),
            rotation,
            opacity,
            uv,
        }),
        Generator::Shape {
            shape,
            rgba,
            width,
            height,
            corner_radius,
            stroke,
        } => resolve_shape(
            shape,
            rgba,
            width,
            height,
            corner_radius,
            stroke.as_ref(),
            tick,
            ref_scale * scale,
            center,
            rotation,
            opacity,
            uv,
            scale,
        ),
        // Stickers/effects/filters/adjustment layers are not composited yet.
        // Skip rather than draw something wrong.
        Generator::Sticker | Generator::Effect | Generator::Filter | Generator::Adjustment => None,
    }
}

/// Resolve one shape generator at `tick` into a placed layer.
///
/// All `Param` curves are sampled here (the resolver is the "animation →
/// values" boundary), and every length is converted to canvas pixels with
/// `px_scale` (reference scale × the clip's animated transform scale), so
/// downstream stages see plain numbers. Parametric shapes become SDF layers
/// whose quad is padded for stroke overhang + anti-aliasing; pen paths become
/// CPU-raster layers that scale like text bitmaps.
#[allow(clippy::too_many_arguments)]
fn resolve_shape(
    shape: &Shape,
    rgba: &Param<[u8; 4]>,
    width: &Param<f32>,
    height: &Param<f32>,
    corner_radius: &Param<f32>,
    stroke: Option<&ShapeStroke>,
    tick: i64,
    px_scale: f32,
    center: [f32; 2],
    rotation: f32,
    opacity: f32,
    uv: [f32; 4],
    transform_scale: f32,
) -> Option<SceneLayer> {
    let fill = rgba.sample(tick);
    let stroke_px = stroke.map(|s| Stroke {
        rgba: s.rgba.sample(tick),
        width: (s.width.sample(tick) * px_scale).max(0.0),
    });

    // Pen paths: rasterized on the CPU at the *reference* scale so the memo
    // stays warm under transform-scale animation (the quad magnifies the
    // bitmap, like text). `px_scale / transform_scale` recovers ref_scale.
    if let Shape::Path(path) = shape {
        let bezier = to_bezier(path);
        if !bezier.is_drawable() {
            return None;
        }
        let raster_scale = if transform_scale > 0.0 {
            px_scale / transform_scale
        } else {
            px_scale
        };
        return Some(SceneLayer {
            source: LayerSource::PathShape {
                path: bezier,
                fill,
                // Raster-space stroke: `PathRaster` folds `raster_scale` into
                // the width itself, so hand it the unscaled model value.
                stroke: stroke.map(|s| Stroke {
                    rgba: s.rgba.sample(tick),
                    width: s.width.sample(tick).max(0.0),
                }),
                raster_scale,
            },
            center,
            size: SizeSpec::BitmapScaled(transform_scale),
            rotation,
            opacity,
            uv,
        });
    }

    let w = width.sample(tick) * px_scale;
    let h = height.sample(tick) * px_scale;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let radius = (corner_radius.sample(tick) * px_scale).max(0.0);

    // Plain rectangles keep the no-texture solid fast path.
    if matches!(shape, Shape::Rectangle) && radius == 0.0 && stroke_px.is_none() {
        return Some(SceneLayer {
            source: LayerSource::Solid(fill),
            center,
            size: SizeSpec::Fixed([w, h]),
            rotation,
            opacity,
            uv,
        });
    }

    let params = match shape {
        Shape::Rectangle => SdfParams::RoundedRect { radius },
        Shape::Ellipse => SdfParams::Ellipse,
        Shape::Polygon { sides } => SdfParams::polygon(*sides, radius),
        Shape::Star {
            points,
            inner_ratio,
        } => SdfParams::Star {
            points: *points,
            inner: inner_ratio.sample(tick).clamp(0.0, 1.0),
            round: radius,
        },
        Shape::Line => SdfParams::Line,
        Shape::Arrow => SdfParams::Arrow,
        Shape::Heart => SdfParams::Heart,
        Shape::Path(_) => unreachable!("handled above"),
    };

    // The quad must cover the stroke's outward half plus the AA ramp, or the
    // shader's ink clips at the quad edge (same margin as the CPU raster).
    let pad = stroke_px.map_or(0.0, |s| s.width * 0.5) + 2.0 * SDF_AA;
    Some(SceneLayer {
        source: LayerSource::Shape {
            params,
            fill,
            stroke: stroke_px,
            pad,
        },
        center,
        size: SizeSpec::Fixed([w + 2.0 * pad, h + 2.0 * pad]),
        rotation,
        opacity,
        uv,
    })
}

/// Convert the model's serialized path into the shapes crate's bezier form.
fn to_bezier(path: &ShapePath) -> BezierPath {
    BezierPath {
        points: path
            .points
            .iter()
            .map(|p| PathPoint {
                anchor: p.anchor,
                handle_in: p.handle_in,
                handle_out: p.handle_out,
            })
            .collect(),
        closed: path.closed,
    }
}

/// Map a model [`ModelTextStyle`] onto a [`cutlass_text`] render style.
///
/// Lossy in v1: stroke, background, shadow, and bold/italic are not rendered
/// yet. Font size is scaled from reference (1080-tall) pixels to the canvas.
///
/// The rasterized bitmap is kept tight to the glyphs (no canvas-width wrap):
/// the layer's placement centers it on the canvas, so passing the canvas width
/// as a wrap constraint here would only double-center the run. Multi-line wrap
/// within a fixed width is a follow-up.
fn map_text_style(style: &ModelTextStyle, _cw: f32, ch: f32) -> TextStyle {
    let font_size = style.size * (ch / REFERENCE_HEIGHT);
    let family = if style.font.is_empty() {
        FontFamily::SansSerif
    } else {
        FontFamily::Named(style.font.clone())
    };
    let align = match style.align_h {
        TextAlignH::Left => TextAlign::Left,
        TextAlignH::Center => TextAlign::Center,
        TextAlignH::Right => TextAlign::Right,
    };
    TextStyle::new(font_size)
        .with_color(style.fill)
        .with_family(family)
        .with_align(align)
        .with_line_height(font_size * style.line_spacing)
}

/// UV rect from a clip's crop, with axes reversed for mirror flags.
fn crop_flip_uv(clip: &cutlass_models::Clip) -> [f32; 4] {
    let c = clip.crop;
    let (mut u0, mut u1) = (c.x, c.x + c.w);
    let (mut v0, mut v1) = (c.y, c.y + c.h);
    if clip.flip_h {
        core::mem::swap(&mut u0, &mut u1);
    }
    if clip.flip_v {
        core::mem::swap(&mut v0, &mut v1);
    }
    [u0, v0, u1, v1]
}

/// Uniform "contain" scale fitting `nw`×`nh` content inside a `cw`×`ch` canvas.
fn fit_scale(nw: f32, nh: f32, cw: f32, ch: f32) -> f32 {
    if nw <= 0.0 || nh <= 0.0 {
        return 1.0;
    }
    (cw / nw).min(ch / nh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{LayerSource, SizeSpec};
    use cutlass_models::{
        CanvasAspect, CanvasSettings, ClipTransform, CropRect, Generator, MediaSource, Project,
        Rational, RationalTime, Shape, TextStyle as ModelTextStyle, TimeRange, TrackKind,
    };

    const FPS_24: Rational = Rational::FPS_24;

    fn rt(value: i64) -> RationalTime {
        RationalTime::new(value, FPS_24)
    }

    fn tr(start: i64, duration: i64) -> TimeRange {
        TimeRange::at_rate(start, duration, FPS_24)
    }

    fn video(width: u32, height: u32) -> MediaSource {
        MediaSource::new("/tmp/v.mp4", width, height, FPS_24, 600, false)
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "expected ~{b}, got {a}");
    }

    fn approx2(a: [f32; 2], b: [f32; 2]) {
        approx(a[0], b[0]);
        approx(a[1], b[1]);
    }

    #[test]
    fn empty_project_uses_default_canvas_and_has_no_layers() {
        let project = Project::new("p", FPS_24);
        let scene = resolve(&project, rt(0)).unwrap();
        assert_eq!((scene.width, scene.height), (1920, 1080));
        assert!(scene.layers.is_empty());
    }

    #[test]
    fn single_video_clip_is_centered_and_aspect_fit() {
        let mut project = Project::new("p", FPS_24);
        let media = project.add_media(video(1920, 1080));
        let track = project.add_track(TrackKind::Video, "V1");
        project.add_clip(track, media, tr(0, 100), rt(0)).unwrap();

        let scene = resolve(&project, rt(5)).unwrap();
        assert_eq!((scene.width, scene.height), (1920, 1080));
        assert_eq!(scene.layers.len(), 1);

        let layer = &scene.layers[0];
        approx2(layer.center, [960.0, 540.0]);
        match layer.size {
            SizeSpec::Fixed(size) => approx2(size, [1920.0, 1080.0]),
            other => panic!("expected fixed size, got {other:?}"),
        }
        approx(layer.opacity, 1.0);
        match &layer.source {
            LayerSource::Media { media: m, source_time } => {
                assert_eq!(*m, media);
                assert_eq!(source_time.value, 5);
            }
            other => panic!("expected media source, got {other:?}"),
        }
    }

    #[test]
    fn clip_inactive_at_time_produces_no_layer() {
        let mut project = Project::new("p", FPS_24);
        let media = project.add_media(video(1920, 1080));
        let track = project.add_track(TrackKind::Video, "V1");
        project.add_clip(track, media, tr(0, 10), rt(0)).unwrap();

        let scene = resolve(&project, rt(50)).unwrap();
        assert!(scene.layers.is_empty());
    }

    #[test]
    fn tracks_stack_bottom_to_top() {
        let mut project = Project::new("p", FPS_24);
        let bottom = project.add_track(TrackKind::Sticker, "S1");
        let top = project.add_track(TrackKind::Sticker, "S2");
        project
            .add_generated(bottom, Generator::SolidColor { rgba: [255, 0, 0, 255] }, tr(0, 100))
            .unwrap();
        project
            .add_generated(top, Generator::SolidColor { rgba: [0, 0, 255, 255] }, tr(0, 100))
            .unwrap();

        let scene = resolve(&project, rt(5)).unwrap();
        assert_eq!(scene.layers.len(), 2);
        assert_eq!(scene.layers[0].source, LayerSource::Solid([255, 0, 0, 255]));
        assert_eq!(scene.layers[1].source, LayerSource::Solid([0, 0, 255, 255]));
    }

    #[test]
    fn text_generator_maps_style_and_defers_size() {
        let mut project = Project::new("p", FPS_24);
        let track = project.add_track(TrackKind::Text, "T1");
        let style = ModelTextStyle {
            size: 90.0,
            fill: [255, 0, 0, 255],
            ..ModelTextStyle::default()
        };
        project
            .add_generated(
                track,
                Generator::Text { content: "Hi".into(), style },
                tr(0, 100),
            )
            .unwrap();

        let scene = resolve(&project, rt(5)).unwrap();
        assert_eq!((scene.width, scene.height), (1920, 1080));
        assert_eq!(scene.layers.len(), 1);
        let layer = &scene.layers[0];
        approx2(layer.center, [960.0, 540.0]);
        assert_eq!(layer.size, SizeSpec::BitmapScaled(1.0));
        match &layer.source {
            LayerSource::Text { content, style } => {
                assert_eq!(content, "Hi");
                approx(style.font_size, 90.0);
                assert_eq!(style.color, [255, 0, 0, 255]);
            }
            other => panic!("expected text source, got {other:?}"),
        }
    }

    #[test]
    fn solid_generator_fills_canvas() {
        let mut project = Project::new("p", FPS_24);
        let track = project.add_track(TrackKind::Sticker, "S1");
        project
            .add_generated(track, Generator::SolidColor { rgba: [10, 20, 30, 255] }, tr(0, 100))
            .unwrap();

        let scene = resolve(&project, rt(5)).unwrap();
        let layer = &scene.layers[0];
        match layer.size {
            SizeSpec::Fixed(size) => approx2(size, [1920.0, 1080.0]),
            other => panic!("expected fixed size, got {other:?}"),
        }
    }

    #[test]
    fn crop_and_horizontal_flip_set_uv() {
        let mut project = Project::new("p", FPS_24);
        let media = project.add_media(video(1920, 1080));
        let track = project.add_track(TrackKind::Video, "V1");
        let clip = project.add_clip(track, media, tr(0, 100), rt(0)).unwrap();
        project
            .set_clip_crop(
                clip,
                CropRect {
                    x: 0.25,
                    y: 0.1,
                    w: 0.5,
                    h: 0.8,
                },
                true,
                false,
            )
            .unwrap();

        let scene = resolve(&project, rt(5)).unwrap();
        let uv = scene.layers[0].uv;
        // base [0.25, 0.1, 0.75, 0.9]; flip_h swaps the u axis.
        approx(uv[0], 0.75);
        approx(uv[1], 0.1);
        approx(uv[2], 0.25);
        approx(uv[3], 0.9);
    }

    #[test]
    fn fixed_aspect_presets_resolve_to_1080_baseline() {
        let mut project = Project::new("p", FPS_24);
        project.timeline_mut().set_canvas(CanvasSettings {
            aspect: CanvasAspect::Tall9x16,
            background: [0, 0, 0],
        });
        assert_eq!(canvas_size(&project), (1080, 1920));

        project.timeline_mut().set_canvas(CanvasSettings {
            aspect: CanvasAspect::Wide16x9,
            background: [0, 0, 0],
        });
        assert_eq!(canvas_size(&project), (1920, 1080));

        project.timeline_mut().set_canvas(CanvasSettings {
            aspect: CanvasAspect::Square1x1,
            background: [0, 0, 0],
        });
        assert_eq!(canvas_size(&project), (1080, 1080));
    }

    #[test]
    fn transform_offsets_center_and_scales_size() {
        let mut project = Project::new("p", FPS_24);
        let media = project.add_media(video(1920, 1080));
        let track = project.add_track(TrackKind::Video, "V1");
        let clip = project.add_clip(track, media, tr(0, 100), rt(0)).unwrap();
        project
            .set_transform(
                clip,
                ClipTransform {
                    position: [0.25, -0.1],
                    scale: 2.0,
                    opacity: 0.5,
                    ..ClipTransform::IDENTITY
                },
                None,
            )
            .unwrap();

        let scene = resolve(&project, rt(5)).unwrap();
        let layer = &scene.layers[0];
        approx2(layer.center, [1920.0 * 0.75, 1080.0 * 0.4]);
        match layer.size {
            SizeSpec::Fixed(size) => approx2(size, [3840.0, 2160.0]),
            other => panic!("expected fixed size, got {other:?}"),
        }
        approx(layer.opacity, 0.5);
    }

    #[test]
    fn image_clip_resolves_to_an_aspect_fit_still_layer() {
        let mut project = Project::new("p", FPS_24);
        // 800×600 still on the default 1920×1080 canvas: fit = 1.8.
        let media = project.add_media(MediaSource::image("/photos/pic.png", 800, 600));
        let window = project.media(media).unwrap().full_range();
        let track = project.add_track(TrackKind::Video, "V1");
        project.add_clip(track, media, window, rt(0)).unwrap();

        let scene = resolve(&project, rt(5)).unwrap();
        assert_eq!(scene.layers.len(), 1);
        let layer = &scene.layers[0];
        assert_eq!(layer.source, LayerSource::Still { media });
        approx2(layer.center, [960.0, 540.0]);
        match layer.size {
            SizeSpec::Fixed(size) => approx2(size, [1440.0, 1080.0]),
            other => panic!("expected fixed size, got {other:?}"),
        }
    }

    #[test]
    fn still_layer_ignores_retiming_and_covers_the_whole_clip() {
        let mut project = Project::new("p", FPS_24);
        let media = project.add_media(MediaSource::image("/photos/pic.png", 1920, 1080));
        let window = project.media(media).unwrap().full_range();
        let track = project.add_track(TrackKind::Video, "V1");
        let clip = project.add_clip(track, media, window, rt(0)).unwrap();
        // Reverse would make a video clip walk its source backward; a still
        // must keep producing its one frame at every covered tick.
        project
            .set_clip_speed(clip, Rational::new(1, 1), true)
            .unwrap();
        let duration = project.timeline().duration().value;
        assert!(duration > 0);

        for t in [0, duration / 2, duration - 1] {
            let scene = resolve(&project, rt(t)).unwrap();
            assert_eq!(scene.layers.len(), 1, "tick {t}");
            assert_eq!(scene.layers[0].source, LayerSource::Still { media });
        }
        // Past the clip: nothing.
        let scene = resolve(&project, rt(duration)).unwrap();
        assert!(scene.layers.is_empty());
    }

    #[test]
    fn audio_track_is_skipped() {
        let mut project = Project::new("p", FPS_24);
        let video_track = project.add_track(TrackKind::Video, "V1");
        let media = project.add_media(video(1920, 1080));
        project.add_clip(video_track, media, tr(0, 100), rt(0)).unwrap();

        let audio_track = project.add_track(TrackKind::Audio, "A1");
        let song = project.add_media(MediaSource::new("/tmp/a.mp3", 0, 0, FPS_24, 600, true));
        project.add_clip(audio_track, song, tr(0, 100), rt(0)).unwrap();

        let scene = resolve(&project, rt(5)).unwrap();
        assert_eq!(scene.layers.len(), 1);
        assert!(matches!(scene.layers[0].source, LayerSource::Media { .. }));
    }

    #[test]
    fn plain_rectangle_keeps_the_solid_fast_path() {
        let mut project = Project::new("p", FPS_24);
        let track = project.add_track(TrackKind::Sticker, "S1");
        project
            .add_generated(track, Generator::shape(Shape::Rectangle, [255, 255, 255, 255]), tr(0, 100))
            .unwrap();
        let scene = resolve(&project, rt(5)).unwrap();
        assert_eq!(scene.layers.len(), 1);
        assert!(matches!(scene.layers[0].source, LayerSource::Solid(_)));
        // Drop size 200×200 reference px on a 1080 canvas: 1:1.
        assert_eq!(scene.layers[0].size, SizeSpec::Fixed([200.0, 200.0]));
    }

    #[test]
    fn ellipse_resolves_to_a_padded_sdf_layer() {
        let mut project = Project::new("p", FPS_24);
        let track = project.add_track(TrackKind::Sticker, "S1");
        project
            .add_generated(track, Generator::shape(Shape::Ellipse, [10, 20, 30, 255]), tr(0, 100))
            .unwrap();
        let scene = resolve(&project, rt(5)).unwrap();
        assert_eq!(scene.layers.len(), 1);
        let layer = &scene.layers[0];
        match &layer.source {
            LayerSource::Shape {
                params,
                fill,
                stroke,
                pad,
            } => {
                assert_eq!(*params, cutlass_shapes::SdfParams::Ellipse);
                assert_eq!(*fill, [10, 20, 30, 255]);
                assert!(stroke.is_none());
                // No stroke: pad is the 2px AA margin only.
                approx(*pad, 2.0 * cutlass_shapes::SDF_AA);
            }
            other => panic!("expected shape source, got {other:?}"),
        }
        // Quad = 200×200 shape + pad on each side.
        match layer.size {
            SizeSpec::Fixed(size) => approx2(size, [204.0, 204.0]),
            other => panic!("expected fixed size, got {other:?}"),
        }
    }

    #[test]
    fn rounded_or_stroked_rectangle_leaves_the_fast_path() {
        let mut project = Project::new("p", FPS_24);
        let track = project.add_track(TrackKind::Sticker, "S1");
        let generator = Generator::Shape {
            shape: Shape::Rectangle,
            rgba: cutlass_models::Param::Constant([255, 0, 0, 255]),
            width: cutlass_models::Param::Constant(100.0),
            height: cutlass_models::Param::Constant(50.0),
            corner_radius: cutlass_models::Param::Constant(8.0),
            stroke: Some(cutlass_models::ShapeStroke::new([0, 0, 0, 255], 6.0)),
        };
        project.add_generated(track, generator, tr(0, 100)).unwrap();
        let scene = resolve(&project, rt(5)).unwrap();
        let layer = &scene.layers[0];
        match &layer.source {
            LayerSource::Shape {
                params,
                stroke,
                pad,
                ..
            } => {
                assert_eq!(
                    *params,
                    cutlass_shapes::SdfParams::RoundedRect { radius: 8.0 }
                );
                let s = stroke.expect("stroke resolved");
                approx(s.width, 6.0);
                // Pad covers the stroke's outward half plus the AA margin.
                approx(*pad, 3.0 + 2.0 * cutlass_shapes::SDF_AA);
            }
            other => panic!("expected shape source, got {other:?}"),
        }
        match layer.size {
            SizeSpec::Fixed(size) => approx2(size, [110.0, 60.0]),
            other => panic!("expected fixed size, got {other:?}"),
        }
    }

    #[test]
    fn animated_shape_params_sample_at_the_clip_tick() {
        use cutlass_models::{ClipParam, Easing, ParamValue, ShapeParam};

        let mut project = Project::new("p", FPS_24);
        let track = project.add_track(TrackKind::Sticker, "S1");
        let clip = project
            .add_generated(
                track,
                Generator::shape(
                    Shape::Star {
                        points: 5,
                        inner_ratio: cutlass_models::Param::Constant(0.5),
                    },
                    [255, 255, 255, 255],
                ),
                tr(0, 100),
            )
            .unwrap();
        let width = ClipParam::Shape {
            param: ShapeParam::Width,
        };
        let fill = ClipParam::Shape {
            param: ShapeParam::Fill,
        };
        for (param, at, value) in [
            (width, 0, ParamValue::Scalar(100.0)),
            (width, 50, ParamValue::Scalar(300.0)),
            (fill, 0, ParamValue::Color([0, 0, 0, 255])),
            (fill, 50, ParamValue::Color([200, 100, 0, 255])),
        ] {
            project
                .set_param_keyframe(clip, param, rt(at), value, Easing::Linear)
                .unwrap();
        }

        let scene = resolve(&project, rt(25)).unwrap();
        let layer = &scene.layers[0];
        match &layer.source {
            LayerSource::Shape { params, fill, pad, .. } => {
                assert!(matches!(
                    params,
                    cutlass_shapes::SdfParams::Star { points: 5, .. }
                ));
                assert_eq!(*fill, [100, 50, 0, 255], "colors lerp per channel");
                // Width halfway between 100 and 300 → 200 + 2·pad quad.
                match layer.size {
                    SizeSpec::Fixed(size) => approx(size[0], 200.0 + 2.0 * pad),
                    other => panic!("expected fixed size, got {other:?}"),
                }
            }
            other => panic!("expected shape source, got {other:?}"),
        }
    }

    #[test]
    fn pen_path_resolves_to_a_bitmap_scaled_path_layer() {
        use cutlass_models::{ShapePath, ShapePathPoint};

        let mut project = Project::new("p", FPS_24);
        let track = project.add_track(TrackKind::Sticker, "S1");
        let path = ShapePath {
            points: vec![
                ShapePathPoint::corner([-40.0, -40.0]),
                ShapePathPoint::corner([40.0, -40.0]),
                ShapePathPoint::corner([0.0, 40.0]),
            ],
            closed: true,
        };
        project
            .add_generated(
                track,
                Generator::shape(Shape::Path(path), [0, 255, 0, 255]),
                tr(0, 100),
            )
            .unwrap();

        let scene = resolve(&project, rt(5)).unwrap();
        let layer = &scene.layers[0];
        match &layer.source {
            LayerSource::PathShape {
                path,
                fill,
                raster_scale,
                ..
            } => {
                assert_eq!(path.points.len(), 3);
                assert!(path.closed);
                assert_eq!(*fill, [0, 255, 0, 255]);
                approx(*raster_scale, 1.0); // 1080 canvas → reference scale 1
            }
            other => panic!("expected path source, got {other:?}"),
        }
        // Transform scale rides the quad, not the raster.
        assert_eq!(layer.size, SizeSpec::BitmapScaled(1.0));
    }

    /// The model's validation cap and the evaluator's vertex-buffer bound
    /// must agree, or a valid project could hold a star the renderer clamps.
    #[test]
    fn star_point_cap_matches_the_shapes_crate() {
        assert_eq!(
            cutlass_models::MAX_STAR_POINTS,
            cutlass_shapes::MAX_STAR_POINTS
        );
    }
}
