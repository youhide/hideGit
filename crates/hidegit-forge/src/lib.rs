//! Forge integration for hideGit: pull requests and the alerts built on them.
//!
//! **M1 ships this crate as a stub.** The `Forge` trait and its
//! provider-neutral data model are finalised against the GitHub implementation
//! in M4; see `docs/ARCHITECTURE.md#forge-integration`.

/// Which hosting provider a repository lives on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeId {
    GitHub,
}
