//! The GraphQL documents, and the shapes they come back as.
//!
//! **Editing a query here is a rate-limit change, not only a schema change.**
//! GitHub prices a query by summing the `first`/`last` arguments across its
//! connections and dividing by 100, so raising a nested page size multiplies by
//! the number of pull requests on the page. The reasoning, and the arithmetic,
//! are in `docs/adr/0006-poll-pull-requests-over-graphql.md`.
//!
//! Every enum arrives as a `String` rather than a Rust enum on purpose. Serde
//! refuses an unknown variant, and a schema is a parsing surface that changes
//! without warning; mapping the string in [`super::translate`] is what lets an
//! unrecognised value cost one field instead of the whole poll.

use serde::Deserialize;

/// How many pull requests one poll reads.
///
/// A repository with more than this many open at once exists, and the extra are
/// simply not polled. Paging would multiply the cost of every poll to cover a
/// case where a sidebar list is already unreadable.
pub const PAGE: usize = 50;

/// How many reviewers, assignees and reviews are read per pull request.
///
/// Small because it is the multiplier in the cost formula: five each keeps the
/// query near 9 points, twenty each pushes it past 26. It is also enough to
/// render a row.
pub const NESTED: usize = 5;

/// One poll: the viewer, the repository's open pull requests, and the budget.
///
/// `viewer` rides along rather than being a separate `current_user` call
/// because roles are computed against the viewer's login, and a poll that
/// learned who you are from a *different* request could grade a pull request
/// against a stale identity.
pub const POLL: &str = r"
query($owner: String!, $name: String!, $page: Int!, $nested: Int!) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: $page, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        number
        title
        url
        isDraft
        updatedAt
        author { login }
        headRefName
        baseRefName
        mergeable
        reviewDecision
        comments { totalCount }
        reviewThreads { totalCount }
        reviewRequests(first: $nested) {
          nodes { requestedReviewer { __typename ... on User { login } } }
        }
        assignees(first: $nested) { nodes { login } }
        latestReviews(first: $nested) { nodes { author { login } state submittedAt } }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      }
    }
  }
  rateLimit { limit cost remaining resetAt }
}
";

/// One pull request, with everything the detail pane adds.
pub const DETAIL: &str = r"
query($owner: String!, $name: String!, $number: Int!, $nested: Int!) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      number
      title
      url
      isDraft
      updatedAt
      state
      body
      additions
      deletions
      changedFiles
      author { login }
      headRefName
      baseRefName
      mergeable
      reviewDecision
      comments { totalCount }
      reviewThreads { totalCount }
      reviewRequests(first: $nested) {
        nodes { requestedReviewer { __typename ... on User { login } } }
      }
      assignees(first: $nested) { nodes { login } }
      latestReviews(first: $nested) { nodes { author { login } state submittedAt } }
      commits(last: 1) { totalCount nodes { commit { statusCheckRollup { state } } } }
    }
  }
}
";

/// Who the token belongs to.
pub const VIEWER: &str = r"
query { viewer { login name avatarUrl } }
";

#[derive(Debug, Deserialize)]
pub struct PollData {
    pub viewer: Actor,
    /// `None` when the token cannot see the repository at all — which for a
    /// GitHub App means it is not installed there. That is a different state
    /// from having no open pull requests, and the two must not render alike.
    pub repository: Option<PollRepository>,
    #[serde(rename = "rateLimit")]
    pub rate_limit: Option<RateLimit>,
}

/// A detail load asks for no `rateLimit` block.
///
/// Unlike a poll it is user-initiated rather than scheduled, so nothing decides
/// anything from its budget — and the next poll reports what is left anyway.
#[derive(Debug, Deserialize)]
pub struct DetailData {
    pub viewer: Actor,
    pub repository: Option<DetailRepository>,
}

#[derive(Debug, Deserialize)]
pub struct ViewerData {
    pub viewer: Actor,
}

#[derive(Debug, Deserialize)]
pub struct PollRepository {
    #[serde(rename = "pullRequests")]
    pub pull_requests: Connection<Node>,
}

#[derive(Debug, Deserialize)]
pub struct DetailRepository {
    #[serde(rename = "pullRequest")]
    pub pull_request: Option<Node>,
}

/// A GraphQL connection.
///
/// `nodes` is optional and its elements are nullable because the schema says
/// so: a node the viewer may not read comes back as `null` beside the ones it
/// may, rather than failing the whole list.
#[derive(Debug, Default, Deserialize)]
pub struct Connection<T> {
    #[serde(default = "Vec::new")]
    pub nodes: Vec<Option<T>>,
    #[serde(rename = "totalCount")]
    pub total_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct Actor {
    pub login: String,
    pub name: Option<String>,
    #[serde(rename = "avatarUrl")]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Node {
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    /// `None` for a pull request whose author deleted their account. It still
    /// appears in the list, so it still has to render.
    pub author: Option<Actor>,
    #[serde(rename = "headRefName")]
    pub head_ref_name: String,
    #[serde(rename = "baseRefName")]
    pub base_ref_name: String,
    pub mergeable: Option<String>,
    #[serde(rename = "reviewDecision")]
    pub review_decision: Option<String>,
    pub comments: Option<Connection<serde_json::Value>>,
    #[serde(rename = "reviewThreads")]
    pub review_threads: Option<Connection<serde_json::Value>>,
    #[serde(rename = "reviewRequests")]
    pub review_requests: Option<Connection<ReviewRequest>>,
    pub assignees: Option<Connection<Actor>>,
    #[serde(rename = "latestReviews")]
    pub latest_reviews: Option<Connection<ReviewNode>>,
    pub commits: Option<Connection<CommitNode>>,

    // Detail only.
    pub state: Option<String>,
    pub body: Option<String>,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    #[serde(rename = "changedFiles")]
    pub changed_files: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ReviewRequest {
    #[serde(rename = "requestedReviewer")]
    pub requested_reviewer: Option<Reviewer>,
}

/// A requested reviewer, which the schema makes a union of user, team and
/// mannequin.
///
/// Only the `... on User` fragment is selected, so `login` is `None` for a team
/// — which is the answer that matters: a team review request is not personally
/// awaiting *your* review, and reading a team name as a login would put every
/// pull request in the repository under that heading.
#[derive(Debug, Deserialize)]
pub struct Reviewer {
    pub login: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReviewNode {
    pub author: Option<Actor>,
    pub state: Option<String>,
    #[serde(rename = "submittedAt")]
    pub submitted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CommitNode {
    pub commit: Option<Commit>,
}

#[derive(Debug, Deserialize)]
pub struct Commit {
    /// `None` when no checks are configured, which is not the same as checks
    /// that have not started.
    #[serde(rename = "statusCheckRollup")]
    pub status_check_rollup: Option<Rollup>,
}

#[derive(Debug, Deserialize)]
pub struct Rollup {
    pub state: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RateLimit {
    pub limit: u32,
    pub cost: u32,
    pub remaining: u32,
    #[serde(rename = "resetAt")]
    pub reset_at: String,
}
