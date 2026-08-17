//! Running a `Task` to completion, so a test can see what it did.
//!
//! `update` answers with a `Task`, and until now nothing in the suite ran one.
//! That left a whole class of message untestable past the point it was
//! dispatched: everything whose entire job is to reach the backend produced a
//! `Task` the test discarded, so the strongest available assertion was the
//! *identity* of the message a button carried — never that the operation
//! happened.
//!
//! `Task` keeps its stream private, and `iced` does not re-export the accessor.
//! `iced_runtime::task::into_stream` does expose it, which is why that crate is
//! a dev-dependency here. It is an internal crate of a pre-1.0 toolkit, so this
//! module is deliberately the only thing that touches it: when iced 1.0 moves
//! it, one file needs changing rather than every test.

use iced_runtime::Action;
use iced_runtime::task::into_stream;

use super::*;

/// Runs `task` to completion and returns the messages it produced.
///
/// Only `Action::Output` — the messages that would be fed back into `update` —
/// is collected. A task that asks the shell to focus a widget or read the
/// clipboard produces other actions, and those are dropped here rather than
/// faked: this answers "what did it do", not "what would the window have done".
///
/// The runtime is per call so a test cannot leak one into the next, and it is
/// the multi-threaded flavour because `blocking` — which is how every backend
/// call in this crate is issued — needs `spawn_blocking` to have somewhere to
/// run.
pub fn drive(task: Task<Message>) -> Vec<Message> {
    let Some(stream) = into_stream(task) else {
        // A task with no stream is `Task::none()`, which did nothing by design.
        return Vec::new();
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime for the test");

    runtime.block_on(async move {
        use iced::futures::StreamExt;

        stream
            .filter_map(|action| async move {
                match action {
                    Action::Output(message) => Some(message),
                    _ => None,
                }
            })
            .collect()
            .await
    })
}

/// Feeds `app` a message and runs whatever it asks for, returning what came
/// back — one turn of the loop the window would run.
pub fn update_and_drive(app: &mut Hidegit, message: Message) -> Vec<Message> {
    let task = app.update(message);
    drive(task)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_that_does_nothing_produces_nothing() {
        assert!(drive(Task::none()).is_empty());
    }

    #[test]
    fn a_done_task_produces_its_message() {
        let messages = drive(Task::done(Message::SettingsDismissed));
        assert!(matches!(messages.as_slice(), [Message::SettingsDismissed]));
    }

    #[test]
    fn a_backend_call_reaches_the_backend() {
        // The assertion that was impossible before this module. `MergeRequested`
        // exists only to ask the backend to merge; nothing it does is visible in
        // the state that comes out of `update`, so every previous test could
        // check that a button *carried* this message and never that sending it
        // merged anything.
        let fake = Arc::new(FakeBackend::new().with_commits(commits(3)));
        let mut app = Hidegit::default();
        let mut opened = opened(3);
        opened.backend = Arc::clone(&fake) as Arc<dyn GitBackend>;
        let _ = app.update(Message::RepositoryOpened(Box::new(Ok(opened))));

        assert!(fake.writes().is_empty(), "nothing has been asked for yet");

        let _ = update_and_drive(
            &mut app,
            Message::Repo(0, RepoMessage::MergeRequested("feature".to_owned())),
        );

        let writes = fake.writes();
        assert!(
            writes
                .iter()
                .any(|call| matches!(call, hidegit_core::backend::WriteCall::Merge { from, .. } if from == "feature")),
            "the merge never reached the backend: {writes:?}"
        );
    }
}

/// Checkpoint rebuilding while history pages in.
///
/// These are the tests the harness above was built for: the whole difference is
/// *which* `Task` a landed page answers with, which nothing could observe until
/// one could be run.
#[cfg(test)]
mod paging {
    use super::*;

    fn page(count: usize, more: bool) -> Box<Result<crate::message::Page, UiError>> {
        Box::new(Ok(crate::message::Page {
            commits: commits(count),
            more,
        }))
    }

    #[test]
    fn a_page_with_more_to_come_does_not_rebuild_the_checkpoints() {
        // Each page invalidates the checkpoints the page before it produced, so
        // building them mid-load is work whose only result is thrown away — and
        // it copies the whole accumulated history to do it, on the UI thread.
        let mut app = app_with(3);

        let produced = update_and_drive(
            &mut app,
            Message::Repo(0, RepoMessage::CommitsLoaded(page(3, true))),
        );

        assert!(
            !produced
                .iter()
                .any(|m| matches!(m, Message::Repo(_, RepoMessage::CheckpointsBuilt(_)))),
            "checkpoints were rebuilt mid-load: {produced:?}"
        );
    }

    #[test]
    fn the_last_page_rebuilds_them_once() {
        // The other half. Skipping the rebuild entirely would leave the graph
        // replaying from HEAD for every frame of a deep scroll, which is the
        // 23.9 ms the benchmark records for a window without checkpoints.
        let mut app = app_with(3);

        let produced = update_and_drive(
            &mut app,
            Message::Repo(0, RepoMessage::CommitsLoaded(page(3, false))),
        );

        assert!(
            produced
                .iter()
                .any(|m| matches!(m, Message::Repo(_, RepoMessage::CheckpointsBuilt(_)))),
            "the last page has to leave usable checkpoints behind: {produced:?}"
        );
    }
}

/// Submodule updates, from the row to the backend and back.
#[cfg(test)]
mod submodules {
    use hidegit_core::backend::WriteCall;
    use hidegit_core::model::Submodule;
    use hidegit_core::ops::SubmoduleUpdate;

    use super::*;

    fn submodule(recorded: Option<&str>, checked_out: Option<&str>) -> Submodule {
        let id = |hex: &str| ObjectId::from_hex(&hex.repeat(40)).expect("valid hex");

        Submodule {
            name: "vendor/lib".to_owned(),
            path: PathBuf::from("vendor/lib"),
            url: "https://example.invalid/lib.git".to_owned(),
            branch: None,
            recorded: recorded.map(id),
            checked_out: checked_out.map(id),
        }
    }

    /// An app whose backend records what is asked of it, with one submodule.
    fn app_with_submodule(submodules: Vec<Submodule>) -> (Hidegit, Arc<FakeBackend>) {
        let fake = Arc::new(
            FakeBackend::new()
                .with_commits(commits(3))
                .with_submodules(submodules.clone()),
        );
        let mut app = Hidegit::default();
        let mut opened = opened(3);
        opened.backend = Arc::clone(&fake) as Arc<dyn GitBackend>;
        opened.submodules = submodules;
        let _ = app.update(Message::RepositoryOpened(Box::new(Ok(opened))));
        (app, fake)
    }

    #[test]
    fn setting_up_a_submodule_reaches_the_backend_with_init() {
        let (mut app, fake) = app_with_submodule(vec![submodule(Some("a"), None)]);

        let _ = update_and_drive(
            &mut app,
            Message::Repo(
                0,
                RepoMessage::SubmoduleUpdateRequested {
                    path: PathBuf::from("vendor/lib"),
                    init: true,
                },
            ),
        );

        let writes = fake.writes();
        assert!(
            writes.iter().any(|call| matches!(
                call,
                WriteCall::UpdateSubmodules { paths, opts }
                    if paths == &[PathBuf::from("vendor/lib")]
                        && *opts == SubmoduleUpdate { init: true, recursive: false }
            )),
            "the update never reached the backend, or not with --init: {writes:?}"
        );
    }

    #[test]
    fn returning_a_moved_submodule_does_not_ask_for_init() {
        // A submodule that moved is already set up. Passing `--init` anyway
        // would be asking for something the user did not, on a repository that
        // does not need it.
        let (mut app, fake) = app_with_submodule(vec![submodule(Some("a"), Some("b"))]);

        let _ = update_and_drive(
            &mut app,
            Message::Repo(
                0,
                RepoMessage::SubmoduleUpdateRequested {
                    path: PathBuf::from("vendor/lib"),
                    init: false,
                },
            ),
        );

        let writes = fake.writes();
        assert!(
            writes.iter().any(|call| matches!(
                call,
                WriteCall::UpdateSubmodules { opts, .. } if !opts.init
            )),
            "the update reached the backend asking for the wrong thing: {writes:?}"
        );
    }

    #[test]
    fn an_update_that_settled_nothing_says_so_rather_than_looking_like_it_worked() {
        // `git submodule update` reports success for a submodule it left
        // exactly as it found it, which is why the outcome carries `settled`.
        let (mut app, _) = app_with_submodule(vec![submodule(Some("a"), None)]);
        let _cancel = pending(&mut app, 3);
        let id = 3;

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationFinished(
                id,
                Box::new(Ok(OperationOutcome::SubmodulesUpdated {
                    path: PathBuf::from("vendor/lib"),
                    settled: false,
                })),
            ),
        ));

        let toast = app.app.toasts.last().expect("it does not pass in silence");
        assert!(
            toast.summary.contains("vendor/lib"),
            "the toast names the submodule: {}",
            toast.summary
        );
    }

    #[test]
    fn an_update_that_settled_passes_in_silence() {
        // The refresh that follows is the result, the same as every other
        // operation that worked.
        let (mut app, _) = app_with_submodule(vec![submodule(Some("a"), Some("a"))]);
        let _cancel = pending(&mut app, 3);
        let id = 3;

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::OperationFinished(
                id,
                Box::new(Ok(OperationOutcome::SubmodulesUpdated {
                    path: PathBuf::from("vendor/lib"),
                    settled: true,
                })),
            ),
        ));

        assert!(
            app.app.toasts.is_empty(),
            "an operation that did what it said needs no announcement"
        );
    }
}

/// Removing and pruning worktrees, from the row to the backend.
#[cfg(test)]
mod worktrees {
    use hidegit_core::backend::WriteCall;
    use hidegit_core::model::Worktree;

    use super::*;

    fn linked(name: &str) -> Worktree {
        Worktree {
            path: PathBuf::from(format!("/elsewhere/{name}")),
            head: Some(Head::Branch {
                name: RefName {
                    kind: RefKind::LocalBranch,
                    full: format!("refs/heads/{name}"),
                    short: name.to_owned(),
                },
                target: ObjectId::from_hex(&"0".repeat(40)).expect("valid hex"),
            }),
            is_current: false,
            is_main: false,
            locked: None,
            prunable: false,
        }
    }

    fn app_with_worktrees(worktrees: Vec<Worktree>) -> (Hidegit, Arc<FakeBackend>) {
        let fake = Arc::new(
            FakeBackend::new()
                .with_commits(commits(3))
                .with_worktrees(worktrees.clone()),
        );
        let mut app = Hidegit::default();
        let mut opened = opened(3);
        opened.backend = Arc::clone(&fake) as Arc<dyn GitBackend>;
        opened.worktrees = worktrees;
        let _ = app.update(Message::RepositoryOpened(Box::new(Ok(opened))));
        (app, fake)
    }

    #[test]
    fn removing_a_worktree_asks_first_and_says_the_branch_comes_back() {
        // The branch is the thing the user is likely to want afterwards, and
        // "it stays" is the reassurance that makes this safe to accept.
        let (mut app, fake) = app_with_worktrees(vec![linked("side")]);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::WorktreeRemoveRequested {
                path: PathBuf::from("/elsewhere/side"),
            },
        ));

        assert!(fake.writes().is_empty(), "nothing has happened yet");
        let confirmation = app.app.confirming.as_ref().expect("removing confirms");
        assert!(
            confirmation.body.contains("side"),
            "it names the branch that comes back: {}",
            confirmation.body
        );
        assert_eq!(confirmation.confirm_label, "Remove");
    }

    #[test]
    fn the_confirmed_removal_reaches_the_backend_unforced() {
        // Force exists on the backend and is never true from here: the safe
        // form runs and Git's own refusal is what reaches the user.
        //
        // Driven through the confirmation rather than by dispatching the
        // confirmed message directly, because the flag is decided *by* the
        // confirmation — sending it by hand would assert nothing about what the
        // dialog actually carries.
        let (mut app, fake) = app_with_worktrees(vec![linked("side")]);

        let _ = app.update(Message::Repo(
            0,
            RepoMessage::WorktreeRemoveRequested {
                path: PathBuf::from("/elsewhere/side"),
            },
        ));
        for message in update_and_drive(&mut app, Message::ConfirmationAccepted) {
            let _ = update_and_drive(&mut app, message);
        }

        let writes = fake.writes();
        assert!(
            writes.iter().any(|call| matches!(
                call,
                WriteCall::RemoveWorktree { path, force: false }
                    if path == &PathBuf::from("/elsewhere/side")
            )),
            "the removal never reached the backend, or forced: {writes:?}"
        );
    }

    #[test]
    fn pruning_reaches_the_backend_without_asking_first() {
        // There is nothing left to lose: pruning clears registrations whose
        // directory is already gone, and a locked one survives it regardless.
        let mut stale = linked("side");
        stale.prunable = true;
        let (mut app, fake) = app_with_worktrees(vec![stale]);

        let _ = update_and_drive(
            &mut app,
            Message::Repo(0, RepoMessage::WorktreePruneRequested),
        );

        assert!(
            app.app.confirming.is_none(),
            "a directory that is already gone is not worth a dialog"
        );
        let writes = fake.writes();
        assert!(
            writes
                .iter()
                .any(|call| matches!(call, WriteCall::PruneWorktrees)),
            "the prune never reached the backend: {writes:?}"
        );
    }
}

/// Making a worktree from the branch row, through the folder picker.
#[cfg(test)]
mod new_worktree {
    use hidegit_core::backend::WriteCall;
    use hidegit_core::ops::{StartPoint, WorktreeSpec};

    use super::*;

    fn app_with_fake() -> (Hidegit, Arc<FakeBackend>) {
        let fake = Arc::new(FakeBackend::new().with_commits(commits(3)));
        let mut app = Hidegit::default();
        let mut opened = opened(3);
        opened.backend = Arc::clone(&fake) as Arc<dyn GitBackend>;
        let _ = app.update(Message::RepositoryOpened(Box::new(Ok(opened))));
        (app, fake)
    }

    #[test]
    fn the_picked_folder_gets_a_directory_named_after_the_branch() {
        // Not the picked folder itself: checking out straight into a directory
        // chosen for other reasons is how a home folder acquires a `.git`.
        // `feat/graph` is not a directory name either, so it is the last
        // segment that names it.
        let (mut app, fake) = app_with_fake();

        let _ = update_and_drive(
            &mut app,
            Message::Repo(
                0,
                RepoMessage::WorktreeDestinationPicked(
                    "feat/graph".to_owned(),
                    Some(PathBuf::from("/checkouts")),
                ),
            ),
        );

        let writes = fake.writes();
        assert!(
            writes.iter().any(|call| matches!(
                call,
                WriteCall::AddWorktree { path, spec }
                    if path == &PathBuf::from("/checkouts/graph")
                        && spec == &WorktreeSpec {
                            new_branch: None,
                            start: StartPoint::Ref("feat/graph".to_owned()),
                        }
            )),
            "the worktree was created somewhere else, or on the wrong branch: {writes:?}"
        );
    }

    #[test]
    fn a_cancelled_picker_creates_nothing() {
        let (mut app, fake) = app_with_fake();

        let _ = update_and_drive(
            &mut app,
            Message::Repo(
                0,
                RepoMessage::WorktreeDestinationPicked("feat/graph".to_owned(), None),
            ),
        );

        assert!(
            fake.writes().is_empty(),
            "changing your mind in a file dialog is not a request"
        );
    }
}

/// Which rebase the confirmed request actually runs.
#[cfg(test)]
mod autosquash {
    use hidegit_core::backend::WriteCall;
    use hidegit_core::ops::RebasePlan;

    use super::*;

    fn app_with_fake() -> (Hidegit, Arc<FakeBackend>) {
        let fake = Arc::new(FakeBackend::new().with_commits(commits(3)));
        let mut app = Hidegit::default();
        let mut opened = opened(3);
        opened.backend = Arc::clone(&fake) as Arc<dyn GitBackend>;
        let _ = app.update(Message::RepositoryOpened(Box::new(Ok(opened))));
        (app, fake)
    }

    fn rebase_plan(fake: &FakeBackend) -> RebasePlan {
        fake.writes()
            .into_iter()
            .find_map(|call| match call {
                WriteCall::Rebase { plan, .. } => Some(plan),
                _ => None,
            })
            .expect("a rebase reached the backend")
    }

    #[test]
    fn confirming_a_squashing_rebase_asks_git_to_squash() {
        // The dialog carrying the flag is worth nothing if the call underneath
        // drops it, and the two are far enough apart that only running the task
        // connects them.
        let (mut app, fake) = app_with_fake();

        let _ = update_and_drive(
            &mut app,
            Message::Repo(
                0,
                RepoMessage::RebaseConfirmed {
                    onto: "main".to_owned(),
                    autosquash: true,
                },
            ),
        );

        assert_eq!(rebase_plan(&fake), RebasePlan::Autosquash);
    }

    #[test]
    fn confirming_a_plain_rebase_does_not() {
        let (mut app, fake) = app_with_fake();

        let _ = update_and_drive(
            &mut app,
            Message::Repo(
                0,
                RepoMessage::RebaseConfirmed {
                    onto: "main".to_owned(),
                    autosquash: false,
                },
            ),
        );

        assert_eq!(
            rebase_plan(&fake),
            RebasePlan::Ordinary,
            "a plain rebase must not quietly rewrite more than it said it would"
        );
    }
}
