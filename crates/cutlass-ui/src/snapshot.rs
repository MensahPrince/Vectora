//! Serializable project snapshot for cross-thread engine → UI updates.

use cutlass_models::{Clip, ClipSource, Generator, Project, TrackKind};

use crate::ids::{clip_id_to_str, project_id_to_str, track_id_to_str};

#[derive(Debug, Clone)]
pub struct ClipSnapshot {
    pub id: String,
    pub name: String,
    pub timeline_start: i32,
    pub source_start: i32,
    pub duration: i32,
    pub rate_num: i32,
    pub rate_den: i32,
}

#[derive(Debug, Clone)]
pub struct TrackSnapshot {
    pub id: String,
    pub name: String,
    pub kind: TrackKind,
    pub kind_index: usize,
    pub clips: Vec<ClipSnapshot>,
}

#[derive(Debug, Clone)]
pub struct ProjectSnapshot {
    pub id: String,
    pub title: String,
    pub fps_num: i32,
    pub fps_den: i32,
    pub width: f32,
    pub height: f32,
    pub tracks: Vec<TrackSnapshot>,
}

#[derive(Debug, Clone)]
pub struct FrameSnapshot {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ProjectSnapshot {
    pub fn from_engine(project: &Project) -> Self {
        let timeline = project.timeline();
        let fps = timeline.frame_rate;
        let (width, height) = canvas_size(project);
        let mut video_count = 0usize;
        let mut audio_count = 0usize;

        let tracks = timeline
            .tracks_ordered()
            .map(|track| {
                let kind_index = match track.kind {
                    TrackKind::Video => {
                        let i = video_count;
                        video_count += 1;
                        i
                    }
                    TrackKind::Audio => {
                        let i = audio_count;
                        audio_count += 1;
                        i
                    }
                };
                TrackSnapshot {
                    id: track_id_to_str(track.id),
                    name: track.name.clone(),
                    kind: track.kind,
                    kind_index,
                    clips: track
                        .clips_ordered()
                        .into_iter()
                        .map(|clip| clip_snapshot(project, clip))
                        .collect(),
                }
            })
            .collect();

        Self {
            id: project_id_to_str(project.id),
            title: project.name.clone(),
            fps_num: fps.num as i32,
            fps_den: fps.den as i32,
            width,
            height,
            tracks,
        }
    }
}

fn clip_snapshot(project: &Project, clip: &Clip) -> ClipSnapshot {
    let rate = clip.timeline.start.rate;
    let source_start = match &clip.content {
        ClipSource::Media { source, .. } => source.start.value,
        ClipSource::Generated(_) => 0,
    };

    ClipSnapshot {
        id: clip_id_to_str(clip.id),
        name: clip_name(project, clip),
        timeline_start: clip.timeline.start.value as i32,
        source_start: source_start as i32,
        duration: clip.timeline.duration.value as i32,
        rate_num: rate.num as i32,
        rate_den: rate.den as i32,
    }
}

fn clip_name(project: &Project, clip: &Clip) -> String {
    match &clip.content {
        ClipSource::Media { media, .. } => project
            .media(*media)
            .and_then(|m| m.path().file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("Clip {}", clip.id.raw())),
        ClipSource::Generated(generator) => match generator {
            Generator::Text { content } => content.clone(),
            Generator::SolidColor { .. } => "Solid".into(),
            Generator::Shape { .. } => "Shape".into(),
            Generator::Adjustment => "Adjustment".into(),
        },
    }
}

fn canvas_size(project: &Project) -> (f32, f32) {
    let mut max_w = 0u32;
    let mut max_h = 0u32;

    for track in project.timeline().tracks_ordered() {
        if track.kind != TrackKind::Video {
            continue;
        }
        for clip in track.clips() {
            if let Some(media_id) = clip.media()
                && let Some(media) = project.media(media_id)
            {
                max_w = max_w.max(media.width);
                max_h = max_h.max(media.height);
            }
        }
    }

    if max_w == 0 || max_h == 0 {
        (1920.0, 1080.0)
    } else {
        (max_w as f32, max_h as f32)
    }
}
