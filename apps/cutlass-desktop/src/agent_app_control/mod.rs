//! Synchronous, bounded app-control tools for the desktop agent.
//!
//! Calls originate on the agent thread. Argument parsing and settings IO stay
//! there; every Slint/window access is dispatched to the UI event loop and
//! acknowledged over a bounded channel.

mod execute;
mod parse;
mod specs;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, bounded};
use cutlass_ai::{HostToolSpec, ToolOutput, ToolTier};
use serde_json::{Map, Value, json};
use slint::{ComponentHandle, Model, SharedString};

// Submodule items are imported for sibling `use super::*` visibility and for
// the crate-visible re-exports below.
#[allow(unused_imports)]
use execute::*;
#[allow(unused_imports)]
use parse::*;
#[allow(unused_imports)]
use specs::*;

pub use execute::call;
pub use specs::specs;
