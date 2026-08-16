//! The filesystem watcher, as an iced `Subscription`.
//!
//! `hidegit-core` owns the watch itself — which paths matter is repository
//! knowledge — but it must never depend on `iced`, so the bridge from "a change
//! happened" to "a `Message` arrived" lives here.
//!
//! The watch is created inside the stream and polled, rather than pushed into a
//! channel the runtime awaits. That is the shape `notify` fits: its callback
//! fires on its own thread, and a subscription that owns the watch keeps it
//! alive for exactly as long as the repository is open.

use std::path::PathBuf;
use std::time::Duration;

use iced::Subscription;
use iced::futures::stream;

use hidegit_core::watch::Watch;

use crate::message::{Message, RepoMessage};

/// How often the watch is drained.
///
/// The watch has already debounced; this only decides how long a change waits
/// before the UI hears about it. Short enough to feel immediate, long enough
/// that an idle repository costs nothing.
const POLL: Duration = Duration::from_millis(250);

/// What identifies a watch, and what it needs to start one.
///
/// `Subscription::run_with` takes a plain function pointer rather than a
/// closure, so everything the stream needs travels here. The hash is also what
/// decides identity: closing a repository ends its watch, and opening a
/// different one in the same slot starts a new one rather than silently keeping
/// the old directory under observation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Target {
    index: usize,
    workdir: PathBuf,
    git_dir: PathBuf,
}

/// Watches one open repository, emitting `RepositoryChanged` when it changes.
pub fn subscribe(index: usize, workdir: PathBuf, git_dir: PathBuf) -> Subscription<Message> {
    Subscription::run_with(
        Target {
            index,
            workdir,
            git_dir,
        },
        |target| {
            stream::unfold(
                (target.clone(), None),
                |(target, watch): (Target, Option<Watch>)| async move {
                    // Started lazily, inside the stream, because a `Watch` is
                    // neither `Clone` nor cheap to make and belongs to this
                    // subscription for its whole life.
                    let watch = match watch {
                        Some(watch) => watch,
                        None => match Watch::start(&target.workdir, &target.git_dir) {
                            Ok(watch) => watch,
                            Err(error) => {
                                // A repository that cannot be watched still
                                // works; it just will not refresh on its own.
                                // Saying so once in the log beats a toast the
                                // user cannot act on.
                                tracing::warn!(
                                    path = %target.workdir.display(),
                                    %error,
                                    "not watching this repository for changes"
                                );
                                return None;
                            }
                        },
                    };

                    loop {
                        tokio::time::sleep(POLL).await;
                        if let Some(change) = watch.drain() {
                            let index = target.index;
                            return Some((
                                Message::Repo(index, RepoMessage::RepositoryChanged(change)),
                                (target, Some(watch)),
                            ));
                        }
                    }
                },
            )
        },
    )
}
