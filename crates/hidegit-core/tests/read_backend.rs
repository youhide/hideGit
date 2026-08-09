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
    RepoState, RevSpec,
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
fn operations_from_later_milestones_say_which_milestone_they_land_in() {
    let repo = fixture().commit("A").build();
    let backend = repo.backend();

    // The whole write surface is declared from M1 so the read/write split stays
    // auditable in one file, and a method whose milestone has not landed says so
    // rather than being absent. These are the ones that have not.
    let unimplemented: Vec<(Result<(), GitError>, &str, &str)> = vec![
        (
            backend.blame(Path::new("A.txt"), repo.id("A")).map(|_| ()),
            "blame",
            "M6",
        ),
        (
            backend
                .rebase("main", &hidegit_core::ops::RebasePlan::default())
                .map(|_| ()),
            "rebase",
            "M5",
        ),
    ];

    for (result, expected_operation, expected_milestone) in unimplemented {
        match result {
            Err(GitError::NotImplementedYet {
                operation,
                milestone,
            }) => {
                assert_eq!(operation, expected_operation);
                assert_eq!(milestone, expected_milestone);
            }
            other => panic!("expected NotImplementedYet for {expected_operation}, got {other:?}"),
        }
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
