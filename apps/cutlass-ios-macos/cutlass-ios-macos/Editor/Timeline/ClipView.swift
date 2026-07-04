import SwiftUI

/// One clip on the track: repeating thumbnail tiles with a fake waveform
/// strip when the source has audio. Width is time-accurate
/// (length x points-per-second) so scrubbing lines up with the ruler.
/// Selected clips grow a white border with draggable trim handles.
struct ClipView: View {
    var clip: MockClip
    var pointsPerSecond: Double
    var isSelected: Bool = false
    /// Square sort mode: while a reorder is live the whole track renders as
    /// uniform square tiles, so shuffling never means dragging across a
    /// long clip. Same view tree (the width just changes) so the in-flight
    /// long-press gesture survives the morph.
    var sortMode: Bool = false
    var onTap: () -> Void = {}
    var onTrim: (EditorState.TrimEdge, MockClip, Double) -> Void = { _, _, _ in }
    var onTrimEnd: () -> Void = {}

    /// Clip snapshot from the moment a trim drag started.
    @State private var trimAnchor: MockClip?

    static let height: CGFloat = 66
    private static let waveformHeight: CGFloat = 20
    private static let handleWidth: CGFloat = 16

    private var timeWidth: CGFloat {
        max(2, clip.length * pointsPerSecond)
    }

    private var width: CGFloat {
        sortMode ? Self.height : timeWidth
    }

    var body: some View {
        VStack(spacing: 0) {
            thumbnailStrip(height: clip.hasAudio && !sortMode ? Self.height - Self.waveformHeight : Self.height)
            if clip.hasAudio, !sortMode {
                WaveformStrip(seed: clip.id.hashValue)
                    .frame(height: Self.waveformHeight)
            }
        }
        .frame(width: width, height: Self.height)
        .clipShape(RoundedRectangle(cornerRadius: sortMode ? 9 : 5, style: .continuous))
        // Hairline separator so adjacent clips read as distinct without
        // shifting the time math the way real HStack spacing would.
        .overlay(alignment: .trailing) {
            if !isSelected, !sortMode {
                Rectangle()
                    .fill(Theme.timelineBed)
                    .frame(width: 1)
            }
        }
        .overlay(alignment: .bottomLeading) {
            if !sortMode {
                badges
            }
        }
        .overlay {
            if !sortMode {
                keyframeDiamonds
            }
        }
        .overlay {
            if sortMode {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .strokeBorder(.white.opacity(0.25), lineWidth: 1)
            }
        }
        .overlay(alignment: .bottom) {
            if sortMode {
                sortDurationLabel
            }
        }
        .overlay {
            if isSelected, !sortMode {
                selectionChrome
            }
        }
        .overlay(alignment: .top) {
            if isSelected, !sortMode {
                durationBubble
            }
        }
        .contentShape(Rectangle())
        .onTapGesture(perform: onTap)
    }

    private var sortDurationLabel: some View {
        Text(String(format: "%.1fs", clip.length))
            .font(.system(size: 8.5, weight: .semibold).monospacedDigit())
            .foregroundStyle(.white)
            .padding(.horizontal, 4)
            .padding(.vertical, 1)
            .background(.black.opacity(0.55), in: Capsule())
            .padding(.bottom, 3)
            .allowsHitTesting(false)
    }

    // MARK: Status badges (speed / mute / reverse / freeze / filter)

    private var badges: some View {
        HStack(spacing: 3) {
            if clip.isFreeze {
                badge(symbol: "snowflake")
            }
            if clip.speed != 1 {
                badge(text: String(format: clip.speed < 1 ? "%.1fx" : "%gx", clip.speed))
            }
            if clip.hasAudio, clip.volume == 0 {
                badge(symbol: "speaker.slash.fill")
            }
            if clip.isReversed {
                badge(symbol: "arrow.uturn.backward")
            }
            if clip.filterName != nil {
                badge(symbol: "camera.filters")
            }
        }
        .padding(4)
        .allowsHitTesting(false)
    }

    private func badge(text: String? = nil, symbol: String? = nil) -> some View {
        HStack(spacing: 0) {
            if let text {
                Text(text)
                    .font(.system(size: 8.5, weight: .bold).monospacedDigit())
            }
            if let symbol {
                Image(systemName: symbol)
                    .font(.system(size: 8, weight: .bold))
            }
        }
        .foregroundStyle(.white)
        .padding(.horizontal, 4)
        .padding(.vertical, 2)
        .background(.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 3))
    }

    /// Keyframe markers stamped by the transport diamond, at their local
    /// times along the clip.
    @ViewBuilder
    private var keyframeDiamonds: some View {
        if !clip.keyframes.isEmpty {
            ZStack(alignment: .leading) {
                ForEach(clip.keyframes, id: \.self) { time in
                    Image(systemName: "diamond.fill")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(.white)
                        .shadow(color: .black.opacity(0.6), radius: 1.5)
                        .offset(x: time * pointsPerSecond - 4)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .allowsHitTesting(false)
        }
    }

    private var durationBubble: some View {
        Text(String(format: "%.1fs", clip.length))
            .font(.system(size: 9, weight: .semibold).monospacedDigit())
            .foregroundStyle(.black)
            .padding(.horizontal, 5)
            .padding(.vertical, 1.5)
            .background(.white, in: Capsule())
            .offset(y: -9)
            .allowsHitTesting(false)
    }

    private func thumbnailStrip(height: CGFloat) -> some View {
        FilmstripView(
            source: FilmstripSource(
                path: clip.mediaPath,
                trimStart: clip.trimStart,
                speed: clip.speed,
                reversed: clip.isReversed,
                isStill: clip.isStill,
                art: clip.art
            ),
            clipLength: clip.length,
            pointsPerSecond: pointsPerSecond,
            tileSize: height,
            width: width
        )
    }

    // MARK: Selection + trim handles

    private var selectionChrome: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 5, style: .continuous)
                .strokeBorder(.white, lineWidth: 2.5)

            HStack(spacing: 0) {
                handle(.leading)
                Spacer(minLength: 0)
                handle(.trailing)
            }
        }
    }

    private func handle(_ edge: EditorState.TrimEdge) -> some View {
        let corners: RectangleCornerRadii = edge == .leading
            ? RectangleCornerRadii(topLeading: 5, bottomLeading: 5)
            : RectangleCornerRadii(bottomTrailing: 5, topTrailing: 5)

        return UnevenRoundedRectangle(cornerRadii: corners, style: .continuous)
            .fill(.white)
            .frame(width: Self.handleWidth, height: Self.height)
            .overlay {
                Capsule()
                    .fill(.black.opacity(0.55))
                    .frame(width: 3, height: 16)
            }
            // minimumDistance 0 claims the touch immediately so the
            // surrounding ScrollView can't steal horizontal drags. Global
            // coordinate space keeps the translation finger-based: the handle
            // itself moves as the clip resizes, so local-space deltas would
            // oscillate (visible as flicker).
            .highPriorityGesture(
                DragGesture(minimumDistance: 0, coordinateSpace: .global)
                    .onChanged { value in
                        let anchor = trimAnchor ?? clip
                        trimAnchor = anchor
                        onTrim(edge, anchor, value.translation.width / pointsPerSecond)
                    }
                    .onEnded { _ in
                        trimAnchor = nil
                        onTrimEnd()
                    }
            )
    }
}

/// Deterministic pseudo-random waveform bars, seeded per clip so the shape
/// is stable across renders.
private struct WaveformStrip: View {
    var seed: Int

    var body: some View {
        Canvas { context, size in
            let phase = Double(seed % 977) * 0.13
            let barWidth: CGFloat = 2
            let gap: CGFloat = 1.5
            var x: CGFloat = 1

            context.fill(Path(CGRect(origin: .zero, size: size)), with: .color(Color(hex: 0x0C2733)))

            while x < size.width - 1 {
                let t = Double(x)
                let envelope = 0.35 + 0.65 * abs(sin(t * 0.021 + phase))
                let detail = 0.45 + 0.55 * abs(sin(t * 0.29 + phase * 3))
                let barHeight = max(1.5, size.height * envelope * detail * 0.92)
                let rect = CGRect(
                    x: x,
                    y: (size.height - barHeight) / 2,
                    width: barWidth,
                    height: barHeight
                )
                context.fill(Path(roundedRect: rect, cornerRadius: 1), with: .color(Theme.waveform))
                x += barWidth + gap
            }
        }
    }
}

#Preview {
    HStack(spacing: 0) {
        ClipView(
            clip: MockClip(
                art: MockData.libraryItems[1].art, sourceDuration: 9, length: 9, hasAudio: true),
            pointsPerSecond: 44, isSelected: true)
        ClipView(
            clip: MockClip(
                art: MockData.libraryItems[0].art, sourceDuration: 30, length: 3, hasAudio: false),
            pointsPerSecond: 44)
    }
    .padding()
    .background(Theme.timelineBed)
}
