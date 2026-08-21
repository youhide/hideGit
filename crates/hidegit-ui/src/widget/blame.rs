//! The blame view: every line of a file, with the commit that wrote it.
//!
//! Read as a column of blocks rather than a column of lines. Consecutive lines
//! from one commit are what a reader is actually looking for — "this paragraph
//! arrived together" — so the gutter names a commit once per block instead of
//! repeating itself down the page, and the blocks alternate in weight so the
//! boundaries are visible without a rule between them.

use hidegit_core::model::{Commit, ObjectId};
use hidegit_core::ops::BlameLine;
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Center, Fill, Font, Length};

use crate::Element;
use crate::format;
use crate::message::RepoMessage;
use crate::metrics;
use crate::state::BlameView;
use crate::theme::Palette;
use crate::widget::common;

/// Wide enough for a short hash, a name and a relative date without wrapping.
const GUTTER_WIDTH: f32 = 250.0;

/// The block each line belongs to: consecutive lines from the same commit share
/// one, and a block starts wherever the commit changes.
///
/// A function rather than a counter kept inside `view`, because this is the rule
/// the banding depends on and it is the thing worth asserting. Held as a rule of
/// its own, a test can run *it* — rather than a copy of it written next to the
/// test, which is what was here before and would have gone on passing however
/// the view drifted.
fn blocks(lines: &[BlameLine]) -> Vec<usize> {
    let mut out = Vec::with_capacity(lines.len());
    let mut previous: Option<ObjectId> = None;
    let mut block = 0usize;

    for line in lines {
        if previous.is_some_and(|p| p != line.commit) {
            block += 1;
        }
        previous = Some(line.commit);
        out.push(block);
    }
    out
}

pub fn view<'a>(blame: &'a BlameView, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let mut rows = column![];
    let blocks = blocks(&blame.lines);

    for (index, line) in blame.lines.iter().enumerate() {
        // Only the first line of a block carries the gutter text: repeating the
        // same hash for forty lines is noise that makes the boundaries harder to
        // see, not easier.
        let block = blocks[index];
        let starts_block = index == 0 || blocks[index - 1] != block;

        // Banded by *block*, not by commit. Banding by commit was tried and is
        // subtly wrong: a file where one commit's lines appear twice with
        // another commit between them can put two same-coloured blocks
        // side by side, and the boundary the banding exists to show disappears.
        let banded = block % 2 == 1;
        let gutter = if starts_block {
            attribution(line.commit, blame.commits.get(&line.commit), palette)
        } else {
            Space::new().width(Length::Fixed(GUTTER_WIDTH)).into()
        };

        let body = row![
            gutter,
            text(format!("{:>5}", line.lineno))
                .size(metrics::text::CODE)
                .font(Font::MONOSPACE)
                .color(palette.muted),
            text(line.text.clone())
                .size(metrics::text::CODE)
                .font(Font::MONOSPACE)
                .color(palette.text),
        ]
        .spacing(10)
        .align_y(Center);

        // Alternating bands rather than a rule between blocks: a rule per block
        // turns a long file into a ladder.
        rows = rows.push(container(body).width(Fill).padding([1, 8]).style(move |_| {
            container::Style {
                background: banded.then(|| palette.selection_idle.into()),
                ..container::Style::default()
            }
        }));
    }

    column![
        header(blame, palette),
        common::divider(palette),
        container(scrollable(rows).height(Fill)).height(Fill),
    ]
    .height(Fill)
    .into()
}

/// The gutter entry for a block: who wrote it, and when.
fn attribution<'a>(
    id: ObjectId,
    commit: Option<&'a Commit>,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let hash = text(id.short(7))
        .size(metrics::text::LABEL)
        .font(Font::MONOSPACE)
        .color(palette.accent);

    let rest: Element<'_, RepoMessage> = match commit {
        Some(commit) => row![
            text(format::truncate(&commit.author.name, 110.0))
                .size(metrics::text::LABEL)
                .color(palette.muted),
            text(format::relative_time(commit.time))
                .size(metrics::text::LABEL)
                .color(palette.muted),
        ]
        .spacing(8)
        .into(),
        // A commit that could not be read costs its own gutter entry, not the
        // view — the hash is still enough to look it up by hand.
        None => text("(commit not read)")
            .size(metrics::text::LABEL)
            .color(palette.muted)
            .into(),
    };

    container(row![hash, rest].spacing(8).align_y(Center))
        .width(Length::Fixed(GUTTER_WIDTH))
        .into()
}

fn header<'a>(blame: &'a BlameView, palette: &'a Palette) -> Element<'a, RepoMessage> {
    container(
        row![
            text(blame.path.display().to_string())
                .size(metrics::text::CODE)
                .color(palette.text),
            // The revision is named because blame answers a different question
            // at every one of them, and a view that hid which it used would be
            // showing an answer to a question nobody asked.
            text(format!("blamed at {}", blame.at.short(7)))
                .size(metrics::text::LABEL)
                .font(Font::MONOSPACE)
                .color(palette.muted),
            Space::new().width(Fill),
            button(text("Close").size(metrics::text::LABEL))
                .padding([3, 9])
                .style(move |_, status| button::Style {
                    background: Some(
                        match status {
                            button::Status::Hovered => palette.border,
                            _ => palette.surface,
                        }
                        .into(),
                    ),
                    text_color: palette.text,
                    border: iced::Border {
                        color: palette.border,
                        width: 1.0,
                        radius: 3.0.into(),
                    },
                    ..button::Style::default()
                })
                .on_press(RepoMessage::BlameDismissed),
        ]
        .spacing(10)
        .align_y(Center),
    )
    .padding([6, 10])
    .width(Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(commit: u8, lineno: u32) -> BlameLine {
        BlameLine {
            commit: ObjectId::from_hex(&format!("{commit:040x}")).unwrap(),
            lineno,
            text: format!("line {lineno}"),
        }
    }

    #[test]
    fn adjacent_blocks_never_share_a_band() {
        // Banding by commit rather than by block was tried, and this is the
        // shape that breaks it: commit 1 appears twice with commit 2 between,
        // so the third block and the first would land on the same band and the
        // boundary the banding exists to show disappears.
        let lines = vec![line(1, 1), line(1, 2), line(2, 3), line(1, 4)];

        let blocks = blocks(&lines);
        assert_eq!(blocks, vec![0, 0, 1, 2]);
        for pair in blocks.windows(2) {
            if pair[0] != pair[1] {
                assert_ne!(
                    pair[0] % 2,
                    pair[1] % 2,
                    "two blocks meeting on the same band: {blocks:?}"
                );
            }
        }
    }
}
