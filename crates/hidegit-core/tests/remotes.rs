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
use hidegit_core::model::{Divergence, Head};
use hidegit_core::ops::{CheckoutTarget, StartPoint};
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
