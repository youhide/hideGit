//! Everything that can happen, as data.
//!
//! iced follows the Elm architecture, so every user intent and every completed
//! background operation arrives here as a `Message`. Nothing in `update`
//! blocks: a message returns a `Task`, the work happens off the UI thread, and
//! its result comes back as another message.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use hidegit_core::graph::Checkpoints;
use hidegit_core::model::{
    Commit, CommitDetail, Diff, Divergence, Head, ObjectId, Refs, Remote, RepoState, StashEntry,
    WorktreeStatus,
};
use hidegit_core::ops::{
    CheckoutTarget, FetchOutcome, ForceMode, ProgressUpdate, PullOutcome, PushOutcome, StartPoint,
    StashOp,
};
use hidegit_core::{GitBackend, GitError};
use hidegit_forge::{
    DeviceCode, ForgeError, GitHub, Identity, PullRequest, PullRequestDetail, RateBudget,
};

use crate::state::{ActionSheet, Pane, Prompt, Selection, StagingRow};

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

/// A forge failure, in the shape the UI shows it.
///
/// Kept separate from the `GitError` conversion because a forge failure is
/// nearly always recoverable and has a next action attached — sign in, install
/// the app, wait for the budget — where a `git` failure mostly wants Git's own
/// words shown verbatim.
impl From<ForgeError> for UiError {
    fn from(error: ForgeError) -> Self {
        Self {
            summary: error.to_string(),
            details: format!("{error:?}"),
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
    pub stashes: Vec<StashEntry>,
    pub remotes: Vec<Remote>,
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
    /// The confirmation dialog was accepted, dismissed, or never raised.
    ConfirmationAccepted,
    ConfirmationDismissed,
    /// Raises the list of things that can be done to one item.
    SheetRequested(Box<ActionSheet>),
    /// An item was chosen: the sheet closes and its message is dispatched.
    ///
    /// Wrapped rather than dispatched directly so closing the sheet happens in
    /// one place. Without it, choosing an action leaves the sheet sitting over
    /// whatever the action produced — including a toast reporting that it failed.
    SheetChosen(Box<Message>),
    SheetDismissed,
    /// Raises a modal that collects text before acting.
    PromptRequested(Box<Prompt>),
    /// A prompt field changed, by index.
    PromptChanged(usize, String),
    /// `Enter`, or the prompt's own button. `update` turns the prompt's kind and
    /// its current values into the message that does the work.
    PromptAccepted,
    PromptDismissed,
    OpenRepository(PathBuf),
    RepositoryOpened(Box<Result<OpenedRepository, UiError>>),
    /// A URL to clone. The destination is asked for next, with the platform's own
    /// picker — pointing at a folder beats typing a path.
    CloneRequested(String),
    /// The URL and the folder the user picked. `None` means they cancelled.
    CloneDestinationPicked(String, Option<PathBuf>),
    /// Progress from the clone in flight.
    CloneProgress(ProgressUpdate),
    /// The clone ended. On success its path is opened.
    CloneFinished(Box<Result<PathBuf, UiError>>),
    /// The Cancel button on the clone banner.
    CloneCancelled,
    CloseRepository(usize),
    Repo(usize, RepoMessage),
    ToastDismissed(u64),

    // ---- the forge ----
    /// The client exists, and a stored session was restored if there was one.
    ///
    /// `Ok(None)` is a first run, not a failure. The client arrives with it
    /// rather than being built on the UI thread, because building it reads the
    /// keychain.
    ForgeClientBuilt(Arc<GitHub>, Box<Result<Option<Identity>, UiError>>),
    /// The Connect row: offers the device flow or a personal access token.
    ConnectRequested,
    /// Start the device flow.
    DeviceFlowRequested,
    /// GitHub issued a code. Raised from inside the flow, which keeps polling
    /// afterwards — so this arrives *while* `ForgeConnected` is still pending,
    /// which is the whole reason it is a message of its own.
    DeviceCodeIssued(Box<DeviceCode>),
    /// `Esc`, or the dialog's own button.
    ///
    /// Does **not** cancel the flow: hideGit keeps polling and the token still
    /// arrives, so the dialog says so rather than offering a Cancel that would
    /// be a lie.
    DeviceCodeDismissed,
    /// The pasted token from the prompt.
    TokenSubmitted(String),
    ForgeConnected(Box<Result<Identity, UiError>>),
    /// Asks to sign out, which confirms rather than acting: signing back in
    /// costs a round trip through a browser.
    DisconnectRequested,
    /// The confirmation was accepted. Only ever sent by the dialog.
    DisconnectConfirmed,
    ForgeSignedOut(Box<Result<(), UiError>>),
    /// Hand a URL to the platform's browser.
    OpenUrl(String),
    /// It did not open. Reported, because a control that looks like it worked
    /// and did not is worse than one that says it could not.
    OpenUrlFailed(Box<UiError>),
    /// The window gained or lost focus, which is what decides how often
    /// hideGit polls. A minimised window asking every minute is exactly what
    /// the interval table exists to prevent.
    WindowFocused(bool),
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
    /// `Space`, or the button on a row: stage what is not staged, unstage what
    /// is. Which one it means is decided from the row's section.
    StageRequested(Vec<PathBuf>),
    UnstageRequested(Vec<PathBuf>),
    /// `Cmd+Backspace`: discard whatever row is selected. Confirms, like every
    /// other route to discarding does.
    DiscardSelectedRequested,
    /// A click on a changed line in the staging view's diff, by hunk and line
    /// index. Toggles whether it is part of the next patch.
    LineToggled(usize, usize),
    /// `[Stage hunk]`, by hunk index.
    HunkStageRequested(usize),
    /// `[Stage file]` — every hunk, the same code path with everything picked.
    FileStageRequested,
    /// Acts on exactly the lines picked out so far.
    SelectedLinesStageRequested,
    // ---- the commit composer ----
    SubjectChanged(String),
    BodyChanged(String),
    AmendToggled(bool),
    SignOffToggled(bool),
    /// A text field gained or lost focus. Bare-letter shortcuts are suppressed
    /// while it holds it.
    EditingChanged(bool),
    /// `Cmd+Enter`, or the Commit button.
    CommitRequested,
    /// The commit landed; its id is read back rather than assumed, because a
    /// hook or a signature can change what was actually recorded.
    Committed(Box<Result<ObjectId, UiError>>),
    /// Asks to discard, which raises a confirmation rather than acting.
    DiscardRequested(Vec<PathBuf>),
    /// The confirmation was accepted. Only ever sent by the dialog.
    DiscardConfirmed(Vec<PathBuf>),
    /// The graph canvas learned how tall it is, in rows.
    ViewportChanged(usize),

    // ---- branches ----
    /// Switch to a branch, a commit, or a new branch. Fails rather than
    /// discarding anything when local changes are in the way.
    CheckoutRequested(CheckoutTarget),
    /// Create a branch without switching to it.
    BranchCreateRequested {
        name: String,
        from: StartPoint,
    },
    BranchRenameRequested {
        from: String,
        to: String,
    },
    /// Asks to delete, which confirms rather than acting.
    BranchDeleteRequested {
        name: String,
    },
    /// The confirmation was accepted. `force` is only ever true because the user
    /// chose it after the safe form was refused.
    BranchDeleteConfirmed {
        name: String,
        force: bool,
    },

    // ---- remotes ----
    /// `Cmd+Shift+F`, or the toolbar. Every remote, pruning as it goes.
    FetchRequested,
    /// `Cmd+Shift+P`, or the toolbar. Uses the branch's own upstream, and the
    /// user's own `pull.rebase` decides how it integrates.
    PullRequested,
    /// `Cmd+Shift+U`, or the toolbar.
    PushRequested {
        force: ForceMode,
    },
    /// A force push was confirmed. Only ever sent by the dialog.
    PushConfirmed {
        force: ForceMode,
    },
    /// A report from the operation in flight, tagged with which one.
    ///
    /// Tagged because a cancelled operation's last report can arrive after the one
    /// that replaced it has started, and it must not redraw that banner.
    OperationProgress(u64, ProgressUpdate),
    /// The operation ended, tagged with which one.
    OperationFinished(u64, Box<Result<OperationOutcome, UiError>>),
    /// The Cancel button on the progress banner.
    OperationCancelled,

    // ---- the stash ----
    /// Stash, apply, pop. Dropping goes through a confirmation first.
    StashRequested(StashOp),
    /// Asks to drop, which confirms rather than acting.
    StashDropRequested(usize),
    /// The confirmation was accepted. Only ever sent by the dialog.
    StashDropConfirmed(usize),

    // ---- remotes and tags ----
    RemoteAddRequested {
        name: String,
        url: String,
    },
    RemoteUrlChangeRequested {
        name: String,
        url: String,
    },
    /// Asks to remove, which confirms rather than acting.
    RemoteRemoveRequested(String),
    /// The confirmation was accepted. Only ever sent by the dialog.
    RemoteRemoveConfirmed(String),
    TagCreateRequested {
        name: String,
        at: StartPoint,
        /// `Some` makes it annotated.
        message: Option<String>,
    },
    /// Asks to delete, which confirms rather than acting.
    TagDeleteRequested(String),
    /// The confirmation was accepted. Only ever sent by the dialog.
    TagDeleteConfirmed(String),
    /// Pushes one tag to a remote, through the ordinary push path.
    TagPushRequested {
        remote: String,
        name: String,
    },

    // ---- async results ----
    CommitsLoaded(Box<Result<Page, UiError>>),
    /// The O(n) pass that makes scrolling to an arbitrary row cheap.
    CheckpointsBuilt(Checkpoints),
    DetailLoaded(Box<Result<CommitLoad, UiError>>),
    /// Both halves of the working-directory diff, loaded together because the
    /// staging view shows both lists at once.
    StatusLoaded(Box<Result<StatusLoad, UiError>>),
    /// A write finished. Carries only its failure: on success the refresh that
    /// follows is the whole result, and a toast per click would be noise.
    WriteFinished(Box<Result<(), UiError>>),
    /// Ahead/behind, loaded separately from a refresh because it costs a commit
    /// walk per tracking branch and a refresh runs on every file save.
    DivergenceLoaded(Box<Result<HashMap<String, Divergence>, UiError>>),
    /// Something changed the repository: reload refs, state and history.
    ///
    /// One code path for "something changed", rather than each operation
    /// remembering which views it invalidated.
    RepositoryChanged,
    /// The reread that `RepositoryChanged` asked for, applied in place.
    Refreshed(Box<Result<Refreshed, UiError>>),

    // ---- pull requests ----
    /// Ask the forge for this repository's open pull requests.
    PrsRefreshRequested,
    PrsLoaded(Box<Result<PrsLoad, UiError>>),
    /// A pull request of yours that is no longer open, loaded to find out
    /// *how* it ended.
    ///
    /// A poll asks only for open ones, so an ending arrives as an absence and
    /// an absence cannot say whether it was merged or closed. Those are
    /// different events, so each disappearance costs one more request — a
    /// handful a day, not one per poll.
    PrEndingLoaded(Box<Result<PullRequestDetail, UiError>>),
    /// A pull request row. Loads its detail into the detail pane.
    PrDetailLoaded(Box<Result<PullRequestDetail, UiError>>),
    /// Open one in the browser — the trait is narrow, and everything past
    /// reading a pull request is the forge's own website's job.
    PrOpenRequested(u64),
    /// Open a pull request from the current branch into `base`.
    PrCreateRequested {
        head: String,
        base: String,
        title: String,
        body: String,
    },
    /// It opened. The number is selected so the next poll has somewhere to put
    /// its state, and the browser is *not* opened on the user's behalf — that
    /// is an action, and it belongs to the row's own control.
    PrCreated(Box<Result<PullRequest, UiError>>),
}

/// How a network operation ended.
///
/// One message for all three, because the banner, the refresh and the error path
/// are the same for each; only what gets *reported* on success differs, and mostly
/// nothing is — the refresh that follows is the result.
#[derive(Debug, Clone)]
pub enum OperationOutcome {
    Fetched(FetchOutcome),
    Pulled(PullOutcome),
    Pushed(PushOutcome),
}

/// A commit and its diff, loaded together because the detail pane shows both.
#[derive(Debug, Clone)]
pub struct CommitLoad {
    pub id: ObjectId,
    pub detail: CommitDetail,
    pub diff: Diff,
}

/// Everything a refresh rereads.
///
/// Deliberately not `OpenedRepository`: opening *creates* a repository entry,
/// refreshing *updates* one. Conflating them is how a write ends up appending a
/// second copy of the repository and throwing away the user's scroll position.
#[derive(Debug, Clone)]
pub struct Refreshed {
    pub head: Head,
    pub refs: Refs,
    pub state: RepoState,
    pub status: WorktreeStatus,
    pub stashes: Vec<StashEntry>,
    pub remotes: Vec<Remote>,
    pub total: usize,
    pub first_page: Vec<Commit>,
}

/// What a poll produced.
///
/// `NotInstalled` is a variant rather than an error because it is not one: the
/// token is good, the request succeeded, and the answer is that hideGit cannot
/// see this repository. It persists until somebody installs the app, it is
/// about this repository rather than about the session, and it carries the URL
/// that fixes it — none of which a toast can express.
#[derive(Debug, Clone)]
pub enum PrsLoad {
    Loaded {
        items: Vec<PullRequest>,
        /// What is left of the API budget, which is what the scheduler widens
        /// its interval on. It rides on the result rather than being asked for
        /// separately — see ADR-0006.
        budget: RateBudget,
    },
    NotInstalled {
        install_url: String,
    },
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
