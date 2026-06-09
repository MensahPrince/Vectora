use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("probe not implemented")]
    NotImplemented,
}
