//! The error taxonomy for repository access.
//!
//! Libraries return typed errors, never stringly-typed ones, because the UI
//! needs to distinguish "this is recoverable and here is the button that fixes
//! it" from "report this".

use std::path::PathBuf;

use thiserror::Error;

/// Anything that can go wrong reading or writing a repository.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("{0} is not a Git repository")]
    NotARepository(PathBuf),

    /// No `git` on `PATH`. Checked once at startup so it surfaces as an
    /// actionable message rather than a mystery failure on the first push.
    #[error("no `git` binary found on PATH")]
    GitNotFound,

    #[error("git {found} is too old; hideGit requires {required} or newer")]
    GitTooOld { found: Version, required: Version },

    #[error("reference not found: {0}")]
    RefNotFound(String),

    #[error("could not read the repository: {0}")]
    Repo(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A three-component version number, as reported by `git --version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}
