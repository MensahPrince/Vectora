use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::clip::Clip;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub name: String,
    /// Lane order on the timeline (bottom-to-top or top-to-bottom — just
    /// stay consistent with how the UI iterates).
    pub clip_order: Vec<String>,
    pub clips: HashMap<String, Clip>,
}
