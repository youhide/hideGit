//! Application state.
//!
//! Shaped as `docs/UI_SPEC.md#application-state` describes it, including the
//! parts M1 does not use yet: `repos` is a vector with an `active` index from
//! the start, because multi-repository tabs are M6 and retrofitting them into a
//! single-repository shape would be a rewrite rather than an addition.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use hidegit_core::graph::{Checkpoints, GraphLayout, LaneState, layout_window};
use hidegit_core::model::{
    Commit, CommitDetail, Diff, Head, ObjectId, Refs, RepoState, WorktreeStatus,
};
use hidegit_core::{GitBackend, LogPage};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    Failed(UiError),
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
    pub graph: GraphView,
    pub selection: Option<Selection>,
    pub detail: DetailPane,
    pub focus: Pane,
    pub diff_mode: DiffMode,
    /// Which hunk `J`/`K` last stepped to, so the diff view can scroll to it.
    pub hunk: usize,
    /// The commit message being written, and whether it is being amended.
    pub draft: Draft,
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
    pub toasts: Vec<Toast>,
    /// The confirmation currently on screen, if any.
    pub confirming: Option<Confirmation>,
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
            toasts: Vec::new(),
            confirming: None,
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
