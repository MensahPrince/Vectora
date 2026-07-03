import SwiftUI

/// Top-level navigation: Home <-> Editor, with the media picker presented as a
/// full-screen sheet from either the home screen or an empty editor.
struct RootView: View {
    private enum Screen {
        case home
        case editor
    }

    @State private var screen: Screen = .home
    @State private var pickerPresented = false
    @State private var editorClips: [MockClip] = []

    /// Dev shortcut: `-startScreen picker|editor` (e.g. via `simctl launch`)
    /// jumps straight to a screen so states deep in the flow are easy to
    /// reach while iterating on the mock UI.
    init() {
        let arguments = ProcessInfo.processInfo.arguments
        guard let flag = arguments.firstIndex(of: "-startScreen"),
              arguments.indices.contains(flag + 1)
        else { return }

        switch arguments[flag + 1] {
        case "picker":
            _pickerPresented = State(initialValue: true)
        case "editor":
            let items = MockData.libraryItems.prefix(4)
            _editorClips = State(initialValue: items.map(MockClip.init(from:)))
            _screen = State(initialValue: .editor)
        default:
            break
        }
    }

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()

            switch screen {
            case .home:
                HomeView(
                    onNewProject: { pickerPresented = true },
                    onBlankProject: {
                        editorClips = []
                        screen = .editor
                    }
                )
            case .editor:
                EditorView(clips: editorClips, onHome: { screen = .home })
            }
        }
        .preferredColorScheme(.dark)
        #if os(macOS)
        .sheet(isPresented: $pickerPresented) { picker }
        #else
        .fullScreenCover(isPresented: $pickerPresented) { picker }
        #endif
    }

    private var picker: some View {
        MediaPickerView(
            onCancel: { pickerPresented = false },
            onDone: { items in
                editorClips = items.map(MockClip.init(from:))
                pickerPresented = false
                screen = .editor
            }
        )
    }
}

#Preview {
    RootView()
}
