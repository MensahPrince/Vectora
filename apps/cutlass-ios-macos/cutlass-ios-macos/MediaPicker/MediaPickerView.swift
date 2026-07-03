import SwiftUI

/// Media picker stub; the mock photo-library grid lands in a later slice.
struct MediaPickerView: View {
    var onCancel: () -> Void
    var onDone: ([MockMediaItem]) -> Void

    var body: some View {
        VStack(spacing: 24) {
            Text("Media picker")
                .font(.title2.bold())
                .foregroundStyle(.white)

            Button("Add sample clips") {
                onDone(Array(MockData.libraryItems.prefix(3)))
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.accent)

            Button("Cancel", action: onCancel)
                .foregroundStyle(Theme.textSecondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
    }
}

#Preview {
    MediaPickerView(onCancel: {}, onDone: { _ in })
}
