//! The authed half of the cloud client: device-authorization sign-in
//! (RFC 8628 against the website's better-auth), JWT refresh, and the
//! backend's account routes (identity, balance, ledger).
//!
//! Sign-in shape: identity lives in `cutlass-website` (better-auth), so
//! the app never sees provider passwords **or** provider choice — it asks
//! the website for a device code, opens the verification page in the
//! system browser, shows the short user code, and polls until the user
//! approves there. The poll yields a long-lived **session token**; a
//! short-lived **JWT** (verified by `cutlass-backend` via JWKS) is then
//! fetched from `/api/auth/token`. In [`TokenPair`] terms the session
//! token sits in the `refresh_token` seat and the JWT in `access_token`;
//! [`refresh`] re-fetches the JWT with the session token. The desktop
//! stores the pair in the OS keychain ([`crate::token_store`]) — never in
//! a file.
//!
//! Everything here blocks and belongs on a worker thread, like the rest
//! of the crate.

use std::time::Duration;

use serde::Deserialize;

use crate::dto::{Balance, LedgerPage, Me, TokenPair};
use crate::error::CloudError;

/// The OAuth client id the website's device-authorization plugin accepts.
pub const DESKTOP_CLIENT_ID: &str = "cutlass-desktop";

/// Fallback JWT lifetime when the token carries no readable `exp`
/// (better-auth's default is 15 minutes; refreshing early is harmless).
const DEFAULT_JWT_TTL_SECONDS: u64 = 15 * 60;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
}

/// `POST /api/auth/device/code` response (RFC 8628 §3.2).
#[derive(Debug, Clone, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: String,
    /// Seconds until the codes expire.
    #[serde(default)]
    expires_in: u64,
    /// Minimum polling interval in seconds.
    #[serde(default)]
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenSuccess {
    /// better-auth's device flow hands back a **session token** here
    /// (bearer-usable against `/api/auth/*`), not a JWT.
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// A device sign-in in flight: the code request succeeded and the user is
/// (about to be) looking at the website's approval page.
pub struct PendingDeviceSignIn {
    auth_base: String,
    agent: ureq::Agent,
    response: DeviceCodeResponse,
}

/// Start a device-authorization sign-in against the website. The caller
/// shows [`PendingDeviceSignIn::user_code`], opens
/// [`PendingDeviceSignIn::verification_url`] in the system browser, then
/// calls [`PendingDeviceSignIn::wait`] (on a worker thread — it blocks).
pub fn start_device_sign_in(auth_base: &str) -> Result<PendingDeviceSignIn, CloudError> {
    let auth_base = auth_base.trim_end_matches('/').to_string();
    let agent = agent();
    let url = format!("{auth_base}/api/auth/device/code");
    let response = agent
        .post(&url)
        .send_json(serde_json::json!({ "client_id": DESKTOP_CLIENT_ID }))
        .map_err(|e| CloudError::from_ureq(&url, e))?;
    let response: DeviceCodeResponse = response
        .into_json()
        .map_err(|e| CloudError::Protocol(format!("{url}: {e}")))?;
    Ok(PendingDeviceSignIn {
        auth_base,
        agent,
        response,
    })
}

impl PendingDeviceSignIn {
    /// The short code the user confirms on the website ("ABCD-1234"
    /// style, shown verbatim in the UI).
    pub fn user_code(&self) -> &str {
        &self.response.user_code
    }

    /// The page to open in the system browser — the complete URI (code
    /// pre-filled) when the server offers one.
    pub fn verification_url(&self) -> &str {
        if self.response.verification_uri_complete.is_empty() {
            &self.response.verification_uri
        } else {
            &self.response.verification_uri_complete
        }
    }

    /// Block until the user approves in the browser (or `timeout` / the
    /// code's own expiry elapses), then exchange the session token for a
    /// JWT. Polls at the server-mandated interval, honoring `slow_down`.
    pub fn wait(self, timeout: Duration) -> Result<TokenPair, CloudError> {
        let timeout = match self.response.expires_in {
            0 => timeout,
            s => timeout.min(Duration::from_secs(s)),
        };
        let deadline = std::time::Instant::now() + timeout;
        let mut interval = Duration::from_secs(self.response.interval.max(1));
        let url = format!("{}/api/auth/device/token", self.auth_base);

        loop {
            if std::time::Instant::now() + interval > deadline {
                return Err(CloudError::Cancelled);
            }
            std::thread::sleep(interval);

            let result = self.agent.post(&url).send_json(serde_json::json!({
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                "device_code": self.response.device_code,
                "client_id": DESKTOP_CLIENT_ID,
            }));
            match result {
                Ok(response) => {
                    let token: DeviceTokenSuccess = response
                        .into_json()
                        .map_err(|e| CloudError::Protocol(format!("{url}: {e}")))?;
                    return session_to_pair(&self.agent, &self.auth_base, &token.access_token);
                }
                // RFC 8628 signals "keep going" through error responses.
                Err(ureq::Error::Status(_, response)) => {
                    let error: DeviceTokenError = response
                        .into_json()
                        .map_err(|e| CloudError::Protocol(format!("{url}: {e}")))?;
                    match error.error.as_str() {
                        "authorization_pending" => {}
                        "slow_down" => interval += Duration::from_secs(5),
                        "access_denied" => {
                            return Err(CloudError::Protocol(
                                "sign-in was denied in the browser".into(),
                            ));
                        }
                        "expired_token" => {
                            return Err(CloudError::Protocol(
                                "the sign-in code expired — try again".into(),
                            ));
                        }
                        other => {
                            let detail = error.error_description.unwrap_or_default();
                            return Err(CloudError::Protocol(format!(
                                "device sign-in failed: {other} {detail}"
                            )));
                        }
                    }
                }
                Err(e) => return Err(CloudError::from_ureq(&url, e)),
            }
        }
    }
}

/// Exchange a better-auth **session token** for a fresh short-lived JWT:
/// `GET /api/auth/token` with the session token as bearer (the website
/// runs better-auth's `bearer` plugin). This is the "refresh" of the new
/// world — the session token itself only changes by signing in again.
pub fn refresh(auth_base: &str, session_token: &str) -> Result<TokenPair, CloudError> {
    session_to_pair(&agent(), auth_base.trim_end_matches('/'), session_token)
}

fn session_to_pair(
    agent: &ureq::Agent,
    auth_base: &str,
    session_token: &str,
) -> Result<TokenPair, CloudError> {
    #[derive(Deserialize)]
    struct TokenResponse {
        token: String,
    }
    let url = format!("{auth_base}/api/auth/token");
    let response = agent
        .get(&url)
        .set("Authorization", &format!("Bearer {session_token}"))
        .call()
        .map_err(|e| CloudError::from_ureq(&url, e))?;
    let token: TokenResponse = response
        .into_json()
        .map_err(|e| CloudError::Protocol(format!("{url}: {e}")))?;
    let expires_in = jwt_ttl_seconds(&token.token).unwrap_or(DEFAULT_JWT_TTL_SECONDS);
    Ok(TokenPair {
        access_token: token.token,
        refresh_token: session_token.to_string(),
        expires_in,
    })
}

/// `POST /api/auth/sign-out` — revoke the session server-side.
/// Best-effort: the local keychain wipe is what actually signs out.
pub fn sign_out(auth_base: &str, session_token: &str) -> Result<(), CloudError> {
    let auth_base = auth_base.trim_end_matches('/');
    let url = format!("{auth_base}/api/auth/sign-out");
    agent()
        .post(&url)
        .set("Authorization", &format!("Bearer {session_token}"))
        .send_json(serde_json::json!({}))
        .map_err(|e| CloudError::from_ureq(&url, e))?;
    Ok(())
}

/// Seconds until the JWT's `exp`, read without verifying (the backend
/// verifies; the client only schedules its own refresh).
fn jwt_ttl_seconds(token: &str) -> Option<u64> {
    #[derive(Deserialize)]
    struct Claims {
        exp: u64,
    }
    let payload = token.split('.').nth(1)?;
    let claims: Claims = serde_json::from_slice(&base64url_decode(payload)?).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(claims.exp.saturating_sub(now))
}

/// URL-safe base64 without padding (RFC 4648 §5) — the JWT segment
/// encoding.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        if chunk.len() == 1 {
            return None;
        }
        let mut n: u32 = 0;
        for &c in chunk {
            n = (n << 6) | value(c)?;
        }
        n <<= 6 * (4 - chunk.len()) as u32;
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Authed client
// ---------------------------------------------------------------------------

/// Blocking client over the backend's account routes; every request sends
/// the bearer JWT.
pub struct AuthedClient {
    base_url: String,
    agent: ureq::Agent,
    access_token: String,
}

impl AuthedClient {
    pub fn new(base_url: &str, access_token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            agent: agent(),
            access_token: access_token.to_string(),
        }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, CloudError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.access_token))
            .call()
            .map_err(|e| CloudError::from_ureq(&url, e))?;
        response
            .into_json()
            .map_err(|e| CloudError::Protocol(format!("{url}: {e}")))
    }

    fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T, CloudError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.access_token))
            .send_json(body)
            .map_err(|e| CloudError::from_ureq(&url, e))?;
        response
            .into_json()
            .map_err(|e| CloudError::Protocol(format!("{url}: {e}")))
    }

    /// `GET /v1/me`.
    pub fn me(&self) -> Result<Me, CloudError> {
        self.get("/v1/me")
    }

    /// `GET /v1/credits/balance`.
    pub fn balance(&self) -> Result<Balance, CloudError> {
        self.get("/v1/credits/balance")
    }

    /// `GET /v1/credits/history`.
    pub fn history(&self, cursor: Option<&str>) -> Result<LedgerPage, CloudError> {
        match cursor {
            Some(cursor) => self.get(&format!("/v1/credits/history?cursor={cursor}")),
            None => self.get("/v1/credits/history"),
        }
    }

    /// `POST /v1/generate/{image|video|tts}` — start a managed job. A 402
    /// surfaces as [`CloudError::Status`] (out of credits / spend cap).
    pub fn generate(
        &self,
        kind: &str,
        request: &crate::dto::GenerateRequest,
    ) -> Result<crate::dto::Job, CloudError> {
        self.post(
            &format!("/v1/generate/{kind}"),
            serde_json::to_value(request).expect("generate request serializes"),
        )
    }

    /// `GET /v1/jobs/{id}` — poll a managed job.
    pub fn job(&self, id: &str) -> Result<crate::dto::Job, CloudError> {
        self.get(&format!("/v1/jobs/{id}"))
    }

    /// The bearer token, for surfaces that speak to the backend through
    /// other clients (the managed chat provider in `cutlass-ai`).
    pub fn access_token(&self) -> &str {
        &self.access_token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_decoding_round_trips() {
        assert_eq!(base64url_decode(""), Some(vec![]));
        assert_eq!(base64url_decode("Zg"), Some(b"f".to_vec()));
        assert_eq!(base64url_decode("Zm8"), Some(b"fo".to_vec()));
        assert_eq!(base64url_decode("Zm9v"), Some(b"foo".to_vec()));
        assert_eq!(base64url_decode("Zm9vYmFy"), Some(b"foobar".to_vec()));
        assert_eq!(base64url_decode("-_8"), Some(vec![0xfb, 0xff]));
        assert_eq!(base64url_decode("a"), None, "lone symbol is malformed");
        assert_eq!(base64url_decode("Zg=="), None, "padding is not accepted");
    }

    #[test]
    fn jwt_ttl_reads_exp() {
        // {"alg":"EdDSA"} . {"sub":"u","exp":<far future>} . (unverified)
        let far = 4_000_000_000u64;
        let header = "eyJhbGciOiJFZERTQSJ9";
        let payload_json = format!(r#"{{"sub":"u","exp":{far}}}"#);
        // Encode via the decoder's inverse using std — quick local encode.
        let encode = |data: &[u8]| {
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for chunk in data.chunks(3) {
                let b = [
                    chunk[0],
                    chunk.get(1).copied().unwrap_or(0),
                    chunk.get(2).copied().unwrap_or(0),
                ];
                let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                out.push(ALPHABET[(n >> 18) as usize & 63] as char);
                out.push(ALPHABET[(n >> 12) as usize & 63] as char);
                if chunk.len() > 1 {
                    out.push(ALPHABET[(n >> 6) as usize & 63] as char);
                }
                if chunk.len() > 2 {
                    out.push(ALPHABET[n as usize & 63] as char);
                }
            }
            out
        };
        let token = format!("{header}.{}.sig", encode(payload_json.as_bytes()));
        let ttl = jwt_ttl_seconds(&token).expect("ttl readable");
        assert!(ttl > 1_000_000, "far-future exp yields a large ttl");

        assert_eq!(jwt_ttl_seconds("garbage"), None);
        assert_eq!(jwt_ttl_seconds("a.b.c"), None);
    }

    #[test]
    fn expired_jwt_ttl_is_zero() {
        let header = "eyJhbGciOiJFZERTQSJ9";
        // exp: 1 (1970) — saturates to 0, never underflows.
        let payload = "eyJzdWIiOiJ1IiwiZXhwIjoxfQ";
        let token = format!("{header}.{payload}.sig");
        assert_eq!(jwt_ttl_seconds(&token), Some(0));
    }
}
