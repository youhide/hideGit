//! A scripted [`GitBackend`] for testing the UI.
//!
//! It exists so `hidegit-ui` can be exercised against repository states that
//! are painful to produce for real — a detached `HEAD` mid-rebase, a
//! thousand-commit history, a `log` that fails — without a window, a
//! subprocess or a temporary directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::GitBackend;
use crate::error::GitError;
use crate::model::{
    Blob, Commit, CommitDetail, Diff, DiffStats, DiffTarget, Divergence, FileChange, Head, LogPage,
    ObjectId, RefKind, RefName, ReflogEntry, Refs, Remote, RepoState, RevSpec, StashEntry,
    Submodule, Worktree, WorktreeStatus,
};
use crate::ops::{
    Blame, CancelToken, CheckoutTarget, CommitOpts, FetchOpts, FetchOutcome, MergeOpts,
    MergeOutcome, Patch, ProgressSink, ProgressUpdate, PullOpts, PullOutcome, PushOutcome,
    PushSpec, RebasePlan, ResetMode, SearchField, SearchHit, SearchQuery, SearchResults,
    SequenceControl, SequenceOutcome, StartPoint, StashOp, StashOutcome, SubmoduleUpdate, TagSpec,
};

/// An error the fake should return instead of data.
///
/// Spelled as a description rather than a [`GitError`] because `GitError` is
/// deliberately not `Clone` — it carries an `io::Error` — and a fake has to be
/// able to produce the same failure on every call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    NotARepository,
    GitNotFound,
    RefNotFound(String),
    /// A `git` invocation that exited non-zero, for exercising the toast that
    /// shows Git's own stderr.
    Command {
        stderr: String,
    },
    IndexLocked,
}

impl Failure {
    fn to_error(&self) -> GitError {
        match self {
            Failure::NotARepository => GitError::NotARepository(PathBuf::from("/fake")),
            Failure::GitNotFound => GitError::GitNotFound,
            Failure::RefNotFound(name) => GitError::RefNotFound(name.clone()),
            Failure::Command { stderr } => GitError::Command {
                argv: vec!["fake".to_owned()],
                status: Some(1),
                stderr: stderr.clone(),
            },
            Failure::IndexLocked => GitError::IndexLocked(PathBuf::from("/fake/.git/index.lock")),
        }
    }
}

/// A write the fake was asked to perform, recorded rather than performed.
///
/// This is what lets a UI test assert the *intent* a click produced — that
/// deleting an unmerged branch passed `force: true`, that a force push asked for
/// `WithLease` — without a repository on disk to inspect afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteCall {
    Stage(Vec<PathBuf>),
    StagePatch(Patch),
    Unstage(Vec<PathBuf>),
    Discard(Vec<PathBuf>),
    Commit {
        message: String,
        opts: CommitOpts,
    },
    Checkout(CheckoutTarget),
    CreateBranch {
        name: String,
        from: StartPoint,
    },
    RenameBranch {
        from: String,
        to: String,
    },
    DeleteBranch {
        name: String,
        force: bool,
    },
    CreateTag(TagSpec),
    DeleteTag(String),
    AddRemote {
        name: String,
        url: String,
    },
    SetRemoteUrl {
        name: String,
        url: String,
    },
    RemoveRemote(String),
    Fetch {
        remote: String,
        opts: FetchOpts,
    },
    Pull(PullOpts),
    Push {
        remote: String,
        spec: PushSpec,
    },
    Stash(StashOp),
    UpdateSubmodules {
        paths: Vec<PathBuf>,
        opts: SubmoduleUpdate,
    },
    Merge {
        from: String,
        opts: MergeOpts,
    },
    Rebase {
        onto: String,
        plan: RebasePlan,
    },
    CherryPick(Vec<ObjectId>),
    Revert(Vec<ObjectId>),
    Reset {
        target: StartPoint,
        mode: ResetMode,
    },
    ControlSequence(SequenceControl),
}

/// A backend whose answers are set up in advance.
#[derive(Debug)]
pub struct FakeBackend {
    workdir: PathBuf,
    head: Head,
    refs: Refs,
    commits: Vec<Commit>,
    state: RepoState,
    status: WorktreeStatus,
    remotes: Vec<Remote>,
    stashes: Vec<StashEntry>,
    submodules: Vec<Submodule>,
    worktrees: Vec<Worktree>,
    divergence: HashMap<String, Divergence>,
    failure: Option<Failure>,
    /// Applied to writes only, so a test can have reads succeed and the write
    /// they lead to fail — which is the interesting half of the error paths.
    write_failure: Option<Failure>,
    /// Progress the network operations replay through the sink they are given.
    progress: Vec<ProgressUpdate>,
    /// Reflog entries, newest first, as [`GitBackend::reflog`] returns them.
    reflog: Vec<ReflogEntry>,
    /// What [`GitBackend::blame`] answers.
    blame: Blame,
    /// What a merge should answer, so the conflict path can be driven without
    /// building two divergent histories on disk.
    merge_outcome: MergeOutcome,
    /// What a cherry-pick, revert or continue should answer.
    sequence_outcome: SequenceOutcome,
    /// Every write asked for, in order.
    writes: Mutex<Vec<WriteCall>>,
    /// How many times [`GitBackend::invalidate`] has been called, so a test can
    /// assert that a mutation actually triggered a refresh.
    invalidations: AtomicUsize,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self {
            workdir: PathBuf::from("/fake"),
            head: Head::Unborn {
                name: RefName {
                    kind: RefKind::LocalBranch,
                    full: "refs/heads/main".to_owned(),
                    short: "main".to_owned(),
                },
            },
            refs: Refs::default(),
            commits: Vec::new(),
            state: RepoState::Clean,
            status: WorktreeStatus::default(),
            remotes: Vec::new(),
            stashes: Vec::new(),
            submodules: Vec::new(),
            worktrees: Vec::new(),
            divergence: HashMap::new(),
            failure: None,
            write_failure: None,
            progress: Vec::new(),
            reflog: Vec::new(),
            blame: Blame::default(),
            // A clean merge is the boring default; the interesting cases are
            // set up per test, because a fixture that conflicts by default
            // would make every unrelated test assert its way past a conflict.
            merge_outcome: MergeOutcome::UpToDate,
            sequence_outcome: SequenceOutcome::Completed,
            writes: Mutex::new(Vec::new()),
            invalidations: AtomicUsize::new(0),
        }
    }
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// History, newest first, exactly as [`GitBackend::log`] should return it.
    pub fn with_commits(mut self, commits: Vec<Commit>) -> Self {
        if let Some(first) = commits.first() {
            self.head = Head::Branch {
                name: RefName {
                    kind: RefKind::LocalBranch,
                    full: "refs/heads/main".to_owned(),
                    short: "main".to_owned(),
                },
                target: first.id,
            };
        }
        self.commits = commits;
        self
    }

    pub fn with_refs(mut self, refs: Refs) -> Self {
        self.refs = refs;
        self
    }

    pub fn with_head(mut self, head: Head) -> Self {
        self.head = head;
        self
    }

    pub fn with_state(mut self, state: RepoState) -> Self {
        self.state = state;
        self
    }

    /// Scripts the working directory, so a staging view can be driven without a
    /// repository on disk.
    ///
    /// `state` on the status is overwritten by [`Self::with_state`], which
    /// remains the single place a fixture declares what the repository is in
    /// the middle of.
    pub fn with_status(mut self, status: WorktreeStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_remotes(mut self, remotes: Vec<Remote>) -> Self {
        self.remotes = remotes;
        self
    }

    pub fn with_stashes(mut self, stashes: Vec<StashEntry>) -> Self {
        self.stashes = stashes;
        self
    }

    pub fn with_submodules(mut self, submodules: Vec<Submodule>) -> Self {
        self.submodules = submodules;
        self
    }

    pub fn with_worktrees(mut self, worktrees: Vec<Worktree>) -> Self {
        self.worktrees = worktrees;
        self
    }

    /// Ahead/behind keyed by full local ref name, as
    /// [`GitBackend::divergence`] returns it.
    pub fn with_divergence(mut self, divergence: HashMap<String, Divergence>) -> Self {
        self.divergence = divergence;
        self
    }

    /// Progress the network operations replay, so the progress banner and its
    /// Cancel button can be driven without a remote.
    pub fn with_progress(mut self, progress: Vec<ProgressUpdate>) -> Self {
        self.progress = progress;
        self
    }

    pub fn with_blame(mut self, blame: Blame) -> Self {
        self.blame = blame;
        self
    }

    /// Reflog entries, newest first, as [`GitBackend::reflog`] returns them.
    pub fn with_reflog(mut self, reflog: Vec<ReflogEntry>) -> Self {
        self.reflog = reflog;
        self
    }

    /// What a merge answers — a conflict, most usefully, so the resolver can be
    /// driven without two divergent histories on disk.
    pub fn with_merge_outcome(mut self, outcome: MergeOutcome) -> Self {
        self.merge_outcome = outcome;
        self
    }

    /// What a cherry-pick, revert or `--continue` answers.
    ///
    /// Pair it with [`Self::with_state`]: a fake that reports a sequence
    /// stopped on a conflict but a `Clean` repository is a state Git cannot
    /// produce, and a test built on one proves nothing.
    pub fn with_sequence_outcome(mut self, outcome: SequenceOutcome) -> Self {
        self.sequence_outcome = outcome;
        self
    }

    /// Makes every read fail, for exercising the error paths.
    pub fn failing(mut self, failure: Failure) -> Self {
        self.failure = Some(failure);
        self
    }

    /// Makes every *write* fail while reads keep working.
    pub fn failing_writes(mut self, failure: Failure) -> Self {
        self.write_failure = Some(failure);
        self
    }

    pub fn invalidations(&self) -> usize {
        self.invalidations.load(Ordering::Relaxed)
    }

    /// Every write the backend was asked for, in order.
    pub fn writes(&self) -> Vec<WriteCall> {
        self.recorded().clone()
    }

    /// The most recent write, which is what a single-action test wants.
    pub fn last_write(&self) -> Option<WriteCall> {
        self.recorded().last().cloned()
    }

    fn recorded(&self) -> std::sync::MutexGuard<'_, Vec<WriteCall>> {
        self.writes
            .lock()
            .expect("the write log mutex is never poisoned across a panic-free path")
    }

    fn check(&self) -> Result<(), GitError> {
        match &self.failure {
            Some(failure) => Err(failure.to_error()),
            None => Ok(()),
        }
    }

    /// Records a write and reports whether it should succeed.
    ///
    /// The call is recorded even when it is scripted to fail: a test asserting
    /// that a failure leaves the repository untouched still needs to know the
    /// right command was attempted.
    fn record(&self, call: WriteCall) -> Result<(), GitError> {
        self.recorded().push(call);
        match &self.write_failure {
            Some(failure) => Err(failure.to_error()),
            None => {
                self.invalidate();
                Ok(())
            }
        }
    }

    /// Replays the scripted progress, stopping if cancellation is asked for.
    fn replay_progress(
        &self,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        for update in &self.progress {
            if cancel.is_cancelled() {
                return Err(GitError::Cancelled { stale_lock: None });
            }
            progress.report(update.clone());
        }
        // Checked again so a token cancelled before the call, with no scripted
        // progress to step through, still reports as cancelled.
        if cancel.is_cancelled() {
            return Err(GitError::Cancelled { stale_lock: None });
        }
        Ok(())
    }
}

impl GitBackend for FakeBackend {
    fn open(_path: &Path) -> Result<Self, GitError> {
        Ok(Self::default())
    }

    fn workdir(&self) -> &Path {
        &self.workdir
    }

    fn git_dir(&self) -> &Path {
        &self.workdir
    }

    fn head(&self) -> Result<Head, GitError> {
        self.check()?;
        Ok(self.head.clone())
    }

    fn refs(&self) -> Result<Refs, GitError> {
        self.check()?;
        Ok(self.refs.clone())
    }

    fn repo_state(&self) -> Result<RepoState, GitError> {
        self.check()?;
        Ok(self.state)
    }

    fn log(&self, _spec: &RevSpec, page: LogPage) -> Result<Vec<Commit>, GitError> {
        self.check()?;
        let start = page.skip.min(self.commits.len());
        let end = start.saturating_add(page.limit).min(self.commits.len());
        Ok(self.commits[start..end].to_vec())
    }

    fn commit_count(&self, _spec: &RevSpec) -> Result<usize, GitError> {
        self.check()?;
        Ok(self.commits.len())
    }

    fn commit(&self, id: ObjectId) -> Result<CommitDetail, GitError> {
        self.check()?;
        let commit = self
            .commits
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| GitError::RefNotFound(id.to_hex()))?;

        Ok(CommitDetail {
            commit,
            changes: Vec::<FileChange>::new(),
            stats: DiffStats::default(),
        })
    }

    fn diff(&self, _target: &DiffTarget) -> Result<Diff, GitError> {
        self.check()?;
        Ok(Diff::default())
    }

    fn read_blob(&self, id: ObjectId) -> Result<Blob, GitError> {
        self.check()?;
        Ok(Blob {
            id,
            bytes: Vec::new(),
        })
    }

    fn status(&self) -> Result<WorktreeStatus, GitError> {
        self.check()?;
        Ok(WorktreeStatus {
            state: self.state,
            ..self.status.clone()
        })
    }

    fn remotes(&self) -> Result<Vec<Remote>, GitError> {
        self.check()?;
        Ok(self.remotes.clone())
    }

    fn stashes(&self) -> Result<Vec<StashEntry>, GitError> {
        self.check()?;
        Ok(self.stashes.clone())
    }

    fn submodules(&self) -> Result<Vec<Submodule>, GitError> {
        self.check()?;
        Ok(self.submodules.clone())
    }

    fn worktrees(&self) -> Result<Vec<Worktree>, GitError> {
        self.check()?;
        Ok(self.worktrees.clone())
    }

    fn divergence(&self) -> Result<HashMap<String, Divergence>, GitError> {
        self.check()?;
        Ok(self.divergence.clone())
    }

    fn blame(&self, _path: &Path, _at: ObjectId) -> Result<Blame, GitError> {
        self.check()?;
        Ok(self.blame.clone())
    }

    fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::Relaxed);
    }

    // ---- write: recorded, not performed -------------------------------
    //
    // Every write succeeds and is logged, unless `failing_writes` scripted a
    // failure. That is what lets a UI test assert the *intent* behind a click,
    // which is all the UI is responsible for — whether the argument vector is
    // right is `HybridBackend`'s job and is tested against a real repository.

    fn stage(&self, paths: &[&Path]) -> Result<(), GitError> {
        self.record(WriteCall::Stage(owned(paths)))
    }

    fn stage_patch(&self, patch: &Patch) -> Result<(), GitError> {
        self.record(WriteCall::StagePatch(patch.clone()))
    }

    fn unstage(&self, paths: &[&Path]) -> Result<(), GitError> {
        self.record(WriteCall::Unstage(owned(paths)))
    }

    fn discard(&self, paths: &[&Path]) -> Result<(), GitError> {
        self.record(WriteCall::Discard(owned(paths)))
    }

    fn create_commit(&self, message: &str, opts: CommitOpts) -> Result<ObjectId, GitError> {
        self.record(WriteCall::Commit {
            message: message.to_owned(),
            opts,
        })?;
        // A plausible new id. The real backend reads it back with `rev-parse`
        // because hooks and signing can change what was recorded; a fake has
        // nothing to read back from.
        Ok(ObjectId::from_hex(&"a".repeat(40)).expect("a valid sha-1"))
    }

    fn checkout(&self, target: &CheckoutTarget) -> Result<(), GitError> {
        self.record(WriteCall::Checkout(target.clone()))
    }

    fn create_branch(&self, name: &str, from: &StartPoint) -> Result<(), GitError> {
        self.record(WriteCall::CreateBranch {
            name: name.to_owned(),
            from: from.clone(),
        })
    }

    fn rename_branch(&self, from: &str, to: &str) -> Result<(), GitError> {
        self.record(WriteCall::RenameBranch {
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }

    fn delete_branch(&self, name: &str, force: bool) -> Result<(), GitError> {
        self.record(WriteCall::DeleteBranch {
            name: name.to_owned(),
            force,
        })
    }

    fn create_tag(&self, spec: &TagSpec) -> Result<(), GitError> {
        self.record(WriteCall::CreateTag(spec.clone()))
    }

    fn delete_tag(&self, name: &str) -> Result<(), GitError> {
        self.record(WriteCall::DeleteTag(name.to_owned()))
    }

    fn add_remote(&self, name: &str, url: &str) -> Result<(), GitError> {
        self.record(WriteCall::AddRemote {
            name: name.to_owned(),
            url: url.to_owned(),
        })
    }

    fn set_remote_url(&self, name: &str, url: &str) -> Result<(), GitError> {
        self.record(WriteCall::SetRemoteUrl {
            name: name.to_owned(),
            url: url.to_owned(),
        })
    }

    fn remove_remote(&self, name: &str) -> Result<(), GitError> {
        self.record(WriteCall::RemoveRemote(name.to_owned()))
    }

    fn fetch(
        &self,
        remote: &str,
        opts: &FetchOpts,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<FetchOutcome, GitError> {
        self.record(WriteCall::Fetch {
            remote: remote.to_owned(),
            opts: *opts,
        })?;
        self.replay_progress(progress, cancel)?;
        Ok(FetchOutcome::default())
    }

    fn update_submodules(
        &self,
        paths: &[&Path],
        opts: SubmoduleUpdate,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<Vec<Submodule>, GitError> {
        self.record(WriteCall::UpdateSubmodules {
            paths: paths.iter().map(|p| p.to_path_buf()).collect(),
            opts,
        })?;
        self.replay_progress(progress, cancel)?;
        // Unchanged, deliberately: the fake does not simulate a checkout, and a
        // caller that assumed an update always moves something should see that
        // assumption fail here rather than in a real repository.
        Ok(self.submodules.clone())
    }

    fn pull(
        &self,
        opts: &PullOpts,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<PullOutcome, GitError> {
        self.record(WriteCall::Pull(opts.clone()))?;
        self.replay_progress(progress, cancel)?;
        Ok(PullOutcome::UpToDate)
    }

    fn push(
        &self,
        remote: &str,
        spec: &PushSpec,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<PushOutcome, GitError> {
        self.record(WriteCall::Push {
            remote: remote.to_owned(),
            spec: spec.clone(),
        })?;
        self.replay_progress(progress, cancel)?;
        Ok(PushOutcome::default())
    }

    fn stash(&self, op: &StashOp) -> Result<StashOutcome, GitError> {
        self.record(WriteCall::Stash(op.clone()))?;
        Ok(match op {
            StashOp::Push { .. } => {
                StashOutcome::Created(ObjectId::from_hex(&"b".repeat(40)).expect("a valid sha-1"))
            }
            StashOp::Apply(_) | StashOp::Pop(_) => StashOutcome::Applied,
            StashOp::Drop(_) => StashOutcome::Dropped,
        })
    }

    fn merge(&self, from: &str, opts: &MergeOpts) -> Result<MergeOutcome, GitError> {
        self.record(WriteCall::Merge {
            from: from.to_owned(),
            opts: opts.clone(),
        })?;
        Ok(self.merge_outcome.clone())
    }

    fn rebase(&self, onto: &str, plan: &RebasePlan) -> Result<SequenceOutcome, GitError> {
        self.record(WriteCall::Rebase {
            onto: onto.to_owned(),
            plan: plan.clone(),
        })?;
        Ok(self.sequence_outcome.clone())
    }

    fn search(&self, query: &SearchQuery) -> Result<SearchResults, GitError> {
        self.check()?;
        // The scripted history, filtered the same way the real one is, so a UI
        // test exercises the same shapes without a repository on disk.
        let needle = query.text.trim().to_lowercase();
        let hits = self
            .commits
            .iter()
            .filter(|c| c.summary.to_lowercase().contains(&needle))
            .take(query.limit)
            .map(|commit| SearchHit {
                commit: commit.clone(),
                field: SearchField::Summary,
            })
            .collect::<Vec<_>>();
        let truncated = hits.len() == query.limit;
        Ok(SearchResults { hits, truncated })
    }

    fn rebase_preview(&self, _onto: &str) -> Result<Vec<Commit>, GitError> {
        self.check()?;
        // The scripted history, oldest first, which is enough to drive a plan
        // editor without a repository on disk.
        Ok(self.commits.iter().rev().cloned().collect())
    }

    fn cherry_pick(&self, ids: &[ObjectId]) -> Result<SequenceOutcome, GitError> {
        self.record(WriteCall::CherryPick(ids.to_vec()))?;
        Ok(self.sequence_outcome.clone())
    }

    fn revert(&self, ids: &[ObjectId]) -> Result<SequenceOutcome, GitError> {
        self.record(WriteCall::Revert(ids.to_vec()))?;
        Ok(self.sequence_outcome.clone())
    }

    fn reset(&self, target: &StartPoint, mode: ResetMode) -> Result<(), GitError> {
        self.record(WriteCall::Reset {
            target: target.clone(),
            mode,
        })
    }

    fn control_sequence(&self, control: SequenceControl) -> Result<SequenceOutcome, GitError> {
        // The real backend refuses this before it records anything, and a test
        // asserting that the UI keeps the button disabled needs the same
        // refusal rather than a recorded call that never happened.
        if !self.state.is_in_progress() {
            return Err(GitError::NothingInProgress(self.state));
        }
        self.record(WriteCall::ControlSequence(control))?;
        match control {
            SequenceControl::Abort => Ok(SequenceOutcome::Completed),
            _ => Ok(self.sequence_outcome.clone()),
        }
    }

    fn reflog(&self, _ref_name: &str, limit: usize) -> Result<Vec<ReflogEntry>, GitError> {
        self.check()?;
        Ok(self.reflog.iter().take(limit).cloned().collect())
    }
}

/// Owns the borrowed paths a write was given, so they can be recorded.
fn owned(paths: &[&Path]) -> Vec<PathBuf> {
    paths.iter().map(|p| p.to_path_buf()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{ForceMode, NoProgress};

    #[test]
    fn a_write_is_recorded_with_the_intent_behind_it() {
        let backend = FakeBackend::new();

        backend
            .delete_branch("feat/graph", true)
            .expect("a scripted write succeeds");

        assert_eq!(
            backend.last_write(),
            Some(WriteCall::DeleteBranch {
                name: "feat/graph".to_owned(),
                force: true,
            }),
            "the force flag is the whole point of the assertion"
        );
        assert_eq!(
            backend.invalidations(),
            1,
            "a write must invalidate, or the next read is stale"
        );
    }

    #[test]
    fn a_scripted_failure_still_records_what_was_attempted() {
        // A test asserting that a failed push leaves things alone still has to
        // know the right push was tried.
        let backend = FakeBackend::new().failing_writes(Failure::Command {
            stderr: "! [rejected] main -> main (non-fast-forward)\n".to_owned(),
        });

        let spec = PushSpec {
            refspec: "refs/heads/main".to_owned(),
            force: ForceMode::WithLease,
            set_upstream: false,
        };
        let error = backend
            .push("origin", &spec, &NoProgress, &CancelToken::new())
            .expect_err("scripted to fail");

        match error {
            GitError::Command { stderr, .. } => assert!(stderr.contains("non-fast-forward")),
            other => panic!("expected a Command error, got {other:?}"),
        }
        assert_eq!(
            backend.last_write(),
            Some(WriteCall::Push {
                remote: "origin".to_owned(),
                spec,
            })
        );
        assert_eq!(
            backend.invalidations(),
            0,
            "a write that failed changed nothing, so nothing is stale"
        );
    }

    #[test]
    fn scripted_progress_reaches_the_sink_in_order() {
        use std::sync::Mutex as StdMutex;

        struct Collect(StdMutex<Vec<String>>);
        impl ProgressSink for Collect {
            fn report(&self, update: ProgressUpdate) {
                self.0.lock().unwrap().push(update.phase);
            }
        }

        let backend = FakeBackend::new().with_progress(vec![
            ProgressUpdate {
                phase: "Counting objects".to_owned(),
                done: 3,
                total: Some(6),
            },
            ProgressUpdate {
                phase: "Receiving objects".to_owned(),
                done: 6,
                total: Some(6),
            },
        ]);

        let sink = Collect(StdMutex::new(Vec::new()));
        backend
            .fetch("origin", &FetchOpts::default(), &sink, &CancelToken::new())
            .expect("a scripted fetch succeeds");

        assert_eq!(
            sink.0.into_inner().unwrap(),
            vec!["Counting objects", "Receiving objects"]
        );
    }

    #[test]
    fn a_cancelled_token_stops_a_scripted_network_operation() {
        let backend = FakeBackend::new();
        let cancel = CancelToken::new();
        cancel.cancel();

        let error = backend
            .fetch("origin", &FetchOpts::default(), &NoProgress, &cancel)
            .expect_err("a cancelled fetch does not report success");

        assert!(matches!(error, GitError::Cancelled { .. }), "got {error:?}");
    }

    #[test]
    fn reads_can_fail_while_writes_are_left_alone_and_the_reverse() {
        let read_fails = FakeBackend::new().failing(Failure::GitNotFound);
        assert!(read_fails.remotes().is_err());
        assert!(
            read_fails
                .checkout(&CheckoutTarget::Branch("main".to_owned()))
                .is_ok(),
            "`failing` scripts reads; writes need `failing_writes`"
        );

        let write_fails = FakeBackend::new().failing_writes(Failure::IndexLocked);
        assert!(write_fails.remotes().is_ok());
        assert!(matches!(
            write_fails.checkout(&CheckoutTarget::Branch("main".to_owned())),
            Err(GitError::IndexLocked(_))
        ));
    }
}
