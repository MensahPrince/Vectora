use serde::{Deserialize, Serialize};

use super::rational::Rational;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RationalTime {
    pub value: i32,
    pub rate: Rational,
}
