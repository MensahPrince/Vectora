import SwiftUI

/// One square cell in the picker grid; shows a duration badge for videos and
/// a numbered badge + border when selected.
struct MediaCell: View {
    var item: MockMediaItem
    /// 1-based position in the selection order, nil when unselected.
    var selectionIndex: Int?

    var body: some View {
        MockArtView(art: item.art, symbolSize: 22)
            .aspectRatio(1, contentMode: .fill)
            .overlay(alignment: .bottomLeading) {
                if let duration = item.videoDuration {
                    DurationBadge(duration: duration)
                        .padding(4)
                }
            }
            .overlay {
                if selectionIndex != nil {
                    Rectangle()
                        .fill(.black.opacity(0.35))
                    Rectangle()
                        .strokeBorder(.white, lineWidth: 2)
                }
            }
            .overlay(alignment: .topTrailing) {
                if let index = selectionIndex {
                    Text("\(index)")
                        .font(.caption2.bold())
                        .foregroundStyle(.white)
                        .frame(width: 20, height: 20)
                        .background(Theme.accent, in: Circle())
                        .padding(5)
                }
            }
            .contentShape(Rectangle())
    }
}

#Preview {
    HStack(spacing: 2) {
        MediaCell(item: MockData.libraryItems[1], selectionIndex: nil)
        MediaCell(item: MockData.libraryItems[3], selectionIndex: 2)
    }
    .background(Theme.background)
}
