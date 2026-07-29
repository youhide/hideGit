//! Round-trip tests for the patch serializer.
//!
//! The unit tests in `patch.rs` assert what the text looks like. These assert
//! the thing that actually matters: that `git apply --cached` accepts it and
//! puts exactly the intended change into the index. A patch that reads
//! correctly but does not apply is worth nothing, and the failure modes —
//! a missing newline marker, a miscounted `@@`, a `\r` eaten somewhere — are
//! all invisible until git rejects the patch or, worse, accepts a wrong one.

use hidegit_core::backend::GitBackend;
use hidegit_core::fixture::fixture;
use hidegit_core::model::{DiffTarget, FileDiff, FileDiffContent};
use hidegit_core::patch::{Selection, serialize};
use hidegit_core::process::GitCommand;

/// Applies `patch` to the index of the repository at `path`.
///
/// Deliberately the real command the backend will use, so these tests fail if
/// the patch text is wrong in any way `git apply` cares about.
fn apply_cached(path: &std::path::Path, patch: &str) {
    let output = GitCommand::new("apply")
        .args(["--cached", "-"])
        .cwd(path)
        .takes_locks()
        .run_with_stdin(Some(patch.as_bytes()));

    if let Err(e) = output {
        panic!("git apply rejected the patch:\n{e}\n\n--- patch ---\n{patch}");
    }
}

/// What `git diff --cached` reports, as the raw patch git itself would write.
fn staged_patch(path: &std::path::Path) -> String {
    let output = GitCommand::new("diff")
        .args(["--cached"])
        .cwd(path)
        .run()
        .expect("git diff --cached succeeds");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn only_file(diff: &hidegit_core::model::Diff) -> &FileDiff {
    assert_eq!(diff.files.len(), 1, "the fixture changed exactly one file");
    &diff.files[0]
}

#[test]
fn a_whole_file_patch_stages_everything_in_it() {
    let repo = fixture()
        .edit("f.txt", "one\ntwo\nthree\n", "base")
        .write("f.txt", "one\nTWO\nthree\n")
        .build();
    let backend = repo.backend();

    let diff = backend.diff(&DiffTarget::Unstaged).unwrap();
    let patch = serialize(only_file(&diff), &Selection::everything()).expect("a patch");
    apply_cached(repo.path(), &patch);

    let staged = staged_patch(repo.path());
    assert!(staged.contains("-two\n"), "{staged}");
    assert!(staged.contains("+TWO\n"), "{staged}");

    // And nothing is left over: the index now matches the working tree.
    let backend = hidegit_core::HybridBackend::open(repo.path()).unwrap();
    assert!(
        backend
            .diff(&DiffTarget::Unstaged)
            .unwrap()
            .files
            .is_empty(),
        "staging the whole file leaves nothing unstaged"
    );
}

#[test]
fn one_hunk_of_several_stages_alone_and_leaves_the_rest_behind() {
    // Twenty lines apart, so the two changes cannot land in one hunk.
    let mut original = String::new();
    for i in 1..=40 {
        original.push_str(&format!("line {i}\n"));
    }
    let edited = original
        .replace("line 2\n", "LINE TWO\n")
        .replace("line 38\n", "LINE THIRTY EIGHT\n");

    let repo = fixture()
        .edit("f.txt", &original, "base")
        .write("f.txt", &edited)
        .build();
    let backend = repo.backend();

    let diff = backend.diff(&DiffTarget::Unstaged).unwrap();
    let file = only_file(&diff);
    let FileDiffContent::Text { hunks } = &file.content else {
        panic!("a text file");
    };
    assert_eq!(hunks.len(), 2, "the two edits are far enough apart");

    let patch = serialize(file, &Selection::hunk(0)).expect("a patch");
    apply_cached(repo.path(), &patch);

    let staged = staged_patch(repo.path());
    assert!(
        staged.contains("+LINE TWO\n"),
        "the first hunk landed: {staged}"
    );
    assert!(
        !staged.contains("LINE THIRTY EIGHT"),
        "the second hunk did not: {staged}"
    );
}

#[test]
fn the_second_hunk_alone_applies_despite_the_first_one_being_skipped() {
    // The regression this guards: the second hunk's `@@` new-side start comes
    // from a diff in which the first hunk was applied. Emitting it unchanged
    // puts the hunk in the wrong place.
    let mut original = String::new();
    for i in 1..=40 {
        original.push_str(&format!("line {i}\n"));
    }
    // The first change adds two lines rather than replacing one, so the drift
    // is non-zero and a copied line number would be visibly wrong.
    let edited = original
        .replace("line 2\n", "line 2\nextra a\nextra b\n")
        .replace("line 38\n", "LINE THIRTY EIGHT\n");

    let repo = fixture()
        .edit("f.txt", &original, "base")
        .write("f.txt", &edited)
        .build();

    let diff = repo.backend().diff(&DiffTarget::Unstaged).unwrap();
    let file = only_file(&diff);

    let patch = serialize(file, &Selection::hunk(1)).expect("a patch");
    apply_cached(repo.path(), &patch);

    let staged = staged_patch(repo.path());
    assert!(staged.contains("+LINE THIRTY EIGHT\n"), "{staged}");
    assert!(
        !staged.contains("extra a"),
        "the skipped hunk stayed out: {staged}"
    );
}

#[test]
fn staging_one_line_of_a_hunk_leaves_the_other_unstaged() {
    let repo = fixture()
        .edit("f.txt", "keep\nalpha\nbeta\nkeep\n", "base")
        .write("f.txt", "keep\nALPHA\nBETA\nkeep\n")
        .build();

    let diff = repo.backend().diff(&DiffTarget::Unstaged).unwrap();
    let file = only_file(&diff);
    let FileDiffContent::Text { hunks } = &file.content else {
        panic!("a text file");
    };

    // Take the removal of `alpha` and the addition of `ALPHA`, and nothing else.
    let chosen: Vec<usize> = hunks[0]
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.text == "alpha" || l.text == "ALPHA")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(chosen.len(), 2, "one removal and one addition");

    let patch = serialize(file, &Selection::lines(0, chosen)).expect("a patch");
    apply_cached(repo.path(), &patch);

    let staged = staged_patch(repo.path());
    assert!(
        staged.contains("+ALPHA\n"),
        "the chosen line landed: {staged}"
    );
    assert!(!staged.contains("BETA"), "the other one did not: {staged}");

    // The rest is still waiting, which is the whole point of line staging.
    let backend = hidegit_core::HybridBackend::open(repo.path()).unwrap();
    let left = backend.diff(&DiffTarget::Unstaged).unwrap();
    let FileDiffContent::Text { hunks } = &left.files[0].content else {
        panic!("a text file");
    };
    let added: Vec<&str> = hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == hidegit_core::model::LineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(added, vec!["BETA"]);
}

#[test]
fn a_file_that_loses_its_trailing_newline_stages_without_gaining_one_back() {
    let repo = fixture()
        .edit("f.txt", "alpha\nomega\n", "base")
        .write("f.txt", "alpha\nomega")
        .build();

    let diff = repo.backend().diff(&DiffTarget::Unstaged).unwrap();
    let patch = serialize(only_file(&diff), &Selection::everything()).expect("a patch");
    apply_cached(repo.path(), &patch);

    let staged = staged_patch(repo.path());
    assert!(
        staged.contains("\\ No newline at end of file"),
        "git agrees the newline is gone: {staged}"
    );

    let backend = hidegit_core::HybridBackend::open(repo.path()).unwrap();
    assert!(
        backend
            .diff(&DiffTarget::Unstaged)
            .unwrap()
            .files
            .is_empty(),
        "the index matches the working tree exactly, newline and all"
    );
}

#[test]
fn a_file_that_gains_a_trailing_newline_stages_that_too() {
    let repo = fixture()
        .edit("f.txt", "alpha\nomega", "base")
        .write("f.txt", "alpha\nomega\n")
        .build();

    let diff = repo.backend().diff(&DiffTarget::Unstaged).unwrap();
    let patch = serialize(only_file(&diff), &Selection::everything()).expect("a patch");
    apply_cached(repo.path(), &patch);

    let backend = hidegit_core::HybridBackend::open(repo.path()).unwrap();
    assert!(
        backend
            .diff(&DiffTarget::Unstaged)
            .unwrap()
            .files
            .is_empty(),
        "the added newline is in the index"
    );
}

#[test]
fn carriage_returns_survive_the_round_trip() {
    // `\r` is content, not formatting. Trimming it while building the patch
    // makes every context line fail to match on a CRLF file.
    let repo = fixture()
        .edit("f.txt", "one\r\ntwo\r\nthree\r\n", "base")
        .write("f.txt", "one\r\nTWO\r\nthree\r\n")
        .build();

    let diff = repo.backend().diff(&DiffTarget::Unstaged).unwrap();
    let patch = serialize(only_file(&diff), &Selection::everything()).expect("a patch");
    apply_cached(repo.path(), &patch);

    let backend = hidegit_core::HybridBackend::open(repo.path()).unwrap();
    assert!(
        backend
            .diff(&DiffTarget::Unstaged)
            .unwrap()
            .files
            .is_empty(),
        "the index matches the CRLF working tree byte for byte"
    );
}

#[test]
fn a_nested_path_applies_where_it_lives() {
    let repo = fixture()
        .edit("src/deep/f.txt", "one\n", "base")
        .write("src/deep/f.txt", "two\n")
        .build();

    let diff = repo.backend().diff(&DiffTarget::Unstaged).unwrap();
    let patch = serialize(only_file(&diff), &Selection::everything()).expect("a patch");
    apply_cached(repo.path(), &patch);

    let staged = staged_patch(repo.path());
    assert!(staged.contains("src/deep/f.txt"), "{staged}");
}

#[test]
fn a_deletion_stages_as_a_deletion() {
    let repo = fixture()
        .edit("doomed.txt", "content\n", "base")
        .delete("doomed.txt")
        .build();

    let diff = repo.backend().diff(&DiffTarget::Unstaged).unwrap();
    let patch = serialize(only_file(&diff), &Selection::everything()).expect("a patch");
    apply_cached(repo.path(), &patch);

    let staged = staged_patch(repo.path());
    assert!(staged.contains("deleted file"), "{staged}");
}

#[test]
fn unstaging_is_the_same_patch_applied_in_reverse() {
    let repo = fixture()
        .edit("f.txt", "before\n", "base")
        .stage("f.txt", "after\n")
        .build();

    let diff = repo.backend().diff(&DiffTarget::Staged).unwrap();
    let patch = serialize(only_file(&diff), &Selection::everything()).expect("a patch");

    let output = GitCommand::new("apply")
        .args(["--cached", "--reverse", "-"])
        .cwd(repo.path())
        .takes_locks()
        .run_with_stdin(Some(patch.as_bytes()));
    assert!(output.is_ok(), "reverse apply failed: {output:?}\n{patch}");

    assert_eq!(
        staged_patch(repo.path()),
        "",
        "the index is back to HEAD, which is what unstaging means"
    );
}

// ---- stage / unstage / discard, by file -------------------------------

#[test]
fn staging_a_file_moves_it_from_changed_to_staged() {
    let repo = fixture()
        .edit("f.txt", "before\n", "base")
        .write("f.txt", "after\n")
        .build();
    let backend = repo.backend();

    backend.stage(&[std::path::Path::new("f.txt")]).unwrap();

    let status = backend.status().unwrap();
    assert_eq!(status.staged.len(), 1);
    assert!(status.unstaged.is_empty());
}

#[test]
fn staging_a_deleted_file_records_the_deletion() {
    // `git add` on a path that no longer exists is the case that silently does
    // nothing without `--all`.
    let repo = fixture()
        .edit("doomed.txt", "content\n", "base")
        .delete("doomed.txt")
        .build();
    let backend = repo.backend();

    backend
        .stage(&[std::path::Path::new("doomed.txt")])
        .unwrap();

    let status = backend.status().unwrap();
    assert_eq!(status.staged.len(), 1, "the deletion is staged");
    assert_eq!(status.staged[0].status.code(), 'D');
    assert!(status.unstaged.is_empty());
}

#[test]
fn staging_an_untracked_file_tracks_it() {
    let repo = fixture()
        .commit("seed")
        .write("new.txt", "brand new\n")
        .build();
    let backend = repo.backend();

    backend.stage(&[std::path::Path::new("new.txt")]).unwrap();

    let status = backend.status().unwrap();
    assert!(status.untracked.is_empty());
    assert_eq!(status.staged[0].status.code(), 'A');
}

#[test]
fn unstaging_puts_a_change_back_without_touching_the_file() {
    let repo = fixture()
        .edit("f.txt", "before\n", "base")
        .stage("f.txt", "after\n")
        .build();
    let backend = repo.backend();

    backend.unstage(&[std::path::Path::new("f.txt")]).unwrap();

    let status = backend.status().unwrap();
    assert!(status.staged.is_empty(), "nothing is staged any more");
    assert_eq!(status.unstaged.len(), 1, "but the edit is still there");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("f.txt")).unwrap(),
        "after\n",
        "unstaging is not discarding"
    );
}

#[test]
fn unstaging_the_first_ever_commit_works_without_a_head_to_restore_from() {
    // `git restore --staged --source=HEAD` has no HEAD to name here, which is
    // why this path uses `git rm --cached` instead.
    let repo = fixture().stage("first.txt", "initial\n").build();
    let backend = repo.backend();

    backend
        .unstage(&[std::path::Path::new("first.txt")])
        .unwrap();

    let status = backend.status().unwrap();
    assert!(status.staged.is_empty());
    assert_eq!(
        status.untracked,
        vec![std::path::PathBuf::from("first.txt")]
    );
    assert!(repo.path().join("first.txt").exists(), "the file survives");
}

#[test]
fn discarding_a_tracked_file_restores_it_from_the_index() {
    let repo = fixture()
        .edit("f.txt", "committed\n", "base")
        .write("f.txt", "scribbled over\n")
        .build();
    let backend = repo.backend();

    backend.discard(&[std::path::Path::new("f.txt")]).unwrap();

    assert_eq!(
        std::fs::read_to_string(repo.path().join("f.txt")).unwrap(),
        "committed\n"
    );
    assert!(backend.status().unwrap().is_clean());
}

#[test]
fn discarding_an_untracked_file_deletes_it() {
    // A different operation wearing the same name: there is no index entry to
    // restore from, so `git restore` would fail outright.
    let repo = fixture()
        .commit("seed")
        .write("junk.txt", "unwanted\n")
        .build();
    let backend = repo.backend();

    backend
        .discard(&[std::path::Path::new("junk.txt")])
        .unwrap();

    assert!(!repo.path().join("junk.txt").exists());
    assert!(backend.status().unwrap().is_clean());
}

#[test]
fn discarding_a_mixed_selection_handles_each_kind_correctly() {
    let repo = fixture()
        .edit("tracked.txt", "committed\n", "base")
        .write("tracked.txt", "scribbled\n")
        .write("junk.txt", "unwanted\n")
        .build();
    let backend = repo.backend();

    backend
        .discard(&[
            std::path::Path::new("tracked.txt"),
            std::path::Path::new("junk.txt"),
        ])
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "committed\n",
        "the tracked file was restored"
    );
    assert!(
        !repo.path().join("junk.txt").exists(),
        "the untracked one was removed"
    );
}

#[test]
fn a_write_refuses_to_run_while_the_index_is_locked() {
    let repo = fixture()
        .edit("f.txt", "before\n", "base")
        .write("f.txt", "after\n")
        .build();
    let backend = repo.backend();

    let lock = backend.git_dir().join("index.lock");
    std::fs::write(&lock, b"").expect("creating a lock file");

    match backend.stage(&[std::path::Path::new("f.txt")]) {
        Err(hidegit_core::GitError::IndexLocked(path)) => assert_eq!(path, lock),
        other => panic!("expected IndexLocked, got {other:?}"),
    }

    // And hideGit does not remove a lock it did not create: the process
    // holding it may still be working.
    assert!(lock.exists(), "the lock is reported, never deleted");
}

#[test]
fn a_write_drops_the_memoised_walk_so_the_next_read_sees_it() {
    let repo = fixture().commit("A").write("new.txt", "x\n").build();
    let backend = repo.backend();

    assert_eq!(
        backend.commit_count(&hidegit_core::RevSpec::All).unwrap(),
        1
    );
    backend.stage(&[std::path::Path::new("new.txt")]).unwrap();

    assert_eq!(
        backend.status().unwrap().staged.len(),
        1,
        "the write is visible without an explicit invalidate"
    );
}

#[test]
fn staging_nothing_is_not_an_error_and_does_not_stage_everything() {
    // The bug this guards: `git add --` with no paths is a no-op, but `git add
    // --all` with no paths stages the entire working tree.
    let repo = fixture()
        .commit("seed")
        .write("untouched.txt", "should stay untracked\n")
        .build();
    let backend = repo.backend();

    backend.stage(&[]).unwrap();

    let status = backend.status().unwrap();
    assert!(
        status.staged.is_empty(),
        "an empty selection stages nothing"
    );
    assert_eq!(status.untracked.len(), 1);
}

// ---- stage_patch, through the backend ---------------------------------

use hidegit_core::ops::Patch;

/// Builds the patch the UI would build for `selection`, ready to apply.
fn patch_for(diff: &hidegit_core::model::Diff, selection: &Selection, reverse: bool) -> Patch {
    let file = only_file(diff);
    Patch {
        file: file.path.clone(),
        text: serialize(file, selection).expect("a patch"),
        reverse,
    }
}

#[test]
fn stage_patch_stages_one_hunk_through_the_backend() {
    let mut original = String::new();
    for i in 1..=40 {
        original.push_str(&format!("line {i}\n"));
    }
    let edited = original
        .replace("line 2\n", "LINE TWO\n")
        .replace("line 38\n", "LINE THIRTY EIGHT\n");

    let repo = fixture()
        .edit("f.txt", &original, "base")
        .write("f.txt", &edited)
        .build();
    let backend = repo.backend();

    let diff = backend.diff(&DiffTarget::Unstaged).unwrap();
    backend
        .stage_patch(&patch_for(&diff, &Selection::hunk(0), false))
        .unwrap();

    let staged = staged_patch(repo.path());
    assert!(staged.contains("+LINE TWO\n"), "{staged}");
    assert!(!staged.contains("LINE THIRTY EIGHT"), "{staged}");

    // And the rest is still waiting, which is what makes this useful.
    assert_eq!(backend.diff(&DiffTarget::Unstaged).unwrap().files.len(), 1);
}

#[test]
fn stage_patch_in_reverse_unstages() {
    let repo = fixture()
        .edit("f.txt", "before\n", "base")
        .stage("f.txt", "after\n")
        .build();
    let backend = repo.backend();

    let diff = backend.diff(&DiffTarget::Staged).unwrap();
    backend
        .stage_patch(&patch_for(&diff, &Selection::everything(), true))
        .unwrap();

    assert!(
        backend.status().unwrap().staged.is_empty(),
        "the index is back at HEAD"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("f.txt")).unwrap(),
        "after\n",
        "and the working tree was never touched"
    );
}

#[test]
fn a_patch_that_does_not_apply_reports_gits_own_words() {
    let repo = fixture()
        .edit("f.txt", "before\n", "base")
        .write("f.txt", "after\n")
        .build();
    let backend = repo.backend();

    let diff = backend.diff(&DiffTarget::Unstaged).unwrap();
    let mut patch = patch_for(&diff, &Selection::everything(), false);
    // Corrupt the context so the patch cannot possibly apply.
    patch.text = patch.text.replace("-before", "-something else entirely");

    match backend.stage_patch(&patch) {
        Err(hidegit_core::GitError::Command { stderr, .. }) => {
            assert!(
                !stderr.is_empty(),
                "git's own message is surfaced rather than paraphrased"
            );
        }
        other => panic!("expected a Command error, got {other:?}"),
    }

    assert!(
        backend.status().unwrap().staged.is_empty(),
        "a rejected patch leaves the index alone"
    );
}

#[test]
fn stage_patch_refuses_while_the_index_is_locked() {
    let repo = fixture()
        .edit("f.txt", "before\n", "base")
        .write("f.txt", "after\n")
        .build();
    let backend = repo.backend();

    let diff = backend.diff(&DiffTarget::Unstaged).unwrap();
    let patch = patch_for(&diff, &Selection::everything(), false);

    let lock = backend.git_dir().join("index.lock");
    std::fs::write(&lock, b"").unwrap();

    assert!(matches!(
        backend.stage_patch(&patch),
        Err(hidegit_core::GitError::IndexLocked(_))
    ));
}
