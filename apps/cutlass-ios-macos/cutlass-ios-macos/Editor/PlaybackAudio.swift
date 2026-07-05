import AVFoundation
import CutlassMobile
import Synchronization

/// Single-producer single-consumer ring of interleaved stereo samples between
/// the feeder task (writes decoded PCM) and the audio render callback (reads,
/// deinterleaving). Lock-free — the render thread must never wait: positions
/// are monotonically increasing sample-frame counters, so head/tail math
/// never wraps and each side only stores its own counter.
nonisolated final class AudioRingBuffer: @unchecked Sendable {
    private let capacity: Int
    private let storage: UnsafeMutablePointer<Float>
    private let written = Atomic<Int>(0)
    private let consumed = Atomic<Int>(0)

    /// `capacityFrames` of interleaved stereo (2 floats per frame).
    init(capacityFrames: Int) {
        capacity = capacityFrames
        storage = .allocate(capacity: capacityFrames * 2)
        storage.initialize(repeating: 0, count: capacityFrames * 2)
    }

    deinit {
        storage.deallocate()
    }

    var freeFrames: Int {
        capacity - (written.load(ordering: .acquiring) - consumed.load(ordering: .acquiring))
    }

    /// Copy up to `frames` interleaved frames in; returns how many fit.
    func write(from source: UnsafePointer<Float>, frames: Int) -> Int {
        let head = written.load(ordering: .relaxed)
        let count = min(frames, capacity - (head - consumed.load(ordering: .acquiring)))
        for frame in 0..<count {
            let slot = ((head + frame) % capacity) * 2
            storage[slot] = source[frame * 2]
            storage[slot + 1] = source[frame * 2 + 1]
        }
        written.store(head + count, ordering: .releasing)
        return count
    }

    /// Deinterleave up to `frames` frames into `left`/`right`; returns how
    /// many were available. Realtime-safe: no locks, no allocation.
    func consume(left: UnsafeMutablePointer<Float>, right: UnsafeMutablePointer<Float>, frames: Int)
        -> Int
    {
        let tail = consumed.load(ordering: .relaxed)
        let count = min(frames, written.load(ordering: .acquiring) - tail)
        for frame in 0..<count {
            let slot = ((tail + frame) % capacity) * 2
            left[frame] = storage[slot]
            right[frame] = storage[slot + 1]
        }
        consumed.store(tail + count, ordering: .releasing)
        return count
    }
}

/// Plays the timeline's mixed audio during preview playback.
///
/// One `AVAudioSourceNode` pulls from a ring buffer that a feeder task keeps
/// filled from the engine's `AudioReader` (plan Phase F: decode never runs on
/// the realtime thread; the render callback only copies). Underruns play
/// silence rather than glitching. Stop-and-reopen is the whole seek/edit
/// story: `EditorState` restarts playback audio whenever the playhead jumps
/// or the revision changes.
final class PlaybackAudio {
    private var engine: AVAudioEngine?
    private var feeder: Task<Void, Never>?

    /// ~1s of buffered audio; the feeder tops it up in 100 ms chunks.
    private nonisolated static let ringCapacityFrames = 48_000
    private nonisolated static let chunkFrames = 4_800

    /// Realtime render callback: copy from the ring, zero-fill underruns.
    /// Built nonisolated — the audio thread must never touch an actor.
    private nonisolated static func renderBlock(ring: AudioRingBuffer)
        -> AVAudioSourceNodeRenderBlock
    {
        { isSilence, _, frameCount, audioBufferList in
            let buffers = UnsafeMutableAudioBufferListPointer(audioBufferList)
            guard buffers.count >= 2,
                let left = buffers[0].mData?.assumingMemoryBound(to: Float.self),
                let right = buffers[1].mData?.assumingMemoryBound(to: Float.self)
            else { return noErr }
            let wanted = Int(frameCount)
            let got = ring.consume(left: left, right: right, frames: wanted)
            if got < wanted {
                left.advanced(by: got).update(repeating: 0, count: wanted - got)
                right.advanced(by: got).update(repeating: 0, count: wanted - got)
                if got == 0 {
                    isSilence.pointee = true
                }
            }
            return noErr
        }
    }

    /// Start playing `reader`'s stream from its opening position. Any
    /// previous stream stops first.
    func start(reader: AudioReader) {
        stop()

        let ring = AudioRingBuffer(capacityFrames: Self.ringCapacityFrames)
        let format = AVAudioFormat(
            standardFormatWithSampleRate: AudioReader.sampleRate,
            channels: AVAudioChannelCount(AudioReader.channelCount))
        guard let format else { return }

        let source = AVAudioSourceNode(format: format, renderBlock: Self.renderBlock(ring: ring))

        let engine = AVAudioEngine()
        engine.attach(source)
        engine.connect(source, to: engine.mainMixerNode, format: format)

        #if os(iOS)
        try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .moviePlayback)
        try? AVAudioSession.sharedInstance().setActive(true)
        #endif
        do {
            try engine.start()
        } catch {
            print("cutlass: audio engine start failed: \(error)")
            return
        }
        print("cutlass: playback audio running (\(Int(AudioReader.sampleRate)) Hz)")
        self.engine = engine

        // The feeder owns the reader: decode off the main actor, stop at end
        // of timeline or on a decode failure (video keeps playing silently).
        feeder = Task.detached(priority: .userInitiated) {
            let scratch = UnsafeMutablePointer<Float>.allocate(capacity: Self.chunkFrames * 2)
            defer { scratch.deallocate() }
            while !Task.isCancelled {
                if ring.freeFrames >= Self.chunkFrames {
                    guard let got = reader.read(into: scratch, maxFrames: Self.chunkFrames),
                        got > 0
                    else { break }
                    // Fits in full: space was checked and we're the only writer.
                    _ = ring.write(from: scratch, frames: got)
                } else {
                    try? await Task.sleep(for: .milliseconds(25))
                }
            }
        }
    }

    /// Stop feeding and tear the engine down. Idempotent.
    func stop() {
        feeder?.cancel()
        feeder = nil
        engine?.stop()
        engine = nil
    }

    deinit {
        feeder?.cancel()
        engine?.stop()
    }
}
