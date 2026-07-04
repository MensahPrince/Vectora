import SwiftUI

/// One undoable state of the whole mock timeline.
nonisolated struct TimelineSnapshot: Equatable {
    var clips: [MockClip] = []
    var lanes: [MockLane] = [MockLane(kind: .video, isMain: true)]
    var overlays: [MockOverlayClip] = []
    var effects: [MockEffectClip] = []
    var audios: [MockAudioClip] = []
    var aspect: AspectRatio = .original
    var background = CanvasBackground()
}

/// What a timeline drag is carrying: a clip lifted off the main track or a
/// free-floating lane clip.
nonisolated enum DragContent: Equatable {
    case main(UUID)
    case lane(TimelineSelection)
}

/// Where a live drag would land if released right now. One resolution per
/// drag frame drives the landing ghost, the snap guide, the tooltip, and the
/// release commit, so the preview can never disagree with the drop (ported
/// from the desktop `resolve_clip_drag`).
nonisolated struct DragResolution: Equatable {
    enum Landing: Equatable {
        /// A free span on an existing lane.
        case land(laneID: UUID, start: TimeInterval)
        /// A new lane inserted at this row of the lane stack (hovering a
        /// foreign kind, a conflicting span, or outside the stack).
        case newLane(row: Int, start: TimeInterval)
        /// Magnetic insertion between main-track clips; commit shifts later
        /// clips right. The caret renders at `caretTime` in current space.
        case mainInsert(index: Int, caretTime: TimeInterval)
    }

    var landing: Landing
    /// Lane kind of the dragged content (styles the ghost / new lane).
    var kind: MockLane.Kind
    var length: TimeInterval
    /// Vertical guide line while a magnet candidate is engaged.
    var snapTime: TimeInterval?
    /// Releasing here recreates the current arrangement; commit skips it.
    var isNoop = false
}

/// In-memory state for the mock editor: an ordered lane stack (desktop
/// rules: one content kind per lane, no same-lane overlap, audio pinned to
/// the bottom) around a sequential magnetic main track.
/// All edits are pure array/state manipulation; nothing touches the engine.
@Observable
final class EditorState {
    var clips: [MockClip] = []
    /// The lane stack, top row first. Exactly one lane `isMain` (the
    /// sequential track backed by `clips`); audio lanes always sit last.
    var lanes: [MockLane] = [MockLane(kind: .video, isMain: true)]
    var overlayClips: [MockOverlayClip] = []
    var effectClips: [MockEffectClip] = []
    var audioClips: [MockAudioClip] = []
    var aspect: AspectRatio = .original
    var background = CanvasBackground()

    var playhead: TimeInterval = 0
    var selection: TimelineSelection?

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

    /// Magnet toggle: when on, trims and lane moves lock onto the playhead,
    /// clip edges, and timeline bounds.
    var magnetEnabled = true
    /// Time a live gesture is currently locked onto (drives the yellow
    /// guide line + snap haptic); nil when not snapped.
    var activeSnapTime: TimeInterval?

    private var undoStack: [TimelineSnapshot] = []
    private var redoStack: [TimelineSnapshot] = []
    private var playbackTask: Task<Void, Never>?

    var canUndo: Bool { !undoStack.isEmpty }
    var canRedo: Bool { !redoStack.isEmpty }

    var isEmpty: Bool {
        clips.isEmpty && overlayClips.isEmpty && effectClips.isEmpty && audioClips.isEmpty
    }

    /// End of the sequential main track.
    var mainDuration: TimeInterval {
        clips.reduce(0) { $0 + $1.length }
    }

    /// End of the whole timeline including floating lane clips.
    var duration: TimeInterval {
        var end = mainDuration
        for clip in overlayClips { end = max(end, clip.start + clip.length) }
        for clip in effectClips { end = max(end, clip.start + clip.length) }
        for clip in audioClips { end = max(end, clip.start + clip.length) }
        return end
    }

    // MARK: Selection accessors

    var selectedClip: MockClip? {
        guard case .main(let id) = selection else { return nil }
        return clips.first { $0.id == id }
    }

    var selectedOverlay: MockOverlayClip? {
        guard case .overlay(let id) = selection else { return nil }
        return overlayClips.first { $0.id == id }
    }

    var selectedEffect: MockEffectClip? {
        guard case .effect(let id) = selection else { return nil }
        return effectClips.first { $0.id == id }
    }

    var selectedAudio: MockAudioClip? {
        guard case .audio(let id) = selection else { return nil }
        return audioClips.first { $0.id == id }
    }

    // MARK: Time <-> clip mapping

    /// Timeline start time of the given main-track clip.
    func startTime(of id: MockClip.ID) -> TimeInterval {
        var start: TimeInterval = 0
        for clip in clips {
            if clip.id == id { break }
            start += clip.length
        }
        return start
    }

    /// The main-track clip under a timeline position. Holds the last frame at
    /// the exact end of the main track; nil past it (lane content may extend
    /// further and renders over the canvas background).
    func clip(at time: TimeInterval) -> MockClip? {
        var start: TimeInterval = 0
        for clip in clips {
            let end = start + clip.length
            if time < end { return clip }
            start = end
        }
        return time <= start + 0.001 ? clips.last : nil
    }

    /// Overlay clips visible at a timeline position (text, stickers, PiP).
    func overlays(at time: TimeInterval) -> [MockOverlayClip] {
        overlayClips.filter { time >= $0.start && time < $0.start + $0.length }
    }

    /// Effect bars active at a timeline position.
    func effects(at time: TimeInterval) -> [MockEffectClip] {
        effectClips.filter { time >= $0.start && time < $0.start + $0.length }
    }

    // MARK: Lane stack

    /// Row index of the main track in the lane stack.
    var mainLaneRow: Int {
        lanes.firstIndex(where: \.isMain) ?? 0
    }

    /// Row index of the first audio lane (== the audio floor); `lanes.count`
    /// when there is none.
    var audioFloorRow: Int {
        lanes.firstIndex(where: { $0.kind == .audio }) ?? lanes.count
    }

    /// Time spans of every clip on a lane, excluding one clip id.
    private func spans(on laneID: UUID, excluding excluded: UUID? = nil) -> [(start: TimeInterval, end: TimeInterval)] {
        var result: [(start: TimeInterval, end: TimeInterval)] = []
        for clip in overlayClips where clip.laneID == laneID && clip.id != excluded {
            result.append((clip.start, clip.start + clip.length))
        }
        for clip in effectClips where clip.laneID == laneID && clip.id != excluded {
            result.append((clip.start, clip.start + clip.length))
        }
        for clip in audioClips where clip.laneID == laneID && clip.id != excluded {
            result.append((clip.start, clip.start + clip.length))
        }
        return result
    }

    /// Whether `[start, start+length)` overlaps no clip on the lane
    /// (touching edges are fine, mirroring the engine's overlap rule).
    func spanIsFree(on laneID: UUID, start: TimeInterval, length: TimeInterval, excluding excluded: UUID? = nil) -> Bool {
        let end = start + length
        let epsilon = 0.001
        return !spans(on: laneID, excluding: excluded).contains { span in
            start < span.end - epsilon && span.start < end - epsilon
        }
    }

    /// Audio floor invariant (desktop `enforce_audio_floor`): every audio
    /// lane sinks below every visual lane, both groups keeping their order.
    private func enforceAudioFloor() {
        let visual = lanes.filter { $0.kind != .audio }
        let audio = lanes.filter { $0.kind == .audio }
        let ordered = visual + audio
        if ordered.map(\.id) != lanes.map(\.id) {
            lanes = ordered
        }
    }

    /// Drops lanes that no longer host any clip (the main track always
    /// stays, even when empty).
    private func pruneEmptyLanes() {
        lanes.removeAll { lane in
            !lane.isMain && lane.kind != .audio && spans(on: lane.id).isEmpty
        }
        // Audio lanes prune too, but only ever from the bottom block.
        lanes.removeAll { lane in
            lane.kind == .audio && spans(on: lane.id).isEmpty
        }
    }

    /// A lane of `kind` whose span at [start, start+length) is free —
    /// `preferred` first, then top-to-bottom — or a brand-new lane inserted
    /// at the kind's default row (video above main, generated kinds just
    /// above the audio floor, audio at the very bottom).
    @discardableResult
    func hostLane(for kind: MockLane.Kind, start: TimeInterval, length: TimeInterval, preferred: UUID? = nil) -> UUID {
        if let preferred,
           let lane = lanes.first(where: { $0.id == preferred }),
           !lane.isMain, lane.kind == kind,
           spanIsFree(on: lane.id, start: start, length: length) {
            return lane.id
        }
        if let lane = lanes.first(where: { !$0.isMain && $0.kind == kind && spanIsFree(on: $0.id, start: start, length: length) }) {
            return lane.id
        }
        let lane = MockLane(kind: kind)
        let row: Int
        switch kind {
        case .video: row = mainLaneRow
        case .audio: row = lanes.count
        case .text, .sticker, .effect: row = audioFloorRow
        }
        lanes.insert(lane, at: min(row, lanes.count))
        enforceAudioFloor()
        return lane.id
    }

    // MARK: Drag resolution (ported from the desktop resolve_clip_drag)

    /// (kind, length, origin lane id or nil for main, origin start) of the
    /// dragged content; nil when the ids are stale.
    private func dragProfile(of content: DragContent) -> (kind: MockLane.Kind, length: TimeInterval, laneID: UUID?, start: TimeInterval, clipID: UUID)? {
        switch content {
        case .main(let id):
            guard let clip = clips.first(where: { $0.id == id }) else { return nil }
            return (.video, clip.length, nil, startTime(of: id), id)
        case .lane(.overlay(let id)):
            guard let clip = overlayClips.first(where: { $0.id == id }) else { return nil }
            return (clip.laneKind, clip.length, clip.laneID, clip.start, id)
        case .lane(.effect(let id)):
            guard let clip = effectClips.first(where: { $0.id == id }) else { return nil }
            return (.effect, clip.length, clip.laneID, clip.start, id)
        case .lane(.audio(let id)):
            guard let clip = audioClips.first(where: { $0.id == id }) else { return nil }
            return (.audio, clip.length, clip.laneID, clip.start, id)
        case .lane(.main):
            return nil
        }
    }

    /// Resolves where a drag would land if released right now.
    ///
    /// - `desiredStart`: the floater's leading edge in timeline seconds.
    /// - `hoverRow`: lane-stack row under the finger; may be out of range
    ///   (above the first or below the last row).
    ///
    /// Policy (desktop `snap.rs`): the main row takes video insertions
    /// between clips; a same-kind lane with a free span lands there (magnet
    /// pulling both edges, and a snap that *causes* a conflict is dropped in
    /// favor of the free unsnapped spot); everything else — foreign kind,
    /// conflicting span, out of range — resolves to a new lane at the row,
    /// clamped so nothing but audio ever enters the audio floor.
    func resolveDrag(content: DragContent, desiredStart: TimeInterval, hoverRow: Int) -> DragResolution? {
        guard let profile = dragProfile(of: content) else { return nil }
        let desired = max(0, desiredStart)

        // Main-track magnet: a video clip over the main row inserts between
        // clips (midpoint rule); the commit opens the hole.
        if profile.kind == .video, lanes.indices.contains(hoverRow), lanes[hoverRow].isMain {
            var excludedIndex: Int?
            if case .main(let id) = content {
                excludedIndex = clips.firstIndex(where: { $0.id == id })
            }
            let insertion = mainInsertion(desired: desired, excludingIndex: excludedIndex)
            return DragResolution(
                landing: .mainInsert(index: insertion.index, caretTime: insertion.caretTime),
                kind: profile.kind,
                length: profile.length,
                snapTime: nil,
                isNoop: insertion.noop
            )
        }

        let exclusion: TimelineSelection? = {
            if case .lane(let selection) = content { return selection }
            return nil
        }()
        let candidates = laneSnapCandidates(excluding: exclusion)
        let snap = snappedDragStart(desired: desired, length: profile.length, candidates: candidates)

        if lanes.indices.contains(hoverRow) {
            let lane = lanes[hoverRow]
            if lane.kind == profile.kind, !lane.isMain {
                let sameSpot = lane.id == profile.laneID && abs(snap.start - profile.start) < 0.001
                if spanIsFree(on: lane.id, start: snap.start, length: profile.length, excluding: profile.clipID) {
                    return DragResolution(
                        landing: .land(laneID: lane.id, start: snap.start),
                        kind: profile.kind,
                        length: profile.length,
                        snapTime: snap.snapTime,
                        isNoop: sameSpot
                    )
                }
                // The snap pulled us into a conflict the raw position doesn't
                // have — prefer landing free without the magnet.
                if snap.snapTime != nil, snap.start != desired,
                   spanIsFree(on: lane.id, start: desired, length: profile.length, excluding: profile.clipID) {
                    return DragResolution(
                        landing: .land(laneID: lane.id, start: desired),
                        kind: profile.kind,
                        length: profile.length,
                        snapTime: nil,
                        isNoop: lane.id == profile.laneID && abs(desired - profile.start) < 0.001
                    )
                }
            }
        }

        // Foreign kind, conflicting span, or outside the stack: a new lane
        // inserted at the hovered row, clamped around the audio floor.
        let row: Int
        if profile.kind == .audio {
            row = min(max(hoverRow, audioFloorRow), lanes.count)
        } else {
            row = min(max(hoverRow, 0), audioFloorRow)
        }
        return DragResolution(
            landing: .newLane(row: row, start: snap.start),
            kind: profile.kind,
            length: profile.length,
            snapTime: snap.snapTime
        )
    }

    /// Applies a drag resolution on release: one undo step, no-ops skipped.
    func commitDrag(content: DragContent, resolution: DragResolution) {
        guard !resolution.isNoop, dragProfile(of: content) != nil else { return }
        pushUndoSnapshot()

        switch resolution.landing {
        case .land(let laneID, let start):
            place(content, onLane: laneID, at: start)
        case .newLane(let row, let start):
            let lane = MockLane(kind: resolution.kind)
            lanes.insert(lane, at: min(max(row, 0), lanes.count))
            enforceAudioFloor()
            place(content, onLane: lane.id, at: start)
        case .mainInsert(let index, _):
            insertIntoMain(content, at: index)
        }

        pruneEmptyLanes()
        clampPlayhead()
    }

    /// Moves the dragged content onto an existing lane at `start`. Main-track
    /// clips leave the sequential track and become free video-lane clips,
    /// keeping their look and audio (no identity change).
    private func place(_ content: DragContent, onLane laneID: UUID, at start: TimeInterval) {
        switch content {
        case .main(let id):
            guard let index = clips.firstIndex(where: { $0.id == id }) else { return }
            let clip = clips.remove(at: index)
            var lifted = MockOverlayClip(kind: .pip, laneID: laneID, start: max(0, start), length: clip.length)
            lifted.art = clip.art
            lifted.sourceDuration = clip.sourceDuration
            lifted.pipHasAudio = clip.hasAudio
            lifted.volume = clip.volume
            // Full-frame: leaving the main track must not shrink the clip.
            lifted.scale = 1
            lifted.posX = 0.5
            lifted.posY = 0.5
            overlayClips.append(lifted)
            selection = .overlay(lifted.id)
        case .lane(.overlay(let id)):
            guard let index = overlayClips.firstIndex(where: { $0.id == id }) else { return }
            overlayClips[index].laneID = laneID
            overlayClips[index].start = max(0, start)
        case .lane(.effect(let id)):
            guard let index = effectClips.firstIndex(where: { $0.id == id }) else { return }
            effectClips[index].laneID = laneID
            effectClips[index].start = max(0, start)
        case .lane(.audio(let id)):
            guard let index = audioClips.firstIndex(where: { $0.id == id }) else { return }
            audioClips[index].laneID = laneID
            audioClips[index].start = max(0, start)
        case .lane(.main):
            break
        }
    }

    /// Inserts the dragged content into the main track at `index` (already in
    /// post-removal space for reorders).
    private func insertIntoMain(_ content: DragContent, at index: Int) {
        switch content {
        case .main(let id):
            guard let from = clips.firstIndex(where: { $0.id == id }) else { return }
            let clip = clips.remove(at: from)
            clips.insert(clip, at: min(max(index, 0), clips.count))
        case .lane(.overlay(let id)):
            guard let overlayIndex = overlayClips.firstIndex(where: { $0.id == id }),
                  overlayClips[overlayIndex].kind == .pip,
                  let art = overlayClips[overlayIndex].art
            else { return }
            let overlay = overlayClips.remove(at: overlayIndex)
            let clip = MockClip(
                art: art,
                sourceDuration: overlay.sourceDuration ?? overlay.length,
                length: overlay.length,
                hasAudio: overlay.pipHasAudio
            )
            clips.insert(clip, at: min(max(index, 0), clips.count))
            selection = .main(clip.id)
        case .lane:
            break
        }
    }

    /// An insertion slot on the gapless main track for content whose left
    /// edge sits at `desired`: before the first clip whose midpoint lies
    /// right of it (crossing a clip's middle flips the caret to its other
    /// side), else after the last clip.
    struct MainInsertion {
        /// Array insertion index, in post-removal space for reorders.
        var index: Int
        /// Caret position in the track's current visual space.
        var caretTime: TimeInterval
        /// The slot is exactly where the excluded clip already sits.
        var noop: Bool
    }

    func mainInsertion(desired: TimeInterval, excludingIndex: Int? = nil) -> MainInsertion {
        var spans: [(start: TimeInterval, end: TimeInterval)] = []
        var excludedStart: TimeInterval?
        var boundary: TimeInterval = 0
        for (position, clip) in clips.enumerated() {
            let start = boundary
            boundary += clip.length
            if position == excludingIndex {
                excludedStart = start
            } else {
                spans.append((start, boundary))
            }
        }

        let index = spans.firstIndex { desired < ($0.start + $0.end) / 2 } ?? spans.count
        let noop = excludingIndex.map { $0 == index } ?? false

        let caretTime: TimeInterval
        if noop, let excludedStart {
            caretTime = excludedStart
        } else if index < spans.count {
            caretTime = spans[index].start
        } else {
            caretTime = spans.last?.end ?? 0
        }
        return MainInsertion(index: index, caretTime: caretTime, noop: noop)
    }

    /// Both edges of the dragged span magnet to the nearest candidate; the
    /// closest edge wins. A snap clamped away (below t=0) drops its guide.
    private func snappedDragStart(
        desired: TimeInterval,
        length: TimeInterval,
        candidates: [TimeInterval]
    ) -> (start: TimeInterval, snapTime: TimeInterval?) {
        guard magnetEnabled else { return (desired, nil) }
        let threshold = 8 * secondsPerPoint
        let end = desired + length
        var best: (distance: TimeInterval, start: TimeInterval, line: TimeInterval)?

        for candidate in candidates {
            let leading = abs(candidate - desired)
            if leading <= threshold, best.map({ leading < $0.distance }) ?? true {
                best = (leading, candidate, candidate)
            }
            let trailing = abs(candidate - end)
            if trailing <= threshold, best.map({ trailing < $0.distance }) ?? true {
                best = (trailing, candidate - length, candidate)
            }
        }

        guard let best else { return (desired, nil) }
        let start = max(0, best.start)
        return (start, start == best.start ? best.line : nil)
    }

    // MARK: Snapshots

    private var snapshot: TimelineSnapshot {
        get {
            TimelineSnapshot(
                clips: clips,
                lanes: lanes,
                overlays: overlayClips,
                effects: effectClips,
                audios: audioClips,
                aspect: aspect,
                background: background
            )
        }
        set {
            clips = newValue.clips
            lanes = newValue.lanes
            overlayClips = newValue.overlays
            effectClips = newValue.effects
            audioClips = newValue.audios
            aspect = newValue.aspect
            background = newValue.background
        }
    }

    private func pushUndoSnapshot() {
        // While a panel session is open the session snapshot owns undo; ops
        // triggered from inside the panel fold into one step on Apply.
        guard panelSnapshot == nil else { return }
        if let anchor = gestureSnapshot {
            // An op landing mid-gesture (cross-lane drop after a lift-move):
            // the gesture anchor is the truthful "before", and consuming it
            // folds the move + op into one undo step (endGesture then has
            // nothing left to push).
            undoStack.append(anchor)
            gestureSnapshot = nil
        } else {
            undoStack.append(snapshot)
        }
        if undoStack.count > 50 {
            undoStack.removeFirst()
        }
        redoStack = []
    }

    // MARK: Project lifecycle

    func startProject(with items: [MockMediaItem]) {
        isPlaying = false
        snapshot = TimelineSnapshot(clips: items.map(MockClip.init(from:)))
        playhead = 0
        selection = nil
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

    // MARK: Undo / redo

    func undo() {
        guard let previous = undoStack.popLast() else { return }
        isPlaying = false
        redoStack.append(snapshot)
        snapshot = previous
        reconcileAfterHistoryJump()
    }

    func redo() {
        guard let next = redoStack.popLast() else { return }
        isPlaying = false
        undoStack.append(snapshot)
        snapshot = next
        reconcileAfterHistoryJump()
    }

    private func reconcileAfterHistoryJump() {
        switch selection {
        case .main(let id) where !clips.contains(where: { $0.id == id }),
             .overlay(let id) where !overlayClips.contains(where: { $0.id == id }),
             .effect(let id) where !effectClips.contains(where: { $0.id == id }),
             .audio(let id) where !audioClips.contains(where: { $0.id == id }):
            selection = nil
        default:
            break
        }
        clampPlayhead()
    }

    // MARK: Panel edit sessions

    /// Property panels mutate state live so the preview reacts; the session
    /// snapshot makes Cancel restore and Apply undoable as one step.
    private var panelSnapshot: TimelineSnapshot?

    func beginPanelSession() {
        panelSnapshot = snapshot
    }

    func commitPanelSession() {
        if let before = panelSnapshot, before != snapshot {
            undoStack.append(before)
            redoStack = []
        }
        panelSnapshot = nil
    }

    func cancelPanelSession() {
        if let before = panelSnapshot {
            snapshot = before
        }
        panelSnapshot = nil
    }

    // MARK: Targeted mutation helpers (used by property panels)

    func updateSelectedClip(_ mutate: (inout MockClip) -> Void) {
        guard case .main(let id) = selection,
              let index = clips.firstIndex(where: { $0.id == id })
        else { return }
        mutate(&clips[index])
    }

    func updateSelectedOverlay(_ mutate: (inout MockOverlayClip) -> Void) {
        guard case .overlay(let id) = selection,
              let index = overlayClips.firstIndex(where: { $0.id == id })
        else { return }
        mutate(&overlayClips[index])
    }

    func updateSelectedEffect(_ mutate: (inout MockEffectClip) -> Void) {
        guard case .effect(let id) = selection,
              let index = effectClips.firstIndex(where: { $0.id == id })
        else { return }
        mutate(&effectClips[index])
    }

    func updateSelectedAudio(_ mutate: (inout MockAudioClip) -> Void) {
        guard case .audio(let id) = selection,
              let index = audioClips.firstIndex(where: { $0.id == id })
        else { return }
        mutate(&audioClips[index])
    }

    /// Changing speed rescales the clip's timeline length so the same source
    /// content plays faster or slower.
    func setSelectedSpeed(_ newSpeed: Double) {
        updateSelectedClip { clip in
            let content = clip.length * clip.speed
            clip.speed = newSpeed
            clip.length = max(MockClip.minDuration, content / newSpeed)
        }
    }

    // MARK: Adding lane content (all insert at the playhead and select)

    @discardableResult
    func addTextClip(text: String = "") -> UUID {
        pushUndoSnapshot()
        let start = insertionTime
        var clip = MockOverlayClip(kind: .text, laneID: hostLane(for: .text, start: start, length: 3), start: start, length: 3)
        clip.text = text
        clip.posY = 0.62
        overlayClips.append(clip)
        selection = .overlay(clip.id)
        return clip.id
    }

    @discardableResult
    func addSticker(symbol: String) -> UUID {
        pushUndoSnapshot()
        let start = insertionTime
        var clip = MockOverlayClip(kind: .sticker, laneID: hostLane(for: .sticker, start: start, length: 3), start: start, length: 3)
        clip.symbol = symbol
        clip.posY = 0.35
        overlayClips.append(clip)
        selection = .overlay(clip.id)
        return clip.id
    }

    @discardableResult
    func addPip(from item: MockMediaItem) -> UUID {
        pushUndoSnapshot()
        let length = item.videoDuration ?? MockClip.photoDefaultDuration
        let start = insertionTime
        var clip = MockOverlayClip(kind: .pip, laneID: hostLane(for: .video, start: start, length: length), start: start, length: length)
        clip.art = item.art
        clip.sourceDuration = item.videoDuration ?? MockClip.photoMaxDuration
        clip.pipHasAudio = item.videoDuration != nil
        clip.scale = 0.5
        clip.posY = 0.32
        overlayClips.append(clip)
        selection = .overlay(clip.id)
        return clip.id
    }

    @discardableResult
    func addEffectClip(name: String, kind: MockEffectClip.Kind) -> UUID {
        pushUndoSnapshot()
        let start = insertionTime
        let clip = MockEffectClip(kind: kind, laneID: hostLane(for: .effect, start: start, length: 3), name: name, start: start, length: 3)
        effectClips.append(clip)
        selection = .effect(clip.id)
        return clip.id
    }

    @discardableResult
    func addAudio(kind: MockAudioClip.Kind, title: String, duration: TimeInterval) -> UUID {
        pushUndoSnapshot()
        let start = insertionTime
        let clip = MockAudioClip(
            kind: kind,
            laneID: hostLane(for: .audio, start: start, length: duration),
            title: title,
            start: start,
            length: duration,
            sourceDuration: duration
        )
        audioClips.append(clip)
        selection = .audio(clip.id)
        return clip.id
    }

    /// Lane content lands at the playhead, clamped inside the timeline.
    private var insertionTime: TimeInterval {
        max(0, min(playhead, max(duration - 0.5, 0)))
    }

    // MARK: Edit operations

    /// Reorders a main-track clip (drag-to-reorder drop).
    func moveClip(fromIndex: Int, toIndex: Int) {
        guard clips.indices.contains(fromIndex),
              clips.indices.contains(toIndex),
              fromIndex != toIndex
        else { return }
        pushUndoSnapshot()
        let clip = clips.remove(at: fromIndex)
        clips.insert(clip, at: toIndex)
    }

    // MARK: Cross-lane moves (drag a clip onto another lane)

    /// Pulls a clip off the main track onto a free video lane at `start`
    /// (snap-aware). It stays a full-frame video clip — same art, audio, and
    /// scale — just free-floating now.
    func moveMainClipToLane(_ id: UUID, at start: TimeInterval) {
        guard clips.contains(where: { $0.id == id }) else { return }
        pushUndoSnapshot()

        var dropStart = max(0, start)
        if let snapped = snapTime(near: dropStart, candidates: laneSnapCandidates(excluding: nil)) {
            dropStart = max(0, snapped)
        }
        guard let profile = dragProfile(of: .main(id)) else { return }
        let laneID = hostLane(for: .video, start: dropStart, length: profile.length)
        place(.main(id), onLane: laneID, at: dropStart)
        pruneEmptyLanes()
        clampPlayhead()
    }

    /// Drops a video-lane clip into the main track at the midpoint-rule
    /// insertion slot nearest `time`.
    func moveLaneClipToMain(_ id: UUID, at time: TimeInterval) {
        guard let clip = overlayClips.first(where: { $0.id == id }), clip.kind == .pip else { return }
        pushUndoSnapshot()
        insertIntoMain(.lane(.overlay(id)), at: mainInsertion(desired: time).index)
        pruneEmptyLanes()
        clampPlayhead()
    }

    /// Splits whatever is selected at the playhead; with no selection, splits
    /// the main-track clip under the playhead.
    func splitAtPlayhead() {
        switch selection {
        case .overlay(let id):
            if let index = overlayClips.firstIndex(where: { $0.id == id }),
               let (left, right) = splitRange(overlayClips[index].start, overlayClips[index].length) {
                pushUndoSnapshot()
                overlayClips[index].length = left
                var second = overlayClips[index]
                second.id = UUID()
                second.start = playhead
                second.length = right
                overlayClips.insert(second, at: index + 1)
            }
        case .effect(let id):
            if let index = effectClips.firstIndex(where: { $0.id == id }),
               let (left, right) = splitRange(effectClips[index].start, effectClips[index].length) {
                pushUndoSnapshot()
                effectClips[index].length = left
                var second = effectClips[index]
                second.id = UUID()
                second.start = playhead
                second.length = right
                effectClips.insert(second, at: index + 1)
            }
        case .audio(let id):
            if let index = audioClips.firstIndex(where: { $0.id == id }),
               let (left, right) = splitRange(audioClips[index].start, audioClips[index].length) {
                pushUndoSnapshot()
                audioClips[index].length = left
                var second = audioClips[index]
                second.id = UUID()
                second.start = playhead
                second.length = right
                second.waveSeed = Int.random(in: 0..<10_000)
                audioClips.insert(second, at: index + 1)
            }
        default:
            splitMainAtPlayhead()
        }
    }

    /// Left/right lengths if the playhead splits the range non-degenerately.
    private func splitRange(_ start: TimeInterval, _ length: TimeInterval) -> (TimeInterval, TimeInterval)? {
        let local = playhead - start
        guard local >= MockClip.minDuration, local <= length - MockClip.minDuration else { return nil }
        return (local, length - local)
    }

    private func splitMainAtPlayhead() {
        guard let clip = clip(at: playhead),
              let index = clips.firstIndex(where: { $0.id == clip.id })
        else { return }

        let local = playhead - startTime(of: clip.id)
        guard local >= MockClip.minDuration, local <= clip.length - MockClip.minDuration
        else { return }

        pushUndoSnapshot()

        var left = clip
        left.length = local
        left.transitionAfter = nil

        var right = clip
        right.id = UUID()
        right.trimStart = clip.trimStart + local
        right.length = clip.length - local

        clips.replaceSubrange(index...index, with: [left, right])
        if selection == .main(clip.id) {
            selection = .main(left.id)
        }
    }

    func deleteSelected() {
        switch selection {
        case .main(let id):
            pushUndoSnapshot()
            clips.removeAll { $0.id == id }
        case .overlay(let id):
            pushUndoSnapshot()
            overlayClips.removeAll { $0.id == id }
        case .effect(let id):
            pushUndoSnapshot()
            effectClips.removeAll { $0.id == id }
        case .audio(let id):
            pushUndoSnapshot()
            audioClips.removeAll { $0.id == id }
        case nil:
            return
        }
        selection = nil
        pruneEmptyLanes()
        clampPlayhead()
    }

    /// Inserts a copy of the selected clip right after it (in time for lane
    /// clips, in order for main-track clips).
    func duplicateSelected() {
        switch selection {
        case .main(let id):
            guard let index = clips.firstIndex(where: { $0.id == id }) else { return }
            pushUndoSnapshot()
            var copy = clips[index]
            copy.id = UUID()
            clips.insert(copy, at: index + 1)
        case .overlay(let id):
            guard let index = overlayClips.firstIndex(where: { $0.id == id }) else { return }
            pushUndoSnapshot()
            var copy = overlayClips[index]
            copy.id = UUID()
            copy.start += copy.length
            copy.laneID = hostLane(for: copy.laneKind, start: copy.start, length: copy.length, preferred: copy.laneID)
            overlayClips.append(copy)
            selection = .overlay(copy.id)
        case .effect(let id):
            guard let index = effectClips.firstIndex(where: { $0.id == id }) else { return }
            pushUndoSnapshot()
            var copy = effectClips[index]
            copy.id = UUID()
            copy.start += copy.length
            copy.laneID = hostLane(for: .effect, start: copy.start, length: copy.length, preferred: copy.laneID)
            effectClips.append(copy)
            selection = .effect(copy.id)
        case .audio(let id):
            guard let index = audioClips.firstIndex(where: { $0.id == id }) else { return }
            pushUndoSnapshot()
            var copy = audioClips[index]
            copy.id = UUID()
            copy.start += copy.length
            copy.laneID = hostLane(for: .audio, start: copy.start, length: copy.length, preferred: copy.laneID)
            copy.waveSeed = Int.random(in: 0..<10_000)
            audioClips.append(copy)
            selection = .audio(copy.id)
        case nil:
            return
        }
    }

    /// Swaps the selected clip's source for a picked library item, keeping
    /// its slot on the timeline. Works for main clips and PiP overlays.
    func replaceSelected(with item: MockMediaItem) {
        switch selection {
        case .main(let id):
            guard let index = clips.firstIndex(where: { $0.id == id }) else { return }
            pushUndoSnapshot()
            let replacement = MockClip(from: item)
            clips[index] = replacement
            selection = .main(replacement.id)
        case .overlay(let id):
            guard let index = overlayClips.firstIndex(where: { $0.id == id }),
                  overlayClips[index].kind == .pip
            else { return }
            pushUndoSnapshot()
            overlayClips[index].art = item.art
            overlayClips[index].sourceDuration = item.videoDuration ?? MockClip.photoMaxDuration
            overlayClips[index].length = min(
                overlayClips[index].length,
                overlayClips[index].sourceDuration ?? .greatestFiniteMagnitude
            )
        default:
            return
        }
        clampPlayhead()
    }

    // MARK: Quick ops

    func setTransition(after clipID: MockClip.ID, _ transition: MockTransition?) {
        guard let index = clips.firstIndex(where: { $0.id == clipID }) else { return }
        pushUndoSnapshot()
        clips[index].transitionAfter = transition
    }

    /// Stamps the same transition on every interior boundary.
    func applyTransitionToAll(_ transition: MockTransition?) {
        guard clips.count > 1 else { return }
        pushUndoSnapshot()
        for index in clips.indices.dropLast() {
            clips[index].transitionAfter = transition
        }
    }

    /// Stamps or removes a keyframe on the selected main clip at the playhead.
    func toggleKeyframeAtPlayhead() {
        guard case .main(let id) = selection,
              let index = clips.firstIndex(where: { $0.id == id })
        else { return }

        let local = playhead - startTime(of: id)
        guard local >= 0, local <= clips[index].length else { return }

        pushUndoSnapshot()
        if let existing = clips[index].keyframes.firstIndex(where: { abs($0 - local) < 0.15 }) {
            clips[index].keyframes.remove(at: existing)
        } else {
            clips[index].keyframes.append(local)
            clips[index].keyframes.sort()
        }
    }

    func reverseSelected() {
        guard case .main = selection else { return }
        pushUndoSnapshot()
        updateSelectedClip { $0.isReversed.toggle() }
    }

    /// Inserts a 3-second freeze-frame segment at the playhead inside the
    /// selected clip (or before/after it when the playhead sits at an edge).
    func freezeFrame() {
        guard case .main(let id) = selection,
              let index = clips.firstIndex(where: { $0.id == id })
        else { return }

        let clip = clips[index]
        let local = playhead - startTime(of: id)
        guard local >= -0.001, local <= clip.length + 0.001 else { return }

        pushUndoSnapshot()

        var freeze = clip
        freeze.id = UUID()
        freeze.isFreeze = true
        freeze.hasAudio = false
        freeze.length = 3
        freeze.speed = 1
        freeze.keyframes = []
        freeze.transitionAfter = nil

        if local < MockClip.minDuration {
            clips.insert(freeze, at: index)
        } else if local > clip.length - MockClip.minDuration {
            clips.insert(freeze, at: index + 1)
        } else {
            var left = clip
            left.length = local
            left.transitionAfter = nil
            var right = clip
            right.id = UUID()
            right.trimStart = clip.trimStart + local
            right.length = clip.length - local
            clips.replaceSubrange(index...index, with: [left, freeze, right])
        }
    }

    /// Adds an "extracted audio" lane clip aligned with the selected clip and
    /// mutes the original.
    func extractAudio() {
        guard case .main(let id) = selection,
              let index = clips.firstIndex(where: { $0.id == id }),
              clips[index].hasAudio
        else { return }

        pushUndoSnapshot()
        let clip = clips[index]
        let start = startTime(of: id)
        let extracted = MockAudioClip(
            kind: .extracted,
            laneID: hostLane(for: .audio, start: start, length: clip.length),
            title: "Extracted audio",
            start: start,
            length: clip.length,
            sourceDuration: clip.length
        )
        audioClips.append(extracted)
        clips[index].volume = 0
    }

    // MARK: Snap engine

    /// Times a dragged lane edge can lock onto: timeline bounds, the
    /// playhead, main-track boundaries, and every other lane clip's edges.
    private func laneSnapCandidates(excluding target: TimelineSelection?) -> [TimeInterval] {
        var times: [TimeInterval] = [0, playhead]
        var boundary: TimeInterval = 0
        for clip in clips {
            boundary += clip.length
            times.append(boundary)
        }
        for clip in overlayClips where target != .overlay(clip.id) {
            times.append(clip.start)
            times.append(clip.start + clip.length)
        }
        for clip in effectClips where target != .effect(clip.id) {
            times.append(clip.start)
            times.append(clip.start + clip.length)
        }
        for clip in audioClips where target != .audio(clip.id) {
            times.append(clip.start)
            times.append(clip.start + clip.length)
        }
        return times
    }

    /// Candidates for a main-track trailing trim: boundaries at or after the
    /// dragged edge shift with the trim, so only earlier ones are stable.
    private func mainSnapCandidates(beforeClipAt index: Int) -> [TimeInterval] {
        var times: [TimeInterval] = [0, playhead]
        var boundary: TimeInterval = 0
        for clip in clips.prefix(index) {
            boundary += clip.length
            times.append(boundary)
        }
        for clip in overlayClips {
            times.append(clip.start)
            times.append(clip.start + clip.length)
        }
        for clip in effectClips {
            times.append(clip.start)
            times.append(clip.start + clip.length)
        }
        for clip in audioClips {
            times.append(clip.start)
            times.append(clip.start + clip.length)
        }
        return times
    }

    /// Nearest candidate within the (zoom-aware) threshold, or nil.
    private func snapTime(near time: TimeInterval, candidates: [TimeInterval]) -> TimeInterval? {
        guard magnetEnabled else { return nil }
        let threshold = 8 * secondsPerPoint
        guard let best = candidates.min(by: { abs($0 - time) < abs($1 - time) }),
              abs(best - time) <= threshold
        else { return nil }
        return best
    }

    // MARK: Trim / move gestures (main + lane clips)

    enum TrimEdge {
        case leading
        case trailing
    }

    /// Snapshot taken at the first update of a trim/move gesture; committed
    /// to the undo stack when the gesture ends.
    private var gestureSnapshot: TimelineSnapshot?

    private func beginGestureIfNeeded() {
        if gestureSnapshot == nil {
            gestureSnapshot = snapshot
        }
    }

    func endGesture() {
        if let before = gestureSnapshot, before != snapshot {
            undoStack.append(before)
            redoStack = []
        }
        gestureSnapshot = nil
        activeSnapTime = nil
        clampPlayhead()
    }

    /// Applies a trim drag to a main-track clip. `anchor` is the clip as it
    /// was when the drag began, so updates compute from absolute math.
    func trim(_ id: MockClip.ID, edge: TrimEdge, anchor: MockClip, by deltaSeconds: Double) {
        guard let index = clips.firstIndex(where: { $0.id == id }) else { return }
        beginGestureIfNeeded()

        var clip = anchor
        switch edge {
        case .leading:
            // The clip's timeline start is pinned by the clips before it, so
            // there is no stable edge to snap; just clamp to the source.
            activeSnapTime = nil
            let delta = min(
                max(deltaSeconds, -anchor.trimStart),
                anchor.length - MockClip.minDuration
            )
            clip.trimStart = anchor.trimStart + delta
            clip.length = anchor.length - delta
        case .trailing:
            let start = startTime(of: id)
            let maxLength = anchor.sourceDuration - anchor.trimStart
            var newLength = min(
                max(anchor.length + deltaSeconds, MockClip.minDuration),
                maxLength
            )
            if let snapped = snapTime(
                near: start + newLength,
                candidates: mainSnapCandidates(beforeClipAt: index)
            ), snapped - start >= MockClip.minDuration, snapped - start <= maxLength {
                newLength = snapped - start
                activeSnapTime = snapped
            } else {
                activeSnapTime = nil
            }
            clip.length = newLength
        }
        clips[index] = clip
    }

    /// The lane and clip id behind a lane-clip selection (nil for main).
    private func laneAddress(of target: TimelineSelection) -> (laneID: UUID, clipID: UUID)? {
        switch target {
        case .overlay(let id):
            return overlayClips.first(where: { $0.id == id }).map { ($0.laneID, id) }
        case .effect(let id):
            return effectClips.first(where: { $0.id == id }).map { ($0.laneID, id) }
        case .audio(let id):
            return audioClips.first(where: { $0.id == id }).map { ($0.laneID, id) }
        case .main:
            return nil
        }
    }

    /// Shared trim math for free-floating lane clips: leading trims move the
    /// start (end pinned), trailing trims change the length. Edges clamp to
    /// the lane's neighbor clips so a trim can never create an overlap, and
    /// snap to timeline landmarks when the magnet is on — a snap the clamp
    /// rejects is dropped rather than shown lying.
    private func trimmedRange(
        target: TimelineSelection,
        edge: TrimEdge,
        start: TimeInterval,
        length: TimeInterval,
        delta: TimeInterval,
        maxLength: TimeInterval?
    ) -> (start: TimeInterval, length: TimeInterval) {
        let candidates = laneSnapCandidates(excluding: target)
        let end = start + length

        // Nearest same-lane neighbors around the anchored range.
        var previousEnd: TimeInterval = 0
        var nextStart: TimeInterval = .greatestFiniteMagnitude
        if let address = laneAddress(of: target) {
            for span in spans(on: address.laneID, excluding: address.clipID) {
                if span.end <= start + 0.001 {
                    previousEnd = max(previousEnd, span.end)
                }
                if span.start >= end - 0.001 {
                    nextStart = min(nextStart, span.start)
                }
            }
        }

        switch edge {
        case .leading:
            let minStart = max(previousEnd, max(0, maxLength.map { end - $0 } ?? 0))
            let maxStart = end - MockClip.minDuration
            var newStart = min(max(start + delta, minStart), maxStart)
            if let snapped = snapTime(near: newStart, candidates: candidates),
               snapped >= minStart, snapped <= maxStart {
                newStart = snapped
                activeSnapTime = snapped
            } else {
                activeSnapTime = nil
            }
            return (newStart, end - newStart)
        case .trailing:
            let lengthCap = min(maxLength ?? .greatestFiniteMagnitude, nextStart - start)
            var newLength = min(max(length + delta, MockClip.minDuration), lengthCap)
            if let snapped = snapTime(near: start + newLength, candidates: candidates),
               snapped - start >= MockClip.minDuration,
               snapped - start <= lengthCap {
                newLength = snapped - start
                activeSnapTime = snapped
            } else {
                activeSnapTime = nil
            }
            return (start, newLength)
        }
    }

    /// Applies a trim drag to a lane clip, anchored at the range captured
    /// when the gesture began. PiP and audio clips clamp to their source.
    func trimLaneClip(
        _ target: TimelineSelection,
        edge: TrimEdge,
        anchorStart: TimeInterval,
        anchorLength: TimeInterval,
        by delta: TimeInterval
    ) {
        beginGestureIfNeeded()
        switch target {
        case .overlay(let id):
            guard let index = overlayClips.firstIndex(where: { $0.id == id }) else { return }
            let limit = overlayClips[index].kind == .pip ? overlayClips[index].sourceDuration : nil
            let range = trimmedRange(target: target, edge: edge, start: anchorStart, length: anchorLength, delta: delta, maxLength: limit)
            overlayClips[index].start = range.start
            overlayClips[index].length = range.length
        case .effect(let id):
            guard let index = effectClips.firstIndex(where: { $0.id == id }) else { return }
            let range = trimmedRange(target: target, edge: edge, start: anchorStart, length: anchorLength, delta: delta, maxLength: nil)
            effectClips[index].start = range.start
            effectClips[index].length = range.length
        case .audio(let id):
            guard let index = audioClips.firstIndex(where: { $0.id == id }) else { return }
            let range = trimmedRange(target: target, edge: edge, start: anchorStart, length: anchorLength, delta: delta, maxLength: audioClips[index].sourceDuration)
            audioClips[index].start = range.start
            audioClips[index].length = range.length
        case .main:
            break
        }
    }

    /// Canvas drag of an overlay clip to a new normalized position; gently
    /// snaps to the frame center on each axis.
    func dragOverlay(_ id: UUID, anchorX: Double, anchorY: Double, deltaX: Double, deltaY: Double) {
        guard let index = overlayClips.firstIndex(where: { $0.id == id }) else { return }
        beginGestureIfNeeded()
        var x = min(max(anchorX + deltaX, 0.03), 0.97)
        var y = min(max(anchorY + deltaY, 0.03), 0.97)
        if magnetEnabled {
            if abs(x - 0.5) < 0.02 { x = 0.5 }
            if abs(y - 0.5) < 0.02 { y = 0.5 }
        }
        overlayClips[index].posX = x
        overlayClips[index].posY = y
    }

    /// Scale/rotate an overlay from its corner grip.
    func transformOverlay(_ id: UUID, anchorScale: Double, anchorRotation: Double, scaleFactor: Double, rotationDelta: Double) {
        guard let index = overlayClips.firstIndex(where: { $0.id == id }) else { return }
        beginGestureIfNeeded()
        overlayClips[index].scale = min(max(anchorScale * scaleFactor, 0.25), 4)
        overlayClips[index].rotationDegrees = anchorRotation + rotationDelta
    }

    /// Horizontal drag of a lane clip to a new start time; either edge can
    /// lock onto a snap candidate.
    func moveLaneClip(_ selectionCase: TimelineSelection, anchorStart: TimeInterval, by delta: TimeInterval) {
        beginGestureIfNeeded()

        let length: TimeInterval
        switch selectionCase {
        case .overlay(let id):
            guard let clip = overlayClips.first(where: { $0.id == id }) else { return }
            length = clip.length
        case .effect(let id):
            guard let clip = effectClips.first(where: { $0.id == id }) else { return }
            length = clip.length
        case .audio(let id):
            guard let clip = audioClips.first(where: { $0.id == id }) else { return }
            length = clip.length
        case .main:
            return
        }

        var newStart = max(0, anchorStart + delta)
        let candidates = laneSnapCandidates(excluding: selectionCase)
        if let snapped = snapTime(near: newStart, candidates: candidates) {
            newStart = snapped
            activeSnapTime = snapped
        } else if let snapped = snapTime(near: newStart + length, candidates: candidates),
                  snapped - length >= 0 {
            newStart = snapped - length
            activeSnapTime = snapped
        } else {
            activeSnapTime = nil
        }

        switch selectionCase {
        case .overlay(let id):
            if let index = overlayClips.firstIndex(where: { $0.id == id }) {
                overlayClips[index].start = newStart
            }
        case .effect(let id):
            if let index = effectClips.firstIndex(where: { $0.id == id }) {
                effectClips[index].start = newStart
            }
        case .audio(let id):
            if let index = audioClips.firstIndex(where: { $0.id == id }) {
                audioClips[index].start = newStart
            }
        case .main:
            break
        }
    }

    private func clampPlayhead() {
        playhead = min(max(0, playhead), duration)
    }
}
