# Changelog

Notes for the latest release. For previous releases, see the
[GitHub releases page](https://github.com/1Mr-Newton/cutlass/releases).

## [alpha-0.6.0] — 2026-07-16

### Added

- **AI editing agent.** Describe edits in natural language and Cutlass applies
  them as structured, undo-friendly timeline commands. The agent can inspect
  footage, build timelines from an empty project, import approved media,
  duplicate clips, reorder effect chains, and run managed background jobs.
  Conversations are stored per project with chat switching and history
  controls; OpenAI Responses transport streams reasoning summaries into the
  transcript. Configure the provider (including Responses) under Settings.

- **Windows support.** Native Media Foundation decode and encode — hardware-
  accelerated H.264/AAC round-trips, GPU frame import into the compositor, and
  a full Windows desktop build path alongside macOS.

- **Mobile apps.** An iOS/macOS SwiftUI editor (timeline, preview, media
  picker, export) backed by the shared engine, plus an Android native/JNI
  foundation with on-device engine smoke tests. The Compose editor UI remains
  a follow-up.

- **Python bindings (`cutlass-py`).** A MoviePy-style track-first API over the
  engine — import media, edit tracks/clips, sample frames, and export — on
  PyPI as a pre-release (`pip install --pre cutlass-py`).

- **CapCut-style timeline lanes.** A permanent main video track with lane-zone
  rules (audio → main → overlays → text), move/rename track actions, pinned
  lanes, and timeline UX parity for zoom, markers, and transitions.

- **Stickers and look animations.** Bundled sticker assets with animated
  GIF/APNG/WebP playback, Lottie stickers from the Library, and catalog
  entrance/exit/combo animations sampled at resolve time — with an animation
  inspector on desktop.

- **Color grades, filters, masks, effects, and transitions.** Per-clip filter
  presets and manual adjustments, masks and chroma key, clip effect/transition
  chains, and lane-level effect/filter/adjustment passes — shared between
  preview and export.

- **3D LUTs.** Apply `.cube` LUT grades from the look inspector, with a
  starter pack included; preview and export share the same compositor path.

- **Export audio DSP.** Varispeed resampling for retimed clips and RNNoise
  denoise on flagged clips during export.

- **Preview proxies and smoother scrubbing.** CapCut-style background H.264
  proxies for interactive preview, plus adaptive seek windows, frame caching,
  and speculative next-tick render so scrubbing stays responsive. Full-
  quality decode still runs for export.

- **Cloud accounts and Library catalogs.** Sign in via Settings → Account
  (device authorization against cutlass.sh). Browse stock media, stickers,
  SFX, text presets, and templates; AI image/video/TTS generation surfaces
  with BYOK-or-managed routing. Drop files from the OS onto the timeline.

- **Extract audio.** Pull a video clip's embedded audio onto a linked audio-
  lane companion — undoable like any other edit.

- **Freeze frames and clip tools.** Insert atomic freeze frames; duplicate
  complete clips, unlink linked groups, and clear beats with reversible
  engine actions.

- **Media analysis and transcription.** Deterministic shot/moment indexing
  with on-disk cache, Whisper-based transcription (cancellable model install),
  and Settings controls for analysis and AI model cache locations.

### Changed

- **App-owned projects with continuous auto-save (CapCut-style).** Cutlass now
  owns every project: each one auto-saves on every edit, so there's no manual
  save and a clean exit never loses work. The launch screen is a project
  gallery — reopen or delete past projects — and the title bar renames the
  current project inline. **Open file…** imports an external `.cutlass` into
  your projects; **Export** renders an `.mp4`.

### Removed

- Manual **Save As**, **Open Recent**, and the unsaved-changes / crash-recovery
  prompts — there's nothing to lose now that edits save continuously.
- The **General** settings pane (the autosave on/off + interval controls):
  auto-save is always on and needs no tuning.

[alpha-0.6.0]: https://github.com/1Mr-Newton/cutlass/releases/tag/alpha-0.6.0
