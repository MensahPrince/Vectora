import Foundation

/// On-disk home for one project's media: `Documents/Projects/<id>/media/`.
/// Picker copies land here, freeze-frame PNGs are written here by the
/// engine, and (come the persistence phase) `project.cutlass` will sit next
/// to it. Directories are created on first use.
nonisolated struct ProjectMediaStore {
    let projectID: UUID

    /// Staging area for picker copies made before/outside a project
    /// (`Documents/Incoming/`). `adopt(_:)` moves staged files in.
    static var incomingDirectory: URL {
        ensured(documents.appendingPathComponent("Incoming", isDirectory: true))
    }

    var mediaDirectory: URL {
        Self.ensured(
            Self.documents
                .appendingPathComponent("Projects", isDirectory: true)
                .appendingPathComponent(projectID.uuidString, isDirectory: true)
                .appendingPathComponent("media", isDirectory: true))
    }

    /// Where the next freeze-frame still should be written.
    func freezeFrameURL() -> URL {
        mediaDirectory.appendingPathComponent(
            "freeze-\(UUID().uuidString.prefix(8)).png")
    }

    /// Claim a file for this project so the project directory is
    /// self-contained (duplicates and reopens relink against `media/`):
    /// staged picks *move* in (same volume, a rename), bundled fixtures
    /// *copy* in (the bundle path changes across installs), and anything
    /// already inside the media directory imports where it lies.
    func adopt(_ url: URL) -> URL {
        let staged = url.path.hasPrefix(Self.incomingDirectory.path)
        let bundled = url.path.hasPrefix(Bundle.main.bundlePath)
        guard staged || bundled else { return url }

        do {
            if staged {
                // Staged names may collide across picks; keep both files.
                let destination = uniqueDestination(for: url.lastPathComponent)
                try FileManager.default.moveItem(at: url, to: destination)
                return destination
            }
            // Fixtures are immutable: the same name is the same bytes, so
            // repeated picks share one copy.
            let destination = mediaDirectory.appendingPathComponent(url.lastPathComponent)
            if !FileManager.default.fileExists(atPath: destination.path) {
                try FileManager.default.copyItem(at: url, to: destination)
            }
            return destination
        } catch {
            print("cutlass: media adopt failed, importing in place: \(error)")
            return url
        }
    }

    /// First free file name for `name` in the media directory (`a.mp4`,
    /// `a-2.mp4`, …): repeated picks of the same source stay distinct files.
    private func uniqueDestination(for name: String) -> URL {
        let base = (name as NSString).deletingPathExtension
        let ext = (name as NSString).pathExtension
        var candidate = mediaDirectory.appendingPathComponent(name)
        var counter = 2
        while FileManager.default.fileExists(atPath: candidate.path) {
            let numbered = ext.isEmpty ? "\(base)-\(counter)" : "\(base)-\(counter).\(ext)"
            candidate = mediaDirectory.appendingPathComponent(numbered)
            counter += 1
        }
        return candidate
    }

    private static var documents: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    private static func ensured(_ url: URL) -> URL {
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }
}
