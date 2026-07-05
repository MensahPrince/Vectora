//! Host-side tests for the session FFI: the same C entry points the Swift and
//! Kotlin bridges call, driven in-process through the rlib.
//!
//! Everything here goes through the JSON wire — commands, intents, UI state —
//! so these tests double as contract tests for the response envelopes the
//! shells parse.

use std::ffi::CStr;
use std::os::raw::c_char;

use cutlass_mobile::wire::cutlass_string_free;
use cutlass_mobile::{
    CutlassSession,
    session::{
        cutlass_session_apply, cutlass_session_can_redo, cutlass_session_can_undo,
        cutlass_session_close, cutlass_session_duration_seconds, cutlass_session_intent,
        cutlass_session_is_dirty, cutlass_session_new, cutlass_session_open, cutlass_session_redo,
        cutlass_session_render_fit, cutlass_session_revision, cutlass_session_ui_state,
        cutlass_session_undo,
    },
};

/// Take ownership of a Rust-allocated C string and parse it as JSON.
fn take_json(ptr: *mut c_char) -> serde_json::Value {
    assert!(!ptr.is_null(), "expected a JSON response, got null");
    // SAFETY: ptr came from a session call that allocated a NUL-terminated
    // string via CString.
    let text = unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .expect("FFI strings are UTF-8")
        .to_owned();
    unsafe { cutlass_string_free(ptr) };
    serde_json::from_str(&text).expect("FFI strings are JSON")
}

fn apply(session: *mut CutlassSession, command: serde_json::Value) -> serde_json::Value {
    let json = command.to_string();
    let response = unsafe { cutlass_session_apply(session, json.as_ptr(), json.len()) };
    take_json(response)
}

fn intent(session: *mut CutlassSession, intent: serde_json::Value) -> serde_json::Value {
    let json = intent.to_string();
    let response = unsafe { cutlass_session_intent(session, json.as_ptr(), json.len()) };
    take_json(response)
}

fn ui_state(session: *mut CutlassSession) -> serde_json::Value {
    take_json(unsafe { cutlass_session_ui_state(session) })
}

/// The `lanes` array of the current UI state.
fn lanes(session: *mut CutlassSession) -> Vec<serde_json::Value> {
    ui_state(session)["lanes"]
        .as_array()
        .expect("lanes")
        .clone()
}

/// Add a sticker lane holding a 2-second full-canvas red solid (media-free,
/// renders everywhere — video tracks only accept media clips). Returns the
/// `AddGenerated` response envelope.
fn add_solid(session: *mut CutlassSession) -> serde_json::Value {
    let created = apply(
        session,
        serde_json::json!({"type": "AddTrack", "kind": "Sticker", "name": "Solids"}),
    );
    let track = created["ok"]["value"]["id"].as_u64().expect("track id");
    apply(
        session,
        serde_json::json!({
            "type": "AddGenerated",
            "track": track,
            "generator": {"SolidColor": {"rgba": [255, 0, 0, 255]}},
            "timeline": {
                "start": {"value": 0, "rate": {"num": 30, "den": 1}},
                "duration": {"value": 60, "rate": {"num": 30, "den": 1}},
            },
        }),
    )
}

/// The clips array of the first lane with `kind`.
fn lane_clips(session: *mut CutlassSession, kind: &str) -> Vec<serde_json::Value> {
    lanes(session)
        .into_iter()
        .find(|l| l["kind"] == kind)
        .map(|l| l["clips"].as_array().expect("clips").clone())
        .unwrap_or_default()
}

#[test]
fn null_handles_are_safe() {
    let null = std::ptr::null_mut();
    let response = apply(null, serde_json::json!({"type": "Undo"}));
    assert_eq!(response["err"]["kind"], "protocol");

    unsafe {
        assert!(cutlass_session_ui_state(null).is_null());
        assert!(!cutlass_session_undo(null));
        assert!(!cutlass_session_can_redo(null));
        assert_eq!(cutlass_session_revision(null), 0);
        assert_eq!(cutlass_session_duration_seconds(null), 0.0);
        let image = cutlass_session_render_fit(null, 0.0, 100, 100);
        assert!(image.data.is_null());
        cutlass_session_close(null); // no-op
    }
}

#[test]
fn new_session_starts_with_an_empty_main_lane() {
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null(), "session_new needs a working GPU");

    let state = ui_state(session);
    assert_eq!(state["revision"], 0);
    assert_eq!(state["dirty"], false);
    assert_eq!(state["can_undo"], false);
    assert_eq!(state["fps"], serde_json::json!({"num": 30, "den": 1}));
    assert_eq!(state["duration_seconds"], 0.0);

    let lanes = state["lanes"].as_array().expect("lanes");
    assert_eq!(lanes.len(), 1, "just the main lane");
    assert_eq!(lanes[0]["is_main"], true);
    assert_eq!(lanes[0]["kind"], "video");
    assert_eq!(lanes[0]["clips"].as_array().expect("clips").len(), 0);

    unsafe { cutlass_session_close(session) };
}

#[test]
fn apply_undo_redo_roundtrip_through_json() {
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());

    let response = add_solid(session);
    assert_eq!(response["ok"]["type"], "Edited", "unexpected: {response}");
    assert_eq!(response["ok"]["value"]["type"], "Created");
    assert!(response["ok"]["value"]["id"].is_u64());
    assert_eq!(response["revision"], 2, "AddTrack then AddGenerated");

    let clips = lane_clips(session, "sticker");
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0]["kind"], "solid");
    assert_eq!(clips[0]["length_seconds"], 2.0);
    assert_eq!(clips[0]["rgba"], serde_json::json!([255, 0, 0, 255]));
    unsafe {
        assert_eq!(cutlass_session_duration_seconds(session), 2.0);
        assert!(cutlass_session_is_dirty(session));
        assert!(cutlass_session_can_undo(session));

        assert!(cutlass_session_undo(session));
    }
    assert_eq!(lane_clips(session, "sticker").len(), 0);
    unsafe {
        assert!(cutlass_session_can_redo(session));
        assert!(cutlass_session_redo(session));
    }
    assert_eq!(lane_clips(session, "sticker").len(), 1);

    // A rejected command reports a stable error kind and bumps nothing.
    let bad = apply(
        session,
        serde_json::json!({"type": "RemoveClip", "clip": 999}),
    );
    assert_eq!(bad["err"]["kind"], "model");
    let malformed = apply(session, serde_json::json!({"type": "NoSuchCommand"}));
    assert_eq!(malformed["err"]["kind"], "protocol");

    unsafe { cutlass_session_close(session) };
}

#[test]
fn intents_group_into_one_undo_step() {
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());

    // add_text creates a text lane *and* the clip — two commands, one intent.
    let response = intent(
        session,
        serde_json::json!({"intent": "add_text", "text": "Hello", "at_seconds": 0.0}),
    );
    assert!(response["ok"]["clip"].is_u64(), "unexpected: {response}");

    let all = lanes(session);
    let text_lane = all.iter().find(|l| l["kind"] == "text").expect("text lane");
    let clips = text_lane["clips"].as_array().expect("clips");
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0]["text"], "Hello");
    assert_eq!(clips[0]["length_seconds"], 3.0);

    // One undo removes the whole gesture (lane + clip).
    unsafe { assert!(cutlass_session_undo(session)) };
    assert!(
        lanes(session).iter().all(|l| l["kind"] != "text"),
        "undo should remove the text lane too"
    );

    // An invalid intent reports through the same envelope.
    let bad = intent(
        session,
        serde_json::json!({"intent": "split", "clip": 12345, "seconds": 0.5}),
    );
    assert!(bad["err"]["kind"].is_string());

    unsafe { cutlass_session_close(session) };
}

/// Export a 2-second solid mp4 the intents can import as real media.
fn temp_media_clip() -> std::path::PathBuf {
    use cutlass_models::{Generator, Project, Rational, TimeRange, TrackKind};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("cutlass_session_media_{nanos}.mp4"));

    let fps = Rational::FPS_30;
    let mut project = Project::new("fixture", fps);
    let track = project.add_track(TrackKind::Sticker, "BG");
    project
        .add_generated(
            track,
            Generator::SolidColor {
                rgba: [40, 80, 160, 255],
            },
            TimeRange::at_rate(0, 60, fps),
        )
        .unwrap();
    let mut renderer = cutlass_render::Renderer::new_headless().expect("headless renderer");
    cutlass_render::export_to_file(&mut renderer, &project, &path).expect("export fixture");
    path
}

/// Whether this host has a native encoder/decoder. Only Apple/Windows/Android
/// do; Linux CI has none, so tests that build and import a real media clip skip
/// there. The solid/text/undo/save tests use no media and run everywhere.
fn native_av() -> bool {
    cfg!(any(
        target_vendor = "apple",
        target_os = "windows",
        target_os = "android"
    ))
}

#[test]
fn lifting_a_main_clip_to_a_lane_closes_the_hole() {
    if !native_av() {
        return; // no native encoder/decoder for the media fixture (e.g. Linux CI)
    }
    let media = temp_media_clip();
    let media_str = media.to_str().expect("utf8 path");

    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());

    // Two 2s clips back to back on main.
    let appended = intent(
        session,
        serde_json::json!({"intent": "append_main", "paths": [media_str, media_str]}),
    );
    let clips = appended["ok"]["clips"].as_array().expect("clips").clone();
    assert_eq!(clips.len(), 2, "unexpected: {appended}");
    let first = clips[0].as_u64().expect("clip id");

    // Lift the first clip onto an overlay lane at t=3.
    let moved = intent(
        session,
        serde_json::json!({"intent": "move_lane", "clip": first, "start_seconds": 3.0}),
    );
    assert!(moved["ok"]["track"].is_u64(), "unexpected: {moved}");

    let all = lanes(session);
    let main = all.iter().find(|l| l["is_main"] == true).expect("main");
    let main_clips = main["clips"].as_array().expect("clips");
    assert_eq!(main_clips.len(), 1);
    assert_eq!(
        main_clips[0]["start_seconds"], 0.0,
        "the survivor slides left into the hole"
    );
    let overlay = all
        .iter()
        .find(|l| l["kind"] == "video" && l["is_main"] == false)
        .expect("overlay video lane");
    assert_eq!(overlay["clips"][0]["start_seconds"], 3.0);

    // The lift (move + ripple close) is one undo step.
    unsafe { assert!(cutlass_session_undo(session)) };
    let all = lanes(session);
    let main = all.iter().find(|l| l["is_main"] == true).expect("main");
    let main_clips = main["clips"].as_array().expect("clips");
    assert_eq!(main_clips.len(), 2);
    assert_eq!(main_clips[0]["start_seconds"], 0.0);
    assert_eq!(main_clips[1]["start_seconds"], 2.0);

    unsafe { cutlass_session_close(session) };
    let _ = std::fs::remove_file(&media);
}

#[test]
fn freeze_splits_the_clip_around_a_three_second_still() {
    if !native_av() {
        return; // no native encoder/decoder for the media fixture (e.g. Linux CI)
    }
    let media = temp_media_clip();
    let media_str = media.to_str().expect("utf8 path");
    let dir = tempfile::tempdir().expect("tempdir");
    let png = dir.path().join("freeze.png");
    let png_str = png.to_str().expect("utf8 path");

    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());

    let appended = intent(
        session,
        serde_json::json!({"intent": "append_main", "paths": [media_str]}),
    );
    let clip = appended["ok"]["clips"][0].as_u64().expect("clip id");

    // Freeze mid-clip: split at 1s, drop a 3s still in the gap.
    let frozen = intent(
        session,
        serde_json::json!({"intent": "freeze", "clip": clip, "seconds": 1.0, "png_path": png_str}),
    );
    assert!(frozen["ok"]["clip"].is_u64(), "unexpected: {frozen}");
    assert!(frozen["ok"]["split"].is_u64(), "mid-clip freeze splits");
    assert!(png.exists(), "the frozen frame lands on disk");

    let main = lane_clips(session, "video");
    assert_eq!(main.len(), 3, "left half + still + right half");
    assert_eq!(main[0]["start_seconds"], 0.0);
    assert_eq!(main[0]["length_seconds"], 1.0);
    assert_eq!(main[1]["kind"], "image");
    assert_eq!(main[1]["is_image"], true);
    assert_eq!(main[1]["start_seconds"], 1.0);
    assert_eq!(main[1]["length_seconds"], 3.0);
    assert_eq!(main[2]["start_seconds"], 4.0);
    assert_eq!(main[2]["length_seconds"], 1.0);

    // The whole choreography is one undo step.
    unsafe { assert!(cutlass_session_undo(session)) };
    let main = lane_clips(session, "video");
    assert_eq!(main.len(), 1);
    assert_eq!(main[0]["length_seconds"], 2.0);

    unsafe { cutlass_session_close(session) };
    let _ = std::fs::remove_file(&media);
}

#[test]
fn freeze_at_the_clip_edge_inserts_without_splitting() {
    if !native_av() {
        return; // no native encoder/decoder for the media fixture (e.g. Linux CI)
    }
    let media = temp_media_clip();
    let media_str = media.to_str().expect("utf8 path");
    let dir = tempfile::tempdir().expect("tempdir");
    let png = dir.path().join("edge.png");
    let png_str = png.to_str().expect("utf8 path");

    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());

    let appended = intent(
        session,
        serde_json::json!({"intent": "append_main", "paths": [media_str]}),
    );
    let clip = appended["ok"]["clips"][0].as_u64().expect("clip id");

    let frozen = intent(
        session,
        serde_json::json!({
            "intent": "freeze", "clip": clip, "seconds": 0.0,
            "png_path": png_str, "duration_seconds": 1.0,
        }),
    );
    assert!(frozen["ok"]["split"].is_null(), "edge freeze never splits");

    let main = lane_clips(session, "video");
    assert_eq!(main.len(), 2, "still slots in ahead of the clip");
    assert_eq!(main[0]["kind"], "image");
    assert_eq!(main[0]["length_seconds"], 1.0);
    assert_eq!(main[1]["start_seconds"], 1.0);
    assert_eq!(main[1]["length_seconds"], 2.0);

    unsafe { cutlass_session_close(session) };
    let _ = std::fs::remove_file(&media);
}

#[test]
fn save_then_open_restores_the_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("roundtrip.cutlass");
    let path_str = path.to_str().expect("utf8 path");

    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());
    add_solid(session);

    let saved = apply(
        session,
        serde_json::json!({"type": "Save", "path": path_str}),
    );
    assert_eq!(saved["ok"]["type"], "Saved", "unexpected: {saved}");
    unsafe {
        assert!(!cutlass_session_is_dirty(session), "save clears dirty");
    }
    let before = lanes(session);
    unsafe { cutlass_session_close(session) };

    let reopened = unsafe { cutlass_session_open(path_str.as_ptr(), path_str.len()) };
    assert!(!reopened.is_null(), "open should succeed");
    assert_eq!(
        lanes(reopened),
        before,
        "lanes survive a save/open roundtrip"
    );
    unsafe {
        assert!(!cutlass_session_is_dirty(reopened));
        cutlass_session_close(reopened);
    }
}

#[test]
fn render_fit_returns_scaled_pixels() {
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());
    add_solid(session);

    let image = unsafe { cutlass_session_render_fit(session, 1.0, 240, 240) };
    assert!(!image.data.is_null(), "render should produce pixels");
    assert!(image.width <= 240 && image.height <= 240);
    assert_eq!(image.len, (image.width * image.height * 4) as usize);
    // SAFETY: data/len describe a live buffer we own until image_free.
    let pixels = unsafe { std::slice::from_raw_parts(image.data, image.len) };
    let center = ((image.height / 2 * image.width + image.width / 2) * 4) as usize;
    assert_eq!(
        &pixels[center..center + 4],
        &[255, 0, 0, 255],
        "center of the canvas is the red solid"
    );
    unsafe { cutlass_mobile::cutlass_image_free(image) };

    // Out-of-range times clamp instead of failing.
    let clamped = unsafe { cutlass_session_render_fit(session, 99.0, 64, 64) };
    assert!(!clamped.data.is_null());
    unsafe { cutlass_mobile::cutlass_image_free(clamped) };

    unsafe { cutlass_session_close(session) };
}
