//! Per-clip color grade: filter presets plus manual adjustments → [`ColorGrade`].
//!
//! [`resolve`](crate::resolve) samples a clip's [`Filter`] and
//! [`ColorAdjustments`] at resolve time and folds them into a single
//! [`ColorGrade`] on each [`SceneLayer`](crate::scene::SceneLayer). The
//! compositor applies that grade in the fragment shaders (Stage A); this module
//! is the model→shader bridge (Stage B).

use cutlass_compositor::ColorGrade;
use cutlass_models::{ColorAdjustments, Filter};

/// Hand-tuned [`ColorGrade`] recipe for one filter-catalog id.
///
/// Returns `None` for unknown ids so [`effective_grade`] can treat them as
/// identity on the preset side (manual adjustments still apply).
fn preset_recipe(id: &str) -> Option<ColorGrade> {
    Some(match id {
        "vivid" => ColorGrade {
            saturation: 0.45,
            contrast: 0.15,
            ..ColorGrade::IDENTITY
        },
        "warm" => ColorGrade {
            temperature: 0.4,
            brightness: 0.05,
            ..ColorGrade::IDENTITY
        },
        "cool" => ColorGrade {
            temperature: -0.4,
            ..ColorGrade::IDENTITY
        },
        "mono" => ColorGrade {
            saturation: -1.0,
            contrast: 0.1,
            ..ColorGrade::IDENTITY
        },
        "fade" => ColorGrade {
            contrast: -0.3,
            brightness: 0.1,
            saturation: -0.2,
            ..ColorGrade::IDENTITY
        },
        "chrome" => ColorGrade {
            contrast: 0.25,
            saturation: 0.1,
            brightness: 0.05,
            ..ColorGrade::IDENTITY
        },
        "noir" => ColorGrade {
            saturation: -1.0,
            contrast: 0.35,
            brightness: -0.05,
            ..ColorGrade::IDENTITY
        },
        "sunset" => ColorGrade {
            temperature: 0.5,
            tint: 0.1,
            saturation: 0.15,
            ..ColorGrade::IDENTITY
        },
        "forest" => ColorGrade {
            tint: -0.25,
            saturation: 0.2,
            brightness: 0.03,
            ..ColorGrade::IDENTITY
        },
        "berry" => ColorGrade {
            tint: 0.35,
            temperature: -0.1,
            saturation: 0.25,
            ..ColorGrade::IDENTITY
        },
        _ => return None,
    })
}

/// Fold a clip's filter preset and manual adjustments into one compositor grade.
///
/// # Preset × intensity
///
/// When a filter is present, its catalog recipe is scaled by `filter.intensity`
/// (clamped to `[0, 1]`). Multiplying each recipe component by intensity is a
/// **parameter-space approximation** of blending the graded result over the
/// original: for the small control values these presets use, `recipe × intensity`
/// tracks `mix(identity, recipe, intensity)` closely without re-running the
/// non-linear shader per blend weight.
///
/// Unknown filter ids contribute nothing on the preset side (identity recipe).
///
/// # Manual adjustments
///
/// [`ColorAdjustments`] fields are added component-wise after the scaled preset.
/// Adjustments have no tint control; any tint from the preset is left as scaled
/// above and is not modified by adjustments.
///
/// # Defensive clamping
///
/// JSON project loads are not re-validated here — only engine setters run
/// `validate`. Non-finite inputs are treated as `0`; intensity is clamped to
/// `[0, 1]`; every adjustment field and every output component is clamped to
/// `[-1, 1]`.
pub(crate) fn effective_grade(filter: Option<&Filter>, adjust: &ColorAdjustments) -> ColorGrade {
    let intensity = filter.map(|f| clamp_unit(f.intensity)).unwrap_or(0.0);

    let preset = filter
        .and_then(|f| preset_recipe(&f.id))
        .unwrap_or(ColorGrade::IDENTITY);

    let mut grade = scale_grade(preset, intensity);
    grade.exposure += clamp_adjust(adjust.exposure);
    grade.brightness += clamp_adjust(adjust.brightness);
    grade.contrast += clamp_adjust(adjust.contrast);
    grade.saturation += clamp_adjust(adjust.saturation);
    grade.temperature += clamp_adjust(adjust.temperature);
    clamp_grade(grade)
}

/// Scale every grade component (including tint) by `intensity`.
fn scale_grade(grade: ColorGrade, intensity: f32) -> ColorGrade {
    ColorGrade {
        exposure: grade.exposure * intensity,
        brightness: grade.brightness * intensity,
        contrast: grade.contrast * intensity,
        saturation: grade.saturation * intensity,
        temperature: grade.temperature * intensity,
        tint: grade.tint * intensity,
    }
}

fn clamp_unit(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    v.clamp(0.0, 1.0)
}

fn clamp_adjust(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    v.clamp(-1.0, 1.0)
}

fn clamp_grade(mut grade: ColorGrade) -> ColorGrade {
    grade.exposure = clamp_adjust(grade.exposure);
    grade.brightness = clamp_adjust(grade.brightness);
    grade.contrast = clamp_adjust(grade.contrast);
    grade.saturation = clamp_adjust(grade.saturation);
    grade.temperature = clamp_adjust(grade.temperature);
    grade.tint = clamp_adjust(grade.tint);
    grade
}

#[cfg(test)]
mod tests {
    use super::*;
    use cutlass_models::filter_catalog;

    #[test]
    fn identity_when_no_filter_and_neutral_adjust() {
        let grade = effective_grade(None, &ColorAdjustments::default());
        assert_eq!(grade, ColorGrade::IDENTITY);
        assert!(grade.is_identity());
    }

    #[test]
    fn intensity_zero_with_neutral_adjust_yields_identity() {
        let filter = Filter {
            id: "mono".into(),
            intensity: 0.0,
        };
        let grade = effective_grade(Some(&filter), &ColorAdjustments::default());
        assert_eq!(grade, ColorGrade::IDENTITY);
    }

    #[test]
    fn catalog_ids_all_have_recipes_and_only_known_ids() {
        for spec in filter_catalog() {
            assert!(
                preset_recipe(spec.id).is_some(),
                "missing recipe for catalog id '{}'",
                spec.id
            );
        }
        for id in [
            "vivid", "warm", "cool", "mono", "fade", "chrome", "noir", "sunset", "forest", "berry",
        ] {
            assert!(preset_recipe(id).is_some());
        }
        assert!(preset_recipe("unknown").is_none());
        assert!(preset_recipe("").is_none());
    }

    #[test]
    fn clamps_out_of_range_and_non_finite_inputs() {
        let filter = Filter {
            id: "vivid".into(),
            intensity: f32::NAN,
        };
        let adjust = ColorAdjustments {
            saturation: f32::INFINITY,
            brightness: 2.0,
            contrast: -3.0,
            exposure: f32::NAN,
            temperature: 1.5,
        };
        let grade = effective_grade(Some(&filter), &adjust);
        // NaN intensity zeroes the preset; adjustments clamp individually.
        assert_eq!(grade.brightness, 1.0);
        assert_eq!(grade.contrast, -1.0);
        assert_eq!(grade.temperature, 1.0);
        assert_eq!(grade.saturation, 0.0);
        assert_eq!(grade.exposure, 0.0);

        let filter = Filter {
            id: "mono".into(),
            intensity: 1.0,
        };
        let adjust = ColorAdjustments {
            saturation: -1.5,
            ..ColorAdjustments::default()
        };
        let grade = effective_grade(Some(&filter), &adjust);
        assert_eq!(grade.saturation, -1.0);
        assert_eq!(grade.contrast, 0.1);
    }

    #[test]
    fn preset_scaled_by_intensity_plus_adjustments() {
        let filter = Filter {
            id: "warm".into(),
            intensity: 0.5,
        };
        let adjust = ColorAdjustments {
            brightness: 0.1,
            ..ColorAdjustments::default()
        };
        let grade = effective_grade(Some(&filter), &adjust);
        assert!((grade.temperature - 0.2).abs() < 1e-6);
        assert!((grade.brightness - 0.125).abs() < 1e-6);
    }
}
