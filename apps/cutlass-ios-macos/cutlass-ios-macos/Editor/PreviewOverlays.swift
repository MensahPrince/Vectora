import SwiftUI

/// Renders every overlay clip visible at the playhead inside the preview
/// frame, honoring each clip's normalized position, scale, and rotation.
struct PreviewOverlayLayer: View {
    var state: EditorState

    var body: some View {
        GeometryReader { geo in
            ForEach(state.overlays(at: state.playhead)) { overlay in
                OverlayContentView(clip: overlay, frameSize: geo.size)
                    .scaleEffect(overlay.scale)
                    .rotationEffect(.degrees(overlay.rotationDegrees))
                    .position(
                        x: overlay.posX * geo.size.width,
                        y: overlay.posY * geo.size.height
                    )
            }
        }
        .clipped()
    }
}

/// The visual body of a single overlay clip (text, sticker, or PiP).
struct OverlayContentView: View {
    var clip: MockOverlayClip
    var frameSize: CGSize

    var body: some View {
        switch clip.kind {
        case .text:
            styledText
        case .sticker:
            Image(systemName: clip.symbol ?? "questionmark")
                .font(.system(size: 46))
                .foregroundStyle(.white)
                .shadow(color: .black.opacity(0.35), radius: 4, y: 2)
        case .pip:
            if let art = clip.art {
                MockArtView(art: art, symbolSize: 22)
                    .frame(width: frameSize.width * 0.52, height: frameSize.width * 0.39)
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(.white.opacity(0.7), lineWidth: 1.5)
                    }
                    .shadow(color: .black.opacity(0.4), radius: 8, y: 4)
            }
        }
    }

    @ViewBuilder
    private var styledText: some View {
        let string = clip.text.isEmpty ? "Enter text" : clip.text
        let base = Text(string)
            .font(TextStyling.font(named: clip.fontName, size: 28))
            .multilineTextAlignment(.center)

        switch clip.textEffect {
        case "Neon":
            base.foregroundStyle(clip.textColor)
                .shadow(color: Theme.accent, radius: 6)
                .shadow(color: Theme.accent.opacity(0.8), radius: 14)
        case "Shadow":
            base.foregroundStyle(clip.textColor)
                .shadow(color: .black.opacity(0.85), radius: 3, x: 0, y: 3)
        case "Outline":
            base.foregroundStyle(clip.textColor)
                .shadow(color: .black, radius: 1, x: 1.2, y: 1.2)
                .shadow(color: .black, radius: 1, x: -1.2, y: -1.2)
                .shadow(color: .black, radius: 1, x: 1.2, y: -1.2)
                .shadow(color: .black, radius: 1, x: -1.2, y: 1.2)
        case "Glow":
            base.foregroundStyle(clip.textColor)
                .shadow(color: .white.opacity(0.9), radius: 8)
        case "Retro":
            base.foregroundStyle(clip.textColor)
                .shadow(color: Color(hex: 0xF97316), radius: 0, x: 3, y: 3)
        case "Gradient":
            base.foregroundStyle(
                LinearGradient(
                    colors: [Color(hex: 0xF472B6), Color(hex: 0x60A5FA)],
                    startPoint: .leading,
                    endPoint: .trailing
                )
            )
        case "Chrome":
            base.foregroundStyle(
                LinearGradient(
                    colors: [.white, Color(hex: 0x94A3B8), .white],
                    startPoint: .top,
                    endPoint: .bottom
                )
            )
        default:
            base.foregroundStyle(clip.text.isEmpty ? clip.textColor.opacity(0.55) : clip.textColor)
        }
    }
}
