//! Talking to the forge from the UI thread, which is to say: not on it.
//!
//! Every function here returns a `Task`. Forge calls are `async` rather than
//! blocking, so they go straight onto the runtime with `Task::perform` instead
//! of through the blocking pool that `gix` calls and `git` subprocesses use.

use std::sync::Arc;

use hidegit_core::model::Remote;
use hidegit_forge::{
    AuthFlow, DeviceCode, Forge, ForgeError, GitHub, Identity, Keychain, NewPullRequest,
    PullRequest, PullRequestDetail, RepoRef, SecretString, WebTarget,
};
use iced::Task;

use crate::message::{Message, PrsLoad, RepoMessage, UiError};

/// Builds the client and restores a stored session.
///
/// Runs once at boot. A first run resolves to `Ok(None)`, which is not a
/// failure and must not raise anything.
pub fn boot() -> Task<Message> {
    Task::perform(
        async {
            let client = Arc::new(GitHub::public(Arc::new(Keychain)));
            let identity = client.resume().await;
            (client, identity)
        },
        |(client, identity)| {
            Message::ForgeClientBuilt(client, Box::new(identity.map_err(UiError::from)))
        },
    )
}

/// Runs the device flow, announcing the code as soon as GitHub issues one.
///
/// The announcement travels back as an ordinary `Message` through a channel,
/// because the flow keeps polling afterwards: the code has to reach the screen
/// *while* this task is still running, not when it finishes.
pub fn device_flow(client: Arc<GitHub>) -> Task<Message> {
    let (sender, receiver) = iced::futures::channel::mpsc::unbounded();

    let announce =
        Task::stream(receiver).map(|code: DeviceCode| Message::DeviceCodeIssued(Box::new(code)));

    let connect = Task::perform(
        async move {
            client
                .authenticate(AuthFlow::Device(Box::new(move |code| {
                    // A closed receiver means the window went away mid-flow.
                    // The flow is still worth finishing: the token is stored,
                    // so the next launch is already signed in.
                    let _ = sender.unbounded_send(code);
                })))
                .await
        },
        |result| Message::ForgeConnected(Box::new(result.map_err(UiError::from))),
    );

    Task::batch([announce, connect])
}

/// Signs in with a pasted personal access token.
pub fn with_token(client: Arc<GitHub>, token: String) -> Task<Message> {
    Task::perform(
        async move {
            client
                .authenticate(AuthFlow::Token(SecretString::new(token)))
                .await
        },
        |result| Message::ForgeConnected(Box::new(result.map_err(UiError::from))),
    )
}

pub fn sign_out(client: Arc<GitHub>) -> Task<Message> {
    // Synchronous — it clears a keychain entry — but it still leaves the UI
    // thread, because a keychain can prompt and a prompt can block.
    Task::perform(
        async move { tokio::task::spawn_blocking(move || client.sign_out()).await },
        |joined| {
            let result = match joined {
                Ok(result) => result.map_err(UiError::from),
                Err(error) => Err(UiError {
                    summary: "could not sign out".to_owned(),
                    details: error.to_string(),
                }),
            };
            Message::ForgeSignedOut(Box::new(result))
        },
    )
}

/// One poll for one repository.
pub fn poll(client: Arc<GitHub>, index: usize, repo: RepoRef) -> Task<Message> {
    Task::perform(
        async move {
            match client.pull_requests(&repo, None).await {
                // A cursor-less poll always carries data; `None` would mean
                // "unchanged", which GraphQL never reports.
                Ok(result) => Ok(PrsLoad::Loaded {
                    items: result.data.unwrap_or_default(),
                    budget: result.budget,
                }),
                // Lifted out of the error channel: the request succeeded and
                // the answer is that hideGit cannot see this repository, which
                // has an action attached rather than a failure to report.
                Err(ForgeError::NotInstalled { install_url, .. }) => {
                    Ok(PrsLoad::NotInstalled { install_url })
                }
                Err(error) => Err(UiError::from(error)),
            }
        },
        move |result| Message::Repo(index, RepoMessage::PrsLoaded(Box::new(result))),
    )
}

pub fn detail(client: Arc<GitHub>, index: usize, repo: RepoRef, number: u64) -> Task<Message> {
    Task::perform(
        async move { client.pull_request(&repo, number).await },
        move |result: Result<PullRequestDetail, _>| {
            Message::Repo(
                index,
                RepoMessage::PrDetailLoaded(Box::new(result.map_err(UiError::from))),
            )
        },
    )
}

/// Finds out how a pull request that vanished from the open list ended.
///
/// The same request as `detail`, landing on a different message: this one feeds
/// a notification rather than the detail pane, and routing both through one
/// message would put a merged pull request on screen because it was merged.
pub fn ending(client: Arc<GitHub>, index: usize, repo: RepoRef, number: u64) -> Task<Message> {
    Task::perform(
        async move { client.pull_request(&repo, number).await },
        move |result: Result<PullRequestDetail, _>| {
            Message::Repo(
                index,
                RepoMessage::PrEndingLoaded(Box::new(result.map_err(UiError::from))),
            )
        },
    )
}

pub fn create(
    client: Arc<GitHub>,
    index: usize,
    repo: RepoRef,
    draft: NewPullRequest,
) -> Task<Message> {
    Task::perform(
        async move { client.create_pull_request(&repo, draft).await },
        move |result: Result<PullRequest, _>| {
            Message::Repo(
                index,
                RepoMessage::PrCreated(Box::new(result.map_err(UiError::from))),
            )
        },
    )
}

/// The web URL for something on the forge.
pub fn web_url(client: &GitHub, repo: &RepoRef, target: WebTarget) -> String {
    client.web_url(repo, target)
}

/// Hands a URL to the platform's browser.
///
/// Failure is a toast rather than silence: a button that looks like it opened
/// something and did not is worse than one that says it could not.
pub fn open_url(url: String) -> Task<Message> {
    Task::perform(
        async move {
            (
                url.clone(),
                tokio::task::spawn_blocking(move || open::that(url)).await,
            )
        },
        |(url, joined)| match joined {
            Ok(Ok(())) => Message::ToastDismissed(u64::MAX),
            Ok(Err(error)) => Message::OpenUrlFailed(Box::new(UiError {
                summary: format!("could not open {url}"),
                details: error.to_string(),
            })),
            Err(error) => Message::OpenUrlFailed(Box::new(UiError {
                summary: format!("could not open {url}"),
                details: error.to_string(),
            })),
        },
    )
}

/// Which forge repository a set of remotes points at.
///
/// The first remote that names one wins, with `origin` preferred: a fork
/// configured with `origin` for your copy and `upstream` for the project has
/// pull requests in both places, and yours is the one you are working in.
pub fn detect(remotes: &[Remote]) -> Option<RepoRef> {
    let named = |name: &str| {
        remotes
            .iter()
            .find(|remote| remote.name == name)
            .and_then(|remote| GitHub::detect(&remote.fetch_url))
    };

    named("origin")
        .or_else(|| named("upstream"))
        .or_else(|| remotes.iter().find_map(|r| GitHub::detect(&r.fetch_url)))
}

/// Whether an identity is the one a session already holds.
pub fn same_identity(a: Option<&Identity>, b: &Identity) -> bool {
    a.is_some_and(|a| a.login == b.login)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(name: &str, url: &str) -> Remote {
        Remote {
            name: name.to_owned(),
            fetch_url: url.to_owned(),
            push_url: None,
        }
    }

    #[test]
    fn a_fork_polls_your_own_copy_rather_than_the_project_you_forked() {
        // `origin` is where you push and where your pull requests live.
        // Preferring whichever remote happens to be listed first would poll
        // upstream about a branch that only exists in your fork.
        let remotes = vec![
            remote("upstream", "https://github.com/original/thing.git"),
            remote("origin", "git@github.com:youhide/thing.git"),
        ];

        assert_eq!(detect(&remotes).unwrap().owner, "youhide");
    }

    #[test]
    fn upstream_is_used_when_there_is_no_origin() {
        let remotes = vec![remote("upstream", "https://github.com/original/thing.git")];
        assert_eq!(detect(&remotes).unwrap().owner, "original");
    }

    #[test]
    fn a_remote_with_an_unusual_name_is_still_found() {
        let remotes = vec![remote("gh", "https://github.com/team/thing.git")];
        assert_eq!(detect(&remotes).unwrap().owner, "team");
    }

    #[test]
    fn a_repository_with_no_forge_remote_names_none() {
        // Every remote in hideGit's own test suite is a bare repository on a
        // local path, so this is the common case rather than the exotic one.
        let remotes = vec![
            remote("origin", "/srv/git/thing.git"),
            remote("backup", "ssh://someone@example.invalid/srv/thing.git"),
        ];

        assert_eq!(detect(&remotes), None);
    }

    #[test]
    fn a_repository_with_no_remotes_at_all_names_none() {
        assert_eq!(detect(&[]), None);
    }
}
