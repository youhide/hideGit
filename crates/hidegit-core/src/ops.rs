//! Inputs and outcomes for the operations that write.
//!
//! The write half of [`crate::GitBackend`] carries its full signature from M1 so
//! the read/write split is auditable in one file, and each operation is designed
//! properly in the milestone that implements it. Staging landed in M2 and
//! branches, remotes and the stash in M3; **the types for history rewriting —
//! [`MergeOpts`], [`RebasePlan`] and their outcomes — are still provisional and
//! will be refined in M5** rather than treated as settled.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::model::{Commit, Conflict, ObjectId};

/// How a commit differs from a plain one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitOpts {
    /// Replace the current `HEAD` commit rather than adding one.
    pub amend: bool,
    /// Append a `Signed-off-by` trailer.
    pub sign_off: bool,
    /// Allow a commit that changes nothing.
    pub allow_empty: bool,
}

/// A patch to apply to the index, for hunk- and line-level staging.
///
/// Staging part of a file is done by feeding `git apply --cached` a patch
/// rather than by rewriting the index directly: the same code path handles
/// hunks, line selections and reverse application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub file: PathBuf,
    /// The patch text, in unified diff format.
    ///
    /// Built by [`crate::patch::serialize`] from the diff the staging view is
    /// already showing, so what is applied is what was on screen.
    pub text: String,
    /// Apply in reverse — how unstaging is expressed.
    pub reverse: bool,
}

/// Where a new branch or tag starts.
///
/// A commit id and a ref name are not interchangeable: `git branch feat main`
/// records `main`'s commit, but naming the ref is what the user asked for and is
/// what Git's own reflog will say. Keeping them distinct also means a caller does
/// not have to resolve a ref before it can use one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartPoint {
    /// Wherever `HEAD` is now.
    Head,
    Commit(ObjectId),
    /// A ref name, full or short — whatever the user picked from.
    Ref(String),
}

/// What `checkout` should switch to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutTarget {
    Branch(String),
    /// Results in a detached `HEAD`.
    Commit(ObjectId),
    NewBranch {
        name: String,
        from: StartPoint,
    },
    /// A remote-tracking branch, as a new local branch that tracks it.
    ///
    /// The most common single action in a day's work, and not expressible as
    /// [`CheckoutTarget::NewBranch`]: that would create a branch at the same
    /// commit with no upstream, so the first push would need `--set-upstream`
    /// and the sidebar would show no ahead/behind.
    TrackRemote {
        /// The remote-tracking ref, e.g. `origin/feat`.
        remote_ref: String,
        /// The local branch to create, e.g. `feat`.
        local: String,
    },
}

/// A tag to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSpec {
    pub name: String,
    pub at: StartPoint,
    /// `Some` makes it an annotated tag carrying this message; `None` makes it a
    /// lightweight tag, which is just a ref.
    pub message: Option<String>,
}

/// How hard a push is allowed to push.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ForceMode {
    #[default]
    None,
    /// `--force-with-lease`: refuses if the remote moved since the last fetch.
    /// The default whenever a force is requested at all.
    WithLease,
    /// `--force`. Has to be selected deliberately; never a fallback for a
    /// failed lease.
    Force,
}

/// What to push where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSpec {
    pub refspec: String,
    pub force: ForceMode,
    pub set_upstream: bool,
}

/// How much of a remote to fetch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchOpts {
    /// Delete remote-tracking refs whose remote branch is gone.
    pub prune: bool,
    /// Fetch tags reachable from the fetched refs.
    pub tags: bool,
    /// Every configured remote rather than one named one.
    pub all_remotes: bool,
}

/// What a fetch brought back.
///
/// The names are as `git` printed them — usually short, like `origin/main` — so
/// they are for showing a person, not for resolving. An empty outcome means the
/// fetch worked and there was nothing to summarise, never that it failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchOutcome {
    pub updated: Vec<String>,
    pub pruned: Vec<String>,
}

/// Which remote to pull from.
///
/// There is deliberately no strategy field. `pull.rebase`, `pull.ff` and
/// `rebase.autoStash` are the user's own Git configuration, and carrying that
/// configuration is half the reason writes shell out at all — a pull that
/// behaved differently here than in the same user's terminal would be exactly
/// the surprise the hybrid backend exists to avoid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullOpts {
    /// `None` uses the current branch's configured upstream remote.
    pub remote: Option<String>,
}

/// How a pull ended. Conflicts are an outcome, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullOutcome {
    UpToDate,
    FastForwarded(ObjectId),
    /// A merge or a rebase, depending on the user's configuration, that landed.
    Integrated(ObjectId),
    Conflicted(Vec<Conflict>),
}

/// What a push did, per ref.
///
/// `rejected` is not, by itself, an error: pushing several refs at once can update
/// some and be refused others, and reporting only the failure would hide what did
/// land. A push where *nothing* landed is a plain failure, and Git's own hint —
/// which says exactly what to do about a non-fast-forward — is what gets shown.
///
/// Names are as `git` printed them, so the short form. For display, not for
/// resolving.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushOutcome {
    pub updated: Vec<String>,
    pub rejected: Vec<String>,
}

/// Whether a merge may or must fast-forward.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FastForward {
    #[default]
    Allow,
    /// Fail rather than create a merge commit.
    Only,
    /// Always create a merge commit.
    Never,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeOpts {
    pub fast_forward: FastForward,
    pub message: Option<String>,
}

/// How a merge ended. Conflicts are an outcome, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    UpToDate,
    FastForwarded(ObjectId),
    Merged(ObjectId),
    Conflicted(Vec<Conflict>),
}

/// What to do with one commit during a rebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseStep {
    pub action: RebaseAction,
    pub commit: ObjectId,
}

/// The full plan for an interactive rebase, in application order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebasePlan {
    pub steps: Vec<RebaseStep>,
}

/// How a rebase, cherry-pick or revert ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceOutcome {
    Completed,
    /// Stopped part-way, either on a conflict or on an `edit` step. The
    /// repository stays in this state until it is continued or aborted.
    Stopped {
        at: ObjectId,
        conflicts: Vec<Conflict>,
    },
}

/// How far a reset moves, and what it does to the index and working tree.
///
/// The three are spelled out rather than collapsed into a boolean because the
/// difference between them is exactly the difference between losing work and
/// not: `Hard` is the only Git operation in hideGit that destroys uncommitted
/// changes without writing them anywhere first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResetMode {
    /// Move `HEAD`. The index and working tree are left alone, so what the
    /// commits contained shows up as staged changes.
    Soft,
    /// Move `HEAD` and reset the index. Changes survive as unstaged.
    #[default]
    Mixed,
    /// Move `HEAD`, the index *and* the working tree. Uncommitted work is
    /// gone, and no reflog entry brings it back.
    Hard,
}

impl ResetMode {
    /// The `git reset` flag for this mode.
    pub fn flag(self) -> &'static str {
        match self {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        }
    }

    /// True when the mode can discard uncommitted work.
    ///
    /// The UI reads this to decide whether the action needs the confirmation
    /// that names what is about to be lost.
    pub fn is_destructive(self) -> bool {
        matches!(self, ResetMode::Hard)
    }
}

/// What to do with an operation the repository is in the middle of.
///
/// Every one of merge, rebase, cherry-pick and revert can stop part-way, and
/// all four are continued and aborted by the same three verbs, so they share
/// one type rather than growing a near-identical enum each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceControl {
    /// Record the resolution and carry on with the remaining steps.
    Continue,
    /// Restore the repository to exactly its state before the operation began.
    Abort,
    /// Drop the current commit and move to the next. Not valid for a merge,
    /// which has a single step to skip.
    Skip,
}

/// What to do to the stash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StashOp {
    Push {
        message: Option<String>,
        include_untracked: bool,
    },
    Apply(usize),
    Pop(usize),
    Drop(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StashOutcome {
    Created(ObjectId),
    Applied,
    Dropped,
    Conflicted(Vec<Conflict>),
}

/// A progress report from a long-running operation.
///
/// Anything that may exceed roughly 300ms reports in a real unit — objects,
/// commits, bytes — rather than an indeterminate spinner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressUpdate {
    pub phase: String,
    pub done: u64,
    /// `None` when the total is not known yet.
    pub total: Option<u64>,
}

/// Where progress reports go.
///
/// A trait object rather than a channel so `hidegit-core` stays free of any
/// particular async runtime; the UI adapts it to a `Subscription`.
pub trait ProgressSink: Send + Sync {
    fn report(&self, update: ProgressUpdate);
}

impl ProgressUpdate {
    /// Parses one line of `git --progress` output, if it is one.
    ///
    /// Git writes `Receiving objects:  42% (42/100)`, sometimes prefixed with
    /// `remote: `, and rewrites the line in place with a bare carriage return.
    /// Parsed by hand rather than with a regular expression: this is the only
    /// pattern that needs matching, and `crate::process::parse_version` is the
    /// precedent for keeping the dependency list short.
    ///
    /// Returns `None` for anything else — a summary line, a warning, a hint.
    /// That is not a failure; the caller keeps the text for `stderr` and moves on.
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim().strip_prefix("remote: ").unwrap_or(line.trim());

        // `<phase>: <n>% (<done>/<total>)`. The colon is the anchor, and the
        // phase is everything before it.
        let (phase, rest) = line.split_once(':')?;
        if phase.is_empty() || !phase.chars().all(|c| c.is_alphabetic() || c == ' ') {
            return None;
        }

        // The counts are what carry meaning; the percentage is redundant with
        // them and is skipped rather than parsed twice.
        let open = rest.find('(')?;
        let counts = rest[open + 1..].split(')').next()?;
        let (done, total) = counts.split_once('/')?;

        Some(Self {
            phase: phase.trim().to_owned(),
            done: done.trim().parse().ok()?,
            total: total.trim().parse().ok(),
        })
    }

    /// Progress as a fraction, when the total is known.
    pub fn fraction(&self) -> Option<f32> {
        match self.total {
            Some(total) if total > 0 => Some((self.done as f32 / total as f32).clamp(0.0, 1.0)),
            _ => None,
        }
    }
}

/// Discards progress. For calls that do not display any.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report(&self, _update: ProgressUpdate) {}
}

/// A cooperative request to stop a long operation.
///
/// An `Arc<AtomicBool>` rather than a channel for the same reason
/// [`ProgressSink`] is a trait object: `hidegit-core` stays free of any
/// particular async runtime. Cloning shares the flag, so the UI keeps one handle
/// and the blocking worker keeps another.
///
/// Setting it does not stop anything by itself — whoever runs the subprocess
/// polls it and kills the child. Cancelling a `git` command can leave
/// `index.lock` behind, which is reported rather than deleted; see
/// [`crate::process::GitCommand::run_streaming`].
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the operation to stop. Idempotent, and never blocks.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// What to look for in history.
///
/// One field rather than a field per kind: people type a fragment and expect it
/// to be found, not to first classify it as a hash or an author. Every field is
/// searched and the caller is told which one matched, so the result list can
/// say *why* a commit is in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Matched case-insensitively against the summary, the body, the author's
    /// name and email, and — as a prefix — the commit id.
    pub text: String,
    /// How many matches to stop at.
    ///
    /// A search with no matches walks the whole history whatever this is; the
    /// cap bounds the *result*, not the work, and the caller says so rather
    /// than implying it found everything.
    pub limit: usize,
}

/// Why a commit is in the results.
///
/// Shown next to each hit: a list that cannot say whether it matched the
/// message or the author leaves the reader to guess, and guessing wrong sends
/// them back to the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    Summary,
    Body,
    Author,
    Hash,
}

impl SearchField {
    pub fn label(self) -> &'static str {
        match self {
            SearchField::Summary => "summary",
            SearchField::Body => "message body",
            SearchField::Author => "author",
            SearchField::Hash => "hash",
        }
    }
}

/// One commit that matched, and what matched in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub commit: Commit,
    pub field: SearchField,
}

/// What a search found, and whether it found everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    /// True when the walk stopped at the limit rather than at the end of
    /// history.
    ///
    /// The difference is the difference between "these are the matches" and
    /// "these are the first matches", and a list that does not distinguish them
    /// is a list that lies by omission.
    pub truncated: bool,
}

/// Blame output. Lands in M6.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blame {
    pub lines: Vec<BlameLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub commit: ObjectId,
    /// 1-based line number in the file as of the blamed revision.
    pub lineno: u32,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_read_out_of_gits_own_counts() {
        let update = ProgressUpdate::parse("Receiving objects:  42% (42/100)").expect("a count");
        assert_eq!(update.phase, "Receiving objects");
        assert_eq!(update.done, 42);
        assert_eq!(update.total, Some(100));
        assert_eq!(update.fraction(), Some(0.42));
    }

    #[test]
    fn a_remote_prefix_is_the_remotes_progress_not_a_phase_name() {
        let update =
            ProgressUpdate::parse("remote: Counting objects:  50% (3/6)").expect("a count");
        assert_eq!(update.phase, "Counting objects");
        assert_eq!(update.done, 3);
    }

    #[test]
    fn a_finished_phase_still_reports_its_final_counts() {
        // Git appends `, done.` to the last line of a phase. The counts before
        // it are the ones that matter, so the trailing text must not defeat it.
        let update =
            ProgressUpdate::parse("Resolving deltas: 100% (12/12), done.").expect("a count");
        assert_eq!((update.done, update.total), (12, Some(12)));
        assert_eq!(update.fraction(), Some(1.0));
    }

    #[test]
    fn a_rate_after_the_counts_is_ignored_rather_than_confusing_the_parse() {
        let update =
            ProgressUpdate::parse("Receiving objects:  90% (90/100), 1.20 MiB | 500 KiB/s")
                .expect("a count");
        assert_eq!((update.done, update.total), (90, Some(100)));
    }

    #[test]
    fn lines_that_are_not_progress_are_not_an_error() {
        // Everything git writes to stderr comes through this parser. Warnings,
        // hints and summaries are kept for `stderr` and must not be mistaken
        // for counts.
        for line in [
            "",
            "From /tmp/remote",
            "   a3f9c21..b7e2d10  main       -> origin/main",
            "warning: redirecting to https://example.invalid/repo.git/",
            "hint: use --force-with-lease",
            "fatal: could not read Username for 'https://example.invalid'",
        ] {
            assert_eq!(
                ProgressUpdate::parse(line),
                None,
                "{line:?} is not a progress report"
            );
        }
    }

    #[test]
    fn an_unknown_total_still_reports_what_is_done() {
        let update = ProgressUpdate {
            phase: "Enumerating objects".to_owned(),
            done: 17,
            total: None,
        };
        assert_eq!(
            update.fraction(),
            None,
            "no total means no fraction to show"
        );
    }

    #[test]
    fn a_cancel_token_is_shared_by_every_clone_of_it() {
        // The UI holds one and the blocking worker holds another; a cancel on
        // either has to be visible to the other or nothing stops.
        let held_by_ui = CancelToken::new();
        let held_by_worker = held_by_ui.clone();

        assert!(!held_by_worker.is_cancelled());
        held_by_ui.cancel();
        assert!(held_by_worker.is_cancelled());

        // Idempotent: a second click on Cancel is not an error.
        held_by_ui.cancel();
        assert!(held_by_worker.is_cancelled());
    }
}
