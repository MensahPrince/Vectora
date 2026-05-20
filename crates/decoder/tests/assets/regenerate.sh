#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Core H.264 fixtures (5s, 30fps, 320x240, GOP=30)
ffmpeg -y -f lavfi -i testsrc=duration=5:size=320x240:rate=30 \
  -c:v libx264 -g 30 -pix_fmt yuv420p testsrc_h264.mp4
ffmpeg -y -f lavfi -i testsrc=duration=5:size=320x240:rate=30 \
  -c:v libx264 -g 30 -bf 3 -pix_fmt yuv420p testsrc_bframes.mp4

# Note: H.264 (libx264) output is tagged YUV420P in FFmpeg; NV12 in the renderer is covered
# by synthetic frames in `tests/gpu_integration.rs` and unit tests.

# Audio-only (negative: no video stream)
ffmpeg -y -f lavfi -i sine=frequency=440:sample_rate=48000:duration=2 \
  -c:a aac audio_only.m4a

# Combined AV — demuxer must skip non-video packets
ffmpeg -y -f lavfi -i testsrc=duration=2:size=128x96:rate=24 \
  -f lavfi -i sine=frequency=220:sample_rate=48000:duration=2 \
  -c:v libx264 -g 24 -pix_fmt yuv420p -c:a aac -shortest test_av.mp4

# Pixel format / codec not in v1 allowlist (decoder rejects at open)
ffmpeg -y -f lavfi -i testsrc=duration=0.5:size=64x48:rate=12 \
  -c:v ffv1 test_unsupported_codec.mkv

# Invalid / tiny file — demuxer open should fail
printf 'not ffmpeg data' > corrupt_truncated.mp4

# Single still-image fixture for probe's image-kind path. 2×2 red PNG
# (~94 bytes). Demuxer reports `format=png_pipe`, so `probe` classifies it
# as `ProbedKind::Image`; ffmpeg exposes the file as a 1-frame PNG video
# stream, so `video` is `Some` with codec="png".
ffmpeg -y -f lavfi -i color=c=red:s=2x2:d=0.04 -frames:v 1 testsrc_image.png
