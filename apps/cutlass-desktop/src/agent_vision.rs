//! Agent-facing frame capture through the editor's real render path.
//!
//! The agent thread owns this service. Its renderer is deliberately private:
//! sharing the preview worker's renderer would contend with scrubbing and mix
//! decoder-cache state between two independently scheduled consumers.

use std::path::{Path, PathBuf};

use cutlass_ai::ImagePart;
use cutlass_models::{MediaId, MediaSource, Project, Rational, RationalTime, TrackKind};
use cutlass_render::Renderer;

/// Maximum width or height accepted for an agent vision frame.
pub(crate) const MAX_VISION_EDGE: u32 = 768;

const MIN_VISION_EDGE: u32 = 64;
const MAX_LABEL_FILE_NAME_CHARS: usize = 128;

/// Reused on the agent thread: one private GPU queue + decoder cache, lazily
/// created so non-vision prompts pay no GPU bring-up.
pub(crate) struct AgentVision {
    renderer: Option<Renderer>,
    media_catalog: Vec<MediaCatalogEntry>,
}

impl Default for AgentVision {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentVision {
    pub(crate) fn new() -> Self {
        Self {
            renderer: None,
            media_catalog: Vec::new(),
        }
    }

    /// Render a frame from an isolated snapshot of the current project.
    pub(crate) fn project_frame(
        &mut self,
        project: &Project,
        seconds: f64,
        max_width: u32,
        max_height: u32,
        label_prefix: &str,
    ) -> Result<ImagePart, String> {
        let (max_width, max_height) = vision_dimensions(max_width, max_height)?;
        let rate = project.timeline().frame_rate;
        let duration = project.timeline().duration().value;
        let frame_time = RationalTime::new(seconds_to_tick(seconds, rate, duration)?, rate);

        // The immutable borrow is already a stable snapshot for the complete
        // synchronous render; cloning a large timeline here would only add
        // latency to every frame request.
        self.sync_media_catalog(project)?;
        let image = self
            .renderer()?
            .render_frame_fit(project, frame_time, max_width, max_height)
            .map_err(|error| {
                format!(
                    "could not render project frame at {:.2}s: {error}",
                    frame_time.seconds()
                )
            })?;
        let png = cutlass_render::encode_png(&image)
            .map_err(|error| format!("could not encode project frame as PNG: {error}"))?;

        Ok(ImagePart::png(
            png,
            format!("{label_prefix} at {:.2}s", frame_time.seconds()),
        ))
    }

    /// Render a source file through a one-clip project, preserving the exact
    /// orientation, color conversion, and compositing behavior of preview.
    pub(crate) fn asset_frame(
        &mut self,
        path: &Path,
        seconds: f64,
        max_width: u32,
        max_height: u32,
    ) -> Result<ImagePart, String> {
        let (max_width, max_height) = vision_dimensions(max_width, max_height)?;
        validate_seconds(seconds)?;
        let asset_name = safe_file_name(path);

        let probe = cutlass_decoder::probe(path).map_err(|error| {
            format!(
                "could not inspect asset {asset_name}: {}",
                redact_asset_path(path, &error)
            )
        })?;
        if probe.width == 0 || probe.height == 0 {
            let kind = if probe.has_audio {
                "audio-only"
            } else {
                "nonvisual"
            };
            return Err(format!(
                "asset {asset_name} is {kind}; choose a video or still image"
            ));
        }

        let mut source = if probe.is_image {
            MediaSource::image(path, probe.width, probe.height)
        } else {
            MediaSource::new(
                path,
                probe.width,
                probe.height,
                probe.frame_rate,
                probe.frame_count,
                probe.has_audio,
            )
        };
        // Scratch projects use one stable media-id namespace. This keeps a
        // repeated frame/strip request for the same asset on one warm decoder
        // instead of allocating a fresh cache key for every call.
        source.id = MediaId::from_raw(1);
        let tick = seconds_to_tick(seconds, source.frame_rate, source.duration.value).map_err(
            |error| format!("could not select a frame from asset {asset_name}: {error}"),
        )?;
        let (project, frame_time) = scratch_project(source, tick).map_err(|error| {
            format!("could not prepare asset {asset_name} for rendering: {error}")
        })?;

        // Do not decode directly here: native decoders may return NV12 and
        // encoded orientation metadata. The scratch project guarantees exact
        // parity with the editor's project renderer.
        self.sync_media_catalog(&project)?;
        let image = self
            .renderer()?
            .render_frame_fit(&project, frame_time, max_width, max_height)
            .map_err(|error| {
                format!(
                    "could not render asset {asset_name} at {:.2}s: {}",
                    frame_time.seconds(),
                    redact_asset_path(path, &error)
                )
            })?;
        let png = cutlass_render::encode_png(&image)
            .map_err(|error| format!("could not encode asset frame as PNG: {error}"))?;

        Ok(ImagePart::png(png, asset_label(path, frame_time)))
    }

    fn renderer(&mut self) -> Result<&mut Renderer, String> {
        if self.renderer.is_none() {
            let renderer = Renderer::new_headless()
                .map_err(|error| format!("vision renderer is unavailable: {error}"))?;
            self.renderer = Some(renderer);
        }
        self.renderer
            .as_mut()
            .ok_or_else(|| "vision renderer is unavailable".to_string())
    }

    fn sync_media_catalog(&mut self, project: &Project) -> Result<(), String> {
        let next = media_catalog(project);
        if self.media_catalog != next {
            self.renderer()?.reset_media_sources();
            self.media_catalog = next;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaCatalogEntry {
    id: u64,
    path: PathBuf,
    width: u32,
    height: u32,
    frame_rate: Rational,
    duration_ticks: i64,
    is_image: bool,
}

fn media_catalog(project: &Project) -> Vec<MediaCatalogEntry> {
    let mut catalog: Vec<_> = project
        .media_iter()
        .map(|source| MediaCatalogEntry {
            id: source.id.raw(),
            path: source.path.clone(),
            width: source.width,
            height: source.height,
            frame_rate: source.frame_rate,
            duration_ticks: source.duration.value,
            is_image: source.is_image,
        })
        .collect();
    catalog.sort_unstable_by_key(|source| source.id);
    catalog
}

/// Reject absent dimensions, then bound render/readback work to a small,
/// model-friendly image without allowing callers to request tiny edge cases.
pub(crate) fn vision_dimensions(max_width: u32, max_height: u32) -> Result<(u32, u32), String> {
    if max_width == 0 || max_height == 0 {
        return Err(format!(
            "vision frame width and height must be greater than zero (got {max_width}x{max_height})"
        ));
    }
    Ok((
        max_width.clamp(MIN_VISION_EDGE, MAX_VISION_EDGE),
        max_height.clamp(MIN_VISION_EDGE, MAX_VISION_EDGE),
    ))
}

/// Snap non-negative seconds to the nearest exact frame tick and clamp it to
/// the final available frame. A zero-duration timeline resolves to tick zero,
/// which lets an empty project render its canvas without `duration - 1`
/// underflow; source construction rejects zero-duration visual media.
pub(crate) fn seconds_to_tick(
    seconds: f64,
    rate: Rational,
    duration_ticks: i64,
) -> Result<i64, String> {
    validate_seconds(seconds)?;
    if !rate.is_valid() {
        return Err(format!(
            "frame rate must be positive (got {}/{})",
            rate.num, rate.den
        ));
    }
    if duration_ticks <= 0 {
        return Ok(0);
    }

    let last_tick = duration_ticks.saturating_sub(1);
    let raw_tick = seconds * f64::from(rate.num) / f64::from(rate.den);
    if !raw_tick.is_finite() {
        // A finite but enormous request overflowed only during conversion; it
        // is unambiguously beyond the source and therefore clamps to its tail.
        return Ok(last_tick);
    }
    let snapped = raw_tick.round();
    if snapped <= 0.0 {
        Ok(0)
    } else if snapped >= last_tick as f64 {
        Ok(last_tick)
    } else {
        Ok(snapped as i64)
    }
}

/// Build the proven thumbnail-style scratch project: one main video lane and
/// one clip spanning the complete source. The returned time is defensively
/// clamped even when a caller supplies a raw tick.
pub(crate) fn scratch_project(
    source: MediaSource,
    requested_tick: i64,
) -> Result<(Project, RationalTime), String> {
    if source.width == 0 || source.height == 0 {
        return if source.is_audio_only() {
            Err("source is audio-only and has no visual frame".to_string())
        } else {
            Err(format!(
                "source has no visual frame (reported {}x{})",
                source.width, source.height
            ))
        };
    }
    if !source.frame_rate.is_valid() {
        return Err(format!(
            "source frame rate must be positive (got {}/{})",
            source.frame_rate.num, source.frame_rate.den
        ));
    }
    let duration_ticks = source.duration.value;
    if duration_ticks <= 0 {
        return Err("visual source has no frames to render".to_string());
    }

    let rate = source.frame_rate;
    let source_range = source.full_range();
    let actual_tick = requested_tick.clamp(0, duration_ticks.saturating_sub(1));
    let mut project = Project::new("agent vision asset", rate);
    let media = project.add_media(source);
    let track = project.add_track(TrackKind::Video, "Media");
    project
        .add_clip(track, media, source_range, RationalTime::new(0, rate))
        .map_err(|error| format!("could not build vision scratch clip: {error}"))?;

    Ok((project, RationalTime::new(actual_tick, rate)))
}

/// Provider-facing asset label. Only the final path component is exposed, and
/// controls/oversized synthetic names are contained before entering a prompt.
pub(crate) fn asset_label(path: &Path, frame_time: RationalTime) -> String {
    format!("{} at {:.2}s", safe_file_name(path), frame_time.seconds())
}

fn validate_seconds(seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() {
        return Err("frame time must be a finite number".to_string());
    }
    if seconds < 0.0 {
        return Err(format!("frame time must not be negative (got {seconds}s)"));
    }
    Ok(())
}

fn safe_file_name(path: &Path) -> String {
    let Some(name) = path.file_name() else {
        return "asset".to_string();
    };
    let mut safe = String::with_capacity(name.len().min(MAX_LABEL_FILE_NAME_CHARS));
    for character in name
        .to_string_lossy()
        .chars()
        .take(MAX_LABEL_FILE_NAME_CHARS)
    {
        safe.push(if character.is_control() {
            '\u{fffd}'
        } else {
            character
        });
    }
    if safe.is_empty() {
        "asset".to_string()
    } else {
        safe
    }
}

fn redact_asset_path(path: &Path, error: &dyn std::fmt::Display) -> String {
    let detail = error.to_string();
    let full_path = path.to_string_lossy();
    if full_path.is_empty() {
        detail
    } else {
        detail.replace(full_path.as_ref(), &safe_file_name(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_catalog_is_ordered_and_detects_relinks() {
        let mut project = Project::new("catalog", Rational::FPS_24);
        let second = project.add_media(MediaSource::new(
            "/private/b.mov",
            1920,
            1080,
            Rational::FPS_24,
            48,
            false,
        ));
        let first = project.add_media(MediaSource::image("/private/a.png", 800, 600));

        let before = media_catalog(&project);
        assert!(before.windows(2).all(|pair| pair[0].id < pair[1].id));

        project.media_mut(first).unwrap().path = "/replacement/a.png".into();
        let after = media_catalog(&project);
        assert_ne!(before, after);
        assert!(
            after
                .iter()
                .any(|source| source.id == second.raw() && source.path.ends_with("b.mov"))
        );
    }

    #[test]
    fn provider_errors_redact_full_asset_paths() {
        let path = Path::new("/private/customer-alpha/unreleased/take.mov");
        let detail = format!("decoder could not open {}", path.display());
        let redacted = redact_asset_path(path, &detail);
        assert_eq!(redacted, "decoder could not open take.mov");
        assert!(!redacted.contains("customer-alpha"));
    }
}
