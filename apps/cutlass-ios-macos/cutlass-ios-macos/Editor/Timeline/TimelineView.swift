import SwiftUI

/// The scrubbable timeline: a fixed center playhead over horizontally
/// scrolling content, where scroll offset IS the current time. Renders the
/// effects lane above the main track and overlay/audio lanes below it;
/// overlapping lane clips pack into sub-rows.
struct TimelineView: View {
    var state: EditorState
    var onAddMedia: () -> Void

    @State private var scrollPosition = ScrollPosition(edge: .leading)
    @State private var scrollPhase: ScrollPhase = .idle
    /// seconds-per-point captured when a pinch begins.
    @State private var pinchBase: Double?

    private static let rowSpacing: CGFloat = 5
    private static let rulerHeight: CGFloat = 18
    private static let emptyLaneHeight: CGFloat = 20

    private var pointsPerSecond: Double { 1 / state.secondsPerPoint }

    private var isUserScrolling: Bool {
        scrollPhase == .tracking || scrollPhase == .interacting || scrollPhase == .decelerating
    }

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

    var body: some View {
        let effects = packedEffects
        let overlays = packedOverlays
        let audio = packedAudio
        let effectRows = effects.map { $0.row + 1 }.max() ?? 0
        let overlayRows = overlays.map { $0.row + 1 }.max() ?? 0
        let audioRows = audio.map { $0.row + 1 }.max() ?? 0

        GeometryReader { geometry in
            let halfWidth = geometry.size.width / 2
            let contentWidth = max(0, state.duration * pointsPerSecond)

            ZStack(alignment: .top) {
                laneBackground(effectRows: effectRows, overlayRows: overlayRows, audioRows: audioRows)

                ScrollView(.horizontal, showsIndicators: false) {
                    VStack(alignment: .leading, spacing: Self.rowSpacing) {
                        TimeRuler(duration: state.duration, pointsPerSecond: pointsPerSecond)
                            .frame(width: contentWidth, height: Self.rulerHeight, alignment: .leading)
                            .clipped()

                        if effectRows == 0 {
                            Color.clear.frame(height: Self.emptyLaneHeight)
                        } else {
                            laneRows(
                                rowCount: effectRows,
                                width: contentWidth,
                                entries: effects,
                                view: effectClipView(_:)
                            )
                        }

                        track

                        if overlayRows == 0 {
                            Color.clear.frame(height: Self.emptyLaneHeight)
                        } else {
                            laneRows(
                                rowCount: overlayRows,
                                width: contentWidth,
                                entries: overlays,
                                view: overlayClipView(_:)
                            )
                        }

                        if audioRows > 0 {
                            laneRows(
                                rowCount: audioRows,
                                width: contentWidth,
                                entries: audio,
                                view: audioClipView(_:)
                            )
                        }
                    }
                    .padding(.horizontal, halfWidth)
                }
                .scrollPosition($scrollPosition)
                .onScrollGeometryChange(for: CGFloat.self) { scrollGeometry in
                    scrollGeometry.contentOffset.x
                } action: { _, offset in
                    guard isUserScrolling else { return }
                    state.isPlaying = false
                    state.playhead = min(max(0, offset * state.secondsPerPoint), state.duration)
                }
                .onScrollPhaseChange { _, newPhase in
                    scrollPhase = newPhase
                }

                playheadLine
                readout
            }
            // Tapping anything that isn't a clip clears the selection.
            .onTapGesture { state.selection = nil }
            .simultaneousGesture(pinchToZoom)
            .onChange(of: state.playhead) {
                syncScrollToPlayhead()
            }
            .onChange(of: state.secondsPerPoint) {
                syncScrollToPlayhead(force: true)
            }
            .onAppear {
                syncScrollToPlayhead(force: true)
            }
        }
        .frame(height: timelineHeight(effectRows: effectRows, overlayRows: overlayRows, audioRows: audioRows))
        .background(Theme.timelineBed)
        .clipped()
    }

    private func timelineHeight(effectRows: Int, overlayRows: Int, audioRows: Int) -> CGFloat {
        let laneHeight = LaneClipView.height + Self.rowSpacing
        var height = Self.rulerHeight + ClipView.height + Self.rowSpacing * 2 + 16
        height += effectRows == 0 ? Self.emptyLaneHeight + Self.rowSpacing : CGFloat(effectRows) * laneHeight
        height += overlayRows == 0 ? Self.emptyLaneHeight + Self.rowSpacing : CGFloat(overlayRows) * laneHeight
        height += CGFloat(audioRows) * laneHeight
        return height
    }

    // MARK: Rows

    /// Sub-rows of one lane; each clip offsets to its start time.
    private func laneRows<Item: Identifiable>(
        rowCount: Int,
        width: CGFloat,
        entries: [(item: Item, row: Int)],
        view: @escaping (Item) -> LaneClipView
    ) -> some View {
        ForEach(0..<rowCount, id: \.self) { row in
            ZStack(alignment: .topLeading) {
                Color.clear
                    .frame(width: max(width, 1), height: LaneClipView.height)
                ForEach(entries.filter { $0.row == row }, id: \.item.id) { entry in
                    view(entry.item)
                }
            }
            .frame(width: max(width, 1), height: LaneClipView.height, alignment: .topLeading)
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
            onGestureEnd: { state.endGesture() }
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
            onGestureEnd: { state.endGesture() }
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
            onGestureEnd: { state.endGesture() }
        )
    }

    private func toggleSelection(_ target: TimelineSelection) {
        state.selection = state.selection == target ? nil : target
    }

    private var track: some View {
        HStack(spacing: 0) {
            ForEach(state.clips) { clip in
                ClipView(
                    clip: clip,
                    pointsPerSecond: pointsPerSecond,
                    isSelected: state.selection == .main(clip.id),
                    onTap: { toggleSelection(.main(clip.id)) },
                    onTrim: { edge, anchor, delta in
                        state.trim(clip.id, edge: edge, anchor: anchor, by: delta)
                    },
                    onTrimEnd: { state.endGesture() }
                )
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
        }
    }

    /// Full-width dim bands behind the scrolling rows, mirroring the actual
    /// lane layout.
    private func laneBackground(effectRows: Int, overlayRows: Int, audioRows: Int) -> some View {
        VStack(spacing: Self.rowSpacing) {
            Color.clear.frame(height: Self.rulerHeight)

            if effectRows == 0 {
                Rectangle().fill(Theme.trackEmpty.opacity(0.55)).frame(height: Self.emptyLaneHeight)
            } else {
                ForEach(0..<effectRows, id: \.self) { _ in
                    Rectangle().fill(Theme.trackEmpty.opacity(0.7)).frame(height: LaneClipView.height)
                }
            }

            Rectangle().fill(Theme.trackEmpty).frame(height: ClipView.height)

            if overlayRows == 0 {
                Rectangle().fill(Theme.trackEmpty.opacity(0.55)).frame(height: Self.emptyLaneHeight)
            } else {
                ForEach(0..<overlayRows, id: \.self) { _ in
                    Rectangle().fill(Theme.trackEmpty.opacity(0.7)).frame(height: LaneClipView.height)
                }
            }

            ForEach(0..<audioRows, id: \.self) { _ in
                Rectangle().fill(Theme.trackEmpty.opacity(0.7)).frame(height: LaneClipView.height)
            }
        }
    }

    // MARK: Overlays

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

    /// Keeps the scroll offset in lockstep with the playhead whenever the
    /// change didn't originate from the user's finger.
    private func syncScrollToPlayhead(force: Bool = false) {
        guard force || !isUserScrolling else { return }
        scrollPosition.scrollTo(x: state.playhead * pointsPerSecond)
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
