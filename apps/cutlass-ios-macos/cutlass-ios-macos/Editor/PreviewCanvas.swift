import SwiftUI

/// The engine-rendered preview: every scrub tick / playback tick / landed
/// edit requests a `render_fit` frame sized to the letterboxed project frame
/// (drop-newest-wins, see `PreviewFeed`). The engine composites footage,
/// stills, PiP, solids, and transforms; SwiftUI draws what the engine can't
/// render yet on top (text, stickers, effect badge) plus the manipulation
/// chrome. The old gradient art remains only as the loading placeholder.
struct PreviewCanvas: View {
    var state: EditorState
    var onEditText: (UUID) -> Void = { _ in }

    @Environment(\.displayScale) private var displayScale
    /// Size of the letterboxed frame in points (from the frame's geometry).
    @State private var frameSize = CGSize.zero

    var body: some View {
        // Fill images (engine frames) must be pinned to measured bounds:
        // `scaledToFill` otherwise reports an oversized ideal width that
        // would inflate the whole editor layout.
        GeometryReader { outer in
            ZStack {
                Color.black

                if let clip = state.clip(at: state.playhead) {
                    canvasBackground(for: clip)
                        .frame(width: outer.size.width, height: outer.size.height)
                        .clipped()
                }

                frame
                    .padding(.vertical, 10)
                    .frame(width: outer.size.width, height: outer.size.height)
            }
        }
        .clipped()
        .onChange(of: renderStamp, initial: true) {
            requestFrame()
        }
    }

    /// The letterboxed project frame.
    private var frame: some View {
        GeometryReader { geo in
            frameContent(size: geo.size)
                .overlay {
                    PreviewOverlayLayer(
                        state: state,
                        engineRendered: state.preview.image != nil,
                        onEditText: onEditText
                    )
                }
                .overlay(alignment: .topLeading) { effectBadge }
                .clipShape(RoundedRectangle(cornerRadius: 4))
                .onChange(of: geo.size, initial: true) { _, size in
                    frameSize = size
                }
        }
        .aspectRatio(frameAspect, contentMode: .fit)
    }

    /// The engine frame — or the loading placeholder until it lands.
    @ViewBuilder
    private func frameContent(size: CGSize) -> some View {
        if !state.isEmpty, let frame = state.preview.image {
            Image(decorative: frame, scale: 1)
                .resizable()
                .interpolation(.high)
                .scaledToFill()
                .frame(width: size.width, height: size.height)
                .clipped()
        } else if let clip = state.clip(at: state.playhead) {
            // Loading state: deterministic gradient until the engine's first
            // frame lands.
            MockArtView(art: clip.art, symbolSize: 64)
                .opacity(clip.opacity)
                .frame(width: size.width, height: size.height)
        } else {
            Group {
                if state.background.kind == .color {
                    state.background.color
                } else {
                    Color.black.opacity(0.6)
                }
            }
            .frame(width: size.width, height: size.height)
        }
    }

    /// Everything that should trigger a new engine frame, in one Equatable.
    private struct RenderStamp: Equatable {
        var seconds: TimeInterval
        var revision: UInt64
        var size: CGSize
        var scale: CGFloat
        var isEmpty: Bool
    }

    private var renderStamp: RenderStamp {
        RenderStamp(
            seconds: quantizedPlayhead,
            revision: state.appliedRevision,
            size: frameSize,
            scale: displayScale,
            isEmpty: state.isEmpty
        )
    }

    /// Playhead snapped to the timeline frame grid: wall-clock playback ticks
    /// (~16 ms) land inside the same 30fps frame about half the time, and the
    /// engine would render an identical image for each. Quantizing lets those
    /// requests dedupe in `PreviewFeed`.
    private var quantizedPlayhead: TimeInterval {
        PreviewFeed.quantize(seconds: state.playhead, fps: state.timelineFPS)
    }

    private func requestFrame() {
        guard !state.isEmpty else { return }
        state.preview.request(
            seconds: quantizedPlayhead,
            revision: state.appliedRevision,
            viewSize: frameSize,
            displayScale: displayScale
        )
    }

    /// Width / height of the project frame. Presets are exact; `.original`
    /// follows the engine's resolved canvas (the footage), falling back to
    /// 9:16 before the first refresh.
    private var frameAspect: CGFloat {
        if state.aspect == .original, let size = state.canvasSize, size.height > 0 {
            return size.width / size.height
        }
        return state.aspect.ratio
    }

    @ViewBuilder
    private var effectBadge: some View {
        let active = state.effects(at: state.playhead)
        if let effect = active.last {
            Label(effect.displayLabel, systemImage: "sparkles")
                .font(.caption2.weight(.medium))
                .foregroundStyle(.white.opacity(0.9))
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(.black.opacity(0.45), in: Capsule())
                .padding(8)
        }
    }

    /// Backdrop outside the letterboxed frame: a blurred echo of the engine
    /// frame (falling back to the clip's art), or the canvas color.
    @ViewBuilder
    private func canvasBackground(for clip: MockClip) -> some View {
        switch state.background.kind {
        case .blur:
            if state.background.blurStrength > 0 {
                blurSource(for: clip)
                    .blur(radius: 12 + 50 * state.background.blurStrength)
                    .opacity(0.55)
            }
        case .color:
            state.background.color
        }
    }

    @ViewBuilder
    private func blurSource(for clip: MockClip) -> some View {
        if let frame = state.preview.image {
            Image(decorative: frame, scale: 1)
                .resizable()
                .scaledToFill()
        } else {
            MockArtView(art: clip.art, symbolSize: 0)
        }
    }
}

#Preview {
    let state = EditorState()
    let _ = state.startProject(with: Array(FixtureLibrary.sampleTimeline.prefix(3)))
    return PreviewCanvas(state: state)
        .frame(height: 420)
}
