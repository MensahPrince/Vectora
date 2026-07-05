package com.scytheralpha.cutlass_android

/**
 * JNI bindings for the `cutlass-mobile` Rust library (`libcutlass_mobile.so`).
 *
 * Mirrors the C ABI the iOS shell links: JSON strings for commands / intents /
 * UI state (same response envelope), `[width, height, argb…]` int arrays for
 * pixels, and opaque `Long` handles for sessions / jobs / readers. Handles are
 * not thread-safe — serialize calls per handle; `0` means open/start failed.
 *
 * Build the .so into `src/main/jniLibs/<abi>/` with:
 * `cargo ndk -t arm64-v8a -o apps/cutlass-android/app/src/main/jniLibs build -p cutlass-mobile --release`
 */
object CutlassNative {
    init {
        System.loadLibrary("cutlass_mobile")
    }

    // Demo / preview harness (device GPU proof).
    external fun renderDemo(width: Int, height: Int): IntArray
    external fun renderFileFrame(path: String, maxWidth: Int, maxHeight: Int): IntArray
    external fun previewOpenDemo(): Long
    external fun previewOpenVideo(path: String): Long
    external fun previewDurationSeconds(handle: Long): Double
    external fun previewRender(handle: Long, seconds: Double): IntArray
    external fun previewClose(handle: Long)

    /** Every preset vocabulary (masks, filters, animations, …) as one JSON document. */
    external fun catalogs(): String?

    // Editing session: the full engine (commands, undo, save) behind one handle.
    external fun sessionNew(fpsNum: Int, fpsDen: Int): Long
    external fun sessionOpen(path: String): Long
    external fun sessionClose(handle: Long)

    /** One wire command (`{"type": …}`); returns the JSON response envelope. */
    external fun sessionApply(handle: Long, json: String): String?

    /** One gesture intent (`{"intent": …}`); grouped + atomic; JSON envelope back. */
    external fun sessionIntent(handle: Long, json: String): String?

    /** The lane-stack presentation state as JSON. */
    external fun sessionUiState(handle: Long): String?

    external fun sessionUndo(handle: Long): Boolean
    external fun sessionRedo(handle: Long): Boolean
    external fun sessionCanUndo(handle: Long): Boolean
    external fun sessionCanRedo(handle: Long): Boolean
    external fun sessionRevision(handle: Long): Long
    external fun sessionIsDirty(handle: Long): Boolean
    external fun sessionBeginGroup(handle: Long)
    external fun sessionCommitGroup(handle: Long)
    external fun sessionRollbackGroup(handle: Long)
    external fun sessionDurationSeconds(handle: Long): Double

    /** The timeline frame nearest `seconds` as `[width, height, argb…]` (empty on failure). */
    external fun sessionRenderFit(handle: Long, seconds: Double, maxWidth: Int, maxHeight: Int): IntArray

    // Background export job over a project snapshot (session stays interactive).
    external fun exportStart(
        session: Long, path: String, width: Int, height: Int, fpsNum: Int, fpsDen: Int,
    ): Long
    external fun exportProgress(job: Long): Double
    external fun exportFinished(job: Long): Boolean
    external fun exportCancel(job: Long)

    /** Blocks until finished; the JSON verdict. */
    external fun exportJoin(job: Long): String?
    external fun exportFree(job: Long)

    // Filmstrip thumbnails (own decoder cache, callable off the session thread).
    external fun thumbnailerOpen(path: String): Long
    external fun thumbnailerDurationSeconds(handle: Long): Double
    external fun thumbnailerThumb(handle: Long, seconds: Double, maxWidth: Int, maxHeight: Int): IntArray
    external fun thumbnailerClose(handle: Long)

    // Realtime mixed audio: pull interleaved stereo f32 at 48 kHz.
    external fun audioRate(): Int
    external fun audioChannels(): Int
    external fun sessionAudioOpen(session: Long, startSeconds: Double): Long

    /** Fills `out` with up to `maxFrames` stereo frames; frames written, 0 = end, -1 = error. */
    external fun audioRead(handle: Long, out: FloatArray, maxFrames: Int): Int
    external fun audioClose(handle: Long)
}
