//! Re-point a media-pool entry at a new file (missing-media relink, M0).

use std::path::Path;

use cutlass_models::{MediaId, ModelError};

use crate::action::ApplyContext;
use crate::error::EngineError;
use crate::import::import_media;

/// Replace `media`'s backing file with `path`: re-probe the file and refresh
/// the entry's metadata in place. The entry keeps its [`MediaId`], so every
/// clip referencing it recovers without being touched.
///
/// Not undoable (no inverse): relink repairs project state to match the disk
/// after files moved, and "undo" back to a dead path is never what the user
/// wants. The session goes dirty so the repaired path gets saved.
///
/// The new file's metadata wins wholesale (dimensions, duration, rate, audio).
/// Relinking to a shorter file can leave clips with source windows past the new
/// end — tolerated the same way missing media is: the model stores it, decode
/// degrades, the next trim re-validates.
pub fn execute(ctx: &mut ApplyContext<'_>, media: MediaId, path: &Path) -> Result<(), EngineError> {
    if ctx.project.media(media).is_none() {
        return Err(EngineError::Model(ModelError::UnknownMedia(media)));
    }
    let path = path.canonicalize().map_err(EngineError::Io)?;
    // Probe exactly like an import; only the freshly allocated id is discarded —
    // the pool entry keeps its identity.
    let probed = import_media(&path)?;
    let entry = ctx
        .project
        .media_mut(media)
        .expect("entry checked above; nothing removes it in between");
    entry.path = probed.path;
    entry.width = probed.width;
    entry.height = probed.height;
    entry.frame_rate = probed.frame_rate;
    entry.duration = probed.duration;
    entry.has_audio = probed.has_audio;
    entry.is_image = probed.is_image;
    Ok(())
}
