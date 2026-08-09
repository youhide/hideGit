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
use std::sync::{Arc, Mutex, RwLock};

use super::{GitBackend, gix_read, not_implemented};
use crate::error::{GitError, classify_remote_failure};
use crate::model::{
    Blob, Commit, CommitDetail, Diff, DiffTarget, Divergence, Head, LogPage, ObjectId, ReflogEntry,
    Refs, Remote, RepoState, RevSpec, StashEntry, WorktreeStatus,
};
use crate::ops::{
    Blame, CancelToken, CheckoutTarget, CommitOpts, FastForward, FetchOpts, FetchOutcome,
    ForceMode, MergeOpts, MergeOutcome, Patch, ProgressSink, PullOpts, PullOutcome, PushOutcome,
    PushSpec, RebasePlan, ResetMode, SequenceControl, SequenceOutcome, StartPoint, StashOp,
    StashOutcome, TagSpec,
};
use crate::process::GitCommand;

/// A repository, opened once and shared.
///
/// Cheap to clone into a task: the underlying handle is reference-counted, so
/// the UI never holds a lock across an await.
#[derive(Debug)]
pub struct HybridBackend {
    /// The gitoxide handle, behind a lock because it is *replaced* on
    /// invalidation, not just consulted.
    ///
    /// gitoxide reads `.git/config` when a repository is opened and caches the
    /// result. A `git` command that rewrites config — renaming a branch, adding a
    /// remote, `push --set-upstream` — therefore leaves this handle answering from
    /// the old file, and the symptom is subtle: the renamed branch silently
    /// appears to track nothing. Reopening is what keeps the read side honest.
    repo: RwLock<gix::ThreadSafeRepository>,
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
        self.repo
            .read()
            .expect("the repository lock is never poisoned across a panic-free path")
            .to_thread_local()
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

    /// A start point as the single operand `git` takes for one.
    ///
    /// `HEAD` is spelled out rather than omitted so every command that takes a
    /// start point takes exactly one operand, which keeps the argument vectors
    /// uniform and leaves no branch where a user-supplied name could land in a
    /// different position.
    fn start_point(from: &StartPoint) -> String {
        match from {
            StartPoint::Head => "HEAD".to_owned(),
            StartPoint::Commit(id) => id.to_hex(),
            StartPoint::Ref(name) => name.clone(),
        }
    }

    /// The commit `HEAD` currently points at.
    ///
    /// Read through `git rev-parse` rather than gitoxide because every caller
    /// asks *after* a write, and the point is to see what that write produced —
    /// including whatever the user's hooks and GPG signing did to it.
    fn head_id(&self) -> Result<ObjectId, GitError> {
        let head = GitCommand::new("rev-parse")
            .arg("HEAD")
            .cwd(&self.workdir)
            .run()?;
        ObjectId::from_hex(head.trimmed_stdout().trim())
            .ok_or_else(|| GitError::RefNotFound("HEAD".to_owned()))
    }

    /// True when `id` has more than one parent.
    ///
    /// This is how a merge tells a fast-forward from a merge commit: the two
    /// differ in the shape of the commit that came out, and reading that is
    /// stable in a way that parsing `git merge`'s localised summary is not.
    fn is_merge_commit(&self, id: ObjectId) -> Result<bool, GitError> {
        Ok(gix_read::parent_count(&self.repo(), id)? > 1)
    }

    /// The commit a stopped sequence is sitting on, if it is stopped at all.
    ///
    /// Git records it in a pseudo-ref whose name depends on the operation —
    /// `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `REBASE_HEAD` — and none of them
    /// exists when nothing is in progress, so a missing ref is a plain `None`
    /// rather than a failure.
    fn stopped_at(&self) -> Result<Option<ObjectId>, GitError> {
        for name in ["CHERRY_PICK_HEAD", "REVERT_HEAD", "REBASE_HEAD"] {
            let found = GitCommand::new("rev-parse")
                .arg("--verify")
                .arg("--quiet")
                .revisions([name])
                .cwd(&self.workdir)
                .run();
            if let Ok(output) = found
                && let Some(id) = ObjectId::from_hex(output.trimmed_stdout().trim())
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Whether a sequence that just ran is finished or stopped part-way.
    ///
    /// Both look like a successful `git` invocation — a cherry-pick that stops
    /// on an `edit` step exits zero — so the repository is asked rather than
    /// the exit status believed.
    fn sequence_state(&self) -> Result<SequenceOutcome, GitError> {
        if !self.repo_state()?.is_in_progress() {
            return Ok(SequenceOutcome::Completed);
        }
        Ok(SequenceOutcome::Stopped {
            at: self.stopped_at()?.unwrap_or(self.head_id()?),
            conflicts: gix_read::status(&self.repo())?.conflicted,
        })
    }

    /// Runs `cherry-pick` or `revert` over `ids`, in the order given.
    ///
    /// The two commands take the same arguments and stop the same way, so they
    /// share this; what differs is only which commit the user is reasoning
    /// about, and that is the caller's distinction to keep.
    fn sequence(
        &self,
        subcommand: &'static str,
        ids: &[ObjectId],
    ) -> Result<SequenceOutcome, GitError> {
        self.guard_index()?;

        if ids.is_empty() {
            // Git would take an empty argument list as "no commits given" and
            // fail with a usage message about the command rather than about
            // what was asked. Nothing to do is not an error.
            return Ok(SequenceOutcome::Completed);
        }

        let result = GitCommand::new(subcommand)
            // Without this, a multi-commit sequence opens an editor per commit.
            .env("GIT_EDITOR", "true")
            .operands(ids.iter().map(|id| id.to_hex()))
            .cwd(&self.workdir)
            .takes_locks()
            .run();
        self.invalidate();

        if let Err(error) = result {
            let conflicts = gix_read::status(&self.repo())?.conflicted;
            if !conflicts.is_empty() {
                return Ok(SequenceOutcome::Stopped {
                    at: self.stopped_at()?.unwrap_or(self.head_id()?),
                    conflicts,
                });
            }
            return Err(error);
        }

        self.sequence_state()
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
            repo: RwLock::new(repo),
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
        gix_read::remotes(&self.repo())
    }

    fn stashes(&self) -> Result<Vec<StashEntry>, GitError> {
        gix_read::stashes(&self.repo())
    }

    fn divergence(&self) -> Result<HashMap<String, Divergence>, GitError> {
        gix_read::divergence(&self.repo())
    }

    fn blame(&self, _path: &Path, _at: ObjectId) -> Result<Blame, GitError> {
        Err(not_implemented("blame", "M6"))
    }

    fn invalidate(&self) {
        self.walks
            .lock()
            .expect("the walk cache mutex is never poisoned across a panic-free path")
            .clear();

        // Reopened, not just cleared. gitoxide caches `.git/config` from the
        // moment a repository is opened, so a rename or a remote change would
        // otherwise be invisible to every subsequent read.
        match gix_read::open(&self.workdir) {
            Ok(reopened) => {
                *self
                    .repo
                    .write()
                    .expect("the repository lock is never poisoned across a panic-free path") =
                    reopened;
            }
            // Keeping the old handle is strictly better than having none: the
            // repository may be mid-operation, and a stale read beats a panic
            // in a method that cannot report an error.
            Err(error) => {
                tracing::warn!(
                    path = %self.workdir.display(),
                    %error,
                    "could not reopen the repository after a write; reads may be stale"
                );
            }
        }
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

    fn checkout(&self, target: &CheckoutTarget) -> Result<(), GitError> {
        self.guard_index()?;

        // `git switch`, not `git checkout`. `switch` only ever switches
        // branches, so a ref named like a path cannot be read as one — the
        // ambiguity that makes `checkout` dangerous with names that came from a
        // repository someone else controls. It has been available since 2.23,
        // well under the 2.30 hideGit already requires.
        let command = match target {
            CheckoutTarget::Branch(name) => GitCommand::new("switch").operands([name]),

            CheckoutTarget::Commit(id) => GitCommand::new("switch")
                // Detaching is stated rather than stumbled into: without
                // `--detach`, `switch` refuses a commit outright.
                .arg("--detach")
                .operands([id.to_hex()]),

            // The new name goes in `--create=<name>` rather than as an operand.
            // `switch` accepts exactly one reference after `--` — it takes no
            // paths, so `--` separates nothing else — and passing two makes it
            // fail with "only one reference expected". Attaching the name to the
            // option keeps it a single argv element either way, so a name
            // starting with `-` is still a value and never a flag.
            CheckoutTarget::NewBranch { name, from } => GitCommand::new("switch")
                .arg(format!("--create={name}"))
                .operands([Self::start_point(from)]),

            // `--track` is what makes the new branch's upstream the remote
            // branch rather than nothing, which is the whole difference from
            // `NewBranch` at the same commit.
            CheckoutTarget::TrackRemote { remote_ref, local } => GitCommand::new("switch")
                .arg(format!("--create={local}"))
                .arg("--track")
                .operands([remote_ref]),
        };

        // Local changes that would be overwritten make this fail, and Git's own
        // message says which files. hideGit does not stash on the user's behalf:
        // that moves their work somewhere they never asked for.
        self.write(command)
    }

    fn create_branch(&self, name: &str, from: &StartPoint) -> Result<(), GitError> {
        self.guard_index()?;
        self.write(GitCommand::new("branch").operands([name.to_owned(), Self::start_point(from)]))
    }

    fn rename_branch(&self, from: &str, to: &str) -> Result<(), GitError> {
        self.guard_index()?;
        // Without `--force`, which is deliberate: renaming onto a name that
        // already exists would destroy that branch, and nothing in the UI asks
        // for that.
        self.write(GitCommand::new("branch").arg("--move").operands([from, to]))
    }

    fn delete_branch(&self, name: &str, force: bool) -> Result<(), GitError> {
        self.guard_index()?;

        let mut command = GitCommand::new("branch").arg("--delete");
        if force {
            command = command.arg("--force");
        }
        // When `force` is false and the branch is unmerged, Git refuses and says
        // so. That refusal is surfaced rather than retried with `--force`:
        // losing commits has to be something the user chose.
        self.write(command.operands([name]))
    }

    fn create_tag(&self, spec: &TagSpec) -> Result<(), GitError> {
        self.guard_index()?;

        let start = Self::start_point(&spec.at);
        match &spec.message {
            // Annotated: it becomes an object of its own, carrying a message and
            // an identity. The message goes on stdin like a commit's, because
            // arbitrary user text must never become an argv element.
            Some(message) => {
                GitCommand::new("tag")
                    .args(["--annotate", "--file", "-"])
                    // Git strips comment lines from a message read from a file.
                    // hideGit's editor is not Git's, so a line the user typed
                    // starting with `#` is theirs to keep.
                    .arg("--cleanup=whitespace")
                    .operands([spec.name.clone(), start])
                    .cwd(&self.workdir)
                    .takes_locks()
                    .run_with_stdin(Some(message.as_bytes()))?;
                self.invalidate();
                Ok(())
            }
            // Lightweight: just a ref.
            None => self.write(GitCommand::new("tag").operands([spec.name.clone(), start])),
        }
    }

    fn delete_tag(&self, name: &str) -> Result<(), GitError> {
        self.guard_index()?;
        self.write(GitCommand::new("tag").arg("--delete").operands([name]))
    }

    fn add_remote(&self, name: &str, url: &str) -> Result<(), GitError> {
        self.guard_index()?;
        // A URL comes from whatever the user pasted, so it goes after `--` like
        // any other operand — one beginning with `-` must not become a flag.
        self.write(GitCommand::new("remote").arg("add").operands([name, url]))
    }

    fn set_remote_url(&self, name: &str, url: &str) -> Result<(), GitError> {
        self.guard_index()?;
        self.write(
            GitCommand::new("remote")
                .arg("set-url")
                .operands([name, url]),
        )
    }

    fn remove_remote(&self, name: &str) -> Result<(), GitError> {
        self.guard_index()?;
        // Takes the remote's tracking refs and its `branch.*.remote` config with
        // it, which is why the gitoxide snapshot has to be dropped afterwards —
        // `write` does that.
        self.write(GitCommand::new("remote").arg("remove").operands([name]))
    }

    fn fetch(
        &self,
        remote: &str,
        opts: &FetchOpts,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<FetchOutcome, GitError> {
        let mut command = GitCommand::new("fetch").arg("--progress");
        if opts.prune {
            command = command.arg("--prune");
        }
        if opts.tags {
            command = command.arg("--tags");
        }
        command = if opts.all_remotes {
            command.arg("--all")
        } else {
            command.operands([remote])
        };

        // A fetch does not take the index lock, so it is not routed through
        // `write` — but it does move refs, so the memoised walk and the config
        // snapshot still have to be dropped afterwards.
        let output = command
            .cwd(&self.workdir)
            .takes_locks()
            .run_streaming(progress, cancel)
            .map_err(|error| classify_remote_failure(remote, error))?;
        self.invalidate();

        Ok(parse_fetch(&output.stderr))
    }

    fn pull(
        &self,
        opts: &PullOpts,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<PullOutcome, GitError> {
        self.guard_index()?;

        let before = self.head()?.target();

        // No strategy flag. `pull.rebase`, `pull.ff` and `rebase.autoStash` are
        // the user's own configuration, and carrying it is half the reason writes
        // shell out — a pull that behaved differently here than in the same
        // user's terminal is the surprise this whole design avoids.
        let mut command = GitCommand::new("pull").arg("--progress");
        if let Some(remote) = &opts.remote {
            command = command.operands([remote]);
        }

        let outcome = command
            .cwd(&self.workdir)
            .takes_locks()
            .run_streaming(progress, cancel);
        self.invalidate();

        if let Err(error) = outcome {
            // A conflicted merge or rebase exits non-zero, and it is an outcome
            // rather than a failure: it routes to the conflict UI, not to a
            // toast. Anything with no conflicts behind it really did fail.
            let conflicts = self.status()?.conflicted;
            if !conflicts.is_empty() {
                return Ok(PullOutcome::Conflicted(conflicts));
            }
            return Err(classify_remote_failure(
                opts.remote.as_deref().unwrap_or("the upstream remote"),
                error,
            ));
        }

        let after = self.head()?.target();
        match (before, after) {
            (Some(before), Some(after)) if before == after => Ok(PullOutcome::UpToDate),
            (Some(before), Some(after)) => {
                let repo = self.repo();
                // A merge commit means it was integrated rather than
                // fast-forwarded, and so does a new HEAD the old one is not an
                // ancestor of — which is what a rebase produces.
                let merged = gix_read::commit_detail(&repo, after)?.commit.is_merge();
                if merged || !gix_read::is_ancestor(&repo, before, after)? {
                    Ok(PullOutcome::Integrated(after))
                } else {
                    Ok(PullOutcome::FastForwarded(after))
                }
            }
            // An unborn branch that just gained its first history.
            (None, Some(after)) => Ok(PullOutcome::FastForwarded(after)),
            (_, None) => Ok(PullOutcome::UpToDate),
        }
    }

    fn push(
        &self,
        remote: &str,
        spec: &PushSpec,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<PushOutcome, GitError> {
        // **A deliberate exception to preferring machine formats.**
        // `git push --porcelain` puts a stable tab-separated result on stdout,
        // and that would be the obvious choice — except that it also *moves the
        // failure detail off stderr*. Asked to push a stale lease, plain `git
        // push` writes `! [rejected] main -> main (stale info)` and a hint saying
        // what to do; with `--porcelain` those go to stdout and stderr keeps only
        // `error: failed to push some refs`. Since Git's own message is the most
        // useful thing hideGit has to say when a command fails, losing it costs
        // more than parsing the human summary does — and this parser fails soft,
        // so a wording change costs a summary line rather than correctness.
        let mut command = GitCommand::new("push").arg("--progress");

        command = match spec.force {
            ForceMode::None => command,
            // The default whenever a force is requested at all: it refuses if the
            // remote moved since the last fetch.
            ForceMode::WithLease => command.arg("--force-with-lease"),
            // Never a fallback for a failed lease. Reaching this means the user
            // selected it deliberately.
            ForceMode::Force => command.arg("--force"),
        };
        if spec.set_upstream {
            command = command.arg("--set-upstream");
        }

        let result = command
            .operands([remote, spec.refspec.as_str()])
            .cwd(&self.workdir)
            .takes_locks()
            .run_streaming(progress, cancel);
        // `--set-upstream` writes `branch.*.merge` into config, so the gitoxide
        // snapshot has to be dropped whether the push succeeded or not.
        self.invalidate();

        match result {
            Ok(output) => Ok(parse_push(&output.stderr)),
            Err(error) => {
                // Pushing several refs can update some and be refused others, and
                // reporting only the failure would hide what did land. Nothing
                // landing at all is a plain failure, and Git's own hint — which
                // says exactly what to do about a non-fast-forward — is then the
                // most useful thing hideGit can show.
                if let GitError::Command { stderr, .. } = &error {
                    let outcome = parse_push(stderr);
                    if !outcome.updated.is_empty() {
                        return Ok(outcome);
                    }
                }
                Err(classify_remote_failure(remote, error))
            }
        }
    }

    fn stash(&self, op: &StashOp) -> Result<StashOutcome, GitError> {
        self.guard_index()?;

        match op {
            StashOp::Push {
                message,
                include_untracked,
            } => {
                let mut command = GitCommand::new("stash").arg("push");
                if *include_untracked {
                    command = command.arg("--include-untracked");
                }
                if let Some(message) = message {
                    // `--message=<text>` as one argument, not `--message -`:
                    // unlike `git commit --file -`, `git stash push` does not read
                    // its message from stdin — it takes `-` as the literal message,
                    // which is how this was found. Attaching the text to the option
                    // keeps it a single argv element, so a message that begins with
                    // a dash is a message and never a flag.
                    command = command.arg(format!("--message={message}"));
                }

                command.cwd(&self.workdir).takes_locks().run()?;
                self.invalidate();

                // Read back rather than assumed: `git stash push` with nothing to
                // stash succeeds and creates nothing, and claiming an entry that
                // does not exist would leave the sidebar pointing at nothing.
                match gix_read::stashes(&self.repo())?.first() {
                    Some(entry) => Ok(StashOutcome::Created(entry.id)),
                    None => Ok(StashOutcome::Applied),
                }
            }

            StashOp::Apply(index) | StashOp::Pop(index) => {
                let verb = if matches!(op, StashOp::Apply(_)) {
                    "apply"
                } else {
                    "pop"
                };
                let result = GitCommand::new("stash")
                    .arg(verb)
                    .operands([format!("stash@{{{index}}}")])
                    .cwd(&self.workdir)
                    .takes_locks()
                    .run();
                self.invalidate();

                if let Err(error) = result {
                    // A stash that conflicts is an outcome, not a failure: the
                    // entry survives, the worktree has markers in it, and the user
                    // has to resolve them. `pop` deliberately keeps the entry in
                    // that case, which is Git's own behaviour.
                    let conflicts = gix_read::status(&self.repo())?.conflicted;
                    if !conflicts.is_empty() {
                        return Ok(StashOutcome::Conflicted(conflicts));
                    }
                    return Err(error);
                }
                Ok(StashOutcome::Applied)
            }

            StashOp::Drop(index) => {
                self.write(
                    GitCommand::new("stash")
                        .arg("drop")
                        .operands([format!("stash@{{{index}}}")]),
                )?;
                Ok(StashOutcome::Dropped)
            }
        }
    }

    fn merge(&self, from: &str, opts: &MergeOpts) -> Result<MergeOutcome, GitError> {
        self.guard_index()?;

        let before = self.head_id()?;

        let mut command = GitCommand::new("merge");
        match opts.fast_forward {
            FastForward::Allow => {}
            FastForward::Only => command = command.arg("--ff-only"),
            FastForward::Never => command = command.arg("--no-ff"),
        }
        if let Some(message) = &opts.message {
            // `--file=-` is not available on `git merge`, so the message is
            // attached to its option instead. One argv element either way, so a
            // message beginning with a dash stays a message.
            command = command.arg(format!("--message={message}"));
        }

        let result = command
            .operands([from])
            .cwd(&self.workdir)
            .takes_locks()
            .run();
        self.invalidate();

        if let Err(error) = result {
            // A merge that conflicts is the outcome the user asked for arriving
            // in the state that needs resolving, not a failure. Anything else —
            // an unknown ref, a refused `--ff-only` — is a real error and keeps
            // Git's own message.
            let conflicts = gix_read::status(&self.repo())?.conflicted;
            if !conflicts.is_empty() {
                return Ok(MergeOutcome::Conflicted(conflicts));
            }
            return Err(error);
        }

        // Which of the three success shapes happened is read back from the
        // repository rather than parsed out of `git merge`'s human summary,
        // whose wording is localised and has changed between versions.
        let after = self.head_id()?;
        if after == before {
            return Ok(MergeOutcome::UpToDate);
        }
        if self.is_merge_commit(after)? {
            Ok(MergeOutcome::Merged(after))
        } else {
            Ok(MergeOutcome::FastForwarded(after))
        }
    }

    fn rebase(&self, _onto: &str, _plan: &RebasePlan) -> Result<SequenceOutcome, GitError> {
        Err(not_implemented("rebase", "M5"))
    }

    fn cherry_pick(&self, ids: &[ObjectId]) -> Result<SequenceOutcome, GitError> {
        self.sequence("cherry-pick", ids)
    }

    fn revert(&self, ids: &[ObjectId]) -> Result<SequenceOutcome, GitError> {
        self.sequence("revert", ids)
    }

    fn reset(&self, target: &StartPoint, mode: ResetMode) -> Result<(), GitError> {
        self.guard_index()?;

        // No `--` and no pathspec: this is the whole-tree reset. A path-scoped
        // `git reset` means something different — it unstages — and that is
        // `unstage`, which is a separate method for exactly that reason.
        self.write(
            GitCommand::new("reset")
                .arg(mode.flag())
                .revisions([Self::start_point(target)]),
        )
    }

    fn control_sequence(&self, control: SequenceControl) -> Result<SequenceOutcome, GitError> {
        self.guard_index()?;

        let state = self.repo_state()?;
        // Git has no single verb for "continue what is in progress", so the
        // pairing is made here from the state hideGit already reads. Asking the
        // caller to pass it would let the UI send `rebase --continue` to a
        // repository that is mid-merge.
        let subcommand = match state {
            RepoState::Merging => "merge",
            RepoState::Rebasing => "rebase",
            RepoState::CherryPicking => "cherry-pick",
            RepoState::Reverting => "revert",
            // Bisect has its own vocabulary — `git bisect good`, not
            // `--continue` — and is not part of this milestone.
            RepoState::Bisecting | RepoState::Clean => {
                return Err(GitError::NothingInProgress(state));
            }
        };

        let flag = match control {
            SequenceControl::Continue => "--continue",
            SequenceControl::Abort => "--abort",
            SequenceControl::Skip => "--skip",
        };

        // A merge is one step, so there is nothing to skip past; Git rejects it
        // and the message is about `git merge` rather than about what the user
        // clicked. Refusing here says the useful thing instead.
        if control == SequenceControl::Skip && state == RepoState::Merging {
            return Err(GitError::NotSkippable);
        }

        let result = GitCommand::new(subcommand)
            .arg(flag)
            // `--continue` opens an editor for the commit message unless told
            // not to, and an editor hideGit cannot see would hang the task
            // forever. The message Git already prepared is the right one: the
            // user edits it in the resolver, not in `vi`.
            .env("GIT_EDITOR", "true")
            .cwd(&self.workdir)
            .takes_locks()
            .run();
        self.invalidate();

        if let Err(error) = result {
            // Continuing with conflicts still unresolved leaves the repository
            // exactly where it was, which is a state to report rather than an
            // error to raise.
            let conflicts = gix_read::status(&self.repo())?.conflicted;
            if !conflicts.is_empty() {
                return Ok(SequenceOutcome::Stopped {
                    at: self.stopped_at()?.unwrap_or(self.head_id()?),
                    conflicts,
                });
            }
            return Err(error);
        }

        // An abort finishes by definition; anything else may have stopped again
        // on the next commit in the sequence.
        if control == SequenceControl::Abort {
            return Ok(SequenceOutcome::Completed);
        }
        self.sequence_state()
    }

    fn reflog(&self, ref_name: &str, limit: usize) -> Result<Vec<ReflogEntry>, GitError> {
        gix_read::reflog(&self.repo(), ref_name, limit)
    }
}

/// The flag `git` puts in the first column of a fetch or push result line.
///
/// One space means an ordinary fast-forward, which is why the flag is read from
/// the trimmed line rather than from a fixed column: leading whitespace varies and
/// a summary like `a3f9c21..b7e2d10` starts with none of these characters.
fn ref_flag(line: &str) -> char {
    let trimmed = line.trim_start();
    match trimmed.chars().next() {
        Some(c @ ('+' | '-' | 't' | '*' | '!' | '=')) if trimmed[1..].starts_with(' ') => c,
        _ => ' ',
    }
}

/// The destination ref of a `<from> -> <to>` line.
fn destination(line: &str) -> Option<String> {
    let (_, to) = line.rsplit_once("-> ")?;
    let to = to.split_whitespace().next()?;
    (!to.is_empty()).then(|| to.to_owned())
}

/// What a fetch brought back, from its stderr.
///
/// `git fetch` has no machine format before 2.41, so this reads the human output
/// — the one place in hideGit that does. It is written to fail soft: a line it
/// does not recognise is skipped, and an outcome that comes back empty means "the
/// fetch worked and we could not summarise it", never an error. The refs
/// themselves are read from the repository afterwards regardless.
fn parse_fetch(stderr: &str) -> FetchOutcome {
    let mut outcome = FetchOutcome::default();

    for line in stderr.lines() {
        let Some(to) = destination(line) else {
            continue;
        };
        match ref_flag(line) {
            '-' => outcome.pruned.push(to),
            // `=` is "already up to date", which is not something that changed.
            '=' => {}
            // `!` is a rejected fetch, which `git` also reports non-zero for.
            '!' => {}
            _ => outcome.updated.push(to),
        }
    }

    outcome
}

/// What a push did, from its stderr summary.
///
/// The same shape as a fetch's, and read the same way, for the reason given at the
/// `--progress`-without-`--porcelain` decision in [`GitBackend::push`]: the
/// machine format would move the failure detail off stderr, and that detail is
/// what the user needs. Also fails soft — an unrecognised line costs a summary
/// entry, never the operation.
fn parse_push(stderr: &str) -> PushOutcome {
    let mut outcome = PushOutcome::default();

    for line in stderr.lines() {
        let Some(to) = destination(line) else {
            continue;
        };
        match ref_flag(line) {
            '!' => outcome.rejected.push(to),
            // `=` is up to date: nothing moved, and saying it did would be a lie.
            '=' => {}
            _ => outcome.updated.push(to),
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fetch_summary_separates_what_moved_from_what_was_pruned() {
        // Captured from a real `git fetch --progress --prune`.
        let stderr = "\
From /tmp/remote
   a3f9c21..b7e2d10  main         -> origin/main
 * [new branch]      feat/graph   -> origin/feat/graph
 + f00dcafe...deadbee forced      -> origin/forced  (forced update)
 - [deleted]         (none)       -> origin/gone
 = [up to date]      release      -> origin/release
 * [new tag]         v0.2.0       -> v0.2.0
";
        let outcome = parse_fetch(stderr);

        assert_eq!(
            outcome.updated,
            vec![
                "origin/main",
                "origin/feat/graph",
                "origin/forced",
                "v0.2.0"
            ]
        );
        assert_eq!(outcome.pruned, vec!["origin/gone"]);
    }

    #[test]
    fn a_fetch_that_brought_nothing_back_is_an_empty_summary_not_a_failure() {
        assert_eq!(parse_fetch(""), FetchOutcome::default());
        // Progress output and nothing else: the fetch worked, there was no news.
        assert_eq!(
            parse_fetch("remote: Enumerating objects: 100% (5/5), done.\n"),
            FetchOutcome::default()
        );
    }

    #[test]
    fn a_line_shape_git_may_change_is_skipped_rather_than_guessed_at() {
        // The reason this parser fails soft: Git's human output is not an
        // interface. Something unrecognised must cost a summary line, never the
        // whole operation.
        let outcome = parse_fetch("From /tmp/remote\nsomething entirely new\n");
        assert_eq!(outcome, FetchOutcome::default());
    }

    #[test]
    fn a_push_summary_reports_the_destination_ref() {
        // Captured from a real `git push --progress`.
        let stderr = "\
To /tmp/remote
   a3f9c21..b7e2d10  main -> main
 * [new branch]      feat -> feat
 = [up to date]      old -> old
";
        let outcome = parse_push(stderr);

        assert_eq!(outcome.updated, vec!["main", "feat"]);
        assert!(outcome.rejected.is_empty());
    }

    #[test]
    fn a_stale_lease_is_reported_as_a_rejection_like_any_other() {
        // The reason `--porcelain` is not used: with it, this line and Git's hint
        // go to stdout and stderr keeps only "failed to push some refs", so the
        // user loses the only part that says what to do.
        let stderr = "\
To /tmp/remote
 ! [rejected]        main -> main (stale info)
error: failed to push some refs to '/tmp/remote'
";
        let outcome = parse_push(stderr);

        assert!(outcome.updated.is_empty());
        assert_eq!(outcome.rejected, vec!["main"]);
    }

    #[test]
    fn a_wholly_rejected_push_reports_nothing_as_updated() {
        // This is what decides between an error carrying Git's hint and a partial
        // success, so it has to be exact.
        let stderr = "\
To /tmp/remote
 ! [rejected]        main -> main (non-fast-forward)
error: failed to push some refs to '/tmp/remote'
hint: Updates were rejected because the tip of your current branch is behind
";
        let outcome = parse_push(stderr);

        assert!(outcome.updated.is_empty(), "got {:?}", outcome.updated);
        assert_eq!(outcome.rejected, vec!["main"]);
    }

    #[test]
    fn a_partly_rejected_push_reports_both_halves() {
        let stderr = "\
To /tmp/remote
   a3f9c21..b7e2d10  ok -> ok
 ! [rejected]        no -> no (non-fast-forward)
";
        let outcome = parse_push(stderr);

        assert_eq!(outcome.updated, vec!["ok"]);
        assert_eq!(outcome.rejected, vec!["no"]);
    }

    #[test]
    fn a_summary_that_merely_starts_with_a_flag_character_is_not_a_flag() {
        // `t` and `-` are flags, but only followed by a space. A summary like
        // `tags/v1..HEAD` must not be read as a tag update.
        assert_eq!(ref_flag("   a3f9c21..b7e2d10  main -> origin/main"), ' ');
        assert_eq!(ref_flag(" t [tag update]      v1 -> v1"), 't');
        assert_eq!(ref_flag(" - [deleted]         (none) -> origin/gone"), '-');
        assert_eq!(ref_flag("total nonsense"), ' ');
    }
}
