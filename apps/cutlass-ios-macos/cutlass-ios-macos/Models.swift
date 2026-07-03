import SwiftUI

// Mock-only data model for the UI build: no engine, no FFI, no real media.
// Every "thumbnail" is a gradient plus an SF Symbol stand-in.
//
// The target defaults to MainActor isolation; these are plain value types
// used from anywhere, so they opt out.

/// Placeholder artwork used anywhere real media would render.
nonisolated struct MockArt: Hashable {
    var top: Color
    var bottom: Color
    var symbol: String?

    var gradient: LinearGradient {
        LinearGradient(colors: [top, bottom], startPoint: .topLeading, endPoint: .bottomTrailing)
    }
}

/// A saved project card on the home screen.
nonisolated struct MockProject: Identifiable, Hashable {
    var id = UUID()
    var dateLabel: String
    var duration: TimeInterval
    var art: MockArt
}

/// A template card in the home screen carousels.
nonisolated struct MockTemplate: Identifiable, Hashable {
    var id = UUID()
    var caption: String?
    var art: MockArt
}

nonisolated struct MockTemplateSection: Identifiable, Hashable {
    var id = UUID()
    var title: String
    var subtitle: String
    var templates: [MockTemplate]
}

/// An item in the mock photo-library picker.
nonisolated struct MockMediaItem: Identifiable, Hashable {
    enum Kind: Hashable {
        case photo
        case video(duration: TimeInterval)
    }

    var id = UUID()
    var kind: Kind
    var art: MockArt

    var videoDuration: TimeInterval? {
        if case .video(let duration) = kind { return duration }
        return nil
    }
}

/// A clip on the (single, sequential) timeline track.
nonisolated struct MockClip: Identifiable, Hashable {
    /// Still images can be stretched up to this long.
    static let photoMaxDuration: TimeInterval = 30
    static let photoDefaultDuration: TimeInterval = 3
    static let minDuration: TimeInterval = 0.5

    var id = UUID()
    var art: MockArt
    /// Full length of the underlying source media.
    var sourceDuration: TimeInterval
    /// Trim in-point within the source.
    var trimStart: TimeInterval = 0
    /// Trimmed length shown on the timeline.
    var length: TimeInterval
    var hasAudio: Bool

    init(from item: MockMediaItem) {
        art = item.art
        switch item.kind {
        case .photo:
            sourceDuration = Self.photoMaxDuration
            length = Self.photoDefaultDuration
            hasAudio = false
        case .video(let duration):
            sourceDuration = duration
            length = duration
            hasAudio = true
        }
    }
}

nonisolated extension TimeInterval {
    /// "1:34"-style label used on badges and the timeline ruler.
    var timecode: String {
        let total = Int(self.rounded())
        return String(format: "%d:%02d", total / 60, total % 60)
    }
}
