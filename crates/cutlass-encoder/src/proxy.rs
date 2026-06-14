//! Proxy builder: transcode a source into a 1080p **all-intra H.264** file.
//!
//! Why all-intra: every frame is its own keyframe (GOP = 1), so a later seek
//! lands exactly on the target and decodes a single frame — turning the
//! long-GOP cold seek (0.4–1.6 s on 4K) into a flat ~9 ms (software) read.
//! See `docs/proxy-cache/research.md` for the measurements behind this.
//!
//! Default pipeline: **software frame-threaded decode → software libx264 at
//! constant quality (CRF)**. A single SW decoder already saturates all cores
//! (≈7.7× realtime), and libx264 at a low CRF produces clean, near-transparent
//! all-intra proxies. Quality-per-bit matters more here than raw build speed,
//! and a SW lane leaves the hardware *decoder* free for live preview.
//!
//! Hardware encode ([`ProxyConfig::hardware`]) is an opt-in fast path: it trades
//! quality-per-bit (VideoToolbox H.264 grains noticeably even at high bitrate)
//! and the CRF knob for raw throughput. Reach for it only when build latency
//! beats fidelity.

use std::path::Path;

use cutlass_decoder::HwAccel;
use ffmpeg_next::format::{self, context::Output};
use ffmpeg_next::media::Type as MediaType;
use ffmpeg_next::software::scaling;
use ffmpeg_next::util::format::Pixel;
use ffmpeg_next::util::frame::video::Video as VideoFrame;
use ffmpeg_next::{
    Dictionary, Error as FfmpegError, Rational, codec, decoder::Video as VideoDecoder,
    encoder::Video as VideoEncoder,
};
use tracing::debug;

use crate::error::EncodeError;
use crate::h264::{drain_encoder, ensure_ffmpeg_init, find_h264_encoder, is_eagain, scaled_dims};

/// How to build a proxy file.
#[derive(Debug, Clone, Copy)]
pub struct ProxyConfig {
    /// Target height in pixels; width is derived from the source aspect ratio
    /// (rounded down to even). The source height caps it — proxies never upscale.
    pub target_height: u32,
    /// Constant-quality level for the software encoder (libx264 CRF, 0–51,
    /// lower = better quality / bigger file). ~18 is visually near-transparent.
    /// Drives the **default** software path; ignored when [`Self::hardware`]
    /// selects a non-CRF encoder (then [`Self::bitrate`] is used instead).
    pub quality: u8,
    /// Fallback target bitrate (bits/sec). Used **only** on the hardware path,
    /// for encoders without a CRF mode (e.g. VideoToolbox / NVENC).
    pub bitrate: usize,
    /// Opt-in hardware H.264 encode (VideoToolbox / NVENC). Faster builds, but
    /// lower quality per bit, and the CRF knob ([`Self::quality`]) no longer
    /// applies — [`Self::bitrate`] is used instead. **Off by default**:
    /// software libx264 at constant quality gives cleaner all-intra proxies,
    /// which matters more than build speed here.
    pub hardware: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            target_height: 1080,
            quality: 18,
            bitrate: 60_000_000,
            // Software libx264 + CRF is the clean, deterministic, provably
            // all-intra path. HW encode is the explicit fast-path opt-in.
            hardware: false,
        }
    }
}

/// Per-lane build knobs that the scheduler varies across concurrent builds.
///
/// The two interesting axes (see `docs/proxy-cache/research.md`):
/// - `decode`: a **software** lane is the fastest single pipeline (one SW decoder
///   frame-threads across all cores), while a **hardware** lane decodes on the GPU
///   block — slower per frame but it frees the CPU so a second concurrent import
///   doesn't fight the SW lane for cores.
/// - `decode_threads`: cap SW frame-threading so two lanes don't oversubscribe.
/// - `encode_threads`: cap encoder slice-threading (mainly libx264; HW encoders
///   typically ignore it).
#[derive(Debug, Clone, Copy)]
pub struct ProxyBuildOptions {
    /// Decode acceleration for reading the *source* during the build.
    pub decode: HwAccel,
    /// Decoder frame-thread count (`0` = all cores). Ignored for HW decode.
    pub decode_threads: u32,
    /// Encoder slice-thread count (`0` = FFmpeg default). Ignored by most HW encoders.
    pub encode_threads: u32,
}

impl Default for ProxyBuildOptions {
    fn default() -> Self {
        Self {
            decode: HwAccel::None,
            decode_threads: 0,
            encode_threads: 0,
        }
    }
}

/// Result of a completed proxy build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyStats {
    pub frames: u64,
    pub width: u32,
    pub height: u32,
}

/// Transcode `source` into an all-intra H.264 proxy at `output`.
///
/// Decodes in software (frame-threaded) and encodes with software libx264 at
/// constant quality by default (see [`ProxyConfig`]). The output is a
/// constant-frame-rate, GOP-1 file whose every frame is a keyframe. Equivalent
/// to [`build_proxy_with`] with the default (software, all-cores) options and
/// no progress reporting.
pub fn build_proxy(
    source: &Path,
    output: &Path,
    config: ProxyConfig,
) -> Result<ProxyStats, EncodeError> {
    build_proxy_with(source, output, config, ProxyBuildOptions::default(), None)
}

/// Like [`build_proxy`], but with per-lane decode options and an optional
/// progress callback.
///
/// `progress`, when given, is invoked periodically with the number of frames
/// encoded so far — the scheduler maps that to a percentage for the UI. It runs
/// on the calling (worker) thread, between packets, so it must not block.
pub fn build_proxy_with(
    source: &Path,
    output: &Path,
    config: ProxyConfig,
    opts: ProxyBuildOptions,
    mut progress: Option<&mut dyn FnMut(u64)>,
) -> Result<ProxyStats, EncodeError> {
    ensure_ffmpeg_init()?;

    let src = source
        .to_str()
        .ok_or_else(|| EncodeError::unsupported("source path not valid UTF-8"))?;

    let mut ictx = format::input(src).map_err(EncodeError::Open)?;

    let (stream_index, src_fps) = {
        let stream = ictx
            .streams()
            .best(MediaType::Video)
            .ok_or_else(|| EncodeError::unsupported("no video stream found"))?;
        (stream.index(), stream.avg_frame_rate())
    };

    let mut dec_ctx = {
        let params = ictx.stream(stream_index).unwrap().parameters();
        codec::context::Context::from_parameters(params).map_err(EncodeError::Open)?
    };

    dec_ctx.set_threading(codec::threading::Config {
        kind: codec::threading::Type::Frame,
        count: opts.decode_threads as usize,
    });

    let _active = cutlass_decoder::attach_hwaccel(&mut dec_ctx, opts.decode)?;
    let mut decoder = dec_ctx.decoder().video().map_err(EncodeError::Open)?;

    let src_w = decoder.width();
    let src_h = decoder.height();

    if src_w == 0 || src_h == 0 {
        return Err(EncodeError::unsupported("source has zero dimensions"));
    }

    let (dst_w, dst_h) = scaled_dims(src_w, src_h, config.target_height);

    let fps = if src_fps.numerator() > 0 && src_fps.denominator() > 0 {
        src_fps
    } else {
        Rational::new(30, 1)
    };

    let enc_tb = fps.invert();

    let chosen = find_h264_encoder(config.hardware)
        .ok_or_else(|| EncodeError::unsupported("no H.264 encoder available"))?;
    let codec = chosen.codec;
    let enc_fmt = chosen.pixel;
    let use_crf = codec.name() == "libx264";

    let mut octx = format::output(output).map_err(EncodeError::Open)?;
    let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);

    let mut enc = codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(EncodeError::Open)?;

    enc.set_width(dst_w);
    enc.set_height(dst_h);
    enc.set_format(enc_fmt);
    enc.set_color_range(ffmpeg_next::color::Range::MPEG);
    enc.set_frame_rate(Some(fps));
    enc.set_time_base(enc_tb);

    // GOP = 1 forces a keyframe on *every* frame (libx264 keyint=1, VideoToolbox
    // max-keyframe-interval=1). This is the whole point of the proxy — do NOT
    // set 0 here: 0 means "let the encoder pick", which on libx264 collapses to
    // a single keyframe at the head and kills exact-seek. `max_b_frames(0)` only
    // removes B-frames; it does not make the stream all-intra on its own.
    enc.set_gop(1);
    enc.set_max_b_frames(0);

    if !use_crf {
        enc.set_bit_rate(config.bitrate);
    }

    if global_header {
        enc.set_flags(codec::Flags::GLOBAL_HEADER);
    }

    enc.set_threading(codec::threading::Config {
        kind: codec::threading::Type::Slice,
        count: opts.encode_threads as usize,
    });

    let mut enc_opts = Dictionary::new();

    if use_crf {
        let crf = config.quality.to_string();
        enc_opts.set("crf", &crf);
        enc_opts.set("preset", "faster");
        // Belt-and-suspenders: pin keyint so no muxer/encoder default can
        // reintroduce inter frames. Redundant with set_gop(1), cheap insurance.
        enc_opts.set("keyint", "1");
        enc_opts.set("min-keyint", "1");
        if opts.encode_threads > 0 {
            enc_opts.set("threads", &opts.encode_threads.to_string());
        }
    }

    let mut encoder = enc.open_with(enc_opts).map_err(EncodeError::Open)?;

    let ost_index = {
        let mut ost = octx.add_stream(codec).map_err(EncodeError::Open)?;
        ost.set_parameters(&encoder);
        ost.index()
    };

    octx.write_header().map_err(EncodeError::Io)?;
    let ost_tb = octx.stream(ost_index).unwrap().time_base();

    let mut scaler: Option<scaling::Context> = None;
    let mut frame_index: i64 = 0;

    for (stream, packet) in ictx.packets() {
        if stream.index() != stream_index {
            continue;
        }
        decoder.send_packet(&packet).map_err(EncodeError::Encode)?;
        drain_decoder(
            &mut decoder,
            &mut scaler,
            (dst_w, dst_h),
            enc_fmt,
            &mut encoder,
            &mut octx,
            ost_index,
            enc_tb,
            ost_tb,
            &mut frame_index,
        )?;
        if let Some(cb) = progress.as_deref_mut() {
            cb(frame_index as u64);
        }
    }

    decoder.send_eof().map_err(EncodeError::Encode)?;

    drain_decoder(
        &mut decoder,
        &mut scaler,
        (dst_w, dst_h),
        enc_fmt,
        &mut encoder,
        &mut octx,
        ost_index,
        enc_tb,
        ost_tb,
        &mut frame_index,
    )?;

    encoder.send_eof().map_err(EncodeError::Encode)?;
    drain_encoder(&mut encoder, &mut octx, ost_index, enc_tb, ost_tb)?;

    octx.write_trailer().map_err(EncodeError::Io)?;

    debug!(frames = frame_index, dst_w, dst_h, "built proxy");

    Ok(ProxyStats {
        frames: frame_index as u64,
        width: dst_w,
        height: dst_h,
    })
}

#[allow(clippy::too_many_arguments)]
fn drain_decoder(
    decoder: &mut VideoDecoder,
    scaler: &mut Option<scaling::Context>,
    dst: (u32, u32),
    dst_fmt: Pixel,
    encoder: &mut VideoEncoder,
    octx: &mut Output,
    ost_index: usize,
    enc_tb: Rational,
    ost_tb: Rational,
    frame_index: &mut i64,
) -> Result<(), EncodeError> {
    let mut decoded = VideoFrame::empty();

    loop {
        match decoder.receive_frame(&mut decoded) {
            Ok(()) => {
                let mut sw = VideoFrame::empty();

                let src: &VideoFrame =
                    if cutlass_decoder::is_hardware_pixel_format(decoded.format()) {
                        cutlass_decoder::transfer_hw_frame_to_cpu(&decoded, &mut sw)?;
                        &sw
                    } else {
                        &decoded
                    };

                if scaler.is_none() {
                    *scaler = Some(
                        scaling::Context::get(
                            src.format(),
                            src.width(),
                            src.height(),
                            dst_fmt,
                            dst.0,
                            dst.1,
                            scaling::Flags::BILINEAR,
                        )
                        .map_err(EncodeError::Encode)?,
                    );
                }

                let sc = scaler.as_mut().unwrap();

                let mut scaled = VideoFrame::empty();
                sc.run(src, &mut scaled).map_err(EncodeError::Encode)?;

                scaled.set_pts(Some(*frame_index));
                encoder.send_frame(&scaled).map_err(EncodeError::Encode)?;

                *frame_index += 1;

                drain_encoder(encoder, octx, ost_index, enc_tb, ost_tb)?;
            }
            Err(FfmpegError::Eof) => return Ok(()),
            Err(e) if is_eagain(&e) => return Ok(()),
            Err(e) => return Err(EncodeError::Encode(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use cutlass_decoder::{DecodeOptions, Decoder, HwAccel};

    use super::*;

    fn workspace_asset(name: &str) -> Option<PathBuf> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../local-assets/assets")
            .join(name);
        path.exists().then_some(path)
    }

    fn first_asset() -> Option<PathBuf> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local-assets/assets");
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "mp4"))
            .map(|e| e.path())
            .collect();
        entries.sort();
        entries.into_iter().next()
    }

    /// In-process equivalent of `ffprobe -show_entries frame=pict_type`: every
    /// video packet in an all-intra (GOP-1) file carries the keyframe flag, so
    /// "all packets are key" ⟺ "every frame is intra". No external `ffprobe`
    /// binary required, so this runs on any CI box that links libav.
    ///
    /// Returns `(all_key, packet_count)`.
    fn intra_audit(path: &Path) -> (bool, u64) {
        let mut ictx = format::input(&path).expect("open built proxy");
        let vstream = ictx
            .streams()
            .best(MediaType::Video)
            .expect("video stream in proxy")
            .index();

        let mut all_key = true;
        let mut count: u64 = 0;
        for (stream, packet) in ictx.packets() {
            if stream.index() != vstream {
                continue;
            }
            count += 1;
            if !packet.is_key() {
                all_key = false;
            }
        }
        (all_key, count)
    }

    #[test]
    fn proxy_builds_all_intra_and_is_seekable() {
        let Some(src) = first_asset() else {
            return;
        };
        let out = std::env::temp_dir().join("cutlass_proxy_smoke.mp4");
        let _ = std::fs::remove_file(&out);

        // Exercise the DEFAULT path: software libx264 + CRF, which is the
        // provably all-intra pipeline. `hardware: false` makes `quality` live.
        let stats = build_proxy(
            &src,
            &out,
            ProxyConfig {
                target_height: 180,
                quality: 23,
                bitrate: 2_000_000,
                hardware: false,
            },
        )
        .expect("build proxy");
        assert!(stats.frames > 0, "no frames encoded");
        assert!(out.exists(), "proxy file missing");

        // The headline invariant: every frame is a keyframe. Without this, the
        // "flat ~9 ms seek" guarantee is a lie and the crate has no reason to
        // exist. A long-GOP file would pass the seek test below but fail here.
        let (all_intra, packets) = intra_audit(&out);
        assert!(packets > 0, "proxy has no video packets");
        assert!(
            all_intra,
            "proxy is NOT all-intra (found inter frames among {packets} packets) — gop/keyint broken"
        );
        // Encoder may drop/dup frames vs decode count; allow a small drift but
        // catch a wholesale mismatch.
        assert!(
            packets.abs_diff(stats.frames) <= 1,
            "packet count {packets} diverges from reported {} frames",
            stats.frames
        );

        let mut dec = Decoder::open_with(&out, DecodeOptions::default().hw_accel(HwAccel::None))
            .expect("open proxy");
        let frame = dec
            .seek_to_frame(Duration::from_millis(500))
            .expect("seek proxy")
            .expect("frame after seek");
        assert!(frame.width > 0 && !frame.planes.is_empty());

        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn proxy_builds_via_hardware_decode_lane() {
        let Some(src) = workspace_asset("11921980_1920_1080_30fps.mp4").or_else(first_asset) else {
            return;
        };
        let out = std::env::temp_dir().join("cutlass_proxy_hwdec.mp4");
        let _ = std::fs::remove_file(&out);

        let mut progressed: u64 = 0;
        let mut on_progress = |done: u64| progressed = done;
        // Decode lane is hardware (the thing under test); encode stays on
        // software libx264 so the all-intra assertion is deterministic and
        // independent of the HW encoder's keyframe-interval behaviour.
        let stats = build_proxy_with(
            &src,
            &out,
            ProxyConfig {
                target_height: 180,
                quality: 23,
                bitrate: 2_000_000,
                hardware: false,
            },
            ProxyBuildOptions {
                decode: HwAccel::Auto,
                decode_threads: 0,
                encode_threads: 0,
            },
            Some(&mut on_progress),
        )
        .expect("build proxy via hw decode");
        assert!(stats.frames > 0, "no frames encoded");
        assert!(progressed > 0, "progress callback never fired");
        assert!(out.exists(), "proxy file missing");

        let (all_intra, packets) = intra_audit(&out);
        assert!(packets > 0, "proxy has no video packets");
        assert!(all_intra, "hw-decode-lane proxy is NOT all-intra");

        let mut dec = Decoder::open_with(&out, DecodeOptions::default().hw_accel(HwAccel::None))
            .expect("open proxy");
        let frame = dec
            .seek_to_frame(Duration::from_millis(300))
            .expect("seek proxy")
            .expect("frame after seek");
        assert!(frame.width > 0 && !frame.planes.is_empty());

        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn scaled_dims_preserve_aspect_and_evenness() {
        assert_eq!(scaled_dims(3840, 2160, 1080), (1920, 1080));
        assert_eq!(scaled_dims(1280, 720, 1080), (1280, 720));
        let (w, h) = scaled_dims(1921, 1081, 1080);
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }
}
