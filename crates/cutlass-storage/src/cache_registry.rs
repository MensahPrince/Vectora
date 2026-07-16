//! Cache identifiers and the static registry of cache descriptors.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

/// Stable identifiers for every cache currently owned by Cutlass.
///
/// Variant order is the registry order. The string forms are persisted API:
/// changing one requires a migration rather than a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum CacheId {
    /// Decoded frames retained in process memory.
    PreviewFrames,
    /// Library thumbnail images retained in process memory.
    LibraryThumbnails,
    /// Timeline filmstrip images retained in process memory.
    TimelineFilmstrips,
    /// Timeline waveform samples retained in process memory.
    TimelineWaveforms,
    /// Generated lower-resolution media proxies.
    Proxies,
    /// Regenerable media-analysis state such as moments and transcripts.
    Analysis,
    /// Downloaded AI model weights for transcription, vision, and embeddings.
    AiModels,
    /// Remotely downloaded source assets.
    Download,
    /// Downloaded asset-catalog data.
    Catalog,
    /// Downloaded lookup-table assets.
    Luts,
    /// Downloaded Lottie animation assets.
    Lottie,
    /// Downloaded template assets.
    Templates,
}

impl CacheId {
    /// Every cache identifier in deterministic registry order.
    pub const ALL: [Self; 12] = [
        Self::PreviewFrames,
        Self::LibraryThumbnails,
        Self::TimelineFilmstrips,
        Self::TimelineWaveforms,
        Self::Proxies,
        Self::Analysis,
        Self::AiModels,
        Self::Download,
        Self::Catalog,
        Self::Luts,
        Self::Lottie,
        Self::Templates,
    ];

    /// Return the exact stable key used in settings and tool payloads.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreviewFrames => "preview_frames",
            Self::LibraryThumbnails => "library_thumbnails",
            Self::TimelineFilmstrips => "timeline_filmstrips",
            Self::TimelineWaveforms => "timeline_waveforms",
            Self::Proxies => "proxies",
            Self::Analysis => "analysis",
            Self::AiModels => "ai_models",
            Self::Download => "download",
            Self::Catalog => "catalog",
            Self::Luts => "luts",
            Self::Lottie => "lottie",
            Self::Templates => "templates",
        }
    }

    /// Parse an exact stable cache key.
    pub fn parse(key: &str) -> Result<Self, ParseCacheIdError> {
        key.parse()
    }

    /// Return this cache's static descriptor.
    pub const fn descriptor(self) -> &'static CacheDescriptor {
        &CACHE_REGISTRY[self as usize]
    }
}

impl fmt::Display for CacheId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CacheId {
    type Err = ParseCacheIdError;

    fn from_str(key: &str) -> Result<Self, Self::Err> {
        match key {
            "preview_frames" => Ok(Self::PreviewFrames),
            "library_thumbnails" => Ok(Self::LibraryThumbnails),
            "timeline_filmstrips" => Ok(Self::TimelineFilmstrips),
            "timeline_waveforms" => Ok(Self::TimelineWaveforms),
            "proxies" => Ok(Self::Proxies),
            "analysis" => Ok(Self::Analysis),
            "ai_models" => Ok(Self::AiModels),
            "download" => Ok(Self::Download),
            "catalog" => Ok(Self::Catalog),
            "luts" => Ok(Self::Luts),
            "lottie" => Ok(Self::Lottie),
            "templates" => Ok(Self::Templates),
            _ => Err(ParseCacheIdError),
        }
    }
}

/// Error returned when a string is not an exact stable [`CacheId`] key.
///
/// The input is intentionally not retained, keeping provider-facing errors
/// bounded even when a caller supplies a hostile string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseCacheIdError;

impl fmt::Display for ParseCacheIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown cache id")
    }
}

impl Error for ParseCacheIdError {}

/// Where a cache's bytes live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheKind {
    /// Process memory; there is no filesystem path.
    Memory,
    /// A directory beneath the storage root or an absolute override.
    Disk,
}

/// Recovery semantics for cached data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheTier {
    /// Derived data that can be regenerated locally.
    Disposable,
    /// Data that can be downloaded again.
    Redownloadable,
    /// Irreplaceable user-owned data.
    ///
    /// No active clearable cache currently uses this tier. The variant exists
    /// so future registries cannot silently collapse user data into a cache.
    UserData,
}

/// Static metadata for one cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheDescriptor {
    /// Stable identifier.
    pub id: CacheId,
    /// Human-readable label.
    pub label: &'static str,
    /// Memory or disk storage.
    pub kind: CacheKind,
    /// Recovery semantics.
    pub tier: CacheTier,
    /// Relative directory beneath [`StorageLayout::root`] for disk caches.
    ///
    /// Memory descriptors always use `None`.
    pub default_relative: Option<&'static str>,
}

/// Complete cache registry in deterministic display order.
///
/// Projects, configuration, and agent sessions are intentionally absent:
/// they are not clearable caches.
pub static CACHE_REGISTRY: [CacheDescriptor; 12] = [
    CacheDescriptor {
        id: CacheId::PreviewFrames,
        label: "Preview frames",
        kind: CacheKind::Memory,
        tier: CacheTier::Disposable,
        default_relative: None,
    },
    CacheDescriptor {
        id: CacheId::LibraryThumbnails,
        label: "Library thumbnails",
        kind: CacheKind::Memory,
        tier: CacheTier::Disposable,
        default_relative: None,
    },
    CacheDescriptor {
        id: CacheId::TimelineFilmstrips,
        label: "Timeline filmstrips",
        kind: CacheKind::Memory,
        tier: CacheTier::Disposable,
        default_relative: None,
    },
    CacheDescriptor {
        id: CacheId::TimelineWaveforms,
        label: "Timeline waveforms",
        kind: CacheKind::Memory,
        tier: CacheTier::Disposable,
        default_relative: None,
    },
    CacheDescriptor {
        id: CacheId::Proxies,
        label: "Proxies",
        kind: CacheKind::Disk,
        tier: CacheTier::Disposable,
        default_relative: Some("proxies"),
    },
    CacheDescriptor {
        id: CacheId::Analysis,
        label: "Media analysis",
        kind: CacheKind::Disk,
        tier: CacheTier::Disposable,
        default_relative: Some("analysis"),
    },
    CacheDescriptor {
        id: CacheId::AiModels,
        label: "AI models",
        kind: CacheKind::Disk,
        tier: CacheTier::Redownloadable,
        default_relative: Some("ai-models"),
    },
    CacheDescriptor {
        id: CacheId::Download,
        label: "Downloads",
        kind: CacheKind::Disk,
        tier: CacheTier::Redownloadable,
        default_relative: Some("download-cache"),
    },
    CacheDescriptor {
        id: CacheId::Catalog,
        label: "Catalog",
        kind: CacheKind::Disk,
        tier: CacheTier::Redownloadable,
        default_relative: Some("catalog-cache"),
    },
    CacheDescriptor {
        id: CacheId::Luts,
        label: "LUTs",
        kind: CacheKind::Disk,
        tier: CacheTier::Redownloadable,
        default_relative: Some("luts"),
    },
    CacheDescriptor {
        id: CacheId::Lottie,
        label: "Lottie assets",
        kind: CacheKind::Disk,
        tier: CacheTier::Redownloadable,
        default_relative: Some("lottie"),
    },
    CacheDescriptor {
        id: CacheId::Templates,
        label: "Templates",
        kind: CacheKind::Disk,
        tier: CacheTier::Redownloadable,
        default_relative: Some("templates"),
    },
];

/// Return the complete cache registry in deterministic display order.
pub const fn cache_descriptors() -> &'static [CacheDescriptor] {
    &CACHE_REGISTRY
}

/// Look up a descriptor by typed identifier.
pub const fn cache_descriptor(id: CacheId) -> &'static CacheDescriptor {
    id.descriptor()
}

/// Look up a descriptor by exact stable key.
pub fn cache_descriptor_by_key(key: &str) -> Option<&'static CacheDescriptor> {
    CacheId::parse(key).ok().map(CacheId::descriptor)
}
