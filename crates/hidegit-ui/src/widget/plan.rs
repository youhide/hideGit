//! The interactive rebase plan editor.
//!
//! A list of the commits a rebase would replay, in the order they will be
//! applied, each with what to do to it. Nothing here touches the repository:
//! the plan is built and reordered in full, and only then handed to
//! `git rebase --interactive` in one go. That is what makes the screen
//! abandonable — closing it costs nothing, because nothing has happened.
//!
//! **Oldest first**, unlike the graph. That is the order `git rebase
//! --interactive` writes its todo list in, and a plan editor that showed
//! newest-first would invert every reorder the user made — silently, since
//! both orders look plausible.

use hidegit_core::ops::RebaseAction;
use iced::widget::{Space, button, column, container, mouse_area, row, scrollable, text};
use iced::{Center, Fill, Font, Length};

use crate::Element;
use crate::message::{Message, RepoMessage};
use crate::state::RebaseEditor;
use crate::theme::Palette;

/// The verbs, in the order the todo list documents them.
///
/// Git's own words, not invented synonyms: someone who learns `fixup` here can
/// use it at a terminal, which is the whole argument for keeping the
/// vocabulary.
const ACTIONS: [(RebaseAction, &str, &str); 6] = [
    (RebaseAction::Pick, "pick", "Keep the commit as it is"),
    (
        RebaseAction::Reword,
        "reword",
        "Keep the changes, stop to change the message",
    ),
    (
        RebaseAction::Edit,
        "edit",
        "Stop after applying it, so it can be amended",
    ),
    (
        RebaseAction::Squash,
        "squash",
        "Fold into the commit above, keeping both messages",
    ),
    (
        RebaseAction::Fixup,
        "fixup",
        "Fold into the commit above, discarding this message",
    ),
    (RebaseAction::Drop, "drop", "Leave the commit out entirely"),
];

pub fn label(action: RebaseAction) -> &'static str {
    ACTIONS
        .iter()
        .find(|(a, _, _)| *a == action)
        .map(|(_, label, _)| *label)
        .unwrap_or("pick")
}

pub fn view<'a>(
    editor: &'a RebaseEditor,
    index: usize,
    palette: &'a Palette,
) -> Element<'a, Message> {
    let repo_message = move |m: RepoMessage| Message::Repo(index, m);

    let scrim = container(
        container(
            column![
                header(editor, palette),
                divider(palette),
                container(scrollable(steps(editor, palette)).height(Fill))
                    .height(Fill)
                    .padding([4, 0]),
                divider(palette),
                footer(editor, palette),
            ]
            .height(Fill),
        )
        .width(Length::Fixed(720.0))
        .height(Length::Fixed(520.0))
        .style(move |_| container::Style {
            background: Some(palette.surface.into()),
            border: iced::Border {
                color: palette.border,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        }),
    )
    .center(Fill)
    .style(move |_| container::Style {
        // A scrim, because the plan owns the screen until it is run or closed.
        background: Some(crate::theme::scrim(palette).into()),
        ..container::Style::default()
    });

    let inner: Element<'_, RepoMessage> = scrim.into();
    inner.map(repo_message)
}

fn header<'a>(editor: &'a RebaseEditor, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let kept = editor.kept();
    let total = editor.steps.len();

    container(
        column![
            text(format!("Rebase onto {}", editor.onto))
                .size(14.0)
                .color(palette.text)
                .font(Font {
                    weight: iced::font::Weight::Semibold,
                    ..Font::DEFAULT
                }),
            text(if kept == total {
                format!(
                    "{total} {} replayed, oldest first. Nothing runs until you start it.",
                    plural(total, "commit", "commits")
                )
            } else {
                format!(
                    "{kept} of {total} {} kept, oldest first. Nothing runs until you start it.",
                    plural(total, "commit", "commits")
                )
            })
            .size(11.0)
            .color(palette.muted),
        ]
        .spacing(3),
    )
    .padding([10, 12])
    .width(Fill)
    .into()
}

fn steps<'a>(editor: &'a RebaseEditor, palette: &'a Palette) -> Element<'a, RepoMessage> {
    if editor.steps.is_empty() {
        return container(
            text("There is nothing to replay onto that branch.")
                .size(12.0)
                .color(palette.muted),
        )
        .center(Fill)
        .padding(20)
        .into();
    }

    column(
        editor
            .steps
            .iter()
            .enumerate()
            .map(|(at, step)| {
                let selected = at == editor.selected;
                let dragged = editor.dragging == Some(at);
                let dropped = matches!(step.action, RebaseAction::Drop);

                // A dropped commit stays in the list, struck through by being
                // muted rather than removed: it has to be findable again to be
                // undropped, and a list that shrank as you worked would make
                // the plan hard to reason about.
                let summary = text(step.commit.summary.clone())
                    .size(12.0)
                    .color(if dropped { palette.muted } else { palette.text });

                let body = row![
                    text(format!("{}.", at + 1))
                        .size(11.0)
                        .font(Font::MONOSPACE)
                        .color(palette.muted)
                        .width(Length::Fixed(26.0)),
                    text(step.commit.id.short(7))
                        .size(11.0)
                        .font(Font::MONOSPACE)
                        .color(palette.muted),
                    summary,
                    Space::new().width(Fill),
                    actions(at, step.action, palette),
                ]
                .spacing(8)
                .align_y(Center);

                let row = button(container(body).padding([5, 10]))
                    .width(Fill)
                    .padding(0)
                    .style(move |_, status| row_style(dragged || selected, status, palette))
                    .on_press(RepoMessage::PlanRowSelected(at));

                // The gesture is spread across three rows' worth of events: the
                // press lands on one row, the crossings on others, and the
                // release wherever the pointer happens to be. Only the editor
                // sees all three, which is why it holds the drag rather than
                // any row doing so.
                mouse_area(row)
                    .on_press(RepoMessage::PlanRowDragStarted(at))
                    .on_enter(RepoMessage::PlanRowDraggedOver(at))
                    .on_release(RepoMessage::PlanRowDropped)
                    .into()
            })
            .collect::<Vec<_>>(),
    )
    .spacing(1)
    .into()
}

/// The six verbs, as a row of small toggles.
///
/// All six are shown rather than hidden behind a dropdown: they are the whole
/// vocabulary of an interactive rebase, and a menu would make discovering
/// `fixup` an act of exploration.
fn actions<'a>(at: usize, current: RebaseAction, palette: &'a Palette) -> Element<'a, RepoMessage> {
    row(ACTIONS
        .iter()
        .map(|(action, name, hint)| {
            let active = *action == current;
            let destructive = matches!(action, RebaseAction::Drop);

            hinted(
                button(text(*name).size(10.0).font(Font::MONOSPACE))
                    .padding([2, 6])
                    .style(move |_, status| verb_style(active, destructive, status, palette))
                    .on_press(RepoMessage::PlanActionChosen(at, *action))
                    .into(),
                hint,
                palette,
            )
        })
        .collect::<Vec<_>>())
    .spacing(3)
    .align_y(Center)
    .into()
}

/// A control with the sentence that explains it.
///
/// The difference between `squash` and `fixup` is exactly the kind of thing
/// nobody guesses from a six-letter verb, so every one of them carries its
/// explanation rather than assuming the vocabulary is already known.
fn hinted<'a>(
    control: Element<'a, RepoMessage>,
    label: &'a str,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let palette = *palette;
    iced::widget::tooltip(
        control,
        container(text(label).size(11.0).color(palette.text))
            .padding([4, 6])
            .style(move |_| container::Style {
                background: Some(palette.background.into()),
                border: iced::Border {
                    color: palette.border,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..container::Style::default()
            }),
        iced::widget::tooltip::Position::Top,
    )
    .into()
}

fn footer<'a>(editor: &'a RebaseEditor, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let can_move = !editor.steps.is_empty();
    let blocked = editor.blocked();

    let move_button = |label: &'a str, delta: i32, enabled: bool| {
        let mut b = button(text(label).size(11.0))
            .padding([4, 9])
            .style(move |_, status| verb_style(false, false, status, palette));
        if enabled {
            b = b.on_press(RepoMessage::PlanRowMoved(delta));
        }
        b
    };

    let mut start = button(text("Start rebase").size(11.0))
        .padding([5, 12])
        .style(move |_, status| accent_style(status, palette));
    if blocked.is_none() {
        start = start.on_press(RepoMessage::PlanStarted);
    }

    // A disabled Start says why. Two quite different things disable it, and a
    // greyed button that explains neither is a dead end.
    let reason: Element<'_, RepoMessage> = match blocked {
        Some(why) => text(why).size(11.0).color(palette.warning).into(),
        None => Space::new().width(0).into(),
    };

    container(
        row![
            move_button("↑ Move up", -1, can_move && editor.selected > 0),
            move_button(
                "↓ Move down",
                1,
                can_move && editor.selected + 1 < editor.steps.len(),
            ),
            Space::new().width(Length::Fixed(8.0)),
            reason,
            Space::new().width(Fill),
            button(text("Cancel").size(11.0))
                .padding([5, 12])
                .style(move |_, status| verb_style(false, false, status, palette))
                .on_press(RepoMessage::PlanDismissed),
            start,
        ]
        .spacing(6)
        .align_y(Center),
    )
    .padding([8, 12])
    .width(Fill)
    .into()
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
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

fn row_style(selected: bool, status: button::Status, palette: &Palette) -> button::Style {
    button::Style {
        background: Some(
            match (selected, status) {
                (true, _) => palette.border,
                (false, button::Status::Hovered) => iced::Color {
                    a: 0.5,
                    ..palette.border
                },
                _ => iced::Color::TRANSPARENT,
            }
            .into(),
        ),
        text_color: palette.text,
        border: iced::Border {
            radius: 3.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

fn verb_style(
    active: bool,
    destructive: bool,
    status: button::Status,
    palette: &Palette,
) -> button::Style {
    // The chosen verb is filled; `drop` fills in the danger colour, because
    // leaving a commit out is the one choice here that loses work.
    let fill = if destructive {
        palette.danger
    } else {
        palette.accent
    };

    button::Style {
        background: Some(
            match (active, status) {
                (true, _) => fill,
                (false, button::Status::Hovered) => palette.border,
                (false, _) => iced::Color::TRANSPARENT,
            }
            .into(),
        ),
        text_color: match (active, status) {
            (true, _) => palette.background,
            (false, button::Status::Disabled) => palette.muted,
            (false, _) => palette.muted,
        },
        border: iced::Border {
            color: palette.border,
            width: 1.0,
            radius: 3.0.into(),
        },
        ..button::Style::default()
    }
}

fn accent_style(status: button::Status, palette: &Palette) -> button::Style {
    button::Style {
        background: Some(
            match status {
                button::Status::Disabled => palette.border,
                _ => palette.accent,
            }
            .into(),
        ),
        text_color: match status {
            button::Status::Disabled => palette.muted,
            _ => palette.background,
        },
        border: iced::Border {
            radius: 3.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_a_verb_and_they_are_gits_own() {
        // Someone who learns `fixup` here can use it at a terminal, which is the
        // whole argument for not inventing synonyms.
        for (action, name, hint) in ACTIONS {
            assert_eq!(label(action), name);
            assert!(!hint.is_empty(), "{name} has no explanation");
        }
        assert_eq!(ACTIONS.len(), 6);
    }
}
