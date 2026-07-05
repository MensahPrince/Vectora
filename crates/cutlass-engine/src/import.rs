//! Register a file in the media pool via native (FFmpeg-free) probing.
//!
//! Probing opens the platform decoder ([`cutlass_decoder::probe`]) just far
//! enough to read the source's dimensions, frame rate, and duration, then
//! builds a [`MediaSource`] for the project's pool. No frame cache and no
//! FFmpeg: the same native codec that decodes for preview/export reports the
//! metadata.

use std::path::Path;

use cutlass_decoder::probe;
use cutlass_models::MediaSource;
use tracing::debug;

use crate::error::EngineError;

/// Probe a media file and build a [`MediaSource`] describing it.
///
/// `path` should already exist and be canonical; callers resolve it before
/// probing so the pool stores a stable absolute path.
pub fn import_media(path: &Path) -> Result<MediaSource, EngineError> {
    let probed = probe(path)?;
    // Stills carry no timebase of their own: the pool applies the model's
    // convention (millisecond ticks, 5s default placement length).
    let media = if probed.is_image {
        MediaSource::image(path, probed.width, probed.height)
    } else {
        MediaSource::new(
            path,
            probed.width,
            probed.height,
            probed.frame_rate,
            probed.frame_count,
            probed.has_audio,
        )
    };

    debug!(
        path = %path.display(),
        width = media.width,
        height = media.height,
        frames = media.duration.value,
        has_audio = media.has_audio,
        "probed media"
    );

    Ok(media)
}
