//! Git domain model and repository access for hideGit.
//!
//! This crate has no UI and no network. Everything that reads a repository
//! goes through `gix`; everything that writes to a remote or rewrites history
//! shells out to the system `git` binary. See
//! `docs/adr/0002-git-backend-hybrid.md`.

pub mod error;

pub use error::GitError;
