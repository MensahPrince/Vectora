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

    /// Slider value while dragging (committed as one intent on release).
    @State private var draftDuration: Double?

    private var duration: Binding<Double> {
        Binding(
            get: { draftDuration ?? current?.duration ?? 0.5 },
            set: { draftDuration = $0 }
        )
    }

    private func commitDuration(editing: Bool) {
        guard !editing, var transition = current, let draft = draftDuration else { return }
        draftDuration = nil
        transition.duration = draft
        state.setTransition(after: afterClipID, transition)
        appliedToAll = false
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

            PanelSlider(
                label: "Duration", value: duration, range: 0.1...2,
                format: { String(format: "%.1fs", $0) },
                onEditingChanged: commitDuration
            )
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
