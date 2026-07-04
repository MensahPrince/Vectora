import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// The on-disk project library: `Documents/Projects/<uuid>/` holds
/// `project.cutlass` (the engine save), `media/` (the clips' files, owned by
/// `ProjectMediaStore`), `meta.json` (name + duration for the Home cards),
/// and `thumb.png` (frame-0 render cached at save time).
///
/// The store is pure file management; the engine session reads and writes
/// `project.cutlass` itself through Save/Load commands.
nonisolated enum ProjectStore {
    struct Entry: Identifiable, Hashable {
        let id: UUID
        var name: String
        var durationSeconds: Double
        var modifiedAt: Date

        var directory: URL { ProjectStore.directory(for: id) }
        var projectFile: URL { ProjectStore.projectFile(for: id) }
        var thumbnailFile: URL { ProjectStore.thumbnailFile(for: id) }

        var dateLabel: String {
            modifiedAt.formatted(.relative(presentation: .named))
        }
    }

    /// Card metadata persisted next to the project file.
    private struct Meta: Codable {
        var name: String
        var durationSeconds: Double
    }

    static var root: URL {
        let url = FileManager.default
            .urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Projects", isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    static func directory(for id: UUID) -> URL {
        root.appendingPathComponent(id.uuidString, isDirectory: true)
    }

    static func projectFile(for id: UUID) -> URL {
        directory(for: id).appendingPathComponent("project.cutlass")
    }

    static func thumbnailFile(for id: UUID) -> URL {
        directory(for: id).appendingPathComponent("thumb.png")
    }

    private static func metaFile(for id: UUID) -> URL {
        directory(for: id).appendingPathComponent("meta.json")
    }

    /// Every saved project, newest first. Directories without a
    /// `project.cutlass` (e.g. media staged but never saved) are skipped.
    static func list() -> [Entry] {
        let contents = (try? FileManager.default.contentsOfDirectory(
            at: root, includingPropertiesForKeys: [.contentModificationDateKey])) ?? []
        return contents
            .compactMap { directory -> Entry? in
                guard let id = UUID(uuidString: directory.lastPathComponent) else { return nil }
                return entry(for: id)
            }
            .sorted { $0.modifiedAt > $1.modifiedAt }
    }

    /// The entry for one project id, nil when it was never saved.
    static func entry(for id: UUID) -> Entry? {
        let file = projectFile(for: id)
        guard FileManager.default.fileExists(atPath: file.path) else { return nil }
        let modified =
            (try? file.resourceValues(forKeys: [.contentModificationDateKey])
                .contentModificationDate) ?? .distantPast
        let meta = readMeta(for: id)
        return Entry(
            id: id,
            name: meta?.name ?? "Untitled",
            durationSeconds: meta?.durationSeconds ?? 0,
            modifiedAt: modified)
    }

    /// Default name for a new project ("Jul 4 project").
    static func defaultName(now: Date = .now) -> String {
        "\(now.formatted(.dateTime.month(.abbreviated).day())) project"
    }

    /// Persist the card metadata (called alongside every engine save).
    /// Creates the project directory if this is the first save.
    static func writeMeta(id: UUID, name: String, durationSeconds: Double) {
        try? FileManager.default.createDirectory(
            at: directory(for: id), withIntermediateDirectories: true)
        let meta = Meta(name: name, durationSeconds: durationSeconds)
        if let data = try? JSONEncoder().encode(meta) {
            try? data.write(to: metaFile(for: id), options: .atomic)
        }
    }

    private static func readMeta(for id: UUID) -> (name: String, durationSeconds: Double)? {
        guard let data = try? Data(contentsOf: metaFile(for: id)),
            let meta = try? JSONDecoder().decode(Meta.self, from: data)
        else { return nil }
        return (meta.name, meta.durationSeconds)
    }

    /// Write the Home-card thumbnail (frame-0 render, PNG).
    static func writeThumbnail(id: UUID, image: CGImage) {
        try? FileManager.default.createDirectory(
            at: directory(for: id), withIntermediateDirectories: true)
        let url = thumbnailFile(for: id)
        guard
            let destination = CGImageDestinationCreateWithURL(
                url as CFURL, UTType.png.identifier as CFString, 1, nil)
        else { return }
        CGImageDestinationAddImage(destination, image, nil)
        CGImageDestinationFinalize(destination)
    }

    static func rename(id: UUID, to name: String) {
        guard let current = entry(for: id) else { return }
        writeMeta(id: id, name: name, durationSeconds: current.durationSeconds)
    }

    static func delete(id: UUID) {
        try? FileManager.default.removeItem(at: directory(for: id))
    }

    /// Launch-time housekeeping: delete project directories that never
    /// reached a save (media copied in, then the project was abandoned
    /// before its first edit) and stale picker staging files. Only safe
    /// before any session is editing.
    static func purgeUnsaved() {
        let contents = (try? FileManager.default.contentsOfDirectory(
            at: root, includingPropertiesForKeys: nil)) ?? []
        for directory in contents {
            guard UUID(uuidString: directory.lastPathComponent) != nil,
                !FileManager.default.fileExists(
                    atPath: directory.appendingPathComponent("project.cutlass").path)
            else { continue }
            try? FileManager.default.removeItem(at: directory)
        }
        try? FileManager.default.removeItem(at: ProjectMediaStore.incomingDirectory)
    }

    /// Copy the whole project directory under a fresh id. The duplicate's
    /// `project.cutlass` still references the original's media paths; opening
    /// it relinks every clip to the copied `media/` directory (projects own
    /// their media).
    static func duplicate(id: UUID) -> Entry? {
        guard let source = entry(for: id) else { return nil }
        let copy = UUID()
        do {
            try FileManager.default.copyItem(
                at: directory(for: id), to: directory(for: copy))
        } catch {
            print("cutlass: project duplicate failed: \(error)")
            return nil
        }
        writeMeta(id: copy, name: source.name + " copy", durationSeconds: source.durationSeconds)
        return entry(for: copy)
    }
}
