//! The conflict resolver: ours, the result, and theirs.
//!
//! Laid out as `docs/UI_SPEC.md#conflict-resolver` describes it — two read-only
//! side panes around an editable middle one, an action bar naming which conflict
//! of how many is open, and a footer that can abort or continue.
//!
//! The rule that shapes everything here is **per conflict, not per file**. The
//! panes show one region at a time and the arrows move between regions, because
//! a file with nine conflicts is the case people abandon a GUI over and a
//! whole-file view makes it look like one enormous decision.

use hidegit_core::conflict::{ConflictRegion, Resolution};
use hidegit_core::model::RepoState;
use iced::widget::{Space, button, column, container, row, scrollable, text, text_editor};
use iced::{Center, Fill, Font, Length};

use crate::Element;
use crate::message::RepoMessage;
use crate::state::Resolver;
use crate::theme::Palette;

/// Monospaced, because every pane here is source code.
const CODE: Font = Font::MONOSPACE;
const CODE_SIZE: f32 = 12.0;

/// `conflicted_paths` is how many paths in the whole repository are still
/// conflicted. Continuing is per operation, not per file, so this file being
/// finished is not enough — the footer needs to know about the others.
pub fn view<'a>(
    resolver: &'a Resolver,
    state: RepoState,
    conflicted_paths: usize,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let Some(region) = resolver.focused_region() else {
        // A conflicted path with no markers in it: a binary file, or one
        // already resolved by hand. Neither is an error, and neither has
        // anything for the panes to show.
        return container(
            column![
                text("Nothing to resolve in this file")
                    .size(13.0)
                    .color(palette.text),
                text(
                    "It has no conflict markers — it may be binary, or already resolved by hand. \
                     Stage it to accept it as it is."
                )
                .size(11.0)
                .color(palette.muted),
            ]
            .spacing(6),
        )
        .center(Fill)
        .into();
    };

    let chosen = resolver
        .resolutions
        .get(resolver.focused)
        .cloned()
        .unwrap_or_default();

    column![
        header(resolver, palette),
        panes(resolver, region, &chosen, palette),
        action_bar(resolver, &chosen, palette),
        footer(resolver, state, conflicted_paths, palette),
    ]
    .height(Fill)
    .into()
}

/// The file being resolved, and how much of it is left.
fn header<'a>(resolver: &'a Resolver, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let remaining = resolver.remaining();
    let status = if remaining == 0 {
        text("all conflicts resolved")
            .size(11.0)
            .color(palette.success)
    } else {
        text(format!("{remaining} left"))
            .size(11.0)
            .color(palette.warning)
    };

    container(
        row![
            text(resolver.path.display().to_string())
                .size(12.0)
                .color(palette.text),
            Space::new().width(Fill),
            status,
        ]
        .align_y(Center)
        .spacing(8),
    )
    .padding([6, 10])
    .width(Fill)
    .into()
}

fn panes<'a>(
    resolver: &'a Resolver,
    region: &'a ConflictRegion,
    chosen: &Resolution,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    // Ours and theirs are labelled with what Git wrote on the marker — usually
    // `HEAD` and the branch being merged — rather than with the words "ours"
    // and "theirs", which are the two most confusable terms in Git.
    let ours = side(
        &region.ours_label,
        "OURS",
        &region.ours,
        palette.success,
        palette,
    );
    let theirs = side(
        &region.theirs_label,
        "THEIRS",
        &region.theirs,
        palette.warning,
        palette,
    );

    row![
        ours,
        vertical_rule(palette),
        result(resolver, region, chosen, palette),
        vertical_rule(palette),
        theirs,
    ]
    .height(Fill)
    .into()
}

/// One read-only side.
fn side<'a>(
    label: &str,
    which: &'a str,
    lines: &'a [String],
    tint: iced::Color,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    // The side is named in its own colour and nowhere else. Filling the pane
    // with the diff palette was tried and is wrong twice over: those colours
    // mean *one changed line* against an unchanged file, and at pane size they
    // drown the code they are supposed to frame.
    let heading = row![
        text(which).size(10.0).color(tint).font(Font {
            weight: iced::font::Weight::Semibold,
            ..Font::DEFAULT
        }),
        text(label.to_owned()).size(10.0).color(palette.muted),
    ]
    .spacing(6);

    let body: Element<'_, RepoMessage> = if lines.is_empty() {
        // A side with nothing in it means that side deleted the region, which
        // is an ordinary conflict and not a rendering failure.
        text("(deleted on this side)")
            .size(11.0)
            .color(palette.muted)
            .into()
    } else {
        column(lines.iter().map(|line| {
            text(line.trim_end_matches(['\r', '\n']).to_owned())
                .size(CODE_SIZE)
                .font(CODE)
                .color(palette.text)
                .into()
        }))
        .into()
    };

    container(
        column![
            container(heading).padding([4, 8]),
            container(scrollable(container(body).padding([0, 8])).height(Fill)).height(Fill),
        ]
        .height(Fill),
    )
    .width(Length::FillPortion(1))
    .height(Fill)
    .into()
}

/// The middle pane: what will be written, and where it can be typed.
fn result<'a>(
    resolver: &'a Resolver,
    region: &'a ConflictRegion,
    chosen: &Resolution,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let heading = container(
        row![
            text("RESULT").size(10.0).color(palette.muted).font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),
            Space::new().width(Fill),
            text(if resolver.editor.is_some() {
                "editing"
            } else {
                "read-only"
            })
            .size(10.0)
            .color(palette.muted),
        ]
        .align_y(Center)
        .spacing(6),
    )
    .padding([4, 8]);

    let body: Element<'_, RepoMessage> = match &resolver.editor {
        Some(content) => text_editor(content)
            .font(CODE)
            .size(CODE_SIZE)
            .height(Fill)
            .on_action(RepoMessage::ConflictEdited)
            .into(),
        None => {
            let lines = region.resolved_lines(chosen);
            if lines.is_empty() {
                let message = if chosen.is_resolved() {
                    // A resolved-to-nothing region is a deletion, which is a
                    // decision and not an absence.
                    "(this region is removed)"
                } else {
                    "Pick a side below, or edit this pane directly."
                };
                container(text(message).size(11.0).color(palette.muted))
                    .center(Fill)
                    .into()
            } else {
                scrollable(
                    container(column(lines.iter().map(|line| {
                        text(line.trim_end_matches(['\r', '\n']).to_owned())
                            .size(CODE_SIZE)
                            .font(CODE)
                            .color(palette.text)
                            .into()
                    })))
                    .padding([0, 8]),
                )
                .height(Fill)
                .into()
            }
        }
    };

    container(column![heading, body].height(Fill))
        .width(Length::FillPortion(1))
        .height(Fill)
        .into()
}

/// Which conflict is open, the presets, and the way between conflicts.
fn action_bar<'a>(
    resolver: &'a Resolver,
    chosen: &Resolution,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let at = resolver.focused;
    let count = resolver.conflict_count();

    let preset = |label: &'a str, resolution: Resolution| {
        let is_current = *chosen == resolution;
        button(text(label).size(11.0))
            .padding([4, 8])
            .style(move |_, status| preset_style(is_current, status, palette))
            .on_press(RepoMessage::ConflictResolved(at, resolution))
    };

    let editing = resolver.editor.is_some();

    container(
        row![
            text(format!("conflict {} of {count}", at + 1))
                .size(11.0)
                .color(palette.muted),
            Space::new().width(Length::Fixed(12.0)),
            preset("Take ours", Resolution::Ours),
            preset("Take theirs", Resolution::Theirs),
            preset("Take both", Resolution::Both),
            button(text(if editing { "Done editing" } else { "Edit" }).size(11.0))
                .padding([4, 8])
                .style(move |_, status| preset_style(editing, status, palette))
                .on_press(RepoMessage::ConflictEditToggled),
            Space::new().width(Fill),
            // Clamped rather than wrapping, so running out of Next is how you
            // learn you are on the last one.
            step_button("‹ prev", -1, at > 0, palette),
            step_button("next ›", 1, at + 1 < count, palette),
        ]
        .align_y(Center)
        .spacing(6),
    )
    .padding([6, 10])
    .width(Fill)
    .style(move |_| container::Style {
        background: Some(palette.surface.into()),
        ..container::Style::default()
    })
    .into()
}

fn step_button<'a>(
    label: &'a str,
    delta: i32,
    enabled: bool,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let mut b = button(text(label).size(11.0))
        .padding([4, 8])
        .style(move |_, status| preset_style(false, status, palette));
    if enabled {
        b = b.on_press(RepoMessage::ConflictStepped(delta));
    }
    b.into()
}

/// Abort, and the way forward once every conflict has a decision.
fn footer<'a>(
    resolver: &'a Resolver,
    state: RepoState,
    conflicted_paths: usize,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let verb = match state {
        RepoState::Merging => "merge",
        RepoState::Rebasing => "rebase",
        RepoState::CherryPicking => "cherry-pick",
        RepoState::Reverting => "revert",
        RepoState::Bisecting | RepoState::Clean => "operation",
    };

    let abort = button(text(format!("Abort {verb}")).size(11.0))
        .padding([5, 10])
        .style(move |_, status| danger_style(status, palette))
        .on_press(RepoMessage::SequenceControlRequested(
            hidegit_core::ops::SequenceControl::Abort,
        ));

    // Marking resolved is per file; continuing is per operation. They are
    // separate buttons because they are separate decisions, and a single
    // "Continue" that silently staged would hide the first one.
    let mut mark = button(text("Mark resolved").size(11.0))
        .padding([5, 10])
        .style(move |_, status| preset_style(false, status, palette));
    if resolver.is_resolved() {
        mark = mark.on_press(RepoMessage::ConflictMarkedResolved);
    }

    let mut carry_on = button(text(format!("Continue {verb}")).size(11.0))
        .padding([5, 10])
        .style(move |_, status| accent_style(status, palette));
    // Continue stays disabled until nothing anywhere is conflicted. This file
    // being done is not the same as the operation being ready to finish, and a
    // Continue that Git would refuse is worse than one that is visibly not yet
    // available.
    if conflicted_paths == 0 {
        carry_on = carry_on.on_press(RepoMessage::SequenceControlRequested(
            hidegit_core::ops::SequenceControl::Continue,
        ));
    }

    let remaining = resolver.remaining();
    let hint = if remaining > 0 {
        text(format!(
            "{remaining} {} in this file still {} a decision.",
            plural(remaining, "conflict", "conflicts"),
            if remaining == 1 { "needs" } else { "need" },
        ))
    } else if conflicted_paths > 1 {
        // Finished here, but Continue is still out of reach and saying why
        // beats a button that is disabled for no visible reason.
        text(format!(
            "Mark this file resolved. {} other {} still conflicted.",
            conflicted_paths - 1,
            plural(conflicted_paths - 1, "file is", "files are"),
        ))
    } else if conflicted_paths == 1 {
        text("Mark this file resolved, then continue.".to_owned())
    } else {
        text("Every conflict is resolved.".to_owned())
    }
    .size(11.0)
    .color(palette.muted);

    container(
        row![abort, hint, Space::new().width(Fill), mark, carry_on]
            .align_y(Center)
            .spacing(8),
    )
    .padding([6, 10])
    .width(Fill)
    .style(move |_| container::Style {
        background: Some(palette.surface.into()),
        ..container::Style::default()
    })
    .into()
}

/// Picks the singular or plural wording for `count`.
///
/// Spelled out rather than written as `conflict(s)`: the parenthesised plural
/// is the kind of thing that reads as unfinished, and every string in the UI is
/// something a person has to read.
fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

fn vertical_rule<'a>(palette: &Palette) -> Element<'a, RepoMessage> {
    let border = palette.border;
    container(Space::new().width(Length::Fixed(1.0)).height(Fill))
        .style(move |_| container::Style {
            background: Some(border.into()),
            ..container::Style::default()
        })
        .into()
}

fn preset_style(active: bool, status: button::Status, palette: &Palette) -> button::Style {
    let background = match (active, status) {
        (true, _) => palette.accent,
        (false, button::Status::Hovered) => palette.border,
        (false, _) => palette.surface,
    };
    button::Style {
        background: Some(background.into()),
        text_color: if active {
            palette.background
        } else if matches!(status, button::Status::Disabled) {
            palette.muted
        } else {
            palette.text
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

fn danger_style(status: button::Status, palette: &Palette) -> button::Style {
    button::Style {
        background: Some(
            match status {
                button::Status::Hovered => palette.danger,
                _ => palette.surface,
            }
            .into(),
        ),
        text_color: match status {
            button::Status::Hovered => palette.background,
            _ => palette.danger,
        },
        border: iced::Border {
            color: palette.danger,
            width: 1.0,
            radius: 3.0.into(),
        },
        ..button::Style::default()
    }
}
