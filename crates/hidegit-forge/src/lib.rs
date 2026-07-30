//! Forge integration for hideGit: pull requests and the alerts built on them.
//!
//! This crate talks to hosting providers behind the [`Forge`] trait. GitHub is
//! the only implementation before 1.0; see
//! `docs/adr/0003-forge-github-first.md`.
//!
//! **It must not depend on `iced`.** Polling is scheduled here and driven by a
//! `Subscription` in `hidegit-ui`, the same split `hidegit-core`'s filesystem
//! watcher already uses — the schedule is testable without a window, and the
//! toolkit stays in one crate.

pub mod auth;
pub mod detect;
pub mod error;
pub mod github;
pub mod model;
pub mod notify;
pub mod poll;
pub mod prefs;
pub mod secret;
pub mod token;

pub use error::{DeviceFlowError, ForgeError};
pub use github::{Endpoint, GitHub};
pub use model::*;
pub use notify::{Desktop, Notifier};
pub use poll::{Activity, Alert, AlertEvent, Next, Observed, Schedule, Watcher};
pub use prefs::{AlertPrefs, EventPrefs, QuietHours};
pub use secret::SecretString;
pub use token::{Keychain, StoredToken, TokenStore};

#[cfg(any(test, feature = "fake"))]
pub use notify::Recorder;
#[cfg(any(test, feature = "fake"))]
pub use token::MemoryStore;

use std::fmt;

use async_trait::async_trait;

/// A code for the user to type, and where to type it.
///
/// Surfaced before polling begins, because the device flow's entire premise is
/// that the user goes and approves it somewhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    /// How long the code is good for.
    pub expires_in: std::time::Duration,
}

/// How to obtain a token.
pub enum AuthFlow {
    /// OAuth 2.0 Device Authorization Flow (RFC 8628).
    ///
    /// Carries a callback because the flow is not one round trip: the code has
    /// to reach the screen before polling starts, and the caller is what knows
    /// how to put it there. `authenticate` still returns once, with the
    /// identity, so nothing else has to know the flow has two halves.
    Device(Box<dyn Fn(DeviceCode) + Send + Sync>),

    /// A personal access token the user created and pasted.
    ///
    /// A first-class fallback rather than a legacy path: it is genuinely better
    /// for GitHub Enterprise, for restricted environments, and for anyone who
    /// would rather scope and revoke a credential themselves.
    Token(SecretString),
}

impl fmt::Debug for AuthFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthFlow::Device(_) => f.write_str("AuthFlow::Device"),
            AuthFlow::Token(_) => f.write_str("AuthFlow::Token(redacted)"),
        }
    }
}

/// A hosting provider.
///
/// Deliberately narrow: list pull requests, fetch one, create one, report poll
/// state. Anything beyond that opens the browser through [`Forge::web_url`]
/// rather than growing an API surface to maintain against four providers'
/// churn.
///
/// `async_trait` rather than native `async fn`, because the UI holds a
/// `dyn Forge` and native async methods are not dyn-compatible.
#[async_trait]
pub trait Forge: Send + Sync + fmt::Debug {
    fn id(&self) -> ForgeId;

    /// Whether this provider hosts the repository a remote URL points at.
    ///
    /// Static, because it answers which implementation to build before one
    /// exists.
    fn detect(remote_url: &str) -> Option<RepoRef>
    where
        Self: Sized;

    async fn authenticate(&self, flow: AuthFlow) -> Result<Identity, ForgeError>;

    async fn current_user(&self) -> Result<Identity, ForgeError>;

    /// One poll. `since` carries the cursor from the previous one.
    async fn pull_requests(
        &self,
        repo: &RepoRef,
        since: Option<PollCursor>,
    ) -> Result<PollResult<Vec<PullRequest>>, ForgeError>;

    async fn pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequestDetail, ForgeError>;

    async fn create_pull_request(
        &self,
        repo: &RepoRef,
        draft: NewPullRequest,
    ) -> Result<PullRequest, ForgeError>;

    fn web_url(&self, repo: &RepoRef, target: WebTarget) -> String;
}
