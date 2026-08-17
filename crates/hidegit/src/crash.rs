//! Panic reports, written to disk and never anywhere else.
//!
//! Local and opt-in, which given [ROADMAP](../../../docs/ROADMAP.md)'s stance on
//! telemetry is the only shape this can take: nothing is sent, nothing is
//! collected, and the report is a file you read, attach to an issue, or delete.
//!
//! **They are panic reports rather than crash reports**, and the difference is
//! not pedantry. Every `gix` read and every `git` subprocess runs on a blocking
//! task, and a panic there is caught by the task's join handle — the window
//! survives it and shows a toast, as
//! [ARCHITECTURE](../../../docs/ARCHITECTURE.md) describes. The hook fires
//! either way, so a report on disk means *something went wrong*, not
//! *hideGit died*. Saying "crash" would have people looking for a window that
//! closed when nothing did.
//!
//! What goes in: the version, the platform, the panic's message and location,
//! and a backtrace. What does not: which repositories are open, which branch is
//! checked out, any remote URL. A panic message can still carry a path if
//! something formatted one into it, which is said in the file rather than
//! quietly hoped about — but nothing here *adds* one.

use std::io::Write;
use std::path::{Path, PathBuf};

/// How many reports are kept. The oldest go first.
///
/// A directory that grows without limit is a directory somebody finds at a
/// gigabyte one day. Ten is enough to see a pattern in and small enough to read.
pub const KEEP: usize = 10;

/// Installs the panic hook.
///
/// The hook is installed whether or not reports are wanted: a panic always goes
/// to the log, because a panic nobody recorded anywhere is the situation this
/// exists to end. `dir` is `None` when reports are switched off or when there is
/// no data directory to write to, and then the log is all that happens.
pub fn install(dir: Option<PathBuf>) {
    let previous = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        // `payload_as_str` would say this in one line, and it is Rust 1.91 —
        // above the 1.88 floor `Cargo.toml` declares.
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("a panic with no message");

        let text = report(
            message,
            info.location().map(ToString::to_string).as_deref(),
            &std::backtrace::Backtrace::force_capture().to_string(),
        );

        // The log first, and unconditionally. Writing the file can fail; being
        // told what happened should not depend on that.
        tracing::error!("{text}");

        if let Some(dir) = &dir {
            match write(dir, &text) {
                Ok(path) => tracing::error!(report = %path.display(), "panic report written"),
                Err(error) => tracing::warn!(%error, "the panic report could not be written"),
            }
        }

        // Whatever the runtime would have done — printing to stderr, mostly —
        // still happens. Replacing it would make `RUST_BACKTRACE` do nothing.
        previous(info);
    }));
}

/// The text of a report.
///
/// Separated from the hook because this is the part worth testing: a hook can
/// only be exercised by panicking, and a test that panics on purpose to read a
/// file it wrote is a test that fights the harness.
pub fn report(message: &str, location: Option<&str>, backtrace: &str) -> String {
    format!(
        "hideGit {version} panicked.\n\
         \n\
         This file is local. Nothing was sent anywhere, and nothing will be.\n\
         hideGit survives most panics — the window may well have stayed open and\n\
         shown a toast — so this is a report of something going wrong, not proof\n\
         that the application died.\n\
         \n\
         Nothing here names a repository, a branch or a remote. The message below\n\
         is whatever the code that panicked wrote, so read it before attaching\n\
         this to a public issue.\n\
         \n\
         version:  {version}\n\
         platform: {os} {arch}\n\
         where:    {location}\n\
         message:  {message}\n\
         \n\
         backtrace:\n\
         {backtrace}\n",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        location = location.unwrap_or("unknown"),
    )
}

/// Writes a report and prunes the directory to [`KEEP`].
pub fn write(dir: &Path, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;

    // Named by the moment it happened, so the directory sorts into the order
    // things went wrong in and a report can be matched against "it was about
    // four o'clock". `%` and `:` are avoided: `:` is not a filename on Windows.
    let now = time::OffsetDateTime::now_utc();
    let name = format!(
        "panic-{:04}{:02}{:02}-{:02}{:02}{:02}.txt",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
    );

    let path = dir.join(name);
    // Appending rather than truncating: two panics in the same second are one
    // file, and losing the first to the second would hide the one that caused
    // the other.
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(text.as_bytes())?;

    prune(dir, KEEP);
    Ok(path)
}

/// Keeps the newest `keep` reports and deletes the rest.
///
/// Never fatal: this runs inside a panic hook, and failing to tidy up is not
/// worth a second panic on top of the first.
pub fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    // By name, which is by time: the filenames are fixed-width and start with
    // the year, so lexical order is chronological order.
    let mut reports: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("panic-") && name.ends_with(".txt"))
        })
        .collect();
    reports.sort();

    for path in reports.iter().rev().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

/// The newest report in `dir`, if there is one.
///
/// What the window uses to say "something went wrong last time" — a report
/// nobody knows about is a file that only ever gets found by accident.
pub fn newest(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;

    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("panic-") && name.ends_with(".txt"))
        })
        .max()
}

/// The report worth telling the user about, if there is one.
///
/// `None` when there are no reports, or when the newest is the one they were
/// already told about. Without that second half the notice appears on every
/// start until the file is deleted by hand, which teaches people to ignore it.
pub fn unannounced(dir: &Path, announced: Option<&str>) -> Option<String> {
    let newest = newest(dir)?.display().to_string();
    (Some(newest.as_str()) != announced).then_some(newest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_says_where_it_came_from_and_that_it_stayed_here() {
        let text = report("index out of bounds", Some("src/app.rs:42"), "0: main");

        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        assert!(text.contains(std::env::consts::OS));
        assert!(text.contains("src/app.rs:42"));
        assert!(text.contains("index out of bounds"));
        assert!(text.contains("0: main"));
        // The two things somebody reading this file needs to be told, because
        // both are otherwise reasonable to assume the other way.
        assert!(text.contains("Nothing was sent anywhere"), "{text}");
        assert!(text.contains("survives most panics"), "{text}");
    }

    #[test]
    fn a_panic_with_no_location_still_produces_a_report() {
        let text = report("something", None, "");
        assert!(text.contains("unknown"), "{text}");
    }

    #[test]
    fn the_directory_keeps_the_newest_and_drops_the_rest() {
        // A directory that grows without limit is one somebody finds at a
        // gigabyte, in the worst week of their year.
        let dir = tempfile::tempdir().unwrap();
        for at in 0..8 {
            std::fs::write(
                dir.path().join(format!("panic-2026081{at}-120000.txt")),
                "x",
            )
            .unwrap();
        }

        prune(dir.path(), 3);

        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "panic-20260815-120000.txt",
                "panic-20260816-120000.txt",
                "panic-20260817-120000.txt"
            ]
        );
    }

    #[test]
    fn pruning_leaves_everything_that_is_not_a_report_alone() {
        // It is a directory on somebody's disk. Deleting what it did not write
        // is not this code's business.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "mine").unwrap();
        std::fs::write(dir.path().join("panic-20260101-000000.txt"), "x").unwrap();

        prune(dir.path(), 0);

        assert!(dir.path().join("notes.txt").exists());
        assert!(!dir.path().join("panic-20260101-000000.txt").exists());
    }

    #[test]
    fn a_report_is_written_and_found_again() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(newest(dir.path()), None, "nothing yet");

        let path = write(dir.path(), "a report").unwrap();

        assert_eq!(newest(dir.path()).as_ref(), Some(&path));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a report");
    }

    #[test]
    fn two_panics_in_the_same_second_are_both_kept() {
        // The filename is the moment, so the second one lands on the first.
        // Truncating would lose whichever panic caused the other.
        let dir = tempfile::tempdir().unwrap();

        let first = write(dir.path(), "one\n").unwrap();
        let second = write(dir.path(), "two\n").unwrap();

        if first == second {
            assert_eq!(std::fs::read_to_string(&first).unwrap(), "one\ntwo\n");
        }
    }

    #[test]
    fn a_report_is_worth_saying_once() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unannounced(dir.path(), None), None, "nothing happened");

        let written = write(dir.path(), "a report").unwrap().display().to_string();

        assert_eq!(unannounced(dir.path(), None).as_deref(), Some(&*written));
        assert_eq!(
            unannounced(dir.path(), Some(&written)),
            None,
            "already said once"
        );
        assert_eq!(
            unannounced(dir.path(), Some("panic-19990101-000000.txt")).as_deref(),
            Some(&*written),
            "a newer one is a different one"
        );
    }

    #[test]
    fn a_directory_that_cannot_be_read_is_not_a_failure() {
        // This runs inside a panic hook. A second panic on top of the first
        // would replace the report with nothing at all.
        prune(Path::new("/definitely/not/here"), 3);
        assert_eq!(newest(Path::new("/definitely/not/here")), None);
    }
}
