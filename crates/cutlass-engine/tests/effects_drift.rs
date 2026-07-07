//! Cross-crate drift guard: the `cutlass-models` effect catalog (validation +
//! UI) and the `cutlass-compositor` render descriptors (WGSL slot order) live
//! in separate crates that can't depend on each other. This test — in the
//! engine, which depends on both — fails CI the moment they disagree on which
//! effect ids or parameter names exist.

#[test]
fn model_catalog_matches_compositor_descriptors() {
    let descriptors = cutlass_compositor::effect_descriptors();

    for spec in cutlass_models::effect_catalog() {
        let desc = descriptors
            .iter()
            .find(|d| d.id == spec.id)
            .unwrap_or_else(|| panic!("compositor cannot render catalog effect '{}'", spec.id));
        let model_names: Vec<&str> = spec.params.iter().map(|p| p.name).collect();
        assert_eq!(
            model_names.as_slice(),
            desc.params,
            "parameter names/order drift for effect '{}'",
            spec.id
        );
    }

    for desc in descriptors {
        assert!(
            cutlass_models::effect_spec(desc.id).is_some(),
            "no catalog entry for renderable effect '{}'",
            desc.id
        );
    }
}

#[test]
fn model_transition_catalog_matches_compositor_set() {
    let renderable = cutlass_compositor::transition_ids();

    for spec in cutlass_models::transition_catalog() {
        assert!(
            renderable.contains(&spec.id),
            "compositor cannot render catalog transition '{}'",
            spec.id
        );
    }

    for id in renderable {
        assert!(
            cutlass_models::transition_spec(id).is_some(),
            "no catalog entry for renderable transition '{}'",
            id
        );
    }
}
