//! HTTP is mocked. No test here touches the network or needs a real token.

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::model::{CheckState, MergeState, PrRole, ReviewState};

/// A client pointed at a mock server instead of GitHub.
async fn client(server: &MockServer) -> GitHub {
    let crab = Octocrab::builder()
        .base_uri(server.uri())
        .expect("a mock server URI is a URI")
        .personal_token("not-a-real-token".to_owned())
        .build()
        .expect("the client builds");

    GitHub::new(PUBLIC_HOST, crab)
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
            assert!(install_url.contains("installations/new"), "{install_url}");
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
}

#[tokio::test]
async fn an_enterprise_host_produces_enterprise_urls() {
    let server = MockServer::start().await;
    let crab = Octocrab::builder()
        .base_uri(server.uri())
        .unwrap()
        .build()
        .unwrap();
    let github = GitHub::new("github.example.com", crab);

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
