# Audio suite roadmap — M8

Sound that doesn't need a DAW round-trip. This is the feature-area plan for
`v1-roadmap.md` § M8. The order is dependency-first: volume envelopes land
on the proven M2 `Param` plumbing (and unblock ducking, which is just
volume keyframes written by analysis), then the DSP-heavy pieces
(varispeed, denoise, beat detection) follow.

Policy reminder: **we follow CapCut.** The volume line + points, the
fade corner handles, varispeed with pitch lock, sidechain ducking, and
beat markers all mirror CapCut desktop's audio panel.

## Status legend

- [x] shipped
- [ ] not started / in progress

---

## Design (locked in Phase 1)

- **`volume` is a `Param<f32>` envelope**, not a bare gain
  (`cutlass-models/src/clip.rs`). One animation type, reused from M2:
  keyframe ticks are clip-relative timeline ticks, sampled with the same
  eased-lerp math as transforms. A constant envelope is the common case
  and serializes as the bare value (`"volume": 0.8`) — byte-identical to
  the pre-M8 shape, so old projects load unchanged and never-animated saves
  keep the old form. A keyframed envelope serializes as `{"kf":[...]}`.
- **Both mixers sample the envelope per sample-frame.** The shared
  `audio_gain_at(pos, len, &Param<f32>, fade_in, fade_out)` takes the
  envelope and multiplies the fades on top. Each mixer rebases the
  clip-relative *tick* keyframes into clip-relative *sample frames* once per
  span (`Param::map_ticks`), so the hot per-sample lookup stays an O(log k)
  tick compare, never a tick→frame conversion. The unity fast path
  (constant 1.0, no fades) still bypasses the gain loop entirely.
- **`set_clip_audio` sets a flat level** (CapCut's basic volume slider):
  it writes `Param::Constant(volume)`, flattening any envelope. Envelopes
  are drawn through the M2 keyframe commands routed to the new
  `ClipParam::Volume`, so ducking and the agent reuse the existing
  `SetParamKeyframe` / `RemoveParamKeyframe` / `SetParamConstant`
  vocabulary — no new command shapes, no new safety surface.
- **`ClipParam::Volume` is an audio property**: it bypasses the visual
  `check_param_target` (audio clips have no canvas placement to animate) and
  takes an audio-capable target check instead (media-backed; volume rides
  any media clip, since linkage lands the audible half on an audio lane).
  Values are validated in `0..=MAX_CLIP_VOLUME` per keyframe, finite.
- **A keyframed envelope is never "silent."** `Clip::is_silent` is true
  only for a constant gain of `0`; an envelope is kept by both mixers (it
  may be non-zero elsewhere) and sampled. Every retimed clip — constant
  speed, reverse, and speed ramps — now plays time-stretched audio (Phase 3);
  only a constant-zero gain mutes.

---

## Phase 1 — Volume envelopes (the keystone)

- [x] **Model**: `Clip.volume: Param<f32>`; serde backward-compat
      (constant ⇔ bare value, keyframed ⇔ `{"kf":..}`); `Param::map_ticks`;
      envelope-aware `audio_gain_at`; `validate_volume` /
      `validate_volume_envelope`; `has_volume_envelope` / `is_silent`
      helpers; split rides the envelope on both halves.
- [x] **Engine / commands**: `ClipParam::Volume` routed at the project
      level to the clip's envelope with an audio-target check; the M2
      `SetParamKeyframe` / `RemoveParamKeyframe` / `SetParamConstant`
      actions drive it with their existing clip-snapshot inverses.
- [x] **Mixers**: the realtime mixer (`cutlass-ui/src/audio.rs`) and the
      export mixer (`cutlass-engine/src/export_audio.rs`) both carry the
      envelope on their span, rebase it to the sample-frame domain, and
      sample per frame; preview and export agree.
- [x] **Agent vocabulary**: `volume` joins `WireClipParam` so the agent can
      write volume keyframes ("fade the music down under the voice"); wire
      DTO + validation + action-log phrasing + schema snapshot bump (v12) +
      eval.
- [x] **Inspector envelope UI**: a keyframe diamond on the Volume row (the
      M2 cluster) — `sample-audio` reads the envelope at the playhead, the
      diamond adds/removes a keyframe, and the slider sculpts the keyframe on
      an animated clip or sets the flat level on a constant one. Projection
      publishes `kf-volume` (absolute-tick keyframes, the transform pattern)
      and a normalized `volume-path` curve.
- [x] **On-clip envelope line**: the gain curve drawn over the waveform
      (densely sampled `volume-path`, easing included) with a dot at each
      keyframe. The dots are markers today; on-clip drag editing rides a
      later slice — editing is the inspector diamond for now.
- [x] **Timeline badge**: an envelope chip marks a keyframed clip; it
      supersedes the M1 muted / volume% / fade chips while the gain is
      animated.

## Phase 2 — Fades as corner handles

- [x] **Envelope-preserving fades**: `SetClipAudio.volume` is now
      `Option<f32>` — `Some` sets a flat level (the basic slider, flattening
      an envelope), `None` keeps the gain (constant or keyframed) and only
      moves the fades. The inspector fade rows route through a `set-clip-fades`
      worker path (volume `None`), so they're visible again on enveloped
      clips and never wipe automation; the agent's omitted volume lowers to
      `None` too, so "fade the music out" past a keyframed clip is safe.
- [x] **Corner handles**: drag the top corners of a selected audio clip to set
      fade-in (left) / fade-out (right) durations — a darkening triangle with
      a bright edge line per ramp and a grab dot riding the corner. Maps px to
      seconds against the card width, committing one envelope-preserving
      `set-clip-fades` on release; declared after the trim handles so a corner
      grab fades rather than trims.

## Phase 3 — Varispeed audio

- [x] **Backend (decided)**: `signalsmith-stretch` — MIT, header-only C++
      via a maintained Rust wrapper, chosen over rubberband (GPL) and a
      vendored phase-vocoder for license fit and quality. Lives in
      `cutlass-decoder` as an *offline per-span render* (`render_stretched`):
      the whole retimed span is stretched into a buffer once (lazily, then
      cached on a `RenderKey`) and served 1:1, so preview and export use
      identical samples and reverse is a buffered flip rather than a streamed
      special case.
- [x] **Constant-speed + reverse audio (M1)**: both mixers drop the
      `is_retimed()` mute for constant-rate and reversed clips, render the
      span through `render_stretched`, and play it. `Clip.preserve_pitch`
      (serde-default true, back-compat) drives the transpose: pitch-locked
      time-stretch by default, pitch-follows-speed ("chipmunk") when off.
- [x] **Pitch toggle**: `set_clip_pitch` model setter + `SetClipPitch`
      command + engine action (clip-snapshot inverse) + a "Keep pitch" switch
      in the Speed inspector (flips the whole link group when linkage is on,
      so an A/V pair stays consistent). Replaces the old "audio is muted while
      retimed" caption.
- [x] **Speed-curve audio (M2)**: ramps play too. `render_stretched_curve`
      generalizes the offline render to a *time-varying* rate — one continuous
      phase-vocoder pass that reuses `exact`'s latency compensation but feeds
      the interior in blocks whose ratio tracks the ramp. Both mixers warp the
      sound along `speed_curve_source_fraction` (the same normalized integral
      `source_time_at` uses for the picture), so audio and video stay in step
      and preview matches export. The agent vocabulary and inspector captions
      no longer call ramp audio muted.

## Phase 4 — Audio ducking

- [ ] Sidechain: detect speech-band energy on chosen "voice" lanes,
      auto-keyframe music lanes down (attack / release / threshold / amount).
      Writes **ordinary volume keyframes** (Phase 1 envelope) so ducking is
      inspectable and editable after the fact. One undoable history group.

## Phase 5 — Noise reduction

- [ ] rnnoise-class denoise as an audio effect on clips (offline render
      into the mixer path, cached like a proxy). **Open decision**: rnnoise
      binding vs ONNX model.

## Phase 6 — Beat detection & snap

- [ ] Onset analysis (local DSP) → beat markers on audio clips; "snap to
      beats" in the timeline magnet; the substrate for agent/M9 beat-sync.
      **Open decision**: aubio-style onset detector vs hand-rolled
      spectral-flux.

## Phase 7 — Audio scrub bursts

- [ ] Short audio bursts while dragging the playhead (the reserved
      `AudioReader` seam from `playback-roadmap.md` Phase 4).

## Phase 8 — MP3 frame-exact seek index

- [ ] Lazily-built MP3 seek index to kill the known tens-of-ms seek offset
      (`decoder` debt called out in the v1 roadmap).

---

## Exit

Music ducks under narration, denoised voice, beat-snapped cuts, audible
speed ramps — preview and export agree, every edit undoable.
