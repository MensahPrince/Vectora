import CutlassMobile
import SwiftUI

/// Tabbed text tool: keyboard input, fonts, style (color + effect), and
/// animation presets. Opening in add-mode drops a text clip at the playhead
/// immediately so the preview reacts to every change.
struct TextPanel: View {
    var state: EditorState
    /// nil = panel was opened to create a new text clip.
    var editingID: UUID?

    @State private var tab: Int
    @FocusState private var keyboardFocused: Bool

    init(state: EditorState, editingID: UUID?, initialTab: Int = 0) {
        self.state = state
        self.editingID = editingID
        _tab = State(initialValue: initialTab)
    }

    var body: some View {
        VStack(spacing: 8) {
            PanelTabs(tabs: ["Keyboard", "Fonts", "Style", "Animation"], selection: $tab)

            switch tab {
            case 0: keyboardTab
            case 1: fontsTab
            case 2: styleTab
            default: animationTab
            }
        }
        .frame(height: 190, alignment: .top)
        .onAppear {
            if editingID == nil, state.selectedOverlay?.kind != .text {
                state.addTextClip()
            } else if let editingID {
                state.selection = .overlay(editingID)
            }
            keyboardFocused = true
        }
    }

    private var textBinding: Binding<String> {
        Binding(
            get: { state.selectedOverlay?.text ?? "" },
            set: { newValue in state.updateSelectedOverlay { $0.text = newValue } }
        )
    }

    private var keyboardTab: some View {
        TextField("Enter text", text: textBinding, axis: .vertical)
            .focused($keyboardFocused)
            .font(.body)
            .foregroundStyle(.white)
            .lineLimit(2...3)
            .padding(12)
            .background(Theme.surfaceElevated, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            .padding(.horizontal, 16)
    }

    private var fontsTab: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 12) {
                ForEach(MockData.fonts, id: \.self) { font in
                    let selected = state.selectedOverlay?.fontName == font
                    Button {
                        state.updateSelectedOverlay { $0.fontName = font }
                    } label: {
                        VStack(spacing: 6) {
                            RoundedRectangle(cornerRadius: 10, style: .continuous)
                                .fill(Theme.surfaceElevated)
                                .frame(width: 58, height: 58)
                                .overlay {
                                    Text("Aa")
                                        .font(TextStyling.font(named: font, size: 22))
                                        .foregroundStyle(.white)
                                }
                                .overlay {
                                    if selected {
                                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                                            .strokeBorder(Theme.accent, lineWidth: 2.5)
                                    }
                                }
                            Text(font)
                                .font(.system(size: 10.5))
                                .foregroundStyle(selected ? .white : Theme.textSecondary)
                        }
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 16)
        }
    }

    private var styleTab: some View {
        VStack(spacing: 14) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 12) {
                    ForEach(Array(MockData.textColors.enumerated()), id: \.offset) { _, color in
                        let selected = state.selectedOverlay?.textColor == color
                        Button {
                            state.updateSelectedOverlay { $0.textColor = color }
                        } label: {
                            Circle()
                                .fill(color)
                                .frame(width: 30, height: 30)
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
                .padding(.horizontal, 16)
            }

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 10) {
                    chip("None", selected: state.selectedOverlay?.textEffect == nil) {
                        state.updateSelectedOverlay { $0.textEffect = nil }
                    }
                    ForEach(Catalogs.shared.textEffects) { effect in
                        let selected = state.selectedOverlay?.textEffect == effect.id
                        chip(effect.label, selected: selected) {
                            state.updateSelectedOverlay { $0.textEffect = effect.id }
                        }
                    }
                }
                .padding(.horizontal, 16)
            }
        }
    }

    /// Text animations are the catalog's text-only combo presets.
    private var animationTab: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 10) {
                chip("None", selected: state.selectedOverlay?.animation == nil) {
                    state.updateSelectedOverlay { $0.animation = nil }
                }
                let presets = Catalogs.shared.animations.filter { $0.textOnly }
                ForEach(presets) { animation in
                    let selected = state.selectedOverlay?.animation == animation.id
                    chip(animation.label, selected: selected) {
                        state.updateSelectedOverlay { $0.animation = animation.id }
                    }
                }
            }
            .padding(.horizontal, 16)
        }
    }

    private func chip(_ label: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
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
}

/// Auto-captions stub: language chips + a generate button that drops a
/// caption-styled text clip at the playhead.
struct CaptionsPanel: View {
    var state: EditorState
    var onGenerated: () -> Void

    @State private var language = "English"

    var body: some View {
        VStack(spacing: 16) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 10) {
                    ForEach(MockData.captionLanguages, id: \.self) { candidate in
                        Button {
                            language = candidate
                        } label: {
                            Text(candidate)
                                .font(.footnote.weight(language == candidate ? .semibold : .regular))
                                .foregroundStyle(language == candidate ? .white : Theme.textSecondary)
                                .padding(.horizontal, 14)
                                .padding(.vertical, 8)
                                .background(
                                    language == candidate
                                        ? AnyShapeStyle(Theme.accent)
                                        : AnyShapeStyle(Theme.surfaceElevated),
                                    in: Capsule()
                                )
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 16)
            }

            Text("Captions are generated from clip audio. This design preview inserts a sample caption.")
                .font(.caption)
                .foregroundStyle(Theme.textTertiary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 30)

            Button {
                let id = state.addTextClip(text: "This is your auto caption")
                state.selection = .overlay(id)
                state.updateSelectedOverlay {
                    $0.textEffect = "outline"
                    $0.posY = 0.8
                }
                onGenerated()
            } label: {
                Text("Generate captions")
                    .font(.headline)
                    .foregroundStyle(.white)
                    .padding(.horizontal, 30)
                    .padding(.vertical, 12)
                    .background(Theme.accent, in: Capsule())
            }
            .buttonStyle(.plain)
        }
        .padding(.vertical, 10)
    }
}

/// Maps mock font/effect names onto approximated SwiftUI styling.
nonisolated enum TextStyling {
    static func font(named name: String, size: CGFloat) -> Font {
        switch name {
        case "Serif": return .system(size: size, design: .serif)
        case "Rounded": return .system(size: size, weight: .semibold, design: .rounded)
        case "Mono": return .system(size: size, design: .monospaced)
        case "Condensed": return .system(size: size, weight: .heavy)
        case "Handwritten": return .system(size: size, weight: .medium, design: .serif).italic()
        case "Poster": return .system(size: size, weight: .black, design: .rounded)
        case "Typewriter": return .system(size: size, weight: .medium, design: .monospaced)
        default: return .system(size: size, weight: .semibold)
        }
    }
}
