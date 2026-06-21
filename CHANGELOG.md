# Changelog

Notes for the latest release. For previous releases, see the
[GitHub releases page](https://github.com/1Mr-Newton/cutlass/releases).

## [alpha-0.5.2] — 2026-06-21

### Fixed

- **Huge memory leak when importing video.** Every demux pass leaked the packets
  it read, so opening a clip leaked hundreds of MB to several GB that was never
  reclaimed — even after deleting the clip and its media. A single long,
  high-bitrate source could push RAM past 4 GB. All decode paths (video, audio,
  keyframe and MP3 indexing) now free packets as they go.
- **Bounded preview decoder memory.** Per-clip decoders are released as soon as a
  clip leaves the timeline (delete, split, trim, undo) instead of living until
  the project is swapped, with an LRU cap as a backstop. Software decode now also
  sizes its worker threads to the frame, so a high-resolution source no longer
  scales RAM with CPU core count (e.g. ~680 MB → ~245 MB for a 3200×2400 clip on
  an 18-core machine).
- **Windows: the AI assistant config is found again.** Its path now resolves from
  the user's home directory.

[alpha-0.5.2]: https://github.com/1Mr-Newton/cutlass/releases/tag/alpha-0.5.2
