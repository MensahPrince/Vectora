# cutlass-desktop

The Cutlass desktop editor — a native Rust + [Slint](https://slint.dev)
frontend combining the Slint interface with the engine, preview worker, audio
playback, timeline gestures, inspector controls, media library, and export
dialog. Unlike the mobile apps, it links `cutlass-engine` **directly** (no
C-ABI/JNI bridge).

This crate owns application behavior and presentation. Timeline state and edit
validation live in the shared engine and model crates.

## Responsibilities

- Launch the desktop application.
- Bind Slint UI state to Rust application state.
- Manage app-owned projects: continuous auto-save, the launch gallery, import
  (Open file…), and relink.
- Display and edit the media library, timeline, preview, inspector, and
  transport.
- Translate user gestures into `cutlass-commands` and send them to
  `cutlass-engine` through the preview worker.
- Render live preview frames with audio sync; live-preview gestures and
  inspector drags through the engine's session-only overrides.
- Manage selection, snapping, trims, drag/drop, keyboard shortcuts, and canvas
  gestures.
- Present export controls and progress.
- Run the AI assistant panel: sandbox rehearsal, dry-run preview card, and
  one-undo plan replay via `cutlass-ai` and `src/agent.rs`.

## Main Areas

- `src/main.rs`: application startup and high-level UI callbacks.
- `src/agent.rs`: AI assistant worker (sandbox rehearsal, plan replay).
- `src/preview_worker.rs`: background engine owner — edits, autosave, preview
  pump, audio snapshots, export thread, live-gesture overrides.
- `src/preview_view.rs`, `src/preview_select.rs`, `src/preview_gesture.rs`,
  `src/placement.rs`: preview display, hit-testing, and canvas interaction.
- `src/audio.rs`: cpal playback fed by `cutlass_render::ExportAudioMixer`.
- `src/thumbnails.rs`, `src/strips.rs`: library tiles, filmstrips, waveforms.
- `src/timeline.rs`, `src/ruler.rs`, `src/snap.rs`, `src/selection.rs`:
  timeline interaction and editing helpers.
- `src/inspector.rs` and `src/params.rs`: inspector bindings and editable clip
  parameters.
- `src/drafts.rs`: app-owned project store (gallery listing, import,
  continuous auto-save targets).
- `ui/`: Slint components, panels, models, and stores.

## Running

```bash
cargo run -p cutlass-desktop
# or open straight into a media file:
cargo run -p cutlass-desktop -- path/to/video.mp4
```

Media decode/encode is platform-native (AVFoundation/VideoToolbox), so macOS
needs no third-party media libraries. The app compiles on Windows and Linux,
but their media backends aren't implemented yet — the UI runs, media won't
play.

## Development Notes

Keep UI-only state in this crate. If behavior affects project correctness,
undo/redo, export, or preview output, it should usually be an engine command
or model change rather than hidden in UI code.

Avoid blocking the Slint event loop. File dialogs and long-running engine work
stay asynchronous or worker-backed.

## Testing

```bash
cargo test -p cutlass-desktop
```

Many UI changes also need targeted engine tests because the engine is the
source of truth for edit behavior.
