//! Watching a repository for changes made outside hideGit.
//!
//! The raw watcher lives here rather than in `hidegit-ui` because it is
//! repository knowledge — which paths matter and which are noise — and this
//! crate is where that belongs. It deliberately stops at "something changed":
//! turning that into a `Subscription` needs `iced`, which this crate must never
//! depend on, so `hidegit-ui` wraps it.
//!
//! Two things make this harder than watching a directory.
//!
//! **`.git` is mostly noise.** Every `git` command rewrites lock files, object
//! files and logs; forwarding those would refresh the UI hundreds of times
//! during a single fetch. Only the handful of paths that describe *state* are
//! interesting.
//!
//! **One save is several events.** Editors write, rename and chmod; a build
//! touches thousands of files. Events are debounced so a burst becomes one
//! refresh.

use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use notify_debouncer_full::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

use crate::error::GitError;

/// How long the filesystem has to go quiet before a change is reported.
///
/// Long enough to collapse an editor's write-rename-chmod into one event and to
/// ride out a `git` command's churn; short enough that saving a file feels like
/// it updates immediately.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Files inside `.git` that describe repository state rather than its contents.
///
/// A change to any of these means the answer to "what is staged" or "what is
/// this repository in the middle of" may have changed. Everything else under
/// `.git` — objects, logs, lock files — is churn.
const INTERESTING: &[&str] = &[
    "index",
    "HEAD",
    "ORIG_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "BISECT_LOG",
    "MERGE_MSG",
];

/// What changed, and therefore what has gone stale.
///
/// The distinction earns its keep: reading the working directory is cheap and
/// reading history is not. Ordering a full topological walk because somebody
/// saved a file costs about a second on a hundred-thousand-commit repository,
/// and a file save cannot move a ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// Files in the working tree. `status` and the diffs are stale; the history
    /// behind them cannot have moved, so a memoised walk is still correct.
    Worktree,
    /// Something under `.git` that describes state: a ref moved, the index was
    /// rewritten, an operation started or finished. Anything may be stale.
    ///
    /// Ordered above [`Change::Worktree`] so a burst containing both is
    /// reported as this one.
    Repository,
}

/// A live watch on a repository.
///
/// Dropping it stops the watch. Changes are collected into a channel rather
/// than delivered by callback, so the consumer decides when to look — which is
/// what lets a UI poll it from its own event loop without a lock.
#[derive(Debug)]
pub struct Watch {
    #[expect(
        dead_code,
        reason = "held to keep the watch alive; dropping it stops it"
    )]
    debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    changes: Receiver<Change>,
}

impl Watch {
    /// Starts watching the working tree and the repository's state files.
    pub fn start(workdir: &Path, git_dir: &Path) -> Result<Self, GitError> {
        let (tx, rx) = channel();
        let git_dir = git_dir.to_path_buf();

        let mut debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                // Not worth interrupting the user over: the worst case is a
                // stale view they can refresh by acting. It is worth *logging*,
                // though — this is the only signal that automatic refresh has
                // stopped, and without it a repository that quietly went stale
                // is undiagnosable. `Watch::start` failing already says so.
                Err(errors) => {
                    for error in errors {
                        tracing::warn!(%error, "the filesystem watcher reported an error");
                    }
                    return;
                }
            };
            // The strongest change in the burst, because a burst that touched a
            // ref *and* a file is a repository change however many file events
            // came with it.
            let strongest = events
                .iter()
                .flat_map(|event| event.paths.iter())
                .filter_map(|path| classify(path, &git_dir))
                .max();

            if let Some(change) = strongest {
                // A full channel means a refresh is already pending, which is
                // exactly as much as anyone needs to know.
                let _ = tx.send(change);
            }
        })
        .map_err(|e| GitError::gix("starting the filesystem watcher", e))?;

        debouncer
            .watch(workdir, RecursiveMode::Recursive)
            .map_err(|e| GitError::gix("watching the working tree", e))?;

        Ok(Self {
            debouncer,
            changes: rx,
        })
    }

    /// Takes every pending change, returning the strongest one seen.
    ///
    /// Draining rather than counting: several bursts still mean one refresh.
    /// The *kind* is kept, though, because it decides how big that refresh has
    /// to be — and a burst that included a repository change is a repository
    /// change even if a hundred file events arrived with it.
    pub fn drain(&self) -> Option<Change> {
        let mut strongest = None;
        loop {
            match self.changes.try_recv() {
                Ok(change) => strongest = strongest.max(Some(change)),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return strongest,
            }
        }
    }
}

/// What kind of refresh this path is worth, if any.
fn classify(path: &Path, git_dir: &Path) -> Option<Change> {
    let Ok(inside) = path.strip_prefix(git_dir) else {
        // Everything in the working tree is interesting: it is what `status`
        // reports on. It cannot move a ref, so history stays valid.
        return Some(Change::Worktree);
    };

    // A lock file is the *start* of a write, not the end of one. Refreshing on
    // it reads a repository mid-change and gets the answer wrong.
    if path.extension().is_some_and(|e| e == "lock") {
        return None;
    }

    let mut components = inside.components();
    let first = components.next()?;
    let name = first.as_os_str();

    // `refs/` and `packed-refs` change when a branch moves; the named state
    // files change when an operation starts or finishes.
    let interesting =
        name == "refs" || name == "packed-refs" || INTERESTING.iter().any(|known| name == *known);

    interesting.then_some(Change::Repository)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git_dir() -> PathBuf {
        PathBuf::from("/repo/.git")
    }

    #[test]
    fn a_file_in_the_working_tree_is_a_worktree_change() {
        // The distinction that matters: editing a file cannot move a ref, so
        // the memoised history walk behind it is still correct.
        assert_eq!(
            classify(Path::new("/repo/src/main.rs"), &git_dir()),
            Some(Change::Worktree)
        );
    }

    #[test]
    fn the_index_matters_but_the_objects_it_points_at_do_not() {
        assert_eq!(
            classify(Path::new("/repo/.git/index"), &git_dir()),
            Some(Change::Repository)
        );
        assert_eq!(
            classify(Path::new("/repo/.git/objects/ab/cdef1234"), &git_dir()),
            None
        );
        assert_eq!(
            classify(Path::new("/repo/.git/logs/HEAD"), &git_dir()),
            None
        );
    }

    #[test]
    fn a_lock_file_is_the_start_of_a_write_rather_than_the_end() {
        // Refreshing here reads the repository mid-change and gets the answer
        // wrong; the unlocked file that follows is the real signal.
        assert_eq!(
            classify(Path::new("/repo/.git/index.lock"), &git_dir()),
            None
        );
        assert_eq!(
            classify(Path::new("/repo/.git/refs/heads/main.lock"), &git_dir()),
            None
        );
    }

    #[test]
    fn the_files_that_say_what_is_in_progress_matter() {
        for name in ["MERGE_HEAD", "REBASE_HEAD", "CHERRY_PICK_HEAD", "HEAD"] {
            assert_eq!(
                classify(&git_dir().join(name), &git_dir()),
                Some(Change::Repository),
                "{name} decides whether the UI offers to commit"
            );
        }
    }

    #[test]
    fn a_branch_moving_matters() {
        assert_eq!(
            classify(Path::new("/repo/.git/refs/heads/feature"), &git_dir()),
            Some(Change::Repository)
        );
        assert_eq!(
            classify(Path::new("/repo/.git/packed-refs"), &git_dir()),
            Some(Change::Repository)
        );
    }

    #[test]
    fn a_repository_change_outranks_the_worktree_changes_it_arrives_with() {
        // A `git commit` from a terminal writes the index and moves a ref while
        // the editor that triggered it is still writing files. Reporting that
        // burst as a worktree change would leave the graph showing the commit
        // before it.
        assert!(Change::Repository > Change::Worktree);
        assert_eq!(
            [Change::Worktree, Change::Repository, Change::Worktree]
                .into_iter()
                .max(),
            Some(Change::Repository)
        );
    }
}
