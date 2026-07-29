//! The diff viewer: unified and side-by-side.
//!
//! Syntax highlighting is deliberately absent in M1 — correct diffing first.
//! Binary and oversized files get a placeholder rather than an attempt to
//! render them, because the alternative is a hang.

use hidegit_core::model::{Diff, FileDiff, FileDiffContent, Hunk, LineKind};
use iced::widget::{Space, column, container, row, scrollable, text};
use iced::{Fill, Font, Length, Padding};

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

/// Renders one file's diff.
pub fn view<'a>(
    diff: &'a Diff,
    file: usize,
    mode: DiffMode,
    palette: &'a Palette,
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
        FileDiffContent::Text { hunks } if hunks.is_empty() => {
            empty("No textual changes — the file's mode changed", palette)
        }
        FileDiffContent::Text { hunks } => match mode {
            DiffMode::Unified => unified(file, hunks, palette),
            DiffMode::SideBySide => side_by_side(file, hunks, palette),
        },
    }
}

fn unified<'a>(
    file: &'a FileDiff,
    hunks: &'a [Hunk],
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let mut body = column![].spacing(0);

    for hunk in hunks {
        body = body.push(hunk_header(&hunk.header, palette));

        for line in &hunk.lines {
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
                text(line.text.as_str())
                    .size(CODE_SIZE)
                    .font(mono())
                    .color(colour),
            ]
            .spacing(6);

            body = body.push(
                container(content)
                    .width(Fill)
                    .padding(Padding::from([1, 8]))
                    .style(move |_| container::Style {
                        background: background.map(Into::into),
                        ..container::Style::default()
                    }),
            );
        }
    }

    with_header(file, scrollable(body).height(Fill).into(), palette)
}

fn side_by_side<'a>(
    file: &'a FileDiff,
    hunks: &'a [Hunk],
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    /// One display row: what to show on the left, and what on the right.
    ///
    /// Removals and additions are paired so the two panes stay vertically
    /// aligned; a side without a counterpart gets a blank rather than letting
    /// the panes drift apart.
    enum Pair<'l> {
        Header(&'l str),
        Lines {
            left: Option<&'l hidegit_core::model::DiffLine>,
            right: Option<&'l hidegit_core::model::DiffLine>,
        },
    }

    let mut pairs: Vec<Pair<'a>> = Vec::new();

    for hunk in hunks {
        pairs.push(Pair::Header(&hunk.header));

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

    let mut left = column![].spacing(0);
    let mut right = column![].spacing(0);

    for pair in &pairs {
        match pair {
            Pair::Header(header) => {
                left = left.push(hunk_header(header, palette));
                right = right.push(hunk_header(header, palette));
            }
            Pair::Lines { left: l, right: r } => {
                left = left.push(match l {
                    Some(line) if line.kind == LineKind::Context => {
                        code_line(line.old_lineno, &line.text, None, palette)
                    }
                    Some(line) => {
                        code_line(line.old_lineno, &line.text, Some(palette.removed), palette)
                    }
                    None => blank_line(palette),
                });
                right = right.push(match r {
                    Some(line) if line.kind == LineKind::Context => {
                        code_line(line.new_lineno, &line.text, None, palette)
                    }
                    Some(line) => {
                        code_line(line.new_lineno, &line.text, Some(palette.added), palette)
                    }
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

    with_header(file, panes.height(Fill).into(), palette)
}

fn with_header<'a>(
    file: &'a FileDiff,
    body: Element<'a, RepoMessage>,
    palette: &'a Palette,
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
    let header = container(
        row![
            text(format!("{} {}", file.status.code(), file.path.display()))
                .size(12.0)
                .font(mono())
                .color(palette.text),
            Space::new().width(Fill),
            text(format::diff_stat(insertions, deletions))
                .size(12.0)
                .color(palette.muted),
        ]
        .align_y(iced::Center),
    )
    .width(Fill)
    .padding(Padding::from([6, 10]))
    .style(move |_| container::Style {
        background: Some(surface.into()),
        ..container::Style::default()
    });

    column![header, body].into()
}

fn hunk_header<'a>(header: &'a str, palette: &Palette) -> Element<'a, RepoMessage> {
    let surface = palette.surface;
    container(
        text(header)
            .size(CODE_SIZE)
            .font(mono())
            .color(palette.accent),
    )
    .width(Fill)
    .padding(Padding::from([3, 8]))
    .style(move |_| container::Style {
        background: Some(surface.into()),
        ..container::Style::default()
    })
    .into()
}

fn code_line<'a>(
    lineno: Option<u32>,
    content: &str,
    background: Option<iced::Color>,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    let colour = if background.is_some() {
        palette.text
    } else {
        palette.muted
    };

    container(
        row![
            gutter(lineno, palette),
            text(content.to_owned())
                .size(CODE_SIZE)
                .font(mono())
                .color(colour),
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
