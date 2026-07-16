use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cutlass_compositor::{CubeLut, LayerLut, RgbaImage};
use cutlass_core::{RationalTime, VideoDecoder, VideoFrame};
use cutlass_decoder::OutputMode;
use cutlass_models::{MediaId, Project};

use crate::error::RenderError;
use crate::scene::SceneLut;

use super::{Renderer, SeekPolicy};

/// A `.cube` path the renderer has seen: parsed, or failed (missing or
/// malformed file — grades nothing, logged once at load).
pub(super) enum CubeLutState {
    Loaded(Box<CubeLut>),
    Failed,
}

/// Borrow the parsed table behind a realized LUT reference for the
/// compositor's [`LayerLut`] (keyed uploads; see `cutlass-compositor`).
/// Realize only emits references [`Renderer::resolve_scene_lut`] loaded.
pub(super) fn layer_lut<'a>(
    lut: &'a Option<SceneLut>,
    luts: &'a HashMap<String, CubeLutState>,
) -> Option<LayerLut<'a>> {
    let lut = lut.as_ref()?;
    let CubeLutState::Loaded(cube) = &luts[&lut.path] else {
        unreachable!("realized LUT reference without a loaded table")
    };
    Some(LayerLut {
        key: &lut.path,
        lut: cube,
        intensity: lut.intensity,
    })
}

/// Per-asset Lottie frame-cache budget. At the 512 px render cap
/// (~1 MB/frame) this holds ~32 frames — a full loop of a typical short
/// sticker, so steady-state playback rasterizes nothing.
pub(super) const LOTTIE_CACHE_BYTES: usize = 32 << 20;

/// A Lottie path the renderer has seen: parsed and playable, or failed
/// (missing/unsupported file — draws nothing, logged once at load).
pub(super) enum LottieState {
    Loaded(Box<LottiePlayer>),
    Failed,
}

pub(super) struct LottiePlayer {
    pub(super) animation: cutlass_decoder::LottieAnimation,
    /// Rasterized frames keyed by sampled-frame index, stamped with the
    /// scene counter of their last use (the LRU key).
    pub(super) frames: HashMap<usize, (RgbaImage, u64)>,
}

/// A decoded sticker asset: every frame up front plus per-frame delays, so
/// per-composite frame selection is a table walk instead of a decode.
pub(super) struct StickerSequence {
    pub(super) frames: Vec<RgbaImage>,
    /// Display duration per frame in milliseconds (parallel to `frames`).
    pub(super) delays_ms: Vec<u32>,
    /// Sum of `delays_ms`.
    pub(super) total_ms: u64,
}

impl StickerSequence {
    /// Index of the frame on screen at `local_time` seconds, looping over
    /// the sequence. Static stickers (one frame) always show frame 0.
    pub(super) fn frame_at(&self, local_time: f64) -> usize {
        if self.frames.len() <= 1 || self.total_ms == 0 {
            return 0;
        }
        let ms = (local_time.max(0.0) * 1000.0) as u64 % self.total_ms;
        let mut acc = 0u64;
        for (index, delay) in self.delays_ms.iter().enumerate() {
            acc += u64::from(*delay);
            if ms < acc {
                return index;
            }
        }
        self.frames.len() - 1
    }
}

/// The renderer's starting decode mode: zero-copy GPU surfaces on Apple and
/// Windows (with a CPU fallback in [`Renderer::render_scene`]), CPU planes
/// elsewhere until those backends grow a zero-copy import path.
#[cfg(any(target_vendor = "apple", target_os = "windows"))]
pub(super) fn default_decode_mode() -> OutputMode {
    OutputMode::Gpu
}

#[cfg(not(any(target_vendor = "apple", target_os = "windows")))]
pub(super) fn default_decode_mode() -> OutputMode {
    OutputMode::Cpu
}

/// Open the platform's native decoder for `path` in `mode`. Only Apple and
/// Windows have a zero-copy import path today, so the other backends always
/// decode to CPU planes regardless of the requested mode.
#[cfg(target_vendor = "apple")]
pub(super) fn open_decoder(
    path: &Path,
    mode: OutputMode,
) -> Result<Box<dyn VideoDecoder>, RenderError> {
    Ok(Box::new(cutlass_decoder::AvfDecoder::open(path, mode)?))
}

#[cfg(target_os = "windows")]
pub(super) fn open_decoder(
    path: &Path,
    mode: OutputMode,
) -> Result<Box<dyn VideoDecoder>, RenderError> {
    Ok(Box::new(cutlass_decoder::WmfDecoder::open(path, mode)?))
}

#[cfg(target_os = "android")]
pub(super) fn open_decoder(
    path: &Path,
    _mode: OutputMode,
) -> Result<Box<dyn VideoDecoder>, RenderError> {
    Ok(Box::new(cutlass_decoder::MediaCodecDecoder::open(
        path,
        OutputMode::Cpu,
    )?))
}

#[cfg(not(any(target_vendor = "apple", target_os = "windows", target_os = "android")))]
pub(super) fn open_decoder(
    _path: &Path,
    _mode: OutputMode,
) -> Result<Box<dyn VideoDecoder>, RenderError> {
    Err(RenderError::unsupported(
        "no native video decoder for this platform",
    ))
}

impl Renderer {
    /// Decode the frame of `media` at `source_time`, opening (and caching) a
    /// decoder for this `(media, slot)` use on first sight — over the
    /// media's proxy file when one is registered (see
    /// [`set_proxy`](Self::set_proxy)). Under [`SeekPolicy::NearestSync`]
    /// the frame may be the sync point before `source_time` rather than the
    /// exact frame (see [`SeekPolicy`]).
    pub(super) fn decode(
        &mut self,
        project: &Project,
        media: MediaId,
        slot: u32,
        source_time: RationalTime,
        policy: SeekPolicy,
    ) -> Result<VideoFrame, RenderError> {
        let mode = self.decode_mode;
        let proxy = self.use_proxies.then(|| self.proxies.get(&media)).flatten();
        let decoder = match self.decoders.entry((media, slot)) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let src = project
                    .media(media)
                    .ok_or(RenderError::MissingMedia(media))?;
                let path = proxy.map(PathBuf::as_path).unwrap_or_else(|| src.path());
                e.insert(open_decoder(path, mode)?)
            }
        };
        let mut frame = match policy {
            SeekPolicy::Exact => decoder.frame_at(source_time)?,
            SeekPolicy::NearestSync => decoder.frame_at_nearest(source_time)?,
        };
        // An exact target at the media's very end can overshoot EOF: the
        // pool trusts the container's frame count, which routinely
        // over-reports by one, and proxies additionally run a frame short
        // by construction (generation drops that untrustworthy tail). Show
        // the nearest decodable frame instead of failing the whole
        // composite — transitions pin the outgoing side at the final
        // source frame, so a miss here would error every frame of the
        // window.
        if frame.is_none() {
            frame = decoder.frame_at_nearest(source_time)?;
        }
        frame.ok_or(RenderError::NoFrame {
            media,
            time: source_time,
        })
    }

    /// Decode `media`'s single still frame into the cache on first use.
    pub(super) fn ensure_still(
        &mut self,
        project: &Project,
        media: MediaId,
    ) -> Result<(), RenderError> {
        if self.stills.contains_key(&media) {
            return Ok(());
        }
        let src = project
            .media(media)
            .ok_or(RenderError::MissingMedia(media))?;
        let image = cutlass_decoder::decode_image(src.path())?;
        self.stills.insert(media, image);
        Ok(())
    }

    /// Decode a bundled sticker's whole frame sequence into the cache on
    /// first use (mirrors [`ensure_still`](Self::ensure_still)).
    pub(super) fn ensure_sticker(
        &mut self,
        spec: &cutlass_models::StickerSpec,
    ) -> Result<(), RenderError> {
        if self.stickers.contains_key(spec.id) {
            return Ok(());
        }
        let decoded = cutlass_decoder::decode_animation(spec.bytes)?;
        let delays_ms: Vec<u32> = decoded.iter().map(|f| f.delay_ms).collect();
        let total_ms = delays_ms.iter().map(|d| u64::from(*d)).sum();
        self.stickers.insert(
            spec.id.to_owned(),
            StickerSequence {
                frames: decoded.into_iter().map(|f| f.image).collect(),
                delays_ms,
                total_ms,
            },
        );
        Ok(())
    }

    /// Make the Lottie frame for `local_time` resident (parse the file on
    /// first sight, rasterize the frame unless cached) and return its
    /// sampled-frame index. `None` — draw nothing — for files that are
    /// missing, unparseable, or fail to rasterize; the failure is
    /// remembered and logged once, not re-probed per frame.
    pub(super) fn ensure_lottie_frame(&mut self, path: &str, local_time: f64) -> Option<usize> {
        let stamp = self.lottie_stamp;
        let state = self.lottie.entry(path.to_owned()).or_insert_with(|| {
            match cutlass_decoder::LottieAnimation::load(std::path::Path::new(path)) {
                Ok(animation) => LottieState::Loaded(Box::new(LottiePlayer {
                    animation,
                    frames: HashMap::new(),
                })),
                Err(e) => {
                    tracing::warn!("lottie '{path}' failed to load: {e}");
                    LottieState::Failed
                }
            }
        });
        let LottieState::Loaded(player) = state else {
            return None;
        };

        let index = player.animation.frame_index_at(local_time);
        if let Some((_, used)) = player.frames.get_mut(&index) {
            *used = stamp;
            return Some(index);
        }
        let image = match player.animation.render_frame(index) {
            Ok(image) => image,
            Err(e) => {
                tracing::warn!("lottie '{path}' frame {index} failed to render: {e}");
                return None;
            }
        };
        player.frames.insert(index, (image, stamp));

        // Enforce the per-asset byte budget, oldest-stamp first. Frames
        // stamped by the current scene are exempt (still borrowed below).
        let mut total: usize = player.frames.values().map(|(f, _)| f.pixels.len()).sum();
        while total > LOTTIE_CACHE_BYTES {
            let Some((&victim, _)) = player
                .frames
                .iter()
                .filter(|(_, (_, used))| *used != stamp)
                .min_by_key(|(_, (_, used))| *used)
            else {
                break;
            };
            let (evicted, _) = player.frames.remove(&victim).expect("victim exists");
            total -= evicted.pixels.len();
        }
        Some(index)
    }

    /// Resolve a layer's `.cube` LUT reference: parse and cache the file on
    /// first sight, and return the reference only when the table is loadable.
    /// Missing/unparseable files log once and grade nothing — the media
    /// offline story, never an error.
    pub(super) fn resolve_scene_lut(&mut self, lut: &Option<SceneLut>) -> Option<SceneLut> {
        let lut = lut.as_ref()?;
        let state = self.luts.entry(lut.path.clone()).or_insert_with(|| {
            match std::fs::read_to_string(&lut.path) {
                Ok(text) => match CubeLut::parse(&text) {
                    Ok(cube) => CubeLutState::Loaded(Box::new(cube)),
                    Err(e) => {
                        tracing::warn!("LUT '{}' failed to parse: {e}", lut.path);
                        CubeLutState::Failed
                    }
                },
                Err(e) => {
                    tracing::warn!("LUT '{}' failed to read: {e}", lut.path);
                    CubeLutState::Failed
                }
            }
        });
        matches!(state, CubeLutState::Loaded(_)).then(|| lut.clone())
    }
}
