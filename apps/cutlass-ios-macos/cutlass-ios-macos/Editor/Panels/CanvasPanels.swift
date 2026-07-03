import SwiftUI

/// Aspect ratio picker: pill row of presets, applied live to the preview.
struct AspectPanel: View {
    var state: EditorState

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 18) {
                ForEach(AspectRatio.allCases, id: \.self) { option in
                    Button {
                        state.aspect = option
                    } label: {
                        VStack(spacing: 7) {
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                .strokeBorder(
                                    state.aspect == option ? Theme.accent : Theme.textTertiary,
                                    lineWidth: 2
                                )
                                .frame(
                                    width: option.ratio >= 1 ? 44 : 44 * option.ratio,
                                    height: option.ratio >= 1 ? 44 / option.ratio : 44
                                )
                                .frame(width: 48, height: 48)

                            Text(option.rawValue)
                                .font(.system(size: 11))
                                .foregroundStyle(state.aspect == option ? .white : Theme.textSecondary)
                        }
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 8)
        }
    }
}

/// Canvas background: blur strengths or a solid color behind the footage.
struct BackgroundPanel: View {
    var state: EditorState

    @State private var tab: Int

    init(state: EditorState) {
        self.state = state
        _tab = State(initialValue: state.background.kind == .blur ? 0 : 1)
    }

    private static let blurLevels: [(name: String, strength: Double)] = [
        ("Off", 0), ("Light", 0.3), ("Medium", 0.55), ("Heavy", 1),
    ]

    var body: some View {
        VStack(spacing: 10) {
            PanelTabs(tabs: ["Blur", "Color"], selection: $tab)

            if tab == 0 {
                HStack(spacing: 14) {
                    ForEach(Self.blurLevels, id: \.name) { level in
                        let selected = state.background.kind == .blur
                            && abs(state.background.blurStrength - level.strength) < 0.01
                        PresetTile(
                            name: level.name,
                            isSelected: selected,
                            art: nil,
                            symbol: level.strength == 0 ? "slash.circle" : "drop.fill"
                        ) {
                            state.background.kind = .blur
                            state.background.blurStrength = level.strength
                        }
                    }
                }
                .padding(.vertical, 6)
            } else {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 12) {
                        ForEach(Array(MockData.backgroundColors.enumerated()), id: \.offset) { _, color in
                            let selected = state.background.kind == .color && state.background.color == color
                            Button {
                                state.background.kind = .color
                                state.background.color = color
                            } label: {
                                Circle()
                                    .fill(color)
                                    .frame(width: 36, height: 36)
                                    .overlay {
                                        Circle().strokeBorder(
                                            selected ? Theme.accent : Theme.stroke,
                                            lineWidth: selected ? 3 : 1
                                        )
                                    }
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)
                }
            }
        }
    }
}

#Preview {
    VStack(spacing: 20) {
        AspectPanel(state: EditorState())
        BackgroundPanel(state: EditorState())
    }
    .background(Theme.surface)
}
