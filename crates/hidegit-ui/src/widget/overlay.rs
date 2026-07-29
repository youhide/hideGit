//! What sits on top of everything else: confirmations, action sheets, prompts
//! and toasts.
//!
//! M1 collected toasts in `App::toasts` and never drew them, so a failure to
//! open a repository was recorded and then silently discarded. All of these
//! layers live here because they are the same mechanism — an `iced::stack` over
//! the screen — and because the first destructive action needed two of them: one
//! to ask before acting, one to report when it fails.
//!
//! M3 added the other two. An **action sheet** is the list of things that can be
//! done to one item, and a **prompt** is a modal that collects text before
//! acting. Both are centred cards over the same scrim as the confirmation.

use iced::widget::{Space, button, column, container, row, stack, text, text_input};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::message::Message;
use crate::state::{ActionSheet, Confirmation, PROMPT_FIELD_IDS, Prompt, SheetItem, Toast};
use crate::theme::Palette;

/// Everything that can sit above the screen, in the order it stacks.
///
/// Grouped into one argument rather than four so adding a fifth layer does not
/// change every call site — `view` passes what the application state holds and
/// this file decides how they stack.
#[derive(Debug, Clone, Copy)]
pub struct Layers<'a> {
    pub confirming: Option<&'a Confirmation>,
    pub sheet: Option<&'a ActionSheet>,
    pub prompt: Option<&'a Prompt>,
    pub toasts: &'a [Toast],
}

/// Wraps `base` with whatever has to sit above it.
pub fn wrap<'a>(
    base: Element<'a, Message>,
    layers: Layers<'a>,
    palette: &'a Palette,
) -> Element<'a, Message> {
    let mut stacked = stack![base];

    if !layers.toasts.is_empty() {
        stacked = stacked.push(toast_layer(layers.toasts, palette));
    }
    // The modal layers go above the toasts: they own the keyboard, and a toast
    // floating over a question the user has to answer would be in the way.
    if let Some(sheet) = layers.sheet {
        stacked = stacked.push(action_sheet(sheet, palette));
    }
    if let Some(prompt) = layers.prompt {
        stacked = stacked.push(prompt_dialog(prompt, palette));
    }
    // A confirmation goes last of all: it is what a sheet's destructive item
    // raises, so it has to be able to sit over the sheet that raised it.
    if let Some(confirmation) = layers.confirming {
        stacked = stacked.push(dialog(confirmation, palette));
    }

    stacked.into()
}

/// The list of things that can be done to one item.
fn action_sheet<'a>(sheet: &'a ActionSheet, palette: &'a Palette) -> Element<'a, Message> {
    let palette = *palette;

    let mut body = column![
        // The title is the item, not a question: the user already knows what
        // they clicked, and repeating "what would you like to do?" wastes the
        // one line that could name it.
        text(sheet.title.as_str())
            .size(13.0)
            .color(palette.muted)
            .font(Font::MONOSPACE),
        Space::new().height(Length::Fixed(4.0)),
    ]
    .spacing(2);

    for item in &sheet.items {
        body = body.push(sheet_row(item, palette));
    }

    body = body.push(Space::new().height(Length::Fixed(6.0)));
    body = body.push(
        row![
            Space::new().width(Fill),
            button(
                container(text("Cancel").size(13.0).color(palette.text))
                    .padding(Padding::from([6, 14]))
            )
            .padding(0)
            .style(move |_, status| quiet_style(palette, status))
            .on_press(Message::SheetDismissed),
        ]
        .align_y(Center),
    );

    scrim(container(body), palette)
}

fn sheet_row<'a>(item: &'a SheetItem, palette: Palette) -> Element<'a, Message> {
    // A destructive action is distinguishable by colour *and* by a glyph, so it
    // reads as destructive without relying on hue.
    let (colour, marker) = if item.destructive {
        (palette.danger, "✕")
    } else {
        (palette.text, "›")
    };

    button(
        container(
            row![
                text(marker).size(12.0).color(colour).font(Font::MONOSPACE),
                text(item.label.as_str()).size(13.0).color(colour),
            ]
            .spacing(8)
            .align_y(Center),
        )
        .width(Fill)
        .padding(Padding::from([7, 10])),
    )
    .width(Fill)
    .padding(0)
    .style(move |_, status| quiet_style(palette, status))
    // Wrapped so the sheet closes as the action is dispatched. Every route out
    // of a sheet — an item, Cancel, `Esc` — has to leave the layer empty.
    .on_press(Message::SheetChosen(Box::new(item.message.clone())))
    .into()
}

/// A modal that collects text before acting.
fn prompt_dialog<'a>(prompt: &'a Prompt, palette: &'a Palette) -> Element<'a, Message> {
    let palette = *palette;

    let mut body = column![
        text(prompt.title.as_str())
            .size(15.0)
            .color(palette.text)
            .font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),
    ]
    .spacing(10);

    for (index, field) in prompt.fields.iter().enumerate() {
        let mut input = text_input(field.placeholder.as_str(), &field.value)
            .on_input(move |value| Message::PromptChanged(index, value))
            // `Enter` accepts, the way it does in every other one-line field.
            .on_submit(Message::PromptAccepted)
            .size(13.0)
            .padding(Padding::from([6, 8]));

        // Ids are what let `update` put the cursor in the first field when the
        // prompt opens. Focus is not observable in iced 0.14, but it is settable.
        if let Some(id) = PROMPT_FIELD_IDS.get(index) {
            input = input.id(*id);
        }

        body = body.push(
            column![
                text(field.label.as_str()).size(11.0).color(palette.muted),
                input,
            ]
            .spacing(4),
        );
    }

    // Why the button is unavailable is stated rather than left to be guessed,
    // the same way the commit composer does it.
    let mut confirm = button(
        container(
            text(prompt.confirm_label.as_str())
                .size(13.0)
                .color(iced::Color::WHITE),
        )
        .padding(Padding::from([6, 14])),
    )
    .padding(0)
    .style(move |_, status| accent_style(palette, status));
    if prompt.is_ready() {
        confirm = confirm.on_press(Message::PromptAccepted);
    }

    body = body.push(
        row![
            Space::new().width(Fill),
            button(
                container(text("Cancel").size(13.0).color(palette.text))
                    .padding(Padding::from([6, 14]))
            )
            .padding(0)
            .style(move |_, status| quiet_style(palette, status))
            .on_press(Message::PromptDismissed),
            confirm,
        ]
        .spacing(8)
        .align_y(Center),
    );

    scrim(container(body), palette)
}

/// A modal question, over a scrim that swallows clicks on what is behind it.
fn dialog<'a>(confirmation: &'a Confirmation, palette: &'a Palette) -> Element<'a, Message> {
    let palette = *palette;

    let body = column![
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
    .spacing(8);

    scrim(container(body), palette)
}

/// Centres a modal card over a scrim.
///
/// The scrim is a container filling the window, so a stray click lands on it
/// rather than on the list underneath. It does not dismiss: `Esc` and the Cancel
/// button do, and a click that both misses the card and destroys the user's typing
/// is not an improvement.
fn scrim<'a>(card: iced::widget::Container<'a, Message>, palette: Palette) -> Element<'a, Message> {
    let card = card
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

/// The primary button of a modal that is *not* destructive.
///
/// Distinct from [`danger_style`] on purpose: `UI_SPEC.md` requires destructive
/// actions to be distinguishable, which only works if creating a branch does not
/// wear the same red as discarding one.
fn accent_style(palette: Palette, status: button::Status) -> button::Style {
    let base = palette.accent;
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => iced::Color { a: 0.85, ..base },
        // A disabled primary says "not yet", not "gone": it stays in place,
        // dimmed, so the shape of the dialog does not shift as the user types.
        button::Status::Disabled => iced::Color { a: 0.35, ..base },
        button::Status::Active => base,
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
