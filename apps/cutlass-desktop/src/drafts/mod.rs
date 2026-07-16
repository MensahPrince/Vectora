//! App-owned project drafts (CapCut-style storage).
//!
//! Cutlass owns every project: there is no user-chosen `.cutlass` path. Each
//! project lives in its own directory under `projects/` in the per-user OS
//! data dir (see [`crate::paths`]) and auto-saves continuously, so the user
//! never saves by hand and no edit is lost on a clean exit. A directory holds
//! the project itself (`project.cutlass` — a plain project file the engine
//! reads and writes through the normal `Save`/`Load` path) and a small
//! `meta.json` sidecar caching the display name so the launch gallery can
//! list projects without parsing every project file.
//!
//! `.cutlass` files are no longer a user-facing concept; they enter via
//! [`import_external`] (Open file…) and leave via Export. The draft directory
//! is the identity — addressed everywhere by its `project.cutlass` path, so
//! the existing path-keyed engine plumbing (`Save`/`Load`) is reused as-is.

mod api;
mod fs_ops;
mod identity;
mod meta;
#[cfg(test)]
mod tests;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

// Submodule items are imported for sibling `use super::*` visibility and for
// the crate-visible re-exports below.
#[allow(unused_imports)]
use api::*;
#[allow(unused_imports)]
use fs_ops::*;
#[allow(unused_imports)]
use identity::*;
#[allow(unused_imports)]
use meta::*;

// `delete_checked` is part of the public drafts surface even when unused in-crate.
pub(crate) use api::list_checked;
#[allow(unused_imports)]
pub use api::{
    create, delete, delete_checked, import_external, list, relative_time, write_meta, DraftSummary,
};
pub(crate) use identity::{draft_id_from_project, resolve_draft_id};
pub use identity::{project_file, root_dir};
