import SwiftUI

/// Default bottom toolbar (nothing selected): horizontally scrollable root
/// entry points, CapCut-style.
struct MediaToolbar: View {
    var onAddMedia: () -> Void
    var onAddOverlay: () -> Void
    var onOpenPanel: (EditorPanel) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 26) {
                item("photo.badge.plus", "Videos\nand images", action: onAddMedia)
                item("waveform.badge.plus", "Music\nand audio") { onOpenPanel(.audio) }
                item("textformat", "Titles\nand captions") { onOpenPanel(.text(editing: nil, tab: 0)) }
                item("face.smiling", "Stickers") { onOpenPanel(.stickers) }
                item("square.on.square.badge.person.crop", "Overlay", action: onAddOverlay)
                item("sparkles", "Effects") { onOpenPanel(.effects) }
                item("camera.filters", "Filters") { onOpenPanel(.filters) }
                item("slider.horizontal.3", "Adjust") { onOpenPanel(.adjust) }
                item("captions.bubble", "Captions") { onOpenPanel(.captions) }
                item("aspectratio", "Aspect\nratio") { onOpenPanel(.aspect) }
                item("rectangle.checkered", "Background") { onOpenPanel(.background) }
            }
            .padding(.horizontal, 24)
        }
        .padding(.top, 10)
        .padding(.bottom, 4)
    }

    private func item(
        _ symbol: String,
        _ label: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            VStack(spacing: 6) {
                Image(systemName: symbol)
                    .font(.system(size: 21, weight: .regular))
                    .foregroundStyle(.white)
                    .frame(height: 24)
                Text(label)
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.textSecondary)
                    .multilineTextAlignment(.center)
                    .lineLimit(2, reservesSpace: true)
            }
        }
        .buttonStyle(.plain)
    }
}

#Preview {
    MediaToolbar(onAddMedia: {}, onAddOverlay: {}, onOpenPanel: { _ in })
        .background(Theme.background)
}
