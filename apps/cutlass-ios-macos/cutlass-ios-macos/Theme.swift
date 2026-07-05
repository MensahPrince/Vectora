import SwiftUI

/// Design tokens for the Cutlass mobile UI: dark editor chrome with a single
/// vivid accent, modeled on CapCut / Premiere mobile.
enum Theme {
    // MARK: Surfaces

    static let background = Color(hex: 0x0A0A0E)
    static let surface = Color(hex: 0x1A1A21)
    static let surfaceElevated = Color(hex: 0x27272F)
    static let timelineBed = Color(hex: 0x111115)
    static let trackEmpty = Color(hex: 0x1B1B21)

    // MARK: Content

    static let textSecondary = Color.white.opacity(0.62)
    static let textTertiary = Color.white.opacity(0.38)
    static let stroke = Color.white.opacity(0.10)

    // MARK: Accents

    /// Primary action blue (FAB, add buttons, selection badges).
    static let accent = Color(hex: 0x4B5BF7)
    /// Fake audio waveform teal on timeline clips.
    static let waveform = Color(hex: 0x37B0d5)

    /// Purple wash behind the home header, fading into the background.
    static let homeHeader = LinearGradient(
        colors: [Color(hex: 0x39296B), Color(hex: 0x201A40), background],
        startPoint: .top,
        endPoint: .bottom
    )

    /// Premium crown badge fill.
    static let premiumBadge = LinearGradient(
        colors: [Color(hex: 0x8B5CF6), Color(hex: 0xD946EF)],
        startPoint: .topLeading,
        endPoint: .bottomTrailing
    )
}

nonisolated extension Color {
    /// `Color(hex: 0x4B5BF7)` convenience for design tokens and mock art.
    init(hex: UInt32, opacity: Double = 1) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            opacity: opacity
        )
    }
}
