//! The staging view: the working directory as four lists.
//!
//! Shown when the sidebar's working-directory row is selected. Staged, changed,
//! untracked and conflicted are separate sections rather than one list with a
//! column, because the action each offers is different and the same file can
//! appear in two of them at once.
//!
//! Every row carries its own actions: `+` to stage, `−` to unstage, `✕` to
//! discard. Discard always asks first, because it is the one with nothing
//! behind it — the change it destroys was never committed anywhere.

use iced::widget::{
    Space, button, column, container, mouse_area, row, scrollable, text, text_input, tooltip,
};
use iced::{Center, Fill, Font, Length, Padding};

use hidegit_core::model::{ChangeStatus, Conflict, Diff, FileChange, RepoState, WorktreeStatus};

use crate::Element;
use crate::message::RepoMessage;
use crate::state::{DiffMode, Draft, Section, StagingRow};
use crate::theme::Palette;
use crate::widget::diff;
use crate::widget::sidebar::{heading, item_style};

const ITEM_SIZE: f32 = 13.0;
const LIST_WIDTH: f32 = 280.0;

#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    status: &'a WorktreeStatus,
    staged: &'a Diff,
    unstaged: &'a Diff,
    selected: Option<StagingRow>,
    lines: &'a std::collections::BTreeSet<(usize, usize)>,
    focused_hunk: usize,
    mode: DiffMode,
    draft: &'a Draft,
    state: RepoState,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    // Even a clean tree gets the composer: amending the last commit is a real
    // thing to want, and it needs nothing staged.
    if status.is_clean() && !draft.amend {
        return column![
            container(clean(palette)).height(Fill),
            horizontal_rule(palette.border),
            container(composer(status, draft, state, palette)).width(Length::Fixed(LIST_WIDTH)),
        ]
        .into();
    }

    let mut list = column![].spacing(0);

    // Conflicts first: nothing else in the working directory matters until
    // they are resolved.
    if !status.conflicted.is_empty() {
        list = list.push(heading("CONFLICTED", status.conflicted.len(), palette));
        for (index, conflict) in status.conflicted.iter().enumerate() {
            list = list.push(conflict_row(conflict, index, selected, palette));
        }
    }
    if !status.staged.is_empty() {
        list = list.push(heading("STAGED", status.staged.len(), palette));
        for (index, change) in status.staged.iter().enumerate() {
            list = list.push(file_row(
                change,
                Section::Staged,
                index,
                selected,
                palette.success,
                palette,
            ));
        }
    }
    if !status.unstaged.is_empty() {
        list = list.push(heading("CHANGED", status.unstaged.len(), palette));
        for (index, change) in status.unstaged.iter().enumerate() {
            list = list.push(file_row(
                change,
                Section::Unstaged,
                index,
                selected,
                palette.warning,
                palette,
            ));
        }
    }
    if !status.untracked.is_empty() {
        list = list.push(heading("UNTRACKED", status.untracked.len(), palette));
        for (index, path) in status.untracked.iter().enumerate() {
            list = list.push(untracked_row(path, index, selected, palette));
        }
    }

    row![
        column![
            mouse_area(container(scrollable(list).height(Fill)).width(Length::Fixed(LIST_WIDTH)))
                .on_press(RepoMessage::EditingChanged(false)),
            horizontal_rule(palette.border),
            composer(status, draft, state, palette),
        ]
        .width(Length::Fixed(LIST_WIDTH)),
        vertical_rule(palette.border),
        mouse_area(
            container(pane(
                staged,
                unstaged,
                selected,
                lines,
                focused_hunk,
                mode,
                palette,
            ))
            .width(Fill),
        )
        .on_press(RepoMessage::EditingChanged(false)),
    ]
    .height(Fill)
    .into()
}

/// The diff for whichever row is open.
///
/// Which list the row came from decides which diff it is read from: the same
/// path in `staged` and in `unstaged` shows two different things.
#[allow(clippy::too_many_arguments)]
fn pane<'a>(
    staged: &'a Diff,
    unstaged: &'a Diff,
    selected: Option<StagingRow>,
    lines: &'a std::collections::BTreeSet<(usize, usize)>,
    focused_hunk: usize,
    mode: DiffMode,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let Some(row) = selected else {
        return placeholder("Select a file to see its changes", palette);
    };

    let staging = |is_staged: bool| {
        Some(diff::Staging {
            staged: is_staged,
            lines,
            focused_hunk,
        })
    };

    match row.section {
        Section::Staged => diff::view(staged, row.index, mode, palette, staging(true)),
        Section::Unstaged => diff::view(unstaged, row.index, mode, palette, staging(false)),
        // An untracked file has no diff to show: nothing in the repository has
        // ever seen it, so every line would be an addition against nothing.
        Section::Untracked => placeholder("Untracked — stage it to see it as a diff", palette),
        Section::Conflicted => placeholder(
            "Conflicted — resolving conflicts inside hideGit arrives in M5",
            palette,
        ),
    }
}

fn file_row<'a>(
    change: &FileChange,
    section: Section,
    index: usize,
    selected: Option<StagingRow>,
    accent: iced::Color,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    let row_id = StagingRow { section, index };
    let is_selected = selected == Some(row_id);
    let palette = *palette;

    // A rename's old path is gone from the working tree, so the path to act on
    // is always the new one.
    let target = vec![change.path.clone()];

    // The glyph carries the status independently of the colour, so the list
    // reads the same without hue.
    let glyph = text(change.status.code().to_string())
        .size(ITEM_SIZE)
        .font(Font::MONOSPACE)
        .color(accent);

    let label = match &change.status {
        ChangeStatus::Renamed { from } | ChangeStatus::Copied { from } => {
            format!("{} → {}", from.display(), change.path.display())
        }
        _ => change.path.display().to_string(),
    };

    let actions = match section {
        Section::Staged => row![action_button(
            "−",
            "Unstage",
            RepoMessage::UnstageRequested(target),
            palette,
        )],
        _ => row![
            action_button(
                "✕",
                "Discard",
                RepoMessage::DiscardRequested(target.clone()),
                palette,
            ),
            action_button("+", "Stage", RepoMessage::StageRequested(target), palette),
        ],
    };

    row![
        button(
            container(
                row![glyph, text(label).size(ITEM_SIZE).color(palette.text)]
                    .spacing(8)
                    .align_y(Center),
            )
            .padding(Padding::from([3, 12])),
        )
        .width(Fill)
        .padding(0)
        .style(move |_, status| item_style(palette, is_selected, status))
        .on_press(RepoMessage::StagingRowSelected(row_id)),
        actions,
    ]
    .align_y(Center)
    .into()
}

/// The stage/unstage control on a row.
///
/// A glyph, because the row is narrow — `+` takes a change toward a commit and
/// `−` takes it back out — with the word behind a tooltip so the meaning is
/// discoverable rather than guessed.
fn action_button<'a>(
    glyph: &'a str,
    label: &'a str,
    message: RepoMessage,
    palette: Palette,
) -> Element<'a, RepoMessage> {
    let control = button(
        container(text(glyph).size(ITEM_SIZE).font(Font::MONOSPACE)).padding(Padding::from([3, 8])),
    )
    .padding(0)
    .style(move |_, status| item_style(palette, false, status))
    .on_press(message);

    tooltip(
        control,
        container(text(label).size(11.0).color(palette.text))
            .padding(Padding::from([3, 6]))
            .style(move |_| container::Style {
                background: Some(palette.surface.into()),
                border: iced::Border {
                    color: palette.border,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..container::Style::default()
            }),
        tooltip::Position::Left,
    )
    .into()
}

fn untracked_row<'a>(
    path: &std::path::Path,
    index: usize,
    selected: Option<StagingRow>,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    let row_id = StagingRow {
        section: Section::Untracked,
        index,
    };
    let is_selected = selected == Some(row_id);
    let palette = *palette;
    let target = vec![path.to_path_buf()];

    row![
        button(
            container(
                row![
                    text("?")
                        .size(ITEM_SIZE)
                        .font(Font::MONOSPACE)
                        .color(palette.muted),
                    text(path.display().to_string())
                        .size(ITEM_SIZE)
                        .color(palette.muted),
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding(Padding::from([3, 12])),
        )
        .width(Fill)
        .padding(0)
        .style(move |_, status| item_style(palette, is_selected, status))
        .on_press(RepoMessage::StagingRowSelected(row_id)),
        // Discarding an untracked file deletes it outright, so it asks like
        // any other discard does.
        action_button(
            "✕",
            "Delete",
            RepoMessage::DiscardRequested(target.clone()),
            palette,
        ),
        action_button("+", "Stage", RepoMessage::StageRequested(target), palette),
    ]
    .align_y(Center)
    .into()
}

fn conflict_row<'a>(
    conflict: &Conflict,
    index: usize,
    selected: Option<StagingRow>,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    let row_id = StagingRow {
        section: Section::Conflicted,
        index,
    };
    let is_selected = selected == Some(row_id);
    let palette = *palette;

    button(
        container(
            row![
                text("!")
                    .size(ITEM_SIZE)
                    .font(Font::MONOSPACE)
                    .color(palette.danger),
                text(conflict.path.display().to_string())
                    .size(ITEM_SIZE)
                    .color(palette.text),
                Space::new().width(Fill),
                text(describe(conflict)).size(11.0).color(palette.muted),
            ]
            .spacing(8)
            .align_y(Center),
        )
        .padding(Padding::from([3, 12])),
    )
    .width(Fill)
    .padding(0)
    .style(move |_, status| item_style(palette, is_selected, status))
    .on_press(RepoMessage::StagingRowSelected(row_id))
    .into()
}

/// Git's own vocabulary for why a path is conflicted.
fn describe(conflict: &Conflict) -> &'static str {
    use hidegit_core::model::ConflictKind as K;

    match conflict.kind {
        K::BothModified => "both modified",
        K::BothAdded => "both added",
        K::BothDeleted => "both deleted",
        K::DeletedByUs => "deleted by us",
        K::DeletedByThem => "deleted by them",
        K::AddedByUs => "added by us",
        K::AddedByThem => "added by them",
    }
}

/// A clean working directory says what it means, and what to do next.
fn clean<'a>(palette: &Palette) -> Element<'a, RepoMessage> {
    let palette = *palette;
    container(
        column![
            text("Nothing to commit").size(15.0).color(palette.text),
            text("The working directory matches the last commit.")
                .size(13.0)
                .color(palette.muted),
        ]
        .spacing(6)
        .align_x(Center),
    )
    .center_x(Fill)
    .center_y(Fill)
    .into()
}

fn placeholder<'a>(message: &'a str, palette: &Palette) -> Element<'a, RepoMessage> {
    let muted = palette.muted;
    container(text(message).size(13.0).color(muted))
        .center_x(Fill)
        .center_y(Fill)
        .into()
}

fn vertical_rule<'a>(colour: iced::Color) -> Element<'a, RepoMessage> {
    container(Space::new())
        .width(Length::Fixed(1.0))
        .height(Fill)
        .style(move |_| container::Style {
            background: Some(colour.into()),
            ..container::Style::default()
        })
        .into()
}

/// The commit message editor, and the button that acts on it.
///
/// Sits under the file lists rather than in the diff pane, because it is about
/// the whole commit rather than about whichever file is open.
fn composer<'a>(
    status: &'a WorktreeStatus,
    draft: &'a Draft,
    state: RepoState,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let palette_copy = *palette;

    // `on_input` is the only focus signal iced 0.14 offers: focus lives inside
    // the widget and is neither observable nor settable from here. Wrapping the
    // field in a `mouse_area` to catch the click that grants focus does not
    // work — the `mouse_area` swallows it and the field never focuses at all.
    let subject = text_input("Summary", &draft.subject)
        .on_input(RepoMessage::SubjectChanged)
        // `Enter` in the subject commits, the way it does in every other
        // one-line message field.
        .on_submit(RepoMessage::CommitRequested)
        .size(13.0)
        .padding(Padding::from([6, 8]));

    let body = text_input("Description (optional)", &draft.body)
        .on_input(RepoMessage::BodyChanged)
        .size(13.0)
        .padding(Padding::from([6, 8]));

    // Why the button is unavailable is stated rather than left to be guessed.
    let blocker = if state.is_in_progress() {
        Some(format!("{} in progress", describe_state(state)))
    } else if !draft.is_ready() {
        Some("A summary is required".to_owned())
    } else if status.staged.is_empty() && !draft.amend {
        Some("Nothing staged".to_owned())
    } else {
        None
    };

    let label = if draft.amend {
        "Amend last commit"
    } else {
        "Commit"
    };
    let mut commit = button(
        container(text(label).size(13.0))
            .center_x(Fill)
            .padding(Padding::from([6, 12])),
    )
    .width(Fill)
    .padding(0)
    .style(move |_, s| commit_style(palette_copy, s));

    if blocker.is_none() {
        commit = commit.on_press(RepoMessage::CommitRequested);
    }

    let mut stack = column![
        subject,
        body,
        row![
            toggle("Amend", draft.amend, RepoMessage::AmendToggled, palette),
            toggle(
                "Sign off",
                draft.sign_off,
                RepoMessage::SignOffToggled,
                palette
            ),
        ]
        .spacing(14),
        commit,
    ]
    .spacing(8);

    if let Some(reason) = blocker {
        stack = stack.push(container(text(reason).size(11.0).color(palette.muted)).center_x(Fill));
    }

    // iced 0.14 keeps text-input focus inside the widget and does not report
    // it, so hideGit tracks it from the click that grants it. Clicking into
    // this area means the next keystroke is a character, not a shortcut;
    // clicking the lists or the diff means it is a shortcut again.
    container(stack)
        .padding(Padding::from([10, 12]))
        .width(Fill)
        .into()
}

fn describe_state(state: RepoState) -> &'static str {
    match state {
        RepoState::Clean => "Nothing",
        RepoState::Merging => "A merge",
        RepoState::Rebasing => "A rebase",
        RepoState::CherryPicking => "A cherry-pick",
        RepoState::Reverting => "A revert",
        RepoState::Bisecting => "A bisect",
    }
}

fn toggle<'a>(
    label: &'a str,
    on: bool,
    message: impl Fn(bool) -> RepoMessage + 'a,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    let palette = *palette;
    // A box glyph as well as a colour, so the state reads without hue.
    let glyph = if on { "☑" } else { "☐" };

    button(
        container(
            row![
                text(glyph)
                    .size(ITEM_SIZE)
                    .color(if on { palette.accent } else { palette.muted }),
                text(label).size(11.0).color(palette.muted),
            ]
            .spacing(5)
            .align_y(Center),
        )
        .padding(Padding::from([2, 4])),
    )
    .padding(0)
    .style(move |_, status| item_style(palette, false, status))
    .on_press(message(!on))
    .into()
}

fn commit_style(palette: Palette, status: button::Status) -> button::Style {
    let (background, text_color) = match status {
        button::Status::Disabled => (
            iced::Color {
                a: 0.25,
                ..palette.accent
            },
            palette.muted,
        ),
        button::Status::Hovered | button::Status::Pressed => (
            iced::Color {
                a: 0.85,
                ..palette.accent
            },
            iced::Color::WHITE,
        ),
        _ => (palette.accent, iced::Color::WHITE),
    };

    button::Style {
        background: Some(background.into()),
        text_color,
        border: iced::Border {
            radius: 6.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

fn horizontal_rule<'a>(colour: iced::Color) -> Element<'a, RepoMessage> {
    container(Space::new())
        .width(Fill)
        .height(Length::Fixed(1.0))
        .style(move |_| container::Style {
            background: Some(colour.into()),
            ..container::Style::default()
        })
        .into()
}
