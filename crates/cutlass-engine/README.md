# cutlass-engine

`cutlass-engine` is the headless editing engine for Cutlass. It owns an editable project session, applies structured commands, records undo/redo history, produces preview frames, and exports timelines.

This crate is the main integration point for non-UI callers. The desktop editor, smoke-test CLI, tests, and AI bridge all rely on the engine instead of mutating project state directly.

## Responsibilities

- Create and manage an `Engine` session.
- Apply `cutlass-commands` values against the current `cutlass-models` project.
- Validate edits that depend on current timeline state.
- Build inverse commands for undo/redo.
- Import media through probing and metadata refresh.
- Resolve timeline content into compositing layers.
- Rasterize generated text, solids, and shapes for preview and export.
- Decode media frames and use the on-disk frame cache for preview.
- Produce `RgbaFrame` preview frames.
- Export timelines to MP4 through `cutlass-encoder`.

## Main APIs

- `Engine`: session object for applying commands and reading project state.
- `EngineConfig`: cache and runtime configuration.
- `ApplyOutcome`: result of applying a command.
- `ExportSettings` and `ExportProgress`: export configuration and progress reporting.
- `export_project`, `export_timeline`, and related helpers: direct export entry points.
- `RgbaFrame`: preview frame returned by the engine.

## How It Fits

`cutlass-engine` depends on most lower-level crates:

- `cutlass-models` for project and timeline state.
- `cutlass-commands` for edit requests.
- `cutlass-probe` and `cutlass-decoder` for media metadata and frames.
- `cutlass-cache` for decoded frame storage.
- `cutlass-compositor` for GPU compositing.
- `cutlass-encoder` for final MP4 output.

The engine should stay UI-agnostic. Slint models, widgets, file dialogs, and interaction state belong in `cutlass-ui`.

## Testing And Benchmarks

Run the engine tests with:

```bash
cargo test -p cutlass-engine
```

Run local preview and export benchmarks with:

```bash
cargo bench -p cutlass-engine --bench preview
cargo bench -p cutlass-engine --bench export
```

Engine tests should cover both the forward edit and the inverse edit when behavior is undoable.
