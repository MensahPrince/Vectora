import SwiftUI

/// Volume for the selected PiP overlay clip.
struct OverlayVolumePanel: View {
    var state: EditorState

    private var binding: Binding<Double> {
        Binding(
            get: { state.selectedOverlay?.volume ?? 1 },
            set: { newValue in state.updateSelectedOverlay { $0.volume = newValue } }
        )
    }

    var body: some View {
        PanelSlider(label: "Volume", value: binding, range: 0...2)
            .padding(.vertical, 14)
    }
}

/// Volume for the selected audio clip.
struct AudioVolumePanel: View {
    var state: EditorState

    private var binding: Binding<Double> {
        Binding(
            get: { state.selectedAudio?.volume ?? 1 },
            set: { newValue in state.updateSelectedAudio { $0.volume = newValue } }
        )
    }

    var body: some View {
        PanelSlider(label: "Volume", value: binding, range: 0...2)
            .padding(.vertical, 14)
    }
}

/// Fade in/out lengths for the selected audio clip.
struct AudioFadePanel: View {
    var state: EditorState

    private var fadeIn: Binding<Double> {
        Binding(
            get: { state.selectedAudio?.fadeIn ?? 0 },
            set: { newValue in state.updateSelectedAudio { $0.fadeIn = newValue } }
        )
    }

    private var fadeOut: Binding<Double> {
        Binding(
            get: { state.selectedAudio?.fadeOut ?? 0 },
            set: { newValue in state.updateSelectedAudio { $0.fadeOut = newValue } }
        )
    }

    var body: some View {
        VStack(spacing: 2) {
            PanelSlider(label: "Fade in", value: fadeIn, range: 0...5, format: Self.seconds)
            PanelSlider(label: "Fade out", value: fadeOut, range: 0...5, format: Self.seconds)
        }
        .padding(.vertical, 8)
    }

    nonisolated static func seconds(_ value: Double) -> String {
        String(format: "%.1fs", value)
    }
}
