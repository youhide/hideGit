//! `update`, `view` and `subscription` — the Elm loop.
//!
//! The one rule this file exists to enforce: **nothing blocking runs here.**
//! `gix` calls and `git` subprocesses are blocking, so every one of them goes
//! through [`blocking`] onto tokio's blocking pool and comes back as another
//! message. `update` itself only ever touches memory.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use hidegit_core::conflict::Resolution;
use hidegit_core::graph::Checkpoints;
use hidegit_core::model::{DiffTarget, LogPage, ObjectId, RepoState, RevSpec};
use hidegit_core::ops::{
    CancelToken, CheckoutTarget, CommitOpts, FetchOpts, ForceMode, MergeOpts, Patch, ProgressSink,
    ProgressUpdate, PullOpts, PullOutcome, PushSpec, RebasePlan, ResetMode, SearchQuery,
    SequenceControl, SequenceOutcome, StartPoint, StashOp, TagSpec,
};
use hidegit_core::patch::Selection as PatchSelection;
use hidegit_core::{GitBackend, GitError, HybridBackend};
use iced::widget::{canvas, text_editor};
use iced::{Element as IcedElement, Subscription, Task, keyboard};

use hidegit_forge::{NewPullRequest, WebTarget};

use crate::message::{
    CommitLoad, Message, OpenedRepository, OperationOutcome, Page, PrsLoad, Refreshed, RepoMessage,
    StatusLoad, UiError,
};
use crate::state::{
    ActionSheet, App, CHECKPOINT_INTERVAL, Confirmation, DetailPane, Draft, GraphView, OpenRepo,
    Operation, PAGE_SIZE, PROMPT_FIELD_IDS, Pane, PrPanel, PrState, Prompt, PromptField,
    PromptKind, ROW_HEIGHT, RebaseEditor, Resolver, Screen, Section, Selection,
};
use crate::widget::tabs::tab_for_digit;
use crate::{alerts, forge, screen, watcher, widget};

/// How many search hits to stop at.
///
/// The list is scrolled, not paged, and a search matching ten thousand commits
/// needs narrowing rather than a longer list. The result says when it stopped
/// here, so the count is never mistaken for the whole answer.
const SEARCH_LIMIT: usize = 200;

/// The application, plus the bits of view state that are not domain state.
#[derive(Debug, Default)]
pub struct Hidegit {
    pub app: App,
    /// Cached graph geometry, one per repository, keyed by its index.
    ///
    /// Redraws that do not move the viewport or change the commit list reuse it
    /// rather than rebuilding every row.
    caches: HashMap<usize, canvas::Cache>,
    /// Monotonic, so a cancelled operation's late messages can be told from the
    /// operation that replaced it.
    next_operation_id: u64,
    /// The clone in flight, if any.
    ///
    /// Held on the application rather than on a repository, because a clone happens
    /// before there is one to hold it.
    cloning: Option<Operation>,
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

/// Feeds a [`ProgressSink`] into a channel the UI can await.
///
/// The adapter `hidegit-core` promises when it says progress goes through a trait
/// object so it stays free of any async runtime. `hidegit-core` reports; this
/// turns each report into a `Message`.
/// `futures`' channel rather than tokio's, because iced re-exports it already and
/// `unbounded_send` works from a blocking thread without an async context.
struct ChannelSink(iced::futures::channel::mpsc::UnboundedSender<ProgressUpdate>);

impl ProgressSink for ChannelSink {
    fn report(&self, update: ProgressUpdate) {
        // A closed receiver means the UI stopped listening. The work carries on —
        // it is cancellation that stops it, not a missing audience.
        let _ = self.0.unbounded_send(update);
    }
}

/// Runs a long operation, delivering its progress and then its result.
///
/// The difference from [`blocking`] is that this yields *many* messages. A network
/// operation is a one-shot, so it is a `Task::stream` rather than a long-lived
/// `Subscription`: the stream ends when the work does.
///
/// The sink is moved into the blocking closure, so the sender is dropped exactly
/// when the work returns — which is what tells the stream to stop waiting for
/// progress and go and collect the result.
fn streaming<F>(index: usize, id: u64, cancel: CancelToken, f: F) -> Task<Message>
where
    F: FnOnce(&dyn ProgressSink, &CancelToken) -> Result<OperationOutcome, GitError>
        + Send
        + 'static,
{
    let (sender, receiver) = iced::futures::channel::mpsc::unbounded();
    let worker = tokio::task::spawn_blocking(move || {
        let sink = ChannelSink(sender);
        f(&sink, &cancel)
    });

    let stream = iced::futures::stream::unfold(Some((receiver, worker)), move |state| async move {
        use iced::futures::StreamExt as _;
        let (mut receiver, worker) = state?;

        match receiver.next().await {
            Some(update) => Some((
                Message::Repo(index, RepoMessage::OperationProgress(id, update)),
                Some((receiver, worker)),
            )),
            // Every sender is gone, so the work has returned.
            None => {
                let result = match worker.await {
                    Ok(result) => result.map_err(UiError::from),
                    Err(join) => Err(UiError {
                        summary: "a background Git operation panicked".to_owned(),
                        details: join.to_string(),
                    }),
                };
                Some((
                    Message::Repo(index, RepoMessage::OperationFinished(id, Box::new(result))),
                    None,
                ))
            }
        }
    });

    Task::stream(stream)
}

impl Hidegit {
    pub fn new(
        initial: Vec<PathBuf>,
        recents: Vec<PathBuf>,
        alerts: hidegit_forge::AlertPrefs,
        theme: &str,
    ) -> (Self, Task<Message>) {
        let mut this = Self::default();
        this.app.recents = recents;
        this.app.alerts = alerts;

        // An unknown name falls back rather than refusing to start: a typo in a
        // config file is not a reason to have no window. It is said out loud,
        // because a theme silently not applying reads as the setting being
        // ignored — which, until now, it was.
        match crate::theme::Theme::by_name(theme) {
            Some(theme) => this.app.theme = theme,
            None => tracing::warn!(
                requested = theme,
                falling_back_to = crate::theme::Theme::DARK_NAME,
                "unknown theme name"
            ),
        }

        // The forge client is built off the UI thread, because building it
        // reads the keychain and a keychain can prompt.
        let mut tasks = vec![forge::boot()];
        // One tab each, in the order given. The last one opened ends up active,
        // which matches the order they were typed.
        for path in initial {
            tasks.push(Task::done(Message::OpenRepository(path)));
        }

        (this, Task::batch(tasks))
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
            Message::SettingsRequested => {
                self.app.settings_open = true;
                Task::none()
            }

            Message::SettingsDismissed => {
                self.app.settings_open = false;
                Task::none()
            }

            Message::ThemeChosen(name) => {
                // Applied immediately: a theme you have to restart to see is a
                // theme you cannot choose between.
                match crate::theme::Theme::by_name(&name) {
                    Some(theme) => {
                        self.app.theme = theme;
                        for cache in self.caches.values() {
                            cache.clear();
                        }
                    }
                    None => tracing::warn!(requested = name, "unknown theme name"),
                }
                Task::none()
            }

            Message::AlertToggled(which) => {
                which.toggle(&mut self.app.alerts);
                Task::none()
            }

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
                // Already open: switch to its tab rather than opening a second
                // copy. Two tabs on one repository would each hold their own
                // idea of its state, and the one you were not looking at would
                // be wrong the moment you committed in the other.
                if let Some(at) = self.app.repos.iter().position(|repo| repo.path == path) {
                    self.app.active = Some(at);
                    self.app.screen = Screen::Repository;
                    return Task::none();
                }

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

            Message::RepositorySelected(index) => {
                if index < self.app.repos.len() {
                    self.app.active = Some(index);
                    self.app.screen = Screen::Repository;
                }
                Task::none()
            }

            Message::CloseRepository(index) => {
                if index >= self.app.repos.len() {
                    return Task::none();
                }
                self.app.repos.remove(index);

                // The caches are keyed by position, and removing a repository
                // shifts every later one down. Dropping the closed entry alone
                // would leave every tab after it drawing another tab's graph —
                // invisible with one repository open, which is why it survived
                // until tabs existed.
                self.caches.remove(&index);
                let shifted: HashMap<usize, canvas::Cache> = self
                    .caches
                    .drain()
                    .map(|(at, cache)| (if at > index { at - 1 } else { at }, cache))
                    .collect();
                self.caches = shifted;

                // The neighbour, not the last tab. Closing the first of three
                // and landing on the third is the kind of jump that makes you
                // lose your place.
                self.app.active = match self.app.repos.len() {
                    0 => None,
                    len => Some(index.min(len - 1)),
                };
                if self.app.repos.is_empty() {
                    self.app.screen = Screen::Welcome;
                }
                Task::none()
            }

            Message::ToastDismissed(id) => {
                self.app.dismiss_toast(id);
                Task::none()
            }

            Message::ToastCopied(id) => {
                let Some(toast) = self.app.toasts.iter().find(|t| t.id == id) else {
                    return Task::none();
                };
                // Summary and details together: the details alone are often a
                // wall of stderr with no statement of what was being attempted.
                let text = if toast.details.is_empty() {
                    toast.summary.clone()
                } else {
                    format!("{}\n\n{}", toast.summary, toast.details)
                };
                iced::clipboard::write(text)
            }

            Message::ForgeClientBuilt(client, restored) => {
                self.app.forge.client = Some(client);
                match *restored {
                    Ok(identity) => {
                        self.app.forge.identity = identity;
                        // Every repository already open was opened before the
                        // keychain had been read, so none of them has asked for
                        // its pull requests yet.
                        self.poll_every_repository()
                    }
                    Err(error) => {
                        // The one failure that is not worth a toast: on a machine
                        // with no keychain there is nothing to retry and nothing
                        // to dismiss. The panel says so where a user would look
                        // for pull requests, which is where the question arises.
                        self.app.forge.no_keychain =
                            error.summary.contains("no OS keychain is available");
                        if !self.app.forge.no_keychain {
                            self.app.toast(&error);
                        }
                        Task::none()
                    }
                }
            }

            Message::ConnectRequested => {
                self.app.sheet = Some(ActionSheet::new("Connect to GitHub")
                    .item("Sign in with a browser…", Message::DeviceFlowRequested)
                    .item(
                        "Paste a personal access token…",
                        Message::PromptRequested(Box::new(Prompt {
                            kind: PromptKind::PersonalAccessToken,
                            title: "Personal access token".to_owned(),
                            confirm_label: "Connect".to_owned(),
                            fields: vec![PromptField::new(
                                "A token with read access to your repositories and pull requests",
                                "github_pat_… or ghp_…",
                            )],
                        })),
                    ));
                Task::none()
            }

            Message::DeviceFlowRequested => match self.app.forge.client.clone() {
                Some(client) => forge::device_flow(client),
                None => Task::none(),
            },

            Message::DeviceCodeIssued(code) => {
                self.app.forge.connecting = Some(*code);
                Task::none()
            }

            Message::DeviceCodeDismissed => {
                self.app.forge.connecting = None;
                Task::none()
            }

            Message::TokenSubmitted(token) => match self.app.forge.client.clone() {
                Some(client) => forge::with_token(client, token),
                None => Task::none(),
            },

            Message::ForgeConnected(result) => {
                // Cleared either way: the code is spent once the flow resolves,
                // and leaving it up after a refusal would show a code that can
                // no longer be typed.
                self.app.forge.connecting = None;

                match *result {
                    Ok(identity) => {
                        let changed =
                            !forge::same_identity(self.app.forge.identity.as_ref(), &identity);
                        if changed {
                            for repo in &mut self.app.repos {
                                repo.prs.watcher.reset();
                            }
                        }
                        self.app.forge.identity = Some(identity);
                        self.poll_every_repository()
                    }
                    Err(error) => {
                        self.app.toast(&error);
                        Task::none()
                    }
                }
            }

            Message::DisconnectRequested => {
                let who = self
                    .app
                    .forge
                    .identity
                    .as_ref()
                    .map_or_else(|| "GitHub".to_owned(), |i| format!("GitHub as {}", i.login));

                self.app.confirming = Some(Confirmation {
                    title: "Sign out?".to_owned(),
                    body: format!(
                        "hideGit will forget the token for {who} and stop showing pull requests. \
                         Signing back in goes through the browser again."
                    ),
                    confirm_label: "Sign out".to_owned(),
                    action: Box::new(Message::DisconnectConfirmed),
                });
                Task::none()
            }

            Message::DisconnectConfirmed => match self.app.forge.client.clone() {
                Some(client) => forge::sign_out(client),
                None => Task::none(),
            },

            Message::ForgeSignedOut(result) => {
                if let Err(error) = *result {
                    self.app.toast(&error);
                    return Task::none();
                }

                self.app.forge.identity = None;
                for repo in &mut self.app.repos {
                    repo.prs.items.clear();
                    repo.prs.state = PrState::Idle;
                    // Signing in as somebody else makes every role different,
                    // and that difference is not news about the pull requests.
                    repo.prs.watcher.reset();
                }
                // A pull request in the detail pane came from a session that no
                // longer exists, so it goes with it.
                for repo in &mut self.app.repos {
                    if matches!(repo.selection, Some(Selection::PullRequest(_))) {
                        repo.selection = None;
                        repo.detail = DetailPane::Empty;
                    }
                }
                Task::none()
            }

            Message::OpenUrl(url) => forge::open_url(url),

            Message::OpenUrlFailed(error) => {
                self.app.toast(&error);
                Task::none()
            }

            Message::WindowFocused(focused) => {
                self.app.focused = focused;
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

            Message::SheetRequested(sheet) => {
                self.app.sheet = Some(*sheet);
                Task::none()
            }

            // The sheet closes as the action goes out. Anything else leaves it
            // sitting over whatever the action produced — including a toast saying
            // the action failed, which is the one thing the user needs to read.
            Message::SheetChosen(action) => {
                self.app.sheet = None;
                Task::done(*action)
            }

            Message::SheetStepped(delta) => {
                if let Some(sheet) = &mut self.app.sheet {
                    sheet.step(delta);
                }
                Task::none()
            }

            Message::SheetAccepted => match self.app.sheet.as_ref().and_then(ActionSheet::chosen) {
                // Nothing highlighted yet, so nothing is chosen. A sheet whose
                // rows include "Delete" must not act on a bare `Enter`.
                None => Task::none(),
                Some(message) => Task::done(Message::SheetChosen(Box::new(message))),
            },

            Message::SheetDismissed => {
                self.app.sheet = None;
                Task::none()
            }

            Message::PromptRequested(prompt) => {
                self.app.prompt = Some(*prompt);

                // Focus is not observable in iced 0.14 but it is settable, so the
                // cursor lands in the first field without the user having to click
                // the thing they just asked for.
                iced::advanced::widget::operate(
                    iced::advanced::widget::operation::focusable::focus(
                        iced::advanced::widget::Id::new(PROMPT_FIELD_IDS[0]),
                    ),
                )
            }

            Message::PromptChanged(field, value) => {
                if let Some(prompt) = &mut self.app.prompt
                    && let Some(field) = prompt.fields.get_mut(field)
                {
                    field.value = value;
                }
                Task::none()
            }

            Message::PromptAccepted => {
                // Taken rather than borrowed: the prompt closes whether or not it
                // produced an action, and leaving it up after `Enter` would invite
                // the same branch being created twice.
                let Some(prompt) = self.app.prompt.take() else {
                    return Task::none();
                };
                match self.prompt_action(&prompt) {
                    Some(message) => Task::done(message),
                    None => Task::none(),
                }
            }

            Message::PromptFieldStepped => {
                // Which field has focus is not observable, so this asks iced to
                // move focus on rather than tracking it here — the same widget
                // operation `Space` uses to find out whether anything holds it.
                iced::advanced::widget::operate(
                    iced::advanced::widget::operation::focusable::focus_next(),
                )
            }

            Message::PromptDismissed => {
                self.app.prompt = None;
                Task::none()
            }

            // Asking for the folder with the platform's own picker rather than a
            // second text field: pointing at a directory beats typing a path, and
            // the picker is the one part of this the OS does better.
            Message::CloneRequested(url) => Task::perform(
                async move {
                    let picked = rfd::AsyncFileDialog::new()
                        .set_title("Clone into…")
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf());
                    (url, picked)
                },
                |(url, picked)| Message::CloneDestinationPicked(url, picked),
            ),

            Message::CloneDestinationPicked(url, Some(parent)) => {
                if self.cloning.is_some() {
                    return Task::none();
                }

                // The repository lands in a folder named after it, inside the one
                // that was picked — cloning straight into a directory the user
                // chose for other reasons is how a home folder gets a `.git`.
                let into = parent.join(repository_name(&url));
                let cancel = CancelToken::new();
                self.cloning = Some(Operation {
                    id: 0,
                    label: format!("Cloning into {}", into.display()),
                    cancel: cancel.clone(),
                    progress: None,
                });

                let (sender, receiver) = iced::futures::channel::mpsc::unbounded();
                let worker = tokio::task::spawn_blocking(move || {
                    let sink = ChannelSink(sender);
                    hidegit_core::clone_repository(&url, &into, &sink, &cancel)
                });

                Task::stream(iced::futures::stream::unfold(
                    Some((receiver, worker)),
                    |state| async move {
                        use iced::futures::StreamExt as _;
                        let (mut receiver, worker) = state?;

                        match receiver.next().await {
                            Some(update) => {
                                Some((Message::CloneProgress(update), Some((receiver, worker))))
                            }
                            None => {
                                let result = match worker.await {
                                    Ok(result) => result.map_err(UiError::from),
                                    Err(join) => Err(UiError {
                                        summary: "the clone panicked".to_owned(),
                                        details: join.to_string(),
                                    }),
                                };
                                Some((Message::CloneFinished(Box::new(result)), None))
                            }
                        }
                    },
                ))
            }

            // A cancelled picker is not an event worth reporting.
            Message::CloneDestinationPicked(_, None) => Task::none(),

            Message::CloneProgress(update) => {
                if let Some(cloning) = &mut self.cloning {
                    cloning.progress = Some(update);
                }
                Task::none()
            }

            Message::CloneFinished(result) => {
                self.cloning = None;
                match *result {
                    // The reward for cloning is the repository, so it opens.
                    Ok(path) => Task::done(Message::OpenRepository(path)),
                    Err(error) => {
                        // Cancelling is what was asked for. A partial clone is left
                        // on disk rather than deleted: hideGit did not create the
                        // folder the user picked and will not go removing things
                        // inside it.
                        if !error.summary.contains("cancelled") {
                            self.app.toast(&error);
                        }
                        Task::none()
                    }
                }
            }

            Message::CloneCancelled => {
                if let Some(cloning) = &self.cloning {
                    cloning.cancel.cancel();
                }
                Task::none()
            }

            Message::Repo(index, message) => self.update_repo(index, message),
        }
    }

    /// Turns a prompt's kind and its typed values into the message that acts.
    ///
    /// This is what `Confirmation` cannot do: its action is a fixed `Message`, and
    /// an action that depends on what was typed has to be built after the typing.
    ///
    /// Every kind but two is about the active repository — a prompt is modal, so
    /// nothing can change which one that is while it is up. Cloning *creates* a
    /// repository, and stashing may be accepted with nothing typed, so both are
    /// resolved before the rest.
    fn prompt_action(&self, prompt: &Prompt) -> Option<Message> {
        // Handled first: there need not be a repository open to clone one, or
        // to sign in.
        if let PromptKind::Clone = &prompt.kind {
            return Some(Message::CloneRequested(prompt.first()?.to_owned()));
        }
        if let PromptKind::PersonalAccessToken = &prompt.kind {
            return Some(Message::TokenSubmitted(prompt.first()?.to_owned()));
        }

        let index = self.app.active?;

        // Handled before the rest for the opposite reason: an empty message means
        // "let Git write its own `WIP on …`", so this kind must not require one.
        if let PromptKind::StashPush { include_untracked } = &prompt.kind {
            return Some(Message::Repo(
                index,
                RepoMessage::StashRequested(StashOp::Push {
                    message: prompt.first().map(str::to_owned),
                    include_untracked: *include_untracked,
                }),
            ));
        }

        let value = prompt.first()?.to_owned();

        let message = match &prompt.kind {
            PromptKind::NewBranch { from, checkout } => {
                if *checkout {
                    // One command rather than create-then-switch: `git switch
                    // --create` is atomic, and two writes would show an
                    // intermediate state and could half-fail.
                    RepoMessage::CheckoutRequested(CheckoutTarget::NewBranch {
                        name: value,
                        from: from.clone(),
                    })
                } else {
                    RepoMessage::BranchCreateRequested {
                        name: value,
                        from: from.clone(),
                    }
                }
            }
            PromptKind::RenameBranch { from } => {
                // Renaming to the name it already has is not a failure to report,
                // it is nothing to do.
                if value == *from {
                    return None;
                }
                RepoMessage::BranchRenameRequested {
                    from: from.clone(),
                    to: value,
                }
            }
            PromptKind::NewTag { at, annotated } => RepoMessage::TagCreateRequested {
                name: value,
                at: at.clone(),
                // A second field only exists on an annotated tag, and an empty
                // message there means the user changed their mind about annotating
                // rather than that they want an empty annotation.
                message: if *annotated {
                    prompt.field(1).map(str::to_owned)
                } else {
                    None
                },
            },
            PromptKind::AddRemote => RepoMessage::RemoteAddRequested {
                name: value,
                url: prompt.field(1)?.to_owned(),
            },
            PromptKind::EditRemote { name } => RepoMessage::RemoteUrlChangeRequested {
                name: name.clone(),
                url: value,
            },
            // A pull request from the branch you are standing on. Offered
            // only when there is one to open it from, so the head is known by
            // the time the prompt exists.
            PromptKind::NewPullRequest { head, base } => RepoMessage::PrCreateRequested {
                head: head.clone(),
                base: base.clone(),
                title: value,
                body: prompt.field(1).unwrap_or_default().to_owned(),
            },
            // All resolved above, before a repository or a value was required.
            PromptKind::StashPush { .. } | PromptKind::Clone | PromptKind::PersonalAccessToken => {
                return None;
            }
        };

        Some(Message::Repo(index, message))
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
        // Read before the remotes move into the repository.
        let forge_repo = forge::detect(&opened.remotes);

        self.app.remember(opened.path.clone());
        self.app.repos.push(OpenRepo {
            path: opened.path,
            backend: opened.backend,
            head: opened.head,
            refs: opened.refs,
            state: opened.state,
            status: opened.status,
            stashes: opened.stashes,
            remotes: opened.remotes,
            divergence: HashMap::new(),
            pending: None,
            graph,
            selection: selection.clone(),
            detail: DetailPane::Empty,
            focus: Pane::Graph,
            diff_mode: crate::state::DiffMode::default(),
            hunk: 0,
            draft: Draft::default(),
            prs: PrPanel {
                repo: forge_repo,
                ..PrPanel::default()
            },
            resolver: None,
            plan: None,
            blame: None,
            search: None,
        });
        self.caches.insert(index, canvas::Cache::new());
        self.app.active = Some(index);
        self.app.screen = Screen::Repository;

        let mut tasks = vec![
            self.checkpoint_task(index),
            self.load_more_task(index),
            self.divergence_task(index),
            self.poll_task(index),
        ];
        if let Some(selection) = selection {
            tasks.push(
                Task::done(RepoMessage::Selected(selection)).map(move |m| Message::Repo(index, m)),
            );
        }

        Task::batch(tasks)
    }

    /// Starts a network operation, if one is not already running.
    ///
    /// The one place that decides an operation is allowed and records it, so the
    /// "one at a time" rule and the banner cannot disagree. Refuses silently while
    /// something is in flight — the toolbar has already replaced its buttons with
    /// the banner, so the only way to get here is a keyboard shortcut, and a toast
    /// saying "wait" would be noise.
    fn start_operation<F>(&mut self, index: usize, label: String, work: F) -> Task<Message>
    where
        F: FnOnce(&dyn ProgressSink, &CancelToken) -> Result<OperationOutcome, GitError>
            + Send
            + 'static,
    {
        let Some(repo) = self.app.repos.get_mut(index) else {
            return Task::none();
        };
        if repo.pending.is_some() {
            return Task::none();
        }

        self.next_operation_id += 1;
        let id = self.next_operation_id;
        let cancel = CancelToken::new();

        repo.pending = Some(Operation {
            id,
            label,
            cancel: cancel.clone(),
            progress: None,
        });

        streaming(index, id, cancel, work)
    }

    /// Loads ahead/behind for every tracking branch.
    ///
    /// Its own task rather than part of `reread`, because it costs a commit walk
    /// per tracking branch and `reread` runs on every file save through the
    /// watcher. Ahead/behind only changes when a ref moves, and the sidebar can
    /// render perfectly well for the moment before it arrives.
    fn divergence_task(&self, index: usize) -> Task<Message> {
        let Some(repo) = self.app.repos.get(index) else {
            return Task::none();
        };
        let backend = Arc::clone(&repo.backend);

        blocking(move || backend.divergence()).map(move |result| {
            Message::Repo(index, RepoMessage::DivergenceLoaded(Box::new(result)))
        })
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

    /// Asks the forge for one repository's pull requests.
    ///
    /// Silent when there is nothing to ask: no client yet, not signed in, or a
    /// repository whose remotes name no forge. None of those is an error, and
    /// all three are ordinary.
    fn poll_task(&self, index: usize) -> Task<Message> {
        let Some(client) = self.app.forge.client.clone() else {
            return Task::none();
        };
        if !self.app.forge.is_connected() {
            return Task::none();
        }
        let Some(repo) = self.app.repos.get(index).and_then(|r| r.prs.repo.clone()) else {
            return Task::none();
        };

        forge::poll(client, index, repo)
    }

    /// Polls every open repository, and marks each as loading.
    ///
    /// Used when the session changes rather than when a repository does: signing
    /// in has to populate the panels of repositories that were opened before
    /// there was a token.
    fn poll_every_repository(&mut self) -> Task<Message> {
        let indices: Vec<usize> = (0..self.app.repos.len()).collect();

        for index in &indices {
            if let Some(repo) = self.app.repos.get_mut(*index)
                && repo.prs.repo.is_some()
            {
                repo.prs.state = PrState::Loading;
            }
        }

        Task::batch(indices.into_iter().map(|index| self.poll_task(index)))
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
                    // A stash is a commit, so it needs no diff code of its own —
                    // `git stash show` is exactly a commit against its first
                    // parent. The index is resolved to an id here, because the
                    // index is what the list is keyed by and the id is what reads.
                    Selection::Stash(at) => {
                        let Some(id) = repo.stashes.get(at).map(|entry| entry.id) else {
                            return Task::none();
                        };
                        repo.detail = DetailPane::Loading;
                        repo.hunk = 0;

                        let backend = Arc::clone(&repo.backend);
                        blocking(move || load_commit(backend.as_ref(), id)).map(move |result| {
                            Message::Repo(index, RepoMessage::DetailLoaded(Box::new(result)))
                        })
                    }
                    // The one selection whose contents are not in the
                    // repository, so it is loaded from the forge rather than
                    // through the backend.
                    Selection::PullRequest(number) => {
                        let (Some(client), Some(forge_repo)) =
                            (self.app.forge.client.clone(), repo.prs.repo.clone())
                        else {
                            return Task::none();
                        };
                        repo.detail = DetailPane::Loading;
                        repo.hunk = 0;

                        forge::detail(client, index, forge_repo, number)
                    }
                }
            }

            RepoMessage::GraphScrolled(pixels) => {
                repo.graph.scroll += pixels / ROW_HEIGHT;
                repo.graph.clamp_scroll();
                cache.clear();
                Task::none()
            }

            RepoMessage::GraphScrolledTo(fraction) => {
                // The scrollable range is the history minus one screenful: the
                // last row belongs at the bottom of the window, not at the top
                // of an empty one.
                let total = repo.graph.total.max(repo.graph.commits.len());
                let last = total.saturating_sub(repo.graph.viewport_rows.max(1));

                repo.graph.scroll = fraction.clamp(0.0, 1.0) * last as f32;
                repo.graph.clamp_scroll();
                cache.clear();

                // Dragging to somewhere history has not been loaded yet is the
                // whole point of a scrollbar on a hundred thousand commits.
                self.load_more_task(index)
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
                        // since. A stash loads through this path too — it *is* a
                        // commit — so the check has to resolve its entry, or the
                        // pane it asked for sits on "Loading…" for ever.
                        let still_selected = match &repo.selection {
                            Some(Selection::Commit(id)) => *id == load.id,
                            Some(Selection::Stash(at)) => {
                                repo.stashes.get(*at).is_some_and(|e| e.id == load.id)
                            }
                            _ => false,
                        };
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

            RepoMessage::StageToggleRequested => {
                // `find_focused` reports the focused *focusable* widget, and the
                // only focusables here are text inputs. `collect` is what makes
                // the answer arrive at all: the operation yields nothing when
                // nothing is focused, and a bare `Task` would then never fire.
                iced::advanced::widget::operate(
                    iced::advanced::widget::operation::focusable::find_focused(),
                )
                .collect()
                .map(move |focused: Vec<iced::advanced::widget::Id>| {
                    Message::Repo(index, RepoMessage::StageToggleResolved(focused.is_empty()))
                })
            }

            RepoMessage::StageToggleResolved(free) => {
                if !free {
                    // Something is being typed into. The key belongs to it.
                    return Task::none();
                }
                let Some((section, path)) = selected_path(repo) else {
                    return Task::none();
                };
                // Which verb it means is read from the row's section, exactly as
                // the row's own button does — one rule, two ways to reach it.
                Task::done(Message::Repo(
                    index,
                    match section {
                        Section::Staged => RepoMessage::UnstageRequested(vec![path]),
                        _ => RepoMessage::StageRequested(vec![path]),
                    },
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

            RepoMessage::CheckoutRequested(target) => {
                // A repository mid-rebase must not be asked to switch branches:
                // the operation owns HEAD until it is finished or aborted.
                if !repo.can_switch_branches() {
                    return Task::none();
                }
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.checkout(&target))
            }

            RepoMessage::BranchCreateRequested { name, from } => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.create_branch(&name, &from))
            }

            RepoMessage::BranchRenameRequested { from, to } => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.rename_branch(&from, &to))
            }

            // Deleting does not act. It asks, and it says what is at stake —
            // which for an unmerged branch is commits nothing else points at.
            RepoMessage::BranchDeleteRequested { name } => {
                let unmerged = repo
                    .divergence_of(&format!("refs/heads/{name}"))
                    .is_none_or(|drift| drift.ahead > 0);

                self.app.confirming = Some(Confirmation {
                    title: format!("Delete {name}?"),
                    body: if unmerged {
                        format!(
                            "{name} may have commits that no other branch points at. \
                             Deleting it makes them unreachable."
                        )
                    } else {
                        format!("{name} is level with its upstream, so nothing is lost.")
                    },
                    confirm_label: "Delete".to_owned(),
                    action: Box::new(Message::Repo(
                        index,
                        // Never forced from here: the safe form runs first, and
                        // Git's own refusal is what offers the choice.
                        RepoMessage::BranchDeleteConfirmed { name, force: false },
                    )),
                });
                Task::none()
            }

            RepoMessage::BranchDeleteConfirmed { name, force } => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.delete_branch(&name, force))
            }

            // ---- remotes ----
            RepoMessage::FetchRequested => {
                let backend = Arc::clone(&repo.backend);
                // Every remote, pruning as it goes: a tracking ref for a branch
                // someone deleted is the sidebar telling a lie, and there is no
                // reason to make the user ask for the truth separately.
                let opts = FetchOpts {
                    prune: true,
                    tags: false,
                    all_remotes: true,
                };
                self.start_operation(index, "Fetching".to_owned(), move |progress, cancel| {
                    backend
                        .fetch("", &opts, progress, cancel)
                        .map(OperationOutcome::Fetched)
                })
            }

            RepoMessage::PullRequested => {
                let backend = Arc::clone(&repo.backend);
                self.start_operation(index, "Pulling".to_owned(), move |progress, cancel| {
                    backend
                        .pull(&PullOpts::default(), progress, cancel)
                        .map(OperationOutcome::Pulled)
                })
            }

            // A plain push acts. A forced one asks first, and names the branch and
            // the remote rather than leaving them to be assumed.
            RepoMessage::PushRequested { force } => {
                let Some(target) = repo.push_target() else {
                    return Task::none();
                };
                if force == ForceMode::None {
                    return Task::done(Message::Repo(index, RepoMessage::PushConfirmed { force }));
                }

                let lease = force == ForceMode::WithLease;
                self.app.confirming = Some(Confirmation {
                    title: format!("Force push {} to {}?", target.branch, target.remote),
                    body: if lease {
                        format!(
                            "This replaces {}/{} with your version. It refuses if the remote \
                             moved since your last fetch.",
                            target.remote, target.branch
                        )
                    } else {
                        format!(
                            "This replaces {}/{} with your version even if someone else has \
                             pushed since your last fetch. Their commits would become \
                             unreachable.",
                            target.remote, target.branch
                        )
                    },
                    confirm_label: if lease { "Force push" } else { "Force" }.to_owned(),
                    action: Box::new(Message::Repo(index, RepoMessage::PushConfirmed { force })),
                });
                Task::none()
            }

            RepoMessage::PushConfirmed { force } => {
                let Some(target) = repo.push_target() else {
                    return Task::none();
                };
                let backend = Arc::clone(&repo.backend);
                let spec = PushSpec {
                    refspec: target.refspec,
                    force,
                    set_upstream: target.set_upstream,
                };
                let remote = target.remote;
                let label = format!("Pushing {} to {remote}", target.branch);

                self.start_operation(index, label, move |progress, cancel| {
                    backend
                        .push(&remote, &spec, progress, cancel)
                        .map(OperationOutcome::Pushed)
                })
            }

            RepoMessage::OperationProgress(id, update) => {
                // Ignored unless it belongs to the operation on screen: a cancelled
                // one's last report can arrive after its replacement has started.
                if let Some(pending) = &mut repo.pending
                    && pending.id == id
                {
                    pending.progress = Some(update);
                }
                Task::none()
            }

            RepoMessage::OperationFinished(id, result) => {
                if repo.pending.as_ref().is_none_or(|p| p.id != id) {
                    // A late result from something already cancelled and replaced.
                    return Task::none();
                }
                repo.pending = None;

                match *result {
                    Ok(outcome) => {
                        // Success is otherwise silent — the refresh is the result —
                        // but a push that was partly refused must not be, because a
                        // push that appears to have worked and did not is a lie.
                        if let OperationOutcome::Pushed(push) = &outcome
                            && !push.rejected.is_empty()
                        {
                            let rejected = push.rejected.join(", ");
                            self.app.toast(&UiError {
                                summary: format!("The remote refused {rejected}"),
                                details: format!(
                                    "Updated: {}\nRefused: {rejected}",
                                    push.updated.join(", ")
                                ),
                            });
                        }
                        if let OperationOutcome::Pulled(PullOutcome::Conflicted(conflicts)) =
                            &outcome
                        {
                            // Not an error, and not silent either: the repository is
                            // now in a state the user has to finish, and the banner
                            // that says so needs the refresh to appear.
                            tracing::info!(
                                paths = conflicts.len(),
                                "the pull conflicted; the repository is mid-merge"
                            );
                        }
                        Task::done(Message::Repo(index, RepoMessage::RepositoryChanged))
                    }
                    Err(error) => {
                        // Cancelling is what was asked for, so it is silent — unless
                        // the killed `git` left a lock behind, which the user has to
                        // know about because hideGit will not delete it.
                        if error.summary.contains("cancelled")
                            && !error.details.contains("index.lock")
                        {
                            return Task::done(Message::Repo(
                                index,
                                RepoMessage::RepositoryChanged,
                            ));
                        }
                        self.app.toast(&error);
                        // Refreshed even on failure: a fetch that failed part-way
                        // may still have moved some refs.
                        Task::done(Message::Repo(index, RepoMessage::RepositoryChanged))
                    }
                }
            }

            RepoMessage::OperationCancelled => {
                // Only asks. The worker notices, kills the subprocess, and reports
                // back through `OperationFinished` like any other ending — so the
                // banner clears in exactly one place.
                if let Some(pending) = &repo.pending {
                    pending.cancel.cancel();
                }
                Task::none()
            }

            // ---- the stash ----
            RepoMessage::StashRequested(op) => {
                let backend = Arc::clone(&repo.backend);
                // The outcome is discarded on purpose: `Created`, `Applied` and
                // `Dropped` are all "it worked", and the refresh that follows says
                // more than a toast could. A conflict shows up as `RepoState`,
                // which the banner already renders.
                write_task(index, move || backend.stash(&op).map(|_| ()))
            }

            RepoMessage::StashDropRequested(at) => {
                let describe = repo
                    .stashes
                    .get(at)
                    .map(|entry| entry.message.clone())
                    .unwrap_or_else(|| format!("stash@{{{at}}}"));

                self.app.confirming = Some(Confirmation {
                    title: "Drop this stash?".to_owned(),
                    body: format!("“{describe}” will be lost. This cannot be undone."),
                    confirm_label: "Drop".to_owned(),
                    action: Box::new(Message::Repo(index, RepoMessage::StashDropConfirmed(at))),
                });
                Task::none()
            }

            RepoMessage::StashDropConfirmed(at) => {
                // The selection is cleared first: dropping shifts every later
                // entry down by one, so `Stash(at)` would then be showing a
                // different stash's diff under the same heading.
                if repo.selection == Some(Selection::Stash(at)) {
                    repo.selection = None;
                    repo.detail = DetailPane::Empty;
                }
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.stash(&StashOp::Drop(at)).map(|_| ()))
            }

            RepoMessage::SearchRequested => {
                repo.search.get_or_insert_with(Default::default);
                // Focused on open: a search you have to click into before
                // typing costs a click every single time.
                iced::advanced::widget::operate(
                    iced::advanced::widget::operation::focusable::focus(
                        crate::state::SEARCH_FIELD_ID.into(),
                    ),
                )
            }

            RepoMessage::SearchDismissed => {
                repo.search = None;
                Task::none()
            }

            RepoMessage::SearchChanged(query) => {
                let Some(search) = &mut repo.search else {
                    return Task::none();
                };
                search.query = query.clone();
                search.selected = 0;

                if query.trim().is_empty() {
                    // Clearing the box clears the list rather than leaving the
                    // last search's hits under an empty query.
                    search.results = Default::default();
                    search.running = false;
                    return Task::none();
                }

                search.running = true;
                let backend = Arc::clone(&repo.backend);
                let answered = query.clone();
                blocking(move || {
                    backend.search(&SearchQuery {
                        text: query,
                        limit: SEARCH_LIMIT,
                    })
                })
                .map(move |results| {
                    Message::Repo(
                        index,
                        RepoMessage::SearchFinished {
                            query: answered.clone(),
                            results: Box::new(results),
                        },
                    )
                })
            }

            RepoMessage::SearchFinished { query, results } => {
                let Some(search) = &mut repo.search else {
                    return Task::none();
                };
                // A slow search for a query the user has already moved on from
                // must not overwrite the results of a newer one. Typing is
                // faster than a walk of a large history, so this happens.
                if search.query != query {
                    return Task::none();
                }
                search.running = false;
                match *results {
                    Ok(found) => {
                        search.results = found;
                        search.selected = 0;
                    }
                    Err(error) => {
                        search.results = Default::default();
                        self.app.toast(&error);
                    }
                }
                Task::none()
            }

            RepoMessage::SearchStepped(delta) => {
                let Some(search) = &mut repo.search else {
                    return Task::none();
                };
                // Zero is `Enter`: the key binding cannot know which commit is
                // under the selection, so it asks the state that does.
                if delta == 0 {
                    return match search.selected_commit() {
                        Some(id) => {
                            Task::done(Message::Repo(index, RepoMessage::SearchAccepted(id)))
                        }
                        None => Task::none(),
                    };
                }
                search.step(delta);
                Task::none()
            }

            RepoMessage::SearchAccepted(id) => {
                repo.search = None;
                // Selecting scrolls the graph to it, which is the whole point:
                // a search that found a commit and left you looking somewhere
                // else has not finished the job.
                // `restore_anchor` puts the row at the top of the viewport,
                // which is what a jump wants — the selection alone would leave
                // the commit off screen if it is far from where you were.
                repo.graph.restore_anchor(id);
                Task::done(Message::Repo(
                    index,
                    RepoMessage::Selected(Selection::Commit(id)),
                ))
            }

            RepoMessage::BlameRequested { path, at } => {
                let backend = Arc::clone(&repo.backend);
                let wanted = path.clone();
                blocking(move || {
                    let blame = backend.blame(&wanted, at)?;

                    // The commits the lines point at, so the gutter can show
                    // who and when. Distinct ids only — a file blames to a
                    // handful of commits however many lines it has — and read
                    // through `log`, which hydrates metadata without computing
                    // the tree diff that `commit` would.
                    let mut seen = std::collections::HashSet::new();
                    let mut commits = Vec::new();
                    for line in &blame.lines {
                        if !seen.insert(line.commit) {
                            continue;
                        }
                        // One unreadable commit costs its gutter entry, not the
                        // whole view.
                        if let Ok(mut found) = backend
                            .log(&RevSpec::Commit(line.commit), LogPage { skip: 0, limit: 1 })
                            && let Some(commit) = found.pop()
                        {
                            commits.push(commit);
                        }
                    }
                    Ok(crate::message::BlameLoad {
                        path: wanted,
                        at,
                        lines: blame.lines,
                        commits,
                    })
                })
                .map(move |result| Message::Repo(index, RepoMessage::BlameLoaded(Box::new(result))))
            }

            RepoMessage::BlameLoaded(result) => {
                match *result {
                    Ok(loaded) => {
                        repo.blame = Some(crate::state::BlameView {
                            path: loaded.path,
                            at: loaded.at,
                            lines: loaded.lines,
                            commits: loaded.commits.into_iter().map(|c| (c.id, c)).collect(),
                        });
                    }
                    Err(error) => {
                        repo.blame = None;
                        self.app.toast(&error);
                    }
                }
                Task::none()
            }

            RepoMessage::BlameDismissed => {
                repo.blame = None;
                Task::none()
            }

            // ---- history operations ----
            RepoMessage::CommitActionsRequested(id) => {
                // Every one of these moves or rewrites history, and none of them
                // is legal while an operation already owns HEAD.
                if repo.state.is_in_progress() {
                    return Task::none();
                }
                self.app.sheet = Some(commit_sheet(index, id));
                Task::none()
            }

            RepoMessage::BranchDropped { source, target } => {
                if repo.state.is_in_progress() {
                    return Task::none();
                }

                // Both operations act on the branch that is checked out, so a
                // drop between two branches you are not on has no meaning
                // without a checkout first — and doing that silently is exactly
                // the unintended thing the gesture has to avoid.
                let head = match &repo.head {
                    hidegit_core::model::Head::Branch { name, .. } => name.short.clone(),
                    _ => {
                        self.app.toast(&UiError {
                            summary: "A detached HEAD has no branch to merge or rebase".to_owned(),
                            details: "Check out a branch first.".to_owned(),
                        });
                        return Task::none();
                    }
                };

                let other = if target == head {
                    source.clone()
                } else if source == head {
                    target.clone()
                } else {
                    self.app.toast(&UiError {
                        summary: format!(
                            "Neither {source} nor {target} is checked out, so there is nothing to \
                             merge or rebase"
                        ),
                        details: format!("Check out {source} or {target} first."),
                    });
                    return Task::none();
                };

                // Both operations act on the branch that is checked out, so the
                // *direction* of the drag does not change what is on offer —
                // only which two branches were named. The title shows the drag
                // as it happened; every entry names both branches in full,
                // because a gesture is exactly the thing whose direction people
                // misread.
                self.app.sheet = Some(
                    ActionSheet::new(format!("{source} → {target}"))
                        .item(
                            format!("Merge {other} into {head}"),
                            Message::Repo(index, RepoMessage::MergeRequested(other.clone())),
                        )
                        .item(
                            format!("Rebase {head} onto {other}…"),
                            Message::Repo(index, RepoMessage::RebaseRequested(other.clone())),
                        )
                        .item(
                            format!("Rebase {head} onto {other}, interactively…"),
                            Message::Repo(index, RepoMessage::RebasePlanRequested(other)),
                        ),
                );
                Task::none()
            }

            RepoMessage::MergeRequested(from) => {
                let backend = Arc::clone(&repo.backend);
                blocking(move || backend.merge(&from, &MergeOpts::default()))
                    .map(move |r| Message::Repo(index, RepoMessage::MergeFinished(Box::new(r))))
            }

            RepoMessage::MergeFinished(result) => match *result {
                Ok(_) => {
                    // A conflict is reported by the refreshed state — the banner
                    // and the conflicted list — rather than by a toast, because
                    // the next thing to do is on screen, not in a message.
                    Task::done(Message::Repo(index, RepoMessage::RepositoryChanged))
                }
                Err(error) => {
                    self.app.toast(&error);
                    Task::none()
                }
            },

            RepoMessage::RebaseRequested(onto) => {
                self.app.confirming = Some(Confirmation {
                    title: format!("Rebase onto {onto}?"),
                    body: format!(
                        "Every commit on this branch that is not already on {onto} is rewritten \
                         with a new id. If the branch is pushed, the next push has to be forced."
                    ),
                    confirm_label: "Rebase".to_owned(),
                    action: Box::new(Message::Repo(index, RepoMessage::RebaseConfirmed(onto))),
                });
                Task::none()
            }

            RepoMessage::RebaseConfirmed(onto) => {
                let backend = Arc::clone(&repo.backend);
                // An empty plan is an ordinary rebase. The plan editor is what
                // fills one in, and it is not built yet.
                blocking(move || backend.rebase(&onto, &RebasePlan::default()))
                    .map(move |r| Message::Repo(index, RepoMessage::SequenceFinished(Box::new(r))))
            }

            RepoMessage::RebasePlanRequested(onto) => {
                let backend = Arc::clone(&repo.backend);
                let named = onto.clone();
                blocking(move || backend.rebase_preview(&onto).map(|c| (named, c))).map(
                    move |result| {
                        Message::Repo(index, RepoMessage::RebasePlanLoaded(Box::new(result)))
                    },
                )
            }

            RepoMessage::RebasePlanLoaded(result) => {
                match *result {
                    Ok((onto, commits)) => repo.plan = Some(RebaseEditor::new(onto, commits)),
                    Err(error) => {
                        repo.plan = None;
                        self.app.toast(&error);
                    }
                }
                Task::none()
            }

            RepoMessage::PlanRowSelected(at) => {
                if let Some(plan) = &mut repo.plan {
                    plan.select(at);
                }
                Task::none()
            }

            RepoMessage::PlanActionChosen(at, action) => {
                if let Some(plan) = &mut repo.plan {
                    plan.set_action(at, action);
                    // Choosing an action also moves the selection there, so the
                    // move buttons act on the row just touched rather than on
                    // whatever was selected before.
                    plan.select(at);
                }
                Task::none()
            }

            RepoMessage::PlanRowMoved(delta) => {
                if let Some(plan) = &mut repo.plan {
                    plan.move_selected(delta);
                }
                Task::none()
            }

            RepoMessage::PlanDismissed => {
                // Nothing has run, so closing costs nothing and needs no
                // confirmation. That is the point of planning before acting.
                repo.plan = None;
                Task::none()
            }

            RepoMessage::PlanStarted => {
                let Some(editor) = &repo.plan else {
                    return Task::none();
                };
                if editor.blocked().is_some() {
                    return Task::none();
                }

                let onto = editor.onto.clone();
                let plan = editor.plan();
                repo.plan = None;
                let backend = Arc::clone(&repo.backend);
                blocking(move || backend.rebase(&onto, &plan))
                    .map(move |r| Message::Repo(index, RepoMessage::SequenceFinished(Box::new(r))))
            }

            RepoMessage::CherryPickRequested(id) => {
                let backend = Arc::clone(&repo.backend);
                blocking(move || backend.cherry_pick(&[id]))
                    .map(move |r| Message::Repo(index, RepoMessage::SequenceFinished(Box::new(r))))
            }

            RepoMessage::RevertRequested(id) => {
                let backend = Arc::clone(&repo.backend);
                blocking(move || backend.revert(&[id]))
                    .map(move |r| Message::Repo(index, RepoMessage::SequenceFinished(Box::new(r))))
            }

            RepoMessage::ResetRequested { to, mode } => {
                // Only a hard reset destroys anything. Confirming the other two
                // would teach people to click through the warning that matters.
                if !mode.is_destructive() {
                    return Task::done(Message::Repo(
                        index,
                        RepoMessage::ResetConfirmed { to, mode },
                    ));
                }

                let changes = repo.status.change_count();
                self.app.confirming = Some(Confirmation {
                    title: format!("Hard reset to {}?", to.short(7)),
                    body: if changes == 0 {
                        "Commits after this one are left with no branch pointing at them. The \
                         reflog can still reach them."
                            .to_owned()
                    } else {
                        // Naming the count is the difference between a warning
                        // and a warning someone reads.
                        format!(
                            "{changes} uncommitted {} discarded, with nothing to undo it. Commits \
                             after this one stay reachable through the reflog.",
                            if changes == 1 {
                                "change is"
                            } else {
                                "changes are"
                            },
                        )
                    },
                    confirm_label: "Hard reset".to_owned(),
                    action: Box::new(Message::Repo(
                        index,
                        RepoMessage::ResetConfirmed { to, mode },
                    )),
                });
                Task::none()
            }

            RepoMessage::ResetConfirmed { to, mode } => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.reset(&StartPoint::Commit(to), mode))
            }

            // ---- the conflict resolver ----
            RepoMessage::ConflictOpenRequested(path) => {
                // Already open on this file: reopening would throw away every
                // decision made so far, which is the one thing the resolver
                // must never do.
                if repo.resolver.as_ref().is_some_and(|r| r.path == path) {
                    return Task::none();
                }
                let full = repo.path.join(&path);
                blocking(move || {
                    let content = std::fs::read_to_string(&full)?;
                    // The parse failure names the line it happened on, which is
                    // worth more to someone staring at the file than a generic
                    // "could not read it".
                    let file = hidegit_core::conflict::parse(&content).map_err(|error| {
                        GitError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("{}: {error}", full.display()),
                        ))
                    })?;
                    Ok((path, file))
                })
                .map(move |result| {
                    Message::Repo(index, RepoMessage::ConflictFileLoaded(Box::new(result)))
                })
            }

            RepoMessage::ConflictFileLoaded(result) => {
                match *result {
                    Ok((path, file)) => repo.resolver = Some(Resolver::new(path, file)),
                    Err(error) => {
                        // Leaving the file alone is the honest answer: it may
                        // have been hand-edited into a shape Git never writes,
                        // and rewriting it from a half-understood parse would
                        // lose a side.
                        repo.resolver = None;
                        self.app.toast(&error);
                    }
                }
                Task::none()
            }

            RepoMessage::ConflictResolved(at, resolution) => {
                if let Some(resolver) = &mut repo.resolver
                    && let Some(slot) = resolver.resolutions.get_mut(at)
                {
                    *slot = resolution;
                    // A preset supersedes whatever was typed: the editor showed
                    // the old choice, and leaving it open would show text that
                    // no longer describes the resolution.
                    if at == resolver.focused {
                        resolver.editor = None;
                    }
                }
                Task::none()
            }

            RepoMessage::ConflictStepped(delta) => {
                if let Some(resolver) = &mut repo.resolver {
                    resolver.step(delta);
                }
                Task::none()
            }

            RepoMessage::ConflictEditToggled => {
                if let Some(resolver) = &mut repo.resolver {
                    if resolver.editor.is_some() {
                        resolver.editor = None;
                    } else if let Some(region) = resolver.focused_region() {
                        // Seeded with what the current choice would produce, so
                        // editing starts from a preset rather than from blank.
                        let current = resolver
                            .resolutions
                            .get(resolver.focused)
                            .cloned()
                            .unwrap_or_default();
                        let seed = match current {
                            Resolution::Unresolved => region.ours.concat(),
                            other => region.resolved_lines(&other).concat(),
                        };
                        resolver.editor = Some(text_editor::Content::with_text(&seed));
                    }
                }
                Task::none()
            }

            RepoMessage::ConflictEdited(action) => {
                if let Some(resolver) = &mut repo.resolver {
                    let focused = resolver.focused;
                    if let Some(editor) = &mut resolver.editor {
                        let edited = action.is_edit();
                        editor.perform(action);
                        // Only a change to the text is a new resolution. A
                        // cursor move is not, and treating it as one would mark
                        // a conflict resolved just for clicking in the pane.
                        if edited {
                            let text = editor.text();
                            if let Some(slot) = resolver.resolutions.get_mut(focused) {
                                *slot = Resolution::Custom(split_kept(&text));
                            }
                        }
                    }
                }
                Task::none()
            }

            RepoMessage::ConflictMarkedResolved => {
                let Some(resolver) = &repo.resolver else {
                    return Task::none();
                };
                if !resolver.is_resolved() {
                    return Task::none();
                }

                let contents = resolver.rendered();
                let relative = resolver.path.clone();
                let full = repo.path.join(&relative);
                let backend = Arc::clone(&repo.backend);

                blocking(move || {
                    std::fs::write(&full, contents)?;
                    // Staging is what actually ends the conflict for this path.
                    // Resolving is not a special kind of write.
                    backend.stage(&[relative.as_path()])?;
                    Ok(relative)
                })
                .map(move |result| {
                    Message::Repo(index, RepoMessage::ConflictSaved(Box::new(result)))
                })
            }

            RepoMessage::ConflictSaved(result) => match *result {
                Ok(_) => {
                    // The file is no longer conflicted, so the resolver has
                    // nothing left to show. The refresh repopulates the list.
                    repo.resolver = None;
                    Task::done(Message::Repo(index, RepoMessage::RepositoryChanged))
                }
                Err(error) => {
                    self.app.toast(&error);
                    Task::none()
                }
            },

            RepoMessage::SequenceControlRequested(control) => {
                if control == SequenceControl::Abort {
                    // Abort throws away every resolution made so far and moves
                    // HEAD back. It says what it will do before doing it.
                    let verb = operation_verb(repo.state);
                    self.app.confirming = Some(Confirmation {
                        title: format!("Abort the {verb}?"),
                        body: format!(
                            "The repository goes back to exactly where it was before the {verb} \
                             started. Every conflict resolved so far is discarded."
                        ),
                        confirm_label: format!("Abort {verb}"),
                        action: Box::new(Message::Repo(index, RepoMessage::SequenceAbortConfirmed)),
                    });
                    return Task::none();
                }

                let backend = Arc::clone(&repo.backend);
                blocking(move || backend.control_sequence(control)).map(move |result| {
                    Message::Repo(index, RepoMessage::SequenceFinished(Box::new(result)))
                })
            }

            RepoMessage::SequenceAbortConfirmed => {
                repo.resolver = None;
                let backend = Arc::clone(&repo.backend);
                blocking(move || backend.control_sequence(SequenceControl::Abort)).map(
                    move |result| {
                        Message::Repo(index, RepoMessage::SequenceFinished(Box::new(result)))
                    },
                )
            }

            RepoMessage::SequenceFinished(result) => {
                match *result {
                    Ok(outcome) => {
                        // Stopping again is ordinary: a rebase hits conflicts on
                        // one commit after another, and each stop is a new file
                        // to resolve rather than a failure to report.
                        if let SequenceOutcome::Stopped { .. } = outcome {
                            repo.resolver = None;
                        }
                        Task::done(Message::Repo(index, RepoMessage::RepositoryChanged))
                    }
                    Err(error) => {
                        self.app.toast(&error);
                        Task::none()
                    }
                }
            }

            // ---- remotes and tags ----
            RepoMessage::RemoteAddRequested { name, url } => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.add_remote(&name, &url))
            }

            RepoMessage::RemoteUrlChangeRequested { name, url } => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.set_remote_url(&name, &url))
            }

            RepoMessage::RemoteRemoveRequested(name) => {
                self.app.confirming = Some(Confirmation {
                    title: format!("Remove {name}?"),
                    body: format!(
                        "Its remote-tracking branches go too, and any local branch \
                         that tracks {name} stops knowing where it came from. \
                         Nothing on the remote itself is touched."
                    ),
                    confirm_label: "Remove".to_owned(),
                    action: Box::new(Message::Repo(
                        index,
                        RepoMessage::RemoteRemoveConfirmed(name),
                    )),
                });
                Task::none()
            }

            RepoMessage::RemoteRemoveConfirmed(name) => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.remove_remote(&name))
            }

            RepoMessage::TagCreateRequested { name, at, message } => {
                let backend = Arc::clone(&repo.backend);
                let spec = TagSpec { name, at, message };
                write_task(index, move || backend.create_tag(&spec))
            }

            RepoMessage::TagDeleteRequested(name) => {
                self.app.confirming = Some(Confirmation {
                    title: format!("Delete {name}?"),
                    body: format!(
                        "{name} is removed here only. A remote that already has it \
                         keeps it until the deletion is pushed."
                    ),
                    confirm_label: "Delete".to_owned(),
                    action: Box::new(Message::Repo(index, RepoMessage::TagDeleteConfirmed(name))),
                });
                Task::none()
            }

            RepoMessage::TagDeleteConfirmed(name) => {
                let backend = Arc::clone(&repo.backend);
                write_task(index, move || backend.delete_tag(&name))
            }

            RepoMessage::TagPushRequested { remote, name } => {
                let backend = Arc::clone(&repo.backend);
                // Fully qualified on both sides, so a tag and a branch of the same
                // name cannot be confused for one another on either end.
                let spec = PushSpec {
                    refspec: format!("refs/tags/{name}:refs/tags/{name}"),
                    force: ForceMode::None,
                    set_upstream: false,
                };
                let label = format!("Pushing {name} to {remote}");

                self.start_operation(index, label, move |progress, cancel| {
                    backend
                        .push(&remote, &spec, progress, cancel)
                        .map(OperationOutcome::Pushed)
                })
            }

            RepoMessage::DivergenceLoaded(result) => {
                match *result {
                    Ok(divergence) => repo.divergence = divergence,
                    // A failure here costs an indicator, not a feature. Saying so
                    // in a toast would be a dialog about an arrow.
                    Err(error) => {
                        tracing::warn!(
                            summary = %error.summary,
                            "could not compute ahead/behind"
                        );
                    }
                }
                Task::none()
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

                // A conflicted row opens the resolver on its file; any other row
                // closes it, so the decisions on screen always belong to the
                // file named above them.
                match row.section {
                    Section::Conflicted => {
                        match repo
                            .status
                            .conflicted
                            .get(row.index)
                            .map(|c| c.path.clone())
                        {
                            Some(path) => Task::done(Message::Repo(
                                index,
                                RepoMessage::ConflictOpenRequested(path),
                            )),
                            None => Task::none(),
                        }
                    }
                    _ => {
                        repo.resolver = None;
                        Task::none()
                    }
                }
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
                if on
                    && repo.draft.subject.trim().is_empty()
                    && let Some(head) = repo.graph.commits.first()
                {
                    repo.draft.subject = head.summary.clone();
                    repo.draft.body = head.body.clone().unwrap_or_default();
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

            RepoMessage::CommitAndPushRequested => {
                // Two operations, not one: a commit that succeeds is kept even
                // when the push fails, which is the common case since the push
                // is the half that needs a network and a credential.
                repo.draft.push_after_commit = true;
                Task::done(Message::Repo(index, RepoMessage::CommitRequested))
            }

            RepoMessage::Committed(result) => match *result {
                Ok(_) => {
                    let then_push = repo.draft.push_after_commit;
                    // The draft is only cleared once the commit actually
                    // landed. A failed hook must not cost the user the message
                    // they wrote.
                    repo.draft = Draft::default();
                    let mut tasks = vec![Task::done(Message::Repo(
                        index,
                        RepoMessage::RepositoryChanged,
                    ))];
                    if then_push {
                        tasks.push(Task::done(Message::Repo(
                            index,
                            RepoMessage::PushRequested {
                                force: ForceMode::None,
                            },
                        )));
                    }
                    Task::batch(tasks)
                }
                Err(error) => {
                    // The push is abandoned with it: pushing a commit that was
                    // never made is not what was asked for.
                    repo.draft.push_after_commit = false;
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
                repo.stashes = refreshed.stashes;
                // A remote may have been added, removed or repointed, so which
                // forge repository this is can change without the app
                // restarting.
                repo.prs.repo = forge::detect(&refreshed.remotes);
                repo.remotes = refreshed.remotes;

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

                // A rewrite can delete the commit the detail pane is showing —
                // a dropped step in a rebase, a squashed one, anything before a
                // hard reset. Left alone the pane goes on rendering it from the
                // copy it already holds, which reads as "still there".
                //
                // Only when the whole history is loaded: a commit missing from a
                // partly-paged graph has probably just not been walked to yet,
                // and clearing the selection then would fight the user on every
                // refresh of a large repository.
                let gone = matches!(repo.selection, Some(Selection::Commit(id))
                    if !repo.graph.loading_more
                        && !repo.graph.commits.iter().any(|c| c.id == id));
                if gone {
                    repo.selection = repo.graph.commits.first().map(|c| Selection::Commit(c.id));
                    repo.detail = DetailPane::Empty;
                }
                cache.clear();

                let mut tasks = vec![
                    self.checkpoint_task(index),
                    // Refs may have moved, so ahead/behind may have changed. It
                    // rides along here rather than inside `reread` so a slow
                    // count never delays the branch list and the graph.
                    self.divergence_task(index),
                ];
                // The commit it fell back to has to be loaded, or the pane stays
                // empty with a selection pointing at it.
                if gone && let Some(selection) = self.app.repos[index].selection.clone() {
                    tasks.push(Task::done(Message::Repo(
                        index,
                        RepoMessage::Selected(selection),
                    )));
                }
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

            // ---- pull requests ----
            RepoMessage::PrsRefreshRequested => {
                if repo.prs.repo.is_some() {
                    repo.prs.state = PrState::Loading;
                }
                self.poll_task(index)
            }

            RepoMessage::PrsLoaded(result) => {
                match *result {
                    Ok(PrsLoad::Loaded { items, budget }) => {
                        repo.prs.schedule.succeeded(budget);
                        // Compared *before* the list is replaced, because the
                        // comparison is against what was there.
                        let observed = repo.prs.watcher.observe(&items);
                        repo.prs.items = items;
                        repo.prs.state = PrState::Loaded;

                        let repository = repo
                            .prs
                            .repo
                            .as_ref()
                            .map_or_else(String::new, ToString::to_string);
                        let forge_repo = repo.prs.repo.clone();

                        self.app.notify(&observed.alerts, &repository);

                        // Each ending costs one more request, and only for pull
                        // requests you wrote.
                        let Some((client, forge_repo)) =
                            self.app.forge.client.clone().zip(forge_repo)
                        else {
                            return Task::none();
                        };
                        return Task::batch(observed.vanished.into_iter().map(|number| {
                            forge::ending(Arc::clone(&client), index, forge_repo.clone(), number)
                        }));
                    }
                    // Its own state rather than a toast: it is about this
                    // repository, it persists until somebody installs the app,
                    // and its action belongs in the panel where the question
                    // comes up.
                    Ok(PrsLoad::NotInstalled { install_url }) => {
                        repo.prs.items.clear();
                        repo.prs.state = PrState::NotInstalled { install_url };
                    }
                    // A failed poll updates an indicator; it never raises a
                    // dialog and never clears what was last known. `items` is
                    // deliberately left alone so the panel goes stale rather
                    // than empty.
                    Err(error) => {
                        repo.prs.schedule.failed();
                        tracing::debug!(error = %error.summary, "a poll failed");
                        repo.prs.state = PrState::Stale(error.summary);
                    }
                }
                Task::none()
            }

            RepoMessage::PrEndingLoaded(result) => {
                // A lookup that fails costs one notification, never the poll:
                // the pull request is gone from the list either way.
                let Ok(detail) = *result else {
                    return Task::none();
                };

                let event = match detail.lifecycle {
                    hidegit_forge::Lifecycle::Merged => hidegit_forge::AlertEvent::PrMerged,
                    hidegit_forge::Lifecycle::Closed => hidegit_forge::AlertEvent::PrClosed,
                    // It is open after all — it fell off the page rather than
                    // ending, which a repository with more than `PAGE` open
                    // pull requests can do. Not an event.
                    hidegit_forge::Lifecycle::Open => return Task::none(),
                };

                let repository = repo
                    .prs
                    .repo
                    .as_ref()
                    .map_or_else(String::new, ToString::to_string);

                self.app.notify(
                    &[hidegit_forge::Alert {
                        event,
                        number: detail.pr.number,
                        title: detail.pr.title.clone(),
                        url: detail.pr.url.clone(),
                    }],
                    &repository,
                );
                Task::none()
            }

            RepoMessage::PrDetailLoaded(result) => {
                match *result {
                    Ok(detail) => repo.detail = DetailPane::PullRequest(Box::new(detail)),
                    Err(error) => repo.detail = DetailPane::Failed(error),
                }
                Task::none()
            }

            RepoMessage::PrOpenRequested(number) => {
                let (Some(client), Some(forge_repo)) =
                    (self.app.forge.client.as_ref(), repo.prs.repo.as_ref())
                else {
                    return Task::none();
                };

                // The pull request's own URL when it is known, because a
                // repository that has been renamed still answers on its old
                // path and an assembled URL would not.
                let url = repo.prs.find(number).map_or_else(
                    || forge::web_url(client, forge_repo, WebTarget::PullRequest(number)),
                    |pr| pr.url.clone(),
                );
                Task::done(Message::OpenUrl(url))
            }

            RepoMessage::PrCreateRequested {
                head,
                base,
                title,
                body,
            } => {
                let (Some(client), Some(forge_repo)) =
                    (self.app.forge.client.clone(), repo.prs.repo.clone())
                else {
                    return Task::none();
                };

                forge::create(
                    client,
                    index,
                    forge_repo,
                    NewPullRequest {
                        title,
                        body,
                        head,
                        base,
                        draft: false,
                    },
                )
            }

            RepoMessage::PrCreated(result) => match *result {
                Ok(created) => {
                    let number = created.number;
                    // Inserted rather than waited for: the poll that would have
                    // brought it may be minutes away, and a pull request that
                    // does not appear after being opened reads as a failure.
                    repo.prs.items.retain(|pr| pr.number != number);
                    repo.prs.items.insert(0, created);
                    repo.prs.state = PrState::Loaded;

                    Task::done(Message::Repo(
                        index,
                        RepoMessage::Selected(Selection::PullRequest(number)),
                    ))
                }
                Err(error) => {
                    self.app.toast(&error);
                    Task::none()
                }
            },
        }
    }

    pub fn view(&self) -> IcedElement<'_, Message> {
        let palette = &self.app.theme.palette;

        // A clone has no repository to hang a banner off, so it goes above the
        // whole screen — including the welcome screen it was started from.
        let base = match &self.cloning {
            Some(cloning) => iced::widget::column![
                widget::overlay::clone_banner(cloning, palette),
                self.screen(),
            ]
            .into(),
            None => self.screen(),
        };

        let base = if self.app.settings_open {
            iced::widget::stack![
                base,
                widget::settings::view(&self.app, &self.app.theme.palette)
            ]
            .into()
        } else {
            base
        };

        widget::overlay::wrap(
            base,
            widget::overlay::Layers {
                confirming: self.app.confirming.as_ref(),
                sheet: self.app.sheet.as_ref(),
                prompt: self.app.prompt.as_ref(),
                device_code: self.app.forge.connecting.as_ref(),
                toasts: &self.app.toasts,
            },
            palette,
        )
    }

    /// The screen itself, without the modal and toast layers.
    fn screen(&self) -> IcedElement<'_, Message> {
        let palette = &self.app.theme.palette;

        let body = self.repository_screen();
        match widget::tabs::view(&self.app.repos, self.app.active, palette) {
            Some(tabs) => iced::widget::column![tabs, body].into(),
            None => body,
        }
    }

    /// The screen itself, without the tab bar above it.
    fn repository_screen(&self) -> IcedElement<'_, Message> {
        let palette = &self.app.theme.palette;

        match (self.app.screen, self.app.active) {
            (Screen::Repository, Some(index)) => {
                let repo = &self.app.repos[index];
                let cache = self
                    .caches
                    .get(&index)
                    .expect("every open repository has a canvas cache");

                let base = screen::repository::view(&self.app, repo, index, palette, cache);

                // The plan editor and the search each sit *over* the repository
                // rather than beside it: both own the screen until they are
                // closed, and nothing behind them can be acted on meaningfully
                // in the meantime.
                let base: IcedElement<'_, Message> = match &repo.plan {
                    Some(plan) => {
                        iced::widget::stack![base, widget::plan::view(plan, index, palette)].into()
                    }
                    None => base,
                };

                match &repo.search {
                    Some(search) => iced::widget::stack![
                        base,
                        widget::search::view(search, palette).map(move |m| Message::Repo(index, m))
                    ]
                    .into(),
                    None => base,
                }
            }
            _ => screen::welcome::view(&self.app.recents, palette),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        // Carried in the subscription's identity so every binding is resolved
        // against state the subscription already has, rather than round-tripping
        // through a message that would arrive a frame late.
        let context = (
            self.modal_keys(),
            KeyContext {
                active: self.app.active,
                editing: self
                    .app
                    .active_repo()
                    .is_some_and(|repo| repo.draft.editing),
                pane: self.app.active_repo().map(|repo| repo.focus),
                settings_open: self.app.settings_open,
                searching: self
                    .app
                    .active
                    .filter(|_| self.app.active_repo().is_some_and(|r| r.search.is_some())),
                open_repos: self.app.repos.len(),
            },
        );

        let keys = keyboard::listen()
            .with(context)
            .map(|((modal, cx), event)| {
                let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
                    return Message::ToastDismissed(u64::MAX);
                };
                // A modal owns the keyboard while it is up. Letting `Space` stage
                // something behind a "discard?" dialog would be the worst
                // possible moment to act on a stray key.
                if let Some(modal) = modal {
                    return modal_shortcut(&key, modal);
                }
                shortcut(&key, modifiers, cx)
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

        // One timer per repository that has something to poll. The interval is
        // part of the subscription's identity, so a failure or a thin budget
        // replaces the timer rather than being noticed on the next tick.
        let now = time::OffsetDateTime::now_utc();
        let total = self.app.repos.len();
        let activity = self.app.activity();
        let connected = self.app.forge.is_connected();

        let polls = self
            .app
            .repos
            .iter()
            .enumerate()
            .filter(move |(_, repo)| connected && repo.prs.repo.is_some())
            .map(move |(index, repo)| {
                alerts::subscribe(index, total, &repo.prs.schedule, activity, now)
            });

        // Focus decides how often polling happens, so it has to be observed
        // rather than assumed. A minimised window asking every minute is the
        // thing the interval table exists to prevent.
        let focus = iced::event::listen_with(|event, _, _| match event {
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::WindowFocused(true)),
            iced::Event::Window(iced::window::Event::Unfocused) => {
                Some(Message::WindowFocused(false))
            }
            _ => None,
        });

        Subscription::batch([keys, focus].into_iter().chain(watches).chain(polls))
    }

    /// Which modal, if any, currently owns the keyboard.
    ///
    /// Topmost first, matching the order `overlay::wrap` stacks them: a
    /// confirmation raised from a sheet sits over that sheet, so `Esc` has to
    /// dismiss the confirmation rather than the thing underneath it.
    fn modal_keys(&self) -> Option<Modal> {
        if self.app.forge.connecting.is_some() {
            Some(Modal::DeviceCode)
        } else if self.app.confirming.is_some() {
            Some(Modal::Confirmation)
        } else if self.app.prompt.is_some() {
            Some(Modal::Prompt)
        } else if self.app.sheet.is_some() {
            Some(Modal::Sheet)
        } else {
            None
        }
    }
}

/// Which modal layer is on top, for deciding what a key means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modal {
    Confirmation,
    Prompt,
    Sheet,
    DeviceCode,
}

/// Everything a key press has to be resolved against.
///
/// A struct rather than a parameter list. It reached eight positional
/// arguments, at which point every new binding that needed context meant
/// editing every call site and reading `false, None, false, 1` at each one to
/// work out which was which.
///
/// `Hash` because it rides in the keyboard subscription's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyContext {
    /// The repository the keys address.
    pub active: Option<usize>,
    /// A text field has focus, so unmodified keys belong to it.
    pub editing: bool,
    /// Which pane has focus, for `Tab`.
    pub pane: Option<Pane>,
    pub settings_open: bool,
    /// The repository whose search panel is open, if one is.
    pub searching: Option<usize>,
    /// How many tabs there are, so `Cmd+<n>` past the last does nothing.
    pub open_repos: usize,
}

/// Maps a key press to a message. `Cmd` on macOS, `Ctrl` elsewhere.
fn shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers, cx: KeyContext) -> Message {
    let KeyContext {
        active,
        editing,
        pane,
        settings_open: settings,
        searching,
        open_repos,
    } = cx;
    use keyboard::key::{Key, Named};

    let nothing = Message::ToastDismissed(u64::MAX);
    let command = modifiers.command();

    // The search owns the keyboard while it is up. Everything unmodified goes
    // to the box being typed into; the arrows and Enter drive the list, which
    // is the whole point of it being a keyboard-first panel.
    if let Some(index) = searching {
        return match key {
            Key::Named(Named::Escape) => Message::Repo(index, RepoMessage::SearchDismissed),
            Key::Named(Named::ArrowDown) => Message::Repo(index, RepoMessage::SearchStepped(1)),
            Key::Named(Named::ArrowUp) => Message::Repo(index, RepoMessage::SearchStepped(-1)),
            // Enter is handled where the selection lives: the binding cannot
            // know which commit is under it.
            Key::Named(Named::Enter) => Message::Repo(index, RepoMessage::SearchStepped(0)),
            _ => nothing,
        };
    }

    // The settings panel owns the keyboard while it is up, the way every other
    // layer over the screen does. Without this, `j` steps through hunks behind
    // it and `Space` stages a file nobody can see.
    if settings {
        return match key {
            Key::Named(Named::Escape) => Message::SettingsDismissed,
            _ => nothing,
        };
    }

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

    let shift = modifiers.shift();

    match key {
        // Checked before the plain `Cmd+Enter`: match arms are tried in order,
        // and the unshifted one matches a shifted press too.
        Key::Named(Named::Enter) if command && shift => repo(RepoMessage::CommitAndPushRequested),
        Key::Named(Named::Enter) if command => repo(RepoMessage::CommitRequested),
        Key::Named(Named::Backspace) if command => repo(RepoMessage::DiscardSelectedRequested),
        // Checked before the unshifted `o`, or `Cmd+Shift+O` would open a picker.
        Key::Character(c) if command && shift && c.eq_ignore_ascii_case("o") => {
            Message::PromptRequested(Box::new(Prompt {
                kind: PromptKind::Clone,
                title: "Clone a repository".to_owned(),
                confirm_label: "Choose a folder…".to_owned(),
                fields: vec![crate::state::PromptField::new(
                    "URL",
                    "https://github.com/owner/repo.git",
                )],
            }))
        }
        Key::Character(c) if command && c.as_str() == "f" && !shift => {
            repo(RepoMessage::SearchRequested)
        }
        // `Cmd+1` … `Cmd+9`. Checked before the other command characters so a
        // digit never falls through to something that treats it as a letter.
        Key::Character(c)
            if command
                && c.chars().count() == 1
                && c.chars().next().is_some_and(|d| d.is_ascii_digit()) =>
        {
            match c.chars().next().and_then(|d| tab_for_digit(d, open_repos)) {
                Some(index) => Message::RepositorySelected(index),
                None => nothing,
            }
        }
        Key::Character(c) if command && c.as_str() == "," => Message::SettingsRequested,
        Key::Character(c) if command && c.as_str() == "o" => Message::OpenDialogRequested,
        Key::Character(c) if command && c.as_str() == "d" => repo(RepoMessage::DiffModeToggled),

        // The remote operations. All modified, so the `editing` guard above lets
        // them through while a commit message is being typed — which is exactly
        // when `Cmd+Shift+U` is wanted.
        //
        // Matched case-insensitively: with Shift held, the character iced reports
        // is the shifted one on most layouts, and hard-coding either case would
        // make the binding depend on the keyboard.
        Key::Character(c) if command && shift && c.eq_ignore_ascii_case("f") => {
            repo(RepoMessage::FetchRequested)
        }
        Key::Character(c) if command && shift && c.eq_ignore_ascii_case("p") => {
            repo(RepoMessage::PullRequested)
        }
        Key::Character(c) if command && shift && c.eq_ignore_ascii_case("u") => {
            repo(RepoMessage::PushRequested {
                force: ForceMode::None,
            })
        }
        // `Space` does not act here. It asks whether anything has keyboard
        // focus first, because iced keeps text-input focus inside the widget
        // and the `editing` flag only turns true once a key has already
        // arrived — so a click into the message field followed by `Space`
        // would otherwise stage a file. See `StageToggleRequested`.
        Key::Named(Named::Space) if !command => repo(RepoMessage::StageToggleRequested),
        Key::Character(c) if c.as_str() == "j" && !command => repo(RepoMessage::HunkStepped(1)),
        Key::Character(c) if c.as_str() == "k" && !command => repo(RepoMessage::HunkStepped(-1)),
        Key::Named(Named::ArrowDown) => repo(RepoMessage::SelectionMoved(1)),
        Key::Named(Named::ArrowUp) => repo(RepoMessage::SelectionMoved(-1)),
        Key::Named(Named::PageDown) => repo(RepoMessage::SelectionMoved(20)),
        Key::Named(Named::PageUp) => repo(RepoMessage::SelectionMoved(-20)),
        // Conflict navigation. Bracket keys carry the command modifier so they
        // still work while the result pane is being typed into, which is
        // exactly when moving to the next conflict is wanted.
        Key::Character(c) if command && c.as_str() == "]" => repo(RepoMessage::ConflictStepped(1)),
        Key::Character(c) if command && c.as_str() == "[" => repo(RepoMessage::ConflictStepped(-1)),
        // Continue whatever is in progress. `update` reads `RepoState` to decide
        // which `git` verb that is, so one key covers all four operations.
        Key::Character(c) if command && shift && matches!(c.as_str(), "." | ">") => repo(
            RepoMessage::SequenceControlRequested(SequenceControl::Continue),
        ),

        Key::Named(Named::Tab) => match pane {
            Some(current) => repo(RepoMessage::FocusCycled(if shift {
                current.previous()
            } else {
                current.next()
            })),
            None => nothing,
        },
        _ => nothing,
    }
}

/// The only keys a modal layer answers to: `Esc` backs out, `Enter` goes ahead.
///
/// Everything else becomes the no-op, which is what keeps a bare `j` from
/// stepping through hunks behind a dialog. A prompt's text still reaches its
/// field: `keyboard::listen()` observes events, it does not consume them, so
/// returning the no-op here leaves the focused widget's own handling intact.
fn modal_shortcut(key: &keyboard::Key, modal: Modal) -> Message {
    use keyboard::key::{Key, Named};

    match (key, modal) {
        (Key::Named(Named::Escape), Modal::Confirmation) => Message::ConfirmationDismissed,
        (Key::Named(Named::Enter), Modal::Confirmation) => Message::ConfirmationAccepted,
        (Key::Named(Named::Escape), Modal::Prompt) => Message::PromptDismissed,
        // `Enter` in the field also submits, through `on_submit`. Both routes
        // land on the same message and `PromptAccepted` takes the prompt, so the
        // second arrival finds nothing and does nothing.
        (Key::Named(Named::Enter), Modal::Prompt) => Message::PromptAccepted,
        (Key::Named(Named::Escape), Modal::Sheet) => Message::SheetDismissed,
        // Every per-item action in the sidebar goes through a sheet, so a sheet
        // that cannot be driven from the keyboard makes merging a branch or
        // deleting a tag reachable only with a mouse.
        (Key::Named(Named::ArrowDown), Modal::Sheet) => Message::SheetStepped(1),
        (Key::Named(Named::ArrowUp), Modal::Sheet) => Message::SheetStepped(-1),
        (Key::Named(Named::Enter), Modal::Sheet) => Message::SheetAccepted,
        // Dismisses the dialog, not the flow: the token still arrives and is
        // still stored, which is why there is no `Enter` binding to "accept".
        (Key::Named(Named::Escape), Modal::DeviceCode) => Message::DeviceCodeDismissed,
        // `Tab` moves between a prompt's fields. A two-field prompt — "Add a
        // remote" takes a name and a URL — was otherwise reachable only with a
        // mouse, because the global `Tab` binding cycles panes and modals
        // swallow everything they do not name.
        (Key::Named(Named::Tab), Modal::Prompt) => Message::PromptFieldStepped,
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
    let stashes = backend.stashes()?;
    let remotes = backend.remotes()?;
    let total = backend.commit_count(&RevSpec::All)?;
    let first_page = backend.log(&RevSpec::All, LogPage::first(PAGE_SIZE))?;

    Ok(OpenedRepository {
        path: backend.workdir().to_path_buf(),
        backend: Arc::new(backend),
        head,
        refs,
        state,
        status,
        stashes,
        remotes,
        total,
        first_page,
    })
}

/// Runs a repository write off the UI thread.
///
/// Every write reports the same way: nothing on success, because the refresh
/// that follows says more than a toast could, and the error verbatim on
/// failure — Git's own stderr is better than any paraphrase of it.
/// What can be done to one commit.
///
/// Built here rather than in the detail widget because a sheet is an
/// application-level [`Message`] and the detail pane speaks [`RepoMessage`].
fn commit_sheet(index: usize, id: ObjectId) -> ActionSheet {
    let short = id.short(7);
    ActionSheet::new(short.clone())
        .item(
            "Cherry-pick onto this branch",
            Message::Repo(index, RepoMessage::CherryPickRequested(id)),
        )
        .item(
            "Revert",
            Message::Repo(index, RepoMessage::RevertRequested(id)),
        )
        // The three resets are spelled out rather than hidden behind one
        // "Reset" with a mode picker: the difference between them is the
        // difference between keeping your work and losing it.
        .item(
            format!("Reset to {short}, keeping the changes staged"),
            Message::Repo(
                index,
                RepoMessage::ResetRequested {
                    to: id,
                    mode: ResetMode::Soft,
                },
            ),
        )
        .item(
            format!("Reset to {short}, keeping the changes"),
            Message::Repo(
                index,
                RepoMessage::ResetRequested {
                    to: id,
                    mode: ResetMode::Mixed,
                },
            ),
        )
        .destructive(
            format!("Reset to {short}, discarding the changes"),
            Message::Repo(
                index,
                RepoMessage::ResetRequested {
                    to: id,
                    mode: ResetMode::Hard,
                },
            ),
        )
}

/// Splits text into lines that keep their own terminators.
///
/// The counterpart to what `hidegit_core::conflict` does when it parses, so a
/// hand-typed resolution goes back through `render` in the same shape every
/// other side is in. Without it a custom resolution would arrive as one line
/// containing newlines, and joining it to another side would glue them.
fn split_kept(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find('\n') {
            Some(at) => {
                lines.push(rest[..=at].to_owned());
                rest = &rest[at + 1..];
            }
            None => {
                lines.push(rest.to_owned());
                break;
            }
        }
    }
    lines
}

/// Git's own word for the operation in progress, for the sentences around it.
///
/// Lowercase because every use is mid-sentence — "Abort the rebase?" — and
/// [`RepoState`]'s own labels are title-case for the banner.
fn operation_verb(state: RepoState) -> &'static str {
    match state {
        RepoState::Merging => "merge",
        RepoState::Rebasing => "rebase",
        RepoState::CherryPicking => "cherry-pick",
        RepoState::Reverting => "revert",
        RepoState::Bisecting => "bisect",
        RepoState::Clean => "operation",
    }
}

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

/// The folder name a clone of `url` should land in.
///
/// The last path segment with any `.git` suffix removed, which is what `git clone`
/// itself does. Falls back to `repository` for a URL with nothing usable in it,
/// rather than cloning into a folder called `""`.
fn repository_name(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed
        .rsplit(['/', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("");
    let name = last.strip_suffix(".git").unwrap_or(last);

    if name.is_empty() {
        "repository".to_owned()
    } else {
        name.to_owned()
    }
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
        // One ref and one reflog, which is cheap enough for the watcher path —
        // unlike ahead/behind, which is a walk per branch and has its own task.
        stashes: backend.stashes()?,
        // Read here rather than in its own task: it is a config read, not a walk,
        // and a remote that was just added has to appear at once.
        remotes: backend.remotes()?,
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
    mod render;

    use super::*;
    use hidegit_core::FakeBackend;
    use hidegit_core::model::{
        ChangeStatus, Commit, Diff, FileChange, Head, RefKind, RefName, Refs, RepoState, Signature,
        WorktreeStatus,
    };
    use hidegit_core::ops::RebaseAction;
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
            stashes: Vec::new(),
            remotes: Vec::new(),
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

    /// An app on branch `main` with a `feature` branch to drag.
    fn app_with_branches() -> Hidegit {
        let mut app = app_with(3);
        {
            let repo = app.app.repos.get_mut(0).unwrap();
            repo.head = Head::Branch {
                name: RefName {
                    kind: RefKind::LocalBranch,
                    full: "refs/heads/main".to_owned(),
                    short: "main".to_owned(),
                },
                target: repo.graph.commits[0].id,
            };
        }
        app
    }

    #[test]
    fn dropping_a_branch_asks_before_anything_runs() {
        let mut app = app_with_branches();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::BranchDropped {
                source: "feature".to_owned(),
                target: "main".to_owned(),
            },
        ));

        let sheet = app.app.sheet.as_ref().expect("a drop asks first");
        let labels: Vec<&str> = sheet.items.iter().map(|i| i.label.as_str()).collect();
        // Every entry names both branches: a gesture is exactly the thing whose
        // direction people misread.
        assert!(
            labels.contains(&"Merge feature into main"),
            "got {labels:?}"
        );
        assert!(
            labels.contains(&"Rebase main onto feature…"),
            "got {labels:?}"
        );
        assert_eq!(sheet.title, "feature → main");
    }

    #[test]
    fn the_drag_direction_does_not_change_what_is_offered() {
        // Both operations act on the checked-out branch, so dragging `main`
        // onto `feature` offers exactly what the reverse does. Only the title
        // records which way the hand moved.
        let mut app = app_with_branches();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::BranchDropped {
                source: "main".to_owned(),
                target: "feature".to_owned(),
            },
        ));

        let sheet = app.app.sheet.as_ref().expect("a drop asks first");
        let labels: Vec<&str> = sheet.items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.contains(&"Merge feature into main"),
            "got {labels:?}"
        );
        assert_eq!(sheet.title, "main → feature");
    }

    #[test]
    fn a_drop_between_two_branches_you_are_not_on_says_so() {
        // Both operations need a checkout first, and doing that silently is the
        // unintended thing the gesture has to avoid.
        let mut app = app_with_branches();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::BranchDropped {
                source: "one".to_owned(),
                target: "two".to_owned(),
            },
        ));

        assert!(app.app.sheet.is_none());
        assert_eq!(app.app.toasts.len(), 1);
        assert!(
            app.app.toasts[0].summary.contains("checked out"),
            "got {:?}",
            app.app.toasts[0].summary
        );
    }

    #[test]
    fn a_drop_mid_operation_is_ignored() {
        let mut app = app_with_branches();
        {
            let repo = app.app.repos.get_mut(0).unwrap();
            repo.state = RepoState::Rebasing;
        }

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::BranchDropped {
                source: "feature".to_owned(),
                target: "main".to_owned(),
            },
        ));

        // A rebase owns HEAD until it is finished or aborted.
        assert!(app.app.sheet.is_none());
    }

    #[test]
    fn a_hard_reset_names_what_it_will_destroy() {
        let mut app = app_with(3);
        {
            let repo = app.app.repos.get_mut(0).unwrap();
            repo.status = dirty();
        }
        let id = app.app.active_repo().unwrap().graph.commits[1].id;

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::ResetRequested {
                to: id,
                mode: ResetMode::Hard,
            },
        ));

        let confirming = app.app.confirming.as_ref().expect("a hard reset confirms");
        // A warning that counts what is at stake is one people read.
        assert!(
            confirming.body.contains('3'),
            "it names how much is at stake, got {:?}",
            confirming.body
        );
        assert_eq!(confirming.confirm_label, "Hard reset");
    }

    #[test]
    fn the_gentler_resets_do_not_confirm() {
        // Confirming a soft reset would teach people to click through the
        // warning that matters.
        for mode in [ResetMode::Soft, ResetMode::Mixed] {
            let mut app = app_with(3);
            let id = app.app.active_repo().unwrap().graph.commits[1].id;

            let _ = app.update(Message::Repo(
                0,
                RepoMessage::ResetRequested { to: id, mode },
            ));

            assert!(
                app.app.confirming.is_none(),
                "{mode:?} keeps the work as changes and needs no warning"
            );
        }
    }

    #[test]
    fn rebasing_confirms_because_it_rewrites_history() {
        let mut app = app_with(3);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::RebaseRequested("main".to_owned()),
        ));

        let confirming = app.app.confirming.as_ref().expect("a rebase confirms");
        assert!(
            confirming.body.contains("new id"),
            "it says the commits are rewritten, got {:?}",
            confirming.body
        );
    }

    #[test]
    fn a_commit_mid_operation_offers_no_history_actions() {
        // A merge or rebase owns HEAD until it is finished or aborted, and every
        // action on the sheet moves it.
        let mut app = app_with(3);
        {
            let repo = app.app.repos.get_mut(0).unwrap();
            repo.state = RepoState::Merging;
        }
        let id = app.app.active_repo().unwrap().graph.commits[0].id;

        let _ = app.update(Message::Repo(0, RepoMessage::CommitActionsRequested(id)));

        assert!(app.app.sheet.is_none());
    }

    #[test]
    fn the_commit_sheet_spells_the_three_resets_out() {
        let mut app = app_with(3);
        let id = app.app.active_repo().unwrap().graph.commits[0].id;

        let _ = app.update(Message::Repo(0, RepoMessage::CommitActionsRequested(id)));

        let sheet = app.app.sheet.as_ref().expect("the sheet opens");
        let labels: Vec<&str> = sheet.items.iter().map(|i| i.label.as_str()).collect();

        assert!(labels.iter().any(|l| l.starts_with("Cherry-pick")));
        assert!(labels.contains(&"Revert"));
        // Three separate entries, not one "Reset" behind a mode picker: the
        // difference between them is the difference between keeping your work
        // and losing it.
        assert_eq!(
            labels.iter().filter(|l| l.starts_with("Reset")).count(),
            3,
            "got {labels:?}"
        );
        // And only the destroying one is marked destructive.
        assert_eq!(sheet.items.iter().filter(|i| i.destructive).count(), 1);
    }

    #[test]
    fn a_rewrite_that_deletes_the_open_commit_moves_the_selection() {
        // Dropping a commit in a rebase, squashing one, resetting past one —
        // all leave the detail pane rendering a commit that no longer exists
        // from the copy it already holds, which reads as "still there".
        let mut app = app_with(5);
        let doomed = app.app.active_repo().unwrap().graph.commits[2].id;
        {
            let repo = app.app.repos.get_mut(0).unwrap();
            repo.selection = Some(Selection::Commit(doomed));
        }

        // A refresh whose history no longer contains it.
        let survivors: Vec<_> = commits(5).into_iter().filter(|c| c.id != doomed).collect();
        let refreshed = Refreshed {
            head: opened(5).head,
            refs: Refs::default(),
            state: RepoState::Clean,
            status: WorktreeStatus::default(),
            stashes: Vec::new(),
            remotes: Vec::new(),
            total: survivors.len(),
            first_page: survivors,
        };
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Refreshed(Box::new(Ok(refreshed))),
        ));

        let repo = app.app.active_repo().unwrap();
        assert_ne!(
            repo.selection,
            Some(Selection::Commit(doomed)),
            "the pane must not go on showing a commit that was rewritten away"
        );
        assert!(matches!(repo.selection, Some(Selection::Commit(_))));
    }

    #[test]
    fn a_commit_merely_not_paged_in_yet_keeps_its_selection() {
        // The counterpart: on a large repository the graph is paged, and a
        // commit missing from the loaded window has usually just not been
        // walked to. Clearing then would fight the user on every refresh.
        let mut app = app_with(5);
        let chosen = app.app.active_repo().unwrap().graph.commits[2].id;
        {
            let repo = app.app.repos.get_mut(0).unwrap();
            repo.selection = Some(Selection::Commit(chosen));
        }

        let refreshed = Refreshed {
            head: opened(5).head,
            refs: Refs::default(),
            state: RepoState::Clean,
            status: WorktreeStatus::default(),
            stashes: Vec::new(),
            remotes: Vec::new(),
            // More history exists than arrived, so the graph is still loading.
            total: 5_000,
            first_page: commits(2),
        };
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Refreshed(Box::new(Ok(refreshed))),
        ));

        assert_eq!(
            app.app.active_repo().unwrap().selection,
            Some(Selection::Commit(chosen)),
        );
    }

    /// An app with a three-commit plan open, onto `main`.
    fn app_planning() -> Hidegit {
        let mut app = app_with(3);
        let commits = app.app.active_repo().unwrap().graph.commits.clone();
        // Oldest first, as `rebase_preview` returns them.
        let oldest_first: Vec<_> = commits.into_iter().rev().collect();
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::RebasePlanLoaded(Box::new(Ok(("main".to_owned(), oldest_first)))),
        ));
        app
    }

    fn plan_of(app: &Hidegit) -> &crate::state::RebaseEditor {
        app.app.active_repo().unwrap().plan.as_ref().unwrap()
    }

    #[test]
    fn a_loaded_plan_starts_as_all_picks() {
        let app = app_planning();
        let plan = plan_of(&app);

        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.kept(), 3);
        assert!(
            plan.steps
                .iter()
                .all(|s| matches!(s.action, RebaseAction::Pick))
        );
        // All picks is exactly an ordinary rebase, so it is runnable as it
        // stands — opening the editor must not require editing anything.
        assert!(plan.blocked().is_none());
    }

    #[test]
    fn moving_a_step_carries_the_selection_with_it() {
        let mut app = app_planning();
        let first = plan_of(&app).steps[0].commit.id;

        let _ = app.update(Message::Repo(0, RepoMessage::PlanRowSelected(0)));
        let _ = app.update(Message::Repo(0, RepoMessage::PlanRowMoved(1)));

        let plan = plan_of(&app);
        assert_eq!(plan.steps[1].commit.id, first, "the step moved down one");
        assert_eq!(
            plan.selected, 1,
            "the selection follows it, or the next move acts on a different commit"
        );
    }

    #[test]
    fn moving_is_clamped_at_both_ends() {
        // A step that jumped from the top to the bottom would be a reorder
        // nobody asked for.
        let mut app = app_planning();
        let before: Vec<_> = plan_of(&app).steps.iter().map(|s| s.commit.id).collect();

        let _ = app.update(Message::Repo(0, RepoMessage::PlanRowSelected(0)));
        let _ = app.update(Message::Repo(0, RepoMessage::PlanRowMoved(-1)));

        let after: Vec<_> = plan_of(&app).steps.iter().map(|s| s.commit.id).collect();
        assert_eq!(before, after);
        assert_eq!(plan_of(&app).selected, 0);
    }

    #[test]
    fn a_plan_that_squashes_first_says_why_it_cannot_run() {
        // `squash` folds into the commit above, and the first step has none.
        // Git refuses it outright, so the editor does too — with the sentence,
        // because a greyed button that explains nothing is a dead end.
        let mut app = app_planning();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PlanActionChosen(0, RebaseAction::Squash),
        ));

        let why = plan_of(&app).blocked().expect("it cannot run");
        assert!(why.contains("nothing above it"), "got {why:?}");
    }

    #[test]
    fn dropping_everything_is_refused() {
        let mut app = app_planning();
        for at in 0..3 {
            let _ = app.update(Message::Repo(
                0,
                RepoMessage::PlanActionChosen(at, RebaseAction::Drop),
            ));
        }

        let plan = plan_of(&app);
        assert_eq!(plan.kept(), 0);
        assert!(
            plan.blocked().is_some_and(|w| w.contains("nothing")),
            "a branch with every commit dropped is not what anyone means"
        );
    }

    #[test]
    fn starting_a_blocked_plan_does_nothing_and_keeps_the_editor_open() {
        let mut app = app_planning();
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PlanActionChosen(0, RebaseAction::Squash),
        ));

        let _ = app.update(Message::Repo(0, RepoMessage::PlanStarted));

        assert!(
            app.app.active_repo().unwrap().plan.is_some(),
            "a refused start must not close the screen and lose the plan"
        );
    }

    #[test]
    fn cancelling_needs_no_confirmation_because_nothing_has_run() {
        let mut app = app_planning();

        let _ = app.update(Message::Repo(0, RepoMessage::PlanDismissed));

        assert!(app.app.active_repo().unwrap().plan.is_none());
        assert!(
            app.app.confirming.is_none(),
            "nothing touched the repository, so there is nothing to warn about"
        );
    }

    #[test]
    fn the_plan_keeps_the_order_on_screen() {
        // The editor shows oldest first, which is todo order. If `plan()` did
        // not preserve it, every reorder would be silently inverted.
        let mut app = app_planning();
        let _ = app.update(Message::Repo(0, RepoMessage::PlanRowSelected(2)));
        let _ = app.update(Message::Repo(0, RepoMessage::PlanRowMoved(-1)));

        let editor = plan_of(&app);
        let on_screen: Vec<_> = editor.steps.iter().map(|s| s.commit.id).collect();
        let handed_over: Vec<_> = editor.plan().steps.iter().map(|s| s.commit).collect();

        assert_eq!(on_screen, handed_over);
    }

    /// A repository stopped mid-merge with one conflicted path.
    fn conflicted() -> WorktreeStatus {
        WorktreeStatus {
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
            conflicted: vec![hidegit_core::model::Conflict {
                path: PathBuf::from("shared.txt"),
                kind: hidegit_core::model::ConflictKind::BothModified,
            }],
            state: RepoState::Merging,
        }
    }

    const MARKED: &str = "before\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> side\nafter\n";

    /// An app mid-merge with the resolver already open on `shared.txt`.
    fn app_resolving() -> Hidegit {
        let mut app = app_with(3);
        {
            let repo = app.app.repos.get_mut(0).unwrap();
            repo.status = conflicted();
            repo.state = RepoState::Merging;
        }
        let file = hidegit_core::conflict::parse(MARKED).expect("the fixture parses");
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::ConflictFileLoaded(Box::new(Ok((PathBuf::from("shared.txt"), file)))),
        ));
        app
    }

    #[test]
    fn a_loaded_conflict_starts_undecided() {
        let app = app_resolving();
        let resolver = app.app.active_repo().unwrap().resolver.as_ref().unwrap();

        assert_eq!(resolver.conflict_count(), 1);
        assert_eq!(resolver.remaining(), 1);
        assert!(
            !resolver.is_resolved(),
            "nothing is decided until the user decides it"
        );
        // Rendering an untouched file must reproduce it, or merely opening the
        // resolver would rewrite the working tree.
        assert_eq!(resolver.rendered(), MARKED);
    }

    #[test]
    fn choosing_a_side_resolves_that_conflict() {
        let mut app = app_resolving();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::ConflictResolved(0, Resolution::Theirs),
        ));

        let resolver = app.app.active_repo().unwrap().resolver.as_ref().unwrap();
        assert!(resolver.is_resolved());
        assert_eq!(resolver.remaining(), 0);
        assert_eq!(resolver.rendered(), "before\ntheirs\nafter\n");
    }

    #[test]
    fn a_failed_parse_leaves_no_resolver_and_says_so() {
        let mut app = app_with(3);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::ConflictFileLoaded(Box::new(Err(UiError {
                summary: "line 4 has a `=======` outside any conflict".to_owned(),
                details: String::new(),
            }))),
        ));

        // Leaving the file alone is the honest answer: rewriting it from a
        // half-understood parse could lose a side.
        assert!(app.app.active_repo().unwrap().resolver.is_none());
        assert_eq!(app.app.toasts.len(), 1);
    }

    #[test]
    fn stepping_is_clamped_rather_than_wrapping() {
        // Wrapping past the last conflict looks identical to having finished,
        // and the difference is the whole point of the screen.
        let mut app = app_resolving();

        let _ = app.update(Message::Repo(0, RepoMessage::ConflictStepped(1)));
        assert_eq!(
            app.app
                .active_repo()
                .unwrap()
                .resolver
                .as_ref()
                .unwrap()
                .focused,
            0
        );

        let _ = app.update(Message::Repo(0, RepoMessage::ConflictStepped(-1)));
        assert_eq!(
            app.app
                .active_repo()
                .unwrap()
                .resolver
                .as_ref()
                .unwrap()
                .focused,
            0
        );
    }

    #[test]
    fn selecting_a_different_row_closes_the_resolver() {
        let mut app = app_resolving();
        {
            let repo = app.app.repos.get_mut(0).unwrap();
            repo.status.unstaged = vec![change("changed.txt", ChangeStatus::Modified)];
        }

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::StagingRowSelected(crate::state::StagingRow {
                section: Section::Unstaged,
                index: 0,
            }),
        ));

        // The decisions on screen must always belong to the file named above
        // them.
        assert!(app.app.active_repo().unwrap().resolver.is_none());
    }

    #[test]
    fn reopening_the_same_file_keeps_the_decisions_made_so_far() {
        // The watcher refreshes status on every save, and the spec says
        // navigation must never lose a partial resolution.
        let mut app = app_resolving();
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::ConflictResolved(0, Resolution::Ours),
        ));

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::ConflictOpenRequested(PathBuf::from("shared.txt")),
        ));

        let resolver = app.app.active_repo().unwrap().resolver.as_ref().unwrap();
        assert!(
            resolver.is_resolved(),
            "reopening the file already open must not discard the decision"
        );
    }

    #[test]
    fn aborting_confirms_before_it_throws_anything_away() {
        let mut app = app_resolving();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::SequenceControlRequested(SequenceControl::Abort),
        ));

        let confirming = app.app.confirming.as_ref().expect("abort confirms first");
        assert!(
            confirming.title.contains("merge"),
            "it names the operation being abandoned, got {:?}",
            confirming.title
        );
        assert!(
            app.app.active_repo().unwrap().resolver.is_some(),
            "nothing is discarded until the confirmation is accepted"
        );
    }

    #[test]
    fn a_custom_resolution_keeps_its_line_terminators() {
        // A hand-typed resolution goes back through `render` alongside the
        // other sides, so it has to arrive in the same shape: one entry per
        // line, terminator included.
        assert_eq!(
            split_kept("one\ntwo\n"),
            vec!["one\n".to_owned(), "two\n".to_owned()]
        );
        // A last line with no terminator stays that way.
        assert_eq!(
            split_kept("one\ntwo"),
            vec!["one\n".to_owned(), "two".to_owned()]
        );
        assert!(split_kept("").is_empty());
    }

    fn app_searching() -> Hidegit {
        let mut app = app_with(5);
        let _ = app.update(Message::Repo(0, RepoMessage::SearchRequested));
        app
    }

    fn search_of(app: &Hidegit) -> &crate::state::Search {
        app.app.active_repo().unwrap().search.as_ref().unwrap()
    }

    #[test]
    fn a_stale_search_result_never_overwrites_a_newer_one() {
        // Typing is faster than a walk of a large history, so a search for
        // "par" can land after the user has already typed "parser". Without the
        // query travelling with the result, the older answer wins.
        let mut app = app_searching();
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::SearchChanged("parser".into()),
        ));

        let stale = hidegit_core::ops::SearchResults {
            hits: vec![hidegit_core::ops::SearchHit {
                commit: commits(1).remove(0),
                field: hidegit_core::ops::SearchField::Summary,
            }],
            truncated: false,
        };
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::SearchFinished {
                query: "par".into(),
                results: Box::new(Ok(stale)),
            },
        ));

        assert!(
            search_of(&app).results.hits.is_empty(),
            "a result for a query nobody is asking any more is dropped"
        );
        assert!(
            search_of(&app).running,
            "the newer search is still in flight"
        );
    }

    #[test]
    fn clearing_the_box_clears_the_results() {
        // Otherwise the last search's hits sit under an empty query, which
        // reads as "these match nothing".
        let mut app = app_searching();
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::SearchFinished {
                query: String::new(),
                results: Box::new(Ok(hidegit_core::ops::SearchResults {
                    hits: vec![hidegit_core::ops::SearchHit {
                        commit: commits(1).remove(0),
                        field: hidegit_core::ops::SearchField::Summary,
                    }],
                    truncated: false,
                })),
            },
        ));
        assert_eq!(search_of(&app).results.hits.len(), 1);

        let _ = app.update(Message::Repo(0, RepoMessage::SearchChanged("   ".into())));

        assert!(search_of(&app).results.hits.is_empty());
        assert!(!search_of(&app).running, "and nothing is left running");
    }

    #[test]
    fn cmd_f_opens_the_search_and_cmd_shift_f_still_fetches() {
        let f = keyboard::Key::Character("f".into());
        let command = keyboard::Modifiers::COMMAND;
        let command_shift = keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT;

        assert!(matches!(
            shortcut(
                &f,
                command,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::SearchRequested)
        ));
        // The shifted form has fetched every remote since M3 and must keep it.
        assert!(matches!(
            shortcut(
                &f,
                command_shift,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::FetchRequested)
        ));
        // With Shift held, most layouts report the shifted character.
        let upper = keyboard::Key::Character("F".into());
        assert!(matches!(
            shortcut(
                &upper,
                command_shift,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::FetchRequested)
        ));
    }

    #[test]
    fn the_search_owns_the_keyboard_and_the_arrows_drive_the_list() {
        let down = keyboard::Key::Named(keyboard::key::Named::ArrowDown);
        let escape = keyboard::Key::Named(keyboard::key::Named::Escape);
        let j = keyboard::Key::Character("j".into());
        let none = keyboard::Modifiers::default();

        assert!(matches!(
            shortcut(
                &down,
                none,
                KeyContext {
                    active: Some(0),
                    searching: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::SearchStepped(1))
        ));
        assert!(matches!(
            shortcut(
                &escape,
                none,
                KeyContext {
                    active: Some(0),
                    searching: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::SearchDismissed)
        ));
        // A bare letter belongs to the box being typed into, not to the hunks
        // behind the panel.
        assert!(matches!(
            shortcut(
                &j,
                none,
                KeyContext {
                    active: Some(0),
                    searching: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::ToastDismissed(_)
        ));
    }

    #[test]
    fn accepting_a_hit_closes_the_search_and_scrolls_to_the_commit() {
        // A search that found a commit and left you looking somewhere else has
        // not finished the job. The selection itself travels as a follow-up
        // message, which this harness does not run — what is asserted here is
        // what `update` does synchronously.
        // A history long enough that scrolling means something: with everything
        // on screen at once, clamping correctly pins the scroll at zero and the
        // assertion would be vacuous.
        let mut app = app_with(500);
        let _ = app.update(Message::Repo(0, RepoMessage::ViewportChanged(20)));
        let _ = app.update(Message::Repo(0, RepoMessage::SearchRequested));
        let wanted = app.app.active_repo().unwrap().graph.commits[200].id;

        let _ = app.update(Message::Repo(0, RepoMessage::SearchAccepted(wanted)));

        let repo = app.app.active_repo().unwrap();
        assert!(repo.search.is_none(), "the panel closes");
        assert_eq!(
            repo.graph.scroll, 200.0,
            "and the graph moves to the commit rather than leaving it off screen"
        );
    }

    #[test]
    fn stepping_is_clamped_to_the_hits_that_exist() {
        let mut app = app_searching();
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::SearchFinished {
                query: String::new(),
                results: Box::new(Ok(hidegit_core::ops::SearchResults {
                    hits: commits(2)
                        .into_iter()
                        .map(|commit| hidegit_core::ops::SearchHit {
                            commit,
                            field: hidegit_core::ops::SearchField::Summary,
                        })
                        .collect(),
                    truncated: false,
                })),
            },
        ));

        let _ = app.update(Message::Repo(0, RepoMessage::SearchStepped(-1)));
        assert_eq!(search_of(&app).selected, 0);

        for _ in 0..5 {
            let _ = app.update(Message::Repo(0, RepoMessage::SearchStepped(1)));
        }
        assert_eq!(search_of(&app).selected, 1, "clamped to the last hit");
    }

    #[test]
    fn the_settings_panel_opens_on_its_shortcut_and_closes_on_escape() {
        // `Cmd+,` has been in the shortcut table since M1 with nothing behind
        // it, and the light theme was unreachable without editing a file the
        // application never creates.
        let mut app = app_with(3);
        let comma = keyboard::Key::Character(",".into());
        assert!(matches!(
            shortcut(
                &comma,
                keyboard::Modifiers::COMMAND,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::SettingsRequested
        ));

        let _ = app.update(Message::SettingsRequested);
        assert!(app.app.settings_open);

        let _ = app.update(Message::SettingsDismissed);
        assert!(!app.app.settings_open);
    }

    #[test]
    fn the_open_panel_owns_the_keyboard() {
        // Otherwise `j` steps through hunks behind it, the way it would behind
        // any other layer over the screen.
        let j = keyboard::Key::Character("j".into());
        let none = keyboard::Modifiers::default();

        assert!(matches!(
            shortcut(
                &j,
                none,
                KeyContext {
                    active: Some(0),
                    settings_open: true,
                    ..KeyContext::default()
                }
            ),
            Message::ToastDismissed(_)
        ));
        let escape = keyboard::Key::Named(keyboard::key::Named::Escape);
        assert!(matches!(
            shortcut(
                &escape,
                none,
                KeyContext {
                    active: Some(0),
                    settings_open: true,
                    ..KeyContext::default()
                }
            ),
            Message::SettingsDismissed
        ));
    }

    #[test]
    fn choosing_a_theme_applies_it_immediately() {
        // A theme you have to restart to see is a theme you cannot choose
        // between.
        let mut app = app_with(3);
        assert_eq!(app.app.theme.palette, crate::theme::Palette::DARK);

        let _ = app.update(Message::ThemeChosen(
            crate::theme::Theme::LIGHT_NAME.to_owned(),
        ));

        assert_eq!(app.app.theme.palette, crate::theme::Palette::LIGHT);
    }

    #[test]
    fn turning_alerts_off_keeps_the_per_event_choices() {
        // So turning them back on restores the set you chose rather than a
        // default. The screen makes the rows unavailable; it must not clear
        // them.
        let mut app = app_with(3);
        let _ = app.update(Message::AlertToggled(
            crate::message::AlertToggle::ChecksPassed,
        ));
        let chosen = app.app.alerts.events.checks_passed;

        let _ = app.update(Message::AlertToggled(crate::message::AlertToggle::Enabled));
        assert!(!app.app.alerts.enabled);
        assert_eq!(app.app.alerts.events.checks_passed, chosen);
    }

    #[test]
    fn the_configured_theme_is_applied_and_a_typo_falls_back() {
        // The config has carried a theme name since M1 and nothing read it, so
        // setting one did nothing at all.
        let (app, _) = Hidegit::new(Vec::new(), Vec::new(), Default::default(), "hidegit-light");
        assert_eq!(app.app.theme.palette, crate::theme::Palette::LIGHT);

        let (app, _) = Hidegit::new(Vec::new(), Vec::new(), Default::default(), "hidegit-nope");
        assert_eq!(
            app.app.theme.palette,
            crate::theme::Palette::DARK,
            "a typo falls back rather than leaving the window unthemed"
        );
    }

    /// An app with `count` repositories open, each at a distinct path.
    fn app_with_tabs(count: usize) -> Hidegit {
        let mut app = Hidegit::default();
        for n in 0..count {
            let mut opened = opened(5);
            opened.path = PathBuf::from(format!("/fake/repo-{n}"));
            let _ = app.update(Message::RepositoryOpened(Box::new(Ok(opened))));
        }
        app
    }

    fn sheet_of(app: &Hidegit) -> &ActionSheet {
        app.app.sheet.as_ref().expect("a sheet is open")
    }

    fn open_sheet(app: &mut Hidegit) {
        let sheet = ActionSheet::new("feat/graph")
            .item("Checkout", Message::ToastDismissed(1))
            .item("Rename…", Message::ToastDismissed(2))
            .destructive("Delete", Message::ToastDismissed(3));
        let _ = app.update(Message::SheetRequested(Box::new(sheet)));
    }

    #[test]
    fn a_sheet_opens_with_nothing_chosen_and_enter_does_nothing() {
        // Every per-item action in the sidebar goes through a sheet, and one of
        // a sheet's rows may be "Delete". `Enter` before you have moved to a row
        // must not pick one for you.
        let mut app = app_with(3);
        open_sheet(&mut app);
        assert_eq!(sheet_of(&app).selected, None);

        let _ = app.update(Message::SheetAccepted);

        assert!(
            app.app.sheet.is_some(),
            "nothing was chosen, so the sheet is still up"
        );
    }

    #[test]
    fn the_arrows_walk_a_sheet_and_wrap() {
        // Without this a sheet is reachable only with a mouse, which puts every
        // branch, tag, remote and stash action behind one.
        let mut app = app_with(3);
        open_sheet(&mut app);

        let _ = app.update(Message::SheetStepped(1));
        assert_eq!(sheet_of(&app).selected, Some(0), "down lands on the first");

        let _ = app.update(Message::SheetStepped(-1));
        assert_eq!(sheet_of(&app).selected, Some(2), "and wraps to the last");

        let _ = app.update(Message::SheetStepped(1));
        assert_eq!(sheet_of(&app).selected, Some(0));
    }

    #[test]
    fn up_from_nothing_lands_on_the_last_row() {
        let mut app = app_with(3);
        open_sheet(&mut app);

        let _ = app.update(Message::SheetStepped(-1));

        assert_eq!(sheet_of(&app).selected, Some(2));
    }

    #[test]
    fn the_sheet_keys_are_bound_on_the_modal_layer() {
        let down = keyboard::Key::Named(keyboard::key::Named::ArrowDown);
        let enter = keyboard::Key::Named(keyboard::key::Named::Enter);
        let tab = keyboard::Key::Named(keyboard::key::Named::Tab);

        assert!(matches!(
            modal_shortcut(&down, Modal::Sheet),
            Message::SheetStepped(1)
        ));
        assert!(matches!(
            modal_shortcut(&enter, Modal::Sheet),
            Message::SheetAccepted
        ));
        // A two-field prompt was otherwise reachable only with a mouse: the
        // global `Tab` cycles panes, and modals swallow what they do not name.
        assert!(matches!(
            modal_shortcut(&tab, Modal::Prompt),
            Message::PromptFieldStepped
        ));
    }

    #[test]
    fn opening_a_repository_that_is_already_open_switches_to_its_tab() {
        // Two tabs on one repository would each hold their own idea of its
        // state, and the one you were not looking at would be wrong the moment
        // you committed in the other.
        let mut app = app_with_tabs(2);
        assert_eq!(app.app.active, Some(1));

        let _ = app.update(Message::OpenRepository(PathBuf::from("/fake/repo-0")));

        assert_eq!(app.app.repos.len(), 2, "no second copy was opened");
        assert_eq!(app.app.active, Some(0));
    }

    #[test]
    fn closing_a_tab_lands_on_its_neighbour_not_the_last() {
        // Closing the first of three and landing on the third is the kind of
        // jump that makes you lose your place.
        let mut app = app_with_tabs(3);

        let _ = app.update(Message::CloseRepository(0));

        assert_eq!(app.app.repos.len(), 2);
        assert_eq!(app.app.active, Some(0));
        assert_eq!(
            app.app.repos[0].path,
            PathBuf::from("/fake/repo-1"),
            "the tab that took its place"
        );
    }

    #[test]
    fn closing_the_last_tab_falls_back_to_the_one_before_it() {
        let mut app = app_with_tabs(3);

        let _ = app.update(Message::CloseRepository(2));

        assert_eq!(app.app.active, Some(1));
    }

    #[test]
    fn closing_a_tab_reindexes_the_canvas_caches() {
        // They are keyed by position, so removing one shifts every later
        // repository down. Left alone, every tab after the closed one would
        // draw another tab's graph — and `screen()` expects a cache per
        // repository, so it would panic outright.
        let mut app = app_with_tabs(3);

        let _ = app.update(Message::CloseRepository(0));

        assert_eq!(app.caches.len(), 2);
        for index in 0..app.app.repos.len() {
            assert!(
                app.caches.contains_key(&index),
                "no cache for tab {index}: {:?}",
                app.caches.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn closing_the_only_tab_goes_back_to_the_welcome_screen() {
        let mut app = app_with_tabs(1);

        let _ = app.update(Message::CloseRepository(0));

        assert_eq!(app.app.screen, Screen::Welcome);
        assert_eq!(app.app.active, None);
    }

    #[test]
    fn the_number_keys_switch_tabs_and_stop_at_the_last() {
        let two = KeyContext {
            open_repos: 3,
            ..KeyContext::default()
        };
        let digit = |d: &str| keyboard::Key::Character(d.into());
        let command = keyboard::Modifiers::COMMAND;

        assert!(matches!(
            shortcut(&digit("2"), command, two),
            Message::RepositorySelected(1)
        ));
        // Past the last tab does nothing rather than clamping: `4` and `3` are
        // adjacent keys, and landing somewhere unintended is worse.
        assert!(matches!(
            shortcut(&digit("4"), command, two),
            Message::ToastDismissed(_)
        ));
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
    fn tab_cycles_panes_and_shift_tab_goes_back() {
        // It was bound to a stub that did nothing, which is worse than being
        // unbound: the shortcut table promised it.
        let tab = keyboard::Key::Named(keyboard::key::Named::Tab);
        let none = keyboard::Modifiers::default();
        let shift = keyboard::Modifiers::SHIFT;

        assert!(matches!(
            shortcut(
                &tab,
                none,
                KeyContext {
                    active: Some(0),
                    pane: Some(Pane::Sidebar),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::FocusCycled(Pane::Graph))
        ));
        assert!(matches!(
            shortcut(
                &tab,
                shift,
                KeyContext {
                    active: Some(0),
                    pane: Some(Pane::Sidebar),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::FocusCycled(Pane::Detail))
        ));
    }

    #[test]
    fn space_asks_about_focus_rather_than_staging_outright() {
        // iced keeps text-input focus inside the widget and `editing` only turns
        // true once a key has arrived, so a click into the message field
        // followed by `Space` would otherwise stage a file.
        let space = keyboard::Key::Named(keyboard::key::Named::Space);
        assert!(matches!(
            shortcut(
                &space,
                keyboard::Modifiers::default(),
                KeyContext {
                    active: Some(0),
                    pane: Some(Pane::Detail),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::StageToggleRequested)
        ));
    }

    #[test]
    fn space_stages_only_when_nothing_holds_focus() {
        // The safety-critical half: `false` means a field has focus, and the key
        // belongs to it. Staging then would put a file in the index in the
        // middle of a sentence.
        let fake = Arc::new(FakeBackend::new().with_commits(commits(3)));
        let mut app = Hidegit::default();
        let mut opened = opened(3);
        opened.backend = Arc::clone(&fake) as Arc<dyn GitBackend>;
        opened.status = dirty();
        let _ = app.update(Message::RepositoryOpened(Box::new(Ok(opened))));
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::StagingRowSelected(crate::state::StagingRow {
                section: Section::Unstaged,
                index: 0,
            }),
        ));

        let _ = app.update(Message::Repo(0, RepoMessage::StageToggleResolved(false)));
        assert!(
            fake.writes().is_empty(),
            "a focused field owns the key, so nothing is staged"
        );
    }

    #[test]
    fn the_commit_and_push_shortcut_is_checked_before_the_plain_one() {
        // Both are `Cmd`+`Enter`; without the shifted arm first the plain one
        // would swallow it and the push would never happen.
        let enter = keyboard::Key::Named(keyboard::key::Named::Enter);
        let command_shift = keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT;

        assert!(matches!(
            shortcut(
                &enter,
                command_shift,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::CommitAndPushRequested)
        ));
        assert!(matches!(
            shortcut(
                &enter,
                keyboard::Modifiers::COMMAND,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::CommitRequested)
        ));
    }

    #[test]
    fn a_failed_commit_abandons_the_push_that_was_asked_for() {
        // Pushing a commit that was never made is not what was asked for.
        let mut app = app_with(3);
        let _ = app.update(Message::Repo(0, RepoMessage::CommitAndPushRequested));
        assert!(app.app.active_repo().unwrap().draft.push_after_commit);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Committed(Box::new(Err(UiError {
                summary: "a hook refused it".to_owned(),
                details: String::new(),
            }))),
        ));

        assert!(!app.app.active_repo().unwrap().draft.push_after_commit);
    }

    #[test]
    fn the_bracket_keys_step_between_conflicts() {
        let command = keyboard::Modifiers::COMMAND;
        let close = keyboard::Key::Character("]".into());
        let open = keyboard::Key::Character("[".into());

        assert!(matches!(
            shortcut(
                &close,
                command,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::ConflictStepped(1))
        ));
        assert!(matches!(
            shortcut(
                &open,
                command,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::ConflictStepped(-1))
        ));
    }

    #[test]
    fn a_focused_text_field_swallows_the_bare_letter_shortcuts() {
        // `keyboard::listen()` is global and `j`/`k` are bound unmodified, so
        // without this, typing a commit message steps through hunks.
        let j = keyboard::Key::Character("j".into());
        let space = keyboard::Key::Named(keyboard::key::Named::Space);
        let mods = keyboard::Modifiers::default();

        assert!(matches!(
            shortcut(
                &j,
                mods,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::HunkStepped(1))
        ));
        assert!(matches!(
            shortcut(
                &j,
                mods,
                KeyContext {
                    active: Some(0),
                    editing: true,
                    ..KeyContext::default()
                }
            ),
            Message::ToastDismissed(_)
        ));

        // `Space` is bound now, but it *asks* rather than acting: the `editing`
        // guard above is not enough for it, because a click into a field
        // followed by `Space` arrives before any keystroke has set the flag.
        // The answer comes back as `StageToggleResolved`.
        assert!(matches!(
            shortcut(
                &space,
                mods,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            ),
            Message::Repo(0, RepoMessage::StageToggleRequested)
        ));
        // And while the flag *is* set, it never even asks.
        assert!(matches!(
            shortcut(
                &space,
                mods,
                KeyContext {
                    active: Some(0),
                    editing: true,
                    ..KeyContext::default()
                }
            ),
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
            shortcut(
                &enter,
                command,
                KeyContext {
                    active: Some(0),
                    editing: true,
                    ..KeyContext::default()
                }
            ),
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
                stashes: Vec::new(),
                remotes: Vec::new(),
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
                stashes: Vec::new(),
                remotes: Vec::new(),
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

    // ---- action sheets and prompts ----
    //
    // These assert what the UI *asked the backend for*, which is all the UI is
    // responsible for. Whether the argument vector is right is `HybridBackend`'s
    // job and is tested against a real repository in `hidegit-core`.

    #[test]
    fn a_sheet_and_a_prompt_are_raised_and_dismissed_by_state_alone() {
        let mut app = app_with(1);

        let _ = app.update(Message::SheetRequested(Box::new(
            crate::state::ActionSheet::new("feat/graph")
                .item("Checkout", Message::ToastDismissed(0)),
        )));
        assert!(app.app.sheet.is_some());
        assert!(app.app.is_modal());

        let _ = app.update(Message::SheetDismissed);
        assert!(app.app.sheet.is_none());
        assert!(!app.app.is_modal());
    }

    #[test]
    fn choosing_an_item_closes_the_sheet_before_the_action_runs() {
        // Found by eye: without this the sheet sat over the toast reporting that
        // the checkout it had just started was refused — which is the one thing
        // the user needed to read.
        let mut app = app_with(1);
        app.app.sheet = Some(crate::state::ActionSheet::new("feat/graph"));

        let _ = app.update(Message::SheetChosen(Box::new(Message::Repo(
            0,
            RepoMessage::CheckoutRequested(CheckoutTarget::Branch("main".to_owned())),
        ))));

        assert!(app.app.sheet.is_none());
    }

    #[test]
    fn a_prompt_raised_from_a_sheet_leaves_no_layer_behind() {
        // Two answers to the same question. Stacking them would leave a dead
        // layer over whatever the user does next.
        let mut app = app_with(1);
        app.app.sheet = Some(crate::state::ActionSheet::new("feat/graph"));

        // The route a sheet item actually takes.
        let _ = app.update(Message::SheetChosen(Box::new(Message::PromptRequested(
            Box::new(rename_prompt("old")),
        ))));
        let _ = app.update(Message::PromptRequested(Box::new(rename_prompt("old"))));

        assert!(app.app.sheet.is_none());
        assert!(app.app.prompt.is_some());
    }

    fn rename_prompt(from: &str) -> Prompt {
        Prompt {
            kind: PromptKind::RenameBranch {
                from: from.to_owned(),
            },
            title: format!("Rename {from}"),
            confirm_label: "Rename".to_owned(),
            fields: vec![crate::state::PromptField::prefilled("New name", from)],
        }
    }

    /// The action a prompt would produce, without running the task that does it.
    ///
    /// A `Task` is interpreted by the iced runtime, which a unit test does not
    /// have, so the assertion is on the pure function that decides *what* to ask
    /// for. Whether the backend then builds the right argument vector is
    /// `HybridBackend`'s job, tested against a real repository in `hidegit-core`.
    fn action_of(app: &Hidegit) -> Option<RepoMessage> {
        let prompt = app.app.prompt.clone()?;
        match app.prompt_action(&prompt)? {
            Message::Repo(_, message) => Some(message),
            other => {
                panic!("a prompt produced something other than a repository action: {other:?}")
            }
        }
    }

    #[test]
    fn a_prompt_builds_its_action_from_what_was_typed() {
        // The whole reason `Prompt` is not a `Confirmation`: the action cannot be
        // known until the typing is done.
        let mut app = app_with(1);
        let _ = app.update(Message::PromptRequested(Box::new(rename_prompt("old"))));
        let _ = app.update(Message::PromptChanged(0, "new".to_owned()));

        match action_of(&app) {
            Some(RepoMessage::BranchRenameRequested { from, to }) => {
                assert_eq!((from.as_str(), to.as_str()), ("old", "new"));
            }
            other => panic!("expected a rename, got {other:?}"),
        }

        let _ = app.update(Message::PromptAccepted);
        assert!(app.app.prompt.is_none(), "accepting closes the prompt");
    }

    /// A "new branch" prompt, as the sidebar raises it.
    fn new_branch_prompt(checkout: bool) -> Prompt {
        Prompt {
            kind: PromptKind::NewBranch {
                from: hidegit_core::ops::StartPoint::Head,
                checkout,
            },
            title: "New branch".to_owned(),
            confirm_label: "Create".to_owned(),
            fields: vec![crate::state::PromptField::new("Name", "feat/something")],
        }
    }

    #[test]
    fn a_prompt_accepted_empty_does_nothing_at_all() {
        let mut app = app_with(1);
        let _ = app.update(Message::PromptRequested(Box::new(new_branch_prompt(true))));

        assert!(
            !app.app.prompt.as_ref().unwrap().is_ready(),
            "the button is unavailable, and Enter must not act either"
        );
        assert!(action_of(&app).is_none(), "nothing to attempt");

        let _ = app.update(Message::PromptAccepted);
        assert!(app.app.prompt.is_none(), "it still closes");
    }

    #[test]
    fn a_name_typed_with_stray_whitespace_is_trimmed_before_it_reaches_git() {
        // Git refuses a ref name with a trailing space, and the refusal would be
        // about whitespace the user cannot see.
        let mut app = app_with(1);
        let _ = app.update(Message::PromptRequested(Box::new(new_branch_prompt(false))));
        let _ = app.update(Message::PromptChanged(0, "  feat/spaced  ".to_owned()));

        match action_of(&app) {
            Some(RepoMessage::BranchCreateRequested { name, from }) => {
                assert_eq!(name, "feat/spaced");
                assert_eq!(from, hidegit_core::ops::StartPoint::Head);
            }
            other => panic!("expected a create, got {other:?}"),
        }
    }

    #[test]
    fn a_name_that_is_only_whitespace_is_not_a_name() {
        let mut app = app_with(1);
        let _ = app.update(Message::PromptRequested(Box::new(new_branch_prompt(true))));
        let _ = app.update(Message::PromptChanged(0, "   ".to_owned()));

        assert!(!app.app.prompt.as_ref().unwrap().is_ready());
        assert!(action_of(&app).is_none());
    }

    #[test]
    fn renaming_a_branch_to_the_name_it_already_has_is_nothing_to_do() {
        let mut app = app_with(1);
        let _ = app.update(Message::PromptRequested(Box::new(rename_prompt("main"))));

        // The field opens prefilled, so accepting without typing means "no
        // change" — not a failure to report, just nothing.
        assert!(
            app.app.prompt.as_ref().unwrap().is_ready(),
            "prefilled, so the button is available"
        );
        assert!(action_of(&app).is_none());
    }

    #[test]
    fn a_new_branch_that_is_checked_out_is_one_command_not_two() {
        // `git switch --create` is atomic. Creating and then switching would show
        // an intermediate state and could half-fail.
        let mut app = app_with(1);
        let _ = app.update(Message::PromptRequested(Box::new(new_branch_prompt(true))));
        let _ = app.update(Message::PromptChanged(0, "feat/graph".to_owned()));

        match action_of(&app) {
            Some(RepoMessage::CheckoutRequested(CheckoutTarget::NewBranch { name, from })) => {
                assert_eq!(name, "feat/graph");
                assert_eq!(from, hidegit_core::ops::StartPoint::Head);
            }
            other => panic!("expected one atomic checkout, got {other:?}"),
        }
    }

    // ---- branches ----

    #[test]
    fn deleting_a_branch_asks_before_it_acts_and_never_forces() {
        let mut app = app_with(1);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::BranchDeleteRequested {
                name: "feat/graph".to_owned(),
            },
        ));

        let confirmation = app
            .app
            .confirming
            .as_ref()
            .expect("deleting a branch confirms");
        assert!(
            confirmation.title.contains("feat/graph"),
            "names the branch"
        );
        assert_eq!(confirmation.confirm_label, "Delete");

        match confirmation.action.as_ref() {
            Message::Repo(0, RepoMessage::BranchDeleteConfirmed { name, force }) => {
                assert_eq!(name, "feat/graph");
                // The safe form runs first; Git's own refusal is what offers the
                // choice to force.
                assert!(!force, "the confirmation must never pre-select --force");
            }
            other => panic!("expected a guarded delete, got {other:?}"),
        }
    }

    #[test]
    fn a_repository_mid_operation_does_not_switch_branches() {
        // A rebase owns HEAD until it is finished or aborted, and the rule the
        // view reads to disable a control is the same one `update` reads before
        // acting on one.
        let mut app = app_with(1);
        assert!(app.app.repos[0].can_switch_branches());

        for state in [
            RepoState::Rebasing,
            RepoState::Merging,
            RepoState::CherryPicking,
        ] {
            app.app.repos[0].state = state;
            assert!(
                !app.app.repos[0].can_switch_branches(),
                "{state:?} owns HEAD"
            );
        }
    }

    #[test]
    fn a_failed_ahead_behind_count_costs_an_indicator_not_a_dialog() {
        let mut app = app_with(1);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::DivergenceLoaded(Box::new(Err(UiError {
                summary: "counting failed".to_owned(),
                details: String::new(),
            }))),
        ));

        assert!(
            app.app.toasts.is_empty(),
            "a dialog about an arrow would be worse than no arrow"
        );
        assert!(app.app.repos[0].divergence.is_empty());
    }

    #[test]
    fn ahead_and_behind_arrive_and_are_kept_per_branch() {
        let mut app = app_with(1);
        let mut counts = HashMap::new();
        counts.insert(
            "refs/heads/main".to_owned(),
            hidegit_core::model::Divergence {
                ahead: 2,
                behind: 1,
            },
        );

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::DivergenceLoaded(Box::new(Ok(counts))),
        ));

        let repo = &app.app.repos[0];
        assert_eq!(
            repo.divergence_of("refs/heads/main"),
            Some(hidegit_core::model::Divergence {
                ahead: 2,
                behind: 1
            })
        );
        assert_eq!(
            repo.divergence_of("refs/heads/never-pushed"),
            None,
            "absent is not the same as level with a remote"
        );
    }

    // ---- network operations ----

    /// Puts an operation on screen without running one.
    fn pending(app: &mut Hidegit, id: u64) -> CancelToken {
        let cancel = CancelToken::new();
        app.app.repos[0].pending = Some(Operation {
            id,
            label: "Fetching".to_owned(),
            cancel: cancel.clone(),
            progress: None,
        });
        cancel
    }

    #[test]
    fn only_one_network_operation_runs_at_a_time() {
        // The toolbar hides its buttons while one is in flight, so the only way
        // here is a shortcut — and a second fetch racing the first for the same
        // refs is not worth supporting.
        let mut app = app_with(1);
        let cancel = pending(&mut app, 7);

        let _ = app.update(Message::Repo(0, RepoMessage::FetchRequested));

        let still = app.app.repos[0].pending.as_ref().expect("still running");
        assert_eq!(still.id, 7, "the first operation was not replaced");
        assert!(!cancel.is_cancelled(), "nor cancelled behind its back");
    }

    #[test]
    fn cancelling_asks_rather_than_clearing_the_banner_itself() {
        // The worker notices, kills the subprocess and reports back like any other
        // ending, so the banner clears in exactly one place. Clearing it here would
        // say the operation had stopped before it had.
        let mut app = app_with(1);
        let cancel = pending(&mut app, 1);

        let _ = app.update(Message::Repo(0, RepoMessage::OperationCancelled));

        assert!(cancel.is_cancelled());
        assert!(
            app.app.repos[0].pending.is_some(),
            "the banner stays up until the worker confirms"
        );
    }

    #[test]
    fn a_late_message_from_a_replaced_operation_is_ignored() {
        // The bug the id exists to prevent: cancel a fetch, start a push, and the
        // fetch's last report or its ending must not touch the push's banner.
        let mut app = app_with(1);
        let _ = pending(&mut app, 1);
        app.app.repos[0].pending.as_mut().unwrap().id = 2;

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationProgress(
                1,
                ProgressUpdate {
                    phase: "Receiving objects".to_owned(),
                    done: 5,
                    total: Some(10),
                },
            ),
        ));
        assert!(
            app.app.repos[0]
                .pending
                .as_ref()
                .unwrap()
                .progress
                .is_none(),
            "the stale report did not redraw the current banner"
        );

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationFinished(
                1,
                Box::new(Ok(OperationOutcome::Fetched(Default::default()))),
            ),
        ));
        assert!(
            app.app.repos[0].pending.is_some(),
            "and the stale ending did not clear it"
        );
    }

    #[test]
    fn progress_reaches_the_banner_and_reads_in_a_real_unit() {
        let mut app = app_with(1);
        let _ = pending(&mut app, 3);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationProgress(
                3,
                ProgressUpdate {
                    phase: "Receiving objects".to_owned(),
                    done: 42,
                    total: Some(100),
                },
            ),
        ));

        let banner = app.app.repos[0].pending.as_ref().expect("still running");
        assert_eq!(banner.detail(), "Receiving objects 42/100");
        assert_eq!(banner.fraction(), Some(0.42));
    }

    #[test]
    fn a_banner_with_no_report_yet_says_so_rather_than_inventing_a_number() {
        let mut app = app_with(1);
        let _ = pending(&mut app, 1);

        let banner = app.app.repos[0].pending.as_ref().unwrap();
        assert_eq!(banner.detail(), "starting…");
        assert_eq!(
            banner.fraction(),
            None,
            "no total means no bar — an indeterminate one is what UI_SPEC rules out"
        );
    }

    #[test]
    fn a_finished_operation_clears_the_banner_and_says_nothing_on_success() {
        let mut app = app_with(1);
        let _ = pending(&mut app, 5);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationFinished(
                5,
                Box::new(Ok(OperationOutcome::Fetched(Default::default()))),
            ),
        ));

        assert!(app.app.repos[0].pending.is_none());
        assert!(
            app.app.toasts.is_empty(),
            "the refresh that follows is the result"
        );
    }

    #[test]
    fn a_partly_refused_push_is_reported_because_a_silent_one_would_be_a_lie() {
        let mut app = app_with(1);
        let _ = pending(&mut app, 5);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationFinished(
                5,
                Box::new(Ok(OperationOutcome::Pushed(
                    hidegit_core::ops::PushOutcome {
                        updated: vec!["main".to_owned()],
                        rejected: vec!["feat/graph".to_owned()],
                    },
                ))),
            ),
        ));

        let toast = app.app.toasts.first().expect("a refusal is reported");
        assert!(
            toast.summary.contains("feat/graph"),
            "it names what was refused: {}",
            toast.summary
        );
    }

    #[test]
    fn a_cancelled_operation_is_silent_because_it_is_what_was_asked_for() {
        let mut app = app_with(1);
        let _ = pending(&mut app, 5);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationFinished(
                5,
                Box::new(Err(UiError::from(GitError::Cancelled { stale_lock: None }))),
            ),
        ));

        assert!(app.app.repos[0].pending.is_none());
        assert!(app.app.toasts.is_empty(), "no dialog about a Cancel click");
    }

    #[test]
    fn a_cancellation_that_left_a_lock_behind_says_so() {
        // hideGit will not delete it, so the user has to be told it is there.
        let mut app = app_with(1);
        let _ = pending(&mut app, 5);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationFinished(
                5,
                Box::new(Err(UiError::from(GitError::Cancelled {
                    stale_lock: Some(PathBuf::from("/repo/.git/index.lock")),
                }))),
            ),
        ));

        let toast = app.app.toasts.first().expect("a stale lock is reported");
        assert!(
            toast.details.contains("index.lock"),
            "it names the file: {}",
            toast.details
        );
    }

    #[test]
    fn a_failed_operation_toasts_gits_own_words() {
        let mut app = app_with(1);
        let _ = pending(&mut app, 5);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationFinished(
                5,
                Box::new(Err(UiError::from(GitError::Command {
                    argv: vec!["push".to_owned(), "origin".to_owned()],
                    status: Some(1),
                    stderr: "hint: Updates were rejected because the remote contains work\n"
                        .to_owned(),
                }))),
            ),
        ));

        let toast = app.app.toasts.first().expect("a failure is reported");
        assert!(
            toast.details.contains("Updates were rejected"),
            "verbatim, not paraphrased: {}",
            toast.details
        );
    }

    #[test]
    fn a_force_push_asks_first_and_names_the_branch_and_the_remote() {
        let mut app = app_with(1);
        // A branch with an upstream, so there is somewhere to push.
        app.app.repos[0].refs = tracking_refs();
        app.app.repos[0].head = head_on("main");

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PushRequested {
                force: ForceMode::WithLease,
            },
        ));

        let confirmation = app
            .app
            .confirming
            .as_ref()
            .expect("forcing a push confirms");
        assert!(
            confirmation.title.contains("main"),
            "{}",
            confirmation.title
        );
        assert!(
            confirmation.title.contains("origin"),
            "{}",
            confirmation.title
        );
        assert!(
            confirmation.body.contains("refuses if the remote moved"),
            "a lease says what protects you: {}",
            confirmation.body
        );
    }

    #[test]
    fn a_plain_push_does_not_ask() {
        let mut app = app_with(1);
        app.app.repos[0].refs = tracking_refs();
        app.app.repos[0].head = head_on("main");

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PushRequested {
                force: ForceMode::None,
            },
        ));

        assert!(app.app.confirming.is_none());
    }

    #[test]
    fn a_bare_force_warns_that_someone_elses_commits_would_be_lost() {
        let mut app = app_with(1);
        app.app.repos[0].refs = tracking_refs();
        app.app.repos[0].head = head_on("main");

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PushRequested {
                force: ForceMode::Force,
            },
        ));

        let body = &app.app.confirming.as_ref().expect("it confirms").body;
        assert!(
            body.contains("unreachable"),
            "the difference from a lease has to be stated: {body}"
        );
    }

    /// A repository with `main` tracking `origin/main`.
    fn tracking_refs() -> Refs {
        Refs {
            locals: vec![hidegit_core::model::Branch {
                name: RefName {
                    kind: RefKind::LocalBranch,
                    full: "refs/heads/main".into(),
                    short: "main".into(),
                },
                target: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
                upstream: Some("refs/remotes/origin/main".into()),
            }],
            remotes: vec![hidegit_core::model::Branch {
                name: RefName {
                    kind: RefKind::RemoteBranch,
                    full: "refs/remotes/origin/main".into(),
                    short: "origin/main".into(),
                },
                target: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
                upstream: None,
            }],
            tags: Vec::new(),
        }
    }

    fn head_on(branch: &str) -> Head {
        Head::Branch {
            name: RefName {
                kind: RefKind::LocalBranch,
                full: format!("refs/heads/{branch}"),
                short: branch.to_owned(),
            },
            target: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
        }
    }

    #[test]
    fn a_push_knows_where_to_go_from_the_branchs_own_upstream() {
        let mut app = app_with(1);
        app.app.repos[0].refs = tracking_refs();
        app.app.repos[0].head = head_on("main");

        let target = app.app.repos[0]
            .push_target()
            .expect("a tracking branch has somewhere to go");

        assert_eq!(target.remote, "origin");
        assert_eq!(target.refspec, "refs/heads/main:refs/heads/main");
        assert!(
            !target.set_upstream,
            "it already has one; recording it again would be noise"
        );
    }

    #[test]
    fn a_renamed_branch_pushes_to_the_branch_it_tracks_not_to_its_own_name() {
        // Found by eye. Renaming leaves the upstream pointing at the old name,
        // which is the ordinary state — and pushing to the local name instead
        // quietly created a *second* branch on the remote rather than updating the
        // one being tracked.
        let mut app = app_with(1);
        let mut refs = tracking_refs();
        refs.locals[0].name = RefName {
            kind: RefKind::LocalBranch,
            full: "refs/heads/main-renamed".into(),
            short: "main-renamed".into(),
        };
        app.app.repos[0].refs = refs;
        app.app.repos[0].head = head_on("main-renamed");

        let target = app.app.repos[0]
            .push_target()
            .expect("it tracks origin/main");

        assert_eq!(target.remote, "origin");
        assert_eq!(
            target.refspec, "refs/heads/main-renamed:refs/heads/main",
            "the destination is the tracked branch, not the local name"
        );
        assert!(!target.set_upstream);
    }

    #[test]
    fn an_upstream_branch_name_containing_slashes_survives_intact() {
        // `origin/release/1.x` is remote `origin` and branch `release/1.x`, not
        // remote `origin` and branch `release`.
        let mut app = app_with(1);
        let mut refs = tracking_refs();
        refs.locals[0].upstream = Some("refs/remotes/origin/release/1.x".into());
        app.app.repos[0].refs = refs;
        app.app.repos[0].head = head_on("main");

        let target = app.app.repos[0].push_target().expect("it tracks something");
        assert_eq!(target.refspec, "refs/heads/main:refs/heads/release/1.x");
    }

    #[test]
    fn a_branch_with_no_upstream_pushes_to_the_default_remote_and_records_it() {
        let mut app = app_with(1);
        app.app.repos[0].refs = tracking_refs();
        app.app.repos[0].head = head_on("feat/graph");

        let target = app.app.repos[0].push_target().expect("origin exists");

        assert_eq!(target.remote, "origin");
        assert_eq!(
            target.refspec,
            "refs/heads/feat/graph:refs/heads/feat/graph"
        );
        assert!(
            target.set_upstream,
            "so the sidebar has ahead/behind from the first push"
        );
    }

    #[test]
    fn a_detached_head_has_nothing_to_push_from() {
        let mut app = app_with(1);
        app.app.repos[0].refs = tracking_refs();
        app.app.repos[0].head = Head::Detached {
            target: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
        };

        assert!(app.app.repos[0].push_target().is_none());
    }

    #[test]
    fn a_repository_with_no_remote_has_nowhere_to_push() {
        let app = app_with(1);
        assert_eq!(app.app.repos[0].default_remote(), None);
        assert!(app.app.repos[0].push_target().is_none());
    }

    #[test]
    fn origin_wins_over_other_remotes_because_that_is_what_it_means() {
        let mut app = app_with(1);
        let mut refs = tracking_refs();
        refs.remotes.push(hidegit_core::model::Branch {
            name: RefName {
                kind: RefKind::RemoteBranch,
                full: "refs/remotes/upstream/main".into(),
                short: "upstream/main".into(),
            },
            target: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
            upstream: None,
        });
        app.app.repos[0].refs = refs;

        assert_eq!(app.app.repos[0].default_remote().as_deref(), Some("origin"));
    }

    #[test]
    fn the_only_remote_is_used_even_when_it_is_not_called_origin() {
        let mut app = app_with(1);
        app.app.repos[0].refs = Refs {
            remotes: vec![hidegit_core::model::Branch {
                name: RefName {
                    kind: RefKind::RemoteBranch,
                    full: "refs/remotes/fork/main".into(),
                    short: "fork/main".into(),
                },
                target: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
                upstream: None,
            }],
            ..Refs::default()
        };

        assert_eq!(app.app.repos[0].default_remote().as_deref(), Some("fork"));
    }

    #[test]
    fn the_remote_shortcuts_are_all_modified_so_they_work_while_typing() {
        // `Cmd+Shift+U` is wanted precisely while a commit message is being
        // written, and the `editing` guard only lets modified keys through.
        let mods = keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT;

        for (character, expected) in [("f", "Fetch"), ("p", "Pull"), ("u", "Push")] {
            let key = keyboard::Key::Character(character.into());
            let message = shortcut(
                &key,
                mods,
                KeyContext {
                    active: Some(0),
                    editing: true,
                    ..KeyContext::default()
                },
            );
            let described = format!("{message:?}");
            assert!(
                described.contains(expected),
                "{character} while editing should still {expected}, got {described}"
            );
        }
    }

    #[test]
    fn the_remote_shortcuts_survive_a_layout_that_reports_a_capital() {
        // With Shift held, iced reports whichever character the layout produces.
        let mods = keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT;
        let key = keyboard::Key::Character("U".into());

        let described = format!(
            "{:?}",
            shortcut(
                &key,
                mods,
                KeyContext {
                    active: Some(0),
                    ..KeyContext::default()
                }
            )
        );
        assert!(described.contains("Push"), "got {described}");
    }

    // ---- the stash ----

    fn stash_entry(index: usize, message: &str) -> hidegit_core::model::StashEntry {
        hidegit_core::model::StashEntry {
            index,
            id: ObjectId::from_hex(&format!("{index:040x}")).unwrap(),
            message: message.to_owned(),
            time: time::OffsetDateTime::UNIX_EPOCH,
            branch: Some("main".to_owned()),
        }
    }

    #[test]
    fn dropping_a_stash_asks_first_and_names_what_is_lost() {
        let mut app = app_with(1);
        app.app.repos[0].stashes = vec![stash_entry(0, "half-finished lane colours")];

        let _ = app.update(Message::Repo(0, RepoMessage::StashDropRequested(0)));

        let confirmation = app.app.confirming.as_ref().expect("dropping confirms");
        assert!(
            confirmation.body.contains("half-finished lane colours"),
            "it names the stash rather than asking generically: {}",
            confirmation.body
        );
        assert!(confirmation.body.contains("cannot be undone"));
        assert_eq!(confirmation.confirm_label, "Drop");
    }

    #[test]
    fn dropping_the_open_stash_closes_the_pane_first() {
        // Dropping shifts every later entry down by one, so `Stash(at)` would then
        // be showing a *different* stash's diff under the same heading.
        let mut app = app_with(1);
        app.app.repos[0].stashes = vec![stash_entry(0, "first"), stash_entry(1, "second")];
        app.app.repos[0].selection = Some(Selection::Stash(0));

        let _ = app.update(Message::Repo(0, RepoMessage::StashDropConfirmed(0)));

        assert_eq!(app.app.repos[0].selection, None);
        assert!(matches!(app.app.repos[0].detail, DetailPane::Empty));
    }

    #[test]
    fn dropping_a_stash_that_is_not_open_leaves_the_selection_alone() {
        let mut app = app_with(1);
        app.app.repos[0].stashes = vec![stash_entry(0, "first"), stash_entry(1, "second")];
        app.app.repos[0].selection = Some(Selection::WorkingDirectory);

        let _ = app.update(Message::Repo(0, RepoMessage::StashDropConfirmed(1)));

        assert_eq!(
            app.app.repos[0].selection,
            Some(Selection::WorkingDirectory)
        );
    }

    #[test]
    fn selecting_a_stash_that_is_no_longer_there_does_nothing() {
        // The list can shrink under the selection — a `git stash pop` in a
        // terminal, picked up by the watcher — and an index past the end must not
        // panic or load someone else's diff.
        let mut app = app_with(1);
        app.app.repos[0].stashes = vec![stash_entry(0, "only one")];

        let _ = app.update(Message::Repo(0, RepoMessage::Selected(Selection::Stash(5))));

        assert!(!matches!(app.app.repos[0].detail, DetailPane::Loading));
    }

    #[test]
    fn a_stashs_diff_is_accepted_when_it_arrives() {
        // Found by eye: the pane sat on "Loading…" for ever, because the staleness
        // guard only recognised `Selection::Commit` and a stash arrives through the
        // same path — it *is* a commit.
        let mut app = app_with(1);
        let entry = stash_entry(0, "half-finished lane colours");
        let id = entry.id;
        app.app.repos[0].stashes = vec![entry];
        app.app.repos[0].selection = Some(Selection::Stash(0));

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::DetailLoaded(Box::new(Ok(CommitLoad {
                id,
                detail: hidegit_core::model::CommitDetail {
                    commit: commits(1).remove(0),
                    changes: Vec::new(),
                    stats: Default::default(),
                },
                diff: Diff::default(),
            }))),
        ));

        assert!(
            matches!(app.app.repos[0].detail, DetailPane::Commit { .. }),
            "the diff has to land, not be discarded as stale"
        );
    }

    #[test]
    fn a_diff_for_a_stash_the_user_has_moved_on_from_is_still_discarded() {
        let mut app = app_with(1);
        app.app.repos[0].stashes = vec![stash_entry(0, "one"), stash_entry(1, "two")];
        app.app.repos[0].selection = Some(Selection::Stash(1));

        // The id belongs to entry 0, but entry 1 is open.
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::DetailLoaded(Box::new(Ok(CommitLoad {
                id: stash_entry(0, "one").id,
                detail: hidegit_core::model::CommitDetail {
                    commit: commits(1).remove(0),
                    changes: Vec::new(),
                    stats: Default::default(),
                },
                diff: Diff::default(),
            }))),
        ));

        assert!(!matches!(
            app.app.repos[0].detail,
            DetailPane::Commit { .. }
        ));
    }

    #[test]
    fn a_stash_refresh_replaces_the_whole_list() {
        let mut app = app_with(1);
        app.app.repos[0].stashes = vec![stash_entry(0, "stale")];

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::Refreshed(Box::new(Ok(Refreshed {
                head: opened(1).head,
                refs: Refs::default(),
                state: RepoState::Clean,
                status: WorktreeStatus::default(),
                stashes: vec![stash_entry(0, "fresh")],
                remotes: Vec::new(),
                total: 1,
                first_page: commits(1),
            }))),
        ));

        let stashes = &app.app.repos[0].stashes;
        assert_eq!(stashes.len(), 1);
        assert_eq!(stashes[0].message, "fresh");
    }

    // ---- remotes, tags and clone ----

    fn remote(name: &str, url: &str) -> hidegit_core::model::Remote {
        hidegit_core::model::Remote {
            name: name.to_owned(),
            fetch_url: url.to_owned(),
            push_url: None,
        }
    }

    fn remote_branch(short: &str) -> hidegit_core::model::Branch {
        hidegit_core::model::Branch {
            name: RefName {
                kind: RefKind::RemoteBranch,
                full: format!("refs/remotes/{short}"),
                short: short.to_owned(),
            },
            target: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
            upstream: None,
        }
    }

    #[test]
    fn a_remote_that_has_never_been_fetched_still_appears_in_the_tree() {
        // Grouping only by tracking-ref name would hide it, and a remote you cannot
        // see is a remote you cannot fetch from or remove.
        let mut app = app_with(1);
        app.app.repos[0].remotes = vec![remote("origin", "https://example.invalid/repo.git")];

        let grouped = app.app.repos[0].remotes_with_branches();

        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].0.name, "origin");
        assert!(grouped[0].1.is_empty());
    }

    #[test]
    fn branches_group_under_the_remote_they_belong_to() {
        let mut app = app_with(1);
        app.app.repos[0].remotes = vec![
            remote("fork", "https://example.invalid/fork.git"),
            remote("origin", "https://example.invalid/repo.git"),
        ];
        app.app.repos[0].refs = Refs {
            remotes: vec![
                remote_branch("origin/main"),
                remote_branch("fork/main"),
                remote_branch("origin/feat/graph"),
            ],
            ..Refs::default()
        };

        let grouped = app.app.repos[0].remotes_with_branches();
        let names = |at: usize| -> Vec<&str> {
            grouped[at]
                .1
                .iter()
                .map(|b| b.name.short.as_str())
                .collect()
        };

        assert_eq!(grouped[0].0.name, "fork");
        assert_eq!(names(0), vec!["fork/main"]);
        assert_eq!(grouped[1].0.name, "origin");
        assert_eq!(names(1), vec!["origin/main", "origin/feat/graph"]);
    }

    #[test]
    fn a_remote_does_not_collect_another_whose_name_starts_the_same() {
        // `origin` must not swallow `origin-mirror/main`, which is why the match is
        // on the whole first segment.
        let mut app = app_with(1);
        app.app.repos[0].remotes = vec![
            remote("origin", "https://example.invalid/a.git"),
            remote("origin-mirror", "https://example.invalid/b.git"),
        ];
        app.app.repos[0].refs = Refs {
            remotes: vec![
                remote_branch("origin/main"),
                remote_branch("origin-mirror/main"),
            ],
            ..Refs::default()
        };

        let grouped = app.app.repos[0].remotes_with_branches();
        assert_eq!(grouped[0].1.len(), 1, "origin has exactly its own");
        assert_eq!(grouped[0].1[0].name.short, "origin/main");
        assert_eq!(grouped[1].1[0].name.short, "origin-mirror/main");
    }

    #[test]
    fn removing_a_remote_asks_first_and_says_what_it_touches_and_what_it_does_not() {
        let mut app = app_with(1);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::RemoteRemoveRequested("origin".to_owned()),
        ));

        let confirmation = app.app.confirming.as_ref().expect("removing confirms");
        assert!(confirmation.title.contains("origin"));
        assert!(
            confirmation.body.contains("Nothing on the remote itself"),
            "the scope has to be stated: {}",
            confirmation.body
        );
    }

    #[test]
    fn deleting_a_tag_says_the_remote_keeps_it_until_that_is_pushed() {
        let mut app = app_with(1);
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::TagDeleteRequested("v0.1.0".to_owned()),
        ));

        let confirmation = app.app.confirming.as_ref().expect("deleting confirms");
        assert!(
            confirmation.body.contains("keeps it until"),
            "{}",
            confirmation.body
        );
    }

    #[test]
    fn a_clone_lands_in_a_folder_named_after_the_repository() {
        // The same name `git clone` itself picks, so a clone into the folder someone
        // chose does not scatter a repository across it.
        for (url, expected) in [
            ("https://github.com/owner/repo.git", "repo"),
            ("https://github.com/owner/repo", "repo"),
            ("https://github.com/owner/repo/", "repo"),
            ("git@github.com:owner/repo.git", "repo"),
            ("ssh://git@example.com:22/owner/repo.git", "repo"),
            ("/srv/git/local-repo.git", "local-repo"),
            ("../sibling", "sibling"),
        ] {
            assert_eq!(repository_name(url), expected, "for {url}");
        }
    }

    #[test]
    fn a_url_with_nothing_usable_in_it_still_gets_a_folder_name() {
        // Better than cloning into a directory called "".
        assert_eq!(repository_name(""), "repository");
        assert_eq!(repository_name("/"), "repository");
    }

    #[test]
    fn a_clone_prompt_produces_a_clone_even_with_no_repository_open() {
        // Cloning is the one action that has to work from the welcome screen, so it
        // must not be routed through the active repository.
        let mut app = Hidegit::default();
        assert!(app.app.active.is_none());

        let _ = app.update(Message::PromptRequested(Box::new(Prompt {
            kind: PromptKind::Clone,
            title: "Clone a repository".to_owned(),
            confirm_label: "Choose a folder…".to_owned(),
            fields: vec![crate::state::PromptField::new("URL", "…")],
        })));
        let _ = app.update(Message::PromptChanged(
            0,
            "https://example.invalid/repo.git".to_owned(),
        ));

        let prompt = app.app.prompt.clone().expect("a prompt is up");
        match app.prompt_action(&prompt) {
            Some(Message::CloneRequested(url)) => {
                assert_eq!(url, "https://example.invalid/repo.git");
            }
            other => panic!("expected a clone, got {other:?}"),
        }
    }

    #[test]
    fn a_stash_prompt_may_be_accepted_with_nothing_typed() {
        // Git writes its own `WIP on …`, so requiring a message would be hideGit
        // inventing a rule Git does not have.
        let mut app = app_with(1);
        let _ = app.update(Message::PromptRequested(Box::new(Prompt {
            kind: PromptKind::StashPush {
                include_untracked: true,
            },
            title: "Stash changes".to_owned(),
            confirm_label: "Stash".to_owned(),
            fields: vec![crate::state::PromptField::new("Message (optional)", "…")],
        })));

        let prompt = app.app.prompt.clone().expect("a prompt is up");
        assert!(
            prompt.is_ready(),
            "the button is available with nothing typed"
        );

        match app.prompt_action(&prompt) {
            Some(Message::Repo(
                0,
                RepoMessage::StashRequested(StashOp::Push {
                    message,
                    include_untracked,
                }),
            )) => {
                assert_eq!(message, None, "no message means Git writes its own");
                assert!(include_untracked);
            }
            other => panic!("expected a stash, got {other:?}"),
        }
    }

    #[test]
    fn every_other_prompt_needs_all_of_its_fields() {
        let mut app = app_with(1);
        let _ = app.update(Message::PromptRequested(Box::new(Prompt {
            kind: PromptKind::AddRemote,
            title: "Add a remote".to_owned(),
            confirm_label: "Add".to_owned(),
            fields: vec![
                crate::state::PromptField::new("Name", "origin"),
                crate::state::PromptField::new("URL", "…"),
            ],
        })));
        let _ = app.update(Message::PromptChanged(0, "origin".to_owned()));

        let prompt = app.app.prompt.clone().expect("a prompt is up");
        assert!(
            !prompt.is_ready(),
            "a remote with no URL is not a remote to add"
        );

        let _ = app.update(Message::PromptChanged(
            1,
            "https://example.invalid/repo.git".to_owned(),
        ));
        let prompt = app.app.prompt.clone().unwrap();
        assert!(prompt.is_ready());

        match app.prompt_action(&prompt) {
            Some(Message::Repo(0, RepoMessage::RemoteAddRequested { name, url })) => {
                assert_eq!(name, "origin");
                assert_eq!(url, "https://example.invalid/repo.git");
            }
            other => panic!("expected an add, got {other:?}"),
        }
    }

    #[test]
    fn a_lightweight_tag_prompt_sends_no_message() {
        let mut app = app_with(1);
        let _ = app.update(Message::PromptRequested(Box::new(Prompt {
            kind: PromptKind::NewTag {
                at: hidegit_core::ops::StartPoint::Head,
                annotated: false,
            },
            title: "New tag".to_owned(),
            confirm_label: "Create".to_owned(),
            fields: vec![crate::state::PromptField::new("Name", "v1.0.0")],
        })));
        let _ = app.update(Message::PromptChanged(0, "v1.0.0".to_owned()));

        let prompt = app.app.prompt.clone().unwrap();
        match app.prompt_action(&prompt) {
            Some(Message::Repo(0, RepoMessage::TagCreateRequested { name, message, .. })) => {
                assert_eq!(name, "v1.0.0");
                assert_eq!(message, None, "lightweight means no object and no message");
            }
            other => panic!("expected a tag, got {other:?}"),
        }
    }

    #[test]
    fn cmd_shift_o_clones_rather_than_opening() {
        // Checked before the unshifted `o`, or the shortcut would open a picker.
        let mods = keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT;
        let described = format!(
            "{:?}",
            shortcut(
                &keyboard::Key::Character("o".into()),
                mods,
                KeyContext::default()
            )
        );
        assert!(described.contains("Clone"), "got {described}");

        let plain = format!(
            "{:?}",
            shortcut(
                &keyboard::Key::Character("o".into()),
                keyboard::Modifiers::COMMAND,
                KeyContext::default()
            )
        );
        assert!(plain.contains("OpenDialogRequested"), "got {plain}");
    }

    #[test]
    fn a_modal_dialog_owns_the_keyboard() {
        // Staging something behind an unanswered "discard?" would be the worst
        // possible moment to act on a stray key.
        let escape = keyboard::Key::Named(keyboard::key::Named::Escape);
        let enter = keyboard::Key::Named(keyboard::key::Named::Enter);
        let space = keyboard::Key::Named(keyboard::key::Named::Space);

        assert!(matches!(
            modal_shortcut(&escape, Modal::Confirmation),
            Message::ConfirmationDismissed
        ));
        assert!(matches!(
            modal_shortcut(&enter, Modal::Confirmation),
            Message::ConfirmationAccepted
        ));
        assert!(matches!(
            modal_shortcut(&space, Modal::Confirmation),
            Message::ToastDismissed(_)
        ));
    }

    #[test]
    fn each_modal_layer_answers_escape_and_enter_for_itself() {
        let escape = keyboard::Key::Named(keyboard::key::Named::Escape);
        let enter = keyboard::Key::Named(keyboard::key::Named::Enter);

        assert!(matches!(
            modal_shortcut(&escape, Modal::Prompt),
            Message::PromptDismissed
        ));
        assert!(matches!(
            modal_shortcut(&enter, Modal::Prompt),
            Message::PromptAccepted
        ));
        assert!(matches!(
            modal_shortcut(&escape, Modal::Sheet),
            Message::SheetDismissed
        ));
        // A sheet still has no default action, but the guarantee moved: `Enter`
        // now asks the sheet what is highlighted, and a freshly opened sheet
        // highlights nothing. `a_sheet_opens_with_nothing_chosen_and_enter_does_nothing`
        // is where that is asserted now — it has to be, because binding `Enter`
        // to nothing at all is what made sheets mouse-only.
        assert!(matches!(
            modal_shortcut(&enter, Modal::Sheet),
            Message::SheetAccepted
        ));
    }

    #[test]
    fn the_topmost_modal_is_the_one_that_owns_the_keyboard() {
        // A destructive sheet item raises a confirmation *over* the sheet, so
        // `Esc` has to back out of the question rather than the list behind it.
        let mut app = app_with(1);
        app.app.sheet = Some(crate::state::ActionSheet::new("feat/graph"));
        assert_eq!(app.modal_keys(), Some(Modal::Sheet));

        app.app.confirming = Some(Confirmation {
            title: "Delete feat/graph?".to_owned(),
            body: String::new(),
            confirm_label: "Delete".to_owned(),
            action: Box::new(Message::ToastDismissed(0)),
        });
        assert_eq!(app.modal_keys(), Some(Modal::Confirmation));
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
            shortcut(&key, mods, KeyContext::default()),
            Message::OpenDialogRequested
        ));
    }

    #[test]
    fn arrow_keys_do_nothing_when_no_repository_is_open() {
        let key = keyboard::Key::Named(keyboard::key::Named::ArrowDown);

        assert!(matches!(
            shortcut(&key, keyboard::Modifiers::default(), KeyContext::default()),
            Message::ToastDismissed(_)
        ));
    }

    // ---- pull requests ---------------------------------------------------

    fn github_repo() -> hidegit_forge::RepoRef {
        hidegit_forge::RepoRef {
            host: "github.com".into(),
            owner: "youhide".into(),
            name: "hideGit".into(),
        }
    }

    fn pull_request(number: u64) -> hidegit_forge::PullRequest {
        hidegit_forge::PullRequest {
            number,
            title: "feat: something".into(),
            url: format!("https://github.com/youhide/hideGit/pull/{number}"),
            author: "youhide".into(),
            head: "feat/x".into(),
            base: "main".into(),
            draft: false,
            updated: OffsetDateTime::UNIX_EPOCH,
            roles: [hidegit_forge::PrRole::Author].into_iter().collect(),
            review: hidegit_forge::ReviewState::Required,
            checks: hidegit_forge::CheckState::Passing,
            merge: hidegit_forge::MergeState::Mergeable,
            comments: 0,
        }
    }

    /// An open repository whose remotes name a GitHub repository.
    fn app_with_forge_repo() -> Hidegit {
        let mut app = app_with(3);
        app.app.repos[0].prs.repo = Some(github_repo());
        // Notifications go to a recorder rather than to a notification daemon,
        // which no CI runner has.
        app.app.notifier = Arc::new(hidegit_forge::Recorder::default());
        app
    }

    fn full_budget() -> hidegit_forge::RateBudget {
        hidegit_forge::RateBudget {
            limit: 5_000,
            remaining: 5_000,
            reset: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn loaded(items: Vec<hidegit_forge::PullRequest>) -> PrsLoad {
        PrsLoad::Loaded {
            items,
            budget: full_budget(),
        }
    }

    #[test]
    fn a_repository_with_no_forge_remote_never_enters_a_pull_request_state() {
        // Every remote in hideGit's own suite is a path on disk, so this is the
        // common case. Such a repository has no pull requests to have, which is
        // not the same as having none.
        let app = app_with(3);

        assert_eq!(app.app.repos[0].prs.repo, None);
        assert_eq!(app.app.repos[0].prs.state, PrState::Idle);
    }

    #[test]
    fn a_failed_poll_goes_stale_rather_than_empty() {
        // What was last known stays on screen. A network blip must not look
        // like every pull request having been closed.
        let mut app = app_with_forge_repo();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![pull_request(47)])))),
        ));
        assert_eq!(app.app.repos[0].prs.items.len(), 1);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Err(UiError {
                summary: "could not reach github.com".into(),
                details: String::new(),
            }))),
        ));

        assert_eq!(
            app.app.repos[0].prs.items.len(),
            1,
            "the previous result survives a failed poll"
        );
        assert!(matches!(app.app.repos[0].prs.state, PrState::Stale(_)));
        assert!(
            app.app.toasts.is_empty(),
            "a failed poll updates an indicator; it never raises a dialog"
        );
    }

    #[test]
    fn not_installed_is_a_state_with_an_action_rather_than_an_empty_list() {
        let mut app = app_with_forge_repo();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(PrsLoad::NotInstalled {
                install_url: "https://github.com/apps/hidegit/installations/new".into(),
            }))),
        ));

        match &app.app.repos[0].prs.state {
            PrState::NotInstalled { install_url } => {
                assert!(install_url.contains("installations/new"));
            }
            other => panic!("expected NotInstalled, got {other:?}"),
        }
        assert!(app.app.toasts.is_empty(), "it is not a failure to report");
    }

    #[test]
    fn signing_out_clears_every_repositorys_panel_and_any_pull_request_on_screen() {
        // The session is application-wide, so one sign-out empties them all —
        // and a pull request in the detail pane came from a session that no
        // longer exists.
        let mut app = app_with_forge_repo();
        app.app.forge.identity = Some(hidegit_forge::Identity {
            login: "youhide".into(),
            name: None,
            avatar_url: None,
        });

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![pull_request(47)])))),
        ));
        app.app.repos[0].selection = Some(Selection::PullRequest(47));

        let _ = app.update(Message::ForgeSignedOut(Box::new(Ok(()))));

        assert!(!app.app.forge.is_connected());
        assert!(app.app.repos[0].prs.items.is_empty());
        assert_eq!(app.app.repos[0].prs.state, PrState::Idle);
        assert_eq!(app.app.repos[0].selection, None);
        assert!(matches!(app.app.repos[0].detail, DetailPane::Empty));
    }

    #[test]
    fn a_missing_keychain_disables_the_panel_without_raising_a_toast() {
        // There is nothing to retry and nothing to dismiss. The panel says so
        // where somebody would look for pull requests.
        let mut app = Hidegit::default();

        // Building a client builds an HTTP stack, which wants a reactor. In the
        // application it is built inside a `Task`, which already has one.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();

        let _ = app.update(Message::ForgeClientBuilt(
            Arc::new(hidegit_forge::GitHub::public(Arc::new(
                hidegit_forge::MemoryStore::default(),
            ))),
            Box::new(Err(UiError {
                summary: "no OS keychain is available, so forge features are disabled".into(),
                details: String::new(),
            })),
        ));

        assert!(app.app.forge.no_keychain);
        assert!(app.app.toasts.is_empty());
    }

    #[test]
    fn a_new_pull_request_appears_without_waiting_for_the_next_poll() {
        // A poll may be minutes away, and a pull request that does not show up
        // after being opened reads as a failure.
        let mut app = app_with_forge_repo();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrCreated(Box::new(Ok(pull_request(48)))),
        ));

        assert_eq!(app.app.repos[0].prs.items.len(), 1);
        assert_eq!(app.app.repos[0].prs.items[0].number, 48);
        assert_eq!(app.app.repos[0].prs.state, PrState::Loaded);
    }

    #[test]
    fn a_pull_request_that_a_poll_also_brought_is_not_listed_twice() {
        let mut app = app_with_forge_repo();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![pull_request(48)])))),
        ));
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrCreated(Box::new(Ok(pull_request(48)))),
        ));

        assert_eq!(app.app.repos[0].prs.items.len(), 1);
    }

    #[test]
    fn the_device_code_dialog_dismisses_without_cancelling_the_flow() {
        // The flow keeps polling and the token still arrives. Only the dialog
        // goes away, which is why there is no message that stops the task.
        let mut app = Hidegit::default();

        let _ = app.update(Message::DeviceCodeIssued(Box::new(
            hidegit_forge::DeviceCode {
                user_code: "WDJB-MJHT".into(),
                verification_uri: "https://github.com/login/device".into(),
                expires_in: std::time::Duration::from_secs(900),
            },
        )));
        assert!(app.app.forge.connecting.is_some());

        let _ = app.update(Message::DeviceCodeDismissed);
        assert!(app.app.forge.connecting.is_none());
    }

    #[test]
    fn a_resolved_flow_clears_the_code_whether_it_worked_or_not() {
        // Leaving it up after a refusal would show a code that can no longer be
        // typed.
        let mut app = Hidegit::default();
        let _ = app.update(Message::DeviceCodeIssued(Box::new(
            hidegit_forge::DeviceCode {
                user_code: "WDJB-MJHT".into(),
                verification_uri: "https://github.com/login/device".into(),
                expires_in: std::time::Duration::from_secs(900),
            },
        )));

        let _ = app.update(Message::ForgeConnected(Box::new(Err(UiError {
            summary: "the code expired before it was approved".into(),
            details: String::new(),
        }))));

        assert!(app.app.forge.connecting.is_none());
        assert!(!app.app.forge.is_connected());
        assert_eq!(app.app.toasts.len(), 1);
    }

    #[test]
    fn escape_closes_the_device_code_dialog_before_anything_underneath_it() {
        let key = keyboard::Key::Named(keyboard::key::Named::Escape);

        assert!(matches!(
            modal_shortcut(&key, Modal::DeviceCode),
            Message::DeviceCodeDismissed
        ));
    }

    #[test]
    fn enter_does_nothing_on_the_device_code_dialog() {
        // There is nothing to accept — hideGit is waiting on GitHub, not on the
        // user's keyboard.
        let key = keyboard::Key::Named(keyboard::key::Named::Enter);

        assert!(matches!(
            modal_shortcut(&key, Modal::DeviceCode),
            Message::ToastDismissed(_)
        ));
    }

    #[test]
    fn the_first_poll_after_launch_notifies_about_nothing() {
        // Otherwise opening a repository alerts about every pull request that
        // already needed attention, which is all of them.
        let recorder = Arc::new(hidegit_forge::Recorder::default());
        let mut app = app_with_forge_repo();
        app.app.notifier = recorder.clone();

        let mut failing = pull_request(47);
        failing.checks = hidegit_forge::CheckState::Failing;

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![failing])))),
        ));

        assert!(recorder.shown().is_empty());
    }

    #[test]
    fn a_change_after_the_baseline_reaches_the_notifier() {
        let recorder = Arc::new(hidegit_forge::Recorder::default());
        let mut app = app_with_forge_repo();
        app.app.notifier = recorder.clone();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![pull_request(47)])))),
        ));

        let mut failing = pull_request(47);
        failing.checks = hidegit_forge::CheckState::Failing;
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![failing])))),
        ));

        let shown = recorder.shown();
        assert_eq!(shown.len(), 1);
        assert!(shown[0].0.contains("Checks failed on #47"), "{shown:?}");
        assert!(shown[0].1.contains("youhide/hideGit"), "{shown:?}");
    }

    #[test]
    fn an_event_turned_off_reaches_no_notifier() {
        // `checks_passed` is off by default, which is the one preference a user
        // is most likely to notice not having changed.
        let recorder = Arc::new(hidegit_forge::Recorder::default());
        let mut app = app_with_forge_repo();
        app.app.notifier = recorder.clone();

        let mut failing = pull_request(47);
        failing.checks = hidegit_forge::CheckState::Failing;
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![failing])))),
        ));

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![pull_request(47)])))),
        ));

        assert!(
            recorder.shown().is_empty(),
            "checks passing is the absence of a problem, and is off by default"
        );
    }

    #[test]
    fn a_muted_repository_stays_silent_while_the_panel_keeps_working() {
        let recorder = Arc::new(hidegit_forge::Recorder::default());
        let mut app = app_with_forge_repo();
        app.app.notifier = recorder.clone();
        app.app.alerts.muted = vec!["youhide/hideGit".to_owned()];

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![pull_request(47)])))),
        ));
        let mut failing = pull_request(47);
        failing.checks = hidegit_forge::CheckState::Failing;
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![failing])))),
        ));

        assert!(recorder.shown().is_empty());
        assert_eq!(
            app.app.repos[0].prs.items.len(),
            1,
            "muting silences the desktop, not the panel"
        );
    }

    #[test]
    fn a_failed_poll_never_notifies() {
        let recorder = Arc::new(hidegit_forge::Recorder::default());
        let mut app = app_with_forge_repo();
        app.app.notifier = recorder.clone();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Ok(loaded(vec![pull_request(47)])))),
        ));
        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrsLoaded(Box::new(Err(UiError {
                summary: "could not reach github.com".into(),
                details: String::new(),
            }))),
        ));

        assert!(recorder.shown().is_empty());
    }

    #[test]
    fn a_merged_pull_request_is_reported_as_merged_rather_than_as_closed() {
        // The poll asks only for open pull requests, so an ending arrives as an
        // absence — and the absence cannot say which of the two it was.
        let recorder = Arc::new(hidegit_forge::Recorder::default());
        let mut app = app_with_forge_repo();
        app.app.notifier = recorder.clone();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrEndingLoaded(Box::new(Ok(hidegit_forge::PullRequestDetail {
                pr: pull_request(47),
                lifecycle: hidegit_forge::Lifecycle::Merged,
                body: String::new(),
                reviews: Vec::new(),
                commits: 1,
                changed_files: 1,
                additions: 1,
                deletions: 0,
            }))),
        ));

        let shown = recorder.shown();
        assert_eq!(shown.len(), 1);
        assert!(shown[0].0.contains("merged"), "{shown:?}");
    }

    #[test]
    fn a_pull_request_that_fell_off_the_page_is_not_an_ending() {
        // A repository with more than one page of open pull requests can drop
        // one out of the window without anything having happened to it.
        let recorder = Arc::new(hidegit_forge::Recorder::default());
        let mut app = app_with_forge_repo();
        app.app.notifier = recorder.clone();

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::PrEndingLoaded(Box::new(Ok(hidegit_forge::PullRequestDetail {
                pr: pull_request(47),
                lifecycle: hidegit_forge::Lifecycle::Open,
                body: String::new(),
                reviews: Vec::new(),
                commits: 1,
                changed_files: 1,
                additions: 1,
                deletions: 0,
            }))),
        ));

        assert!(recorder.shown().is_empty());
    }

    #[test]
    fn dragging_the_scrollbar_to_the_end_lands_on_the_last_screenful() {
        // Not on the last row at the top of an empty window: the scrollable
        // range is the history minus one screenful.
        let mut app = app_with(100);
        app.app.repos[0].graph.viewport_rows = 10;

        let _ = app.update(Message::Repo(0, RepoMessage::GraphScrolledTo(1.0)));
        assert_eq!(app.app.repos[0].graph.scroll, 90.0);

        let _ = app.update(Message::Repo(0, RepoMessage::GraphScrolledTo(0.0)));
        assert_eq!(app.app.repos[0].graph.scroll, 0.0);

        let _ = app.update(Message::Repo(0, RepoMessage::GraphScrolledTo(0.5)));
        assert_eq!(app.app.repos[0].graph.scroll, 45.0);
    }

    #[test]
    fn a_fraction_outside_the_bar_is_clamped_rather_than_trusted() {
        // The pointer goes past both ends of the track while dragging.
        let mut app = app_with(100);
        app.app.repos[0].graph.viewport_rows = 10;

        let _ = app.update(Message::Repo(0, RepoMessage::GraphScrolledTo(4.0)));
        assert_eq!(app.app.repos[0].graph.scroll, 90.0);

        let _ = app.update(Message::Repo(0, RepoMessage::GraphScrolledTo(-2.0)));
        assert_eq!(app.app.repos[0].graph.scroll, 0.0);
    }

    #[test]
    fn a_history_that_fits_on_screen_does_not_scroll() {
        let mut app = app_with(5);
        app.app.repos[0].graph.viewport_rows = 40;

        let _ = app.update(Message::Repo(0, RepoMessage::GraphScrolledTo(1.0)));
        assert_eq!(app.app.repos[0].graph.scroll, 0.0);
    }

    #[test]
    fn copying_a_failure_takes_what_was_attempted_along_with_the_detail() {
        // Details alone are often a wall of stderr with no statement of what
        // was being tried, which is the half a bug report needs most.
        let mut app = Hidegit::default();
        app.app.toast(&UiError {
            summary: "git push origin failed".into(),
            details: "! [rejected] main -> main (stale info)".into(),
        });

        let id = app.app.toasts[0].id;
        let _ = app.update(Message::ToastCopied(id));

        assert_eq!(app.app.toasts.len(), 1, "copying does not dismiss it");
    }

    #[test]
    fn a_forge_failure_carries_something_worth_copying() {
        // `DeviceFlow(Disabled)` on a clipboard is a shrug. The detail is what
        // someone pastes into a message asking for help.
        let error = UiError::from(hidegit_forge::ForgeError::DeviceFlow(
            hidegit_forge::DeviceFlowError::Disabled,
        ));

        assert_eq!(error.summary, "this app does not have device flow enabled");
        assert!(error.details.contains("Enable Device Flow"), "{error:?}");
        assert!(!error.details.contains("DeviceFlow("), "{error:?}");
    }

    #[test]
    fn a_pasted_token_is_accepted_with_no_repository_open() {
        // Signing in is not something you do to a repository.
        let app = Hidegit::default();
        let prompt = Prompt {
            kind: PromptKind::PersonalAccessToken,
            title: "Personal access token".into(),
            confirm_label: "Connect".into(),
            fields: vec![PromptField {
                label: "Token".into(),
                placeholder: String::new(),
                value: "  ghp_pasted  ".into(),
            }],
        };

        match app.prompt_action(&prompt) {
            Some(Message::TokenSubmitted(token)) => {
                assert_eq!(token, "ghp_pasted", "trimmed, like every other prompt");
            }
            other => panic!("expected TokenSubmitted, got {other:?}"),
        }
    }
}
