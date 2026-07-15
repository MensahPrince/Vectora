//! Derived per-clip capabilities: what inspector sections and context-menu
//! actions apply to a clip on a given lane.
//!
//! This is **not** persisted — it is computed from [`Clip`] + [`TrackKind`]
//! (+ project context for extract-audio) and mirrors the engine's edit-command
//! rejection rules. The desktop UI projects these flags onto the Slint clip
//! model; the AI agent can use the same descriptor to pre-validate commands
//! before dispatching them.

use crate::clip::{Clip, ClipSource, Generator};
use crate::project::Project;
use crate::track::TrackKind;

/// What a clip supports for inspector panels and context-menu actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClipCapabilities {
    // --- inspector sections -------------------------------------------------
    /// Spatial transform (position, scale, rotation, opacity).
    pub has_transform: bool,
    /// Crop and flip framing.
    pub has_crop: bool,
    /// Volume, fades, denoise, duck, beats — the audio inspector block.
    pub has_audio: bool,
    /// Constant speed, reverse, and speed ramps (media-backed only).
    pub has_speed: bool,
    /// Text content and styling.
    pub has_text: bool,
    /// Shape size and fill (generator shapes on sticker lanes).
    pub has_shape: bool,
    /// Effect chain sliders.
    pub has_effects: bool,
    /// Filter / adjustment / look properties (mask, chroma, grade).
    pub has_filter_adjust: bool,

    // --- context-menu / edit actions ----------------------------------------
    /// Split at the playhead (all clip kinds).
    pub can_split: bool,
    /// Toggle reverse playback (`SetClipSpeed` with flipped `reversed`).
    pub can_reverse: bool,
    /// Delete and close the gap on the clip's lane (`RippleDelete`).
    pub can_ripple_delete: bool,
    /// CapCut "extract audio": detach the video clip's sound onto a linked
    /// audio-lane companion (same media, no new library asset).
    pub can_extract_audio: bool,
}

/// A timeline edit the UI or agent may offer for a selected clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipAction {
    Split,
    Copy,
    Duplicate,
    Delete,
    RippleDelete,
    Reverse,
    ExtractAudio,
}

impl ClipCapabilities {
    /// Derive capabilities from the clip's content, the lane it sits on, and
    /// project context (media `has_audio`, already-detached link state).
    pub fn for_clip(project: &Project, clip: &Clip, kind: TrackKind) -> Self {
        let is_visual = kind.is_visual();
        let is_media = clip.source_range().is_some();

        let (has_text, has_shape) = match &clip.content {
            ClipSource::Generated(Generator::Text { .. }) => (true, false),
            ClipSource::Generated(Generator::Shape { .. }) => (false, true),
            _ => (false, false),
        };

        let media_is_video_with_audio = match &clip.content {
            ClipSource::Media { media, .. } => project
                .media(*media)
                .is_some_and(|media| media.kind() == crate::MediaKind::Video && media.has_audio),
            ClipSource::Generated(_) => false,
        };
        let can_extract_audio = kind == TrackKind::Video
            && media_is_video_with_audio
            && !project.timeline().detached_to_audio_lane(clip.id);

        Self {
            has_transform: is_visual,
            has_crop: is_visual,
            has_audio: kind == TrackKind::Audio,
            has_speed: is_media,
            has_text,
            has_shape,
            has_effects: is_visual,
            has_filter_adjust: is_visual,
            can_split: true,
            can_reverse: is_media,
            can_ripple_delete: true,
            can_extract_audio,
        }
    }

    /// Whether this clip kind allows `action`. Selection-scoped actions
    /// (`Copy`, `Duplicate`, `Delete`) are always permitted when a clip is
    /// selected — gating happens UI-side on selection emptiness.
    pub fn allows(self, action: ClipAction) -> bool {
        match action {
            ClipAction::Split => self.can_split,
            ClipAction::Reverse => self.can_reverse,
            ClipAction::RippleDelete => self.can_ripple_delete,
            ClipAction::ExtractAudio => self.can_extract_audio,
            ClipAction::Copy | ClipAction::Duplicate | ClipAction::Delete => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::{Clip, Generator, Shape};
    use crate::ids::{LinkId, MediaId};
    use crate::media::MediaSource;
    use crate::project::Project;
    use crate::time::{Rational, TimeRange};
    use crate::track::Track;

    const R24: Rational = Rational::FPS_24;

    fn tr(start: i64, duration: i64) -> TimeRange {
        TimeRange::at_rate(start, duration, R24)
    }

    fn project_with_media(has_audio: bool) -> (Project, MediaId) {
        let mut project = Project::new("caps", R24);
        let media = project.add_media(MediaSource::new(
            "/tmp/caps.mp4",
            1920,
            1080,
            R24,
            480,
            has_audio,
        ));
        (project, media)
    }

    fn place_media(project: &mut Project, media: MediaId, kind: TrackKind) -> Clip {
        let track = project
            .timeline_mut()
            .add_track(Track::new(kind, format!("{kind:?}")));
        let clip = Clip::from_media(media, tr(0, 48), tr(0, 48));
        let id = project
            .timeline_mut()
            .add_clip(track, clip)
            .expect("place clip");
        project.clip(id).expect("clip").clone()
    }

    fn assert_caps(project: &Project, clip: &Clip, kind: TrackKind, expect: ClipCapabilities) {
        assert_eq!(ClipCapabilities::for_clip(project, clip, kind), expect);
    }

    #[test]
    fn media_video_clip_caps() {
        let (mut project, media) = project_with_media(true);
        let clip = place_media(&mut project, media, TrackKind::Video);
        assert_caps(
            &project,
            &clip,
            TrackKind::Video,
            ClipCapabilities {
                has_transform: true,
                has_crop: true,
                has_audio: false,
                has_speed: true,
                has_text: false,
                has_shape: false,
                has_effects: true,
                has_filter_adjust: true,
                can_split: true,
                can_reverse: true,
                can_ripple_delete: true,
                can_extract_audio: true,
            },
        );
    }

    #[test]
    fn media_video_without_audio_cannot_extract() {
        let (mut project, media) = project_with_media(false);
        let clip = place_media(&mut project, media, TrackKind::Video);
        let caps = ClipCapabilities::for_clip(&project, &clip, TrackKind::Video);
        assert!(!caps.can_extract_audio);
    }

    #[test]
    fn nonvideo_media_cannot_extract_even_with_an_audio_flag() {
        let (mut project, media) = project_with_media(true);
        project.media_mut(media).unwrap().is_image = true;
        let clip = place_media(&mut project, media, TrackKind::Video);
        let caps = ClipCapabilities::for_clip(&project, &clip, TrackKind::Video);
        assert!(!caps.can_extract_audio);
    }

    #[test]
    fn media_audio_clip_caps() {
        let (mut project, media) = project_with_media(true);
        let clip = place_media(&mut project, media, TrackKind::Audio);
        assert_caps(
            &project,
            &clip,
            TrackKind::Audio,
            ClipCapabilities {
                has_transform: false,
                has_crop: false,
                has_audio: true,
                has_speed: true,
                has_text: false,
                has_shape: false,
                has_effects: false,
                has_filter_adjust: false,
                can_split: true,
                can_reverse: true,
                can_ripple_delete: true,
                can_extract_audio: false,
            },
        );
    }

    #[test]
    fn text_clip_caps() {
        let project = Project::new("text", R24);
        let clip = Clip::generated(Generator::text("Hello"), tr(0, 48));
        assert_caps(
            &project,
            &clip,
            TrackKind::Text,
            ClipCapabilities {
                has_transform: true,
                has_crop: true,
                has_audio: false,
                has_speed: false,
                has_text: true,
                has_shape: false,
                has_effects: true,
                has_filter_adjust: true,
                can_split: true,
                can_reverse: false,
                can_ripple_delete: true,
                can_extract_audio: false,
            },
        );
    }

    #[test]
    fn shape_clip_caps() {
        let project = Project::new("shape", R24);
        let clip = Clip::generated(
            Generator::shape(Shape::Rectangle, [255, 0, 0, 255]),
            tr(0, 48),
        );
        assert_caps(
            &project,
            &clip,
            TrackKind::Sticker,
            ClipCapabilities {
                has_transform: true,
                has_crop: true,
                has_audio: false,
                has_speed: false,
                has_text: false,
                has_shape: true,
                has_effects: true,
                has_filter_adjust: true,
                can_split: true,
                can_reverse: false,
                can_ripple_delete: true,
                can_extract_audio: false,
            },
        );
    }

    #[test]
    fn solid_and_adjustment_caps() {
        let project = Project::new("gen", R24);
        let solid = Clip::generated(
            Generator::SolidColor {
                rgba: [0, 0, 0, 255],
            },
            tr(0, 24),
        );
        assert_caps(
            &project,
            &solid,
            TrackKind::Sticker,
            ClipCapabilities {
                has_transform: true,
                has_crop: true,
                has_effects: true,
                has_filter_adjust: true,
                can_split: true,
                can_ripple_delete: true,
                ..Default::default()
            },
        );

        let adj = Clip::generated(Generator::Adjustment, tr(0, 24));
        assert_caps(
            &project,
            &adj,
            TrackKind::Adjustment,
            ClipCapabilities {
                has_transform: true,
                has_crop: true,
                has_effects: true,
                has_filter_adjust: true,
                can_split: true,
                can_reverse: false,
                can_ripple_delete: true,
                ..Default::default()
            },
        );
    }

    #[test]
    fn extract_audio_disabled_once_detached() {
        let (mut project, media) = project_with_media(true);
        let video = place_media(&mut project, media, TrackKind::Video);
        let audio_track = project
            .timeline_mut()
            .add_track(Track::new(TrackKind::Audio, "A1"));
        let companion = Clip::from_media(media, video.source_range().unwrap(), video.timeline);
        let audio_id = project
            .timeline_mut()
            .add_clip(audio_track, companion)
            .expect("audio companion");
        let link = LinkId::next();
        project.timeline_mut().clip_mut(video.id).unwrap().link = Some(link);
        project.timeline_mut().clip_mut(audio_id).unwrap().link = Some(link);

        let caps =
            ClipCapabilities::for_clip(&project, project.clip(video.id).unwrap(), TrackKind::Video);
        assert!(!caps.can_extract_audio);
        assert!(!project.timeline().carries_own_audio(video.id));
    }

    #[test]
    fn allows_action_matches_flags() {
        let (mut project, media) = project_with_media(true);
        let clip = place_media(&mut project, media, TrackKind::Video);
        let video = ClipCapabilities::for_clip(&project, &clip, TrackKind::Video);
        assert!(video.allows(ClipAction::Reverse));
        assert!(video.allows(ClipAction::Split));
        assert!(video.allows(ClipAction::ExtractAudio));

        let text_project = Project::new("text", R24);
        let text = ClipCapabilities::for_clip(
            &text_project,
            &Clip::generated(Generator::text("Hi"), tr(0, 24)),
            TrackKind::Text,
        );
        assert!(!text.allows(ClipAction::Reverse));
        assert!(!text.allows(ClipAction::ExtractAudio));
        assert!(text.allows(ClipAction::Copy));
    }
}
