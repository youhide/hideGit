//! The GitHub implementation of [`Forge`].
//!
//! Polling goes over GraphQL — one query per repository per poll returns every
//! field the sidebar and every notification need. See
//! `docs/adr/0006-poll-pull-requests-over-graphql.md` for why, and for what
//! editing a query costs.

mod query;
mod translate;

use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use octocrab::{GraphqlResponse, Octocrab};
use serde_json::json;
use time::OffsetDateTime;

use crate::error::ForgeError;
use crate::model::{
    ForgeId, Identity, NewPullRequest, PollCursor, PollResult, PullRequest, PullRequestDetail,
    RepoRef, WebTarget,
};
use crate::token::{self, StoredToken, TokenStore};
use crate::{AuthFlow, Forge, auth, detect};

/// The host hideGit knows without being told.
///
/// A GitHub Enterprise instance lives on an arbitrary domain, which no static
/// method can recognise — [`GitHub::detect`] has no configuration to consult —
/// so detection alone only ever claims `github.com`.
pub const PUBLIC_HOST: &str = "github.com";

/// Where a GitHub lives.
///
/// Three separate bases because GitHub uses three: the API is on a different
/// host from the website on `github.com`, and the OAuth endpoints are on the
/// website rather than the API. Carrying them apart is also what lets a test
/// point all three at one mock server.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// The bare hostname, for display and for web URLs.
    pub host: String,
    /// Base for REST and `/graphql`.
    pub api: String,
    /// Base for `/login/*`.
    pub oauth: String,
}

impl Endpoint {
    pub fn public() -> Self {
        Self {
            host: PUBLIC_HOST.to_owned(),
            api: "https://api.github.com".to_owned(),
            oauth: "https://github.com".to_owned(),
        }
    }

    /// Every base pointed at one server.
    ///
    /// **GitHub Enterprise is not wired up.** It would need `/api/v3` for REST
    /// and `/api/graphql` for GraphQL — a different layout, not a different
    /// hostname — and there is no configuration surface to supply a host from
    /// yet. This constructor exists for tests, and naming it so keeps it from
    /// being mistaken for enterprise support.
    #[cfg(any(test, feature = "fake"))]
    pub fn testing(uri: &str) -> Self {
        Self {
            host: PUBLIC_HOST.to_owned(),
            api: uri.to_owned(),
            oauth: uri.to_owned(),
        }
    }
}

#[derive(Debug)]
pub struct GitHub {
    endpoint: Endpoint,
    client_id: String,
    store: Arc<dyn TokenStore>,
    /// Swapped when a token is issued or refreshed, so the credential lives in
    /// exactly one place and every request picks up the current one.
    crab: RwLock<Octocrab>,
}

impl GitHub {
    /// A client with no credentials yet.
    pub fn new(
        endpoint: Endpoint,
        client_id: impl Into<String>,
        store: Arc<dyn TokenStore>,
    ) -> Self {
        let crab = build(&endpoint.api, None);
        Self {
            endpoint,
            client_id: client_id.into(),
            store,
            crab: RwLock::new(crab),
        }
    }

    /// `github.com`, hideGit's registered app, and the OS keychain.
    pub fn public(store: Arc<dyn TokenStore>) -> Self {
        Self::new(Endpoint::public(), auth::CLIENT_ID, store)
    }

    /// Restores a saved session, refreshing the token if it is about to expire.
    ///
    /// `Ok(None)` when nothing is stored, which is the ordinary state of a
    /// first run rather than a failure.
    pub async fn resume(&self) -> Result<Option<Identity>, ForgeError> {
        let Some(token) = token::load(&self.store, &self.endpoint.host).await? else {
            return Ok(None);
        };

        let token = if token.needs_refresh(OffsetDateTime::now_utc()) {
            self.refreshed(&token).await?
        } else {
            token
        };

        self.adopt(&token);
        Ok(Some(Identity {
            login: token.login,
            name: None,
            avatar_url: None,
        }))
    }

    /// Forgets the token, here and in the keychain.
    pub async fn sign_out(&self) -> Result<(), ForgeError> {
        self.adopt_none();
        token::clear(&self.store, &self.endpoint.host).await
    }

    /// Exchanges a refresh token and stores what came back.
    async fn refreshed(&self, token: &StoredToken) -> Result<StoredToken, ForgeError> {
        let refresh = token
            .refresh
            .as_ref()
            .ok_or(ForgeError::NotAuthenticated(ForgeId::GitHub))?;

        let issued = auth::refresh(
            &self.oauth_client(),
            &self.endpoint.oauth,
            &self.client_id,
            refresh,
        )
        .await?;
        let fresh = issued.into_stored(token.login.clone());
        token::save(&self.store, &self.endpoint.host, fresh.clone()).await?;
        Ok(fresh)
    }

    /// A client for the OAuth endpoints, which are unauthenticated and live on
    /// the website rather than the API host.
    fn oauth_client(&self) -> Octocrab {
        Octocrab::builder()
            .base_uri(&self.endpoint.oauth)
            .and_then(|builder| {
                // GitHub's OAuth endpoints answer form-encoded by default;
                // asking for JSON is what makes the reply parseable.
                builder
                    .add_header(http::header::ACCEPT, "application/json".to_owned())
                    .build()
            })
            .unwrap_or_else(|error| {
                // The base URI came from `Endpoint`, not from user input, so
                // this is a programming error rather than something to surface.
                tracing::error!(%error, "the OAuth base URI is not a URI");
                Octocrab::default()
            })
    }

    fn adopt(&self, token: &StoredToken) {
        *self.crab.write().unwrap_or_else(PoisonError::into_inner) =
            build(&self.endpoint.api, Some(token));
    }

    fn adopt_none(&self) {
        *self.crab.write().unwrap_or_else(PoisonError::into_inner) =
            build(&self.endpoint.api, None);
    }

    /// The HTTP client, recovered if a panic poisoned the lock.
    ///
    /// This guards a client handle, not state anything can be half-way through.
    /// Honouring a poison would mean one panicked request turns every later call
    /// into a panic for the life of the process — sign-in included, so there
    /// would be no way back short of a restart.
    fn crab(&self) -> Octocrab {
        self.crab
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// `https://github.com`, or `https://<host>` for another instance.
    fn web_root(&self) -> String {
        format!("https://{}", self.endpoint.host)
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
            .crab()
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
                host: self.endpoint.host.clone(),
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

    /// Obtains a token, adopts it, and saves it to the keychain.
    ///
    /// The client keeps custody of the token rather than handing it back: it is
    /// the only object that needs the value, and a token that is never returned
    /// is a token that cannot end up in a log line somebody added upstream.
    ///
    /// The keychain is written *before* the identity is returned, so a caller
    /// that sees success can rely on the session surviving a restart.
    async fn authenticate(&self, flow: AuthFlow) -> Result<Identity, ForgeError> {
        let issued = match flow {
            AuthFlow::Device(announce) => {
                auth::device_flow(
                    &self.oauth_client(),
                    &self.endpoint.oauth,
                    &self.client_id,
                    announce,
                )
                .await?
            }
            // A personal access token is already a token. It has no expiry
            // hideGit can see and no refresh token, which `StoredToken`
            // represents as absent rather than as a guessed lifetime.
            AuthFlow::Token(token) => auth::Issued {
                access: token,
                expires_at: None,
                refresh: None,
                refresh_expires_at: None,
            },
        };

        // Adopted before the login is known, because reading the login is the
        // first request the new credential makes — and that request doubles as
        // proof the token works, which is what a pasted personal access token
        // most needs.
        let provisional = issued.into_stored(String::new());
        self.adopt(&provisional);

        let identity = match self.current_user().await {
            Ok(identity) => identity,
            Err(error) => {
                // A token that could not name its owner is not one to keep
                // holding: leaving it adopted would make every later call fail
                // in the same way, with no sign of why.
                self.adopt_none();
                return Err(error);
            }
        };

        let stored = StoredToken {
            login: identity.login.clone(),
            ..provisional
        };
        token::save(&self.store, &self.endpoint.host, stored.clone()).await?;
        self.adopt(&stored);

        Ok(identity)
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
            .crab()
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
            //
            // The slug is a constant rather than the project's name because
            // GitHub generates one at registration and it need not match — see
            // `auth::APP_SLUG`.
            WebTarget::Install => {
                format!("{root}/apps/{}/installations/new", auth::APP_SLUG)
            }
        }
    }
}

/// An API client, with a token if there is one.
///
/// `user_access_token` rather than `personal_token`: both send a bearer token,
/// but the first is what a GitHub App's user-to-server token is, and naming it
/// correctly is what keeps the next reader from concluding the App path was
/// never implemented.
fn build(api: &str, token: Option<&StoredToken>) -> Octocrab {
    let builder = Octocrab::builder();
    let builder = match token {
        Some(token) => builder.user_access_token(token.access.expose().to_owned()),
        None => builder,
    };

    builder
        .base_uri(api)
        .and_then(|builder| builder.build())
        .unwrap_or_else(|error| {
            // The base URI comes from `Endpoint`, never from user input.
            tracing::error!(%error, "the API base URI is not a URI");
            Octocrab::default()
        })
}

#[cfg(test)]
mod tests;
