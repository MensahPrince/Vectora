# Cutlass

**Cutlass** is an open-source video editor where you edit by describing what you want. Tell it to trim the intro, cut a section, or tighten a clip — and it does the work on your timeline.

It's built for everyday editing: cuts, trims, and the basics you actually use. Think of the speed and simplicity of apps like CapCut, with an assistant that understands plain language instead of making you dig through menus for every change.

Cutlass is still in early development. The sections below describe what runs **today** versus where it's headed, so there are no surprises.

## Status

This is an early-stage project. The headless editing core is real and tested, and a basic desktop UI now drives it; the natural-language agent is not built yet.

**Works today**

- A Rust workspace with a tested project/timeline model and a headless editing engine.
- Media decode via FFmpeg, with hardware-accelerated decode where available.
- A closed set of deterministic, undo/redo-able edit commands: add clip, add generated clip, split, trim, move, remove, and ripple-delete.
- Frame resolution through the engine: timeline frame → ordered layers → composited image.
- A WGPU compositor and an on-disk proxy/transcode cache to keep cold seeks fast.
- A desktop editor shell (`cutlass-ui`, built on [Slint](https://slint.dev/)): import media, multi-lane timeline editing (snap, magnet, linkage, trim, split, undo/redo), live GPU preview with real-time playback and audio sync, and export to H.264/AAC MP4.
- A small end-to-end CLI (`cutlass-app`) that imports a clip, saves a `.cutlass` project, and exports an MP4 — a smoke test for the whole pipeline.

**Not built yet (the goal)**

- The natural-language agent that turns a prompt into edit commands. The command layer it will drive already exists.

## Architecture

The codebase is a Cargo workspace split into focused crates:

| Crate | Responsibility |
| --- | --- |
| `cutlass-models` | Project, timeline, track, and clip data model with edit invariants. |
| `cutlass-decoder` | FFmpeg demux + decode, hardware acceleration, keyframe indexing, proxy encode. |
| `cutlass-compositor` | WGPU frame compositor (multi-layer alpha-over, RGBA readback). |
| `cutlass-engine` | Headless editing engine: edit commands + undo/redo, WGPU preview, timeline export, frame cache. |
| `cutlass-ui` | Slint desktop shell: preview, scrub/playback, timeline editing, undo/redo, proxy progress. |
| `cutlass-app` | End-to-end session CLI: import → save project → export MP4 under `.cutlass/`. |

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

The `cutlass-ui` shell opens a window where you can import a video, scrub and play a live preview, drag clips on the timeline, split/delete/ripple-delete, and undo/redo:

```bash
# Open the editor (use the Import button to add a video)
cargo run -p cutlass-ui

# …or open with a video already loaded
cargo run -p cutlass-ui -- path/to/video.mp4
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
