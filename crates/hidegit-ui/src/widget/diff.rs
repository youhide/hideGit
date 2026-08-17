//! The diff viewer: unified and side-by-side.
//!
//! Syntax highlighting was deliberately absent in M1 — correct diffing first —
//! and arrived in M6; see [`crate::highlight`] for why a diff needs two parsers
//! rather than one. Binary and oversized files get a placeholder rather than an
//! attempt to render them, because the alternative is a hang.

use std::collections::BTreeSet;

use hidegit_core::model::{Diff, FileDiff, FileDiffContent, Hunk, LfsPointer, LineKind};
use iced::widget::{Space, button, column, container, rich_text, row, scrollable, text};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::format;
use crate::message::RepoMessage;
use crate::state::DiffMode;
use crate::theme::Palette;

const CODE_SIZE: f32 = 12.0;
const GUTTER_WIDTH: f32 = 44.0;

fn mono() -> Font {
    Font::MONOSPACE
}

/// What the staging view adds to a diff: actions, and a line selection.
///
/// Absent when a commit's diff is being read, which has nothing to stage.
#[derive(Debug, Clone, Copy)]
pub struct Staging<'a> {
    /// This diff is the index against `HEAD`, so its actions unstage.
    pub staged: bool,
    /// Chosen changed lines, as `(hunk, line)` indices into the diff.
    pub lines: &'a BTreeSet<(usize, usize)>,
    /// The hunk `J`/`K` last stepped to, highlighted so the keys do something
    /// visible.
    pub focused_hunk: usize,
}

impl Staging<'_> {
    fn verb(&self) -> &'static str {
        if self.staged { "Unstage" } else { "Stage" }
    }

    fn selected(&self, hunk: usize, line: usize) -> bool {
        self.lines.contains(&(hunk, line))
    }

    fn any_in(&self, hunk: usize) -> bool {
        self.lines.iter().any(|(h, _)| *h == hunk)
    }
}

/// Renders one file's diff.
/// What changed about an LFS-tracked file, in the only terms available.
///
/// Both sides being pointers is the ordinary case and reads as a size change.
/// One side missing is a file moving into or out of LFS, which is a change of
/// *storage* rather than of content — worth saying plainly, because a diff that
/// showed nothing would look like nothing happened.
fn lfs_summary(old: Option<&LfsPointer>, new: Option<&LfsPointer>) -> String {
    match (old, new) {
        (Some(old), Some(new)) if old.oid == new.oid => {
            format!("unchanged, {}", format::bytes(new.size))
        }
        // Two different files that happen to be the same length. Rendering
        // that as "4.0 KiB → 4.0 KiB" would read as a no-op, and deciding
        // sameness by size rather than by object would call it one.
        (Some(old), Some(new)) if old.size == new.size => {
            format!("changed, {}", format::bytes(new.size))
        }
        (Some(old), Some(new)) => {
            format!("{} → {}", format::bytes(old.size), format::bytes(new.size))
        }
        (None, Some(new)) => format!("now tracked by LFS, {}", format::bytes(new.size)),
        (Some(old), None) => format!("no longer tracked by LFS, was {}", format::bytes(old.size)),
        // Unreachable through `assemble`, which only produces this variant when
        // a side parsed as a pointer. Stated rather than unwrapped.
        (None, None) => "no pointer on either side".to_owned(),
    }
}

pub fn view<'a>(
    diff: &'a Diff,
    file: usize,
    mode: DiffMode,
    palette: &'a Palette,
    staging: Option<Staging<'a>>,
) -> Element<'a, RepoMessage> {
    let Some(file) = diff.files.get(file) else {
        return empty("Select a file to see its changes", palette);
    };

    match &file.content {
        FileDiffContent::Binary => empty(
            &format!("{} is binary — no text to show", file.path.display()),
            palette,
        ),
        FileDiffContent::TooLarge { bytes } => empty(
            &format!(
                "{} is {} — too large to diff without stalling",
                file.path.display(),
                format::bytes(*bytes)
            ),
            palette,
        ),
        // The size, not the pointer. Three lines of `oid sha256:…` are what
        // Git stores, not what changed — and "4.2 MB → 5.1 MB" is the most a
        // diff can honestly say about a file whose content it does not have.
        FileDiffContent::Lfs { old, new } => empty(
            &format!(
                "{} is stored with Git LFS — {}",
                file.path.display(),
                lfs_summary(old.as_ref(), new.as_ref())
            ),
            palette,
        ),
        FileDiffContent::Text { hunks } if hunks.is_empty() => {
            empty("No textual changes — the file's mode changed", palette)
        }
        FileDiffContent::Text { hunks } => match mode {
            DiffMode::Unified => unified(file, hunks, palette, staging),
            DiffMode::SideBySide => side_by_side(file, hunks, palette, staging),
        },
    }
}

fn unified<'a>(
    file: &'a FileDiff,
    hunks: &'a [Hunk],
    palette: &'a Palette,
    staging: Option<Staging<'a>>,
) -> Element<'a, RepoMessage> {
    let mut body = column![].spacing(0);
    let mut painter = painter_for(file, hunks, palette);

    for (index, hunk) in hunks.iter().enumerate() {
        body = body.push(hunk_header(hunk, index, palette, staging));

        for (position, line) in hunk.lines.iter().enumerate() {
            let (marker, colour, background) = match line.kind {
                LineKind::Added => ("+", palette.text, Some(palette.added)),
                LineKind::Removed => ("−", palette.text, Some(palette.removed)),
                LineKind::Context => (" ", palette.muted, None),
            };

            let numbers = row![
                gutter(line.old_lineno, palette),
                gutter(line.new_lineno, palette),
            ];

            // The marker is a glyph as well as a colour, so an added line is
            // identifiable without relying on hue.
            let content = row![
                numbers,
                text(marker).size(CODE_SIZE).font(mono()).color(colour),
                line_text(painter.line(line.kind, line.text.as_str(), colour)),
            ]
            .spacing(6);

            let chosen = staging.is_some_and(|s| s.selected(index, position));

            // Marked twice over: a bar in the margin and a lightened
            // background. The bar is what survives without colour, and it is
            // what lets the background stay subtle enough that the
            // added/removed reading is not traded away for the selection one.
            let bar = text(if chosen { "▌" } else { " " })
                .size(CODE_SIZE)
                .font(mono())
                .color(palette.accent);

            let background = match (chosen, background) {
                (true, Some(base)) => Some(lighten(base)),
                (true, None) => Some(iced::Color {
                    a: 0.22,
                    ..palette.accent
                }),
                (false, base) => base,
            };

            let painted = container(row![bar, content].spacing(2))
                .width(Fill)
                .padding(Padding::from([1, 8]))
                .style(move |_| container::Style {
                    background: background.map(Into::into),
                    ..container::Style::default()
                });

            // Only a changed line can be chosen: context is what `git apply`
            // matches on, so it is never optional.
            body = body.push(match (staging, line.kind) {
                (Some(_), LineKind::Added | LineKind::Removed) => {
                    selectable(painted.into(), index, position)
                }
                _ => painted.into(),
            });
        }
    }

    with_header(
        file,
        scrollable(body).height(Fill).into(),
        palette,
        staging,
        hunks,
    )
}

/// Brightens a diff-line background to mark it as chosen.
///
/// Deliberately not a blend toward the accent: the accent is a bright blue and
/// the backgrounds are very dark, so even a fifth of it turns a removal purple
/// and an addition teal — trading the added/removed reading for the selection
/// one rather than adding to it. Raising the line's own lightness keeps red red
/// and green green, and the margin bar carries the accent instead.
fn lighten(base: iced::Color) -> iced::Color {
    const FACTOR: f32 = 2.1;
    iced::Color {
        r: (base.r * FACTOR).min(1.0),
        g: (base.g * FACTOR).min(1.0),
        b: (base.b * FACTOR).min(1.0),
        a: 1.0,
    }
}

/// Wraps a changed line so clicking it toggles its selection.
///
/// The button paints nothing: the line inside it already has an opaque
/// background, so anything painted here would be hidden behind it. That is the
/// bug this shape exists to avoid — the selection has to be drawn *by* the
/// line, not underneath it.
fn selectable<'a>(
    line: Element<'a, RepoMessage>,
    hunk: usize,
    position: usize,
) -> Element<'a, RepoMessage> {
    button(line)
        .width(Fill)
        .padding(0)
        .style(|_, _| button::Style::default())
        .on_press(RepoMessage::LineToggled(hunk, position))
        .into()
}

fn side_by_side<'a>(
    file: &'a FileDiff,
    hunks: &'a [Hunk],
    palette: &'a Palette,
    staging: Option<Staging<'a>>,
) -> Element<'a, RepoMessage> {
    /// One display row: what to show on the left, and what on the right.
    ///
    /// Removals and additions are paired so the two panes stay vertically
    /// aligned; a side without a counterpart gets a blank rather than letting
    /// the panes drift apart.
    enum Pair<'l> {
        /// Which hunk starts here, so the header can carry its action.
        Header(usize),
        Lines {
            left: Option<&'l hidegit_core::model::DiffLine>,
            right: Option<&'l hidegit_core::model::DiffLine>,
        },
    }

    let mut pairs: Vec<Pair<'a>> = Vec::new();

    for (index, hunk) in hunks.iter().enumerate() {
        pairs.push(Pair::Header(index));

        let mut removals = Vec::new();
        let mut additions = Vec::new();

        let flush = |removals: &mut Vec<&'a hidegit_core::model::DiffLine>,
                     additions: &mut Vec<&'a hidegit_core::model::DiffLine>,
                     out: &mut Vec<Pair<'a>>| {
            for i in 0..removals.len().max(additions.len()) {
                out.push(Pair::Lines {
                    left: removals.get(i).copied(),
                    right: additions.get(i).copied(),
                });
            }
            removals.clear();
            additions.clear();
        };

        for line in &hunk.lines {
            match line.kind {
                LineKind::Removed => removals.push(line),
                LineKind::Added => additions.push(line),
                LineKind::Context => {
                    flush(&mut removals, &mut additions, &mut pairs);
                    pairs.push(Pair::Lines {
                        left: Some(line),
                        right: Some(line),
                    });
                }
            }
        }

        flush(&mut removals, &mut additions, &mut pairs);
    }

    // Coloured up front, in the diff's own order, because the parser's state
    // after a line depends on having seen the one before it — and the panes are
    // built by walking pairs, which is not that order.
    let mut painter = painter_for(file, hunks, palette);
    let mut coloured: std::collections::HashMap<*const hidegit_core::model::DiffLine, Vec<_>> =
        std::collections::HashMap::new();

    for hunk in hunks {
        for line in &hunk.lines {
            let colour = match line.kind {
                LineKind::Context => palette.muted,
                _ => palette.text,
            };
            coloured.insert(
                std::ptr::from_ref(line),
                painter.line(line.kind, line.text.as_str(), colour),
            );
        }
    }

    let mut left = column![].spacing(0);
    let mut right = column![].spacing(0);

    for pair in &pairs {
        match pair {
            Pair::Header(index) => {
                // Both panes carry the header; only the right one carries the
                // action, so the same button is not offered twice.
                left = left.push(hunk_header(&hunks[*index], *index, palette, None));
                right = right.push(hunk_header(&hunks[*index], *index, palette, staging));
            }
            Pair::Lines { left: l, right: r } => {
                left = left.push(match l {
                    Some(line) if line.kind == LineKind::Context => {
                        code_line(line.old_lineno, spans(&coloured, line), " ", None, palette)
                    }
                    Some(line) => code_line(
                        line.old_lineno,
                        spans(&coloured, line),
                        "−",
                        Some(palette.removed),
                        palette,
                    ),
                    None => blank_line(palette),
                });
                right = right.push(match r {
                    Some(line) if line.kind == LineKind::Context => {
                        code_line(line.new_lineno, spans(&coloured, line), " ", None, palette)
                    }
                    Some(line) => code_line(
                        line.new_lineno,
                        spans(&coloured, line),
                        "+",
                        Some(palette.added),
                        palette,
                    ),
                    None => blank_line(palette),
                });
            }
        }
    }

    let border = palette.border;
    let panes = row![
        scrollable(left).width(Fill),
        container(Space::new().width(1))
            .height(Fill)
            .style(move |_| container::Style {
                background: Some(border.into()),
                ..container::Style::default()
            }),
        scrollable(right).width(Fill),
    ];

    with_header(file, panes.height(Fill).into(), palette, staging, hunks)
}

fn with_header<'a>(
    file: &'a FileDiff,
    body: Element<'a, RepoMessage>,
    palette: &'a Palette,
    staging: Option<Staging<'a>>,
    hunks: &'a [Hunk],
) -> Element<'a, RepoMessage> {
    let (insertions, deletions) =
        match &file.content {
            FileDiffContent::Text { hunks } => hunks.iter().flat_map(|h| &h.lines).fold(
                (0, 0),
                |(added, removed), line| match line.kind {
                    LineKind::Added => (added + 1, removed),
                    LineKind::Removed => (added, removed + 1),
                    LineKind::Context => (added, removed),
                },
            ),
            _ => (0, 0),
        };

    let surface = palette.surface;
    let mut bar = row![
        text(format!("{} {}", file.status.code(), file.path.display()))
            .size(12.0)
            .font(mono())
            .color(palette.text),
        Space::new().width(Fill),
        text(format::diff_stat(insertions, deletions))
            .size(12.0)
            .color(palette.muted),
    ]
    .spacing(10)
    .align_y(Center);

    if let Some(staging) = staging {
        // A line selection takes precedence over the whole file: having picked
        // lines, the obvious next action is to act on exactly those.
        let count = staging.lines.len();
        bar = bar.push(if count > 0 {
            action(
                &format!("{} {count} line{}", staging.verb(), plural(count)),
                RepoMessage::SelectedLinesStageRequested,
                palette,
            )
        } else {
            action(
                &format!("{} file", staging.verb()),
                RepoMessage::FileStageRequested,
                palette,
            )
        });
        let _ = hunks;
    }

    let header = container(bar)
        .width(Fill)
        .padding(Padding::from([6, 10]))
        .style(move |_| container::Style {
            background: Some(surface.into()),
            ..container::Style::default()
        });

    column![header, body].into()
}

fn hunk_header<'a>(
    hunk: &'a Hunk,
    index: usize,
    palette: &Palette,
    staging: Option<Staging<'a>>,
) -> Element<'a, RepoMessage> {
    let surface = palette.surface;
    let accent = palette.accent;
    // `J`/`K` moved here. Marking it is what makes those keys do something the
    // user can see — before this, the field they updated was read by nothing.
    let focused = staging.is_some_and(|s| s.focused_hunk == index);

    let mut bar = row![
        text(hunk.header.as_str())
            .size(CODE_SIZE)
            .font(mono())
            .color(accent),
        Space::new().width(Fill),
    ]
    .spacing(10)
    .align_y(Center);

    if let Some(staging) = staging {
        // A hunk with lines picked out of it offers the lines instead: acting
        // on the whole hunk would silently include what was left out.
        if !staging.any_in(index) {
            bar = bar.push(action(
                &format!("{} hunk", staging.verb()),
                RepoMessage::HunkStageRequested(index),
                palette,
            ));
        }
    }

    container(bar)
        .width(Fill)
        .padding(Padding::from([3, 8]))
        .style(move |_| container::Style {
            background: Some(
                if focused {
                    iced::Color { a: 0.18, ..accent }
                } else {
                    surface
                }
                .into(),
            ),
            ..container::Style::default()
        })
        .into()
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// A small text action on a header bar.
fn action<'a>(label: &str, message: RepoMessage, palette: &Palette) -> Element<'a, RepoMessage> {
    let palette = *palette;
    let owned = label.to_owned();

    button(container(text(owned).size(11.0)).padding(Padding::from([2, 8])))
        .padding(0)
        .style(move |_, status| {
            let background = match status {
                button::Status::Hovered | button::Status::Pressed => Some(
                    iced::Color {
                        a: 0.22,
                        ..palette.accent
                    }
                    .into(),
                ),
                _ => Some(
                    iced::Color {
                        a: 0.12,
                        ..palette.accent
                    }
                    .into(),
                ),
            };
            button::Style {
                background,
                text_color: palette.text,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..iced::Border::default()
                },
                ..button::Style::default()
            }
        })
        .on_press(message)
        .into()
}

/// One line of code, as cheap a widget as it can be.
///
/// A line the highlighter left in one piece is a plain `text`, not a `rich_text`
/// of one span: most of a diff is such lines, and `Rich` lays out per span. It
/// is also what keeps an unhighlighted diff visible to the render tests, which
/// see `text` widgets and not the insides of rich ones.
fn line_text<'a>(spans: Vec<iced::widget::text::Span<'a, (), Font>>) -> Element<'a, RepoMessage> {
    match <[_; 1]>::try_from(spans) {
        Ok([only]) => text(only.text)
            .size(CODE_SIZE)
            .font(only.font.unwrap_or_else(mono))
            .color_maybe(only.color)
            .into(),
        Err(spans) => rich_text(spans).size(CODE_SIZE).into(),
    }
}

/// A painter for this file, or a plain one when the diff is too big to colour.
fn painter_for(file: &FileDiff, hunks: &[Hunk], palette: &Palette) -> crate::highlight::Painter {
    let lines = hunks.iter().map(|hunk| hunk.lines.len()).sum();
    crate::highlight::Painter::new(&file.path, lines, palette)
}

/// The coloured pieces of a line, looked up by identity.
///
/// Keyed by address because a line has no id and two identical lines in one
/// hunk are genuinely different rows, each with its own parser state behind it.
fn spans<'a>(
    coloured: &std::collections::HashMap<
        *const hidegit_core::model::DiffLine,
        Vec<iced::widget::text::Span<'a, (), Font>>,
    >,
    line: &'a hidegit_core::model::DiffLine,
) -> Vec<iced::widget::text::Span<'a, (), Font>> {
    coloured
        .get(&std::ptr::from_ref(line))
        .cloned()
        .unwrap_or_default()
}

fn code_line<'a>(
    lineno: Option<u32>,
    content: Vec<iced::widget::text::Span<'a, (), Font>>,
    marker: &'static str,
    background: Option<iced::Color>,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    let colour = if background.is_some() {
        palette.text
    } else {
        palette.muted
    };

    // Side by side, the pane a line sits in already implies its kind, but hue
    // alone would carry it for anyone who cannot separate the two backgrounds.
    // The glyph is the same one the unified view uses.
    container(
        row![
            gutter(lineno, palette),
            text(marker).size(CODE_SIZE).font(mono()).color(colour),
            line_text(content),
        ]
        .spacing(6),
    )
    .width(Fill)
    .padding(Padding::from([1, 8]))
    .style(move |_| container::Style {
        background: background.map(Into::into),
        ..container::Style::default()
    })
    .into()
}

fn blank_line<'a>(palette: &Palette) -> Element<'a, RepoMessage> {
    let background = iced::Color {
        a: 0.35,
        ..palette.surface
    };
    container(text(" ").size(CODE_SIZE).font(mono()))
        .width(Fill)
        .padding(Padding::from([1, 8]))
        .style(move |_| container::Style {
            background: Some(background.into()),
            ..container::Style::default()
        })
        .into()
}

fn gutter<'a>(lineno: Option<u32>, palette: &Palette) -> Element<'a, RepoMessage> {
    let label = lineno.map(|n| n.to_string()).unwrap_or_default();

    container(
        text(label)
            .size(CODE_SIZE)
            .font(mono())
            .color(palette.muted)
            .align_x(iced::alignment::Horizontal::Right)
            .width(Fill),
    )
    .width(Length::Fixed(GUTTER_WIDTH))
    .into()
}

fn empty<'a>(message: &str, palette: &Palette) -> Element<'a, RepoMessage> {
    container(text(message.to_owned()).size(13.0).color(palette.muted))
        .width(Fill)
        .height(Fill)
        .center(Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(oid: &str, size: u64) -> LfsPointer {
        LfsPointer {
            oid: format!("sha256:{}", oid.repeat(64)),
            size,
        }
    }

    #[test]
    fn two_pointers_read_as_the_size_change_they_stand_for() {
        // The only thing a diff can honestly say about a file whose content it
        // does not have.
        assert_eq!(
            lfs_summary(Some(&pointer("a", 1024)), Some(&pointer("b", 4096))),
            "1.0 KiB → 4.0 KiB"
        );
    }

    #[test]
    fn the_same_object_on_both_sides_does_not_read_as_a_change() {
        // A mode change, or a rename. Showing "4.0 KB → 4.0 KB" would imply
        // something moved that did not.
        assert_eq!(
            lfs_summary(Some(&pointer("a", 4096)), Some(&pointer("a", 4096))),
            "unchanged, 4.0 KiB"
        );
    }

    #[test]
    fn two_different_files_of_the_same_length_do_not_read_as_a_no_op() {
        // Sameness is the object, not the size. Judging by size would call this
        // unchanged, and "4.0 KiB → 4.0 KiB" would read as one too.
        assert_eq!(
            lfs_summary(Some(&pointer("a", 4096)), Some(&pointer("b", 4096))),
            "changed, 4.0 KiB"
        );
    }

    #[test]
    fn a_file_moving_into_or_out_of_lfs_says_which() {
        // A change of storage rather than of content. A placeholder that showed
        // nothing would look like nothing happened.
        assert_eq!(
            lfs_summary(None, Some(&pointer("a", 8192))),
            "now tracked by LFS, 8.0 KiB"
        );
        assert_eq!(
            lfs_summary(Some(&pointer("a", 8192)), None),
            "no longer tracked by LFS, was 8.0 KiB"
        );
    }
}
