import SwiftUI

/// Home screen stub; the full quick-actions / projects / templates layout
/// lands in the next slice.
struct HomeView: View {
    var onNewProject: () -> Void

    var body: some View {
        VStack(spacing: 24) {
            Text("Cutlass")
                .font(.largeTitle.bold())
                .foregroundStyle(.white)

            Button("New project", action: onNewProject)
                .buttonStyle(.borderedProminent)
                .tint(Theme.accent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
    }
}

#Preview {
    HomeView(onNewProject: {})
}
