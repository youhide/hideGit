//! The detail pane: commit metadata, message, changed files, and the diff.

use hidegit_core::model::{CommitDetail, Diff};
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::format;
use crate::message::{RepoMessage, UiError};
use crate::state::{DetailPane, DiffMode, OpenRepo};
use crate::theme::Palette;

pub fn view<'a>(repo: &'a OpenRepo, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let body: Element<'a, RepoMessage> = match &repo.detail {
        DetailPane::Empty => placeholder("Select a commit to read its message and diff", palette),
        DetailPane::Loading => placeholder("Loading…", palette),
        DetailPane::Failed(error) => failure(error, palette),
        DetailPane::Commit { detail, diff, file } => {
            commit(detail, diff, *file, repo.diff_mode, palette)
        }
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

fn commit<'a>(
    detail: &'a CommitDetail,
    diff: &'a Diff,
    selected_file: usize,
    mode: DiffMode,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let c = &detail.commit;

    let mut header = column![
        text(c.summary.clone())
            .size(15.0)
            .color(palette.text)
            .font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),
        row![
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
        .align_y(Center),
    ]
    .spacing(6);

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
                    (true, _) => Some(
                        iced::Color {
                            a: 0.20,
                            ..palette_copy.accent
                        }
                        .into(),
                    ),
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
                palette
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
