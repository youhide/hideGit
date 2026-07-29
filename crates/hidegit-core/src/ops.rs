//! Inputs and outcomes for the operations that write.
//!
//! **These types are provisional.** The write half of [`crate::GitBackend`]
//! carries its full signature from M1 so the read/write split is auditable in
//! one file, but each operation is designed properly in the milestone that
//! implements it: staging in M2, remotes in M3, history rewriting in M5.
//! Expect the shapes here to be refined then rather than treated as settled.

use std::path::PathBuf;

use crate::model::{Conflict, ObjectId};

/// How a commit differs from a plain one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitOpts {
    /// Replace the current `HEAD` commit rather than adding one.
    pub amend: bool,
    /// Append a `Signed-off-by` trailer.
    pub sign_off: bool,
    /// Allow a commit that changes nothing.
    pub allow_empty: bool,
}

/// A patch to apply to the index, for hunk- and line-level staging.
///
/// Staging part of a file is done by feeding `git apply --cached` a patch
/// rather than by rewriting the index directly: the same code path handles
/// hunks, line selections and reverse application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub file: PathBuf,
    /// The patch text, in unified diff format.
    pub text: String,
    /// Apply in reverse — how unstaging is expressed.
    pub reverse: bool,
}

/// What `checkout` should switch to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutTarget {
    Branch(String),
    /// Results in a detached `HEAD`.
    Commit(ObjectId),
    NewBranch {
        name: String,
        from: ObjectId,
    },
}

/// How hard a push is allowed to push.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ForceMode {
    #[default]
    None,
    /// `--force-with-lease`: refuses if the remote moved since the last fetch.
    /// The default whenever a force is requested at all.
    WithLease,
    /// `--force`. Has to be selected deliberately; never a fallback for a
    /// failed lease.
    Force,
}

/// What to push where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSpec {
    pub refspec: String,
    pub force: ForceMode,
    pub set_upstream: bool,
}

/// What a fetch brought back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchOutcome {
    pub updated: Vec<String>,
    pub pruned: Vec<String>,
}

/// Whether a merge may or must fast-forward.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FastForward {
    #[default]
    Allow,
    /// Fail rather than create a merge commit.
    Only,
    /// Always create a merge commit.
    Never,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeOpts {
    pub fast_forward: FastForward,
    pub message: Option<String>,
}

/// How a merge ended. Conflicts are an outcome, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    UpToDate,
    FastForwarded(ObjectId),
    Merged(ObjectId),
    Conflicted(Vec<Conflict>),
}

/// What to do with one commit during a rebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseStep {
    pub action: RebaseAction,
    pub commit: ObjectId,
}

/// The full plan for an interactive rebase, in application order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebasePlan {
    pub steps: Vec<RebaseStep>,
}

/// How a rebase, cherry-pick or revert ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceOutcome {
    Completed,
    /// Stopped part-way, either on a conflict or on an `edit` step. The
    /// repository stays in this state until it is continued or aborted.
    Stopped {
        at: ObjectId,
        conflicts: Vec<Conflict>,
    },
}

/// What to do to the stash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StashOp {
    Push {
        message: Option<String>,
        include_untracked: bool,
    },
    Apply(usize),
    Pop(usize),
    Drop(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StashOutcome {
    Created(ObjectId),
    Applied,
    Dropped,
    Conflicted(Vec<Conflict>),
}

/// A progress report from a long-running operation.
///
/// Anything that may exceed roughly 300ms reports in a real unit — objects,
/// commits, bytes — rather than an indeterminate spinner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressUpdate {
    pub phase: String,
    pub done: u64,
    /// `None` when the total is not known yet.
    pub total: Option<u64>,
}

/// Where progress reports go.
///
/// A trait object rather than a channel so `hidegit-core` stays free of any
/// particular async runtime; the UI adapts it to a `Subscription`.
pub trait ProgressSink: Send + Sync {
    fn report(&self, update: ProgressUpdate);
}

/// Discards progress. For calls that do not display any.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report(&self, _update: ProgressUpdate) {}
}

/// Blame output. Lands in M6.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blame {
    pub lines: Vec<BlameLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub commit: ObjectId,
    /// 1-based line number in the file as of the blamed revision.
    pub lineno: u32,
    pub text: String,
}
