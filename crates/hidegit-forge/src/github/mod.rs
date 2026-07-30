//! The GitHub implementation of [`Forge`].
//!
//! Polling goes over GraphQL — one query per repository per poll returns every
//! field the sidebar and every notification need. See
//! `docs/adr/0006-poll-pull-requests-over-graphql.md` for why, and for what
//! editing a query costs.

mod query;
mod translate;

use async_trait::async_trait;
use octocrab::{GraphqlResponse, Octocrab};
use serde_json::json;

use crate::detect;
use crate::error::ForgeError;
use crate::model::{
    ForgeId, Identity, NewPullRequest, PollCursor, PollResult, PullRequest, PullRequestDetail,
    RepoRef, WebTarget,
};
use crate::{AuthFlow, Forge};

/// The host hideGit knows without being told.
///
/// A GitHub Enterprise instance lives on an arbitrary domain, which no static
/// method can recognise — [`GitHub::detect`] has no configuration to consult.
/// An enterprise host therefore has to be configured, and [`GitHub::new`] takes
/// one; detection alone only ever claims `github.com`.
pub const PUBLIC_HOST: &str = "github.com";

#[derive(Debug)]
pub struct GitHub {
    crab: Octocrab,
    host: String,
}

impl GitHub {
    /// A client for `host`, authenticated with `token`.
    pub fn new(host: impl Into<String>, crab: Octocrab) -> Self {
        Self {
            crab,
            host: host.into(),
        }
    }

    /// `https://github.com`, or `https://<host>` for an enterprise instance.
    fn web_root(&self) -> String {
        format!("https://{}", self.host)
    }

    /// Runs a GraphQL document, keeping partial results.
    ///
    /// Deliberately not `Octocrab::graphql`, which treats any `errors` array as
    /// a failure. GitHub answers a repository the token cannot see with
    /// `data: { repository: null }` *and* an error, and that pair is the only
    /// way to tell "not installed here" from "no open pull requests" — so both
    /// halves have to survive the call.
    async fn run<T: serde::de::DeserializeOwned>(
        &self,
        document: &str,
        variables: serde_json::Value,
    ) -> Result<T, ForgeError> {
        let payload = json!({ "query": document, "variables": variables });

        let response: GraphqlResponse<T> = self
            .crab
            .post("/graphql", Some(&payload))
            .await
            .map_err(|error| self.transport(error))?;

        match response {
            GraphqlResponse::Ok(ok) => Ok(ok.data),
            GraphqlResponse::Err(partial) => {
                let messages: Vec<&str> =
                    partial.errors.iter().map(|e| e.message.as_str()).collect();

                match partial.data {
                    // A partial success. GitHub's own message is kept in the
                    // log rather than shown, because the caller can still see
                    // what did arrive and deciding whether it is enough is its
                    // job, not this function's.
                    Some(data) => {
                        tracing::warn!(errors = ?messages, "GraphQL returned a partial result");
                        Ok(data)
                    }
                    None => Err(ForgeError::Api {
                        status: 200,
                        message: messages.join("; "),
                    }),
                }
            }
        }
    }

    /// Classifies an octocrab failure, keeping GitHub's own words.
    ///
    /// The same discipline the Git side uses on stderr: a status GitHub
    /// explains is reported with its explanation, and anything unrecognised
    /// degrades to the transport error rather than to a confident wrong
    /// diagnosis.
    fn transport(&self, error: octocrab::Error) -> ForgeError {
        match error {
            octocrab::Error::GitHub { source, .. } => {
                let status = source.status_code.as_u16();
                if status == 401 {
                    return ForgeError::NotAuthenticated(ForgeId::GitHub);
                }
                ForgeError::Api {
                    status,
                    message: source.message.clone(),
                }
            }
            other => ForgeError::Network {
                host: self.host.clone(),
                source: Box::new(other),
            },
        }
    }

    /// The error for a repository the token cannot see.
    ///
    /// For a GitHub App that means it is not installed there, which is a state
    /// with an action attached — install it — rather than an empty list.
    fn not_installed(&self, repo: &RepoRef) -> ForgeError {
        ForgeError::NotInstalled {
            install_url: self.web_url(repo, WebTarget::Install),
            repo: Box::new(repo.clone()),
        }
    }
}

#[async_trait]
impl Forge for GitHub {
    fn id(&self) -> ForgeId {
        ForgeId::GitHub
    }

    fn detect(remote_url: &str) -> Option<RepoRef> {
        let repo = detect::parse_remote_url(remote_url)?;
        (repo.host == PUBLIC_HOST).then_some(repo)
    }

    async fn authenticate(&self, _flow: AuthFlow) -> Result<Identity, ForgeError> {
        Err(ForgeError::NotImplementedYet {
            operation: "authenticate",
            milestone: "M4",
        })
    }

    async fn current_user(&self) -> Result<Identity, ForgeError> {
        let data: query::ViewerData = self.run(query::VIEWER, json!({})).await?;
        Ok(translate::identity(&data.viewer))
    }

    async fn pull_requests(
        &self,
        repo: &RepoRef,
        _since: Option<PollCursor>,
    ) -> Result<PollResult<Vec<PullRequest>>, ForgeError> {
        let data: query::PollData = self
            .run(
                query::POLL,
                json!({
                    "owner": repo.owner,
                    "name": repo.name,
                    "page": query::PAGE,
                    "nested": query::NESTED,
                }),
            )
            .await?;

        let budget = translate::budget(data.rate_limit.as_ref());
        let repository = data.repository.ok_or_else(|| self.not_installed(repo))?;

        let pull_requests = repository
            .pull_requests
            .nodes
            .iter()
            .flatten()
            .map(|node| translate::pull_request(node, &data.viewer.login))
            .collect();

        Ok(PollResult {
            data: Some(pull_requests),
            // GraphQL has no conditional requests, so there is nothing to carry
            // forward. The cursor stays on the trait for a REST-based forge.
            cursor: PollCursor::none(),
            budget,
        })
    }

    async fn pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequestDetail, ForgeError> {
        let data: query::DetailData = self
            .run(
                query::DETAIL,
                json!({
                    "owner": repo.owner,
                    "name": repo.name,
                    "number": number,
                    "nested": query::NESTED,
                }),
            )
            .await?;

        let node = data
            .repository
            .ok_or_else(|| self.not_installed(repo))?
            .pull_request
            .ok_or_else(|| ForgeError::Api {
                status: 404,
                message: format!("{repo} has no pull request #{number}"),
            })?;

        Ok(translate::detail(&node, &data.viewer.login))
    }

    async fn create_pull_request(
        &self,
        repo: &RepoRef,
        draft: NewPullRequest,
    ) -> Result<PullRequest, ForgeError> {
        // REST rather than the GraphQL mutation, which needs the repository's
        // node id and therefore a round trip to look it up first.
        let created = self
            .crab
            .pulls(&repo.owner, &repo.name)
            .create(&draft.title, &draft.head, &draft.base)
            .body(&draft.body)
            .draft(draft.draft)
            .send()
            .await
            .map_err(|error| self.transport(error))?;

        // Review and check state are deliberately not invented here. A pull
        // request one second old has no review decision and no checks that have
        // reported, and the poll that follows is what fills them in.
        Ok(PullRequest {
            number: created.number,
            title: created.title.clone().unwrap_or(draft.title),
            url: created.html_url.as_ref().map_or_else(
                || self.web_url(repo, WebTarget::Repository),
                ToString::to_string,
            ),
            author: created
                .user
                .as_ref()
                .map_or_else(|| "ghost".to_owned(), |user| user.login.clone()),
            head: draft.head,
            base: draft.base,
            draft: created.draft.unwrap_or(draft.draft),
            updated: time::OffsetDateTime::now_utc(),
            roles: std::collections::BTreeSet::from([crate::model::PrRole::Author]),
            review: crate::model::ReviewState::NotRequired,
            checks: crate::model::CheckState::None,
            merge: crate::model::MergeState::Unknown,
            comments: 0,
        })
    }

    fn web_url(&self, repo: &RepoRef, target: WebTarget) -> String {
        let root = self.web_root();
        let RepoRef { owner, name, .. } = repo;

        match target {
            WebTarget::Repository => format!("{root}/{owner}/{name}"),
            WebTarget::PullRequest(number) => format!("{root}/{owner}/{name}/pull/{number}"),
            WebTarget::NewPullRequest { head, base } => match base {
                Some(base) => format!("{root}/{owner}/{name}/compare/{base}...{head}?expand=1"),
                None => format!("{root}/{owner}/{name}/compare/{head}?expand=1"),
            },
            // Where the App is installed or granted access to more
            // repositories. Not a deep link to this repository: GitHub's
            // installation settings live under the account that owns the App,
            // and the owner is the one who has to act.
            WebTarget::Install => format!("{root}/apps/hidegit/installations/new"),
        }
    }
}

#[cfg(test)]
mod tests;
