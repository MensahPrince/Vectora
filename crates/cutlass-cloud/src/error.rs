//! One error type for everything this crate does; variants map to the UX
//! the Library needs (offline → placeholder, rate-limited → retry hint,
//! cancelled → silence).

/// Why a cloud operation failed.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    /// Could not reach the host at all (offline, DNS, refused). The Library
    /// sections degrade to their placeholders on this.
    #[error("network: {0}")]
    Network(String),

    /// The server answered with an error status. `retryable` is true for
    /// 429/5xx so callers can back off instead of giving up.
    #[error("server returned {status}: {message}")]
    Status {
        status: u16,
        message: String,
        retryable: bool,
    },

    /// The response body didn't parse as the expected DTO — a contract
    /// drift or a broken proxy, never expected in normal operation.
    #[error("bad response: {0}")]
    Protocol(String),

    /// Local filesystem trouble (cache dir, downloads).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The caller's cancel flag was set mid-operation.
    #[error("cancelled")]
    Cancelled,
}

impl CloudError {
    pub(crate) fn from_ureq(url: &str, err: ureq::Error) -> Self {
        match err {
            ureq::Error::Status(status, response) => {
                let message = response
                    .into_string()
                    .unwrap_or_else(|_| "<unreadable error body>".into());
                let message = truncate(&message, 300);
                CloudError::Status {
                    status,
                    message,
                    retryable: status == 429 || status >= 500,
                }
            }
            ureq::Error::Transport(t) => CloudError::Network(format!("{url}: {t}")),
        }
    }
}

/// Cut `s` to at most `max` bytes on a char boundary, appending `…` when
/// something was dropped (server error bodies can be huge HTML pages).
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
