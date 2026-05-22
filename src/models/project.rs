use serde::{Deserialize, Serialize};

use super::sequence::Sequence;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub title: String,
    pub sequence: Sequence,
}
