# cutlass-ui

`cutlass-ui` is the Cutlass desktop editor. It combines the Slint interface with the Rust engine, preview worker, audio playback, timeline gestures, inspector controls, media library, export dialog, and AI assistant panel.

This crate owns application behavior and presentation. Timeline state and edit validation still live in the shared engine and model crates.

## Responsibilities

- Launch the desktop application.
- Bind Slint UI state to Rust application state.
- Manage app-owned projects: continuous auto-save, the launch gallery, import (Open file…), and relink.
- Display and edit the media library, timeline, preview, inspector, transport, and assistant panel.
- Translate user gestures into `cutlass-commands`.
- Send commands to `cutlass-engine` through the preview worker.
- Render live preview frames with audio sync.
- Manage selection, snapping, trims, drag/drop, keyboard shortcuts, and canvas gestures.
- Configure and run the AI assistant panel through `cutlass-ai`.
- Present export controls and progress.

## Main Areas

- `src/main.rs`: application startup and high-level UI callbacks.
- `src/preview_worker.rs`: background engine owner and preview/export worker.
- `src/preview.rs`, `src/preview_view.rs`, `src/preview_select.rs`, and `src/preview_gesture.rs`: preview display and canvas interaction.
- `src/timeline.rs`, `src/ruler.rs`, `src/snap.rs`, and `src/selection.rs`: timeline interaction and editing helpers.
- `src/inspector.rs` and `src/params.rs`: inspector bindings and editable clip parameters.
- `src/agent.rs`: assistant panel integration.
- `src/drafts.rs`: app-owned project store (gallery listing, import, continuous auto-save targets).
- `ui/`: Slint components, panels, models, and stores.

## Running

Start the editor:

```bash
cargo run -p cutlass-ui
```

Start with a media file:

```bash
cargo run -p cutlass-ui -- path/to/video.mp4
```

The UI uses FFmpeg-backed media support through lower-level crates, so local builds need the prerequisites listed in the root README.

## Development Notes

Keep UI-only state in this crate. If behavior affects project correctness, undo/redo, export, or preview output, it should usually be represented as an engine command or model change rather than hidden in UI code.

Avoid blocking the Slint event loop. File dialogs and long-running engine work should stay asynchronous or worker-backed.

## Testing

Run UI crate tests with:

```bash
cargo test -p cutlass-ui
```

Many UI changes also need targeted engine tests because the engine is the source of truth for edit behavior.
