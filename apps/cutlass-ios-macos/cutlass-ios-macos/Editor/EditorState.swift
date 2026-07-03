import SwiftUI

/// In-memory state for the mock editor: a single sequential video track.
/// All edits are pure array/state manipulation; nothing touches the engine.
@Observable
final class EditorState {
    var clips: [MockClip] = []
    var playhead: TimeInterval = 0
    var selectedClipID: MockClip.ID?

    var isPlaying = false {
        didSet {
            guard oldValue != isPlaying else { return }
            if isPlaying {
                startPlayback()
            } else {
                playbackTask?.cancel()
                playbackTask = nil
            }
        }
    }

    /// Timeline zoom: how many seconds one point of track width represents.
    var secondsPerPoint: Double = 1.0 / 44.0

    private var undoStack: [[MockClip]] = []
    private var redoStack: [[MockClip]] = []
    private var playbackTask: Task<Void, Never>?

    var canUndo: Bool { !undoStack.isEmpty }
    var canRedo: Bool { !redoStack.isEmpty }

    var isEmpty: Bool { clips.isEmpty }

    var duration: TimeInterval {
        clips.reduce(0) { $0 + $1.length }
    }

    var selectedClip: MockClip? {
        selectedClipID.flatMap { id in clips.first { $0.id == id } }
    }

    // MARK: Time <-> clip mapping

    /// Timeline start time of the given clip.
    func startTime(of id: MockClip.ID) -> TimeInterval {
        var start: TimeInterval = 0
        for clip in clips {
            if clip.id == id { break }
            start += clip.length
        }
        return start
    }

    /// The clip under a timeline position; the playhead is always clamped to
    /// the timeline, so positions at or past the end hold the last clip.
    func clip(at time: TimeInterval) -> MockClip? {
        var start: TimeInterval = 0
        for clip in clips {
            let end = start + clip.length
            if time < end { return clip }
            start = end
        }
        return clips.last
    }

    // MARK: Project lifecycle

    func startProject(with items: [MockMediaItem]) {
        isPlaying = false
        clips = items.map(MockClip.init(from:))
        playhead = 0
        selectedClipID = nil
        undoStack = []
        redoStack = []
    }

    func appendMedia(_ items: [MockMediaItem]) {
        guard !items.isEmpty else { return }
        pushUndoSnapshot()
        clips.append(contentsOf: items.map(MockClip.init(from:)))
    }

    // MARK: Transport

    func stepFrame(by direction: Double) {
        isPlaying = false
        playhead = min(max(0, playhead + direction / 30.0), duration)
    }

    /// Advances the playhead in wall-clock time until the timeline ends or
    /// playback is stopped (pause button, scrubbing, frame step).
    private func startPlayback() {
        if playhead >= duration {
            playhead = 0
        }
        playbackTask = Task { [weak self] in
            guard let self else { return }
            var lastTick = Date.now
            while !Task.isCancelled, self.isPlaying {
                try? await Task.sleep(for: .milliseconds(16))
                guard !Task.isCancelled else { return }

                let now = Date.now
                self.playhead = min(self.playhead + now.timeIntervalSince(lastTick), self.duration)
                lastTick = now

                if self.playhead >= self.duration {
                    self.isPlaying = false
                }
            }
        }
    }

    // MARK: Undo / redo (whole-timeline snapshots; plenty for mock edits)

    func undo() {
        guard let previous = undoStack.popLast() else { return }
        isPlaying = false
        redoStack.append(clips)
        clips = previous
        reconcileAfterHistoryJump()
    }

    func redo() {
        guard let next = redoStack.popLast() else { return }
        isPlaying = false
        undoStack.append(clips)
        clips = next
        reconcileAfterHistoryJump()
    }

    private func pushUndoSnapshot() {
        undoStack.append(clips)
        if undoStack.count > 50 {
            undoStack.removeFirst()
        }
        redoStack = []
    }

    private func reconcileAfterHistoryJump() {
        if let id = selectedClipID, !clips.contains(where: { $0.id == id }) {
            selectedClipID = nil
        }
        clampPlayhead()
    }

    // MARK: Edit operations (mock: pure array manipulation)

    /// Splits the clip under the playhead into two at the playhead. No-op
    /// within a frame of either clip edge, where a split would be degenerate.
    func splitAtPlayhead() {
        guard let clip = clip(at: playhead),
              let index = clips.firstIndex(where: { $0.id == clip.id })
        else { return }

        let local = playhead - startTime(of: clip.id)
        guard local >= MockClip.minDuration, local <= clip.length - MockClip.minDuration
        else { return }

        pushUndoSnapshot()

        var left = clip
        left.length = local

        var right = clip
        right.id = UUID()
        right.trimStart = clip.trimStart + local
        right.length = clip.length - local

        clips.replaceSubrange(index...index, with: [left, right])
        if selectedClipID == clip.id {
            selectedClipID = left.id
        }
    }

    func deleteSelected() {
        guard let id = selectedClipID else { return }
        pushUndoSnapshot()
        clips.removeAll { $0.id == id }
        selectedClipID = nil
        clampPlayhead()
    }

    /// Inserts a copy of the selected clip right after it.
    func duplicateSelected() {
        guard let id = selectedClipID,
              let index = clips.firstIndex(where: { $0.id == id })
        else { return }

        pushUndoSnapshot()
        var copy = clips[index]
        copy.id = UUID()
        clips.insert(copy, at: index + 1)
    }

    /// Swaps the selected clip's source for a picked library item, keeping
    /// its slot on the timeline.
    func replaceSelected(with item: MockMediaItem) {
        guard let id = selectedClipID,
              let index = clips.firstIndex(where: { $0.id == id })
        else { return }

        pushUndoSnapshot()
        let replacement = MockClip(from: item)
        clips[index] = replacement
        selectedClipID = replacement.id
        clampPlayhead()
    }

    // MARK: Trimming

    enum TrimEdge {
        case leading
        case trailing
    }

    /// Snapshot of the whole timeline taken at the first update of a trim
    /// gesture; committed to the undo stack when the gesture ends.
    private var trimGestureSnapshot: [MockClip]?

    /// Applies a trim drag. `anchor` is the clip as it was when the drag
    /// began, so each update computes from absolute math and never drifts.
    func trim(_ id: MockClip.ID, edge: TrimEdge, anchor: MockClip, by deltaSeconds: Double) {
        guard let index = clips.firstIndex(where: { $0.id == id }) else { return }

        if trimGestureSnapshot == nil {
            trimGestureSnapshot = clips
        }

        var clip = anchor
        switch edge {
        case .leading:
            let delta = min(
                max(deltaSeconds, -anchor.trimStart),
                anchor.length - MockClip.minDuration
            )
            clip.trimStart = anchor.trimStart + delta
            clip.length = anchor.length - delta
        case .trailing:
            let maxLength = anchor.sourceDuration - anchor.trimStart
            clip.length = min(
                max(anchor.length + deltaSeconds, MockClip.minDuration),
                maxLength
            )
        }
        clips[index] = clip
    }

    func endTrim() {
        if let snapshot = trimGestureSnapshot, snapshot != clips {
            undoStack.append(snapshot)
            redoStack = []
        }
        trimGestureSnapshot = nil
        clampPlayhead()
    }

    private func clampPlayhead() {
        playhead = min(max(0, playhead), duration)
    }
}
