import CutlassMobile
import SwiftUI

/// Volume for the selected main-track clip (0 shows the mute badge).
struct ClipVolumePanel: View {
    var state: EditorState

    private var binding: Binding<Double> {
        Binding(
            get: { state.selectedClip?.volume ?? 1 },
            set: { newValue in state.updateSelectedClip { $0.volume = newValue } }
        )
    }

    var body: some View {
        PanelSlider(label: "Volume", value: binding, range: 0...2)
            .padding(.vertical, 14)
    }
}

/// Speed: log-scaled constant slider (0.1x-10x, 1x centered) that rescales
/// the clip's timeline length, plus curve preset tiles.
struct SpeedPanel: View {
    var state: EditorState

    /// Slider position 0...1 mapped to speed 0.1...10 logarithmically.
    private var logBinding: Binding<Double> {
        Binding(
            get: { (log10(state.selectedClip?.speed ?? 1) + 1) / 2 },
            set: { newValue in state.setSelectedSpeed(pow(10, newValue * 2 - 1)) }
        )
    }

    var body: some View {
        VStack(spacing: 10) {
            HStack(spacing: 12) {
                Text("Speed")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
                    .frame(width: 82, alignment: .leading)

                Slider(value: logBinding, in: 0...1)
                    .tint(Theme.accent)

                Text(String(format: "%.1fx", state.selectedClip?.speed ?? 1))
                    .font(.footnote.weight(.semibold).monospacedDigit())
                    .foregroundStyle(.white)
                    .frame(width: 44, alignment: .trailing)
            }
            .padding(.horizontal, 18)

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 12) {
                    PresetTile(
                        name: "Constant",
                        isSelected: state.selectedClip?.speedCurve == nil,
                        art: nil,
                        symbol: "minus"
                    ) {
                        state.updateSelectedClip { $0.speedCurve = nil }
                    }
                    ForEach(Catalogs.shared.speedPresets) { preset in
                        PresetTile(
                            name: preset.label,
                            isSelected: state.selectedClip?.speedCurve == preset.id,
                            art: nil,
                            symbol: "point.topleft.down.to.point.bottomright.curvepath"
                        ) {
                            state.updateSelectedClip { $0.speedCurve = preset.id }
                        }
                    }
                }
                .padding(.horizontal, 16)
            }
        }
        .padding(.vertical, 6)
    }
}

/// In/Out/Combo animation presets for the selected clip.
struct ClipAnimationPanel: View {
    var state: EditorState

    @State private var tab = 0

    var body: some View {
        VStack(spacing: 8) {
            PanelTabs(tabs: ["In", "Out", "Combo"], selection: $tab)

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 10) {
                    chip(label: "None", id: nil)
                    ForEach(options) { preset in
                        chip(label: preset.label, id: preset.id)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
            }
        }
    }

    private func chip(label: String, id: String?) -> some View {
        let selected = current == id
        return Button {
            select(id)
        } label: {
            Text(label)
                .font(.footnote.weight(selected ? .semibold : .regular))
                .foregroundStyle(selected ? .white : Theme.textSecondary)
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .background(
                    selected ? AnyShapeStyle(Theme.accent) : AnyShapeStyle(Theme.surfaceElevated),
                    in: Capsule()
                )
        }
        .buttonStyle(.plain)
    }

    private var slot: String {
        switch tab {
        case 0: return "in"
        case 1: return "out"
        default: return "combo"
        }
    }

    private var options: [AnimationCatalogEntry] {
        Catalogs.shared.animations(slot: slot, includeTextOnly: false)
    }

    private var current: String? {
        switch tab {
        case 0: return state.selectedClip?.animationIn
        case 1: return state.selectedClip?.animationOut
        default: return state.selectedClip?.animationCombo
        }
    }

    /// Combo animations replace in/out and vice versa, like CapCut (the
    /// engine enforces the same rule on commit).
    private func select(_ id: String?) {
        state.updateSelectedClip { clip in
            switch tab {
            case 0:
                clip.animationIn = id
                if id != nil { clip.animationCombo = nil }
            case 1:
                clip.animationOut = id
                if id != nil { clip.animationCombo = nil }
            default:
                clip.animationCombo = id
                if id != nil {
                    clip.animationIn = nil
                    clip.animationOut = nil
                }
            }
        }
    }
}

/// Filter presets + intensity for the selected clip.
struct ClipFilterPanel: View {
    var state: EditorState

    private var intensity: Binding<Double> {
        Binding(
            get: { state.selectedClip?.filterIntensity ?? 0.8 },
            set: { newValue in state.updateSelectedClip { $0.filterIntensity = newValue } }
        )
    }

    var body: some View {
        VStack(spacing: 4) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 12) {
                    PresetTile(
                        name: "None",
                        isSelected: state.selectedClip?.filterName == nil,
                        art: nil,
                        symbol: "slash.circle"
                    ) {
                        state.updateSelectedClip { $0.filterName = nil }
                    }
                    ForEach(Catalogs.shared.filters) { filter in
                        PresetTile(
                            name: filter.label,
                            isSelected: state.selectedClip?.filterName == filter.id,
                            art: MockData.tileArt(for: filter.label),
                            symbol: nil
                        ) {
                            state.updateSelectedClip { $0.filterName = filter.id }
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 6)
            }

            PanelSlider(label: "Intensity", value: intensity, range: 0...1)
                .disabled(state.selectedClip?.filterName == nil)
                .opacity(state.selectedClip?.filterName == nil ? 0.4 : 1)
        }
    }
}

/// Color-grade sliders stored on the selected clip.
struct ClipAdjustPanel: View {
    var state: EditorState

    private func binding(_ keyPath: WritableKeyPath<AdjustValues, Double>) -> Binding<Double> {
        Binding(
            get: { state.selectedClip?.adjust[keyPath: keyPath] ?? 0 },
            set: { newValue in state.updateSelectedClip { $0.adjust[keyPath: keyPath] = newValue } }
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            PanelSlider(label: "Brightness", value: binding(\.brightness), range: -1...1, format: AdjustPanel.signedPercent)
            PanelSlider(label: "Contrast", value: binding(\.contrast), range: -1...1, format: AdjustPanel.signedPercent)
            PanelSlider(label: "Saturation", value: binding(\.saturation), range: -1...1, format: AdjustPanel.signedPercent)
            PanelSlider(label: "Exposure", value: binding(\.exposure), range: -1...1, format: AdjustPanel.signedPercent)
            PanelSlider(label: "Temperature", value: binding(\.temperature), range: -1...1, format: AdjustPanel.signedPercent)
        }
    }
}

/// Opacity slider; the preview letterbox reflects it live.
struct OpacityPanel: View {
    var state: EditorState

    private var binding: Binding<Double> {
        Binding(
            get: { state.selectedClip?.opacity ?? 1 },
            set: { newValue in state.updateSelectedClip { $0.opacity = newValue } }
        )
    }

    var body: some View {
        PanelSlider(label: "Opacity", value: binding, range: 0...1)
            .padding(.vertical, 14)
    }
}

/// Crop ratio presets (design-only).
struct CropPanel: View {
    var state: EditorState

    private static let presets: [(name: String, symbol: String)] = [
        ("Free", "crop"), ("9:16", "rectangle.portrait"), ("16:9", "rectangle"),
        ("1:1", "square"), ("4:3", "rectangle.ratio.4.to.3"), ("3:4", "rectangle.ratio.3.to.4"),
    ]

    var body: some View {
        HStack(spacing: 14) {
            ForEach(Self.presets, id: \.name) { preset in
                let selected = (state.selectedClip?.cropPreset ?? "Free") == preset.name
                PresetTile(name: preset.name, isSelected: selected, art: nil, symbol: preset.symbol) {
                    state.updateSelectedClip {
                        $0.cropPreset = preset.name == "Free" ? nil : preset.name
                    }
                }
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 10)
    }
}

/// Mask shape presets.
struct MaskPanel: View {
    var state: EditorState

    private static let symbols: [String: String] = [
        "linear": "rectangle.split.1x2", "mirror": "rectangle.split.3x1",
        "circle": "circle", "rectangle": "rectangle", "heart": "heart", "star": "star",
    ]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 14) {
                PresetTile(
                    name: "None",
                    isSelected: state.selectedClip?.maskName == nil,
                    art: nil,
                    symbol: "slash.circle"
                ) {
                    state.updateSelectedClip { $0.maskName = nil }
                }
                ForEach(Catalogs.shared.masks) { mask in
                    PresetTile(
                        name: mask.label,
                        isSelected: state.selectedClip?.maskName == mask.id,
                        art: nil,
                        symbol: Self.symbols[mask.id] ?? "circle"
                    ) {
                        state.updateSelectedClip { $0.maskName = mask.id }
                    }
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
        }
    }
}

/// Chroma key: pick a key color, then strength/shadow sliders.
struct ChromaPanel: View {
    var state: EditorState

    private static let keyColors: [Color] = [
        Color(hex: 0x22C55E), Color(hex: 0x3B82F6), Color(hex: 0xE879F9),
        Color(hex: 0xF43F5E), Color(hex: 0xFACC15), .white,
    ]

    private var strength: Binding<Double> {
        Binding(
            get: { state.selectedClip?.chromaStrength ?? 0 },
            set: { newValue in state.updateSelectedClip { $0.chromaStrength = newValue } }
        )
    }

    private var shadow: Binding<Double> {
        Binding(
            get: { state.selectedClip?.chromaShadow ?? 0 },
            set: { newValue in state.updateSelectedClip { $0.chromaShadow = newValue } }
        )
    }

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 14) {
                Text("Key color")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)

                ForEach(Array(Self.keyColors.enumerated()), id: \.offset) { _, color in
                    let selected = state.selectedClip?.chromaColor == color
                    Button {
                        state.updateSelectedClip { $0.chromaColor = color }
                    } label: {
                        Circle()
                            .fill(color)
                            .frame(width: 26, height: 26)
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
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 18)

            PanelSlider(label: "Strength", value: strength, range: 0...1)
            PanelSlider(label: "Shadow", value: shadow, range: 0...1)
        }
        .padding(.vertical, 4)
    }
}

/// Stabilization level tiles.
struct StabilizePanel: View {
    var state: EditorState

    var body: some View {
        HStack(spacing: 14) {
            PresetTile(
                name: "None",
                isSelected: state.selectedClip?.stabilizeLevel == nil,
                art: nil,
                symbol: "slash.circle"
            ) {
                state.updateSelectedClip { $0.stabilizeLevel = nil }
            }
            ForEach(Catalogs.shared.stabilizeLevels) { level in
                PresetTile(
                    name: level.label,
                    isSelected: state.selectedClip?.stabilizeLevel == level.id,
                    art: nil,
                    symbol: "gyroscope"
                ) {
                    state.updateSelectedClip { $0.stabilizeLevel = level.id }
                }
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 10)
    }
}
