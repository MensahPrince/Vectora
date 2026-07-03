import SwiftUI

/// Transition picker for one clip boundary: style tiles, duration slider,
/// and apply-to-all.
struct TransitionPanel: View {
    var state: EditorState
    var afterClipID: UUID

    @State private var appliedToAll = false

    private var current: MockTransition? {
        state.clips.first { $0.id == afterClipID }?.transitionAfter
    }

    private var duration: Binding<Double> {
        Binding(
            get: { current?.duration ?? 0.5 },
            set: { newValue in
                guard var transition = current else { return }
                transition.duration = newValue
                state.setTransition(after: afterClipID, transition)
            }
        )
    }

    var body: some View {
        VStack(spacing: 6) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 12) {
                    ForEach(MockData.transitionStyles, id: \.self) { style in
                        let selected = style == "None" ? current == nil : current?.style == style
                        PresetTile(
                            name: style,
                            isSelected: selected,
                            art: nil,
                            symbol: MockData.transitionSymbols[style] ?? "circle"
                        ) {
                            if style == "None" {
                                state.setTransition(after: afterClipID, nil)
                            } else {
                                state.setTransition(
                                    after: afterClipID,
                                    MockTransition(style: style, duration: current?.duration ?? 0.5)
                                )
                            }
                            appliedToAll = false
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 6)
            }

            PanelSlider(label: "Duration", value: duration, range: 0.1...2, format: { String(format: "%.1fs", $0) })
                .disabled(current == nil)
                .opacity(current == nil ? 0.4 : 1)

            Button {
                state.applyTransitionToAll(current)
                appliedToAll = true
            } label: {
                Label(
                    appliedToAll ? "Applied to all" : "Apply to all",
                    systemImage: appliedToAll ? "checkmark.circle.fill" : "square.3.layers.3d"
                )
                .font(.footnote.weight(.semibold))
                .foregroundStyle(appliedToAll ? Theme.accent : .white)
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
                .background(Theme.surfaceElevated, in: Capsule())
            }
            .buttonStyle(.plain)
            .disabled(current == nil)
            .opacity(current == nil ? 0.4 : 1)
        }
        .padding(.bottom, 4)
    }
}
