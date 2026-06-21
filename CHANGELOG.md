# Changelog

Notes for the latest release. For previous releases, see the
[GitHub releases page](https://github.com/1Mr-Newton/cutlass/releases).

## [alpha-0.5.3] — 2026-06-21

### Added

- **Settings screen.** A new in-app Settings dialog, reachable from the
  title-bar gear (or the Cutlass menu on macOS), with four sections:
  - **AI provider** — set the assistant's endpoint, model, and API key (a
    literal or read from an environment variable) without hand-editing a file,
    with a one-click **Test connection**.
  - **Appearance** — switch between the Graphite, Ember, and Dark blue themes,
    applied instantly.
  - **General** — turn autosave on or off and set how often it runs.
  - **Cache** — see the frame cache location and on-disk size, reveal it in
    your file browser, and set the disk budget.

  Settings are saved to `~/.cutlass/config.toml`, preserving any comments and
  hand-edits already in the file.

[alpha-0.5.3]: https://github.com/1Mr-Newton/cutlass/releases/tag/alpha-0.5.3
