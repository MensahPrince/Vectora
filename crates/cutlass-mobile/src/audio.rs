//! Realtime audio playback over the C ABI.
//!
//! Preview playback pulls PCM from the same streamed mixer export uses
//! ([`ExportAudioMixer`]): open a reader on a **project snapshot**, then pull
//! interleaved stereo `f32` blocks at 48 kHz from a feeder thread. Decode
//! never runs on the audio realtime thread — the shell buffers into a ring
//! and its render callback only copies.
//!
//! Edits don't touch a live reader (it owns a snapshot); the shell watches
//! the session revision and reopens the reader at the current playhead when
//! it changes, same as seeking.
//!
//! Handle lifecycle: [`cutlass_session_audio_open`] → [`cutlass_audio_read`]
//! from one thread at a time → [`cutlass_audio_close`]. Readers are
//! independent of each other and of the session after open.

use cutlass_render::{EXPORT_AUDIO_CHANNELS, EXPORT_AUDIO_RATE, ExportAudioMixer};

use crate::session::CutlassSession;

/// A pull reader of the timeline's mixed audio. Free with
/// [`cutlass_audio_close`].
pub struct CutlassAudioReader {
    mixer: ExportAudioMixer,
    /// Next sample frame [`cutlass_audio_read`] will deliver.
    position: i64,
    /// Timeline end in sample frames — reads stop here (end of stream).
    end: i64,
    /// A decode failure is sticky: every later read reports it too.
    failed: bool,
}

/// Sample rate of every [`cutlass_audio_read`] block, in Hz.
#[unsafe(no_mangle)]
pub extern "C" fn cutlass_audio_rate() -> u32 {
    EXPORT_AUDIO_RATE
}

/// Channels per sample frame (interleaved) in every read block.
#[unsafe(no_mangle)]
pub extern "C" fn cutlass_audio_channels() -> u32 {
    EXPORT_AUDIO_CHANNELS as u32
}

/// Open a mixed-audio reader on a snapshot of the session's current project,
/// positioned at `start_seconds` (clamped into the timeline). Later edits
/// never affect it — reopen on seek or revision change.
///
/// Null when the session is null or the timeline has no audible clips (no
/// point spinning an audio engine for silence).
///
/// # Safety
/// `handle` must be null or a live session used non-concurrently during this
/// call. The returned reader is independent afterwards; use it from one
/// thread at a time and free it with [`cutlass_audio_close`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_session_audio_open(
    handle: *mut CutlassSession,
    start_seconds: f64,
) -> *mut CutlassAudioReader {
    // SAFETY: caller contract.
    let Some(session) = (unsafe { handle.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let project = session.engine().project().clone();
    let Some(mixer) = ExportAudioMixer::for_project(&project) else {
        return std::ptr::null_mut();
    };
    let end = (project.timeline().duration().seconds() * f64::from(EXPORT_AUDIO_RATE)) as i64;
    let position = ((start_seconds.max(0.0) * f64::from(EXPORT_AUDIO_RATE)) as i64).min(end);
    Box::into_raw(Box::new(CutlassAudioReader {
        mixer,
        position,
        end,
        failed: false,
    }))
}

/// Pull up to `max_frames` interleaved stereo sample frames into `out`,
/// advancing the reader. Returns the number of sample frames written:
/// `0` at the end of the timeline, `-1` after a decode failure (sticky).
/// `out` must hold `max_frames * 2` floats.
///
/// Blocks while sources decode — call from a feeder thread, never from the
/// audio render callback.
///
/// # Safety
/// `handle` must be null or a live reader used non-concurrently; `out` must
/// point to `max_frames * 2` writable floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_audio_read(
    handle: *mut CutlassAudioReader,
    out: *mut f32,
    max_frames: usize,
) -> isize {
    // SAFETY: caller contract.
    let Some(reader) = (unsafe { handle.as_mut() }) else {
        return -1;
    };
    if reader.failed {
        return -1;
    }
    if out.is_null() || max_frames == 0 {
        return 0;
    }
    let frames = (max_frames as i64).min(reader.end - reader.position).max(0) as usize;
    if frames == 0 {
        return 0;
    }
    let channels = EXPORT_AUDIO_CHANNELS as usize;
    // SAFETY: caller guarantees `max_frames * 2` floats; `frames <= max_frames`.
    let block = unsafe { std::slice::from_raw_parts_mut(out, frames * channels) };
    match reader.mixer.mix_into(reader.position, block) {
        Ok(()) => {
            reader.position += frames as i64;
            frames as isize
        }
        Err(e) => {
            log::error!("audio reader: mix failed at frame {}: {e}", reader.position);
            reader.failed = true;
            -1
        }
    }
}

/// Release a reader handle. Null is a no-op.
///
/// # Safety
/// `handle` must be null or a live reader pointer, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_audio_close(handle: *mut CutlassAudioReader) {
    if !handle.is_null() {
        // SAFETY: came from Box::into_raw, freed exactly once.
        drop(unsafe { Box::from_raw(handle) });
    }
}
