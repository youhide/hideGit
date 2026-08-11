//! The detail pane: commit metadata, message, changed files, and the diff.

use hidegit_core::model::{CommitDetail, Diff, StashEntry};
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::format;
use crate::message::{RepoMessage, UiError};
use crate::state::{DetailPane, DiffMode, OpenRepo, Selection};
use crate::theme::Palette;

pub fn view<'a>(repo: &'a OpenRepo, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let body: Element<'a, RepoMessage> = match &repo.detail {
        DetailPane::Empty => placeholder("Select a commit to read its message and diff", palette),
        DetailPane::Loading => placeholder("Loading…", palette),
        DetailPane::Failed(error) => failure(error, palette),
        DetailPane::Commit { detail, diff, file } => {
            // A stash *is* a commit, which is what lets it reuse all of this — but
            // it is not part of history, and labelling it as an ordinary commit
            // would invite treating it like one.
            let stash = match repo.selection {
                Some(Selection::Stash(at)) => repo.stashes.get(at),
                _ => None,
            };
            commit(detail, diff, *file, repo.diff_mode, stash, palette)
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
fn commit<'a>(
    detail: &'a CommitDetail,
    diff: &'a Diff,
    selected_file: usize,
    mode: DiffMode,
    stash: Option<&'a StashEntry>,
    palette: &'a Palette,
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
                .size(11.0)
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

    header = header.push(text(title).size(15.0).color(palette.text).font(Font {
        weight: iced::font::Weight::Semibold,
        ..Font::DEFAULT
    }));
    let mut identity = row![
        text(c.id.short(10))
            .size(12.0)
            .font(Font::MONOSPACE)
            .color(palette.muted),
        text("·").size(12.0).color(palette.muted),
        text(c.author.name.clone()).size(12.0).color(palette.muted),
        text("·").size(12.0).color(palette.muted),
        text(format::timestamp(c.time))
            .size(12.0)
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
            button(text("⋯").size(12.0).color(muted))
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
            .size(11.0)
            .color(palette.muted),
        );
    }

    if let Some(body) = &c.body {
        header = header.push(Space::new().height(6));
        header = header.push(
            text(body.clone())
                .size(13.0)
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
            .size(11.0)
            .font(Font::MONOSPACE)
            .color(palette.muted),
        );
    }

    let mut files = column![].spacing(0);
    for (i, change) in detail.changes.iter().enumerate() {
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

        files = files.push(
            button(
                container(
                    row![
                        text(glyph).size(12.0).font(Font::MONOSPACE).color(colour),
                        text(label)
                            .size(12.0)
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
            .on_press(RepoMessage::FileSelected(i)),
        );
    }

    let border = palette.border;
    let file_list = container(scrollable(files).height(Fill))
        .width(Length::Fixed(280.0))
        .height(Fill);

    let stats = text(format!(
        "{} file(s)   {}",
        detail.stats.files_changed,
        format::diff_stat(detail.stats.insertions, detail.stats.deletions)
    ))
    .size(11.0)
    .color(palette.muted);

    column![
        container(column![header, Space::new().height(6), stats].spacing(2))
            .width(Fill)
            .padding(Padding::from([10, 14])),
        divider(border),
        row![
            file_list,
            container(Space::new().width(1))
                .height(Fill)
                .style(move |_| container::Style {
                    background: Some(border.into()),
                    ..container::Style::default()
                }),
            container(crate::widget::diff::view(
                diff,
                selected_file,
                mode,
                palette,
                // A commit's diff is history: there is nothing to stage in it.
                None
            ))
            .width(Fill)
            .height(Fill),
        ]
        .height(Fill),
    ]
    .into()
}

fn divider<'a>(colour: iced::Color) -> Element<'a, RepoMessage> {
    container(Space::new().height(1))
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(colour.into()),
            ..container::Style::default()
        })
        .into()
}

fn placeholder<'a>(message: &str, palette: &Palette) -> Element<'a, RepoMessage> {
    container(text(message.to_owned()).size(13.0).color(palette.muted))
        .width(Fill)
        .height(Fill)
        .center(Fill)
        .into()
}

/// A failure shown where the action was attempted, with Git's own words rather
/// than a paraphrase.
fn failure<'a>(error: &UiError, palette: &Palette) -> Element<'a, RepoMessage> {
    container(
        column![
            text(error.summary.clone()).size(13.0).color(palette.danger),
            text(error.details.clone())
                .size(11.0)
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
