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

    /// The app has device flow turned off in its settings. Retrying cannot
    /// help, so it says what would.
    #[error("this app does not have device flow enabled")]
    Disabled,

    /// No client ID compiled in or configured — a source build against an
    /// unregistered app. A personal access token still works.
    #[error("no OAuth client ID is configured for this build")]
    NotConfigured,
}

impl DeviceFlowError {
    /// What to do about it.
    ///
    /// Kept apart from `Display` because a toast shows the summary as a title
    /// and the detail underneath, and because this is the half worth copying:
    /// `DeviceFlow(Disabled)` on a clipboard tells nobody anything, where a
    /// sentence naming the checkbox does.
    ///
    /// hideGit paraphrasing rather than quoting the provider is deliberate
    /// here. GitHub's own wording — *"Device Flow must be explicitly enabled
    /// for this App"* — states the problem again; what a person stuck on it
    /// needs is where the setting lives and what still works meanwhile.
    pub fn next_step(self) -> &'static str {
        match self {
            DeviceFlowError::Expired => {
                "The code is good for fifteen minutes. Start signing in again to get a new one."
            }
            DeviceFlowError::Denied => {
                "Nothing was authorised, and no token was stored. \
                 Start again if that was not what you meant."
            }
            DeviceFlowError::Disabled => {
                "Turn on \"Enable Device Flow\" in the app's settings on GitHub — \
                 Settings › Developer settings › GitHub Apps › hideGit. \
                 A personal access token works meanwhile."
            }
            DeviceFlowError::NotConfigured => {
                "This build has no OAuth client ID compiled in. \
                 Use a personal access token."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_way_the_device_flow_can_end_says_what_to_do_about_it() {
        // The summary names the problem; this is the half worth copying, and a
        // variant added later must not quietly ship without one.
        for error in [
            DeviceFlowError::Expired,
            DeviceFlowError::Denied,
            DeviceFlowError::Disabled,
            DeviceFlowError::NotConfigured,
        ] {
            let step = error.next_step();
            assert!(step.len() > 20, "{error:?} has no next step worth reading");
            assert!(
                step.ends_with('.'),
                "{error:?} reads as a fragment rather than a sentence"
            );
        }
    }

    #[test]
    fn the_one_that_needs_a_setting_changed_names_where_it_is() {
        // "This app does not have device flow enabled" is a fact. Where the
        // checkbox lives is the part somebody stuck on it actually needs.
        let step = DeviceFlowError::Disabled.next_step();

        assert!(step.contains("Enable Device Flow"));
        assert!(step.contains("GitHub Apps"));
        assert!(
            step.contains("personal access token"),
            "and what still works meanwhile"
        );
    }
}
