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
use crate::metrics;
use crate::state::{COMPOSER_FIELD_IDS, DiffMode, Draft, Resolver, Section, StagingRow};
use crate::theme::Palette;
use crate::widget::common;
use crate::widget::diff;
use crate::widget::sidebar::{heading, item_style};

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
    resolver: Option<&'a Resolver>,
    palette: &'a Palette,
    text_catalogue: &'a crate::i18n::Catalogue,
    head: Option<hidegit_core::model::ObjectId>,
) -> Element<'a, RepoMessage> {
    // Even a clean tree gets the composer: amending the last commit is a real
    // thing to want, and it needs nothing staged.
    if status.is_clean() && !draft.amend {
        return column![
            container(clean(palette, text_catalogue, head)).height(Fill),
            common::divider(palette),
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
            common::divider(palette),
            composer(status, draft, state, palette),
        ]
        .width(Length::Fixed(LIST_WIDTH)),
        common::vertical_rule(palette),
        mouse_area(
            container(pane(
                staged,
                unstaged,
                selected,
                lines,
                focused_hunk,
                mode,
                state,
                resolver,
                status.conflicted.len(),
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
    state: RepoState,
    resolver: Option<&'a Resolver>,
    conflicted_paths: usize,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let Some(row) = selected else {
        return common::empty("Select a file to see its changes", palette);
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
        Section::Untracked => common::empty("Untracked — stage it to see it as a diff", palette),
        // The resolver loads its file asynchronously, so between selecting the
        // row and the file arriving there is genuinely nothing to show. Saying
        // so beats an empty pane that looks broken.
        Section::Conflicted => match resolver {
            Some(resolver) => {
                crate::widget::resolver::view(resolver, state, conflicted_paths, palette)
            }
            None => common::loading("Reading the conflicted file…", palette),
        },
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
        .size(metrics::text::BODY)
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
                row![
                    glyph,
                    text(label).size(metrics::text::BODY).color(palette.text)
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
        container(text(glyph).size(metrics::text::BODY).font(Font::MONOSPACE))
            .padding(Padding::from([3, 8])),
    )
    .padding(0)
    .style(move |_, status| item_style(palette, false, status))
    .on_press(message);

    tooltip(
        control,
        container(text(label).size(metrics::text::LABEL).color(palette.text))
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
                        .size(metrics::text::BODY)
                        .font(Font::MONOSPACE)
                        .color(palette.muted),
                    text(path.display().to_string())
                        .size(metrics::text::BODY)
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
                    .size(metrics::text::BODY)
                    .font(Font::MONOSPACE)
                    .color(palette.danger),
                text(conflict.path.display().to_string())
                    .size(metrics::text::BODY)
                    .color(palette.text),
                Space::new().width(Fill),
                text(describe(conflict))
                    .size(metrics::text::LABEL)
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

/// A clean working directory says what it means, and offers the commit it
/// matches.
///
/// The old wording — "the working directory matches the last commit" — was a
/// description standing in for an action nobody could take from here.
/// `UI_SPEC.md` asks an empty state to carry the next one, and the next one is
/// reading that commit.
///
/// An unborn branch has no commit to offer, so it keeps the sentence alone.
fn clean<'a>(
    palette: &Palette,
    text_catalogue: &'a crate::i18n::Catalogue,
    head: Option<hidegit_core::model::ObjectId>,
) -> Element<'a, RepoMessage> {
    let message = text_catalogue.get("empty.nothing-to-commit");

    match head {
        Some(id) => common::empty_offering(
            message,
            text_catalogue.get("empty.show-last-commit"),
            RepoMessage::Selected(crate::state::Selection::Commit(id)),
            palette,
        ),
        None => common::empty(message, palette),
    }
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
        // The id is what lets the `Space` binding find out this field has focus.
        // See `COMPOSER_FIELD_IDS`.
        .id(COMPOSER_FIELD_IDS[0])
        .on_input(RepoMessage::SubjectChanged)
        // `Enter` in the subject commits, the way it does in every other
        // one-line message field.
        .on_submit(RepoMessage::CommitRequested)
        .size(metrics::text::BODY)
        .padding(Padding::from([6, 8]));

    let body = text_input("Description (optional)", &draft.body)
        .id(COMPOSER_FIELD_IDS[1])
        .on_input(RepoMessage::BodyChanged)
        .size(metrics::text::BODY)
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
        container(text(label).size(metrics::text::BODY))
            .center_x(Fill)
            .padding(Padding::from([6, 12])),
    )
    .width(Fill)
    .padding(0)
    .style(move |_, s| common::button::primary(palette_copy, s));

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
        stack = stack.push(
            container(text(reason).size(metrics::text::LABEL).color(palette.muted)).center_x(Fill),
        );
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
                text(glyph).size(metrics::text::BODY).color(if on {
                    palette.accent
                } else {
                    palette.muted
                }),
                text(label).size(metrics::text::LABEL).color(palette.muted),
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
