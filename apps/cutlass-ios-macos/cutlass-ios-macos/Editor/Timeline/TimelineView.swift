import SwiftUI

/// The scrubbable timeline: a fixed center playhead over horizontally
/// scrolling content, where scroll offset IS the current time. Renders the
/// effects lane above the main track and overlay/audio lanes below it;
/// overlapping lane clips pack into sub-rows.
struct TimelineView: View {
    var state: EditorState
    /// User-chosen height (from the grab bar above the transport row);
    /// nil = fit content. Clamped to [minHeight, natural stack height].
    var userHeight: CGFloat?
    var onAddMedia: () -> Void
    var onTransitionTap: (UUID) -> Void = { _ in }

    /// seconds-per-point captured when a pinch begins.
    @State private var pinchBase: Double?
    /// Live long-press drag-to-reorder on the main track (square sort mode).
    @State private var reorder: ReorderDrag?
    /// Live long-press lift of a lane clip (2D drag; may convert on drop).
    @State private var laneLift: (selection: TimelineSelection, overMain: Bool)?
    /// Bumped on every successful cross-lane conversion (success haptic).
    @State private var conversionPulse = 0
    /// How far the lane stack is panned up inside its viewport.
    @State private var laneOffset: CGFloat = 0
    /// Playhead + lane offset captured when a timeline pan begins.
    @State private var panAnchor: (playhead: TimeInterval, lane: CGFloat)?
    /// Direction lock for the live pan, decided from the first translation.
    @State private var panMode: PanMode?
    /// Post-flick momentum; cancelled by any new touch or playback.
    @State private var decayTask: Task<Void, Never>?
    /// The timeline's frame in screen space; drag locations arrive in the
    /// global coordinate space, so band/time hit-testing needs this origin.
    @State private var globalFrame: CGRect = .zero

    private enum PanMode {
        case scrub, lanes
    }

    private struct ReorderDrag: Equatable {
        var clipID: UUID
        var fromIndex: Int
        /// Keeps the square strip anchored where the lifted clip's leading
        /// edge was in time layout, so the morph doesn't jump.
        var stripShift: CGFloat
        var translation: CGSize = .zero
        var location: CGPoint = .zero
        var insertionIndex: Int
        /// Finger is over the overlay band: drop converts to a PiP overlay.
        var overOverlayBand = false
    }

    /// Vertical band a finger can hover during a cross-lane drag, in the
    /// lane stack's own coordinates (shared with `timelineHeight`).
    private struct BandLayout {
        var effects: ClosedRange<CGFloat>
        var main: ClosedRange<CGFloat>
        var overlay: ClosedRange<CGFloat>
        var audio: ClosedRange<CGFloat>?
    }

    private static let rowSpacing: CGFloat = 5
    private static let rulerHeight: CGFloat = 18
    private static let emptyLaneHeight: CGFloat = 20

    private var pointsPerSecond: Double { 1 / state.secondsPerPoint }

    // MARK: Lane packing (recomputed per render; tiny n)

    private var packedEffects: [(item: MockEffectClip, row: Int)] {
        packLaneRows(state.effectClips, maxRows: 2, start: \.start, length: \.length)
    }

    private var packedOverlays: [(item: MockOverlayClip, row: Int)] {
        packLaneRows(state.overlayClips, maxRows: 3, start: \.start, length: \.length)
    }

    private var packedAudio: [(item: MockAudioClip, row: Int)] {
        packLaneRows(state.audioClips, maxRows: 2, start: \.start, length: \.length)
    }

    /// Compact height: ruler + main track only.
    static let minHeight: CGFloat = 118

    var body: some View {
        let effects = packedEffects
        let overlays = packedOverlays
        let audio = packedAudio
        let effectRows = effects.map { $0.row + 1 }.max() ?? 0
        let overlayRows = overlays.map { $0.row + 1 }.max() ?? 0
        let audioRows = audio.map { $0.row + 1 }.max() ?? 0
        let naturalHeight = timelineHeight(effectRows: effectRows, overlayRows: overlayRows, audioRows: audioRows)
        let displayHeight = min(max(userHeight ?? naturalHeight, Self.minHeight), max(naturalHeight, Self.minHeight))

        GeometryReader { geometry in
            let halfWidth = geometry.size.width / 2
            let contentWidth = max(0, state.duration * pointsPerSecond)
            let lanesViewport = max(displayHeight - Self.rulerHeight - Self.rowSpacing, 40)
            let lanesNatural = naturalHeight - Self.rulerHeight - Self.rowSpacing
            let maxLaneOffset = max(0, lanesNatural - lanesViewport)
            let effectiveLaneOffset = min(laneOffset, maxLaneOffset)

            // topLeading matters: .top would horizontally CENTER the
            // wider-than-viewport content before the -playhead offset
            // applies, silently shifting visual time away from the model.
            ZStack(alignment: .topLeading) {
                // Not a ScrollView: the content offsets by -playhead so the
                // fixed center line IS the playhead, and one drag gesture on
                // the whole surface owns panning (horizontal = scrub,
                // vertical = lane browse). UIScrollView's pan recognizer
                // raced the clip gestures and silently dropped pans that
                // started on clips, which is why scrubbing only worked from
                // the ruler.
                VStack(alignment: .leading, spacing: Self.rowSpacing) {
                    TimeRuler(duration: state.duration, pointsPerSecond: pointsPerSecond)
                        .frame(width: contentWidth, height: Self.rulerHeight, alignment: .leading)
                        .clipped()

                    VStack(alignment: .leading, spacing: Self.rowSpacing) {
                        Group {
                            if effectRows == 0 {
                                Color.clear.frame(height: Self.emptyLaneHeight)
                            } else {
                                laneRows(
                                    rowCount: effectRows,
                                    width: contentWidth,
                                    entries: effects,
                                    liftedID: liftedLaneClipID,
                                    view: effectClipView(_:)
                                )
                            }
                        }
                        .opacity(reorder == nil ? 1 : 0.35)

                        ZStack(alignment: .leading) {
                            track
                            transitionBoundaries
                        }
                        // Band tint above the clips (the background band is
                        // fully covered by them) while a PiP hovers here.
                        .overlay {
                            if laneLift?.overMain == true {
                                Rectangle()
                                    .fill(Theme.accent.opacity(0.28))
                                    .allowsHitTesting(false)
                            }
                        }
                        // The lifted square renders above the lane rows it is
                        // dragged across.
                        .zIndex(reorder != nil ? 5 : 0)

                        Group {
                            if overlayRows == 0 {
                                Color.clear.frame(height: Self.emptyLaneHeight)
                            } else {
                                laneRows(
                                    rowCount: overlayRows,
                                    width: contentWidth,
                                    entries: overlays,
                                    liftedID: liftedLaneClipID,
                                    view: overlayClipView(_:)
                                )
                            }
                        }
                        .opacity(reorder == nil ? 1 : 0.35)

                        if audioRows > 0 {
                            laneRows(
                                rowCount: audioRows,
                                width: contentWidth,
                                entries: audio,
                                liftedID: liftedLaneClipID,
                                view: audioClipView(_:)
                            )
                            .opacity(reorder == nil ? 1 : 0.35)
                        }
                    }
                    // Bands pan with the lane stack so they always line up
                    // with their rows.
                    .background(alignment: .top) {
                        laneBackground(effectRows: effectRows, overlayRows: overlayRows, audioRows: audioRows)
                    }
                    .offset(y: -effectiveLaneOffset)
                    .frame(height: lanesViewport, alignment: .topLeading)
                    .clipped()
                }
                .overlay(alignment: .topLeading) { snapGuide }
                .frame(width: max(contentWidth, 1), alignment: .topLeading)
                .offset(x: halfWidth - state.playhead * pointsPerSecond)
            }
            .frame(width: geometry.size.width, height: displayHeight, alignment: .topLeading)
            .clipped()
            // Chrome sits outside the wide content ZStack: inside it these
            // flexible views would size to the full content width and land
            // offscreen; as overlays they pin to the visible viewport.
            .overlay { playheadLine }
            .overlay(alignment: .top) { readout }
            .overlay(alignment: .bottomTrailing) { magnetToggle }
            .contentShape(Rectangle())
            // Tapping anything that isn't a clip clears the selection.
            .onTapGesture { state.selection = nil }
            .gesture(timelinePanGesture(maxLaneOffset: maxLaneOffset))
            .simultaneousGesture(pinchToZoom)
            .sensoryFeedback(.impact(weight: .light), trigger: state.activeSnapTime) { _, newValue in
                newValue != nil
            }
        }
        .frame(height: displayHeight)
        .background(Theme.timelineBed)
        .clipped()
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("timeline")
        .onGeometryChange(for: CGRect.self) { proxy in
            proxy.frame(in: .global)
        } action: { frame in
            globalFrame = frame
        }
        .onChange(of: displayHeight) { _, newHeight in
            keepMainTrackVisible(displayHeight: newHeight, naturalHeight: naturalHeight)
        }
        .sensoryFeedback(.impact(weight: .medium), trigger: liftedLaneClipID) { _, newValue in
            newValue != nil
        }
        .sensoryFeedback(.success, trigger: conversionPulse)
        .onDisappear { decayTask?.cancel() }
    }

    /// As the grab bar resizes the timeline, nudge the lane pan by the
    /// minimum needed to keep the main track fully inside the viewport, so
    /// collapsing always lands focused on the track (CapCut behavior). The
    /// user can still pan away afterwards.
    private func keepMainTrackVisible(displayHeight: CGFloat, naturalHeight: CGFloat) {
        let viewport = max(displayHeight - Self.rulerHeight - Self.rowSpacing, 40)
        let maxOffset = max(0, naturalHeight - Self.rulerHeight - Self.rowSpacing - viewport)
        let main = currentBandLayout().main

        var target = min(max(0, laneOffset), maxOffset)
        if target > main.lowerBound { target = main.lowerBound }
        if target < main.upperBound - viewport { target = main.upperBound - viewport }
        laneOffset = min(max(0, target), maxOffset)
    }

    private func timelineHeight(effectRows: Int, overlayRows: Int, audioRows: Int) -> CGFloat {
        let layout = bandLayout(effectRows: effectRows, overlayRows: overlayRows, audioRows: audioRows)
        let bottom = layout.audio?.upperBound ?? layout.overlay.upperBound
        return Self.rulerHeight + Self.rowSpacing + bottom + Self.rowSpacing + 16
    }

    // MARK: Band geometry (cross-lane drag targets)

    private var rowCounts: (effects: Int, overlays: Int, audio: Int) {
        (
            packedEffects.map { $0.row + 1 }.max() ?? 0,
            packedOverlays.map { $0.row + 1 }.max() ?? 0,
            packedAudio.map { $0.row + 1 }.max() ?? 0
        )
    }

    /// Y-ranges of each band inside the lane stack; the same math that sizes
    /// `timelineHeight`, so hit-testing can't drift from the layout.
    private func bandLayout(effectRows: Int, overlayRows: Int, audioRows: Int) -> BandLayout {
        let spacing = Self.rowSpacing
        let lane = LaneClipView.height
        func blockHeight(_ rows: Int) -> CGFloat {
            rows == 0 ? Self.emptyLaneHeight : CGFloat(rows) * lane + CGFloat(rows - 1) * spacing
        }

        let effectsEnd = blockHeight(effectRows)
        let mainStart = effectsEnd + spacing
        let mainEnd = mainStart + ClipView.height
        let overlayEnd = mainEnd + spacing + blockHeight(overlayRows)
        let audio: ClosedRange<CGFloat>? = audioRows > 0
            ? (overlayEnd + spacing)...(overlayEnd + spacing + blockHeight(audioRows))
            : nil
        return BandLayout(
            effects: 0...effectsEnd,
            main: mainStart...mainEnd,
            overlay: (mainEnd + spacing)...overlayEnd,
            audio: audio
        )
    }

    private func currentBandLayout() -> BandLayout {
        let counts = rowCounts
        return bandLayout(effectRows: counts.effects, overlayRows: counts.overlays, audioRows: counts.audio)
    }

    /// Converts a global (screen) y to the lane stack's coordinate space,
    /// accounting for the ruler and the live lane pan.
    private func laneStackY(fromGlobalY y: CGFloat) -> CGFloat? {
        guard globalFrame != .zero else { return nil }
        let counts = rowCounts
        let natural = timelineHeight(effectRows: counts.effects, overlayRows: counts.overlays, audioRows: counts.audio)
        let display = min(max(userHeight ?? natural, Self.minHeight), max(natural, Self.minHeight))
        let viewport = max(display - Self.rulerHeight - Self.rowSpacing, 40)
        let maxOffset = max(0, natural - Self.rulerHeight - Self.rowSpacing - viewport)
        let effectiveOffset = min(laneOffset, maxOffset)
        return y - globalFrame.minY - Self.rulerHeight - Self.rowSpacing + effectiveOffset
    }

    /// Is the finger over the overlay band (main-clip drop target)?
    private func overlayDropTarget(globalY: CGFloat) -> Bool {
        guard let y = laneStackY(fromGlobalY: globalY) else { return false }
        let overlay = currentBandLayout().overlay
        return y > overlay.lowerBound - 8 && y < overlay.upperBound + 8
    }

    /// Is the finger over the main track (PiP-overlay drop target)?
    private func mainDropTarget(globalY: CGFloat) -> Bool {
        guard let y = laneStackY(fromGlobalY: globalY) else { return false }
        let main = currentBandLayout().main
        return y > main.lowerBound - 8 && y < main.upperBound + 8
    }

    /// Timeline time under a global (screen) x; the fixed center is the
    /// playhead.
    private func timeAt(globalX x: CGFloat) -> TimeInterval {
        guard globalFrame.width > 0 else { return state.playhead }
        return max(0, state.playhead + (x - globalFrame.midX) * state.secondsPerPoint)
    }

    private var liftedLaneClipID: UUID? {
        switch laneLift?.selection {
        case .overlay(let id), .effect(let id), .audio(let id):
            return id
        case .main, nil:
            return nil
        }
    }

    // MARK: Rows

    /// Sub-rows of one lane; each clip offsets to its start time. The row
    /// holding a lifted clip raises above the other lanes.
    private func laneRows<Item: Identifiable>(
        rowCount: Int,
        width: CGFloat,
        entries: [(item: Item, row: Int)],
        liftedID: UUID? = nil,
        view: @escaping (Item) -> LaneClipView
    ) -> some View where Item.ID == UUID {
        ForEach(0..<rowCount, id: \.self) { row in
            ZStack(alignment: .topLeading) {
                Color.clear
                    .frame(width: max(width, 1), height: LaneClipView.height)
                ForEach(entries.filter { $0.row == row }, id: \.item.id) { entry in
                    view(entry.item)
                        .zIndex(entry.item.id == liftedID ? 2 : 0)
                }
            }
            .frame(width: max(width, 1), height: LaneClipView.height, alignment: .topLeading)
            .zIndex(entries.contains { $0.row == row && $0.item.id == liftedID } ? 6 : 0)
        }
    }

    private func overlayClipView(_ clip: MockOverlayClip) -> LaneClipView {
        let style: LaneClipView.Style
        let symbol: String
        switch clip.kind {
        case .text:
            style = .text
            symbol = "textformat"
        case .sticker:
            style = .sticker
            symbol = clip.symbol ?? "face.smiling"
        case .pip:
            style = .pip(clip.art)
            symbol = "square.on.square"
        }
        return LaneClipView(
            style: style,
            label: clip.displayLabel,
            symbol: symbol,
            start: clip.start,
            length: clip.length,
            pointsPerSecond: pointsPerSecond,
            isSelected: state.selection == .overlay(clip.id),
            isMuted: clip.kind == .pip && clip.volume == 0,
            onTap: { toggleSelection(.overlay(clip.id)) },
            onTrim: { edge, anchorStart, anchorLength, delta in
                state.trimLaneClip(.overlay(clip.id), edge: edge, anchorStart: anchorStart, anchorLength: anchorLength, by: delta)
            },
            onMove: { anchorStart, delta in
                state.moveLaneClip(.overlay(clip.id), anchorStart: anchorStart, by: delta)
            },
            onGestureEnd: { state.endGesture() },
            onLiftChange: { location in
                // Only PiP clips have a main-track equivalent; text/stickers
                // lift visually but never arm the main band.
                let overMain = clip.kind == .pip && mainDropTarget(globalY: location.y)
                laneLift = (.overlay(clip.id), overMain)
            },
            onLiftEnd: { location in
                defer { laneLift = nil }
                guard clip.kind == .pip, let location, mainDropTarget(globalY: location.y) else { return }
                state.moveLaneClipToMain(clip.id, at: timeAt(globalX: location.x))
                conversionPulse += 1
            }
        )
    }

    private func effectClipView(_ clip: MockEffectClip) -> LaneClipView {
        LaneClipView(
            style: .effect(clip.kind),
            label: clip.displayLabel,
            symbol: clip.kind == .adjust ? "slider.horizontal.3" : "sparkles",
            start: clip.start,
            length: clip.length,
            pointsPerSecond: pointsPerSecond,
            isSelected: state.selection == .effect(clip.id),
            onTap: { toggleSelection(.effect(clip.id)) },
            onTrim: { edge, anchorStart, anchorLength, delta in
                state.trimLaneClip(.effect(clip.id), edge: edge, anchorStart: anchorStart, anchorLength: anchorLength, by: delta)
            },
            onMove: { anchorStart, delta in
                state.moveLaneClip(.effect(clip.id), anchorStart: anchorStart, by: delta)
            },
            onGestureEnd: { state.endGesture() },
            onLiftChange: { _ in laneLift = (.effect(clip.id), false) },
            onLiftEnd: { _ in laneLift = nil }
        )
    }

    private func audioClipView(_ clip: MockAudioClip) -> LaneClipView {
        LaneClipView(
            style: .audio(seed: clip.waveSeed),
            label: clip.title,
            symbol: clip.symbol,
            start: clip.start,
            length: clip.length,
            pointsPerSecond: pointsPerSecond,
            isSelected: state.selection == .audio(clip.id),
            isMuted: clip.volume == 0,
            onTap: { toggleSelection(.audio(clip.id)) },
            onTrim: { edge, anchorStart, anchorLength, delta in
                state.trimLaneClip(.audio(clip.id), edge: edge, anchorStart: anchorStart, anchorLength: anchorLength, by: delta)
            },
            onMove: { anchorStart, delta in
                state.moveLaneClip(.audio(clip.id), anchorStart: anchorStart, by: delta)
            },
            onGestureEnd: { state.endGesture() },
            onLiftChange: { _ in laneLift = (.audio(clip.id), false) },
            onLiftEnd: { _ in laneLift = nil }
        )
    }

    private func toggleSelection(_ target: TimelineSelection) {
        state.selection = state.selection == target ? nil : target
    }

    private var track: some View {
        let widths = state.clips.map { CGFloat($0.length * pointsPerSecond) }

        return HStack(spacing: 0) {
            ForEach(Array(state.clips.enumerated()), id: \.element.id) { index, clip in
                let isLifted = reorder?.clipID == clip.id

                ClipView(
                    clip: clip,
                    pointsPerSecond: pointsPerSecond,
                    isSelected: state.selection == .main(clip.id),
                    sortMode: reorder != nil,
                    onTap: { toggleSelection(.main(clip.id)) },
                    onTrim: { edge, anchor, delta in
                        state.trim(clip.id, edge: edge, anchor: anchor, by: delta)
                    },
                    onTrimEnd: { state.endGesture() }
                )
                .scaleEffect(isLifted ? 1.07 : 1)
                .shadow(color: .black.opacity(isLifted ? 0.55 : 0), radius: 10, y: 3)
                .offset(reorderTileOffset(index: index, clip: clip, timeWidths: widths))
                .animation(
                    isLifted ? nil : .easeOut(duration: 0.18),
                    value: reorder?.insertionIndex
                )
                // Morph between time widths and square tiles on lift/drop.
                .animation(.snappy(duration: 0.22), value: reorder != nil)
                .zIndex(isLifted ? 10 : 0)
                .gesture(reorderGesture(clip: clip, index: index, timeWidths: widths))
            }

            Button(action: onAddMedia) {
                RoundedRectangle(cornerRadius: 5, style: .continuous)
                    .fill(Theme.surfaceElevated)
                    .frame(width: 40, height: ClipView.height)
                    .overlay {
                        Image(systemName: "plus")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(.white)
                    }
            }
            .buttonStyle(.plain)
            .padding(.leading, state.isEmpty ? 0 : 4)
            .opacity(reorder == nil ? 1 : 0)
        }
        .sensoryFeedback(.impact(weight: .medium), trigger: reorder != nil) { _, lifted in
            lifted
        }
    }

    // MARK: Timeline pan (scrub / lane browse)

    /// One direction-locked drag for the whole timeline surface: horizontal
    /// pans scrub (translation -> playhead, with flick momentum), vertical
    /// pans browse the lane stack inside its clipped viewport.
    private func timelinePanGesture(maxLaneOffset: CGFloat) -> some Gesture {
        DragGesture(minimumDistance: 10, coordinateSpace: .global)
            .onChanged { value in
                if panAnchor == nil {
                    decayTask?.cancel()
                    state.isPlaying = false
                    panAnchor = (state.playhead, laneOffset)
                    panMode = abs(value.translation.height) > abs(value.translation.width)
                        ? .lanes
                        : .scrub
                }
                guard let anchor = panAnchor else { return }

                switch panMode {
                case .scrub:
                    let time = anchor.playhead - value.translation.width * state.secondsPerPoint
                    state.playhead = min(max(0, time), state.duration)
                case .lanes:
                    laneOffset = min(max(0, anchor.lane - value.translation.height), maxLaneOffset)
                case nil:
                    break
                }
            }
            .onEnded { value in
                let mode = panMode
                panAnchor = nil
                panMode = nil
                guard mode == .scrub else { return }

                // Flick momentum: glide through the distance UIKit predicts
                // beyond the finger-tracked translation, decaying to a stop.
                let extra = value.predictedEndTranslation.width - value.translation.width
                startScrubDecay(distance: -extra * state.secondsPerPoint)
            }
    }

    /// Advances the playhead over `distance` seconds with an ease-out decay,
    /// keeping the model truthful frame by frame (readout ticks, snapping
    /// stays live). Cancelled by any new pan or playback.
    private func startScrubDecay(distance: TimeInterval) {
        guard abs(distance) > 0.05 else { return }
        decayTask?.cancel()
        decayTask = Task { @MainActor in
            let duration: TimeInterval = 0.55
            let start = ContinuousClock.now
            var lastProgress: Double = 0
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(16))
                if Task.isCancelled || state.isPlaying { return }
                let elapsed = min((ContinuousClock.now - start) / .seconds(duration), 1)
                // Ease-out cubic: fast start, glides to rest.
                let progress = 1 - pow(1 - elapsed, 3)
                let step = (progress - lastProgress) * distance
                lastProgress = progress
                state.playhead = min(max(0, state.playhead + step), state.duration)
                if elapsed >= 1 || state.playhead == 0 || state.playhead == state.duration {
                    return
                }
            }
        }
    }

    // MARK: Drag to reorder (square sort mode) / drop onto the overlay band

    /// Long-press lifts a main-track clip and morphs the whole track into
    /// uniform square tiles; horizontal drag slides an insertion gap through
    /// the neighbors, dragging down onto the overlay band converts the clip
    /// to a PiP overlay at the drop time.
    private func reorderGesture(clip: MockClip, index: Int, timeWidths: [CGFloat]) -> some Gesture {
        // Global coordinate space: the lifted clip follows the finger, so a
        // local-space translation would feed back on itself. The tight
        // maximumDistance matters: anything looser swallows slow-starting
        // scrub pans and the timeline "stops scrolling" from the tracks.
        LongPressGesture(minimumDuration: 0.35, maximumDistance: 12)
            .sequenced(before: DragGesture(minimumDistance: 0, coordinateSpace: .global))
            .onChanged { value in
                guard case .second(true, let drag) = value else { return }
                state.isPlaying = false
                var current = reorder ?? ReorderDrag(
                    clipID: clip.id,
                    fromIndex: index,
                    // Anchor the square strip so the lifted tile morphs in
                    // place of the original clip's leading edge.
                    stripShift: timeWidths.prefix(index).reduce(0, +) - CGFloat(index) * ClipView.height,
                    insertionIndex: index
                )
                if let drag {
                    current.translation = drag.translation
                    current.location = drag.location
                    current.overOverlayBand = overlayDropTarget(globalY: drag.location.y)
                    current.insertionIndex = insertionIndex(
                        fromIndex: index,
                        translation: drag.translation.width,
                        widths: Array(repeating: ClipView.height, count: timeWidths.count)
                    )
                }
                reorder = current
            }
            .onEnded { value in
                defer { reorder = nil }
                guard case .second(true, _) = value, let final = reorder else { return }
                if final.overOverlayBand {
                    state.moveMainClipToLane(final.clipID, at: timeAt(globalX: final.location.x))
                    conversionPulse += 1
                } else {
                    state.moveClip(fromIndex: final.fromIndex, toIndex: final.insertionIndex)
                }
            }
    }

    /// Where the dragged clip would land: how many other clips' centers lie
    /// left of the dragged clip's current center.
    private func insertionIndex(fromIndex: Int, translation: CGFloat, widths: [CGFloat]) -> Int {
        let leading = widths.prefix(fromIndex).reduce(0, +)
        let draggedCenter = leading + widths[fromIndex] / 2 + translation

        var slot = 0
        var x: CGFloat = 0
        for (index, width) in widths.enumerated() {
            if index != fromIndex, x + width / 2 < draggedCenter {
                slot += 1
            }
            x += width
        }
        return slot
    }

    /// Tile offsets while sorting: every tile shifts by the strip anchor,
    /// the lifted one follows the finger in 2D, and the others slide to
    /// visualize the removal + insertion gap (gap hidden while the drop
    /// would convert to an overlay instead of reordering).
    private func reorderTileOffset(index: Int, clip: MockClip, timeWidths: [CGFloat]) -> CGSize {
        guard let reorder, timeWidths.indices.contains(reorder.fromIndex) else { return .zero }
        let square = ClipView.height
        if reorder.clipID == clip.id {
            return CGSize(
                width: reorder.stripShift + reorder.translation.width,
                height: reorder.translation.height
            )
        }

        var x = reorder.stripShift
        let postRemovalIndex = index > reorder.fromIndex ? index - 1 : index
        if index > reorder.fromIndex {
            x -= square
        }
        if !reorder.overOverlayBand, postRemovalIndex >= reorder.insertionIndex {
            x += square
        }
        return CGSize(width: x, height: 0)
    }

    /// Small boundary buttons between adjacent main-track clips; accent when
    /// a transition is set. Hidden while a main clip is selected so they
    /// don't fight the trim handles.
    @ViewBuilder
    private var transitionBoundaries: some View {
        if state.clips.count > 1, state.selectedClip == nil, reorder == nil {
            ForEach(state.clips.dropLast()) { clip in
                let isSet = clip.transitionAfter != nil
                let boundaryX = (state.startTime(of: clip.id) + clip.length) * pointsPerSecond

                Button {
                    onTransitionTap(clip.id)
                } label: {
                    RoundedRectangle(cornerRadius: 5, style: .continuous)
                        .fill(isSet ? Theme.accent : .white)
                        .frame(width: 20, height: 20)
                        .overlay {
                            Image(systemName: "arrow.left.arrow.right")
                                .font(.system(size: 8, weight: .heavy))
                                .foregroundStyle(isSet ? .white : .black)
                        }
                        .shadow(color: .black.opacity(0.4), radius: 2)
                }
                .buttonStyle(.plain)
                .offset(x: boundaryX - 10)
            }
        }
    }

    /// Full-width dim bands behind the scrolling rows, mirroring the actual
    /// lane layout (ruler excluded; it sits above the lane scroll view).
    /// During cross-lane drags the hovered target band tints accent and the
    /// bystander bands dim.
    private func laneBackground(effectRows: Int, overlayRows: Int, audioRows: Int) -> some View {
        let sortActive = reorder != nil
        let highlightOverlay = reorder?.overOverlayBand == true
        let highlightMain = laneLift?.overMain == true

        return VStack(spacing: Self.rowSpacing) {
            Group {
                if effectRows == 0 {
                    Rectangle().fill(Theme.trackEmpty.opacity(0.55)).frame(height: Self.emptyLaneHeight)
                } else {
                    ForEach(0..<effectRows, id: \.self) { _ in
                        Rectangle().fill(Theme.trackEmpty.opacity(0.7)).frame(height: LaneClipView.height)
                    }
                }
            }
            .opacity(sortActive ? 0.35 : 1)

            Rectangle().fill(Theme.trackEmpty)
                .overlay {
                    if highlightMain {
                        Theme.accent.opacity(0.3)
                    }
                }
                .frame(height: ClipView.height)

            Group {
                if overlayRows == 0 {
                    overlayBandRect(height: Self.emptyLaneHeight, baseOpacity: 0.55, highlighted: highlightOverlay)
                } else {
                    ForEach(0..<overlayRows, id: \.self) { _ in
                        overlayBandRect(height: LaneClipView.height, baseOpacity: 0.7, highlighted: highlightOverlay)
                    }
                }
            }
            .opacity(sortActive && !highlightOverlay ? 0.35 : 1)

            ForEach(0..<audioRows, id: \.self) { _ in
                Rectangle().fill(Theme.trackEmpty.opacity(0.7)).frame(height: LaneClipView.height)
            }
            .opacity(sortActive ? 0.35 : 1)
        }
    }

    private func overlayBandRect(height: CGFloat, baseOpacity: Double, highlighted: Bool) -> some View {
        Rectangle()
            .fill(Theme.trackEmpty.opacity(highlighted ? 0.95 : baseOpacity))
            .overlay {
                if highlighted {
                    Theme.accent.opacity(0.32)
                }
            }
            .frame(height: height)
    }

    // MARK: Overlays

    /// Yellow guide line through all lanes while a gesture is locked onto a
    /// snap candidate.
    @ViewBuilder
    private var snapGuide: some View {
        if let time = state.activeSnapTime {
            Rectangle()
                .fill(Color(hex: 0xFACC15))
                .frame(width: 1.5)
                .frame(maxHeight: .infinity)
                .offset(x: time * pointsPerSecond - 0.75)
                .allowsHitTesting(false)
        }
    }

    /// Magnet chip pinned to the timeline's corner; accent when snapping is
    /// on, dim when free-dragging.
    private var magnetToggle: some View {
        Button {
            state.magnetEnabled.toggle()
        } label: {
            Image(systemName: state.magnetEnabled ? "dot.squareshape.split.2x2" : "squareshape.split.2x2.dotted")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(state.magnetEnabled ? .white : Theme.textTertiary)
                .frame(width: 26, height: 26)
                .background(
                    state.magnetEnabled ? AnyShapeStyle(Theme.accent) : AnyShapeStyle(Theme.surfaceElevated),
                    in: RoundedRectangle(cornerRadius: 7, style: .continuous)
                )
                .shadow(color: .black.opacity(0.35), radius: 3)
        }
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
        .padding(.trailing, 8)
        .padding(.bottom, 8)
    }

    private var playheadLine: some View {
        RoundedRectangle(cornerRadius: 1)
            .fill(.white)
            .frame(width: 2)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
            .allowsHitTesting(false)
    }

    private var readout: some View {
        HStack(spacing: 3) {
            Text(state.playhead.timecode)
                .foregroundStyle(.white)
            Text("/ \(state.duration.timecode)")
                .foregroundStyle(Theme.textTertiary)
        }
        .font(.caption2.weight(.semibold).monospacedDigit())
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(Theme.timelineBed.opacity(0.9), in: Capsule())
        .frame(maxWidth: .infinity, alignment: .trailing)
        .padding(.trailing, 8)
        .allowsHitTesting(false)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("playheadReadout")
    }

    // MARK: Gestures and sync

    private var pinchToZoom: some Gesture {
        MagnifyGesture()
            .onChanged { value in
                let base = pinchBase ?? state.secondsPerPoint
                pinchBase = base
                // More magnification = fewer seconds per point (zoom in).
                let pps = min(max((1 / base) * value.magnification, 10), 150)
                state.secondsPerPoint = 1 / pps
            }
            .onEnded { _ in
                pinchBase = nil
            }
    }
}

#Preview {
    let state = EditorState()
    let _ = state.startProject(with: Array(MockData.libraryItems.prefix(4)))
    let _ = state.addSticker(symbol: "heart.fill")
    let _ = state.addAudio(kind: .music, title: "Slow Morning", duration: 30)
    return TimelineView(state: state, onAddMedia: {})
        .background(Theme.background)
}
