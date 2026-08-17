//! The read half of `GitBackend`, exercised against real repositories.
//!
//! Every fixture is built programmatically by running `git`, so each test reads
//! as a description of the history it covers rather than as a reference to an
//! opaque blob checked into the repository.

use std::path::{Path, PathBuf};

use hidegit_core::backend::{GitBackend, HybridBackend};
use hidegit_core::fixture::fixture;
use hidegit_core::model::{
    ChangeStatus, ConflictKind, DiffTarget, FileDiffContent, Head, LineKind, LogPage, RefKind,
    RepoState, RevSpec, SubmoduleState,
};
use hidegit_core::{GitError, ObjectId};

#[test]
fn opening_a_directory_that_is_not_a_repository_says_so() {
    let dir = tempfile::tempdir().expect("a temporary directory");

    match HybridBackend::open(dir.path()) {
        Err(GitError::NotARepository(path)) => assert_eq!(path, dir.path()),
        Err(other) => panic!("expected NotARepository, got {other:?}"),
        Ok(_) => panic!("an empty directory is not a repository"),
    }
}

#[test]
fn opening_finds_the_repository_from_a_subdirectory() {
    let repo = fixture().commit("A").build();
    let nested = repo.path().join("deep/nested");
    std::fs::create_dir_all(&nested).expect("creating a nested directory");

    let backend = HybridBackend::open(&nested).expect("discovery searches upward");
    assert!(backend.head().is_ok());
}

#[test]
fn a_repository_with_no_commits_has_an_unborn_head() {
    let repo = fixture().build();
    let backend = repo.backend();

    match backend.head().expect("an unborn HEAD is not an error") {
        Head::Unborn { name } => assert_eq!(name.short, "main"),
        other => panic!("expected an unborn HEAD, got {other:?}"),
    }

    assert!(
        backend
            .log(&RevSpec::All, LogPage::first(10))
            .unwrap()
            .is_empty()
    );
    assert_eq!(backend.commit_count(&RevSpec::All).unwrap(), 0);
}

#[test]
fn head_reports_the_branch_it_is_attached_to() {
    let repo = fixture().commit("A").build();

    match repo.backend().head().expect("HEAD is readable") {
        Head::Branch { name, target } => {
            assert_eq!(name.short, "main");
            assert_eq!(name.kind, RefKind::LocalBranch);
            assert_eq!(target, repo.id("A"));
        }
        other => panic!("expected an attached HEAD, got {other:?}"),
    }
}

#[test]
fn a_detached_head_is_reported_as_detached() {
    let repo = fixture().commit("A").commit("B").build();
    let backend = repo.backend();

    // Detach by checking out the first commit directly.
    let first = repo.id("A");
    std::process::Command::new("git")
        .args(["checkout", "--detach", &first.to_hex()])
        .current_dir(repo.path())
        .output()
        .expect("git checkout --detach");

    let backend = HybridBackend::open(backend.workdir()).expect("reopening the repository");
    match backend.head().expect("HEAD is readable") {
        Head::Detached { target } => assert_eq!(target, first),
        other => panic!("expected a detached HEAD, got {other:?}"),
    }
}

#[test]
fn refs_are_grouped_and_tags_report_whether_they_are_annotated() {
    let repo = fixture()
        .commit("A")
        .tag("v0.1.0")
        .branch("feature")
        .commit("B")
        .annotated_tag("v0.2.0")
        .build();

    let refs = repo.backend().refs().expect("references are readable");

    let branches: Vec<&str> = refs.locals.iter().map(|b| b.name.short.as_str()).collect();
    assert_eq!(branches, vec!["feature", "main"]);

    let lightweight = refs.tags.iter().find(|t| t.name.short == "v0.1.0").unwrap();
    assert!(!lightweight.annotated);
    assert_eq!(lightweight.target, repo.id("A"));

    let annotated = refs.tags.iter().find(|t| t.name.short == "v0.2.0").unwrap();
    assert!(annotated.annotated, "an annotated tag has its own object");
    assert_eq!(
        annotated.target,
        repo.id("B"),
        "a tag's target is the commit it peels to, not the tag object"
    );
}

#[test]
fn refs_pointing_at_a_commit_become_its_badges() {
    let repo = fixture().commit("A").tag("v1").build();
    let backend = repo.backend();

    let commits = backend.log(&RevSpec::All, LogPage::first(10)).unwrap();
    let names: Vec<&str> = commits[0].refs.iter().map(|r| r.short.as_str()).collect();

    assert!(names.contains(&"main"));
    assert!(names.contains(&"v1"));
}

#[test]
fn log_returns_history_newest_first_with_parents_attached() {
    let repo = fixture().commit("A").commit("B").commit("C").build();

    let commits = repo
        .backend()
        .log(&RevSpec::All, LogPage::first(10))
        .expect("history is readable");

    let summaries: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, vec!["C", "B", "A"]);

    assert_eq!(commits[0].parents, vec![repo.id("B")]);
    assert!(commits[2].parents.is_empty(), "A is the root commit");
    assert_eq!(commits[0].author.name, "hideGit Fixture");
}

#[test]
fn a_commit_always_precedes_its_parents_across_a_merge() {
    let repo = fixture()
        .commit("A")
        .branch("feature")
        .commit("B")
        .checkout("main")
        .commit("C")
        .merge("feature")
        .build();

    let commits = repo
        .backend()
        .log(&RevSpec::All, LogPage::first(20))
        .expect("history is readable");

    let position = |summary: &str| {
        commits
            .iter()
            .position(|c| c.summary == summary)
            .unwrap_or_else(|| panic!("{summary} is missing from the log"))
    };

    assert!(position("Merge feature") < position("C"));
    assert!(position("Merge feature") < position("B"));
    assert!(position("C") < position("A"));
    assert!(position("B") < position("A"));

    let merge = &commits[position("Merge feature")];
    assert!(merge.is_merge());
    assert_eq!(merge.parents.len(), 2);
}

#[test]
fn an_octopus_merge_keeps_every_parent() {
    let repo = fixture()
        .commit("A")
        .branch("one")
        .commit("B")
        .checkout("main")
        .branch("two")
        .commit("C")
        .checkout("main")
        .commit("D")
        .merge_many(&["one", "two"])
        .build();

    let commits = repo
        .backend()
        .log(&RevSpec::All, LogPage::first(20))
        .expect("history is readable");

    let merge = commits
        .iter()
        .find(|c| c.parents.len() > 2)
        .expect("the octopus merge is present");
    assert_eq!(merge.parents.len(), 3);
}

#[test]
fn an_orphan_branch_appears_with_no_common_ancestor() {
    let repo = fixture().commit("A").orphan("docs").commit("Z").build();

    let backend = repo.backend();
    let all = backend.log(&RevSpec::All, LogPage::first(20)).unwrap();
    let summaries: Vec<&str> = all.iter().map(|c| c.summary.as_str()).collect();

    assert!(summaries.contains(&"A"));
    assert!(summaries.contains(&"Z"));

    let roots = all.iter().filter(|c| c.parents.is_empty()).count();
    assert_eq!(roots, 2, "two roots, because the branches share no history");
}

#[test]
fn a_rev_spec_narrows_the_walk_to_one_ref() {
    let repo = fixture()
        .commit("A")
        .branch("feature")
        .commit("B")
        .checkout("main")
        .commit("C")
        .build();

    let backend = repo.backend();

    let on_main = backend.log(&RevSpec::Head, LogPage::first(20)).unwrap();
    let summaries: Vec<&str> = on_main.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, vec!["C", "A"], "B is only on the feature branch");

    let on_feature = backend
        .log(&RevSpec::Ref("feature".to_owned()), LogPage::first(20))
        .unwrap();
    let summaries: Vec<&str> = on_feature.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, vec!["B", "A"]);
}

#[test]
fn an_unknown_ref_is_reported_rather_than_returning_nothing() {
    let repo = fixture().commit("A").build();

    match repo.backend().log(
        &RevSpec::Ref("no-such-branch".to_owned()),
        LogPage::first(1),
    ) {
        Err(GitError::RefNotFound(name)) => assert_eq!(name, "no-such-branch"),
        other => panic!("expected RefNotFound, got {other:?}"),
    }
}

#[test]
fn pages_partition_history_without_overlap_or_gaps() {
    let repo = fixture()
        .commit("A")
        .commit("B")
        .commit("C")
        .commit("D")
        .commit("E")
        .build();

    let backend = repo.backend();
    assert_eq!(backend.commit_count(&RevSpec::All).unwrap(), 5);

    let page = LogPage { skip: 0, limit: 2 };
    let first = backend.log(&RevSpec::All, page).unwrap();
    let second = backend.log(&RevSpec::All, page.next()).unwrap();
    let third = backend.log(&RevSpec::All, page.next().next()).unwrap();

    let all: Vec<String> = first
        .iter()
        .chain(&second)
        .chain(&third)
        .map(|c| c.summary.clone())
        .collect();
    assert_eq!(all, vec!["E", "D", "C", "B", "A"]);

    let past_the_end = backend
        .log(
            &RevSpec::All,
            LogPage {
                skip: 99,
                limit: 10,
            },
        )
        .unwrap();
    assert!(
        past_the_end.is_empty(),
        "a page past the end is empty, not an error"
    );
}

#[test]
fn commit_detail_lists_the_files_the_commit_changed() {
    let repo = fixture()
        .commit("A")
        .edit("A.txt", "one\ntwo\nthree\n", "expand A")
        .build();

    let detail = repo
        .backend()
        .commit(repo.id("expand A"))
        .expect("the commit is readable");

    assert_eq!(detail.commit.summary, "expand A");
    assert_eq!(detail.changes.len(), 1);
    assert_eq!(detail.changes[0].path, Path::new("A.txt"));
    assert_eq!(detail.changes[0].status, ChangeStatus::Modified);
    assert_eq!(detail.stats.files_changed, 1);
}

#[test]
fn a_root_commit_diffs_against_nothing_so_every_file_reads_as_added() {
    let repo = fixture().commit("A").build();

    let diff = repo
        .backend()
        .diff(&DiffTarget::Commit(repo.id("A")))
        .expect("a root commit is diffable");

    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].status, ChangeStatus::Added);
    assert_eq!(diff.stats.insertions, 1);
    assert_eq!(diff.stats.deletions, 0);
}

#[test]
fn a_nested_file_reports_the_file_and_not_the_directories_above_it() {
    // A tree diff reports every directory along the path as a change of its
    // own. Reporting them would show `a`, `a/b` and `a/b/c.txt` as three
    // modified files, each directory rendering as binary because a tree has no
    // text. Every other fixture commits at the repository root, which is
    // exactly why this went unnoticed.
    let repo = fixture()
        .edit("a/b/c.txt", "first\n", "nest")
        .edit("a/b/c.txt", "second\n", "change the nested file")
        .build();

    let diff = repo
        .backend()
        .diff(&DiffTarget::Commit(repo.id("change the nested file")))
        .expect("the commit is diffable");

    let paths: Vec<_> = diff.files.iter().map(|f| f.path.as_path()).collect();
    assert_eq!(
        paths,
        vec![std::path::Path::new("a/b/c.txt")],
        "only the file changed; `a` and `a/b` are the path to it, not changes"
    );
    assert_eq!(
        diff.stats.files_changed, 1,
        "a directory must not inflate the changed-file count"
    );
}

#[test]
fn a_modification_produces_hunks_with_line_numbers_on_both_sides() {
    let repo = fixture()
        .commit("A")
        .edit("A.txt", "first\nsecond\nthird\n", "rewrite")
        .build();

    let diff = repo
        .backend()
        .diff(&DiffTarget::Commit(repo.id("rewrite")))
        .expect("the commit is diffable");

    let FileDiffContent::Text { hunks } = &diff.files[0].content else {
        panic!("a text file must produce text hunks");
    };
    assert_eq!(hunks.len(), 1);

    let removed: Vec<&str> = hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Removed)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(removed, vec!["contents of A"]);

    let added: Vec<&str> = hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(added, vec!["first", "second", "third"]);

    for line in &hunks[0].lines {
        match line.kind {
            LineKind::Added => assert!(line.old_lineno.is_none() && line.new_lineno.is_some()),
            LineKind::Removed => assert!(line.old_lineno.is_some() && line.new_lineno.is_none()),
            LineKind::Context => assert!(line.old_lineno.is_some() && line.new_lineno.is_some()),
        }
    }

    assert_eq!(diff.stats.insertions, 3);
    assert_eq!(diff.stats.deletions, 1);
}

#[test]
fn a_binary_file_gets_a_placeholder_rather_than_an_attempt_to_render_it() {
    let repo = fixture()
        .commit("A")
        .binary("blob.bin", "add binary")
        .build();

    let diff = repo
        .backend()
        .diff(&DiffTarget::Commit(repo.id("add binary")))
        .expect("the commit is diffable");

    let file = diff
        .files
        .iter()
        .find(|f| f.path == Path::new("blob.bin"))
        .expect("the binary file is in the diff");
    assert_eq!(file.content, FileDiffContent::Binary);
}

#[test]
fn a_deletion_is_reported_as_a_deletion() {
    let repo = fixture().commit("A").commit("B").build();
    std::fs::remove_file(repo.path().join("A.txt")).expect("removing a fixture file");
    std::process::Command::new("git")
        .args(["commit", "--all", "--message", "drop A"])
        .current_dir(repo.path())
        .output()
        .expect("git commit");

    let backend = HybridBackend::open(repo.path()).expect("the repository is still valid");
    let head = backend.head().unwrap().target().unwrap();
    let diff = backend.diff(&DiffTarget::Commit(head)).unwrap();

    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].status, ChangeStatus::Deleted);
}

#[test]
fn a_range_diffs_two_arbitrary_commits() {
    let repo = fixture().commit("A").commit("B").commit("C").build();

    let diff = repo
        .backend()
        .diff(&DiffTarget::Range {
            from: repo.id("A"),
            to: repo.id("C"),
        })
        .expect("a range is diffable");

    let paths: Vec<_> = diff.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(paths, vec![Path::new("B.txt"), Path::new("C.txt")]);
}

#[test]
fn read_blob_returns_the_bytes_git_stored() {
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    let diff = backend.diff(&DiffTarget::Commit(repo.id("A"))).unwrap();
    assert_eq!(diff.files.len(), 1);

    // Resolve the blob through the tree the commit points at.
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD:A.txt"])
        .current_dir(repo.path())
        .output()
        .expect("git rev-parse");
    let hex = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let id = ObjectId::from_hex(&hex).expect("a full hash");

    let blob = backend.read_blob(id).expect("the blob is readable");
    assert_eq!(blob.bytes, b"contents of A\n");
    assert!(!blob.is_binary());
}

#[test]
fn a_clean_repository_reports_no_operation_in_progress() {
    let repo = fixture().commit("A").build();

    let state = repo.backend().repo_state().expect("state is readable");
    assert_eq!(state, RepoState::Clean);
    assert!(!state.is_in_progress());
}

#[test]
fn invalidating_the_cache_picks_up_commits_made_afterwards() {
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    assert_eq!(backend.commit_count(&RevSpec::All).unwrap(), 1);

    std::fs::write(repo.path().join("B.txt"), "b\n").expect("writing a file");
    std::process::Command::new("git")
        .args(["add", "--all"])
        .current_dir(repo.path())
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "--message", "B"])
        .current_dir(repo.path())
        .output()
        .expect("git commit");

    assert_eq!(
        backend.commit_count(&RevSpec::All).unwrap(),
        1,
        "the walk is memoised until something says it is stale"
    );

    backend.invalidate();
    assert_eq!(backend.commit_count(&RevSpec::All).unwrap(), 2);
}

#[test]
fn a_freshly_committed_repository_has_a_clean_working_directory() {
    let repo = fixture().commit("A").build();

    let status = repo.backend().status().expect("status is readable");

    assert!(status.is_clean(), "nothing was touched after the commit");
    assert_eq!(status.state, RepoState::Clean);
}

#[test]
fn a_repository_with_no_commits_still_reports_what_is_staged_for_the_first_one() {
    // There is no `HEAD` tree to diff the index against, which is the case
    // most likely to panic rather than answer.
    let repo = fixture().stage("first.txt", "initial\n").build();

    let status = repo
        .backend()
        .status()
        .expect("an unborn HEAD is not an error");

    assert_eq!(status.staged.len(), 1);
    assert_eq!(status.staged[0].status.code(), 'A');
    assert_eq!(status.staged[0].path, PathBuf::from("first.txt"));
}

#[test]
fn the_three_lists_separate_staged_from_unstaged_from_untracked() {
    let repo = fixture()
        .commit("tracked")
        .stage("staged.txt", "staged\n")
        .write("tracked.txt", "changed on disk\n")
        .write("untracked.txt", "never added\n")
        .build();

    let status = repo.backend().status().expect("status is readable");

    assert_eq!(
        status
            .staged
            .iter()
            .map(|c| (c.path.as_path(), c.status.code()))
            .collect::<Vec<_>>(),
        vec![(Path::new("staged.txt"), 'A')]
    );
    assert_eq!(
        status
            .unstaged
            .iter()
            .map(|c| (c.path.as_path(), c.status.code()))
            .collect::<Vec<_>>(),
        vec![(Path::new("tracked.txt"), 'M')]
    );
    assert_eq!(status.untracked, vec![PathBuf::from("untracked.txt")]);
    assert_eq!(status.change_count(), 3);
}

#[test]
fn a_file_changed_in_the_index_and_again_on_disk_appears_in_both_lists() {
    // Not double-counting: `staged` is what a commit would contain and
    // `unstaged` is what it would leave behind, and the staging view offers a
    // different action for each.
    let repo = fixture()
        .commit("base")
        .stage("base.txt", "staged version\n")
        .write("base.txt", "and then edited again\n")
        .build();

    let status = repo.backend().status().expect("status is readable");

    assert_eq!(status.staged.len(), 1, "the index differs from HEAD");
    assert_eq!(status.unstaged.len(), 1, "the disk differs from the index");
    assert_eq!(status.staged[0].path, PathBuf::from("base.txt"));
    assert_eq!(status.unstaged[0].path, PathBuf::from("base.txt"));
}

#[test]
fn an_ignored_file_is_not_reported_as_untracked() {
    let repo = fixture()
        .edit(".gitignore", "*.log\n", "ignore logs")
        .write("noise.log", "ignored\n")
        .write("signal.txt", "not ignored\n")
        .build();

    let status = repo.backend().status().expect("status is readable");

    assert_eq!(
        status.untracked,
        vec![PathBuf::from("signal.txt")],
        "`.gitignore` is respected rather than reimplemented"
    );
}

#[test]
fn a_deleted_file_is_an_unstaged_deletion_rather_than_a_disappearance() {
    let repo = fixture().commit("doomed").delete("doomed.txt").build();

    let status = repo.backend().status().expect("status is readable");

    assert_eq!(status.unstaged.len(), 1);
    assert_eq!(status.unstaged[0].status.code(), 'D');
}

#[test]
fn a_staged_rename_is_reported_as_one_rename_and_not_a_delete_plus_an_add() {
    let repo = fixture()
        .edit("before.txt", "enough content to match on\n", "add")
        .rename("before.txt", "after.txt")
        .build();

    let status = repo.backend().status().expect("status is readable");

    assert_eq!(status.staged.len(), 1, "a rename is a single change");
    assert_eq!(
        status.staged[0].status,
        ChangeStatus::Renamed {
            from: PathBuf::from("before.txt")
        }
    );
    assert_eq!(status.staged[0].path, PathBuf::from("after.txt"));
}

#[test]
fn a_conflicted_merge_reports_the_path_and_leaves_the_state_visible() {
    let repo = fixture()
        .edit("shared.txt", "original\n", "base")
        .branch("theirs")
        .edit("shared.txt", "their version\n", "theirs edit")
        .checkout("main")
        .edit("shared.txt", "our version\n", "our edit")
        .conflict("theirs")
        .build();

    let status = repo.backend().status().expect("status is readable");

    assert_eq!(
        status.state,
        RepoState::Merging,
        "the repository is mid-merge and must say so"
    );
    assert_eq!(status.conflicted.len(), 1);
    assert_eq!(status.conflicted[0].path, PathBuf::from("shared.txt"));
    assert_eq!(status.conflicted[0].kind, ConflictKind::BothModified);
}

#[test]
fn the_lists_are_sorted_by_path_whatever_order_the_walk_finished_in() {
    let repo = fixture()
        .commit("seed")
        .write("zebra.txt", "z\n")
        .write("alpha.txt", "a\n")
        .write("middle.txt", "m\n")
        .build();

    let status = repo.backend().status().expect("status is readable");

    let mut sorted = status.untracked.clone();
    sorted.sort();
    assert_eq!(
        status.untracked, sorted,
        "gitoxide emits these interleaved from parallel threads; the UI needs one order"
    );
}

#[test]
fn the_staged_diff_shows_the_index_against_head() {
    let repo = fixture()
        .edit("f.txt", "one\ntwo\nthree\n", "base")
        .stage("f.txt", "one\nTWO\nthree\n")
        .build();

    let diff = repo
        .backend()
        .diff(&DiffTarget::Staged)
        .expect("the index is diffable against HEAD");

    assert_eq!(diff.files.len(), 1);
    let FileDiffContent::Text { hunks } = &diff.files[0].content else {
        panic!("a text file must produce text hunks");
    };
    let added: Vec<&str> = hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(added, vec!["TWO"]);
}

#[test]
fn the_unstaged_diff_shows_the_working_tree_against_the_index() {
    let repo = fixture()
        .edit("f.txt", "one\ntwo\n", "base")
        .write("f.txt", "one\ntwo\nthree\n")
        .build();

    let diff = repo
        .backend()
        .diff(&DiffTarget::Unstaged)
        .expect("the working tree is diffable against the index");

    let FileDiffContent::Text { hunks } = &diff.files[0].content else {
        panic!("a text file must produce text hunks");
    };
    let added: Vec<&str> = hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(added, vec!["three"]);
}

#[test]
fn staging_a_change_moves_it_from_one_half_of_the_diff_to_the_other() {
    let repo = fixture()
        .edit("f.txt", "before\n", "base")
        .stage("f.txt", "after\n")
        .build();
    let backend = repo.backend();

    assert_eq!(
        backend.diff(&DiffTarget::Staged).unwrap().files.len(),
        1,
        "the change is in the index"
    );
    assert!(
        backend
            .diff(&DiffTarget::Unstaged)
            .unwrap()
            .files
            .is_empty(),
        "and the working tree matches the index, so nothing is left over"
    );
}

#[test]
fn a_file_without_a_trailing_newline_says_so_on_the_line_that_ends_it() {
    // The marker a patch needs. Losing it means a patch built from this diff
    // silently appends a newline.
    let repo = fixture()
        .edit("f.txt", "has newline\n", "base")
        .write("f.txt", "no trailing newline")
        .build();

    let diff = repo.backend().diff(&DiffTarget::Unstaged).unwrap();
    let FileDiffContent::Text { hunks } = &diff.files[0].content else {
        panic!("a text file must produce text hunks");
    };

    let added = hunks[0]
        .lines
        .iter()
        .find(|l| l.kind == LineKind::Added)
        .expect("the rewritten line is an addition");
    assert!(added.no_newline, "the new side ends without a newline");

    let removed = hunks[0]
        .lines
        .iter()
        .find(|l| l.kind == LineKind::Removed)
        .expect("the original line is a removal");
    assert!(!removed.no_newline, "the old side ended with one");
}

#[test]
fn every_backend_method_is_implemented() {
    // This used to assert the opposite: that `blame`, `merge`, `rebase` and the
    // rest reported which milestone they were waiting for. The whole surface
    // was declared from M1 and filled in milestone by milestone, and as of M6
    // the last of them — `blame` — landed. Nothing here returns
    // `NotImplementedYet` any more, so the test that policed the scaffold now
    // asserts its absence.
    let repo = fixture()
        .commit("A")
        .edit("A.txt", "one\n", "write it")
        .build();
    let backend = repo.backend();
    let head = repo.id("write it");

    let results: Vec<Result<(), GitError>> = vec![
        backend.blame(Path::new("A.txt"), head).map(|_| ()),
        backend
            .rebase("main", &hidegit_core::ops::RebasePlan::default())
            .map(|_| ()),
        backend.rebase_preview("main").map(|_| ()),
        backend.reflog("HEAD", 1).map(|_| ()),
    ];

    for result in results {
        assert!(
            !matches!(result, Err(GitError::NotImplementedYet { .. })),
            "a method still reports itself unimplemented: {result:?}"
        );
    }
}

#[test]
fn the_graph_lays_out_a_real_repository_the_way_git_describes_it() {
    let repo = fixture()
        .commit("A")
        .branch("feature")
        .commit("B")
        .checkout("main")
        .commit("C")
        .merge("feature")
        .build();

    let commits = repo
        .backend()
        .log(&RevSpec::All, LogPage::first(50))
        .expect("history is readable");
    let layout = hidegit_core::graph::layout(&commits);

    assert_eq!(layout.rows.len(), commits.len());
    assert_eq!(layout.width, 2, "one branch off the mainline is two lanes");

    let merge_row = &layout.rows[0];
    assert_eq!(merge_row.kind, hidegit_core::graph::NodeKind::Merge);
    assert_eq!(merge_row.lane, 0, "the mainline holds the leftmost lane");

    let root_row = layout.rows.last().unwrap();
    assert_eq!(root_row.kind, hidegit_core::graph::NodeKind::Root);
    assert_eq!(root_row.commit, repo.id("A"));
}

#[test]
fn a_symbolic_ref_is_a_pointer_and_is_not_listed_as_a_branch() {
    // Every clone has `origin/HEAD`, which points at another ref rather than
    // holding an object id of its own.
    let repo = fixture()
        .commit("A")
        .remote_ref("refs/remotes/origin/main")
        .symbolic_ref("refs/remotes/origin/HEAD", "refs/remotes/origin/main")
        .build();

    let refs = repo.backend().refs().expect("references are readable");

    let remotes: Vec<&str> = refs.remotes.iter().map(|b| b.name.short.as_str()).collect();
    assert_eq!(
        remotes,
        vec!["origin/main"],
        "origin/HEAD duplicates whatever it points at and is not a branch"
    );
}

#[test]
fn a_generated_history_has_the_shape_the_benchmarks_assume() {
    // The benchmark's fixture is only useful if it exercises lane allocation,
    // so this asserts it actually branches and merges rather than being one
    // straight line.
    let repo = fixture().generate(200, 50).build();
    let backend = repo.backend();

    let commits = backend
        .log(&RevSpec::All, LogPage::first(1_000))
        .expect("a generated history is readable");

    assert_eq!(commits.len(), 200);
    assert!(
        commits.iter().any(|c| c.is_merge()),
        "a history with no merges would not exercise lane allocation"
    );

    let layout = hidegit_core::graph::layout(&commits);
    assert!(
        layout.width >= 2,
        "a merged side branch needs a second lane"
    );
}

// ---- submodules ----------------------------------------------------------

#[test]
fn a_repository_with_no_submodules_lists_none() {
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    assert_eq!(
        backend
            .submodules()
            .expect("no .gitmodules is not an error"),
        Vec::new(),
        "the overwhelming majority of repositories have no .gitmodules at all"
    );
}

#[test]
fn a_submodule_is_listed_with_its_name_path_and_url() {
    let repo = fixture().commit("A").with_submodule("vendor/lib").build();
    let backend = repo.backend();

    let submodules = backend.submodules().expect("a submodule is readable");
    assert_eq!(submodules.len(), 1, "one submodule was added");

    let submodule = &submodules[0];
    assert_eq!(submodule.name, "vendor/lib");
    assert_eq!(submodule.path, PathBuf::from("vendor/lib"));
    assert_eq!(
        submodule.url,
        repo.submodule_source("vendor/lib").display().to_string(),
        "the URL is what .gitmodules records, verbatim"
    );
}

#[test]
fn a_submodule_at_the_recorded_commit_is_current() {
    let repo = fixture().commit("A").with_submodule("vendor/lib").build();
    let backend = repo.backend();

    let submodules = backend.submodules().expect("a submodule is readable");
    let submodule = &submodules[0];

    let recorded = submodule
        .recorded
        .expect("the superproject staged a gitlink");
    let checked_out = submodule
        .checked_out
        .expect("`submodule add` leaves a checkout behind");
    assert_eq!(
        recorded, checked_out,
        "a freshly added submodule is at the commit the superproject records"
    );
    assert_eq!(submodule.state(), SubmoduleState::Current);

    // Against real Git rather than against our own reader: `git submodule
    // status` prints a leading space for exactly this state.
    let status = repo.git(["submodule", "status"]);
    assert!(
        status.starts_with(' ') || status.starts_with(&recorded.to_hex()),
        "git itself calls this in sync, got {status:?}"
    );
}

#[test]
fn a_submodule_moved_off_the_recorded_commit_says_so() {
    let repo = fixture()
        .commit("A")
        .with_submodule("vendor/lib")
        .commit_in_submodule("vendor/lib", "Nested change")
        .build();
    let backend = repo.backend();

    let submodules = backend.submodules().expect("a submodule is readable");
    let submodule = &submodules[0];

    let recorded = submodule.recorded.expect("the gitlink is still staged");
    let checked_out = submodule.checked_out.expect("the checkout is still there");
    assert_ne!(
        recorded, checked_out,
        "the nested repository moved and the superproject did not"
    );
    assert_eq!(submodule.state(), SubmoduleState::Moved);

    assert_eq!(
        checked_out.to_hex(),
        hidegit_core::fixture::Repo::git_in(&repo.path().join("vendor/lib"), ["rev-parse", "HEAD"]),
        "checked_out is the nested repository's own HEAD, not the superproject's idea of it"
    );
}

#[test]
fn a_submodule_that_was_never_checked_out_is_still_listed() {
    // The state a fresh `git clone` of the superproject leaves every submodule
    // in, because clone does not clone them. Reporting it as missing rather
    // than as absent is the whole point.
    let repo = fixture()
        .commit("A")
        .with_submodule("vendor/lib")
        .deinit_submodule("vendor/lib")
        .build();
    let backend = repo.backend();

    let submodules = backend
        .submodules()
        .expect("a deinitialised submodule is readable");
    assert_eq!(submodules.len(), 1, "the .gitmodules entry did not go away");

    let submodule = &submodules[0];
    assert!(
        submodule.recorded.is_some(),
        "the superproject still records a commit for it"
    );
    assert_eq!(
        submodule.checked_out, None,
        "there is no nested repository left to ask"
    );
    assert_eq!(submodule.state(), SubmoduleState::Uninitialised);
}

#[test]
fn submodules_come_back_in_path_order() {
    let repo = fixture()
        .commit("A")
        .with_submodule("zeta")
        .with_submodule("alpha")
        .build();
    let backend = repo.backend();

    let paths: Vec<_> = backend
        .submodules()
        .expect("two submodules are readable")
        .into_iter()
        .map(|s| s.path)
        .collect();

    assert_eq!(
        paths,
        vec![PathBuf::from("alpha"), PathBuf::from("zeta")],
        ".gitmodules order is the order they were added in, which is not an order to show"
    );
}

// ---- worktrees -----------------------------------------------------------

#[test]
fn a_repository_with_no_linked_worktrees_still_lists_the_one_it_is() {
    // gitoxide counts linked worktrees only, so the main one has to be
    // prepended. An empty list here would say this repository has no checkout,
    // which is the opposite of true.
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    let worktrees = backend.worktrees().expect("worktrees are readable");
    assert_eq!(worktrees.len(), 1);
    assert!(worktrees[0].is_main);
    assert!(worktrees[0].is_current, "it is the one that was opened");
    assert!(!worktrees[0].prunable);
    assert_eq!(
        worktrees[0].locked, None,
        "the main worktree cannot be locked"
    );
    assert!(
        matches!(&worktrees[0].head, Some(Head::Branch { name, .. }) if name.short == "main"),
        "a worktree with no HEAD would be one nothing can say is holding a branch: {:?}",
        worktrees[0].head
    );
}

#[test]
fn a_linked_worktree_is_listed_after_the_main_one_with_its_own_head() {
    // The reason to read worktrees at all: a branch checked out in one cannot
    // be checked out in another, and only HEAD makes that visible.
    let repo = fixture().commit("A").with_worktree("side").build();
    let backend = repo.backend();

    let worktrees = backend.worktrees().expect("worktrees are readable");
    assert_eq!(worktrees.len(), 2);
    assert!(
        worktrees[0].is_main,
        "the main one comes first, as git lists it"
    );

    let linked = &worktrees[1];
    assert!(!linked.is_main);
    assert!(!linked.is_current, "hideGit was opened on the main one");
    assert!(
        matches!(&linked.head, Some(Head::Branch { name, .. }) if name.short == "side"),
        "the linked worktree is on its own branch: {:?}",
        linked.head
    );
}

#[test]
fn a_locked_worktree_carries_the_reason_it_was_locked_with() {
    let repo = fixture()
        .commit("A")
        .with_worktree("side")
        .lock_worktree("side", "on the external drive")
        .build();
    let backend = repo.backend();

    let worktrees = backend.worktrees().expect("worktrees are readable");
    assert_eq!(
        worktrees[1].locked.as_deref(),
        Some("on the external drive"),
        "the reason is what makes a lock actionable rather than mysterious"
    );
}

#[test]
fn a_worktree_whose_directory_is_gone_is_listed_as_prunable_rather_than_dropped() {
    // A stale registration still holds its branch, so hiding it would leave a
    // refused checkout with no visible cause anywhere.
    let repo = fixture()
        .commit("A")
        .with_worktree("side")
        .orphan_worktree("side")
        .build();
    let backend = repo.backend();

    let worktrees = backend.worktrees().expect("worktrees are readable");
    assert_eq!(worktrees.len(), 2, "the registration did not go away");
    assert!(worktrees[1].prunable);
    assert!(
        matches!(&worktrees[1].head, Some(Head::Branch { name, .. }) if name.short == "side"),
        "the branch it is still holding is the whole reason to list it, so opening it \
         strictly — and losing that — would defeat the point: {:?}",
        worktrees[1].head
    );

    // Real git agrees, and calls it the same thing.
    assert!(
        repo.git(["worktree", "list"]).contains("prunable"),
        "git itself calls this prunable"
    );
}

#[test]
fn opening_from_a_linked_worktree_still_lists_the_main_one_first() {
    // The reason the main entry comes from `main_repo` rather than from the
    // repository in hand: opened from a linked worktree, the repository in hand
    // *is* the linked one, and the main worktree would otherwise vanish from a
    // list that is supposed to show every checkout.
    let repo = fixture().commit("A").with_worktree("side").build();
    let backend =
        HybridBackend::open(repo.worktree_path("side")).expect("a linked worktree is a repository");

    let worktrees = backend.worktrees().expect("worktrees are readable");
    assert_eq!(worktrees.len(), 2);
    assert!(worktrees[0].is_main);
    assert!(
        !worktrees[0].is_current,
        "the main worktree is not the one this was opened from"
    );
    assert!(
        worktrees[1].is_current,
        "the linked one is: {:?}",
        worktrees[1].path
    );
}

// ---- Git LFS -------------------------------------------------------------

/// A pointer as `git lfs` writes one.
///
/// Written by hand rather than by `git lfs`, deliberately: a pointer is a plain
/// text file, and what hideGit has to recognise is the file — not the tool. The
/// suite therefore needs no `git-lfs` on any of the three CI platforms, and the
/// test says the same thing on a machine that has never had it installed.
fn lfs_pointer(oid: char, size: u64) -> String {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {size}\n",
        oid.to_string().repeat(64)
    )
}

#[test]
fn a_diff_between_two_pointers_reports_the_sizes_rather_than_the_pointers() {
    // What Git stores for an LFS-tracked file *is* the pointer, so the
    // alternative is three lines of `oid sha256:…` presented as the change.
    let repo = fixture()
        .edit("big.bin", &lfs_pointer('a', 1024), "add")
        .edit("big.bin", &lfs_pointer('b', 4096), "grow")
        .build();
    let backend = repo.backend();

    let diff = backend
        .diff(&DiffTarget::Commit(repo.id("grow")))
        .expect("a diff of a pointer change");
    let file = &diff.files[0];

    match &file.content {
        FileDiffContent::Lfs { old, new, .. } => {
            assert_eq!(old.as_ref().expect("the old side is a pointer").size, 1024);
            assert_eq!(new.as_ref().expect("the new side is a pointer").size, 4096);
        }
        other => panic!("expected an LFS placeholder, got {other:?}"),
    }
}

/// The full 64-character oid [`lfs_pointer`] builds for a given character.
fn lfs_oid(oid: char) -> String {
    oid.to_string().repeat(64)
}

/// The fetch state and size of the new side of `path`'s diff.
///
/// Looked up by name rather than taken as `files[0]`: a real `git lfs track`
/// commits `.gitattributes` alongside the file, and that sorts first.
fn lfs_diff_content(diff: &hidegit_core::model::Diff, path: &str) -> (Option<bool>, u64) {
    let file = diff
        .files
        .iter()
        .find(|f| f.path == Path::new(path))
        .unwrap_or_else(|| panic!("no {path} in the diff"));

    match &file.content {
        FileDiffContent::Lfs { new, fetched, .. } => (
            *fetched,
            new.as_ref().expect("the new side is a pointer").size,
        ),
        other => panic!("expected an LFS placeholder, got {other:?}"),
    }
}

#[test]
fn an_lfs_object_that_was_never_fetched_says_so() {
    // The state a clone leaves every LFS object in, and the one worth a
    // sentence: the file on disk is the pointer text, which reads as
    // corruption rather than as an absence.
    let repo = fixture()
        .edit("big.bin", &lfs_pointer('a', 4096), "add")
        .build();
    let backend = repo.backend();

    let diff = backend
        .diff(&DiffTarget::Commit(repo.id("add")))
        .expect("a diff of a pointer");

    assert_eq!(
        lfs_diff_content(&diff, "big.bin"),
        (Some(false), 4096),
        "nothing put the object in the local store"
    );
}

#[test]
fn an_lfs_object_present_in_the_local_store_is_reported_as_fetched() {
    let repo = fixture()
        .edit("big.bin", &lfs_pointer('a', 4096), "add")
        .with_lfs_object(&lfs_oid('a'), b"the real contents")
        .build();
    let backend = repo.backend();

    let diff = backend
        .diff(&DiffTarget::Commit(repo.id("add")))
        .expect("a diff of a pointer");

    assert_eq!(lfs_diff_content(&diff, "big.bin"), (Some(true), 4096));
}

#[test]
fn a_file_leaving_lfs_has_no_fetch_state_to_report() {
    // The content is in Git itself now, so "do you have it locally" is not a
    // question with an answer. `None` rather than `false`, which would read as
    // something missing.
    let repo = fixture()
        .edit("big.bin", &lfs_pointer('a', 8192), "track with lfs")
        .edit("big.bin", "the real contents\n", "untrack")
        .build();
    let backend = repo.backend();

    let diff = backend
        .diff(&DiffTarget::Commit(repo.id("untrack")))
        .expect("a diff of a file leaving LFS");

    match &diff.files[0].content {
        FileDiffContent::Lfs { new, fetched, .. } => {
            assert_eq!(*new, None, "the new side is ordinary text");
            assert_eq!(*fetched, None);
        }
        other => panic!("expected an LFS placeholder, got {other:?}"),
    }
}

#[test]
fn lfs_storage_moves_where_the_objects_are_looked_for() {
    // `lfs.storage` is a real knob and a relative one is resolved against the
    // `.git` directory — checked against git-lfs 3.7.1, not assumed.
    let repo = fixture()
        .edit("big.bin", &lfs_pointer('a', 4096), "add")
        .config("lfs.storage", "mylfs")
        .build();

    // In the default place, which `lfs.storage` has just moved away from.
    let oid = lfs_oid('a');
    let stray = repo
        .path()
        .join(".git/lfs/objects")
        .join(&oid[0..2])
        .join(&oid[2..4]);
    std::fs::create_dir_all(&stray).expect("a writable fixture");
    std::fs::write(stray.join(&oid), b"wrong place").expect("a writable fixture");

    let backend = repo.backend();
    let diff = backend
        .diff(&DiffTarget::Commit(repo.id("add")))
        .expect("a diff of a pointer");

    assert_eq!(
        lfs_diff_content(&diff, "big.bin").0,
        Some(false),
        "the object is in .git/lfs, and lfs.storage says to look in .git/mylfs"
    );
}

#[test]
#[ignore = "needs git-lfs installed; run by hand to re-check the store layout"]
fn a_real_git_lfs_store_has_the_layout_this_suite_builds_by_hand() {
    // The one test that checks the assumption everything above rests on. The
    // rest of the suite builds the store by hand so it needs no `git-lfs` on
    // any of the three CI platforms; this one runs the real tool and asserts
    // it puts the object where hideGit looks.
    let repo = fixture().commit("A").build();
    let path = repo.path();

    repo.git(["lfs", "install", "--local"]);
    repo.git(["lfs", "track", "*.bin"]);
    std::fs::write(path.join("big.bin"), vec![7u8; 5000]).expect("a writable fixture");
    repo.git(["add", "--all"]);
    repo.git(["commit", "--message", "add big"]);

    let oid = repo
        .git(["cat-file", "-p", "HEAD:big.bin"])
        .lines()
        .find_map(|line| line.strip_prefix("oid sha256:").map(str::to_owned))
        .expect("git-lfs wrote a pointer");

    let expected = path
        .join(".git/lfs/objects")
        .join(&oid[0..2])
        .join(&oid[2..4])
        .join(&oid);
    assert!(
        expected.is_file(),
        "git-lfs stores objects somewhere else now: {}",
        expected.display()
    );

    // And hideGit reads that same store through the production path, rather
    // than the two agreeing only about a directory name.
    let head = hidegit_core::model::ObjectId::from_hex(&repo.git(["rev-parse", "HEAD"]))
        .expect("git prints a full hash");
    let diff = repo
        .backend()
        .diff(&DiffTarget::Commit(head))
        .expect("a diff");
    assert_eq!(
        lfs_diff_content(&diff, "big.bin").0,
        Some(true),
        "git-lfs fetched the object on commit, so hideGit must see it"
    );
}

#[test]
fn a_file_moving_into_lfs_says_so_rather_than_showing_the_pointer_replacing_it() {
    // The content did not change, the storage did. A three-line diff of the
    // pointer would say neither.
    let repo = fixture()
        .edit("big.bin", "the real contents\n", "add")
        .edit("big.bin", &lfs_pointer('a', 8192), "track with lfs")
        .build();
    let backend = repo.backend();

    let diff = backend
        .diff(&DiffTarget::Commit(repo.id("track with lfs")))
        .expect("a diff of a file becoming tracked");

    match &diff.files[0].content {
        FileDiffContent::Lfs { old, new, .. } => {
            assert_eq!(*old, None, "the old side was ordinary text, not a pointer");
            assert_eq!(new.as_ref().expect("the new side is a pointer").size, 8192);
        }
        other => panic!("expected an LFS placeholder, got {other:?}"),
    }
}

#[test]
fn a_file_leaving_lfs_says_so_too() {
    let repo = fixture()
        .edit("big.bin", &lfs_pointer('a', 8192), "add")
        .edit("big.bin", "the real contents\n", "stop tracking")
        .build();
    let backend = repo.backend();

    let diff = backend
        .diff(&DiffTarget::Commit(repo.id("stop tracking")))
        .expect("a diff of a file leaving LFS");

    match &diff.files[0].content {
        FileDiffContent::Lfs { old, new, .. } => {
            assert_eq!(old.as_ref().expect("the old side was a pointer").size, 8192);
            assert_eq!(*new, None);
        }
        other => panic!("expected an LFS placeholder, got {other:?}"),
    }
}

#[test]
fn a_file_that_only_talks_about_lfs_still_gets_an_ordinary_diff() {
    // A README describing how the project uses LFS is a README, and hiding its
    // changes behind a placeholder would be worse than showing a pointer.
    let repo = fixture()
        .edit("README.md", "We use Git LFS for assets.\n", "add")
        // The prose contains the version line *verbatim*, which is the trap: a
        // check for "does this text mention the spec" rather than "does this
        // text begin with the spec line" would hide this file's changes.
        .edit(
            "README.md",
            "We use Git LFS for assets.\nA pointer begins \
             version https://git-lfs.github.com/spec/v1\nand names an oid.\n",
            "expand",
        )
        .build();
    let backend = repo.backend();

    let diff = backend
        .diff(&DiffTarget::Commit(repo.id("expand")))
        .expect("an ordinary diff");

    assert!(
        matches!(&diff.files[0].content, FileDiffContent::Text { hunks } if !hunks.is_empty()),
        "expected ordinary hunks, got {:?}",
        diff.files[0].content
    );
}

#[test]
fn a_repository_with_no_gitattributes_tracks_nothing_with_lfs() {
    let repo = fixture().commit("A").build();

    assert!(
        !repo
            .backend()
            .uses_lfs()
            .expect("no attributes is not an error"),
        "most repositories have no .gitattributes at all"
    );
}

#[test]
fn a_gitattributes_that_hands_a_pattern_to_lfs_says_so() {
    // The line `git lfs track "*.bin"` writes, verbatim. Written by hand for
    // the reason the pointers are: what hideGit reads is the file, and the
    // suite needs no `git-lfs` on any platform to say so.
    let repo = fixture()
        .edit(
            ".gitattributes",
            "*.bin filter=lfs diff=lfs merge=lfs -text\n",
            "track",
        )
        .build();

    assert!(repo.backend().uses_lfs().expect("readable"));
}

#[test]
fn diff_and_merge_attributes_alone_do_not_make_a_repository_lfs() {
    // `filter=lfs` is what routes a file through the clean and smudge filters.
    // The other two are written alongside it and neither is what makes a file
    // stored as a pointer, so neither is what this question is about.
    let repo = fixture()
        .edit(".gitattributes", "*.bin diff=lfs merge=lfs\n", "track")
        .build();

    assert!(!repo.backend().uses_lfs().expect("readable"));
}

#[test]
fn a_comment_mentioning_the_filter_is_not_a_rule_that_sets_it() {
    let repo = fixture()
        .edit(
            ".gitattributes",
            "# we removed the *.bin filter=lfs rule in 2024\n*.txt text\n",
            "explain",
        )
        .build();

    assert!(!repo.backend().uses_lfs().expect("readable"));
}

#[test]
fn a_pattern_named_after_the_filter_is_not_a_rule_either() {
    // `filter=lfs` in the *pattern* column is a filename, not an attribute.
    // Scanning the whole line rather than the attributes would call it one.
    let repo = fixture()
        .edit(".gitattributes", "filter=lfs text\n", "odd name")
        .build();

    assert!(!repo.backend().uses_lfs().expect("readable"));
}

#[test]
fn the_repositorys_own_info_attributes_counts_too() {
    // `.git/info/attributes` is the private, uncommitted half of the same
    // mechanism, and a user who put their LFS rules there has an LFS
    // repository just as much.
    let repo = fixture().commit("A").build();
    let info = repo.path().join(".git").join("info");
    std::fs::create_dir_all(&info).expect("a writable git dir");
    std::fs::write(info.join("attributes"), "*.psd filter=lfs -text\n").expect("writable");

    assert!(repo.backend().uses_lfs().expect("readable"));
}
