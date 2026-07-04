import CutlassMobile
import SwiftUI

/// Sticker picker: categorized grid of symbol "stickers"; each tap drops one
/// at the playhead.
struct StickersPanel: View {
    var state: EditorState

    @State private var category = 0

    private let columns = Array(repeating: GridItem(.flexible()), count: 6)

    var body: some View {
        VStack(spacing: 6) {
            PanelTabs(
                tabs: MockData.stickerCategories.map(\.name),
                selection: $category
            )

            ScrollView(showsIndicators: false) {
                LazyVGrid(columns: columns, spacing: 14) {
                    ForEach(MockData.stickerCategories[category].symbols, id: \.self) { symbol in
                        Button {
                            state.addSticker(symbol: symbol)
                        } label: {
                            Image(systemName: symbol)
                                .font(.system(size: 26))
                                .foregroundStyle(.white)
                                .frame(width: 48, height: 48)
                                .background(Theme.surfaceElevated, in: RoundedRectangle(cornerRadius: 10))
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.top, 4)
            }
            .frame(height: 130)
        }
    }
}

/// Effect picker: categorized gradient tiles; each tap adds an effect bar at
/// the playhead.
struct EffectsPanel: View {
    var state: EditorState

    @State private var category = 0

    var body: some View {
        VStack(spacing: 6) {
            PanelTabs(
                tabs: MockData.effectCategories.map(\.name),
                selection: $category
            )

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 12) {
                    ForEach(MockData.effectCategories[category].effects, id: \.self) { effect in
                        PresetTile(
                            name: effect,
                            isSelected: state.selectedEffect?.name == effect,
                            art: MockData.tileArt(for: effect),
                            symbol: nil
                        ) {
                            state.addEffectClip(name: effect, kind: .effect)
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 6)
            }
        }
    }
}

/// Filter picker (root level): tapping a filter adds a filter bar; the
/// slider tunes its intensity.
struct FiltersPanel: View {
    var state: EditorState

    private var intensityBinding: Binding<Double> {
        Binding(
            get: { state.selectedEffect?.intensity ?? 0.8 },
            set: { newValue in state.updateSelectedEffect { $0.intensity = newValue } }
        )
    }

    var body: some View {
        VStack(spacing: 4) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 12) {
                    ForEach(Catalogs.shared.filters) { filter in
                        let selected =
                            state.selectedEffect?.kind == .filter
                            && state.selectedEffect?.filterID == filter.id
                        PresetTile(
                            name: filter.label,
                            isSelected: selected,
                            art: MockData.tileArt(for: filter.label),
                            symbol: nil
                        ) {
                            // Retint the selected bar, or drop a new one.
                            if state.selectedEffect?.kind == .filter {
                                state.updateSelectedEffect {
                                    $0.filterID = filter.id
                                    $0.name = filter.label
                                }
                            } else {
                                state.addFilterClip(id: filter.id, label: filter.label)
                            }
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 6)
            }

            PanelSlider(label: "Intensity", value: intensityBinding, range: 0...1)
                .disabled(state.selectedEffect == nil)
                .opacity(state.selectedEffect == nil ? 0.4 : 1)
        }
    }
}

/// Root-level Adjust: drops an adjust layer at the playhead and edits its
/// grade (persisted on the bar clip via `SetClipAdjustments` on Apply).
struct AdjustPanel: View {
    var state: EditorState

    private func binding(_ keyPath: WritableKeyPath<AdjustValues, Double>) -> Binding<Double> {
        Binding(
            get: { state.selectedEffect?.adjust[keyPath: keyPath] ?? 0 },
            set: { newValue in
                state.updateSelectedEffect { $0.adjust[keyPath: keyPath] = newValue }
            }
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            PanelSlider(label: "Brightness", value: binding(\.brightness), range: -1...1, format: Self.signedPercent)
            PanelSlider(label: "Contrast", value: binding(\.contrast), range: -1...1, format: Self.signedPercent)
            PanelSlider(label: "Saturation", value: binding(\.saturation), range: -1...1, format: Self.signedPercent)
            PanelSlider(label: "Exposure", value: binding(\.exposure), range: -1...1, format: Self.signedPercent)
            PanelSlider(label: "Temperature", value: binding(\.temperature), range: -1...1, format: Self.signedPercent)
        }
        .onAppear {
            if state.selectedEffect?.kind != .adjust {
                state.addEffectClip(name: "Adjust", kind: .adjust)
            }
        }
    }

    nonisolated static func signedPercent(_ value: Double) -> String {
        let rounded = Int((value * 100).rounded())
        return rounded > 0 ? "+\(rounded)" : "\(rounded)"
    }
}
