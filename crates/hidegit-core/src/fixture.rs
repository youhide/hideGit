//! Programmatically built repositories for tests and benchmarks.
//!
//! Fixtures are built by running `git`, never committed as binary blobs, so a
//! test reads as a description of the history it exercises:
//!
//! ```no_run
//! # use hidegit_core::fixture::fixture;
//! let repo = fixture()
//!     .commit("A")
//!     .branch("feature")
//!     .commit("B")
//!     .checkout("main")
//!     .commit("C")
//!     .merge("feature")
//!     .build();
//! ```
//!
//! Every commit gets a distinct, increasing timestamp, so tests that care
//! about ordering are not at the mercy of how fast the machine runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::backend::{GitBackend, HybridBackend};
use crate::model::ObjectId;
use crate::process::GitCommand;

/// Starts building a repository. It is deleted when the [`Repo`] is dropped.
pub fn fixture() -> Fixture {
    Fixture::new()
}

/// A repository under construction.
#[derive(Debug)]
pub struct Fixture {
    dir: TempDir,
    commits: HashMap<String, ObjectId>,
    /// Seconds since the epoch for the next commit. Fixed rather than "now" so
    /// two runs of the same fixture produce the same ordering.
    clock: i64,
    /// Bare repositories acting as remotes, by remote name.
    ///
    /// Each is a real repository on a local path, so fetch, push and pull are
    /// exercised end to end with no network and no credentials — which is what
    /// keeps the suite hermetic on all three CI platforms.
    remotes: HashMap<String, TempDir>,
    /// The repositories submodules were cloned from, by submodule path. Held
    /// so they outlive the checkout that points at them.
    submodules: HashMap<String, TempDir>,
    /// The directories linked worktrees were checked out into, by name.
    ///
    /// Outside the repository rather than inside it, so a worktree does not
    /// show up as an untracked directory in the status of the repository that
    /// owns it.
    worktrees: HashMap<String, TempDir>,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("a writable temporary directory");

        let this = Self {
            dir,
            commits: HashMap::new(),
            // 2020-01-01T00:00:00Z, far enough from any real timestamp to be
            // recognisable in a failure message.
            clock: 1_577_836_800,
            remotes: HashMap::new(),
            submodules: HashMap::new(),
            worktrees: HashMap::new(),
        };

        this.git(["init", "-b", "main"]);
        // Fixtures must not depend on the developer's global Git config, and
        // must not pick up a repository-level identity from a parent checkout.
        this.git(["config", "user.name", "hideGit Fixture"]);
        this.git(["config", "user.email", "fixture@hidegit.invalid"]);
        this.git(["config", "commit.gpgsign", "false"]);
        // Windows installs default `core.autocrlf` to true, which rewrites line
        // endings on checkout: a file restored from the index comes back as
        // CRLF and a byte comparison against what the test wrote fails. That is
        // Git doing its job — honouring the user's configuration is half the
        // point of delegating writes to it — so the fixture opts out rather
        // than the assertions going vague about what they expect.
        this.git(["config", "core.autocrlf", "false"]);

        this
    }

    /// The repository's working directory.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git<const N: usize>(&self, args: [&str; N]) -> String {
        self.git_at(args, self.clock)
    }

    fn git_at<const N: usize>(&self, args: [&str; N], at: i64) -> String {
        let date = format!("{at} +0000");
        let mut command = GitCommand::new("--no-pager")
            .args(args)
            .cwd(self.dir.path())
            .takes_locks()
            // Author and committer dates are pinned so a fixture's ordering is
            // reproducible rather than dependent on wall-clock time.
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date);
        // Never let a fixture open an editor and block the test run.
        command = command.env("GIT_EDITOR", "true");

        match command.run() {
            Ok(output) => output.trimmed_stdout(),
            Err(e) => panic!("fixture command failed: {e}"),
        }
    }

    fn record(mut self, name: &str) -> Self {
        let id = self.git(["rev-parse", "HEAD"]);
        let id = ObjectId::from_hex(&id).expect("git prints a full hash");
        self.commits.insert(name.to_owned(), id);
        self.clock += 60;
        self
    }

    /// Adds a commit that creates a file named after it.
    pub fn commit(self, name: &str) -> Self {
        let file = self.dir.path().join(format!("{name}.txt"));
        std::fs::write(&file, format!("contents of {name}\n")).expect("writing a fixture file");

        self.git(["add", "--all"]);
        self.git(["commit", "--message", name]);
        self.record(name)
    }

    /// Adds a commit that changes a file, for diff tests.
    ///
    /// `file` may name a path in a subdirectory; the directories are created as
    /// needed, so a test can exercise a nested path rather than only the
    /// repository root.
    pub fn edit(self, file: &str, contents: &str, message: &str) -> Self {
        let path = self.dir.path().join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("creating a fixture directory");
        }
        std::fs::write(&path, contents).expect("writing a fixture file");
        self.git(["add", "--all"]);
        self.git(["commit", "--message", message]);
        self.record(message)
    }

    /// Adds a commit containing a binary file.
    pub fn binary(self, file: &str, message: &str) -> Self {
        let bytes: Vec<u8> = (0u8..=255).collect();
        std::fs::write(self.dir.path().join(file), bytes).expect("writing a fixture file");
        self.git(["add", "--all"]);
        self.git(["commit", "--message", message]);
        self.record(message)
    }

    /// Creates a branch at `HEAD` and switches to it.
    pub fn branch(self, name: &str) -> Self {
        self.git(["checkout", "-b", name]);
        self
    }

    /// Switches to an existing branch.
    pub fn checkout(self, name: &str) -> Self {
        self.git(["checkout", name]);
        self
    }

    /// Starts a branch with no history at all, the way an imported or
    /// documentation branch looks.
    pub fn orphan(self, name: &str) -> Self {
        self.git(["checkout", "--orphan", name]);
        self.git(["rm", "-rf", "--cached", "."]);
        self
    }

    /// Merges one branch into the current one, always creating a merge commit.
    pub fn merge(self, name: &str) -> Self {
        let message = format!("Merge {name}");
        self.git(["merge", "--no-ff", "--no-edit", "--message", &message, name]);
        self.record(&message)
    }

    /// Merges several branches at once — an octopus merge.
    pub fn merge_many(self, names: &[&str]) -> Self {
        let message = format!("Merge {}", names.join(" "));
        let mut args = vec!["merge", "--no-ff", "--no-edit", "--message", &message];
        args.extend(names.iter().copied());

        let date = format!("{} +0000", self.clock);
        let output = GitCommand::new("--no-pager")
            .args(&args)
            .cwd(self.dir.path())
            .takes_locks()
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_EDITOR", "true")
            .run();
        if let Err(e) = output {
            panic!("fixture octopus merge failed: {e}");
        }

        self.record(&message)
    }

    /// Points one ref at another, the way `origin/HEAD` points at a remote's
    /// default branch. A symbolic ref is a pointer, not a branch.
    pub fn symbolic_ref(self, name: &str, target: &str) -> Self {
        self.git(["symbolic-ref", name, target]);
        self
    }

    /// Creates a remote-tracking ref without needing an actual remote.
    ///
    /// Enough for testing how refs are *read*. Anything that talks to a remote —
    /// fetch, push, pull, ahead/behind — needs [`Fixture::with_remote`] instead,
    /// because a ref invented with `update-ref` has nothing on the other end.
    pub fn remote_ref(self, name: &str) -> Self {
        let head = self.git(["rev-parse", "HEAD"]);
        self.git(["update-ref", name, &head]);
        self
    }

    /// Adds a real remote: a bare repository on a local path, with the current
    /// branch pushed to it and tracking configured.
    ///
    /// A local path rather than a URL on purpose. Fetch and push run for real,
    /// through the same code as any other remote, and the suite still needs no
    /// network, no credential helper and no fixture server on any platform.
    ///
    /// Call it after at least one commit exists — there is nothing to push from
    /// an unborn branch, and `--set-upstream` has no branch to attach to.
    pub fn with_remote(mut self, name: &str) -> Self {
        let bare = TempDir::new().expect("a writable temporary directory");
        let url = bare.path().display().to_string();

        // `--bare` because a remote that has a worktree refuses a push to its
        // checked-out branch, which is not the failure any test here is about.
        let init = GitCommand::new("init")
            .args(["--bare", "-b", "main"])
            .operands([bare.path()])
            .takes_locks()
            .run();
        if let Err(e) = init {
            panic!("could not create a bare remote: {e}");
        }

        self.git(["remote", "add", name, &url]);

        let branch = self.git(["rev-parse", "--abbrev-ref", "HEAD"]);
        self.git(["push", "--set-upstream", name, &branch]);

        self.remotes.insert(name.to_owned(), bare);
        self
    }

    /// Adds a commit to the remote that the local repository does not have yet,
    /// so a fetch has something to bring back and `behind` is non-zero.
    ///
    /// The file is named after the commit, so it cannot conflict with anything
    /// local. Use [`Fixture::commit_on_remote_edit`] to set up a conflict
    /// deliberately.
    pub fn commit_on_remote(self, remote: &str, name: &str) -> Self {
        let file = format!("{name}.txt");
        let contents = format!("{name}\n");
        self.commit_on_remote_edit(remote, &file, &contents, name)
    }

    /// Changes a specific file on the remote, so a pull has to merge — and, when
    /// the same file also changed locally, conflict.
    ///
    /// Done by cloning the bare repository, committing there and pushing back,
    /// rather than by writing objects by hand: the result is a history a real
    /// `git` produced, which is the only kind worth asserting against.
    pub fn commit_on_remote_edit(
        self,
        remote: &str,
        file: &str,
        contents: &str,
        message: &str,
    ) -> Self {
        use std::ffi::OsStr;

        let bare = self
            .remotes
            .get(remote)
            .unwrap_or_else(|| panic!("no fixture remote named {remote}"));
        let url = bare.path().display().to_string();

        let scratch = TempDir::new().expect("a writable temporary directory");
        let work = scratch.path().join("work");
        let date = format!("{} +0000", self.clock);

        let run = |args: &[&OsStr], cwd: &Path| {
            let result = GitCommand::new("--no-pager")
                .args(args)
                .cwd(cwd)
                .takes_locks()
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .env("GIT_EDITOR", "true")
                .run();
            if let Err(e) = result {
                panic!("remote-side fixture command failed: {e}");
            }
        };

        run(
            &[OsStr::new("clone"), OsStr::new(&url), work.as_os_str()],
            scratch.path(),
        );
        // The clone must not inherit the developer's identity either.
        for (key, value) in [
            ("user.name", "hideGit Fixture"),
            ("user.email", "fixture@hidegit.invalid"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            run(
                &[OsStr::new("config"), OsStr::new(key), OsStr::new(value)],
                &work,
            );
        }

        let path = work.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a writable clone");
        }
        std::fs::write(&path, contents).expect("a writable clone");

        run(&[OsStr::new("add"), OsStr::new("--all")], &work);
        run(
            &[
                OsStr::new("commit"),
                OsStr::new("--message"),
                OsStr::new(message),
            ],
            &work,
        );
        run(&[OsStr::new("push")], &work);

        self
    }

    /// Adds a real submodule at `path`, checked out and committed.
    ///
    /// The source is a separate repository on a local path, for the same reason
    /// [`Fixture::with_remote`] uses one: `git submodule add` really clones it,
    /// through the same code any submodule goes through, with no network.
    ///
    /// That is also why `protocol.file.allow` appears here. Git has refused the
    /// `file` transport for submodules since CVE-2022-39253, where a malicious
    /// `.gitmodules` pointed at a path on the victim's own machine. The
    /// permission is granted for this one command, on a repository this fixture
    /// just created, and never by hideGit itself.
    pub fn with_submodule(mut self, path: &str) -> Self {
        let source = TempDir::new().expect("a writable temporary directory");
        let url = source.path().display().to_string();
        let date = format!("{} +0000", self.clock);

        let run = |args: &[&str], cwd: &Path| {
            let result = GitCommand::new("--no-pager")
                .args(args)
                .cwd(cwd)
                .takes_locks()
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .env("GIT_EDITOR", "true")
                .run();
            if let Err(e) = result {
                panic!("submodule-side fixture command failed: {e}");
            }
        };

        run(&["init", "-b", "main"], source.path());
        for (key, value) in [
            ("user.name", "hideGit Fixture"),
            ("user.email", "fixture@hidegit.invalid"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            run(&["config", key, value], source.path());
        }
        std::fs::write(source.path().join("nested.txt"), "nested\n")
            .expect("writing a fixture file");
        run(&["add", "--all"], source.path());
        run(&["commit", "--message", "Nested"], source.path());

        self.git([
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &url,
            path,
        ]);
        let message = format!("Add submodule {path}");
        self.git(["commit", "--message", &message]);

        self.submodules.insert(path.to_owned(), source);
        self.record(&message)
    }

    /// Commits inside a submodule's own checkout, moving its `HEAD` away from
    /// the commit the superproject records.
    ///
    /// Deliberately leaves the superproject alone: the point of the state is
    /// that the two disagree, and staging the gitlink here would resolve it.
    pub fn commit_in_submodule(self, path: &str, message: &str) -> Self {
        let work = self.dir.path().join(path);
        let date = format!("{} +0000", self.clock);

        std::fs::write(work.join("nested.txt"), format!("{message}\n"))
            .expect("writing a fixture file");

        for args in [vec!["add", "--all"], vec!["commit", "--message", message]] {
            let result = GitCommand::new("--no-pager")
                .args(&args)
                .cwd(&work)
                .takes_locks()
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .env("GIT_EDITOR", "true")
                .run();
            if let Err(e) = result {
                panic!("submodule-side fixture command failed: {e}");
            }
        }

        self
    }

    /// Removes a submodule's checkout, leaving the `.gitmodules` entry and the
    /// gitlink in place.
    ///
    /// This is the state a fresh clone of the superproject is in — `git clone`
    /// does not clone submodules — so it is the *common* case, not an edge one.
    pub fn deinit_submodule(self, path: &str) -> Self {
        self.git(["submodule", "deinit", "--force", "--", path]);
        self
    }

    /// Adds a linked worktree checked out on a new branch named after it.
    ///
    /// The directory sits outside the repository, so it does not appear as an
    /// untracked directory in the owning repository's status — which it would
    /// if it were nested, and which is a distraction in every test that is not
    /// about that.
    pub fn with_worktree(mut self, name: &str) -> Self {
        let dir = TempDir::new().expect("a writable temporary directory");
        let at = dir.path().join(name);

        let result = GitCommand::new("worktree")
            .args(["add", "-b", name])
            .operands([at.as_os_str()])
            .cwd(self.dir.path())
            .takes_locks()
            .env("GIT_EDITOR", "true")
            .run();
        if let Err(e) = result {
            panic!("could not add a fixture worktree: {e}");
        }

        self.worktrees.insert(name.to_owned(), dir);
        self
    }

    /// Locks a linked worktree, with a reason.
    ///
    /// A locked worktree refuses to be pruned or removed, which is what makes
    /// keeping one on a drive that is not always plugged in safe.
    pub fn lock_worktree(self, name: &str, reason: &str) -> Self {
        let at = self.worktree_path(name);
        self.git([
            "worktree",
            "lock",
            "--reason",
            reason,
            &at.display().to_string(),
        ]);
        self
    }

    /// Deletes a worktree's directory behind Git's back, leaving the
    /// registration in place.
    ///
    /// What a user produces by moving or deleting a checkout without
    /// `git worktree remove`, and the state `git worktree prune` exists for.
    pub fn orphan_worktree(self, name: &str) -> Self {
        let at = self.worktree_path(name);
        std::fs::remove_dir_all(&at).expect("removing a fixture worktree directory");
        self
    }

    fn worktree_path(&self, name: &str) -> PathBuf {
        self.worktrees
            .get(name)
            .unwrap_or_else(|| panic!("no fixture worktree named {name}"))
            .path()
            .join(name)
    }

    /// Stashes whatever is in the working directory, with Git's own message.
    ///
    /// Needs something to stash: `git stash push` with a clean worktree succeeds
    /// and creates nothing, which would make a test about entry `n` silently be
    /// about a different one.
    pub fn stash(self, file: &str, contents: &str) -> Self {
        std::fs::write(self.dir.path().join(file), contents).expect("writing a fixture file");
        self.git(["stash", "push", "--include-untracked"]);
        self
    }

    /// Stashes with a message the user would have typed.
    pub fn stash_named(self, file: &str, contents: &str, message: &str) -> Self {
        std::fs::write(self.dir.path().join(file), contents).expect("writing a fixture file");
        self.git(["stash", "push", "--include-untracked", "--message", message]);
        self
    }

    /// Adds a lightweight tag at `HEAD`.
    pub fn tag(self, name: &str) -> Self {
        self.git(["tag", name]);
        self
    }

    /// Adds an annotated tag at `HEAD`.
    pub fn annotated_tag(self, name: &str) -> Self {
        self.git(["tag", "--annotate", "--message", name, name]);
        self
    }

    /// Builds a synthetic history of `count` commits, fast.
    ///
    /// Runs `git fast-import` rather than `git commit` in a loop: 100,000
    /// commits take a couple of seconds instead of hours, which is what makes
    /// benchmarking against a repository of that size practical at all.
    ///
    /// Every `branch_every` commits the history forks and merges back, so the
    /// result exercises lane allocation rather than being one straight line.
    pub fn generate(mut self, count: usize, branch_every: usize) -> Self {
        use std::fmt::Write as _;

        assert!(count > 0, "a generated history needs at least one commit");
        let who = "hideGit Fixture <fixture@hidegit.invalid>";
        let mut stream = String::with_capacity(count * 160);

        // One blob, reused by every commit: the benchmark is about history
        // shape, not about content.
        stream.push_str("blob\nmark :1\ndata 9\ncontents\n\n");

        // Mark 1 is the blob, so commits number from 2.
        let mut main_tip: Option<usize> = None;
        let mut side_tip: Option<usize> = None;

        for (mark, i) in (2..).zip(0..count) {
            let time = self.clock + i as i64 * 60;
            let message = format!("commit {i}");

            // Fork a side branch, then merge it back on the next multiple.
            let forking = branch_every > 0 && i % branch_every == branch_every / 2;
            let merging = branch_every > 0 && i % branch_every == 0 && side_tip.is_some();

            let _ = writeln!(stream, "commit refs/heads/main\nmark :{mark}");
            let _ = writeln!(stream, "author {who} {time} +0000");
            let _ = writeln!(stream, "committer {who} {time} +0000");
            let _ = writeln!(stream, "data {}\n{message}", message.len());

            if let Some(parent) = main_tip {
                let _ = writeln!(stream, "from :{parent}");
            }
            if merging && let Some(side) = side_tip.take() {
                let _ = writeln!(stream, "merge :{side}");
            }
            let _ = writeln!(stream, "M 100644 :1 file{}.txt\n", i % 32);

            if forking {
                side_tip = main_tip;
            }
            main_tip = Some(mark);
        }

        stream.push_str("done\n");

        let output = GitCommand::new("fast-import")
            .arg("--done")
            .cwd(self.dir.path())
            .takes_locks()
            .run_with_stdin(Some(stream.as_bytes()));
        if let Err(e) = output {
            panic!("fixture fast-import failed: {e}");
        }

        self.clock += count as i64 * 60;
        self
    }

    // ---- dirty state -----------------------------------------------------
    //
    // Everything above commits, which is why nothing above can produce a
    // working directory to take the status of. The methods below deliberately
    // stop short of committing, and are the ones `status` is tested against.

    /// Writes a file without staging or committing it.
    ///
    /// The file becomes an unstaged modification if it is tracked, and an
    /// untracked file if it is not. Parent directories are created as needed.
    pub fn write(self, file: &str, contents: &str) -> Self {
        let path = self.dir.path().join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("creating a fixture directory");
        }
        std::fs::write(&path, contents).expect("writing a fixture file");
        self
    }

    /// Writes a file and stages it, without committing.
    pub fn stage(self, file: &str, contents: &str) -> Self {
        let this = self.write(file, contents);
        this.git(["add", "--", file]);
        this
    }

    /// Deletes a file from the working tree, leaving the deletion unstaged.
    pub fn delete(self, file: &str) -> Self {
        std::fs::remove_file(self.dir.path().join(file)).expect("removing a fixture file");
        self
    }

    /// Renames a tracked file and stages the rename.
    pub fn rename(self, from: &str, to: &str) -> Self {
        self.git(["mv", from, to]);
        self
    }

    /// Leaves the repository mid-merge with a conflicted path.
    ///
    /// `branch` must already exist and must have changed the same file as the
    /// current branch, so the merge cannot resolve it. The merge is expected to
    /// fail, so its exit status is deliberately not checked.
    pub fn conflict(self, branch: &str) -> Self {
        let date = format!("{} +0000", self.clock);
        let _ = GitCommand::new("--no-pager")
            .args(["merge", "--no-edit", branch])
            .cwd(self.dir.path())
            .takes_locks()
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_EDITOR", "true")
            .run();
        self
    }

    /// Finishes the repository.
    pub fn build(self) -> Repo {
        let remotes = self
            .remotes
            .iter()
            .map(|(name, dir)| (name.clone(), dir.path().to_path_buf()))
            .collect();

        let submodule_sources = self
            .submodules
            .iter()
            .map(|(path, dir)| (path.clone(), dir.path().to_path_buf()))
            .collect();

        let worktree_paths = self
            .worktrees
            .iter()
            .map(|(name, dir)| (name.clone(), dir.path().join(name)))
            .collect();

        Repo {
            path: self.dir.path().to_path_buf(),
            dir: self.dir,
            commits: self.commits,
            remote_paths: remotes,
            remote_dirs: self.remotes.into_values().collect(),
            submodule_sources,
            submodule_dirs: self.submodules.into_values().collect(),
            worktree_paths,
            worktree_dirs: self.worktrees.into_values().collect(),
        }
    }
}

/// A built fixture. Deleting it removes the repository from disk.
#[derive(Debug)]
pub struct Repo {
    #[expect(dead_code, reason = "held solely to keep the directory alive")]
    dir: TempDir,
    path: PathBuf,
    commits: HashMap<String, ObjectId>,
    remote_paths: HashMap<String, PathBuf>,
    #[expect(dead_code, reason = "held solely to keep the remotes alive")]
    remote_dirs: Vec<TempDir>,
    submodule_sources: HashMap<String, PathBuf>,
    #[expect(dead_code, reason = "held solely to keep the submodule sources alive")]
    submodule_dirs: Vec<TempDir>,
    worktree_paths: HashMap<String, PathBuf>,
    #[expect(dead_code, reason = "held solely to keep the worktrees alive")]
    worktree_dirs: Vec<TempDir>,
}

impl Repo {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opens the repository through the production backend.
    pub fn backend(&self) -> HybridBackend {
        HybridBackend::open(&self.path).expect("a fixture is a valid repository")
    }

    /// The id of the commit created by the step with this name.
    pub fn id(&self, name: &str) -> ObjectId {
        *self
            .commits
            .get(name)
            .unwrap_or_else(|| panic!("no fixture commit named {name}"))
    }

    /// The bare repository standing in for a named remote.
    ///
    /// Tests assert against the far side directly through this — after a push,
    /// what matters is what the *remote* now has, not what hideGit's own reader
    /// says about it.
    pub fn remote_path(&self, name: &str) -> &Path {
        self.remote_paths
            .get(name)
            .unwrap_or_else(|| panic!("no fixture remote named {name}"))
    }

    /// The repository a submodule was cloned from, keyed by the path it was
    /// added at.
    pub fn submodule_source(&self, path: &str) -> &Path {
        self.submodule_sources
            .get(path)
            .unwrap_or_else(|| panic!("no fixture submodule at {path}"))
    }

    /// Where a linked worktree was checked out.
    pub fn worktree_path(&self, name: &str) -> &Path {
        self.worktree_paths
            .get(name)
            .unwrap_or_else(|| panic!("no fixture worktree named {name}"))
    }

    /// Runs `git` in a repository and returns its trimmed stdout, for asserting
    /// against real Git rather than against hideGit's own reader.
    ///
    /// Panics on failure, printing the command — a fixture assertion that cannot
    /// run is a broken test, not a test failure to interpret.
    pub fn git_in<const N: usize>(at: &Path, args: [&str; N]) -> String {
        match GitCommand::new("--no-pager")
            .args(args)
            .cwd(at)
            .takes_locks()
            .run()
        {
            Ok(output) => output.trimmed_stdout(),
            Err(e) => panic!("git {args:?} in {} failed: {e}", at.display()),
        }
    }

    /// [`Repo::git_in`], in this repository.
    pub fn git<const N: usize>(&self, args: [&str; N]) -> String {
        Self::git_in(&self.path, args)
    }
}
