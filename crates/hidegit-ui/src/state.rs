//! Application state.
//!
//! Shaped as `docs/UI_SPEC.md#application-state` describes it, including the
//! parts M1 does not use yet: `repos` is a vector with an `active` index from
//! the start, because multi-repository tabs are M6 and retrofitting them into a
//! single-repository shape would be a rewrite rather than an addition.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use hidegit_core::conflict::{ConflictFile, ConflictRegion, Resolution};
use hidegit_core::graph::{Checkpoints, GraphLayout, LaneState, layout_window};
use hidegit_core::model::{
    Commit, CommitDetail, Diff, Divergence, Head, ObjectId, Refs, Remote, RepoState, StashEntry,
    WorktreeStatus,
};
use hidegit_core::ops::{
    BlameLine, CancelToken, ProgressUpdate, RebaseAction, RebasePlan, RebaseStep, SearchResults,
    StartPoint,
};
use hidegit_core::{GitBackend, LogPage};
use hidegit_forge::{
    Activity, Alert, AlertPrefs, DeviceCode, GitHub, Identity, Notifier, PrRole, PullRequest,
    PullRequestDetail, RepoRef, Schedule, Watcher,
};

use iced::widget::text_editor;

use crate::message::{Message, UiError};
use crate::theme::Theme;

/// How many commits one background load fetches.
///
/// Large enough that a page is worth a task, small enough that the first
/// screenful appears without waiting for a full traversal.
pub const PAGE_SIZE: usize = 2_000;

/// How often lane state is snapshotted while scanning history.
///
/// A jump to an arbitrary scroll position replays at most this many rows.
pub const CHECKPOINT_INTERVAL: usize = 128;

/// Rows laid out beyond the viewport, so a fast scroll does not reveal a gap
/// before the next frame.
pub const OVERSCAN: usize = 16;

/// The height of a graph row, in logical pixels.
///
/// Fixed on purpose: it is what lets a scroll position map to a row index
/// arithmetically, and that arithmetic is what keeps scrolling a
/// 100,000-commit history cheap. Variable heights would allow richer rows and
/// cost exactly that.
pub const ROW_HEIGHT: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Welcome,
    Repository,
}

/// Which pane has keyboard focus.
///
/// `Hash` because it rides in a subscription's identity: the `Tab` binding has
/// to know which pane is focused to know which is next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    Sidebar,
    Graph,
    Detail,
}

impl Pane {
    /// `Tab` order: sidebar → graph → detail, wrapping.
    pub fn next(self) -> Self {
        match self {
            Pane::Sidebar => Pane::Graph,
            Pane::Graph => Pane::Detail,
            Pane::Detail => Pane::Sidebar,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Pane::Sidebar => Pane::Detail,
            Pane::Graph => Pane::Sidebar,
            Pane::Detail => Pane::Graph,
        }
    }
}

/// What the detail pane is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// The staging view: what is staged, changed and untracked right now.
    WorkingDirectory,
    Commit(ObjectId),
    /// A stash entry, by position from the top.
    ///
    /// Carries the index rather than the commit id because the index is what every
    /// stash subcommand takes, and it is what the list is keyed by. The id is
    /// looked up from `stashes` when the diff is loaded.
    Stash(usize),
    /// A pull request, by number.
    ///
    /// The number rather than a position, because the list is reordered by every
    /// poll and a position would select a different pull request each time one
    /// is updated.
    PullRequest(u64),
}

/// Unified or side-by-side, remembered per user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffMode {
    #[default]
    Unified,
    SideBySide,
}

impl DiffMode {
    pub fn toggled(self) -> Self {
        match self {
            DiffMode::Unified => DiffMode::SideBySide,
            DiffMode::SideBySide => DiffMode::Unified,
        }
    }
}

/// The detail pane's contents.
#[derive(Debug, Clone, Default)]
pub enum DetailPane {
    #[default]
    Empty,
    Loading,
    Commit {
        detail: Box<CommitDetail>,
        diff: Box<Diff>,
        /// Which file's diff is expanded.
        file: usize,
    },
    /// The staging view. Both diffs are held at once because the pane shows
    /// both lists, and a file can legitimately appear in each.
    WorkingDirectory {
        staged: Box<Diff>,
        unstaged: Box<Diff>,
        /// Which row of the combined list is open, if any.
        selected: Option<StagingRow>,
        /// Changed lines picked out of the open file's diff, as `(hunk, line)`
        /// indices. Cleared whenever the open row changes, because the indices
        /// mean nothing against a different diff.
        lines: BTreeSet<(usize, usize)>,
    },
    /// A pull request, read from the forge rather than from the repository.
    PullRequest(Box<PullRequestDetail>),
    Failed(UiError),
}

/// The forge session, which is application-wide rather than per repository.
///
/// One token signs you in to every repository you have open, so this does not
/// live on `OpenRepo` — and a second repository from the same host must not
/// prompt for a second sign-in.
#[derive(Debug, Default)]
pub struct ForgeSession {
    /// Built at boot; `None` only before the first task has run.
    pub client: Option<Arc<GitHub>>,
    /// Who the stored token belongs to. `None` means signed out.
    pub identity: Option<Identity>,
    /// The device code currently on screen, while the flow waits for it.
    pub connecting: Option<DeviceCode>,
    /// Set when there is no OS keychain.
    ///
    /// Forge features are disabled rather than downgraded to a file, so the UI
    /// has to say which of the two it is: "sign in" and "hideGit cannot store a
    /// token on this machine" call for entirely different next actions.
    pub no_keychain: bool,
}

impl ForgeSession {
    pub fn is_connected(&self) -> bool {
        self.identity.is_some()
    }
}

/// Pull requests for one repository.
#[derive(Debug, Default)]
pub struct PrPanel {
    /// Which forge repository the remotes point at.
    ///
    /// `None` when no remote names one — a repository with only a local remote
    /// has no pull requests to have, which is different from having none.
    pub repo: Option<RepoRef>,
    /// The last successful poll's list, kept across a failed one so the panel
    /// can go stale rather than empty.
    pub items: Vec<PullRequest>,
    pub state: PrState,
    /// When to ask next: the interval, the backoff and the budget.
    pub schedule: Schedule,
    /// What was seen last time, so a poll produces *transitions* rather than
    /// state. Per repository, because the numbers are.
    pub watcher: Watcher,
}

impl PrPanel {
    pub fn find(&self, number: u64) -> Option<&PullRequest> {
        self.items.iter().find(|pr| pr.number == number)
    }

    /// The pull requests under each heading, strongest role first.
    ///
    /// A pull request appears once, under its strongest role, because listing
    /// one you wrote *and* were assigned to under two headings would make the
    /// panel's counts disagree with its contents.
    pub fn grouped(&self) -> Vec<(PrRole, Vec<&PullRequest>)> {
        [PrRole::Author, PrRole::Reviewer, PrRole::Assignee]
            .into_iter()
            .filter_map(|role| {
                let items: Vec<&PullRequest> = self
                    .items
                    .iter()
                    .filter(|pr| pr.primary_role() == Some(role))
                    .collect();
                (!items.is_empty()).then_some((role, items))
            })
            .collect()
    }
}

/// What the panel last learned.
///
/// The four non-empty states read almost alike in a sidebar and mean entirely
/// different things, which is why they are distinct rather than a bool and a
/// list: "not signed in", "signed in but hideGit cannot see this repository",
/// "nothing open", and "this is what it looked like before the network went".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum PrState {
    /// Nothing has been asked for — no forge repository, or not signed in.
    #[default]
    Idle,
    Loading,
    Loaded,
    /// Authenticated, but the app is not installed on this repository.
    NotInstalled {
        install_url: String,
    },
    /// The last poll failed. `items` still holds the previous result.
    Stale(String),
}

/// A row in the staging view, identifying which list it came from.
///
/// The list matters as much as the path: the same file can sit in `staged` and
/// in `unstaged` at once, showing a different diff and offering a different
/// action in each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagingRow {
    pub section: Section,
    pub index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Staged,
    Unstaged,
    Untracked,
    Conflicted,
}

/// A destructive action, waiting to be confirmed.
///
/// `UI_SPEC` is explicit that these name what will be lost rather than asking
/// a generic "are you sure?", so the body is built by whoever raises it and
/// carries the count and the paths.
#[derive(Debug, Clone)]
pub struct Confirmation {
    pub title: String,
    pub body: String,
    /// The verb on the button that goes ahead — "Discard", never "OK".
    pub confirm_label: String,
    /// Dispatched if the user accepts. Nothing happens until then.
    pub action: Box<Message>,
}

/// A list of things that can be done to one item.
///
/// Deliberately a **centred card, not a positioned context menu.** iced 0.14
/// gives a `button`'s `on_press` no cursor coordinates and has no popover widget,
/// so anchoring a menu where the click happened would mean writing a custom
/// widget with its own overlay layer. A sheet titled with the item it acts on —
/// `feat/graph` — says the same thing, works from the keyboard, and is the one
/// mechanism behind per-item actions for branches, tags, remotes and stashes.
#[derive(Debug, Clone)]
pub struct ActionSheet {
    /// The item being acted on, e.g. `feat/graph`.
    pub title: String,
    pub items: Vec<SheetItem>,
    /// Which row the keyboard is on.
    ///
    /// `None` until an arrow key moves it, which is deliberate: a sheet has no
    /// default action, and one of its rows may be "Delete". `Enter` before you
    /// have chosen a row must not pick one for you.
    pub selected: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SheetItem {
    pub label: String,
    /// Dispatched when it is chosen. The sheet closes first, so an action that
    /// raises a confirmation of its own is not fighting for the same layer.
    pub message: Message,
    /// Rendered distinctly, per `UI_SPEC.md`: destructive actions must not look
    /// like the rest.
    pub destructive: bool,
}

impl ActionSheet {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            items: Vec::new(),
            selected: None,
        }
    }

    /// Moves the highlight by `delta`, wrapping.
    ///
    /// From nothing selected, down lands on the first row and up on the last —
    /// which is what every menu does, and what a hand reaching for the arrow
    /// keys expects.
    pub fn step(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as i32;
        self.selected = Some(match self.selected {
            None if delta > 0 => 0,
            None => (len - 1) as usize,
            Some(at) => (((at as i32 + delta) % len + len) % len) as usize,
        });
    }

    /// The message the highlighted row would dispatch, if one is highlighted.
    pub fn chosen(&self) -> Option<Message> {
        self.items
            .get(self.selected?)
            .map(|item| item.message.clone())
    }

    pub fn item(mut self, label: impl Into<String>, message: Message) -> Self {
        self.items.push(SheetItem {
            label: label.into(),
            message,
            destructive: false,
        });
        self
    }

    pub fn destructive(mut self, label: impl Into<String>, message: Message) -> Self {
        self.items.push(SheetItem {
            label: label.into(),
            message,
            destructive: true,
        });
        self
    }
}

/// A modal that collects text before acting.
///
/// A sibling of [`Confirmation`] rather than an extension of it:
/// `Confirmation::action` is a fixed [`Message`], and an action that depends on
/// what the user types cannot be one. So the *kind* is stored instead, and
/// `update` builds the real message from the kind plus the current field values
/// when it is accepted. That keeps `Message` cloneable and keeps closures out of
/// application state.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub kind: PromptKind,
    pub title: String,
    /// The verb on the button that goes ahead — "Create", never "OK".
    pub confirm_label: String,
    pub fields: Vec<PromptField>,
}

/// One field, and the widget id that lets it be focused.
///
/// A correction to what M2 concluded: focus in iced 0.14 is not *observable*, but
/// it is *settable*, through a widget operation. So raising a prompt can put the
/// cursor in the first field, and the user does not have to click the thing they
/// just asked for.
#[derive(Debug, Clone)]
pub struct PromptField {
    pub label: String,
    pub placeholder: String,
    pub value: String,
}

impl PromptField {
    pub fn new(label: impl Into<String>, placeholder: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            placeholder: placeholder.into(),
            value: String::new(),
        }
    }

    /// A field that starts from an existing value — renaming opens holding the
    /// current name, the way `git commit --amend` opens holding the old message.
    pub fn prefilled(label: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            label: label.into(),
            placeholder: value.clone(),
            value,
        }
    }
}

/// Widget ids for the prompt's fields, so they can be focused.
///
/// Static because a prompt never has more than two: naming them is simpler than
/// carrying a generated id through application state.
pub const PROMPT_FIELD_IDS: [&str; 2] = ["hidegit-prompt-field-0", "hidegit-prompt-field-1"];

/// The commit composer's two fields.
///
/// They carry ids because `find_focused` — which is how the `Space` binding
/// asks whether a field is being typed into — **ignores widgets that have
/// none**. Without these it would report nothing focused while a commit message
/// was being written, and `Space` would stage a file mid-sentence.
/// The search box.
///
/// Carries an id so it can be focused when the panel opens — a search you have
/// to click into before typing is a search that costs a click every time.
pub const SEARCH_FIELD_ID: &str = "hidegit-search-field";
pub const COMMAND_FIELD_ID: &str = "hidegit-command-field";

pub const COMPOSER_FIELD_IDS: [&str; 2] = ["hidegit-composer-subject", "hidegit-composer-body"];

/// What a [`Prompt`] is collecting text for.
///
/// The prompt is always about the active repository — it is modal, so nothing can
/// change which one that is while it is up — so the index is not carried here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    /// One field: the new branch's name.
    NewBranch { from: StartPoint, checkout: bool },
    /// One field: the new name.
    RenameBranch { from: String },
    /// One field: the tag's name. A message makes it annotated.
    NewTag { at: StartPoint, annotated: bool },
    /// Two fields: name, then URL.
    AddRemote,
    /// One field: the new URL.
    EditRemote { name: String },
    /// One field: the message. Optional — an empty one is Git's own `WIP on …`.
    StashPush { include_untracked: bool },
    /// One field: the URL. The destination is chosen with the platform's picker
    /// afterwards, because typing a path is worse than pointing at one.
    Clone,
    /// One field: the token. Accepted with no repository open, because signing
    /// in is not something you do to a repository.
    PersonalAccessToken,
    /// Two fields: title, then body. The branches are already decided — you
    /// open a pull request *from* where you are standing.
    NewPullRequest { head: String, base: String },
}

impl Prompt {
    /// The first field's value, trimmed. `None` when it is empty.
    ///
    /// Trimmed because a branch name with a trailing space is a name Git will
    /// refuse, and the refusal would be about whitespace the user cannot see.
    pub fn first(&self) -> Option<&str> {
        self.field(0)
    }

    /// A field's value, trimmed, or `None` when it is empty.
    pub fn field(&self, at: usize) -> Option<&str> {
        let value = self.fields.get(at)?.value.trim();
        (!value.is_empty()).then_some(value)
    }

    /// Is there enough here to act on?
    ///
    /// Every field is required except a stash's message, which Git will invent —
    /// so that one prompt can be accepted empty and the others cannot.
    pub fn is_ready(&self) -> bool {
        match self.kind {
            PromptKind::StashPush { .. } => true,
            _ => (0..self.fields.len()).all(|at| self.field(at).is_some()),
        }
    }
}

/// A long operation in flight.
///
/// One at a time per repository, which is why the toolbar replaces its buttons
/// with a banner rather than queueing: two fetches racing for the same refs is not
/// a thing worth supporting, and `id` is what keeps a *cancelled* operation's late
/// result from clearing the banner of the one that replaced it.
#[derive(Debug, Clone)]
pub struct Operation {
    pub id: u64,
    /// What to call it in the banner — "Fetching", "Pushing to origin".
    pub label: String,
    /// Set by the Cancel button. The worker polls it and kills the subprocess.
    pub cancel: CancelToken,
    /// The most recent report, or `None` before the first one arrives.
    pub progress: Option<ProgressUpdate>,
}

impl Operation {
    /// The banner's right-hand text: the phase and a real unit.
    ///
    /// `UI_SPEC.md` requires a real unit rather than an indeterminate spinner, so
    /// before the first report this says what it is waiting for rather than
    /// inventing a number.
    pub fn detail(&self) -> String {
        match &self.progress {
            None => "starting…".to_owned(),
            Some(update) => match update.total {
                Some(total) => format!("{} {}/{}", update.phase, update.done, total),
                None => format!("{} {}", update.phase, update.done),
            },
        }
    }

    /// Progress as a fraction, when the total is known.
    pub fn fraction(&self) -> Option<f32> {
        self.progress.as_ref().and_then(ProgressUpdate::fraction)
    }
}

/// Where a push would go, and under what name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
    pub remote: String,
    pub branch: String,
    pub refspec: String,
    /// True when the branch has no upstream yet, so the push should record one.
    pub set_upstream: bool,
}

/// A transient message.
#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub summary: String,
    pub details: String,
}

/// Loaded history, plus where the viewport sits in it.
#[derive(Debug, Default)]
pub struct GraphView {
    /// Every commit loaded so far, newest first.
    pub commits: Vec<Commit>,
    /// Ids of everything loaded, so a commit whose parents are simply not
    /// loaded yet is not mistaken for a shallow boundary.
    pub known: HashSet<ObjectId>,
    /// Total reachable commits, for sizing the scrollbar before loading
    /// finishes.
    pub total: usize,
    pub checkpoints: Option<Checkpoints>,
    /// Scroll position, as a fractional row index.
    pub scroll: f32,
    /// How many whole rows fit in the viewport.
    pub viewport_rows: usize,
    /// The selected row.
    pub selected: usize,
    /// Whether another page is still coming.
    pub loading_more: bool,
}

impl GraphView {
    pub fn append(&mut self, commits: Vec<Commit>) {
        self.known.extend(commits.iter().map(|c| c.id));
        self.commits.extend(commits);
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    /// The page to request next.
    pub fn next_page(&self) -> LogPage {
        LogPage {
            skip: self.commits.len(),
            limit: PAGE_SIZE,
        }
    }

    /// The first row the viewport shows.
    pub fn first_visible(&self) -> usize {
        self.scroll.max(0.0) as usize
    }

    /// The range of rows to lay out: what is visible, plus overscan.
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        let start = self.first_visible().saturating_sub(OVERSCAN);
        let end = (self.first_visible() + self.viewport_rows + OVERSCAN).min(self.commits.len());
        start..end.max(start)
    }

    /// Lays out just the rows the viewport needs.
    ///
    /// Resumes from the nearest checkpoint rather than replaying from `HEAD`,
    /// so the cost is bounded by the checkpoint interval no matter how far
    /// down the history the viewport has scrolled.
    pub fn layout_visible(&self) -> (usize, GraphLayout) {
        let range = self.visible_range();
        if range.is_empty() {
            return (0, GraphLayout::default());
        }

        let (from, mut state) = match &self.checkpoints {
            Some(checkpoints) => checkpoints.resume_at(range.start),
            None => (0, LaneState::new()),
        };

        let layout = layout_window(&self.commits[from..range.end], &self.known, &mut state);
        (from, layout)
    }

    /// Clamps the scroll position to the loaded history.
    pub fn clamp_scroll(&mut self) {
        let max = self.commits.len().saturating_sub(self.viewport_rows.max(1)) as f32;
        self.scroll = self.scroll.clamp(0.0, max.max(0.0));
    }

    /// Scrolls just enough to bring the selected row into view.
    pub fn scroll_to_selection(&mut self) {
        let first = self.first_visible();
        let last = first + self.viewport_rows.saturating_sub(1);

        if self.selected < first {
            self.scroll = self.selected as f32;
        } else if self.selected > last {
            self.scroll = (self.selected + 1).saturating_sub(self.viewport_rows) as f32;
        }
        self.clamp_scroll();
    }

    /// The commit the viewport is anchored to.
    ///
    /// Scroll position is restored by commit id rather than by row index: new
    /// commits arriving at `HEAD` shift every row down, and a background fetch
    /// must not silently move the user's place.
    pub fn anchor(&self) -> Option<ObjectId> {
        self.commits.get(self.first_visible()).map(|c| c.id)
    }

    /// Restores an anchor recorded by [`GraphView::anchor`].
    pub fn restore_anchor(&mut self, anchor: ObjectId) {
        if let Some(row) = self.commits.iter().position(|c| c.id == anchor) {
            self.scroll = row as f32;
            self.clamp_scroll();
        }
    }
}

/// One open repository.
#[derive(Debug)]
pub struct OpenRepo {
    pub path: PathBuf,
    pub backend: Arc<dyn GitBackend>,
    pub head: Head,
    pub refs: Refs,
    /// Consulted before rendering any action: a repository mid-rebase does not
    /// offer "commit" as though nothing is happening.
    pub state: RepoState,
    /// The working directory as the staging view shows it.
    pub status: WorktreeStatus,
    /// The stash, newest first.
    ///
    /// Read as part of a refresh, unlike `divergence`: it is one ref and one
    /// reflog, which is cheap enough for the watcher path.
    pub stashes: Vec<StashEntry>,
    /// Every configured remote, with its URLs.
    ///
    /// Read alongside `refs`, because a remote that has never been fetched has no
    /// tracking refs and would otherwise be invisible.
    pub remotes: Vec<Remote>,
    /// Ahead/behind per local branch, keyed by full ref name.
    ///
    /// Loaded by its own task rather than as part of a refresh: it costs a commit
    /// walk per tracking branch, and a refresh runs on every file save through
    /// the watcher. A branch that tracks nothing is absent, which is different
    /// from being level with a remote and has to render differently.
    pub divergence: HashMap<String, Divergence>,
    /// The network operation in flight, if any.
    pub pending: Option<Operation>,
    pub graph: GraphView,
    pub selection: Option<Selection>,
    pub detail: DetailPane,
    pub focus: Pane,
    pub diff_mode: DiffMode,
    /// Which hunk `J`/`K` last stepped to, so the diff view can scroll to it.
    pub hunk: usize,
    /// The commit message being written, and whether it is being amended.
    pub draft: Draft,
    /// Pull requests for this repository, and what the last poll learned.
    pub prs: PrPanel,
    /// The conflicted file open in the resolver, if one is.
    pub resolver: Option<Resolver>,
    /// The interactive rebase being planned, if one is.
    pub plan: Option<RebaseEditor>,
    /// The file open in the blame view, if one is.
    pub blame: Option<BlameView>,
    /// The commit search, if it is open.
    pub search: Option<Search>,
}

/// The command palette, while it is up.
///
/// The query and the selection, and nothing else: the list of matches is
/// derived from the query every frame rather than stored, because it is fifteen
/// rows filtered by a substring — caching it would be more state to keep right
/// than work to save.
#[derive(Debug, Default)]
pub struct CommandPalette {
    pub query: String,
    /// Which match the keyboard is on, as an index into the filtered list.
    pub selected: usize,
}

/// The commit search.
///
/// The query and its results are separate on purpose: typing runs a new search,
/// and the previous results stay on screen until the new ones arrive rather than
/// blinking empty on every keystroke.
#[derive(Debug, Default)]
pub struct Search {
    pub query: String,
    pub results: SearchResults,
    /// A search is in flight for a query that is not `query` any more.
    ///
    /// Shown as a quiet marker rather than a spinner: on most repositories the
    /// walk finishes between keystrokes, and a spinner that flashes on every
    /// letter is worse than no spinner.
    pub running: bool,
    /// Which hit the keyboard is on.
    pub selected: usize,
}

impl Search {
    /// Moves the selection, clamped to the hits that exist.
    pub fn step(&mut self, delta: i32) {
        if self.results.hits.is_empty() {
            return;
        }
        let last = self.results.hits.len() as i32 - 1;
        self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
    }

    pub fn selected_commit(&self) -> Option<ObjectId> {
        self.results.hits.get(self.selected).map(|h| h.commit.id)
    }
}

/// One file, with the commit that last touched each line.
#[derive(Debug)]
pub struct BlameView {
    pub path: PathBuf,
    /// The revision blamed. Not necessarily `HEAD`: blaming an older commit is
    /// most of the point of having the view.
    pub at: ObjectId,
    pub lines: Vec<BlameLine>,
    /// Metadata for the commits the lines point at, so the gutter can show who
    /// and when rather than only a hash.
    ///
    /// Loaded alongside the blame in the same task. Absent for a commit that
    /// could not be read, which the gutter renders as the hash alone rather
    /// than as blank.
    pub commits: HashMap<ObjectId, Commit>,
}

/// An interactive rebase being planned, before anything has been run.
///
/// Nothing here has touched the repository yet: the plan is built, reordered
/// and only then handed to `git rebase --interactive` in one go. That is what
/// makes the whole screen abandonable — closing it costs nothing, because
/// nothing has happened.
#[derive(Debug)]
pub struct RebaseEditor {
    /// The branch or commit being rebased onto.
    pub onto: String,
    /// The plan, in the order it will be applied — **oldest first**, which is
    /// the order `git rebase --interactive` writes its todo list in.
    pub steps: Vec<PlannedStep>,
    /// Which row the keyboard and the move buttons act on.
    pub selected: usize,
}

/// One commit in the plan, with what to do to it.
#[derive(Debug, Clone)]
pub struct PlannedStep {
    pub commit: Commit,
    pub action: RebaseAction,
}

impl RebaseEditor {
    pub fn new(onto: String, commits: Vec<Commit>) -> Self {
        Self {
            onto,
            steps: commits
                .into_iter()
                .map(|commit| PlannedStep {
                    commit,
                    action: RebaseAction::Pick,
                })
                .collect(),
            selected: 0,
        }
    }

    /// Moves the selected step by `delta`, carrying the selection with it.
    ///
    /// Clamped rather than wrapping: a step that jumped from the top to the
    /// bottom would be a reorder nobody asked for, and reordering is the one
    /// thing here that is hard to undo by eye.
    pub fn move_selected(&mut self, delta: i32) {
        if self.steps.is_empty() {
            return;
        }
        let target = (self.selected as i32 + delta).clamp(0, self.steps.len() as i32 - 1) as usize;
        if target != self.selected {
            self.steps.swap(self.selected, target);
            self.selected = target;
        }
    }

    pub fn select(&mut self, at: usize) {
        if at < self.steps.len() {
            self.selected = at;
        }
    }

    pub fn set_action(&mut self, at: usize, action: RebaseAction) {
        if let Some(step) = self.steps.get_mut(at) {
            step.action = action;
        }
    }

    /// The plan as the backend takes it.
    pub fn plan(&self) -> RebasePlan {
        RebasePlan {
            steps: self
                .steps
                .iter()
                .map(|s| RebaseStep {
                    action: s.action,
                    commit: s.commit.id,
                })
                .collect(),
        }
    }

    /// How many commits survive the plan.
    pub fn kept(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| !matches!(s.action, RebaseAction::Drop))
            .count()
    }

    /// Why the plan cannot be run, if it cannot.
    ///
    /// Returned as the sentence to show rather than a bool, because "Start" is
    /// disabled for two quite different reasons and a user staring at a greyed
    /// button deserves to know which.
    pub fn blocked(&self) -> Option<&'static str> {
        if self.steps.is_empty() {
            return Some("There is nothing to rebase onto that branch.");
        }
        // `squash` and `fixup` fold into the commit above them, so a plan that
        // opens with one has nothing to fold into and Git refuses it outright.
        if matches!(
            self.steps.first().map(|s| s.action),
            Some(RebaseAction::Squash) | Some(RebaseAction::Fixup)
        ) {
            return Some("The first step cannot squash or fixup — there is nothing above it.");
        }
        if self.kept() == 0 {
            return Some("Every commit is dropped, which would leave the branch with nothing.");
        }
        None
    }
}

/// One conflicted file, and the decisions made about it so far.
///
/// Kept on the repository rather than in the widget for the same reason the
/// commit draft is: the watcher refreshes status on every file save, and a
/// resolver that lived in the view would lose a half-finished resolution every
/// time it fired. The spec names that failure mode explicitly.
#[derive(Debug)]
pub struct Resolver {
    pub path: PathBuf,
    pub file: ConflictFile,
    /// One per conflict in `file`, in the same order.
    pub resolutions: Vec<Resolution>,
    /// Which conflict the action bar acts on.
    pub focused: usize,
    /// The result pane's editor, open only while the focused conflict is being
    /// edited by hand.
    ///
    /// `None` means the presets are driving it. The editor is not kept open
    /// across a move between conflicts, because its content belongs to the
    /// conflict it was opened on.
    pub editor: Option<text_editor::Content>,
}

impl Resolver {
    pub fn new(path: PathBuf, file: ConflictFile) -> Self {
        let resolutions = vec![Resolution::Unresolved; file.conflict_count()];
        Self {
            path,
            file,
            resolutions,
            focused: 0,
            editor: None,
        }
    }

    pub fn conflict_count(&self) -> usize {
        self.file.conflict_count()
    }

    /// How many conflicts still have no decision.
    ///
    /// Shown next to a disabled Continue, because "you cannot continue yet" is
    /// only useful with "and here is how much is left".
    pub fn remaining(&self) -> usize {
        self.resolutions.iter().filter(|r| !r.is_resolved()).count()
    }

    pub fn is_resolved(&self) -> bool {
        self.file.is_resolved(&self.resolutions)
    }

    /// The region the action bar acts on.
    pub fn focused_region(&self) -> Option<&ConflictRegion> {
        self.file.conflicts().nth(self.focused)
    }

    /// Moves the focus by `delta`, clamped to the conflicts that exist.
    ///
    /// Clamped rather than wrapping: wrapping past the last conflict looks
    /// identical to having finished, and the difference matters when the point
    /// of the screen is knowing whether you are done.
    pub fn step(&mut self, delta: i32) {
        let count = self.conflict_count();
        if count == 0 {
            return;
        }
        let target = (self.focused as i32 + delta).clamp(0, count as i32 - 1);
        if target as usize != self.focused {
            self.focused = target as usize;
            // The editor's content belongs to the conflict it was opened on.
            self.editor = None;
        }
    }

    /// The file as it would be written right now.
    pub fn rendered(&self) -> String {
        self.file.render(&self.resolutions)
    }
}

/// The commit message in progress.
///
/// Kept on the repository rather than in the widget so it survives a refresh:
/// staging another hunk halfway through writing a message must not throw the
/// message away.
#[derive(Debug, Default, Clone)]
pub struct Draft {
    pub subject: String,
    pub body: String,
    pub amend: bool,
    pub sign_off: bool,
    /// `Cmd+Shift+Enter` asked for a push once the commit lands.
    ///
    /// Held here rather than passed through the commit task because the push
    /// must only happen if the commit actually succeeded, and only the result
    /// knows that.
    pub push_after_commit: bool,
    /// A text field has keyboard focus, so bare-letter shortcuts must not fire.
    ///
    /// Without this, typing "jk" into a commit message steps through hunks:
    /// `keyboard::listen()` is global and `j`/`k` are bound unmodified.
    pub editing: bool,
}

impl Draft {
    /// The message as Git stores it: subject, blank line, body.
    pub fn message(&self) -> String {
        let subject = self.subject.trim();
        let body = self.body.trim();
        if body.is_empty() {
            subject.to_owned()
        } else {
            format!("{subject}\n\n{body}\n")
        }
    }

    /// Is there enough here to commit?
    ///
    /// A subject is the one part Git will not invent: an empty message aborts
    /// the commit, which reads as nothing happening.
    pub fn is_ready(&self) -> bool {
        !self.subject.trim().is_empty()
    }
}

impl OpenRepo {
    /// The branch name to show in the toolbar.
    pub fn head_label(&self) -> String {
        match &self.head {
            Head::Branch { name, .. } => name.short.clone(),
            Head::Unborn { name } => format!("{} (no commits yet)", name.short),
            Head::Detached { target } => format!("detached at {}", target.short(7)),
        }
    }

    /// May `HEAD` be moved right now?
    ///
    /// A merge or rebase in progress owns `HEAD` until it is finished or aborted,
    /// so switching branches mid-operation is not something to offer. Read both by
    /// `update`, before acting, and by the view, to say why a control is disabled —
    /// `UI_SPEC.md` requires those two answers to be the same one.
    pub fn can_switch_branches(&self) -> bool {
        !self.state.is_in_progress()
    }

    /// The remote to reach for when nothing names one.
    ///
    /// `origin` when it exists, because that is what it means; otherwise the first
    /// remote there is, alphabetically, so a repository whose only remote is called
    /// something else still works. Derived from remote-tracking refs, which is all
    /// `refs` carries — `remotes()` arrives with the rest of remote management.
    pub fn default_remote(&self) -> Option<String> {
        let mut names: Vec<&str> = self.remotes.iter().map(|r| r.name.as_str()).collect();

        // Falls back to the names implied by tracking refs, so this still answers
        // before `remotes` has been read — and for a repository whose config
        // gitoxide could not parse.
        if names.is_empty() {
            names = self
                .refs
                .remotes
                .iter()
                .filter_map(|b| b.name.short.split_once('/').map(|(remote, _)| remote))
                .collect();
        }
        names.sort_unstable();
        names.dedup();

        if names.contains(&"origin") {
            return Some("origin".to_owned());
        }
        names.first().map(|n| (*n).to_owned())
    }

    /// Remote-tracking branches grouped under the remote they belong to.
    ///
    /// Every configured remote appears, fetched or not, so the sidebar cannot imply
    /// that a remote which has never been fetched does not exist.
    pub fn remotes_with_branches(&self) -> Vec<(&Remote, Vec<&hidegit_core::model::Branch>)> {
        self.remotes
            .iter()
            .map(|remote| {
                let prefix = format!("{}/", remote.name);
                let branches = self
                    .refs
                    .remotes
                    .iter()
                    // Matched on the whole first segment, so `origin` does not
                    // collect `origin-mirror/main`.
                    .filter(|b| b.name.short.starts_with(&prefix))
                    .collect();
                (remote, branches)
            })
            .collect()
    }

    /// Where a push from the current branch would go.
    ///
    /// `None` when there is nothing to push *from* — a detached `HEAD` or an unborn
    /// branch — or nowhere to push *to*. The toolbar reads this both to decide
    /// whether Push is available and to say why it is not.
    pub fn push_target(&self) -> Option<PushTarget> {
        let Head::Branch { name, .. } = &self.head else {
            return None;
        };
        let branch = name.short.clone();

        // An existing upstream names both the remote *and the branch on it*, and
        // the two are not always the same name. Renaming a local branch leaves its
        // upstream pointing at the old one, which is the ordinary state — pushing
        // to the local name instead would quietly create a second branch on the
        // remote rather than updating the one being tracked.
        let upstream = self
            .refs
            .locals
            .iter()
            .find(|b| b.name.full == name.full)
            .and_then(|b| b.upstream.as_deref())
            .and_then(|full| full.strip_prefix("refs/remotes/"))
            .and_then(|rest| rest.split_once('/'))
            .map(|(remote, on_remote)| (remote.to_owned(), on_remote.to_owned()));

        let set_upstream = upstream.is_none();
        let (remote, on_remote) = match upstream {
            Some(pair) => pair,
            // Nothing tracked yet, so it goes out under its own name.
            None => (self.default_remote()?, branch.clone()),
        };

        Some(PushTarget {
            remote,
            // Fully qualified on both sides, so a branch whose name also matches a
            // tag cannot be resolved to the wrong thing on either end.
            refspec: format!("refs/heads/{branch}:refs/heads/{on_remote}"),
            branch,
            set_upstream,
        })
    }

    /// How far ahead the current branch is of its upstream, for the Push button.
    pub fn head_ahead(&self) -> usize {
        match &self.head {
            Head::Branch { name, .. } => self
                .divergence_of(&name.full)
                .map_or(0, |drift| drift.ahead),
            _ => 0,
        }
    }

    /// Ahead/behind for a branch, or `None` when it tracks nothing.
    ///
    /// `None` and `Some(0, 0)` mean different things and must render differently:
    /// one is "there is no remote to compare with", the other is "level with the
    /// remote".
    pub fn divergence_of(&self, full_ref: &str) -> Option<Divergence> {
        self.divergence.get(full_ref).copied()
    }

    /// The repository's own name, for a window title or a tab.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// The whole application.
#[derive(Debug)]
pub struct App {
    pub screen: Screen,
    pub repos: Vec<OpenRepo>,
    pub active: Option<usize>,
    /// Most recently opened first, deduplicated.
    pub recents: Vec<PathBuf>,
    pub theme: Theme,
    /// Every theme that can be chosen: the two that ship, then whatever was
    /// found in the themes directory. Held rather than recomputed because the
    /// custom ones came off disk once, at startup.
    pub themes: Vec<Theme>,
    pub toasts: Vec<Toast>,
    /// The confirmation currently on screen, if any.
    pub confirming: Option<Confirmation>,
    /// The action sheet currently on screen, if any.
    /// The settings panel is open.
    ///
    /// A flag rather than a draft: every change applies as it is made, so there
    /// is nothing to hold and nothing to discard on close. Settings that only
    /// take effect on OK are settings people are afraid to explore.
    pub settings_open: bool,
    /// The command palette, if it is up.
    pub palette: Option<CommandPalette>,
    /// The keyboard shortcut reference is open.
    ///
    /// Its own flag rather than a mode of the settings panel: it is opened from
    /// a key, read, and closed, and putting it behind a settings tab would make
    /// the answer to "what was that key again" three keystrokes away.
    pub shortcuts_open: bool,
    /// Why the last settings change did not reach the file, if it did not.
    ///
    /// Set by whoever owns the file — the shell, not the interface — because
    /// `hidegit-ui` has no business knowing where `config.toml` lives. Carried
    /// as text rather than a typed error for the same reason: the crate that
    /// writes the file is the one that can say what went wrong, and this crate
    /// only has to show it.
    ///
    /// The panel says settings are saved as they are made. When that is not
    /// true, it has to say so instead — a toggle that visibly flips, a footer
    /// that claims it saved, and a change that is gone on restart is worse than
    /// having no settings screen at all.
    pub settings_error: Option<String>,
    pub sheet: Option<ActionSheet>,
    /// The prompt currently on screen, if any.
    pub prompt: Option<Prompt>,
    /// The forge session: one token, every open repository.
    pub forge: ForgeSession,
    /// Where a notification goes. A trait object because nothing in CI can
    /// receive one, so everything that *decides* to notify is tested against a
    /// recorder instead.
    pub notifier: Arc<dyn Notifier>,
    /// Whether the window has focus. Half of what decides the poll interval.
    pub focused: bool,
    /// Which alerts to send, and when not to.
    pub alerts: AlertPrefs,
    /// Reopen at the size and position the window was last closed at.
    ///
    /// Held here only so the settings panel can show and change it. The window
    /// itself belongs to the shell — `hidegit-ui` never reads it, and nothing
    /// in this crate behaves differently either way.
    pub remember_geometry: bool,
    next_toast_id: u64,
}

impl Default for App {
    fn default() -> Self {
        Self {
            screen: Screen::Welcome,
            repos: Vec::new(),
            active: None,
            recents: Vec::new(),
            theme: Theme::default(),
            themes: Theme::built_in(),
            settings_open: false,
            palette: None,
            shortcuts_open: false,
            settings_error: None,
            toasts: Vec::new(),
            confirming: None,
            sheet: None,
            prompt: None,
            forge: ForgeSession::default(),
            notifier: Arc::new(hidegit_forge::Desktop),
            // Assumed focused until told otherwise: a window that has just
            // opened is being looked at, and iced reports focus by event rather
            // than on demand.
            focused: true,
            alerts: AlertPrefs::default(),
            remember_geometry: true,
            next_toast_id: 0,
        }
    }
}

impl App {
    pub fn active_repo(&self) -> Option<&OpenRepo> {
        self.active.and_then(|i| self.repos.get(i))
    }

    pub fn active_repo_mut(&mut self) -> Option<&mut OpenRepo> {
        match self.active {
            Some(i) => self.repos.get_mut(i),
            None => None,
        }
    }

    /// Is a modal layer up?
    ///
    /// While one is, it owns the keyboard: letting a bare key reach the screen
    /// behind a question the user has to answer is the worst possible moment to
    /// act on a stray press.
    /// How often to poll, given what is on screen.
    ///
    /// `Foreground` needs somebody actually reading the answer, which is what a
    /// selected pull request means. Being focused with the graph open is
    /// ordinary use, not a reason to ask every minute.
    pub fn activity(&self) -> Activity {
        if !self.focused {
            return Activity::Background;
        }
        let reading = self
            .active_repo()
            .is_some_and(|repo| matches!(repo.selection, Some(Selection::PullRequest(_))));

        if reading {
            Activity::Foreground
        } else {
            Activity::Normal
        }
    }

    /// Sends whatever the preferences allow, for one repository.
    ///
    /// The one place that reads the clock and consults the preferences, so
    /// there is exactly one answer to "why did that not notify me?" — rather
    /// than a filter at each of the two call sites that produce alerts.
    pub fn notify(&self, alerts: &[Alert], repository: &str) {
        // The local hour, because quiet hours are the user's evening rather
        // than UTC's. A machine with no discoverable offset falls back to UTC:
        // being an hour out on a quiet-hours boundary beats not applying them.
        let hour = time::OffsetDateTime::now_local()
            .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
            .hour();

        let allowed: Vec<Alert> = alerts
            .iter()
            .filter(|alert| self.alerts.allows(alert.event, repository, hour))
            .cloned()
            .collect();

        for (summary, body) in hidegit_forge::notify::compose(&allowed, repository) {
            self.notifier.notify(&summary, &body);
        }
    }

    pub fn is_modal(&self) -> bool {
        self.confirming.is_some() || self.sheet.is_some() || self.prompt.is_some()
    }

    /// Raises a toast carrying the error's details for copying.
    pub fn toast(&mut self, error: &UiError) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts.push(Toast {
            id,
            summary: error.summary.clone(),
            details: error.details.clone(),
        });
    }

    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
    }

    /// Records a repository as recently opened, newest first, without
    /// duplicates.
    pub fn remember(&mut self, path: PathBuf) {
        self.recents.retain(|p| p != &path);
        self.recents.insert(0, path);
        self.recents.truncate(10);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_cycles_the_three_panes_and_wraps() {
        assert_eq!(Pane::Sidebar.next(), Pane::Graph);
        assert_eq!(Pane::Graph.next(), Pane::Detail);
        assert_eq!(Pane::Detail.next(), Pane::Sidebar);
        assert_eq!(Pane::Sidebar.previous(), Pane::Detail);
    }

    #[test]
    fn recent_repositories_are_newest_first_and_deduplicated() {
        let mut app = App::default();
        app.remember(PathBuf::from("/a"));
        app.remember(PathBuf::from("/b"));
        app.remember(PathBuf::from("/a"));

        assert_eq!(app.recents, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn the_recent_list_does_not_grow_without_bound() {
        let mut app = App::default();
        for i in 0..25 {
            app.remember(PathBuf::from(format!("/repo{i}")));
        }
        assert_eq!(app.recents.len(), 10);
        assert_eq!(app.recents[0], PathBuf::from("/repo24"));
    }

    #[test]
    fn a_dismissed_toast_leaves_the_others_alone() {
        let mut app = App::default();
        app.toast(&UiError {
            summary: "first".into(),
            details: String::new(),
        });
        app.toast(&UiError {
            summary: "second".into(),
            details: String::new(),
        });

        let first = app.toasts[0].id;
        app.dismiss_toast(first);

        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].summary, "second");
    }
}
