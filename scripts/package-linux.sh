#!/usr/bin/env bash
# Assemble a distributable Linux tarball for Cutlass (alpha packaging).
#
# DORMANT: the app compiles and launches on Linux, but this branch's media
# stack (decode/encode) has no Linux backend yet — imported media won't play.
# The script is kept working so packaging is ready the day the backend lands.
#
# Usage:
#   ./scripts/package-linux.sh
#
# Output:
#   dist/Cutlass-<version>-linux-<arch>.tar.gz

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"
ARCH="$(uname -m)"
DIST="dist"
STAGING="$DIST/staging-linux-$ARCH"
PKG="cutlass-$VERSION-linux-$ARCH"

echo "==> packaging Cutlass $VERSION for Linux ($ARCH)"

BINARY_SRC="target/release/cutlass-desktop"
if [[ ! -f "$BINARY_SRC" ]]; then
    echo "==> release binary missing; building cutlass-desktop"
    cargo build --release -p cutlass-desktop
fi

rm -rf "$STAGING"
mkdir -p "$STAGING/$PKG"
cp "$BINARY_SRC" "$STAGING/$PKG/cutlass-desktop"
chmod +x "$STAGING/$PKG/cutlass-desktop"
cp LICENSE-MIT LICENSE-APACHE "$STAGING/$PKG/"

cat >"$STAGING/$PKG/README-INSTALL.txt" <<EOF
Cutlass $VERSION — Linux ($ARCH)
================================

Run:
  ./cutlass-desktop

Preview build: the editor UI runs, but video/audio decode and export are
not implemented on Linux yet in this line — imported media will not play.

See https://github.com/1Mr-Newton/cutlass for source and issue tracker.
EOF

TARBALL="$DIST/Cutlass-${VERSION}-linux-${ARCH}.tar.gz"
rm -f "$TARBALL"
tar -C "$STAGING" -czf "$TARBALL" "$PKG"

echo "==> wrote $TARBALL ($(du -h "$TARBALL" | awk '{print $1}'))"
