//! Cloning a repository that is not open yet.
//!
//! Deliberately not a [`crate::GitBackend`] method: there is no repository for it
//! to be a method *on*. Keeping it out preserves what the trait means — the
//! things you can ask of an *open* repository — and keeps `open` the only way to
//! get a backend.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::{GitError, classify_remote_failure};
use crate::ops::{CancelToken, ProgressSink};
use crate::process::GitCommand;

/// Clones `url` into `into`, reporting progress and honouring cancellation.
///
/// Returns the path the working tree ended up at, which is what the caller opens.
///
/// `into` must not already exist. Checked here rather than left to `git`, because
/// "destination path already exists and is not an empty directory" is a message
/// about a path the user chose in a picker, and the answer is to pick another one.
pub fn clone_repository(
    url: &str,
    into: &Path,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<PathBuf, GitError> {
    if into.exists() {
        return Err(GitError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", into.display()),
        )));
    }

    // The parent has to exist for `git` to write into it; the picker gives a
    // directory the user chose, so this only creates the repository's own folder.
    if let Some(parent) = into.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    // A URL and a destination are both operands, and a URL beginning with `-` is
    // exactly the kind of thing `--` exists to stop being read as a flag. No
    // `cwd` is set: both paths are absolute and the process should not inherit a
    // working directory that happens to be inside another repository.
    let command = GitCommand::new("clone")
        .arg("--progress")
        .operands([OsStr::new(url), into.as_os_str()]);

    command
        .run_streaming(progress, cancel)
        .map_err(|error| classify_remote_failure(url, error))?;

    Ok(into.to_path_buf())
}

#[cfg(all(test, feature = "fixture"))]
mod tests {
    use super::*;
    use crate::ops::NoProgress;

    #[test]
    fn a_destination_that_exists_is_refused_before_git_is_spawned() {
        let occupied = tempfile::tempdir().expect("a temporary directory");

        let error = clone_repository(
            "https://example.invalid/repo.git",
            occupied.path(),
            &NoProgress,
            &CancelToken::new(),
        )
        .expect_err("an existing destination must be refused");

        match error {
            GitError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists),
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn a_clone_cancelled_before_it_starts_never_touches_the_network() {
        let parent = tempfile::tempdir().expect("a temporary directory");
        let into = parent.path().join("repo");

        let cancel = CancelToken::new();
        cancel.cancel();

        // The URL is unreachable on purpose: if this returned anything other than
        // `Cancelled`, the pre-spawn check is not happening.
        let error = clone_repository(
            "https://example.invalid/repo.git",
            &into,
            &NoProgress,
            &cancel,
        )
        .expect_err("a cancelled clone must not run");

        assert!(matches!(error, GitError::Cancelled { .. }), "got {error:?}");
        assert!(!into.exists(), "nothing was created");
    }

    #[test]
    fn a_local_repository_clones_and_carries_its_history() {
        let source = crate::fixture::fixture().commit("A").commit("B").build();

        let parent = tempfile::tempdir().expect("a temporary directory");
        let into = parent.path().join("cloned");

        let cloned = clone_repository(
            &source.path().display().to_string(),
            &into,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("a local clone needs no credentials");

        assert_eq!(cloned, into);
        assert!(
            into.join(".git").is_dir(),
            "a working clone, not a bare one"
        );
        assert!(into.join("B.txt").is_file(), "the checkout happened");
    }
}
