//! The welcome screen: open a repository, or reopen a recent one.
//!
//! Its empty state carries the next action rather than just reporting an
//! absence — "no repositories" with nothing to click is a dead end.

use std::path::Path;

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Center, Fill, Font, Padding};

use crate::Element;
use crate::message::Message;
use crate::state::{Prompt, PromptField, PromptKind};
use crate::theme::Palette;

pub fn view<'a>(recents: &'a [std::path::PathBuf], palette: &'a Palette) -> Element<'a, Message> {
    let palette_copy = *palette;

    let title = column![
        text("hideGit").size(28.0).color(palette.text).font(Font {
            weight: iced::font::Weight::Bold,
            ..Font::DEFAULT
        }),
        text("A Git client that tells you when your pull requests need you")
            .size(13.0)
            .color(palette.muted),
    ]
    .spacing(6)
    .align_x(Center);

    let open =
        button(container(text("Open a repository…").size(14.0)).padding(Padding::from([10, 20])))
            .style(move |_, status| primary_style(palette_copy, status))
            .on_press(Message::OpenDialogRequested);

    // Cloning is the other way to arrive at a repository, and it is secondary
    // because most sessions start from one that already exists on disk.
    let clone = button(container(text("Clone…").size(14.0)).padding(Padding::from([10, 20])))
        .style(move |_, status| secondary_style(palette_copy, status))
        .on_press(Message::PromptRequested(Box::new(Prompt {
            kind: PromptKind::Clone,
            title: "Clone a repository".to_owned(),
            confirm_label: "Choose a folder…".to_owned(),
            fields: vec![PromptField::new("URL", "https://github.com/owner/repo.git")],
        })));

    let mut body = column![
        title,
        Space::new().height(24),
        row![open, clone].spacing(10).align_y(Center),
    ]
    .spacing(0)
    .align_x(Center);

    if recents.is_empty() {
        body = body.push(Space::new().height(20));
        body = body.push(
            text("No repositories opened yet")
                .size(12.0)
                .color(palette.muted),
        );
    } else {
        body = body.push(Space::new().height(32));
        body = body.push(
            row![text("RECENT").size(11.0).color(palette.muted).font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),]
            .width(Fill),
        );
        body = body.push(Space::new().height(6));

        let mut list = column![].spacing(2);
        for path in recents {
            list = list.push(recent_row(path, palette_copy));
        }
        body = body.push(scrollable(list).height(iced::Length::Fixed(240.0)));
    }

    container(container(body).max_width(560))
        .width(Fill)
        .height(Fill)
        .center(Fill)
        .style(move |_| container::Style {
            background: Some(palette_copy.background.into()),
            ..container::Style::default()
        })
        .into()
}

fn recent_row<'a>(path: &Path, palette: Palette) -> Element<'a, Message> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let full = path.display().to_string();
    let target = path.to_path_buf();

    button(
        container(
            column![
                text(name).size(13.0).color(palette.text),
                text(full).size(11.0).color(palette.muted),
            ]
            .spacing(1),
        )
        .width(Fill)
        .padding(Padding::from([6, 10])),
    )
    .width(Fill)
    .padding(0)
    .style(move |_, status| match status {
        button::Status::Hovered => button::Style {
            background: Some(
                iced::Color {
                    a: 0.08,
                    ..palette.text
                }
                .into(),
            ),
            text_color: palette.text,
            ..button::Style::default()
        },
        _ => button::Style {
            background: None,
            text_color: palette.text,
            ..button::Style::default()
        },
    })
    .on_press(Message::OpenRepository(target))
    .into()
}

/// The secondary action next to the primary one: outlined rather than filled, so
/// there is one obvious thing to click and one alternative.
fn secondary_style(palette: Palette, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(
            iced::Color {
                a: 0.10,
                ..palette.text
            }
            .into(),
        ),
        _ => None,
    };

    button::Style {
        background,
        text_color: palette.text,
        border: iced::Border {
            color: palette.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..button::Style::default()
    }
}

fn primary_style(palette: Palette, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => iced::Color {
            a: 0.9,
            ..palette.accent
        },
        _ => palette.accent,
    };

    button::Style {
        background: Some(background.into()),
        text_color: iced::Color::WHITE,
        border: iced::Border {
            radius: 6.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}
