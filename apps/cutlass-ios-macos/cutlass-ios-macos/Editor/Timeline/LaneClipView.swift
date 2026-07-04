import SwiftUI

/// One clip bar on a floating lane (text/sticker/PiP, effect, or audio).
/// Width is time-accurate; selection adds a white frame, trim handles, a
/// duration bubble, and drag-to-move.
struct LaneClipView: View {
    enum Style {
        case text
        case sticker
        case pip(MockArt?)
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
    var onTap: () -> Void
    /// (edge, anchorStart, anchorLength, delta seconds); the anchor is the
    /// clip's range captured when the drag began, so math never drifts.
    var onTrim: (EditorState.TrimEdge, TimeInterval, TimeInterval, TimeInterval) -> Void
    /// (anchorStart, delta seconds) for horizontal drags to a new start.
    var onMove: (TimeInterval, TimeInterval) -> Void
    var onGestureEnd: () -> Void
    /// Global finger location while the clip is lifted (2D drag); the
    /// timeline uses it for cross-lane band hit-testing.
    var onLiftChange: (CGPoint) -> Void = { _ in }
    /// Final global finger location on release (nil if the lift never
    /// produced a drag). Called before onGestureEnd.
    var onLiftEnd: (CGPoint?) -> Void = { _ in }

    static let height: CGFloat = 30
    private static let handleWidth: CGFloat = 13

    /// (start, length) captured at the first update of a trim/move drag.
    @State private var anchor: (start: TimeInterval, length: TimeInterval)?
    /// Vertical finger-follow while lifted; springs back unless the drop
    /// converted the clip to another lane (then this view disappears).
    @State private var liftY: CGFloat = 0
    @State private var isLifting = false

    private var width: CGFloat {
        max(6, length * pointsPerSecond)
    }

    var body: some View {
        background
            .frame(width: width, height: Self.height)
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
            .contentShape(Rectangle())
            .onTapGesture(perform: onTap)
            // The .none mask matters: even a nil optional gesture installs a
            // recognizer that can stall other gestures in the hierarchy.
            // Selected clips move on an immediate drag; unselected ones need
            // the long-press lift so plain drags still pan the timeline.
            .highPriorityGesture(moveGesture, including: isSelected ? .all : .none)
            .gesture(liftGesture, including: isSelected ? .none : .all)
            .scaleEffect(isLifting ? 1.06 : 1)
            .shadow(color: .black.opacity(isLifting ? 0.5 : 0), radius: 8, y: 3)
            .offset(x: start * pointsPerSecond, y: liftY)
    }

    // MARK: Pieces

    @ViewBuilder
    private var background: some View {
        switch style {
        case .text:
            Color(hex: 0x4338CA)
        case .sticker:
            Color(hex: 0x9D174D)
        case .pip(let art):
            if let art {
                HStack(spacing: 0) {
                    let tiles = max(1, Int((width / Self.height).rounded(.up)))
                    ForEach(0..<tiles, id: \.self) { index in
                        MockArtView(art: art, symbolSize: 9)
                            .frame(width: Self.height, height: Self.height)
                            .overlay(Color.black.opacity(index.isMultiple(of: 2) ? 0 : 0.12))
                            .clipped()
                    }
                }
                .frame(width: width, alignment: .leading)
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
            .frame(width: Self.handleWidth, height: Self.height)
            .overlay {
                Capsule()
                    .fill(.black.opacity(0.55))
                    .frame(width: 2.5, height: 12)
            }
            // Global coordinate space: the handle moves with the edge it
            // trims, so local-space translations would feed back and flicker.
            .highPriorityGesture(
                DragGesture(minimumDistance: 0, coordinateSpace: .global)
                    .onChanged { value in
                        let anchor = self.anchor ?? (start, length)
                        self.anchor = anchor
                        onTrim(edge, anchor.start, anchor.length, value.translation.width / pointsPerSecond)
                    }
                    .onEnded { _ in
                        anchor = nil
                        onGestureEnd()
                    }
            )
    }

    /// Selected lane clips drag on an immediate touch; the drag claims the
    /// touch so the timeline doesn't scroll underneath. Global coordinate
    /// space because the clip offsets under the finger as it moves —
    /// local deltas would oscillate.
    private var moveGesture: some Gesture {
        DragGesture(minimumDistance: 6, coordinateSpace: .global)
            .onChanged { value in
                applyLiftDrag(translation: value.translation, location: value.location)
            }
            .onEnded { value in
                finishLiftDrag(location: value.location)
            }
    }

    /// Unselected clips lift after a long press, then track the finger in
    /// 2D exactly like the selected-move drag.
    private var liftGesture: some Gesture {
        LongPressGesture(minimumDuration: 0.35, maximumDistance: 12)
            .sequenced(before: DragGesture(minimumDistance: 0, coordinateSpace: .global))
            .onChanged { value in
                guard case .second(true, let drag) = value, let drag else { return }
                applyLiftDrag(translation: drag.translation, location: drag.location)
            }
            .onEnded { value in
                guard case .second(true, let drag) = value else { return }
                finishLiftDrag(location: drag?.location)
            }
    }

    /// Shared 2D lift handling: horizontal component re-times the clip via
    /// onMove (sub-rows re-pack live), vertical component is a visual
    /// follow, and the raw location feeds cross-lane band hit-testing.
    private func applyLiftDrag(translation: CGSize, location: CGPoint) {
        let anchor = self.anchor ?? (start, length)
        self.anchor = anchor
        isLifting = true
        liftY = translation.height
        onMove(anchor.start, translation.width / pointsPerSecond)
        onLiftChange(location)
    }

    private func finishLiftDrag(location: CGPoint?) {
        anchor = nil
        isLifting = false
        onLiftEnd(location)
        // If the drop converted the clip to another lane this view is gone
        // and the spring-back never renders; otherwise snap home.
        withAnimation(.snappy(duration: 0.25)) {
            liftY = 0
        }
        onGestureEnd()
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
