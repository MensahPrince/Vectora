import CoreGraphics
import CutlassMobileFFI
import Foundation

/// One editing session on the Rust engine: project state, command dispatch,
/// undo/redo history, dirty tracking, save/load, and a persistent renderer.
///
/// The native handle is not thread-safe; the actor provides the required
/// serialization, so every call is `await`ed and may run off the main thread
/// (renders and applies never block the UI).
public actor CutlassSession {
    private let handle: OpaquePointer

    private init(handle: OpaquePointer) {
        self.handle = handle
    }

    deinit {
        cutlass_session_close(handle)
    }

    // MARK: Lifecycle

    /// A fresh session: an empty project at `fps` with a main video track.
    /// `nil` when the GPU renderer can't be brought up.
    public static func create(fps: Fraction = .fps30) -> CutlassSession? {
        guard let handle = cutlass_session_new(fps.num, fps.den) else { return nil }
        return CutlassSession(handle: handle)
    }

    /// Open a session from a `.cutlass` project file. Missing media paths are
    /// tolerated (clips relink later). `nil` on failure.
    public static func open(path: String) -> CutlassSession? {
        let bytes = Array(path.utf8)
        let handle = bytes.withUnsafeBufferPointer {
            cutlass_session_open($0.baseAddress, $0.count)
        }
        guard let handle else { return nil }
        return CutlassSession(handle: handle)
    }

    // MARK: Commands and intents

    /// Apply one raw wire command. Throws `CutlassError` when the engine
    /// rejects it (state is unchanged in that case).
    @discardableResult
    public func apply(_ command: Command) throws -> ApplyOutcome {
        let response = try call(json: .object(command.object)) {
            cutlass_session_apply(handle, $0, $1)
        }
        let type: String
        if case .string(let tag)? = response.payload["type"] {
            type = tag
        } else {
            throw CutlassError.protocolError("outcome without a type tag")
        }
        return ApplyOutcome(
            type: type, value: response.payload["value"], revision: response.revision)
    }

    /// Run one gesture-level intent (grouped into a single undo step; rolls
    /// back atomically on failure).
    @discardableResult
    public func run(_ intent: Intent) throws -> IntentResult {
        let response = try call(json: .object(intent.object)) {
            cutlass_session_intent(handle, $0, $1)
        }
        return IntentResult(payload: response.payload, revision: response.revision)
    }

    // MARK: State

    /// The full presentation state for the current revision.
    public func uiState() throws -> UiState {
        guard let pointer = cutlass_session_ui_state(handle) else {
            throw CutlassError.protocolError("ui_state returned null")
        }
        let json = Self.takeString(pointer)
        do {
            return try WireCoding.decoder.decode(UiState.self, from: Data(json.utf8))
        } catch {
            throw CutlassError.protocolError("undecodable ui_state: \(error)")
        }
    }

    public func undo() -> Bool { cutlass_session_undo(handle) }
    public func redo() -> Bool { cutlass_session_redo(handle) }
    public func canUndo() -> Bool { cutlass_session_can_undo(handle) }
    public func canRedo() -> Bool { cutlass_session_can_redo(handle) }

    /// Monotonic revision; bumps on every successful mutation (cache key for
    /// thumbnails, audio readers, …).
    public func revision() -> UInt64 { cutlass_session_revision(handle) }

    /// Whether there are edits not yet saved to the project file.
    public func isDirty() -> Bool { cutlass_session_is_dirty(handle) }

    /// End of the timeline in seconds (0 for an empty project).
    public func durationSeconds() -> Double { cutlass_session_duration_seconds(handle) }

    // MARK: History groups

    /// Fold every command until `commitGroup` into one undo step (property
    /// panel sessions).
    public func beginGroup() { cutlass_session_begin_group(handle) }
    public func commitGroup() { cutlass_session_commit_group(handle) }
    /// Abort the open group, reverting its commands (panel Cancel).
    public func rollbackGroup() { cutlass_session_rollback_group(handle) }

    // MARK: Convenience I/O

    /// Register a media file with the project pool; returns its media id.
    @discardableResult
    public func importMedia(path: String) throws -> UInt64 {
        let outcome = try apply(.importMedia(path: path))
        guard let media = outcome.value?["media"]?.uint64Value else {
            throw CutlassError.protocolError("Import outcome without a media id")
        }
        return media
    }

    /// Write the project to a `.cutlass` file (clears the dirty flag).
    public func save(to path: String) throws {
        try apply(.save(path: path))
    }

    // MARK: Rendering

    /// Render the frame nearest `seconds`, scaled to fit `maxWidth` ×
    /// `maxHeight` (aspect preserved, never upscaled). `nil` on failure.
    public func renderFrame(atSeconds seconds: Double, maxWidth: Int, maxHeight: Int) -> CGImage? {
        CutlassMobile.makeImage(
            cutlass_session_render_fit(handle, seconds, UInt32(maxWidth), UInt32(maxHeight)))
    }

    // MARK: Audio

    /// A pull reader of the timeline's mixed audio, opened on a snapshot of
    /// the current project at `seconds`. Later edits never affect it — watch
    /// `revision()` and reopen at the playhead on change, same as seeking.
    /// `nil` when the timeline has no audible clips.
    public func openAudioReader(atSeconds seconds: Double) -> AudioReader? {
        guard let reader = cutlass_session_audio_open(handle, seconds) else { return nil }
        return AudioReader(handle: reader)
    }

    // MARK: Export

    /// Start exporting the current project to `path` (H.264/AAC mp4 on Apple)
    /// on a background Rust thread; the session stays fully interactive.
    /// Later edits don't affect the running job. `nil` when the job can't
    /// start. `width`/`height`/`fps` of `nil` keep the project's native
    /// values.
    public func startExport(
        to path: String, width: Int? = nil, height: Int? = nil, fps: Fraction? = nil
    ) -> ExportJob? {
        let bytes = Array(path.utf8)
        let job = bytes.withUnsafeBufferPointer {
            cutlass_export_start(
                handle, $0.baseAddress, $0.count,
                UInt32(width ?? 0), UInt32(height ?? 0),
                fps?.num ?? 0, fps?.den ?? 0)
        }
        guard let job else { return nil }
        return ExportJob(handle: job, outputPath: path)
    }

    // MARK: Plumbing

    private struct Response {
        var payload: JSONValue
        var revision: UInt64
    }

    /// Encode `json`, run one string-returning FFI call, and unwrap the
    /// response envelope into payload + revision (or throw its error).
    private func call(
        json: JSONValue, _ body: (UnsafePointer<UInt8>?, Int) -> UnsafeMutablePointer<CChar>?
    ) throws -> Response {
        let request: Data
        do {
            request = try WireCoding.plainEncoder.encode(json)
        } catch {
            throw CutlassError.protocolError("unencodable request: \(error)")
        }
        let pointer = request.withUnsafeBytes { raw in
            body(raw.bindMemory(to: UInt8.self).baseAddress, raw.count)
        }
        guard let pointer else {
            throw CutlassError.protocolError("session call returned null")
        }
        let text = Self.takeString(pointer)
        let envelope: ResponseEnvelope
        do {
            envelope = try WireCoding.plainDecoder.decode(
                ResponseEnvelope.self, from: Data(text.utf8))
        } catch {
            throw CutlassError.protocolError("undecodable response: \(error)")
        }
        if let error = envelope.err {
            throw error
        }
        return Response(payload: envelope.ok ?? .null, revision: envelope.revision ?? 0)
    }

    /// Copy a Rust-allocated C string into a Swift `String` and free it.
    private static func takeString(_ pointer: UnsafeMutablePointer<CChar>) -> String {
        defer { cutlass_string_free(pointer) }
        return String(cString: pointer)
    }
}

/// A background export: progress and cancel are safe from any thread; the
/// verdict arrives through `wait()`.
public final class ExportJob: @unchecked Sendable {
    private let handle: OpaquePointer
    /// Where the movie lands on success (a cancelled/failed export deletes
    /// the partial file).
    public let outputPath: String

    init(handle: OpaquePointer, outputPath: String) {
        self.handle = handle
        self.outputPath = outputPath
    }

    deinit {
        // Freeing joins the thread; a running job is cancelled first so
        // dropping the last reference can't hang on a long export.
        cutlass_export_cancel(handle)
        cutlass_export_free(handle)
    }

    /// Fraction of frames written so far, 0…1.
    public var progress: Double { cutlass_export_progress(handle) }

    /// Whether the export thread has finished (successfully or not).
    public var isFinished: Bool { cutlass_export_finished(handle) }

    /// Ask the job to stop after the frame in flight; the partial output file
    /// is deleted and `wait()` reports `kind == "cancelled"`.
    public func cancel() { cutlass_export_cancel(handle) }

    /// Await the verdict: the number of frames written, or a `CutlassError`
    /// (`kind == "cancelled"` after `cancel()`). Polls off the cooperative
    /// pool so it never blocks a Swift concurrency thread.
    public func wait(pollingEveryMilliseconds interval: UInt64 = 100) async throws -> UInt64 {
        while !isFinished {
            try await Task.sleep(nanoseconds: interval * 1_000_000)
        }
        guard let pointer = cutlass_export_join(handle) else {
            throw CutlassError.protocolError("export join returned null")
        }
        defer { cutlass_string_free(pointer) }
        let verdict: JSONValue
        do {
            verdict = try WireCoding.plainDecoder.decode(
                JSONValue.self, from: Data(String(cString: pointer).utf8))
        } catch {
            throw CutlassError.protocolError("undecodable export verdict: \(error)")
        }
        if let frames = verdict["ok"]?["frames"]?.uint64Value {
            return frames
        }
        if case .object(let fields)? = verdict["err"],
            case .string(let kind)? = fields["kind"],
            case .string(let message)? = fields["message"]
        {
            throw CutlassError(kind: kind, message: message)
        }
        throw CutlassError.protocolError("malformed export verdict")
    }
}

/// A pull reader of the timeline's mixed audio: every audible clip summed
/// with volume/fades applied, as interleaved stereo `f32` at 48 kHz, decoded
/// from a project snapshot.
///
/// Reads block while sources decode — pull from a feeder task into a ring
/// buffer and let the audio render callback only copy. Not internally
/// synchronized: one consumer at a time (the feeder), like `ExportJob`.
public final class AudioReader: @unchecked Sendable {
    /// Sample rate of every read, in Hz.
    public static let sampleRate = Double(cutlass_audio_rate())
    /// Interleaved channels per sample frame.
    public static let channelCount = Int(cutlass_audio_channels())

    private let handle: OpaquePointer

    init(handle: OpaquePointer) {
        self.handle = handle
    }

    deinit {
        cutlass_audio_close(handle)
    }

    /// Fill `out` (holding `maxFrames * channelCount` floats) with up to
    /// `maxFrames` sample frames, advancing the reader. Returns the frames
    /// written — 0 at the end of the timeline — or `nil` after a decode
    /// failure (sticky; reopen to retry).
    public func read(into out: UnsafeMutablePointer<Float>, maxFrames: Int) -> Int? {
        let got = cutlass_audio_read(handle, out, maxFrames)
        return got < 0 ? nil : Int(got)
    }
}

/// Filmstrip thumbnails for one media file, rendered by a private decoder +
/// GPU pipeline so they never contend with the editing session. One actor per
/// file; run as many in parallel as you like.
public actor Thumbnailer {
    private let handle: OpaquePointer

    private init(handle: OpaquePointer) {
        self.handle = handle
    }

    deinit {
        cutlass_thumbnailer_close(handle)
    }

    /// `nil` if the file can't be probed or the GPU is unavailable.
    public static func open(path: String) -> Thumbnailer? {
        let bytes = Array(path.utf8)
        let handle = bytes.withUnsafeBufferPointer {
            cutlass_thumbnailer_open($0.baseAddress, $0.count)
        }
        guard let handle else { return nil }
        return Thumbnailer(handle: handle)
    }

    /// Media length in seconds.
    public func durationSeconds() -> Double {
        cutlass_thumbnailer_duration_seconds(handle)
    }

    /// The frame nearest `seconds`, scaled to fit (never upscaled).
    public func thumbnail(atSeconds seconds: Double, maxWidth: Int, maxHeight: Int) -> CGImage? {
        CutlassMobile.makeImage(
            cutlass_thumbnailer_thumb(handle, seconds, UInt32(maxWidth), UInt32(maxHeight)))
    }
}
