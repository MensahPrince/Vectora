# Cutlass

**Cutlass** is an open-source video editor where you edit by describing what you want. Tell it to trim the intro, cut a section, or tighten a clip — and it does the work on your timeline.

It's built for everyday editing: cuts, trims, and the basics you actually use. Think of the speed and simplicity of apps like CapCut, with an assistant that understands plain language instead of making you dig through menus for every change.

Cutlass is still in early development. The sections below describe what runs **today** versus where it's headed, so there are no surprises.

## Status

This is an early-stage project. The headless editing core is real and tested, a desktop UI drives it, and the first cut of the natural-language agent now ships — a chat panel that turns prompts into validated, undoable timeline edits through the same command layer the UI uses.

**Works today**

- A Rust workspace with a tested project/timeline model and a headless editing engine: a closed set of deterministic, undo/redo-able edit commands (add/split/trim/move/remove clips, ripple ops, linking, track flags, transforms, keyframes, speed, audio). Every UI gesture and every agent edit goes through it.
- **AI agent panel**: describe an edit ("cut the first 3 seconds, add a title that says INTRO") and the agent emits commands that are validated, applied as one undoable history entry per prompt, and listed in the transcript; dry-run preview and read-only Q&A included. Works against any OpenAI-compatible endpoint — local (Ollama, llama.cpp-server) or cloud — configured in `~/.cutlass/config.toml`.
- **Project lifecycle**: New / Open / Save / Save As / Recent, dirty-state tracking, autosave with crash recovery, and a relink dialog when a project opens with missing media files.
- **Media**: video + audio import via FFmpeg (hardware-accelerated decode where available) and PNG/JPEG/WebP stills, in a library panel with thumbnails.
- **Multi-lane timeline editing**: snap, main-track magnet, linked A/V, trim (ripple trim on the magnet lane included), split, ripple-delete, multi-select, group drag/copy/duplicate, link/unlink, undo/redo — with filmstrips, waveforms, ruler markers, and speed/volume badges on clips.
- **Clip speed**: any constant rate (0.05×–100×) plus reverse, honored by preview, export, trim, and split. Audio on retimed clips is muted until varispeed lands.
- **Clip audio**: volume + fade in/out per clip, with sample-accurate ramps in both playback and export.
- **Keyframes** on clip transforms (position / scale / rotation / opacity) with easing curves; CapCut-style diamond UI in the inspector and draggable keyframe markers on timeline clips. Preview and export sample the same curves.
- **Styled titles**: text clips with font, size, color, stroke, shadow, background, spacing, case, and alignment controls.
- **Live GPU preview** (WGPU compositor) with real-time playback, audio sync, JKL transport, fullscreen mode, and on-canvas move/scale/rotate gestures.
- **Export** to H.264/AAC MP4 with resolution, frame-rate, and quality presets.
- An on-disk **decoded-frame cache** keeps scrubbing and cold seeks fast. (A proxy transcoder exists in `cutlass-encoder` but is **not yet wired into preview** — see the roadmap.)
- A small end-to-end CLI (`cutlass-app`) that imports a clip, saves a `.cutlass` project, and exports an MP4 — a smoke test for the whole pipeline.

**Not shipped yet**

- Crop / flip, canvas and aspect presets, speed curves.
- The look stack: effects, transitions, filters, color correction / LUTs, masks, chroma key, blend modes.
- The audio suite: volume envelopes, ducking, noise reduction, varispeed.
- AI media tools: auto captions, transcript-based editing, silence removal, TTS, background removal.
- Windows builds; signed/notarized macOS builds.

Several of these are in progress — [docs/v1-roadmap.md](docs/v1-roadmap.md) tracks the milestone plan and the current tick state.

## Architecture

The codebase is a Cargo workspace split into focused crates:

| Crate | Responsibility |
| --- | --- |
| `cutlass-models` | Project, media pool, timeline, track, and clip data model with edit invariants; `Param<T>` keyframe curves; project file schema. |
| `cutlass-commands` | The closed, serializable edit-command vocabulary — the layer both the UI and the AI agent drive. |
| `cutlass-engine` | Headless editing engine: executes commands with inverse-based undo/redo, resolves timeline frames to composited images, exports, owns the frame cache. |
| `cutlass-compositor` | WGPU frame compositor (multi-layer alpha-over, GPU YUV conversion, RGBA readback). |
| `cutlass-decoder` | FFmpeg demux + decode (hardware-accelerated where available), keyframe indexing, image stills, audio peaks + clocked playback streaming. |
| `cutlass-encoder` | H.264/AAC MP4 export encode + mux; all-intra proxy transcoder (built and tested, not yet consumed by preview). |
| `cutlass-probe` | Media probing: container / codec / stream metadata at import time. |
| `cutlass-cache` | On-disk decoded-frame cache. |
| `cutlass-ai` | AI agent: LLM-facing wire vocabulary + generated tool schemas, validation against the project, provider abstraction (OpenAI-compatible), agent loop. |
| `cutlass-ui` | Slint desktop shell: timeline, preview, inspector, library, agent panel, transport, export dialog. |
| `cutlass-app` | End-to-end session CLI: import → edit → save project → export MP4 under `.cutlass/`. |

## Benchmarks

Criterion benches for the compositor GPU path and engine preview/export (local only; not run in CI):

```bash
# GPU compositor (solid / RGBA / two-layer stack @ 1080p)
cargo bench -p cutlass-compositor --bench composite

# get_frame: solid clip always; media clip when assets/*.mp4 or CUTLASS_BENCH_ASSET is set
cargo bench -p cutlass-engine --bench preview

# Full export: 48-frame solid timeline → MP4
cargo bench -p cutlass-engine --bench export
```

HTML reports land in `target/criterion/`. See [docs/benchmarks.md](docs/benchmarks.md) for case descriptions, env vars, and how to interpret cold/warm preview numbers.

## Prerequisites

- A recent stable **Rust** toolchain (edition 2024; Rust 1.85 or newer).
- **FFmpeg** development libraries, required by the `ffmpeg-next` bindings.

Install FFmpeg:

```bash
# macOS (Homebrew)
brew install ffmpeg pkg-config

# Debian / Ubuntu
sudo apt-get install -y pkg-config clang \
  libavcodec-dev libavformat-dev libavutil-dev \
  libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev
```

## Releases

Prebuilt **alpha** builds are published on [GitHub Releases](https://github.com/1Mr-Newton/cutlass/releases) when tagged (`alpha-0.1.0`, …).

| Platform | Install |
| --- | --- |
| **macOS** (Apple Silicon) | Download `Cutlass-*-macos-arm64.zip`, unzip, drag `Cutlass.app` to Applications. **First launch:** right-click `Cutlass.app` → **Open** (alpha builds are not notarized). See `INSTALL-macos.txt` in the zip. FFmpeg is bundled. |
| **Linux** (x86_64) | Download `Cutlass-*-linux-x86_64.tar.gz`, extract, run `./cutlass-ui`. Install FFmpeg dev/runtime libs first (see `README-INSTALL.txt` in the archive). |

Maintainers: see [packaging/README.md](packaging/README.md) for local build scripts and the release workflow.

## Build & run

```bash
# Build everything
cargo build --workspace

# Run the tests
cargo test --workspace
```

### Desktop editor

The `cutlass-ui` shell opens the full editor: library, multi-lane timeline, live preview, inspector, agent panel, and export dialog:

```bash
# Open the editor (use the Import button to add media)
cargo run -p cutlass-ui

# …or open with a video already loaded
cargo run -p cutlass-ui -- path/to/video.mp4
```

To enable the AI panel, point it at any OpenAI-compatible endpoint in `~/.cutlass/config.toml` (keys never live in project files):

```toml
[ai]
base_url = "http://localhost:11434/v1"   # e.g. Ollama
model = "qwen3:14b"
# api_key = "sk-..."                # literal key, or:
# api_key_env = "OPENAI_API_KEY"    # read from the environment
```

### Session CLI (`cutlass-app`)

End-to-end engine smoke test: import a clip, preview one frame, save a `.cutlass` project, and export an MP4 under `.cutlass/`:

```bash
# First MP4 in assets/, writes .cutlass/projects/demo.cutlass + .cutlass/exports/demo.mp4
cargo run -p cutlass-app

# Specific source and session name
cargo run -p cutlass-app -- assets/foo.mp4 --name foo_edit
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

### Third-party dependencies

The MIT/Apache-2.0 dual license above covers Cutlass's own source. Cutlass builds on third-party components that are distributed under their **own** licenses, and those terms continue to apply to the parts they cover:

- **FFmpeg**, used via the [`ffmpeg-next`](https://crates.io/crates/ffmpeg-next) bindings, is licensed under the **LGPL-2.1-or-later** by default, and can fall under the **GPL** depending on how the FFmpeg libraries you link against were configured (e.g. with GPL-only components enabled). If you distribute builds that link FFmpeg, you are responsible for complying with its license — review the licensing terms of the specific FFmpeg build you ship. See the [FFmpeg legal page](https://www.ffmpeg.org/legal.html).
- The Rust crate dependencies (such as `ffmpeg-next`, `rustc-hash`, `thiserror`, `tracing`, `png`, and others) are each distributed under their own licenses (commonly MIT and/or Apache-2.0). Run `cargo tree` to see the full dependency graph, and consult each crate for its exact terms.

Cutlass does not bundle FFmpeg; it links against the FFmpeg development libraries you install separately (see [Prerequisites](#prerequisites)).
