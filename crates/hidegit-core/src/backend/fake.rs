//! A scripted [`GitBackend`] for testing the UI.
//!
//! It exists so `hidegit-ui` can be exercised against repository states that
//! are painful to produce for real — a detached `HEAD` mid-rebase, a
//! thousand-commit history, a `log` that fails — without a window, a
//! subprocess or a temporary directory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{GitBackend, not_implemented};
use crate::error::GitError;
use crate::model::{
    Blob, Commit, CommitDetail, Diff, DiffStats, DiffTarget, FileChange, Head, LogPage, ObjectId,
    RefKind, RefName, Refs, RepoState, RevSpec, WorktreeStatus,
};
use crate::ops::{
    Blame, CheckoutTarget, CommitOpts, FetchOutcome, MergeOpts, MergeOutcome, Patch, ProgressSink,
    PushSpec, RebasePlan, SequenceOutcome, StashOp, StashOutcome,
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
}

impl Failure {
    fn to_error(&self) -> GitError {
        match self {
            Failure::NotARepository => GitError::NotARepository(PathBuf::from("/fake")),
            Failure::GitNotFound => GitError::GitNotFound,
            Failure::RefNotFound(name) => GitError::RefNotFound(name.clone()),
        }
    }
}

/// A backend whose answers are set up in advance.
#[derive(Debug)]
pub struct FakeBackend {
    workdir: PathBuf,
    head: Head,
    refs: Refs,
    commits: Vec<Commit>,
    state: RepoState,
    failure: Option<Failure>,
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
            failure: None,
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

    /// Makes every read fail, for exercising the error paths.
    pub fn failing(mut self, failure: Failure) -> Self {
        self.failure = Some(failure);
        self
    }

    pub fn invalidations(&self) -> usize {
        self.invalidations.load(Ordering::Relaxed)
    }

    fn check(&self) -> Result<(), GitError> {
        match &self.failure {
            Some(failure) => Err(failure.to_error()),
            None => Ok(()),
        }
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
            ..WorktreeStatus::default()
        })
    }

    fn blame(&self, _path: &Path, _at: ObjectId) -> Result<Blame, GitError> {
        Err(not_implemented("blame", "M6"))
    }

    fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::Relaxed);
    }

    fn stage(&self, _paths: &[&Path]) -> Result<(), GitError> {
        Err(not_implemented("stage", "M2"))
    }

    fn stage_patch(&self, _patch: &Patch) -> Result<(), GitError> {
        Err(not_implemented("hunk staging", "M2"))
    }

    fn unstage(&self, _paths: &[&Path]) -> Result<(), GitError> {
        Err(not_implemented("unstage", "M2"))
    }

    fn discard(&self, _paths: &[&Path]) -> Result<(), GitError> {
        Err(not_implemented("discard", "M2"))
    }

    fn create_commit(&self, _message: &str, _opts: CommitOpts) -> Result<ObjectId, GitError> {
        Err(not_implemented("commit", "M2"))
    }

    fn checkout(&self, _target: &CheckoutTarget) -> Result<(), GitError> {
        Err(not_implemented("checkout", "M3"))
    }

    fn fetch(&self, _remote: &str, _progress: &dyn ProgressSink) -> Result<FetchOutcome, GitError> {
        Err(not_implemented("fetch", "M3"))
    }

    fn push(
        &self,
        _remote: &str,
        _spec: &PushSpec,
        _progress: &dyn ProgressSink,
    ) -> Result<(), GitError> {
        Err(not_implemented("push", "M3"))
    }

    fn stash(&self, _op: &StashOp) -> Result<StashOutcome, GitError> {
        Err(not_implemented("stash", "M3"))
    }

    fn merge(&self, _from: &str, _opts: &MergeOpts) -> Result<MergeOutcome, GitError> {
        Err(not_implemented("merge", "M5"))
    }

    fn rebase(&self, _onto: &str, _plan: &RebasePlan) -> Result<SequenceOutcome, GitError> {
        Err(not_implemented("rebase", "M5"))
    }

    fn cherry_pick(&self, _ids: &[ObjectId]) -> Result<SequenceOutcome, GitError> {
        Err(not_implemented("cherry-pick", "M5"))
    }
}
