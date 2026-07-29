//! Everything that can happen, as data.
//!
//! iced follows the Elm architecture, so every user intent and every completed
//! background operation arrives here as a `Message`. Nothing in `update`
//! blocks: a message returns a `Task`, the work happens off the UI thread, and
//! its result comes back as another message.

use std::path::PathBuf;
use std::sync::Arc;

use hidegit_core::graph::Checkpoints;
use hidegit_core::model::{
    Commit, CommitDetail, Diff, Head, ObjectId, Refs, RepoState, WorktreeStatus,
};
use hidegit_core::{GitBackend, GitError};

use crate::state::{Pane, Selection, StagingRow};

/// A failure, in the shape the UI shows it.
///
/// `GitError` is deliberately not `Clone` — it carries an `io::Error` — while
/// iced messages must be. The conversion happens once, at the task boundary,
/// and keeps Git's own words: `details` is what a "copy details" action puts on
/// the clipboard, and it contains the argument vector and raw stderr rather
/// than a paraphrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiError {
    pub summary: String,
    pub details: String,
}

impl From<GitError> for UiError {
    fn from(error: GitError) -> Self {
        let details = match &error {
            GitError::Command {
                argv,
                status,
                stderr,
            } => format!(
                "git {}\nexit status: {}\n\n{stderr}",
                argv.join(" "),
                status.map_or_else(|| "killed by a signal".to_owned(), |s| s.to_string()),
            ),
            other => format!("{other:?}"),
        };

        Self {
            summary: error.to_string(),
            details,
        }
    }
}

/// A repository, opened and read far enough to render its first screen.
#[derive(Debug, Clone)]
pub struct OpenedRepository {
    pub path: PathBuf,
    pub backend: Arc<dyn GitBackend>,
    pub head: Head,
    pub refs: Refs,
    pub state: RepoState,
    pub status: WorktreeStatus,
    pub total: usize,
    pub first_page: Vec<Commit>,
}

/// One page of history, and whether more follows.
#[derive(Debug, Clone)]
pub struct Page {
    pub commits: Vec<Commit>,
    pub more: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// `Cmd+O`: ask for a repository with the platform's own picker.
    OpenDialogRequested,
    OpenRepository(PathBuf),
    RepositoryOpened(Box<Result<OpenedRepository, UiError>>),
    CloseRepository(usize),
    Repo(usize, RepoMessage),
    ToastDismissed(u64),
}

#[derive(Debug, Clone)]
pub enum RepoMessage {
    // ---- user intent ----
    Selected(Selection),
    /// A wheel or trackpad scroll, in pixels. Positive scrolls toward older
    /// history.
    GraphScrolled(f32),
    /// Arrow keys: move the selection by rows, scrolling to keep it visible.
    SelectionMoved(i32),
    /// `Tab` / `Shift+Tab`: sidebar → graph → detail.
    FocusCycled(Pane),
    /// `Cmd+D`: unified ⇄ side-by-side.
    DiffModeToggled,
    /// `J` / `K`: next or previous hunk.
    HunkStepped(i32),
    FileSelected(usize),
    /// A row in the staging view: which list it came from, and where in it.
    StagingRowSelected(StagingRow),
    /// The graph canvas learned how tall it is, in rows.
    ViewportChanged(usize),

    // ---- async results ----
    CommitsLoaded(Box<Result<Page, UiError>>),
    /// The O(n) pass that makes scrolling to an arbitrary row cheap.
    CheckpointsBuilt(Checkpoints),
    DetailLoaded(Box<Result<CommitLoad, UiError>>),
    /// Both halves of the working-directory diff, loaded together because the
    /// staging view shows both lists at once.
    StatusLoaded(Box<Result<StatusLoad, UiError>>),
    /// Something changed the repository: reload refs, state and history.
    ///
    /// One code path for "something changed", rather than each operation
    /// remembering which views it invalidated.
    RepositoryChanged,
}

/// A commit and its diff, loaded together because the detail pane shows both.
#[derive(Debug, Clone)]
pub struct CommitLoad {
    pub id: ObjectId,
    pub detail: CommitDetail,
    pub diff: Diff,
}

/// The working directory, and both of its diffs.
///
/// One unit of work rather than three messages: the staging view is not usable
/// until all of it has arrived, and showing the lists before the diffs would
/// flash an empty pane next to a populated one.
#[derive(Debug, Clone)]
pub struct StatusLoad {
    pub status: WorktreeStatus,
    pub staged: Diff,
    pub unstaged: Diff,
}
