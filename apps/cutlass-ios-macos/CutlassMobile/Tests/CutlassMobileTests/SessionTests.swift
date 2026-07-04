import CoreGraphics
import XCTest

@testable import CutlassMobile

/// End-to-end tests of the session bridge: every call crosses the real FFI
/// into the Rust engine (same binary the app ships), so these validate the
/// JSON protocol, the Codable mirrors, and the actor plumbing at once.
final class SessionTests: XCTestCase {
    private func makeSession() throws -> CutlassSession {
        try XCTUnwrap(CutlassSession.create(), "session needs a working GPU")
    }

    /// A 2-second red solid on a sticker lane — the media-free way to put
    /// pixels on the timeline.
    @discardableResult
    private func addSolid(_ session: CutlassSession, seconds: Int64 = 2) async throws -> UInt64 {
        let created = try await session.apply(.addTrack(kind: "Sticker", name: "Solids"))
        let track = try XCTUnwrap(created.editedID)
        let added = try await session.apply(
            .addGenerated(
                track: track,
                generator: .solidColor(rgba: [255, 0, 0, 255]),
                startTicks: 0, durationTicks: seconds * 30, fps: .fps30))
        return try XCTUnwrap(added.editedID)
    }

    func testLifecycleAndEmptyState() async throws {
        let session = try makeSession()
        let state = try await session.uiState()

        XCTAssertEqual(state.revision, 0)
        XCTAssertFalse(state.dirty)
        XCTAssertFalse(state.canUndo)
        XCTAssertEqual(state.fps, .fps30)
        XCTAssertEqual(state.durationSeconds, 0)
        XCTAssertEqual(state.lanes.count, 1, "a fresh session has just the main lane")

        let main = try XCTUnwrap(state.mainLane)
        XCTAssertEqual(main.kind, "video")
        XCTAssertTrue(main.clips.isEmpty)
    }

    func testAddSolidClipShowsUpInUiState() async throws {
        let session = try makeSession()
        let clip = try await addSolid(session)

        let state = try await session.uiState()
        XCTAssertEqual(state.revision, 2, "AddTrack then AddGenerated")
        XCTAssertTrue(state.dirty)
        XCTAssertTrue(state.canUndo)
        XCTAssertEqual(state.durationSeconds, 2)

        let lane = try XCTUnwrap(state.lanes.first(where: { $0.kind == "sticker" }))
        XCTAssertEqual(lane.clips.count, 1)
        let solid = lane.clips[0]
        XCTAssertEqual(solid.id, clip)
        XCTAssertEqual(solid.kind, "solid")
        XCTAssertEqual(solid.lengthSeconds, 2)
        XCTAssertEqual(solid.rgba, [255, 0, 0, 255])
        XCTAssertEqual(solid.transform?.posX, 0.5)
    }

    func testUndoRedoRoundtrip() async throws {
        let session = try makeSession()
        try await addSolid(session)

        let undone = await session.undo()
        XCTAssertTrue(undone, "undo the AddGenerated")
        var state = try await session.uiState()
        XCTAssertEqual(state.lanes.first(where: { $0.kind == "sticker" })?.clips.count, 0)
        XCTAssertTrue(state.canRedo)

        let redone = await session.redo()
        XCTAssertTrue(redone)
        state = try await session.uiState()
        XCTAssertEqual(state.lanes.first(where: { $0.kind == "sticker" })?.clips.count, 1)
    }

    func testIntentGroupsUndoAsOneStep() async throws {
        let session = try makeSession()
        // add_text creates the text lane *and* the clip — one undo step.
        let result = try await session.run(.addText(text: "Hello", atSeconds: 0))
        XCTAssertNotNil(result.clip)

        var state = try await session.uiState()
        let textLane = try XCTUnwrap(state.lanes.first(where: { $0.kind == "text" }))
        XCTAssertEqual(textLane.clips.first?.text, "Hello")
        XCTAssertEqual(textLane.clips.first?.lengthSeconds, 3)
        XCTAssertEqual(textLane.clips.first?.textStyle?.fill, [255, 255, 255, 255])

        let undone = await session.undo()
        XCTAssertTrue(undone)
        state = try await session.uiState()
        XCTAssertNil(
            state.lanes.first(where: { $0.kind == "text" }),
            "one undo removes the whole gesture (lane + clip)")
    }

    func testEngineRejectionThrowsTypedError() async throws {
        let session = try makeSession()
        do {
            try await session.apply(.removeClip(clip: 12345))
            XCTFail("expected a model error")
        } catch let error as CutlassError {
            XCTAssertEqual(error.kind, "model")
            XCTAssertFalse(error.message.isEmpty)
        }
    }

    func testRenderFitReturnsCanvasPixels() async throws {
        let session = try makeSession()
        try await addSolid(session)

        let rendered = await session.renderFrame(atSeconds: 1, maxWidth: 240, maxHeight: 240)
        let image = try XCTUnwrap(rendered)
        XCTAssertLessThanOrEqual(image.width, 240)
        XCTAssertLessThanOrEqual(image.height, 240)
    }

    func testSaveThenOpenRestoresProject() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let path = directory.appendingPathComponent("roundtrip.cutlass").path

        let session = try makeSession()
        try await addSolid(session)
        try await session.save(to: path)
        let wasDirty = await session.isDirty()
        XCTAssertFalse(wasDirty, "save clears the dirty flag")

        let reopened = try XCTUnwrap(CutlassSession.open(path: path))
        let state = try await reopened.uiState()
        XCTAssertEqual(state.lanes.first(where: { $0.kind == "sticker" })?.clips.count, 1)
        XCTAssertEqual(state.durationSeconds, 2)
    }

    func testImportAndAppendBundledSample() async throws {
        let sample = try XCTUnwrap(
            Bundle.module.url(forResource: "sample", withExtension: "mp4"),
            "package bundles sample.mp4")

        let session = try makeSession()
        let result = try await session.run(.appendMain(paths: [sample.path]))
        XCTAssertEqual(result.clips.count, 1)

        let state = try await session.uiState()
        let main = try XCTUnwrap(state.mainLane)
        XCTAssertEqual(main.clips.count, 1)
        let clip = main.clips[0]
        XCTAssertEqual(clip.kind, "video")
        XCTAssertEqual(clip.path, sample.path)
        XCTAssertGreaterThan(clip.lengthSeconds, 0)

        let rendered = await session.renderFrame(atSeconds: 0, maxWidth: 320, maxHeight: 240)
        let frame = try XCTUnwrap(rendered, "engine renders the imported video's first frame")
        XCTAssertLessThanOrEqual(frame.width, 320)
    }

    func testExportJobCompletesAndReportsFrames() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let path = directory.appendingPathComponent("out.mp4").path

        let session = try makeSession()
        try await addSolid(session)

        let started = await session.startExport(to: path, width: 320, height: 180)
        let job = try XCTUnwrap(started)
        let frames = try await job.wait()
        XCTAssertEqual(frames, 60, "2s at 30fps")
        XCTAssertEqual(job.progress, 1)
        XCTAssertTrue(FileManager.default.fileExists(atPath: path))
    }

    func testExportJobCancelReportsCancelled() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let path = directory.appendingPathComponent("cancelled.mp4").path

        let session = try makeSession()
        try await addSolid(session, seconds: 60)

        let started = await session.startExport(to: path)
        let job = try XCTUnwrap(started)
        job.cancel()
        do {
            _ = try await job.wait()
            XCTFail("expected cancellation")
        } catch let error as CutlassError {
            XCTAssertEqual(error.kind, "cancelled")
        }
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: path),
            "a cancelled export cleans up its partial file")
    }

    func testThumbnailerRendersBundledSample() async throws {
        let sample = try XCTUnwrap(
            Bundle.module.url(forResource: "sample", withExtension: "mp4"))
        let thumbnailer = try XCTUnwrap(Thumbnailer.open(path: sample.path))

        let duration = await thumbnailer.durationSeconds()
        XCTAssertGreaterThan(duration, 0)

        let rendered = await thumbnailer.thumbnail(atSeconds: 0, maxWidth: 120, maxHeight: 90)
        let thumb = try XCTUnwrap(rendered)
        XCTAssertLessThanOrEqual(thumb.width, 120)
        XCTAssertLessThanOrEqual(thumb.height, 90)
    }
}
