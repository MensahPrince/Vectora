# cutlass `decoder`

CPU video **demux + decode** via [`ffmpeg-next`](https://crates.io/crates/ffmpeg-next). Passive `Decoder` type: run it on **one** engine-owned worker thread (see `../../docs/decoder/research.md`).

## Prerequisite

System FFmpeg + pkg-config (e.g. macOS: `brew install ffmpeg pkg-config`).

## Quick check

```bash
cargo run -p decoder --example hello_ffmpeg
cargo test -p decoder
```

Test fixtures live in `tests/assets/` (regenerate with `tests/assets/regenerate.sh`).

## API sketch

- `Decoder::open(path)` — lazy `ffmpeg` init (`OnceLock`), best video stream, **YUV420P / NV12 / RGBA** only (v1).
- `seek_scrub(target)` — backward keyframe seek + **first** picture (no PTS chase).
- `seek_exact(target)` — backward seek + decode until PTS ≥ `target` (seconds as [`Rational`]).
- `next_frame()` — sequential decode; `Ok(DecodeOutcome::Eof)` is normal end-of-stream.

Design rationale: `../../docs/decoder/research.md`. Implementation phases: `../../docs/decoder/roadmap.md`.
