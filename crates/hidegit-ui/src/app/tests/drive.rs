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
