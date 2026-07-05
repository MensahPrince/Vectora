//! Host-side tests for the realtime audio FFI: open a mixed reader on a
//! session snapshot, pull PCM blocks, and check gain/edges — the same calls
//! the Swift feeder task makes.

use std::ffi::CStr;
use std::io::Write;
use std::os::raw::c_char;

use cutlass_mobile::audio::{
    cutlass_audio_channels, cutlass_audio_close, cutlass_audio_rate, cutlass_audio_read,
    cutlass_session_audio_open,
};
use cutlass_mobile::wire::cutlass_string_free;
use cutlass_mobile::{
    CutlassSession,
    session::{cutlass_session_close, cutlass_session_intent, cutlass_session_new},
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

fn intent(session: *mut CutlassSession, intent: serde_json::Value) -> serde_json::Value {
    let json = intent.to_string();
    let response = unsafe { cutlass_session_intent(session, json.as_ptr(), json.len()) };
    take_json(response)
}

/// Write a 2-second stereo PCM16 WAV at 48 kHz with constant samples:
/// left = +0.5, right = −0.25 — flat known values every read can assert on.
fn temp_wav() -> std::path::PathBuf {
    const RATE: u32 = 48_000;
    const FRAMES: u32 = 2 * RATE;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("cutlass_audio_fixture_{nanos}.wav"));

    let data_len = FRAMES * 4; // 2 channels × 2 bytes
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes()); // stereo
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 4).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&4u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    let left = (16384i16).to_le_bytes(); // +0.5
    let right = (-8192i16).to_le_bytes(); // −0.25
    for _ in 0..FRAMES {
        bytes.extend_from_slice(&left);
        bytes.extend_from_slice(&right);
    }
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&bytes))
        .expect("write wav fixture");
    path
}

/// Pull `frames` sample frames, asserting the reader delivered all of them.
fn read_all(reader: *mut cutlass_mobile::CutlassAudioReader, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    let got = unsafe { cutlass_audio_read(reader, out.as_mut_ptr(), frames) };
    assert_eq!(got, frames as isize);
    out
}

fn assert_stereo_levels(block: &[f32], left: f32, right: f32, tolerance: f32) {
    for pair in block.chunks_exact(2) {
        assert!(
            (pair[0] - left).abs() <= tolerance,
            "left sample {} != {left}",
            pair[0]
        );
        assert!(
            (pair[1] - right).abs() <= tolerance,
            "right sample {} != {right}",
            pair[1]
        );
    }
}

/// Whether this host decodes media with a native backend. Only Apple/Windows/
/// Android ship one; Linux CI has none, so the tests that import a real audio
/// file skip there instead of asserting on an empty decoder — they still run on
/// every platform the Swift/Kotlin shells actually target.
fn native_av() -> bool {
    cfg!(any(
        target_vendor = "apple",
        target_os = "windows",
        target_os = "android"
    ))
}

#[test]
fn format_constants_match_the_mixer() {
    assert_eq!(cutlass_audio_rate(), 48_000);
    assert_eq!(cutlass_audio_channels(), 2);
}

#[test]
fn null_handles_are_safe() {
    let mut out = [0.0f32; 8];
    assert_eq!(
        unsafe { cutlass_audio_read(std::ptr::null_mut(), out.as_mut_ptr(), 4) },
        -1
    );
    unsafe { cutlass_audio_close(std::ptr::null_mut()) };
    assert!(unsafe { cutlass_session_audio_open(std::ptr::null_mut(), 0.0) }.is_null());
}

#[test]
fn a_silent_timeline_opens_no_reader() {
    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());
    assert!(unsafe { cutlass_session_audio_open(session, 0.0) }.is_null());
    unsafe { cutlass_session_close(session) };
}

#[test]
fn reads_mixed_pcm_and_snapshots_the_project() {
    if !native_av() {
        return; // no native decoder to import the WAV (e.g. Linux CI)
    }
    let wav = temp_wav();
    let wav_str = wav.to_str().expect("utf8 path");

    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());
    let added = intent(
        session,
        serde_json::json!({"intent": "add_audio", "path": wav_str, "at_seconds": 0.0}),
    );
    let clip = added["ok"]["clip"].as_u64().expect("clip id");

    // Mid-span block at unity volume: the flat source values come through.
    let reader = unsafe { cutlass_session_audio_open(session, 0.5) };
    assert!(!reader.is_null());
    let block = read_all(reader, 4800);
    assert_stereo_levels(&block, 0.5, -0.25, 0.02);

    // Halving the clip volume must not touch the open reader (snapshot) …
    let set = intent(
        session,
        serde_json::json!({"intent": "set_audio", "clip": clip, "volume": 0.5}),
    );
    assert!(set["ok"].is_object(), "unexpected: {set}");
    let block = read_all(reader, 4800);
    assert_stereo_levels(&block, 0.5, -0.25, 0.02);
    unsafe { cutlass_audio_close(reader) };

    // … while a reader reopened after the edit mixes at the new gain.
    let reader = unsafe { cutlass_session_audio_open(session, 0.5) };
    assert!(!reader.is_null());
    let block = read_all(reader, 4800);
    assert_stereo_levels(&block, 0.25, -0.125, 0.02);
    unsafe { cutlass_audio_close(reader) };

    unsafe { cutlass_session_close(session) };
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn reads_stop_at_the_timeline_end() {
    if !native_av() {
        return; // no native decoder to import the WAV (e.g. Linux CI)
    }
    let wav = temp_wav();
    let wav_str = wav.to_str().expect("utf8 path");

    let session = cutlass_session_new(30, 1);
    assert!(!session.is_null());
    intent(
        session,
        serde_json::json!({"intent": "add_audio", "path": wav_str, "at_seconds": 0.0}),
    );

    // Open 0.1s before the 2s end: one short read drains the tail, then EOS.
    let reader = unsafe { cutlass_session_audio_open(session, 1.9) };
    assert!(!reader.is_null());
    let mut out = vec![0.0f32; 9600 * 2];
    let got = unsafe { cutlass_audio_read(reader, out.as_mut_ptr(), 9600) };
    assert_eq!(got, 4800, "only the remaining 0.1s is delivered");
    let got = unsafe { cutlass_audio_read(reader, out.as_mut_ptr(), 9600) };
    assert_eq!(got, 0, "end of timeline reads as EOS");
    unsafe { cutlass_audio_close(reader) };

    unsafe { cutlass_session_close(session) };
    let _ = std::fs::remove_file(&wav);
}
