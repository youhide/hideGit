//! Parsing and re-rendering a file Git left conflict markers in.
//!
//! The resolver works **per conflict, not per file** — a file with one conflict
//! is the easy case and a file with nine is the one people abandon a GUI over —
//! so a conflicted file has to be split into the regions Git marked and the
//! untouched text between them. That is what this module does, and it does it
//! without a repository: the markers are in the file, and everything here is a
//! pure function over its bytes.
//!
//! Re-rendering is the same operation backwards. [`ConflictFile::render`] takes
//! one [`Resolution`] per conflict and produces the file that should be written
//! back, which is then staged exactly like any other edit — resolving is not a
//! special kind of write, it is an ordinary one that happens to end a conflict.

use std::fmt;

/// The three marker prefixes Git writes, and the one it writes only under
/// `merge.conflictStyle = diff3` or `zdiff3`.
///
/// Git writes exactly seven characters, and a line inside a conflict may itself
/// begin with fewer or more — a file of Markdown or of Git documentation is the
/// obvious way to hit that — so the count is checked rather than assumed.
const OURS: &str = "<<<<<<<";
const BASE: &str = "|||||||";
const SPLIT: &str = "=======";
const THEIRS: &str = ">>>>>>>";

/// The common ancestor of a conflicting region.
///
/// Label and lines travel together because they are present or absent together:
/// Git writes `||||||| <short hash>` and the section under it, or neither. Two
/// separate `Option`s could disagree, and the way that shows up is a saved
/// half-resolution whose markers no longer match what Git wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictBase {
    /// What Git wrote after `|||||||`, usually the ancestor's short hash.
    pub label: String,
    pub lines: Vec<String>,
}

/// One conflicting region, as Git marked it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictRegion {
    /// The side already on this branch. Lines carry their own terminators.
    pub ours: Vec<String>,
    /// The common ancestor, present only when the user's `merge.conflictStyle`
    /// is `diff3` or `zdiff3`.
    ///
    /// `None` is the common case and not a degraded one: the two-pane
    /// comparison the resolver shows does not need it. It is offered when it is
    /// there because a three-way view settles arguments a two-way view cannot.
    pub base: Option<ConflictBase>,
    /// The side being merged in.
    pub theirs: Vec<String>,
    /// What Git wrote after `<<<<<<<`, usually a branch name or `HEAD`.
    pub ours_label: String,
    /// What Git wrote after `>>>>>>>`, usually the other branch or a commit
    /// subject.
    pub theirs_label: String,
    /// The line terminator the marker lines used — `\r\n` or `\n`.
    ///
    /// Kept because re-rendering an undecided region has to write the markers
    /// back, and writing `\n` into a CRLF file turns a saved half-resolution
    /// into a whole-file diff the next time anyone looks at it.
    pub eol: String,
}

/// What to do with one conflicting region.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Resolution {
    /// Still to be decided. A file with any of these is not resolved, which is
    /// what keeps Continue disabled.
    #[default]
    Unresolved,
    Ours,
    Theirs,
    /// Both sides, ours first. The order is the one Git's own markers imply, so
    /// it is the one that matches what the user is looking at.
    Both,
    /// Both sides, theirs first.
    BothReversed,
    /// Whatever the user typed in the result pane. Lines carry their own
    /// terminators, like every other side here.
    ///
    /// This is why the result pane is editable at all: the three presets cover
    /// most conflicts and none of the interesting ones.
    Custom(Vec<String>),
}

impl Resolution {
    pub fn is_resolved(&self) -> bool {
        !matches!(self, Resolution::Unresolved)
    }
}

/// A run of the file: either text nobody disagreed about, or a conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Context(Vec<String>),
    Conflict(Box<ConflictRegion>),
}

/// A conflicted file, split into its conflicting and non-conflicting runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConflictFile {
    pub segments: Vec<Segment>,
}

/// Why a file could not be parsed as a conflicted one.
///
/// Every variant means the markers are not a shape Git produces. Rendering
/// anything from one of these would risk writing back a file that silently
/// loses a side, so parsing refuses instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictParseError {
    /// A `<<<<<<<` with no `>>>>>>>` after it.
    UnterminatedConflict { line: usize },
    /// A `<<<<<<<` inside a conflict Git had already opened.
    NestedConflict { line: usize },
    /// A `=======` or `>>>>>>>` with no `<<<<<<<` before it.
    StrayMarker { line: usize, marker: String },
    /// A conflict that ended without a `=======` to separate the sides.
    MissingSeparator { line: usize },
}

impl fmt::Display for ConflictParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // One-based, because these are shown next to a file the user can open.
        match self {
            ConflictParseError::UnterminatedConflict { line } => {
                write!(f, "the conflict opened on line {line} is never closed")
            }
            ConflictParseError::NestedConflict { line } => {
                write!(f, "line {line} opens a conflict inside another one")
            }
            ConflictParseError::StrayMarker { line, marker } => {
                write!(f, "line {line} has a `{marker}` outside any conflict")
            }
            ConflictParseError::MissingSeparator { line } => {
                write!(f, "the conflict closed on line {line} has no `=======`")
            }
        }
    }
}

impl std::error::Error for ConflictParseError {}

/// Splits `content` into lines that keep their own terminators.
///
/// Terminators are preserved rather than normalised because a resolver that
/// rewrites a CRLF file as LF turns one conflict into a whole-file diff, and
/// the user gets blamed for it in review. A final line with no terminator stays
/// that way for the same reason.
fn split_lines(content: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        match rest.find('\n') {
            Some(at) => {
                lines.push(rest[..=at].to_owned());
                rest = &rest[at + 1..];
            }
            None => {
                lines.push(rest.to_owned());
                break;
            }
        }
    }
    lines
}

/// The line terminator `line` ends with, which may be empty at end of file.
fn terminator(line: &str) -> &str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// True when `line` is a marker of exactly seven characters, and returns what
/// Git wrote after it.
///
/// The eighth character must be a space or the end of the line: `<<<<<<<<` is
/// eight `<` and is content, not a marker. Getting this wrong on a file that
/// documents conflict markers — this project's own docs, for instance — would
/// corrupt it.
fn marker<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let rest = trimmed.strip_prefix(prefix)?;
    match rest.chars().next() {
        None => Some(""),
        Some(' ') => Some(rest[1..].trim_end()),
        // A longer run of the same character, so not this marker.
        Some(_) => None,
    }
}

/// Reads a file Git left conflict markers in.
///
/// A file with no markers parses fine and comes back as a single context
/// segment. That is not an error: it is what a conflict already resolved by
/// hand looks like, and refusing it would make the resolver useless on exactly
/// the files a careful user has already started fixing.
pub fn parse(content: &str) -> Result<ConflictFile, ConflictParseError> {
    let lines = split_lines(content);
    let mut segments = Vec::new();
    let mut context: Vec<String> = Vec::new();

    let mut index = 0;
    while index < lines.len() {
        let Some(ours_label) = marker(&lines[index], OURS) else {
            // A closing marker out here has nothing to close. Left alone it
            // would be written back into the file as ordinary text, quietly
            // producing a file that still looks conflicted.
            for stray in [SPLIT, THEIRS] {
                if marker(&lines[index], stray).is_some() {
                    return Err(ConflictParseError::StrayMarker {
                        line: index + 1,
                        marker: stray.to_owned(),
                    });
                }
            }
            context.push(lines[index].clone());
            index += 1;
            continue;
        };

        if !context.is_empty() {
            segments.push(Segment::Context(std::mem::take(&mut context)));
        }

        let opened_at = index + 1;
        // Taken from the marker Git itself wrote, so it is the file's own
        // convention rather than the platform's.
        let eol = {
            let found = terminator(&lines[index]);
            if found.is_empty() { "\n" } else { found }.to_owned()
        };
        index += 1;

        let mut ours = Vec::new();
        let mut base: Option<ConflictBase> = None;
        let mut theirs = Vec::new();
        // Which side the lines being read belong to: 0 ours, 1 base, 2 theirs.
        let mut side = 0;
        let mut theirs_label = None;

        while index < lines.len() {
            let line = &lines[index];

            if marker(line, OURS).is_some() {
                return Err(ConflictParseError::NestedConflict { line: index + 1 });
            }
            if let Some(label) = marker(line, BASE)
                && side == 0
            {
                base = Some(ConflictBase {
                    label: label.to_owned(),
                    lines: Vec::new(),
                });
                side = 1;
                index += 1;
                continue;
            }
            if marker(line, SPLIT).is_some() && side < 2 {
                side = 2;
                index += 1;
                continue;
            }
            if let Some(label) = marker(line, THEIRS) {
                if side != 2 {
                    return Err(ConflictParseError::MissingSeparator { line: index + 1 });
                }
                theirs_label = Some(label.to_owned());
                index += 1;
                break;
            }

            match side {
                0 => ours.push(line.clone()),
                1 => base
                    .as_mut()
                    .expect("side 1 is only reachable once base is set")
                    .lines
                    .push(line.clone()),
                _ => theirs.push(line.clone()),
            }
            index += 1;
        }

        let Some(theirs_label) = theirs_label else {
            return Err(ConflictParseError::UnterminatedConflict { line: opened_at });
        };

        segments.push(Segment::Conflict(Box::new(ConflictRegion {
            ours,
            base,
            theirs,
            ours_label: ours_label.to_owned(),
            theirs_label,
            eol,
        })));
    }

    if !context.is_empty() {
        segments.push(Segment::Context(context));
    }

    Ok(ConflictFile { segments })
}

impl ConflictFile {
    /// How many conflicting regions the file has.
    pub fn conflict_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| matches!(s, Segment::Conflict(_)))
            .count()
    }

    /// The conflicting regions, in the order they appear in the file.
    pub fn conflicts(&self) -> impl Iterator<Item = &ConflictRegion> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Conflict(region) => Some(region.as_ref()),
            Segment::Context(_) => None,
        })
    }

    /// Whether every conflict has a resolution.
    ///
    /// A count mismatch is `false` rather than a panic: it means the caller is
    /// holding resolutions for a file that has since been re-read, and
    /// answering "not yet" leaves Continue disabled, which is the safe way to
    /// be wrong.
    pub fn is_resolved(&self, resolutions: &[Resolution]) -> bool {
        resolutions.len() == self.conflict_count()
            && resolutions.iter().all(Resolution::is_resolved)
    }

    /// Rebuilds the file with each conflict replaced by its resolution.
    ///
    /// An `Unresolved` region is written back **with its markers intact**, so a
    /// half-finished resolution can be saved and returned to. Losing a partial
    /// resolution is the failure mode the spec calls out by name, and this is
    /// where it would happen.
    pub fn render(&self, resolutions: &[Resolution]) -> String {
        let mut out = String::new();
        let mut next = 0;

        for segment in &self.segments {
            match segment {
                Segment::Context(lines) => out.extend(lines.iter().map(String::as_str)),
                Segment::Conflict(region) => {
                    let resolution = resolutions.get(next).unwrap_or(&Resolution::Unresolved);
                    next += 1;
                    region.write_into(&mut out, resolution);
                }
            }
        }

        out
    }
}

impl ConflictRegion {
    /// The lines this region contributes under `resolution`.
    ///
    /// Separate from [`Self::write_into`] so the result pane can show exactly
    /// what a preset would produce before the user commits to it.
    pub fn resolved_lines(&self, resolution: &Resolution) -> Vec<String> {
        match resolution {
            Resolution::Ours => self.ours.clone(),
            Resolution::Theirs => self.theirs.clone(),
            Resolution::Both => {
                let mut lines = self.ours.clone();
                lines.extend(self.theirs.iter().cloned());
                lines
            }
            Resolution::BothReversed => {
                let mut lines = self.theirs.clone();
                lines.extend(self.ours.iter().cloned());
                lines
            }
            Resolution::Custom(lines) => lines.clone(),
            // Handled by write_into, which puts the markers back.
            Resolution::Unresolved => Vec::new(),
        }
    }

    fn write_into(&self, out: &mut String, resolution: &Resolution) {
        if matches!(resolution, Resolution::Unresolved) {
            let eol = &self.eol;
            out.push_str(&format!("{OURS} {}{eol}", self.ours_label));
            out.extend(self.ours.iter().map(String::as_str));
            if let Some(base) = &self.base {
                // The label is written back because Git puts the ancestor's
                // short hash there, and a round-trip that dropped it would
                // rewrite a line the user never touched.
                if base.label.is_empty() {
                    out.push_str(&format!("{BASE}{eol}"));
                } else {
                    out.push_str(&format!("{BASE} {}{eol}", base.label));
                }
                out.extend(base.lines.iter().map(String::as_str));
            }
            out.push_str(&format!("{SPLIT}{eol}"));
            out.extend(self.theirs.iter().map(String::as_str));
            out.push_str(&format!("{THEIRS} {}{eol}", self.theirs_label));
            return;
        }

        let lines = self.resolved_lines(resolution);
        // Joining two sides can put a line with no terminator in the middle of
        // the file, which would glue two lines together. Only the very last
        // line of a file may lack one.
        for (position, line) in lines.iter().enumerate() {
            out.push_str(line);
            if position + 1 < lines.len() && terminator(line).is_empty() {
                out.push_str(&self.eol);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `git merge` writes by default.
    const SIMPLE: &str = "\
context before
<<<<<<< HEAD
ours line
=======
theirs line
>>>>>>> feature
context after
";

    /// The shape `merge.conflictStyle = diff3` writes.
    const WITH_BASE: &str = "\
<<<<<<< HEAD
ours line
||||||| merged common ancestors
base line
=======
theirs line
>>>>>>> feature
";

    fn only_conflict(file: &ConflictFile) -> &ConflictRegion {
        file.conflicts().next().expect("there is one conflict")
    }

    #[test]
    fn a_default_conflict_splits_into_context_and_sides() {
        let file = parse(SIMPLE).expect("the markers are well formed");

        assert_eq!(file.conflict_count(), 1);
        assert_eq!(file.segments.len(), 3, "context, conflict, context");

        let region = only_conflict(&file);
        assert_eq!(region.ours, vec!["ours line\n"]);
        assert_eq!(region.theirs, vec!["theirs line\n"]);
        assert_eq!(region.ours_label, "HEAD");
        assert_eq!(region.theirs_label, "feature");
        assert!(
            region.base.is_none(),
            "the default conflict style writes no base"
        );
    }

    #[test]
    fn diff3_style_carries_the_base() {
        let file = parse(WITH_BASE).expect("the markers are well formed");
        let region = only_conflict(&file);

        let base = region.base.as_ref().expect("diff3 carries a base");
        assert_eq!(base.lines, vec!["base line\n"]);
        assert_eq!(base.label, "merged common ancestors");
        assert_eq!(region.ours, vec!["ours line\n"]);
        assert_eq!(region.theirs, vec!["theirs line\n"]);
    }

    #[test]
    fn a_file_with_no_markers_is_one_context_segment() {
        // What an already-resolved file looks like. Refusing it would make the
        // resolver useless on the files a careful user has started fixing.
        let file = parse("just text\nand more\n").expect("plain text parses");
        assert_eq!(file.conflict_count(), 0);
        assert_eq!(file.segments.len(), 1);
        assert_eq!(file.render(&[]), "just text\nand more\n");
    }

    #[test]
    fn every_preset_renders_the_side_it_names() {
        let file = parse(SIMPLE).expect("well formed");

        assert_eq!(
            file.render(&[Resolution::Ours]),
            "context before\nours line\ncontext after\n"
        );
        assert_eq!(
            file.render(&[Resolution::Theirs]),
            "context before\ntheirs line\ncontext after\n"
        );
        assert_eq!(
            file.render(&[Resolution::Both]),
            "context before\nours line\ntheirs line\ncontext after\n"
        );
        assert_eq!(
            file.render(&[Resolution::BothReversed]),
            "context before\ntheirs line\nours line\ncontext after\n"
        );
    }

    #[test]
    fn a_custom_resolution_is_written_verbatim() {
        let file = parse(SIMPLE).expect("well formed");
        let custom = Resolution::Custom(vec!["merged by hand\n".to_owned()]);

        assert_eq!(
            file.render(&[custom]),
            "context before\nmerged by hand\ncontext after\n"
        );
    }

    #[test]
    fn an_unresolved_region_round_trips_byte_for_byte() {
        // The spec's rule that navigation never loses a partial resolution
        // rests on this: a file saved half-done must come back the same.
        let file = parse(SIMPLE).expect("well formed");
        assert_eq!(file.render(&[Resolution::Unresolved]), SIMPLE);

        // Including the base label, which Git fills with the ancestor's short
        // hash: a round-trip that dropped it would rewrite a line nobody
        // touched.
        let with_base = parse(WITH_BASE).expect("well formed");
        assert_eq!(with_base.render(&[Resolution::Unresolved]), WITH_BASE);
    }

    #[test]
    fn a_partially_resolved_file_keeps_the_rest_conflicted() {
        let two = "\
<<<<<<< HEAD
first ours
=======
first theirs
>>>>>>> feature
middle
<<<<<<< HEAD
second ours
=======
second theirs
>>>>>>> feature
";
        let file = parse(two).expect("well formed");
        assert_eq!(file.conflict_count(), 2);

        let rendered = file.render(&[Resolution::Ours, Resolution::Unresolved]);
        assert!(rendered.starts_with("first ours\nmiddle\n"));
        assert!(
            rendered.contains("<<<<<<< HEAD\nsecond ours\n"),
            "the undecided conflict keeps its markers, got:\n{rendered}"
        );
    }

    #[test]
    fn is_resolved_needs_one_decision_per_conflict() {
        let file = parse(SIMPLE).expect("well formed");

        assert!(!file.is_resolved(&[]), "no decisions is not resolved");
        assert!(!file.is_resolved(&[Resolution::Unresolved]));
        assert!(file.is_resolved(&[Resolution::Ours]));
        // A count that does not match means the file was re-read underneath the
        // resolutions. Answering "not yet" leaves Continue disabled, which is
        // the safe way to be wrong.
        assert!(!file.is_resolved(&[Resolution::Ours, Resolution::Ours]));
    }

    #[test]
    fn eight_angle_brackets_are_content_not_a_marker() {
        // A file documenting conflict markers is the realistic way to hit this,
        // and this repository contains several.
        let text = "<<<<<<<<\nnot a conflict\n";
        let file = parse(text).expect("eight brackets are ordinary text");
        assert_eq!(file.conflict_count(), 0);
        assert_eq!(file.render(&[]), text);
    }

    #[test]
    fn crlf_line_endings_survive_a_resolution() {
        // Rewriting a CRLF file as LF turns one conflict into a whole-file diff
        // that the user gets blamed for in review.
        let text =
            "before\r\n<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> feature\r\nafter\r\n";
        let file = parse(text).expect("well formed");

        assert_eq!(
            file.render(&[Resolution::Ours]),
            "before\r\nours\r\nafter\r\n"
        );
        assert_eq!(file.render(&[Resolution::Unresolved]), text);
    }

    #[test]
    fn a_side_with_no_lines_is_a_deletion_not_an_error() {
        // One side deleting what the other changed is an ordinary conflict, and
        // "take theirs" here means "keep it deleted".
        let text = "<<<<<<< HEAD\nours\n=======\n>>>>>>> feature\n";
        let file = parse(text).expect("an empty side is well formed");
        let region = only_conflict(&file);

        assert!(region.theirs.is_empty());
        assert_eq!(file.render(&[Resolution::Theirs]), "");
        assert_eq!(file.render(&[Resolution::Ours]), "ours\n");
    }

    #[test]
    fn joining_two_sides_never_glues_lines_together() {
        // A file whose last line has no terminator. Taking both sides moves
        // that line into the middle, where it would otherwise swallow the one
        // after it and silently produce `theirsours`.
        let file = parse("<<<<<<< HEAD\nours\n=======\ntheirs>>>>>>> feature\n");
        // `theirs>>>>>>> feature` is one line, so the closing marker never
        // appears on a line of its own.
        assert_eq!(
            file,
            Err(ConflictParseError::UnterminatedConflict { line: 1 })
        );

        let unterminated = ConflictRegion {
            ours: vec!["ours".to_owned()],
            base: None,
            theirs: vec!["theirs\n".to_owned()],
            ours_label: "HEAD".to_owned(),
            theirs_label: "feature".to_owned(),
            eol: "\n".to_owned(),
        };
        assert_eq!(
            unterminated.resolved_lines(&Resolution::Both),
            vec!["ours".to_owned(), "theirs\n".to_owned()]
        );

        let mut out = String::new();
        unterminated.write_into(&mut out, &Resolution::Both);
        assert_eq!(out, "ours\ntheirs\n", "the missing terminator is supplied");
    }

    #[test]
    fn an_unterminated_conflict_is_refused() {
        let error =
            parse("<<<<<<< HEAD\nours\n=======\ntheirs\n").expect_err("there is no closing marker");
        assert_eq!(
            error,
            ConflictParseError::UnterminatedConflict { line: 1 },
            "the error names the line the conflict opened on"
        );
    }

    #[test]
    fn a_nested_conflict_is_refused() {
        let error = parse("<<<<<<< HEAD\n<<<<<<< HEAD\n=======\n>>>>>>> f\n")
            .expect_err("Git never nests conflicts");
        assert_eq!(error, ConflictParseError::NestedConflict { line: 2 });
    }

    #[test]
    fn a_stray_closing_marker_is_refused() {
        // Left alone this would be written back as ordinary text, producing a
        // file that still looks conflicted to everyone who opens it.
        let error = parse("text\n=======\nmore\n").expect_err("nothing opened this");
        assert_eq!(
            error,
            ConflictParseError::StrayMarker {
                line: 2,
                marker: SPLIT.to_owned()
            }
        );
    }

    #[test]
    fn a_conflict_closed_without_a_separator_is_refused() {
        let error =
            parse("<<<<<<< HEAD\nours\n>>>>>>> feature\n").expect_err("there is no `=======`");
        assert_eq!(error, ConflictParseError::MissingSeparator { line: 3 });
    }

    #[test]
    fn the_error_wording_names_the_line() {
        let error = ConflictParseError::UnterminatedConflict { line: 12 };
        assert!(error.to_string().contains("line 12"), "got {error}");
    }
}
