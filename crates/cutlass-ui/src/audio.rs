//! Audio playback: device output, timeline mixing, and the playback master
//! clock (playback roadmap Phase 3).
//!
//! Three players, all decoupled by lock-free state:
//!
//! - The **UI thread** owns transport intent: play/seek bump an *epoch* and
//!   reset the shared clock atomically, then notify the mixer. It also reads
//!   the clock (`AudioHandle::current_tick`) from the playback timer.
//! - The **mixer thread** owns `AudioReader`s and the timeline snapshot. It
//!   decodes + sums every audible span into fixed-size stereo blocks, tagged
//!   with the epoch they were mixed for, and feeds them through a bounded
//!   channel sized to ~100ms.
//! - The **device callback** pops blocks, drops any with a stale epoch
//!   (seek flush without locks), spreads stereo onto the device's channel
//!   count, and advances `frames_played` — *consumed frames are the clock*,
//!   so A/V sync is device-true regardless of buffer depth.
//!
//! The clock counts only real consumed frames: an underrun stalls the
//! playhead briefly instead of letting video run away from audio. Silence
//! (no clips under the playhead) still produces zero-blocks, so the audio
//! device paces silent timelines too; the wall-clock transport is only a
//! fallback for machines with no output device (and for shuttle speeds,
//! which play muted — see `transport.rs`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use cutlass_decoder::{AUDIO_CHANNELS, AudioReader};
use cutlass_models::Param;
use tracing::{debug, info, warn};

/// Frames per mixed block. 1024 @ 48kHz ≈ 21ms.
const BLOCK_FRAMES: usize = 1024;
/// Blocks in flight device-ward. 6 × 21ms ≈ 128ms of buffered audio — also
/// the worst-case latency for a mid-playback edit to become audible.
const BLOCK_CAPACITY: usize = 6;
/// Blocks one scrub gesture emits before the mixer goes idle (M8 Phase 7).
/// 4 × 21ms ≈ 85ms — long enough to *hear* the content under the playhead,
/// short enough that the next drag step retriggers a fresh burst (CapCut's
/// scrub-audio feel). Each step bumps the epoch, so the callback drops the
/// previous burst's tail and the newest position wins.
const SCRUB_BURST_BLOCKS: usize = 4;

// ---------------------------------------------------------------------------
// Snapshot: what the timeline sounds like (worker → mixer)
// ---------------------------------------------------------------------------

/// One audible clip in rational time. The mixer converts to device frames.
pub struct AudioSpan {
    pub path: PathBuf,
    /// Timeline placement, sequence ticks at the snapshot's fps.
    pub start_tick: i64,
    pub end_tick: i64,
    /// Source-in value at `source_rate` (the media's native rate).
    pub source_start: i64,
    pub source_rate: (i32, i32),
    /// Source window length at `source_rate` (the clip's in/out range). Drives
    /// the varispeed stretch ratio for retimed clips; ignored at 1×.
    pub source_duration: i64,
    /// Retimed (constant speed ≠ 1× and/or reversed and/or a speed ramp, M8
    /// Phase 3): the mixer time-stretches the source window to the span length
    /// instead of reading it 1:1.
    pub retimed: bool,
    /// Play the source window back-to-front (CapCut reverse).
    pub reversed: bool,
    /// Varispeed pitch factor (`1.0` keeps pitch; `> 1.0` is chipmunk mode).
    /// Only consulted when `retimed`.
    pub pitch_factor: f32,
    /// Speed ramp (CapCut speed curves, M2): `Some` ⇒ the retime rate varies
    /// over the clip, and the stretch follows the curve's normalized integral
    /// (`Param` over `0..=SPEED_CURVE_SCALE`). `None` ⇒ a constant-rate retime.
    pub speed_curve: Option<Param<f32>>,
    /// Clip gain envelope (volume, M1 → M8): `1.0` ⇔ unchanged. Keyframe
    /// ticks are clip-relative sequence ticks at the snapshot's fps; the
    /// mixer rebases them into sample frames once per span.
    pub volume: Param<f32>,
    /// Fade ramp lengths, sequence ticks at the snapshot's fps.
    pub fade_in_ticks: i64,
    pub fade_out_ticks: i64,
    /// Noise reduction (M8 Phase 5): render the span's audio through RNNoise
    /// and serve the cleaned buffer 1:1, like a retime. Stacks with a retime
    /// (denoise rides on top of the stretched buffer).
    pub denoise: bool,
}

/// Every unmuted audio clip on the timeline + the sequence rate.
pub struct AudioSnapshot {
    pub fps: (i32, i32),
    pub spans: Vec<AudioSpan>,
}

enum AudioMsg {
    Snapshot(AudioSnapshot),
    /// Start (or re-anchor) producing from `tick`. `epoch` was assigned by
    /// the UI thread *before* send; blocks tagged older are already dead.
    Play {
        tick: i64,
        epoch: u64,
    },
    Pause,
    /// Emit one short audio burst from `tick` (M8 Phase 7 scrub audio), then
    /// idle. `epoch` (assigned UI-side) supersedes any earlier play/scrub
    /// burst so only the newest scrub position is heard.
    Scrub {
        tick: i64,
        epoch: u64,
    },
}

/// One mixed block on its way to the device.
struct AudioBlock {
    epoch: u64,
    /// Interleaved stereo, `BLOCK_FRAMES * AUDIO_CHANNELS` long.
    samples: Vec<f32>,
}

// ---------------------------------------------------------------------------
// Shared clock state
// ---------------------------------------------------------------------------

struct AudioShared {
    /// Bumped by every play/seek/scrub; the callback discards stale-tagged
    /// blocks (the lock-free flush).
    epoch: AtomicU64,
    /// Timeline tick at the current epoch's origin.
    anchor_tick: AtomicI64,
    /// Device frames of the current epoch consumed by the callback.
    frames_played: AtomicU64,
    playing: AtomicBool,
    /// Scrub-audio gate (M8 Phase 7): the callback plays queued bursts while
    /// set, but — unlike `playing` — never advances `frames_played`, so a
    /// scrub burst is heard without moving the master clock (the playhead is
    /// driven by the drag, not the audio).
    scrubbing: AtomicBool,
    underruns: AtomicU64,
}

/// Cloneable, `Send` interface to the audio system: transport control for
/// the UI thread, snapshot publishing for the engine worker, and the master
/// clock for the playback timer.
#[derive(Clone)]
pub struct AudioHandle {
    shared: Arc<AudioShared>,
    tx: Option<Sender<AudioMsg>>,
    sample_rate: u32,
}

impl AudioHandle {
    /// Whether a real output device is pacing playback. False ⇒ the UI must
    /// fall back to the wall-clock transport.
    pub fn active(&self) -> bool {
        self.tx.is_some()
    }

    /// Start playing from `tick` (also the mid-playback seek: every seek is
    /// a re-anchored play). Clock state flips on the caller's thread, so a
    /// `current_tick` immediately after returns `tick` — no mixer round-trip.
    pub fn play(&self, tick: i64) {
        let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.anchor_tick.store(tick, Ordering::Release);
        self.shared.frames_played.store(0, Ordering::Release);
        self.shared.scrubbing.store(false, Ordering::Release);
        self.shared.playing.store(true, Ordering::Release);
        if let Some(tx) = &self.tx {
            let _ = tx.send(AudioMsg::Play { tick, epoch });
        }
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::Release);
        self.shared.scrubbing.store(false, Ordering::Release);
        if let Some(tx) = &self.tx {
            let _ = tx.send(AudioMsg::Pause);
        }
    }

    /// Play a short audio burst from `tick` (M8 Phase 7): the UI calls this as
    /// the playhead is dragged while paused, so the user hears the content
    /// under the playhead. Bumps the epoch so the burst supersedes the prior
    /// one; never touches the master clock (a scrub must not move the
    /// playhead, the drag already does). No-op without an output device.
    pub fn scrub(&self, tick: i64) {
        if self.tx.is_none() {
            return;
        }
        let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.playing.store(false, Ordering::Release);
        self.shared.scrubbing.store(true, Ordering::Release);
        if let Some(tx) = &self.tx {
            let _ = tx.send(AudioMsg::Scrub { tick, epoch });
        }
    }

    pub fn publish_snapshot(&self, snapshot: AudioSnapshot) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(AudioMsg::Snapshot(snapshot));
        }
    }

    /// Playhead tick by the audio clock: anchor + consumed device frames at
    /// the sequence rate (exact i128, floored).
    pub fn current_tick(&self, fps_num: i32, fps_den: i32) -> i64 {
        let anchor = self.shared.anchor_tick.load(Ordering::Acquire);
        if fps_num <= 0 || fps_den <= 0 || self.sample_rate == 0 {
            return anchor;
        }
        let frames = self.shared.frames_played.load(Ordering::Acquire);
        let ticks = i128::from(frames) * i128::from(fps_num)
            / (i128::from(self.sample_rate) * i128::from(fps_den));
        (i128::from(anchor) + ticks).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }
}

// ---------------------------------------------------------------------------
// System: device stream + mixer thread
// ---------------------------------------------------------------------------

/// Owns the cpal stream (`!Send` — lives on the main thread) and the mixer
/// thread. Dropping it stops audio; hand out [`AudioHandle`]s freely.
pub struct AudioSystem {
    handle: AudioHandle,
    _stream: Option<cpal::Stream>,
}

impl AudioSystem {
    /// Bring up the default output device. A machine without one degrades to
    /// a disabled system whose handle reports `active() == false`.
    pub fn start() -> Self {
        match Self::try_start() {
            Ok(system) => system,
            Err(e) => {
                warn!("audio output unavailable, playback clock falls back to wall time: {e}");
                Self {
                    handle: AudioHandle {
                        shared: Arc::new(AudioShared::new()),
                        tx: None,
                        sample_rate: 0,
                    },
                    _stream: None,
                }
            }
        }
    }

    pub fn handle(&self) -> AudioHandle {
        self.handle.clone()
    }

    fn try_start() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no default output device")?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        if config.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!(
                "default output format {:?} is not f32",
                config.sample_format()
            ));
        }
        let stream_config: cpal::StreamConfig = config.into();
        let sample_rate = stream_config.sample_rate;
        let device_channels = stream_config.channels as usize;

        let shared = Arc::new(AudioShared::new());
        let (msg_tx, msg_rx) = unbounded::<AudioMsg>();
        let (block_tx, block_rx) = bounded::<AudioBlock>(BLOCK_CAPACITY);

        let cb_shared = Arc::clone(&shared);
        let mut sink = CallbackSink::new(block_rx);
        let stream = device
            .build_output_stream(
                stream_config,
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    sink.fill(out, device_channels, &cb_shared);
                },
                |e| warn!("audio stream error: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;

        let mixer_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("cutlass-audio-mixer".into())
            .spawn(move || mixer_loop(msg_rx, block_tx, mixer_shared, sample_rate))
            .map_err(|e| e.to_string())?;

        info!(sample_rate, device_channels, "audio output ready");
        Ok(Self {
            handle: AudioHandle {
                shared,
                tx: Some(msg_tx),
                sample_rate,
            },
            _stream: Some(stream),
        })
    }
}

impl AudioShared {
    fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            anchor_tick: AtomicI64::new(0),
            frames_played: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            scrubbing: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
        }
    }
}

// ---------------------------------------------------------------------------
// Device callback
// ---------------------------------------------------------------------------

/// Callback-side state: the block being consumed plus its read cursor.
struct CallbackSink {
    rx: Receiver<AudioBlock>,
    current: Option<AudioBlock>,
    cursor: usize,
}

impl CallbackSink {
    fn new(rx: Receiver<AudioBlock>) -> Self {
        Self {
            rx,
            current: None,
            cursor: 0,
        }
    }

    /// Real-time path: no locks, no blocking waits; allocation only when a
    /// stale block is dropped (acceptable for a desktop editor's callback).
    fn fill(&mut self, out: &mut [f32], device_channels: usize, shared: &AudioShared) {
        out.fill(0.0);
        let playing = shared.playing.load(Ordering::Acquire);
        // Scrub bursts play through the same ring (M8 Phase 7) but don't drive
        // the clock — only real playback advances `frames_played`.
        let scrubbing = !playing && shared.scrubbing.load(Ordering::Acquire);
        if !playing && !scrubbing {
            self.current = None;
            return;
        }
        let epoch = shared.epoch.load(Ordering::Acquire);
        let want_frames = out.len() / device_channels.max(1);
        let mut filled = 0usize;

        while filled < want_frames {
            let block = match &self.current {
                Some(b) if b.epoch == epoch && self.cursor < b.samples.len() => {
                    self.current.as_ref().expect("just matched")
                }
                _ => {
                    self.current = None;
                    match self.rx.try_recv() {
                        Ok(b) if b.epoch == epoch => {
                            self.cursor = 0;
                            self.current.insert(b)
                        }
                        Ok(_stale) => continue, // pre-seek block: drop, next
                        Err(_) => break,        // ring empty: underrun
                    }
                }
            };

            let frames_left = (block.samples.len() - self.cursor) / AUDIO_CHANNELS;
            let take = frames_left.min(want_frames - filled);
            for i in 0..take {
                let src = self.cursor + i * AUDIO_CHANNELS;
                let (l, r) = (block.samples[src], block.samples[src + 1]);
                let dst = (filled + i) * device_channels;
                match device_channels {
                    0 => {}
                    1 => out[dst] = 0.5 * (l + r),
                    _ => {
                        out[dst] = l;
                        out[dst + 1] = r;
                        // extra channels stay silent
                    }
                }
            }
            self.cursor += take * AUDIO_CHANNELS;
            filled += take;
        }

        // Only real playback owns the clock; a scrub burst draining to silence
        // is expected, not an underrun.
        if playing {
            if filled > 0 {
                shared
                    .frames_played
                    .fetch_add(filled as u64, Ordering::AcqRel);
            }
            if filled < want_frames {
                shared.underruns.fetch_add(1, Ordering::AcqRel);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mixer thread
// ---------------------------------------------------------------------------

/// A span with its times resolved to device sample frames.
struct ResolvedSpan {
    path: PathBuf,
    start_frame: i64,
    end_frame: i64,
    source_start_frame: i64,
    /// Gain envelope rebased into clip-relative sample frames.
    volume: Param<f32>,
    fade_in_frames: i64,
    fade_out_frames: i64,
    /// Speed ramp (M2): drives the variable-rate render when present.
    speed_curve: Option<Param<f32>>,
    /// Retimed (varispeed): time-stretch the source window. A render key may
    /// also exist without this — a denoise-only span renders 1:1 (M8 Phase 5).
    retimed: bool,
    /// Render identity for a span that must be rendered once + served 1:1
    /// (retimed and/or denoised); `None` ⇒ stream the source 1:1.
    render: Option<RenderKey>,
}

/// Identifies a retimed span's time-stretch render (M8 Phase 3). Doubles as
/// the cache key so a span keeps its rendered buffer across snapshots while
/// nothing it depends on changes, and re-renders the moment any does.
#[derive(Clone, PartialEq, Eq, Hash)]
struct RenderKey {
    path: PathBuf,
    source_start_frame: i64,
    /// Source window length in device sample frames.
    source_frames: i64,
    /// Stretched output length in device sample frames (the span length).
    out_frames: i64,
    reversed: bool,
    /// Pitch factor bit pattern (f32 isn't `Hash`/`Eq`).
    pitch_bits: u32,
    /// Speed-ramp identity (M2): the curve's keyframes flattened to a hashable
    /// form so editing the ramp re-renders. Empty for a constant-rate retime.
    curve: Vec<CurveKeyframe>,
    /// Noise reduction (M8 Phase 5): part of the cache identity so toggling it
    /// re-renders, and so a denoised + a clean copy of the same window don't
    /// collide.
    denoise: bool,
}

/// One speed-ramp keyframe as a hashable tuple — `(tick, value bits, easing
/// tag, bezier control-point bits)` — for the [`RenderKey`] cache identity
/// (`f32` and [`cutlass_models::Easing`] are neither `Hash` nor `Eq`).
type CurveKeyframe = (i64, u32, u8, [u32; 4]);

/// Flatten a speed ramp to a hashable cache identity. The empty vec stands for
/// a constant-rate retime (no ramp).
fn curve_key(curve: Option<&Param<f32>>) -> Vec<CurveKeyframe> {
    let Some(curve) = curve else {
        return Vec::new();
    };
    curve
        .keyframes()
        .iter()
        .map(|kf| {
            let (tag, pts) = match kf.easing {
                cutlass_models::Easing::Linear => (0u8, [0u32; 4]),
                cutlass_models::Easing::EaseIn => (1, [0; 4]),
                cutlass_models::Easing::EaseOut => (2, [0; 4]),
                cutlass_models::Easing::EaseInOut => (3, [0; 4]),
                cutlass_models::Easing::Bezier { points } => (4, points.map(f32::to_bits)),
            };
            (kf.tick, kf.value.to_bits(), tag, pts)
        })
        .collect()
}

fn mixer_loop(
    msg_rx: Receiver<AudioMsg>,
    block_tx: Sender<AudioBlock>,
    shared: Arc<AudioShared>,
    sample_rate: u32,
) {
    let mut spans: Vec<ResolvedSpan> = Vec::new();
    let mut pending_snapshot: Option<AudioSnapshot> = None;
    let mut fps = (0i32, 0i32);
    // Keyed by (path, timeline start): one reader per span, so sequential
    // playback never seeks, and two clips of the same media never thrash a
    // shared demuxer position.
    let mut readers: HashMap<(PathBuf, i64), AudioReader> = HashMap::new();
    let mut failed_opens: HashMap<PathBuf, ()> = HashMap::new();
    // Time-stretched buffers for retimed spans (M8 Phase 3), rendered once
    // and served 1:1; kept across snapshots while the render identity holds.
    let mut rendered: HashMap<RenderKey, Arc<Vec<f32>>> = HashMap::new();
    let mut epoch = 0u64;
    let mut playing = false;
    // Scrub-burst state (M8 Phase 7): how many burst blocks remain to emit
    // before the mixer idles. Reset on each `Scrub`, zeroed by play/pause.
    let mut scrub_blocks_left = 0usize;
    // Timeline device-frame position the next block mixes from.
    let mut write_frame = 0i64;
    let mut last_underruns = 0u64;

    loop {
        // Heartbeat doubles as the pacing valve: when the block channel is
        // full we sleep here instead of spinning.
        match msg_rx.recv_timeout(Duration::from_millis(4)) {
            Ok(msg) => {
                let mut latest = Some(msg);
                while let Some(msg) = latest.take() {
                    match msg {
                        AudioMsg::Snapshot(snapshot) => pending_snapshot = Some(snapshot),
                        AudioMsg::Play { tick, epoch: e } => {
                            epoch = e;
                            playing = true;
                            scrub_blocks_left = 0;
                            write_frame = ticks_to_frames(tick, fps, sample_rate);
                        }
                        AudioMsg::Pause => {
                            playing = false;
                            scrub_blocks_left = 0;
                        }
                        AudioMsg::Scrub { tick, epoch: e } => {
                            epoch = e;
                            playing = false;
                            scrub_blocks_left = SCRUB_BURST_BLOCKS;
                            write_frame = ticks_to_frames(tick, fps, sample_rate);
                        }
                    }
                    latest = msg_rx.try_recv().ok();
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }

        if let Some(snapshot) = pending_snapshot.take() {
            if playing && fps != snapshot.fps && fps.0 > 0 {
                // Rate change mid-play (project swap): keep the time, not
                // the frame count.
                let tick = frames_to_ticks(write_frame, fps, sample_rate);
                write_frame = ticks_to_frames(tick, snapshot.fps, sample_rate);
            }
            fps = snapshot.fps;
            spans = resolve_spans(&snapshot, sample_rate);
            let live: std::collections::HashSet<(PathBuf, i64)> = spans
                .iter()
                .map(|s| (s.path.clone(), s.start_frame))
                .collect();
            readers.retain(|key, _| live.contains(key));
            let live_renders: std::collections::HashSet<RenderKey> =
                spans.iter().filter_map(|s| s.render.clone()).collect();
            rendered.retain(|key, _| live_renders.contains(key));
            failed_opens.clear();
        }

        // Idle unless playing or mid-scrub-burst.
        if !playing && scrub_blocks_left == 0 {
            continue;
        }

        // Fill whatever room the device side has left, but never starve the
        // message queue: a pending play/seek/scrub/snapshot outranks the next
        // block. A scrub burst stops after its fixed block count (then idles).
        while !block_tx.is_full() && msg_rx.is_empty() {
            if !playing && scrub_blocks_left == 0 {
                break;
            }
            let mut samples = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
            mix_block(
                &spans,
                &mut readers,
                &mut failed_opens,
                &mut rendered,
                write_frame,
                sample_rate,
                &mut samples,
            );
            if block_tx.send(AudioBlock { epoch, samples }).is_err() {
                return; // device side gone
            }
            write_frame += BLOCK_FRAMES as i64;
            if !playing {
                scrub_blocks_left -= 1;
            }
        }

        let underruns = shared.underruns.load(Ordering::Acquire);
        if underruns > last_underruns {
            debug!(underruns, "audio underruns (device starved)");
            last_underruns = underruns;
        }
    }
}

/// Sum every span overlapping `[pos, pos + BLOCK_FRAMES)` into `out`.
fn mix_block(
    spans: &[ResolvedSpan],
    readers: &mut HashMap<(PathBuf, i64), AudioReader>,
    failed_opens: &mut HashMap<PathBuf, ()>,
    rendered: &mut HashMap<RenderKey, Arc<Vec<f32>>>,
    pos: i64,
    sample_rate: u32,
    out: &mut [f32],
) {
    let block_frames = (out.len() / AUDIO_CHANNELS) as i64;
    let block_end = pos + block_frames;
    let mut scratch = [0f32; BLOCK_FRAMES * AUDIO_CHANNELS];

    for span in spans {
        if span.start_frame >= block_end || span.end_frame <= pos {
            continue;
        }
        let s = span.start_frame.max(pos);
        let e = span.end_frame.min(block_end);

        // Rendered spans — retimed (M8 Phase 3) and/or denoised (M8 Phase 5) —
        // play a buffer rendered once and served 1:1 instead of reading the
        // source at native rate.
        if let Some(key) = &span.render {
            mix_rendered_span(
                span,
                key,
                rendered,
                failed_opens,
                sample_rate,
                pos,
                s,
                e,
                out,
            );
            continue;
        }
        let want = (e - s) as usize;

        let key = (span.path.clone(), span.start_frame);
        let reader = match readers.entry(key) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                if failed_opens.contains_key(&span.path) {
                    continue;
                }
                match AudioReader::open(&span.path, sample_rate) {
                    Ok(reader) => v.insert(reader),
                    Err(e) => {
                        warn!(path = %span.path.display(), "audio open failed: {e}");
                        failed_opens.insert(span.path.clone(), ());
                        continue;
                    }
                }
            }
        };

        let src_from = span.source_start_frame + (s - span.start_frame);
        if reader.seek_to_frame(src_from).is_err() {
            continue;
        }
        // A stream that starts after the requested point leaves a lead gap;
        // shift the mix-in to keep the rest aligned.
        let lead = reader
            .position()
            .map_or(0, |p| (p - src_from).clamp(0, e - s) as usize);

        let slots = &mut scratch[..want * AUDIO_CHANNELS];
        let got = match reader.read(&mut slots[lead * AUDIO_CHANNELS..]) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let offset = ((s - pos) as usize + lead) * AUDIO_CHANNELS;
        let unity = span.volume.constant() == Some(1.0)
            && span.fade_in_frames == 0
            && span.fade_out_frames == 0;
        if unity {
            for (i, sample) in slots[lead * AUDIO_CHANNELS..(lead + got) * AUDIO_CHANNELS]
                .iter()
                .enumerate()
            {
                out[offset + i] += sample;
            }
        } else {
            // Volume envelope + fade ramps (M1/M8): gain per sample frame so
            // automation and fades are smooth at sample resolution, not
            // block-stepped.
            let span_len = span.end_frame - span.start_frame;
            let first = s + lead as i64 - span.start_frame;
            for frame in 0..got {
                let gain = cutlass_models::audio_gain_at(
                    first + frame as i64,
                    span_len,
                    &span.volume,
                    span.fade_in_frames,
                    span.fade_out_frames,
                );
                let src = (lead + frame) * AUDIO_CHANNELS;
                let dst = offset + frame * AUDIO_CHANNELS;
                for ch in 0..AUDIO_CHANNELS {
                    out[dst + ch] += slots[src + ch] * gain;
                }
            }
        }
    }

    for sample in out.iter_mut() {
        *sample = sample.clamp(-1.0, 1.0);
    }
}

/// Render a span's full buffer (interleaved stereo). Retimed spans (M8 Phase 3)
/// time-stretch — variable-rate for a ramp (M2), uniform otherwise; a pure
/// denoise span (M8 Phase 5) reads the window 1:1. Denoise then rides on top of
/// whichever, so a clip can be both sped up and cleaned.
fn render_span(
    span: &ResolvedSpan,
    key: &RenderKey,
    sample_rate: u32,
) -> Result<Vec<f32>, cutlass_decoder::DecodeError> {
    let mut buf = if span.retimed {
        match &span.speed_curve {
            Some(curve) => cutlass_decoder::render_stretched_curve(
                &span.path,
                sample_rate,
                key.source_start_frame,
                key.source_frames,
                key.out_frames,
                key.reversed,
                f32::from_bits(key.pitch_bits),
                |p| cutlass_models::speed_curve_source_fraction(curve, p),
            )?,
            None => cutlass_decoder::render_stretched(
                &span.path,
                sample_rate,
                key.source_start_frame,
                key.source_frames,
                key.out_frames,
                key.reversed,
                f32::from_bits(key.pitch_bits),
            )?,
        }
    } else {
        // Denoise-only: read the window 1:1 and clean it.
        cutlass_decoder::render_denoised(
            &span.path,
            sample_rate,
            key.source_start_frame,
            key.source_frames,
            key.out_frames,
            key.reversed,
        )?
    };
    // Denoise stacks on top of a retime's stretched buffer.
    if key.denoise && span.retimed {
        cutlass_decoder::denoise_interleaved(&mut buf);
    }
    Ok(buf)
}

/// Mix the overlap `[s, e)` of a rendered span — retimed (M8 Phase 3) and/or
/// denoised (M8 Phase 5) — by serving its rendered buffer 1:1. The buffer is
/// rendered + cached on first touch; the volume envelope and fades ride on top
/// exactly like the 1× path.
#[allow(clippy::too_many_arguments)]
fn mix_rendered_span(
    span: &ResolvedSpan,
    key: &RenderKey,
    rendered: &mut HashMap<RenderKey, Arc<Vec<f32>>>,
    failed_opens: &mut HashMap<PathBuf, ()>,
    sample_rate: u32,
    pos: i64,
    s: i64,
    e: i64,
    out: &mut [f32],
) {
    let buf = match rendered.get(key) {
        Some(buf) => buf.clone(),
        None => {
            if failed_opens.contains_key(&span.path) {
                return;
            }
            match render_span(span, key, sample_rate) {
                Ok(buf) => {
                    let buf = Arc::new(buf);
                    rendered.insert(key.clone(), Arc::clone(&buf));
                    buf
                }
                Err(err) => {
                    warn!(path = %span.path.display(), "audio render failed: {err}");
                    failed_opens.insert(span.path.clone(), ());
                    return;
                }
            }
        }
    };

    let span_len = span.end_frame - span.start_frame;
    let total_frames = buf.len() / AUDIO_CHANNELS;
    let unity = span.volume.constant() == Some(1.0)
        && span.fade_in_frames == 0
        && span.fade_out_frames == 0;
    for f in s..e {
        let bi = (f - span.start_frame) as usize;
        if bi >= total_frames {
            break;
        }
        let dst = ((f - pos) as usize) * AUDIO_CHANNELS;
        let gain = if unity {
            1.0
        } else {
            cutlass_models::audio_gain_at(
                f - span.start_frame,
                span_len,
                &span.volume,
                span.fade_in_frames,
                span.fade_out_frames,
            )
        };
        for ch in 0..AUDIO_CHANNELS {
            out[dst + ch] += buf[bi * AUDIO_CHANNELS + ch] * gain;
        }
    }
}

fn resolve_spans(snapshot: &AudioSnapshot, sample_rate: u32) -> Vec<ResolvedSpan> {
    snapshot
        .spans
        .iter()
        .map(|span| {
            let start_frame = ticks_to_frames(span.start_tick, snapshot.fps, sample_rate);
            let end_frame = ticks_to_frames(span.end_tick, snapshot.fps, sample_rate);
            let source_start_frame =
                ticks_to_frames(span.source_start, span.source_rate, sample_rate);
            // A span renders-once-and-caches when retimed (varispeed) and/or
            // denoised (M8 Phase 5). A pure denoise reads the window 1:1, so
            // its source length is just the span length.
            let needs_render = span.retimed || span.denoise;
            let render = needs_render.then(|| RenderKey {
                path: span.path.clone(),
                source_start_frame,
                source_frames: if span.retimed {
                    ticks_to_frames(span.source_duration, span.source_rate, sample_rate)
                } else {
                    end_frame - start_frame
                },
                out_frames: end_frame - start_frame,
                reversed: span.reversed,
                pitch_bits: span.pitch_factor.to_bits(),
                curve: curve_key(span.speed_curve.as_ref()),
                denoise: span.denoise,
            });
            ResolvedSpan {
                path: span.path.clone(),
                start_frame,
                end_frame,
                source_start_frame,
                // Rebase the envelope's clip-relative ticks into clip-relative
                // sample frames, matching the per-frame `pos` the mixer feeds it.
                volume: span
                    .volume
                    .map_ticks(|tick| ticks_to_frames(tick, snapshot.fps, sample_rate)),
                fade_in_frames: ticks_to_frames(span.fade_in_ticks, snapshot.fps, sample_rate),
                fade_out_frames: ticks_to_frames(span.fade_out_ticks, snapshot.fps, sample_rate),
                // The ramp's keyframe ticks are normalized (0..=SPEED_CURVE_SCALE),
                // independent of fps, so it rides through unmapped.
                speed_curve: span.speed_curve.clone(),
                retimed: span.retimed,
                render,
            }
        })
        .collect()
}

/// `value` ticks at `rate` fps → device sample frames, exact i128, floored.
fn ticks_to_frames(value: i64, rate: (i32, i32), sample_rate: u32) -> i64 {
    let (num, den) = rate;
    if num <= 0 || den <= 0 || sample_rate == 0 {
        return 0;
    }
    let frames = i128::from(value) * i128::from(den) * i128::from(sample_rate) / i128::from(num);
    frames.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Inverse of [`ticks_to_frames`] (floored).
fn frames_to_ticks(frames: i64, rate: (i32, i32), sample_rate: u32) -> i64 {
    let (num, den) = rate;
    if num <= 0 || den <= 0 || sample_rate == 0 {
        return 0;
    }
    let ticks = i128::from(frames) * i128::from(num) / (i128::from(den) * i128::from(sample_rate));
    ticks.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_frame_conversion_is_exact_for_integer_rates() {
        // 24 ticks (1s at 24fps) = 48000 frames at 48kHz.
        assert_eq!(ticks_to_frames(24, (24, 1), 48_000), 48_000);
        assert_eq!(frames_to_ticks(48_000, (24, 1), 48_000), 24);
        // One tick = 2000 frames.
        assert_eq!(ticks_to_frames(1, (24, 1), 48_000), 2_000);
    }

    #[test]
    fn resolve_spans_rebases_volume_envelope_to_frames() {
        use cutlass_models::{Easing, Keyframe};
        // A 0→1 ramp over clip ticks [0, 24] (1s at 24fps) must rebase to
        // sample frames [0, 48000] so the mixer's per-frame lookup matches.
        let snapshot = AudioSnapshot {
            fps: (24, 1),
            spans: vec![AudioSpan {
                path: PathBuf::from("/tmp/x.mp3"),
                start_tick: 0,
                end_tick: 24,
                source_start: 0,
                source_rate: (24, 1),
                source_duration: 0,
                retimed: false,
                reversed: false,
                pitch_factor: 1.0,
                speed_curve: None,
                volume: Param::Keyframed {
                    keyframes: vec![
                        Keyframe {
                            tick: 0,
                            value: 0.0,
                            easing: Easing::Linear,
                        },
                        Keyframe {
                            tick: 24,
                            value: 1.0,
                            easing: Easing::Linear,
                        },
                    ],
                },
                fade_in_ticks: 0,
                fade_out_ticks: 0,
                denoise: false,
            }],
        };
        let spans = resolve_spans(&snapshot, 48_000);
        let kfs = spans[0].volume.keyframes();
        assert_eq!(kfs.len(), 2);
        assert_eq!((kfs[0].tick, kfs[1].tick), (0, 48_000));
        // Halfway through (frame 24000) the ramp reads 0.5.
        assert_eq!(spans[0].volume.sample(24_000), 0.5);
    }

    #[test]
    fn tick_frame_conversion_handles_ntsc() {
        // 30000 ticks at 30000/1001 fps = 1001 seconds = 1001 · 48000 frames.
        assert_eq!(
            ticks_to_frames(30_000, (30_000, 1_001), 48_000),
            1_001 * 48_000
        );
        assert_eq!(
            frames_to_ticks(1_001 * 48_000, (30_000, 1_001), 48_000),
            30_000
        );
    }

    #[test]
    fn invalid_rates_collapse_to_zero() {
        assert_eq!(ticks_to_frames(100, (0, 1), 48_000), 0);
        assert_eq!(ticks_to_frames(100, (24, 1), 0), 0);
        assert_eq!(frames_to_ticks(100, (24, 0), 48_000), 0);
    }

    #[test]
    fn clock_reports_anchor_plus_consumed_frames() {
        let handle = AudioHandle {
            shared: Arc::new(AudioShared::new()),
            tx: None,
            sample_rate: 48_000,
        };
        handle.play(100);
        assert_eq!(handle.current_tick(24, 1), 100);
        // 1.5s of consumed audio = 36 ticks at 24fps.
        handle.shared.frames_played.store(72_000, Ordering::Release);
        assert_eq!(handle.current_tick(24, 1), 136);
    }

    #[test]
    fn scrub_burst_plays_without_advancing_the_clock() {
        // A scrub-tagged block is audible through the callback, but a scrub
        // never advances `frames_played` (the drag drives the playhead, not
        // the audio clock) — M8 Phase 7.
        let shared = AudioShared::new();
        shared.scrubbing.store(true, Ordering::Release);
        let (tx, rx) = bounded::<AudioBlock>(BLOCK_CAPACITY);
        tx.send(AudioBlock {
            epoch: 0, // matches the fresh AudioShared epoch
            samples: vec![0.5; BLOCK_FRAMES * AUDIO_CHANNELS],
        })
        .unwrap();

        let mut sink = CallbackSink::new(rx);
        let mut out = vec![0f32; 256 * 2];
        sink.fill(&mut out, 2, &shared);
        assert!(out.iter().any(|&s| s != 0.0), "scrub burst is audible");
        assert_eq!(
            shared.frames_played.load(Ordering::Acquire),
            0,
            "a scrub burst must not move the master clock"
        );
    }

    #[test]
    fn mixer_thread_emits_a_finite_scrub_burst_then_idles() {
        let Some(path) = audio_asset() else {
            return;
        };
        const RATE: u32 = 48_000;
        let shared = Arc::new(AudioShared::new());
        let (msg_tx, msg_rx) = unbounded::<AudioMsg>();
        let (block_tx, block_rx) = bounded::<AudioBlock>(BLOCK_CAPACITY);
        let mixer_shared = Arc::clone(&shared);
        let mixer = std::thread::spawn(move || mixer_loop(msg_rx, block_tx, mixer_shared, RATE));

        // A clip over the scrub position so the burst carries real audio.
        msg_tx
            .send(AudioMsg::Snapshot(AudioSnapshot {
                fps: (24, 1),
                spans: vec![AudioSpan {
                    path,
                    start_tick: 0,
                    end_tick: 48,
                    source_start: 0,
                    source_rate: (24, 1),
                    source_duration: 0,
                    retimed: false,
                    reversed: false,
                    pitch_factor: 1.0,
                    speed_curve: None,
                    volume: Param::Constant(1.0),
                    fade_in_ticks: 0,
                    fade_out_ticks: 0,
                    denoise: false,
                }],
            }))
            .unwrap();
        msg_tx.send(AudioMsg::Scrub { tick: 0, epoch: 7 }).unwrap();

        // Exactly SCRUB_BURST_BLOCKS blocks, all tagged with the scrub epoch.
        let mut heard = false;
        for _ in 0..SCRUB_BURST_BLOCKS {
            let block = block_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("scrub burst produces");
            assert_eq!(block.epoch, 7);
            heard |= block.samples.iter().any(|&s| s != 0.0);
        }
        assert!(heard, "scrub burst is audible over the clip");

        // Then the mixer idles: no further blocks without a new message.
        assert!(
            block_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "the burst is finite — the mixer stops producing after it"
        );

        drop(msg_tx);
        while block_rx.try_recv().is_ok() {}
        mixer.join().expect("mixer thread exits cleanly");
    }

    #[test]
    fn mix_block_sums_and_clamps_silence_outside_spans() {
        // No spans: block stays silent.
        let mut readers = HashMap::new();
        let mut failed = HashMap::new();
        let mut rendered = HashMap::new();
        let mut out = vec![0.5f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        mix_block(
            &[],
            &mut readers,
            &mut failed,
            &mut rendered,
            0,
            48_000,
            &mut out,
        );
        assert!(out.iter().all(|&s| s == 0.5), "no spans leave input alone");
    }

    fn audio_asset() -> Option<PathBuf> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local-assets/assets");
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| {
                p.extension().is_some_and(|e| e == "mp3")
                    || (p.extension().is_some_and(|e| e == "mp4")
                        && AudioReader::open(p, 48_000).is_ok())
            })
    }

    #[test]
    fn mixer_renders_a_span_and_silence_around_it() {
        let Some(path) = audio_asset() else {
            return;
        };
        const RATE: u32 = 48_000;
        // One clip: timeline ticks [24, 48) at 24fps = seconds [1, 2),
        // playing the source from its start.
        let snapshot = AudioSnapshot {
            fps: (24, 1),
            spans: vec![AudioSpan {
                path,
                start_tick: 24,
                end_tick: 48,
                source_start: 0,
                source_rate: (24, 1),
                source_duration: 0,
                retimed: false,
                reversed: false,
                pitch_factor: 1.0,
                speed_curve: None,
                volume: Param::Constant(1.0),
                fade_in_ticks: 0,
                fade_out_ticks: 0,
                denoise: false,
            }],
        };
        let spans = resolve_spans(&snapshot, RATE);
        assert_eq!(spans[0].start_frame, i64::from(RATE));
        assert_eq!(spans[0].end_frame, 2 * i64::from(RATE));

        let mut readers = HashMap::new();
        let mut failed = HashMap::new();
        let mut rendered = HashMap::new();

        // Block fully before the clip: silence.
        let mut before = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            0,
            RATE,
            &mut before,
        );
        assert!(before.iter().all(|&s| s == 0.0), "silence before the clip");

        // Block inside the clip: real audio.
        let mut inside = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            i64::from(RATE) + 4 * BLOCK_FRAMES as i64,
            RATE,
            &mut inside,
        );
        assert!(inside.iter().any(|&s| s != 0.0), "audible inside the clip");
        assert!(
            inside.iter().all(|&s| (-1.0..=1.0).contains(&s)),
            "clamped to [-1, 1]"
        );

        // Block straddling the clip start: leading samples stay silent.
        let straddle_pos = i64::from(RATE) - (BLOCK_FRAMES / 2) as i64;
        let mut straddle = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            straddle_pos,
            RATE,
            &mut straddle,
        );
        let lead = (BLOCK_FRAMES / 2) * AUDIO_CHANNELS;
        assert!(
            straddle[..lead].iter().all(|&s| s == 0.0),
            "silence before the in-point inside a straddling block"
        );
    }

    #[test]
    fn mix_block_applies_volume_and_fade_gain() {
        let Some(path) = audio_asset() else {
            return;
        };
        const RATE: u32 = 48_000;
        let span_at = |volume: f32, fade_in_ticks: i64| AudioSnapshot {
            fps: (24, 1),
            spans: vec![AudioSpan {
                path: path.clone(),
                start_tick: 0,
                end_tick: 48,
                source_start: 0,
                source_rate: (24, 1),
                source_duration: 0,
                retimed: false,
                reversed: false,
                pitch_factor: 1.0,
                speed_curve: None,
                volume: Param::Constant(volume),
                fade_in_ticks,
                fade_out_ticks: 0,
                denoise: false,
            }],
        };
        // Mix the same block at full and half volume: every sample halves.
        let mut readers = HashMap::new();
        let mut failed = HashMap::new();
        let mut rendered = HashMap::new();
        let pos = 8 * BLOCK_FRAMES as i64;
        let mut full = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        let spans = resolve_spans(&span_at(1.0, 0), RATE);
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            pos,
            RATE,
            &mut full,
        );
        assert!(full.iter().any(|&s| s != 0.0), "fixture block is audible");

        let mut half = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        let spans = resolve_spans(&span_at(0.5, 0), RATE);
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            pos,
            RATE,
            &mut half,
        );
        for (f, h) in full.iter().zip(&half) {
            assert!((f * 0.5 - h).abs() < 1e-4, "half volume halves samples");
        }

        // A fade-in covering the whole clip silences its very first sample
        // and leaves the block quieter than the flat mix.
        let mut faded = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        let spans = resolve_spans(&span_at(1.0, 48), RATE);
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            0,
            RATE,
            &mut faded,
        );
        assert_eq!(faded[0], 0.0, "fade-in starts from silence");
        let mut flat = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        let spans = resolve_spans(&span_at(1.0, 0), RATE);
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            0,
            RATE,
            &mut flat,
        );
        let energy = |b: &[f32]| b.iter().map(|s| f64::from(s * s)).sum::<f64>();
        if energy(&flat) > 0.0 {
            assert!(energy(&faded) < energy(&flat), "ramp lowers block energy");
        }
    }

    #[test]
    fn mixer_time_stretches_a_retimed_span() {
        let Some(path) = audio_asset() else {
            return;
        };
        const RATE: u32 = 48_000;
        // A 2× clip: 2s of source (ticks [0, 48) at 24fps) plays over a 1s
        // timeline span (ticks [0, 24)). The mixer time-stretches it instead
        // of muting (M8 Phase 3).
        let snapshot = AudioSnapshot {
            fps: (24, 1),
            spans: vec![AudioSpan {
                path,
                start_tick: 0,
                end_tick: 24,
                source_start: 0,
                source_rate: (24, 1),
                source_duration: 48,
                retimed: true,
                reversed: false,
                pitch_factor: 1.0,
                speed_curve: None,
                volume: Param::Constant(1.0),
                fade_in_ticks: 0,
                fade_out_ticks: 0,
                denoise: false,
            }],
        };
        let spans = resolve_spans(&snapshot, RATE);
        assert!(
            spans[0].render.is_some(),
            "a retimed span carries a render key"
        );

        let mut readers = HashMap::new();
        let mut failed = HashMap::new();
        let mut rendered = HashMap::new();
        let mut inside = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            4 * BLOCK_FRAMES as i64,
            RATE,
            &mut inside,
        );
        assert_eq!(
            rendered.len(),
            1,
            "the stretched buffer is rendered + cached"
        );
        assert!(inside.iter().any(|&s| s != 0.0), "retimed audio plays");
        assert!(
            inside.iter().all(|&s| (-1.0..=1.0).contains(&s)),
            "clamped to [-1, 1]"
        );
    }

    #[test]
    fn mixer_denoises_a_non_retimed_span() {
        let Some(path) = audio_asset() else {
            return;
        };
        const RATE: u32 = 48_000;
        // A 1× clip with noise reduction on: the mixer renders + caches a
        // cleaned buffer and serves it 1:1 (M8 Phase 5), not a retime.
        let snapshot = AudioSnapshot {
            fps: (24, 1),
            spans: vec![AudioSpan {
                path,
                start_tick: 0,
                end_tick: 24,
                source_start: 0,
                source_rate: (24, 1),
                source_duration: 0,
                retimed: false,
                reversed: false,
                pitch_factor: 1.0,
                speed_curve: None,
                volume: Param::Constant(1.0),
                fade_in_ticks: 0,
                fade_out_ticks: 0,
                denoise: true,
            }],
        };
        let spans = resolve_spans(&snapshot, RATE);
        let key = spans[0]
            .render
            .as_ref()
            .expect("a denoised span carries a render key");
        assert!(!spans[0].retimed, "denoise-only is not a retime");
        assert!(key.denoise, "the render identity carries the denoise flag");

        let mut readers = HashMap::new();
        let mut failed = HashMap::new();
        let mut rendered = HashMap::new();
        let mut inside = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            4 * BLOCK_FRAMES as i64,
            RATE,
            &mut inside,
        );
        assert_eq!(
            rendered.len(),
            1,
            "the denoised buffer is rendered + cached"
        );
        assert!(
            inside.iter().all(|&s| (-1.0..=1.0).contains(&s)),
            "clamped to [-1, 1]"
        );
    }

    #[test]
    fn mixer_time_stretches_a_speed_ramp_span() {
        use cutlass_models::{Easing, Keyframe, SPEED_CURVE_SCALE};
        let Some(path) = audio_asset() else {
            return;
        };
        const RATE: u32 = 48_000;
        // A ramped clip (M2): 2s of source (ticks [0, 48)) over a 1s span
        // (ticks [0, 24)), rate ramping 1→3 (average 2×). The mixer renders it
        // variable-rate instead of muting (M8 Phase 3).
        let snapshot = AudioSnapshot {
            fps: (24, 1),
            spans: vec![AudioSpan {
                path,
                start_tick: 0,
                end_tick: 24,
                source_start: 0,
                source_rate: (24, 1),
                source_duration: 48,
                retimed: true,
                reversed: false,
                pitch_factor: 1.0,
                speed_curve: Some(Param::Keyframed {
                    keyframes: vec![
                        Keyframe {
                            tick: 0,
                            value: 1.0,
                            easing: Easing::Linear,
                        },
                        Keyframe {
                            tick: SPEED_CURVE_SCALE,
                            value: 3.0,
                            easing: Easing::Linear,
                        },
                    ],
                }),
                volume: Param::Constant(1.0),
                fade_in_ticks: 0,
                fade_out_ticks: 0,
                denoise: false,
            }],
        };
        let spans = resolve_spans(&snapshot, RATE);
        let key = spans[0]
            .render
            .as_ref()
            .expect("a ramp carries a render key");
        assert!(
            !key.curve.is_empty(),
            "the ramp identity is part of the cache key"
        );

        let mut readers = HashMap::new();
        let mut failed = HashMap::new();
        let mut rendered = HashMap::new();
        let mut inside = vec![0f32; BLOCK_FRAMES * AUDIO_CHANNELS];
        mix_block(
            &spans,
            &mut readers,
            &mut failed,
            &mut rendered,
            4 * BLOCK_FRAMES as i64,
            RATE,
            &mut inside,
        );
        assert_eq!(
            rendered.len(),
            1,
            "the variable-rate buffer is rendered + cached"
        );
        assert!(inside.iter().any(|&s| s != 0.0), "ramped audio plays");
        assert!(
            inside.iter().all(|&s| (-1.0..=1.0).contains(&s)),
            "clamped to [-1, 1]"
        );
    }

    /// End-to-end mixer thread, no device: snapshot + play produce blocks
    /// whose epoch tags and silence/audio boundaries match the timeline;
    /// a re-anchored play (seek) switches epochs so stale blocks are
    /// identifiable for the callback's lock-free flush.
    #[test]
    fn mixer_thread_produces_tagged_blocks_and_reanchors() {
        let Some(path) = audio_asset() else {
            return;
        };
        const RATE: u32 = 48_000;
        let shared = Arc::new(AudioShared::new());
        let (msg_tx, msg_rx) = unbounded::<AudioMsg>();
        let (block_tx, block_rx) = bounded::<AudioBlock>(BLOCK_CAPACITY);
        let mixer_shared = Arc::clone(&shared);
        let mixer = std::thread::spawn(move || mixer_loop(msg_rx, block_tx, mixer_shared, RATE));

        // Clip covers timeline seconds [0, 2), source from 0.
        msg_tx
            .send(AudioMsg::Snapshot(AudioSnapshot {
                fps: (24, 1),
                spans: vec![AudioSpan {
                    path,
                    start_tick: 0,
                    end_tick: 48,
                    source_start: 0,
                    source_rate: (24, 1),
                    source_duration: 0,
                    retimed: false,
                    reversed: false,
                    pitch_factor: 1.0,
                    speed_curve: None,
                    volume: Param::Constant(1.0),
                    fade_in_ticks: 0,
                    fade_out_ticks: 0,
                    denoise: false,
                }],
            }))
            .unwrap();
        msg_tx.send(AudioMsg::Play { tick: 0, epoch: 1 }).unwrap();

        // Drain one second of blocks: all epoch 1, audible.
        let mut heard = false;
        for _ in 0..(RATE as usize / BLOCK_FRAMES) {
            let block = block_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("mixer produces");
            assert_eq!(block.epoch, 1);
            heard |= block.samples.iter().any(|&s| s != 0.0);
        }
        assert!(heard, "the first second is audible");

        // Seek past the clip (tick 96 = 4s): fresh epoch, silent blocks.
        msg_tx.send(AudioMsg::Play { tick: 96, epoch: 2 }).unwrap();
        let mut saw_epoch2 = None;
        for _ in 0..(2 * BLOCK_CAPACITY + 4) {
            let block = block_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("mixer keeps producing");
            if block.epoch == 2 {
                saw_epoch2 = Some(block);
                break;
            }
        }
        let block = saw_epoch2.expect("epoch flips after the seek");
        assert!(
            block.samples.iter().all(|&s| s == 0.0),
            "past the clip the mixer paces with silence"
        );

        drop(msg_tx); // disconnect ⇒ mixer exits
        // Unblock a mixer stuck on a full block channel.
        while block_rx.try_recv().is_ok() {}
        mixer.join().expect("mixer thread exits cleanly");
    }
}
