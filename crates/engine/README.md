# `engine`

Decode **scheduling** for Cutlass: one worker thread owns [`decoder::Decoder`] instances and answers “give me a frame at time *T*” via channels.

- Design: [`docs/engine/research.md`](../docs/engine/research.md)
- Plan / MVP phases: [`docs/engine/roadmap.md`](../docs/engine/roadmap.md)

## Quickstart

```rust
use engine::{Engine, EngineEvent, Rational};
use std::path::PathBuf;

let (engine, rx) = Engine::new();
let (sid, _open_rid) = engine.open(PathBuf::from("media.mp4"));

match rx.recv_timeout(std::time::Duration::from_secs(5)).expect("recv") {
    EngineEvent::Opened { .. } => {}
    other => panic!("{other:?}"),
}

let _ = engine.seek_exact(sid, Rational::new_raw(2, 1));
while let Ok(ev) = rx.recv() {
    println!("{ev:?}");
    break;
}
```

Run the smoke harness:

```bash
cargo run -p engine --example playground -- crates/engine/tests/assets/testsrc_h264.mp4
```

Optional script (lines like `seek_exact 2/1`, `next 5`, `scrub 5/2`, `close`, `sleep_ms 20`):

```bash
cargo run -p engine --example playground -- ./video.mp4 --script ./script.txt
```

## Contracts (MVP)

- **Drain events:** the outbound queue is bounded (`EVENT_CHANNEL_CAPACITY`); consumers must read or the worker blocks on send.
- **`seek_exact` vs `seek_scrub`:** exact seeks correlate with `RequestId` on events; scrub is latest-wins and uses `request_id: None` on frames.
- **One decoder:** opening a second source without closing the first yields `EngineError::SourceAlreadyOpen` on the event stream.

## Tests

Integration tests use FFmpeg fixtures under `tests/assets/` (symlinks to `crates/decoder/tests/assets/`). Regenerate via `tests/assets/regenerate.sh` (calls the decoder asset script).
