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
    Blob, Commit, CommitDetail, Diff, DiffTarget, Divergence, Head, LogPage, ObjectId, Refs,
    Remote, RepoState, RevSpec, StashEntry, WorktreeStatus,
};
use crate::ops::{
    Blame, CancelToken, CheckoutTarget, CommitOpts, FetchOpts, FetchOutcome, MergeOpts,
    MergeOutcome, Patch, ProgressSink, PullOpts, PullOutcome, PushOutcome, PushSpec, RebasePlan,
    SequenceOutcome, StartPoint, StashOp, StashOutcome, TagSpec,
};
use crate::process::GitCommand;

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

    /// Refuses to write while another Git process holds the index.
    ///
    /// Checked rather than discovered: `git` would fail with a message about a
    /// lock file, which is true but reads as a crash. `IndexLocked` names the
    /// file so the UI can say which process to look for — and hideGit never
    /// deletes a lock it did not create, because the process holding it may
    /// still be working.
    fn guard_index(&self) -> Result<(), GitError> {
        match crate::process::index_lock(&self.git_dir) {
            Some(lock) => Err(GitError::IndexLocked(lock)),
            None => Ok(()),
        }
    }

    /// Runs a command that changes the repository, then drops stale reads.
    ///
    /// Everything that writes goes through here, so the two things every write
    /// owes the rest of the application happen in one place: the index lock is
    /// taken honestly, and the memoised walk is invalidated so the next read
    /// sees what just happened.
    fn write(&self, command: GitCommand) -> Result<(), GitError> {
        command.cwd(&self.workdir).takes_locks().run()?;
        self.invalidate();
        Ok(())
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
        gix_read::status(&self.repo())
    }

    fn remotes(&self) -> Result<Vec<Remote>, GitError> {
        Err(not_implemented("remotes", "M3"))
    }

    fn stashes(&self) -> Result<Vec<StashEntry>, GitError> {
        Err(not_implemented("stashes", "M3"))
    }

    fn divergence(&self) -> Result<HashMap<String, Divergence>, GitError> {
        Err(not_implemented("divergence", "M3"))
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
    // Every one of these goes through `crate::process`, never by building a
    // command as a shell string: a branch name or a path from an untrusted
    // repository must never reach a shell. Arguments go in a vector and paths
    // go after `--`.

    fn stage(&self, paths: &[&Path]) -> Result<(), GitError> {
        if paths.is_empty() {
            return Ok(());
        }
        self.guard_index()?;

        // `--all` so a path that names a deleted file records the deletion
        // rather than being skipped for not existing.
        self.write(GitCommand::new("add").arg("--all").operands(paths))
    }

    fn stage_patch(&self, patch: &Patch) -> Result<(), GitError> {
        self.guard_index()?;

        // The patch arrives on stdin rather than through a temporary file:
        // nothing to name, nothing to clean up, and no window in which a path
        // built from repository content reaches the filesystem.
        let mut command = GitCommand::new("apply").arg("--cached");
        if patch.reverse {
            command = command.arg("--reverse");
        }
        // `-` is the operand meaning stdin, and it goes after `--` like any
        // other so a patch can never be mistaken for a flag.
        command = command.operands(["-"]);

        command
            .cwd(&self.workdir)
            .takes_locks()
            .run_with_stdin(Some(patch.text.as_bytes()))?;
        self.invalidate();
        Ok(())
    }

    fn unstage(&self, paths: &[&Path]) -> Result<(), GitError> {
        if paths.is_empty() {
            return Ok(());
        }
        self.guard_index()?;

        // `git restore --staged` needs something to restore *from*, and an
        // unborn HEAD has no commit to name. `git rm --cached` is the only way
        // to take a path back out of a first commit that does not exist yet.
        if matches!(gix_read::head(&self.repo())?, Head::Unborn { .. }) {
            return self.write(
                GitCommand::new("rm")
                    .args(["--cached", "--quiet", "-r"])
                    .operands(paths),
            );
        }

        self.write(
            GitCommand::new("restore")
                .args(["--staged", "--source=HEAD"])
                .operands(paths),
        )
    }

    fn discard(&self, paths: &[&Path]) -> Result<(), GitError> {
        if paths.is_empty() {
            return Ok(());
        }
        self.guard_index()?;

        // Two different operations wear one name. A tracked file is restored
        // from the index; an untracked one has no index entry to restore from
        // and has to be deleted outright. `git restore` fails on the latter,
        // so they are separated first rather than discovered by failure.
        let status = gix_read::status(&self.repo())?;
        let (untracked, tracked): (Vec<&Path>, Vec<&Path>) = paths
            .iter()
            .partition(|p| status.untracked.iter().any(|u| u == *p));

        if !tracked.is_empty() {
            self.write(
                GitCommand::new("restore")
                    .arg("--worktree")
                    .operands(&tracked),
            )?;
        }
        if !untracked.is_empty() {
            // `-d` because an untracked entry may be a whole directory: the
            // status walk collapses one into a single row.
            self.write(
                GitCommand::new("clean")
                    .args(["--force", "-d", "--quiet"])
                    .operands(&untracked),
            )?;
        }

        Ok(())
    }

    fn create_commit(&self, message: &str, opts: CommitOpts) -> Result<ObjectId, GitError> {
        self.guard_index()?;

        // `--file -` rather than `-m`: a commit message is arbitrary text from
        // the user, and passing it on stdin means no length limit, no encoding
        // surprises, and no argument that could begin with a `-`.
        let mut command = GitCommand::new("commit").args(["--file", "-"]);
        if opts.amend {
            command = command.arg("--amend");
        }
        if opts.sign_off {
            command = command.arg("--signoff");
        }
        if opts.allow_empty {
            command = command.arg("--allow-empty");
        }
        // Git strips comment lines and trailing whitespace from a message it
        // reads from a file. hideGit's editor is not Git's, so a line the user
        // typed starting with `#` is theirs to keep.
        command = command.arg("--cleanup=whitespace");

        command
            .cwd(&self.workdir)
            .takes_locks()
            .run_with_stdin(Some(message.as_bytes()))?;
        self.invalidate();

        // Hooks and GPG signing are the user's `git` doing its job, which is
        // half the reason writes shell out at all — so the new commit's id is
        // read back rather than assumed.
        let head = GitCommand::new("rev-parse")
            .arg("HEAD")
            .cwd(&self.workdir)
            .run()?;
        ObjectId::from_hex(head.trimmed_stdout().trim())
            .ok_or_else(|| GitError::RefNotFound("HEAD".to_owned()))
    }

    fn checkout(&self, _target: &CheckoutTarget) -> Result<(), GitError> {
        Err(not_implemented("checkout", "M3"))
    }

    fn create_branch(&self, _name: &str, _from: &StartPoint) -> Result<(), GitError> {
        Err(not_implemented("create-branch", "M3"))
    }

    fn rename_branch(&self, _from: &str, _to: &str) -> Result<(), GitError> {
        Err(not_implemented("rename-branch", "M3"))
    }

    fn delete_branch(&self, _name: &str, _force: bool) -> Result<(), GitError> {
        Err(not_implemented("delete-branch", "M3"))
    }

    fn create_tag(&self, _spec: &TagSpec) -> Result<(), GitError> {
        Err(not_implemented("create-tag", "M3"))
    }

    fn delete_tag(&self, _name: &str) -> Result<(), GitError> {
        Err(not_implemented("delete-tag", "M3"))
    }

    fn add_remote(&self, _name: &str, _url: &str) -> Result<(), GitError> {
        Err(not_implemented("add-remote", "M3"))
    }

    fn set_remote_url(&self, _name: &str, _url: &str) -> Result<(), GitError> {
        Err(not_implemented("set-remote-url", "M3"))
    }

    fn remove_remote(&self, _name: &str) -> Result<(), GitError> {
        Err(not_implemented("remove-remote", "M3"))
    }

    fn fetch(
        &self,
        _remote: &str,
        _opts: &FetchOpts,
        _progress: &dyn ProgressSink,
        _cancel: &CancelToken,
    ) -> Result<FetchOutcome, GitError> {
        Err(not_implemented("fetch", "M3"))
    }

    fn pull(
        &self,
        _opts: &PullOpts,
        _progress: &dyn ProgressSink,
        _cancel: &CancelToken,
    ) -> Result<PullOutcome, GitError> {
        Err(not_implemented("pull", "M3"))
    }

    fn push(
        &self,
        _remote: &str,
        _spec: &PushSpec,
        _progress: &dyn ProgressSink,
        _cancel: &CancelToken,
    ) -> Result<PushOutcome, GitError> {
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
