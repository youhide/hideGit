//! The single production [`GitBackend`], routing each method to `gix` or to
//! the system `git` binary.
//!
//! Which side a method sits on is a decision, not an accident, and this file is
//! where that decision is visible. Reads run on gitoxide because they are fast
//! and need no subprocess; writes shell out because gitoxide does not implement
//! `push` and because delegating gives hideGit credential helpers, hooks,
//! submodules and LFS for free. See `docs/adr/0002-git-backend-hybrid.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{GitBackend, gix_read, not_implemented};
use crate::error::GitError;
use crate::model::{
    Blob, Commit, CommitDetail, Diff, DiffTarget, Head, LogPage, ObjectId, Refs, RepoState,
    RevSpec, WorktreeStatus,
};
use crate::ops::{
    Blame, CheckoutTarget, CommitOpts, FetchOutcome, MergeOpts, MergeOutcome, Patch, ProgressSink,
    PushSpec, RebasePlan, SequenceOutcome, StashOp, StashOutcome,
};

/// A repository, opened once and shared.
///
/// Cheap to clone into a task: the underlying handle is reference-counted, so
/// the UI never holds a lock across an await.
#[derive(Debug)]
pub struct HybridBackend {
    repo: gix::ThreadSafeRepository,
    workdir: PathBuf,
    git_dir: PathBuf,
    /// Memoised traversals, keyed by what was walked.
    ///
    /// Computing topological order is the expensive part of drawing a graph and
    /// it does not change until the repository does, so it is computed once and
    /// only the visible page is hydrated into full commits. Cleared by
    /// [`GitBackend::invalidate`].
    walks: Mutex<HashMap<RevSpec, Arc<Vec<gix_read::WalkEntry>>>>,
}

impl HybridBackend {
    /// Borrows a thread-local view of the repository.
    ///
    /// `gix::Repository` carries per-thread caches and is deliberately not
    /// `Sync`; the thread-safe handle hands out a local one per call.
    fn repo(&self) -> gix::Repository {
        self.repo.to_thread_local()
    }

    /// Returns the memoised walk for `spec`, computing it if needed.
    fn walk(&self, spec: &RevSpec) -> Result<Arc<Vec<gix_read::WalkEntry>>, GitError> {
        if let Some(cached) = self
            .walks
            .lock()
            .expect("the walk cache mutex is never poisoned across a panic-free path")
            .get(spec)
        {
            return Ok(Arc::clone(cached));
        }

        let entries = Arc::new(gix_read::walk(&self.repo(), spec)?);

        self.walks
            .lock()
            .expect("the walk cache mutex is never poisoned across a panic-free path")
            .insert(spec.clone(), Arc::clone(&entries));

        Ok(entries)
    }
}

impl GitBackend for HybridBackend {
    fn open(path: &Path) -> Result<Self, GitError> {
        let repo = gix_read::open(path)?;
        let local = repo.to_thread_local();

        let git_dir = local.path().to_path_buf();
        let workdir = local
            .workdir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| git_dir.clone());

        Ok(Self {
            repo,
            workdir,
            git_dir,
            walks: Mutex::new(HashMap::new()),
        })
    }

    // ---- read: gix ---------------------------------------------------

    fn workdir(&self) -> &Path {
        &self.workdir
    }

    fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn head(&self) -> Result<Head, GitError> {
        gix_read::head(&self.repo())
    }

    fn refs(&self) -> Result<Refs, GitError> {
        gix_read::refs(&self.repo())
    }

    fn repo_state(&self) -> Result<RepoState, GitError> {
        Ok(gix_read::repo_state(&self.repo()))
    }

    fn log(&self, spec: &RevSpec, page: LogPage) -> Result<Vec<Commit>, GitError> {
        let entries = self.walk(spec)?;
        let start = page.skip.min(entries.len());
        let end = start.saturating_add(page.limit).min(entries.len());

        let repo = self.repo();
        let refs = gix_read::refs(&repo)?;
        gix_read::hydrate(&repo, &entries[start..end], &refs)
    }

    fn commit_count(&self, spec: &RevSpec) -> Result<usize, GitError> {
        Ok(self.walk(spec)?.len())
    }

    fn commit(&self, id: ObjectId) -> Result<CommitDetail, GitError> {
        gix_read::commit_detail(&self.repo(), id)
    }

    fn diff(&self, target: &DiffTarget) -> Result<Diff, GitError> {
        gix_read::diff(&self.repo(), target)
    }

    fn read_blob(&self, id: ObjectId) -> Result<Blob, GitError> {
        gix_read::read_blob(&self.repo(), id)
    }

    fn status(&self) -> Result<WorktreeStatus, GitError> {
        // Reporting an empty status here would be a lie, and the UI is built
        // on never lying about repository state. M2 implements it properly,
        // with rename detection and `.gitignore` handling.
        Err(not_implemented("status", "M2"))
    }

    fn blame(&self, _path: &Path, _at: ObjectId) -> Result<Blame, GitError> {
        Err(not_implemented("blame", "M6"))
    }

    fn invalidate(&self) {
        self.walks
            .lock()
            .expect("the walk cache mutex is never poisoned across a panic-free path")
            .clear();
    }

    // ---- write: git CLI ----------------------------------------------
    //
    // M1 is read-only on purpose. Each of these lands in the milestone named
    // in its error, and each will be implemented through `crate::process`,
    // never by building a command as a shell string.

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
