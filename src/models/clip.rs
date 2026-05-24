use serde::{Deserialize, Serialize};

use super::rational_time::RationalTime;
use super::time_range::TimeRange;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    pub name: String,
    pub timeline_start: RationalTime,
    pub source_range: TimeRange,
}
