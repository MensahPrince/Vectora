use cutlass_models::{ClipId, ProjectId, TrackId};

pub fn track_id_from_str(s: &str) -> Option<TrackId> {
    s.parse::<u64>().ok().map(TrackId::from_raw)
}

pub fn clip_id_from_str(s: &str) -> Option<ClipId> {
    s.parse::<u64>().ok().map(ClipId::from_raw)
}

pub fn project_id_to_str(id: ProjectId) -> String {
    id.raw().to_string()
}

pub fn track_id_to_str(id: TrackId) -> String {
    id.raw().to_string()
}

pub fn clip_id_to_str(id: ClipId) -> String {
    id.raw().to_string()
}
