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
