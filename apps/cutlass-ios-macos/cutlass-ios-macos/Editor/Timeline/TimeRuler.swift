import SwiftUI

/// Scrolling tick ruler above the track: a label every interval with a dot
/// midway between labels. The label interval widens as the timeline zooms
/// out so text never collides.
struct TimeRuler: View {
    var duration: TimeInterval
    var pointsPerSecond: Double

    private static let niceIntervals: [TimeInterval] = [1, 2, 5, 10, 15, 30, 60]

    private var interval: TimeInterval {
        let minLabelSpacing: Double = 56
        return Self.niceIntervals.first { $0 * pointsPerSecond >= minLabelSpacing } ?? 60
    }

    var body: some View {
        let interval = interval
        let cellWidth = interval * pointsPerSecond
        let cellCount = max(1, Int((duration / interval).rounded(.up)))

        HStack(spacing: 0) {
            ForEach(0..<cellCount, id: \.self) { index in
                Text((TimeInterval(index) * interval).timecode)
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(index == 0 ? .white : Theme.textTertiary)
                    .fontWeight(index == 0 ? .semibold : .regular)
                    .lineLimit(1)
                    .fixedSize()
                    .frame(width: cellWidth, alignment: .leading)
                    .overlay {
                        Circle()
                            .fill(Theme.textTertiary)
                            .frame(width: 2.5, height: 2.5)
                    }
            }
        }
        .frame(height: 18)
    }
}

#Preview {
    ScrollView(.horizontal) {
        TimeRuler(duration: 30, pointsPerSecond: 44)
    }
    .background(Theme.background)
}
