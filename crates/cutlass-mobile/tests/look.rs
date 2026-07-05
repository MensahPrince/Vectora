//! Host-side tests for the Phase I look surface: catalogs over FFI, look
//! commands through the wire, and the speed-preset / audio-role intents —
//! ending in the `ui_state` fields the panels read back.

use std::ffi::CStr;
use std::io::Write;
use std::os::raw::c_char;

use cutlass_mobile::catalogs::cutlass_catalogs;
use cutlass_mobile::wire::cutlass_string_free;
use cutlass_mobile::{
    CutlassSession,
    session::{
        cutlass_session_apply, cutlass_session_close, cutlass_session_intent, cutlass_session_new,
        cutlass_session_ui_state, cutlass_session_undo,
    },
};

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

/// The first clip of the first lane with `kind`.
fn first_clip(session: *mut CutlassSession, kind: &str) -> serde_json::Value {
    ui_state(session)["lanes"]
        .as_array()
        .expect("lanes")
        .iter()
        .find(|l| l["kind"] == kind)
        .map(|l| l["clips"][0].clone())
        .expect("lane with kind")
}

/// Export a 2-second solid mp4 the intents can import as real (video) media.
fn temp_media_clip() -> std::path::PathBuf {
    use cutlass_models::{Generator, Project, Rational, TimeRange, TrackKind};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("cutlass_look_media_{nanos}.mp4"));

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

/// A 2-second stereo PCM16 WAV (constant samples) for audio-lane intents.
fn temp_wav() -> std::path::PathBuf {
    const RATE: u32 = 48_000;
    const FRAMES: u32 = 2 * RATE;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("cutlass_look_audio_{nanos}.wav"));

    let data_len = FRAMES * 4;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    let sample = (16384i16).to_le_bytes();
    for _ in 0..FRAMES * 2 {
        bytes.extend_from_slice(&sample);
    }
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&bytes))
        .expect("write wav fixture");
    path
}

/// Whether this host has a native encoder/decoder for the media fixtures. Only
/// Apple/Windows/Android do; Linux CI has none, so tests that import real
/// audio/video skip there. The catalog test needs no media and runs everywhere.
fn native_av() -> bool {
    cfg!(any(
        target_vendor = "apple",
        target_os = "windows",
        target_os = "android"
    ))
}

#[test]
fn catalogs_ffi_returns_every_vocabulary() {
    let doc = take_json(cutlass_catalogs());
    for key in [
        "effects",
        "transitions",
        "masks",
        "filters",
        "animations",
        "text_effects",
        "speed_presets",
        "stabilize_levels",
        "audio_roles",
    ] {
        assert!(
            doc[key].as_array().is_some_and(|l| !l.is_empty()),
            "{key} missing or empty: {doc}"
        );
    }
    assert!(doc["effects"][0]["params"].is_array());
    assert_eq!(doc["masks"][0]["id"], "linear");
}

#[test]
fn look_commands_surface_in_ui_state() {
    if !native_av() {
        return; // no native encoder/decoder for the media fixture (e.g. Linux CI)
    }
    let media = temp_media_clip();
    let media_str = media.to_str().expect("utf8 path");
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());

    let appended = intent(
        session,
        serde_json::json!({"intent": "append_main", "paths": [media_str]}),
    );
    let clip = appended["ok"]["clips"][0].as_u64().expect("clip id");

    // Untouched clips carry none of the look fields on the wire.
    let before = first_clip(session, "video");
    for key in ["mask", "chroma_key", "stabilize", "filter", "adjust"] {
        assert!(before.get(key).is_none(), "{key} unexpectedly present");
    }

    for command in [
        serde_json::json!({"type": "SetClipMask", "clip": clip, "mask": {"kind": "circle"}}),
        serde_json::json!({"type": "SetClipChroma", "clip": clip,
            "chroma": {"rgb": [0, 255, 0], "strength": 0.5}}),
        serde_json::json!({"type": "SetClipStabilize", "clip": clip, "stabilize": "smooth"}),
        serde_json::json!({"type": "SetClipFilter", "clip": clip,
            "filter": {"id": "noir", "intensity": 0.5}}),
        serde_json::json!({"type": "SetClipAdjustments", "clip": clip,
            "adjust": {"brightness": 0.25}}),
        serde_json::json!({"type": "SetClipAnimation", "clip": clip,
            "slot": "in", "animation": {"id": "zoom_in"}}),
    ] {
        let response = apply(session, command);
        assert!(response["ok"].is_object(), "unexpected: {response}");
    }

    let styled = first_clip(session, "video");
    assert_eq!(styled["mask"]["kind"], "circle");
    assert_eq!(styled["chroma_key"]["rgb"][1], 255);
    assert_eq!(styled["stabilize"], "smooth");
    assert_eq!(styled["filter"]["id"], "noir");
    assert_eq!(styled["adjust"]["brightness"], 0.25);
    assert_eq!(styled["animation_in"], "zoom_in");

    // Each look edit is one undo step.
    unsafe { assert!(cutlass_session_undo(session)) };
    let clip_state = first_clip(session, "video");
    assert!(clip_state.get("animation_in").is_none());
    assert_eq!(clip_state["filter"]["id"], "noir");

    unsafe { cutlass_session_close(session) };
    let _ = std::fs::remove_file(&media);
}

#[test]
fn speed_preset_intent_applies_and_clears_catalog_curves() {
    if !native_av() {
        return; // no native encoder/decoder for the media fixture (e.g. Linux CI)
    }
    let media = temp_media_clip();
    let media_str = media.to_str().expect("utf8 path");
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());

    let appended = intent(
        session,
        serde_json::json!({"intent": "append_main", "paths": [media_str]}),
    );
    let clip = appended["ok"]["clips"][0].as_u64().expect("clip id");
    assert!(first_clip(session, "video").get("speed_preset").is_none());

    let response = intent(
        session,
        serde_json::json!({"intent": "set_speed_preset", "clip": clip, "preset": "ramp_up"}),
    );
    assert!(response["ok"].is_object(), "unexpected: {response}");
    assert_eq!(first_clip(session, "video")["speed_preset"], "ramp_up");

    // Clearing restores constant speed; unknown ids bounce with a clear error.
    let cleared = intent(
        session,
        serde_json::json!({"intent": "set_speed_preset", "clip": clip}),
    );
    assert!(cleared["ok"].is_object(), "unexpected: {cleared}");
    assert!(first_clip(session, "video").get("speed_preset").is_none());

    let unknown = intent(
        session,
        serde_json::json!({"intent": "set_speed_preset", "clip": clip, "preset": "warp9"}),
    );
    assert!(
        unknown["err"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("warp9")),
        "unexpected: {unknown}"
    );

    unsafe { cutlass_session_close(session) };
    let _ = std::fs::remove_file(&media);
}

#[test]
fn add_audio_role_lands_on_the_lane_clip() {
    if !native_av() {
        return; // no native decoder to import the WAV (e.g. Linux CI)
    }
    let wav = temp_wav();
    let wav_str = wav.to_str().expect("utf8 path");
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());

    let added = intent(
        session,
        serde_json::json!({
            "intent": "add_audio", "path": wav_str, "at_seconds": 0.0, "role": "music",
        }),
    );
    assert!(added["ok"]["clip"].is_u64(), "unexpected: {added}");
    assert_eq!(first_clip(session, "audio")["audio_role"], "music");

    // The import + place + tag is one undo step.
    unsafe { assert!(cutlass_session_undo(session)) };
    let lanes = ui_state(session)["lanes"]
        .as_array()
        .expect("lanes")
        .clone();
    assert!(!lanes.iter().any(|l| l["kind"] == "audio"));

    unsafe { cutlass_session_close(session) };
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn duplicate_carries_the_look_onto_the_copy() {
    if !native_av() {
        return; // no native encoder/decoder for the media fixture (e.g. Linux CI)
    }
    let media = temp_media_clip();
    let media_str = media.to_str().expect("utf8 path");
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());

    let appended = intent(
        session,
        serde_json::json!({"intent": "append_main", "paths": [media_str]}),
    );
    let clip = appended["ok"]["clips"][0].as_u64().expect("clip id");

    for command in [
        serde_json::json!({"type": "SetClipFilter", "clip": clip,
            "filter": {"id": "vivid", "intensity": 0.5}}),
        serde_json::json!({"type": "SetClipAdjustments", "clip": clip,
            "adjust": {"contrast": -0.25}}),
        serde_json::json!({"type": "SetClipAnimation", "clip": clip,
            "slot": "combo", "animation": {"id": "pulse"}}),
    ] {
        let response = apply(session, command);
        assert!(response["ok"].is_object(), "unexpected: {response}");
    }

    let duplicated = intent(
        session,
        serde_json::json!({"intent": "duplicate", "clip": clip}),
    );
    assert!(
        duplicated["ok"]["clip"].is_u64(),
        "unexpected: {duplicated}"
    );

    let state = ui_state(session);
    let main = state["lanes"]
        .as_array()
        .expect("lanes")
        .iter()
        .find(|l| l["is_main"] == true)
        .expect("main lane")
        .clone();
    let clips = main["clips"].as_array().expect("clips");
    assert_eq!(clips.len(), 2);
    for c in clips {
        assert_eq!(c["filter"]["id"], "vivid", "copy lost the look: {c}");
        assert_eq!(c["adjust"]["contrast"], -0.25);
        assert_eq!(c["animation_combo"], "pulse");
    }

    unsafe { cutlass_session_close(session) };
    let _ = std::fs::remove_file(&media);
}
