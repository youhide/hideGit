//! What can go wrong talking to a forge.
//!
//! Typed, for the same reason `hidegit-core` is: the UI has to tell "this is
//! recoverable and here is the button that fixes it" from "report this". A
//! forge failure is nearly always the first kind — not connected, not
//! installed, out of budget — and each of those has a different next action.

use thiserror::Error;
use time::OffsetDateTime;

use crate::model::RepoRef;

#[derive(Debug, Error)]
pub enum ForgeError {
    /// No token. The next action is to connect, not to retry.
    #[error("not signed in to {0}")]
    NotAuthenticated(crate::model::ForgeId),

    /// Authenticated correctly, but the hideGit app is not installed on this
    /// repository.
    ///
    /// Its own variant rather than an empty list, because "you have no open
    /// pull requests" and "hideGit cannot see this repository" look identical
    /// in a sidebar and mean opposite things. The URL is carried because an
    /// error the user cannot act on is barely better than no error.
    #[error("hideGit is not installed on {repo}")]
    NotInstalled {
        repo: Box<RepoRef>,
        install_url: String,
    },

    /// The API budget ran out. Polling stops until `reset` rather than
    /// retrying, which is what would exhaust it further.
    #[error("rate limited until {reset}")]
    RateLimited { reset: OffsetDateTime },

    /// No OS keychain — a headless Linux session with no Secret Service, say.
    ///
    /// Forge features are disabled rather than falling back to a file. That is
    /// ADR-0003's deliberate refusal to silently downgrade credential storage,
    /// so it is reported plainly instead of being worked around.
    #[error("no OS keychain is available, so forge features are disabled")]
    NoKeychain,

    #[error("could not reach {host}")]
    Network {
        host: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The API answered, and said no. `message` is the provider's own wording,
    /// kept verbatim for the same reason Git's stderr is.
    #[error("{status}: {message}")]
    Api { status: u16, message: String },

    /// A remote URL that names no forge repository — a local path, or a host
    /// hideGit has no implementation for.
    #[error("{0} is not a forge repository hideGit recognises")]
    NotAForgeRepository(String),

    /// The response parsed, but a field hideGit needs was absent.
    ///
    /// A schema is a parsing surface the same way subprocess output is, and it
    /// changes without warning. Translation fails soft wherever it can; this is
    /// for the cases where it cannot.
    #[error("unexpected response from {host}: {detail}")]
    Malformed { host: String, detail: String },

    #[error(transparent)]
    DeviceFlow(#[from] DeviceFlowError),

    #[error("{operation} is not implemented yet; it lands in {milestone}")]
    NotImplementedYet {
        operation: &'static str,
        milestone: &'static str,
    },
}

/// Why the device flow did not produce a token.
///
/// Separate from [`ForgeError`] because each of these is a different sentence
/// to show somebody staring at a code they just typed, and collapsing them into
/// one "authorisation failed" would throw away the only useful part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeviceFlowError {
    /// The user code expired before it was approved. Recoverable by starting
    /// again, so the UI offers exactly that.
    #[error("the code expired before it was approved")]
    Expired,

    #[error("authorisation was declined")]
    Denied,

    /// The app has device flow turned off in its settings. Nothing the user can
    /// do, so it says so rather than offering a retry that cannot work.
    #[error("this app does not have device flow enabled")]
    Disabled,

    /// No client ID compiled in or configured — a source build against an
    /// unregistered app. A personal access token still works.
    #[error("no OAuth client ID is configured for this build")]
    NotConfigured,
}
