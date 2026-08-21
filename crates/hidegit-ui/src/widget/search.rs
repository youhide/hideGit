//! Commit search.
//!
//! One box, and every field searched behind it: summary, message body, author
//! and hash. People type a fragment and expect it found — asking them to first
//! classify what they typed is asking them to do the computer's job.
//!
//! Each hit says **which** field matched. A list that cannot distinguish "this
//! matched the message" from "this matched the author" leaves the reader to
//! guess, and guessing wrong sends them back to the terminal.

use hidegit_core::ops::SearchHit;
use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Center, Fill, Font, Length};

use crate::Element;
use crate::format;
use crate::message::RepoMessage;
use crate::metrics;
use crate::state::{SEARCH_FIELD_ID, Search};
use crate::theme::Palette;

pub fn view<'a>(search: &'a Search, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let panel = container(
        column![
            box_row(search, palette),
            divider(palette),
            container(results(search, palette)).height(Fill),
        ]
        .height(Fill),
    )
    .width(Length::Fixed(680.0))
    .height(Length::Fixed(440.0))
    .style(move |_| container::Style {
        background: Some(palette.surface.into()),
        border: iced::Border {
            color: palette.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    });

    container(panel)
        .center(Fill)
        .style(move |_| container::Style {
            background: Some(
                iced::Color {
                    a: 0.75,
                    ..palette.background
                }
                .into(),
            ),
            ..container::Style::default()
        })
        .into()
}

fn box_row<'a>(search: &'a Search, palette: &'a Palette) -> Element<'a, RepoMessage> {
    // A count, not a spinner. On most repositories the walk finishes between
    // keystrokes, and a spinner that flashes on every letter is worse than
    // none — but a long search has to say it is still going.
    let status: Element<'_, RepoMessage> = if search.running {
        text("searching…")
            .size(metrics::text::LABEL)
            .color(palette.muted)
            .into()
    } else if search.query.trim().is_empty() {
        text("summary, message, author or hash")
            .size(metrics::text::LABEL)
            .color(palette.muted)
            .into()
    } else {
        let count = search.results.hits.len();
        text(if search.results.truncated {
            // "these are the first matches", never "these are the matches".
            format!("first {count} matches — narrow the search to see the rest")
        } else if count == 1 {
            "1 match".to_owned()
        } else {
            format!("{count} matches")
        })
        .size(metrics::text::LABEL)
        .color(if count == 0 {
            palette.muted
        } else {
            palette.success
        })
        .into()
    };

    container(
        column![
            text_input("Search commits", &search.query)
                .id(SEARCH_FIELD_ID)
                .on_input(RepoMessage::SearchChanged)
                .size(metrics::text::BODY)
                .padding([6, 8]),
            status,
        ]
        .spacing(5),
    )
    .padding([10, 12])
    .width(Fill)
    .into()
}

fn results<'a>(search: &'a Search, palette: &'a Palette) -> Element<'a, RepoMessage> {
    if search.results.hits.is_empty() {
        let message = if search.query.trim().is_empty() {
            "Type to search every commit in this repository."
        } else if search.running {
            "Searching…"
        } else {
            "Nothing matched."
        };
        return container(text(message).size(metrics::text::CODE).color(palette.muted))
            .center(Fill)
            .into();
    }

    let rows = column(
        search
            .results
            .hits
            .iter()
            .enumerate()
            .map(|(at, hit)| row_for(hit, at == search.selected, palette))
            .collect::<Vec<_>>(),
    )
    .spacing(1);

    scrollable(rows).height(Fill).into()
}

fn row_for<'a>(
    hit: &'a SearchHit,
    selected: bool,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let commit = &hit.commit;

    let body = row![
        text(commit.id.short(7))
            .size(metrics::text::LABEL)
            .font(Font::MONOSPACE)
            .color(palette.muted)
            .width(Length::Fixed(60.0)),
        text(commit.summary.clone())
            .size(metrics::text::CODE)
            .color(palette.text),
        Space::new().width(Fill),
        // Why this commit is in the list.
        text(hit.field.label())
            .size(metrics::text::MICRO)
            .color(palette.accent)
            .width(Length::Fixed(84.0)),
        text(format::truncate(&commit.author.name, 100.0))
            .size(metrics::text::LABEL)
            .color(palette.muted),
        text(format::relative_time(commit.time))
            .size(metrics::text::LABEL)
            .color(palette.muted)
            .width(Length::Fixed(56.0)),
    ]
    .spacing(10)
    .align_y(Center);

    button(container(body).padding([4, 12]))
        .width(Fill)
        .padding(0)
        .style(move |_, status| button::Style {
            background: Some(
                match (selected, status) {
                    (true, _) => palette.selection,
                    (false, button::Status::Hovered) => palette.selection_idle,
                    _ => iced::Color::TRANSPARENT,
                }
                .into(),
            ),
            text_color: palette.text,
            ..button::Style::default()
        })
        .on_press(RepoMessage::SearchAccepted(commit.id))
        .into()
}

fn divider<'a>(palette: &Palette) -> Element<'a, RepoMessage> {
    let border = palette.border;
    container(Space::new().height(Length::Fixed(1.0)))
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(border.into()),
            ..container::Style::default()
        })
        .into()
}
