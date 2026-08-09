//! HTTP is mocked. No test here touches the network or needs a real token.

use std::sync::Arc;

use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::model::{CheckState, MergeState, PrRole, ReviewState};
use crate::token::MemoryStore;
use crate::{DeviceFlowError, SecretString};

/// A client pointed at a mock server instead of GitHub, already holding a token.
async fn client(server: &MockServer) -> GitHub {
    let store = Arc::new(MemoryStore::default());
    store
        .save(
            PUBLIC_HOST,
            &StoredToken::permanent("youhide", SecretString::new("not-a-real-token")),
        )
        .expect("an in-memory store always saves");

    let github = GitHub::new(Endpoint::testing(&server.uri()), auth::CLIENT_ID, store);
    github.resume().await.expect("the stored token loads");
    github
}

/// A client with no token yet.
fn signed_out(server: &MockServer, store: Arc<MemoryStore>) -> GitHub {
    GitHub::new(Endpoint::testing(&server.uri()), auth::CLIENT_ID, store)
}

fn repo() -> RepoRef {
    RepoRef {
        host: PUBLIC_HOST.to_owned(),
        owner: "youhide".to_owned(),
        name: "hideGit".to_owned(),
    }
}

async fn responding(body: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

fn rate_limit(remaining: u32) -> Value {
    json!({
        "limit": 5000,
        "cost": 9,
        "remaining": remaining,
        "resetAt": "2026-07-30T13:00:00Z",
    })
}

/// One pull request, in the shape the poll query asks for.
fn node(number: u64, author: &str) -> Value {
    json!({
        "number": number,
        "title": "feat: hunk staging",
        "url": format!("https://github.com/youhide/hideGit/pull/{number}"),
        "isDraft": false,
        "updatedAt": "2026-07-30T12:00:00Z",
        "author": { "login": author },
        "headRefName": "feat/graph",
        "baseRefName": "main",
        "mergeable": "MERGEABLE",
        "reviewDecision": "REVIEW_REQUIRED",
        "comments": { "totalCount": 2 },
        "reviewThreads": { "totalCount": 1 },
        "reviewRequests": { "nodes": [] },
        "assignees": { "nodes": [] },
        "latestReviews": { "nodes": [] },
        "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "state": "SUCCESS" } } }] },
    })
}

fn poll_body(nodes: Vec<Value>) -> Value {
    json!({
        "data": {
            "viewer": { "login": "youhide" },
            "repository": { "pullRequests": { "nodes": nodes } },
            "rateLimit": rate_limit(4_991),
        }
    })
}

#[tokio::test]
async fn a_poll_returns_pull_requests_graded_against_the_viewer() {
    let server = responding(poll_body(vec![node(47, "youhide"), node(45, "someone")])).await;
    let github = client(&server).await;

    let result = github.pull_requests(&repo(), None).await.unwrap();
    let prs = result.data.expect("a successful poll carries data");

    assert_eq!(prs.len(), 2);
    assert_eq!(prs[0].number, 47);
    assert_eq!(prs[0].primary_role(), Some(PrRole::Author));
    assert_eq!(prs[0].checks, CheckState::Passing);
    assert_eq!(prs[0].merge, MergeState::Mergeable);
    assert_eq!(prs[0].review, ReviewState::Required);
    assert_eq!(prs[0].comments, 3, "issue comments plus review threads");

    assert_eq!(
        prs[1].primary_role(),
        None,
        "somebody else's pull request that does not involve you has no role"
    );
}

#[tokio::test]
async fn the_budget_rides_on_the_result_rather_than_costing_a_second_call() {
    let server = responding(poll_body(vec![])).await;
    let github = client(&server).await;

    let result = github.pull_requests(&repo(), None).await.unwrap();

    assert_eq!(result.budget.limit, 5_000);
    assert_eq!(result.budget.remaining, 4_991);
    assert!(result.budget.fraction_remaining() > 0.99);
}

#[tokio::test]
async fn graphql_polls_carry_no_cursor_because_there_is_nothing_conditional_to_carry() {
    let server = responding(poll_body(vec![])).await;
    let github = client(&server).await;

    let result = github.pull_requests(&repo(), None).await.unwrap();
    assert_eq!(result.cursor, PollCursor::none());
}

#[tokio::test]
async fn a_repository_the_token_cannot_see_is_not_a_repository_with_no_pull_requests() {
    // The case the whole NotInstalled variant exists for: GitHub answers a
    // repository the App is not installed on with a null repository and an
    // error beside it, and an empty sidebar would say the opposite of the truth.
    let server = responding(json!({
        "data": { "viewer": { "login": "youhide" }, "repository": null, "rateLimit": rate_limit(4_999) },
        "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to a Repository" }],
    }))
    .await;
    let github = client(&server).await;

    match github.pull_requests(&repo(), None).await {
        Err(ForgeError::NotInstalled { repo, install_url }) => {
            assert_eq!(repo.to_string(), "youhide/hideGit");
            assert_eq!(
                install_url,
                "https://github.com/apps/hidegit-github/installations/new"
            );
        }
        other => panic!("expected NotInstalled, got {other:?}"),
    }
}

#[tokio::test]
async fn a_partial_result_is_kept_rather_than_thrown_away() {
    // GitHub returns data *and* errors when one field of many was refused.
    // Discarding the response would drop every pull request over one
    // unreadable nested field.
    let server = responding(json!({
        "data": {
            "viewer": { "login": "youhide" },
            "repository": { "pullRequests": { "nodes": [node(47, "youhide")] } },
            "rateLimit": rate_limit(4_990),
        },
        "errors": [{ "message": "Although you appear to have the correct authorization credentials, the organisation has enabled OAuth App access restrictions" }],
    }))
    .await;
    let github = client(&server).await;

    let result = github.pull_requests(&repo(), None).await.unwrap();
    assert_eq!(result.data.expect("partial data survives").len(), 1);
}

#[tokio::test]
async fn a_null_node_beside_readable_ones_costs_only_itself() {
    let server = responding(json!({
        "data": {
            "viewer": { "login": "youhide" },
            "repository": { "pullRequests": { "nodes": [node(47, "youhide"), null] } },
            "rateLimit": rate_limit(4_990),
        }
    }))
    .await;
    let github = client(&server).await;

    let prs = github
        .pull_requests(&repo(), None)
        .await
        .unwrap()
        .data
        .unwrap();
    assert_eq!(prs.len(), 1);
}

#[tokio::test]
async fn an_expired_or_revoked_token_is_reported_as_not_signed_in() {
    // Distinguishable from every other failure because it is the one with an
    // obvious next action, and it is not "retry".
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "message": "Bad credentials",
            "documentation_url": "https://docs.github.com/graphql",
        })))
        .mount(&server)
        .await;
    let github = client(&server).await;

    assert!(matches!(
        github.pull_requests(&repo(), None).await,
        Err(ForgeError::NotAuthenticated(ForgeId::GitHub))
    ));
}

#[tokio::test]
async fn the_poll_asks_for_the_page_sizes_the_cost_was_reasoned_about_with() {
    // ADR-0006 prices the query on these numbers. A change here is a
    // rate-limit change, so it should fail a test rather than pass quietly.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(move |request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let variables = &body["variables"];

            assert_eq!(variables["page"], 50);
            assert_eq!(variables["nested"], 5);
            assert_eq!(variables["owner"], "youhide");
            assert_eq!(variables["name"], "hideGit");

            ResponseTemplate::new(200).set_body_json(poll_body(vec![]))
        })
        .mount(&server)
        .await;

    client(&server)
        .await
        .pull_requests(&repo(), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_detail_load_carries_the_body_and_the_reviews() {
    let mut node = node(47, "youhide");
    node["body"] = json!("Stages one hunk at a time.");
    node["additions"] = json!(48);
    node["deletions"] = json!(12);
    node["changedFiles"] = json!(2);
    node["commits"] = json!({
        "totalCount": 4,
        "nodes": [{ "commit": { "statusCheckRollup": { "state": "FAILURE" } } }],
    });
    node["latestReviews"] = json!({
        "nodes": [{
            "author": { "login": "someone" },
            "state": "CHANGES_REQUESTED",
            "submittedAt": "2026-07-30T11:00:00Z",
        }],
    });

    let server = responding(json!({
        "data": {
            "viewer": { "login": "youhide" },
            "repository": { "pullRequest": node },
            "rateLimit": rate_limit(4_995),
        }
    }))
    .await;

    let detail = client(&server)
        .await
        .pull_request(&repo(), 47)
        .await
        .unwrap();

    assert_eq!(detail.body, "Stages one hunk at a time.");
    assert_eq!(detail.additions, 48);
    assert_eq!(detail.deletions, 12);
    assert_eq!(detail.changed_files, 2);
    assert_eq!(detail.commits, 4);
    assert_eq!(detail.pr.checks, CheckState::Failing);
    assert_eq!(detail.reviews.len(), 1);
    assert_eq!(detail.reviews[0].author, "someone");
}

#[tokio::test]
async fn a_pull_request_number_that_does_not_exist_says_so() {
    let server = responding(json!({
        "data": {
            "viewer": { "login": "youhide" },
            "repository": { "pullRequest": null },
            "rateLimit": rate_limit(4_999),
        }
    }))
    .await;

    match client(&server).await.pull_request(&repo(), 9_999).await {
        Err(ForgeError::Api { status: 404, .. }) => {}
        other => panic!("expected a 404, got {other:?}"),
    }
}

#[tokio::test]
async fn the_current_user_is_read_from_the_token_rather_than_configured() {
    let server = responding(json!({
        "data": { "viewer": { "login": "youhide", "name": "Youri", "avatarUrl": "https://example.invalid/a.png" } }
    }))
    .await;

    let identity = client(&server).await.current_user().await.unwrap();
    assert_eq!(identity.login, "youhide");
    assert_eq!(identity.name.as_deref(), Some("Youri"));
}

#[test]
fn detection_claims_only_the_host_it_can_be_sure_of() {
    // A self-hosted instance lives on an arbitrary domain and no static method
    // can recognise one; claiming `github.example.com` would be a guess that
    // sends a token to whoever owns it.
    assert!(GitHub::detect("git@github.com:youhide/hideGit.git").is_some());
    assert!(GitHub::detect("https://github.com/youhide/hideGit").is_some());
    assert!(GitHub::detect("https://gitlab.com/youhide/hideGit").is_none());
    assert!(GitHub::detect("https://github.example.com/youhide/hideGit").is_none());
    assert!(GitHub::detect("/srv/git/hideGit.git").is_none());
}

#[tokio::test]
async fn every_web_target_builds_a_url_on_the_configured_host() {
    let server = MockServer::start().await;
    let github = client(&server).await;
    let repo = repo();

    assert_eq!(
        github.web_url(&repo, WebTarget::Repository),
        "https://github.com/youhide/hideGit"
    );
    assert_eq!(
        github.web_url(&repo, WebTarget::PullRequest(47)),
        "https://github.com/youhide/hideGit/pull/47"
    );
    assert_eq!(
        github.web_url(
            &repo,
            WebTarget::NewPullRequest {
                head: "feat/graph".to_owned(),
                base: Some("main".to_owned()),
            }
        ),
        "https://github.com/youhide/hideGit/compare/main...feat/graph?expand=1"
    );
    // Pinned in full, because this one is not derivable from anything in the
    // repository: GitHub generated the slug at registration and it does not
    // match the project's name. The first version of this line guessed
    // `hidegit` and led to a 404.
    assert_eq!(
        github.web_url(&repo, WebTarget::Install),
        "https://github.com/apps/hidegit-github/installations/new"
    );
}

#[tokio::test]
async fn another_host_produces_urls_on_that_host() {
    let server = MockServer::start().await;
    let endpoint = Endpoint {
        host: "github.example.com".to_owned(),
        api: server.uri(),
        oauth: server.uri(),
    };
    let github = GitHub::new(endpoint, auth::CLIENT_ID, Arc::new(MemoryStore::default()));

    let repo = RepoRef {
        host: "github.example.com".to_owned(),
        owner: "team".to_owned(),
        name: "thing".to_owned(),
    };

    assert_eq!(
        github.web_url(&repo, WebTarget::PullRequest(3)),
        "https://github.example.com/team/thing/pull/3"
    );
}

// ---- authentication ------------------------------------------------------

/// Answers `/user`-shaped GraphQL viewer queries with `login`.
async fn viewer_server(login: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "viewer": { "login": login, "name": null, "avatarUrl": null } }
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_personal_access_token_is_proved_before_it_is_stored() {
    // Reading the login is the first request the new credential makes, which
    // is also what tells a user their pasted token actually works.
    let server = viewer_server("youhide").await;
    let store = Arc::new(MemoryStore::default());
    let github = signed_out(&server, store.clone());

    let identity = github
        .authenticate(AuthFlow::Token(SecretString::new("ghp_pasted")))
        .await
        .unwrap();

    assert_eq!(identity.login, "youhide");

    let stored = store.load(PUBLIC_HOST).unwrap().expect("it was saved");
    assert_eq!(stored.login, "youhide");
    assert_eq!(stored.access.expose(), "ghp_pasted");
    assert_eq!(
        stored.expires_at, None,
        "a personal access token has no expiry hideGit can see, and none is invented"
    );
}

#[tokio::test]
async fn a_token_that_cannot_name_its_owner_is_neither_kept_nor_stored() {
    // Holding on to it would make every later call fail identically, with no
    // sign of why.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({ "message": "Bad credentials" })),
        )
        .mount(&server)
        .await;

    let store = Arc::new(MemoryStore::default());
    let github = signed_out(&server, store.clone());

    let outcome = github
        .authenticate(AuthFlow::Token(SecretString::new("ghp_wrong")))
        .await;

    assert!(matches!(
        outcome,
        Err(ForgeError::NotAuthenticated(ForgeId::GitHub))
    ));
    assert_eq!(store.load(PUBLIC_HOST).unwrap(), None, "nothing is stored");
}

#[tokio::test]
async fn the_device_flow_shows_a_code_before_it_starts_waiting_for_it() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "d-code",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 1,
        })))
        .mount(&server)
        .await;

    // Pending once, then issued: the ordinary shape of the flow, and the reason
    // a code has to be on screen before polling begins.
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": "authorization_pending",
            "error_description": "The authorization request is still pending",
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ghu_issued",
            "token_type": "bearer",
            "expires_in": 28_800,
            "refresh_token": "ghr_issued",
            "refresh_token_expires_in": 15_811_200,
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "viewer": { "login": "youhide", "name": null, "avatarUrl": null } }
        })))
        .mount(&server)
        .await;

    let store = Arc::new(MemoryStore::default());
    let github = signed_out(&server, store.clone());

    let seen = Arc::new(std::sync::Mutex::new(None));
    let recorder = seen.clone();

    let identity = github
        .authenticate(AuthFlow::Device(Box::new(move |code| {
            *recorder.lock().unwrap() = Some(code);
        })))
        .await
        .unwrap();

    let code = seen.lock().unwrap().clone().expect("a code was announced");
    assert_eq!(code.user_code, "WDJB-MJHT");
    assert_eq!(code.verification_uri, "https://github.com/login/device");

    assert_eq!(identity.login, "youhide");
    let stored = store.load(PUBLIC_HOST).unwrap().unwrap();
    assert_eq!(stored.access.expose(), "ghu_issued");
    assert!(
        stored.refresh.is_some(),
        "a GitHub App's user token expires, so the refresh token is kept with it"
    );
    assert!(stored.expires_at.is_some());
}

#[tokio::test]
async fn each_way_the_device_flow_can_end_is_reported_as_itself() {
    // Each of these is a different sentence to show somebody staring at a code
    // they just typed. "Authorisation failed" would throw away the useful part.
    for (error, expected) in [
        ("expired_token", DeviceFlowError::Expired),
        ("access_denied", DeviceFlowError::Denied),
        ("device_flow_disabled", DeviceFlowError::Disabled),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "d-code",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
                "interval": 1,
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "error": error })))
            .mount(&server)
            .await;

        let github = signed_out(&server, Arc::new(MemoryStore::default()));
        let outcome = github
            .authenticate(AuthFlow::Device(Box::new(|_| {})))
            .await;

        match outcome {
            Err(ForgeError::DeviceFlow(got)) => assert_eq!(got, expected, "for {error}"),
            other => panic!("expected {expected:?} for {error}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_session_resumes_from_the_keychain_without_asking_github_who_you_are() {
    // A restart should not cost a request, and it should not cost the user a
    // sign-in.
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::default());
    store
        .save(
            PUBLIC_HOST,
            &StoredToken::permanent("youhide", SecretString::new("ghp_stored")),
        )
        .unwrap();

    let identity = signed_out(&server, store)
        .resume()
        .await
        .unwrap()
        .expect("a stored session resumes");

    assert_eq!(identity.login, "youhide");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "resuming spends no request"
    );
}

#[tokio::test]
async fn a_first_run_resumes_to_nothing_rather_than_to_an_error() {
    let server = MockServer::start().await;
    let github = signed_out(&server, Arc::new(MemoryStore::default()));

    assert_eq!(github.resume().await.unwrap(), None);
}

#[tokio::test]
async fn an_expiring_token_is_refreshed_on_the_way_back_in() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(body_string_contains("refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ghu_fresh",
            "token_type": "bearer",
            "expires_in": 28_800,
            "refresh_token": "ghr_fresh",
        })))
        .mount(&server)
        .await;

    let store = Arc::new(MemoryStore::default());
    store
        .save(
            PUBLIC_HOST,
            &StoredToken {
                login: "youhide".to_owned(),
                access: SecretString::new("ghu_stale"),
                // Already past. The margin means even a token a minute from
                // expiry is refreshed rather than raced.
                expires_at: Some(crate::token::to_unix(
                    time::OffsetDateTime::now_utc() - time::Duration::minutes(5),
                )),
                refresh: Some(SecretString::new("ghr_stale")),
                refresh_expires_at: None,
            },
        )
        .unwrap();

    let identity = signed_out(&server, store.clone())
        .resume()
        .await
        .unwrap()
        .expect("a refreshable session resumes");

    assert_eq!(identity.login, "youhide");

    let stored = store.load(PUBLIC_HOST).unwrap().unwrap();
    assert_eq!(stored.access.expose(), "ghu_fresh");
    assert_eq!(
        stored.login, "youhide",
        "the login survives a refresh rather than costing a request to relearn"
    );
}

#[tokio::test]
async fn a_refresh_token_github_will_not_honour_ends_the_session() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "error": "bad_refresh_token" })),
        )
        .mount(&server)
        .await;

    let store = Arc::new(MemoryStore::default());
    store
        .save(
            PUBLIC_HOST,
            &StoredToken {
                login: "youhide".to_owned(),
                access: SecretString::new("ghu_stale"),
                expires_at: Some(crate::token::to_unix(
                    time::OffsetDateTime::now_utc() - time::Duration::minutes(5),
                )),
                refresh: Some(SecretString::new("ghr_revoked")),
                refresh_expires_at: None,
            },
        )
        .unwrap();

    assert!(matches!(
        signed_out(&server, store).resume().await,
        Err(ForgeError::NotAuthenticated(ForgeId::GitHub))
    ));
}

#[tokio::test]
async fn signing_out_forgets_the_token_here_and_in_the_keychain() {
    let server = viewer_server("youhide").await;
    let store = Arc::new(MemoryStore::default());
    let github = signed_out(&server, store.clone());

    github
        .authenticate(AuthFlow::Token(SecretString::new("ghp_pasted")))
        .await
        .unwrap();
    assert!(store.load(PUBLIC_HOST).unwrap().is_some());

    github.sign_out().await.unwrap();
    assert_eq!(store.load(PUBLIC_HOST).unwrap(), None);
    assert_eq!(github.resume().await.unwrap(), None);
}
