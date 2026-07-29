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

use hidegit_core::fixture::{Repo, fixture};

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
