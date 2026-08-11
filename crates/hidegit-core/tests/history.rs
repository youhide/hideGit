//! Merge, reset, cherry-pick, revert and the reflog, against real repositories.
//!
//! These run the actual `git` commands rather than a fake, because the whole
//! question this milestone raises is whether hideGit reads Git's *state*
//! correctly after an operation that can stop part-way. A merge that conflicts
//! and a merge that fast-forwards both come back through the same method, and
//! only a real repository tells them apart honestly.

use hidegit_core::backend::GitBackend;
use hidegit_core::conflict::Resolution;
use hidegit_core::error::GitError;
use hidegit_core::fixture::fixture;
use hidegit_core::model::{ObjectId, RepoState};
use hidegit_core::ops::{
    CommitOpts, FastForward, MergeOpts, MergeOutcome, RebaseAction, RebasePlan, RebaseStep,
    ResetMode, SearchField, SearchQuery, SequenceControl, SequenceOutcome, StartPoint,
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

// --- conflict markers --------------------------------------------------------

#[test]
fn the_parser_reads_what_git_actually_wrote() {
    // The unit tests in `conflict.rs` parse strings this project wrote, which
    // proves the parser is self-consistent and nothing else. This one parses a
    // file Git produced.
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

    let content = read(repo.path(), "shared.txt");
    let file = hidegit_core::conflict::parse(&content).expect("Git's own markers parse");

    assert_eq!(file.conflict_count(), 1);
    let region = file.conflicts().next().expect("there is one");
    assert_eq!(region.ours, vec!["ours\n"]);
    assert_eq!(region.theirs, vec!["theirs\n"]);

    // Round-tripping an undecided file must be byte-for-byte, or saving a
    // half-finished resolution would rewrite lines nobody touched.
    assert_eq!(file.render(&[Resolution::Unresolved]), content);
}

#[test]
fn a_resolution_written_back_ends_the_conflict() {
    // The whole point of the parser: what it renders has to be something Git
    // accepts as a resolution, not merely something that looks resolved.
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

    let content = read(repo.path(), "shared.txt");
    let file = hidegit_core::conflict::parse(&content).expect("the markers parse");
    let resolved = file.render(&[Resolution::Theirs]);

    std::fs::write(repo.path().join("shared.txt"), &resolved).expect("the write succeeds");
    backend
        .stage(&[std::path::Path::new("shared.txt")])
        .expect("staging the resolution succeeds");

    let outcome = backend
        .control_sequence(SequenceControl::Continue)
        .expect("continuing succeeds");

    assert_eq!(outcome, SequenceOutcome::Completed);
    assert_eq!(backend.repo_state().expect("state reads"), RepoState::Clean);
    assert_eq!(read(repo.path(), "shared.txt"), "theirs\n");
}

#[test]
fn the_parser_reads_diff3_style_when_the_user_configured_it() {
    // `merge.conflictStyle` is the user's setting, so the base section is
    // present or absent depending on a config hideGit does not control.
    let repo = fixture()
        .commit("one")
        .edit("shared.txt", "base\n", "base")
        .branch("side")
        .checkout("side")
        .edit("shared.txt", "theirs\n", "theirs")
        .checkout("main")
        .edit("shared.txt", "ours\n", "ours")
        .build();

    GitCommand::new("config")
        .args(["merge.conflictStyle", "diff3"])
        .cwd(repo.path())
        .run()
        .expect("setting the conflict style succeeds");

    let backend = repo.backend();
    backend
        .merge("side", &MergeOpts::default())
        .expect("the merge conflicts");

    let content = read(repo.path(), "shared.txt");
    let file = hidegit_core::conflict::parse(&content).expect("diff3 markers parse");
    let region = file.conflicts().next().expect("there is one");

    let base = region
        .base
        .as_ref()
        .expect("diff3 carries the common ancestor, got {content:?}");
    assert_eq!(base.lines, vec!["base\n"]);
    assert!(
        !base.label.is_empty(),
        "Git labels the base section with the ancestor's short hash"
    );
    assert_eq!(file.render(&[Resolution::Unresolved]), content);
}

// --- rebase ------------------------------------------------------------------

/// Commit subjects on the current branch, oldest first.
fn subjects(path: &std::path::Path) -> Vec<String> {
    let out = GitCommand::new("log")
        .args(["--reverse", "--format=%s"])
        .cwd(path)
        .run()
        .expect("git log succeeds");
    out.trimmed_stdout().lines().map(|l| l.to_owned()).collect()
}

fn id_of(path: &std::path::Path, rev: &str) -> ObjectId {
    let out = GitCommand::new("rev-parse")
        .arg("--verify")
        .revisions([rev])
        .cwd(path)
        .run()
        .expect("rev-parse succeeds");
    ObjectId::from_hex(out.trimmed_stdout().trim()).expect("a valid id")
}

#[test]
fn a_plain_rebase_replays_the_branch() {
    let repo = fixture()
        .commit("base")
        .branch("side")
        .checkout("side")
        .edit("mine.txt", "mine\n", "mine")
        .checkout("main")
        .edit("theirs.txt", "theirs\n", "theirs")
        .checkout("side")
        .build();
    let backend = repo.backend();

    let outcome = backend
        .rebase("main", &RebasePlan::default())
        .expect("a clean rebase succeeds");

    assert_eq!(outcome, SequenceOutcome::Completed);
    assert_eq!(
        subjects(repo.path()),
        vec!["base", "theirs", "mine"],
        "the branch is replayed on top of main"
    );
}

#[test]
fn a_conflicting_rebase_stops_with_the_conflict() {
    let repo = fixture()
        .commit("base")
        .edit("shared.txt", "base\n", "shared base")
        .branch("side")
        .checkout("side")
        .edit("shared.txt", "mine\n", "mine")
        .checkout("main")
        .edit("shared.txt", "theirs\n", "theirs")
        .checkout("side")
        .build();
    let backend = repo.backend();

    let outcome = backend
        .rebase("main", &RebasePlan::default())
        .expect("a conflicting rebase reports rather than fails");

    match outcome {
        SequenceOutcome::Stopped { conflicts, .. } => {
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].path, std::path::Path::new("shared.txt"));
        }
        other => panic!("expected to stop, got {other:?}"),
    }
    assert_eq!(
        backend.repo_state().expect("state reads"),
        RepoState::Rebasing
    );

    // And it can be abandoned, which is the promise the resolver rests on.
    backend
        .control_sequence(SequenceControl::Abort)
        .expect("aborting succeeds");
    assert_eq!(backend.repo_state().expect("state reads"), RepoState::Clean);
    assert_eq!(read(repo.path(), "shared.txt"), "mine\n");
}

#[test]
fn an_interactive_plan_squashes_and_drops() {
    let repo = fixture()
        .commit("base")
        .edit("a.txt", "a\n", "keep me")
        .edit("b.txt", "b\n", "squash me")
        .edit("c.txt", "c\n", "drop me")
        .build();
    let backend = repo.backend();

    let base = id_of(repo.path(), "HEAD~3");
    let plan = RebasePlan {
        steps: vec![
            RebaseStep {
                action: RebaseAction::Pick,
                commit: id_of(repo.path(), "HEAD~2"),
            },
            RebaseStep {
                action: RebaseAction::Squash,
                commit: id_of(repo.path(), "HEAD~1"),
            },
            RebaseStep {
                action: RebaseAction::Drop,
                commit: id_of(repo.path(), "HEAD"),
            },
        ],
    };

    let outcome = backend
        .rebase(&base.to_hex(), &plan)
        .expect("the planned rebase succeeds");

    assert_eq!(outcome, SequenceOutcome::Completed);
    // "squash me" folded into "keep me", and "drop me" is gone entirely.
    assert_eq!(
        subjects(repo.path()).len(),
        2,
        "base plus the squashed pair"
    );
    assert!(repo.path().join("a.txt").exists());
    assert!(
        repo.path().join("b.txt").exists(),
        "a squash keeps the changes, only the commit goes"
    );
    assert!(
        !repo.path().join("c.txt").exists(),
        "a dropped commit takes its changes with it"
    );
}

#[test]
fn a_plan_reorders_commits() {
    let repo = fixture()
        .commit("base")
        .edit("a.txt", "a\n", "first")
        .edit("b.txt", "b\n", "second")
        .build();
    let backend = repo.backend();

    let base = id_of(repo.path(), "HEAD~2");
    // The plan is applied in the order given, which is what makes reordering a
    // plan change rather than a separate operation.
    let plan = RebasePlan {
        steps: vec![
            RebaseStep {
                action: RebaseAction::Pick,
                commit: id_of(repo.path(), "HEAD"),
            },
            RebaseStep {
                action: RebaseAction::Pick,
                commit: id_of(repo.path(), "HEAD~1"),
            },
        ],
    };

    backend
        .rebase(&base.to_hex(), &plan)
        .expect("the reordering rebase succeeds");

    assert_eq!(subjects(repo.path()), vec!["base", "second", "first"]);
}

#[test]
fn an_edit_step_stops_the_rebase_for_the_user() {
    let repo = fixture()
        .commit("base")
        .edit("a.txt", "a\n", "stop here")
        .edit("b.txt", "b\n", "and then this")
        .build();
    let backend = repo.backend();

    let base = id_of(repo.path(), "HEAD~2");
    let plan = RebasePlan {
        steps: vec![
            RebaseStep {
                action: RebaseAction::Edit,
                commit: id_of(repo.path(), "HEAD~1"),
            },
            RebaseStep {
                action: RebaseAction::Pick,
                commit: id_of(repo.path(), "HEAD"),
            },
        ],
    };

    // An `edit` step exits zero, so a backend that trusted the exit status
    // would report this finished while the repository is mid-rebase.
    let outcome = backend
        .rebase(&base.to_hex(), &plan)
        .expect("the rebase runs");

    match outcome {
        SequenceOutcome::Stopped { conflicts, .. } => {
            assert!(conflicts.is_empty(), "stopping to edit is not a conflict");
        }
        other => panic!("expected to stop on the edit step, got {other:?}"),
    }
    assert_eq!(
        backend.repo_state().expect("state reads"),
        RepoState::Rebasing
    );

    backend
        .control_sequence(SequenceControl::Continue)
        .expect("continuing finishes the rest of the plan");
    assert_eq!(backend.repo_state().expect("state reads"), RepoState::Clean);
    assert_eq!(
        subjects(repo.path()),
        vec!["base", "stop here", "and then this"]
    );
}

#[test]
fn a_commit_subject_never_reaches_the_sequence_editor() {
    // The todo list is handed to `sh` through an environment variable, so a
    // commit subject containing shell syntax is the case that would prove the
    // plan is data rather than code. It is also ordinary: plenty of real
    // subjects contain `$`, backticks or quotes.
    let repo = fixture()
        .commit("base")
        .edit(
            "a.txt",
            "a\n",
            "fix `rm -rf $HOME` in the docs; echo \"pwned\"",
        )
        .build();
    let backend = repo.backend();

    let base = id_of(repo.path(), "HEAD~1");
    let plan = RebasePlan {
        steps: vec![RebaseStep {
            action: RebaseAction::Pick,
            commit: id_of(repo.path(), "HEAD"),
        }],
    };

    backend
        .rebase(&base.to_hex(), &plan)
        .expect("a hostile subject rebases like any other");

    assert_eq!(
        subjects(repo.path()),
        vec!["base", "fix `rm -rf $HOME` in the docs; echo \"pwned\""],
        "the subject survives unchanged, having never been interpreted"
    );
}

// --- the milestone's own bar ---------------------------------------------------

/// Resolves every conflicted path with `choice`, then stages them.
///
/// What the resolver does, without the window: parse the markers, choose a
/// side, write the file back, stage it.
///
/// Which side to pass is not obvious during a rebase, and getting it wrong is
/// how this helper was written the first time. Git replays your commits *onto*
/// the upstream, so `ours` is the branch being rebased onto and `theirs` is the
/// commit being replayed — the reverse of what "ours" means during a merge.
/// Taking `ours` through a rebase therefore discards every commit it moves.
fn resolve_all(backend: &impl GitBackend, path: &std::path::Path, choice: Resolution) -> usize {
    let conflicts = backend.status().expect("status reads").conflicted;
    for conflict in &conflicts {
        let full = path.join(&conflict.path);
        let content = std::fs::read_to_string(&full).expect("the conflicted file reads");
        let file = hidegit_core::conflict::parse(&content).expect("Git's markers parse");
        let choices = vec![choice.clone(); file.conflict_count()];
        std::fs::write(&full, file.render(&choices)).expect("the write succeeds");
        backend
            .stage(&[conflict.path.as_path()])
            .expect("staging the resolution succeeds");
    }
    conflicts.len()
}

#[test]
fn a_rebase_conflicting_on_three_commits_can_be_finished_here() {
    // M5's acceptance bar, as ROADMAP.md states it. Three commits that each
    // touch the same line, rebased onto a branch that also touched it, so the
    // rebase stops three separate times.
    // Each side commit changes a different file, and main changed all three, so
    // every replayed commit conflicts on its own. Three commits against the
    // *same* file do not do this: resolving the first leaves exactly the
    // content the second expects, and the rest apply cleanly.
    let repo = fixture()
        .commit("base")
        .edit("a.txt", "base a\n", "base a")
        .edit("b.txt", "base b\n", "base b")
        .edit("c.txt", "base c\n", "base c")
        .branch("side")
        .checkout("side")
        .edit("a.txt", "side a\n", "side one")
        .edit("b.txt", "side b\n", "side two")
        .edit("c.txt", "side c\n", "side three")
        .checkout("main")
        .edit("a.txt", "main a\n", "main moved a")
        .edit("b.txt", "main b\n", "main moved b")
        .edit("c.txt", "main c\n", "main moved c")
        .checkout("side")
        .build();
    let backend = repo.backend();

    let mut outcome = backend
        .rebase("main", &RebasePlan::default())
        .expect("the rebase starts");

    let mut stops = 0;
    while let SequenceOutcome::Stopped { .. } = outcome {
        stops += 1;
        assert!(stops <= 5, "the rebase is not converging");
        assert_eq!(
            backend.repo_state().expect("state reads"),
            RepoState::Rebasing
        );

        // `Theirs` is the commit being replayed, which is what keeping your own
        // work means during a rebase.
        let resolved = resolve_all(&backend, repo.path(), Resolution::Theirs);
        assert!(resolved > 0, "a stop with nothing conflicted is a bug");

        outcome = backend
            .control_sequence(SequenceControl::Continue)
            .expect("continuing succeeds");
    }

    assert_eq!(outcome, SequenceOutcome::Completed);
    assert_eq!(
        stops, 3,
        "each of the three commits conflicts on its own, so the rebase stops three times"
    );
    assert_eq!(backend.repo_state().expect("state reads"), RepoState::Clean);
    assert_eq!(
        subjects(repo.path()),
        vec![
            "base",
            "base a",
            "base b",
            "base c",
            "main moved a",
            "main moved b",
            "main moved c",
            "side one",
            "side two",
            "side three",
        ],
        "the branch is replayed on top of main with every commit kept"
    );
    // And the resolutions are what survived, not main's side.
    assert_eq!(read(repo.path(), "a.txt"), "side a\n");
    assert_eq!(read(repo.path(), "b.txt"), "side b\n");
    assert_eq!(read(repo.path(), "c.txt"), "side c\n");
}

#[test]
fn aborting_a_rebase_part_way_restores_exactly_the_prior_state() {
    // The other half of the bar: aborting *at any point* puts the repository
    // back exactly as it was. Here that means after resolving one conflict and
    // stopping on the next, which is the case a plain abort-at-the-start test
    // would miss.
    let repo = fixture()
        .commit("base")
        .edit("a.txt", "base a\n", "base a")
        .edit("b.txt", "base b\n", "base b")
        .branch("side")
        .checkout("side")
        .edit("a.txt", "side a\n", "side one")
        .edit("b.txt", "side b\n", "side two")
        .checkout("main")
        .edit("a.txt", "main a\n", "main moved a")
        .edit("b.txt", "main b\n", "main moved b")
        .checkout("side")
        .build();
    let backend = repo.backend();

    let before_head = head_of(repo.path());
    let before_subjects = subjects(repo.path());
    let before_a = read(repo.path(), "a.txt");
    let before_b = read(repo.path(), "b.txt");

    backend
        .rebase("main", &RebasePlan::default())
        .expect("the rebase starts");
    resolve_all(&backend, repo.path(), Resolution::Theirs);
    let outcome = backend
        .control_sequence(SequenceControl::Continue)
        .expect("continuing succeeds");
    assert!(
        matches!(outcome, SequenceOutcome::Stopped { .. }),
        "the second commit conflicts too, got {outcome:?}"
    );

    backend
        .control_sequence(SequenceControl::Abort)
        .expect("aborting succeeds");

    assert_eq!(backend.repo_state().expect("state reads"), RepoState::Clean);
    assert_eq!(head_of(repo.path()), before_head);
    assert_eq!(subjects(repo.path()), before_subjects);
    assert_eq!(read(repo.path(), "a.txt"), before_a);
    assert_eq!(read(repo.path(), "b.txt"), before_b);
    let status = backend.status().expect("status reads");
    assert!(
        status.staged.is_empty() && status.unstaged.is_empty() && status.conflicted.is_empty(),
        "abort leaves nothing behind, got {status:?}"
    );
}

// --- the rebase plan's own preview ---------------------------------------------

#[test]
fn the_preview_lists_what_a_rebase_would_replay_oldest_first() {
    let repo = fixture()
        .commit("base")
        .branch("side")
        .checkout("side")
        .edit("a.txt", "a\n", "first")
        .edit("b.txt", "b\n", "second")
        .edit("c.txt", "c\n", "third")
        .checkout("main")
        .edit("m.txt", "m\n", "main moved")
        .checkout("side")
        .build();
    let backend = repo.backend();

    let preview = backend.rebase_preview("main").expect("the preview reads");

    // Oldest first, because that is todo order — the commit applied first sits
    // at the top of `git rebase --interactive`. Newest-first, like the graph,
    // would invert every reorder the user made.
    assert_eq!(
        preview
            .iter()
            .map(|c| c.summary.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second", "third"],
        "main's own commit is not replayed, and the order is the todo's"
    );
}

#[test]
fn a_branch_with_nothing_to_replay_previews_empty() {
    let repo = fixture()
        .commit("base")
        .branch("side")
        .checkout("side")
        .build();
    let backend = repo.backend();

    // Not an error: a branch level with its upstream has nothing to rebase, and
    // the editor shows that rather than refusing to open.
    assert!(
        backend
            .rebase_preview("main")
            .expect("an empty preview is not an error")
            .is_empty()
    );
}

#[test]
fn previewing_an_unknown_ref_says_which_ref() {
    let repo = fixture().commit("base").build();

    let error = repo
        .backend()
        .rebase_preview("no-such-branch")
        .expect_err("an unknown ref is an error");

    match error {
        GitError::RefNotFound(name) => assert_eq!(name, "no-such-branch"),
        other => panic!("expected RefNotFound, got {other:?}"),
    }
}

#[test]
fn the_preview_matches_what_the_plan_then_rebases() {
    // The editor builds its plan from the preview, so a preview that disagreed
    // with what `git rebase` replays would produce a plan that drops commits.
    let repo = fixture()
        .commit("base")
        .branch("side")
        .checkout("side")
        .edit("a.txt", "a\n", "first")
        .edit("b.txt", "b\n", "second")
        .checkout("main")
        .edit("m.txt", "m\n", "main moved")
        .checkout("side")
        .build();
    let backend = repo.backend();

    let preview = backend.rebase_preview("main").expect("the preview reads");
    let plan = RebasePlan {
        steps: preview
            .iter()
            .map(|c| RebaseStep {
                action: RebaseAction::Pick,
                commit: c.id,
            })
            .collect(),
    };

    let outcome = backend.rebase("main", &plan).expect("the rebase succeeds");

    assert_eq!(outcome, SequenceOutcome::Completed);
    assert_eq!(
        subjects(repo.path()),
        vec!["base", "main moved", "first", "second"],
        "every previewed commit survives, in the order the plan gave"
    );
}

// --- blame ---------------------------------------------------------------------

#[test]
fn blame_attributes_every_line_to_the_commit_that_wrote_it() {
    let repo = fixture()
        .commit("base")
        .edit("poem.txt", "one\ntwo\n", "first two lines")
        .edit("poem.txt", "one\ntwo\nthree\n", "add the third")
        .build();
    let backend = repo.backend();
    let head = head_of(repo.path());

    let blame = backend
        .blame(std::path::Path::new("poem.txt"), head)
        .expect("blame reads");

    assert_eq!(
        blame
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>(),
        vec!["one", "two", "three"],
        "one entry per line, in file order"
    );
    assert_eq!(
        blame.lines.iter().map(|l| l.lineno).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "line numbers are 1-based and count the file as of the blamed revision"
    );

    // The first two lines predate the third, so they carry an older commit.
    assert_eq!(blame.lines[0].commit, blame.lines[1].commit);
    assert_ne!(
        blame.lines[2].commit, blame.lines[0].commit,
        "the line added last is attributed to the commit that added it"
    );
    assert_eq!(
        blame.lines[2].commit, head,
        "and that commit is the most recent one"
    );
}

#[test]
fn blame_reads_the_file_as_of_the_commit_asked_for() {
    // Not as of HEAD: blaming an older revision is most of the point.
    let repo = fixture()
        .commit("base")
        .edit("poem.txt", "one\n", "just one")
        .edit("poem.txt", "one\ntwo\n", "and two")
        .build();
    let backend = repo.backend();

    let earlier = GitCommand::new("rev-parse")
        .arg("--verify")
        .revisions(["HEAD~1"])
        .cwd(repo.path())
        .run()
        .expect("rev-parse succeeds");
    let earlier = ObjectId::from_hex(earlier.trimmed_stdout().trim()).expect("a valid id");

    let blame = backend
        .blame(std::path::Path::new("poem.txt"), earlier)
        .expect("blame reads");

    assert_eq!(
        blame
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>(),
        vec!["one"],
        "the second line does not exist yet at that revision"
    );
}

#[test]
fn blaming_a_path_that_is_not_there_is_an_error_not_an_empty_answer() {
    // An empty blame and a missing file look identical in a view, and the
    // difference matters: one is a file nobody has edited, the other is a typo.
    let repo = fixture()
        .commit("base")
        .edit("poem.txt", "one\n", "one")
        .build();
    let backend = repo.backend();
    let head = head_of(repo.path());

    assert!(
        backend
            .blame(std::path::Path::new("no-such-file.txt"), head)
            .is_err()
    );
}

#[test]
fn blame_follows_a_file_through_a_rename() {
    let repo = fixture()
        .commit("base")
        .edit("before.txt", "written once\n", "write it")
        .rename("before.txt", "after.txt")
        .build();
    let backend = repo.backend();

    // The rename is staged by the fixture but not committed, so commit it.
    backend
        .create_commit("rename it", CommitOpts::default())
        .expect("the commit succeeds");
    let head = head_of(repo.path());

    let blame = backend
        .blame(std::path::Path::new("after.txt"), head)
        .expect("blame reads");

    assert_eq!(blame.lines.len(), 1);
    assert_ne!(
        blame.lines[0].commit, head,
        "the line is attributed to the commit that wrote it, not to the rename"
    );
}

// --- search --------------------------------------------------------------------

fn query(text: &str, limit: usize) -> SearchQuery {
    SearchQuery {
        text: text.to_owned(),
        limit,
    }
}

#[test]
fn search_finds_a_commit_by_its_summary_and_says_why() {
    let repo = fixture()
        .commit("add the parser")
        .commit("fix the lexer")
        .commit("document the parser")
        .build();

    let found = repo
        .backend()
        .search(&query("parser", 50))
        .expect("the search runs");

    assert_eq!(
        found
            .hits
            .iter()
            .map(|h| h.commit.summary.as_str())
            .collect::<Vec<_>>(),
        vec!["document the parser", "add the parser"],
        "newest first, and the unrelated commit is absent"
    );
    // A list that cannot say whether it matched the message or the author
    // leaves the reader to guess.
    assert!(found.hits.iter().all(|h| h.field == SearchField::Summary));
    assert!(!found.truncated);
}

#[test]
fn search_matches_the_author_and_the_hash_too() {
    let repo = fixture().commit("something").build();
    let backend = repo.backend();
    let head = head_of(repo.path());

    // The fixture commits as "hideGit Fixture".
    let by_author = backend.search(&query("fixture", 10)).expect("it runs");
    assert_eq!(by_author.hits.len(), 1);
    assert_eq!(by_author.hits[0].field, SearchField::Author);

    let by_hash = backend
        .search(&query(&head.to_hex()[..7], 10))
        .expect("it runs");
    assert_eq!(by_hash.hits.len(), 1);
    assert_eq!(by_hash.hits[0].field, SearchField::Hash);
    assert_eq!(by_hash.hits[0].commit.id, head);
}

#[test]
fn a_hash_matches_as_a_prefix_rather_than_anywhere_in_the_id() {
    // A substring match would drag unrelated commits in whenever somebody
    // searched for a short hex string, which "abc" and "dad" both are.
    let repo = fixture().commit("one").build();
    let head = head_of(repo.path());

    let middle = &head.to_hex()[10..17];
    let found = repo
        .backend()
        .search(&query(middle, 10))
        .expect("the search runs");

    assert!(
        found.hits.is_empty(),
        "matching mid-hash would make short hex searches useless"
    );
}

#[test]
fn search_says_when_it_stopped_at_the_limit() {
    let repo = fixture()
        .commit("match one")
        .commit("match two")
        .commit("match three")
        .build();

    let all = repo
        .backend()
        .search(&query("match", 50))
        .expect("the search runs");
    assert_eq!(all.hits.len(), 3);
    assert!(!all.truncated, "it reached the end of history");

    let capped = repo
        .backend()
        .search(&query("match", 2))
        .expect("the search runs");
    assert_eq!(capped.hits.len(), 2);
    assert!(
        capped.truncated,
        "these are the first matches, not the matches, and the caller has to be able to say so"
    );
}

#[test]
fn an_empty_query_finds_nothing_rather_than_everything() {
    // Reachable on every keystroke as a search box is cleared, and walking the
    // whole history to match everything would be the most expensive possible
    // way to say nothing.
    let repo = fixture().commit("one").commit("two").build();

    assert!(
        repo.backend()
            .search(&query("   ", 50))
            .expect("the search runs")
            .hits
            .is_empty()
    );
}

#[test]
fn search_ignores_case_in_both_directions() {
    let repo = fixture().commit("Fix the Parser").build();
    let backend = repo.backend();

    assert_eq!(backend.search(&query("parser", 10)).unwrap().hits.len(), 1);
    assert_eq!(backend.search(&query("FIX", 10)).unwrap().hits.len(), 1);
}
