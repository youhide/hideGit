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
