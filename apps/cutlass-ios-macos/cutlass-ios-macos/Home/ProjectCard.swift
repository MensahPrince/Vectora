import SwiftUI

/// A recent-project card in the horizontally scrolling Projects row: the
/// cached frame-0 thumbnail (written on auto-save), duration badge, name,
/// and last-modified label.
struct ProjectCard: View {
    var project: ProjectStore.Entry
    var onMenu: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            thumbnail
                .frame(width: 138, height: 92)
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                .overlay(alignment: .topLeading) {
                    DurationBadge(duration: project.durationSeconds)
                        .padding(6)
                }
                .overlay(alignment: .bottomTrailing) {
                    Button(action: onMenu) {
                        Image(systemName: "ellipsis")
                            .font(.system(size: 10, weight: .bold))
                            .foregroundStyle(.white)
                            .frame(width: 22, height: 22)
                            .background(.black.opacity(0.45), in: Circle())
                    }
                    .buttonStyle(.plain)
                    .padding(6)
                }

            VStack(alignment: .leading, spacing: 1) {
                Text(project.name)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.white)
                    .lineLimit(1)
                Text(project.dateLabel)
                    .font(.caption2)
                    .foregroundStyle(Theme.textSecondary)
            }
        }
        .frame(width: 138, alignment: .leading)
    }

    @ViewBuilder
    private var thumbnail: some View {
        // Loaded straight from disk (not SwiftUI's named-image cache) so a
        // re-listed row shows the freshest save.
        if let image = Self.loadImage(at: project.thumbnailFile) {
            Image(decorative: image, scale: 1)
                .resizable()
                .scaledToFill()
                .frame(width: 138, height: 92)
                .background(Theme.surfaceElevated)
        } else {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Theme.surfaceElevated)
                .overlay {
                    Image(systemName: "film")
                        .font(.system(size: 24))
                        .foregroundStyle(Theme.textTertiary)
                }
        }
    }

    private static func loadImage(at url: URL) -> CGImage? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }
}

#Preview {
    ProjectCard(
        project: ProjectStore.Entry(
            id: UUID(), name: "Night drive", durationSeconds: 94, modifiedAt: .now))
        .padding()
        .background(Theme.background)
}
