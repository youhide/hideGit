//! What sits on top of everything else: confirmations and toasts.
//!
//! M1 collected toasts in `App::toasts` and never drew them, so a failure to
//! open a repository was recorded and then silently discarded. Both layers land
//! here together because they are the same mechanism — an `iced::stack` over
//! the screen — and because the first destructive action needs both: one to ask
//! before acting, one to report when it fails.

use iced::widget::{Space, button, column, container, row, stack, text};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::message::Message;
use crate::state::{Confirmation, Toast};
use crate::theme::Palette;

/// Wraps `base` with whatever has to sit above it.
pub fn wrap<'a>(
    base: Element<'a, Message>,
    confirming: Option<&'a Confirmation>,
    toasts: &'a [Toast],
    palette: &'a Palette,
) -> Element<'a, Message> {
    let mut layers = stack![base];

    if !toasts.is_empty() {
        layers = layers.push(toast_layer(toasts, palette));
    }
    // The dialog goes last so it is above the toasts: it is modal, and a toast
    // floating over a question the user has to answer would be in the way.
    if let Some(confirmation) = confirming {
        layers = layers.push(dialog(confirmation, palette));
    }

    layers.into()
}

/// A modal question, over a scrim that swallows clicks on what is behind it.
fn dialog<'a>(confirmation: &'a Confirmation, palette: &'a Palette) -> Element<'a, Message> {
    let palette = *palette;

    let card = container(
        column![
            text(confirmation.title.as_str())
                .size(15.0)
                .color(palette.text)
                .font(Font {
                    weight: iced::font::Weight::Semibold,
                    ..Font::DEFAULT
                }),
            text(confirmation.body.as_str())
                .size(13.0)
                .color(palette.muted),
            Space::new().height(Length::Fixed(6.0)),
            row![
                Space::new().width(Fill),
                // Cancel is first and unemphasised: the safe choice should not
                // be the one that takes aim.
                button(
                    container(text("Cancel").size(13.0).color(palette.text))
                        .padding(Padding::from([6, 14]))
                )
                .padding(0)
                .style(move |_, status| quiet_style(palette, status))
                .on_press(Message::ConfirmationDismissed),
                button(
                    container(
                        text(confirmation.confirm_label.as_str())
                            .size(13.0)
                            .color(iced::Color::WHITE)
                    )
                    .padding(Padding::from([6, 14]))
                )
                .padding(0)
                .style(move |_, status| danger_style(palette, status))
                .on_press(Message::ConfirmationAccepted),
            ]
            .spacing(8)
            .align_y(Center),
        ]
        .spacing(8),
    )
    .width(Length::Fixed(420.0))
    .padding(Padding::from([16, 18]))
    .style(move |_| container::Style {
        background: Some(palette.surface.into()),
        border: iced::Border {
            color: palette.border,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    });

    // The scrim is a container filling the window, so a stray click lands on it
    // rather than on the list underneath.
    container(card)
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill)
        .style(move |_| container::Style {
            background: Some(
                iced::Color {
                    a: 0.55,
                    ..iced::Color::BLACK
                }
                .into(),
            ),
            ..container::Style::default()
        })
        .into()
}

/// Failures, stacked in the bottom corner.
///
/// Each carries Git's own stderr rather than a paraphrase of it, because that
/// text is the most useful thing hideGit has to say when a command fails.
fn toast_layer<'a>(toasts: &'a [Toast], palette: &'a Palette) -> Element<'a, Message> {
    let palette = *palette;
    let mut stackable = column![].spacing(8);

    for toast in toasts {
        stackable = stackable.push(one_toast(toast, palette));
    }

    container(stackable)
        .width(Fill)
        .height(Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .align_y(iced::alignment::Vertical::Bottom)
        .padding(16)
        .into()
}

fn one_toast<'a>(toast: &'a Toast, palette: Palette) -> Element<'a, Message> {
    let mut body = column![
        row![
            text(toast.summary.as_str()).size(13.0).color(palette.text),
            Space::new().width(Fill),
            button(
                container(text("✕").size(12.0).color(palette.muted)).padding(Padding::from([0, 4]))
            )
            .padding(0)
            .style(move |_, status| quiet_style(palette, status))
            .on_press(Message::ToastDismissed(toast.id)),
        ]
        .spacing(12)
        .align_y(Center),
    ]
    .spacing(6);

    if !toast.details.is_empty() {
        body = body.push(
            text(toast.details.as_str())
                .size(11.0)
                .font(Font::MONOSPACE)
                .color(palette.muted),
        );
    }

    container(body)
        .width(Length::Fixed(420.0))
        .padding(Padding::from([10, 12]))
        .style(move |_| container::Style {
            background: Some(palette.surface.into()),
            border: iced::Border {
                color: palette.danger,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

fn quiet_style(palette: Palette, status: button::Status) -> button::Style {
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
            radius: 6.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

fn danger_style(palette: Palette, status: button::Status) -> button::Style {
    let base = palette.danger;
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => iced::Color { a: 0.85, ..base },
        _ => base,
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
