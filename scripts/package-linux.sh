#!/usr/bin/env bash
# Assemble a distributable Linux tarball for Cutlass (alpha packaging).
#
# Usage:
#   ./scripts/package-linux.sh
#
# Output:
#   dist/Cutlass-<version>-linux-<arch>.tar.gz
#
# The archive contains the cutlass-ui binary, licenses, and a short README.
# FFmpeg must be installed on the target system (see README-INSTALL.txt).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"
ARCH="$(uname -m)"
DIST="dist"
STAGING="$DIST/staging-linux-$ARCH"
PKG="cutlass-$VERSION-linux-$ARCH"

echo "==> packaging Cutlass $VERSION for Linux ($ARCH)"

BINARY_SRC="target/release/cutlass-ui"
if [[ ! -f "$BINARY_SRC" ]]; then
    echo "==> release binary missing; building cutlass-ui"
    cargo build --release -p cutlass-ui
fi

rm -rf "$STAGING"
mkdir -p "$STAGING/$PKG"
cp "$BINARY_SRC" "$STAGING/$PKG/cutlass-ui"
chmod +x "$STAGING/$PKG/cutlass-ui"
cp LICENSE-MIT LICENSE-APACHE "$STAGING/$PKG/"

cat >"$STAGING/$PKG/README-INSTALL.txt" <<EOF
Cutlass $VERSION — Linux ($ARCH)
================================

Run:
  ./cutlass-ui

Requires FFmpeg development libraries at runtime. On Debian/Ubuntu:
  sudo apt-get install -y libavcodec-dev libavformat-dev libavutil-dev \\
    libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev

Or install the matching runtime packages from your distro if the binary
was linked against shared FFmpeg libs.

See https://github.com/1Mr-Newton/cutlass for source and issue tracker.
EOF

TARBALL="$DIST/Cutlass-${VERSION}-linux-${ARCH}.tar.gz"
rm -f "$TARBALL"
tar -C "$STAGING" -czf "$TARBALL" "$PKG"

echo "==> wrote $TARBALL ($(du -h "$TARBALL" | awk '{print $1}'))"
