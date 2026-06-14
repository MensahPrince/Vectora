# Release packaging

Cutlass alpha builds ship as prebuilt binaries. The editor is `cutlass-ui`;
`cutlass-app` remains a headless smoke-test CLI and is not packaged for desktop
users in this alpha.

## Versioning

- Cargo workspace version: `0.1.0-alpha.0` (semver pre-release) in the root
  `Cargo.toml`.
- Git tag for the alpha line: `alpha-0.1.0` (and `alpha-0.1.1`, …).
- macOS `Info.plist` short version: `0.1.0-alpha`.

## Local builds

```bash
# macOS .app (bundles Homebrew FFmpeg into the bundle)
brew install ffmpeg dylibbundler
cargo build --release -p cutlass-ui
./scripts/package-macos.sh
# → dist/Cutlass-0.1.0-alpha.0-macos-aarch64.zip

# Linux tarball (user installs FFmpeg separately)
cargo build --release -p cutlass-ui
./scripts/package-linux.sh
# → dist/Cutlass-0.1.0-alpha.0-linux-x86_64.tar.gz

# Windows zip (bundles vcpkg FFmpeg DLLs)
# See Slint's FFmpeg example for vcpkg + LLVM setup on Windows:
# https://github.com/slint-ui/slint/tree/master/examples/ffmpeg#building
cargo build --release -p cutlass-ui
.\scripts\package-windows.ps1
# → dist/Cutlass-0.1.0-alpha.0-windows-x86_64.zip
```

`dist/` is gitignored. Use `--no-ffmpeg` on the macOS script only for local
smoke tests — distributed builds must bundle FFmpeg or users on machines
without Homebrew will fail to launch.

### macOS Gatekeeper (alpha)

Release zips are **adhoc-signed**, not notarized. Double-click may do nothing
until the user right-clicks `Cutlass.app` → **Open** once. The zip includes
`INSTALL-macos.txt` with full steps. Proper fix for a stable channel is Apple
Developer ID signing + notarization in CI.

## GitHub release

Push a tag to trigger `.github/workflows/release.yml`:

```bash
git tag alpha-0.1.0
git push origin alpha-0.1.0
```

The workflow builds macOS (arm64), Linux (x86_64), and Windows (x86_64)
artifacts, attaches them to a GitHub Release, and uses `CHANGELOG.md` for the
release body.

## FFmpeg / licensing

macOS bundles copy the FFmpeg dylibs linked at build time. Cutlass does not
modify FFmpeg; comply with [FFmpeg's license](https://www.ffmpeg.org/legal.html)
(LGPL/GPL depending on your FFmpeg build) when redistributing binaries.
