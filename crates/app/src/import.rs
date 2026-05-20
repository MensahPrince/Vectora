//! Media import: file picker → engine probe → `Session::add_media`.
//!
//! Flow on `AppState::import_media`:
//!   1. Open a native file picker via `rfd` (sync). The Slint event loop is
//!      already pumping on this thread; rfd's modal dialog co-operates with
//!      that loop on macOS/Win/Linux.
//!   2. For each picked path, run a synchronous `engine::probe`. Probing
//!      takes single-digit milliseconds for local files (no codec open), so
//!      doing it on the UI thread is fine for the small batches a picker
//!      yields. If the import surface grows to drag-drop of large folders,
//!      move probing onto a worker (the engine API is `Send`-safe).
//!   3. Build a `models::MediaSource` (full populated on probe success;
//!      `is_supported: false` with an error message on failure — the UI
//!      renders that as a red-bordered tile so the user can still see the
//!      file landed).
//!   4. Hand it to [`Session::add_media`], which appends to the
//!      authoritative `Project.media_bin`, sets `is_dirty`, and refreshes
//!      the Slint DTO via the session's `on_changed` callback.
//!
//! The Slint callback is registered once, in `install`, and lives for the
//! lifetime of the editor window. The `Rc<RefCell<Session>>` is the
//! single source of truth for project state.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use models::{
    AudioStreamInfo, MediaId, MediaKind, MediaSource, Rational, RationalTime, VideoStreamInfo,
};
use slint::ComponentHandle;
use tracing::{info, warn};

use crate::session::Session;
use crate::ui::{AppState, EditorWindow};

/// File extensions exposed in the picker's "Media" filter. The probe doesn't
/// care about extensions (ffmpeg sniffs the container), but the dialog needs
/// a list to gate visibility, and these are the formats we expect to actually
/// import. Order is alphabetical so it's easy to spot omissions.
const MEDIA_EXTENSIONS: &[&str] = &[
    // Video containers
    "avi", "m4v", "mkv", "mov", "mp4", "mts", "ts", "webm", "wmv",
    // Audio
    "aac", "aif", "aiff", "flac", "m4a", "mp3", "ogg", "opus", "wav",
    // Image
    "bmp", "gif", "jpeg", "jpg", "png", "tif", "tiff", "webp",
];

pub fn install(editor: &EditorWindow, session: Rc<RefCell<Session>>) {
    editor.global::<AppState>().on_import_media(move || {
        let paths = pick_paths();
        if paths.is_empty() {
            return;
        }
        info!(count = paths.len(), "import: probing");

        // One borrow for the whole batch is simpler than re-borrowing
        // per file. NOTE: `add_media` notifies after every append, so a
        // 10-file import currently triggers 10 full-DTO refreshes. If
        // that becomes visible jank, add `Session::batch(|s| { … })`
        // that suppresses notify until the closure ends and rebuild
        // once at the end. Realistic batches are small enough today
        // that it isn't worth the API surface.
        let mut session = session.borrow_mut();
        for path in paths {
            let source = build_media_source(path);
            session.add_media(source);
        }
    });
}

fn pick_paths() -> Vec<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Import media")
        .add_filter("Media", MEDIA_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_files()
        .unwrap_or_default()
}

/// Probe a file, then synthesize the `MediaSource` whether probe succeeded
/// or not — failures still land in the bin so the user sees them rather than
/// having files silently disappear into the void.
pub(crate) fn build_media_source(path: PathBuf) -> MediaSource {
    let name = display_name(&path);
    let id = MediaId::new();

    match engine::probe(&path) {
        Ok(p) => MediaSource {
            id,
            name,
            path,
            kind: probed_kind_to_model(p.kind),
            has_video: p.video.is_some(),
            has_audio: p.audio.is_some(),
            duration: rational_to_time(p.duration),
            video: p.video.map(|v| VideoStreamInfo {
                width: v.width,
                height: v.height,
                fps: rational_to_models(v.fps),
                codec: v.codec,
            }),
            audio: p.audio.map(|a| AudioStreamInfo {
                sample_rate: a.sample_rate,
                codec: a.codec,
            }),
            is_supported: true,
            is_loading: false,
            is_missing: false,
            error: None,
        },
        Err(e) => {
            warn!(?path, error = %e, "import: probe failed");
            MediaSource {
                id,
                name,
                path,
                // Best-effort guess from extension so the failing tile still
                // gets the right placeholder colour; engine refused so
                // `is_supported = false` regardless.
                kind: kind_from_extension(),
                has_video: false,
                has_audio: false,
                duration: RationalTime::ZERO,
                video: None,
                audio: None,
                is_supported: false,
                is_loading: false,
                is_missing: false,
                error: Some(e.to_string()),
            }
        }
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn probed_kind_to_model(k: engine::ProbedKind) -> MediaKind {
    match k {
        engine::ProbedKind::Video => MediaKind::Video,
        engine::ProbedKind::Audio => MediaKind::Audio,
        engine::ProbedKind::Image => MediaKind::Image,
    }
}

fn rational_to_time(r: Option<engine::Rational>) -> RationalTime {
    match r {
        // `engine::Rational` and `RationalTime` share `(i64, u32)`; the probe
        // already guards against zero denominators so this is a direct copy.
        Some(r) if r.den != 0 => RationalTime::new_raw(r.num, r.den),
        _ => RationalTime::ZERO,
    }
}

/// `engine::Rational` is 64-bit numerator; `models::Rational` (used for fps
/// and clip speed) is 32-bit. fps numerators stay tiny (≤ 240_000 for
/// 240 fps NTSC) so saturation is a paranoia clamp, not an expected branch.
fn rational_to_models(r: engine::Rational) -> Rational {
    let num = r.num.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    Rational {
        num,
        den: r.den.max(1),
    }
}

/// Probe failed → we have nothing to classify with. Returning `Video` keeps
/// the placeholder neutral; we don't try to guess from extension yet because
/// (a) the user-visible state is the red error border anyway, and (b)
/// guessing-then-being-wrong adds confusion.
fn kind_from_extension() -> MediaKind {
    MediaKind::Video
}

#[cfg(test)]
mod tests {
    //! Pure-helper coverage. The rfd-driven flow (`pick_paths`) and the
    //! Slint-bound `on_import_media` callback need a real `EditorWindow`
    //! + event loop; those paths are exercised end-to-end manually.

    use super::*;

    // --- rational_to_models -------------------------------------------------

    #[test]
    fn rational_to_models_clamps_to_i32() {
        // engine::Rational has 64-bit numerator; models::Rational is 32-bit.
        // A real fps numerator never goes near i32::MAX (240 fps NTSC tops
        // out at 240_000), but probe is FFI-backed so a hostile / corrupt
        // container could in principle deliver a wild value. Saturating
        // clamp keeps that from wrapping into a negative fps.
        let hi = rational_to_models(engine::Rational { num: i64::MAX, den: 1 });
        assert_eq!(hi.num, i32::MAX);
        assert_eq!(hi.den, 1);

        let lo = rational_to_models(engine::Rational { num: i64::MIN, den: 1 });
        assert_eq!(lo.num, i32::MIN);
        assert_eq!(lo.den, 1);
    }

    #[test]
    fn rational_to_models_preserves_normal_fps() {
        // 30000/1001 ≈ 29.97 is the canonical NTSC fps — must survive the
        // domain bridge byte-for-byte.
        let r = rational_to_models(engine::Rational {
            num: 30_000,
            den: 1_001,
        });
        assert_eq!(r.num, 30_000);
        assert_eq!(r.den, 1_001);
    }

    #[test]
    fn rational_to_models_forces_nonzero_den() {
        // models::Rational invariant: den > 0. Probe pre-guards zero
        // denominators, but the helper is defensive — feed it a poisoned
        // value and verify it self-heals to 1 rather than producing a
        // structurally invalid rational.
        let r = rational_to_models(engine::Rational { num: 30, den: 0 });
        assert_eq!(r.num, 30);
        assert_eq!(r.den, 1, "den must be clamped to ≥ 1");
    }

    // --- rational_to_time ---------------------------------------------------

    #[test]
    fn rational_to_time_none_returns_zero() {
        assert_eq!(rational_to_time(None), RationalTime::ZERO);
    }

    #[test]
    fn rational_to_time_zero_den_returns_zero() {
        // Probe currently never emits den=0 (guarded by Rational::new), but
        // engine::Rational fields are public so a future regression could.
        // Helper defends against that and falls through to ZERO.
        let zero_den = engine::Rational { num: 0, den: 0 };
        assert_eq!(rational_to_time(Some(zero_den)), RationalTime::ZERO);
    }

    #[test]
    fn rational_to_time_preserves_normal_duration() {
        // 5 s at microsecond timebase — the exact shape probe emits for an
        // mp4 container (AV_TIME_BASE = 1/1_000_000).
        let r = engine::Rational {
            num: 5_000_000,
            den: 1_000_000,
        };
        let t = rational_to_time(Some(r));
        assert_eq!(t.num, 5_000_000);
        assert_eq!(t.den, 1_000_000);
    }

    // --- display_name -------------------------------------------------------

    #[test]
    fn display_name_extracts_filename() {
        let name = display_name(Path::new("/foo/bar/baz.mp4"));
        assert_eq!(name, "baz.mp4");
    }

    #[test]
    fn display_name_falls_back_on_path_for_no_filename() {
        // `Path::file_name` returns None for paths ending in `..` or for `/`
        // itself; in either case the user-facing label needs *something*.
        let name = display_name(Path::new("/"));
        assert!(!name.is_empty(), "fallback must produce a label");
        assert_eq!(name, "/");
    }

    // --- probed_kind_to_model -----------------------------------------------

    #[test]
    fn probed_kind_to_model_round_trip() {
        assert_eq!(
            probed_kind_to_model(engine::ProbedKind::Video),
            MediaKind::Video
        );
        assert_eq!(
            probed_kind_to_model(engine::ProbedKind::Audio),
            MediaKind::Audio
        );
        assert_eq!(
            probed_kind_to_model(engine::ProbedKind::Image),
            MediaKind::Image
        );
    }
}
