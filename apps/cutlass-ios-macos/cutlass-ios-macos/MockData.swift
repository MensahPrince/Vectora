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
    static let screen = MockArt(top: Color(hex: 0x475569), bottom: Color(hex: 0x1E293B), symbol: "qrcode")
    static let coffee = MockArt(top: Color(hex: 0xA16207), bottom: Color(hex: 0x451A03), symbol: "cup.and.saucer.fill")
    static let forest = MockArt(top: Color(hex: 0x22C55E), bottom: Color(hex: 0x14532D), symbol: "leaf.fill")
    static let flowers = MockArt(top: Color(hex: 0xFB7185), bottom: Color(hex: 0x9F1239), symbol: "camera.macro")
    static let metal = MockArt(top: Color(hex: 0x9CA3AF), bottom: Color(hex: 0x374151), symbol: "drop.fill")
    static let laptop = MockArt(top: Color(hex: 0x38BDF8), bottom: Color(hex: 0x0C4A6E), symbol: "laptopcomputer")
    static let mountain = MockArt(top: Color(hex: 0x818CF8), bottom: Color(hex: 0x1E1B4B), symbol: "mountain.2.fill")
    static let soup = MockArt(top: Color(hex: 0x84CC16), bottom: Color(hex: 0x3F6212), symbol: "takeoutbag.and.cup.and.straw.fill")
    static let interview = MockArt(top: Color(hex: 0xE879F9), bottom: Color(hex: 0x701A75), symbol: "video.square.fill")

    // MARK: Home

    static let projects: [MockProject] = [
        MockProject(dateLabel: "Jun 25, 2026 (2)", duration: 94, art: night),
        MockProject(dateLabel: "Jun 25, 2026 (1)", duration: 4, art: forest),
        MockProject(dateLabel: "Jun 24, 2026 (1)", duration: 4, art: interview),
        MockProject(dateLabel: "Jun 24, 2026", duration: 42, art: city),
        MockProject(dateLabel: "Jun 22, 2026", duration: 128, art: ocean),
    ]

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
}
