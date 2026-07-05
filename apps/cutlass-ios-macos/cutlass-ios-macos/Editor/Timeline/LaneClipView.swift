import SwiftUI

/// One clip bar on a floating lane (text/sticker/PiP, effect, or audio).
/// Width is time-accurate; selection adds a white frame, trim handles, a
/// duration bubble, and drag-to-move (commit-on-release via the timeline).
struct LaneClipView: View {
    enum Style {
        case text
        case sticker
        case pip(FilmstripSource)
        case effect(MockEffectClip.Kind)
        case audio(seed: Int)
    }

    var style: Style
    var label: String
    var symbol: String
    var start: TimeInterval
    var length: TimeInterval
    var pointsPerSecond: Double
    var isSelected: Bool
    var isMuted = false
    /// Row height; video lanes render taller than generated-content bars.
    var rowHeight: CGFloat = LaneClipView.height
    /// True while the timeline owns a floating copy — the original dims.
    var isBeingDragged = false
    var accessibilityIdentifier: String?
    var onTap: () -> Void
    /// (edge, anchorStart, anchorLength, delta seconds); the anchor is the
    /// clip's range captured when the drag began, so math never drifts.
    var onTrim: (EditorState.TrimEdge, TimeInterval, TimeInterval, TimeInterval) -> Void
    /// Live drag deltas; the timeline resolves landing and commits on release.
    var onDragBegin: () -> Void = {}
    var onDragChanged: (CGSize, CGPoint) -> Void = { _, _ in }
    var onDragEnded: (CGPoint?) -> Void = { _ in }
    var onGestureEnd: () -> Void

    static let height: CGFloat = 30
    private static let handleWidth: CGFloat = 13

    /// (start, length) captured at the first update of a trim drag.
    @State private var trimAnchor: (start: TimeInterval, length: TimeInterval)?
    @State private var dragAnchor: (start: TimeInterval, length: TimeInterval)?

    private var width: CGFloat {
        max(6, length * pointsPerSecond)
    }

    var body: some View {
        background
            .frame(width: width, height: rowHeight)
            .overlay(alignment: .leading) { content }
            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            .overlay {
                if isSelected {
                    selectionChrome
                }
            }
            .overlay(alignment: .top) {
                if isSelected {
                    durationBubble
                }
            }
            .opacity(isBeingDragged ? 0.35 : 1)
            .contentShape(Rectangle())
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(label)
            .accessibilityIdentifier(accessibilityIdentifier ?? label)
            .onTapGesture(perform: onTap)
            .highPriorityGesture(moveGesture, including: isSelected ? .all : .none)
            .gesture(liftGesture, including: isSelected ? .none : .all)
            .offset(x: start * pointsPerSecond)
    }

    // MARK: Pieces

    @ViewBuilder
    private var background: some View {
        switch style {
        case .text:
            Color(hex: 0x4338CA)
        case .sticker:
            Color(hex: 0x9D174D)
        case .pip(let source):
            if source.path != nil || source.art != nil {
                FilmstripView(
                    source: source,
                    clipLength: length,
                    pointsPerSecond: pointsPerSecond,
                    tileSize: rowHeight,
                    width: width,
                    symbolSize: 9
                )
            } else {
                Color(hex: 0x155E75)
            }
        case .effect(let kind):
            switch kind {
            case .effect:
                LinearGradient(
                    colors: [Color(hex: 0x7C3AED), Color(hex: 0x4C1D95)],
                    startPoint: .leading,
                    endPoint: .trailing
                )
            case .filter:
                LinearGradient(
                    colors: [Color(hex: 0x2563EB), Color(hex: 0x1E3A8A)],
                    startPoint: .leading,
                    endPoint: .trailing
                )
            case .adjust:
                LinearGradient(
                    colors: [Color(hex: 0x64748B), Color(hex: 0x334155)],
                    startPoint: .leading,
                    endPoint: .trailing
                )
            }
        case .audio(let seed):
            ZStack {
                Color(hex: 0x0C2733)
                LaneWaveform(seed: seed)
                    .opacity(0.85)
            }
        }
    }

    private var content: some View {
        HStack(spacing: 4) {
            Image(systemName: symbol)
                .font(.system(size: 9, weight: .bold))
            Text(label)
                .font(.system(size: 10, weight: .medium))
                .lineLimit(1)
            if isMuted {
                Image(systemName: "speaker.slash.fill")
                    .font(.system(size: 8, weight: .bold))
                    .opacity(0.85)
            }
        }
        .foregroundStyle(.white.opacity(0.95))
        .padding(.horizontal, 6)
        .frame(maxWidth: width, alignment: .leading)
        .allowsHitTesting(false)
    }

    private var durationBubble: some View {
        Text(String(format: "%.1fs", length))
            .font(.system(size: 9, weight: .semibold).monospacedDigit())
            .foregroundStyle(.black)
            .padding(.horizontal, 5)
            .padding(.vertical, 1.5)
            .background(.white, in: Capsule())
            .offset(y: -14)
            .allowsHitTesting(false)
    }

    private var selectionChrome: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .strokeBorder(.white, lineWidth: 2)

            HStack(spacing: 0) {
                handle(.leading)
                Spacer(minLength: 0)
                handle(.trailing)
            }
        }
    }

    private func handle(_ edge: EditorState.TrimEdge) -> some View {
        let corners: RectangleCornerRadii = edge == .leading
            ? RectangleCornerRadii(topLeading: 6, bottomLeading: 6)
            : RectangleCornerRadii(bottomTrailing: 6, topTrailing: 6)

        return UnevenRoundedRectangle(cornerRadii: corners, style: .continuous)
            .fill(.white)
            .frame(width: Self.handleWidth, height: rowHeight)
            .overlay {
                Capsule()
                    .fill(.black.opacity(0.55))
                    .frame(width: 2.5, height: 12)
            }
            .highPriorityGesture(
                DragGesture(minimumDistance: 0, coordinateSpace: .global)
                    .onChanged { value in
                        let anchor = trimAnchor ?? (start, length)
                        trimAnchor = anchor
                        onTrim(edge, anchor.start, anchor.length, value.translation.width / pointsPerSecond)
                    }
                    .onEnded { _ in
                        trimAnchor = nil
                        onGestureEnd()
                    }
            )
    }

    private var moveGesture: some Gesture {
        DragGesture(minimumDistance: 6, coordinateSpace: .global)
            .onChanged { value in
                reportDrag(translation: value.translation, location: value.location)
            }
            .onEnded { value in
                dragAnchor = nil
                onDragEnded(value.location)
            }
    }

    private var liftGesture: some Gesture {
        LongPressGesture(minimumDuration: 0.35, maximumDistance: 12)
            .sequenced(before: DragGesture(minimumDistance: 0, coordinateSpace: .global))
            .onChanged { value in
                guard case .second(true, let drag) = value, let drag else { return }
                reportDrag(translation: drag.translation, location: drag.location)
            }
            .onEnded { value in
                dragAnchor = nil
                guard case .second(true, let drag) = value else { return }
                onDragEnded(drag?.location)
            }
    }

    private func reportDrag(translation: CGSize, location: CGPoint) {
        if dragAnchor == nil {
            dragAnchor = (start, length)
            onDragBegin()
        }
        onDragChanged(translation, location)
    }
}

/// Compact deterministic waveform for audio lane bars.
private struct LaneWaveform: View {
    var seed: Int

    var body: some View {
        Canvas { context, size in
            let phase = Double(seed % 977) * 0.13
            let barWidth: CGFloat = 1.8
            let gap: CGFloat = 1.4
            var x: CGFloat = 1
            while x < size.width - 1 {
                let t = Double(x)
                let envelope = 0.35 + 0.65 * abs(sin(t * 0.024 + phase))
                let detail = 0.45 + 0.55 * abs(sin(t * 0.31 + phase * 3))
                let barHeight = max(1.5, size.height * envelope * detail * 0.8)
                let rect = CGRect(x: x, y: (size.height - barHeight) / 2, width: barWidth, height: barHeight)
                context.fill(Path(roundedRect: rect, cornerRadius: 1), with: .color(Theme.waveform))
                x += barWidth + gap
            }
        }
    }
}
