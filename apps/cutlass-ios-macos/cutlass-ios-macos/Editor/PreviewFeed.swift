import CoreGraphics
import OSLog
import SwiftUI

/// Pumps engine frames into the preview canvas.
///
/// Frame-drop policy (plan Phase F): at most one `render_fit` call is ever in
/// flight; while it runs, newer requests overwrite a single pending slot, so
/// intermediate scrub positions are dropped — never queued — and the preview
/// always converges on the latest playhead. When sustained render cost can't
/// hold scrub interactivity, a resolution ladder shrinks the requested size
/// (and grows it back once renders are fast again).
@MainActor
@Observable
final class PreviewFeed {
    /// One frame request. Pixel sizes are bucketed by the caller so layout
    /// jitter doesn't read as a new request.
    private struct Request: Equatable {
        var seconds: Double
        var revision: UInt64
        var maxWidth: Int
        var maxHeight: Int
    }

    /// The latest engine frame; nil until the first render lands (views show
    /// their loading placeholder meanwhile).
    private(set) var image: CGImage?

    /// Exponential moving average of recent render costs in milliseconds —
    /// drives the quality ladder and doubles as the perf checkpoint number.
    private(set) var averageRenderMillis: Double = 0

    /// Render callback: (seconds, maxWidth, maxHeight) -> frame.
    @ObservationIgnored private let render: @MainActor (Double, Int, Int) async -> CGImage?
    @ObservationIgnored private var pending: Request?
    @ObservationIgnored private var delivered: Request?
    @ObservationIgnored private var pump: Task<Void, Never>?

    /// Whether the transport is playing. Playback budgets a render per
    /// timeline frame (~33 ms at 30fps), so the quality ladder drops a tier
    /// on sustained overruns that scrubbing would tolerate — cadence beats
    /// sharpness while content is in motion.
    @ObservationIgnored var isPlaying = false

    /// Fraction of the view's pixel size each tier requests, sharpest first.
    private static let qualityLadder: [Double] = [1.0, 0.7, 0.5]
    /// Hard cap on the long side: preview readback is CPU-bound and anything
    /// past this is invisible at phone-screen preview sizes.
    private static let maxLongSide = 1440.0
    /// EMA above this drops a tier; below `raiseBelowMillis` climbs back.
    private static let dropAboveMillis = 45.0
    /// Tighter drop threshold while playing: just inside the 30fps budget.
    private static let playbackDropAboveMillis = 30.0
    private static let raiseBelowMillis = 18.0
    @ObservationIgnored private var qualityTier = 0
    @ObservationIgnored private var lastTierChange = ContinuousClock.now

    /// Instruments track for the device perf pass (`render_fit` intervals).
    private static let signposter = OSSignposter(
        subsystem: "com.scytheralpha.cutlass", category: "preview")

    init(render: @escaping @MainActor (Double, Int, Int) async -> CGImage?) {
        self.render = render
    }

    /// Snap `seconds` to the timeline frame grid. The engine rounds to the
    /// nearest frame anyway (`frame_tick`), so two requests inside the same
    /// frame are the same render — quantizing makes them compare equal and
    /// dedupe instead of re-rendering an identical frame every playhead tick.
    nonisolated static func quantize(seconds: Double, fps: Double) -> Double {
        guard fps > 0 else { return seconds }
        return (max(seconds, 0) * fps).rounded() / fps
    }

    /// Ask for the frame at `seconds` sized for `viewSize`. Cheap to call on
    /// every scrub tick; a request identical to the delivered frame is
    /// skipped, everything else coalesces into the single pending slot.
    func request(seconds: Double, revision: UInt64, viewSize: CGSize, displayScale: CGFloat) {
        guard viewSize.width > 1, viewSize.height > 1 else { return }

        let quality = Self.qualityLadder[qualityTier]
        var pixelWidth = viewSize.width * displayScale * quality
        var pixelHeight = viewSize.height * displayScale * quality
        let longSide = max(pixelWidth, pixelHeight)
        if longSide > Self.maxLongSide {
            pixelWidth *= Self.maxLongSide / longSide
            pixelHeight *= Self.maxLongSide / longSide
        }

        let request = Request(
            seconds: seconds,
            revision: revision,
            // Bucket to 16px so ±point layout jitter can't force re-renders.
            maxWidth: max(Int(pixelWidth / 16) * 16, 64),
            maxHeight: max(Int(pixelHeight / 16) * 16, 64)
        )
        if request == delivered, image != nil { return }
        pending = request
        startPumpIfIdle()
    }

    /// Forget everything (project reset): views fall back to the loading
    /// placeholder until the next request renders.
    func reset() {
        pending = nil
        delivered = nil
        image = nil
    }

    /// Suspends until the pump has drained every pending request (tests).
    func settle() async {
        while let task = pump {
            await task.value
        }
    }

    private func startPumpIfIdle() {
        guard pump == nil else { return }
        pump = Task { @MainActor [weak self] in
            while let request = self?.takePending() {
                guard let self else { return }
                let started = ContinuousClock.now
                let interval = Self.signposter.beginInterval("render_fit")
                let frame = await self.render(request.seconds, request.maxWidth, request.maxHeight)
                Self.signposter.endInterval("render_fit", interval)

                if let frame {
                    self.image = frame
                    self.delivered = request
                    self.noteRenderCost(started.duration(to: .now))
                } else {
                    // Session not up yet or a render failure: keep the last
                    // frame, forget the stamp so the next trigger retries.
                    self.delivered = nil
                }
            }
            self?.pump = nil
        }
    }

    private func takePending() -> Request? {
        defer { pending = nil }
        return pending
    }

    /// Fold one render's cost into the EMA and walk the quality ladder with
    /// hysteresis + cooldown so it can't flap.
    private func noteRenderCost(_ elapsed: Duration) {
        let millis =
            Double(elapsed.components.seconds) * 1000
            + Double(elapsed.components.attoseconds) / 1e15
        averageRenderMillis =
            averageRenderMillis == 0 ? millis : averageRenderMillis * 0.7 + millis * 0.3

        let now = ContinuousClock.now
        guard lastTierChange.duration(to: now) > .milliseconds(800) else { return }
        let dropAbove = isPlaying ? Self.playbackDropAboveMillis : Self.dropAboveMillis
        if averageRenderMillis > dropAbove, qualityTier < Self.qualityLadder.count - 1 {
            qualityTier += 1
            lastTierChange = now
        } else if averageRenderMillis < Self.raiseBelowMillis, qualityTier > 0 {
            qualityTier -= 1
            lastTierChange = now
        }
    }
}
