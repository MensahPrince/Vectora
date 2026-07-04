//! The model catalogs, shaped for the shells.
//!
//! One JSON document lists every preset vocabulary the panels render —
//! effects, transitions, masks, filters, animations, text effects, speed
//! presets, stabilize levels, audio roles — sourced from the same
//! `cutlass-models` catalogs the engine validates against, so a chip the UI
//! shows is by construction a value the engine accepts. Static data: fetch
//! once per process, no session required.

use std::ffi::c_char;

use serde_json::{Value, json};

use crate::wire::to_c_string;

/// All catalogs as one JSON document. Entries are `{id, label}` plus
/// catalog-specific extras (effect params, animation slots).
pub fn catalogs_json() -> String {
    catalogs_value().to_string()
}

fn catalogs_value() -> Value {
    let effects: Vec<Value> = cutlass_models::effect_catalog()
        .iter()
        .map(|e| {
            json!({
                "id": e.id,
                "label": e.label,
                "params": e.params.iter().map(|p| json!({
                    "name": p.name,
                    "label": p.label,
                    "default": p.default,
                    "min": p.min,
                    "max": p.max,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let transitions: Vec<Value> = cutlass_models::transition_catalog()
        .iter()
        .map(|t| json!({ "id": t.id, "label": t.label }))
        .collect();
    let masks: Vec<Value> = cutlass_models::mask_catalog()
        .iter()
        .map(|m| json!({ "id": m.kind.id(), "label": m.label }))
        .collect();
    let filters: Vec<Value> = cutlass_models::filter_catalog()
        .iter()
        .map(|f| json!({ "id": f.id, "label": f.label }))
        .collect();
    let animations: Vec<Value> = cutlass_models::animation_catalog()
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "label": a.label,
                "slot": a.slot.id(),
                "text_only": a.text_only,
            })
        })
        .collect();
    let text_effects: Vec<Value> = cutlass_models::text_effect_catalog()
        .iter()
        .map(|t| json!({ "id": t.id, "label": t.label }))
        .collect();
    let speed_presets: Vec<Value> = cutlass_models::speed_preset_catalog()
        .iter()
        .map(|s| json!({ "id": s.id, "label": s.label }))
        .collect();
    let stabilize_levels: Vec<Value> = cutlass_models::StabilizeLevel::ALL
        .iter()
        .map(|s| json!({ "id": s.id(), "label": s.label() }))
        .collect();
    let audio_roles: Vec<Value> = cutlass_models::AudioRole::ALL
        .iter()
        .map(|r| json!({ "id": r.id(), "label": r.label() }))
        .collect();

    json!({
        "effects": effects,
        "transitions": transitions,
        "masks": masks,
        "filters": filters,
        "animations": animations,
        "text_effects": text_effects,
        "speed_presets": speed_presets,
        "stabilize_levels": stabilize_levels,
        "audio_roles": audio_roles,
    })
}

/// All preset catalogs as JSON (see [`catalogs_json`]). Static data — callable
/// any time, no session needed. Free with `cutlass_string_free`.
#[unsafe(no_mangle)]
pub extern "C" fn cutlass_catalogs() -> *mut c_char {
    to_c_string(catalogs_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_cover_every_vocabulary_with_ids_and_labels() {
        let doc = catalogs_value();
        for key in [
            "effects",
            "transitions",
            "masks",
            "filters",
            "animations",
            "text_effects",
            "speed_presets",
            "stabilize_levels",
            "audio_roles",
        ] {
            let list = doc[key]
                .as_array()
                .unwrap_or_else(|| panic!("{key} missing"));
            assert!(!list.is_empty(), "{key} is empty");
            for entry in list {
                assert!(entry["id"].is_string(), "{key} entry lacks id: {entry}");
                assert!(
                    entry["label"].is_string(),
                    "{key} entry lacks label: {entry}"
                );
            }
        }
        // Catalog-specific extras survive the shaping.
        assert!(doc["effects"][0]["params"].is_array());
        assert!(doc["animations"][0]["slot"].is_string());
    }

    #[test]
    fn animation_slots_use_wire_ids() {
        let doc = catalogs_value();
        let slots: std::collections::BTreeSet<&str> = doc["animations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["slot"].as_str().unwrap())
            .collect();
        assert!(slots.contains("in") && slots.contains("out") && slots.contains("combo"));
    }
}
