import SwiftUI

/// What a filmstrip renders from: the backing file plus the source-window
/// mapping (trim/speed/reverse), with mock art as the loading fallback.
nonisolated struct FilmstripSource: Hashable {
    var path: String?
    var trimStart: TimeInterval = 0
    var speed: Double = 1
    var reversed = false
    /// Stills repeat one frame, so every tile shares slot zero.
    var isStill = false
    var art: MockArt?
}

/// A strip of engine-rendered frame tiles along a clip: tile *i* shows the
/// source frame that plays at that point of the clip (trim, speed, and
/// reverse respected). Tiles draw synchronously from the shared cache and
/// fetch misses through the per-file thumbnailer, falling back to the mock
/// art gradient until pixels arrive.
struct FilmstripView: View {
    var source: FilmstripSource
    /// Clip length in timeline seconds (maps tiles to source times).
    var clipLength: TimeInterval
    var pointsPerSecond: Double
    /// Square tile side, normally the strip height.
    var tileSize: CGFloat
    /// Total strip width (time width, or the square in sort mode).
    var width: CGFloat
    /// Alternate-tile shading (fakes frame separation while tiles repeat).
    var shading = true
    var symbolSize: CGFloat = 15

    var body: some View {
        let tileCount = max(1, Int((width / tileSize).rounded(.up)))
        HStack(spacing: 0) {
            ForEach(0..<tileCount, id: \.self) { index in
                FilmstripTile(
                    path: source.path,
                    seconds: sourceSeconds(tile: index),
                    tileSize: tileSize,
                    art: source.art,
                    symbolSize: symbolSize
                )
                .overlay(
                    Color.black.opacity(shading && !index.isMultiple(of: 2) ? 0.10 : 0)
                )
            }
        }
        .frame(width: width, alignment: .leading)
        .clipped()
    }

    /// Source time (seconds into the file) for a tile's leading edge.
    private func sourceSeconds(tile index: Int) -> Double {
        guard !source.isStill else { return 0 }
        let local = Double(index) * Double(tileSize) / pointsPerSecond
        let clamped = min(local, max(0, clipLength - 0.001))
        let consumed = clipLength * source.speed
        let offset =
            source.reversed
            ? max(0, consumed - clamped * source.speed)
            : clamped * source.speed
        return source.trimStart + offset
    }
}

/// One square tile: cached image if available, else mock art while the
/// thumbnailer decodes.
private struct FilmstripTile: View {
    /// Thumbnails render at 2x the tallest strip so every row shares one
    /// cache slot per (file, source bucket).
    static let pixelHeight = 132

    var path: String?
    var seconds: Double
    var tileSize: CGFloat
    var art: MockArt?
    var symbolSize: CGFloat

    @State private var image: CGImage?

    var body: some View {
        ZStack {
            if let image = image ?? cached {
                Image(decorative: image, scale: 1)
                    .resizable()
                    .scaledToFill()
            } else if let art {
                MockArtView(art: art, symbolSize: symbolSize)
            } else {
                Rectangle().fill(Theme.surfaceElevated)
            }
        }
        .frame(width: tileSize, height: tileSize)
        .clipped()
        .task(id: taskKey) {
            guard let path, cached == nil else { return }
            image = await ThumbnailCache.shared.thumbnail(
                path: path, seconds: seconds, height: Self.pixelHeight)
        }
    }

    private var cached: CGImage? {
        guard let path else { return nil }
        return ThumbnailCache.shared.cached(
            path: path, seconds: seconds, height: Self.pixelHeight)
    }

    private var taskKey: String {
        "\(path ?? "")#\(Int((seconds * 2).rounded()))"
    }
}
