//! Which alerts to send, and when not to.
//!
//! Defined here rather than in the binary's config module so there is one
//! definition rather than a config copy and a UI copy that drift. It is plain
//! `serde`, and it lands in `config.toml` under `[alerts]`.
//!
//! **Every value has a working default**, and the defaults are the table in
//! `docs/UI_SPEC.md#pr-panel`: everything on except `ChecksPassed`, which is
//! the one event that fires when nothing needs doing.

use serde::{Deserialize, Serialize};

use crate::poll::AlertEvent;

/// Alert preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlertPrefs {
    /// The master switch. Off means the panel still works — pull requests are
    /// still listed and still polled — and nothing is shown on the desktop.
    pub enabled: bool,
    pub events: EventPrefs,
    pub quiet_hours: QuietHours,
    /// Repositories to stay silent about, as `owner/name`.
    ///
    /// Per repository rather than per remote URL, so a repository cloned twice
    /// is muted once.
    pub muted: Vec<String>,
}

impl Default for AlertPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            events: EventPrefs::default(),
            quiet_hours: QuietHours::default(),
            muted: Vec::new(),
        }
    }
}

/// Per-event toggles.
///
/// Named fields rather than a map keyed by event, so an unknown key in the file
/// is a visible error and `deny_unknown_fields` can catch a typo instead of
/// silently ignoring it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventPrefs {
    pub review_requested: bool,
    pub review_submitted: bool,
    pub pr_commented: bool,
    pub checks_failed: bool,
    /// **Off by default.** Every other event is something that needs your
    /// attention; a build going green is the absence of a problem, and a
    /// notification for it is one more thing to dismiss.
    pub checks_passed: bool,
    pub pr_conflicting: bool,
    pub pr_merged: bool,
    pub pr_closed: bool,
}

impl Default for EventPrefs {
    fn default() -> Self {
        Self {
            review_requested: true,
            review_submitted: true,
            pr_commented: true,
            checks_failed: true,
            checks_passed: false,
            pr_conflicting: true,
            pr_merged: true,
            pr_closed: true,
        }
    }
}

impl EventPrefs {
    pub fn allows(&self, event: AlertEvent) -> bool {
        match event {
            AlertEvent::ReviewRequested => self.review_requested,
            AlertEvent::ReviewSubmitted => self.review_submitted,
            AlertEvent::PrCommented => self.pr_commented,
            AlertEvent::ChecksFailed => self.checks_failed,
            AlertEvent::ChecksPassed => self.checks_passed,
            AlertEvent::PrConflicting => self.pr_conflicting,
            AlertEvent::PrMerged => self.pr_merged,
            AlertEvent::PrClosed => self.pr_closed,
        }
    }
}

/// A window of the day to stay silent in.
///
/// Hours in local time, `from` inclusive and `to` exclusive. A window that
/// wraps midnight — the usual one — is expressed by `from > to`, which is what
/// `22` to `8` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuietHours {
    pub enabled: bool,
    pub from: u8,
    pub to: u8,
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            from: 22,
            to: 8,
        }
    }
}

impl QuietHours {
    /// Whether `hour` (local, 0–23) falls inside the window.
    ///
    /// The hour is passed in rather than read here, which keeps this a pure
    /// function and keeps `hidegit-forge` off the local-timezone machinery —
    /// the crate that already owns a clock is the one that should read it.
    pub fn covers(&self, hour: u8) -> bool {
        if !self.enabled {
            return false;
        }
        // An empty window silences nothing; equal bounds would otherwise
        // silence either everything or nothing depending on the comparison,
        // and neither reading is obviously right.
        if self.from == self.to {
            return false;
        }

        if self.from < self.to {
            (self.from..self.to).contains(&hour)
        } else {
            // Wraps midnight.
            hour >= self.from || hour < self.to
        }
    }
}

impl AlertPrefs {
    /// Whether one alert should reach the desktop.
    ///
    /// `repository` is `owner/name`; `hour` is the local hour it is now.
    pub fn allows(&self, event: AlertEvent, repository: &str, hour: u8) -> bool {
        self.enabled
            && !self.quiet_hours.covers(hour)
            && !self.muted.iter().any(|muted| muted == repository)
            && self.events.allows(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_table_in_the_spec() {
        let prefs = AlertPrefs::default();

        assert!(prefs.enabled);
        assert!(prefs.events.review_requested);
        assert!(prefs.events.review_submitted);
        assert!(prefs.events.pr_commented);
        assert!(prefs.events.checks_failed);
        assert!(prefs.events.pr_conflicting);
        assert!(prefs.events.pr_merged);
        assert!(prefs.events.pr_closed);
        assert!(
            !prefs.events.checks_passed,
            "a build going green is the absence of a problem, not something to interrupt for"
        );
        assert!(!prefs.quiet_hours.enabled);
    }

    #[test]
    fn a_quiet_window_that_wraps_midnight_is_the_ordinary_one() {
        let quiet = QuietHours {
            enabled: true,
            from: 22,
            to: 8,
        };

        assert!(quiet.covers(23));
        assert!(quiet.covers(0));
        assert!(quiet.covers(7));
        assert!(!quiet.covers(8), "the end is exclusive");
        assert!(!quiet.covers(12));
        assert!(quiet.covers(22), "the start is inclusive");
    }

    #[test]
    fn a_quiet_window_inside_one_day_also_works() {
        let quiet = QuietHours {
            enabled: true,
            from: 9,
            to: 17,
        };

        assert!(quiet.covers(9));
        assert!(quiet.covers(16));
        assert!(!quiet.covers(17));
        assert!(!quiet.covers(8));
        assert!(!quiet.covers(23));
    }

    #[test]
    fn an_empty_window_silences_nothing() {
        // Equal bounds would otherwise silence everything or nothing depending
        // on which comparison ran first, and neither reading is obviously right.
        let quiet = QuietHours {
            enabled: true,
            from: 3,
            to: 3,
        };

        for hour in 0..24 {
            assert!(!quiet.covers(hour), "hour {hour}");
        }
    }

    #[test]
    fn quiet_hours_do_nothing_until_they_are_turned_on() {
        let quiet = QuietHours {
            enabled: false,
            from: 0,
            to: 23,
        };
        assert!(!quiet.covers(12));
    }

    #[test]
    fn a_muted_repository_is_silent_without_turning_anything_else_off() {
        let prefs = AlertPrefs {
            muted: vec!["youhide/noisy".to_owned()],
            ..AlertPrefs::default()
        };

        assert!(!prefs.allows(AlertEvent::ChecksFailed, "youhide/noisy", 12));
        assert!(prefs.allows(AlertEvent::ChecksFailed, "youhide/hideGit", 12));
    }

    #[test]
    fn the_master_switch_silences_every_event() {
        let prefs = AlertPrefs {
            enabled: false,
            ..AlertPrefs::default()
        };

        for event in [
            AlertEvent::ReviewRequested,
            AlertEvent::ChecksFailed,
            AlertEvent::PrMerged,
        ] {
            assert!(!prefs.allows(event, "youhide/hideGit", 12), "{event:?}");
        }
    }

    #[test]
    fn a_config_file_naming_only_one_setting_keeps_every_other_default() {
        // The rule the whole config module is built on: a missing or partial
        // file produces defaults, never a failure.
        let prefs: AlertPrefs =
            toml::from_str("[events]\nchecks_passed = true\n").expect("it parses");

        assert!(prefs.events.checks_passed, "the one that was set");
        assert!(
            prefs.events.checks_failed,
            "and the rest keep their default"
        );
        assert!(prefs.enabled);
    }

    #[test]
    fn a_misspelled_setting_is_rejected_rather_than_ignored() {
        // `deny_unknown_fields` is why the toggles are named fields rather than
        // a map: a typo that silently turns nothing on is worse than an error.
        let outcome: Result<AlertPrefs, _> = toml::from_str("[events]\nchecks_faild = true\n");
        assert!(outcome.is_err());
    }

    #[test]
    fn preferences_round_trip_through_toml() {
        let prefs = AlertPrefs {
            enabled: true,
            events: EventPrefs {
                checks_passed: true,
                ..EventPrefs::default()
            },
            quiet_hours: QuietHours {
                enabled: true,
                from: 21,
                to: 9,
            },
            muted: vec!["youhide/noisy".to_owned()],
        };

        let text = toml::to_string_pretty(&prefs).unwrap();
        assert_eq!(toml::from_str::<AlertPrefs>(&text).unwrap(), prefs);
    }
}
