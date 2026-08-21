//! Turning domain values into the strings the UI shows.

use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

/// Rough advance width per character at a given font size.
///
/// The canvas has no cheap text measurement, and the alternative — a shaping
/// pass per row per frame — is exactly the cost virtualisation exists to avoid.
/// Deliberately generous, so truncation errs toward cutting a character early
/// rather than overflowing into the next column.
pub const CHAR_WIDTH_RATIO: f32 = 0.62;
const NOMINAL_SIZE: f32 = crate::metrics::text::BODY;

/// Shortens `text` to fit `width` logical pixels, with an ellipsis.
pub fn truncate(text: &str, width: f32) -> String {
    let budget = (width / (NOMINAL_SIZE * CHAR_WIDTH_RATIO)).floor().max(1.0) as usize;
    let count = text.chars().count();

    if count <= budget {
        return text.to_owned();
    }

    let keep = budget.saturating_sub(1).max(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

/// A compact age: `2m`, `1h`, `3d`, `2y`.
///
/// Ages, not dates, because the graph's question is "how recent is this?" — the
/// exact timestamp lives in the detail pane, where there is room for it.
pub fn relative_time(then: OffsetDateTime) -> String {
    let seconds = (OffsetDateTime::now_utc() - then).whole_seconds();

    // Commit dates lie: clock skew and rebases both produce timestamps in the
    // future. Saying "now" is better than rendering a negative age.
    if seconds < 60 {
        return "now".to_owned();
    }

    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;

    match seconds {
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < MONTH => format!("{}d", s / DAY),
        s if s < YEAR => format!("{}mo", s / MONTH),
        s => format!("{}y", s / YEAR),
    }
}

const TIMESTAMP: &[BorrowedFormatItem<'static>] = format_description!(
    "[year]-[month]-[day] [hour]:[minute] [offset_hour sign:mandatory][offset_minute]"
);

/// The full timestamp, in the timezone the commit was made in.
///
/// Not converted to the reader's timezone: that a commit was made at 02:00
/// local time is information, and normalising it away loses it.
pub fn timestamp(at: OffsetDateTime) -> String {
    at.format(TIMESTAMP)
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}

/// A byte count, for oversized-file placeholders.
pub fn bytes(count: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;

    match count {
        b if b < KIB => format!("{b} B"),
        b if b < MIB => format!("{:.1} KiB", b as f64 / KIB as f64),
        b => format!("{:.1} MiB", b as f64 / MIB as f64),
    }
}

/// `+48 −12`, using a real minus sign rather than a hyphen.
pub fn diff_stat(insertions: usize, deletions: usize) -> String {
    format!("+{insertions} −{deletions}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    #[test]
    fn short_text_is_left_alone() {
        assert_eq!(truncate("fix: graph scroll", 400.0), "fix: graph scroll");
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis_and_never_grows() {
        let long = "a".repeat(400);
        let out = truncate(&long, 100.0);

        assert!(out.ends_with('…'));
        assert!(out.chars().count() < 20);
    }

    #[test]
    fn truncation_counts_characters_rather_than_bytes() {
        // A multi-byte summary must not be cut mid-character.
        let out = truncate("corrigir açúcar não é código", 60.0);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 8);
    }

    #[test]
    fn a_commit_from_the_future_reads_as_now_rather_than_a_negative_age() {
        let future = OffsetDateTime::now_utc() + Duration::hours(3);
        assert_eq!(relative_time(future), "now");
    }

    #[test]
    fn ages_step_through_sensible_units() {
        let ago = |d: Duration| relative_time(OffsetDateTime::now_utc() - d);

        assert_eq!(ago(Duration::seconds(10)), "now");
        assert_eq!(ago(Duration::minutes(5)), "5m");
        assert_eq!(ago(Duration::hours(3)), "3h");
        assert_eq!(ago(Duration::days(2)), "2d");
        assert_eq!(ago(Duration::days(60)), "2mo");
        assert_eq!(ago(Duration::days(800)), "2y");
    }

    #[test]
    fn a_timestamp_keeps_the_offset_it_was_recorded_in() {
        let at = OffsetDateTime::from_unix_timestamp(1_577_836_800)
            .unwrap()
            .to_offset(time::UtcOffset::from_hms(-3, 0, 0).unwrap());

        assert_eq!(timestamp(at), "2019-12-31 21:00 -0300");
    }

    #[test]
    fn byte_counts_pick_a_readable_unit() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2048), "2.0 KiB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn diff_stats_use_a_minus_sign_not_a_hyphen() {
        assert_eq!(diff_stat(48, 12), "+48 −12");
    }
}
