import SwiftUI

/// Canned content that fills every screen of the mock UI.
enum MockData {
    // MARK: Art palette

    static let night = MockArt(top: Color(hex: 0x312E81), bottom: Color(hex: 0x0B1026), symbol: "moon.stars.fill")
    static let ocean = MockArt(top: Color(hex: 0x0EA5E9), bottom: Color(hex: 0x1E3A8A), symbol: "water.waves")
    static let beach = MockArt(top: Color(hex: 0x34D399), bottom: Color(hex: 0x065F46), symbol: "beach.umbrella.fill")
    static let portrait = MockArt(top: Color(hex: 0xF472B6), bottom: Color(hex: 0x831843), symbol: "person.fill")
    static let food = MockArt(top: Color(hex: 0xFBBF24), bottom: Color(hex: 0x92400E), symbol: "fork.knife")
    static let podcast = MockArt(top: Color(hex: 0x8B5CF6), bottom: Color(hex: 0x4C1D95), symbol: "mic.fill")
    static let city = MockArt(top: Color(hex: 0x64748B), bottom: Color(hex: 0x0F172A), symbol: "building.2.fill")
    // Not "qrcode": the iOS 26 simulator rejects that glyph and spams
    // CoreUI "No symbol named 'qrcode'" logs on every render.
    static let screen = MockArt(top: Color(hex: 0x475569), bottom: Color(hex: 0x1E293B), symbol: "display")
    static let coffee = MockArt(top: Color(hex: 0xA16207), bottom: Color(hex: 0x451A03), symbol: "cup.and.saucer.fill")
    static let forest = MockArt(top: Color(hex: 0x22C55E), bottom: Color(hex: 0x14532D), symbol: "leaf.fill")
    static let flowers = MockArt(top: Color(hex: 0xFB7185), bottom: Color(hex: 0x9F1239), symbol: "camera.macro")
    static let metal = MockArt(top: Color(hex: 0x9CA3AF), bottom: Color(hex: 0x374151), symbol: "drop.fill")
    static let laptop = MockArt(top: Color(hex: 0x38BDF8), bottom: Color(hex: 0x0C4A6E), symbol: "laptopcomputer")
    static let mountain = MockArt(top: Color(hex: 0x818CF8), bottom: Color(hex: 0x1E1B4B), symbol: "mountain.2.fill")
    static let soup = MockArt(top: Color(hex: 0x84CC16), bottom: Color(hex: 0x3F6212), symbol: "takeoutbag.and.cup.and.straw.fill")
    static let interview = MockArt(top: Color(hex: 0xE879F9), bottom: Color(hex: 0x701A75), symbol: "video.square.fill")

    // MARK: Home

    static let templateSections: [MockTemplateSection] = [
        MockTemplateSection(
            title: "Shorts templates",
            subtitle: "Templates crafted with exclusive music and effects.",
            templates: [
                MockTemplate(caption: "SLOW MORNING", art: beach),
                MockTemplate(caption: "This is your sign to take yourself on a date", art: food),
                MockTemplate(caption: "the things we didn't say", art: portrait),
                MockTemplate(caption: "GOLDEN HOUR", art: mountain),
            ]
        ),
        MockTemplateSection(
            title: "Lifestyle",
            subtitle: "Vlog your everyday lifestyle moments.",
            templates: [
                MockTemplate(caption: "Screentime has never been lower", art: laptop),
                MockTemplate(caption: "bloom where you're planted", art: flowers),
                MockTemplate(caption: "Outdoor Activity", art: forest),
                MockTemplate(caption: "morning reset", art: coffee),
            ]
        ),
    ]

    // MARK: Media picker library

    static let libraryItems: [MockMediaItem] = [
        MockMediaItem(kind: .photo, art: screen),
        MockMediaItem(kind: .video(duration: 9), art: portrait),
        MockMediaItem(kind: .photo, art: soup),
        MockMediaItem(kind: .video(duration: 42), art: interview),
        MockMediaItem(kind: .photo, art: laptop),
        MockMediaItem(kind: .video(duration: 4), art: metal),
        MockMediaItem(kind: .photo, art: coffee),
        MockMediaItem(kind: .video(duration: 12), art: beach),
        MockMediaItem(kind: .photo, art: city),
        MockMediaItem(kind: .photo, art: flowers),
        MockMediaItem(kind: .video(duration: 27), art: ocean),
        MockMediaItem(kind: .photo, art: night),
        MockMediaItem(kind: .video(duration: 63), art: podcast),
        MockMediaItem(kind: .photo, art: mountain),
        MockMediaItem(kind: .photo, art: food),
        MockMediaItem(kind: .video(duration: 8), art: forest),
        MockMediaItem(kind: .photo, art: metal),
        MockMediaItem(kind: .video(duration: 95), art: city),
        MockMediaItem(kind: .photo, art: beach),
        MockMediaItem(kind: .photo, art: podcast),
        MockMediaItem(kind: .video(duration: 15), art: screen),
    ]

    // MARK: Editor catalogs (fonts, presets, effects, songs, ...)

    static let fonts = [
        "Default", "Serif", "Rounded", "Mono", "Condensed",
        "Handwritten", "Poster", "Typewriter",
    ]

    static let textColors: [Color] = [
        .white, .black,
        Color(hex: 0xF43F5E), Color(hex: 0xFB923C), Color(hex: 0xFACC15),
        Color(hex: 0x4ADE80), Color(hex: 0x2DD4BF), Color(hex: 0x38BDF8),
        Color(hex: 0x818CF8), Color(hex: 0xE879F9),
    ]

    static let textEffects = [
        "None", "Neon", "Shadow", "Outline", "Glow", "Retro", "Gradient", "Chrome",
    ]

    static let textAnimations = [
        "None", "Typewriter", "Fade", "Bounce", "Slide", "Pop", "Wave",
    ]

    static let stickerCategories: [(name: String, symbols: [String])] = [
        ("Faces", ["face.smiling.inverse", "heart.fill", "star.fill", "flame.fill", "bolt.fill", "sparkles"]),
        ("Nature", ["leaf.fill", "sun.max.fill", "moon.stars.fill", "cloud.rain.fill", "snowflake", "tornado"]),
        ("Things", ["crown.fill", "gift.fill", "balloon.2.fill", "party.popper.fill", "camera.fill", "gamecontroller.fill"]),
        ("Arrows", ["arrow.right.circle.fill", "arrow.up.heart.fill", "arrowshape.right.fill", "hand.point.right.fill", "hand.thumbsup.fill", "hands.clap.fill"]),
    ]

    static let effectCategories: [(name: String, effects: [String])] = [
        ("Basic", ["Blur", "Zoom pulse", "Shake", "Flash", "Heartbeat", "Mirror"]),
        ("Retro", ["VHS", "Film grain", "Old TV", "8mm", "Sepia burn"]),
        ("Glitch", ["RGB split", "Distort", "Scanlines", "Pixelate", "Static"]),
        ("Party", ["Confetti", "Neon rings", "Strobe", "Disco", "Sparkle rain"]),
    ]

    static let filters = [
        "Vivid", "Warm", "Cool", "Mono", "Fade", "Chrome", "Noir", "Sunset", "Forest", "Berry",
    ]

    static let masks = ["None", "Linear", "Mirror", "Circle", "Rectangle", "Heart", "Star"]

    static let maskSymbols: [String: String] = [
        "None": "slash.circle", "Linear": "rectangle.split.1x2", "Mirror": "rectangle.split.3x1",
        "Circle": "circle", "Rectangle": "rectangle", "Heart": "heart", "Star": "star",
    ]

    static let stabilizeLevels = ["None", "Recommended", "Smooth", "Max smooth"]

    static let transitionStyles = [
        "None", "Fade", "Dissolve", "Slide", "Wipe", "Zoom", "Spin", "Blur",
    ]

    static let transitionSymbols: [String: String] = [
        "None": "slash.circle", "Fade": "circle.lefthalf.filled", "Dissolve": "circle.dotted",
        "Slide": "arrow.right.square", "Wipe": "rectangle.lefthalf.filled",
        "Zoom": "plus.magnifyingglass", "Spin": "arrow.trianglehead.2.clockwise.rotate.90", "Blur": "drop.halffull",
    ]

    static let speedCurves = [
        "Constant", "Montage", "Hero", "Bullet", "Jump cut", "Flash in", "Flash out",
    ]

    static let animationsIn = ["None", "Fade in", "Slide up", "Zoom in", "Spin in", "Bounce"]
    static let animationsOut = ["None", "Fade out", "Slide down", "Zoom out", "Spin out", "Drop"]
    static let animationsCombo = ["None", "Pulse", "Rock", "Swing", "Flicker", "Breathe"]

    static let songs: [MockSong] = [
        MockSong(title: "Slow Morning", artist: "Ambient Works", duration: 132),
        MockSong(title: "Golden Hour Drive", artist: "Neon Coast", duration: 187),
        MockSong(title: "Paper Planes", artist: "Field Day", duration: 154),
        MockSong(title: "Midnight Bloom", artist: "Violet Room", duration: 201),
        MockSong(title: "Static Summer", artist: "The Frequencies", duration: 176),
        MockSong(title: "Cloud Runner", artist: "Kite Theory", duration: 143),
    ]

    static let soundEffects: [MockSong] = [
        MockSong(title: "Whoosh", artist: "Transitions", duration: 1),
        MockSong(title: "Pop", artist: "UI", duration: 1),
        MockSong(title: "Riser", artist: "Build-ups", duration: 4),
        MockSong(title: "Camera shutter", artist: "Foley", duration: 1),
        MockSong(title: "Crowd cheer", artist: "Ambience", duration: 6),
        MockSong(title: "Vinyl stop", artist: "Transitions", duration: 2),
    ]

    static let captionLanguages = ["English", "Spanish", "French", "German", "Japanese", "Korean"]

    static let backgroundColors: [Color] = [
        .black, .white,
        Color(hex: 0x1E293B), Color(hex: 0x312E81), Color(hex: 0x4C1D95),
        Color(hex: 0x831843), Color(hex: 0x14532D), Color(hex: 0x7C2D12),
    ]

    /// Deterministic placeholder art for named tiles (effects, filters, ...).
    static func tileArt(for name: String) -> MockArt {
        let palette = [night, ocean, beach, portrait, food, podcast, city, coffee, forest, flowers, mountain, interview]
        var hash = 5381
        for byte in name.utf8 {
            hash = ((hash << 5) &+ hash) &+ Int(byte)
        }
        var art = palette[abs(hash) % palette.count]
        art.symbol = nil
        return art
    }
}
