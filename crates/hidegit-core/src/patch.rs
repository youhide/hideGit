//! Turning a file's diff back into a unified patch.
//!
//! Staging part of a file is done by feeding `git apply --cached` a patch
//! rather than by rewriting the index directly, which is what makes hunk
//! staging, line staging and unstaging one code path — see
//! `docs/adr/0002-git-backend-hybrid.md`. This module is the half that builds
//! the patch text; `GitBackend::stage_patch` is the half that applies it.
//!
//! The output has to be byte-accurate, because `git apply` matches context
//! lines exactly. Two details are easy to lose and expensive to debug:
//!
//! - **`\ No newline at end of file`.** Omitting it appends a newline to a file
//!   that did not have one, silently, on every partial stage.
//! - **The `@@` counts.** They are recomputed rather than copied, because a
//!   partial selection changes them: an unselected removal becomes a context
//!   line, and an unselected addition disappears.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::model::{ChangeStatus, DiffLine, FileDiff, FileDiffContent, Hunk, LineKind};

/// Which parts of a file's diff a patch should contain.
///
/// Selection is expressed over *changed* lines only. Context lines are not
/// selectable and are always emitted: they are how `git apply` finds its place
/// in the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Hunk index to the changed lines chosen within it, by their index into
    /// [`Hunk::lines`]. `None` means every changed line in that hunk.
    ///
    /// A hunk absent from the map contributes nothing to the patch.
    hunks: BTreeMap<usize, Option<BTreeSet<usize>>>,
    /// Every hunk is selected in full, whatever the map says.
    everything: bool,
}

impl Selection {
    /// The whole file: every hunk, every line.
    pub fn everything() -> Self {
        Self {
            hunks: BTreeMap::new(),
            everything: true,
        }
    }

    /// One hunk, in full.
    pub fn hunk(index: usize) -> Self {
        Self::default().with_hunk(index)
    }

    /// Specific changed lines within one hunk, by their index into
    /// [`Hunk::lines`].
    pub fn lines(hunk: usize, lines: impl IntoIterator<Item = usize>) -> Self {
        Self::default().with_lines(hunk, lines)
    }

    /// Adds another hunk, in full.
    pub fn with_hunk(mut self, index: usize) -> Self {
        self.hunks.insert(index, None);
        self
    }

    /// Adds specific lines within another hunk.
    pub fn with_lines(mut self, hunk: usize, lines: impl IntoIterator<Item = usize>) -> Self {
        self.hunks
            .entry(hunk)
            .or_insert_with(|| Some(BTreeSet::new()))
            .get_or_insert_with(BTreeSet::new)
            .extend(lines);
        self
    }

    pub fn is_empty(&self) -> bool {
        !self.everything && self.hunks.is_empty()
    }

    /// Is this hunk in the patch at all?
    fn includes_hunk(&self, hunk: usize) -> bool {
        self.everything || self.hunks.contains_key(&hunk)
    }

    /// Is this changed line one of the chosen ones?
    fn includes_line(&self, hunk: usize, line: usize) -> bool {
        if self.everything {
            return true;
        }
        match self.hunks.get(&hunk) {
            Some(None) => true,
            Some(Some(lines)) => lines.contains(&line),
            None => false,
        }
    }
}

/// Builds the patch text for `file`, or `None` if there is nothing to apply.
///
/// `None` covers a binary file, a file too large to diff, a file stored with
/// Git LFS — whose hunks would be the pointer's three lines rather than the
/// file's content — and a selection that resolves to no changed lines at all — in each case there is no patch to
/// hand to `git apply`, and an empty one is an error rather than a no-op.
pub fn serialize(file: &FileDiff, selection: &Selection) -> Option<String> {
    let FileDiffContent::Text { hunks } = &file.content else {
        return None;
    };
    if selection.is_empty() {
        return None;
    }

    // How far the new side has drifted from the old one across the hunks
    // already emitted. Skipping a hunk means the ones after it start somewhere
    // other than where the original diff said they did.
    let mut drift: i64 = 0;
    let mut body = String::new();

    for (index, hunk) in hunks.iter().enumerate() {
        let rendered = if selection.includes_hunk(index) {
            render_hunk(hunk, index, selection, drift)
        } else {
            None
        };

        match rendered {
            Some(rendered) => {
                drift += rendered.drift;
                body.push_str(&rendered.text);
            }
            // A hunk left out of the patch does not happen at all, so the new
            // side keeps the old side's line numbering across it.
            None => drift -= i64::from(hunk.new_lines) - i64::from(hunk.old_lines),
        }
    }

    if body.is_empty() {
        return None;
    }

    let mut patch = header(file);
    patch.push_str(&body);
    Some(patch)
}

/// The `diff --git` preamble, including the `---`/`+++` pair.
///
/// A file being created has no old side and a file being deleted has no new
/// one; `git apply` wants `/dev/null` there rather than a path that is not
/// supposed to exist.
fn header(file: &FileDiff) -> String {
    let new = quote(&file.path);
    let old = match &file.status {
        ChangeStatus::Renamed { from } | ChangeStatus::Copied { from } => quote(from),
        _ => new.clone(),
    };

    let mut out = format!("diff --git a/{old} b/{new}\n");

    match &file.status {
        ChangeStatus::Renamed { .. } => {
            let _ = write!(out, "rename from {old}\nrename to {new}\n");
        }
        ChangeStatus::Copied { .. } => {
            let _ = write!(out, "copy from {old}\ncopy to {new}\n");
        }
        _ => {}
    }

    match file.status {
        ChangeStatus::Added => {
            let _ = write!(out, "--- /dev/null\n+++ b/{new}\n");
        }
        ChangeStatus::Deleted => {
            let _ = write!(out, "--- a/{old}\n+++ /dev/null\n");
        }
        _ => {
            let _ = write!(out, "--- a/{old}\n+++ b/{new}\n");
        }
    }

    out
}

/// Renders a path the way Git writes it in a patch header.
///
/// Git uses forward slashes regardless of platform, so a patch built on Windows
/// applies to the same repository elsewhere.
fn quote(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

struct RenderedHunk {
    text: String,
    /// How much this hunk moves everything after it on the new side.
    drift: i64,
}

fn render_hunk(
    hunk: &Hunk,
    index: usize,
    selection: &Selection,
    drift: i64,
) -> Option<RenderedHunk> {
    let mut lines = String::new();
    let mut old_count: u32 = 0;
    let mut new_count: u32 = 0;
    let mut changed = false;

    for (position, line) in hunk.lines.iter().enumerate() {
        let selected = selection.includes_line(index, position);

        match (line.kind, selected) {
            (LineKind::Context, _) => {
                emit(&mut lines, ' ', line);
                old_count += 1;
                new_count += 1;
            }
            (LineKind::Removed, true) => {
                emit(&mut lines, '-', line);
                old_count += 1;
                changed = true;
            }
            // Leaving a removal out means the line stays where it is, so the
            // patch has to carry it as context rather than drop it: `git apply`
            // would otherwise not find its place.
            (LineKind::Removed, false) => {
                emit(&mut lines, ' ', line);
                old_count += 1;
                new_count += 1;
            }
            (LineKind::Added, true) => {
                emit(&mut lines, '+', line);
                new_count += 1;
                changed = true;
            }
            // An addition left out simply does not happen yet. It is absent
            // from both sides, so it is absent from the patch.
            (LineKind::Added, false) => {}
        }
    }

    // A hunk whose every change was deselected is now pure context. Emitting it
    // is harmless but pointless, and `git apply` rejects a patch of nothing.
    if !changed {
        return None;
    }

    let new_start = (i64::from(hunk.new_start) + drift).max(0) as u32;
    let mut text = format!(
        "@@ -{} +{} @@\n",
        range(hunk.old_start, old_count),
        range(new_start, new_count)
    );
    text.push_str(&lines);

    Some(RenderedHunk {
        text,
        drift: i64::from(new_count)
            - i64::from(old_count)
            - (i64::from(hunk.new_lines) - i64::from(hunk.old_lines)),
    })
}

/// One side of an `@@` header.
///
/// Git abbreviates a single-line range to just its start, and writes the start
/// as `0` when the range is empty — a file being created has no old side.
fn range(start: u32, count: u32) -> String {
    match count {
        0 => "0,0".to_owned(),
        1 => format!("{start}"),
        _ => format!("{start},{count}"),
    }
}

fn emit(out: &mut String, marker: char, line: &DiffLine) {
    out.push(marker);
    out.push_str(&line.text);
    out.push('\n');
    if line.no_newline {
        out.push_str("\\ No newline at end of file\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_lineno: None,
            new_lineno: None,
            text: text.to_owned(),
            no_newline: false,
        }
    }

    /// A hunk replacing `one` with `ONE`, with a line of context either side.
    fn swap_hunk(old_start: u32, new_start: u32) -> Hunk {
        Hunk {
            old_start,
            old_lines: 3,
            new_start,
            new_lines: 3,
            header: String::new(),
            lines: vec![
                line(LineKind::Context, "above"),
                line(LineKind::Removed, "one"),
                line(LineKind::Added, "ONE"),
                line(LineKind::Context, "below"),
            ],
        }
    }

    fn file(status: ChangeStatus, hunks: Vec<Hunk>) -> FileDiff {
        FileDiff {
            path: PathBuf::from("src/main.rs"),
            status,
            content: FileDiffContent::Text { hunks },
        }
    }

    #[test]
    fn a_whole_file_round_trips_to_the_patch_it_came_from() {
        let patch = serialize(
            &file(ChangeStatus::Modified, vec![swap_hunk(10, 10)]),
            &Selection::everything(),
        )
        .expect("a text file with a change produces a patch");

        assert_eq!(
            patch,
            "diff --git a/src/main.rs b/src/main.rs\n\
             --- a/src/main.rs\n\
             +++ b/src/main.rs\n\
             @@ -10,3 +10,3 @@\n\
             \x20above\n\
             -one\n\
             +ONE\n\
             \x20below\n"
        );
    }

    #[test]
    fn a_deselected_removal_becomes_context_so_apply_can_find_its_place() {
        // Line 1 is the removal, line 2 the addition. Taking only the addition
        // means the old line stays: it is context now, and both counts grow.
        let patch = serialize(
            &file(ChangeStatus::Modified, vec![swap_hunk(10, 10)]),
            &Selection::lines(0, [2]),
        )
        .expect("selecting one line still produces a patch");

        assert!(
            patch.contains("@@ -10,3 +10,4 @@"),
            "three lines in, four out — the removal stayed and the addition arrived: {patch}"
        );
        assert!(
            patch.contains(" one\n"),
            "the removal is carried as context"
        );
        assert!(patch.contains("+ONE\n"));
        assert!(!patch.contains("-one\n"));
    }

    #[test]
    fn a_deselected_addition_is_absent_from_both_sides() {
        let patch = serialize(
            &file(ChangeStatus::Modified, vec![swap_hunk(10, 10)]),
            &Selection::lines(0, [1]),
        )
        .expect("selecting the removal alone still produces a patch");

        assert!(
            patch.contains("@@ -10,3 +10,2 @@"),
            "three lines in, two out — the removal happened and nothing replaced it: {patch}"
        );
        assert!(patch.contains("-one\n"));
        assert!(!patch.contains("ONE"));
    }

    #[test]
    fn a_hunk_left_out_moves_the_ones_after_it_back() {
        // The first hunk would have added two lines. Skipping it means the
        // second hunk applies two lines earlier than the original diff said.
        let grow = Hunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 3,
            header: String::new(),
            lines: vec![
                line(LineKind::Context, "top"),
                line(LineKind::Added, "extra one"),
                line(LineKind::Added, "extra two"),
            ],
        };

        let patch = serialize(
            &file(ChangeStatus::Modified, vec![grow, swap_hunk(20, 22)]),
            &Selection::hunk(1),
        )
        .expect("the second hunk alone produces a patch");

        assert!(
            patch.contains("@@ -20,3 +20,3 @@"),
            "the new side starts at 20, not the 22 the full diff predicted: {patch}"
        );
    }

    #[test]
    fn a_missing_trailing_newline_is_marked_on_the_line_that_ends_the_file() {
        let mut hunk = swap_hunk(1, 1);
        hunk.lines.pop();
        hunk.lines[2].no_newline = true;
        hunk.old_lines = 2;
        hunk.new_lines = 2;

        let patch = serialize(
            &file(ChangeStatus::Modified, vec![hunk]),
            &Selection::everything(),
        )
        .expect("a patch is produced");

        assert!(
            patch.ends_with("+ONE\n\\ No newline at end of file\n"),
            "without this marker, applying the patch appends a newline: {patch}"
        );
    }

    #[test]
    fn a_new_file_has_no_old_side() {
        let hunk = Hunk {
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: 1,
            header: String::new(),
            lines: vec![line(LineKind::Added, "hello")],
        };

        let patch = serialize(
            &file(ChangeStatus::Added, vec![hunk]),
            &Selection::everything(),
        )
        .expect("a patch is produced");

        assert!(patch.contains("--- /dev/null\n"));
        assert!(patch.contains("+++ b/src/main.rs\n"));
        assert!(
            patch.contains("@@ -0,0 +1 @@\n"),
            "a one-line range omits its count: {patch}"
        );
    }

    #[test]
    fn a_deleted_file_has_no_new_side() {
        let hunk = Hunk {
            old_start: 1,
            old_lines: 1,
            new_start: 0,
            new_lines: 0,
            header: String::new(),
            lines: vec![line(LineKind::Removed, "goodbye")],
        };

        let patch = serialize(
            &file(ChangeStatus::Deleted, vec![hunk]),
            &Selection::everything(),
        )
        .expect("a patch is produced");

        assert!(patch.contains("+++ /dev/null\n"));
        assert!(patch.contains("@@ -1 +0,0 @@\n"), "{patch}");
    }

    #[test]
    fn a_rename_names_both_paths() {
        let patch = serialize(
            &file(
                ChangeStatus::Renamed {
                    from: PathBuf::from("src/old.rs"),
                },
                vec![swap_hunk(1, 1)],
            ),
            &Selection::everything(),
        )
        .expect("a patch is produced");

        assert!(patch.starts_with("diff --git a/src/old.rs b/src/main.rs\n"));
        assert!(patch.contains("rename from src/old.rs\nrename to src/main.rs\n"));
    }

    #[test]
    fn a_selection_that_chooses_nothing_produces_no_patch() {
        // An empty patch is an error to `git apply`, not a no-op, so the
        // caller has to be told there is nothing to do.
        assert!(
            serialize(
                &file(ChangeStatus::Modified, vec![swap_hunk(1, 1)]),
                &Selection::default()
            )
            .is_none()
        );
        assert!(
            serialize(
                &file(ChangeStatus::Modified, vec![swap_hunk(1, 1)]),
                &Selection::lines(0, [])
            )
            .is_none(),
            "a hunk with no lines chosen contributes nothing"
        );
    }

    #[test]
    fn a_binary_file_has_no_patch_to_build() {
        let file = FileDiff {
            path: PathBuf::from("logo.png"),
            status: ChangeStatus::Modified,
            content: FileDiffContent::Binary,
        };

        assert!(serialize(&file, &Selection::everything()).is_none());
    }

    #[test]
    fn paths_use_forward_slashes_whatever_the_platform_wrote_them_with() {
        assert_eq!(quote(Path::new("a/b/c.rs")), "a/b/c.rs");
        assert_eq!(
            quote(&PathBuf::from("a").join("b").join("c.rs")),
            "a/b/c.rs",
            "a patch built on Windows has to apply everywhere else"
        );
    }
}
