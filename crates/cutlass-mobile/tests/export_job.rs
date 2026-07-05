//! Host-side tests for the background export job FFI.
//!
//! These encode through the platform encoder, so they only run where one
//! exists (Apple: H.264/mp4 via AVAssetWriter — exactly what iOS uses).
#![cfg(any(target_os = "macos", target_os = "ios"))]

use std::ffi::CStr;

use cutlass_mobile::CutlassSession;
use cutlass_mobile::export_job::{
    cutlass_export_cancel, cutlass_export_finished, cutlass_export_free, cutlass_export_join,
    cutlass_export_progress, cutlass_export_start,
};
use cutlass_mobile::session::{cutlass_session_apply, cutlass_session_close, cutlass_session_new};
use cutlass_mobile::wire::cutlass_string_free;

fn apply(session: *mut CutlassSession, command: serde_json::Value) -> serde_json::Value {
    let json = command.to_string();
    let ptr = unsafe { cutlass_session_apply(session, json.as_ptr(), json.len()) };
    assert!(!ptr.is_null());
    let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { cutlass_string_free(ptr) };
    serde_json::from_str(&text).unwrap()
}

/// A session holding one `seconds`-long full-canvas solid (media-free).
fn solid_session(seconds: i64) -> *mut CutlassSession {
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());
    let created = apply(
        session,
        serde_json::json!({"type": "AddTrack", "kind": "Sticker", "name": "Solids"}),
    );
    let track = created["ok"]["value"]["id"].as_u64().expect("track id");
    let added = apply(
        session,
        serde_json::json!({
            "type": "AddGenerated",
            "track": track,
            "generator": {"SolidColor": {"rgba": [0, 128, 255, 255]}},
            "timeline": {
                "start": {"value": 0, "rate": {"num": 30, "den": 1}},
                "duration": {"value": seconds * 30, "rate": {"num": 30, "den": 1}},
            },
        }),
    );
    assert!(added["ok"]["value"]["id"].is_u64(), "unexpected: {added}");
    session
}

fn join_verdict(job: *const cutlass_mobile::CutlassExportJob) -> serde_json::Value {
    let ptr = unsafe { cutlass_export_join(job) };
    assert!(!ptr.is_null());
    let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { cutlass_string_free(ptr) };
    serde_json::from_str(&text).unwrap()
}

#[test]
fn export_job_completes_with_progress() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.mp4");
    let path_str = path.to_str().unwrap();

    let session = solid_session(2); // 60 frames
    // Half-size override exercises the sized-export path end to end.
    let job =
        unsafe { cutlass_export_start(session, path_str.as_ptr(), path_str.len(), 640, 360, 0, 0) };
    assert!(!job.is_null());

    let verdict = join_verdict(job);
    assert_eq!(verdict["ok"]["frames"], 60, "unexpected: {verdict}");
    unsafe {
        assert!(cutlass_export_finished(job));
        assert_eq!(cutlass_export_progress(job), 1.0);
    }
    assert!(path.exists(), "finished export leaves the file in place");
    // Joining again returns the cached verdict.
    assert_eq!(join_verdict(job), verdict);

    unsafe {
        cutlass_export_free(job);
        cutlass_session_close(session);
    }
}

#[test]
fn export_job_cancel_stops_and_removes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancelled.mp4");
    let path_str = path.to_str().unwrap();

    // Long enough that cancel lands mid-flight.
    let session = solid_session(60); // 1800 frames
    let job =
        unsafe { cutlass_export_start(session, path_str.as_ptr(), path_str.len(), 0, 0, 0, 0) };
    assert!(!job.is_null());

    unsafe { cutlass_export_cancel(job) };
    let verdict = join_verdict(job);
    assert_eq!(verdict["err"]["kind"], "cancelled", "unexpected: {verdict}");
    assert!(!path.exists(), "cancelled export must clean up its file");

    unsafe {
        cutlass_export_free(job);
        cutlass_session_close(session);
    }
}

#[test]
fn export_null_args_are_safe() {
    let path = b"/tmp/never.mp4";
    let job = unsafe {
        cutlass_export_start(std::ptr::null_mut(), path.as_ptr(), path.len(), 0, 0, 0, 0)
    };
    assert!(job.is_null());

    unsafe {
        assert_eq!(cutlass_export_progress(std::ptr::null()), 0.0);
        assert!(!cutlass_export_finished(std::ptr::null()));
        cutlass_export_cancel(std::ptr::null());
        assert!(cutlass_export_join(std::ptr::null()).is_null());
        cutlass_export_free(std::ptr::null_mut());
    }
}
