//! JNI mirrors of the editing-session C ABI (Android front door).
//!
//! Every function marshals Java types to the exact same Rust entry points the
//! iOS shell calls — strings in/out for JSON, `[width, height, argb…]` int
//! arrays for pixels, `jlong` for opaque handles — so Kotlin and Swift ride
//! one engine surface. Threading contracts match the C header: serialize all
//! calls on one session handle; export jobs/thumbnailers/audio readers are
//! independent.
//!
//! Class: `com.scytheralpha.cutlass_android.CutlassNative`.

use jni::JNIEnv;
use jni::objects::{JClass, JFloatArray, JString};
use jni::sys::{jboolean, jdouble, jint, jintArray, jlong, jstring};

use crate::audio::{
    CutlassAudioReader, cutlass_audio_channels, cutlass_audio_close, cutlass_audio_rate,
    cutlass_audio_read, cutlass_session_audio_open,
};
use crate::catalogs::cutlass_catalogs;
use crate::export_job::{
    CutlassExportJob, cutlass_export_cancel, cutlass_export_finished, cutlass_export_free,
    cutlass_export_join, cutlass_export_progress, cutlass_export_start,
};
use crate::session::{
    CutlassSession, cutlass_session_apply, cutlass_session_begin_group, cutlass_session_can_redo,
    cutlass_session_can_undo, cutlass_session_close, cutlass_session_commit_group,
    cutlass_session_duration_seconds, cutlass_session_intent, cutlass_session_is_dirty,
    cutlass_session_new, cutlass_session_open, cutlass_session_redo, cutlass_session_render_fit,
    cutlass_session_revision, cutlass_session_rollback_group, cutlass_session_ui_state,
    cutlass_session_undo,
};
use crate::thumbs::{
    CutlassThumbnailer, cutlass_thumbnailer_close, cutlass_thumbnailer_duration_seconds,
    cutlass_thumbnailer_open, cutlass_thumbnailer_thumb,
};
use crate::wire::cutlass_string_free;
use crate::{CutlassImage, cutlass_image_free};

/// Take ownership of a C string from the FFI, hand it to Java, and free it.
/// Null becomes a null `jstring` (calls that can fail return nullable JSON).
fn take_string(env: &JNIEnv, ptr: *mut std::ffi::c_char) -> jstring {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `ptr` came from a `cutlass_*` string API this crate allocated.
    let text = unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { cutlass_string_free(ptr) };
    match env.new_string(text) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Read a Java string argument (empty on conversion failure — the callee then
/// answers with its usual invalid-arg response).
fn string_arg(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(String::from).unwrap_or_default()
}

fn empty_int_array(env: &JNIEnv) -> jintArray {
    env.new_int_array(0)
        .map(|a| a.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Consume a [`CutlassImage`] into `[width, height, argb…]` (ARGB_8888, the
/// `Bitmap.createBitmap` layout) and free the Rust buffer.
fn take_image(env: &JNIEnv, image: CutlassImage) -> jintArray {
    if image.data.is_null() || image.len == 0 {
        return empty_int_array(env);
    }
    // SAFETY: non-null `data`/`len` came from `CutlassImage::from_rgba`.
    let pixels = unsafe { std::slice::from_raw_parts(image.data, image.len) };
    let count = (image.width as usize).saturating_mul(image.height as usize);
    let mut argb = vec![0i32; count + 2];
    argb[0] = image.width as i32;
    argb[1] = image.height as i32;
    for (slot, px) in argb[2..].iter_mut().zip(pixels.chunks_exact(4)) {
        let r = i32::from(px[0]);
        let g = i32::from(px[1]);
        let b = i32::from(px[2]);
        let a = i32::from(px[3]);
        *slot = (a << 24) | (r << 16) | (g << 8) | b;
    }
    unsafe { cutlass_image_free(image) };
    match env.new_int_array(argb.len() as jint) {
        Ok(array) => {
            if env.set_int_array_region(&array, 0, &argb).is_err() {
                return empty_int_array(env);
            }
            array.into_raw()
        }
        Err(_) => empty_int_array(env),
    }
}

// --- Catalogs ---------------------------------------------------------------

/// `CutlassNative.catalogs()`: every preset vocabulary as one JSON document.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_catalogs<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    crate::android_jni::init_logging();
    take_string(&env, cutlass_catalogs())
}

// --- Session lifecycle -------------------------------------------------------

/// `CutlassNative.sessionNew(fpsNum, fpsDen)`: a fresh project session.
/// Returns the handle as a `jlong` (`0` on failure).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionNew<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    fps_num: jint,
    fps_den: jint,
) -> jlong {
    crate::android_jni::init_logging();
    cutlass_session_new(fps_num, fps_den) as jlong
}

/// `CutlassNative.sessionOpen(path)`: open a `.cutlass` project file.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionOpen<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> jlong {
    crate::android_jni::init_logging();
    let path = string_arg(&mut env, &path);
    // SAFETY: the pointer/length pair borrows the live `path` String.
    unsafe { cutlass_session_open(path.as_ptr(), path.len()) as jlong }
}

/// `CutlassNative.sessionClose(handle)`: release a session.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionClose<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    // SAFETY: `handle` came from `sessionNew`/`sessionOpen`, freed once.
    unsafe { cutlass_session_close(handle as *mut CutlassSession) }
}

// --- Commands / intents / state ----------------------------------------------

/// `CutlassNative.sessionApply(handle, json)`: one wire command; the JSON
/// response envelope comes back.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionApply<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    json: JString<'local>,
) -> jstring {
    let json = string_arg(&mut env, &json);
    // SAFETY: session handle per caller contract; bytes borrow `json`.
    let response =
        unsafe { cutlass_session_apply(handle as *mut CutlassSession, json.as_ptr(), json.len()) };
    take_string(&env, response)
}

/// `CutlassNative.sessionIntent(handle, json)`: one gesture intent.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionIntent<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    json: JString<'local>,
) -> jstring {
    let json = string_arg(&mut env, &json);
    // SAFETY: same contract as `sessionApply`.
    let response =
        unsafe { cutlass_session_intent(handle as *mut CutlassSession, json.as_ptr(), json.len()) };
    take_string(&env, response)
}

/// `CutlassNative.sessionUiState(handle)`: the lane-stack JSON (null for a
/// null handle).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionUiState<
    'local,
>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jstring {
    // SAFETY: session handle per caller contract.
    take_string(&env, unsafe {
        cutlass_session_ui_state(handle as *mut CutlassSession)
    })
}

/// `CutlassNative.sessionUndo(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionUndo<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    // SAFETY: session handle per caller contract.
    (unsafe { cutlass_session_undo(handle as *mut CutlassSession) }) as jboolean
}

/// `CutlassNative.sessionRedo(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionRedo<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    // SAFETY: session handle per caller contract.
    (unsafe { cutlass_session_redo(handle as *mut CutlassSession) }) as jboolean
}

/// `CutlassNative.sessionCanUndo(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionCanUndo<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    // SAFETY: session handle per caller contract.
    (unsafe { cutlass_session_can_undo(handle as *mut CutlassSession) }) as jboolean
}

/// `CutlassNative.sessionCanRedo(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionCanRedo<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    // SAFETY: session handle per caller contract.
    (unsafe { cutlass_session_can_redo(handle as *mut CutlassSession) }) as jboolean
}

/// `CutlassNative.sessionRevision(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionRevision<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jlong {
    // SAFETY: session handle per caller contract.
    (unsafe { cutlass_session_revision(handle as *mut CutlassSession) }) as jlong
}

/// `CutlassNative.sessionIsDirty(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionIsDirty<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jboolean {
    // SAFETY: session handle per caller contract.
    (unsafe { cutlass_session_is_dirty(handle as *mut CutlassSession) }) as jboolean
}

/// `CutlassNative.sessionBeginGroup(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionBeginGroup<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    // SAFETY: session handle per caller contract.
    unsafe { cutlass_session_begin_group(handle as *mut CutlassSession) }
}

/// `CutlassNative.sessionCommitGroup(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionCommitGroup<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    // SAFETY: session handle per caller contract.
    unsafe { cutlass_session_commit_group(handle as *mut CutlassSession) }
}

/// `CutlassNative.sessionRollbackGroup(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionRollbackGroup<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    // SAFETY: session handle per caller contract.
    unsafe { cutlass_session_rollback_group(handle as *mut CutlassSession) }
}

/// `CutlassNative.sessionDurationSeconds(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionDurationSeconds<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jdouble {
    // SAFETY: session handle per caller contract.
    unsafe { cutlass_session_duration_seconds(handle as *mut CutlassSession) }
}

/// `CutlassNative.sessionRenderFit(handle, seconds, maxWidth, maxHeight)`:
/// the timeline frame nearest `seconds` as `[width, height, argb…]`. Empty
/// array on failure.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionRenderFit<
    'local,
>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    seconds: jdouble,
    max_width: jint,
    max_height: jint,
) -> jintArray {
    // SAFETY: session handle per caller contract.
    let image = unsafe {
        cutlass_session_render_fit(
            handle as *mut CutlassSession,
            seconds,
            max_width.max(0) as u32,
            max_height.max(0) as u32,
        )
    };
    take_image(&env, image)
}

// --- Export jobs ---------------------------------------------------------------

/// `CutlassNative.exportStart(session, path, width, height, fpsNum, fpsDen)`:
/// start a background export. Zero size/fps keeps the project's native value.
/// Returns the job handle (`0` on failure).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_exportStart<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    session: jlong,
    path: JString<'local>,
    width: jint,
    height: jint,
    fps_num: jint,
    fps_den: jint,
) -> jlong {
    let path = string_arg(&mut env, &path);
    // SAFETY: session handle per caller contract; bytes borrow `path`.
    unsafe {
        cutlass_export_start(
            session as *mut CutlassSession,
            path.as_ptr(),
            path.len(),
            width.max(0) as u32,
            height.max(0) as u32,
            fps_num,
            fps_den,
        ) as jlong
    }
}

/// `CutlassNative.exportProgress(job)`: fraction written, 0.0–1.0.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_exportProgress<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    job: jlong,
) -> jdouble {
    // SAFETY: job handle per caller contract (progress is poll-safe).
    unsafe { cutlass_export_progress(job as *const CutlassExportJob) }
}

/// `CutlassNative.exportFinished(job)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_exportFinished<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    job: jlong,
) -> jboolean {
    // SAFETY: job handle per caller contract.
    (unsafe { cutlass_export_finished(job as *const CutlassExportJob) }) as jboolean
}

/// `CutlassNative.exportCancel(job)`: stop after the frame in flight.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_exportCancel<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    job: jlong,
) {
    // SAFETY: job handle per caller contract.
    unsafe { cutlass_export_cancel(job as *const CutlassExportJob) }
}

/// `CutlassNative.exportJoin(job)`: block until finished; the JSON verdict.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_exportJoin<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    job: jlong,
) -> jstring {
    // SAFETY: job handle per caller contract.
    take_string(&env, unsafe {
        cutlass_export_join(job as *const CutlassExportJob)
    })
}

/// `CutlassNative.exportFree(job)`: release the job (joins if needed).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_exportFree<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    job: jlong,
) {
    // SAFETY: job handle per caller contract, freed once.
    unsafe { cutlass_export_free(job as *mut CutlassExportJob) }
}

// --- Thumbnails ------------------------------------------------------------------

/// `CutlassNative.thumbnailerOpen(path)`: a filmstrip thumbnailer for one
/// media file. Returns the handle (`0` on failure).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_thumbnailerOpen<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> jlong {
    crate::android_jni::init_logging();
    let path = string_arg(&mut env, &path);
    // SAFETY: bytes borrow the live `path` String.
    unsafe { cutlass_thumbnailer_open(path.as_ptr(), path.len()) as jlong }
}

/// `CutlassNative.thumbnailerDurationSeconds(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_thumbnailerDurationSeconds<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jdouble {
    // SAFETY: thumbnailer handle per caller contract.
    unsafe { cutlass_thumbnailer_duration_seconds(handle as *const CutlassThumbnailer) }
}

/// `CutlassNative.thumbnailerThumb(handle, seconds, maxWidth, maxHeight)`:
/// the frame nearest `seconds` as `[width, height, argb…]`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_thumbnailerThumb<
    'local,
>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    seconds: jdouble,
    max_width: jint,
    max_height: jint,
) -> jintArray {
    // SAFETY: thumbnailer handle per caller contract.
    let image = unsafe {
        cutlass_thumbnailer_thumb(
            handle as *mut CutlassThumbnailer,
            seconds,
            max_width.max(0) as u32,
            max_height.max(0) as u32,
        )
    };
    take_image(&env, image)
}

/// `CutlassNative.thumbnailerClose(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_thumbnailerClose<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    // SAFETY: thumbnailer handle per caller contract, freed once.
    unsafe { cutlass_thumbnailer_close(handle as *mut CutlassThumbnailer) }
}

// --- Realtime audio -----------------------------------------------------------

/// `CutlassNative.audioRate()`: sample rate of every read block (Hz).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_audioRate<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jint {
    cutlass_audio_rate() as jint
}

/// `CutlassNative.audioChannels()`: interleaved channel count.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_audioChannels<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jint {
    cutlass_audio_channels() as jint
}

/// `CutlassNative.sessionAudioOpen(session, startSeconds)`: a mixed-audio
/// pull reader over the current timeline. Returns the handle (`0` when the
/// timeline has no audible clips).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_sessionAudioOpen<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    session: jlong,
    start_seconds: jdouble,
) -> jlong {
    // SAFETY: session handle per caller contract.
    unsafe { cutlass_session_audio_open(session as *mut CutlassSession, start_seconds) as jlong }
}

/// `CutlassNative.audioRead(handle, out, maxFrames)`: pull up to `maxFrames`
/// interleaved stereo frames into `out` (`maxFrames * 2` floats). Returns
/// frames written; `0` at the end of the timeline, `-1` after a decode
/// failure (sticky).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_audioRead<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    out: JFloatArray<'local>,
    max_frames: jint,
) -> jint {
    if handle == 0 || max_frames <= 0 {
        return 0;
    }
    let capacity = env.get_array_length(&out).unwrap_or(0).max(0) as usize;
    let frames = (max_frames as usize).min(capacity / 2);
    if frames == 0 {
        return 0;
    }
    let mut buffer = vec![0f32; frames * 2];
    // SAFETY: reader handle per caller contract; `buffer` holds `frames * 2`
    // floats.
    let written = unsafe {
        cutlass_audio_read(
            handle as *mut CutlassAudioReader,
            buffer.as_mut_ptr(),
            frames,
        )
    };
    if written > 0 {
        let floats = (written as usize) * 2;
        if env
            .set_float_array_region(&out, 0, &buffer[..floats])
            .is_err()
        {
            return -1;
        }
    }
    written as jint
}

/// `CutlassNative.audioClose(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_scytheralpha_cutlass_1android_CutlassNative_audioClose<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    // SAFETY: reader handle per caller contract, freed once.
    unsafe { cutlass_audio_close(handle as *mut CutlassAudioReader) }
}
