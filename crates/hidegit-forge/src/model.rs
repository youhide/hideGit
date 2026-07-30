//! The provider-neutral data model.
//!
//! A GitLab merge request becomes a [`PullRequest`] at the boundary, so
//! `hidegit-ui` never branches on provider. That translation is the whole point
//! of the trait; see `docs/adr/0003-forge-github-first.md`.

use std::collections::BTreeSet;
use std::fmt;

use hidegit_core::model::Remote;
use time::OffsetDateTime;

use crate::detect;

/// Which hosting provider a repository lives on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForgeId {
    GitHub,
}

impl fmt::Display for ForgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ForgeId::GitHub => f.write_str("GitHub"),
        }
    }
}

/// A repository on a forge: enough to address it in an API call.
///
/// `host` is carried rather than assumed because GitHub Enterprise lives on an
/// arbitrary domain, and it is what decides which API endpoint a request goes to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoRef {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    /// Reads a named remote's fetch URL.
    ///
    /// Takes the fetch URL rather than the push URL: a repository with a
    /// read-only mirror configured for fetch and a fork for push is still the
    /// same project on the forge, and the fetch URL is the one always present.
    pub fn from_remote(remote: &Remote) -> Option<Self> {
        detect::parse_remote_url(&remote.fetch_url)
    }
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Who you are, as the forge knows you.
///
/// `login` is the identity everything else is compared against — whether a pull
/// request is yours, whether a review request names you, and whether an event
/// was caused by you and therefore must not notify you about your own action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// Your relationship to a pull request.
///
/// A set rather than a single value: being both the author and an assignee is
/// ordinary, and the sidebar groups by the first one in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrRole {
    Author,
    Reviewer,
    Assignee,
}

impl PrRole {
    /// The heading this role gets in the sidebar.
    pub fn heading(self) -> &'static str {
        match self {
            PrRole::Author => "YOURS",
            PrRole::Reviewer => "AWAITING YOUR REVIEW",
            PrRole::Assignee => "ASSIGNED TO YOU",
        }
    }
}

/// Where a pull request's review stands overall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    /// The repository does not require a review on this branch, and nobody has
    /// left one. Distinct from `Required`, which is waiting on somebody.
    NotRequired,
    Required,
    Approved,
    ChangesRequested,
}

/// The rolled-up state of every check on the head commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    /// No checks are configured, which is not the same as checks that have not
    /// started — the sidebar shows nothing rather than a pending marker.
    None,
    Pending,
    Passing,
    Failing,
}

/// Whether the pull request would merge.
///
/// **Three variants, not two.** GitHub computes mergeability lazily and answers
/// `UNKNOWN` on the first query after a push, while a background job runs.
/// Folding that into "not conflicting" is how a false `PrConflicting`
/// notification gets sent, so [`MergeState::Unknown`] is carried through the
/// model and never produces an alert on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeState {
    Mergeable,
    Conflicting,
    Unknown,
}

/// One pull request, in the shape the sidebar lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    /// The forge's own web URL, kept rather than rebuilt: a redirect after a
    /// repository rename would make a URL assembled from `RepoRef` wrong.
    pub url: String,
    pub author: String,
    /// Branch names, head into base.
    pub head: String,
    pub base: String,
    pub draft: bool,
    pub updated: OffsetDateTime,
    pub roles: BTreeSet<PrRole>,
    pub review: ReviewState,
    pub checks: CheckState,
    pub merge: MergeState,
    /// Issue comments plus review comments. Carried as a count because that is
    /// all a "there is a new comment" transition needs, and fetching the
    /// comments themselves would cost a page per pull request.
    pub comments: u32,
}

impl PullRequest {
    /// The heading this pull request is listed under.
    ///
    /// `roles` is ordered, so the first is the strongest claim: a pull request
    /// you wrote is yours even when you are also assigned to it.
    pub fn primary_role(&self) -> Option<PrRole> {
        self.roles.iter().copied().next()
    }
}

/// What somebody said in a review.
///
/// Deliberately not [`ReviewState`]: that is a pull request's overall decision,
/// and a review that only comments does not change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub author: String,
    pub verdict: ReviewVerdict,
    pub submitted: OffsetDateTime,
}

/// A pull request, loaded for the detail pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestDetail {
    pub pr: PullRequest,
    pub body: String,
    pub reviews: Vec<Review>,
    pub commits: u32,
    pub changed_files: u32,
    pub additions: u32,
    pub deletions: u32,
}

/// A pull request to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPullRequest {
    pub title: String,
    pub body: String,
    pub head: String,
    pub base: String,
    pub draft: bool,
}

/// A page on the forge's website.
///
/// The trait is narrow on purpose — anything past listing, reading and creating
/// a pull request opens the browser instead of growing the API surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebTarget {
    Repository,
    PullRequest(u64),
    /// The compare page, prefilled to open a pull request from a branch.
    NewPullRequest {
        head: String,
        base: Option<String>,
    },
    /// Where the user installs or grants access to the hideGit app.
    Install,
}

/// What a provider needs to make the next poll conditional.
///
/// Opaque and provider-defined rather than an `ETag` string. The GitHub
/// implementation polls over GraphQL, which has no conditional requests, and
/// always returns [`PollCursor::none`] — but a REST-based forge would put an
/// `ETag` here, and keeping the shape is what lets it. See
/// `docs/adr/0006-poll-pull-requests-over-graphql.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PollCursor(pub(crate) Option<String>);

impl PollCursor {
    /// A cursor that makes no claim about what was seen before.
    pub fn none() -> Self {
        Self(None)
    }
}

/// How much of the API budget is left, and when it refills.
///
/// Not a detail a provider can hide: the poll scheduler widens its interval on
/// it, so it rides on every result rather than being asked for separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateBudget {
    pub limit: u32,
    pub remaining: u32,
    pub reset: OffsetDateTime,
}

impl RateBudget {
    /// Remaining budget as a fraction, for the 20% and 5% thresholds in
    /// `docs/ARCHITECTURE.md#polling`.
    ///
    /// A zero limit is reported as exhausted rather than dividing by it: an
    /// absent budget is not a reason to poll harder.
    pub fn fraction_remaining(&self) -> f32 {
        if self.limit == 0 {
            return 0.0;
        }
        self.remaining as f32 / self.limit as f32
    }
}

/// The result of one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollResult<T> {
    /// `None` means unchanged since the cursor was issued.
    pub data: Option<T>,
    pub cursor: PollCursor,
    pub budget: RateBudget,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(limit: u32, remaining: u32) -> RateBudget {
        RateBudget {
            limit,
            remaining,
            reset: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_fraction_of_the_budget_drives_the_backoff_thresholds() {
        assert_eq!(budget(5_000, 5_000).fraction_remaining(), 1.0);
        assert_eq!(budget(5_000, 1_000).fraction_remaining(), 0.2);
        assert_eq!(budget(5_000, 0).fraction_remaining(), 0.0);
    }

    #[test]
    fn a_budget_with_no_limit_reads_as_exhausted_rather_than_dividing_by_zero() {
        assert_eq!(budget(0, 0).fraction_remaining(), 0.0);
    }

    #[test]
    fn the_strongest_role_is_the_one_a_pull_request_is_listed_under() {
        let roles = BTreeSet::from([PrRole::Assignee, PrRole::Author, PrRole::Reviewer]);
        assert_eq!(roles.iter().copied().next(), Some(PrRole::Author));
    }
}
