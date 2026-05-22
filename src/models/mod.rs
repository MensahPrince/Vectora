//! Editor domain model (Rust) and Slint view-model conversion.
//!
//! Tracks and clips are stored in `HashMap`s for O(1) lookup by id — the
//! shape the command layer and agent will mutate. Slint has no map type,
//! so we project ordered `VecModel`s at the boundary via [`dto`].

pub mod clip;
pub mod dto;
pub mod project;
pub mod rational;
pub mod rational_time;
pub mod sample;
pub mod sequence;
pub mod time_range;
pub mod track;

pub use clip::Clip;
pub use project::Project;
pub use rational::Rational;
pub use rational_time::RationalTime;
pub use sequence::Sequence;
pub use time_range::TimeRange;
pub use track::Track;

pub use sample::sample_project;
