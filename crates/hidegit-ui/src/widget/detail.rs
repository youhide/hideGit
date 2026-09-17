//! The detail pane: commit metadata, message, changed files, and the diff.

use hidegit_core::model::{CommitDetail, Diff, FileChange, StashEntry};
use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::format;
use crate::message::{RepoMessage, UiError};
use crate::metrics;
use crate::state::{DetailPane, DiffMode, FILE_FILTER_ID, OpenRepo, Selection};
use crate::theme::Palette;
use crate::widget::common;

pub fn view<'a>(
    repo: &'a OpenRepo,
    palette: &'a Palette,
    text_catalogue: &'a crate::i18n::Catalogue,
) -> Element<'a, RepoMessage> {
    let body: Element<'a, RepoMessage> = match &repo.detail {
        DetailPane::Empty => common::empty("Select a commit to read its message and diff", palette),
        DetailPane::Loading => common::loading("Loading…", palette),
        DetailPane::Failed(error) => failure(error, palette),
        DetailPane::Commit { detail, diff, file } => {
            // A stash *is* a commit, which is what lets it reuse all of this — but
            // it is not part of history, and labelling it as an ordinary commit
            // would invite treating it like one.
            let stash = match repo.selection {
                Some(Selection::Stash(at)) => repo.stashes.get(at),
                _ => None,
            };
            commit(
                detail,
                diff,
                *file,
                &repo.file_filter,
                repo.diff_mode,
                stash,
                repo.blame.as_ref(),
                palette,
                text_catalogue,
            )
        }
        DetailPane::WorkingDirectory {
            staged,
            unstaged,
            selected,
            lines,
        } => crate::widget::staging::view(
            &repo.status,
            staged,
            unstaged,
            *selected,
            lines,
            repo.hunk,
            repo.diff_mode,
            &repo.draft,
            repo.state,
            repo.resolver.as_ref(),
            palette,
            text_catalogue,
            repo.head.target(),
        ),
        // The one pane whose contents did not come from the repository.
        DetailPane::PullRequest(detail) => crate::widget::pr::detail(detail, palette),
    };

    let surface = palette.surface;
    container(body)
        .width(Fill)
        .height(Fill)
        .style(move |_| container::Style {
            background: Some(surface.into()),
            ..container::Style::default()
        })
        .into()
}

/// A commit's metadata, message and diff.
///
/// `stash` is set when this commit is a stash entry rather than part of history, in
/// which case the heading says so — `git stash show` is exactly a commit against
/// its first parent, so everything below the heading is shared.
#[allow(clippy::too_many_arguments)]
fn commit<'a>(
    detail: &'a CommitDetail,
    diff: &'a Diff,
    selected_file: usize,
    filter: &'a str,
    mode: DiffMode,
    stash: Option<&'a StashEntry>,
    blame: Option<&'a crate::state::BlameView>,
    palette: &'a Palette,
    text_catalogue: &'a crate::i18n::Catalogue,
) -> Element<'a, RepoMessage> {
    let c = &detail.commit;

    let mut header = column![];

    if let Some(entry) = stash {
        // Named in Git's own vocabulary, because `stash@{0}` is what the user
        // would type at a terminal and what every stash command takes.
        let where_from = match &entry.branch {
            Some(branch) => format!("stash@{{{}}} · on {branch}", entry.index),
            None => format!("stash@{{{}}}", entry.index),
        };
        header = header.push(
            text(where_from)
                .size(metrics::text::LABEL)
                .font(Font::MONOSPACE)
                .color(palette.warning),
        );
    }

    // A stash carries the user's own message, which is what the sidebar shows and
    // what they will recognise; a commit's summary is its first line.
    let title = match stash {
        Some(entry) => entry.message.clone(),
        None => c.summary.clone(),
    };

    header = header.push(
        text(title)
            .size(metrics::text::LEAD)
            .color(palette.text)
            .font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),
    );
    let mut identity = row![
        text(c.id.short(10))
            .size(metrics::text::CODE)
            .font(Font::MONOSPACE)
            .color(palette.muted),
        text("·").size(metrics::text::CODE).color(palette.muted),
        text(c.author.name.clone())
            .size(metrics::text::CODE)
            .color(palette.muted),
        text("·").size(metrics::text::CODE).color(palette.muted),
        text(format::timestamp(c.time))
            .size(metrics::text::CODE)
            .color(palette.muted),
    ]
    .spacing(8)
    .align_y(Center);

    // A stash is a commit, but cherry-picking or resetting to one is not what
    // anybody means by acting on a stash — the stash rows carry apply, pop and
    // drop, and offering these here as well would be two vocabularies for the
    // same object.
    if stash.is_none() {
        let id = c.id;
        let muted = palette.muted;
        let surface = palette.surface;
        identity = identity.push(Space::new().width(Fill));
        identity = identity.push(
            button(text("⋯").size(metrics::text::CODE).color(muted))
                .padding([1, 6])
                .style(move |_, status| button::Style {
                    background: Some(
                        match status {
                            button::Status::Hovered => surface,
                            _ => iced::Color::TRANSPARENT,
                        }
                        .into(),
                    ),
                    text_color: muted,
                    border: iced::Border {
                        radius: 3.0.into(),
                        ..iced::Border::default()
                    },
                    ..button::Style::default()
                })
                .on_press(RepoMessage::CommitActionsRequested(id)),
        );
    }

    header = header.push(identity);
    header = header.spacing(6);

    // Author and committer differ after a rebase or an applied patch, and that
    // difference is worth showing rather than flattening.
    if c.author.name != c.committer.name || c.author.time != c.committer.time {
        header = header.push(
            text(format!(
                "committed by {} on {}",
                c.committer.name,
                format::timestamp(c.committer.time)
            ))
            .size(metrics::text::LABEL)
            .color(palette.muted),
        );
    }

    if let Some(body) = &c.body {
        header = header.push(Space::new().height(6));
        header = header.push(
            text(body.clone())
                .size(metrics::text::BODY)
                .font(Font::MONOSPACE)
                .color(palette.text),
        );
    }

    if !c.parents.is_empty() {
        header = header.push(Space::new().height(4));
        header = header.push(
            text(format!(
                "{}: {}",
                if c.is_merge() { "parents" } else { "parent" },
                c.parents
                    .iter()
                    .map(|p| p.short(8))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .size(metrics::text::LABEL)
            .font(Font::MONOSPACE)
            .color(palette.muted),
        );
    }

    // Filtered by path, keeping each row's own index: `FileSelected` addresses
    // the commit's list, not the one on screen, and a filtered index would open
    // whichever file happened to sit in that position.
    let needle = filter.trim().to_lowercase();
    let shown: Vec<(usize, &FileChange)> = detail
        .changes
        .iter()
        .enumerate()
        .filter(|(_, change)| {
            needle.is_empty()
                || change
                    .path
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&needle)
        })
        .collect();

    let mut files = column![].spacing(0);
    for (i, change) in shown.iter().copied() {
        let is_selected = i == selected_file;
        let palette_copy = *palette;

        let (glyph, colour) = match change.status {
            hidegit_core::model::ChangeStatus::Added => ("A", palette.success),
            hidegit_core::model::ChangeStatus::Deleted => ("D", palette.danger),
            hidegit_core::model::ChangeStatus::Renamed { .. } => ("R", palette.accent),
            hidegit_core::model::ChangeStatus::Copied { .. } => ("C", palette.accent),
            hidegit_core::model::ChangeStatus::TypeChange => ("T", palette.warning),
            hidegit_core::model::ChangeStatus::Modified => ("M", palette.warning),
        };

        let label = match &change.status {
            hidegit_core::model::ChangeStatus::Renamed { from }
            | hidegit_core::model::ChangeStatus::Copied { from } => {
                format!("{} → {}", from.display(), change.path.display())
            }
            _ => change.path.display().to_string(),
        };

        // A deleted file has no lines to blame at this revision, so the row does
        // not offer it rather than offering an action that would fail.
        let blamable = !matches!(change.status, hidegit_core::model::ChangeStatus::Deleted);
        let blame_target = change.path.clone();
        let blame_at = c.id;

        let row_button = button(
            container(
                row![
                    text(glyph)
                        .size(metrics::text::CODE)
                        .font(Font::MONOSPACE)
                        .color(colour),
                    text(label)
                        .size(metrics::text::CODE)
                        .font(Font::MONOSPACE)
                        .color(palette.text),
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding(Padding::from([3, 10])),
        )
        .width(Fill)
        .padding(0)
        .style(move |_, status| {
            let background = match (is_selected, status) {
                (true, _) => Some(palette_copy.selection.into()),
                (false, button::Status::Hovered) => Some(
                    iced::Color {
                        a: 0.07,
                        ..palette_copy.text
                    }
                    .into(),
                ),
                _ => None,
            };
            button::Style {
                background,
                text_color: palette_copy.text,
                ..button::Style::default()
            }
        })
        .on_press(RepoMessage::FileSelected(i));

        let mut entry = row![row_button].align_y(Center);
        if blamable {
            entry = entry.push(
                button(text("blame").size(metrics::text::MICRO))
                    .padding([2, 6])
                    .style(move |_, status| button::Style {
                        background: Some(
                            match status {
                                button::Status::Hovered => palette_copy.border,
                                _ => iced::Color::TRANSPARENT,
                            }
                            .into(),
                        ),
                        text_color: palette_copy.muted,
                        border: iced::Border {
                            radius: 3.0.into(),
                            ..iced::Border::default()
                        },
                        ..button::Style::default()
                    })
                    .on_press(RepoMessage::BlameRequested {
                        path: blame_target,
                        // Blamed at the commit being looked at, not at HEAD:
                        // the pane is showing that revision, and answering
                        // about a different one would be a quiet lie.
                        at: blame_at,
                    }),
            );
        }
        files = files.push(entry);
    }

    let border = palette.border;

    // Said rather than left as an empty column, which reads as a commit that
    // touched nothing.
    let listing: Element<'a, RepoMessage> = if shown.is_empty() {
        common::empty_offering(
            text_catalogue.get("empty.no-file-matches"),
            text_catalogue.get("empty.clear-filter"),
            RepoMessage::FileFilterChanged(String::new()),
            palette,
        )
    } else {
        scrollable(files).height(Fill).into()
    };

    let file_list = container(
        column![
            container(
                text_input("Filter files…", filter)
                    .id(FILE_FILTER_ID)
                    .on_input(RepoMessage::FileFilterChanged)
                    .size(metrics::text::LABEL)
                    .padding(Padding::from([4, 6]))
            )
            .padding(Padding::from([6, 8])),
            listing,
        ]
        .height(Fill),
    )
    .width(Length::Fixed(280.0))
    .height(Fill);

    // While filtering, the count says how much of the commit is being hidden.
    // "3 file(s)" over a commit that touched forty is a quiet lie.
    let counted = if needle.is_empty() {
        format!("{} file(s)", detail.stats.files_changed)
    } else {
        format!("{} of {} file(s)", shown.len(), detail.stats.files_changed)
    };

    let stats = text(format!(
        "{counted}   {}",
        format::diff_stat(detail.stats.insertions, detail.stats.deletions)
    ))
    .size(metrics::text::LABEL)
    .color(palette.muted);

    column![
        container(column![header, Space::new().height(6), stats].spacing(2))
            .width(Fill)
            .padding(Padding::from([10, 14])),
        common::divider(palette),
        row![
            file_list,
            container(Space::new().width(1))
                .height(Fill)
                .style(move |_| container::Style {
                    background: Some(border.into()),
                    ..container::Style::default()
                }),
            container(match blame {
                // Blame replaces the diff rather than sitting beside it: both
                // answer "what is in this file", and two answers at once in one
                // pane is how a screen stops being readable.
                Some(blame) => crate::widget::blame::view(blame, palette),
                None => crate::widget::diff::view(
                    diff,
                    selected_file,
                    mode,
                    palette,
                    // A commit's diff is history: there is nothing to stage in it.
                    None
                ),
            })
            .width(Fill)
            .height(Fill),
        ]
        .height(Fill),
    ]
    .into()
}

/// A failure shown where the action was attempted, with Git's own words rather
/// than a paraphrase.
fn failure<'a>(error: &UiError, palette: &Palette) -> Element<'a, RepoMessage> {
    container(
        column![
            text(error.summary.clone())
                .size(metrics::text::BODY)
                .color(palette.danger),
            text(error.details.clone())
                .size(metrics::text::LABEL)
                .font(Font::MONOSPACE)
                .color(palette.muted),
        ]
        .spacing(8)
        .max_width(600),
    )
    .width(Fill)
    .height(Fill)
    .center(Fill)
    .into()
}
