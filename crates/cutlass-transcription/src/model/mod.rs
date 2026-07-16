//! Built-in Whisper model catalog, download, and transactional installation.

mod catalog;
mod download;
// File is `fs.rs` per the split layout; module name avoids shadowing `std::fs`
// for unit tests that resolve filesystem helpers via `use super::*`.
#[path = "fs.rs"]
mod filesystem;
mod manager;

#[cfg(test)]
mod tests;

pub use catalog::{
    DownloadError, ModelIntegrityError, ModelManagerError, ModelSpec, ModelStatus, WhisperModel,
};
pub use download::{DownloadReader, HttpDownloader, ModelDownloader};
pub use manager::ModelManager;

// Keep private helpers and the std items former `model.rs` imported in the
// `model` namespace so unit tests (`use super::*`) keep compiling after the split.
#[cfg(test)]
#[allow(unused_imports)]
use std::fs;
#[cfg(test)]
#[allow(unused_imports)]
use std::io::{self, Read, Write};
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use std::time::Duration;

#[cfg(test)]
#[allow(unused_imports)]
use catalog::*;
#[cfg(test)]
#[allow(unused_imports)]
use download::*;
#[cfg(test)]
#[allow(unused_imports)]
use filesystem::*;
#[cfg(test)]
#[allow(unused_imports)]
use manager::*;
