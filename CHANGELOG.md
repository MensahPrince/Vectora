# Changelog

Notes for the latest release. For previous releases, see the
[GitHub releases page](https://github.com/1Mr-Newton/cutlass/releases).

## [alpha-0.5.1] — 2026-06-21

### Fixed

- **Windows: no longer crashes on launch / needs admin.** Cache, autosave, and
  recent projects now live in the per-user OS directories (`%LOCALAPPDATA%` /
  `%APPDATA%`) instead of the read-only install folder. Fixes the same latent
  path bug on macOS/Linux too (data moves to the standard OS locations).

[alpha-0.5.1]: https://github.com/1Mr-Newton/cutlass/releases/tag/alpha-0.5.1
