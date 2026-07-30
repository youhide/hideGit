//! Native desktop notifications.
//!
//! Behind a trait, because nothing in CI can receive one: a Linux runner has no
//! notification daemon and a macOS runner has no bundle to send from. The trait
//! is what lets everything that *decides* to notify be tested — which is the
//! part with the logic in it.
//!
//! **On macOS, `notify-rust` needs a bundled `.app`.** It goes through
//! `mac-notification-sys`, which asks the notification centre for a registered
//! bundle identifier; a notification sent from `cargo run` has none and does
//! not appear. Verifying alerts there therefore means
//! `cargo run -p xtask -- bundle-macos` and running the bundle. Stated here
//! because the failure is silent.

use std::fmt;

use crate::poll::Alert;

/// Somewhere a notification can go.
pub trait Notifier: Send + Sync + fmt::Debug {
    /// Shows one notification. Failure is logged, never propagated: a
    /// notification that could not be delivered is not a reason to fail the
    /// poll that produced it.
    fn notify(&self, summary: &str, body: &str);
}

/// Above this many alerts in one poll, they collapse into a summary.
///
/// Three separate notifications is a glance; ten is a wall, and a wall gets
/// dismissed unread — which costs the one that mattered.
pub const COLLAPSE_ABOVE: usize = 3;

/// Turns a poll's alerts into what should actually be shown.
///
/// Returns `(summary, body)` pairs, already collapsed. Kept apart from
/// [`Notifier`] so the decision can be asserted on without a notification
/// daemon anywhere near it.
pub fn compose(alerts: &[Alert], repository: &str) -> Vec<(String, String)> {
    if alerts.is_empty() {
        return Vec::new();
    }

    if alerts.len() > COLLAPSE_ABOVE {
        return vec![(
            format!("{} pull request updates", alerts.len()),
            format!("in {repository}"),
        )];
    }

    alerts
        .iter()
        .map(|alert| {
            (
                alert.event.summary(alert.number),
                format!("{} · {repository}", alert.title),
            )
        })
        .collect()
}

/// The real one.
#[derive(Debug, Default)]
pub struct Desktop;

impl Notifier for Desktop {
    fn notify(&self, summary: &str, body: &str) {
        let mut notification = notify_rust::Notification::new();
        notification.summary(summary).body(body).appname("hideGit");

        // The icon is looked up by name in the hicolor theme, which
        // `packaging/linux/install.sh` populates. It is ignored elsewhere.
        #[cfg(target_os = "linux")]
        notification.icon("hidegit");

        if let Err(error) = notification.show() {
            // Logged rather than surfaced. A machine with no notification
            // daemon is a machine where this will fail every time, and a toast
            // for each would be worse than the missing notification.
            tracing::debug!(%error, "could not show a notification");
        }
    }
}

/// One that records instead of showing.
#[cfg(any(test, feature = "fake"))]
#[derive(Debug, Default)]
pub struct Recorder(std::sync::Mutex<Vec<(String, String)>>);

#[cfg(any(test, feature = "fake"))]
impl Recorder {
    pub fn shown(&self) -> Vec<(String, String)> {
        self.0.lock().expect("not poisoned").clone()
    }
}

#[cfg(any(test, feature = "fake"))]
impl Notifier for Recorder {
    fn notify(&self, summary: &str, body: &str) {
        self.0
            .lock()
            .expect("not poisoned")
            .push((summary.to_owned(), body.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poll::AlertEvent;

    fn alert(event: AlertEvent, number: u64) -> Alert {
        Alert {
            event,
            number,
            title: "feat: something".to_owned(),
            url: format!("https://github.com/youhide/hideGit/pull/{number}"),
        }
    }

    #[test]
    fn nothing_happened_shows_nothing() {
        assert!(compose(&[], "youhide/hideGit").is_empty());
    }

    #[test]
    fn a_few_alerts_are_shown_one_by_one() {
        let alerts = [
            alert(AlertEvent::ReviewRequested, 47),
            alert(AlertEvent::ChecksFailed, 45),
        ];

        let shown = compose(&alerts, "youhide/hideGit");
        assert_eq!(shown.len(), 2);
        assert!(shown[0].0.contains("#47"));
        assert!(shown[0].1.contains("youhide/hideGit"));
    }

    #[test]
    fn a_burst_collapses_into_one_summary() {
        // Ten notifications get dismissed unread, which costs the one that
        // mattered.
        let alerts: Vec<Alert> = (1..=10)
            .map(|n| alert(AlertEvent::PrCommented, n))
            .collect();

        let shown = compose(&alerts, "youhide/hideGit");
        assert_eq!(shown.len(), 1);
        assert!(shown[0].0.contains("10"));
        assert!(shown[0].1.contains("youhide/hideGit"));
    }

    #[test]
    fn the_threshold_itself_is_still_shown_individually() {
        let alerts: Vec<Alert> = (1..=COLLAPSE_ABOVE as u64)
            .map(|n| alert(AlertEvent::PrCommented, n))
            .collect();

        assert_eq!(compose(&alerts, "youhide/hideGit").len(), COLLAPSE_ABOVE);
    }

    #[test]
    fn every_event_says_which_pull_request_it_is_about() {
        // A notification that does not name the pull request is one the user
        // has to open the application to understand.
        for event in [
            AlertEvent::ReviewRequested,
            AlertEvent::ReviewSubmitted,
            AlertEvent::PrCommented,
            AlertEvent::ChecksFailed,
            AlertEvent::ChecksPassed,
            AlertEvent::PrConflicting,
            AlertEvent::PrMerged,
            AlertEvent::PrClosed,
        ] {
            assert!(event.summary(47).contains("#47"), "{event:?}");
        }
    }

    #[test]
    fn a_recorder_captures_what_would_have_been_shown() {
        let recorder = Recorder::default();
        recorder.notify("Checks failed on #47", "feat: something");

        assert_eq!(
            recorder.shown(),
            vec![(
                "Checks failed on #47".to_owned(),
                "feat: something".to_owned()
            )]
        );
    }
}
