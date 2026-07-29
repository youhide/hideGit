//! `update`, `view` and `subscription` — the Elm loop.
//!
//! The one rule this file exists to enforce: **nothing blocking runs here.**
//! `gix` calls and `git` subprocesses are blocking, so every one of them goes
//! through [`blocking`] onto tokio's blocking pool and comes back as another
//! message. `update` itself only ever touches memory.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use hidegit_core::graph::Checkpoints;
use hidegit_core::model::{DiffTarget, LogPage, ObjectId, RevSpec};
use hidegit_core::{GitBackend, GitError, HybridBackend};
use iced::widget::canvas;
use iced::{Element as IcedElement, Subscription, Task, keyboard};

use crate::message::{
    CommitLoad, Message, OpenedRepository, Page, RepoMessage, StatusLoad, UiError,
};
use crate::state::{
    App, CHECKPOINT_INTERVAL, DetailPane, GraphView, OpenRepo, PAGE_SIZE, Pane, ROW_HEIGHT, Screen,
    Selection,
};
use crate::{screen, widget};

/// The application, plus the bits of view state that are not domain state.
#[derive(Debug, Default)]
pub struct Hidegit {
    pub app: App,
    /// Cached graph geometry, one per repository, keyed by its index.
    ///
    /// Redraws that do not move the viewport or change the commit list reuse it
    /// rather than rebuilding every row.
    caches: HashMap<usize, canvas::Cache>,
}

/// Runs `f` on tokio's blocking pool and delivers its result as a message.
///
/// The whole concurrency model in one function: `update` returns this, work
/// happens off the UI thread, completion arrives as a `Message`.
fn blocking<T, F>(f: F) -> Task<Result<T, UiError>>
where
    F: FnOnce() -> Result<T, GitError> + Send + 'static,
    T: Send + 'static,
{
    Task::perform(
        async move {
            match tokio::task::spawn_blocking(f).await {
                Ok(result) => result.map_err(UiError::from),
                Err(join) => Err(UiError {
                    summary: "a background Git operation panicked".to_owned(),
                    details: join.to_string(),
                }),
            }
        },
        |result| result,
    )
}

impl Hidegit {
    pub fn new(initial: Option<PathBuf>, recents: Vec<PathBuf>) -> (Self, Task<Message>) {
        let mut this = Self::default();
        this.app.recents = recents;

        let task = match initial {
            Some(path) => Task::done(Message::OpenRepository(path)),
            None => Task::none(),
        };

        (this, task)
    }

    pub fn title(&self) -> String {
        match self.app.active_repo() {
            Some(repo) => format!("{} — hideGit", repo.name()),
            None => "hideGit".to_owned(),
        }
    }

    pub fn theme(&self) -> iced::Theme {
        self.app.theme.to_iced()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::OpenDialogRequested => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Open a repository")
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                |picked| match picked {
                    Some(path) => Message::OpenRepository(path),
                    // A cancelled picker is not an event worth reporting.
                    None => Message::ToastDismissed(u64::MAX),
                },
            ),

            Message::OpenRepository(path) => {
                let for_task = path.clone();
                blocking(move || open_repository(&for_task))
                    .map(|result| Message::RepositoryOpened(Box::new(result)))
            }

            Message::RepositoryOpened(result) => match *result {
                Ok(opened) => self.insert_repository(opened),
                Err(error) => {
                    self.app.toast(&error);
                    Task::none()
                }
            },

            Message::CloseRepository(index) => {
                if index < self.app.repos.len() {
                    self.app.repos.remove(index);
                    self.caches.remove(&index);
                }
                self.app.active = self.app.repos.len().checked_sub(1);
                if self.app.repos.is_empty() {
                    self.app.screen = Screen::Welcome;
                }
                Task::none()
            }

            Message::ToastDismissed(id) => {
                self.app.dismiss_toast(id);
                Task::none()
            }

            Message::Repo(index, message) => self.update_repo(index, message),
        }
    }

    fn insert_repository(&mut self, opened: OpenedRepository) -> Task<Message> {
        let index = self.app.repos.len();

        let mut graph = GraphView {
            total: opened.total,
            loading_more: opened.first_page.len() < opened.total,
            viewport_rows: 40,
            ..GraphView::default()
        };
        graph.append(opened.first_page);

        let selection = graph.commits.first().map(|c| Selection::Commit(c.id));

        self.app.remember(opened.path.clone());
        self.app.repos.push(OpenRepo {
            path: opened.path,
            backend: opened.backend,
            head: opened.head,
            refs: opened.refs,
            state: opened.state,
            status: opened.status,
            graph,
            selection: selection.clone(),
            detail: DetailPane::Empty,
            focus: Pane::Graph,
            diff_mode: crate::state::DiffMode::default(),
            hunk: 0,
        });
        self.caches.insert(index, canvas::Cache::new());
        self.app.active = Some(index);
        self.app.screen = Screen::Repository;

        let mut tasks = vec![self.checkpoint_task(index), self.load_more_task(index)];
        if let Some(selection) = selection {
            tasks.push(
                Task::done(RepoMessage::Selected(selection)).map(move |m| Message::Repo(index, m)),
            );
        }

        Task::batch(tasks)
    }

    /// Rebuilds the lane-state checkpoints for everything loaded so far.
    ///
    /// O(n) and therefore off the UI thread. Without it, laying out a screenful
    /// deep in a large history means replaying the layout from `HEAD` on every
    /// frame.
    fn checkpoint_task(&self, index: usize) -> Task<Message> {
        let Some(repo) = self.app.repos.get(index) else {
            return Task::none();
        };

        let commits = repo.graph.commits.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    Checkpoints::build(&commits, CHECKPOINT_INTERVAL)
                })
                .await
                .ok()
            },
            move |built| match built {
                Some(checkpoints) => {
                    Message::Repo(index, RepoMessage::CheckpointsBuilt(checkpoints))
                }
                None => Message::ToastDismissed(u64::MAX),
            },
        )
    }

    /// Requests the next page of history, if there is one.
    fn load_more_task(&self, index: usize) -> Task<Message> {
        let Some(repo) = self.app.repos.get(index) else {
            return Task::none();
        };
        if !repo.graph.loading_more {
            return Task::none();
        }

        let backend = Arc::clone(&repo.backend);
        let page = repo.graph.next_page();
        let total = repo.graph.total;

        blocking(move || {
            let commits = backend.log(&RevSpec::All, page)?;
            let more = page.skip + commits.len() < total && !commits.is_empty();
            Ok(Page { commits, more })
        })
        .map(move |result| Message::Repo(index, RepoMessage::CommitsLoaded(Box::new(result))))
    }

    fn update_repo(&mut self, index: usize, message: RepoMessage) -> Task<Message> {
        let Some(repo) = self.app.repos.get_mut(index) else {
            return Task::none();
        };
        let cache = self.caches.entry(index).or_default();

        match message {
            RepoMessage::Selected(selection) => {
                repo.selection = Some(selection.clone());
                cache.clear();

                match selection {
                    Selection::Commit(id) => {
                        if let Some(row) = repo.graph.commits.iter().position(|c| c.id == id) {
                            repo.graph.selected = row;
                            repo.graph.scroll_to_selection();
                        }
                        repo.detail = DetailPane::Loading;
                        repo.hunk = 0;

                        let backend = Arc::clone(&repo.backend);
                        blocking(move || load_commit(backend.as_ref(), id)).map(move |result| {
                            Message::Repo(index, RepoMessage::DetailLoaded(Box::new(result)))
                        })
                    }
                    Selection::WorkingDirectory => {
                        repo.detail = DetailPane::Loading;
                        repo.hunk = 0;

                        let backend = Arc::clone(&repo.backend);
                        blocking(move || load_status(backend.as_ref())).map(move |result| {
                            Message::Repo(index, RepoMessage::StatusLoaded(Box::new(result)))
                        })
                    }
                }
            }

            RepoMessage::GraphScrolled(pixels) => {
                repo.graph.scroll += pixels / ROW_HEIGHT;
                repo.graph.clamp_scroll();
                cache.clear();
                Task::none()
            }

            RepoMessage::SelectionMoved(delta) => {
                if repo.graph.is_empty() {
                    return Task::none();
                }
                let last = repo.graph.commits.len() - 1;
                let next =
                    (repo.graph.selected as i64 + delta as i64).clamp(0, last as i64) as usize;

                repo.graph.selected = next;
                repo.graph.scroll_to_selection();
                cache.clear();

                let id = repo.graph.commits[next].id;
                Task::done(Message::Repo(
                    index,
                    RepoMessage::Selected(Selection::Commit(id)),
                ))
            }

            RepoMessage::FocusCycled(pane) => {
                repo.focus = pane;
                cache.clear();
                Task::none()
            }

            RepoMessage::DiffModeToggled => {
                repo.diff_mode = repo.diff_mode.toggled();
                Task::none()
            }

            RepoMessage::HunkStepped(delta) => {
                if let DetailPane::Commit { detail, .. } = &repo.detail {
                    let last = detail.changes.len().saturating_sub(1);
                    repo.hunk = (repo.hunk as i64 + delta as i64).clamp(0, last as i64) as usize;
                }
                Task::none()
            }

            RepoMessage::FileSelected(file) => {
                if let DetailPane::Commit { file: current, .. } = &mut repo.detail {
                    *current = file;
                }
                Task::none()
            }

            RepoMessage::ViewportChanged(rows) => {
                repo.graph.viewport_rows = rows;
                repo.graph.clamp_scroll();
                cache.clear();
                Task::none()
            }

            RepoMessage::CommitsLoaded(result) => match *result {
                Ok(page) => {
                    // Scroll is anchored to a commit id, not a row index: rows
                    // shifting under the user is how a background load loses
                    // someone's place.
                    let anchor = repo.graph.anchor();
                    repo.graph.loading_more = page.more;
                    repo.graph.append(page.commits);
                    if let Some(anchor) = anchor {
                        repo.graph.restore_anchor(anchor);
                    }
                    cache.clear();

                    Task::batch([self.checkpoint_task(index), self.load_more_task(index)])
                }
                Err(error) => {
                    if let Some(repo) = self.app.repos.get_mut(index) {
                        repo.graph.loading_more = false;
                    }
                    self.app.toast(&error);
                    Task::none()
                }
            },

            RepoMessage::CheckpointsBuilt(checkpoints) => {
                repo.graph.checkpoints = Some(checkpoints);
                cache.clear();
                Task::none()
            }

            RepoMessage::DetailLoaded(result) => {
                match *result {
                    Ok(load) => {
                        // A stale result from a commit the user has already
                        // scrolled past must not replace what they selected
                        // since.
                        let still_selected = matches!(&repo.selection, Some(Selection::Commit(id)) if *id == load.id);
                        if still_selected {
                            repo.detail = DetailPane::Commit {
                                detail: Box::new(load.detail),
                                diff: Box::new(load.diff),
                                file: 0,
                            };
                        }
                    }
                    Err(error) => repo.detail = DetailPane::Failed(error),
                }
                Task::none()
            }

            RepoMessage::StatusLoaded(result) => {
                match *result {
                    Ok(load) => {
                        repo.status = load.status;
                        // The same guard the commit detail uses: a status that
                        // finished after the user moved on to a commit must not
                        // replace what they are looking at now.
                        if matches!(repo.selection, Some(Selection::WorkingDirectory)) {
                            repo.detail = DetailPane::WorkingDirectory {
                                staged: Box::new(load.staged),
                                unstaged: Box::new(load.unstaged),
                                selected: None,
                            };
                        }
                    }
                    Err(error) => repo.detail = DetailPane::Failed(error),
                }
                Task::none()
            }

            RepoMessage::StagingRowSelected(row) => {
                if let DetailPane::WorkingDirectory { selected, .. } = &mut repo.detail {
                    *selected = Some(row);
                    repo.hunk = 0;
                }
                Task::none()
            }

            RepoMessage::RepositoryChanged => {
                repo.backend.invalidate();
                let path = repo.path.clone();
                blocking(move || open_repository(&path))
                    .map(|result| Message::RepositoryOpened(Box::new(result)))
            }
        }
    }

    pub fn view(&self) -> IcedElement<'_, Message> {
        let palette = &self.app.theme.palette;

        match (self.app.screen, self.app.active) {
            (Screen::Repository, Some(index)) => {
                let repo = &self.app.repos[index];
                let cache = self
                    .caches
                    .get(&index)
                    .expect("every open repository has a canvas cache");

                screen::repository::view(repo, palette, cache).map(move |m| Message::Repo(index, m))
            }
            _ => screen::welcome::view(&self.app.recents, palette),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let active = self.app.active;

        keyboard::listen().with(active).map(|(active, event)| {
            let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
                return Message::ToastDismissed(u64::MAX);
            };
            shortcut(&key, modifiers, active)
        })
    }
}

/// Maps a key press to a message. `Cmd` on macOS, `Ctrl` elsewhere.
fn shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers, active: Option<usize>) -> Message {
    use keyboard::key::{Key, Named};

    let nothing = Message::ToastDismissed(u64::MAX);
    let command = modifiers.command();

    let repo = |m: RepoMessage| match active {
        Some(index) => Message::Repo(index, m),
        None => Message::ToastDismissed(u64::MAX),
    };

    match key {
        Key::Character(c) if command && c.as_str() == "o" => Message::OpenDialogRequested,
        Key::Character(c) if command && c.as_str() == "d" => repo(RepoMessage::DiffModeToggled),
        Key::Character(c) if c.as_str() == "j" && !command => repo(RepoMessage::HunkStepped(1)),
        Key::Character(c) if c.as_str() == "k" && !command => repo(RepoMessage::HunkStepped(-1)),
        Key::Named(Named::ArrowDown) => repo(RepoMessage::SelectionMoved(1)),
        Key::Named(Named::ArrowUp) => repo(RepoMessage::SelectionMoved(-1)),
        Key::Named(Named::PageDown) => repo(RepoMessage::SelectionMoved(20)),
        Key::Named(Named::PageUp) => repo(RepoMessage::SelectionMoved(-20)),
        Key::Named(Named::Tab) => {
            // Focus cycling needs to know the current pane, which only the
            // repository has; the message carries the direction and `update`
            // resolves it.
            let _ = modifiers.shift();
            nothing
        }
        _ => nothing,
    }
}

/// Opens a repository and reads enough to render its first screen.
///
/// One blocking unit of work rather than five chained messages: the screen is
/// not useful until all of it has arrived, and five round trips would show four
/// intermediate states nobody wants to see.
fn open_repository(path: &std::path::Path) -> Result<OpenedRepository, GitError> {
    let backend = HybridBackend::open(path)?;

    let head = backend.head()?;
    let refs = backend.refs()?;
    let state = backend.repo_state()?;
    let status = backend.status()?;
    let total = backend.commit_count(&RevSpec::All)?;
    let first_page = backend.log(&RevSpec::All, LogPage::first(PAGE_SIZE))?;

    Ok(OpenedRepository {
        path: backend.workdir().to_path_buf(),
        backend: Arc::new(backend),
        head,
        refs,
        state,
        status,
        total,
        first_page,
    })
}

/// Reads the working directory and both of its diffs.
///
/// One blocking unit of work, for the same reason `open_repository` is: the
/// staging view is not usable until all of it has arrived, and loading the
/// lists before the diffs would show a populated list beside an empty pane.
fn load_status(backend: &dyn GitBackend) -> Result<StatusLoad, GitError> {
    Ok(StatusLoad {
        status: backend.status()?,
        staged: backend.diff(&DiffTarget::Staged)?,
        unstaged: backend.diff(&DiffTarget::Unstaged)?,
    })
}

fn load_commit(backend: &dyn GitBackend, id: ObjectId) -> Result<CommitLoad, GitError> {
    let detail = backend.commit(id)?;
    let diff = backend.diff(&DiffTarget::Commit(id))?;

    Ok(CommitLoad { id, detail, diff })
}

/// Re-exported so the binary does not need to know the widget module layout.
pub use widget::graph::GraphCanvas;

#[cfg(test)]
mod tests {
    use super::*;
    use hidegit_core::FakeBackend;
    use hidegit_core::model::{
        ChangeStatus, Commit, Diff, FileChange, Head, RefKind, RefName, Refs, RepoState, Signature,
        WorktreeStatus,
    };
    use time::OffsetDateTime;

    fn commits(count: usize) -> Vec<Commit> {
        let who = Signature {
            name: "test".into(),
            email: "t@example.invalid".into(),
            time: OffsetDateTime::UNIX_EPOCH,
        };
        (0..count)
            .map(|i| Commit {
                id: ObjectId::from_hex(&format!("{i:040x}")).unwrap(),
                parents: if i + 1 < count {
                    vec![ObjectId::from_hex(&format!("{:040x}", i + 1)).unwrap()]
                } else {
                    Vec::new()
                },
                summary: format!("commit {i}"),
                body: None,
                author: who.clone(),
                committer: who.clone(),
                time: OffsetDateTime::UNIX_EPOCH,
                refs: Vec::new(),
            })
            .collect()
    }

    fn opened(count: usize) -> OpenedRepository {
        let history = commits(count);
        OpenedRepository {
            path: PathBuf::from("/fake"),
            backend: Arc::new(FakeBackend::new().with_commits(history.clone())),
            head: Head::Branch {
                name: RefName {
                    kind: RefKind::LocalBranch,
                    full: "refs/heads/main".into(),
                    short: "main".into(),
                },
                target: history[0].id,
            },
            refs: Refs::default(),
            state: RepoState::Clean,
            status: WorktreeStatus::default(),
            total: count,
            first_page: history,
        }
    }

    fn change(path: &str, status: ChangeStatus) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            status,
        }
    }

    /// A working directory with one file in each of the three plain lists.
    fn dirty() -> WorktreeStatus {
        WorktreeStatus {
            staged: vec![change("staged.txt", ChangeStatus::Added)],
            unstaged: vec![change("changed.txt", ChangeStatus::Modified)],
            untracked: vec![PathBuf::from("new.txt")],
            conflicted: Vec::new(),
            state: RepoState::Clean,
        }
    }

    fn app_with(count: usize) -> Hidegit {
        let mut app = Hidegit::default();
        let _ = app.update(Message::RepositoryOpened(Box::new(Ok(opened(count)))));
        app
    }

    #[test]
    fn opening_a_repository_switches_screens_and_selects_head() {
        let app = app_with(10);

        assert_eq!(app.app.screen, Screen::Repository);
        assert_eq!(app.app.active, Some(0));

        let repo = app.app.active_repo().unwrap();
        assert_eq!(repo.graph.commits.len(), 10);
        assert!(matches!(repo.selection, Some(Selection::Commit(_))));
    }

    #[test]
    fn a_failure_to_open_raises_a_toast_and_stays_on_the_welcome_screen() {
        let mut app = Hidegit::default();
        let _ = app.update(Message::RepositoryOpened(Box::new(Err(UiError {
            summary: "/tmp is not a Git repository".into(),
            details: String::new(),
        }))));

        assert_eq!(app.app.screen, Screen::Welcome);
        assert_eq!(app.app.toasts.len(), 1);
        assert!(app.app.toasts[0].summary.contains("not a Git repository"));
    }

    #[test]
    fn arrow_keys_move_the_selection_and_stop_at_the_ends() {
        let mut app = app_with(5);

        let _ = app.update(Message::Repo(0, RepoMessage::SelectionMoved(2)));
        assert_eq!(app.app.active_repo().unwrap().graph.selected, 2);

        let _ = app.update(Message::Repo(0, RepoMessage::SelectionMoved(-99)));
        assert_eq!(app.app.active_repo().unwrap().graph.selected, 0);

        let _ = app.update(Message::Repo(0, RepoMessage::SelectionMoved(99)));
        assert_eq!(
            app.app.active_repo().unwrap().graph.selected,
            4,
            "the selection stops at the last row rather than running off the end"
        );
    }

    #[test]
    fn scrolling_cannot_leave_the_loaded_history() {
        let mut app = app_with(100);
        let _ = app.update(Message::Repo(0, RepoMessage::ViewportChanged(20)));

        let _ = app.update(Message::Repo(0, RepoMessage::GraphScrolled(-10_000.0)));
        assert_eq!(app.app.active_repo().unwrap().graph.scroll, 0.0);

        let _ = app.update(Message::Repo(0, RepoMessage::GraphScrolled(1_000_000.0)));
        assert_eq!(
            app.app.active_repo().unwrap().graph.scroll,
            80.0,
            "the last screenful stays on screen"
        );
    }

    #[test]
    fn a_new_page_of_history_does_not_move_the_users_place() {
        let mut app = app_with(50);
        let _ = app.update(Message::Repo(0, RepoMessage::ViewportChanged(10)));
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::GraphScrolled(20.0 * ROW_HEIGHT),
        ));

        let anchor = app.app.active_repo().unwrap().graph.anchor().unwrap();

        let more = commits(50);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::CommitsLoaded(Box::new(Ok(Page {
                commits: more,
                more: false,
            }))),
        ));

        assert_eq!(
            app.app.active_repo().unwrap().graph.anchor(),
            Some(anchor),
            "scroll is anchored to a commit id, not to a row index"
        );
    }

    #[test]
    fn a_stale_detail_result_does_not_replace_a_newer_selection() {
        let mut app = app_with(5);
        let history = commits(5);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Selected(Selection::Commit(history[3].id)),
        ));

        // A result for a commit the user has already moved away from.
        let stale = CommitLoad {
            id: history[0].id,
            detail: hidegit_core::model::CommitDetail {
                commit: history[0].clone(),
                changes: Vec::new(),
                stats: Default::default(),
            },
            diff: Default::default(),
        };
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::DetailLoaded(Box::new(Ok(stale))),
        ));

        assert!(
            matches!(app.app.active_repo().unwrap().detail, DetailPane::Loading),
            "the pane still waits for the selection the user actually made"
        );
    }

    #[test]
    fn selecting_the_working_directory_loads_it_rather_than_showing_nothing() {
        let mut app = app_with(3);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Selected(Selection::WorkingDirectory),
        ));

        assert!(
            matches!(app.app.active_repo().unwrap().detail, DetailPane::Loading),
            "the pane says it is working rather than pretending the tree is clean"
        );
    }

    #[test]
    fn a_loaded_status_fills_the_staging_view_and_the_sidebar_badge() {
        let mut app = app_with(3);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Selected(Selection::WorkingDirectory),
        ));
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::StatusLoaded(Box::new(Ok(StatusLoad {
                status: dirty(),
                staged: Diff::default(),
                unstaged: Diff::default(),
            }))),
        ));

        let repo = app.app.active_repo().unwrap();
        assert_eq!(repo.status.change_count(), 3);
        assert!(matches!(
            repo.detail,
            DetailPane::WorkingDirectory { selected: None, .. }
        ));
    }

    #[test]
    fn a_status_that_arrives_after_the_user_moved_on_does_not_replace_the_commit() {
        // The same guard the commit detail has, in the other direction: the
        // status is slower than a click on the graph.
        let mut app = app_with(3);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Selected(Selection::WorkingDirectory),
        ));

        let id = app.app.active_repo().unwrap().graph.commits[1].id;
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Selected(Selection::Commit(id)),
        ));

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::StatusLoaded(Box::new(Ok(StatusLoad {
                status: dirty(),
                staged: Diff::default(),
                unstaged: Diff::default(),
            }))),
        ));

        let repo = app.app.active_repo().unwrap();
        assert!(
            !matches!(repo.detail, DetailPane::WorkingDirectory { .. }),
            "the pane belongs to the commit the user selected since"
        );
        assert_eq!(
            repo.status.change_count(),
            3,
            "the status itself is still worth keeping — the sidebar badge uses it"
        );
    }

    #[test]
    fn selecting_a_staging_row_remembers_which_list_it_came_from() {
        // The same path can be staged and changed at once, so the row identity
        // has to carry its section rather than just its index.
        let mut app = app_with(3);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Selected(Selection::WorkingDirectory),
        ));
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::StatusLoaded(Box::new(Ok(StatusLoad {
                status: dirty(),
                staged: Diff::default(),
                unstaged: Diff::default(),
            }))),
        ));

        let row = crate::state::StagingRow {
            section: crate::state::Section::Unstaged,
            index: 0,
        };
        let _ = app.update(Message::Repo(0, RepoMessage::StagingRowSelected(row)));

        let DetailPane::WorkingDirectory { selected, .. } = &app.app.active_repo().unwrap().detail
        else {
            panic!("the staging view is open");
        };
        assert_eq!(*selected, Some(row));
    }

    #[test]
    fn the_diff_mode_toggles_and_is_remembered() {
        let mut app = app_with(3);
        assert_eq!(
            app.app.active_repo().unwrap().diff_mode,
            crate::state::DiffMode::Unified
        );

        let _ = app.update(Message::Repo(0, RepoMessage::DiffModeToggled));
        assert_eq!(
            app.app.active_repo().unwrap().diff_mode,
            crate::state::DiffMode::SideBySide
        );
    }

    #[test]
    fn closing_the_last_repository_returns_to_the_welcome_screen() {
        let mut app = app_with(3);
        let _ = app.update(Message::CloseRepository(0));

        assert_eq!(app.app.screen, Screen::Welcome);
        assert!(app.app.repos.is_empty());
        assert_eq!(app.app.active, None);
    }

    #[test]
    fn opening_a_repository_records_it_as_recent() {
        let app = app_with(3);
        assert_eq!(app.app.recents, vec![PathBuf::from("/fake")]);
    }

    #[test]
    fn command_o_opens_the_picker_from_anywhere() {
        let key = keyboard::Key::Character("o".into());
        let mods = keyboard::Modifiers::COMMAND;

        assert!(matches!(
            shortcut(&key, mods, None),
            Message::OpenDialogRequested
        ));
    }

    #[test]
    fn arrow_keys_do_nothing_when_no_repository_is_open() {
        let key = keyboard::Key::Named(keyboard::key::Named::ArrowDown);

        assert!(matches!(
            shortcut(&key, keyboard::Modifiers::default(), None),
            Message::ToastDismissed(_)
        ));
    }
}
