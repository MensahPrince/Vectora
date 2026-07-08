//! Bundled sticker catalog: the assets a [`Generator::Sticker`] may
//! reference, embedded into the binary at compile time.
//!
//! Mirrors the effects-catalog pattern ([`crate::effects`]): the catalog here
//! is the validation + UI source of truth (ids, display labels, intrinsic
//! sizes), and the encoded bytes ship inside the crate so the engine,
//! desktop, mobile, and Python bindings never resolve asset paths at
//! runtime. A render-side drift test decodes every entry and pins the
//! declared dimensions to the actual bytes.
//!
//! The starter pack is placeholder art generated in-repo; swapping in real
//! artwork only touches `assets/stickers/` and this table.
//!
//! [`Generator::Sticker`]: crate::Generator::Sticker

/// One bundled sticker: a stable id (what [`crate::Generator::Sticker`]
/// stores), a display label, the intrinsic pixel size of the encoded asset,
/// whether it animates, and the encoded bytes (PNG, APNG, or GIF).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StickerSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Intrinsic width of the encoded asset, in pixels.
    pub width: u32,
    /// Intrinsic height of the encoded asset, in pixels.
    pub height: u32,
    /// Whether the asset has more than one frame (APNG / animated GIF).
    pub animated: bool,
    /// The encoded asset, embedded at compile time.
    pub bytes: &'static [u8],
}

macro_rules! sticker_bytes {
    ($file:literal) => {
        include_bytes!(concat!("../../../assets/stickers/", $file))
    };
}

/// The bundled starter pack. Ids are permanent once shipped — projects store
/// them — so retire an asset by keeping its entry and swapping the art.
const CATALOG: &[StickerSpec] = &[
    StickerSpec {
        id: "heart",
        label: "Heart",
        width: 256,
        height: 256,
        animated: false,
        bytes: sticker_bytes!("heart.png"),
    },
    StickerSpec {
        id: "star",
        label: "Star",
        width: 256,
        height: 256,
        animated: false,
        bytes: sticker_bytes!("star.png"),
    },
    StickerSpec {
        id: "smiley",
        label: "Smiley",
        width: 256,
        height: 256,
        animated: false,
        bytes: sticker_bytes!("smiley.png"),
    },
    StickerSpec {
        id: "bolt",
        label: "Lightning Bolt",
        width: 256,
        height: 256,
        animated: false,
        bytes: sticker_bytes!("bolt.png"),
    },
    StickerSpec {
        id: "bubble",
        label: "Speech Bubble",
        width: 256,
        height: 256,
        animated: false,
        bytes: sticker_bytes!("bubble.png"),
    },
    StickerSpec {
        id: "flower",
        label: "Flower",
        width: 256,
        height: 256,
        animated: false,
        bytes: sticker_bytes!("flower.png"),
    },
    StickerSpec {
        id: "star_spin",
        label: "Spinning Star",
        width: 128,
        height: 128,
        animated: true,
        bytes: sticker_bytes!("star_spin.gif"),
    },
    StickerSpec {
        id: "heart_beat",
        label: "Beating Heart",
        width: 128,
        height: 128,
        animated: true,
        bytes: sticker_bytes!("heart_beat.png"),
    },
];

/// Every bundled sticker, in UI display order.
pub fn sticker_catalog() -> &'static [StickerSpec] {
    CATALOG
}

/// The spec for `id`, or `None` for an unknown id.
pub fn sticker_spec(id: &str) -> Option<&'static StickerSpec> {
    CATALOG.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_lookup_works() {
        for (i, spec) in CATALOG.iter().enumerate() {
            assert_eq!(sticker_spec(spec.id), Some(spec), "lookup of '{}'", spec.id);
            assert!(
                !CATALOG[..i].iter().any(|s| s.id == spec.id),
                "duplicate sticker id '{}'",
                spec.id
            );
            assert!(!spec.bytes.is_empty());
            assert!(spec.width > 0 && spec.height > 0);
        }
        assert_eq!(sticker_spec("nope"), None);
    }
}
