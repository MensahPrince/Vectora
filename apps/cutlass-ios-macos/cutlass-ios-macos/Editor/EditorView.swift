import SwiftUI

/// Editor stub; preview, transport, timeline, and toolbars land in later
/// slices.
struct EditorView: View {
    var clips: [MockClip]
    var onHome: () -> Void

    var body: some View {
        VStack(spacing: 24) {
            Text("Editor — \(clips.count) clip(s)")
                .font(.title3.bold())
                .foregroundStyle(.white)

            Button("Home", action: onHome)
                .buttonStyle(.borderedProminent)
                .tint(Theme.accent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
    }
}

#Preview {
    EditorView(clips: [], onHome: {})
}
