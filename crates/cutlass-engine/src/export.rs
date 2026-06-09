//! Timeline → composited RGBA frames → H.264 MP4.

use std::path::Path;

use cutlass_cache::FrameCache;
use cutlass_compositor::{Compositor, GpuContext};
use cutlass_encoder::{ExportConfig, ExportStats, VideoExport};
use cutlass_models::{Project, RationalTime};
use tracing::info;

use crate::composite::composite_canvas_size;
use crate::decoder_pool::DecoderPool;
use crate::error::EngineError;
use crate::preview;

/// Build encoder settings from the project timeline rate and composite canvas.
pub fn export_config_for(project: &Project) -> Result<ExportConfig, EngineError> {
    let (width, height) = composite_canvas_size(project);
    let rate = project.timeline().frame_rate;
    if !rate.is_valid() {
        return Err(EngineError::Export("invalid timeline frame rate".into()));
    }
    Ok(ExportConfig {
        width,
        height,
        frame_rate_num: rate.num,
        frame_rate_den: rate.den,
        ..Default::default()
    })
}

/// Composite every timeline frame `0..duration` and mux to `output`.
pub fn export_timeline(
    project: &Project,
    cache: &FrameCache,
    pool: &mut DecoderPool,
    gpu: &GpuContext,
    compositor: &mut Compositor,
    output: &Path,
) -> Result<ExportStats, EngineError> {
    let frame_count = project.timeline().duration().value;
    if frame_count <= 0 {
        return Err(EngineError::Export("timeline has no content to export".into()));
    }

    let config = export_config_for(project)?;
    let rate = project.timeline().frame_rate;
    let mut sink = VideoExport::create(output, config)?;

    info!(
        frames = frame_count,
        width = config.width,
        height = config.height,
        path = %output.display(),
        "exporting timeline"
    );

    for tick in 0..frame_count {
        let time = RationalTime::new(tick, rate);
        let frame = preview::get_frame(project, cache, pool, gpu, compositor, time)?;
        if frame.width != config.width || frame.height != config.height {
            return Err(EngineError::Export(format!(
                "composite size {}x{} does not match export {}x{}",
                frame.width, frame.height, config.width, config.height
            )));
        }
        sink.push_rgba(&frame.bytes)?;
    }

    sink.finish().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cutlass_models::{Rational, TrackKind};

    #[test]
    fn export_config_uses_timeline_rate_and_canvas_fallback() {
        let project = Project::new("test", Rational::FPS_24);
        let cfg = export_config_for(&project).unwrap();
        assert_eq!(cfg.width, 1920);
        assert_eq!(cfg.height, 1080);
        assert_eq!(cfg.frame_rate_num, 24);
        assert_eq!(cfg.frame_rate_den, 1);
    }

    #[test]
    fn export_config_matches_media_dimensions() {
        let mut project = Project::new("test", Rational::FPS_24);
        let media_id = project.add_media(cutlass_models::MediaSource::new(
            "/tmp/x.mp4",
            1280,
            720,
            Rational::FPS_24,
            100,
            false,
        ));
        let track = project.add_track(TrackKind::Video, "V1");
        project
            .add_clip(
                track,
                media_id,
                cutlass_models::TimeRange::at_rate(0, 48, Rational::FPS_24),
                RationalTime::new(0, Rational::FPS_24),
            )
            .unwrap();
        let cfg = export_config_for(&project).unwrap();
        assert_eq!(cfg.width, 1280);
        assert_eq!(cfg.height, 720);
    }
}
