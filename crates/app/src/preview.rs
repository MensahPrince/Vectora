//! Glue between the Slint UI and the [`engine`] crate for the live preview.
//!
//! Responsibilities:
//!
//! * Spawn the engine worker and open the demo video.
//! * Forward `AppState.request-preview(time-sec)` → `Engine::seek_scrub`.
//!   The call is non-blocking — the engine has a latest-wins scrub slot so we
//!   can drive this every pointer move without backing up.
//! * Drain `EngineEvent`s on a small background thread and post the resulting
//!   `slint::Image` to the UI thread via `slint::invoke_from_event_loop`.
//!
//! The returned [`PreviewSession`] keeps the engine handle + drain thread
//! alive for the lifetime of the app. Drop it and the engine shuts down.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Weak};
use tracing::{debug, error, warn};

use engine::{Engine, EngineEvent, EventReceiver, Rational};

use crate::ui::{AppState, AppWindow};

/// Demo asset shipped under `assets/`. 720p H.264, ~13 s — small enough to
/// load fast, large enough to look like real footage. Swap to a real
/// media-bin lookup once the importer lands.
const DEMO_RELATIVE: &str = "assets/15881269_3840_2160_60fps_720p_proxy.mp4";

pub struct PreviewSession {
    /// Holding the engine here keeps the worker thread alive.
    _engine: Arc<Engine>,
    /// Background thread that funnels engine events to the UI.
    _drain: JoinHandle<()>,
}

/// Install the preview pipeline. Spawns the engine, opens the demo source,
/// connects the playhead → seek bridge, and starts pumping frames into
/// `AppState.preview-frame`.
pub fn install(app: &AppWindow) -> PreviewSession {
    let (engine, rx) = Engine::spawn();
    let engine = Arc::new(engine);

    let demo_path = resolve_demo_path();
    if let Err(e) = engine.open(demo_path.clone()) {
        error!(?e, path = %demo_path.display(), "engine: open failed");
    } else {
        debug!(path = %demo_path.display(), "engine: opened demo source");
    }

    install_playhead_bridge(app, engine.clone());
    let drain = spawn_event_drain(app.as_weak(), rx);

    // Kick a seek so the first frame appears even before the user touches
    // anything. `playhead-x = 160px, zoom = 50 px/s → 3.2 s`.
    engine.seek_scrub(seconds_to_rational(3.2));

    PreviewSession {
        _engine: engine,
        _drain: drain,
    }
}

fn install_playhead_bridge(app: &AppWindow, engine: Arc<Engine>) {
    app.global::<AppState>().on_request_preview(move |sec: f32| {
        engine.seek_scrub(seconds_to_rational(sec as f64));
    });
}

fn spawn_event_drain(weak_app: Weak<AppWindow>, rx: EventReceiver) -> JoinHandle<()> {
    thread::Builder::new()
        .name("cutlass-preview-drain".into())
        .spawn(move || drain_loop(weak_app, rx))
        .expect("spawn cutlass-preview-drain thread")
}

fn drain_loop(weak_app: Weak<AppWindow>, rx: EventReceiver) {
    while let Ok(ev) = rx.recv() {
        match ev {
            EngineEvent::Opened {
                width,
                height,
                duration,
            } => {
                debug!(
                    width,
                    height,
                    duration_sec = duration.map(|r| r.as_f64()).unwrap_or(0.0),
                    "engine: source opened"
                );
            }
            EngineEvent::Frame(frame) => {
                // `slint::Image` is `!Send` (contains a VRc), so we have to
                // ship the raw RGBA bytes across the thread and rebuild the
                // image on the UI thread inside the closure.
                let width = frame.width;
                let height = frame.height;
                let rgba = frame.rgba;
                let app = weak_app.clone();
                if let Err(e) = slint::invoke_from_event_loop(move || {
                    if let Some(app) = app.upgrade() {
                        let image = rgba_bytes_to_image(width, height, &rgba);
                        app.global::<AppState>().set_preview_frame(image);
                    }
                }) {
                    warn!(?e, "engine drain: event loop closed");
                    break;
                }
            }
            EngineEvent::Eof => {
                debug!("engine: EOF (seek past end of stream)");
            }
            EngineEvent::Error(e) => {
                error!(?e, "engine error");
            }
        }
    }
}

/// Build a Slint image from packed RGBA8 bytes. Must run on the UI thread —
/// `Image` is `!Send`.
fn rgba_bytes_to_image(width: u32, height: u32, rgba: &[u8]) -> Image {
    debug_assert_eq!(
        rgba.len(),
        (width as usize) * (height as usize) * 4,
        "rgba must be tightly packed",
    );
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buf.make_mut_bytes().copy_from_slice(rgba);
    Image::from_rgba8(buf)
}

/// Convert seconds (as a `f64` from Slint) into a [`Rational`] with a fixed
/// microsecond denominator. Sub-microsecond is plenty for scrub previews.
fn seconds_to_rational(sec: f64) -> Rational {
    const DEN: u32 = 1_000_000;
    let num = (sec.max(0.0) * f64::from(DEN)).round() as i64;
    Rational::new_raw(num, DEN)
}

/// Resolve the demo asset path relative to the workspace root. Falls back to
/// the cwd-relative path if the workspace can't be located (e.g. installed
/// binary), which is fine for dev workflow.
fn resolve_demo_path() -> PathBuf {
    // CARGO_MANIFEST_DIR points at `crates/app/`; the assets live two levels up.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace) = manifest.ancestors().nth(2) {
        let p = workspace.join(DEMO_RELATIVE);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from(DEMO_RELATIVE)
}
