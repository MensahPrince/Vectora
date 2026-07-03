#ifndef CUTLASS_MOBILE_H
#define CUTLASS_MOBILE_H

#include <stdint.h>
#include <stddef.h>

/*
 * C ABI for the `cutlass-mobile` Rust library.
 *
 * The engine composites a frame on a headless wgpu (Metal) device and returns
 * the pixels as RGBA8. Every non-null buffer must be released exactly once with
 * `cutlass_image_free`.
 */

typedef struct CutlassImage {
    /* RGBA8 pixels (`len` bytes), or NULL if rendering failed. */
    uint8_t *data;
    /* Length of `data` in bytes (== width * height * 4). */
    size_t len;
    uint32_t width;
    uint32_t height;
} CutlassImage;

/* Render the built-in demo scene at `width` x `height`. `data` is NULL on failure. */
CutlassImage cutlass_render_demo(uint32_t width, uint32_t height);

/*
 * Decode + composite the first frame of the video at `path_utf8` (a UTF-8 path
 * of `path_len` bytes, no NUL terminator required), scaled to fit
 * `max_width` x `max_height`. `data` is NULL on failure.
 */
CutlassImage cutlass_render_file_frame(const uint8_t *path_utf8, size_t path_len,
                                       uint32_t max_width, uint32_t max_height);

/* Release a buffer returned by `cutlass_render_demo`. NULL/empty is a no-op. */
void cutlass_image_free(CutlassImage img);

/*
 * Interactive preview.
 *
 * A `CutlassPreview` holds a persistent GPU device + decoder cache bound to a
 * project, so scrubbing only pays for the frame at a given time. Open a session,
 * call `cutlass_preview_render` per slider tick, and free it with
 * `cutlass_preview_close`. Not thread-safe: serialize calls on one handle.
 */
typedef struct CutlassPreview CutlassPreview;

/* Open the synthetic scrub demo (no assets). NULL on failure. */
CutlassPreview *cutlass_preview_open_demo(void);

/*
 * Open a preview that scrubs the video at `path_utf8` (`path_len` UTF-8 bytes).
 * NULL if the file can't be probed or the GPU is unavailable.
 */
CutlassPreview *cutlass_preview_open_video(const uint8_t *path_utf8, size_t path_len);

/* Total scrub length in seconds. 0.0 for a NULL handle. */
double cutlass_preview_duration_seconds(const CutlassPreview *handle);

/*
 * Render the preview frame at `seconds` (clamped to range). `data` is NULL on
 * failure; release every non-null result once with `cutlass_image_free`.
 */
CutlassImage cutlass_preview_render(CutlassPreview *handle, double seconds);

/* Release a handle from `cutlass_preview_open_*`. NULL is a no-op. */
void cutlass_preview_close(CutlassPreview *handle);

#endif /* CUTLASS_MOBILE_H */
