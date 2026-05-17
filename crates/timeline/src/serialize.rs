use serde::Deserialize;

use crate::error::TimelineError;
use crate::model::{Project, CURRENT_SCHEMA_VERSION};

#[derive(Deserialize)]
struct SchemaProbe {
    schema_version: u32,
}

/// Serialize a project to JSON (no file I/O).
pub fn serialize_project(project: &Project) -> Result<String, TimelineError> {
    serde_json::to_string_pretty(project).map_err(|e| TimelineError::Serde(e.to_string()))
}

/// Deserialize a project from JSON with schema version checks.
pub fn deserialize_project(json: &str) -> Result<Project, TimelineError> {
    let probe: SchemaProbe =
        serde_json::from_str(json).map_err(|e| TimelineError::Serde(e.to_string()))?;
    match probe.schema_version {
        v if v == CURRENT_SCHEMA_VERSION => serde_json::from_str(json)
            .map_err(|e| TimelineError::Serde(e.to_string())),
        v if v > CURRENT_SCHEMA_VERSION => Err(TimelineError::SchemaUnsupported {
            found: v,
            supported_max: CURRENT_SCHEMA_VERSION,
        }),
        v => Err(TimelineError::SchemaUnsupported {
            found: v,
            supported_max: CURRENT_SCHEMA_VERSION,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    #[test]
    fn round_trip_empty_project() {
        let p = Project::new().with_default_video_track();
        let json = serialize_project(&p).unwrap();
        let back = deserialize_project(&json).unwrap();
        assert_eq!(p.schema_version, back.schema_version);
        assert_eq!(p.id, back.id);
        assert_eq!(p.tracks.len(), back.tracks.len());
        assert!(back.history.undo_depth() == 0);
    }
}
