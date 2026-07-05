import CoreGraphics
import CutlassMobile
import Foundation
import SwiftUI
import Testing

@testable import cutlass_ios_macos

/// Engine-backed coverage for every `EditorState` edit op: each test drives
/// the public API the views call, waits for the engine round trip, and
/// asserts on the re-projected view-model arrays (`ui_state` truth).
///
/// Media comes from the bundled fixtures (`demo1.mp4` 4s, `demo2.mp4` 6s,
/// `photo.png` still, `tone.m4a` 8s) — the same files the picker's Samples
/// tab offers.
@MainActor
struct EditorStateEngineTests {
    /// 4-second video (`demo1.mp4`) and 6-second video (`demo2.mp4`).
    private var shortVideo: URL { FixtureLibrary.shortVideo! }
    private var video: URL { FixtureLibrary.video! }
    /// Still photo (imports as a 5s image clip).
    private var photo: URL { FixtureLibrary.photo! }

    private func makeProject(_ urls: [URL]) async -> EditorState {
        let state = EditorState()
        state.startProject(with: urls)
        await state.waitForEngine()
        return state
    }

    private func near(
        _ value: Double, _ expected: Double, tolerance: Double = 0.05
    ) -> Bool {
        abs(value - expected) <= tolerance
    }

    // MARK: Project lifecycle

    @Test func startProjectAppendsPicksToMain() async throws {
        let state = await makeProject([shortVideo, video])

        #expect(state.clips.count == 2)
        #expect(state.clips.allSatisfy { $0.engineID != nil })
        #expect(near(state.clips[0].length, 4))
        #expect(near(state.clips[1].length, 6))
        #expect(near(state.mainDuration, 10))
        #expect(state.lanes.count == 1)
        #expect(state.lanes[0].isMain)
        #expect(state.canUndo, "the append intent is one undo step")
        #expect(!state.canRedo)
        // Engine media backs every clip.
        #expect(state.clips[0].mediaPath?.hasSuffix("demo1.mp4") == true)
        #expect(state.clips[0].hasAudio, "fixture videos carry AAC audio")
    }

    @Test func appendMediaExtendsTheTimeline() async throws {
        let state = await makeProject([shortVideo])
        state.appendMedia([video])
        await state.waitForEngine()

        #expect(state.clips.count == 2)
        #expect(near(state.mainDuration, 10))
    }

    @Test func photoPicksLandAsFiveSecondStills() async throws {
        let state = await makeProject([photo])

        #expect(state.clips.count == 1)
        let still = state.clips[0]
        #expect(near(still.length, 5), "stills use the 5s placement default")
        #expect(!still.hasAudio)
        #expect(!still.isFreeze, "photo picks don't wear the freeze badge")
        #expect(still.mediaPath?.hasSuffix("photo.png") == true)
    }

    @Test func photoPipLandsOnAVideoLane() async throws {
        let state = await makeProject([video])
        state.playhead = 1

        let id = state.addPip(from: photo)
        await state.waitForEngine()

        let overlay = try #require(state.overlayClips.first { $0.id == id })
        #expect(overlay.kind == .pip)
        #expect(overlay.engineID != nil)
        #expect(near(overlay.length, 5))
        #expect(!overlay.pipHasAudio)
    }

    @Test func undoToEmptyAndRedoBack() async throws {
        let state = await makeProject([shortVideo])
        #expect(state.clips.count == 1)

        state.undo()
        await state.waitForEngine()
        #expect(state.clips.isEmpty)
        #expect(!state.canUndo)
        #expect(state.canRedo)

        state.redo()
        await state.waitForEngine()
        #expect(state.clips.count == 1)
    }

    // MARK: Structural ops

    @Test func splitAtPlayheadMakesTwoPieces() async throws {
        let state = await makeProject([shortVideo])
        state.playhead = 2

        state.splitAtPlayhead()
        await state.waitForEngine()
        #expect(state.clips.count == 2)
        #expect(near(state.clips[0].length, 2))
        #expect(near(state.clips[1].length, 2))
        #expect(near(state.mainDuration, 4), "split never changes duration")

        // One undo step restores the single clip; redo re-splits.
        state.undo()
        await state.waitForEngine()
        #expect(state.clips.count == 1)
        #expect(near(state.clips[0].length, 4))
        state.redo()
        await state.waitForEngine()
        #expect(state.clips.count == 2)
    }

    @Test func rippleTrimTrailingEdge() async throws {
        let state = await makeProject([shortVideo, video])
        let anchor = state.clips[0]

        state.trim(anchor.id, edge: .trailing, anchor: anchor, by: -1)
        #expect(near(state.clips[0].length, 3), "local preview trims live")
        state.endGesture()
        await state.waitForEngine()

        #expect(near(state.clips[0].length, 3), "engine confirms the trim")
        #expect(near(state.mainDuration, 9), "later clips ripple left")
    }

    @Test func rippleTrimLeadingEdgeConsumesSource() async throws {
        let state = await makeProject([shortVideo])
        let anchor = state.clips[0]

        state.trim(anchor.id, edge: .leading, anchor: anchor, by: 1)
        state.endGesture()
        await state.waitForEngine()

        #expect(near(state.clips[0].length, 3))
        #expect(near(state.clips[0].trimStart, 1), "in-point advanced")
    }

    @Test func reorderMainClips() async throws {
        let state = await makeProject([shortVideo, video])
        let first = state.clips[0].engineID

        state.moveClip(fromIndex: 0, toIndex: 1)
        await state.waitForEngine()

        #expect(near(state.clips[0].length, 6))
        #expect(near(state.clips[1].length, 4))
        #expect(state.clips[1].engineID == first, "identity travels with the clip")
    }

    @Test func deleteSelectedRipples() async throws {
        let state = await makeProject([shortVideo, video])
        state.selection = .main(state.clips[0].id)

        state.deleteSelected()
        await state.waitForEngine()

        #expect(state.clips.count == 1)
        #expect(near(state.mainDuration, 6), "survivor slides to t=0")
        #expect(state.selection == nil)
    }

    @Test func duplicateSelectsTheCopy() async throws {
        let state = await makeProject([shortVideo])
        state.selection = .main(state.clips[0].id)

        state.duplicateSelected()
        await state.waitForEngine()

        #expect(state.clips.count == 2)
        #expect(near(state.clips[1].length, 4))
        #expect(state.clips[0].engineID != state.clips[1].engineID)
        #expect(state.selection == .main(state.clips[1].id), "copy is selected")
    }

    @Test func replaceSwapsTheSourceKeepingTheSlot() async throws {
        let state = await makeProject([shortVideo, video])
        state.selection = .main(state.clips[0].id)

        state.replaceSelected(with: video)
        await state.waitForEngine()

        #expect(state.clips.count == 2)
        #expect(state.clips[0].mediaPath?.hasSuffix("demo2.mp4") == true)
        #expect(near(state.clips[0].length, 4), "slot length is preserved")
        #expect(near(state.clips[0].sourceDuration, 6), "new source window")
    }

    // MARK: Lane content

    @Test func addTextAdoptsThePlaceholderIdentity() async throws {
        let state = await makeProject([shortVideo])
        let id = state.addTextClip(text: "Hello")
        #expect(state.overlayClips.count == 1, "optimistic placeholder shows now")
        #expect(state.overlayClips[0].engineID == nil)

        await state.waitForEngine()
        #expect(state.overlayClips.count == 1)
        #expect(state.overlayClips[0].id == id, "engine clip kept the placeholder UUID")
        #expect(state.overlayClips[0].engineID != nil)
        #expect(state.overlayClips[0].kind == .text)
        #expect(state.overlayClips[0].text == "Hello")
        #expect(state.lanes.contains { $0.kind == .text && !$0.isMain })
        #expect(state.selection == .overlay(id), "selection survives the refresh")
    }

    @Test func addStickerEffectAndAudioLand() async throws {
        let state = await makeProject([shortVideo])
        let sticker = state.addSticker(symbol: "heart.fill")
        let effect = state.addEffectClip(name: "Blur", kind: .effect)
        let audio = state.addAudio(kind: .music, title: "Tone", duration: 8)
        await state.waitForEngine()

        #expect(state.overlayClips.count == 1)
        #expect(state.overlayClips[0].id == sticker)
        #expect(state.overlayClips[0].engineID != nil)
        #expect(state.overlayClips[0].symbol == "heart.fill", "panel pick survives refresh")

        #expect(state.effectClips.count == 1)
        #expect(state.effectClips[0].id == effect)
        #expect(state.effectClips[0].engineID != nil)
        #expect(state.effectClips[0].name == "Blur")

        #expect(state.audioClips.count == 1)
        #expect(state.audioClips[0].id == audio)
        #expect(state.audioClips[0].engineID != nil)
        #expect(near(state.audioClips[0].length, 8), "tone.m4a is 8s")

        // Lane stack order: main on top, generated kinds between, audio floor.
        #expect(state.lanes.first?.isMain == true)
        #expect(state.lanes.last?.kind == .audio)
    }

    @Test func addPipGetsTheDropPose() async throws {
        let state = await makeProject([video])
        state.playhead = 1
        let id = state.addPip(from: shortVideo)
        await state.waitForEngine()

        #expect(state.overlayClips.count == 1)
        let pip = try #require(state.overlayClips.first { $0.id == id })
        #expect(pip.engineID != nil)
        #expect(pip.kind == .pip)
        #expect(near(pip.start, 1))
        #expect(near(pip.length, 4), "pip spans its media length")
        #expect(near(pip.scale, 0.5), "CapCut drop pose")
        #expect(near(pip.posY, 0.32))
        // The pip lane sits above the main row.
        let pipLaneRow = state.lanes.firstIndex { $0.id == pip.laneID }
        #expect(pipLaneRow != nil && pipLaneRow! < state.mainLaneRow)
    }

    @Test func laneTrimAndMoveCommitOnRelease() async throws {
        let state = await makeProject([video])
        let id = state.addAudio(kind: .music, title: "Tone", duration: 8)
        await state.waitForEngine()
        let target = TimelineSelection.audio(id)

        state.trimLaneClip(target, edge: .trailing, anchorStart: 0, anchorLength: 8, by: -2)
        state.endGesture()
        await state.waitForEngine()
        #expect(near(state.audioClips[0].length, 6))

        state.moveLaneClip(target, anchorStart: 0, by: 1.2)
        state.endGesture()
        await state.waitForEngine()
        #expect(near(state.audioClips[0].start, 1.2))
        #expect(near(state.audioClips[0].length, 6), "move keeps the length")
    }

    // MARK: Cross-lane moves

    @Test func mainClipLiftsToALaneAndBack() async throws {
        let state = await makeProject([shortVideo, video])
        let lifted = state.clips[0].id

        state.moveMainClipToLane(lifted, at: 8)
        await state.waitForEngine()
        #expect(state.clips.count == 1)
        #expect(near(state.mainDuration, 6), "main closes the hole")
        #expect(state.overlayClips.count == 1)
        let pip = state.overlayClips[0]
        #expect(pip.id == lifted, "same identity across the lane change")
        #expect(pip.kind == .pip)
        #expect(near(pip.start, 8))

        state.moveLaneClipToMain(lifted, at: 100)
        await state.waitForEngine()
        #expect(state.overlayClips.isEmpty)
        #expect(state.clips.count == 2)
        #expect(state.clips[1].id == lifted)
        #expect(near(state.mainDuration, 10))
    }

    // MARK: Quick ops

    @Test func transitionsPersistAndClear() async throws {
        let state = await makeProject([shortVideo, video])
        let first = state.clips[0].id

        state.setTransition(after: first, MockTransition(style: "Fade", duration: 0.5))
        await state.waitForEngine()
        let applied = try #require(state.clips[0].transitionAfter)
        #expect(applied.style == "Fade")
        #expect(near(applied.duration, 0.5))

        state.setTransition(after: first, nil)
        await state.waitForEngine()
        #expect(state.clips[0].transitionAfter == nil)
    }

    @Test func keyframeToggleStampsAndRemoves() async throws {
        let state = await makeProject([shortVideo])
        state.selection = .main(state.clips[0].id)
        state.playhead = 1

        state.toggleKeyframeAtPlayhead()
        await state.waitForEngine()
        #expect(state.clips[0].keyframes.contains { near($0, 1) })

        state.toggleKeyframeAtPlayhead()
        await state.waitForEngine()
        #expect(state.clips[0].keyframes.isEmpty)
    }

    @Test func extractAudioLandsALinkedAudioClip() async throws {
        let state = await makeProject([video])
        state.selection = .main(state.clips[0].id)

        state.extractAudio()
        await state.waitForEngine()

        #expect(state.audioClips.count == 1)
        #expect(state.audioClips[0].engineID != nil)
        #expect(near(state.audioClips[0].start, 0), "aligned with its video")
        #expect(state.audioClips[0].kind == .extracted)
        #expect(state.clips.count == 1, "original stays on main")
    }

    @Test func freezeFrameSplitsAroundAStill() async throws {
        let state = await makeProject([shortVideo])
        state.selection = .main(state.clips[0].id)
        state.playhead = 2

        state.freezeFrame()
        await state.waitForEngine()

        #expect(state.clips.count == 3, "left half + still + right half")
        #expect(near(state.clips[0].length, 2))
        #expect(state.clips[1].isFreeze, "the still wears the snowflake")
        #expect(near(state.clips[1].length, 3))
        #expect(!state.clips[1].hasAudio)
        #expect(near(state.clips[2].length, 2))
        #expect(near(state.mainDuration, 7))

        state.undo()
        await state.waitForEngine()
        #expect(state.clips.count == 1, "the freeze undoes as one step")
        #expect(near(state.mainDuration, 4))
    }

    @Test func reverseSelectedRoundTrips() async throws {
        let state = await makeProject([shortVideo])
        state.selection = .main(state.clips[0].id)

        state.reverseSelected()
        await state.waitForEngine()
        #expect(state.clips[0].isReversed)

        state.reverseSelected()
        await state.waitForEngine()
        #expect(!state.clips[0].isReversed)
    }

    // MARK: Panel sessions

    @Test func panelSpeedCommitRetimesTheClip() async throws {
        let state = await makeProject([video])
        state.selection = .main(state.clips[0].id)

        state.beginPanelSession()
        state.setSelectedSpeed(2)
        #expect(near(state.clips[0].length, 3), "local preview rescales")
        state.commitPanelSession()
        await state.waitForEngine()

        #expect(near(state.clips[0].speed, 2))
        #expect(near(state.clips[0].length, 3), "engine confirms 6s content at 2x")
    }

    @Test func panelVolumeCommitAndCancel() async throws {
        let state = await makeProject([shortVideo])
        state.selection = .main(state.clips[0].id)

        // Cancel: local change reverts, engine never sees it.
        state.beginPanelSession()
        state.updateSelectedClip { $0.volume = 0.2 }
        state.cancelPanelSession()
        await state.waitForEngine()
        #expect(near(state.clips[0].volume, 1))

        // Commit: the diff lands as one intent.
        state.beginPanelSession()
        state.updateSelectedClip { $0.volume = 0.4 }
        state.commitPanelSession()
        await state.waitForEngine()
        #expect(near(state.clips[0].volume, 0.4))

        // The volume edit is one undo step.
        state.undo()
        await state.waitForEngine()
        #expect(near(state.clips[0].volume, 1))
    }

    @Test func panelCanvasAspectCommits() async throws {
        let state = await makeProject([shortVideo])

        state.beginPanelSession()
        state.aspect = .vertical
        state.commitPanelSession()
        await state.waitForEngine()

        #expect(state.aspect == .vertical, "engine round-trips 9:16")
        state.undo()
        await state.waitForEngine()
        #expect(state.aspect == .original)
    }

    @Test func panelOverlayTransformCommits() async throws {
        let state = await makeProject([shortVideo])
        let id = state.addTextClip(text: "Move me")
        await state.waitForEngine()
        state.selection = .overlay(id)

        state.beginPanelSession()
        state.updateSelectedOverlay {
            $0.posX = 0.25
            $0.posY = 0.75
            $0.scale = 1.5
            $0.opacity = 0.5
        }
        state.commitPanelSession()
        await state.waitForEngine()

        let overlay = try #require(state.overlayClips.first { $0.id == id })
        #expect(near(overlay.posX, 0.25))
        #expect(near(overlay.posY, 0.75))
        #expect(near(overlay.scale, 1.5))
        #expect(near(overlay.opacity, 0.5))
    }

    // MARK: Look panels (mask / chroma / stabilize / filter / adjust / animation)

    @Test func panelLookCommitsPersistTheClipStyle() async throws {
        let state = await makeProject([video])
        state.selection = .main(state.clips[0].id)

        state.beginPanelSession()
        state.updateSelectedClip {
            $0.maskName = "circle"
            $0.chromaColor = .green
            $0.chromaStrength = 0.6
            $0.stabilizeLevel = "smooth"
            $0.filterName = "vivid"
            $0.filterIntensity = 0.5
            $0.adjust.brightness = 0.25
            $0.animationIn = "fade_in"
        }
        state.commitPanelSession()
        await state.waitForEngine()

        // Every value below was re-projected from the engine's ui_state.
        #expect(state.engineOpFailures.isEmpty, "\(state.engineOpFailures)")
        let clip = state.clips[0]
        #expect(clip.maskName == "circle")
        #expect(clip.chromaColor != nil)
        #expect(near(clip.chromaStrength, 0.6))
        #expect(clip.stabilizeLevel == "smooth")
        #expect(clip.filterName == "vivid")
        #expect(near(clip.filterIntensity, 0.5))
        #expect(near(clip.adjust.brightness, 0.25))
        #expect(clip.animationIn == "fade_in")

        // Each look property is its own engine undo step.
        state.undo()
        await state.waitForEngine()
        #expect(state.clips[0].animationIn == nil, "undo peels the last look edit")
        #expect(state.clips[0].filterName == "vivid", "earlier edits survive")
    }

    @Test func panelLookCancelNeverReachesTheEngine() async throws {
        let state = await makeProject([video])
        state.selection = .main(state.clips[0].id)
        let undoBefore = state.canUndo

        state.beginPanelSession()
        state.updateSelectedClip {
            $0.filterName = "noir"
            $0.maskName = "heart"
        }
        state.cancelPanelSession()
        await state.waitForEngine()

        #expect(state.clips[0].filterName == nil)
        #expect(state.clips[0].maskName == nil)
        #expect(state.canUndo == undoBefore, "no engine history entries")
    }

    @Test func panelComboAnimationEvictsTheEntrance() async throws {
        let state = await makeProject([video])
        state.selection = .main(state.clips[0].id)

        state.beginPanelSession()
        state.updateSelectedClip { $0.animationIn = "zoom_in" }
        state.commitPanelSession()
        await state.waitForEngine()
        #expect(state.clips[0].animationIn == "zoom_in")

        // The panel clears in/out locally when a combo is picked; the engine
        // enforces the same eviction on commit.
        state.beginPanelSession()
        state.updateSelectedClip {
            $0.animationCombo = "pulse"
            $0.animationIn = nil
        }
        state.commitPanelSession()
        await state.waitForEngine()
        #expect(state.clips[0].animationCombo == "pulse")
        #expect(state.clips[0].animationIn == nil)
    }

    @Test func panelSpeedPresetCommitsAndClears() async throws {
        let state = await makeProject([video])
        state.selection = .main(state.clips[0].id)
        let plainLength = state.clips[0].length

        state.beginPanelSession()
        state.updateSelectedClip { $0.speedCurve = "montage" }
        state.commitPanelSession()
        await state.waitForEngine()
        #expect(state.clips[0].speedCurve == "montage", "curve matches the catalog preset")
        #expect(state.clips[0].length != plainLength, "the curve integral retimes the clip")

        state.beginPanelSession()
        state.updateSelectedClip { $0.speedCurve = nil }
        state.commitPanelSession()
        await state.waitForEngine()
        #expect(state.clips[0].speedCurve == nil)
        #expect(near(state.clips[0].length, plainLength), "constant speed restores the length")
    }

    @Test func panelTextEffectRoundTripsThroughTheStyle() async throws {
        let state = await makeProject([shortVideo])
        let id = state.addTextClip(text: "Neon title")
        await state.waitForEngine()
        state.selection = .overlay(id)

        state.beginPanelSession()
        state.updateSelectedOverlay {
            $0.textEffect = "neon"
            $0.animation = "typewriter"
        }
        state.commitPanelSession()
        await state.waitForEngine()

        let overlay = try #require(state.overlayClips.first { $0.id == id })
        #expect(overlay.textEffect == "neon", "preset id baked into the engine style")
        #expect(overlay.animation == "typewriter", "text-only combo persisted")

        // The values came from the engine, not a stale local array: undo
        // strips them, redo restores them.
        state.undo()
        state.undo()
        await state.waitForEngine()
        let plain = try #require(state.overlayClips.first { $0.id == id })
        #expect(plain.textEffect == nil)
        #expect(plain.animation == nil)
        state.redo()
        state.redo()
        await state.waitForEngine()
        let restored = try #require(state.overlayClips.first { $0.id == id })
        #expect(restored.textEffect == "neon")
        #expect(restored.animation == "typewriter")
    }

    @Test func panelAdjustBarPersistsItsGrade() async throws {
        let state = await makeProject([shortVideo])
        state.addEffectClip(name: "Adjust", kind: .adjust)
        await state.waitForEngine()

        state.beginPanelSession()
        state.updateSelectedEffect { $0.adjust.exposure = 0.4 }
        state.commitPanelSession()
        await state.waitForEngine()

        let bar = try #require(state.effectClips.first)
        #expect(bar.kind == .adjust)
        #expect(near(bar.adjust.exposure, 0.4), "the AdjustPanel gap: sliders now persist")
    }

    @Test func filterBarDropAppliesTheCatalogFilter() async throws {
        let state = await makeProject([shortVideo])
        state.addFilterClip(id: "noir", label: "Noir")
        await state.waitForEngine()

        let bar = try #require(state.effectClips.first)
        #expect(bar.kind == .filter)
        #expect(bar.filterID == "noir", "filter id landed on the engine bar clip")
        #expect(bar.name == "Noir", "label resolves from the catalog")
    }

    @Test func audioPicksCarryTheirRole() async throws {
        let state = await makeProject([shortVideo])
        state.addAudio(kind: .voiceover, title: "VO", duration: 3)
        await state.waitForEngine()

        let audio = try #require(state.audioClips.first)
        #expect(audio.kind == .voiceover, "role round-trips through the engine tag")
    }

    // MARK: Gesture transforms

    @Test func overlayDragCommitsTheFinalPose() async throws {
        let state = await makeProject([shortVideo])
        let id = state.addTextClip(text: "Drag")
        await state.waitForEngine()

        state.dragOverlay(id, anchorX: 0.5, anchorY: 0.5, deltaX: 0.2, deltaY: -0.1)
        state.endGesture()
        await state.waitForEngine()

        let overlay = try #require(state.overlayClips.first { $0.id == id })
        #expect(near(overlay.posX, 0.7))
        #expect(near(overlay.posY, 0.4))
    }

    // MARK: Preview rendering (Phase F)

    @Test func previewRendersEngineFramesFitToTheBox() async throws {
        let state = await makeProject([shortVideo])

        let frame = try #require(
            await state.renderPreviewFrame(atSeconds: 0.5, maxWidth: 320, maxHeight: 640))
        #expect(frame.width <= 320 && frame.height <= 640)
        #expect(frame.width > 0 && frame.height > 0)

        // The refresh reported the resolved canvas; the fit frame keeps its
        // aspect (demo1.mp4 is 640x360 → auto canvas is 16:9).
        let canvas = try #require(state.canvasSize)
        let canvasAspect = canvas.width / canvas.height
        let frameAspect = Double(frame.width) / Double(frame.height)
        #expect(abs(canvasAspect - frameAspect) < 0.05)
        #expect(state.appliedRevision > 0, "refreshes stamp the observable revision")

        // Perf checkpoint (plan Phase F): a 30-position scrub burst at
        // preview size, sequential decode. Printed, not asserted — the hard
        // bound would be flaky across machines; regressions show up in the
        // logged number and the feed's quality ladder.
        let start = ContinuousClock.now
        for tick in 1...30 {
            _ = await state.renderPreviewFrame(
                atSeconds: Double(tick) / 10, maxWidth: 640, maxHeight: 360)
        }
        let elapsed = start.duration(to: .now)
        let perFrame = Double(elapsed.components.seconds) * 1000 / 30
            + Double(elapsed.components.attoseconds) / 1e15 / 30
        print("cutlass-perf: render_fit(640x360) averaged \(perFrame) ms/frame over 30 frames")
    }

    /// Mutable capture box for feed-callback assertions.
    private final class RenderLog {
        var seconds: [Double] = []
    }

    @Test func previewFeedDropsIntermediateScrubPositions() async throws {
        let log = RenderLog()
        let feed = PreviewFeed { seconds, _, _ in
            log.seconds.append(seconds)
            try? await Task.sleep(for: .milliseconds(20))
            return Self.tinyImage()
        }

        // A scrub burst: 30 positions, yielding between ticks so the pump is
        // mid-render while new requests arrive.
        for tick in 0..<30 {
            feed.request(
                seconds: Double(tick), revision: 1,
                viewSize: CGSize(width: 200, height: 400), displayScale: 2)
            await Task.yield()
        }
        await feed.settle()

        #expect(feed.image != nil)
        #expect(log.seconds.last == 29, "the preview converges on the newest position")
        #expect(log.seconds.count < 30, "intermediate positions are dropped, not queued")
    }

    @Test func previewFeedSkipsRequestsIdenticalToTheDeliveredFrame() async throws {
        let log = RenderLog()
        let feed = PreviewFeed { seconds, _, _ in
            log.seconds.append(seconds)
            return Self.tinyImage()
        }

        for _ in 0..<3 {
            feed.request(
                seconds: 1, revision: 7,
                viewSize: CGSize(width: 200, height: 400), displayScale: 2)
            await feed.settle()
        }

        #expect(log.seconds.count == 1, "an unchanged (time, revision, size) never re-renders")
    }

    // MARK: Frame-grid quantization

    @Test func quantizeSnapsSecondsToTheFrameGrid() {
        // Wall-clock ticks inside the same 30fps frame collapse to one value…
        let a = PreviewFeed.quantize(seconds: 0.500, fps: 30)
        let b = PreviewFeed.quantize(seconds: 0.512, fps: 30)
        #expect(a == b)
        // …and the next frame is a distinct grid point.
        let c = PreviewFeed.quantize(seconds: 0.517, fps: 30)
        #expect(c != a)
        #expect(near(c, 16.0 / 30.0, tolerance: 1e-9))

        // Degenerate rates pass through; negative times clamp like the engine.
        #expect(PreviewFeed.quantize(seconds: 1.23, fps: 0) == 1.23)
        #expect(PreviewFeed.quantize(seconds: -0.4, fps: 30) == 0)
    }

    @Test func quantizedRequestsDedupeSameFrameTicks() async throws {
        let log = RenderLog()
        let feed = PreviewFeed { seconds, _, _ in
            log.seconds.append(seconds)
            return Self.tinyImage()
        }

        // Simulated 16 ms playback ticks quantized to the 30fps grid, the way
        // PreviewCanvas issues them: two wall ticks per frame, so half the
        // requests match the delivered frame and are skipped.
        for tick in 0..<8 {
            let raw = Double(tick) * 0.016
            feed.request(
                seconds: PreviewFeed.quantize(seconds: raw, fps: 30), revision: 1,
                viewSize: CGSize(width: 200, height: 400), displayScale: 2)
            await feed.settle()
        }

        #expect(log.seconds == [0, 1.0 / 30.0, 2.0 / 30.0, 3.0 / 30.0])
    }

    @Test func timelineFPSCarriesFromTheEngine() async throws {
        let state = await makeProject([shortVideo])
        #expect(state.timelineFPS == 30, "sessions open at 30fps; ui_state carries it through")
    }

    private static func tinyImage() -> CGImage? {
        let context = CGContext(
            data: nil, width: 2, height: 2, bitsPerComponent: 8, bytesPerRow: 8,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        return context?.makeImage()
    }

    // MARK: Export

    @Test func exportJobWritesTheTimelineToAMovie() async throws {
        let state = await makeProject([shortVideo])

        // 240p at 24 fps: small enough to stay fast, still the full pipeline
        // (settings mapping, render, H.264/AAC encode, temp file).
        let job = try #require(await state.startExport(shortSide: 240, fps: 24))
        let frames = try await job.wait()
        #expect(frames == 96, "4 seconds at 24 fps")

        let attributes = try FileManager.default.attributesOfItem(atPath: job.outputPath)
        let bytes = (attributes[.size] as? Int) ?? 0
        #expect(bytes > 10_000, "the mp4 has real encoded payload")
        try? FileManager.default.removeItem(atPath: job.outputPath)
    }

    @Test func cancellingAnExportDeletesThePartialFile() async throws {
        let state = await makeProject([shortVideo, video])

        // Native size (no overrides) so there's enough work to cancel into.
        let job = try #require(await state.startExport())
        job.cancel()
        await #expect(throws: CutlassError.self) { try await job.wait() }
        #expect(!FileManager.default.fileExists(atPath: job.outputPath))
    }

    // MARK: Persistence (Phase H)

    @Test func flushedSaveRoundTripsThroughTheStore() async throws {
        let state = await makeProject([shortVideo])
        let id = state.mediaStore.projectID
        defer { ProjectStore.delete(id: id) }

        state.flushSave()
        await state.waitForEngine()

        // The store now lists the project with real card metadata.
        let entry = try #require(ProjectStore.entry(for: id))
        #expect(near(entry.durationSeconds, 4))
        #expect(FileManager.default.fileExists(atPath: entry.thumbnailFile.path))
        #expect(ProjectStore.list().contains { $0.id == id })

        // A fresh editor restores the session from the file.
        let reopened = EditorState()
        reopened.openProject(entry)
        await reopened.waitForEngine()
        #expect(reopened.clips.count == 1)
        #expect(near(reopened.clips[0].length, 4))
        #expect(reopened.mediaStore.projectID == id)
        #expect(!reopened.canUndo, "loading starts a fresh history")
    }

    @Test func editsAutoSaveWithoutAnExplicitFlush() async throws {
        let state = await makeProject([shortVideo])
        let id = state.mediaStore.projectID
        defer { ProjectStore.delete(id: id) }

        // The append scheduled a debounced save; give it its second plus
        // engine time, no flush call.
        try await Task.sleep(for: .milliseconds(1400))
        await state.waitForEngine()

        #expect(
            FileManager.default.fileExists(atPath: ProjectStore.projectFile(for: id).path),
            "the debounced auto-save wrote project.cutlass")
    }

    @Test func duplicatedProjectsRelinkOntoTheirOwnMediaCopies() async throws {
        let state = await makeProject([shortVideo])
        let id = state.mediaStore.projectID
        state.flushSave()
        await state.waitForEngine()
        defer { ProjectStore.delete(id: id) }

        let copy = try #require(ProjectStore.duplicate(id: id))
        defer { ProjectStore.delete(id: copy.id) }
        #expect(copy.name.hasSuffix("copy"))

        // Deleting the original before opening the copy proves the relink:
        // the file the duplicate's project.cutlass references is gone, so
        // only its own media/ copy can back the clip.
        ProjectStore.delete(id: id)

        let reopened = EditorState()
        reopened.openProject(copy)
        await reopened.waitForEngine()
        #expect(reopened.clips.count == 1)
        let mediaPath = try #require(reopened.clips[0].mediaPath)
        #expect(mediaPath.hasPrefix(ProjectStore.directory(for: copy.id).path))

        // The relinked media decodes: frame renders are non-nil.
        let frame = await reopened.renderPreviewFrame(atSeconds: 1, maxWidth: 160, maxHeight: 160)
        #expect(frame != nil)
    }
}
