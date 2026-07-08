//! Session-token storage in the OS keychain (macOS Keychain, Windows
//! Credential Manager, Linux keyutils) via the `keyring` crate.
//!
//! Tokens **never** touch `config.toml` or any other file — an API key in
//! a dotfile is a support ticket and a security hole; the keychain is the
//! platform's answer. On a Linux box without a working keyring the store
//! degrades to "not signed in" with a visible warning (Linux desktop is
//! dormant anyway).

use serde::{Deserialize, Serialize};

use crate::dto::TokenPair;
use crate::error::CloudError;

const SERVICE: &str = "app.cutlass.desktop";
const ACCOUNT: &str = "session";

/// What the keychain entry holds: the token pair plus the wall-clock
/// expiry computed at store time (the keychain is the only durable home
/// for this, since nothing else about the session persists).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when the access token expires.
    pub expires_at: u64,
}

impl StoredSession {
    /// Bundle a fresh [`TokenPair`] with its computed expiry.
    pub fn from_pair(pair: &TokenPair) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            access_token: pair.access_token.clone(),
            refresh_token: pair.refresh_token.clone(),
            expires_at: now.saturating_add(pair.expires_in),
        }
    }

    /// Whether the access token is (about to be) stale — refresh first.
    /// The 60 s slack absorbs clock skew and in-flight time.
    pub fn needs_refresh(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now + 60 >= self.expires_at
    }
}

fn entry() -> Result<keyring::Entry, CloudError> {
    keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|e| CloudError::Io(std::io::Error::other(format!("keychain unavailable: {e}"))))
}

/// Persist the session in the OS keychain (overwrites any previous one).
pub fn store(session: &StoredSession) -> Result<(), CloudError> {
    let json = serde_json::to_string(session)
        .map_err(|e| CloudError::Protocol(format!("session serialize: {e}")))?;
    entry()?
        .set_password(&json)
        .map_err(|e| CloudError::Io(std::io::Error::other(format!("keychain write: {e}"))))
}

/// The stored session, or `None` when signed out (no entry). A keychain
/// that exists but can't be read degrades to `None` with a warning — the
/// user just signs in again.
pub fn load() -> Option<StoredSession> {
    let entry = match entry() {
        Ok(entry) => entry,
        Err(e) => {
            tracing::warn!("keychain unavailable, treating as signed out: {e}");
            return None;
        }
    };
    match entry.get_password() {
        Ok(json) => match serde_json::from_str(&json) {
            Ok(session) => Some(session),
            Err(e) => {
                tracing::warn!("stored session unreadable, treating as signed out: {e}");
                None
            }
        },
        Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            tracing::warn!("keychain read failed, treating as signed out: {e}");
            None
        }
    }
}

/// Remove the stored session (sign-out). Missing entries are fine.
pub fn clear() {
    if let Ok(entry) = entry() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => tracing::warn!("keychain delete failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_session_expiry_math() {
        let pair = TokenPair {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_in: 3600,
        };
        let session = StoredSession::from_pair(&pair);
        assert!(!session.needs_refresh(), "an hour out is fresh");

        let stale = StoredSession {
            expires_at: 0,
            ..session
        };
        assert!(stale.needs_refresh());
    }
}
