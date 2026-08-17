//! The one place hideGit invokes the system `git` binary.
//!
//! This is the most security-sensitive boundary in the application, because
//! branch names, paths and remote URLs come from repositories that may have
//! been cloned from anywhere. Every invocation goes through [`GitCommand`],
//! which enforces the invariants in `docs/ARCHITECTURE.md#shelling-out-safely`:
//!
//! - arguments are passed as a vector and **no shell is ever spawned**, so a
//!   `;` or `$(…)` in a branch name is data and never code;
//! - `--` precedes operands, so a ref or path starting with `-` cannot be
//!   absorbed as a flag;
//! - the environment is an allowlist rather than the user's shell wholesale;
//! - `stderr` is surfaced verbatim on failure, because Git's error messages
//!   are good and paraphrasing them destroys information;
//! - every invocation is logged at `debug` with its full argument vector.

use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::{GitError, Version};
use crate::ops::{CancelToken, ProgressSink, ProgressUpdate};

/// The oldest `git` hideGit runs against.
///
/// 2.30 is where the porcelain formats hideGit parses are stable and where
/// every platform's packaged Git had caught up.
pub const MINIMUM_GIT_VERSION: Version = Version::new(2, 30, 0);

/// Environment variables passed through to `git`.
///
/// The environment is rebuilt rather than inherited so a stray `GIT_DIR`,
/// `GIT_INDEX_FILE` or alias configuration in the user's shell cannot redirect
/// an invocation somewhere unexpected. Anything a credential helper or an SSH
/// agent genuinely needs is added here deliberately, not by default.
const INHERITED_VARS: &[&str] = &[
    // Locating the binary and the user's own configuration.
    "PATH",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    // Windows needs these to start a process at all.
    "SYSTEMROOT",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    // Temporary files.
    "TMPDIR",
    "TMP",
    "TEMP",
    // Credential and SSH plumbing, needed from M3 onward.
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "DISPLAY",
    "XDG_CONFIG_HOME",
    "XDG_RUNTIME_DIR",
];

/// A `git` invocation, built argument by argument.
#[derive(Debug, Clone)]
pub struct GitCommand {
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    /// Whether this command may take `index.lock`.
    ///
    /// Read-adjacent commands set `GIT_OPTIONAL_LOCKS=0` so a background
    /// refresh never contends with something the user started.
    takes_locks: bool,
    /// Variables set deliberately for this invocation, on top of the
    /// allowlist. Used for the `GIT_*` knobs a specific command needs.
    extra_env: Vec<(OsString, OsString)>,
}

impl GitCommand {
    /// Starts a command. `subcommand` is a fixed string in hideGit's own
    /// source, never anything derived from repository content.
    pub fn new(subcommand: &'static str) -> Self {
        Self {
            args: vec![OsString::from(subcommand)],
            cwd: None,
            takes_locks: false,
            extra_env: Vec::new(),
        }
    }

    /// Sets one environment variable for this invocation.
    ///
    /// The name is a fixed string in hideGit's own source. This is how a
    /// command asks for a specific `GIT_*` knob without widening the
    /// allowlist for every other invocation.
    pub fn env(mut self, name: &'static str, value: impl AsRef<OsStr>) -> Self {
        self.extra_env
            .push((OsString::from(name), value.as_ref().to_os_string()));
        self
    }

    /// Runs the command in `path`.
    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    /// Declares that this command writes and therefore needs the index lock.
    pub fn takes_locks(mut self) -> Self {
        self.takes_locks = true;
        self
    }

    /// Appends one argument. Passed to `exec` as a single element, so its
    /// contents are never interpreted.
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Appends several arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// Appends `--` followed by operands.
    ///
    /// Everything after `--` is a ref or a path, never a flag, which is what
    /// stops a branch named `--upload-pack=…` from becoming one.
    pub fn operands<I, S>(mut self, operands: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.push(OsString::from("--"));
        self.args
            .extend(operands.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// Appends `--end-of-options` followed by revisions.
    ///
    /// The counterpart to [`GitCommand::operands`] for commands that take
    /// revisions and no paths. `--` cannot be used there: to `git reset`,
    /// `git rev-parse` and `git rev-list` it means *paths follow*, so
    /// `git reset --hard -- HEAD~1` is a request to reset the path `HEAD~1`
    /// and fails with "Cannot do hard reset with paths". `--end-of-options`
    /// stops flag parsing without claiming what comes next is a path, which is
    /// the guarantee actually wanted: a revision that begins with a dash stays
    /// a revision and never becomes a flag.
    ///
    /// Available since Git 2.24, comfortably under the 2.30 hideGit requires.
    pub fn revisions<I, S>(mut self, revisions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.push(OsString::from("--end-of-options"));
        self.args
            .extend(revisions.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// The argument vector, lossily stringified for logs and error reports.
    pub fn argv(&self) -> Vec<String> {
        self.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The environment this command runs with, as `(name, value)` pairs.
    fn environment(&self) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> = INHERITED_VARS
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|v| (OsString::from(name), v)))
            .collect();

        // A subprocess blocking on a hidden prompt is an app that appears to
        // hang, so prompting is off and a missing credential is an error.
        env.push(("GIT_TERMINAL_PROMPT".into(), "0".into()));
        // Git's output must not shift under the user's locale.
        env.push(("LC_ALL".into(), "C".into()));
        env.push(("LANG".into(), "C".into()));
        if !self.takes_locks {
            env.push(("GIT_OPTIONAL_LOCKS".into(), "0".into()));
        }

        env.extend(self.extra_env.iter().cloned());

        env
    }

    /// Runs the command to completion.
    ///
    /// Returns [`GitError::Command`] with Git's own stderr on a non-zero exit,
    /// and [`GitError::GitNotFound`] when there is no `git` on `PATH`.
    pub fn run(&self) -> Result<GitOutput, GitError> {
        self.run_with_stdin(None)
    }

    /// Runs the command, feeding `input` to its standard input.
    ///
    /// How content reaches Git without ever touching a shell or a temporary
    /// file: patches go to `git apply --cached` this way, which is what makes
    /// hunk- and line-level staging one code path.
    pub fn run_with_stdin(&self, input: Option<&[u8]>) -> Result<GitOutput, GitError> {
        use std::io::Write as _;

        let argv = self.argv();
        tracing::debug!(argv = ?argv, cwd = ?self.cwd, "running git");

        let mut command = Command::new("git");
        command
            .args(&self.args)
            .env_clear()
            .envs(self.environment())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(GitError::GitNotFound);
            }
            Err(e) => return Err(GitError::Io(e)),
        };

        if let Some(input) = input {
            // Dropping the handle closes the pipe, which is what tells Git the
            // input is complete. Without it, a command that reads to EOF hangs.
            let mut stdin = child.stdin.take().expect("stdin was piped");
            stdin.write_all(input)?;
            drop(stdin);
        }

        let output = child.wait_with_output()?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if !output.status.success() {
            return Err(GitError::Command {
                argv,
                status: output.status.code(),
                stderr,
            });
        }

        Ok(GitOutput {
            stdout: output.stdout,
            stderr,
        })
    }
}

/// How long to wait for more stderr before checking the cancel flag again.
///
/// Short enough that Cancel feels immediate, long enough that a quiet fetch does
/// not spin a core. The read itself is what blocks, so this is only the ceiling
/// on how stale the cancellation check can be.
const CANCEL_POLL: Duration = Duration::from_millis(50);

impl GitCommand {
    /// Runs a long command, reporting progress and honouring cancellation.
    ///
    /// The difference from [`GitCommand::run`] is that stderr is read
    /// *incrementally* instead of at the end. Git writes `--progress` output
    /// there and rewrites each line in place with a bare carriage return, so a
    /// reader that waits for a newline sees nothing until the operation is over —
    /// which is exactly the report the user wanted while it was running.
    ///
    /// Everything is still accumulated verbatim, so a failure carries Git's own
    /// words the way `run` does. Cancellation kills the child and then checks for
    /// a leftover `index.lock`, which is **reported, never deleted**: whatever
    /// holds it may still be working.
    pub fn run_streaming(
        &self,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<GitOutput, GitError> {
        // Checked before spawning as well as during: a click on Cancel while the
        // task was still queued should not start a network operation at all.
        if cancel.is_cancelled() {
            return Err(self.cancelled());
        }

        let argv = self.argv();
        tracing::debug!(argv = ?argv, cwd = ?self.cwd, "running git with progress");

        let mut command = Command::new("git");
        command
            .args(&self.args)
            .env_clear()
            .envs(self.environment())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(GitError::GitNotFound);
            }
            Err(e) => return Err(GitError::Io(e)),
        };

        // stdout is drained on its own thread. A command whose output fills the
        // pipe buffer while this thread reads stderr would deadlock otherwise,
        // and `git push --porcelain` produces enough to matter.
        let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
        let stdout_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut buf);
            buf
        });

        // stderr is read on its own thread too, and chunks arrive over a channel.
        // Reading the pipe directly would block, and a fetch that stalls on the
        // network produces no output at all — so a Cancel during the stall, which
        // is exactly when a user reaches for it, would do nothing until the
        // network gave up. `recv_timeout` bounds the wait instead.
        let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
        let (chunks, incoming) = std::sync::mpsc::channel::<Vec<u8>>();
        let stderr_reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match stderr_pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if chunks.send(buf[..n].to_vec()).is_err() {
                            // The main thread gave up on us — it cancelled.
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        let mut stderr = String::new();
        let mut pending = String::new();

        loop {
            if cancel.is_cancelled() {
                // Killing the child closes both pipes, which is what lets the
                // reader threads finish rather than blocking for ever.
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_reader.join();
                let _ = stdout_reader.join();
                return Err(self.cancelled());
            }

            match incoming.recv_timeout(CANCEL_POLL) {
                Ok(chunk) => {
                    let chunk = String::from_utf8_lossy(&chunk);
                    stderr.push_str(&chunk);
                    pending.push_str(&chunk);
                    report_complete_lines(&mut pending, progress);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                // The sender is gone, so stderr is at end of file.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        // Whatever is left has no separator after it, which is how the final
        // `100% (n/n), done.` of a phase usually arrives.
        report_line(&pending, progress);
        let _ = stderr_reader.join();

        let status = child.wait()?;
        let stdout = stdout_reader
            .join()
            .unwrap_or_else(|_| panic!("the stdout reader thread panicked"));

        if !status.success() {
            // A killed child looks like a failure. Reporting it as one would
            // put a wall of stderr in front of a user who pressed Cancel.
            if cancel.is_cancelled() {
                return Err(self.cancelled());
            }
            return Err(GitError::Command {
                argv,
                status: status.code(),
                stderr,
            });
        }

        Ok(GitOutput { stdout, stderr })
    }

    /// The cancellation error, naming a lock the killed process left behind.
    fn cancelled(&self) -> GitError {
        // The lock lives in the git directory, and what this command was given
        // is a worktree — so it is discovered from there rather than assumed.
        let stale_lock = self
            .cwd
            .as_deref()
            .and_then(git_dir_of)
            .as_deref()
            .and_then(index_lock);

        GitError::Cancelled { stale_lock }
    }
}

/// Reports every progress line that is terminated, leaving any partial tail.
///
/// Git separates progress updates with a bare carriage return so each rewrites
/// the last, and ends a phase with a newline. Both count as terminators, which is
/// the whole reason a line-based reader is not enough here.
fn report_complete_lines(pending: &mut String, progress: &dyn ProgressSink) {
    while let Some(end) = pending.find(['\r', '\n']) {
        let line: String = pending.drain(..=end).collect();
        report_line(&line, progress);
    }
}

fn report_line(line: &str, progress: &dyn ProgressSink) {
    if let Some(update) = ProgressUpdate::parse(line) {
        progress.report(update);
    }
}

/// Finds the git directory for a worktree, for locating `index.lock`.
///
/// A plain `.git` join covers the ordinary case; a `.git` *file* means a worktree
/// or a submodule, where the real directory is named inside it.
fn git_dir_of(workdir: &Path) -> Option<PathBuf> {
    let dot_git = workdir.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    if dot_git.is_file() {
        let contents = std::fs::read_to_string(&dot_git).ok()?;
        let path = contents.strip_prefix("gitdir:")?.trim();
        return Some(workdir.join(path));
    }
    // A bare repository is its own git directory.
    workdir
        .join("HEAD")
        .is_file()
        .then(|| workdir.to_path_buf())
}

/// What a successful invocation produced.
#[derive(Debug, Clone)]
pub struct GitOutput {
    pub stdout: Vec<u8>,
    /// Git writes warnings and progress here even on success.
    pub stderr: String,
}

impl GitOutput {
    /// `stdout` as text, with trailing whitespace removed.
    pub fn trimmed_stdout(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim_end().to_owned()
    }

    /// `stdout` split on NUL, the way `-z` output is framed.
    ///
    /// Paths are NUL-separated rather than newline-separated because a newline
    /// is a legal character in a filename and a NUL is not.
    pub fn nul_separated(&self) -> impl Iterator<Item = &[u8]> {
        self.stdout
            .split(|b| *b == 0)
            .filter(|chunk| !chunk.is_empty())
    }
}

/// Checks that `git` is present on `PATH` and new enough.
///
/// Run once at startup: a missing `git` reported here is an actionable message,
/// whereas the same problem discovered on a first push is a mystery failure.
pub fn git_preflight() -> Result<Version, GitError> {
    let output = GitCommand::new("--version").run()?;
    let found = parse_version(&output.trimmed_stdout()).ok_or_else(|| GitError::Command {
        argv: vec!["--version".to_owned()],
        status: Some(0),
        stderr: format!(
            "could not parse a version out of {:?}",
            output.trimmed_stdout()
        ),
    })?;

    if found < MINIMUM_GIT_VERSION {
        return Err(GitError::GitTooOld {
            found,
            required: MINIMUM_GIT_VERSION,
        });
    }

    Ok(found)
}

/// Pulls a version out of `git --version` output.
///
/// Distributions decorate the line freely — `git version 2.39.5 (Apple
/// Git-154)`, `git version 2.45.1.windows.1` — so this takes the first three
/// dot-separated numbers and ignores everything after them.
fn parse_version(line: &str) -> Option<Version> {
    let rest = line.strip_prefix("git version ")?;
    let mut parts = rest
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u32>().ok());

    Some(Version {
        major: parts.next()??,
        minor: parts.next()??,
        // A two-component version is legal: `git version 2.30`.
        patch: parts.next().flatten().unwrap_or(0),
    })
}

/// Whether `git lfs` answers at all.
///
/// The question is only ever "is the tool there", never which version, because
/// hideGit does not drive it — Git does, through the clean and smudge filters a
/// repository's own `.gitattributes` configures. What this changes is what
/// hideGit can *tell* the user: an LFS repository opened without the tool
/// checks out pointer files everywhere, which looks like corruption and is not.
///
/// Deliberately not parsed. `git lfs version` prints `git-lfs/3.4.1 (GitHub;
/// …)` rather than anything resembling `git --version`, and hideGit has no
/// minimum to enforce, so reading a number out of it would be inventing a
/// requirement to go with it.
pub fn lfs_available() -> bool {
    GitCommand::new("lfs").arg("version").run().is_ok()
}

/// Whether a stale `index.lock` is sitting in `git_dir`.
///
/// Cancelling an operation kills the child process, and a killed `git` may
/// leave its lock behind. hideGit reports that rather than deleting it: the
/// lock may equally belong to a process that is still running.
pub fn index_lock(git_dir: &Path) -> Option<PathBuf> {
    let lock = git_dir.join("index.lock");
    lock.exists().then_some(lock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::NoProgress;

    #[test]
    fn arguments_are_never_concatenated_into_a_shell_string() {
        // A branch name that would be catastrophic if a shell ever saw it.
        let hostile = "main; rm -rf ~ #$(whoami)`id`";
        let cmd = GitCommand::new("log").arg("--oneline").operands([hostile]);

        assert_eq!(
            cmd.argv(),
            vec!["log", "--oneline", "--", hostile],
            "the hostile name must survive as exactly one argument"
        );
    }

    #[test]
    fn operands_are_separated_from_flags_by_a_double_dash() {
        // A ref that looks like a flag must land after `--`.
        let cmd = GitCommand::new("log").operands(["--upload-pack=evil"]);
        let argv = cmd.argv();

        let dash = argv.iter().position(|a| a == "--").expect("`--` present");
        assert!(
            argv[dash + 1..].contains(&"--upload-pack=evil".to_owned()),
            "the flag-shaped operand must sit after `--`"
        );
    }

    #[test]
    fn prompting_and_locale_are_pinned_on_every_invocation() {
        let env = GitCommand::new("status").environment();
        let get = |k: &str| {
            env.iter()
                .find(|(name, _)| name == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };

        assert_eq!(get("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
        assert_eq!(get("LC_ALL").as_deref(), Some("C"));
    }

    #[test]
    fn read_adjacent_commands_do_not_contend_for_the_index_lock() {
        let reading = GitCommand::new("status").environment();
        assert!(
            reading
                .iter()
                .any(|(k, v)| k == "GIT_OPTIONAL_LOCKS" && v == "0")
        );

        let writing = GitCommand::new("commit").takes_locks().environment();
        assert!(
            !writing.iter().any(|(k, _)| k == "GIT_OPTIONAL_LOCKS"),
            "a command that legitimately writes must be allowed to lock"
        );
    }

    #[test]
    fn the_environment_is_an_allowlist_not_an_inheritance() {
        // Cargo sets this in every test process, and it is not on the
        // allowlist — so if it reaches the child, the environment is being
        // inherited wholesale.
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());

        let env = GitCommand::new("status").environment();
        assert!(
            !env.iter().any(|(k, _)| k == "CARGO_MANIFEST_DIR"),
            "variables from the surrounding process must not reach git"
        );
        assert!(
            env.iter().any(|(k, _)| k == "PATH"),
            "PATH is on the allowlist because git has to be findable"
        );
    }

    #[test]
    fn version_parsing_survives_vendor_decoration() {
        assert_eq!(
            parse_version("git version 2.39.5 (Apple Git-154)"),
            Some(Version::new(2, 39, 5))
        );
        assert_eq!(
            parse_version("git version 2.45.1.windows.1"),
            Some(Version::new(2, 45, 1))
        );
        assert_eq!(
            parse_version("git version 2.30"),
            Some(Version::new(2, 30, 0))
        );
        assert_eq!(parse_version("not git output"), None);
    }

    #[test]
    fn versions_order_by_component_not_lexically() {
        assert!(Version::new(2, 9, 0) < Version::new(2, 30, 0));
        assert!(Version::new(2, 30, 0) >= MINIMUM_GIT_VERSION);
    }

    #[test]
    fn preflight_finds_the_git_this_suite_needs_anyway() {
        // The fixture builder shells out to git, so a missing binary would
        // fail everything else too. This asserts the actual check.
        let version = git_preflight().expect("git on PATH and new enough");
        assert!(version >= MINIMUM_GIT_VERSION);
    }

    #[test]
    fn a_streamed_command_that_reports_nothing_still_returns_its_output() {
        // `rev-parse --git-dir` writes no progress at all. The streaming path
        // must not need any in order to complete.
        let output = GitCommand::new("--version")
            .run_streaming(&NoProgress, &CancelToken::new())
            .expect("git --version succeeds");

        assert!(output.trimmed_stdout().starts_with("git version"));
    }

    #[test]
    fn a_streamed_command_cancelled_before_it_runs_is_never_spawned() {
        let cancel = CancelToken::new();
        cancel.cancel();

        // A subcommand that does not exist: if this were spawned, the error
        // would be `Command`, not `Cancelled`.
        let error = GitCommand::new("definitely-not-a-git-subcommand")
            .run_streaming(&NoProgress, &cancel)
            .expect_err("a cancelled command does not run");

        assert!(matches!(error, GitError::Cancelled { stale_lock: None }));
    }

    #[test]
    fn a_streamed_failure_carries_gits_own_stderr_like_an_ordinary_one() {
        let error = GitCommand::new("cat-file")
            .arg("-p")
            .arg("0000000000000000000000000000000000000000")
            .cwd(std::env::temp_dir())
            .run_streaming(&NoProgress, &CancelToken::new())
            .expect_err("bogus object must fail");

        match error {
            GitError::Command { argv, stderr, .. } => {
                assert_eq!(argv[0], "cat-file");
                assert!(!stderr.is_empty(), "git's message must be preserved");
            }
            other => panic!("expected a Command error, got {other:?}"),
        }
    }

    #[test]
    fn progress_lines_separated_by_carriage_returns_are_each_reported() {
        // The reason a line reader is not enough: git rewrites its progress line
        // in place with a bare CR, so a reader waiting for a newline would see
        // nothing until the operation was already over.
        struct Collect(std::sync::Mutex<Vec<(String, u64)>>);
        impl ProgressSink for Collect {
            fn report(&self, update: ProgressUpdate) {
                self.0.lock().unwrap().push((update.phase, update.done));
            }
        }

        let sink = Collect(std::sync::Mutex::new(Vec::new()));
        let mut pending = String::from(
            "Counting objects:  33% (1/3)\rCounting objects:  66% (2/3)\r\
             Counting objects: 100% (3/3), done.\nReceiving objects:  50% (1/2)\r",
        );
        report_complete_lines(&mut pending, &sink);

        assert_eq!(
            sink.0.into_inner().unwrap(),
            vec![
                ("Counting objects".to_owned(), 1),
                ("Counting objects".to_owned(), 2),
                ("Counting objects".to_owned(), 3),
                ("Receiving objects".to_owned(), 1),
            ]
        );
        assert!(pending.is_empty(), "every line here was terminated");
    }

    #[test]
    fn a_partial_progress_line_waits_for_its_terminator() {
        struct Count(std::sync::atomic::AtomicUsize);
        impl ProgressSink for Count {
            fn report(&self, _update: ProgressUpdate) {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let sink = Count(std::sync::atomic::AtomicUsize::new(0));
        // A chunk boundary can fall anywhere, including mid-number. Reporting
        // `(4/` as progress would show a nonsense count.
        let mut pending = String::from("Receiving objects:  40% (4/");
        report_complete_lines(&mut pending, &sink);

        assert_eq!(sink.0.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(
            pending, "Receiving objects:  40% (4/",
            "kept for the next read"
        );
    }

    #[test]
    fn the_git_directory_is_found_for_a_worktree_and_for_a_bare_repository() {
        let repo = tempfile::tempdir().expect("a temporary directory");
        GitCommand::new("init")
            .args(["-b", "main"])
            .operands([repo.path()])
            .takes_locks()
            .run()
            .expect("git init");
        assert_eq!(
            git_dir_of(repo.path()),
            Some(repo.path().join(".git")),
            "an ordinary worktree keeps its git directory beside it"
        );

        let bare = tempfile::tempdir().expect("a temporary directory");
        GitCommand::new("init")
            .args(["--bare", "-b", "main"])
            .operands([bare.path()])
            .takes_locks()
            .run()
            .expect("git init --bare");
        assert_eq!(
            git_dir_of(bare.path()),
            Some(bare.path().to_path_buf()),
            "a bare repository is its own git directory"
        );

        assert_eq!(
            git_dir_of(std::env::temp_dir().as_path()),
            None,
            "somewhere that is not a repository has no git directory"
        );
    }

    #[test]
    fn a_lock_left_by_a_cancelled_command_is_reported_and_never_deleted() {
        let repo = tempfile::tempdir().expect("a temporary directory");
        GitCommand::new("init")
            .args(["-b", "main"])
            .operands([repo.path()])
            .takes_locks()
            .run()
            .expect("git init");

        // Stands in for the lock a killed `git` leaves behind.
        let lock = repo.path().join(".git").join("index.lock");
        std::fs::write(&lock, b"").expect("a writable git directory");

        let cancel = CancelToken::new();
        cancel.cancel();
        let error = GitCommand::new("status")
            .cwd(repo.path())
            .run_streaming(&NoProgress, &cancel)
            .expect_err("cancelled");

        match error {
            GitError::Cancelled {
                stale_lock: Some(reported),
            } => assert_eq!(reported, lock),
            other => panic!("expected a reported stale lock, got {other:?}"),
        }
        assert!(
            lock.exists(),
            "the lock is named, never removed — it may belong to a live process"
        );
    }

    #[test]
    fn a_failing_command_carries_its_argv_and_gits_own_stderr() {
        let err = GitCommand::new("cat-file")
            .arg("-p")
            .arg("0000000000000000000000000000000000000000")
            .cwd(std::env::temp_dir())
            .run()
            .expect_err("bogus object must fail");

        match err {
            GitError::Command { argv, stderr, .. } => {
                assert_eq!(argv[0], "cat-file");
                assert!(!stderr.is_empty(), "git's message must be preserved");
            }
            other => panic!("expected a Command error, got {other:?}"),
        }
    }
}
