use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiskCacheError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("source {source_id} is not registered")]
    SourceNotRegistered { source_id: u64 },
    #[error("disk full: frame cache writes are paused")]
    DiskFull,
}
