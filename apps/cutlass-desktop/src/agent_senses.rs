//! Standalone, read-only media senses for the editor agent.
//!
//! The agent/sandbox thread owns this service. Frame rendering is lazy and
//! private to that thread through [`AgentVision`], while the timeline map stays
//! CPU-only and deterministic.

use cutlass_ai::{HostToolSpec, ImagePart, ToolOutput, ToolTier};
use cutlass_models::{MediaId, MediaSource, Project};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::agent_vision::{AgentVision, MAX_VISION_EDGE};
use crate::timeline_map::{self, TimelineMapOptions};

const TIMELINE_MAP_TOOL: &str = "media_timeline_map";
const PREVIEW_FRAME_TOOL: &str = "media_preview_frame";
const ASSET_FRAME_TOOL: &str = "media_asset_frame";
const ASSET_STRIP_TOOL: &str = "media_asset_strip";

const DEFAULT_IMAGE_EDGE: u32 = 768;
const MIN_IMAGE_EDGE: u32 = 64;
const DEFAULT_MAP_WIDTH: u32 = 768;
const MIN_MAP_WIDTH: u32 = 320;
const MAX_MAP_WIDTH: u32 = 1024;
const DEFAULT_STRIP_FRAMES: u32 = 6;
pub(crate) const MAX_STRIP_FRAMES: u32 = 8;

/// Read-only media tools, intended to live for the lifetime of one agent
/// sandbox thread so repeated frame requests can reuse its private renderer.
pub(crate) struct AgentSenses {
    vision: AgentVision,
}

impl Default for AgentSenses {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentSenses {
    pub(crate) fn new() -> Self {
        Self {
            vision: AgentVision::new(),
        }
    }

    /// The complete Phase 1 media-sense surface.
    pub(crate) fn specs() -> Vec<HostToolSpec> {
        vec![
            HostToolSpec {
                name: TIMELINE_MAP_TOOL.to_string(),
                description: "Render a labeled schematic of timeline lanes, clips, markers, and an optional playhead for a bounded time window.".to_string(),
                parameters: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "start_seconds": {
                            "type": "number",
                            "minimum": 0,
                            "description": "Window start in project seconds (default 0)."
                        },
                        "end_seconds": {
                            "type": "number",
                            "minimum": 0,
                            "description": "Window end in project seconds; must be after start (default project end, with at least a one-second window)."
                        },
                        "playhead_seconds": {
                            "type": "number",
                            "minimum": 0,
                            "description": "Optional project playhead to label when it falls inside the window."
                        },
                        "width": {
                            "type": "integer",
                            "minimum": MIN_MAP_WIDTH,
                            "maximum": MAX_MAP_WIDTH,
                            "default": DEFAULT_MAP_WIDTH,
                            "description": "Output width in pixels."
                        }
                    }
                }),
                tier: ToolTier::ReadOnly,
            },
            HostToolSpec {
                name: PREVIEW_FRAME_TOOL.to_string(),
                description: "Render the composited project preview at a project time, defaulting to the editor's current playhead.".to_string(),
                parameters: preview_frame_schema(),
                tier: ToolTier::ReadOnly,
            },
            HostToolSpec {
                name: ASSET_FRAME_TOOL.to_string(),
                description: "Render one source frame from a media-pool item selected only by its project media ID.".to_string(),
                parameters: asset_frame_schema(),
                tier: ToolTier::ReadOnly,
            },
            HostToolSpec {
                name: ASSET_STRIP_TOOL.to_string(),
                description: "Render an ordered, bounded strip of source frames from a media-pool item, including both window endpoints when more than one frame is requested.".to_string(),
                parameters: asset_strip_schema(),
                tier: ToolTier::ReadOnly,
            },
        ]
    }

    /// Dispatch one registered media sense against the supplied immutable
    /// project snapshot.
    pub(crate) fn call(
        &mut self,
        project: &Project,
        default_playhead_seconds: f64,
        name: &str,
        arguments: &Value,
    ) -> Result<ToolOutput, String> {
        match name {
            TIMELINE_MAP_TOOL => self.timeline_map(project, arguments),
            PREVIEW_FRAME_TOOL => self.preview_frame(project, default_playhead_seconds, arguments),
            ASSET_FRAME_TOOL => self.asset_frame(project, arguments),
            ASSET_STRIP_TOOL => self.asset_strip(project, arguments),
            _ => Err(format!(
                "unknown media sense tool '{name}'; expected one of: {TIMELINE_MAP_TOOL}, \
                 {PREVIEW_FRAME_TOOL}, {ASSET_FRAME_TOOL}, {ASSET_STRIP_TOOL}"
            )),
        }
    }

    fn timeline_map(&mut self, project: &Project, arguments: &Value) -> Result<ToolOutput, String> {
        let request = parse_timeline_map_request(project, arguments)?;
        let image = timeline_map::render(
            project,
            TimelineMapOptions {
                width: request.width,
                start_seconds: request.start_seconds,
                end_seconds: Some(request.end_seconds),
                playhead_seconds: request.playhead_seconds,
            },
        )?;
        let png = cutlass_render::encode_png(&image)
            .map_err(|error| format!("could not encode timeline map as PNG: {error}"))?;
        let label = format!(
            "timeline map {:.2}s-{:.2}s",
            request.start_seconds, request.end_seconds
        );
        let image = ImagePart::png(png, label);
        let playhead = request.playhead_seconds.map_or_else(
            || "no playhead requested".to_string(),
            |seconds| format!("playhead {seconds:.2}s"),
        );
        let text = format!(
            "Timeline schematic '{}': {:.2}s-{:.2}s at {}px, {} lanes, {} clips, {playhead}.",
            image.label,
            request.start_seconds,
            request.end_seconds,
            request.width,
            project.timeline().track_count(),
            project.timeline().clip_count(),
        );

        Ok(ToolOutput {
            text,
            images: vec![image],
        })
    }

    fn preview_frame(
        &mut self,
        project: &Project,
        default_playhead_seconds: f64,
        arguments: &Value,
    ) -> Result<ToolOutput, String> {
        let request = parse_preview_request(arguments, default_playhead_seconds)?;
        let image = self.vision.project_frame(
            project,
            request.seconds,
            request.max_width,
            request.max_height,
            "project preview",
        )?;
        let text = format!(
            "Composited project frame '{}' (bounded to {}x{}px).",
            image.label, request.max_width, request.max_height
        );

        Ok(ToolOutput {
            text,
            images: vec![image],
        })
    }

    fn asset_frame(&mut self, project: &Project, arguments: &Value) -> Result<ToolOutput, String> {
        let request = parse_asset_frame_request(arguments)?;
        let source = resolve_visual_media(project, request.media_id)?;
        let image = self.vision.asset_frame(
            source.path(),
            request.seconds,
            request.max_width,
            request.max_height,
        )?;
        let text = format!(
            "Media #{} source frame '{}' (bounded to {}x{}px).",
            request.media_id, image.label, request.max_width, request.max_height
        );

        Ok(ToolOutput {
            text,
            images: vec![image],
        })
    }

    fn asset_strip(&mut self, project: &Project, arguments: &Value) -> Result<ToolOutput, String> {
        // Parse and validate every caller-controlled field, then resolve and
        // reject nonvisual media before the first decoder/GPU operation.
        let request = parse_strip_request(arguments)?;
        let source = resolve_visual_media(project, request.media_id)?;
        let samples = strip_sample_times(
            request.start_seconds,
            request.end_seconds,
            request.count,
            source.is_image,
        )?;

        let mut images = Vec::with_capacity(samples.len());
        for seconds in samples {
            images.push(self.vision.asset_frame(
                source.path(),
                seconds,
                request.max_width,
                request.max_height,
            )?);
        }

        let labels = images
            .iter()
            .map(|image| image.label.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let deduplicated = if source.is_image {
            ", still image deduplicated"
        } else {
            ""
        };
        let text = format!(
            "Rendered {} ordered source frame(s) for media #{} over {:.2}s-{:.2}s{}; labels: {}.",
            images.len(),
            request.media_id,
            request.start_seconds,
            request.end_seconds,
            deduplicated,
            labels
        );

        Ok(ToolOutput { text, images })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PreviewFrameRequest {
    pub seconds: f64,
    pub max_width: u32,
    pub max_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StripRequest {
    pub media_id: u64,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub count: u32,
    pub max_width: u32,
    pub max_height: u32,
}

#[derive(Debug, Clone, Copy)]
struct TimelineMapRequest {
    width: u32,
    start_seconds: f64,
    end_seconds: f64,
    playhead_seconds: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct AssetFrameRequest {
    media_id: u64,
    seconds: f64,
    max_width: u32,
    max_height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimelineMapArguments {
    #[serde(default, deserialize_with = "optional_non_null")]
    start_seconds: Option<f64>,
    #[serde(default, deserialize_with = "optional_non_null")]
    end_seconds: Option<f64>,
    #[serde(default, deserialize_with = "optional_non_null")]
    playhead_seconds: Option<f64>,
    #[serde(default, deserialize_with = "optional_non_null")]
    width: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewFrameArguments {
    #[serde(default, deserialize_with = "optional_non_null")]
    seconds: Option<f64>,
    #[serde(default, deserialize_with = "optional_non_null")]
    max_width: Option<u32>,
    #[serde(default, deserialize_with = "optional_non_null")]
    max_height: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetFrameArguments {
    media_id: u64,
    #[serde(default, deserialize_with = "optional_non_null")]
    seconds: Option<f64>,
    #[serde(default, deserialize_with = "optional_non_null")]
    max_width: Option<u32>,
    #[serde(default, deserialize_with = "optional_non_null")]
    max_height: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetStripArguments {
    media_id: u64,
    start_seconds: f64,
    end_seconds: f64,
    #[serde(default, deserialize_with = "optional_non_null")]
    count: Option<u32>,
    #[serde(default, deserialize_with = "optional_non_null")]
    max_width: Option<u32>,
    #[serde(default, deserialize_with = "optional_non_null")]
    max_height: Option<u32>,
}

fn parse_timeline_map_request(
    project: &Project,
    arguments: &Value,
) -> Result<TimelineMapRequest, String> {
    let arguments: TimelineMapArguments = parse_object(TIMELINE_MAP_TOOL, arguments)?;
    let start_seconds = arguments.start_seconds.unwrap_or(0.0);
    validate_time("timeline-map start", start_seconds)?;
    let end_seconds = match arguments.end_seconds {
        Some(end_seconds) => {
            validate_time("timeline-map end", end_seconds)?;
            if end_seconds <= start_seconds {
                return Err("timeline-map end must be after start".to_string());
            }
            end_seconds
        }
        None => {
            let minimum_end = start_seconds + 1.0;
            if !minimum_end.is_finite() || minimum_end <= start_seconds {
                return Err("timeline-map start is too large for a one-second window".to_string());
            }
            let project_end = project.timeline().duration().seconds();
            if project_end.is_finite() {
                project_end.max(minimum_end)
            } else {
                minimum_end
            }
        }
    };
    if let Some(playhead_seconds) = arguments.playhead_seconds {
        validate_time("timeline-map playhead", playhead_seconds)?;
    }
    let width = arguments.width.unwrap_or(DEFAULT_MAP_WIDTH);
    validate_dimension("timeline-map width", width, MIN_MAP_WIDTH, MAX_MAP_WIDTH)?;

    Ok(TimelineMapRequest {
        width,
        start_seconds,
        end_seconds,
        playhead_seconds: arguments.playhead_seconds,
    })
}

/// Parse the project-frame request without touching a renderer. Keeping this
/// pure makes the caller-supplied playhead default independently testable.
pub(crate) fn parse_preview_request(
    arguments: &Value,
    default_playhead_seconds: f64,
) -> Result<PreviewFrameRequest, String> {
    let arguments: PreviewFrameArguments = parse_object(PREVIEW_FRAME_TOOL, arguments)?;
    let seconds = arguments.seconds.unwrap_or(default_playhead_seconds);
    validate_time("preview frame time", seconds)?;
    let max_width = arguments.max_width.unwrap_or(DEFAULT_IMAGE_EDGE);
    let max_height = arguments.max_height.unwrap_or(DEFAULT_IMAGE_EDGE);
    validate_image_dimensions(max_width, max_height)?;

    Ok(PreviewFrameRequest {
        seconds,
        max_width,
        max_height,
    })
}

fn parse_asset_frame_request(arguments: &Value) -> Result<AssetFrameRequest, String> {
    let arguments: AssetFrameArguments = parse_object(ASSET_FRAME_TOOL, arguments)?;
    validate_media_id(arguments.media_id)?;
    let seconds = arguments.seconds.unwrap_or(0.0);
    validate_time("asset frame time", seconds)?;
    let max_width = arguments.max_width.unwrap_or(DEFAULT_IMAGE_EDGE);
    let max_height = arguments.max_height.unwrap_or(DEFAULT_IMAGE_EDGE);
    validate_image_dimensions(max_width, max_height)?;

    Ok(AssetFrameRequest {
        media_id: arguments.media_id,
        seconds,
        max_width,
        max_height,
    })
}

/// Parse a strip request without starting the decoder or renderer.
pub(crate) fn parse_strip_request(arguments: &Value) -> Result<StripRequest, String> {
    let arguments: AssetStripArguments = parse_object(ASSET_STRIP_TOOL, arguments)?;
    validate_media_id(arguments.media_id)?;
    let count = arguments.count.unwrap_or(DEFAULT_STRIP_FRAMES);
    strip_sample_times(arguments.start_seconds, arguments.end_seconds, count, false)?;
    let max_width = arguments.max_width.unwrap_or(DEFAULT_IMAGE_EDGE);
    let max_height = arguments.max_height.unwrap_or(DEFAULT_IMAGE_EDGE);
    validate_image_dimensions(max_width, max_height)?;

    Ok(StripRequest {
        media_id: arguments.media_id,
        start_seconds: arguments.start_seconds,
        end_seconds: arguments.end_seconds,
        count,
        max_width,
        max_height,
    })
}

/// Return deterministic source-time samples. Multi-frame strips include both
/// endpoints; a one-frame strip samples the start. Stills always collapse to
/// one frame, but only after the caller's count/window has been validated.
pub(crate) fn strip_sample_times(
    start_seconds: f64,
    end_seconds: f64,
    count: u32,
    is_still: bool,
) -> Result<Vec<f64>, String> {
    validate_time("asset-strip start", start_seconds)?;
    validate_time("asset-strip end", end_seconds)?;
    if end_seconds <= start_seconds {
        return Err("asset-strip end must be after start".to_string());
    }
    if !(1..=MAX_STRIP_FRAMES).contains(&count) {
        return Err(format!(
            "asset-strip count must be between 1 and {MAX_STRIP_FRAMES} (got {count})"
        ));
    }
    if is_still || count == 1 {
        return Ok(vec![start_seconds]);
    }

    let denominator = f64::from(count - 1);
    let span = end_seconds - start_seconds;
    let mut samples = Vec::with_capacity(count as usize);
    for index in 0..count {
        samples.push(start_seconds + span * f64::from(index) / denominator);
    }
    if let Some(last) = samples.last_mut() {
        *last = end_seconds;
    }
    Ok(samples)
}

fn resolve_visual_media(project: &Project, raw_id: u64) -> Result<&MediaSource, String> {
    let source = project
        .media(MediaId::from_raw(raw_id))
        .ok_or_else(|| format!("unknown project media id {raw_id}"))?;
    if source.is_audio_only() {
        return Err(format!(
            "media #{raw_id} is audio-only; choose a video or still image"
        ));
    }
    if source.width == 0 || source.height == 0 {
        return Err(format!(
            "media #{raw_id} is nonvisual (reported {}x{}); choose a video or still image",
            source.width, source.height
        ));
    }
    Ok(source)
}

fn parse_object<T: DeserializeOwned>(tool: &str, arguments: &Value) -> Result<T, String> {
    if !arguments.is_object() {
        return Err(format!("arguments for '{tool}' must be a JSON object"));
    }
    serde_json::from_value(arguments.clone())
        .map_err(|error| format!("invalid arguments for '{tool}': {error}"))
}

fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn validate_media_id(media_id: u64) -> Result<(), String> {
    if media_id == 0 {
        return Err("media_id must be a positive integer".to_string());
    }
    Ok(())
}

fn validate_time(label: &str, seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() {
        return Err(format!("{label} must be finite"));
    }
    if seconds < 0.0 {
        return Err(format!("{label} must be non-negative (got {seconds}s)"));
    }
    Ok(())
}

fn validate_image_dimensions(max_width: u32, max_height: u32) -> Result<(), String> {
    validate_dimension("max_width", max_width, MIN_IMAGE_EDGE, MAX_VISION_EDGE)?;
    validate_dimension("max_height", max_height, MIN_IMAGE_EDGE, MAX_VISION_EDGE)
}

fn validate_dimension(label: &str, value: u32, minimum: u32, maximum: u32) -> Result<(), String> {
    if !(minimum..=maximum).contains(&value) {
        return Err(format!(
            "{label} must be between {minimum} and {maximum} pixels (got {value})"
        ));
    }
    Ok(())
}

fn preview_frame_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "seconds": {
                "type": "number",
                "minimum": 0,
                "description": "Project seconds; defaults to the caller-supplied playhead."
            },
            "max_width": {
                "type": "integer",
                "minimum": MIN_IMAGE_EDGE,
                "maximum": MAX_VISION_EDGE,
                "default": DEFAULT_IMAGE_EDGE
            },
            "max_height": {
                "type": "integer",
                "minimum": MIN_IMAGE_EDGE,
                "maximum": MAX_VISION_EDGE,
                "default": DEFAULT_IMAGE_EDGE
            }
        }
    })
}

fn asset_frame_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["media_id"],
        "properties": {
            "media_id": {
                "type": "integer",
                "minimum": 1,
                "description": "ID of an item already in this project's media pool."
            },
            "seconds": {
                "type": "number",
                "minimum": 0,
                "default": 0,
                "description": "Time in source-media seconds."
            },
            "max_width": {
                "type": "integer",
                "minimum": MIN_IMAGE_EDGE,
                "maximum": MAX_VISION_EDGE,
                "default": DEFAULT_IMAGE_EDGE
            },
            "max_height": {
                "type": "integer",
                "minimum": MIN_IMAGE_EDGE,
                "maximum": MAX_VISION_EDGE,
                "default": DEFAULT_IMAGE_EDGE
            }
        }
    })
}

fn asset_strip_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["media_id", "start_seconds", "end_seconds"],
        "properties": {
            "media_id": {
                "type": "integer",
                "minimum": 1,
                "description": "ID of an item already in this project's media pool."
            },
            "start_seconds": {
                "type": "number",
                "minimum": 0,
                "description": "Inclusive source-window start."
            },
            "end_seconds": {
                "type": "number",
                "minimum": 0,
                "description": "Inclusive source-window end; must be after start."
            },
            "count": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_STRIP_FRAMES,
                "default": DEFAULT_STRIP_FRAMES
            },
            "max_width": {
                "type": "integer",
                "minimum": MIN_IMAGE_EDGE,
                "maximum": MAX_VISION_EDGE,
                "default": DEFAULT_IMAGE_EDGE
            },
            "max_height": {
                "type": "integer",
                "minimum": MIN_IMAGE_EDGE,
                "maximum": MAX_VISION_EDGE,
                "default": DEFAULT_IMAGE_EDGE
            }
        }
    })
}
