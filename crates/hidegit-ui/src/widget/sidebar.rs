//! The left sidebar: one tree, one mental model for "places I can jump to".
//!
//! Working directory, local branches, remotes, tags — and, from M3 and M4,
//! stashes and pull requests. Sections that belong to a later milestone are
//! absent rather than present-and-empty, because an empty "STASHES" heading
//! reads as "you have no stashes" and that would be a lie.

use hidegit_core::model::{Branch, Head, Tag};
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::message::RepoMessage;
use crate::state::{OpenRepo, Selection};
use crate::theme::Palette;

const HEADING_SIZE: f32 = 11.0;
const ITEM_SIZE: f32 = 13.0;

pub fn view<'a>(repo: &'a OpenRepo, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let mut sections = column![].spacing(2).padding(Padding::from([8, 0]));

    sections = sections.push(working_directory(repo, palette));
    sections = sections.push(Space::new().height(8));

    sections = sections.push(heading("LOCAL", repo.refs.locals.len(), palette));
    for branch in &repo.refs.locals {
        sections = sections.push(branch_row(branch, &repo.head, palette));
    }

    if !repo.refs.remotes.is_empty() {
        sections = sections.push(Space::new().height(8));
        sections = sections.push(heading("REMOTES", repo.refs.remotes.len(), palette));
        for branch in &repo.refs.remotes {
            sections = sections.push(branch_row(branch, &repo.head, palette));
        }
    }

    if !repo.refs.tags.is_empty() {
        sections = sections.push(Space::new().height(8));
        sections = sections.push(heading("TAGS", repo.refs.tags.len(), palette));
        for tag in &repo.refs.tags {
            sections = sections.push(tag_row(tag, palette));
        }
    }

    let palette = *palette;
    container(scrollable(sections).height(Fill))
        .width(Length::Fixed(230.0))
        .height(Fill)
        .style(move |_| container::Style {
            background: Some(palette.surface.into()),
            ..container::Style::default()
        })
        .into()
}

pub(crate) fn heading<'a>(
    label: &'a str,
    count: usize,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    let muted = palette.muted;
    let count = if count > 0 {
        text(count.to_string()).size(HEADING_SIZE).color(muted)
    } else {
        text("").size(HEADING_SIZE)
    };

    container(
        row![
            text(label).size(HEADING_SIZE).color(muted).font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),
            Space::new().width(Fill),
            count,
        ]
        .align_y(Center),
    )
    .padding(Padding::from([4, 12]))
    .into()
}

fn working_directory<'a>(repo: &OpenRepo, palette: &Palette) -> Element<'a, RepoMessage> {
    let selected = matches!(repo.selection, Some(Selection::WorkingDirectory));
    let palette = *palette;

    // The badge counts every entry across all four lists, so a file both
    // staged and edited again counts twice — the same way `git status` lists
    // it under two headings.
    let count = repo.status.change_count();
    let badge = if count > 0 {
        text(count.to_string())
            .size(HEADING_SIZE)
            .color(palette.accent)
    } else {
        text("").size(HEADING_SIZE)
    };

    let label = row![
        text("WORKING DIRECTORY")
            .size(HEADING_SIZE)
            .color(palette.muted),
        Space::new().width(Fill),
        badge,
    ]
    .align_y(Center);

    button(container(label).padding(Padding::from([4, 12])))
        .width(Fill)
        .padding(0)
        .style(move |_, status| item_style(palette, selected, status))
        .on_press(RepoMessage::Selected(Selection::WorkingDirectory))
        .into()
}

fn branch_row<'a>(branch: &Branch, head: &Head, palette: &Palette) -> Element<'a, RepoMessage> {
    let is_head = matches!(head, Head::Branch { name, .. } if name.full == branch.name.full);
    let palette = *palette;

    let name = text(branch.name.short.clone())
        .size(ITEM_SIZE)
        .color(if is_head { palette.text } else { palette.muted })
        .font(if is_head {
            Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }
        } else {
            Font::DEFAULT
        });

    // The current branch is marked with a glyph as well as with weight and
    // colour, so it is identifiable without relying on either.
    let marker = text(if is_head { "▸" } else { " " })
        .size(ITEM_SIZE)
        .color(palette.accent);

    let selected = false;
    button(container(row![marker, name].spacing(6).align_y(Center)).padding(Padding::from([3, 12])))
        .width(Fill)
        .padding(0)
        .style(move |_, status| item_style(palette, selected, status))
        .on_press(RepoMessage::Selected(Selection::Commit(branch.target)))
        .into()
}

fn tag_row<'a>(tag: &Tag, palette: &Palette) -> Element<'a, RepoMessage> {
    let palette = *palette;
    // Annotated and lightweight tags look different, because they are.
    let glyph = if tag.annotated { "◆" } else { "◇" };

    button(
        container(
            row![
                text(glyph).size(ITEM_SIZE).color(palette.warning),
                text(tag.name.short.clone())
                    .size(ITEM_SIZE)
                    .color(palette.muted),
            ]
            .spacing(6)
            .align_y(Center),
        )
        .padding(Padding::from([3, 12])),
    )
    .width(Fill)
    .padding(0)
    .style(move |_, status| item_style(palette, false, status))
    .on_press(RepoMessage::Selected(Selection::Commit(tag.target)))
    .into()
}

pub(crate) fn item_style(
    palette: Palette,
    selected: bool,
    status: button::Status,
) -> button::Style {
    let background = match (selected, status) {
        (true, _) => Some(
            iced::Color {
                a: 0.22,
                ..palette.accent
            }
            .into(),
        ),
        (false, button::Status::Hovered) => Some(
            iced::Color {
                a: 0.08,
                ..palette.text
            }
            .into(),
        ),
        _ => None,
    };

    button::Style {
        background,
        text_color: palette.text,
        ..button::Style::default()
    }
}
