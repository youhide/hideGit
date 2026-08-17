//! Branches, remotes and the stash, exercised against real repositories.
//!
//! Every remote here is a bare repository on a local path, so fetch, push and
//! pull run through exactly the same code as a remote over SSH would — with no
//! network, no credential helper and no fixture server, which is what keeps this
//! suite hermetic on Linux, macOS and Windows alike.
//!
//! Assertions about what a write did are made against **real `git`**, on the far
//! side where it matters, rather than against hideGit's own reader. A bug shared
//! by the writer and the reader would otherwise pass.

use std::path::Path;

use hidegit_core::backend::GitBackend;
use hidegit_core::fixture::{Repo, fixture};
use hidegit_core::model::{DiffTarget, Divergence, Head, RevSpec, SubmoduleState};
use hidegit_core::ops::{
    CancelToken, CheckoutTarget, FetchOpts, ForceMode, NoProgress, ProgressSink, ProgressUpdate,
    PullOpts, PullOutcome, PushSpec, StartPoint, StashOp, StashOutcome, SubmoduleUpdate, TagSpec,
    WorktreeSpec,
};
use hidegit_core::{GitError, ObjectId};

/// The branch `HEAD` is on, or a description of why it is not on one.
fn head_branch(backend: &impl GitBackend) -> String {
    match backend.head().expect("HEAD is readable") {
        Head::Branch { name, .. } => name.short,
        Head::Unborn { name } => format!("unborn {}", name.short),
        Head::Detached { target } => format!("detached at {}", target.short(7)),
    }
}

// ---- checkout ------------------------------------------------------------

#[test]
fn checking_out_a_branch_moves_head_to_it() {
    let repo = fixture()
        .commit("A")
        .branch("feature")
        .commit("B")
        .checkout("main")
        .build();
    let backend = repo.backend();

    assert_eq!(head_branch(&backend), "main");

    backend
        .checkout(&CheckoutTarget::Branch("feature".to_owned()))
        .expect("a clean worktree switches");

    assert_eq!(head_branch(&backend), "feature");
    assert_eq!(
        repo.git(["rev-parse", "HEAD"]),
        repo.id("B").to_hex(),
        "the worktree moved too, not just the ref"
    );
}

#[test]
fn checking_out_a_commit_detaches_head_rather_than_inventing_a_branch() {
    let repo = fixture().commit("A").commit("B").build();
    let backend = repo.backend();

    backend
        .checkout(&CheckoutTarget::Commit(repo.id("A")))
        .expect("detaching is legal");

    assert!(
        matches!(backend.head().expect("HEAD is readable"), Head::Detached { target } if target == repo.id("A")),
        "got {}",
        head_branch(&backend)
    );
}

#[test]
fn a_new_branch_starts_where_it_was_told_to() {
    let repo = fixture().commit("A").commit("B").build();
    let backend = repo.backend();

    backend
        .checkout(&CheckoutTarget::NewBranch {
            name: "from-a".to_owned(),
            from: StartPoint::Commit(repo.id("A")),
        })
        .expect("a new branch at an older commit");

    assert_eq!(head_branch(&backend), "from-a");
    assert_eq!(repo.git(["rev-parse", "HEAD"]), repo.id("A").to_hex());
}

#[test]
fn a_new_branch_from_a_ref_records_the_ref_the_user_named() {
    let repo = fixture()
        .commit("A")
        .branch("release")
        .commit("B")
        .checkout("main")
        .build();
    let backend = repo.backend();

    backend
        .checkout(&CheckoutTarget::NewBranch {
            name: "hotfix".to_owned(),
            from: StartPoint::Ref("release".to_owned()),
        })
        .expect("a ref is a valid start point");

    assert_eq!(repo.git(["rev-parse", "HEAD"]), repo.id("B").to_hex());
}

#[test]
fn checking_out_a_remote_branch_creates_a_local_one_that_tracks_it() {
    // The single most common action in a day's work, and the reason
    // `TrackRemote` exists rather than being expressed as `NewBranch`: that
    // would produce a branch at the same commit with no upstream at all.
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .branch("feature")
        .commit("B")
        .build();
    repo.git(["push", "origin", "feature"]);
    repo.git(["checkout", "main"]);
    repo.git(["branch", "--delete", "--force", "feature"]);
    repo.git(["fetch", "origin"]);

    let backend = repo.backend();
    backend
        .checkout(&CheckoutTarget::TrackRemote {
            remote_ref: "origin/feature".to_owned(),
            local: "feature".to_owned(),
        })
        .expect("tracking a remote branch");

    assert_eq!(head_branch(&backend), "feature");
    assert_eq!(
        repo.git(["rev-parse", "--abbrev-ref", "feature@{upstream}"]),
        "origin/feature",
        "an upstream, which is the whole difference from a plain new branch"
    );
}

#[test]
fn a_checkout_that_would_lose_local_changes_reports_gits_own_words() {
    // hideGit does not stash on the user's behalf — that moves work somewhere
    // nobody asked for — so this has to fail, and fail legibly.
    let repo = fixture()
        .commit("A")
        .branch("feature")
        .edit("shared.txt", "from the feature branch\n", "B")
        .checkout("main")
        .edit("shared.txt", "from main\n", "C")
        .write("shared.txt", "uncommitted work\n")
        .build();
    let backend = repo.backend();

    let error = backend
        .checkout(&CheckoutTarget::Branch("feature".to_owned()))
        .expect_err("switching would overwrite an edited file");

    match error {
        GitError::Command { stderr, .. } => assert!(
            stderr.contains("shared.txt"),
            "the message has to name the file: {stderr}"
        ),
        other => panic!("expected a Command error carrying git's message, got {other:?}"),
    }
    assert_eq!(head_branch(&backend), "main", "nothing moved");
}

#[test]
fn checking_out_refuses_while_the_index_is_locked() {
    let repo = fixture().commit("A").branch("feature").build();
    let backend = repo.backend();

    let lock = backend.git_dir().join("index.lock");
    std::fs::write(&lock, b"").expect("a writable git directory");

    let error = backend
        .checkout(&CheckoutTarget::Branch("main".to_owned()))
        .expect_err("another process holds the index");

    assert!(matches!(error, GitError::IndexLocked(_)), "got {error:?}");
    assert!(lock.exists(), "the lock is reported, never deleted");
}

// ---- branch create, rename, delete ---------------------------------------

#[test]
fn creating_a_branch_does_not_switch_to_it() {
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    backend
        .create_branch("feature", &StartPoint::Head)
        .expect("creating a branch");

    assert_eq!(head_branch(&backend), "main", "create is not checkout");
    let refs = backend.refs().expect("refs are readable");
    assert!(
        refs.locals.iter().any(|b| b.name.short == "feature"),
        "the branch exists"
    );
}

#[test]
fn renaming_a_branch_keeps_its_commit_and_loses_its_old_name() {
    let repo = fixture().commit("A").branch("old-name").commit("B").build();
    let backend = repo.backend();

    backend
        .rename_branch("old-name", "new-name")
        .expect("renaming a branch");

    let refs = backend.refs().expect("refs are readable");
    let names: Vec<&str> = refs.locals.iter().map(|b| b.name.short.as_str()).collect();
    assert!(names.contains(&"new-name"), "renamed to: {names:?}");
    assert!(!names.contains(&"old-name"), "the old name is gone");
    assert_eq!(
        repo.git(["rev-parse", "new-name"]),
        repo.id("B").to_hex(),
        "a rename moves the name, not the commit"
    );
    assert_eq!(
        head_branch(&backend),
        "new-name",
        "renaming the checked-out branch takes HEAD with it"
    );
}

#[test]
fn deleting_a_merged_branch_needs_no_force() {
    let repo = fixture()
        .commit("A")
        .branch("feature")
        .commit("B")
        .checkout("main")
        .merge("feature")
        .build();
    let backend = repo.backend();

    backend
        .delete_branch("feature", false)
        .expect("a merged branch is safe to delete");

    let refs = backend.refs().expect("refs are readable");
    assert!(!refs.locals.iter().any(|b| b.name.short == "feature"));
}

#[test]
fn deleting_an_unmerged_branch_is_refused_until_it_is_forced() {
    let repo = fixture()
        .commit("A")
        .branch("feature")
        .commit("B")
        .checkout("main")
        .build();
    let backend = repo.backend();

    // The refusal is surfaced, never retried with `--force` behind the user's
    // back: losing commits has to be something they chose.
    let error = backend
        .delete_branch("feature", false)
        .expect_err("an unmerged branch is not safe to delete");
    match error {
        GitError::Command { stderr, .. } => assert!(
            stderr.contains("not fully merged"),
            "git explains why: {stderr}"
        ),
        other => panic!("expected git's refusal, got {other:?}"),
    }
    assert!(
        backend
            .refs()
            .expect("refs are readable")
            .locals
            .iter()
            .any(|b| b.name.short == "feature"),
        "the branch survived the refusal"
    );

    backend
        .delete_branch("feature", true)
        .expect("forcing is the deliberate choice");
    assert!(
        !backend
            .refs()
            .expect("refs are readable")
            .locals
            .iter()
            .any(|b| b.name.short == "feature")
    );
}

#[test]
fn a_branch_name_that_looks_like_a_flag_stays_a_name() {
    // Names come from repositories that may have been cloned from anywhere, and
    // `--` before the operands is what stops one being read as an option.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    // `git branch` rejects this name itself, which is the correct outcome — what
    // matters is that it is rejected as a *name* rather than acted on as a flag.
    let error = backend
        .create_branch("--upload-pack=touch /tmp/pwned", &StartPoint::Head)
        .expect_err("git refuses an invalid ref name");

    match error {
        GitError::Command { argv, .. } => {
            let dash = argv
                .iter()
                .position(|a| a == "--")
                .expect("`--` is present");
            assert!(
                argv[dash + 1..]
                    .iter()
                    .any(|a| a.starts_with("--upload-pack=")),
                "the hostile name sits after `--`: {argv:?}"
            );
        }
        other => panic!("expected a Command error, got {other:?}"),
    }
    assert!(
        !std::path::Path::new("/tmp/pwned").exists(),
        "nothing was executed"
    );
}

// ---- ahead and behind ----------------------------------------------------

#[test]
fn a_branch_level_with_its_upstream_is_neither_ahead_nor_behind() {
    let repo = fixture().commit("A").with_remote("origin").build();

    let divergence = repo
        .backend()
        .divergence()
        .expect("ahead/behind is readable");

    assert_eq!(
        divergence.get("refs/heads/main"),
        Some(&Divergence::default()),
        "in sync: {divergence:?}"
    );
}

#[test]
fn a_local_commit_makes_the_branch_ahead() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit("B")
        .commit("C")
        .build();

    let divergence = repo
        .backend()
        .divergence()
        .expect("ahead/behind is readable");

    assert_eq!(
        divergence.get("refs/heads/main"),
        Some(&Divergence {
            ahead: 2,
            behind: 0
        })
    );
}

#[test]
fn a_fetched_remote_commit_makes_the_branch_behind() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "made-elsewhere")
        .build();
    // Ahead/behind compares against the *tracking* ref, so it only knows what a
    // fetch has brought back — which is why the fetch is part of the setup.
    repo.git(["fetch", "origin"]);

    let divergence = repo
        .backend()
        .divergence()
        .expect("ahead/behind is readable");

    assert_eq!(
        divergence.get("refs/heads/main"),
        Some(&Divergence {
            ahead: 0,
            behind: 1
        })
    );
}

#[test]
fn commits_on_both_sides_count_separately_rather_than_cancelling_out() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "theirs")
        .commit("mine-one")
        .commit("mine-two")
        .build();
    repo.git(["fetch", "origin"]);

    let divergence = repo
        .backend()
        .divergence()
        .expect("ahead/behind is readable");
    let main = divergence
        .get("refs/heads/main")
        .expect("main tracks origin/main");

    assert_eq!(
        *main,
        Divergence {
            ahead: 2,
            behind: 1
        }
    );
    assert!(
        main.has_diverged(),
        "both sides moved, so a push will be rejected"
    );
}

#[test]
fn a_branch_with_no_upstream_is_absent_rather_than_reported_as_in_sync() {
    // Reporting `0 ahead, 0 behind` would say "up to date with a remote", and
    // there is no remote. The sidebar has to be able to tell those apart.
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .branch("local-only")
        .commit("B")
        .build();

    let divergence = repo
        .backend()
        .divergence()
        .expect("ahead/behind is readable");

    assert!(divergence.contains_key("refs/heads/main"));
    assert!(
        !divergence.contains_key("refs/heads/local-only"),
        "a branch that tracks nothing has nothing to be ahead of: {divergence:?}"
    );
}

#[test]
fn a_branch_whose_upstream_was_pruned_away_is_skipped_rather_than_failing() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .branch("feature")
        .commit("B")
        .build();
    repo.git(["push", "--set-upstream", "origin", "feature"]);

    // The remote branch goes away and the tracking ref is pruned, but the
    // `branch.feature.merge` config stays behind. That is an ordinary state after
    // someone merges and deletes a PR branch, not a reason to fail every other
    // branch's count.
    repo.git(["push", "origin", "--delete", "feature"]);
    repo.git(["fetch", "--prune", "origin"]);

    let divergence = repo
        .backend()
        .divergence()
        .expect("a pruned upstream is not an error");

    assert!(!divergence.contains_key("refs/heads/feature"));
    assert!(divergence.contains_key("refs/heads/main"));
}

#[test]
fn an_unborn_repository_has_nothing_to_diverge() {
    let repo = fixture().build();

    let divergence = repo.backend().divergence().expect("no branches, no error");

    assert!(divergence.is_empty(), "got {divergence:?}");
}

#[test]
fn an_object_id_survives_the_round_trip_through_gitoxide() {
    // `divergence` hands ids back to gitoxide to walk from, so the conversion
    // being exact is load-bearing for the counts above.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    let head = backend.head().expect("HEAD is readable").target().unwrap();
    let from_git = ObjectId::from_hex(&repo.git(["rev-parse", "HEAD"])).expect("a full hash");

    assert_eq!(head, from_git);
}

#[test]
fn a_fixture_remote_is_a_real_repository_with_the_branch_pushed_to_it() {
    let repo = fixture()
        .commit("A")
        .commit("B")
        .with_remote("origin")
        .build();

    // Asserted through `git` on the bare repository itself: if `with_remote` only
    // wrote a config entry, this is where that shows up.
    let there = Repo::git_in(repo.remote_path("origin"), ["rev-parse", "refs/heads/main"]);
    let here = repo.git(["rev-parse", "refs/heads/main"]);
    assert_eq!(there, here, "the remote has the branch that was pushed");

    assert_eq!(
        repo.git(["rev-parse", "--abbrev-ref", "main@{upstream}"]),
        "origin/main",
        "`with_remote` configures tracking, so ahead/behind has something to compare"
    );
}

#[test]
fn a_commit_made_on_the_remote_is_not_in_the_local_repository_until_it_is_fetched() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "made-elsewhere")
        .build();

    let on_remote = Repo::git_in(repo.remote_path("origin"), ["rev-parse", "refs/heads/main"]);
    let tracking = repo.git(["rev-parse", "refs/remotes/origin/main"]);

    assert_ne!(
        on_remote, tracking,
        "the point of the builder: the remote has moved and hideGit does not know yet"
    );
    assert_eq!(
        repo.git([
            "rev-list",
            "--count",
            "refs/heads/main..refs/remotes/origin/main"
        ]),
        "0",
        "nothing has been fetched, so there is nothing to be behind by yet"
    );
}

#[test]
fn two_remotes_on_one_repository_stay_separate() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .with_remote("fork")
        .commit_on_remote("fork", "only-on-the-fork")
        .build();

    assert_ne!(
        repo.remote_path("origin"),
        repo.remote_path("fork"),
        "each remote is its own repository on disk"
    );
    assert_ne!(
        Repo::git_in(repo.remote_path("origin"), ["rev-parse", "refs/heads/main"]),
        Repo::git_in(repo.remote_path("fork"), ["rev-parse", "refs/heads/main"]),
        "a commit pushed to one remote did not reach the other"
    );
}

#[test]
fn a_rename_does_not_lose_the_branchs_upstream() {
    // Renaming rewrites `branch.*` in `.git/config`. gitoxide loads config when
    // the repository is opened and caches it, so a backend that does not refresh
    // that snapshot keeps answering from the old file — and the renamed branch
    // silently appears to track nothing.
    let repo = fixture().commit("A").with_remote("origin").build();
    let backend = repo.backend();

    assert!(
        backend
            .divergence()
            .expect("readable")
            .contains_key("refs/heads/main"),
        "main tracks origin/main to begin with"
    );

    backend
        .rename_branch("main", "trunk")
        .expect("renaming a branch");

    assert_eq!(
        repo.git(["rev-parse", "--abbrev-ref", "trunk@{upstream}"]),
        "origin/main",
        "git itself still knows the upstream"
    );

    let refs = backend.refs().expect("refs are readable");
    let trunk = refs
        .locals
        .iter()
        .find(|b| b.name.short == "trunk")
        .expect("the renamed branch");
    assert_eq!(
        trunk.upstream.as_deref(),
        Some("refs/remotes/origin/main"),
        "and so must hideGit, without being reopened"
    );

    assert!(
        backend
            .divergence()
            .expect("readable")
            .contains_key("refs/heads/trunk"),
        "so ahead/behind survives the rename"
    );
}

// ---- fetch, pull and push ------------------------------------------------

/// Collects progress reports, so a test can assert the sink was actually fed.
#[derive(Default)]
struct Reports(std::sync::Mutex<Vec<ProgressUpdate>>);

impl ProgressSink for Reports {
    fn report(&self, update: ProgressUpdate) {
        self.0.lock().expect("not poisoned").push(update);
    }
}

impl Reports {
    fn phases(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("not poisoned")
            .iter()
            .map(|u| u.phase.clone())
            .collect()
    }
}

#[test]
fn a_fetch_brings_the_remotes_new_commit_into_a_tracking_ref() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "theirs")
        .build();
    let backend = repo.backend();

    let outcome = backend
        .fetch(
            "origin",
            &FetchOpts::default(),
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("a local remote needs no credentials");

    assert_eq!(
        outcome.updated,
        vec!["origin/main"],
        "the summary names what moved"
    );
    // Asserted against the far side rather than hideGit's own reader: a bug
    // shared by the writer and the reader would pass otherwise.
    assert_eq!(
        repo.git(["rev-parse", "refs/remotes/origin/main"]),
        Repo::git_in(repo.remote_path("origin"), ["rev-parse", "refs/heads/main"]),
    );
}

#[test]
fn a_fetch_that_had_nothing_to_bring_back_is_not_a_failure() {
    let repo = fixture().commit("A").with_remote("origin").build();

    let outcome = repo
        .backend()
        .fetch(
            "origin",
            &FetchOpts::default(),
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("an up-to-date fetch succeeds");

    assert!(outcome.updated.is_empty(), "got {:?}", outcome.updated);
    assert!(outcome.pruned.is_empty());
}

#[test]
fn a_pruning_fetch_reports_the_tracking_ref_it_removed() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .branch("doomed")
        .commit("B")
        .build();
    repo.git(["push", "origin", "doomed"]);
    repo.git(["fetch", "origin"]);
    // Deleted *on the remote itself*, not through this repository's own push:
    // `git push --delete` prunes the local tracking ref as it goes, so there
    // would be nothing left to prune. Going behind hideGit's back is also what
    // actually happens when someone merges and deletes a pull request branch.
    Repo::git_in(
        repo.remote_path("origin"),
        ["branch", "--delete", "--force", "doomed"],
    );

    let outcome = repo
        .backend()
        .fetch(
            "origin",
            &FetchOpts {
                prune: true,
                ..FetchOpts::default()
            },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("pruning succeeds");

    assert_eq!(outcome.pruned, vec!["origin/doomed"]);
}

#[test]
fn a_fetch_reports_progress_in_real_units() {
    // `UI_SPEC` requires a real unit rather than an indeterminate spinner, so the
    // sink has to actually be fed. A local remote is fast, but `--progress` still
    // reports its phases because stderr is a pipe rather than a terminal.
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "theirs")
        .build();

    let reports = Reports::default();
    repo.backend()
        .fetch(
            "origin",
            &FetchOpts::default(),
            &reports,
            &CancelToken::new(),
        )
        .expect("fetching");

    let phases = reports.phases();
    assert!(
        !phases.is_empty(),
        "a fetch that transferred an object reported nothing"
    );
    assert!(
        phases.iter().any(|p| p.contains("objects")),
        "phases should name objects: {phases:?}"
    );
}

#[test]
fn a_fetch_cancelled_before_it_starts_does_not_touch_the_remote() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "theirs")
        .build();
    let before = repo.git(["rev-parse", "refs/remotes/origin/main"]);

    let cancel = CancelToken::new();
    cancel.cancel();
    let error = repo
        .backend()
        .fetch("origin", &FetchOpts::default(), &NoProgress, &cancel)
        .expect_err("a cancelled fetch does not report success");

    assert!(matches!(error, GitError::Cancelled { .. }), "got {error:?}");
    assert_eq!(
        repo.git(["rev-parse", "refs/remotes/origin/main"]),
        before,
        "nothing was fetched"
    );
}

#[test]
fn a_push_puts_the_local_commit_on_the_remote() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit("B")
        .build();
    let backend = repo.backend();

    let outcome = backend
        .push(
            "origin",
            &PushSpec {
                refspec: "refs/heads/main:refs/heads/main".to_owned(),
                force: ForceMode::None,
                set_upstream: false,
            },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("a fast-forward push");

    // The names are as Git printed them, which is the short form — these are for
    // showing a person, not for resolving.
    assert_eq!(outcome.updated, vec!["main"]);
    assert!(outcome.rejected.is_empty());
    assert_eq!(
        Repo::git_in(repo.remote_path("origin"), ["rev-parse", "refs/heads/main"]),
        repo.id("B").to_hex(),
        "the remote really has it"
    );
}

#[test]
fn a_push_of_a_new_branch_can_set_its_upstream_in_the_same_command() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .branch("feat/graph")
        .commit("B")
        .build();

    repo.backend()
        .push(
            "origin",
            &PushSpec {
                refspec: "refs/heads/feat/graph:refs/heads/feat/graph".to_owned(),
                force: ForceMode::None,
                set_upstream: true,
            },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("pushing a new branch");

    assert_eq!(
        repo.git(["rev-parse", "--abbrev-ref", "feat/graph@{upstream}"]),
        "origin/feat/graph",
        "so the sidebar has ahead/behind from the first push"
    );
    // The upstream lives in `.git/config`, which gitoxide caches from the moment
    // the repository was opened — so this is the read that would go stale if a
    // push did not drop that snapshot.
    let refs = repo.backend().refs().expect("refs are readable");
    assert_eq!(
        refs.locals
            .iter()
            .find(|b| b.name.short == "feat/graph")
            .and_then(|b| b.upstream.as_deref()),
        Some("refs/remotes/origin/feat/graph"),
    );
}

#[test]
fn a_rejected_push_reports_gits_own_hint_rather_than_forcing() {
    // Losing the remote's commits has to be a deliberate choice, so the refusal
    // is surfaced verbatim — Git's hint says exactly what to do about it.
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "theirs")
        .commit("mine")
        .build();
    let on_remote_before =
        Repo::git_in(repo.remote_path("origin"), ["rev-parse", "refs/heads/main"]);

    let error = repo
        .backend()
        .push(
            "origin",
            &PushSpec {
                refspec: "refs/heads/main:refs/heads/main".to_owned(),
                force: ForceMode::None,
                set_upstream: false,
            },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect_err("a non-fast-forward push is refused");

    match error {
        GitError::Command { stderr, .. } => assert!(
            stderr.contains("rejected") || stderr.contains("fetch first"),
            "git's own words must survive: {stderr}"
        ),
        other => panic!("expected a Command error, got {other:?}"),
    }
    assert_eq!(
        Repo::git_in(repo.remote_path("origin"), ["rev-parse", "refs/heads/main"]),
        on_remote_before,
        "the remote is untouched"
    );
}

#[test]
fn force_with_lease_refuses_when_the_remote_moved_since_the_last_fetch() {
    // The whole point of a lease: forcing past someone else's commit that you
    // have never seen is exactly what plain `--force` would do and this must not.
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "theirs")
        .commit("mine")
        .build();

    let error = repo
        .backend()
        .push(
            "origin",
            &PushSpec {
                refspec: "refs/heads/main:refs/heads/main".to_owned(),
                force: ForceMode::WithLease,
                set_upstream: false,
            },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect_err("the lease is stale, so the push is refused");

    match error {
        GitError::Command { stderr, .. } => assert!(
            stderr.contains("stale info") || stderr.contains("rejected"),
            "got {stderr}"
        ),
        other => panic!("expected a Command error, got {other:?}"),
    }
}

#[test]
fn force_with_lease_succeeds_when_the_local_view_of_the_remote_is_current() {
    // Having fetched, the lease holds, and rewriting your own branch is allowed.
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .branch("feat")
        .commit("B")
        .build();
    repo.git(["push", "--set-upstream", "origin", "feat"]);
    // Rewrite the branch so the push is not a fast-forward.
    repo.git(["commit", "--amend", "--no-edit", "--allow-empty"]);

    repo.backend()
        .push(
            "origin",
            &PushSpec {
                refspec: "refs/heads/feat:refs/heads/feat".to_owned(),
                force: ForceMode::WithLease,
                set_upstream: false,
            },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("a current lease permits the rewrite");

    assert_eq!(
        Repo::git_in(repo.remote_path("origin"), ["rev-parse", "refs/heads/feat"]),
        repo.git(["rev-parse", "refs/heads/feat"]),
    );
}

#[test]
fn a_pull_that_can_fast_forward_says_so() {
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "theirs")
        .build();

    let outcome = repo
        .backend()
        .pull(&PullOpts::default(), &NoProgress, &CancelToken::new())
        .expect("pulling");

    match outcome {
        PullOutcome::FastForwarded(id) => assert_eq!(
            id.to_hex(),
            repo.git(["rev-parse", "HEAD"]),
            "the reported id is where HEAD actually ended up"
        ),
        other => panic!("expected a fast-forward, got {other:?}"),
    }
}

#[test]
fn a_pull_with_nothing_to_bring_back_says_up_to_date() {
    let repo = fixture().commit("A").with_remote("origin").build();

    let outcome = repo
        .backend()
        .pull(&PullOpts::default(), &NoProgress, &CancelToken::new())
        .expect("pulling");

    assert_eq!(outcome, PullOutcome::UpToDate, "got {outcome:?}");
}

#[test]
fn a_pull_that_has_to_merge_reports_an_integration_not_a_fast_forward() {
    // Both sides moved, so the user's own `pull.rebase` decides how — and either
    // way HEAD is not a plain fast-forward of where it was.
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "theirs")
        .edit("mine.txt", "mine\n", "mine")
        .build();
    // Pinned so the outcome does not depend on the machine's git config.
    repo.git(["config", "pull.rebase", "false"]);

    let outcome = repo
        .backend()
        .pull(&PullOpts::default(), &NoProgress, &CancelToken::new())
        .expect("a clean merge");

    match outcome {
        PullOutcome::Integrated(id) => {
            assert_eq!(id.to_hex(), repo.git(["rev-parse", "HEAD"]));
            assert_eq!(
                repo.git(["rev-list", "--count", "--merges", "HEAD", "-1"]),
                "1",
                "a merge commit is what integrated it"
            );
        }
        other => panic!("expected an integration, got {other:?}"),
    }
}

#[test]
fn a_pull_that_conflicts_is_an_outcome_and_not_an_error() {
    // Conflicts route to the resolution UI. Reporting them as a failed command
    // would put a wall of stderr in front of a state the user has to work in.
    let repo = fixture()
        .commit("A")
        .edit("shared.txt", "original\n", "base")
        .with_remote("origin")
        .commit_on_remote_edit("origin", "shared.txt", "theirs\n", "theirs")
        .edit("shared.txt", "mine\n", "mine")
        .build();
    repo.git(["config", "pull.rebase", "false"]);

    let outcome = repo
        .backend()
        .pull(&PullOpts::default(), &NoProgress, &CancelToken::new())
        .expect("a conflict is not an error");

    match outcome {
        PullOutcome::Conflicted(conflicts) => {
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].path, std::path::Path::new("shared.txt"));
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        repo.backend().repo_state().expect("readable"),
        hidegit_core::model::RepoState::Merging,
        "and the repository says what it is in the middle of"
    );
}

#[test]
fn pushing_refuses_nothing_but_still_drops_stale_reads_when_it_fails() {
    // A failed push may still have written config — `--set-upstream` does — so the
    // gitoxide snapshot has to be dropped either way.
    let repo = fixture()
        .commit("A")
        .with_remote("origin")
        .commit_on_remote("origin", "theirs")
        .commit("mine")
        .build();
    let backend = repo.backend();
    let before = backend.commit_count(&RevSpec::All).expect("readable");

    let _ = backend.push(
        "origin",
        &PushSpec {
            refspec: "refs/heads/main:refs/heads/main".to_owned(),
            force: ForceMode::None,
            set_upstream: false,
        },
        &NoProgress,
        &CancelToken::new(),
    );

    assert_eq!(
        backend.commit_count(&RevSpec::All).expect("readable"),
        before,
        "the repository is readable and unchanged after a refused push"
    );
}

// ---- the stash -----------------------------------------------------------

#[test]
fn a_repository_that_has_never_stashed_has_an_empty_stash_not_an_error() {
    // There is no `refs/stash` at all, which is a normal state.
    let repo = fixture().commit("A").build();

    let stashes = repo.backend().stashes().expect("readable");

    assert!(stashes.is_empty(), "got {stashes:?}");
}

#[test]
fn stash_entries_come_back_newest_first_the_way_stash_at_zero_means() {
    let repo = fixture()
        .commit("A")
        .stash_named("one.txt", "one\n", "the older one")
        .stash_named("two.txt", "two\n", "the newer one")
        .build();

    let stashes = repo.backend().stashes().expect("readable");

    assert_eq!(stashes.len(), 2);
    assert_eq!(stashes[0].index, 0);
    assert_eq!(stashes[0].message, "the newer one");
    assert_eq!(stashes[1].index, 1);
    assert_eq!(stashes[1].message, "the older one");
}

#[test]
fn a_stash_records_the_branch_it_was_made_on() {
    let repo = fixture()
        .commit("A")
        .branch("feat/graph")
        .commit("B")
        .stash("wip.txt", "work in progress\n")
        .build();

    let stashes = repo.backend().stashes().expect("readable");

    assert_eq!(stashes[0].branch.as_deref(), Some("feat/graph"));
    assert!(
        stashes[0].message.contains("B"),
        "Git's own WIP message names the commit it was on: {}",
        stashes[0].message
    );
}

#[test]
fn a_stash_is_a_commit_so_its_contents_read_through_the_ordinary_diff() {
    // The reason `Selection::Stash` needs no new diff code: `git stash show` is a
    // commit against its first parent, which `DiffTarget::Commit` already is.
    let repo = fixture()
        .commit("A")
        .edit("tracked.txt", "original\n", "B")
        .build();
    std::fs::write(repo.path().join("tracked.txt"), "changed\n").expect("writable");
    repo.git(["stash", "push"]);

    let backend = repo.backend();
    let entry = &backend.stashes().expect("readable")[0];
    let diff = backend
        .diff(&DiffTarget::Commit(entry.id))
        .expect("a stash diffs like any commit");

    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].path, std::path::Path::new("tracked.txt"));
}

#[test]
fn stashing_takes_the_change_out_of_the_working_directory() {
    let repo = fixture()
        .commit("A")
        .edit("tracked.txt", "original\n", "B")
        .build();
    std::fs::write(repo.path().join("tracked.txt"), "changed\n").expect("writable");

    let backend = repo.backend();
    let outcome = backend
        .stash(&StashOp::Push {
            message: Some("a message the user typed".to_owned()),
            include_untracked: false,
        })
        .expect("stashing");

    assert!(
        matches!(outcome, StashOutcome::Created(_)),
        "got {outcome:?}"
    );
    assert!(
        backend.status().expect("readable").is_clean(),
        "the working directory is clean again"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).expect("readable"),
        "original\n",
        "and the file is back to what was committed"
    );
    assert_eq!(
        backend.stashes().expect("readable")[0].message,
        "a message the user typed",
        "the message went through stdin intact"
    );
}

#[test]
fn a_stash_message_that_looks_like_a_flag_is_still_a_message() {
    // It travels on stdin rather than in argv, exactly so this cannot be read as
    // an option.
    let repo = fixture().commit("A").build();
    std::fs::write(repo.path().join("new.txt"), "untracked\n").expect("writable");

    let backend = repo.backend();
    backend
        .stash(&StashOp::Push {
            message: Some("--include-untracked --all".to_owned()),
            include_untracked: true,
        })
        .expect("stashing");

    assert_eq!(
        backend.stashes().expect("readable")[0].message,
        "--include-untracked --all"
    );
}

#[test]
fn stashing_nothing_creates_nothing_rather_than_claiming_it_did() {
    // `git stash push` with a clean worktree succeeds and creates no entry.
    // Reporting `Created` would leave the sidebar pointing at something absent.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    let outcome = backend
        .stash(&StashOp::Push {
            message: None,
            include_untracked: false,
        })
        .expect("not an error");

    assert!(
        !matches!(outcome, StashOutcome::Created(_)),
        "nothing was created, so nothing is reported as created: {outcome:?}"
    );
    assert!(backend.stashes().expect("readable").is_empty());
}

#[test]
fn applying_a_stash_restores_the_change_and_keeps_the_entry() {
    let repo = fixture()
        .commit("A")
        .edit("tracked.txt", "original\n", "B")
        .stash_named("tracked.txt", "changed\n", "wip")
        .build();
    let backend = repo.backend();

    backend
        .stash(&StashOp::Apply(0))
        .expect("applying the stash");

    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).expect("readable"),
        "changed\n",
    );
    assert_eq!(
        backend.stashes().expect("readable").len(),
        1,
        "apply keeps the entry; only pop and drop remove it"
    );
}

#[test]
fn popping_a_stash_restores_the_change_and_removes_the_entry() {
    let repo = fixture()
        .commit("A")
        .edit("tracked.txt", "original\n", "B")
        .stash_named("tracked.txt", "changed\n", "wip")
        .build();
    let backend = repo.backend();

    backend.stash(&StashOp::Pop(0)).expect("popping the stash");

    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).expect("readable"),
        "changed\n",
    );
    assert!(backend.stashes().expect("readable").is_empty());
}

#[test]
fn dropping_a_stash_removes_it_without_touching_the_working_directory() {
    let repo = fixture()
        .commit("A")
        .edit("tracked.txt", "original\n", "B")
        .stash_named("tracked.txt", "changed\n", "wip")
        .build();
    let backend = repo.backend();

    backend
        .stash(&StashOp::Drop(0))
        .expect("dropping the stash");

    assert!(backend.stashes().expect("readable").is_empty());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).expect("readable"),
        "original\n",
        "dropping discards the change rather than restoring it"
    );
}

#[test]
fn the_right_entry_is_dropped_when_there_are_several() {
    let repo = fixture()
        .commit("A")
        .stash_named("one.txt", "one\n", "older")
        .stash_named("two.txt", "two\n", "newer")
        .build();
    let backend = repo.backend();

    // `stash@{1}` is the older one, which is what index 1 has to mean.
    backend.stash(&StashOp::Drop(1)).expect("dropping");

    let left = backend.stashes().expect("readable");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].message, "newer");
}

#[test]
fn a_stash_that_conflicts_is_an_outcome_and_not_an_error() {
    // The entry survives and the worktree has markers in it, which is a state the
    // user has to work in rather than an error to dismiss.
    let repo = fixture()
        .commit("A")
        .edit("shared.txt", "original\n", "B")
        .stash_named("shared.txt", "from the stash\n", "wip")
        .edit("shared.txt", "from a commit\n", "C")
        .build();
    let backend = repo.backend();

    let outcome = backend
        .stash(&StashOp::Pop(0))
        .expect("a conflict is not an error");

    match outcome {
        StashOutcome::Conflicted(conflicts) => {
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].path, std::path::Path::new("shared.txt"));
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        backend.stashes().expect("readable").len(),
        1,
        "a conflicted pop keeps the entry, which is Git's own behaviour"
    );
}

#[test]
fn stashing_refuses_while_the_index_is_locked() {
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    let lock = backend.git_dir().join("index.lock");
    std::fs::write(&lock, b"").expect("a writable git directory");

    let error = backend
        .stash(&StashOp::Push {
            message: None,
            include_untracked: false,
        })
        .expect_err("another process holds the index");

    assert!(matches!(error, GitError::IndexLocked(_)), "got {error:?}");
    assert!(lock.exists(), "the lock is reported, never deleted");
}

// ---- named remotes -------------------------------------------------------

#[test]
fn a_remote_that_has_never_been_fetched_is_still_a_remote() {
    // The reason `remotes()` exists separately from `Refs::remotes`: this one has
    // no tracking refs at all, and leaving it out would say it does not exist.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    backend
        .add_remote("upstream", "https://example.invalid/repo.git")
        .expect("adding a remote");

    let remotes = backend.remotes().expect("readable");
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name, "upstream");
    assert_eq!(remotes[0].fetch_url, "https://example.invalid/repo.git");
    assert!(
        backend.refs().expect("readable").remotes.is_empty(),
        "and it has no tracking refs to be found under"
    );
}

#[test]
fn remotes_are_listed_by_name_in_order() {
    let repo = fixture().commit("A").with_remote("origin").build();
    let backend = repo.backend();
    backend
        .add_remote("fork", "https://example.invalid/fork.git")
        .expect("adding a remote");

    let remotes = backend.remotes().expect("readable");
    let names: Vec<&str> = remotes.iter().map(|r| r.name.as_str()).collect();

    assert_eq!(names, vec!["fork", "origin"]);
}

#[test]
fn a_push_url_is_reported_only_when_it_actually_differs() {
    // gitoxide reports the fetch URL for both directions when no `pushurl` is set,
    // and showing the same string twice would imply a distinction that is not
    // there.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();
    backend
        .add_remote("origin", "https://example.invalid/repo.git")
        .expect("adding a remote");

    assert_eq!(
        backend.remotes().expect("readable")[0].push_url,
        None,
        "one URL, so nothing to distinguish"
    );

    repo.git([
        "remote",
        "set-url",
        "--push",
        "origin",
        "ssh://git@example.invalid/repo.git",
    ]);
    backend.invalidate();

    let remote = &backend.remotes().expect("readable")[0];
    assert_eq!(remote.fetch_url, "https://example.invalid/repo.git");
    assert_eq!(
        remote.push_url.as_deref(),
        Some("ssh://git@example.invalid/repo.git")
    );
}

#[test]
fn changing_a_remotes_url_is_visible_to_the_next_read() {
    // A `remote set-url` rewrites config, which gitoxide caches from the moment
    // the repository was opened — the same trap a rename fell into.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();
    backend
        .add_remote("origin", "https://example.invalid/old.git")
        .expect("adding");

    backend
        .set_remote_url("origin", "https://example.invalid/new.git")
        .expect("changing the URL");

    assert_eq!(
        backend.remotes().expect("readable")[0].fetch_url,
        "https://example.invalid/new.git",
        "without the backend being reopened"
    );
}

#[test]
fn removing_a_remote_takes_its_tracking_refs_with_it() {
    let repo = fixture().commit("A").with_remote("origin").build();
    let backend = repo.backend();
    assert!(!backend.refs().expect("readable").remotes.is_empty());

    backend.remove_remote("origin").expect("removing a remote");

    assert!(backend.remotes().expect("readable").is_empty());
    assert!(
        backend.refs().expect("readable").remotes.is_empty(),
        "the tracking refs went too"
    );
    assert!(
        backend.divergence().expect("readable").is_empty(),
        "and nothing claims to be ahead of a remote that is gone"
    );
}

#[test]
fn a_remote_url_that_looks_like_a_flag_is_recorded_as_a_url() {
    // `git remote add` does not validate a URL, it records one — so the invariant
    // is not that this is rejected but that it lands in config *as a URL* rather
    // than being read as `git remote add --upload-pack=…`, which is what `--`
    // before the operands buys.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();
    let hostile = "--upload-pack=touch /tmp/pwned-remote";

    backend
        .add_remote("origin", hostile)
        .expect("a URL is data, not an option");

    assert_eq!(
        backend.remotes().expect("readable")[0].fetch_url,
        hostile,
        "stored verbatim"
    );
    assert_eq!(
        repo.git(["config", "--get", "remote.origin.url"]),
        hostile,
        "and git agrees it is the URL"
    );
    assert!(
        !std::path::Path::new("/tmp/pwned-remote").exists(),
        "nothing was executed"
    );
}

// ---- tags ----------------------------------------------------------------

#[test]
fn a_lightweight_tag_is_just_a_ref() {
    let repo = fixture().commit("A").commit("B").build();
    let backend = repo.backend();

    backend
        .create_tag(&TagSpec {
            name: "v0.1.0".to_owned(),
            at: StartPoint::Head,
            message: None,
        })
        .expect("creating a tag");

    let tags = backend.refs().expect("readable").tags;
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name.short, "v0.1.0");
    assert_eq!(tags[0].target, repo.id("B"));
    assert!(!tags[0].annotated, "no object of its own");
}

#[test]
fn an_annotated_tag_carries_its_message() {
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    backend
        .create_tag(&TagSpec {
            name: "v1.0.0".to_owned(),
            at: StartPoint::Head,
            message: Some("the first real release".to_owned()),
        })
        .expect("creating an annotated tag");

    let tags = backend.refs().expect("readable").tags;
    assert!(tags[0].annotated, "an annotated tag has its own object");
    assert!(
        repo.git(["tag", "-n99", "--list", "v1.0.0"])
            .contains("the first real release"),
        "and git can read the message back"
    );
}

#[test]
fn a_tag_message_starting_with_a_hash_is_kept() {
    // Git strips comment lines from a message read from a file. hideGit's editor
    // is not Git's, so a line the user typed is theirs to keep.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    backend
        .create_tag(&TagSpec {
            name: "v1.0.0".to_owned(),
            at: StartPoint::Head,
            message: Some("#1234 shipped".to_owned()),
        })
        .expect("creating a tag");

    assert!(
        repo.git(["tag", "-n99", "--list", "v1.0.0"])
            .contains("#1234 shipped"),
        "got {}",
        repo.git(["tag", "-n99", "--list", "v1.0.0"])
    );
}

#[test]
fn a_tag_can_be_created_somewhere_other_than_head() {
    let repo = fixture().commit("A").commit("B").build();
    let backend = repo.backend();

    backend
        .create_tag(&TagSpec {
            name: "v0.0.1".to_owned(),
            at: StartPoint::Commit(repo.id("A")),
            message: None,
        })
        .expect("tagging an older commit");

    assert_eq!(
        backend.refs().expect("readable").tags[0].target,
        repo.id("A")
    );
}

#[test]
fn deleting_a_tag_removes_it() {
    let repo = fixture().commit("A").tag("v0.1.0").build();
    let backend = repo.backend();
    assert_eq!(backend.refs().expect("readable").tags.len(), 1);

    backend.delete_tag("v0.1.0").expect("deleting a tag");

    assert!(backend.refs().expect("readable").tags.is_empty());
}

#[test]
fn a_tag_can_be_pushed_and_deleted_on_the_remote() {
    // Tags are pushed through the ordinary push path, with a fully qualified
    // refspec — which is what stops a tag and a branch of the same name from
    // being confused for each other.
    let repo = fixture().commit("A").with_remote("origin").build();
    let backend = repo.backend();
    backend
        .create_tag(&TagSpec {
            name: "v0.1.0".to_owned(),
            at: StartPoint::Head,
            message: None,
        })
        .expect("tagging");

    backend
        .push(
            "origin",
            &PushSpec {
                refspec: "refs/tags/v0.1.0:refs/tags/v0.1.0".to_owned(),
                force: ForceMode::None,
                set_upstream: false,
            },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("pushing a tag");

    assert_eq!(
        Repo::git_in(
            repo.remote_path("origin"),
            ["rev-parse", "refs/tags/v0.1.0"]
        ),
        repo.id("A").to_hex(),
        "the remote has the tag"
    );
}

#[test]
fn creating_a_tag_refuses_while_the_index_is_locked() {
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    let lock = backend.git_dir().join("index.lock");
    std::fs::write(&lock, b"").expect("a writable git directory");

    let error = backend
        .create_tag(&TagSpec {
            name: "v0.1.0".to_owned(),
            at: StartPoint::Head,
            message: None,
        })
        .expect_err("another process holds the index");

    assert!(matches!(error, GitError::IndexLocked(_)), "got {error:?}");
    assert!(lock.exists(), "the lock is reported, never deleted");
}

// ---- submodules ----------------------------------------------------------

#[test]
fn updating_a_submodule_that_was_deinitialised_checks_it_out_again() {
    let repo = fixture()
        .commit("A")
        .with_submodule("vendor/lib")
        .deinit_submodule("vendor/lib")
        .build();
    let backend = repo.backend();

    let before = &backend.submodules().expect("readable")[0];
    assert_eq!(before.state(), SubmoduleState::Uninitialised);

    let after = backend
        .update_submodules(
            &[Path::new("vendor/lib")],
            SubmoduleUpdate {
                init: true,
                recursive: false,
            },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("an update with --init sets up a submodule");

    assert_eq!(after.len(), 1);
    assert_eq!(after[0].state(), SubmoduleState::Current);
    // Against the filesystem rather than against hideGit's own reader: a
    // checkout that reported success without producing a file would pass a test
    // that only asked the reader.
    assert!(
        repo.path().join("vendor/lib/nested.txt").is_file(),
        "the working tree is actually there"
    );
}

#[test]
fn updating_without_init_reports_a_submodule_that_did_not_move() {
    // `git submodule update` exits 0 having done nothing for a submodule that
    // was never set up. That is why the method answers with the state
    // afterwards: a `Result<(), _>` would have called this a success.
    let repo = fixture()
        .commit("A")
        .with_submodule("vendor/lib")
        .deinit_submodule("vendor/lib")
        .build();
    let backend = repo.backend();

    let after = backend
        .update_submodules(
            &[Path::new("vendor/lib")],
            SubmoduleUpdate::default(),
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("git reports no error for this, which is the point");

    assert_eq!(
        after[0].state(),
        SubmoduleState::Uninitialised,
        "nothing was set up, and the answer says so rather than implying otherwise"
    );
    assert!(
        !repo.path().join("vendor/lib/nested.txt").exists(),
        "no checkout was made"
    );
}

#[test]
fn updating_a_submodule_that_moved_puts_it_back_on_the_recorded_commit() {
    let repo = fixture()
        .commit("A")
        .with_submodule("vendor/lib")
        .commit_in_submodule("vendor/lib", "Nested change")
        .build();
    let backend = repo.backend();

    let before = &backend.submodules().expect("readable")[0];
    assert_eq!(before.state(), SubmoduleState::Moved);
    let recorded = before.recorded.expect("the gitlink is staged");

    let after = backend
        .update_submodules(
            &[],
            SubmoduleUpdate::default(),
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("an empty path list means every submodule");

    assert_eq!(after[0].state(), SubmoduleState::Current);
    assert_eq!(
        Repo::git_in(&repo.path().join("vendor/lib"), ["rev-parse", "HEAD"]),
        recorded.to_hex(),
        "real git agrees the nested checkout moved back"
    );
}

#[test]
fn updating_a_path_that_is_not_a_submodule_fails_with_gits_own_words() {
    let repo = fixture().commit("A").with_submodule("vendor/lib").build();
    let backend = repo.backend();

    let error = backend
        .update_submodules(
            &[Path::new("not-a-submodule")],
            SubmoduleUpdate {
                init: true,
                recursive: false,
            },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect_err("a path that is not a submodule is not silently ignored");

    assert!(
        error.to_string().contains("not-a-submodule"),
        "git's own message names the path, and it reaches the user: {error}"
    );
}

// ---- worktrees -----------------------------------------------------------

#[test]
fn adding_a_worktree_creates_a_checkout_on_a_new_branch() {
    let repo = fixture().commit("A").build();
    let backend = repo.backend();
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let at = scratch.path().join("side");

    backend
        .add_worktree(
            &at,
            &WorktreeSpec {
                new_branch: Some("side".to_owned()),
                start: StartPoint::Head,
            },
        )
        .expect("a worktree on a new branch");

    // Against the filesystem and real git, not against hideGit's own reader.
    assert!(at.join("A.txt").is_file(), "the checkout is actually there");
    assert_eq!(
        Repo::git_in(&at, ["rev-parse", "--abbrev-ref", "HEAD"]),
        "side",
        "git agrees the new worktree is on the new branch"
    );

    let worktrees = backend.worktrees().expect("worktrees are readable");
    assert_eq!(
        worktrees.len(),
        2,
        "the read side sees it without reopening"
    );
}

#[test]
fn adding_a_worktree_on_a_branch_checked_out_elsewhere_is_refused() {
    // The one rule worktrees have. hideGit does not paper over it: a second
    // checkout that quietly became something else is worse than a refusal.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();
    let scratch = tempfile::tempdir().expect("a temporary directory");

    let error = backend
        .add_worktree(
            &scratch.path().join("dup"),
            &WorktreeSpec {
                new_branch: None,
                start: StartPoint::Ref("main".to_owned()),
            },
        )
        .expect_err("main is already checked out in the main worktree");

    assert!(
        error.to_string().contains("main"),
        "git's own message names the branch, and it reaches the user: {error}"
    );
    assert!(
        !scratch.path().join("dup").exists(),
        "a refused add leaves nothing behind"
    );
}

#[test]
fn removing_a_worktree_takes_the_checkout_and_the_registration_with_it() {
    let repo = fixture().commit("A").with_worktree("side").build();
    let backend = repo.backend();
    let at = repo.worktree_path("side").to_path_buf();
    assert!(at.is_dir(), "the fixture made one");

    backend
        .remove_worktree(&at, false)
        .expect("a clean worktree removes without force");

    assert!(!at.exists(), "the directory went too");
    assert_eq!(
        backend.worktrees().expect("readable").len(),
        1,
        "and so did the registration"
    );
}

#[test]
fn removing_a_worktree_with_uncommitted_work_needs_force() {
    // The safe form refuses, and `force` is what the user chooses afterwards —
    // never a silent retry.
    let repo = fixture().commit("A").with_worktree("side").build();
    let backend = repo.backend();
    let at = repo.worktree_path("side").to_path_buf();
    std::fs::write(at.join("A.txt"), "edited in the other checkout\n")
        .expect("a writable worktree");

    backend
        .remove_worktree(&at, false)
        .expect_err("a dirty worktree is not removed by the safe form");
    assert!(at.is_dir(), "and nothing was taken");

    backend
        .remove_worktree(&at, true)
        .expect("force is what the user chooses afterwards");
    assert!(!at.exists());
}

#[test]
fn pruning_clears_a_registration_whose_directory_is_gone() {
    let repo = fixture()
        .commit("A")
        .with_worktree("side")
        .orphan_worktree("side")
        .build();
    let backend = repo.backend();
    assert_eq!(backend.worktrees().expect("readable").len(), 2);

    backend.prune_worktrees().expect("pruning a stale entry");

    assert_eq!(
        backend.worktrees().expect("readable").len(),
        1,
        "the stale registration was holding a branch nothing could check out"
    );
}

#[test]
fn pruning_with_nothing_stale_is_not_a_failure() {
    // "There was nothing to prune" is the answer, not an error.
    let repo = fixture().commit("A").with_worktree("side").build();
    let backend = repo.backend();

    backend
        .prune_worktrees()
        .expect("nothing stale is not an error");

    assert_eq!(
        backend.worktrees().expect("readable").len(),
        2,
        "a live worktree is not stale"
    );
}

#[test]
fn pruning_leaves_a_locked_worktree_alone_even_when_its_directory_is_gone() {
    // Which is what locking one is for: a checkout on a drive that is not
    // always plugged in looks exactly like a stale registration.
    let repo = fixture()
        .commit("A")
        .with_worktree("side")
        .lock_worktree("side", "on the external drive")
        .orphan_worktree("side")
        .build();
    let backend = repo.backend();

    backend.prune_worktrees().expect("pruning succeeds");

    assert_eq!(
        backend.worktrees().expect("readable").len(),
        2,
        "the lock is what stops it being pruned"
    );
}
