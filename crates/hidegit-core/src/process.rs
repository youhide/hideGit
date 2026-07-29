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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{GitError, Version};

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
        let argv = self.argv();
        tracing::debug!(argv = ?argv, cwd = ?self.cwd, "running git");

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

        let output = match command.output() {
            Ok(output) => output,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(GitError::GitNotFound);
            }
            Err(e) => return Err(GitError::Io(e)),
        };

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
