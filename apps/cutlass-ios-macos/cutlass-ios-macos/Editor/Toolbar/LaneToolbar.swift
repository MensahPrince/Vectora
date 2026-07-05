import SwiftUI

/// One action in a bottom toolbar strip.
struct ToolbarAction: Identifiable {
    var id: String { label }
    var symbol: String
    var label: String
    var action: () -> Void
}

/// Generic scrollable bottom toolbar for selected lane clips (text, sticker,
/// PiP, effect, audio); mirrors ClipToolbar's look.
struct LaneToolbar: View {
    var actions: [ToolbarAction]

    var body: some View {
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
            .padding(.horizontal, 24)
            .frame(maxWidth: .infinity)
        }
        .padding(.top, 10)
        .padding(.bottom, 4)
    }
}

#Preview {
    LaneToolbar(actions: [
        ToolbarAction(symbol: "pencil", label: "Edit", action: {}),
        ToolbarAction(symbol: "scissors", label: "Split", action: {}),
        ToolbarAction(symbol: "trash", label: "Delete", action: {}),
    ])
    .background(Theme.background)
}
