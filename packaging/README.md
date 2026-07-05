# Release packaging

Cutlass alpha builds ship as prebuilt binaries. The desktop editor is
`cutlass-desktop`; the iOS/macOS SwiftUI app ships through Xcode/TestFlight
and is not covered here.

macOS is the only desktop platform with a working media stack today
(AVFoundation/VideoToolbox). Windows and Linux packages are **dormant**: the
app compiles and the UI runs there, but imported media cannot decode until
their native backends land. The scripts are kept working so packaging is
ready the day that happens.

## Versioning

- Cargo workspace version: `0.5.3-alpha.0` (semver pre-release) in the root
  `Cargo.toml`.
- Git tag for the alpha line: `alpha-0.5.3` (and `alpha-0.5.4`, …).
- macOS `Info.plist` short version: `0.5.3-alpha`.

## Local builds

```bash
# macOS .app — no bundled media libraries; AVFoundation is part of the OS
cargo build --release -p cutlass-desktop
./scripts/package-macos.sh
# → dist/Cutlass-0.5.3-alpha.0-macos-arm64.zip

# Linux tarball (dormant preview: UI only, no media decode yet)
cargo build --release -p cutlass-desktop
./scripts/package-linux.sh
# → dist/Cutlass-0.5.3-alpha.0-linux-x86_64.tar.gz

# Windows zip (dormant preview: UI only, no media decode yet)
cargo build --release -p cutlass-desktop
.\scripts\package-windows.ps1
# → dist/Cutlass-0.5.3-alpha.0-windows-x86_64.zip
```

### Windows installer (Setup.exe)

The portable zip above runs in place. For a real setup wizard (Start-menu
shortcut, uninstaller, optional desktop icon) build an Inno Setup installer:

```powershell
# one-time: install the Inno Setup compiler
choco install innosetup

# stages the payload (reusing package-windows.ps1) and compiles the installer
.\scripts\package-windows-installer.ps1
# → dist/Cutlass-0.5.3-alpha.0-windows-x86_64-Setup.exe

# native ARM64 installer (run on an ARM64 Windows host):
.\scripts\package-windows-installer.ps1 -Arch arm64
# → dist/Cutlass-0.5.3-alpha.0-windows-arm64-Setup.exe
```

The Inno Setup script lives at `packaging/windows/cutlass.iss`; the PowerShell
wrapper passes the version, staged source dir, and output path as `/D` defines.
The installer is unsigned for now — Windows SmartScreen will warn on first run
until the `Setup.exe` is Authenticode-signed.

`dist/` is gitignored.

### macOS Gatekeeper (alpha)

Release zips are **adhoc-signed**, not notarized. Double-click may do nothing
until the user right-clicks `Cutlass.app` → **Open** once. The zip includes
`INSTALL-macos.txt` with full steps. Proper fix for a stable channel is Apple
Developer ID signing + notarization in CI.

## Licensing

Bundles carry no third-party media libraries: decode/encode goes through the
operating system's frameworks (AVFoundation/VideoToolbox on Apple platforms).
Cutlass itself is MIT OR Apache-2.0.
