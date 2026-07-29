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

use hidegit_core::backend::GitBackend;
use hidegit_core::fixture::{Repo, fixture};
use hidegit_core::model::{Divergence, Head, RevSpec};
use hidegit_core::ops::{
    CancelToken, CheckoutTarget, FetchOpts, ForceMode, NoProgress, ProgressSink, ProgressUpdate,
    PullOpts, PullOutcome, PushSpec, StartPoint,
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
