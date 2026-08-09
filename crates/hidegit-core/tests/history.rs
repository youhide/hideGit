//! Merge, reset, cherry-pick, revert and the reflog, against real repositories.
//!
//! These run the actual `git` commands rather than a fake, because the whole
//! question this milestone raises is whether hideGit reads Git's *state*
//! correctly after an operation that can stop part-way. A merge that conflicts
//! and a merge that fast-forwards both come back through the same method, and
//! only a real repository tells them apart honestly.

use hidegit_core::backend::GitBackend;
use hidegit_core::error::GitError;
use hidegit_core::fixture::fixture;
use hidegit_core::model::{ObjectId, RepoState};
use hidegit_core::ops::{
    FastForward, MergeOpts, MergeOutcome, ResetMode, SequenceControl, SequenceOutcome, StartPoint,
};
use hidegit_core::process::GitCommand;

/// The commit `HEAD` points at, read with `git` rather than through the backend
/// so a test never confirms a bug by using the code under test to check itself.
fn head_of(path: &std::path::Path) -> ObjectId {
    let out = GitCommand::new("rev-parse")
        .arg("HEAD")
        .cwd(path)
        .run()
        .expect("rev-parse HEAD succeeds");
    ObjectId::from_hex(out.trimmed_stdout().trim()).expect("HEAD is a valid id")
}

fn read(path: &std::path::Path, file: &str) -> String {
    std::fs::read_to_string(path.join(file)).expect("the file exists")
}

// --- merge -------------------------------------------------------------------

#[test]
fn merging_an_ancestor_is_up_to_date() {
    let repo = fixture().commit("one").branch("side").commit("two").build();
    let backend = repo.backend();
    let before = head_of(repo.path());

    let outcome = backend
        .merge("side", &MergeOpts::default())
        .expect("merging an ancestor succeeds");

    assert_eq!(outcome, MergeOutcome::UpToDate);
    assert_eq!(
        head_of(repo.path()),
        before,
        "an up-to-date merge moves nothing"
    );
}

#[test]
fn a_mergeable_branch_ahead_fast_forwards() {
    let repo = fixture()
        .commit("one")
        .branch("side")
        .checkout("side")
        .commit("two")
        .checkout("main")
        .build();
    let backend = repo.backend();

    let outcome = backend
        .merge("side", &MergeOpts::default())
        .expect("a fast-forward merge succeeds");

    // The distinction matters to the user — a fast-forward leaves no merge
    // commit to find later — so it is reported rather than flattened into
    // "merged".
    match outcome {
        MergeOutcome::FastForwarded(id) => assert_eq!(id, head_of(repo.path())),
        other => panic!("expected a fast-forward, got {other:?}"),
    }
}

#[test]
fn no_fast_forward_makes_a_merge_commit() {
    let repo = fixture()
        .commit("one")
        .branch("side")
        .checkout("side")
        .commit("two")
        .checkout("main")
        .build();
    let backend = repo.backend();

    let outcome = backend
        .merge(
            "side",
            &MergeOpts {
                fast_forward: FastForward::Never,
                ..MergeOpts::default()
            },
        )
        .expect("--no-ff succeeds");

    match outcome {
        MergeOutcome::Merged(id) => {
            assert_eq!(id, head_of(repo.path()));
            let parents = GitCommand::new("rev-list")
                .args(["--parents", "-n", "1"])
                .revisions([id.to_hex()])
                .cwd(repo.path())
                .run()
                .expect("rev-list succeeds");
            // The commit itself plus two parents.
            assert_eq!(
                parents.trimmed_stdout().split_whitespace().count(),
                3,
                "--no-ff produces a commit with two parents"
            );
        }
        other => panic!("expected a merge commit, got {other:?}"),
    }
}

#[test]
fn ff_only_refuses_a_divergent_branch_as_an_error() {
    let repo = fixture()
        .commit("one")
        .branch("side")
        .checkout("side")
        .edit("shared.txt", "theirs\n", "theirs")
        .checkout("main")
        .edit("other.txt", "ours\n", "ours")
        .build();
    let backend = repo.backend();

    let error = backend
        .merge(
            "side",
            &MergeOpts {
                fast_forward: FastForward::Only,
                ..MergeOpts::default()
            },
        )
        .expect_err("--ff-only cannot fast-forward a divergent branch");

    // A refused `--ff-only` is a genuine failure, not a conflict: nothing is
    // conflicted and there is nothing to resolve. It must not be misreported as
    // one, or the UI would open a resolver over an unchanged worktree.
    assert!(
        matches!(error, GitError::Command { .. }),
        "expected Git's own refusal, got {error:?}"
    );
    assert_eq!(backend.repo_state().expect("state reads"), RepoState::Clean);
}

#[test]
fn a_conflicting_merge_is_an_outcome_not_an_error() {
    let repo = fixture()
        .commit("one")
        .edit("shared.txt", "base\n", "base")
        .branch("side")
        .checkout("side")
        .edit("shared.txt", "theirs\n", "theirs")
        .checkout("main")
        .edit("shared.txt", "ours\n", "ours")
        .build();
    let backend = repo.backend();

    let outcome = backend
        .merge("side", &MergeOpts::default())
        .expect("a conflicting merge reports rather than fails");

    match outcome {
        MergeOutcome::Conflicted(conflicts) => {
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].path, std::path::Path::new("shared.txt"));
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        backend.repo_state().expect("state reads"),
        RepoState::Merging,
        "the repository is left mid-merge for the resolver to finish"
    );
}

// --- continue and abort ------------------------------------------------------

#[test]
fn aborting_a_conflicted_merge_restores_the_previous_state() {
    let repo = fixture()
        .commit("one")
        .edit("shared.txt", "base\n", "base")
        .branch("side")
        .checkout("side")
        .edit("shared.txt", "theirs\n", "theirs")
        .checkout("main")
        .edit("shared.txt", "ours\n", "ours")
        .build();
    let backend = repo.backend();
    let before = head_of(repo.path());

    backend
        .merge("side", &MergeOpts::default())
        .expect("the merge conflicts");

    let outcome = backend
        .control_sequence(SequenceControl::Abort)
        .expect("aborting succeeds");

    assert_eq!(outcome, SequenceOutcome::Completed);
    assert_eq!(backend.repo_state().expect("state reads"), RepoState::Clean);
    assert_eq!(head_of(repo.path()), before, "abort moves HEAD back");
    assert_eq!(
        read(repo.path(), "shared.txt"),
        "ours\n",
        "abort restores the working tree, markers and all"
    );
}

#[test]
fn continuing_a_resolved_merge_completes_it() {
    let repo = fixture()
        .commit("one")
        .edit("shared.txt", "base\n", "base")
        .branch("side")
        .checkout("side")
        .edit("shared.txt", "theirs\n", "theirs")
        .checkout("main")
        .edit("shared.txt", "ours\n", "ours")
        .build();
    let backend = repo.backend();

    backend
        .merge("side", &MergeOpts::default())
        .expect("the merge conflicts");

    // What the resolver will do: write the resolved contents, then stage them.
    std::fs::write(repo.path().join("shared.txt"), "resolved\n").expect("the write succeeds");
    backend
        .stage(&[std::path::Path::new("shared.txt")])
        .expect("staging the resolution succeeds");

    let outcome = backend
        .control_sequence(SequenceControl::Continue)
        .expect("continuing succeeds");

    assert_eq!(outcome, SequenceOutcome::Completed);
    assert_eq!(backend.repo_state().expect("state reads"), RepoState::Clean);
    assert_eq!(read(repo.path(), "shared.txt"), "resolved\n");
}

#[test]
fn continuing_with_the_conflict_still_unresolved_reports_it_again() {
    let repo = fixture()
        .commit("one")
        .edit("shared.txt", "base\n", "base")
        .branch("side")
        .checkout("side")
        .edit("shared.txt", "theirs\n", "theirs")
        .checkout("main")
        .edit("shared.txt", "ours\n", "ours")
        .build();
    let backend = repo.backend();

    backend
        .merge("side", &MergeOpts::default())
        .expect("the merge conflicts");

    // Continuing without staging anything: the user clicked Continue too early.
    // Git refuses, and the repository stays exactly where it was — which is a
    // state to report, not an error to raise.
    let outcome = backend
        .control_sequence(SequenceControl::Continue)
        .expect("an early continue reports rather than fails");

    match outcome {
        SequenceOutcome::Stopped { conflicts, .. } => assert_eq!(conflicts.len(), 1),
        other => panic!("expected to still be stopped, got {other:?}"),
    }
    assert_eq!(
        backend.repo_state().expect("state reads"),
        RepoState::Merging
    );
}

#[test]
fn continuing_a_clean_repository_is_refused() {
    let repo = fixture().commit("one").build();
    let backend = repo.backend();

    let error = backend
        .control_sequence(SequenceControl::Continue)
        .expect_err("there is nothing to continue");

    assert!(
        matches!(error, GitError::NothingInProgress(RepoState::Clean)),
        "expected a refusal naming the state, got {error:?}"
    );
}

#[test]
fn skipping_a_merge_is_refused_before_git_sees_it() {
    let repo = fixture()
        .commit("one")
        .edit("shared.txt", "base\n", "base")
        .branch("side")
        .checkout("side")
        .edit("shared.txt", "theirs\n", "theirs")
        .checkout("main")
        .edit("shared.txt", "ours\n", "ours")
        .build();
    let backend = repo.backend();

    backend
        .merge("side", &MergeOpts::default())
        .expect("the merge conflicts");

    let error = backend
        .control_sequence(SequenceControl::Skip)
        .expect_err("a merge has a single step");

    assert!(matches!(error, GitError::NotSkippable), "got {error:?}");
    assert_eq!(
        backend.repo_state().expect("state reads"),
        RepoState::Merging,
        "the refusal leaves the merge exactly as it was"
    );
}

// --- reset -------------------------------------------------------------------

#[test]
fn a_soft_reset_keeps_the_change_staged() {
    let repo = fixture()
        .commit("one")
        .edit("file.txt", "first\n", "one")
        .edit("file.txt", "second\n", "two")
        .build();
    let backend = repo.backend();

    backend
        .reset(&StartPoint::Ref("HEAD~1".to_owned()), ResetMode::Soft)
        .expect("a soft reset succeeds");

    let status = backend.status().expect("status reads");
    assert_eq!(status.staged.len(), 1, "the commit's change is now staged");
    assert!(status.unstaged.is_empty());
    assert_eq!(read(repo.path(), "file.txt"), "second\n");
}

#[test]
fn a_mixed_reset_leaves_the_change_unstaged() {
    let repo = fixture()
        .commit("one")
        .edit("file.txt", "first\n", "one")
        .edit("file.txt", "second\n", "two")
        .build();
    let backend = repo.backend();

    backend
        .reset(&StartPoint::Ref("HEAD~1".to_owned()), ResetMode::Mixed)
        .expect("a mixed reset succeeds");

    let status = backend.status().expect("status reads");
    assert!(status.staged.is_empty());
    assert_eq!(status.unstaged.len(), 1);
    assert_eq!(read(repo.path(), "file.txt"), "second\n");
}

#[test]
fn a_hard_reset_discards_the_change() {
    let repo = fixture()
        .commit("one")
        .edit("file.txt", "first\n", "one")
        .edit("file.txt", "second\n", "two")
        .build();
    let backend = repo.backend();

    backend
        .reset(&StartPoint::Ref("HEAD~1".to_owned()), ResetMode::Hard)
        .expect("a hard reset succeeds");

    let status = backend.status().expect("status reads");
    assert!(status.staged.is_empty());
    assert!(status.unstaged.is_empty());
    assert_eq!(
        read(repo.path(), "file.txt"),
        "first\n",
        "a hard reset takes the working tree with it"
    );
}

#[test]
fn only_a_hard_reset_is_marked_destructive() {
    // The UI reads this to decide which confirmation to show, and getting it
    // wrong in either direction is bad: a missing warning loses work, and a
    // spurious one teaches people to click through warnings.
    assert!(!ResetMode::Soft.is_destructive());
    assert!(!ResetMode::Mixed.is_destructive());
    assert!(ResetMode::Hard.is_destructive());
}

// --- cherry-pick and revert --------------------------------------------------

#[test]
fn cherry_picking_applies_the_commit_here() {
    let repo = fixture()
        .commit("one")
        .branch("side")
        .checkout("side")
        .edit("picked.txt", "picked\n", "pick me")
        .checkout("main")
        .build();
    let backend = repo.backend();

    let side = GitCommand::new("rev-parse")
        .arg("--verify")
        .revisions(["side"])
        .cwd(repo.path())
        .run()
        .expect("rev-parse side succeeds");
    let id = ObjectId::from_hex(side.trimmed_stdout().trim()).expect("a valid id");

    let outcome = backend
        .cherry_pick(&[id])
        .expect("the cherry-pick succeeds");

    assert_eq!(outcome, SequenceOutcome::Completed);
    assert_eq!(read(repo.path(), "picked.txt"), "picked\n");
    assert_eq!(backend.repo_state().expect("state reads"), RepoState::Clean);
}

#[test]
fn cherry_picking_nothing_is_not_an_error() {
    let repo = fixture().commit("one").build();
    let backend = repo.backend();

    // An empty selection reaches here from a UI that allows one, and Git would
    // answer with a usage message about the command rather than about what was
    // asked. Nothing to do is not a failure.
    assert_eq!(
        backend.cherry_pick(&[]).expect("an empty pick succeeds"),
        SequenceOutcome::Completed
    );
}

#[test]
fn a_conflicting_cherry_pick_stops_on_the_commit() {
    let repo = fixture()
        .commit("one")
        .edit("shared.txt", "base\n", "base")
        .branch("side")
        .checkout("side")
        .edit("shared.txt", "theirs\n", "theirs")
        .checkout("main")
        .edit("shared.txt", "ours\n", "ours")
        .build();
    let backend = repo.backend();

    let side = GitCommand::new("rev-parse")
        .arg("--verify")
        .revisions(["side"])
        .cwd(repo.path())
        .run()
        .expect("rev-parse side succeeds");
    let id = ObjectId::from_hex(side.trimmed_stdout().trim()).expect("a valid id");

    let outcome = backend
        .cherry_pick(&[id])
        .expect("a conflicting pick reports rather than fails");

    match outcome {
        SequenceOutcome::Stopped { at, conflicts } => {
            assert_eq!(conflicts.len(), 1);
            // Which commit stopped is what the resolver's header says, so it
            // has to be the commit being applied and not `HEAD`.
            assert_eq!(at, id, "the sequence stopped on the commit being applied");
        }
        other => panic!("expected to stop, got {other:?}"),
    }
    assert_eq!(
        backend.repo_state().expect("state reads"),
        RepoState::CherryPicking
    );
}

#[test]
fn reverting_undoes_the_commit() {
    let repo = fixture()
        .commit("one")
        .edit("file.txt", "first\n", "one")
        .edit("file.txt", "second\n", "two")
        .build();
    let backend = repo.backend();
    let head = head_of(repo.path());

    let outcome = backend.revert(&[head]).expect("the revert succeeds");

    assert_eq!(outcome, SequenceOutcome::Completed);
    assert_eq!(
        read(repo.path(), "file.txt"),
        "first\n",
        "reverting the top commit restores what it changed"
    );
    // A revert adds a commit rather than removing one, which is the whole
    // difference from a reset and the reason both exist.
    assert_ne!(head_of(repo.path()), head);
}

// --- reflog ------------------------------------------------------------------

#[test]
fn the_reflog_records_what_moved_head() {
    let repo = fixture().commit("one").commit("two").build();
    let backend = repo.backend();
    let before = head_of(repo.path());

    backend
        .reset(&StartPoint::Ref("HEAD~1".to_owned()), ResetMode::Hard)
        .expect("the reset succeeds");

    let log = backend.reflog("HEAD", 10).expect("the reflog reads");
    let latest = log.first().expect("the reset left an entry");

    assert_eq!(
        latest.old_id, before,
        "the newest entry names where HEAD was, which is what makes it recoverable"
    );
    assert_eq!(latest.new_id, head_of(repo.path()));
    assert_eq!(latest.index, 0);
    assert!(
        latest.message.starts_with("reset:"),
        "Git's own wording is kept verbatim, got {:?}",
        latest.message
    );
}

#[test]
fn the_reflog_honours_its_limit_and_is_newest_first() {
    let repo = fixture()
        .commit("one")
        .commit("two")
        .commit("three")
        .build();
    let backend = repo.backend();

    let all = backend.reflog("HEAD", 100).expect("the reflog reads");
    assert!(all.len() >= 3, "three commits leave at least three entries");

    let limited = backend.reflog("HEAD", 2).expect("the reflog reads");
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0], all[0], "the limit takes from the newest end");
    assert_eq!(limited[1], all[1]);
    assert_eq!(limited[0].new_id, head_of(repo.path()));
}

#[test]
fn a_reflog_for_an_unknown_ref_is_an_error_not_an_empty_list() {
    let repo = fixture().commit("one").build();
    let backend = repo.backend();

    // Distinguishable on purpose: a branch that exists but has no log is an
    // empty view, while a name that resolves to nothing is a bug in whatever
    // asked for it.
    let error = backend
        .reflog("refs/heads/nope", 10)
        .expect_err("an unknown ref is an error");
    assert!(matches!(error, GitError::RefNotFound(_)), "got {error:?}");
}
