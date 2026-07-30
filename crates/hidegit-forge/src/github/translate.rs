//! Turning GitHub's schema into the provider-neutral model.
//!
//! This is the boundary ADR-0003 promised: past it, nothing knows which forge
//! the data came from. It is also where a schema change is absorbed. Every
//! mapping here **fails soft** — an enum value GitHub adds tomorrow costs one
//! field, never the poll — because the alternative is that a deploy on their
//! side stops notifications on ours.

use std::collections::BTreeSet;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::query;
use crate::model::{
    CheckState, Identity, MergeState, PrRole, PullRequest, PullRequestDetail, RateBudget, Review,
    ReviewState, ReviewVerdict,
};

/// Parses a GitHub timestamp, falling back to the epoch.
///
/// A timestamp is used for sorting and for "2 minutes ago". Neither is worth
/// dropping a pull request over, so an unparseable one sorts last and is logged
/// rather than failing the row.
fn timestamp(raw: &str) -> OffsetDateTime {
    OffsetDateTime::parse(raw, &Rfc3339).unwrap_or_else(|error| {
        tracing::warn!(raw, %error, "unparseable timestamp; falling back to the epoch");
        OffsetDateTime::UNIX_EPOCH
    })
}

/// `MERGEABLE` / `CONFLICTING` / `UNKNOWN`.
///
/// Anything else — including the absent field — is [`MergeState::Unknown`], and
/// `Unknown` never produces a `PrConflicting` alert. GitHub computes this
/// lazily, so `UNKNOWN` is the ordinary answer for the first poll after a push
/// rather than an error.
fn merge_state(raw: Option<&str>) -> MergeState {
    match raw {
        Some("MERGEABLE") => MergeState::Mergeable,
        Some("CONFLICTING") => MergeState::Conflicting,
        Some("UNKNOWN") | None => MergeState::Unknown,
        Some(other) => {
            tracing::debug!(
                other,
                "unrecognised mergeable state; treating it as unknown"
            );
            MergeState::Unknown
        }
    }
}

/// `reviewDecision`, which is null when the branch requires no review.
fn review_state(raw: Option<&str>) -> ReviewState {
    match raw {
        Some("APPROVED") => ReviewState::Approved,
        Some("CHANGES_REQUESTED") => ReviewState::ChangesRequested,
        Some("REVIEW_REQUIRED") => ReviewState::Required,
        None => ReviewState::NotRequired,
        Some(other) => {
            tracing::debug!(
                other,
                "unrecognised review decision; treating it as pending"
            );
            ReviewState::Required
        }
    }
}

/// The rolled-up check state on the head commit.
///
/// `ERROR` and `FAILURE` are both failing: the distinction is whether a check
/// crashed or reported a failure, and neither is something to merge on.
/// `EXPECTED` — a check that has been declared but has not reported — is
/// pending, because that is what it looks like to somebody waiting.
fn check_state(raw: Option<&str>) -> CheckState {
    match raw {
        Some("SUCCESS") => CheckState::Passing,
        Some("FAILURE" | "ERROR") => CheckState::Failing,
        Some("PENDING" | "EXPECTED") => CheckState::Pending,
        None => CheckState::None,
        Some(other) => {
            tracing::debug!(other, "unrecognised check state; treating it as pending");
            CheckState::Pending
        }
    }
}

fn review_verdict(raw: Option<&str>) -> Option<ReviewVerdict> {
    match raw {
        Some("APPROVED") => Some(ReviewVerdict::Approved),
        Some("CHANGES_REQUESTED") => Some(ReviewVerdict::ChangesRequested),
        Some("COMMENTED") => Some(ReviewVerdict::Commented),
        Some("DISMISSED") => Some(ReviewVerdict::Dismissed),
        // `PENDING` is a review somebody is still drafting and has not
        // submitted. It is genuinely not a review yet, so it is dropped rather
        // than shown as one.
        _ => None,
    }
}

/// Which of `viewer`'s relationships to a pull request apply.
///
/// A team review request does not count: the query reads a `login` only for a
/// user, and "your team was asked" is not "you were asked". Erring the other
/// way would put every pull request in the repository under
/// *awaiting your review*.
fn roles(node: &query::Node, viewer: &str) -> BTreeSet<PrRole> {
    let mut roles = BTreeSet::new();

    if node.author.as_ref().is_some_and(|a| a.login == viewer) {
        roles.insert(PrRole::Author);
    }

    let requested = node
        .review_requests
        .iter()
        .flat_map(|c| c.nodes.iter().flatten())
        .filter_map(|request| request.requested_reviewer.as_ref())
        .filter_map(|reviewer| reviewer.login.as_deref())
        .any(|login| login == viewer);

    // A submitted review also makes you a reviewer. Without this, approving
    // something removes it from your list the moment you act on it, and there
    // is then nowhere to see that checks later failed on it.
    let reviewed = node
        .latest_reviews
        .iter()
        .flat_map(|c| c.nodes.iter().flatten())
        .filter_map(|review| review.author.as_ref())
        .any(|author| author.login == viewer);

    if requested || reviewed {
        roles.insert(PrRole::Reviewer);
    }

    let assigned = node
        .assignees
        .iter()
        .flat_map(|c| c.nodes.iter().flatten())
        .any(|actor| actor.login == viewer);

    if assigned {
        roles.insert(PrRole::Assignee);
    }

    roles
}

/// Issue comments plus review threads.
///
/// **A reply inside an existing review thread changes neither count**, so it
/// does not fire `PrCommented`. Counting replies would mean reading every
/// thread on every pull request on every poll, which is the N+1 the whole
/// GraphQL design exists to avoid. The gap is real and stated here rather than
/// discovered.
fn comment_count(node: &query::Node) -> u32 {
    let count = |c: &Option<query::Connection<serde_json::Value>>| {
        c.as_ref().and_then(|c| c.total_count).unwrap_or(0)
    };
    count(&node.comments).saturating_add(count(&node.review_threads))
}

pub fn pull_request(node: &query::Node, viewer: &str) -> PullRequest {
    let rollup = node
        .commits
        .as_ref()
        .and_then(|c| c.nodes.first())
        .and_then(Option::as_ref)
        .and_then(|node| node.commit.as_ref())
        .and_then(|commit| commit.status_check_rollup.as_ref())
        .and_then(|rollup| rollup.state.as_deref());

    PullRequest {
        number: node.number,
        title: node.title.clone(),
        url: node.url.clone(),
        // A pull request whose author deleted their account keeps its place in
        // the list; GitHub's own web UI calls them "ghost".
        author: node
            .author
            .as_ref()
            .map_or_else(|| "ghost".to_owned(), |a| a.login.clone()),
        head: node.head_ref_name.clone(),
        base: node.base_ref_name.clone(),
        draft: node.is_draft,
        updated: timestamp(&node.updated_at),
        roles: roles(node, viewer),
        review: review_state(node.review_decision.as_deref()),
        checks: check_state(rollup),
        merge: merge_state(node.mergeable.as_deref()),
        comments: comment_count(node),
    }
}

pub fn detail(node: &query::Node, viewer: &str) -> PullRequestDetail {
    let reviews = node
        .latest_reviews
        .iter()
        .flat_map(|c| c.nodes.iter().flatten())
        .filter_map(|review| {
            Some(Review {
                author: review.author.as_ref()?.login.clone(),
                verdict: review_verdict(review.state.as_deref())?,
                submitted: review
                    .submitted_at
                    .as_deref()
                    .map_or(OffsetDateTime::UNIX_EPOCH, timestamp),
            })
        })
        .collect();

    PullRequestDetail {
        pr: pull_request(node, viewer),
        body: node.body.clone().unwrap_or_default(),
        reviews,
        commits: node
            .commits
            .as_ref()
            .and_then(|c| c.total_count)
            .unwrap_or(0),
        changed_files: node.changed_files.unwrap_or(0),
        additions: node.additions.unwrap_or(0),
        deletions: node.deletions.unwrap_or(0),
    }
}

pub fn identity(actor: &query::Actor) -> Identity {
    Identity {
        login: actor.login.clone(),
        name: actor.name.clone(),
        avatar_url: actor.avatar_url.clone(),
    }
}

/// The budget, or a conservative stand-in when the response omitted it.
///
/// A missing `rateLimit` block is reported as exhausted rather than unlimited.
/// The scheduler widens its interval on a low budget, so guessing high is the
/// direction that burns through a real one.
pub fn budget(limit: Option<&query::RateLimit>) -> RateBudget {
    match limit {
        Some(limit) => {
            // Logged because ADR-0006 makes the query's page sizes a rate-limit
            // decision, and what a poll actually costs is otherwise invisible
            // at the call site — the one place somebody editing the query would
            // look.
            tracing::debug!(
                cost = limit.cost,
                remaining = limit.remaining,
                "polled GitHub"
            );

            RateBudget {
                limit: limit.limit,
                remaining: limit.remaining,
                reset: timestamp(&limit.reset_at),
            }
        }
        None => {
            tracing::debug!("response carried no rateLimit block; assuming none is left");
            RateBudget {
                limit: 0,
                remaining: 0,
                reset: OffsetDateTime::UNIX_EPOCH,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(login: &str) -> query::Actor {
        query::Actor {
            login: login.to_owned(),
            name: None,
            avatar_url: None,
        }
    }

    fn connection<T>(nodes: Vec<T>) -> query::Connection<T> {
        query::Connection {
            nodes: nodes.into_iter().map(Some).collect(),
            total_count: None,
        }
    }

    fn node() -> query::Node {
        query::Node {
            number: 47,
            title: "feat: hunk staging".to_owned(),
            url: "https://github.com/youhide/hideGit/pull/47".to_owned(),
            is_draft: false,
            updated_at: "2026-07-30T12:00:00Z".to_owned(),
            author: Some(actor("youhide")),
            head_ref_name: "feat/graph".to_owned(),
            base_ref_name: "main".to_owned(),
            mergeable: Some("MERGEABLE".to_owned()),
            review_decision: Some("REVIEW_REQUIRED".to_owned()),
            comments: None,
            review_threads: None,
            review_requests: None,
            assignees: None,
            latest_reviews: None,
            commits: None,
            body: None,
            additions: None,
            deletions: None,
            changed_files: None,
        }
    }

    #[test]
    fn an_unknown_mergeable_state_is_never_read_as_not_conflicting() {
        // The whole reason MergeState has three variants. GitHub answers
        // UNKNOWN while it computes, and a false PrConflicting is worse than a
        // late one.
        assert_eq!(merge_state(Some("UNKNOWN")), MergeState::Unknown);
        assert_eq!(merge_state(None), MergeState::Unknown);
        assert_eq!(merge_state(Some("MERGEABLE")), MergeState::Mergeable);
        assert_eq!(merge_state(Some("CONFLICTING")), MergeState::Conflicting);
    }

    #[test]
    fn a_value_the_schema_gains_tomorrow_costs_one_field_and_not_the_poll() {
        assert_eq!(merge_state(Some("SOMETHING_NEW")), MergeState::Unknown);
        assert_eq!(check_state(Some("SOMETHING_NEW")), CheckState::Pending);
        assert_eq!(review_state(Some("SOMETHING_NEW")), ReviewState::Required);
        assert_eq!(review_verdict(Some("SOMETHING_NEW")), None);
    }

    #[test]
    fn no_checks_configured_is_not_the_same_as_checks_that_have_not_run() {
        assert_eq!(check_state(None), CheckState::None);
        assert_eq!(check_state(Some("EXPECTED")), CheckState::Pending);
        assert_eq!(check_state(Some("PENDING")), CheckState::Pending);
    }

    #[test]
    fn an_errored_check_is_a_failing_check() {
        assert_eq!(check_state(Some("ERROR")), CheckState::Failing);
        assert_eq!(check_state(Some("FAILURE")), CheckState::Failing);
    }

    #[test]
    fn a_branch_needing_no_review_is_distinct_from_one_waiting_on_somebody() {
        assert_eq!(review_state(None), ReviewState::NotRequired);
        assert_eq!(review_state(Some("REVIEW_REQUIRED")), ReviewState::Required);
    }

    #[test]
    fn every_relationship_you_have_to_a_pull_request_is_recorded() {
        let mut node = node();
        node.assignees = Some(connection(vec![actor("youhide")]));

        let roles = roles(&node, "youhide");
        assert!(roles.contains(&PrRole::Author));
        assert!(roles.contains(&PrRole::Assignee));
        assert_eq!(
            roles.iter().next(),
            Some(&PrRole::Author),
            "authorship is the strongest claim, so it decides the heading"
        );
    }

    #[test]
    fn a_team_review_request_is_not_a_request_of_you() {
        // Reading a team name as a login would put every pull request in the
        // repository under "awaiting your review".
        let mut node = node();
        node.author = Some(actor("someone-else"));
        node.review_requests = Some(connection(vec![query::ReviewRequest {
            requested_reviewer: Some(query::Reviewer { login: None }),
        }]));

        assert!(roles(&node, "reviewers").is_empty());
    }

    #[test]
    fn having_already_reviewed_keeps_it_in_your_list() {
        // Otherwise approving something drops it the instant you act, and there
        // is nowhere left to see that checks failed on it afterwards.
        let mut node = node();
        node.author = Some(actor("someone-else"));
        node.latest_reviews = Some(connection(vec![query::ReviewNode {
            author: Some(actor("youhide")),
            state: Some("APPROVED".to_owned()),
            submitted_at: Some("2026-07-30T12:00:00Z".to_owned()),
        }]));

        assert!(roles(&node, "youhide").contains(&PrRole::Reviewer));
    }

    #[test]
    fn a_pull_request_whose_author_deleted_their_account_still_renders() {
        let mut node = node();
        node.author = None;

        let pr = pull_request(&node, "youhide");
        assert_eq!(pr.author, "ghost");
        assert!(pr.roles.is_empty());
    }

    #[test]
    fn a_draft_review_is_not_a_review() {
        assert_eq!(review_verdict(Some("PENDING")), None);

        let mut node = node();
        node.latest_reviews = Some(connection(vec![query::ReviewNode {
            author: Some(actor("someone")),
            state: Some("PENDING".to_owned()),
            submitted_at: None,
        }]));

        assert!(detail(&node, "youhide").reviews.is_empty());
    }

    #[test]
    fn comments_and_review_threads_are_counted_together() {
        let mut node = node();
        node.comments = Some(query::Connection {
            nodes: Vec::new(),
            total_count: Some(3),
        });
        node.review_threads = Some(query::Connection {
            nodes: Vec::new(),
            total_count: Some(2),
        });

        assert_eq!(comment_count(&node), 5);
    }

    #[test]
    fn an_unparseable_timestamp_does_not_cost_the_pull_request() {
        let mut node = node();
        node.updated_at = "not a date".to_owned();

        assert_eq!(
            pull_request(&node, "youhide").updated,
            OffsetDateTime::UNIX_EPOCH
        );
    }

    #[test]
    fn an_absent_budget_reads_as_exhausted_rather_than_unlimited() {
        // Guessing high is the direction that burns through a real budget.
        let budget = budget(None);
        assert_eq!(budget.remaining, 0);
        assert_eq!(budget.fraction_remaining(), 0.0);
    }
}
