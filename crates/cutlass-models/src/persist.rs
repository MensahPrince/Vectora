//! On-disk project files (`.cutlass` JSON).

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use serde::Deserialize;

use crate::error::ModelError;
use crate::project::Project;
use crate::schema::{PROJECT_SCHEMA_VERSION, ProjectSchema};

/// Recommended extension for Cutlass project files.
pub const PROJECT_FILE_EXTENSION: &str = "cutlass";

/// Numeric version from [`ProjectSchema::current`] for simple callers.
pub const PROJECT_FILE_VERSION: u32 = PROJECT_SCHEMA_VERSION;

impl Project {
    /// Serialize this project to a `.cutlass` JSON file.
    ///
    /// The document is stamped with this build's schema version regardless
    /// of what was loaded: the writer defines the format. (A project opened
    /// from a v1 file may now hold v2-only data like keyframes; persisting
    /// it as "v1" would lie to older readers.)
    pub fn save_to_file(&self, path: &Path) -> io::Result<()> {
        let mut doc = self.clone();
        doc.schema.version = PROJECT_SCHEMA_VERSION;
        doc.schema.kind = crate::schema::PROJECT_SCHEMA_KIND.into();
        let json = serde_json::to_string_pretty(&doc).map_err(io::Error::other)?;
        let mut writer = BufWriter::new(File::create(path)?);
        writer.write_all(json.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }

    /// Deserialize a project from a `.cutlass` JSON file.
    ///
    /// The document's schema version is read and validated *before* the
    /// typed parse, and [`migrate_document`] rewrites older shapes up to the
    /// current one — so the strict parse below only ever sees the current
    /// format. Files newer than this build are refused, never half-parsed.
    pub fn load_from_file(path: &Path) -> Result<Project, ModelError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ModelError::InvalidProjectFile(e.to_string()))?;
        let mut doc: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ModelError::InvalidProjectFile(e.to_string()))?;

        unwrap_legacy_envelope(&mut doc);
        let mut schema = read_schema(&doc)?;
        normalize_legacy_schema(&mut schema);
        validate_schema(&schema)?;
        migrate_document(&mut doc, schema.version);

        let mut project: Project = serde_json::from_value(doc)
            .map_err(|e| ModelError::InvalidProjectFile(e.to_string()))?;
        // Keep the file's original (normalized) schema as provenance; the
        // writer re-stamps the current version on save.
        project.schema = schema;
        // Files written before the main-track flag (or edited externally)
        // re-derive the lane-zone / main-track invariants on entry.
        project.timeline_mut().normalize_lanes();
        Ok(project)
    }
}

/// Unwrap the legacy `{ "version", "project" }` envelope (early v1 saves)
/// to the bare project document, pushing the envelope version into the
/// inner schema when the project carries none (or a placeholder version 0).
fn unwrap_legacy_envelope(doc: &mut serde_json::Value) {
    let Some(obj) = doc.as_object_mut() else {
        return;
    };
    // A real project document has a schema (or its legacy alias); only the
    // envelope nests the project under "project". Unknown root fields named
    // "project"/"version" on a current file must not trip this.
    let looks_like_envelope = obj.contains_key("project")
        && obj.contains_key("version")
        && !obj.contains_key("schema")
        && !obj.contains_key("schema_version");
    if !looks_like_envelope {
        return;
    }
    let version = obj.get("version").and_then(serde_json::Value::as_u64);
    let Some(mut project) = obj.remove("project") else {
        return;
    };
    if let (Some(version), Some(inner)) = (version, project.as_object_mut()) {
        let placeholder = inner
            .get("schema")
            .and_then(|s| s.get("version"))
            .and_then(serde_json::Value::as_u64)
            == Some(0);
        let missing = !inner.contains_key("schema") && !inner.contains_key("schema_version");
        if missing || placeholder {
            inner.insert("schema".into(), serde_json::json!(version));
        }
    }
    *doc = project;
}

/// Read the document's schema (object or legacy bare-integer form) without
/// deserializing the whole project. Shared with the template loader, which
/// wraps a project document and versions in lockstep with it.
pub(crate) fn read_schema(doc: &serde_json::Value) -> Result<ProjectSchema, ModelError> {
    #[derive(Deserialize)]
    struct Holder(#[serde(deserialize_with = "crate::schema::deserialize")] ProjectSchema);

    let raw = doc
        .get("schema")
        .or_else(|| doc.get("schema_version"))
        .ok_or_else(|| ModelError::InvalidProjectFile("missing schema".into()))?;
    serde_json::from_value::<Holder>(raw.clone())
        .map(|holder| holder.0)
        .map_err(|e| ModelError::InvalidProjectFile(format!("invalid schema: {e}")))
}

/// Rewrite a raw project document from `from`'s shape to the current
/// schema's, one version step at a time, before the typed parse.
///
/// This is the format versioning policy (v1 roadmap M0):
///
/// - **Additive optional fields don't bump the version.** New fields ship
///   with `#[serde(default)]` (+ skip-if-default on save); readers of the
///   same version ignore fields they don't know — and drop them on resave.
///   That tolerance is the compatibility contract *within* a version.
/// - **Shape changes bump [`PROJECT_SCHEMA_VERSION`]** and add a
///   `migrate_vN_to_vN1` step here rewriting the previous shape into the
///   new one.
/// - **Newer documents are refused** before this runs
///   ([`ModelError::UnsupportedProjectSchema`] from `validate_schema`) —
///   guessing at a future format risks silent data loss.
///
/// The template loader reuses this on its embedded project document
/// (`.cutlasst` files version in lockstep with [`PROJECT_SCHEMA_VERSION`]).
pub(crate) fn migrate_document(doc: &mut serde_json::Value, from: u32) {
    for step in from..PROJECT_SCHEMA_VERSION {
        match step {
            1 => migrate_v1_to_v2(doc),
            // `validate_schema` bounds `from` to supported versions, so a
            // missing arm is a bug: a version bump landed without its step.
            _ => unreachable!("no migration step for schema v{step} -> v{}", step + 1),
        }
    }
}

/// v1 → v2 (M2 animatable params): transform properties gained the
/// `{"kf": [...]}` curve shape, but constants kept the bare v1 value shape,
/// so every v1 document is already a valid v2 document — nothing to
/// rewrite. The step exists to anchor the chain.
fn migrate_v1_to_v2(_doc: &mut serde_json::Value) {}

fn normalize_legacy_schema(schema: &mut ProjectSchema) {
    if schema.kind.is_empty() {
        schema.kind = crate::schema::PROJECT_SCHEMA_KIND.into();
    }
}

fn validate_schema(found: &ProjectSchema) -> Result<(), ModelError> {
    let expected = ProjectSchema::current();
    if !found.is_supported() {
        return Err(ModelError::UnsupportedProjectSchema {
            found: found.clone(),
            expected,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::{Clip, Generator};
    use crate::time::{Rational, RationalTime, TimeRange};
    use crate::track::TrackKind;

    const R24: Rational = Rational::FPS_24;

    #[test]
    fn roundtrip_save_load_preserves_timeline_and_metadata() {
        let mut project = Project::new("demo", R24);
        project.metadata_mut().description = "rough cut".into();
        project.metadata_mut().author = Some("editor".into());
        let media_id = project.add_media(crate::MediaSource::new(
            "/tmp/clip.mp4",
            1920,
            1080,
            R24,
            240,
            true,
        ));
        let track = project.add_track(TrackKind::Video, "V1");
        project
            .add_clip(
                track,
                media_id,
                TimeRange::at_rate(0, 48, R24),
                RationalTime::new(0, R24),
            )
            .unwrap();
        let overlay = project.add_track(TrackKind::Text, "T1");
        project
            .timeline_mut()
            .add_clip(
                overlay,
                Clip::generated(Generator::text("hi"), TimeRange::at_rate(48, 24, R24)),
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.cutlass");
        project.save_to_file(&path).unwrap();
        let loaded = Project::load_from_file(&path).unwrap();

        assert_eq!(loaded.schema, ProjectSchema::current());
        assert_eq!(loaded.id, project.id);
        assert_eq!(loaded.name, "demo");
        assert_eq!(loaded.metadata, project.metadata);
        assert_eq!(loaded.media_count(), 1);
        assert_eq!(loaded.timeline().clip_count(), 2);
    }

    #[test]
    fn saved_file_writes_schema_object() {
        let project = Project::new("shape", R24);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shape.cutlass");
        project.save_to_file(&path).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let schema = json.get("schema").expect("schema object");
        assert_eq!(schema["version"], PROJECT_SCHEMA_VERSION);
        assert_eq!(schema["kind"], "cutlass.project");
    }

    #[test]
    fn load_accepts_v1_schema_files() {
        // Every pre-M2 alpha save is a v1 file; v2 readers open them as-is.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.cutlass");
        std::fs::write(
            &path,
            r#"{"schema":{"version":1,"kind":"cutlass.project"},"id":1,"name":"v1","metadata":{},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]}}"#,
        )
        .unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.schema.version, 1);

        // Re-saving a v1 project writes the current format version.
        let resaved = dir.path().join("resaved.cutlass");
        loaded.save_to_file(&resaved).unwrap();
        let reloaded = Project::load_from_file(&resaved).unwrap();
        assert_eq!(reloaded.schema.version, PROJECT_SCHEMA_VERSION);
    }

    #[test]
    fn keyframed_transform_survives_save_load() {
        use crate::clip::{ClipParam, ParamValue};
        use crate::param::Easing;

        let mut project = Project::new("anim", R24);
        let track = project.add_track(TrackKind::Text, "T1");
        let clip = project
            .timeline_mut()
            .add_clip(
                track,
                Clip::generated(Generator::text("fade"), TimeRange::at_rate(0, 48, R24)),
            )
            .unwrap();
        project
            .set_param_keyframe(
                clip,
                ClipParam::Opacity,
                RationalTime::new(0, R24),
                ParamValue::Scalar(0.0),
                Easing::EaseInOut,
            )
            .unwrap();
        project
            .set_param_keyframe(
                clip,
                ClipParam::Opacity,
                RationalTime::new(24, R24),
                ParamValue::Scalar(1.0),
                Easing::Linear,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anim.cutlass");
        project.save_to_file(&path).unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        let transform = &loaded.clip(clip).unwrap().transform;
        assert!(transform.is_animated());
        assert_eq!(transform.opacity.keyframes().len(), 2);
        assert_eq!(transform.sample(24).opacity, 1.0);
    }

    #[test]
    fn load_rejects_unknown_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.cutlass");
        std::fs::write(
            &path,
            r#"{"schema":{"version":99,"kind":"cutlass.project"},"id":1,"name":"x","metadata":{},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]}}"#,
        )
        .unwrap();
        let err = Project::load_from_file(&path).unwrap_err();
        assert!(matches!(err, ModelError::UnsupportedProjectSchema { .. }));
    }

    #[test]
    fn load_accepts_legacy_integer_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-int.cutlass");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"id":1,"name":"legacy","metadata":{},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]}}"#,
        )
        .unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.schema.version, 1);
        assert_eq!(loaded.schema.kind, crate::schema::PROJECT_SCHEMA_KIND);
    }

    #[test]
    fn load_tolerates_unknown_optional_fields() {
        // The versioning policy: additive optional fields don't bump the
        // schema version, so a same-version file written by a newer build
        // may carry fields this build doesn't know. Loading must succeed
        // and keep everything this build *does* know.
        let mut project = Project::new("tolerant", R24);
        let media_id = project.add_media(crate::MediaSource::new(
            "/tmp/clip.mp4",
            1920,
            1080,
            R24,
            240,
            true,
        ));
        let track = project.add_track(TrackKind::Video, "V1");
        project
            .add_clip(
                track,
                media_id,
                TimeRange::at_rate(0, 48, R24),
                RationalTime::new(0, R24),
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tolerant.cutlass");
        project.save_to_file(&path).unwrap();

        // Doctor the saved JSON with unknown fields at every level a future
        // build is likely to extend.
        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["color_management"] = serde_json::json!({ "working_space": "bt709" });
        // The media pool serializes as `[id, source]` pairs.
        doc["media"][0][1]["pixel_aspect"] = serde_json::json!(1.0);
        doc["timeline"]["markers_future"] = serde_json::json!([{ "tick": 5, "name": "beat" }]);
        doc["timeline"]["tracks"][0][1]["clips"][0][1]["future_field"] = serde_json::json!(true);
        std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();

        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.name, "tolerant");
        assert_eq!(loaded.media_count(), 1);
        assert_eq!(loaded.timeline().clip_count(), 1);

        // The other half of the contract: unknown fields are ignored, not
        // preserved — a resave by this build drops them.
        let resaved = dir.path().join("resaved.cutlass");
        loaded.save_to_file(&resaved).unwrap();
        let json = std::fs::read_to_string(&resaved).unwrap();
        assert!(!json.contains("color_management"));
        assert!(!json.contains("markers_future"));
    }

    #[test]
    fn migration_chain_covers_every_supported_version() {
        // Walking from the oldest supported version must find a step arm
        // for every gap — `migrate_document` panics if a schema bump landed
        // without its migration step.
        for from in 1..=PROJECT_SCHEMA_VERSION {
            let mut doc = serde_json::json!({});
            migrate_document(&mut doc, from);
            // No current step rewrites anything (v1 shapes are valid v2).
            assert_eq!(doc, serde_json::json!({}));
        }
    }

    #[test]
    fn unknown_root_project_field_does_not_trip_envelope_unwrap() {
        // A current-version file may carry unknown root fields named
        // "project"/"version" (tolerance policy); the legacy-envelope
        // detection must not unwrap those.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notenvelope.cutlass");
        std::fs::write(
            &path,
            r#"{"schema":{"version":2,"kind":"cutlass.project"},"id":7,"name":"keep","metadata":{},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]},"project":"unknown-extension","version":9}"#,
        )
        .unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.name, "keep");
        assert_eq!(loaded.schema.version, 2);
    }

    #[test]
    fn load_accepts_legacy_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.cutlass");
        std::fs::write(
            &path,
            r#"{"version":1,"project":{"schema":{"version":1,"kind":"cutlass.project"},"id":1,"name":"legacy","metadata":{"description":"old envelope"},"media":[],"timeline":{"frame_rate":{"num":24,"den":1},"tracks":[],"order":[],"clip_index":[]}}}"#,
        )
        .unwrap();
        let loaded = Project::load_from_file(&path).unwrap();
        assert_eq!(loaded.name, "legacy");
        assert_eq!(loaded.metadata.description, "old envelope");
    }
}
