//! Where tokens live.
//!
//! The OS keychain, and nowhere else. Never the config file, never a log, never
//! sent anywhere but the provider's own API — see `SECURITY.md`.
//!
//! **If no keychain is available, forge features are disabled.** There is no
//! file fallback and there will not be one: falling back silently would mean a
//! user who chose an encrypted credential store gets a plaintext one without
//! being told. That is ADR-0003's deliberate refusal, and
//! [`ForgeError::NoKeychain`] is how it is reported.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::ForgeError;
use crate::secret::SecretString;

/// The keychain service name. Matches the `ProjectDirs` qualifier and the macOS
/// bundle identifier, so a user auditing their keychain sees one recognisable
/// entry rather than a bare "hidegit".
const SERVICE: &str = "com.youhide.hidegit";

/// How close to expiry a token is refreshed.
///
/// A token that expires during the request carrying it produces a 401 the user
/// has to see; a minute of slack costs one early refresh a day.
const REFRESH_MARGIN: time::Duration = time::Duration::minutes(1);

/// A token as it sits in the keychain.
///
/// A GitHub App's user token expires — eight hours by default — and arrives
/// with a refresh token, so both are stored together with the expiry that
/// decides when to use the second. When the App has expiry turned off, GitHub
/// sends no refresh token and both optional fields stay `None`; nothing has to
/// be configured for either shape to work.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToken {
    /// Who the token belongs to, so a session can be restored without spending
    /// a request to ask.
    pub login: String,
    pub access: SecretString,
    /// Unix seconds. `None` for a token that does not expire — a personal
    /// access token, or a GitHub App with expiry turned off.
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub refresh: Option<SecretString>,
    #[serde(default)]
    pub refresh_expires_at: Option<i64>,
}

impl fmt::Debug for StoredToken {
    /// Manual, so that adding a field cannot accidentally print one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredToken")
            .field("login", &self.login)
            .field("expires_at", &self.expires_at)
            .field("refreshable", &self.refresh.is_some())
            .finish()
    }
}

impl StoredToken {
    /// A token with no expiry — what a personal access token is.
    pub fn permanent(login: impl Into<String>, access: SecretString) -> Self {
        Self {
            login: login.into(),
            access,
            expires_at: None,
            refresh: None,
            refresh_expires_at: None,
        }
    }

    /// Whether this token should be refreshed before it is used again.
    ///
    /// A token with no expiry is never stale, and a token with no refresh token
    /// cannot be refreshed however stale it is — in that case the honest answer
    /// is that the user has to sign in again, which is what the resulting 401
    /// tells them.
    pub fn needs_refresh(&self, now: OffsetDateTime) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };
        self.refresh.is_some() && now + REFRESH_MARGIN >= from_unix(expires_at)
    }
}

pub fn to_unix(at: OffsetDateTime) -> i64 {
    at.unix_timestamp()
}

fn from_unix(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// Somewhere a token can be kept.
///
/// A trait so the keychain is not in the way of a test. The real implementation
/// cannot be exercised in CI — Linux runners have no Secret Service and macOS
/// runners have a locked keychain — so everything *around* storage is tested
/// against [`MemoryStore`], and the keychain itself stays a manual check.
pub trait TokenStore: Send + Sync + fmt::Debug {
    /// `Ok(None)` when nothing is stored, which is not an error: it is the
    /// ordinary state of a first run.
    fn load(&self, account: &str) -> Result<Option<StoredToken>, ForgeError>;
    fn save(&self, account: &str, token: &StoredToken) -> Result<(), ForgeError>;
    fn clear(&self, account: &str) -> Result<(), ForgeError>;
}

/// Reads a token without holding the async runtime.
///
/// **`keyring` is synchronous, and on macOS it can raise an authorisation
/// dialog that waits for a human.** Called straight from an `async fn`, that
/// stops the executor thread it is on from serving anything else — observed as
/// a repository that never opened because the keychain prompt at startup was
/// still up. Every keychain touch therefore goes through one of these, and none
/// of them is `pub`: there is no way to reach a store from async code without
/// leaving the runtime alone.
pub(crate) async fn load(
    store: &Arc<dyn TokenStore>,
    account: &str,
) -> Result<Option<StoredToken>, ForgeError> {
    let store = Arc::clone(store);
    let account = account.to_owned();
    off_thread(move || store.load(&account)).await
}

pub(crate) async fn save(
    store: &Arc<dyn TokenStore>,
    account: &str,
    token: StoredToken,
) -> Result<(), ForgeError> {
    let store = Arc::clone(store);
    let account = account.to_owned();
    off_thread(move || store.save(&account, &token)).await
}

pub(crate) async fn clear(store: &Arc<dyn TokenStore>, account: &str) -> Result<(), ForgeError> {
    let store = Arc::clone(store);
    let account = account.to_owned();
    off_thread(move || store.clear(&account)).await
}

/// Runs one keychain call on the blocking pool.
async fn off_thread<T, F>(work: F) -> Result<T, ForgeError>
where
    F: FnOnce() -> Result<T, ForgeError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        // The pool cancelled or panicked. Reported as an unusable keychain,
        // because from the caller's side that is what it is.
        Err(error) => {
            tracing::error!(%error, "the keychain task did not finish");
            Err(ForgeError::NoKeychain)
        }
    }
}

/// The environment variable that turns the keychain off.
///
/// Set to anything non-empty and hideGit behaves exactly as it does on a
/// machine with no keychain at all: forge features are disabled and say so,
/// and nothing is read or written.
///
/// This exists for **development builds**. macOS ties a keychain entry's access
/// list to the requesting binary's code signature, and an unsigned bundle gets
/// a fresh identity on every `cargo build` — so every launch of a freshly built
/// hideGit raises the authorisation dialog again, and someone testing an
/// unrelated change has to type a password to get a window. Skipping the
/// keychain is honest about what it costs: you are signed out.
pub const DISABLE_VAR: &str = "HIDEGIT_NO_KEYCHAIN";

/// True when [`DISABLE_VAR`] asks for the keychain to be left alone.
pub fn disabled_by_environment() -> bool {
    std::env::var_os(DISABLE_VAR).is_some_and(|value| !value.is_empty())
}

/// The OS keychain: Keychain Services, Credential Manager, or Secret Service.
#[derive(Debug, Default)]
pub struct Keychain;

impl Keychain {
    fn entry(account: &str) -> Result<keyring::Entry, ForgeError> {
        keyring::Entry::new(SERVICE, account).map_err(classify)
    }
}

impl TokenStore for Keychain {
    fn load(&self, account: &str) -> Result<Option<StoredToken>, ForgeError> {
        // Checked here rather than at construction so the variable governs every
        // route to the keychain, including one added later by someone who never
        // read this comment.
        if disabled_by_environment() {
            return Err(ForgeError::NoKeychain);
        }
        match Self::entry(account)?.get_password() {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(token) => Ok(Some(token)),
                // A keychain entry hideGit cannot read is treated as absent
                // rather than fatal: the shape changed between versions, and
                // asking somebody to sign in again beats refusing to start.
                // The entry's *contents* are never logged.
                Err(error) => {
                    tracing::warn!(%error, account, "stored token is unreadable; ignoring it");
                    Ok(None)
                }
            },
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(classify(error)),
        }
    }

    fn save(&self, account: &str, token: &StoredToken) -> Result<(), ForgeError> {
        if disabled_by_environment() {
            return Err(ForgeError::NoKeychain);
        }
        let json = serde_json::to_string(token).map_err(|error| ForgeError::Malformed {
            host: account.to_owned(),
            detail: error.to_string(),
        })?;
        Self::entry(account)?.set_password(&json).map_err(classify)
    }

    fn clear(&self, account: &str) -> Result<(), ForgeError> {
        if disabled_by_environment() {
            return Err(ForgeError::NoKeychain);
        }
        match Self::entry(account)?.delete_credential() {
            // Signing out of an account that was never signed in is what the
            // caller asked for, not a failure.
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(classify(error)),
        }
    }
}

/// Distinguishes "there is no keychain here" from "the keychain said no".
///
/// The first disables forge features with an explanation; the second is a
/// locked or refusing store, which the user can usually unlock.
fn classify(error: keyring::Error) -> ForgeError {
    match error {
        keyring::Error::NoDefaultStore | keyring::Error::NotSupportedByStore(_) => {
            ForgeError::NoKeychain
        }
        other => ForgeError::Malformed {
            host: "the OS keychain".to_owned(),
            detail: other.to_string(),
        },
    }
}

/// A store that forgets when the process does.
///
/// For tests only. It is never wired up as a fallback: see the module note.
#[cfg(any(test, feature = "fake"))]
#[derive(Debug, Default)]
pub struct MemoryStore(std::sync::Mutex<std::collections::HashMap<String, StoredToken>>);

#[cfg(any(test, feature = "fake"))]
impl TokenStore for MemoryStore {
    fn load(&self, account: &str) -> Result<Option<StoredToken>, ForgeError> {
        Ok(self.0.lock().expect("not poisoned").get(account).cloned())
    }

    fn save(&self, account: &str, token: &StoredToken) -> Result<(), ForgeError> {
        if disabled_by_environment() {
            return Err(ForgeError::NoKeychain);
        }
        self.0
            .lock()
            .expect("not poisoned")
            .insert(account.to_owned(), token.clone());
        Ok(())
    }

    fn clear(&self, account: &str) -> Result<(), ForgeError> {
        if disabled_by_environment() {
            return Err(ForgeError::NoKeychain);
        }
        self.0.lock().expect("not poisoned").remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// The opt-out is read from the environment on every call, so this asserts
    /// the predicate rather than mutating the process environment — which is
    /// global, and would race every other test in the binary.
    #[test]
    fn the_keychain_opt_out_is_named_once_and_needs_a_value() {
        assert_eq!(super::DISABLE_VAR, "HIDEGIT_NO_KEYCHAIN");
        // Unset in a normal test run, so the keychain is live by default and a
        // developer never loses their session by accident.
        assert!(!super::disabled_by_environment());
    }

    use super::*;

    fn expiring(in_seconds: i64) -> StoredToken {
        StoredToken {
            login: "youhide".to_owned(),
            access: SecretString::new("ghu_token"),
            expires_at: Some(to_unix(OffsetDateTime::now_utc()) + in_seconds),
            refresh: Some(SecretString::new("ghr_token")),
            refresh_expires_at: None,
        }
    }

    #[test]
    fn a_token_about_to_expire_is_refreshed_before_it_is_used() {
        let now = OffsetDateTime::now_utc();

        assert!(expiring(30).needs_refresh(now), "inside the margin");
        assert!(expiring(-1).needs_refresh(now), "already expired");
        assert!(!expiring(3_600).needs_refresh(now), "plenty of time left");
    }

    #[test]
    fn a_token_that_does_not_expire_is_never_stale() {
        // A personal access token, or a GitHub App with expiry turned off. The
        // refresh path is simply never taken, and nothing has to be configured
        // for that to be true.
        let pat = StoredToken::permanent("youhide", SecretString::new("ghp_token"));

        assert!(!pat.needs_refresh(OffsetDateTime::now_utc()));
        assert!(pat.refresh.is_none());
    }

    #[test]
    fn a_token_with_no_refresh_token_is_not_reported_as_refreshable() {
        // Claiming otherwise would send a refresh request with nothing to
        // refresh. The honest outcome is a 401 telling the user to sign in.
        let mut token = expiring(-1);
        token.refresh = None;

        assert!(!token.needs_refresh(OffsetDateTime::now_utc()));
    }

    #[test]
    fn neither_the_access_token_nor_the_refresh_token_appears_in_debug_output() {
        let printed = format!("{:?}", expiring(3_600));

        assert!(printed.contains("youhide"));
        assert!(printed.contains("refreshable: true"));
        assert!(!printed.contains("ghu_"), "{printed}");
        assert!(!printed.contains("ghr_"), "{printed}");
    }

    #[test]
    fn a_token_round_trips_through_a_store() {
        let store = MemoryStore::default();
        let token = expiring(3_600);

        assert_eq!(store.load("github.com").unwrap(), None);

        store.save("github.com", &token).unwrap();
        let read = store.load("github.com").unwrap().expect("it was saved");
        assert_eq!(read.access.expose(), "ghu_token");
        assert_eq!(
            read.refresh.map(|r| r.expose().to_owned()).as_deref(),
            Some("ghr_token")
        );

        store.clear("github.com").unwrap();
        assert_eq!(store.load("github.com").unwrap(), None);
    }

    #[test]
    fn signing_out_when_nothing_is_stored_is_what_was_asked_for() {
        assert!(MemoryStore::default().clear("github.com").is_ok());
    }

    #[test]
    fn a_stored_token_survives_a_round_trip_through_json() {
        // The keychain holds one JSON string, so this is the format the real
        // store reads back. `#[serde(default)]` on the optional fields is what
        // lets a token written by an older build still load.
        let json = r#"{"login":"youhide","access":"ghp_token"}"#;
        let token: StoredToken = serde_json::from_str(json).unwrap();

        assert_eq!(token.login, "youhide");
        assert_eq!(token.access.expose(), "ghp_token");
        assert_eq!(token.expires_at, None);
    }
}
