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
        };

        this.git(["init", "-b", "main"]);
        // Fixtures must not depend on the developer's global Git config, and
        // must not pick up a repository-level identity from a parent checkout.
        this.git(["config", "user.name", "hideGit Fixture"]);
        this.git(["config", "user.email", "fixture@hidegit.invalid"]);
        this.git(["config", "commit.gpgsign", "false"]);

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

    /// Adds a commit that changes an existing file, for diff tests.
    pub fn edit(self, file: &str, contents: &str, message: &str) -> Self {
        std::fs::write(self.dir.path().join(file), contents).expect("writing a fixture file");
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
    pub fn remote_ref(self, name: &str) -> Self {
        let head = self.git(["rev-parse", "HEAD"]);
        self.git(["update-ref", name, &head]);
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

    /// Finishes the repository.
    pub fn build(self) -> Repo {
        Repo {
            path: self.dir.path().to_path_buf(),
            dir: self.dir,
            commits: self.commits,
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
}
