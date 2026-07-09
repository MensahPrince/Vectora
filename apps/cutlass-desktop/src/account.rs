//! Cutlass account: the Rust half of `AccountBackend`.
//!
//! One worker thread owns everything network-flavored about the account —
//! device-authorization sign-in (system browser against the website's
//! better-auth), JWT refresh, balance fetches, the "Buy credits" hand-off
//! to the website's account page, and the startup update check. Tokens
//! live in the OS keychain (`cutlass_cloud::token_store`); nothing here
//! writes secrets to disk.
//!
//! Threading mirrors `cloud.rs`: commands in over a channel, results
//! hopped to the UI thread with `invoke_from_event_loop`.

use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Sender, unbounded};
use cutlass_cloud::token_store::{self, StoredSession};
use cutlass_cloud::{CloudClient, auth};
use slint::ComponentHandle;
use tracing::{info, warn};

use crate::AccountBackend;

/// How long the device-token poll waits for browser approval before
/// giving up (the user may need to sign in at the provider first).
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

enum Command {
    /// Startup: restore the keychain session (refreshing if stale) and run
    /// the update check.
    Init,
    SignIn,
    SignOut,
    RefreshBalance,
    BuyCredits,
    OpenUpdate,
}

#[derive(Clone)]
pub struct AccountHandle {
    tx: Sender<Command>,
}

impl AccountHandle {
    pub fn init(&self) {
        let _ = self.tx.send(Command::Init);
    }

    pub fn sign_in(&self) {
        let _ = self.tx.send(Command::SignIn);
    }

    pub fn sign_out(&self) {
        let _ = self.tx.send(Command::SignOut);
    }

    pub fn refresh_balance(&self) {
        let _ = self.tx.send(Command::RefreshBalance);
    }

    pub fn buy_credits(&self) {
        let _ = self.tx.send(Command::BuyCredits);
    }

    pub fn open_update(&self) {
        let _ = self.tx.send(Command::OpenUpdate);
    }
}

pub struct AccountWorker {
    handle: AccountHandle,
    _join: JoinHandle<()>,
}

impl AccountWorker {
    pub fn spawn(backend_weak: slint::Weak<crate::AppWindow>) -> Result<Self, String> {
        let (tx, rx) = unbounded::<Command>();
        let join = std::thread::Builder::new()
            .name("cutlass-account".into())
            .spawn(move || {
                let mut worker = Worker::new(backend_weak);
                while let Ok(command) = rx.recv() {
                    worker.run(command);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            handle: AccountHandle { tx },
            _join: join,
        })
    }

    pub fn handle(&self) -> AccountHandle {
        self.handle.clone()
    }
}

/// Backend (API) base URL: `[account] base_url` in config.toml, then the
/// `CUTLASS_API_BASE` env override, then production.
pub fn base_url() -> String {
    let from_settings = cutlass_settings::load(&cutlass_settings::default_config_path())
        .map(|s| s.account.base_url)
        .unwrap_or_default();
    if !from_settings.is_empty() {
        return from_settings;
    }
    std::env::var("CUTLASS_API_BASE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| cutlass_cloud::DEFAULT_BASE_URL.to_string())
}

/// Auth base URL (the website hosting better-auth): `[account]
/// auth_base_url` in config.toml, then the `CUTLASS_AUTH_BASE` env
/// override, then production.
pub fn auth_base_url() -> String {
    let from_settings = cutlass_settings::load(&cutlass_settings::default_config_path())
        .map(|s| s.account.auth_base_url)
        .unwrap_or_default();
    if !from_settings.is_empty() {
        return from_settings;
    }
    std::env::var("CUTLASS_AUTH_BASE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| cutlass_cloud::DEFAULT_AUTH_BASE_URL.to_string())
}

/// A fresh access JWT from the keychain session, refreshing (and
/// re-storing) when stale. The shared entry point for every surface that
/// talks to the backend as the signed-in user from its own thread (the
/// managed chat provider, generation fallbacks).
pub fn managed_access_token() -> Result<String, String> {
    let mut session =
        token_store::load().ok_or("Not signed in — sign in under Settings > Account.")?;
    if session.needs_refresh() {
        let pair = auth::refresh(&auth_base_url(), &session.refresh_token)
            .map_err(|e| format!("Session expired — sign in again. ({e})"))?;
        session = StoredSession::from_pair(&pair);
        if let Err(e) = token_store::store(&session) {
            warn!("keychain store after refresh failed: {e}");
        }
    }
    Ok(session.access_token)
}

struct Worker {
    backend_weak: slint::Weak<crate::AppWindow>,
    base_url: String,
    auth_base_url: String,
    session: Option<StoredSession>,
    update_url: String,
}

impl Worker {
    fn new(backend_weak: slint::Weak<crate::AppWindow>) -> Self {
        Self {
            backend_weak,
            base_url: base_url(),
            auth_base_url: auth_base_url(),
            session: None,
            update_url: String::new(),
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::Init => {
                self.restore_session();
                self.check_for_update();
            }
            Command::SignIn => self.sign_in(),
            Command::SignOut => self.sign_out(),
            Command::RefreshBalance => self.fetch_account_state(),
            Command::BuyCredits => {
                // The device-flow approval already left a browser session
                // on the website, so /account lands signed-in.
                open_in_browser(&format!("{}/account", self.auth_base_url));
            }
            Command::OpenUpdate => {
                if !self.update_url.is_empty() {
                    open_in_browser(&self.update_url);
                }
            }
        }
    }

    // --- session lifecycle ------------------------------------------------

    fn restore_session(&mut self) {
        let Some(mut session) = token_store::load() else {
            return;
        };
        if session.needs_refresh() {
            match auth::refresh(&self.auth_base_url, &session.refresh_token) {
                Ok(pair) => {
                    session = StoredSession::from_pair(&pair);
                    if let Err(e) = token_store::store(&session) {
                        warn!("keychain store after refresh failed: {e}");
                    }
                }
                Err(e) => {
                    // A dead refresh token means the session is over; a
                    // network blip means try again later — either way the
                    // safe UI state is signed-out (the token stays in the
                    // keychain for the next launch unless it was rejected).
                    if matches!(e, cutlass_cloud::CloudError::Status { .. }) {
                        info!("stored session rejected, signing out: {e}");
                        token_store::clear();
                    } else {
                        warn!("session refresh failed (offline?): {e}");
                    }
                    return;
                }
            }
        }
        self.session = Some(session);
        self.fetch_account_state();
    }

    fn sign_in(&mut self) {
        self.publish(|b| {
            b.set_status("signing-in".into());
            b.set_user_code("".into());
            b.set_error("".into());
        });
        // Device flow: show the short code, open the website's approval
        // page (code pre-filled), poll until the user approves there.
        let result = auth::start_device_sign_in(&self.auth_base_url).and_then(|pending| {
            let user_code = pending.user_code().to_string();
            self.publish(move |b| b.set_user_code(user_code.as_str().into()));
            open_in_browser(pending.verification_url());
            pending.wait(SIGN_IN_TIMEOUT)
        });
        match result.map(|pair| StoredSession::from_pair(&pair)) {
            Ok(session) => {
                if let Err(e) = token_store::store(&session) {
                    warn!("keychain store failed (session won't survive restart): {e}");
                }
                self.session = Some(session);
                info!("signed in via the browser device flow");
                self.publish(|b| b.set_user_code("".into()));
                self.fetch_account_state();
            }
            Err(e) => {
                warn!("sign-in failed: {e}");
                let message = sign_in_error_message(&e);
                self.publish(move |b| {
                    b.set_status("signed-out".into());
                    b.set_user_code("".into());
                    b.set_error(message.as_str().into());
                });
            }
        }
    }

    fn sign_out(&mut self) {
        if let Some(session) = self.session.take() {
            // Server revocation is best-effort; the keychain wipe is what
            // actually signs out.
            if let Err(e) = auth::sign_out(&self.auth_base_url, &session.refresh_token) {
                warn!("server sign-out failed (token revoked locally anyway): {e}");
            }
        }
        token_store::clear();
        self.publish(|b| {
            b.set_status("signed-out".into());
            b.set_email("".into());
            b.set_provider("".into());
            b.set_user_code("".into());
            b.set_credits(0);
            b.set_balance_known(false);
            b.set_error("".into());
        });
    }

    /// Refresh the access JWT if stale, then return a client for the
    /// account routes. `None` means signed out.
    fn authed_client(&mut self) -> Option<auth::AuthedClient> {
        let session = self.session.as_mut()?;
        if session.needs_refresh() {
            match auth::refresh(&self.auth_base_url, &session.refresh_token) {
                Ok(pair) => {
                    *session = StoredSession::from_pair(&pair);
                    if let Err(e) = token_store::store(session) {
                        warn!("keychain store after refresh failed: {e}");
                    }
                }
                Err(e) => {
                    warn!("token refresh failed: {e}");
                    return None;
                }
            }
        }
        Some(auth::AuthedClient::new(
            &self.base_url,
            &session.access_token,
        ))
    }

    // --- account state (identity + balance) ---------------------------------

    fn fetch_account_state(&mut self) {
        let Some(client) = self.authed_client() else {
            return;
        };
        let me = match client.me() {
            Ok(me) => me,
            Err(e) => {
                warn!("GET /v1/me failed: {e}");
                let message = format!("Couldn't load the account: {e}");
                self.publish(move |b| b.set_error(message.as_str().into()));
                return;
            }
        };
        let balance = client.balance();

        let email = if me.email.is_empty() {
            me.display_name.clone()
        } else {
            me.email.clone()
        };
        let provider = me.provider.clone();
        self.publish(move |b| {
            b.set_status("signed-in".into());
            b.set_email(email.as_str().into());
            b.set_provider(provider.as_str().into());
            b.set_error("".into());
            match &balance {
                Ok(balance) => {
                    b.set_credits(balance.credits.min(i32::MAX as i64) as i32);
                    b.set_balance_known(true);
                }
                Err(_) => b.set_balance_known(false),
            }
        });
    }

    // --- update nudge -------------------------------------------------------

    fn check_for_update(&mut self) {
        let client = CloudClient::new(&self.base_url, None);
        let latest = match client.latest_version() {
            Ok(latest) => latest,
            // Silent: the nudge is best-effort and most launches are offline
            // from the backend's point of view during development.
            Err(e) => {
                info!("update check skipped: {e}");
                return;
            }
        };
        if !version_is_newer(&latest.version, env!("CARGO_PKG_VERSION")) {
            return;
        }
        info!(
            "update available: {} (running {})",
            latest.version,
            env!("CARGO_PKG_VERSION")
        );
        self.update_url = latest.download_url.clone();
        let version = latest.version.clone();
        self.publish(move |b| {
            b.set_update_available(true);
            b.set_update_version(version.as_str().into());
        });
    }

    // --- UI publishing ------------------------------------------------------

    fn publish(&self, f: impl FnOnce(AccountBackend<'_>) + Send + 'static) {
        let weak = self.backend_weak.clone();
        if let Err(e) = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                f(app.global::<AccountBackend>());
            }
        }) {
            warn!("account UI update failed: {e}");
        }
    }
}

fn sign_in_error_message(e: &cutlass_cloud::CloudError) -> String {
    use cutlass_cloud::CloudError;
    match e {
        CloudError::Cancelled => "Sign-in timed out — try again.".into(),
        CloudError::Network(_) => {
            "Couldn't reach the Cutlass service — check your connection.".into()
        }
        CloudError::Status { status, .. } => format!("Sign-in was rejected ({status})."),
        // Device-flow outcomes arrive as protocol errors with readable
        // messages ("sign-in was denied in the browser", "the sign-in
        // code expired — try again").
        CloudError::Protocol(message) => format!("Sign-in failed: {message}."),
        _ => "Sign-in failed — try again.".into(),
    }
}

/// `major.minor.patch` triple, ignoring any pre-release suffix
/// (`0.5.3-alpha.0` → `(0, 5, 3)`).
fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Whether `remote` is a strictly newer release than `local`. Unparseable
/// versions never nudge (a bad catalog entry must not spam every user).
fn version_is_newer(remote: &str, local: &str) -> bool {
    match (version_triple(remote), version_triple(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

/// Open a URL in the default browser, off the UI thread. The URL is
/// always one of ours or one the backend vouched for (authorize URL,
/// checkout URL, download page).
fn open_in_browser(url: &str) {
    let spawn = |program: &str, args: &[&str]| {
        if let Err(e) = std::process::Command::new(program).args(args).spawn() {
            warn!("failed to open browser: {e}");
        }
    };
    #[cfg(target_os = "macos")]
    spawn("open", &[url]);
    #[cfg(target_os = "windows")]
    spawn("cmd", &["/C", "start", "", url]);
    #[cfg(all(unix, not(target_os = "macos")))]
    spawn("xdg-open", &[url]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_triples() {
        assert_eq!(version_triple("0.5.3-alpha.0"), Some((0, 5, 3)));
        assert_eq!(version_triple("1.2.3"), Some((1, 2, 3)));
        assert_eq!(version_triple("1.2"), Some((1, 2, 0)));
        assert_eq!(version_triple("nope"), None);
    }

    #[test]
    fn newer_version_detection() {
        assert!(version_is_newer("0.6.0", "0.5.3-alpha.0"));
        assert!(version_is_newer("0.5.4", "0.5.3"));
        assert!(
            !version_is_newer("0.5.3", "0.5.3-alpha.0"),
            "same triple never nudges"
        );
        assert!(!version_is_newer("0.5.2", "0.5.3"));
        assert!(!version_is_newer("garbage", "0.5.3"));
    }
}
