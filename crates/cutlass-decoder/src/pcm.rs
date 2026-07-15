//! Bounded whole-stream PCM decoding for transcription and media analysis.

use std::{
    collections::TryReserveError,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
};

use cutlass_core::{AudioReader, DecodeError};
use thiserror::Error;

/// Number of mono sample frames requested from the backend per read.
const READ_FRAMES: usize = 32 * 1024;

/// Failure returned by [`read_mono_pcm_with_cancel`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReadMonoPcmError {
    /// The requested output sample rate was zero.
    #[error("PCM sample rate must be nonzero")]
    InvalidSampleRate,
    /// The caller supplied no room for output sample frames.
    #[error("PCM frame limit must be positive")]
    InvalidMaxFrames,
    /// The caller requested cancellation, or its cancellation callback panicked.
    #[error("PCM decode was cancelled")]
    Cancelled,
    /// The decoded stream contains more sample frames than the caller permits.
    #[error("PCM stream exceeds the limit of {max_frames} sample frames")]
    LimitExceeded {
        /// Maximum number of mono sample frames accepted by the call.
        max_frames: usize,
    },
    /// Reserving bounded storage for the output or read buffer failed.
    #[error("failed to allocate bounded PCM storage (limit: {max_frames} sample frames)")]
    AllocationFailed {
        /// Caller-provided output bound in mono sample frames.
        max_frames: usize,
        /// Allocation failure reported by the standard library.
        #[source]
        source: TryReserveError,
    },
    /// A backend reported writing more frames than its output buffer can hold.
    #[error(
        "audio reader reported {reported_frames} frames for a {requested_frames}-frame mono buffer"
    )]
    ReaderContractViolation {
        /// Number of sample frames requested from the backend.
        requested_frames: usize,
        /// Impossible number of sample frames reported by the backend.
        reported_frames: usize,
    },
    /// Opening or decoding the media stream failed.
    #[error("audio decode failed: {0}")]
    Decode(
        #[from]
        #[source]
        DecodeError,
    ),
}

/// Decode an entire media audio stream as mono `f32` PCM.
///
/// `sample_rate` selects the output rate; the platform audio backend performs
/// resampling and downmixing to one channel. `max_frames` is a hard output
/// bound: the function never appends more PCM values than that limit and uses
/// only a fixed 32K-frame read buffer. Its time complexity is `O(n)` for `n`
/// decoded output frames, and its memory use is `O(min(n, max_frames) + 32K)`
/// samples (at most `max_frames` requested output slots plus the fixed buffer).
///
/// Cancellation is cooperative around backend calls: `cancelled` is checked
/// before and after opening, before and after every blocking read, and before
/// success. A panic from the callback is treated as cancellation. A read that
/// is already in progress cannot be interrupted until the platform backend
/// returns.
///
/// Once the output reaches `max_frames`, one final one-frame read distinguishes
/// exact-length end-of-stream from over-limit input.
pub fn read_mono_pcm_with_cancel(
    path: &Path,
    sample_rate: u32,
    max_frames: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<f32>, ReadMonoPcmError> {
    if sample_rate == 0 {
        return Err(ReadMonoPcmError::InvalidSampleRate);
    }
    if max_frames == 0 {
        return Err(ReadMonoPcmError::InvalidMaxFrames);
    }
    if cancellation_requested(&cancelled) {
        return Err(ReadMonoPcmError::Cancelled);
    }

    let mut reader = crate::open_audio_reader(path, sample_rate, 1)?;
    read_mono_pcm_from_reader(reader.as_mut(), max_frames, &cancelled)
}

fn read_mono_pcm_from_reader<C>(
    reader: &mut dyn AudioReader,
    max_frames: usize,
    cancelled: &C,
) -> Result<Vec<f32>, ReadMonoPcmError>
where
    C: Fn() -> bool + ?Sized,
{
    // For the public entry point this is the check immediately after opening.
    if cancellation_requested(cancelled) {
        return Err(ReadMonoPcmError::Cancelled);
    }

    let buffer_frames = READ_FRAMES.min(max_frames);
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(buffer_frames)
        .map_err(|source| ReadMonoPcmError::AllocationFailed { max_frames, source })?;
    buffer.resize(buffer_frames, 0.0);

    let mut pcm = Vec::new();
    loop {
        if cancellation_requested(cancelled) {
            return Err(ReadMonoPcmError::Cancelled);
        }

        let remaining = max_frames - pcm.len();
        let requested_frames = if remaining == 0 {
            1
        } else {
            READ_FRAMES.min(remaining)
        };
        let read_result = reader.read(&mut buffer[..requested_frames]);

        if cancellation_requested(cancelled) {
            return Err(ReadMonoPcmError::Cancelled);
        }

        let reported_frames = read_result?;
        if reported_frames > requested_frames {
            return Err(ReadMonoPcmError::ReaderContractViolation {
                requested_frames,
                reported_frames,
            });
        }

        if remaining == 0 {
            if reported_frames != 0 {
                return Err(ReadMonoPcmError::LimitExceeded { max_frames });
            }
            if cancellation_requested(cancelled) {
                return Err(ReadMonoPcmError::Cancelled);
            }
            return Ok(pcm);
        }

        if reported_frames == 0 {
            if cancellation_requested(cancelled) {
                return Err(ReadMonoPcmError::Cancelled);
            }
            return Ok(pcm);
        }

        reserve_for_append(&mut pcm, reported_frames, max_frames)?;
        pcm.extend_from_slice(&buffer[..reported_frames]);
    }
}

fn reserve_for_append(
    pcm: &mut Vec<f32>,
    additional: usize,
    max_frames: usize,
) -> Result<(), ReadMonoPcmError> {
    let required = pcm.len() + additional;
    debug_assert!(required <= max_frames);
    if required <= pcm.capacity() {
        return Ok(());
    }

    // Grow geometrically to avoid quadratic copying, but never request output
    // capacity beyond the caller's hard frame bound.
    let target_capacity = required
        .max(pcm.capacity().saturating_mul(2))
        .min(max_frames);
    pcm.try_reserve_exact(target_capacity - pcm.len())
        .map_err(|source| ReadMonoPcmError::AllocationFailed { max_frames, source })
}

fn cancellation_requested<C>(cancelled: &C) -> bool
where
    C: Fn() -> bool + ?Sized,
{
    catch_unwind(AssertUnwindSafe(cancelled)).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;

    enum ReadAction {
        Samples(Vec<f32>),
        Error(DecodeError),
        OverReport(usize),
    }

    struct FakeAudioReader {
        actions: VecDeque<ReadAction>,
        read_calls: Arc<AtomicUsize>,
        requested_frames: Vec<usize>,
    }

    impl FakeAudioReader {
        fn new(actions: impl IntoIterator<Item = ReadAction>) -> Self {
            Self {
                actions: actions.into_iter().collect(),
                read_calls: Arc::new(AtomicUsize::new(0)),
                requested_frames: Vec::new(),
            }
        }
    }

    impl AudioReader for FakeAudioReader {
        fn read(&mut self, out: &mut [f32]) -> Result<usize, DecodeError> {
            self.read_calls.fetch_add(1, Ordering::SeqCst);
            self.requested_frames.push(out.len());
            match self
                .actions
                .pop_front()
                .unwrap_or_else(|| ReadAction::Samples(Vec::new()))
            {
                ReadAction::Samples(samples) => {
                    assert!(
                        samples.len() <= out.len(),
                        "fake sample action exceeds requested buffer"
                    );
                    out[..samples.len()].copy_from_slice(&samples);
                    Ok(samples.len())
                }
                ReadAction::Error(error) => Err(error),
                ReadAction::OverReport(frames) => Ok(frames),
            }
        }

        fn seek_to_frame(&mut self, _frame: i64) -> Result<(), DecodeError> {
            Ok(())
        }

        fn position(&self) -> Option<i64> {
            None
        }
    }

    #[test]
    fn accumulates_multiple_chunks_in_order() {
        let mut reader = FakeAudioReader::new([
            ReadAction::Samples(vec![0.25; READ_FRAMES]),
            ReadAction::Samples(vec![-0.5, 0.75, 1.0]),
            ReadAction::Samples(Vec::new()),
        ]);

        let pcm = read_mono_pcm_from_reader(&mut reader, READ_FRAMES * 3, &|| false).expect("PCM");

        assert_eq!(pcm.len(), READ_FRAMES + 3);
        assert!(pcm[..READ_FRAMES].iter().all(|sample| *sample == 0.25));
        assert_eq!(&pcm[READ_FRAMES..], &[-0.5, 0.75, 1.0]);
        assert_eq!(
            reader.requested_frames,
            vec![READ_FRAMES, READ_FRAMES, READ_FRAMES]
        );
    }

    #[test]
    fn exact_limit_uses_one_frame_eof_probe() {
        let expected = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let mut reader = FakeAudioReader::new([
            ReadAction::Samples(expected.clone()),
            ReadAction::Samples(Vec::new()),
        ]);

        let pcm = read_mono_pcm_from_reader(&mut reader, expected.len(), &|| false).expect("PCM");

        assert_eq!(pcm, expected);
        assert_eq!(reader.requested_frames, vec![5, 1]);
    }

    #[test]
    fn one_frame_over_limit_discards_pcm() {
        let mut reader = FakeAudioReader::new([
            ReadAction::Samples(vec![0.0; 5]),
            ReadAction::Samples(vec![1.0]),
        ]);

        let error =
            read_mono_pcm_from_reader(&mut reader, 5, &|| false).expect_err("must exceed limit");

        assert!(matches!(
            error,
            ReadMonoPcmError::LimitExceeded { max_frames: 5 }
        ));
        assert_eq!(reader.requested_frames, vec![5, 1]);
    }

    #[test]
    fn pre_read_cancellation_does_not_touch_reader() {
        let mut reader = FakeAudioReader::new([ReadAction::Samples(vec![1.0])]);

        let error = read_mono_pcm_from_reader(&mut reader, 8, &|| true).expect_err("must cancel");

        assert!(matches!(error, ReadMonoPcmError::Cancelled));
        assert_eq!(reader.read_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancellation_during_read_is_observed_after_read() {
        let mut reader = FakeAudioReader::new([ReadAction::Samples(vec![1.0])]);
        let read_calls = Arc::clone(&reader.read_calls);

        let error =
            read_mono_pcm_from_reader(&mut reader, 8, &|| read_calls.load(Ordering::SeqCst) != 0)
                .expect_err("must cancel");

        assert!(matches!(error, ReadMonoPcmError::Cancelled));
        assert_eq!(reader.read_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancellation_between_reads_discards_accumulated_pcm() {
        let mut reader = FakeAudioReader::new([
            ReadAction::Samples(vec![0.5; READ_FRAMES]),
            ReadAction::Samples(vec![1.0]),
        ]);
        let checks = AtomicUsize::new(0);

        let error = read_mono_pcm_from_reader(&mut reader, READ_FRAMES * 2, &|| {
            checks.fetch_add(1, Ordering::SeqCst) + 1 == 4
        })
        .expect_err("must cancel before the second read");

        assert!(matches!(error, ReadMonoPcmError::Cancelled));
        assert_eq!(reader.read_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancellation_before_success_is_observed_after_eof() {
        let mut reader = FakeAudioReader::new([ReadAction::Samples(Vec::new())]);
        let checks = AtomicUsize::new(0);

        let error = read_mono_pcm_from_reader(&mut reader, 8, &|| {
            checks.fetch_add(1, Ordering::SeqCst) + 1 == 4
        })
        .expect_err("must cancel before success");

        assert!(matches!(error, ReadMonoPcmError::Cancelled));
        assert_eq!(reader.read_calls.load(Ordering::SeqCst), 1);
        assert_eq!(checks.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn callback_panic_fails_closed_as_cancellation() {
        let mut reader = FakeAudioReader::new([ReadAction::Samples(vec![1.0])]);

        let error =
            read_mono_pcm_from_reader(&mut reader, 8, &|| panic!("cancellation callback panic"))
                .expect_err("panic must cancel");

        assert!(matches!(error, ReadMonoPcmError::Cancelled));
        assert_eq!(reader.read_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn decode_error_is_preserved_as_source_variant() {
        let mut reader =
            FakeAudioReader::new([ReadAction::Error(DecodeError::Decode("bad packet".into()))]);

        let error =
            read_mono_pcm_from_reader(&mut reader, 8, &|| false).expect_err("must preserve error");

        match error {
            ReadMonoPcmError::Decode(DecodeError::Decode(message)) => {
                assert_eq!(message, "bad packet");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn impossible_over_report_is_rejected() {
        let mut reader = FakeAudioReader::new([ReadAction::OverReport(5)]);

        let error =
            read_mono_pcm_from_reader(&mut reader, 4, &|| false).expect_err("must reject backend");

        assert!(matches!(
            error,
            ReadMonoPcmError::ReaderContractViolation {
                requested_frames: 4,
                reported_frames: 5,
            }
        ));
    }

    #[test]
    fn public_api_validates_arguments_before_opening() {
        let path = Path::new("/definitely/not/a/media/file");
        assert!(matches!(
            read_mono_pcm_with_cancel(path, 0, 1, || false),
            Err(ReadMonoPcmError::InvalidSampleRate)
        ));
        assert!(matches!(
            read_mono_pcm_with_cancel(path, 16_000, 0, || false),
            Err(ReadMonoPcmError::InvalidMaxFrames)
        ));
    }

    #[test]
    fn public_api_checks_cancellation_before_opening() {
        let error =
            read_mono_pcm_with_cancel(Path::new("/definitely/not/a/media/file"), 16_000, 1, || {
                true
            })
            .expect_err("must cancel before open");

        assert!(matches!(error, ReadMonoPcmError::Cancelled));
    }
}
