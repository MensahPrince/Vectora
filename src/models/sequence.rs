use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::rational::Rational;

use super::track::Track;
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub id: String,
    pub name: String,
    pub fps: Rational,
    pub drop_frame: bool,
    pub track_order: Vec<String>,
    pub tracks: HashMap<String, Track>,
    pub width: f32,
    pub height: f32,
}
