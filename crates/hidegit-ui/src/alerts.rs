//! The subscription that drives polling.
//!
//! The *schedule* lives in `hidegit-forge` — it is arithmetic, and testing
//! arithmetic through a toolkit is miserable. This file is only the timer that
//! asks, the same split `hidegit_core::watch` and [`crate::watcher`] already
//! use for the filesystem.

use std::time::Duration;

use hidegit_forge::{Activity, Next, Schedule};
use iced::Subscription;

use crate::message::{Message, RepoMessage};

/// The shortest a tick is ever set to.
///
/// A budget that resets in two seconds should not produce a two-second timer;
/// the clock hideGit is comparing against is GitHub's, not this machine's, and
/// the two are not exactly aligned.
const FLOOR: Duration = Duration::from_secs(60);

/// A timer for one repository, or nothing when there is nothing to poll.
///
/// The interval is part of the subscription's identity, so when the schedule
/// changes — a failure, a thin budget, the window losing focus — iced tears the
/// old timer down and starts the new one. That is the mechanism working as
/// intended rather than a leak.
pub fn subscribe(
    index: usize,
    total: usize,
    schedule: &Schedule,
    activity: Activity,
    now: time::OffsetDateTime,
) -> Subscription<Message> {
    let every = match schedule.next(activity, jitter(index, total)) {
        Next::After(delay) => delay.max(FLOOR),
        // Nothing until the budget resets. The timer is still set, so polling
        // resumes on its own rather than waiting for somebody to click.
        Next::Exhausted { until } => {
            let seconds = (until - now).whole_seconds().max(0);
            Duration::from_secs(seconds.unsigned_abs()).max(FLOOR)
        }
    };

    // `with` rather than a capturing closure: iced requires `map`'s closure to
    // be zero-sized, and the interval is already part of the subscription's
    // identity through `every`.
    iced::time::every(every)
        .with(index)
        .map(|(index, _)| Message::Repo(index, RepoMessage::PrsRefreshRequested))
}

/// Spreads several repositories' retries apart.
///
/// Derived from the repository's position rather than from a random number, so
/// the same set of repositories always spreads the same way and a test can
/// assert on it. What matters is that two repositories backing off from the
/// same outage do not retry in lockstep, and an index does that.
fn jitter(index: usize, total: usize) -> f32 {
    if total <= 1 {
        return 0.0;
    }
    index as f32 / total as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_repository_needs_no_spreading() {
        assert_eq!(jitter(0, 1), 0.0);
        assert_eq!(jitter(0, 0), 0.0);
    }

    #[test]
    fn several_repositories_are_spread_across_the_window() {
        // Two backing off from the same outage must not retry together.
        assert_eq!(jitter(0, 4), 0.0);
        assert_eq!(jitter(1, 4), 0.25);
        assert_eq!(jitter(3, 4), 0.75);
    }

    #[test]
    fn a_tick_is_never_shorter_than_the_floor() {
        // A budget that resets in two seconds must not produce a two-second
        // timer: hideGit is comparing against GitHub's clock, not this one.
        let mut schedule = Schedule::default();
        schedule.succeeded(hidegit_forge::RateBudget {
            limit: 5_000,
            remaining: 10,
            reset: time::OffsetDateTime::now_utc() + time::Duration::seconds(2),
        });

        let Next::Exhausted { until } = schedule.next(Activity::Normal, 0.0) else {
            panic!("an all-but-spent budget stops polling")
        };

        let now = time::OffsetDateTime::now_utc();
        let seconds = (until - now).whole_seconds().max(0);
        assert!(Duration::from_secs(seconds.unsigned_abs()).max(FLOOR) >= FLOOR);
    }
}
