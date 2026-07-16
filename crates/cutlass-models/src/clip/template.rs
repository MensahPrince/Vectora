use serde::{Deserialize, Serialize};

/// What kind of media a [`Replaceable`] template slot accepts, mirroring
/// CapCut's per-clip "video only" / "image only" restriction (plus an audio
/// variant for marking a swappable music/soundtrack clip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotMedia {
    /// Any visual media — a video clip or a still image.
    #[default]
    Any,
    /// Video clips only.
    VideoOnly,
    /// Still images only.
    ImageOnly,
    /// Audio only — marks a swappable music/soundtrack clip.
    AudioOnly,
}

impl SlotMedia {
    /// Whether a source of `kind` may fill a slot with this restriction.
    pub fn accepts(self, kind: crate::media::MediaKind) -> bool {
        use crate::media::MediaKind;
        match self {
            SlotMedia::Any => matches!(kind, MediaKind::Video | MediaKind::Image),
            SlotMedia::VideoOnly => kind == MediaKind::Video,
            SlotMedia::ImageOnly => kind == MediaKind::Image,
            SlotMedia::AudioOnly => kind == MediaKind::Audio,
        }
    }
}

/// Marks a [`Clip`] as a user-replaceable template slot (CapCut's "set
/// replaceable material clips"). The clip keeps its sample media so the
/// template previews like the author's video; applying the template swaps the
/// media in slot `order` while the slot's locked timeline duration, transform,
/// effects, and transitions are preserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replaceable {
    /// Fill order: slots are filled in ascending `order`, matching the
    /// sequence the user/agent picks media in.
    pub order: u32,
    /// Media-type restriction for this slot.
    #[serde(default)]
    pub accepts: SlotMedia,
    /// Optional author hint shown on the placeholder ("Your clip here"); also
    /// surfaced to the AI agent when auto-filling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Replaceable {
    /// A slot at `order` accepting any visual media.
    pub fn new(order: u32) -> Self {
        Self {
            order,
            accepts: SlotMedia::Any,
            label: None,
        }
    }

    /// Restrict the media type this slot accepts.
    pub fn with_accepts(mut self, accepts: SlotMedia) -> Self {
        self.accepts = accepts;
        self
    }

    /// Attach an author hint for the placeholder.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}
