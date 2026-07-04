//! Background export jobs over the C ABI.
//!
//! An export must not freeze the editor: the job snapshots the session's
//! project (cheap clone of plain data), spins up a **fresh renderer on its own
//! thread** (separate GPU queue + decoder cache, so preview stays live), and
//! reports progress through atomics the shell polls once a frame. Cancel is a
//! flag checked between frames; a cancelled or failed export deletes its
//! partial output file.
//!
//! Handle lifecycle: [`cutlass_export_start`] → poll
//! [`cutlass_export_progress`] / [`cutlass_export_finished`] (optionally
//! [`cutlass_export_cancel`]) → [`cutlass_export_join`] for the JSON verdict →
//! [`cutlass_export_free`].

use std::ffi::c_char;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use cutlass_models::Project;
use cutlass_render::{ExportSettings, RenderError, Renderer, export_to_file_observed};

use crate::session::CutlassSession;
use crate::wire::{str_arg, to_c_string};

/// Progress state shared between the export thread and the polling shell.
struct ExportShared {
    frames_done: AtomicU64,
    frames_total: AtomicU64,
    cancel: AtomicBool,
    finished: AtomicBool,
}

/// An in-flight (or finished) export. Free with [`cutlass_export_free`].
pub struct CutlassExportJob {
    shared: Arc<ExportShared>,
    thread: Mutex<Option<JoinHandle<Result<u64, RenderError>>>>,
    /// Cached verdict so `join` is idempotent after the first call.
    verdict: Mutex<Option<String>>,
}

fn verdict_json(result: &Result<u64, RenderError>) -> String {
    let value = match result {
        Ok(frames) => serde_json::json!({ "ok": { "frames": frames } }),
        Err(e) => {
            let kind = match e {
                RenderError::Cancelled => "cancelled",
                _ => "render",
            };
            serde_json::json!({ "err": { "kind": kind, "message": e.to_string() } })
        }
    };
    value.to_string()
}

/// The export body, run on the job thread.
fn run_export(
    project: Project,
    path: PathBuf,
    settings: ExportSettings,
    shared: &ExportShared,
) -> Result<u64, RenderError> {
    let mut renderer = Renderer::new_headless()?;
    let result = export_to_file_observed(
        &mut renderer,
        &project,
        &path,
        settings,
        &mut |done, total| {
            shared.frames_done.store(done, Ordering::Relaxed);
            shared.frames_total.store(total, Ordering::Relaxed);
            !shared.cancel.load(Ordering::Relaxed)
        },
    );
    if result.is_err() {
        // A cancelled/failed output was never finalized — don't leave the
        // corpse for the shell to mistake for a finished movie.
        let _ = std::fs::remove_file(&path);
    }
    result
}

/// Start exporting the session's current project to the file at `path_utf8`
/// (UTF-8, `path_len` bytes; the container/codec follows the platform encoder
/// — H.264/AAC mp4 on Apple).
///
/// `width`/`height` override the output size when both are non-zero (aspect
/// preserved, letterboxed on mismatch); `fps_num`/`fps_den` override the frame
/// sampling rate when both are positive. Zero/negative leaves the project's
/// native value in place. Odd sizes are rounded down to even for H.264.
///
/// The job snapshots the project at call time — later edits don't affect a
/// running export. Returns null when the session/path is invalid. Free the
/// returned job with [`cutlass_export_free`] after joining it.
///
/// # Safety
/// `handle` must be a live session used non-concurrently during this call;
/// `path_utf8` must point to `path_len` initialized bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_export_start(
    handle: *mut CutlassSession,
    path_utf8: *const u8,
    path_len: usize,
    width: u32,
    height: u32,
    fps_num: i32,
    fps_den: i32,
) -> *mut CutlassExportJob {
    // SAFETY: caller contract.
    let (session, path) = unsafe { (handle.as_ref(), str_arg(path_utf8, path_len)) };
    let (Some(session), Some(path)) = (session, path) else {
        return std::ptr::null_mut();
    };

    let project = session.engine().project().clone();
    let mut settings = ExportSettings::for_project(&project);
    if width > 0 && height > 0 {
        settings.size = (width, height);
    }
    if fps_num > 0 && fps_den > 0 {
        settings.frame_rate = cutlass_models::Rational::new(fps_num, fps_den);
    }

    let shared = Arc::new(ExportShared {
        frames_done: AtomicU64::new(0),
        frames_total: AtomicU64::new(0),
        cancel: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });
    let path = PathBuf::from(path);
    let thread = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let result = run_export(project, path, settings, &shared);
            shared.finished.store(true, Ordering::Release);
            result
        })
    };

    Box::into_raw(Box::new(CutlassExportJob {
        shared,
        thread: Mutex::new(Some(thread)),
        verdict: Mutex::new(None),
    }))
}

/// Borrow the job behind an FFI handle.
///
/// # Safety
/// Caller contract of every job fn: `handle` is null or a live pointer from
/// [`cutlass_export_start`] not yet freed.
unsafe fn job<'a>(handle: *const CutlassExportJob) -> Option<&'a CutlassExportJob> {
    // SAFETY: per the caller contract above.
    unsafe { handle.as_ref() }
}

/// Fraction of frames written so far in `0.0..=1.0` (0 before the job has
/// announced its length).
///
/// # Safety
/// `handle` must be null or a live job pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_export_progress(handle: *const CutlassExportJob) -> f64 {
    // SAFETY: caller contract.
    let Some(job) = (unsafe { job(handle) }) else {
        return 0.0;
    };
    let done = job.shared.frames_done.load(Ordering::Relaxed);
    let total = job.shared.frames_total.load(Ordering::Relaxed);
    if total == 0 {
        return 0.0;
    }
    (done as f64 / total as f64).clamp(0.0, 1.0)
}

/// Whether the job's thread has finished (successfully or not). Once true,
/// [`cutlass_export_join`] returns without blocking.
///
/// # Safety
/// `handle` must be null or a live job pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_export_finished(handle: *const CutlassExportJob) -> bool {
    // SAFETY: caller contract.
    unsafe { job(handle) }.is_some_and(|j| j.shared.finished.load(Ordering::Acquire))
}

/// Ask the job to stop after the frame in flight. Idempotent; the join
/// verdict will be `{"err":{"kind":"cancelled",…}}` and the partial output
/// file is deleted.
///
/// # Safety
/// `handle` must be null or a live job pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_export_cancel(handle: *const CutlassExportJob) {
    // SAFETY: caller contract.
    if let Some(job) = unsafe { job(handle) } {
        job.shared.cancel.store(true, Ordering::Relaxed);
    }
}

/// Block until the export finishes and return its verdict as JSON:
/// `{"ok":{"frames":n}}` or `{"err":{"kind":"cancelled"|"render","message":…}}`.
/// Idempotent — later calls return the cached verdict. Null for a null
/// handle. Free the string with [`cutlass_string_free`].
///
/// # Safety
/// `handle` must be null or a live job pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_export_join(handle: *const CutlassExportJob) -> *mut c_char {
    // SAFETY: caller contract.
    let Some(job) = (unsafe { job(handle) }) else {
        return std::ptr::null_mut();
    };
    let mut verdict = job.verdict.lock().expect("export verdict lock");
    if verdict.is_none() {
        let joined = job
            .thread
            .lock()
            .expect("export thread lock")
            .take()
            .map(|t| t.join());
        let result = match joined {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(RenderError::Unsupported("export thread panicked".into())),
            None => Err(RenderError::Unsupported(
                "export job already joined".into(), // unreachable: verdict caches the first join
            )),
        };
        *verdict = Some(verdict_json(&result));
    }
    to_c_string(verdict.clone().expect("verdict just set"))
}

/// Release a job handle. Joins the thread first if nobody has, so dropping a
/// running job blocks until it stops — cancel first for a fast teardown. Null
/// is a no-op.
///
/// # Safety
/// `handle` must be null or a live job pointer, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutlass_export_free(handle: *mut CutlassExportJob) {
    if handle.is_null() {
        return;
    }
    // SAFETY: came from Box::into_raw, freed exactly once.
    let job = unsafe { Box::from_raw(handle) };
    if let Some(thread) = job.thread.lock().expect("export thread lock").take() {
        let _ = thread.join();
    }
}
