#!/usr/bin/env bash
# Assemble a distributable Cutlass.app for macOS (alpha packaging).
#
# Usage:
#   ./scripts/package-macos.sh              # aarch64 (native)
#   ./scripts/package-macos.sh --no-ffmpeg  # skip dylib bundling (dev only)
#
# Output:
#   dist/Cutlass-<version>-macos-<arch>.zip   (the .app inside)
#
# Prerequisites:
#   - Rust stable (see rust-toolchain.toml)
#   - FFmpeg via Homebrew (linked at build time; bundled into the .app)
#   - dylibbundler (brew install dylibbundler) unless --no-ffmpeg

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BUNDLE_FFMPEG=1
for arg in "$@"; do
    case "$arg" in
        --no-ffmpeg) BUNDLE_FFMPEG=0 ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *)
            echo "unknown argument: $arg" >&2
            exit 1
            ;;
    esac
done

VERSION="$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"
ARCH="$(uname -m)"
DIST="dist"
APP_NAME="Cutlass.app"
STAGING="$DIST/staging-$ARCH"
APP="$STAGING/$APP_NAME"
BINARY_SRC="target/release/cutlass-ui"
ICON_PNG="assets/icon/cutlass-in-app.png"

echo "==> packaging Cutlass $VERSION for macOS ($ARCH)"

if [[ ! -f "$BINARY_SRC" ]]; then
    echo "==> release binary missing; building cutlass-ui"
    cargo build --release -p cutlass-ui
fi

rm -rf "$STAGING"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp packaging/macos/Info.plist "$APP/Contents/Info.plist"
cp "$BINARY_SRC" "$APP/Contents/MacOS/cutlass-ui"
chmod +x "$APP/Contents/MacOS/cutlass-ui"

# App icon (.icns) from the same PNG the dock icon uses at runtime.
# Optional: skip if the source PNG isn't present so packaging still succeeds.
if [[ -f "$ICON_PNG" ]]; then
    ICONSET="$STAGING/AppIcon.iconset"
    mkdir -p "$ICONSET"
    for size in 16 32 128 256 512; do
        sips -z $size $size "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
        dbl=$((size * 2))
        sips -z $dbl $dbl "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
else
    echo "==> note: $ICON_PNG missing; building .app without a custom icon"
fi

if [[ "$BUNDLE_FFMPEG" -eq 1 ]]; then
    if ! command -v dylibbundler >/dev/null; then
        echo "dylibbundler not found — install with: brew install dylibbundler" >&2
        echo "or re-run with --no-ffmpeg (not suitable for distribution)" >&2
        exit 1
    fi
    echo "==> bundling dynamic libraries into Contents/Frameworks"
    mkdir -p "$APP/Contents/Frameworks"
    dylibbundler -od -b -x "$APP/Contents/MacOS/cutlass-ui" \
        -d "$APP/Contents/Frameworks" \
        -p @executable_path/../Frameworks/
fi

# Adhoc-sign the full bundle so Launch Services can validate nested Frameworks.
echo "==> adhoc-signing app bundle"
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

RELEASE="$STAGING/release"
rm -rf "$RELEASE"
mkdir -p "$RELEASE"
ditto "$APP" "$RELEASE/$APP_NAME"
cp packaging/macos/INSTALL.txt "$RELEASE/INSTALL-macos.txt"

ZIP="$DIST/Cutlass-${VERSION}-macos-${ARCH}.zip"
rm -f "$ZIP"
(
    cd "$RELEASE"
    # zip -y preserves symlinks inside the .app Frameworks tree.
    zip -r -y "$ROOT/$ZIP" "$APP_NAME" INSTALL-macos.txt
)

echo "==> wrote $ZIP ($(du -h "$ZIP" | awk '{print $1}'))"
echo "    install: unzip, read INSTALL-macos.txt, drag Cutlass.app to /Applications"
echo "    first launch: Right-click Cutlass.app → Open (Gatekeeper; not notarized yet)"
