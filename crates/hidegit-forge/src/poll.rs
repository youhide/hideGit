//! When to poll, and what changed since the last one.
//!
//! Both halves live here rather than in `hidegit-ui` because neither needs a
//! window: an interval is arithmetic and a transition is a comparison, and both
//! are exactly the kind of thing that is painful to test through a toolkit. The
//! `Subscription` that drives them is in `hidegit-ui`, the same split
//! `hidegit-core`'s filesystem watcher already uses.

use std::collections::HashMap;
use std::time::Duration;

use time::OffsetDateTime;

use crate::model::{CheckState, MergeState, PrRole, PullRequest, RateBudget, ReviewState};

/// What the application is doing, which is what decides how often it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Activity {
    /// Focused, with the pull request panel on screen. The only case worth a
    /// one-minute interval, because it is the only one where somebody is
    /// looking at the answer.
    Foreground,
    #[default]
    Normal,
    Background,
}

impl Activity {
    fn interval(self) -> Duration {
        match self {
            Activity::Foreground => Duration::from_secs(60),
            Activity::Normal => Duration::from_secs(5 * 60),
            Activity::Background => Duration::from_secs(15 * 60),
        }
    }
}

/// Below this fraction of the budget, the interval widens.
const WIDEN_BELOW: f32 = 0.20;
/// Below this, polling stops until the budget resets.
const STOP_BELOW: f32 = 0.05;

/// The first backoff after a failure, and the ceiling it grows to.
const BACKOFF_FIRST: Duration = Duration::from_secs(30);
const BACKOFF_CEILING: Duration = Duration::from_secs(30 * 60);

/// What the scheduler knows.
#[derive(Debug, Default, Clone)]
pub struct Schedule {
    /// Consecutive failures. Reset by any success.
    failures: u32,
    budget: Option<RateBudget>,
}

/// What to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Wait this long, then poll.
    After(Duration),
    /// Do not poll at all until the budget resets. The UI says so rather than
    /// quietly going silent.
    Exhausted { until: OffsetDateTime },
}

impl Schedule {
    /// Records a poll that worked.
    pub fn succeeded(&mut self, budget: RateBudget) {
        self.failures = 0;
        self.budget = Some(budget);
    }

    /// Records one that did not.
    ///
    /// The budget is left alone: a failure that never reached GitHub says
    /// nothing about how much of it is left, and forgetting it would restore a
    /// short interval on exactly the run that is having trouble.
    pub fn failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    pub fn budget(&self) -> Option<&RateBudget> {
        self.budget.as_ref()
    }

    /// How long until the next poll.
    ///
    /// `jitter` is a fraction in `0.0..1.0`, supplied by the caller rather than
    /// generated here so the result stays a pure function — it is what keeps
    /// several repositories from retrying in lockstep after a network outage.
    pub fn next(&self, activity: Activity, jitter: f32) -> Next {
        if let Some(budget) = &self.budget {
            let left = budget.fraction_remaining();
            if left < STOP_BELOW {
                return Next::Exhausted {
                    until: budget.reset,
                };
            }
            if left < WIDEN_BELOW {
                // Four times the interval rather than a fixed long one: the
                // shape of the slowdown should still reflect whether anybody is
                // looking.
                return Next::After(activity.interval() * 4);
            }
        }

        if self.failures == 0 {
            return Next::After(activity.interval());
        }

        // Exponential from 30s to a 30-minute ceiling, plus up to 25% jitter.
        let doubled = BACKOFF_FIRST
            .checked_mul(1u32 << self.failures.min(16).saturating_sub(1))
            .unwrap_or(BACKOFF_CEILING)
            .min(BACKOFF_CEILING);

        let spread = doubled.mul_f32(0.25 * jitter.clamp(0.0, 1.0));
        Next::After(doubled + spread)
    }
}

/// Something worth telling the user about.
///
/// Names match `docs/UI_SPEC.md#pr-panel`, which is also where the defaults
/// live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AlertEvent {
    ReviewRequested,
    ReviewSubmitted,
    PrCommented,
    ChecksFailed,
    ChecksPassed,
    PrConflicting,
    PrMerged,
    PrClosed,
}

impl AlertEvent {
    /// What the notification says, given the pull request it is about.
    pub fn summary(self, number: u64) -> String {
        match self {
            AlertEvent::ReviewRequested => format!("Your review is requested on #{number}"),
            AlertEvent::ReviewSubmitted => format!("#{number} was reviewed"),
            AlertEvent::PrCommented => format!("New comment on #{number}"),
            AlertEvent::ChecksFailed => format!("Checks failed on #{number}"),
            AlertEvent::ChecksPassed => format!("Checks passed on #{number}"),
            AlertEvent::PrConflicting => format!("#{number} now conflicts with its base"),
            AlertEvent::PrMerged => format!("#{number} was merged"),
            AlertEvent::PrClosed => format!("#{number} was closed"),
        }
    }
}

/// One thing to notify about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub event: AlertEvent,
    pub number: u64,
    pub title: String,
    pub url: String,
}

/// What was last seen about one pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    review: ReviewState,
    checks: CheckState,
    merge: MergeState,
    comments: u32,
    /// Whether you wrote it, which decides whether its ending is your business.
    mine: bool,
    /// Whether you were being asked to review it.
    requested: bool,
    title: String,
    url: String,
}

impl Snapshot {
    fn of(pr: &PullRequest) -> Self {
        Self {
            review: pr.review,
            checks: pr.checks,
            merge: pr.merge,
            comments: pr.comments,
            mine: pr.roles.contains(&PrRole::Author),
            requested: pr.roles.contains(&PrRole::Reviewer),
            title: pr.title.clone(),
            url: pr.url.clone(),
        }
    }
}

/// Turns a sequence of polls into a sequence of events.
#[derive(Debug, Default)]
pub struct Watcher {
    seen: HashMap<u64, Snapshot>,
    /// Whether a baseline has been established.
    ///
    /// **The first poll after startup is silent.** Without this, launching the
    /// application produces a notification for every pull request that already
    /// needed your attention — which is every one of them, since none of it has
    /// been seen before.
    primed: bool,
}

/// What one poll produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Observed {
    pub alerts: Vec<Alert>,
    /// Pull requests **you wrote** that are no longer open.
    ///
    /// The poll asks only for open ones, so an ending shows up as an absence
    /// and the absence cannot say whether it was a merge or a close. Those two
    /// are different events, so each number here needs one more request to find
    /// out — which is worth it because it happens a handful of times a day, not
    /// on every poll.
    pub vanished: Vec<u64>,
}

impl Watcher {
    /// Compares a poll against the last one.
    pub fn observe(&mut self, prs: &[PullRequest]) -> Observed {
        let fresh: HashMap<u64, Snapshot> =
            prs.iter().map(|pr| (pr.number, Snapshot::of(pr))).collect();

        if !self.primed {
            self.primed = true;
            self.seen = fresh;
            return Observed::default();
        }

        let mut observed = Observed::default();

        for pr in prs {
            let now = Snapshot::of(pr);
            let Some(before) = self.seen.get(&pr.number) else {
                // Newly visible. The only thing worth saying about a pull
                // request hideGit has never seen is that you are being asked to
                // review it — everything else about it is not *news*, it is
                // just its state.
                if now.requested && !now.mine {
                    observed.alerts.push(alert(AlertEvent::ReviewRequested, pr));
                }
                continue;
            };

            let mut push = |event| observed.alerts.push(alert(event, pr));

            if !before.requested && now.requested && !now.mine {
                push(AlertEvent::ReviewRequested);
            }

            // A decision, not a review count: an approval that replaces a
            // previous approval is not news.
            if now.mine && before.review != now.review {
                match now.review {
                    ReviewState::Approved | ReviewState::ChangesRequested => {
                        push(AlertEvent::ReviewSubmitted);
                    }
                    _ => {}
                }
            }

            if now.comments > before.comments {
                push(AlertEvent::PrCommented);
            }

            // Transitions only. A pull request that was already failing when
            // hideGit started, and is still failing, is not an event.
            if before.checks != now.checks {
                match now.checks {
                    CheckState::Failing => push(AlertEvent::ChecksFailed),
                    CheckState::Passing => push(AlertEvent::ChecksPassed),
                    _ => {}
                }
            }

            // Only a *known* mergeable becoming a *known* conflicting. GitHub
            // answers UNKNOWN while it recomputes after every push, so
            // Unknown → Conflicting is the ordinary shape of "it finished
            // checking" and firing on it would alert on every push.
            if before.merge == MergeState::Mergeable && now.merge == MergeState::Conflicting {
                push(AlertEvent::PrConflicting);
            }
        }

        // Yours, gone from a list of open pull requests.
        for (number, before) in &self.seen {
            if before.mine && !fresh.contains_key(number) {
                observed.vanished.push(*number);
            }
        }
        observed.vanished.sort_unstable();

        self.seen = fresh;
        observed
    }

    /// Forgets everything, so the next poll re-establishes a baseline silently.
    ///
    /// Used when the session changes: signing in as somebody else makes every
    /// role different, and the difference is not news about the pull requests.
    pub fn reset(&mut self) {
        self.seen.clear();
        self.primed = false;
    }
}

fn alert(event: AlertEvent, pr: &PullRequest) -> Alert {
    Alert {
        event,
        number: pr.number,
        title: pr.title.clone(),
        url: pr.url.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn budget(limit: u32, remaining: u32) -> RateBudget {
        RateBudget {
            limit,
            remaining,
            reset: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn pr(number: u64, roles: &[PrRole]) -> PullRequest {
        PullRequest {
            number,
            title: "feat: something".to_owned(),
            url: format!("https://github.com/youhide/hideGit/pull/{number}"),
            author: "youhide".to_owned(),
            head: "feat/x".to_owned(),
            base: "main".to_owned(),
            draft: false,
            updated: OffsetDateTime::UNIX_EPOCH,
            roles: roles.iter().copied().collect::<BTreeSet<_>>(),
            review: ReviewState::Required,
            checks: CheckState::Pending,
            merge: MergeState::Unknown,
            comments: 0,
        }
    }

    fn events(observed: &Observed) -> Vec<AlertEvent> {
        observed.alerts.iter().map(|a| a.event).collect()
    }

    // ---- the schedule ----------------------------------------------------

    #[test]
    fn how_often_it_asks_follows_whether_anybody_is_looking() {
        let schedule = Schedule::default();

        assert_eq!(
            schedule.next(Activity::Foreground, 0.0),
            Next::After(Duration::from_secs(60))
        );
        assert_eq!(
            schedule.next(Activity::Normal, 0.0),
            Next::After(Duration::from_secs(300))
        );
        assert_eq!(
            schedule.next(Activity::Background, 0.0),
            Next::After(Duration::from_secs(900))
        );
    }

    #[test]
    fn a_thin_budget_widens_the_interval_and_an_empty_one_stops() {
        let mut schedule = Schedule::default();

        schedule.succeeded(budget(5_000, 500)); // 10%
        assert_eq!(
            schedule.next(Activity::Foreground, 0.0),
            Next::After(Duration::from_secs(240))
        );

        schedule.succeeded(budget(5_000, 100)); // 2%
        assert!(matches!(
            schedule.next(Activity::Foreground, 0.0),
            Next::Exhausted { .. }
        ));
    }

    #[test]
    fn failures_back_off_exponentially_and_stop_growing_at_half_an_hour() {
        let mut schedule = Schedule::default();

        schedule.failed();
        assert_eq!(
            schedule.next(Activity::Normal, 0.0),
            Next::After(Duration::from_secs(30))
        );

        schedule.failed();
        assert_eq!(
            schedule.next(Activity::Normal, 0.0),
            Next::After(Duration::from_secs(60))
        );

        for _ in 0..20 {
            schedule.failed();
        }
        assert_eq!(
            schedule.next(Activity::Normal, 0.0),
            Next::After(BACKOFF_CEILING),
            "and it does not overflow on the way there"
        );
    }

    #[test]
    fn jitter_keeps_several_repositories_from_retrying_in_lockstep() {
        let mut schedule = Schedule::default();
        schedule.failed();

        let Next::After(none) = schedule.next(Activity::Normal, 0.0) else {
            panic!("expected a delay")
        };
        let Next::After(full) = schedule.next(Activity::Normal, 1.0) else {
            panic!("expected a delay")
        };

        assert!(full > none);
        assert!(full <= none.mul_f32(1.25) + Duration::from_millis(1));
    }

    #[test]
    fn a_success_clears_the_backoff() {
        let mut schedule = Schedule::default();
        schedule.failed();
        schedule.failed();
        schedule.succeeded(budget(5_000, 5_000));

        assert_eq!(
            schedule.next(Activity::Normal, 0.0),
            Next::After(Duration::from_secs(300))
        );
    }

    #[test]
    fn a_failure_does_not_forget_what_the_budget_was() {
        // A request that never reached GitHub says nothing about how much
        // budget is left, and forgetting it would restore a short interval on
        // exactly the run that is having trouble.
        let mut schedule = Schedule::default();
        schedule.succeeded(budget(5_000, 100));
        schedule.failed();

        assert!(matches!(
            schedule.next(Activity::Normal, 0.0),
            Next::Exhausted { .. }
        ));
    }

    // ---- transitions -----------------------------------------------------

    #[test]
    fn the_first_poll_after_startup_is_silent() {
        // Otherwise launching the application notifies about every pull request
        // that already needed attention, which is all of them.
        let mut watcher = Watcher::default();

        let mut needs_review = pr(47, &[PrRole::Reviewer]);
        needs_review.checks = CheckState::Failing;

        assert_eq!(watcher.observe(&[needs_review]), Observed::default());
    }

    #[test]
    fn a_review_request_that_arrives_after_the_baseline_notifies() {
        let mut watcher = Watcher::default();
        watcher.observe(&[pr(47, &[])]);

        let observed = watcher.observe(&[pr(47, &[PrRole::Reviewer])]);
        assert_eq!(events(&observed), vec![AlertEvent::ReviewRequested]);
    }

    #[test]
    fn a_pull_request_you_have_never_seen_only_notifies_if_it_wants_you() {
        // Everything else about a newly visible pull request is its state, not
        // news — and treating state as news is what makes an alert stream
        // unreadable.
        let mut watcher = Watcher::default();
        watcher.observe(&[]);

        let mut failing = pr(45, &[]);
        failing.checks = CheckState::Failing;

        let observed = watcher.observe(&[failing, pr(47, &[PrRole::Reviewer])]);
        assert_eq!(events(&observed), vec![AlertEvent::ReviewRequested]);
    }

    #[test]
    fn checks_notify_on_the_transition_and_not_on_the_state() {
        let mut watcher = Watcher::default();
        let base = pr(47, &[PrRole::Author]);
        watcher.observe(std::slice::from_ref(&base));

        let mut failing = base.clone();
        failing.checks = CheckState::Failing;
        assert_eq!(
            events(&watcher.observe(&[failing.clone()])),
            vec![AlertEvent::ChecksFailed]
        );

        // Still failing on the next poll, and still failing on the one after.
        assert!(watcher.observe(&[failing.clone()]).alerts.is_empty());
        assert!(watcher.observe(&[failing]).alerts.is_empty());
    }

    #[test]
    fn a_pull_request_still_computing_never_reports_a_conflict() {
        // GitHub answers UNKNOWN while it recomputes after every push, so
        // Unknown → Conflicting is what "it finished checking" looks like.
        // Firing on it would alert on every push.
        let mut watcher = Watcher::default();
        let base = pr(47, &[PrRole::Author]);
        watcher.observe(std::slice::from_ref(&base));

        let mut conflicting = base.clone();
        conflicting.merge = MergeState::Conflicting;
        assert!(
            watcher.observe(&[conflicting.clone()]).alerts.is_empty(),
            "Unknown → Conflicting is not an event"
        );

        // But once it is known to merge cleanly, becoming conflicted is.
        let mut mergeable = base;
        mergeable.merge = MergeState::Mergeable;
        watcher.observe(&[mergeable]);

        assert_eq!(
            events(&watcher.observe(&[conflicting])),
            vec![AlertEvent::PrConflicting]
        );
    }

    #[test]
    fn a_review_on_somebody_elses_pull_request_is_not_your_business() {
        let mut watcher = Watcher::default();
        let base = pr(45, &[]);
        watcher.observe(std::slice::from_ref(&base));

        let mut approved = base;
        approved.review = ReviewState::Approved;

        assert!(watcher.observe(&[approved]).alerts.is_empty());
    }

    #[test]
    fn a_review_on_your_own_pull_request_notifies_once_per_decision() {
        let mut watcher = Watcher::default();
        let base = pr(47, &[PrRole::Author]);
        watcher.observe(std::slice::from_ref(&base));

        let mut approved = base;
        approved.review = ReviewState::Approved;

        assert_eq!(
            events(&watcher.observe(&[approved.clone()])),
            vec![AlertEvent::ReviewSubmitted]
        );
        assert!(
            watcher.observe(&[approved]).alerts.is_empty(),
            "a second approval that changes nothing is not a second event"
        );
    }

    #[test]
    fn a_comment_count_that_falls_is_not_a_new_comment() {
        // Deleting a comment lowers it. Comparing for inequality rather than
        // for growth would notify about a deletion.
        let mut watcher = Watcher::default();
        let mut base = pr(47, &[PrRole::Author]);
        base.comments = 5;
        watcher.observe(std::slice::from_ref(&base));

        let mut fewer = base.clone();
        fewer.comments = 4;
        assert!(watcher.observe(&[fewer]).alerts.is_empty());

        let mut more = base;
        more.comments = 6;
        assert_eq!(
            events(&watcher.observe(&[more])),
            vec![AlertEvent::PrCommented]
        );
    }

    #[test]
    fn only_your_own_pull_requests_are_followed_up_when_they_disappear() {
        // The poll asks for open ones, so an ending is an absence — and
        // somebody else's pull request ending is not an event you asked for.
        let mut watcher = Watcher::default();
        watcher.observe(&[pr(47, &[PrRole::Author]), pr(45, &[PrRole::Reviewer])]);

        let observed = watcher.observe(&[]);
        assert_eq!(observed.vanished, vec![47]);
        assert!(observed.alerts.is_empty());
    }

    #[test]
    fn resetting_makes_the_next_poll_a_silent_baseline_again() {
        // Signing in as somebody else changes every role, and the difference is
        // not news about the pull requests.
        let mut watcher = Watcher::default();
        watcher.observe(&[pr(47, &[PrRole::Author])]);
        watcher.reset();

        let mut approved = pr(47, &[PrRole::Author]);
        approved.review = ReviewState::Approved;

        assert_eq!(watcher.observe(&[approved]), Observed::default());
    }

    #[test]
    fn several_changes_in_one_poll_all_arrive() {
        // Collapsing them into one summary is the notifier's job, above a
        // threshold — this layer reports what happened.
        let mut watcher = Watcher::default();
        let base = pr(47, &[PrRole::Author]);
        watcher.observe(std::slice::from_ref(&base));

        let mut busy = base;
        busy.checks = CheckState::Failing;
        busy.comments = 3;
        busy.review = ReviewState::ChangesRequested;

        let observed = watcher.observe(&[busy]);
        assert_eq!(observed.alerts.len(), 3);
    }
}
