//! The error taxonomy for repository access.
//!
//! Libraries return typed errors, never stringly-typed ones, because the UI
//! needs to distinguish "this is recoverable and here is the button that fixes
//! it" from "report this". See `docs/ARCHITECTURE.md#error-taxonomy`.

use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

use crate::model::Conflict;

/// Anything that can go wrong reading or writing a repository.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("{} is not a Git repository", .0.display())]
    NotARepository(PathBuf),

    /// No `git` on `PATH`. Checked once at startup so it surfaces as an
    /// actionable message rather than a mystery failure on the first push.
    #[error("no `git` binary was found on PATH")]
    GitNotFound,

    #[error("git {found} is too old; hideGit needs {required} or newer")]
    GitTooOld { found: Version, required: Version },

    #[error("reference not found: {0}")]
    RefNotFound(String),

    /// An expected outcome of merge and rebase, not a failure. It routes to
    /// the conflict resolution UI rather than to an error dialog.
    #[error("{} path(s) conflict", .0.len())]
    Conflict(Vec<Conflict>),

    /// Another Git process holds the index, or a killed one left the lock
    /// behind. Reported rather than silently deleted — removing a lock that a
    /// live process owns corrupts the index.
    #[error("the index is locked by {}", .0.display())]
    IndexLocked(PathBuf),

    #[error(transparent)]
    Auth(#[from] AuthError),

    /// A long operation the user stopped.
    ///
    /// Not a failure — it is what was asked for — so the UI reports it silently
    /// unless `stale_lock` is set. Killing `git` mid-write can leave `index.lock`
    /// behind, and hideGit names it rather than deleting it: the lock may equally
    /// belong to a process that is still running.
    #[error("the operation was cancelled")]
    Cancelled { stale_lock: Option<PathBuf> },

    /// A `git` invocation exited non-zero. Carries the argument vector and
    /// Git's own stderr, so a bug report contains what is needed to reproduce
    /// it and the UI can show Git's message verbatim instead of paraphrasing.
    #[error("git {} failed: {stderr}", argv.join(" "))]
    Command {
        argv: Vec<String>,
        /// `None` when the process was killed by a signal.
        status: Option<i32>,
        stderr: String,
    },

    /// A failure from gitoxide.
    ///
    /// `gix` has no single crate-wide error type — each operation has its own —
    /// so the source is boxed and `context` names the operation that failed.
    #[error("{context}")]
    Gix {
        context: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A `GitBackend` method whose milestone has not landed.
    ///
    /// The trait carries its full signature from the start so the read/write
    /// split stays auditable in one file; the write half fills in from M2.
    #[error("{operation} is not implemented yet; it lands in {milestone}")]
    NotImplementedYet {
        operation: &'static str,
        milestone: &'static str,
    },
}

impl GitError {
    /// Wraps a gitoxide error, naming the operation that produced it.
    pub(crate) fn gix<E>(context: &'static str, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        GitError::Gix {
            context,
            source: Box::new(source),
        }
    }
}

/// Recognises a credential failure in a network command's stderr.
///
/// The point is to let the UI offer the action that fixes it — "no credential
/// helper answered for `origin`" with the remote named — instead of only showing
/// a wall of text.
///
/// **Matching on Git's wording is a real maintenance cost.** Its messages are
/// good but they are not an interface, and they change. That is mitigated by
/// returning the original [`GitError::Command`] untouched whenever nothing
/// matches: a phrase hideGit stops recognising degrades to "here is exactly what
/// git said", which is never wrong, rather than to a confident wrong diagnosis.
pub(crate) fn classify_remote_failure(remote: &str, error: GitError) -> GitError {
    let GitError::Command { ref stderr, .. } = error else {
        return error;
    };

    // `GIT_TERMINAL_PROMPT=0` is set on every invocation precisely so a missing
    // credential fails fast instead of hanging on a prompt nobody can see. These
    // are the shapes that failure takes.
    const NO_CREDENTIALS: &[&str] = &[
        "terminal prompts disabled",
        "could not read Username",
        "could not read Password",
        "no askpass",
    ];
    const REJECTED: &[&str] = &[
        "Authentication failed",
        "Permission denied (publickey)",
        "Invalid username or token",
        "access denied or repository not exported",
    ];

    let contains = |needles: &[&str]| needles.iter().any(|needle| stderr.contains(needle));

    if contains(NO_CREDENTIALS) {
        return AuthError::NoCredentials {
            remote: remote.to_owned(),
        }
        .into();
    }
    if contains(REJECTED) {
        return AuthError::Rejected {
            remote: remote.to_owned(),
        }
        .into();
    }

    error
}

/// Why authentication failed. Populated from M3, when writes to a remote
/// start going through the user's credential helper.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("authentication failed for {remote}")]
    Rejected { remote: String },

    /// `git` wanted to prompt on a terminal that does not exist. hideGit sets
    /// `GIT_TERMINAL_PROMPT=0` precisely so this fails fast instead of hanging.
    #[error("{remote} needs credentials, and no credential helper supplied them")]
    NoCredentials { remote: String },
}

/// A three-component version number, as reported by `git --version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(stderr: &str) -> GitError {
        GitError::Command {
            argv: vec!["push".to_owned(), "origin".to_owned()],
            status: Some(128),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn a_prompt_that_could_not_be_shown_is_a_missing_credential() {
        let error = classify_remote_failure(
            "origin",
            failure(
                "fatal: could not read Username for 'https://example.invalid': \
                 terminal prompts disabled\n",
            ),
        );

        match error {
            GitError::Auth(AuthError::NoCredentials { remote }) => assert_eq!(remote, "origin"),
            other => panic!("expected NoCredentials, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_key_or_password_is_a_rejection() {
        for stderr in [
            "remote: Invalid username or token.\nfatal: Authentication failed for 'https://example.invalid'\n",
            "git@example.invalid: Permission denied (publickey).\nfatal: Could not read from remote repository.\n",
        ] {
            match classify_remote_failure("upstream", failure(stderr)) {
                GitError::Auth(AuthError::Rejected { remote }) => assert_eq!(remote, "upstream"),
                other => panic!("expected Rejected for {stderr:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_unrecognised_failure_keeps_gits_own_words_rather_than_guessing() {
        // The load-bearing case: Git's wording is not an interface, so a phrase
        // hideGit does not know must degrade to the verbatim message instead of
        // being forced into an auth diagnosis it cannot support.
        let stderr = "error: failed to push some refs to 'https://example.invalid'\n\
                      hint: Updates were rejected because the tip of your current branch is behind\n";

        match classify_remote_failure("origin", failure(stderr)) {
            GitError::Command {
                argv,
                status,
                stderr: kept,
            } => {
                assert_eq!(argv, vec!["push", "origin"]);
                assert_eq!(status, Some(128));
                assert_eq!(kept, stderr, "stderr must survive verbatim");
            }
            other => panic!("expected the original Command error, got {other:?}"),
        }
    }

    #[test]
    fn only_a_command_failure_is_classified() {
        // A cancellation or an IO error is not a credential problem, and must
        // pass through untouched.
        let cancelled = classify_remote_failure(
            "origin",
            GitError::Cancelled {
                stale_lock: Some(PathBuf::from("/repo/.git/index.lock")),
            },
        );
        assert!(matches!(
            cancelled,
            GitError::Cancelled {
                stale_lock: Some(_)
            }
        ));
    }
}
