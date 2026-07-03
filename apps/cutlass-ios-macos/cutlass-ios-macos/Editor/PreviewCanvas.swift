import SwiftUI

/// Mock preview: renders the art of the clip under the playhead inside the
/// project's aspect ratio, over the chosen canvas background (blurred copy
/// or solid color). Empty timelines show a black canvas.
struct PreviewCanvas: View {
    var state: EditorState

    var body: some View {
        ZStack {
            Color.black

            if let clip = state.clip(at: state.playhead) {
                canvasBackground(for: clip)

                MockArtView(art: clip.art, symbolSize: 64)
                    .aspectRatio(state.aspect.ratio, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                    .padding(.vertical, 10)
                    .opacity(clip.opacity)
            } else if state.background.kind == .color {
                state.background.color
            }
        }
        .clipped()
    }

    @ViewBuilder
    private func canvasBackground(for clip: MockClip) -> some View {
        switch state.background.kind {
        case .blur:
            if state.background.blurStrength > 0 {
                MockArtView(art: clip.art, symbolSize: 0)
                    .blur(radius: 12 + 50 * state.background.blurStrength)
                    .opacity(0.55)
                    .clipped()
            }
        case .color:
            state.background.color
        }
    }
}

#Preview {
    let state = EditorState()
    let _ = state.startProject(with: Array(MockData.libraryItems.prefix(3)))
    return PreviewCanvas(state: state)
        .frame(height: 420)
}
