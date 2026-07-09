# cutlass-commands

`cutlass-commands` defines the structured command vocabulary for Cutlass. UI gestures, AI-generated edits, tests, and CLI flows all use these command values when they want to change a project.

The crate describes what can be requested. `cutlass-engine` decides whether a command is valid for the current project state, applies it, and records undo/redo history.

## Responsibilities

- Define the top-level `Command` enum.
- Define `ProjectCommand` for session and media-pool actions such as import, open, save, relink, and export.
- Define `EditCommand` for timeline edits such as add track, add clip, split, trim, move, remove, ripple edit, link clips, markers, keyframes, transforms, crop, speed, audio, effects, transitions, and canvas settings.
- Define `EditOutcome` values returned by successful edit commands.
- Re-export common model IDs and time types used by command callers.

## Design notes

Commands are explicit Rust values rather than loosely typed callbacks or UI events. This keeps editing behavior auditable and makes it possible for several frontends to share one execution path.

The command layer should stay small and deterministic:

- A command should describe one user-visible operation.
- Validation that needs project state should live in `cutlass-engine`.
- Pure data types shared by commands should live in `cutlass-models`.
- AI-specific request shapes should live in `cutlass-ai` and lower into these commands only after validation.

## Common entry points

- `Command`: wrapper over project-level and timeline-level actions.
- `ProjectCommand`: import, save, open, load, relink, and export.
- `EditCommand`: all timeline editing operations.
- `EditOutcome`: structured result for edits that create or affect entities.

## Testing

Run command-related tests through the engine and AI crates:

```bash
cargo test -p cutlass-engine
cargo test -p cutlass-ai
```

When adding a command, add engine coverage for apply and undo behavior. If the AI assistant should be able to use it, add corresponding validation and schema coverage in `cutlass-ai`.
