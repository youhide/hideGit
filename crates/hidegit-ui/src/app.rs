//! `update`, `view` and `subscription` — the Elm loop.
//!
//! The one rule this file exists to enforce: **nothing blocking runs here.**
//! `gix` calls and `git` subprocesses are blocking, so every one of them goes
//! through [`blocking`] onto tokio's blocking pool and comes back as another
//! message. `update` itself only ever touches memory.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use hidegit_core::graph::Checkpoints;
use hidegit_core::model::{DiffTarget, LogPage, ObjectId, RevSpec};
use hidegit_core::ops::{CommitOpts, Patch};
use hidegit_core::patch::Selection as PatchSelection;
use hidegit_core::{GitBackend, GitError, HybridBackend};
use iced::widget::canvas;
use iced::{Element as IcedElement, Subscription, Task, keyboard};

use crate::message::{
    CommitLoad, Message, OpenedRepository, Page, Refreshed, RepoMessage, StatusLoad, UiError,
};
use crate::state::{
    App, CHECKPOINT_INTERVAL, Confirmation, DetailPane, Draft, GraphView, OpenRepo, PAGE_SIZE,
    Pane, ROW_HEIGHT, Screen, Section, Selection,
};
use crate::{screen, watcher, widget};

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

            Message::ConfirmationAccepted => match self.app.confirming.take() {
                Some(confirmation) => Task::done(*confirmation.action),
                None => Task::none(),
            },

            Message::ConfirmationDismissed => {
                self.app.confirming = None;
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
            draft: Draft::default(),
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

    /// Turns a selection into a patch and applies it to the index.
    ///
    /// Which direction it goes is decided by which list the open row is in:
    /// a staged file's patch is applied in reverse, which is what unstaging a
    /// hunk means.
    fn apply_patch(&mut self, index: usize, selection: PatchSelection) -> Task<Message> {
        let Some(repo) = self.app.repos.get(index) else {
            return Task::none();
        };
        let DetailPane::WorkingDirectory {
            staged,
            unstaged,
            selected: Some(row),
            ..
        } = &repo.detail
        else {
            return Task::none();
        };

        let (diff, reverse) = match row.section {
            Section::Staged => (staged, true),
            Section::Unstaged => (unstaged, false),
            // Untracked files have no diff to slice, and conflicts are M5.
            _ => return Task::none(),
        };
        let Some(file) = diff.files.get(row.index) else {
            return Task::none();
        };
        // A binary file, or a selection that resolves to nothing: there is no
        // patch to build, and an empty one is an error to `git apply` rather
        // than a no-op.
        let Some(text) = hidegit_core::patch::serialize(file, &selection) else {
            return Task::none();
        };

        let patch = Patch {
            file: file.path.clone(),
            text,
            reverse,
        };
        let backend = Arc::clone(&repo.backend);
        write_task(index, move || backend.stage_patch(&patch))
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
                            // The open row is kept across a refresh so staging
                            // one hunk does not close the file the user is
                            // working through. Line indices do not survive:
                            // they meant something about a diff that no longer
                            // exists.
                            let previous = match &repo.detail {
                                DetailPane::WorkingDirectory { selected, .. } => *selected,
                                _ => None,
                            };
                            repo.detail = DetailPane::WorkingDirectory {
                                staged: Box::new(load.staged),
                                unstaged: Box::new(load.unstaged),
                                selected: previous,
                                lines: BTreeSet::new(),
                            };
                        }
                    }
                    Err(error) => repo.detail = DetailPane::Failed(error),
                }
                Task::none()
            }

            RepoMessage::DiscardSelectedRequested => {
                let Some((section, path)) = selected_path(repo) else {
                    return Task::none();
                };
                // Discarding a staged change would mean unstaging and then
                // throwing it away, which is two decisions wearing one key.
                if matches!(section, Section::Staged | Section::Conflicted) {
                    return Task::none();
                }
                Task::done(Message::Repo(
                    index,
                    RepoMessage::DiscardRequested(vec![path]),
                ))
            }

            RepoMessage::StageRequested(paths) => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.stage(&borrowed(&paths)))
            }

            RepoMessage::UnstageRequested(paths) => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.unstage(&borrowed(&paths)))
            }

            // Discarding does not act. It asks, naming what will be lost —
            // there is no undo behind this one, so a generic confirmation
            // would be the wrong shape.
            RepoMessage::DiscardRequested(paths) => {
                self.app.confirming = Some(Confirmation {
                    title: "Discard changes?".to_owned(),
                    body: describe_discard(&paths),
                    confirm_label: "Discard".to_owned(),
                    action: Box::new(Message::Repo(index, RepoMessage::DiscardConfirmed(paths))),
                });
                Task::none()
            }

            RepoMessage::DiscardConfirmed(paths) => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.discard(&borrowed(&paths)))
            }

            RepoMessage::WriteFinished(result) => {
                match *result {
                    // Success has no message of its own: the refresh that
                    // follows is the result, and a toast saying "staged" for
                    // every click would be noise.
                    Ok(()) => Task::done(Message::Repo(index, RepoMessage::RepositoryChanged)),
                    Err(error) => {
                        self.app.toast(&error);
                        Task::none()
                    }
                }
            }

            RepoMessage::StagingRowSelected(row) => {
                if let DetailPane::WorkingDirectory {
                    selected, lines, ..
                } = &mut repo.detail
                {
                    // Indices are relative to the open file's diff, so opening
                    // a different one makes them meaningless rather than stale.
                    if *selected != Some(row) {
                        lines.clear();
                    }
                    *selected = Some(row);
                    repo.hunk = 0;
                }
                Task::none()
            }

            RepoMessage::SubjectChanged(text) => {
                repo.draft.editing = true;
                // Newlines are how a subject stops being a subject. Pasting a
                // whole message into the field folds the rest into the body
                // rather than silently producing a one-line commit.
                match text.split_once('\n') {
                    Some((subject, rest)) => {
                        repo.draft.subject = subject.to_owned();
                        if !rest.trim().is_empty() {
                            if !repo.draft.body.is_empty() {
                                repo.draft.body.push('\n');
                            }
                            repo.draft.body.push_str(rest.trim_start_matches('\n'));
                        }
                    }
                    None => repo.draft.subject = text,
                }
                Task::none()
            }

            RepoMessage::BodyChanged(text) => {
                repo.draft.editing = true;
                repo.draft.body = text;
                Task::none()
            }

            RepoMessage::AmendToggled(on) => {
                repo.draft.amend = on;
                // Amending starts from the message being replaced, the way
                // `git commit --amend` opens an editor already holding it.
                if on && repo.draft.subject.trim().is_empty() {
                    if let Some(head) = repo.graph.commits.first() {
                        repo.draft.subject = head.summary.clone();
                        repo.draft.body = head.body.clone().unwrap_or_default();
                    }
                }
                Task::none()
            }

            RepoMessage::SignOffToggled(on) => {
                repo.draft.sign_off = on;
                Task::none()
            }

            RepoMessage::EditingChanged(editing) => {
                repo.draft.editing = editing;
                Task::none()
            }

            RepoMessage::CommitRequested => {
                // A repository mid-rebase does not offer to commit, and an
                // empty subject is not a message Git will accept.
                if repo.state.is_in_progress() || !repo.draft.is_ready() {
                    return Task::none();
                }

                let message = repo.draft.message();
                let opts = CommitOpts {
                    amend: repo.draft.amend,
                    sign_off: repo.draft.sign_off,
                    allow_empty: false,
                };
                let backend = Arc::clone(&repo.backend);

                blocking(move || backend.create_commit(&message, opts)).map(move |result| {
                    Message::Repo(index, RepoMessage::Committed(Box::new(result)))
                })
            }

            RepoMessage::Committed(result) => match *result {
                Ok(_) => {
                    // The draft is only cleared once the commit actually
                    // landed. A failed hook must not cost the user the message
                    // they wrote.
                    repo.draft = Draft::default();
                    Task::done(Message::Repo(index, RepoMessage::RepositoryChanged))
                }
                Err(error) => {
                    self.app.toast(&error);
                    Task::none()
                }
            },

            RepoMessage::LineToggled(hunk, line) => {
                if let DetailPane::WorkingDirectory { lines, .. } = &mut repo.detail
                    && !lines.insert((hunk, line))
                {
                    lines.remove(&(hunk, line));
                }
                Task::none()
            }

            RepoMessage::HunkStageRequested(hunk) => {
                self.apply_patch(index, PatchSelection::hunk(hunk))
            }

            RepoMessage::FileStageRequested => {
                self.apply_patch(index, PatchSelection::everything())
            }

            RepoMessage::SelectedLinesStageRequested => {
                let DetailPane::WorkingDirectory { lines, .. } = &repo.detail else {
                    return Task::none();
                };
                let mut selection = PatchSelection::default();
                for (hunk, line) in lines {
                    selection = selection.with_lines(*hunk, [*line]);
                }
                self.apply_patch(index, selection)
            }

            // Reread and apply in place. Reopening would push a *second* entry
            // for the same repository and reset the user's scroll and
            // selection — every write, and eventually every file save once the
            // watcher exists.
            RepoMessage::RepositoryChanged => {
                repo.backend.invalidate();
                let backend = Arc::clone(&repo.backend);
                blocking(move || reread(backend.as_ref())).map(move |result| {
                    Message::Repo(index, RepoMessage::Refreshed(Box::new(result)))
                })
            }

            RepoMessage::Refreshed(result) => {
                let refreshed = match *result {
                    Ok(refreshed) => refreshed,
                    Err(error) => {
                        self.app.toast(&error);
                        return Task::none();
                    }
                };

                repo.head = refreshed.head;
                repo.refs = refreshed.refs;
                repo.state = refreshed.state;
                repo.status = refreshed.status;

                // Restored by commit id rather than row index: a new commit at
                // HEAD shifts every row down, and a refresh must not silently
                // move the user's place.
                let anchor = repo.graph.anchor();
                let selected = repo.graph.commits.get(repo.graph.selected).map(|c| c.id);

                repo.graph.commits.clear();
                repo.graph.known.clear();
                repo.graph.total = refreshed.total;
                repo.graph.loading_more = refreshed.first_page.len() < refreshed.total;
                repo.graph.append(refreshed.first_page);

                if let Some(anchor) = anchor {
                    repo.graph.restore_anchor(anchor);
                }
                if let Some(id) = selected
                    && let Some(row) = repo.graph.commits.iter().position(|c| c.id == id)
                {
                    repo.graph.selected = row;
                }
                cache.clear();

                let mut tasks = vec![self.checkpoint_task(index)];
                // The staging view holds diffs, which the write just changed.
                // Reselecting reloads them through the one path that knows how.
                if matches!(
                    self.app.repos[index].selection,
                    Some(Selection::WorkingDirectory)
                ) {
                    tasks.push(Task::done(Message::Repo(
                        index,
                        RepoMessage::Selected(Selection::WorkingDirectory),
                    )));
                }
                Task::batch(tasks)
            }
        }
    }

    pub fn view(&self) -> IcedElement<'_, Message> {
        let palette = &self.app.theme.palette;

        let base = self.screen();
        widget::overlay::wrap(
            base,
            self.app.confirming.as_ref(),
            &self.app.toasts,
            palette,
        )
    }

    /// The screen itself, without the confirmation and toast layers.
    fn screen(&self) -> IcedElement<'_, Message> {
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
        let context = (
            self.app.active,
            self.app.confirming.is_some(),
            self.app
                .active_repo()
                .is_some_and(|repo| repo.draft.editing),
        );

        let keys =
            keyboard::listen()
                .with(context)
                .map(|((active, confirming, editing), event)| {
                    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
                        return Message::ToastDismissed(u64::MAX);
                    };
                    // A modal question owns the keyboard while it is up. Letting
                    // `Space` stage something behind a "discard?" dialog would be
                    // the worst possible moment to act on a stray key.
                    if confirming {
                        return modal_shortcut(&key);
                    }
                    shortcut(&key, modifiers, active, editing)
                });

        // One watch per open repository, so a change made in an editor or by a
        // `git` command in a terminal refreshes the view on its own.
        let watches = self.app.repos.iter().enumerate().map(|(index, repo)| {
            watcher::subscribe(
                index,
                repo.path.clone(),
                repo.backend.git_dir().to_path_buf(),
            )
        });

        Subscription::batch(std::iter::once(keys).chain(watches))
    }
}

/// Maps a key press to a message. `Cmd` on macOS, `Ctrl` elsewhere.
fn shortcut(
    key: &keyboard::Key,
    modifiers: keyboard::Modifiers,
    active: Option<usize>,
    editing: bool,
) -> Message {
    use keyboard::key::{Key, Named};

    let nothing = Message::ToastDismissed(u64::MAX);
    let command = modifiers.command();

    // While a text field has focus, every unmodified key belongs to it.
    // `keyboard::listen()` is global and `j`, `k` and `Space` are all bound, so
    // without this, typing a commit message navigates hunks and stages files.
    if editing && !command {
        return nothing;
    }

    let repo = |m: RepoMessage| match active {
        Some(index) => Message::Repo(index, m),
        None => Message::ToastDismissed(u64::MAX),
    };

    match key {
        Key::Named(Named::Enter) if command => repo(RepoMessage::CommitRequested),
        Key::Named(Named::Backspace) if command => repo(RepoMessage::DiscardSelectedRequested),
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

/// The only keys a confirmation dialog answers to.
fn modal_shortcut(key: &keyboard::Key) -> Message {
    use keyboard::key::{Key, Named};

    match key {
        Key::Named(Named::Escape) => Message::ConfirmationDismissed,
        Key::Named(Named::Enter) => Message::ConfirmationAccepted,
        _ => Message::ToastDismissed(u64::MAX),
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

/// Runs a repository write off the UI thread.
///
/// Every write reports the same way: nothing on success, because the refresh
/// that follows says more than a toast could, and the error verbatim on
/// failure — Git's own stderr is better than any paraphrase of it.
fn write_task<F>(index: usize, f: F) -> Task<Message>
where
    F: FnOnce() -> Result<(), GitError> + Send + 'static,
{
    blocking(f)
        .map(move |result| Message::Repo(index, RepoMessage::WriteFinished(Box::new(result))))
}

/// The path the staging view currently has open, and which list it is in.
///
/// Reads the path out of `status` rather than the diff, because an untracked
/// file has no diff to read it from.
fn selected_path(repo: &OpenRepo) -> Option<(Section, PathBuf)> {
    let DetailPane::WorkingDirectory {
        selected: Some(row),
        ..
    } = &repo.detail
    else {
        return None;
    };

    let path = match row.section {
        Section::Staged => repo.status.staged.get(row.index).map(|c| c.path.clone()),
        Section::Unstaged => repo.status.unstaged.get(row.index).map(|c| c.path.clone()),
        Section::Untracked => repo.status.untracked.get(row.index).cloned(),
        Section::Conflicted => repo
            .status
            .conflicted
            .get(row.index)
            .map(|c| c.path.clone()),
    }?;

    Some((row.section, path))
}

/// Borrows owned paths for the `&[&Path]` the backend takes.
fn borrowed(paths: &[PathBuf]) -> Vec<&std::path::Path> {
    paths.iter().map(PathBuf::as_path).collect()
}

/// Names what a discard will destroy, rather than asking "are you sure?".
fn describe_discard(paths: &[PathBuf]) -> String {
    let what = match paths {
        [one] => format!("Changes to {} will be lost.", one.display()),
        many => format!("Changes to {} files will be lost.", many.len()),
    };
    format!("{what} This cannot be undone.")
}

/// Rereads everything a change to the repository can affect.
///
/// The counterpart to `open_repository`, and deliberately separate from it:
/// opening creates an entry, refreshing updates one.
fn reread(backend: &dyn GitBackend) -> Result<Refreshed, GitError> {
    Ok(Refreshed {
        head: backend.head()?,
        refs: backend.refs()?,
        state: backend.repo_state()?,
        status: backend.status()?,
        total: backend.commit_count(&RevSpec::All)?,
        first_page: backend.log(&RevSpec::All, LogPage::first(PAGE_SIZE))?,
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

    /// Opens the staging view with `dirty()` loaded and a row selected.
    fn staging(section: crate::state::Section, index: usize) -> Hidegit {
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
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::StagingRowSelected(crate::state::StagingRow { section, index }),
        ));
        app
    }

    #[test]
    fn a_focused_text_field_swallows_the_bare_letter_shortcuts() {
        // `keyboard::listen()` is global and `j`/`k` are bound unmodified, so
        // without this, typing a commit message steps through hunks.
        let j = keyboard::Key::Character("j".into());
        let space = keyboard::Key::Named(keyboard::key::Named::Space);
        let mods = keyboard::Modifiers::default();

        assert!(matches!(
            shortcut(&j, mods, Some(0), false),
            Message::Repo(0, RepoMessage::HunkStepped(1))
        ));
        assert!(matches!(
            shortcut(&j, mods, Some(0), true),
            Message::ToastDismissed(_)
        ));

        // `Space` is not a global shortcut at all. It is the one bare key whose
        // leak would be destructive, and iced 0.14 offers no way to know a text
        // field holds focus until its first keystroke has already arrived.
        assert!(matches!(
            shortcut(&space, mods, Some(0), false),
            Message::ToastDismissed(_)
        ));
    }

    #[test]
    fn a_command_shortcut_still_works_while_typing() {
        // `Cmd+Enter` has to commit from inside the message field, which is
        // where the user's hands already are.
        let enter = keyboard::Key::Named(keyboard::key::Named::Enter);
        let command = keyboard::Modifiers::COMMAND;

        assert!(matches!(
            shortcut(&enter, command, Some(0), true),
            Message::Repo(0, RepoMessage::CommitRequested)
        ));
    }

    #[test]
    fn a_draft_becomes_the_message_git_stores() {
        let mut draft = Draft {
            subject: "  The subject  ".into(),
            body: "  The body.  ".into(),
            ..Draft::default()
        };
        assert_eq!(draft.message(), "The subject\n\nThe body.\n");

        draft.body = String::new();
        assert_eq!(
            draft.message(),
            "The subject",
            "no trailing blank line when there is no body"
        );
    }

    #[test]
    fn committing_needs_a_subject_and_a_repository_that_is_not_mid_operation() {
        let mut app = app_with(3);

        // No subject yet.
        let _ = app.update(Message::Repo(0, RepoMessage::CommitRequested));
        assert!(app.app.toasts.is_empty(), "nothing was attempted");

        app.app.repos[0].draft.subject = "Subject".into();
        app.app.repos[0].state = RepoState::Rebasing;
        let _ = app.update(Message::Repo(0, RepoMessage::CommitRequested));
        assert!(
            app.app.toasts.is_empty(),
            "a repository mid-rebase does not offer to commit"
        );
    }

    #[test]
    fn a_failed_commit_keeps_the_message_the_user_wrote() {
        // A rejected hook must not cost them the text.
        let mut app = app_with(3);
        app.app.repos[0].draft.subject = "Subject".into();
        app.app.repos[0].draft.body = "Body".into();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Committed(Box::new(Err(UiError {
                summary: "the pre-commit hook failed".into(),
                details: String::new(),
            }))),
        ));

        let draft = &app.app.active_repo().unwrap().draft;
        assert_eq!(draft.subject, "Subject");
        assert_eq!(draft.body, "Body");
        assert_eq!(app.app.toasts.len(), 1);
    }

    #[test]
    fn a_successful_commit_clears_the_draft() {
        let mut app = app_with(3);
        app.app.repos[0].draft.subject = "Subject".into();
        app.app.repos[0].draft.amend = true;

        let id = ObjectId::from_hex(&format!("{:040x}", 7)).unwrap();
        let _ = app.update(Message::Repo(0, RepoMessage::Committed(Box::new(Ok(id)))));

        let draft = &app.app.active_repo().unwrap().draft;
        assert!(draft.subject.is_empty());
        assert!(!draft.amend, "and stops amending");
    }

    #[test]
    fn a_pasted_message_folds_its_extra_lines_into_the_body() {
        // Newlines are how a subject stops being a subject.
        let mut app = app_with(3);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::SubjectChanged("Subject line\nand the rest".into()),
        ));

        let draft = &app.app.active_repo().unwrap().draft;
        assert_eq!(draft.subject, "Subject line");
        assert_eq!(draft.body, "and the rest");
    }

    #[test]
    fn toggling_a_line_adds_it_and_toggling_again_takes_it_back_out() {
        let mut app = staging(crate::state::Section::Unstaged, 0);

        let _ = app.update(Message::Repo(0, RepoMessage::LineToggled(0, 2)));
        let DetailPane::WorkingDirectory { lines, .. } = &app.app.active_repo().unwrap().detail
        else {
            panic!("the staging view is open");
        };
        assert!(lines.contains(&(0, 2)));

        let _ = app.update(Message::Repo(0, RepoMessage::LineToggled(0, 2)));
        let DetailPane::WorkingDirectory { lines, .. } = &app.app.active_repo().unwrap().detail
        else {
            panic!("the staging view is open");
        };
        assert!(lines.is_empty());
    }

    #[test]
    fn opening_a_different_file_drops_the_line_selection() {
        // The indices meant something about the previous file's diff. Carrying
        // them over would stage whatever happened to sit at those positions.
        let mut app = staging(crate::state::Section::Unstaged, 0);
        let _ = app.update(Message::Repo(0, RepoMessage::LineToggled(0, 1)));

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::StagingRowSelected(crate::state::StagingRow {
                section: crate::state::Section::Staged,
                index: 0,
            }),
        ));

        let DetailPane::WorkingDirectory { lines, .. } = &app.app.active_repo().unwrap().detail
        else {
            panic!("the staging view is open");
        };
        assert!(lines.is_empty());
    }

    #[test]
    fn a_refresh_keeps_the_open_file_but_drops_the_line_selection() {
        // Staging one hunk of a file should leave you looking at the rest of
        // it, not back at the list — but the line indices describe a diff that
        // no longer exists.
        let mut app = staging(crate::state::Section::Unstaged, 0);
        let _ = app.update(Message::Repo(0, RepoMessage::LineToggled(0, 1)));

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::StatusLoaded(Box::new(Ok(StatusLoad {
                status: dirty(),
                staged: Diff::default(),
                unstaged: Diff::default(),
            }))),
        ));

        let DetailPane::WorkingDirectory {
            selected, lines, ..
        } = &app.app.active_repo().unwrap().detail
        else {
            panic!("the staging view is open");
        };
        assert_eq!(
            *selected,
            Some(crate::state::StagingRow {
                section: crate::state::Section::Unstaged,
                index: 0
            }),
            "the file the user was working through stayed open"
        );
        assert!(lines.is_empty(), "but its line selection did not");
    }

    #[test]
    fn staging_a_hunk_of_a_file_with_no_diff_loaded_does_nothing() {
        // `dirty()` carries no diffs, so there is no patch to build. The guard
        // matters because an empty patch is an error to `git apply`, not a
        // no-op.
        let mut app = staging(crate::state::Section::Unstaged, 0);
        let _ = app.update(Message::Repo(0, RepoMessage::HunkStageRequested(0)));

        assert!(
            app.app.toasts.is_empty(),
            "and it does not report a failure"
        );
    }

    #[test]
    fn a_refresh_updates_the_repository_rather_than_opening_a_second_one() {
        // The bug this guards: `RepositoryChanged` used to reopen, and opening
        // pushes a new entry. Every write appended a copy of the repository and
        // reset the user's place — and the filesystem watcher will fire this on
        // every file save.
        // More commits than the viewport is tall, or the scroll position
        // clamps to zero and there is nothing to preserve.
        let mut app = app_with(100);
        let anchor = app.app.active_repo().unwrap().graph.commits[50].id;
        app.app.repos[0].graph.scroll = 50.0;
        app.app.repos[0].graph.selected = 50;

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Refreshed(Box::new(Ok(Refreshed {
                head: opened(100).head,
                refs: Refs::default(),
                state: RepoState::Clean,
                status: dirty(),
                total: 100,
                first_page: commits(100),
            }))),
        ));

        assert_eq!(app.app.repos.len(), 1, "one repository, not two");
        let repo = app.app.active_repo().unwrap();
        assert_eq!(
            repo.graph.commits[repo.graph.first_visible()].id,
            anchor,
            "the viewport stayed where the user left it"
        );
        assert_eq!(repo.graph.selected, 50, "and so did the selection");
        assert_eq!(repo.status.change_count(), 3, "the new status was applied");
    }

    #[test]
    fn a_refresh_keeps_the_users_place_even_when_a_commit_arrived_at_head() {
        // Row indices all shift by one; the anchor is a commit id for exactly
        // this reason.
        let mut app = app_with(100);
        let anchor = app.app.active_repo().unwrap().graph.commits[50].id;
        app.app.repos[0].graph.scroll = 50.0;

        // A history one longer, with the same commits pushed down by a new one.
        let mut grown = vec![Commit {
            id: ObjectId::from_hex(&format!("{:040x}", 999)).unwrap(),
            parents: vec![commits(100)[0].id],
            summary: "brand new".into(),
            body: None,
            author: commits(1)[0].author.clone(),
            committer: commits(1)[0].committer.clone(),
            time: OffsetDateTime::UNIX_EPOCH,
            refs: Vec::new(),
        }];
        grown.extend(commits(100));

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Refreshed(Box::new(Ok(Refreshed {
                head: opened(100).head,
                refs: Refs::default(),
                state: RepoState::Clean,
                status: WorktreeStatus::default(),
                total: 101,
                first_page: grown,
            }))),
        ));

        let repo = app.app.active_repo().unwrap();
        assert_eq!(
            repo.graph.commits[repo.graph.first_visible()].id,
            anchor,
            "the same commit is at the top, one row further down than before"
        );
        assert_eq!(repo.graph.first_visible(), 51);
    }

    #[test]
    fn discarding_asks_before_it_acts_and_names_what_is_lost() {
        let mut app = app_with(1);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::DiscardRequested(vec![PathBuf::from("changed.txt")]),
        ));

        let confirmation = app
            .app
            .confirming
            .as_ref()
            .expect("discarding raises a confirmation rather than acting");
        assert!(
            confirmation.body.contains("changed.txt"),
            "it names the file: {}",
            confirmation.body
        );
        assert!(confirmation.body.contains("cannot be undone"));
        assert_eq!(
            confirmation.confirm_label, "Discard",
            "the button says what it does, not OK"
        );
    }

    #[test]
    fn dismissing_a_confirmation_drops_the_action_entirely() {
        let mut app = app_with(1);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::DiscardRequested(vec![PathBuf::from("changed.txt")]),
        ));
        let _ = app.update(Message::ConfirmationDismissed);

        assert!(app.app.confirming.is_none());
    }

    #[test]
    fn a_failed_write_becomes_a_toast_carrying_gits_own_words() {
        let mut app = app_with(1);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::WriteFinished(Box::new(Err(UiError {
                summary: "git add failed".into(),
                details: "fatal: pathspec did not match any files".into(),
            }))),
        ));

        assert_eq!(app.app.toasts.len(), 1);
        assert_eq!(
            app.app.toasts[0].details, "fatal: pathspec did not match any files",
            "the toast keeps stderr verbatim rather than paraphrasing it"
        );
    }

    #[test]
    fn the_selected_row_is_resolved_from_the_section_it_sits_in() {
        // `Cmd+Backspace` and the row buttons both need the path behind the
        // open row, and the same path can sit in two sections at once.
        let app = staging(crate::state::Section::Unstaged, 0);
        let repo = app.app.active_repo().unwrap();
        assert_eq!(
            selected_path(repo),
            Some((
                crate::state::Section::Unstaged,
                PathBuf::from("changed.txt")
            ))
        );

        let app = staging(crate::state::Section::Staged, 0);
        let repo = app.app.active_repo().unwrap();
        assert_eq!(
            selected_path(repo),
            Some((crate::state::Section::Staged, PathBuf::from("staged.txt")))
        );
    }

    #[test]
    fn discarding_a_staged_row_by_keyboard_does_nothing() {
        // Throwing away a staged change is unstage-then-destroy: two decisions,
        // so one key must not stand for both.
        let mut app = staging(crate::state::Section::Staged, 0);
        let _ = app.update(Message::Repo(0, RepoMessage::DiscardSelectedRequested));

        assert!(app.app.confirming.is_none());
    }

    #[test]
    fn a_modal_dialog_owns_the_keyboard() {
        // Staging something behind an unanswered "discard?" would be the worst
        // possible moment to act on a stray key.
        assert!(matches!(
            modal_shortcut(&keyboard::Key::Named(keyboard::key::Named::Escape)),
            Message::ConfirmationDismissed
        ));
        assert!(matches!(
            modal_shortcut(&keyboard::Key::Named(keyboard::key::Named::Enter)),
            Message::ConfirmationAccepted
        ));
        assert!(matches!(
            modal_shortcut(&keyboard::Key::Named(keyboard::key::Named::Space)),
            Message::ToastDismissed(_)
        ));
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
            shortcut(&key, mods, None, false),
            Message::OpenDialogRequested
        ));
    }

    #[test]
    fn arrow_keys_do_nothing_when_no_repository_is_open() {
        let key = keyboard::Key::Named(keyboard::key::Named::ArrowDown);

        assert!(matches!(
            shortcut(&key, keyboard::Modifiers::default(), None, false),
            Message::ToastDismissed(_)
        ));
    }
}
