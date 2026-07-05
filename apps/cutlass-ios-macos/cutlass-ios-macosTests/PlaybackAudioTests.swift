import Testing

@testable import cutlass_ios_macos

/// The SPSC ring between the audio feeder and the realtime render callback.
/// These run the exact write/consume paths the audio thread uses, minus the
/// thread: correctness of ordering, clamping, and wraparound is what matters.
struct AudioRingBufferTests {
    /// Interleaved frames written come out deinterleaved, in order.
    @Test func roundtripsInterleavedFramesInOrder() {
        let ring = AudioRingBuffer(capacityFrames: 8)
        var source: [Float] = [0.1, -0.1, 0.2, -0.2, 0.3, -0.3]
        let wrote = source.withUnsafeBufferPointer { ring.write(from: $0.baseAddress!, frames: 3) }
        #expect(wrote == 3)

        var left = [Float](repeating: 99, count: 4)
        var right = [Float](repeating: 99, count: 4)
        let got = left.withUnsafeMutableBufferPointer { leftBuffer in
            right.withUnsafeMutableBufferPointer { rightBuffer in
                ring.consume(
                    left: leftBuffer.baseAddress!, right: rightBuffer.baseAddress!, frames: 4)
            }
        }
        #expect(got == 3)
        #expect(left[0..<3] == [0.1, 0.2, 0.3])
        #expect(right[0..<3] == [-0.1, -0.2, -0.3])
    }

    /// An empty ring yields zero frames and a full ring rejects extra writes;
    /// nothing blocks in either direction.
    @Test func clampsAtEmptyAndFull() {
        let ring = AudioRingBuffer(capacityFrames: 4)
        var left = [Float](repeating: 0, count: 2)
        var right = [Float](repeating: 0, count: 2)
        let empty = left.withUnsafeMutableBufferPointer { leftBuffer in
            right.withUnsafeMutableBufferPointer { rightBuffer in
                ring.consume(
                    left: leftBuffer.baseAddress!, right: rightBuffer.baseAddress!, frames: 2)
            }
        }
        #expect(empty == 0)

        let six = [Float](repeating: 0.5, count: 12)
        let wrote = six.withUnsafeBufferPointer { ring.write(from: $0.baseAddress!, frames: 6) }
        #expect(wrote == 4)
        #expect(ring.freeFrames == 0)
    }

    /// Positions are monotonic counters, so the ring stays correct once the
    /// indices wrap past the capacity repeatedly.
    @Test func survivesWraparound() {
        let ring = AudioRingBuffer(capacityFrames: 3)
        var next: Float = 0
        var expected: Float = 0
        for _ in 0..<10 {
            var chunk: [Float] = []
            for _ in 0..<2 {
                chunk.append(next)
                chunk.append(-next)
                next += 1
            }
            let wrote = chunk.withUnsafeBufferPointer { ring.write(from: $0.baseAddress!, frames: 2) }
            #expect(wrote == 2)

            var left = [Float](repeating: 99, count: 2)
            var right = [Float](repeating: 99, count: 2)
            let got = left.withUnsafeMutableBufferPointer { leftBuffer in
                right.withUnsafeMutableBufferPointer { rightBuffer in
                    ring.consume(
                        left: leftBuffer.baseAddress!, right: rightBuffer.baseAddress!, frames: 2)
                }
            }
            #expect(got == 2)
            for frame in 0..<2 {
                #expect(left[frame] == expected)
                #expect(right[frame] == -expected)
                expected += 1
            }
        }
    }
}
