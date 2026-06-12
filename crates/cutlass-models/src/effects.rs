//! Effects as data (v1 roadmap M4): a clip carries a list of
//! [`EffectInstance`]s, each `{effect_id, params}`. The model never holds
//! shader code — the compositor owns the WGSL and maps ids to GPU passes.
//!
//! The [`effect_catalog`] here is the validation + UI source of truth
//! (display names, parameter defaults / ranges). It is drift-checked against
//! the compositor's renderable descriptors from `cutlass-engine`, so the two
//! crates can never disagree on which ids and parameter names exist.

use serde::{Deserialize, Serialize};

use crate::Map;
use crate::error::ModelError;
use crate::param::{Easing, Param};

/// One scalar parameter of an effect: its stable name, a human label, and the
/// default + inclusive range commands validate against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectParamSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub default: f32,
    pub min: f32,
    pub max: f32,
}

/// A catalog entry: an effect id, its display label, and its ordered scalar
/// parameters. The order matches the compositor's uniform slot order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub params: &'static [EffectParamSpec],
}

impl EffectSpec {
    /// The spec for `name`, or `None`.
    pub fn param(&self, name: &str) -> Option<&'static EffectParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }

    /// The spec at slot `index`, or `None`.
    pub fn param_at(&self, index: usize) -> Option<&'static EffectParamSpec> {
        self.params.get(index)
    }
}

/// The starter pack (M4). Phase 3 extends this list; ids and parameter names
/// must stay in lockstep with `cutlass_compositor::effect_descriptors`.
const CATALOG: &[EffectSpec] = &[
    EffectSpec {
        id: "gaussian_blur",
        label: "Gaussian Blur",
        params: &[EffectParamSpec {
            name: "radius",
            label: "Radius",
            default: 4.0,
            min: 0.0,
            max: 64.0,
        }],
    },
    EffectSpec {
        id: "vignette",
        label: "Vignette",
        params: &[EffectParamSpec {
            name: "amount",
            label: "Amount",
            default: 0.6,
            min: 0.0,
            max: 1.0,
        }],
    },
    EffectSpec {
        id: "sharpen",
        label: "Sharpen",
        params: &[EffectParamSpec {
            name: "amount",
            label: "Amount",
            default: 0.5,
            min: 0.0,
            max: 3.0,
        }],
    },
    EffectSpec {
        id: "pixelate",
        label: "Pixelate",
        params: &[EffectParamSpec {
            name: "size",
            label: "Cell Size",
            default: 8.0,
            min: 1.0,
            max: 256.0,
        }],
    },
    EffectSpec {
        id: "glitch",
        label: "Glitch",
        params: &[
            EffectParamSpec {
                name: "amount",
                label: "Amount",
                default: 0.5,
                min: 0.0,
                max: 1.0,
            },
            EffectParamSpec {
                name: "seed",
                label: "Seed",
                default: 0.0,
                min: 0.0,
                max: 1000.0,
            },
        ],
    },
    EffectSpec {
        id: "chromatic_aberration",
        label: "Chromatic Aberration",
        params: &[EffectParamSpec {
            name: "amount",
            label: "Amount",
            default: 0.4,
            min: 0.0,
            max: 1.0,
        }],
    },
    EffectSpec {
        id: "grain",
        label: "Film Grain",
        params: &[
            EffectParamSpec {
                name: "amount",
                label: "Amount",
                default: 0.3,
                min: 0.0,
                max: 1.0,
            },
            EffectParamSpec {
                name: "seed",
                label: "Seed",
                default: 0.0,
                min: 0.0,
                max: 1000.0,
            },
        ],
    },
    EffectSpec {
        id: "glow",
        label: "Glow",
        params: &[
            EffectParamSpec {
                name: "threshold",
                label: "Threshold",
                default: 0.7,
                min: 0.0,
                max: 1.0,
            },
            EffectParamSpec {
                name: "intensity",
                label: "Intensity",
                default: 0.8,
                min: 0.0,
                max: 4.0,
            },
        ],
    },
    EffectSpec {
        id: "zoom_blur",
        label: "Zoom Blur",
        params: &[EffectParamSpec {
            name: "amount",
            label: "Amount",
            default: 0.5,
            min: 0.0,
            max: 1.0,
        }],
    },
    EffectSpec {
        id: "mirror",
        label: "Mirror",
        params: &[EffectParamSpec {
            name: "mode",
            label: "Mode",
            default: 0.0,
            min: 0.0,
            max: 3.0,
        }],
    },
];

/// Every effect the model knows about (validation + UI browsing).
pub fn effect_catalog() -> &'static [EffectSpec] {
    CATALOG
}

/// The catalog entry for `id`, or `None`.
pub fn effect_spec(id: &str) -> Option<&'static EffectSpec> {
    CATALOG.iter().find(|s| s.id == id)
}

/// An effect placed on a clip. Only parameters that differ from their catalog
/// default are stored (others fall back to the default), so a freshly-added
/// effect serializes to just its id and old files that predate a new
/// parameter keep working.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectInstance {
    pub effect_id: String,
    /// Explicitly-set parameters, keyed by name; constant or keyframed.
    /// Keyframe ticks are clip-relative, like the transform params.
    #[serde(
        default,
        with = "crate::serde_map",
        skip_serializing_if = "Map::is_empty"
    )]
    pub params: Map<String, Param<f32>>,
}

impl EffectInstance {
    /// A new instance of `effect_id` with every parameter at its default.
    pub fn new(effect_id: impl Into<String>) -> Self {
        Self {
            effect_id: effect_id.into(),
            params: Map::default(),
        }
    }

    /// The catalog entry for this instance, or an error if the id is unknown.
    pub fn spec(&self) -> Result<&'static EffectSpec, ModelError> {
        effect_spec(&self.effect_id)
            .ok_or_else(|| ModelError::InvalidParam(format!("unknown effect '{}'", self.effect_id)))
    }

    fn param_spec(&self, index: usize) -> Result<&'static EffectParamSpec, ModelError> {
        self.spec()?.param_at(index).ok_or_else(|| {
            ModelError::InvalidParam(format!(
                "effect '{}' has no parameter at index {index}",
                self.effect_id
            ))
        })
    }

    /// Sampled value of `param` at clip-relative fractional `tick`, falling
    /// back to the catalog default when the parameter was never set. `None`
    /// when the effect id or parameter name is unknown.
    pub fn sample_param(&self, param: &str, tick: f64) -> Option<f32> {
        let pspec = self.spec().ok()?.param(param)?;
        Some(match self.params.get(param) {
            Some(p) => p.sample_at(tick),
            None => pspec.default,
        })
    }

    /// Insert or replace a keyframe on parameter slot `index`.
    pub fn set_param_keyframe(
        &mut self,
        index: usize,
        tick: i64,
        value: f32,
        easing: Easing,
    ) -> Result<(), ModelError> {
        let pspec = self.param_spec(index)?;
        range_check(pspec, value)?;
        easing.validate()?;
        let (name, default) = (pspec.name, pspec.default);
        self.params
            .entry(name.to_string())
            .or_insert(Param::Constant(default))
            .set_keyframe(tick, value, easing);
        Ok(())
    }

    /// Remove the keyframe at exactly `tick` on parameter slot `index`.
    pub fn remove_param_keyframe(&mut self, index: usize, tick: i64) -> Result<(), ModelError> {
        let pspec = self.param_spec(index)?;
        let name = pspec.name;
        let removed = self
            .params
            .get_mut(name)
            .is_some_and(|p| p.remove_keyframe(tick));
        if removed {
            Ok(())
        } else {
            Err(ModelError::InvalidParam(format!(
                "no keyframe at tick {tick} on {}.{name}",
                self.effect_id
            )))
        }
    }

    /// Replace parameter slot `index` with a constant, dropping keyframes.
    pub fn set_param_constant(&mut self, index: usize, value: f32) -> Result<(), ModelError> {
        let pspec = self.param_spec(index)?;
        range_check(pspec, value)?;
        self.params
            .insert(pspec.name.to_string(), Param::Constant(value));
        Ok(())
    }

    /// `Ok` iff the id is known, every set parameter names a real slot, every
    /// curve is structurally sound, and every value lies in range.
    pub fn validate(&self) -> Result<(), ModelError> {
        let spec = self.spec()?;
        for (name, param) in &self.params {
            let pspec = spec.param(name).ok_or_else(|| {
                ModelError::InvalidParam(format!(
                    "effect '{}' has no parameter '{name}'",
                    self.effect_id
                ))
            })?;
            param.validate_shape()?;
            param.for_each_value(|v| range_check(pspec, *v))?;
        }
        Ok(())
    }
}

fn range_check(pspec: &EffectParamSpec, value: f32) -> Result<(), ModelError> {
    if !value.is_finite() || value < pspec.min || value > pspec.max {
        return Err(ModelError::InvalidParam(format!(
            "{} = {value} out of range [{}, {}]",
            pspec.name, pspec.min, pspec.max
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique() {
        let mut ids: Vec<&str> = effect_catalog().iter().map(|s| s.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), effect_catalog().len());
    }

    #[test]
    fn unknown_effect_fails_validation() {
        assert!(EffectInstance::new("nope").validate().is_err());
        assert!(EffectInstance::new("gaussian_blur").validate().is_ok());
    }

    #[test]
    fn sampled_param_falls_back_to_default() {
        let fx = EffectInstance::new("gaussian_blur");
        assert_eq!(fx.sample_param("radius", 0.0), Some(4.0));
        assert_eq!(fx.sample_param("missing", 0.0), None);
    }

    #[test]
    fn out_of_range_constant_rejected() {
        let mut fx = EffectInstance::new("vignette");
        assert!(fx.set_param_constant(0, 2.0).is_err()); // amount max 1.0
        assert!(fx.set_param_constant(0, 0.5).is_ok());
        assert_eq!(fx.sample_param("amount", 0.0), Some(0.5));
    }

    #[test]
    fn keyframe_roundtrip_on_a_param() {
        let mut fx = EffectInstance::new("gaussian_blur");
        fx.set_param_keyframe(0, 0, 0.0, Easing::Linear).unwrap();
        fx.set_param_keyframe(0, 24, 8.0, Easing::Linear).unwrap();
        assert_eq!(fx.sample_param("radius", 12.0), Some(4.0));
        fx.validate().unwrap();
        fx.remove_param_keyframe(0, 24).unwrap();
        // Removing the last-but-one keyframe leaves a constant.
        assert!(fx.remove_param_keyframe(0, 999).is_err());
    }
}
