//! The one seam every Git access goes through.
//!
//! There is exactly one production implementation, [`HybridBackend`], which
//! routes each method to either `gix` or the system `git` binary. The trait
//! exists for three concrete reasons, none of them speculative abstraction:
//!
//! 1. **It is the migration path.** As gitoxide lands `push` and a complete
//!    rebase workflow, methods move from the CLI side to the gix side one at a
//!    time, and this trait's test suite is the contract that says the move did
//!    not change behaviour.
//! 2. **It makes the split auditable.** One file answers "what do we shell out
//!    for, and why is this one still on the CLI side?"
//! 3. **It enables a fake.** [`FakeBackend`] lets `hidegit-ui` be tested
//!    against scripted repository states, including error states that are
//!    painful to produce with a real repository.
//!
//! See `docs/ARCHITECTURE.md#the-gitbackend-seam`.

mod gix_read;
mod hybrid;

#[cfg(any(test, feature = "fake"))]
mod fake;

pub use hybrid::HybridBackend;

#[cfg(any(test, feature = "fake"))]
pub use fake::FakeBackend;

use std::path::Path;

use crate::error::GitError;
use crate::model::{
    Blob, Commit, CommitDetail, Diff, DiffTarget, Head, LogPage, ObjectId, Refs, RepoState,
    RevSpec, WorktreeStatus,
};
use crate::ops::{
    Blame, CheckoutTarget, CommitOpts, FetchOutcome, MergeOpts, MergeOutcome, Patch, ProgressSink,
    PushSpec, RebasePlan, SequenceOutcome, StashOp, StashOutcome,
};

/// Everything hideGit can ask of a repository.
///
/// The read half runs on `gix`; the write half shells out to `git`. Methods
/// whose milestone has not landed return [`GitError::NotImplementedYet`]
/// rather than being absent, so the shape of the whole surface stays visible
/// in one place.
pub trait GitBackend: Send + Sync + std::fmt::Debug {
    /// Opens a repository, searching upward from `path` for one.
    fn open(path: &Path) -> Result<Self, GitError>
    where
        Self: Sized;

    // ---- read: gix ---------------------------------------------------

    /// The worktree root, or the repository directory for a bare repository.
    fn workdir(&self) -> &Path;

    /// The `.git` directory.
    fn git_dir(&self) -> &Path;

    fn head(&self) -> Result<Head, GitError>;

    fn refs(&self) -> Result<Refs, GitError>;

    /// Which operation, if any, the repository is in the middle of.
    ///
    /// Read separately from [`GitBackend::status`] because it is cheap — a
    /// handful of `stat` calls — and the UI consults it before rendering any
    /// action at all.
    fn repo_state(&self) -> Result<RepoState, GitError>;

    /// One page of history.
    ///
    /// Commits come back newest first, in topological order with commit date
    /// as the tiebreak: a commit always precedes its parents, and the layout
    /// does not re-sort. Sorting by date alone would produce a graph with
    /// edges pointing upward, because commit dates lie — clock skew and
    /// rebases both produce out-of-order timestamps.
    fn log(&self, spec: &RevSpec, page: LogPage) -> Result<Vec<Commit>, GitError>;

    /// How many commits `spec` reaches, for sizing the scrollbar.
    fn commit_count(&self, spec: &RevSpec) -> Result<usize, GitError>;

    /// One commit with the file list the detail pane shows.
    fn commit(&self, id: ObjectId) -> Result<CommitDetail, GitError>;

    fn diff(&self, target: &DiffTarget) -> Result<Diff, GitError>;

    fn read_blob(&self, id: ObjectId) -> Result<Blob, GitError>;

    /// Working directory state. Lands in M2.
    fn status(&self) -> Result<WorktreeStatus, GitError>;

    /// Line-by-line authorship. Lands in M6.
    fn blame(&self, path: &Path, at: ObjectId) -> Result<Blame, GitError>;

    /// Drops any cached traversal, after something changed the repository.
    ///
    /// The history walk is memoised because computing topological order is the
    /// expensive part of drawing a graph; this is the one call that says the
    /// memo is stale.
    fn invalidate(&self);

    // ---- write: git CLI ----------------------------------------------

    /// Lands in M2.
    fn stage(&self, paths: &[&Path]) -> Result<(), GitError>;

    /// Lands in M2.
    fn stage_patch(&self, patch: &Patch) -> Result<(), GitError>;

    /// Lands in M2.
    fn unstage(&self, paths: &[&Path]) -> Result<(), GitError>;

    /// Lands in M2.
    fn discard(&self, paths: &[&Path]) -> Result<(), GitError>;

    /// Lands in M2.
    fn create_commit(&self, message: &str, opts: CommitOpts) -> Result<ObjectId, GitError>;

    /// Lands in M3.
    fn checkout(&self, target: &CheckoutTarget) -> Result<(), GitError>;

    /// Lands in M3.
    fn fetch(&self, remote: &str, progress: &dyn ProgressSink) -> Result<FetchOutcome, GitError>;

    /// Lands in M3.
    fn push(
        &self,
        remote: &str,
        spec: &PushSpec,
        progress: &dyn ProgressSink,
    ) -> Result<(), GitError>;

    /// Lands in M3.
    fn stash(&self, op: &StashOp) -> Result<StashOutcome, GitError>;

    /// Lands in M5.
    fn merge(&self, from: &str, opts: &MergeOpts) -> Result<MergeOutcome, GitError>;

    /// Lands in M5.
    fn rebase(&self, onto: &str, plan: &RebasePlan) -> Result<SequenceOutcome, GitError>;

    /// Lands in M5.
    fn cherry_pick(&self, ids: &[ObjectId]) -> Result<SequenceOutcome, GitError>;
}

/// Builds the error returned by a method whose milestone has not landed.
pub(crate) fn not_implemented(operation: &'static str, milestone: &'static str) -> GitError {
    GitError::NotImplementedYet {
        operation,
        milestone,
    }
}
