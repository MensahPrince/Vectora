//! Filmstrip thumbnails over the C ABI.
//!
//! Clip filmstrips and picker cells want many small frames from one media
//! file, and they must not contend with the editor session for its renderer.
//! A `CutlassThumbnailer` owns a private renderer (its own GPU queue + decoder
//! cache) bound to a single media file, so the shell can run it from any
//! worker queue: thumbnails decode while the session keeps scrubbing.
//!
//! Same non-concurrency rule as the session: calls on one handle must be
//! serialized, but different handles are independent.

use cutlass_models::{MediaSource, Project, Rational, RationalTime, TimeRange, TrackKind};
use cutlass_render::Renderer;

use crate::CutlassImage;
use crate::wire::str_arg;

/// A thumbnail source for one media file. Free with
/// [`cutlass_thumbnailer_close`].
pub struct CutlassThumbnailer {
    renderer: Renderer,
    /// A single-clip project spanning the whole file — thumbnails are just
    /// fit-rendered frames of it, riding the same resolve/render path as the
    /// editor so orientation/aspect handling can never drift.
    project: Project,
    frame_rate: Rational,
    duration_frames: i64,
}

impl CutlassThumbnailer {
    fn open(path: &str) -> Option<Self> {
        let path = std::path::Path::new(path);
        let probe = match cutlass_decoder::probe(path) {
            Ok(probe) => probe,
            Err(e) => {
                log::error!("thumbnailer: probe failed for {}: {e}", path.display());
                return None;
            }
        };
        let source = if probe.is_image {
            // One cached frame for any slot: the still convention gives the
            // filmstrip a nominal 5s window so slot spacing math stays sane.
            MediaSource::image(path, probe.width, probe.height)
        } else {
            MediaSource::new(
                path,
                probe.width,
                probe.height,
                probe.frame_rate,
                probe.frame_count.max(1),
                probe.has_audio,
            )
        };
        let frame_rate = source.frame_rate;
        let frames = source.duration.value.max(1);
        let mut project = Project::new("thumbnailer", frame_rate);
        let media = project.add_media(source);
        let track = project.add_track(TrackKind::Video, "Media");
        if let Err(e) = project.add_clip(
            track,
            media,
            TimeRange::at_rate(0, frames, frame_rate),
            RationalTime::new(0, frame_rate),
        ) {
            log::error!("thumbnailer: add_clip failed: {e}");
            return None;
        }
        let renderer = match Renderer::new_headless() {
            Ok(renderer) => renderer,
            Err(e) => {
                log::error!("thumbnailer: renderer init failed: {e}");
                return None;
            }
        };
        Some(Self {
            renderer,
            project,
            frame_rate,
            duration_frames: frames,
        })
    }

    fn thumb(&mut self, seconds: f64, max_width: u32, max_height: u32) -> Option<CutlassImage> {
        let max_frame = (self.duration_frames - 1).max(0);
        let frame = crate::intents::frame_tick(seconds, self.frame_rate).clamp(0, max_frame);
        match self.renderer.render_frame_fit(
            &self.project,
            RationalTime::new(frame, self.frame_rate),
            max_width,
            max_height,
        ) {
            Ok(image) => Some(CutlassImage::from_rgba(image)),
            Err(e) => {
                log::error!("thumbnailer: render at frame {frame} failed: {e}");
                None
            }
        }
    }
}

/// Open a thumbnailer for the media file at `path_utf8` (UTF-8, `path_len`
/// bytes). Null if the file can't be probed or the GPU is unavailable.
///
/// # Safety
/// `path_utf8` must point to `path_len` initialized bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_thumbnailer_open(
    path_utf8: *const u8,
    path_len: usize,
) -> *mut CutlassThumbnailer {
    // SAFETY: caller guarantees the byte range.
    let Some(path) = (unsafe { str_arg(path_utf8, path_len) }) else {
        return std::ptr::null_mut();
    };
    match CutlassThumbnailer::open(path) {
        Some(thumbnailer) => Box::into_raw(Box::new(thumbnailer)),
        None => std::ptr::null_mut(),
    }
}

/// Media length in seconds (0 for a null handle) — lets the shell space
/// filmstrip slots without a second probe.
///
/// # Safety
/// `handle` must be null or a live thumbnailer pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_thumbnailer_duration_seconds(
    handle: *const CutlassThumbnailer,
) -> f64 {
    // SAFETY: caller contract.
    let Some(thumbnailer) = (unsafe { handle.as_ref() }) else {
        return 0.0;
    };
    RationalTime::new(thumbnailer.duration_frames, thumbnailer.frame_rate).seconds()
}

/// Render the frame nearest `seconds`, scaled to fit `max_width`×`max_height`
/// (never upscaled). On failure the returned [`CutlassImage`] has
/// `data == null`; free non-null results with [`cutlass_image_free`].
///
/// # Safety
/// `handle` must be null or a live thumbnailer pointer used non-concurrently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_thumbnailer_thumb(
    handle: *mut CutlassThumbnailer,
    seconds: f64,
    max_width: u32,
    max_height: u32,
) -> CutlassImage {
    // SAFETY: caller contract.
    let Some(thumbnailer) = (unsafe { handle.as_mut() }) else {
        return CutlassImage::empty();
    };
    if max_width == 0 || max_height == 0 {
        return CutlassImage::empty();
    }
    thumbnailer
        .thumb(seconds, max_width, max_height)
        .unwrap_or_else(CutlassImage::empty)
}

/// Release a thumbnailer handle. Null is a no-op.
///
/// # Safety
/// `handle` must be null or a live thumbnailer pointer, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_thumbnailer_close(handle: *mut CutlassThumbnailer) {
    if !handle.is_null() {
        // SAFETY: came from Box::into_raw, freed exactly once.
        drop(unsafe { Box::from_raw(handle) });
    }
}
