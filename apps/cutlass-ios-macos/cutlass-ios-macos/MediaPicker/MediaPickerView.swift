import SwiftUI

/// Full-screen mock photo-library picker: Cancel/Skip pills, a Photos |
/// Collections segmented pill, a 3-column grid with ordered multi-select,
/// and a floating bottom action bar.
struct MediaPickerView: View {
    private enum Tab: String, CaseIterable {
        case photos = "Photos"
        case collections = "Collections"
    }

    var onCancel: () -> Void
    var onDone: ([MockMediaItem]) -> Void

    @State private var tab: Tab = .photos
    @State private var selection: [MockMediaItem] = []

    private let columns = Array(repeating: GridItem(.flexible(), spacing: 2), count: 3)

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()

            VStack(spacing: 14) {
                topBar
                segmentedPill
                switch tab {
                case .photos:
                    grid
                case .collections:
                    collectionsPlaceholder
                }
            }
        }
        .overlay(alignment: .bottom) {
            bottomBar
        }
    }

    private var topBar: some View {
        HStack {
            Button("Cancel", action: onCancel)
                .font(.subheadline)
                .foregroundStyle(.white)
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
                .background(Theme.surfaceElevated, in: Capsule())
                .buttonStyle(.plain)

            Spacer()

            // Skip opens the editor with an empty timeline.
            Button("Skip") { onDone([]) }
                .font(.subheadline)
                .foregroundStyle(.white)
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
                .background(Theme.surfaceElevated, in: Capsule())
                .buttonStyle(.plain)
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
    }

    private var segmentedPill: some View {
        HStack(spacing: 4) {
            ForEach(Tab.allCases, id: \.self) { candidate in
                Button {
                    tab = candidate
                } label: {
                    Text(candidate.rawValue)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(tab == candidate ? .white : Theme.textTertiary)
                        .padding(.horizontal, 18)
                        .padding(.vertical, 7)
                        .background {
                            if tab == candidate {
                                Capsule().fill(Theme.surfaceElevated)
                            }
                        }
                }
                .buttonStyle(.plain)
            }
        }
        .padding(3)
        .background(Theme.surface, in: Capsule())
    }

    private var grid: some View {
        ScrollView(showsIndicators: false) {
            LazyVGrid(columns: columns, spacing: 2) {
                ForEach(MockData.libraryItems) { item in
                    MediaCell(item: item, selectionIndex: selectionIndex(of: item))
                        .onTapGesture { toggle(item) }
                }
            }
            .padding(.bottom, 110)
        }
    }

    private var collectionsPlaceholder: some View {
        VStack(spacing: 10) {
            Image(systemName: "rectangle.stack")
                .font(.system(size: 34))
                .foregroundStyle(Theme.textTertiary)
            Text("No collections yet")
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var bottomBar: some View {
        HStack {
            Image(systemName: "line.3.horizontal.decrease")
                .font(.system(size: 17, weight: .medium))
                .foregroundStyle(.white)
                .frame(width: 46, height: 46)
                .background(Theme.surfaceElevated, in: Circle())

            Spacer()

            if selection.isEmpty {
                Text("Select Items")
                    .font(.headline)
                    .foregroundStyle(.white)
            } else {
                Button {
                    onDone(selection)
                } label: {
                    Text("Add \(selection.count)")
                        .font(.headline)
                        .foregroundStyle(.white)
                        .padding(.horizontal, 28)
                        .padding(.vertical, 12)
                        .background(Theme.accent, in: Capsule())
                }
                .buttonStyle(.plain)
            }

            Spacer()

            Image(systemName: "magnifyingglass")
                .font(.system(size: 17, weight: .medium))
                .foregroundStyle(.white)
                .frame(width: 46, height: 46)
                .background(Theme.surfaceElevated, in: Circle())
        }
        .padding(.horizontal, 18)
        .padding(.bottom, 8)
    }

    private func selectionIndex(of item: MockMediaItem) -> Int? {
        selection.firstIndex(of: item).map { $0 + 1 }
    }

    private func toggle(_ item: MockMediaItem) {
        if let index = selection.firstIndex(of: item) {
            selection.remove(at: index)
        } else {
            selection.append(item)
        }
    }
}

#Preview {
    MediaPickerView(onCancel: {}, onDone: { _ in })
}
