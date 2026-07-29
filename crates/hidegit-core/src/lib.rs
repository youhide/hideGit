//! Git domain model and repository access for hideGit.
//!
//! This crate has no UI and no network. That constraint is load-bearing: it is
//! what lets the graph algorithm and the diff model be exercised headless, in
//! CI, without a window and without a token.
//!
//! Everything that reads a repository goes through [`gix`]; everything that
//! writes to a remote or rewrites history shells out to the system `git`
//! binary. Both sides sit behind [`GitBackend`], which is what makes the split
//! auditable in one file. See `docs/adr/0002-git-backend-hybrid.md`.

pub mod backend;
pub mod clone;
pub mod error;
pub mod graph;
pub mod model;
pub mod ops;
pub mod patch;
pub mod process;
pub mod watch;

#[cfg(any(test, feature = "fixture"))]
pub mod fixture;

pub use backend::{GitBackend, HybridBackend};
pub use clone::clone_repository;
pub use error::{AuthError, GitError, Version};
pub use model::*;
pub use process::{MINIMUM_GIT_VERSION, git_preflight};

#[cfg(any(test, feature = "fake"))]
pub use backend::FakeBackend;
