//! Clocked, seekable audio decode for playback — the seam reserved by the
//! module docs. Streams interleaved stereo f32 at a caller-chosen output
//! rate (the audio device rate) from an arbitrary source position.
//!
//! Position is expressed in *output sample frames* since the start of the
//! source (`frame / out_rate` seconds), because that is the unit the mixer
//! does all its span math in. Conversions from stream PTS run in exact i128.
//!
//! Sequential reads roll forward; [`AudioReader::seek_to_frame`] no-ops when
//! the target is already the current position and decodes-and-discards
//! through small forward gaps instead of paying a container seek + decoder
//! flush — the same philosophy as the video path's `frame_at`.

use std::path::Path;

use ffmpeg_next::error::EAGAIN;
use ffmpeg_next::format::{self, context::Input};
use ffmpeg_next::media::Type;
use ffmpeg_next::software::resampling;
use ffmpeg_next::util::channel_layout::ChannelLayout;
use ffmpeg_next::util::format::sample::{Sample, Type as SampleType};
use ffmpeg_next::util::frame::audio::Audio;
use ffmpeg_next::{Error as FfmpegError, Rational, codec, packet::Packet};
use tracing::debug;

use crate::error::DecodeError;
use crate::video::ensure_ffmpeg_init;

/// Output channel count: interleaved stereo (mono upmixes, surround
/// downmixes through swresample's default matrix).
pub const CHANNELS: usize = 2;

/// Forward seek gaps up to this many output frames decode-and-discard
/// instead of container-seeking (≈ 1s: cheaper than seek + flush + decoder
/// re-prime for the gap sizes scrubbing and block mixing produce).
const ROLL_FORWARD_LIMIT: i64 = 48_000;

pub struct AudioReader {
    input: Input,
    decoder: codec::decoder::Audio,
    resampler: resampling::Context,
    stream_index: usize,
    time_base: Rational,
    in_layout: ChannelLayout,
    out_rate: u32,
    demuxer_done: bool,
    /// Interleaved stereo samples resampled but not yet handed out.
    pending: Vec<f32>,
    pending_cursor: usize,
    /// Output-frame position of the next sample [`read`](Self::read) emits,
    /// or `None` before the first decode establishes the PTS anchor.
    position: Option<i64>,
    resampler_flushed: bool,
}

impl AudioReader {
    /// Open the best audio stream of `path`, decoding to interleaved stereo
    /// f32 at `out_rate` Hz.
    pub fn open(path: &Path, out_rate: u32) -> Result<Self, DecodeError> {
        ensure_ffmpeg_init()?;
        if out_rate == 0 {
            return Err(DecodeError::unsupported("zero output sample rate"));
        }

        let path_str = path
            .to_str()
            .ok_or_else(|| DecodeError::unsupported("path is not valid UTF-8"))?;
        let input = format::input(path_str).map_err(DecodeError::Open)?;

        let stream = input
            .streams()
            .best(Type::Audio)
            .ok_or_else(|| DecodeError::unsupported("no audio stream found"))?;
        let stream_index = stream.index();
        let time_base = stream.time_base();

        let mut decoder = codec::Context::from_parameters(stream.parameters())
            .map_err(DecodeError::Open)?
            .decoder()
            .audio()
            .map_err(DecodeError::Open)?;

        let rate = decoder.rate();
        if rate == 0 {
            return Err(DecodeError::unsupported(
                "audio stream reports zero sample rate",
            ));
        }
        let in_layout = if decoder.channel_layout().channels() == 0 {
            ChannelLayout::default(i32::from(decoder.channels()))
        } else {
            decoder.channel_layout()
        };
        decoder.set_channel_layout(in_layout);

        let resampler = resampling::Context::get(
            decoder.format(),
            in_layout,
            rate,
            Sample::F32(SampleType::Packed),
            ChannelLayout::STEREO,
            out_rate,
        )
        .map_err(DecodeError::Decode)?;

        Ok(Self {
            input,
            decoder,
            resampler,
            stream_index,
            time_base,
            in_layout,
            out_rate,
            demuxer_done: false,
            pending: Vec::new(),
            pending_cursor: 0,
            position: None,
            resampler_flushed: false,
        })
    }

    /// Output sample rate this reader resamples to.
    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    /// Output-frame position of the next sample `read` will emit, when known.
    pub fn position(&self) -> Option<i64> {
        self.position
    }

    /// Position the reader so the next [`read`](Self::read) emits the sample
    /// at output frame `target` (clamped to 0).
    ///
    /// No-op when already there; small forward gaps decode-and-discard;
    /// everything else container-seeks to the nearest preceding point and
    /// discards up to the target. The reader can land *past* a target that
    /// sits before the stream's first sample — callers pad the gap.
    pub fn seek_to_frame(&mut self, target: i64) -> Result<(), DecodeError> {
        let target = target.max(0);
        match self.position {
            Some(pos) if pos == target => return Ok(()),
            Some(pos) if target > pos && target - pos <= ROLL_FORWARD_LIMIT => {
                return self.discard_until(target);
            }
            _ => {}
        }

        // µs = frames / out_rate, floored — the container snaps backward.
        // Aim a pre-roll *before* the target: predictive codecs (MP3's bit
        // reservoir, AAC inter-frame state) decode garbage for the first few
        // frames after a flush, and the discard walk below eats exactly that
        // stretch before the target is reached.
        const PREROLL_US: i64 = 200_000;
        let us = (i128::from(target) * 1_000_000 / i128::from(self.out_rate)) as i64;
        let us = (us - PREROLL_US).max(0);
        if self.input.seek(us, ..us).is_err() {
            debug!(
                target,
                us, "audio seek failed; decoding from current position"
            );
        }
        self.decoder.flush();
        // swresample keeps cross-call state; recreate instead of flushing so
        // stale tail samples never bleed into the post-seek stream.
        self.resampler = resampling::Context::get(
            self.decoder.format(),
            self.in_layout,
            self.decoder.rate(),
            Sample::F32(SampleType::Packed),
            ChannelLayout::STEREO,
            self.out_rate,
        )
        .map_err(DecodeError::Decode)?;
        self.demuxer_done = false;
        self.resampler_flushed = false;
        self.pending.clear();
        self.pending_cursor = 0;
        self.position = None;

        self.discard_until(target)
    }

    /// Fill `out` (interleaved stereo, `out.len() / 2` frames) from the
    /// current position. Returns frames written — short only at end of
    /// stream; `0` means the stream is exhausted.
    pub fn read(&mut self, out: &mut [f32]) -> Result<usize, DecodeError> {
        let want_frames = out.len() / CHANNELS;
        let mut filled = 0usize;

        while filled < want_frames {
            let available = (self.pending.len() - self.pending_cursor) / CHANNELS;
            if available > 0 {
                let take = available.min(want_frames - filled);
                let from = self.pending_cursor;
                let to = from + take * CHANNELS;
                out[filled * CHANNELS..(filled + take) * CHANNELS]
                    .copy_from_slice(&self.pending[from..to]);
                self.pending_cursor = to;
                filled += take;
                if let Some(pos) = self.position.as_mut() {
                    *pos += take as i64;
                }
                continue;
            }
            if !self.refill_pending()? {
                break; // stream exhausted
            }
        }
        Ok(filled)
    }

    /// Decode and drop output frames until `position == target` (or the
    /// stream ends, or the stream starts past the target).
    fn discard_until(&mut self, target: i64) -> Result<(), DecodeError> {
        loop {
            match self.position {
                Some(pos) if pos >= target => return Ok(()),
                Some(pos) => {
                    let gap = (target - pos) as usize;
                    let available = (self.pending.len() - self.pending_cursor) / CHANNELS;
                    let drop = gap.min(available);
                    self.pending_cursor += drop * CHANNELS;
                    self.position = Some(pos + drop as i64);
                    if drop == gap {
                        return Ok(());
                    }
                }
                None => {}
            }
            if !self.refill_pending()? {
                return Ok(()); // stream ended before the target
            }
        }
    }

    /// Decode + resample one more audio frame into `pending`. `Ok(false)`
    /// when the stream (and the resampler tail) is exhausted.
    fn refill_pending(&mut self) -> Result<bool, DecodeError> {
        if self.pending_cursor >= self.pending.len() {
            self.pending.clear();
            self.pending_cursor = 0;
        }

        let mut frame = Audio::empty();
        loop {
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    if frame.channel_layout().channels() == 0 {
                        frame.set_channel_layout(self.in_layout);
                    }
                    // First frame after open/seek anchors the position from
                    // its PTS: frames = pts · tb · out_rate, exact in i128.
                    if self.position.is_none() {
                        let pts = frame.timestamp().or_else(|| frame.pts()).unwrap_or(0);
                        let frames = i128::from(pts)
                            * i128::from(self.time_base.numerator())
                            * i128::from(self.out_rate)
                            / i128::from(self.time_base.denominator().max(1));
                        self.position = Some(frames.clamp(0, i128::from(i64::MAX)) as i64);
                    }
                    let mut converted = Audio::empty();
                    self.resampler
                        .run(&frame, &mut converted)
                        .map_err(DecodeError::Decode)?;
                    self.push_converted(&converted);
                    if self.pending_cursor < self.pending.len() {
                        return Ok(true);
                    }
                    // Rate conversion can buffer a whole input frame; pull
                    // more input until something comes out.
                    continue;
                }
                Err(FfmpegError::Eof) => return self.flush_resampler_tail(),
                Err(FfmpegError::Other { errno }) if errno == EAGAIN => {
                    if self.demuxer_done {
                        return self.flush_resampler_tail();
                    }
                    self.read_packet()?;
                }
                Err(e) => return Err(DecodeError::Decode(e)),
            }
        }
    }

    fn flush_resampler_tail(&mut self) -> Result<bool, DecodeError> {
        if self.resampler_flushed {
            return Ok(false);
        }
        self.resampler_flushed = true;
        let mut tail = Audio::new(Sample::F32(SampleType::Packed), 4096, ChannelLayout::STEREO);
        if self.resampler.flush(&mut tail).is_ok() && tail.samples() > 0 {
            self.push_converted(&tail);
            return Ok(self.pending_cursor < self.pending.len());
        }
        Ok(false)
    }

    fn push_converted(&mut self, converted: &Audio) {
        if converted.planes() == 0 || converted.samples() == 0 {
            return;
        }
        // Packed stereo is one plane of (L, R) pairs, `samples()` frames
        // long. (`plane::<f32>` would slice `samples()` *floats* — half the
        // interleaved data — and play the content double-speed, chopped.)
        for &(l, r) in converted.plane::<(f32, f32)>(0) {
            self.pending.push(l);
            self.pending.push(r);
        }
    }

    fn read_packet(&mut self) -> Result<(), DecodeError> {
        let mut packet = Packet::empty();
        loop {
            match packet.read(&mut self.input) {
                Ok(()) => {
                    if packet.stream() == self.stream_index {
                        return self
                            .decoder
                            .send_packet(&packet)
                            .map_err(DecodeError::Decode);
                    }
                }
                Err(FfmpegError::Eof) => {
                    self.demuxer_done = true;
                    return self.decoder.send_eof().map_err(DecodeError::Decode);
                }
                Err(e) => return Err(DecodeError::Io(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const RATE: u32 = 48_000;

    fn audio_asset() -> Option<PathBuf> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "mp3"))
    }

    fn video_with_audio_asset() -> Option<PathBuf> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "mp4"))
            .find(|p| AudioReader::open(p, RATE).is_ok())
    }

    #[test]
    fn reads_interleaved_stereo_from_start() {
        let Some(path) = audio_asset() else {
            return;
        };
        let mut reader = AudioReader::open(&path, RATE).expect("open");
        let mut buf = vec![0f32; 4096 * CHANNELS];
        let frames = reader.read(&mut buf).expect("read");
        assert_eq!(frames, 4096, "full read away from EOF");
        assert!(buf.iter().any(|&s| s != 0.0), "real audio is not silence");
        assert_eq!(reader.position(), Some(frames as i64));
    }

    #[test]
    fn seek_positions_subsequent_reads() {
        let Some(path) = audio_asset() else {
            return;
        };
        let mut reader = AudioReader::open(&path, RATE).expect("open");
        let target = i64::from(RATE); // 1s in
        reader.seek_to_frame(target).expect("seek");
        let pos = reader.position().expect("anchored");
        assert_eq!(pos, target, "seek lands exactly when the stream covers it");

        let mut buf = vec![0f32; 1024 * CHANNELS];
        let frames = reader.read(&mut buf).expect("read");
        assert_eq!(frames, 1024);
        assert_eq!(reader.position(), Some(target + 1024));
    }

    #[test]
    fn seek_matches_sequential_read_content() {
        // MP4/AAC: the sample table makes mid-stream PTS exact. (MP3 seeks
        // by bitrate estimate and can land tens of ms off — see module
        // docs / roadmap known gaps.)
        let Some(path) = video_with_audio_asset() else {
            return;
        };
        // Read 1s sequentially, remember the tail block...
        let mut seq = AudioReader::open(&path, RATE).expect("open");
        let total = RATE as usize;
        let mut all = vec![0f32; total * CHANNELS];
        let mut filled = 0;
        while filled < total {
            let n = seq.read(&mut all[filled * CHANNELS..]).expect("read");
            if n == 0 {
                return; // asset shorter than 1s: nothing to compare
            }
            filled += n;
        }
        let tail_at = (total - 256) as i64;

        // ...then seek straight to it with a fresh reader.
        let mut seeked = AudioReader::open(&path, RATE).expect("open");
        seeked.seek_to_frame(tail_at).expect("seek");
        let mut block = vec![0f32; 256 * CHANNELS];
        let n = seeked.read(&mut block).expect("read");
        assert_eq!(n, 256);

        let expected = &all[(tail_at as usize) * CHANNELS..];
        // Decoder warm-up after a mid-stream seek is not bit-exact for lossy
        // codecs; demand close, not identical.
        let max_err = block
            .iter()
            .zip(expected.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(max_err < 0.05, "seeked audio diverged: max err {max_err}");
    }

    #[test]
    fn forward_roll_does_not_container_seek() {
        let Some(path) = audio_asset() else {
            return;
        };
        let mut reader = AudioReader::open(&path, RATE).expect("open");
        let mut buf = vec![0f32; 512 * CHANNELS];
        reader.read(&mut buf).expect("read");
        let pos = reader.position().expect("pos");
        reader.seek_to_frame(pos + 1000).expect("roll");
        assert_eq!(reader.position(), Some(pos + 1000));
    }

    #[test]
    fn emitted_duration_matches_the_container() {
        // Regression: packed-stereo planes are `samples()` *frame pairs*;
        // slicing them as `samples()` floats dropped half of every frame
        // and played content at double speed. Decoding the whole stream
        // must emit out_rate × duration frames, not half of it.
        let Some(path) = audio_asset() else {
            return;
        };
        let duration_s = {
            let input = ffmpeg_next::format::input(path.to_str().unwrap()).expect("open");
            input.duration() as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE)
        };
        if duration_s <= 1.0 {
            return;
        }

        let mut reader = AudioReader::open(&path, RATE).expect("open");
        let mut buf = vec![0f32; 4096 * CHANNELS];
        let mut frames = 0u64;
        loop {
            let n = reader.read(&mut buf).expect("read");
            if n == 0 {
                break;
            }
            frames += n as u64;
        }
        let emitted_s = frames as f64 / f64::from(RATE);
        let ratio = emitted_s / duration_s;
        assert!(
            (0.95..=1.05).contains(&ratio),
            "emitted {emitted_s:.2}s of audio for a {duration_s:.2}s stream (ratio {ratio:.3})"
        );
    }

    #[test]
    fn eof_returns_short_then_zero() {
        let Some(path) = audio_asset() else {
            return;
        };
        let mut reader = AudioReader::open(&path, RATE).expect("open");
        // Jump far past any sane asset length.
        reader
            .seek_to_frame(i64::from(RATE) * 36_000)
            .expect("seek");
        let mut buf = vec![0f32; 256 * CHANNELS];
        let n = reader.read(&mut buf).expect("read");
        assert_eq!(n, 0, "stream exhausted past EOF");
    }

    #[test]
    fn mp4_audio_track_decodes() {
        let Some(path) = video_with_audio_asset() else {
            return;
        };
        let mut reader = AudioReader::open(&path, RATE).expect("open");
        let mut buf = vec![0f32; 2048 * CHANNELS];
        let frames = reader.read(&mut buf).expect("read");
        assert!(frames > 0);
    }

    #[test]
    fn zero_rate_is_rejected() {
        assert!(matches!(
            AudioReader::open(Path::new("/nonexistent.mp3"), 0),
            Err(DecodeError::Unsupported { .. })
        ));
    }
}
