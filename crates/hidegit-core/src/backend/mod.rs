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
pub use fake::{Failure, FakeBackend, WriteCall};

use std::collections::HashMap;
use std::path::Path;

use crate::error::GitError;
use crate::model::{
    Blob, Commit, CommitDetail, Diff, DiffTarget, Divergence, Head, LogPage, ObjectId, ReflogEntry,
    Refs, Remote, RepoState, RevSpec, StashEntry, Submodule, WorktreeStatus,
};
use crate::ops::{
    Blame, CancelToken, CheckoutTarget, CommitOpts, FetchOpts, FetchOutcome, MergeOpts,
    MergeOutcome, Patch, ProgressSink, PullOpts, PullOutcome, PushOutcome, PushSpec, RebasePlan,
    ResetMode, SearchQuery, SearchResults, SequenceControl, SequenceOutcome, StartPoint, StashOp,
    StashOutcome, SubmoduleUpdate, TagSpec,
};

/// Everything hideGit can ask of a repository.
///
/// The read half runs on `gix`; the write half shells out to `git`.
///
/// The whole surface was declared from M1 and filled in milestone by milestone,
/// with the unlanded methods returning [`GitError::NotImplementedYet`] so the
/// shape stayed visible in one file. As of M6 **every method is implemented**,
/// and that variant is no longer produced here — it stays in the error type for
/// whatever a later backend cannot do.
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

    /// Working directory state.
    fn status(&self) -> Result<WorktreeStatus, GitError>;

    /// The remotes `git remote` would list, with their URLs.
    ///
    /// Separate from [`Refs::remotes`], which holds remote-*tracking* branches: a
    /// remote that has been added but never fetched has no tracking refs, and
    /// leaving it out would be saying it does not exist.
    fn remotes(&self) -> Result<Vec<Remote>, GitError>;

    /// The stash, newest entry first.
    ///
    /// An empty vector for a repository that has never stashed — `refs/stash`
    /// simply does not exist, which is not an error.
    fn stashes(&self) -> Result<Vec<StashEntry>, GitError>;

    /// The submodules `.gitmodules` declares, in path order.
    ///
    /// An empty vector for the overwhelming majority of repositories, which
    /// have no `.gitmodules` at all — absence is not an error.
    ///
    /// Costs one repository open per submodule, because "where is the nested
    /// checkout actually at" can only be answered by the nested repository.
    /// That is why this is its own method rather than part of
    /// [`GitBackend::refs`], which the filesystem watcher rereads on every
    /// file save.
    fn submodules(&self) -> Result<Vec<Submodule>, GitError>;

    /// Ahead/behind for every local branch that has an upstream, keyed by the
    /// branch's full ref name.
    ///
    /// One call rather than one per branch, so the UI spends one task and one
    /// message on it. Deliberately **not** folded into [`GitBackend::refs`]: this
    /// costs a commit walk per branch, `refs` is reread on every file save
    /// through the filesystem watcher, and ahead/behind only changes when a ref
    /// moves.
    fn divergence(&self) -> Result<HashMap<String, Divergence>, GitError>;

    /// Line-by-line authorship. Lands in M6.
    fn blame(&self, path: &Path, at: ObjectId) -> Result<Blame, GitError>;

    /// Drops everything cached about the repository, after something changed it.
    ///
    /// Two things are cached, and both have to go. The history walk is memoised
    /// because computing topological order is the expensive part of drawing a
    /// graph. And gitoxide caches `.git/config` from the moment a repository is
    /// opened, so a `git` command that rewrites it — a branch rename, a remote
    /// change, `push --set-upstream` — leaves the read side describing the old
    /// file. That second one fails quietly: an upstream simply disappears.
    fn invalidate(&self);

    // ---- write: git CLI ----------------------------------------------

    fn stage(&self, paths: &[&Path]) -> Result<(), GitError>;

    /// Applies a patch to the index, which is how hunk- and line-level staging is
    /// expressed — and, applied in reverse, how unstaging is.
    fn stage_patch(&self, patch: &Patch) -> Result<(), GitError>;

    fn unstage(&self, paths: &[&Path]) -> Result<(), GitError>;

    fn discard(&self, paths: &[&Path]) -> Result<(), GitError>;

    /// Returns the new commit's id, read back rather than assumed: a hook or a
    /// signature can change what was actually recorded.
    fn create_commit(&self, message: &str, opts: CommitOpts) -> Result<ObjectId, GitError>;

    /// Switches `HEAD`, and the working tree with it.
    ///
    /// Fails rather than discarding anything when local changes would be
    /// overwritten. hideGit does not stash on the user's behalf: that moves their
    /// work somewhere they did not ask for.
    fn checkout(&self, target: &CheckoutTarget) -> Result<(), GitError>;

    /// Creates a branch without switching to it.
    fn create_branch(&self, name: &str, from: &StartPoint) -> Result<(), GitError>;

    fn rename_branch(&self, from: &str, to: &str) -> Result<(), GitError>;

    /// Deletes a branch. `force` is what allows deleting an unmerged one, and it
    /// is never a silent retry after the safe form was refused.
    fn delete_branch(&self, name: &str, force: bool) -> Result<(), GitError>;

    fn create_tag(&self, spec: &TagSpec) -> Result<(), GitError>;

    fn delete_tag(&self, name: &str) -> Result<(), GitError>;

    fn add_remote(&self, name: &str, url: &str) -> Result<(), GitError>;

    fn set_remote_url(&self, name: &str, url: &str) -> Result<(), GitError>;

    fn remove_remote(&self, name: &str) -> Result<(), GitError>;

    /// Updates remote-tracking refs. Reports progress and can be cancelled.
    ///
    /// On the CLI side despite gitoxide implementing fetch, because fetch and
    /// push share credential handling and two authentication paths is not worth
    /// it. See `docs/adr/0002-git-backend-hybrid.md`.
    fn fetch(
        &self,
        remote: &str,
        opts: &FetchOpts,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<FetchOutcome, GitError>;

    /// Fetches and integrates, using the user's own `pull.rebase` and `pull.ff`
    /// configuration rather than a strategy hideGit chose for them.
    fn pull(
        &self,
        opts: &PullOpts,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<PullOutcome, GitError>;

    fn push(
        &self,
        remote: &str,
        spec: &PushSpec,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<PushOutcome, GitError>;

    fn stash(&self, op: &StashOp) -> Result<StashOutcome, GitError>;

    /// Brings submodules to the commit the superproject records, cloning the
    /// ones that were never set up when [`SubmoduleUpdate::init`] is set.
    ///
    /// An empty `paths` means every submodule, which is what a bare `--`
    /// separator means to `git submodule update` itself.
    ///
    /// **Returns the state afterwards rather than `()`, read back.** This
    /// command's failure mode is not an error: `git submodule update` without
    /// `--init` exits 0 having done nothing for a submodule that was never set
    /// up, and a caller with only a `Result<(), _>` would report that as a
    /// success. Answering with the submodules as they now are lets the caller
    /// see that nothing moved.
    ///
    /// Reports progress and can be cancelled, because in the general case this
    /// clones: a fresh clone of a superproject has no submodule checkouts at
    /// all, and bringing them in is a network operation.
    fn update_submodules(
        &self,
        paths: &[&Path],
        opts: SubmoduleUpdate,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<Vec<Submodule>, GitError>;

    /// Merges `from` into the current branch.
    ///
    /// A conflict is a [`MergeOutcome`], not a `GitError`: it is what the user
    /// asked for arriving in the state that needs resolving, and it routes to
    /// the conflict UI rather than to an error dialog.
    fn merge(&self, from: &str, opts: &MergeOpts) -> Result<MergeOutcome, GitError>;

    /// Replays the current branch onto `onto`.
    ///
    /// An empty [`RebasePlan`] is an ordinary rebase; a plan with steps drives
    /// `git rebase --interactive`. See
    /// `docs/adr/0007-rebase-plan-through-the-environment.md`.
    fn rebase(&self, onto: &str, plan: &RebasePlan) -> Result<SequenceOutcome, GitError>;

    /// Walks history looking for `query`, newest first.
    ///
    /// Searches the summary, the body, the author's name and email, and the id
    /// as a prefix. The result says which field matched and whether the walk
    /// stopped at the limit rather than at the end of history — a list that
    /// cannot distinguish "these are the matches" from "these are the first
    /// matches" lies by omission.
    fn search(&self, query: &SearchQuery) -> Result<SearchResults, GitError>;

    /// The commits a rebase onto `onto` would replay, **oldest first**.
    ///
    /// Oldest first because that is the order `git rebase --interactive` writes
    /// its todo list in, and the plan editor is that list. Showing them
    /// newest-first like the graph would invert every reorder the user made.
    fn rebase_preview(&self, onto: &str) -> Result<Vec<Commit>, GitError>;

    /// Applies each of `ids` on top of the current branch, in the order given.
    fn cherry_pick(&self, ids: &[ObjectId]) -> Result<SequenceOutcome, GitError>;

    /// Applies the inverse of each of `ids`, in the order given.
    ///
    /// Separate from [`GitBackend::cherry_pick`] despite the near-identical
    /// shape: the two differ in which commit the user is reasoning about, and
    /// folding them into one method with a direction flag would put that
    /// distinction in a boolean at every call site.
    fn revert(&self, ids: &[ObjectId]) -> Result<SequenceOutcome, GitError>;

    /// Moves `HEAD` to `target`, taking the index and working tree with it as
    /// far as `mode` says.
    fn reset(&self, target: &StartPoint, mode: ResetMode) -> Result<(), GitError>;

    /// Continues, aborts or skips the operation currently in progress.
    ///
    /// Which `git` command this runs depends on [`RepoState`], because Git has
    /// no single verb for it: a merge in progress is continued by
    /// `git merge --continue` and a rebase by `git rebase --continue`. Reading
    /// the state here rather than asking the caller to pass it means the UI
    /// cannot get the pairing wrong.
    fn control_sequence(&self, control: SequenceControl) -> Result<SequenceOutcome, GitError>;

    /// The reflog for `ref_name`, most recent entry first.
    ///
    /// This is what makes the destructive operations in this milestone
    /// recoverable, so it is part of the milestone rather than a later
    /// convenience.
    fn reflog(&self, ref_name: &str, limit: usize) -> Result<Vec<ReflogEntry>, GitError>;
}
