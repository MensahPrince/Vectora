//! The first-party LUT starter pack: every filter-preset recipe from
//! [`crate::grade`], baked into a `.cube` table via the shared CPU grade
//! (`ColorGrade::apply` — the same math the shader runs).
//!
//! The pack is generated, not authored: `cargo run -p cutlass-render --bin
//! lut-pack -- <out-dir>` writes the `.cube` files plus a `luts.json` catalog
//! document for `cutlass-backend`'s `CATALOG_DIR`. A drift test pins each
//! table's content hash so a recipe change is a conscious catalog re-bake,
//! not silent drift between shipped LUTs and in-app filters.

use cutlass_compositor::CubeLut;
use cutlass_models::filter_catalog;

use crate::grade::preset_recipe;

/// Grid resolution of baked starter LUTs. 33 is the industry-standard size
/// (Resolve/Adobe default): fine enough that trilinear error on these smooth
/// recipes is far below 8-bit quantization.
pub const STARTER_LUT_SIZE: u32 = 33;

/// One baked starter LUT.
pub struct StarterLut {
    /// Catalog id (`cutlass-<filter id>`), stable once shipped.
    pub id: String,
    /// Display label (the filter preset's label).
    pub label: &'static str,
    /// The baked table (unit domain).
    pub cube: CubeLut,
}

/// Bake the full starter pack: one LUT per filter-catalog recipe at full
/// intensity (in-app intensity blending happens in the LUT pass itself).
pub fn starter_lut_pack() -> Vec<StarterLut> {
    filter_catalog()
        .iter()
        .map(|spec| {
            let grade = preset_recipe(spec.id).expect("every catalog id has a recipe");
            StarterLut {
                id: format!("cutlass-{}", spec.id),
                label: spec.label,
                cube: CubeLut::from_fn(STARTER_LUT_SIZE, |rgb| grade.apply(rgb)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a over the serialized `.cube` text exactly as the `lut-pack`
    /// baker writes it — stable across platforms (the text is
    /// decimal-formatted, not raw float bits).
    fn content_hash(lut: &StarterLut) -> u64 {
        let text = lut.cube.to_cube_string(lut.label);
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    #[test]
    fn pack_covers_every_filter_recipe() {
        let pack = starter_lut_pack();
        assert_eq!(pack.len(), filter_catalog().len());
        for lut in &pack {
            assert_eq!(lut.cube.size(), STARTER_LUT_SIZE);
        }
    }

    #[test]
    fn baked_tables_match_their_recipes() {
        for lut in starter_lut_pack() {
            let filter_id = lut.id.strip_prefix("cutlass-").unwrap();
            let grade = preset_recipe(filter_id).unwrap();
            for probe in [
                [0.0, 0.0, 0.0],
                [1.0, 1.0, 1.0],
                [0.5, 0.5, 0.5],
                [0.8, 0.2, 0.4],
            ] {
                let want = grade.apply(probe);
                let got = lut.cube.sample(probe);
                for ch in 0..3 {
                    assert!(
                        (want[ch] - got[ch]).abs() < 0.02,
                        "{}: probe {probe:?} channel {ch}: recipe {want:?} vs LUT {got:?}",
                        lut.id
                    );
                }
            }
        }
    }

    /// Drift pin: these hashes change iff a recipe (or the bake) changes.
    /// That's allowed — but re-bake and re-upload the served pack in the
    /// same change, then update the pins here.
    #[test]
    fn starter_pack_is_pinned() {
        let pack = starter_lut_pack();
        let got: Vec<(String, u64)> = pack
            .iter()
            .map(|lut| (lut.id.clone(), content_hash(lut)))
            .collect();
        let pinned: &[(&str, u64)] = &[
            ("cutlass-vivid", 0x8cfbba8c2c31e91a),
            ("cutlass-warm", 0x101c937634e3ebf3),
            ("cutlass-cool", 0x7a678f3ca49392e5),
            ("cutlass-mono", 0xa92d463fcb1e6d1c),
            ("cutlass-fade", 0xcb3aab359c00a3a6),
            ("cutlass-chrome", 0x6e0c0434425fef21),
            ("cutlass-noir", 0x33f4a8c26c46eaec),
            ("cutlass-sunset", 0x2818785eecec0848),
            ("cutlass-forest", 0x3027f87cdf11a137),
            ("cutlass-berry", 0xd08d9de1d63e4854),
        ];
        assert_eq!(got.len(), pinned.len());
        let dump: Vec<String> = got
            .iter()
            .map(|(id, hash)| format!("(\"{id}\", 0x{hash:016x}),"))
            .collect();
        for ((id, hash), (want_id, want_hash)) in got.iter().zip(pinned) {
            assert_eq!(id, want_id);
            assert_eq!(
                *hash,
                *want_hash,
                "{id} drifted: re-bake the served pack and update the pins to:\n{}",
                dump.join("\n")
            );
        }
    }
}
