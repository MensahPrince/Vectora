import SwiftUI

/// One clip on the track: repeating thumbnail tiles with a fake waveform
/// strip when the source has audio. Width is time-accurate
/// (length x points-per-second) so scrubbing lines up with the ruler.
struct ClipView: View {
    var clip: MockClip
    var pointsPerSecond: Double

    static let height: CGFloat = 66
    private static let waveformHeight: CGFloat = 20

    private var width: CGFloat {
        max(2, clip.length * pointsPerSecond)
    }

    var body: some View {
        VStack(spacing: 0) {
            thumbnailStrip(height: clip.hasAudio ? Self.height - Self.waveformHeight : Self.height)
            if clip.hasAudio {
                WaveformStrip(seed: clip.id.hashValue)
                    .frame(height: Self.waveformHeight)
            }
        }
        .frame(width: width, height: Self.height)
        .clipShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
        // Hairline separator so adjacent clips read as distinct without
        // shifting the time math the way real HStack spacing would.
        .overlay(alignment: .trailing) {
            Rectangle()
                .fill(Theme.timelineBed)
                .frame(width: 1)
        }
    }

    private func thumbnailStrip(height: CGFloat) -> some View {
        let tileCount = max(1, Int((width / height).rounded(.up)))
        return HStack(spacing: 0) {
            ForEach(0..<tileCount, id: \.self) { index in
                MockArtView(art: clip.art, symbolSize: 15)
                    .frame(width: height, height: height)
                    // Alternate slight shading to fake distinct frames.
                    .overlay(Color.black.opacity(index.isMultiple(of: 2) ? 0 : 0.10))
                    .clipped()
            }
        }
        .frame(width: width, alignment: .leading)
        .clipped()
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
        ClipView(clip: MockClip(from: MockData.libraryItems[1]), pointsPerSecond: 44)
        ClipView(clip: MockClip(from: MockData.libraryItems[0]), pointsPerSecond: 44)
    }
    .padding()
    .background(Theme.timelineBed)
}
