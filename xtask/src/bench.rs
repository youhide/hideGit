//! Reading the benchmark results and deciding whether anything broke.
//!
//! **Timings are not gated, and cannot be.** A shared CI runner is two to three
//! times slower than a laptop on a bad minute and unremarkable on a good one, so
//! failing a pull request because `layout_whole_history_100k` took 40% longer
//! would mean failing pull requests at random. The numbers in
//! `docs/COMMIT_GRAPH.md` are measured on a known machine and stay that way.
//!
//! **Ratios are gated, because they survive a slow runner.** Two benchmarks in
//! the same run on the same machine divide out the machine. The one that matters
//! is checkpoints: laying out a visible window at row 50,000 against laying out
//! the same window with checkpoints skipped. That gap is the entire reason
//! checkpoints exist, and if it closes, they have stopped working — whatever
//! either number happens to be that day.

use std::path::Path;

/// The floor the checkpoint ratio has to clear.
///
/// Measured at roughly 400× on a laptop. The floor is set an order of magnitude
/// below it, so this fails when checkpoints stop working rather than when a
/// runner has a bad afternoon — a gate that trips on noise is a gate people
/// learn to re-run until it passes.
pub const CHECKPOINT_SPEEDUP: f64 = 50.0;

/// The two benchmarks whose ratio is checked, as criterion names them.
const WITH: &str = "graph/visible_window_at_row_50000";
const WITHOUT: &str = "graph/visible_window_at_row_50000_without_checkpoints";

/// Checks what the last benchmark run left in `target/criterion`.
pub fn check(root: &Path) -> Result<(), String> {
    let dir = root.join("target/criterion");

    let with = median(&dir, WITH)?;
    let without = median(&dir, WITHOUT)?;
    let ratio = speedup(with, without);

    println!("  {WITH:<58} {:>10.1} µs", with / 1000.0);
    println!("  {WITHOUT:<58} {:>10.1} µs", without / 1000.0);
    println!("  checkpoints are {ratio:.0}× faster (floor is {CHECKPOINT_SPEEDUP:.0}×)");

    if ratio < CHECKPOINT_SPEEDUP {
        return Err(format!(
            "checkpoints are only {ratio:.1}× faster than not having them, and the floor is \
             {CHECKPOINT_SPEEDUP:.0}×.\nThat gap is the whole reason checkpoints exist; see \
             docs/COMMIT_GRAPH.md#performance."
        ));
    }

    Ok(())
}

/// How many times faster the first is than the second.
///
/// A zero or a negative time is nonsense a divide would turn into infinity, so
/// it answers zero and lets the caller fail rather than pass.
pub fn speedup(with: f64, without: f64) -> f64 {
    if with <= 0.0 || without <= 0.0 {
        return 0.0;
    }
    without / with
}

/// The median of one benchmark's last run, in nanoseconds.
fn median(dir: &Path, name: &str) -> Result<f64, String> {
    let path = dir.join(name).join("new/estimates.json");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "{}: {error}\nRun `cargo bench -p hidegit-core` first.",
            path.display()
        )
    })?;

    point_estimate(&text).ok_or_else(|| format!("{}: no median in it", path.display()))
}

/// Pulls `median.point_estimate` out of criterion's `estimates.json`.
///
/// Read with a scan rather than a JSON parser: this is the only JSON `xtask`
/// will ever read, and a dependency for one field in one file written by one
/// known program is a dependency that has to be updated forever.
pub fn point_estimate(json: &str) -> Option<f64> {
    let median = json.find("\"median\"")?;
    let key = json[median..].find("\"point_estimate\"")? + median;
    let colon = json[key..].find(':')? + key + 1;

    let number: String = json[colon..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == 'e' || *c == '-' || *c == '+')
        .collect();

    number.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESTIMATES: &str = r#"{
        "mean": {"point_estimate": 999.0},
        "median": {
            "confidence_interval": {"lower_bound": 80048.5, "upper_bound": 80763.1},
            "point_estimate": 80405.85620117188,
            "standard_error": 252.6
        },
        "std_dev": {"point_estimate": 1.0}
    }"#;

    #[test]
    fn the_median_is_read_and_not_the_mean() {
        // `mean` comes first in the file and has a `point_estimate` of its own.
        // Reading the wrong one would compare two different statistics.
        assert_eq!(point_estimate(ESTIMATES), Some(80405.85620117188));
    }

    #[test]
    fn a_file_with_nothing_usable_in_it_is_not_a_number() {
        for nonsense in ["", "{}", r#"{"mean": {"point_estimate": 1.0}}"#, "not json"] {
            assert_eq!(point_estimate(nonsense), None, "“{nonsense}” parsed");
        }
    }

    #[test]
    fn the_ratio_is_how_much_the_checkpoints_buy() {
        // The numbers this shipped with: 80 µs against 32 ms.
        assert!((speedup(80_405.0, 32_268_000.0) - 401.3).abs() < 1.0);
        assert!(speedup(80_405.0, 32_268_000.0) > CHECKPOINT_SPEEDUP);
    }

    #[test]
    fn a_ratio_that_cannot_be_computed_fails_rather_than_passes() {
        // A divide by zero is infinity, which clears every floor there is.
        assert_eq!(speedup(0.0, 100.0), 0.0);
        assert_eq!(speedup(100.0, 0.0), 0.0);
        assert!(speedup(0.0, 100.0) < CHECKPOINT_SPEEDUP);
    }

    #[test]
    fn checkpoints_that_stopped_working_are_caught() {
        // The failure this exists for: the two paths converge because the
        // checkpoint lookup silently stopped being used.
        assert!(speedup(30_000_000.0, 32_268_000.0) < CHECKPOINT_SPEEDUP);
    }

    /// A criterion output tree with the two benchmarks the gate reads.
    fn results(dir: &Path, with: f64, without: f64) {
        for (name, median) in [(WITH, with), (WITHOUT, without)] {
            let at = dir.join("target/criterion").join(name).join("new");
            std::fs::create_dir_all(&at).unwrap();
            std::fs::write(
                at.join("estimates.json"),
                format!(r#"{{"median": {{"point_estimate": {median}}}}}"#),
            )
            .unwrap();
        }
    }

    #[test]
    fn a_run_where_checkpoints_still_pay_passes() {
        let dir = tempfile::tempdir().unwrap();
        results(dir.path(), 80_405.0, 32_268_000.0);

        assert!(check(dir.path()).is_ok());
    }

    #[test]
    fn a_run_where_they_stopped_paying_fails_and_says_why() {
        // The end of the gate, not just the arithmetic behind it: without this
        // the comparison could be computed correctly and then ignored.
        let dir = tempfile::tempdir().unwrap();
        results(dir.path(), 30_000_000.0, 32_268_000.0);

        let error = check(dir.path()).unwrap_err();
        assert!(error.contains("COMMIT_GRAPH"), "{error}");
        assert!(error.contains("1.1×"), "it names the ratio it saw: {error}");
    }

    #[test]
    fn a_missing_run_says_what_to_do_about_it() {
        let dir = tempfile::tempdir().unwrap();
        let error = check(dir.path()).unwrap_err();

        assert!(error.contains("cargo bench"), "{error}");
    }
}
