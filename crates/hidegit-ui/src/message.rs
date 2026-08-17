//! Everything that can happen, as data.
//!
//! iced follows the Elm architecture, so every user intent and every completed
//! background operation arrives here as a `Message`. Nothing in `update`
//! blocks: a message returns a `Task`, the work happens off the UI thread, and
//! its result comes back as another message.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use hidegit_core::conflict::{ConflictFile, Resolution};
use hidegit_core::graph::Checkpoints;
use hidegit_core::model::{
    Commit, CommitDetail, Diff, Divergence, Head, ObjectId, Refs, Remote, RepoState, StashEntry,
    Submodule, WorktreeStatus,
};
use hidegit_core::ops::{
    BlameLine, CheckoutTarget, FetchOutcome, ForceMode, MergeOutcome, ProgressUpdate, PullOutcome,
    PushOutcome, RebaseAction, ResetMode, SearchResults, SequenceControl, SequenceOutcome,
    StartPoint, StashOp,
};
use hidegit_core::{GitBackend, GitError};
use hidegit_forge::{
    DeviceCode, ForgeError, GitHub, Identity, PullRequest, PullRequestDetail, RateBudget,
};

use iced::widget::text_editor;

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
        let details = match &error {
            // The one family where the `Debug` form says nothing a person can
            // act on: `DeviceFlow(Disabled)` on a clipboard is not a bug
            // report, it is a shrug.
            ForgeError::DeviceFlow(flow) => flow.next_step().to_owned(),
            // Not installed already carries the URL that fixes it, and the
            // panel offers it as a row — the detail is for a bug report, so it
            // names the repository rather than repeating the instruction.
            ForgeError::NotInstalled { repo, install_url } => {
                format!("{repo}\n{install_url}")
            }
            other => format!("{other:?}"),
        };

        Self {
            summary: error.to_string(),
            details,
        }
    }
}

/// Which end of the quiet-hours window an hour was chosen for.
///
/// Named rather than a bool, because "from" and "to" are not symmetrical to a
/// reader and `QuietHourChosen(true, 22)` says nothing about which is which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietBound {
    From,
    To,
}

/// Which alert switch a [`Message::AlertToggled`] flipped.
///
/// An enum rather than a closure or a field path, because messages have to be
/// `Clone` and the shell inspects them to know when to write settings back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertToggle {
    /// The master switch.
    Enabled,
    ReviewRequested,
    ReviewSubmitted,
    PrCommented,
    ChecksFailed,
    ChecksPassed,
    PrConflicting,
    PrMerged,
    PrClosed,
}

impl AlertToggle {
    /// Every switch, in the order the screen lists them.
    pub const ALL: [Self; 9] = [
        Self::Enabled,
        Self::ReviewRequested,
        Self::ReviewSubmitted,
        Self::PrCommented,
        Self::ChecksFailed,
        Self::ChecksPassed,
        Self::PrConflicting,
        Self::PrMerged,
        Self::PrClosed,
    ];

    /// The label the screen shows, in the second person: these are things that
    /// happen to *you*, which is why they are worth a notification at all.
    pub fn label(self) -> &'static str {
        match self {
            Self::Enabled => "Show desktop notifications",
            Self::ReviewRequested => "A review is requested from you",
            Self::ReviewSubmitted => "Someone reviews your pull request",
            Self::PrCommented => "Someone comments on your pull request",
            Self::ChecksFailed => "CI fails on your pull request",
            Self::ChecksPassed => "CI passes on your pull request",
            Self::PrConflicting => "Your pull request starts conflicting",
            Self::PrMerged => "Your pull request is merged",
            Self::PrClosed => "Your pull request is closed",
        }
    }

    pub fn get(self, prefs: &hidegit_forge::AlertPrefs) -> bool {
        match self {
            Self::Enabled => prefs.enabled,
            Self::ReviewRequested => prefs.events.review_requested,
            Self::ReviewSubmitted => prefs.events.review_submitted,
            Self::PrCommented => prefs.events.pr_commented,
            Self::ChecksFailed => prefs.events.checks_failed,
            Self::ChecksPassed => prefs.events.checks_passed,
            Self::PrConflicting => prefs.events.pr_conflicting,
            Self::PrMerged => prefs.events.pr_merged,
            Self::PrClosed => prefs.events.pr_closed,
        }
    }

    pub fn toggle(self, prefs: &mut hidegit_forge::AlertPrefs) {
        let slot = match self {
            Self::Enabled => &mut prefs.enabled,
            Self::ReviewRequested => &mut prefs.events.review_requested,
            Self::ReviewSubmitted => &mut prefs.events.review_submitted,
            Self::PrCommented => &mut prefs.events.pr_commented,
            Self::ChecksFailed => &mut prefs.events.checks_failed,
            Self::ChecksPassed => &mut prefs.events.checks_passed,
            Self::PrConflicting => &mut prefs.events.pr_conflicting,
            Self::PrMerged => &mut prefs.events.pr_merged,
            Self::PrClosed => &mut prefs.events.pr_closed,
        };
        *slot = !*slot;
    }
}

/// One file blamed, with everything the view needs to render it.
///
/// A named shape rather than a tuple: four fields in a message is where a
/// reader starts counting positions to work out which is which.
#[derive(Debug, Clone)]
pub struct BlameLoad {
    pub path: PathBuf,
    /// The revision blamed, which is not necessarily `HEAD`.
    pub at: ObjectId,
    pub lines: Vec<BlameLine>,
    /// Metadata for the distinct commits `lines` point at.
    pub commits: Vec<Commit>,
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
    pub submodules: Vec<Submodule>,
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
    /// `↑` / `↓` in a sheet.
    SheetStepped(i32),
    /// `Enter` in a sheet: choose the highlighted row, if one is highlighted.
    SheetAccepted,
    SheetDismissed,
    /// Raises a modal that collects text before acting.
    PromptRequested(Box<Prompt>),
    /// A prompt field changed, by index.
    PromptChanged(usize, String),
    /// `Enter`, or the prompt's own button. `update` turns the prompt's kind and
    /// its current values into the message that does the work.
    /// `Tab` in a prompt: move to the next field, wrapping.
    PromptFieldStepped,
    PromptAccepted,
    PromptDismissed,
    OpenRepository(PathBuf),
    RepositoryOpened(Box<Result<OpenedRepository, UiError>>),
    /// An open in flight reached a new step.
    OpeningProgress(u64, crate::state::OpenPhase),
    /// An open in flight is over, whichever way it went. Separate from
    /// `RepositoryOpened` so that message keeps its shape, and sent after it, so
    /// the banner clears once the repository is actually on screen.
    OpeningFinished(u64),
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
    /// `Cmd+1` … `Cmd+9`, or a click on a tab.
    RepositorySelected(usize),
    CloseRepository(usize),
    Repo(usize, RepoMessage),
    ToastDismissed(u64),
    /// Puts a failure's details on the clipboard.
    ///
    /// `UI_SPEC` has asked for this since M1 and it was never built, so the one
    /// thing a bug report needs — the argument vector, Git's own stderr, the
    /// forge's own message — could be read on screen and not taken anywhere.
    ToastCopied(u64),

    /// `Cmd+,`, or the toolbar.
    SettingsRequested,
    SettingsDismissed,
    /// A theme was picked. Applied immediately — a theme you have to restart to
    /// see is a theme you cannot choose between.
    ThemeChosen(String),
    /// One alert switch was flipped.
    AlertToggled(AlertToggle),
    /// Quiet hours were turned on or off.
    QuietHoursToggled,
    /// One end of the quiet-hours window was set to an hour.
    QuietHourChosen(QuietBound, u8),
    /// A repository was muted or unmuted, keyed as `owner/name`.
    /// A chord prefix was pressed, and the next key completes it.
    ChordStarted(char),
    /// The key after a chord prefix did not complete one.
    ChordCancelled,
    /// A chord completed. Clears the pending prefix and dispatches what it
    /// meant, so the chord and the shortcut run the same message.
    ChordResolved(Box<Message>),
    /// The command palette was asked for.
    PaletteRequested,
    /// …and dismissed.
    PaletteDismissed,
    /// The palette's query changed.
    PaletteQueryChanged(String),
    /// The palette selection moved by this many rows.
    PaletteStepped(i32),
    /// The selected command was chosen.
    PaletteAccepted,
    /// The keyboard shortcut reference was asked for.
    ShortcutsRequested,
    /// …and dismissed.
    ShortcutsDismissed,
    /// Panic reports were switched on or off.
    PanicReportsToggled,
    /// The update check was switched on or off.
    UpdateCheckToggled,
    /// The window should — or should no longer — reopen where it was left.
    RememberGeometryToggled,
    RepositoryMuteToggled(String),

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
    /// The scrollbar was dragged, as a fraction of the way through history.
    ///
    /// A fraction rather than a row, because that is what the thumb's position
    /// actually means: the widget knows where the pointer is and the state
    /// knows how many commits there are, and neither should have to learn the
    /// other's units.
    GraphScrolledTo(f32),
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
    /// `Space`: stage what is not staged, unstage what is.
    ///
    /// Does not act directly. iced keeps text-input focus inside the widget, so
    /// whether a field is being typed into has to be *asked* — the answer comes
    /// back as [`RepoMessage::StageToggleResolved`].
    StageToggleRequested,
    /// The focus query answered: `true` when no text input holds focus, so the
    /// key belongs to the file list rather than to something being typed.
    StageToggleResolved(bool),
    /// The button on a row: stage what is not staged, unstage what is. Which one
    /// it means is decided from the row's section.
    StageRequested(Vec<PathBuf>),
    UnstageRequested(Vec<PathBuf>),
    /// `Cmd+Backspace`: discard whatever row is selected. Confirms, like every
    /// other route to discarding does.
    DiscardSelectedRequested,
    /// The file filter above a commit's file list changed.
    FileFilterChanged(String),
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
    /// `Cmd+Shift+Enter`: commit, then push what was just committed.
    ///
    /// Two operations rather than one, so a commit that succeeds is kept even
    /// when the push that follows fails — which is the common case, since the
    /// push is the half that needs a network and a credential.
    CommitAndPushRequested,
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

    // ---- submodules ----
    /// Brings one submodule to the commit the superproject records.
    ///
    /// One path rather than all of them, because that is the granularity the
    /// sidebar row acts at and "update everything" is a decision the user has
    /// not been asked to make. `init` is carried rather than always set: it is
    /// what separates setting up a submodule that has no checkout from moving
    /// one that already has.
    SubmoduleUpdateRequested {
        path: PathBuf,
        init: bool,
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

    /// `Cmd+F`: open the commit search.
    SearchRequested,
    SearchDismissed,
    /// The query changed. Starts a debounce rather than a search: searching is
    /// a walk of the whole history, and a typed word is not a request for one
    /// search per letter.
    SearchChanged(String),
    /// A debounce timer finished. Carries the query it was started for, so a
    /// timer the user has already typed past runs nothing.
    SearchDebounceElapsed(String),
    /// Results for a query. Carries the query they answer, so a slow search for
    /// an older query cannot overwrite the results of a newer one.
    SearchFinished {
        query: String,
        results: Box<Result<SearchResults, UiError>>,
    },
    /// `↑` / `↓` in the results.
    SearchStepped(i32),
    /// `Enter`, or a click: select the commit and close.
    SearchAccepted(ObjectId),

    /// Open the blame view for a path, at the commit being shown.
    BlameRequested {
        path: PathBuf,
        at: ObjectId,
    },
    /// The blame, and the commits its lines point at.
    BlameLoaded(Box<Result<BlameLoad, UiError>>),
    BlameDismissed,

    // ---- history operations ----
    /// The `⋯` on a commit. `update` builds the sheet, because a sheet is an
    /// application-level `Message` and the detail pane speaks `RepoMessage`.
    CommitActionsRequested(ObjectId),
    /// One branch badge on the graph was dragged onto another.
    ///
    /// Carries short names, which is what every operation here takes and what
    /// the confirmation shows. `update` decides which operations are legal —
    /// both depend on which branch is checked out — and always asks first: the
    /// discoverability of the gesture is the point, but not at the cost of an
    /// unintended rebase.
    BranchDropped {
        source: String,
        target: String,
    },
    /// Merge a branch into the current one.
    ///
    /// Not confirmed: a merge adds a commit and can be undone by resetting, and
    /// the outcome — including a conflict — is reported rather than assumed.
    MergeRequested(String),
    MergeFinished(Box<Result<MergeOutcome, UiError>>),
    /// Asks to rebase the current branch onto `onto`, which confirms first:
    /// rebasing rewrites every commit it moves.
    RebaseRequested(String),
    /// The confirmation was accepted. Only ever sent by the dialog.
    RebaseConfirmed(String),
    /// Opens the interactive plan editor for a rebase onto this ref. Nothing
    /// runs until the plan is started.
    RebasePlanRequested(String),
    /// The commits the rebase would replay, oldest first.
    RebasePlanLoaded(Box<Result<(String, Vec<Commit>), UiError>>),
    /// A row in the plan was picked.
    PlanRowSelected(usize),
    /// What to do with one commit, by row.
    PlanActionChosen(usize, RebaseAction),
    /// Move the selected row up (-1) or down (1).
    PlanRowMoved(i32),
    /// Run the plan as it stands.
    PlanStarted,
    /// Close the editor without running anything.
    PlanDismissed,
    CherryPickRequested(ObjectId),
    RevertRequested(ObjectId),
    /// Asks to reset. A hard reset confirms first; the other two do not, because
    /// they keep the work as changes.
    ResetRequested {
        to: ObjectId,
        mode: ResetMode,
    },
    /// The confirmation was accepted. Only ever sent by the dialog.
    ResetConfirmed {
        to: ObjectId,
        mode: ResetMode,
    },

    // ---- the conflict resolver ----
    /// A conflicted file was opened. The file is read and parsed off the UI
    /// thread, so this only asks for it.
    ConflictOpenRequested(PathBuf),
    /// The file was read and parsed, or could not be.
    ///
    /// A parse failure is not a crash: the file may have been edited by hand
    /// into a shape Git never writes, and the honest answer is to say so and
    /// leave the file alone.
    ConflictFileLoaded(Box<Result<(PathBuf, ConflictFile), UiError>>),
    /// A preset or a hand edit for one conflict, by index.
    ConflictResolved(usize, Resolution),
    /// `Cmd+]` / `Cmd+[`, or the arrows on the action bar.
    ConflictStepped(i32),
    /// The Edit button: opens the result pane for typing, seeded with whatever
    /// the current resolution would produce.
    ConflictEditToggled,
    /// A keystroke in the result pane's editor.
    ConflictEdited(text_editor::Action),
    /// Writes the resolved file and stages it, which is what ends the conflict
    /// for this path. Ordinary staging — resolving is not a special write.
    ConflictMarkedResolved,
    /// The file was written and staged.
    ConflictSaved(Box<Result<PathBuf, UiError>>),
    /// Continue, abort or skip whatever the repository is in the middle of.
    ///
    /// Abort confirms first: it throws away every resolution made so far, which
    /// is exactly the kind of thing the spec says must be unmistakable.
    SequenceControlRequested(SequenceControl),
    /// The confirmation was accepted. Only ever sent by the dialog.
    SequenceAbortConfirmed,
    /// The sequence finished, or stopped again on the next commit.
    SequenceFinished(Box<Result<SequenceOutcome, UiError>>),

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
    /// Something changed the repository: reload what the change can have made
    /// stale.
    ///
    /// One code path for "something changed", rather than each operation
    /// remembering which views it invalidated — but carrying *what* changed,
    /// because rereading history costs about a second on a large repository and
    /// a file save cannot have moved a ref.
    RepositoryChanged(hidegit_core::watch::Change),
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
    /// A submodule update, and whether the submodule actually ended up where
    /// the superproject says it should be.
    ///
    /// `settled` is carried because `git submodule update` reports success for
    /// a submodule it left exactly as it found it — so "the operation
    /// succeeded" is not the same claim as "the submodule is now current", and
    /// the user is owed the second one.
    SubmodulesUpdated {
        path: PathBuf,
        settled: bool,
    },
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
    pub submodules: Vec<Submodule>,
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
