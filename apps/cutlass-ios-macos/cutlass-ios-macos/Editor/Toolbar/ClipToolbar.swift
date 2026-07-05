import SwiftUI

/// Bottom toolbar when a main-track clip is selected: fixed add button, then
/// the full scrollable strip of clip operations and property panels.
struct ClipToolbar: View {
    var onAdd: () -> Void
    var actions: [ToolbarAction]

    var body: some View {
        HStack(spacing: 6) {
            Button(action: onAdd) {
                Circle()
                    .fill(Theme.accent)
                    .frame(width: 44, height: 44)
                    .overlay {
                        Image(systemName: "plus")
                            .font(.system(size: 19, weight: .semibold))
                            .foregroundStyle(.white)
                    }
                    .shadow(color: .black.opacity(0.4), radius: 6, y: 2)
            }
            .buttonStyle(.plain)
            .padding(.leading, 14)

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 26) {
                    ForEach(actions) { entry in
                        Button(action: entry.action) {
                            VStack(spacing: 6) {
                                Image(systemName: entry.symbol)
                                    .font(.system(size: 19, weight: .regular))
                                    .foregroundStyle(.white)
                                    .frame(height: 22)
                                Text(entry.label)
                                    .font(.system(size: 11))
                                    .foregroundStyle(Theme.textSecondary)
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 18)
            }
        }
        .padding(.top, 10)
        .padding(.bottom, 4)
    }
}

#Preview {
    ClipToolbar(onAdd: {}, actions: [
        ToolbarAction(symbol: "scissors", label: "Split", action: {}),
        ToolbarAction(symbol: "speedometer", label: "Speed", action: {}),
        ToolbarAction(symbol: "trash", label: "Delete", action: {}),
    ])
    .background(Theme.background)
}
