//! Per-media decoder reuse for preview and export.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cutlass_decoder::{DecodeOptions, Decoder, HwAccel, KeyframeIndex, STILL_MAX_DIM};
use cutlass_models::MediaId;

use crate::error::EngineError;

struct Entry {
    path: PathBuf,
    decoder: Decoder,
    index: KeyframeIndex,
}

/// One decoded still image, shared by every composite that shows it.
/// The `Arc` is what `CompositeLayer::rgba` wants, so re-showing a still
/// is a refcount bump — no copy, no re-decode.
struct StillEntry {
    path: PathBuf,
    bytes: Arc<Vec<u8>>,
    width: u32,
    height: u32,
}

pub struct DecoderPool {
    entries: HashMap<MediaId, Entry>,
    stills: HashMap<MediaId, StillEntry>,
    options: DecodeOptions,
}

impl DecoderPool {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            stills: HashMap::new(),
            options: DecodeOptions::default().hw_accel(HwAccel::None),
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.stills.clear();
    }

    pub fn decoder_and_index(
        &mut self,
        media_id: MediaId,
        path: &Path,
    ) -> Result<(&mut Decoder, &KeyframeIndex), EngineError> {
        let stale = self.entries.get(&media_id).is_none_or(|e| e.path != path);

        if stale {
            let decoder = Decoder::open_with(path, self.options)?;
            let index = KeyframeIndex::build(path)?;
            self.entries.insert(
                media_id,
                Entry {
                    path: path.to_path_buf(),
                    decoder,
                    index,
                },
            );
        }

        let entry = self.entries.get_mut(&media_id).expect("just inserted");
        Ok((&mut entry.decoder, &entry.index))
    }

    /// The decoded RGBA for a still-image media, decoding on first use
    /// (capped to [`STILL_MAX_DIM`] per side; the GPU scales into place).
    /// Returns `(bytes, width, height)`.
    pub fn still(
        &mut self,
        media_id: MediaId,
        path: &Path,
    ) -> Result<(Arc<Vec<u8>>, u32, u32), EngineError> {
        let stale = self.stills.get(&media_id).is_none_or(|e| e.path != path);

        if stale {
            let image = cutlass_decoder::decode_image(path, STILL_MAX_DIM, STILL_MAX_DIM)?;
            self.stills.insert(
                media_id,
                StillEntry {
                    path: path.to_path_buf(),
                    bytes: Arc::new(image.rgba),
                    width: image.width,
                    height: image.height,
                },
            );
        }

        let entry = self.stills.get(&media_id).expect("just inserted");
        Ok((Arc::clone(&entry.bytes), entry.width, entry.height))
    }
}

impl Default for DecoderPool {
    fn default() -> Self {
        Self::new()
    }
}
