//! Glue between the Slint UI and the [`engine`] crate for the live preview.
//!
//! Responsibilities:
//!
//! * Spawn the engine worker so the editor has a live decoder to talk to.
//! * Forward `AppState.request-preview(time-sec)` → `Engine::seek_scrub`.
//!   The call is non-blocking — the engine has a latest-wins scrub slot so we
//!   can drive this every pointer move without backing up.
//! * Drain `EngineEvent`s on a small background thread and post the resulting
//!   `slint::Image` to the UI thread via `slint::invoke_from_event_loop`.
//!
//! On a fresh empty project there's no source open yet — the preview pane
//! stays blank until media import wires `Engine::open` into project state.
//!
//! The returned [`PreviewSession`] keeps the engine handle + drain thread
//! alive for the lifetime of the editor. Drop it and the engine shuts down.

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Weak};
use tracing::{debug, error, warn};

use engine::{Engine, EngineEvent, EventReceiver, Rational};

use crate::ui::{AppState, EditorWindow};

pub struct PreviewSession {
    /// Holding the engine here keeps the worker thread alive.
    _engine: Arc<Engine>,
    /// Background thread that funnels engine events to the UI.
    _drain: JoinHandle<()>,
}

/// Install the preview pipeline. Spawns the engine, connects the playhead →
/// seek bridge, and starts pumping decoded frames into `AppState.preview-frame`.
/// No source is opened — that happens when the user imports media.
pub fn install(editor: &EditorWindow) -> PreviewSession {
    let (engine, rx) = Engine::spawn();
    let engine = Arc::new(engine);

    install_playhead_bridge(editor, engine.clone());
    let drain = spawn_event_drain(editor.as_weak(), rx);

    PreviewSession {
        _engine: engine,
        _drain: drain,
    }
}

fn install_playhead_bridge(editor: &EditorWindow, engine: Arc<Engine>) {
    editor
        .global::<AppState>()
        .on_request_preview(move |sec: f32| {
            engine.seek_scrub(seconds_to_rational(sec as f64));
        });
}

fn spawn_event_drain(weak_editor: Weak<EditorWindow>, rx: EventReceiver) -> JoinHandle<()> {
    thread::Builder::new()
        .name("cutlass-preview-drain".into())
        .spawn(move || drain_loop(weak_editor, rx))
        .expect("spawn cutlass-preview-drain thread")
}

fn drain_loop(weak_editor: Weak<EditorWindow>, rx: EventReceiver) {
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
                let editor = weak_editor.clone();
                if let Err(e) = slint::invoke_from_event_loop(move || {
                    if let Some(editor) = editor.upgrade() {
                        let image = rgba_bytes_to_image(width, height, &rgba);
                        editor.global::<AppState>().set_preview_frame(image);
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
