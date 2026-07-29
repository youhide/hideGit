//! The staging view: the working directory as four lists.
//!
//! Shown when the sidebar's working-directory row is selected. Staged, changed,
//! untracked and conflicted are separate sections rather than one list with a
//! column, because the action each offers is different and the same file can
//! appear in two of them at once.
//!
//! Nothing here writes yet — the buttons arrive with `stage`, `unstage` and
//! `discard`. This is the surface they attach to.

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Center, Fill, Font, Length, Padding};

use hidegit_core::model::{ChangeStatus, Conflict, Diff, FileChange, WorktreeStatus};

use crate::Element;
use crate::message::RepoMessage;
use crate::state::{DiffMode, Section, StagingRow};
use crate::theme::Palette;
use crate::widget::diff;
use crate::widget::sidebar::{heading, item_style};

const ITEM_SIZE: f32 = 13.0;
const LIST_WIDTH: f32 = 280.0;

pub fn view<'a>(
    status: &'a WorktreeStatus,
    staged: &'a Diff,
    unstaged: &'a Diff,
    selected: Option<StagingRow>,
    mode: DiffMode,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    if status.is_clean() {
        return clean(palette);
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
        container(scrollable(list).height(Fill)).width(Length::Fixed(LIST_WIDTH)),
        vertical_rule(palette.border),
        container(pane(staged, unstaged, selected, mode, palette)).width(Fill),
    ]
    .height(Fill)
    .into()
}

/// The diff for whichever row is open.
///
/// Which list the row came from decides which diff it is read from: the same
/// path in `staged` and in `unstaged` shows two different things.
fn pane<'a>(
    staged: &'a Diff,
    unstaged: &'a Diff,
    selected: Option<StagingRow>,
    mode: DiffMode,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let Some(row) = selected else {
        return placeholder("Select a file to see its changes", palette);
    };

    match row.section {
        Section::Staged => diff::view(staged, row.index, mode, palette),
        Section::Unstaged => diff::view(unstaged, row.index, mode, palette),
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
    .on_press(RepoMessage::StagingRowSelected(row_id))
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
    .on_press(RepoMessage::StagingRowSelected(row_id))
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
