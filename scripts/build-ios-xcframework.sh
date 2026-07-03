#!/usr/bin/env bash
# Build CutlassMobileFFI.xcframework for the CutlassMobile Swift package.
#
# The xcframework bundles the cutlass-mobile static lib for device, simulator,
# and macOS so Xcode can link whichever slice it needs. It is a build artifact
# (gitignored); run this after changing any Rust the mobile FFI depends on,
# then rebuild the app in Xcode.
#
# Usage: scripts/build-ios-xcframework.sh [--debug]

set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE=release
CARGO_FLAGS=(--release)
if [[ "${1:-}" == "--debug" ]]; then
    PROFILE=debug
    CARGO_FLAGS=()
fi

TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin)
for t in "${TARGETS[@]}"; do
    rustup target add "$t" >/dev/null
    cargo build -p cutlass-mobile --target "$t" "${CARGO_FLAGS[@]}"
done

PKG=apps/cutlass-ios-macos/CutlassMobile
OUT="$PKG/CutlassMobileFFI.xcframework"
rm -rf "$OUT"
xcodebuild -create-xcframework \
    -library "target/aarch64-apple-ios/$PROFILE/libcutlass_mobile.a" -headers "$PKG/include" \
    -library "target/aarch64-apple-ios-sim/$PROFILE/libcutlass_mobile.a" -headers "$PKG/include" \
    -library "target/aarch64-apple-darwin/$PROFILE/libcutlass_mobile.a" -headers "$PKG/include" \
    -output "$OUT"

echo "wrote $OUT ($PROFILE)"
