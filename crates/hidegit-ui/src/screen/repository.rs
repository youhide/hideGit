//! The main window: toolbar, sidebar, graph and detail pane.

use hidegit_core::model::RepoState;
use iced::widget::{Space, column, container, responsive, row, text};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::message::{Message, RepoMessage};
use crate::state::{OpenRepo, Pane, ROW_HEIGHT};
use crate::theme::Palette;
use crate::widget::{detail, graph, sidebar};

/// The main window.
///
/// Emits top-level `Message` rather than `RepoMessage`, because the sidebar's row
/// actions raise application-level state — an action sheet, a prompt — as well as
/// addressing one repository. `index` is what lets a row wrap its own
/// `RepoMessage` up for later dispatch.
pub fn view<'a>(
    repo: &'a OpenRepo,
    index: usize,
    palette: &'a Palette,
    cache: &'a iced::widget::canvas::Cache,
) -> Element<'a, Message> {
    let border = palette.border;
    let repo_message = move |m: RepoMessage| Message::Repo(index, m);

    let mut stack = column![toolbar(repo, palette).map(repo_message)];

    // A repository mid-operation says so, permanently and undismissably: the
    // repository genuinely is in that state, and hiding it is how people lose
    // work.
    if repo.state.is_in_progress() {
        stack = stack.push(operation_banner(repo.state, palette).map(repo_message));
    }

    stack = stack.push(divider(border).map(repo_message));
    stack = stack.push(
        row![
            sidebar::view(repo, index, palette),
            vertical_rule(border).map(repo_message),
            column![
                graph_pane(repo, palette, cache).map(repo_message),
                divider(border).map(repo_message),
                container(detail::view(repo, palette).map(repo_message))
                    .height(Length::FillPortion(4))
                    .width(Fill),
            ]
            .height(Fill)
            .width(Fill),
        ]
        .height(Fill),
    );

    stack.into()
}

fn graph_pane<'a>(
    repo: &'a OpenRepo,
    palette: &'a Palette,
    cache: &'a iced::widget::canvas::Cache,
) -> Element<'a, RepoMessage> {
    if repo.graph.is_empty() {
        let message = match &repo.head {
            hidegit_core::model::Head::Unborn { name } => format!(
                "No commits yet. The first commit will create {}.",
                name.short
            ),
            _ => "Loading history…".to_owned(),
        };

        return container(text(message).size(13.0).color(palette.muted))
            .width(Fill)
            .height(Length::FillPortion(6))
            .center(Fill)
            .into();
    }

    let focused = repo.focus == Pane::Graph;
    let background = palette.background;

    // `responsive` is how the canvas learns its real height before any event
    // has been delivered — the viewport has to be measured, not assumed.
    let canvas = responsive(move |size| {
        let rows = (size.height / ROW_HEIGHT).floor().max(0.0) as usize;

        iced::widget::canvas(graph::GraphCanvas {
            view: &repo.graph,
            palette,
            selection: repo.selection.as_ref(),
            focused,
            viewport_rows: rows,
            cache,
        })
        .width(Fill)
        .height(Fill)
        .into()
    });

    container(canvas)
        .width(Fill)
        .height(Length::FillPortion(6))
        .style(move |_| container::Style {
            background: Some(background.into()),
            ..container::Style::default()
        })
        .into()
}

fn toolbar<'a>(repo: &'a OpenRepo, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let surface = palette.surface;

    let loaded = if repo.graph.loading_more {
        format!(
            "{} of {} commits",
            repo.graph.commits.len(),
            repo.graph.total
        )
    } else {
        format!("{} commits", repo.graph.commits.len())
    };

    container(
        row![
            text(repo.name()).size(13.0).color(palette.text).font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),
            text("·").size(13.0).color(palette.muted),
            text(repo.head_label()).size(13.0).color(palette.accent),
            Space::new().width(Fill),
            text(loaded).size(11.0).color(palette.muted),
        ]
        .spacing(10)
        .align_y(Center),
    )
    .width(Fill)
    .padding(Padding::from([8, 14]))
    .style(move |_| container::Style {
        background: Some(surface.into()),
        ..container::Style::default()
    })
    .into()
}

fn operation_banner<'a>(state: RepoState, palette: &Palette) -> Element<'a, RepoMessage> {
    let (label, detail) = match state {
        RepoState::Merging => ("Merging", "Finish or abort the merge with `git merge`."),
        RepoState::Rebasing => ("Rebasing", "Finish or abort the rebase with `git rebase`."),
        RepoState::CherryPicking => ("Cherry-picking", "Finish or abort with `git cherry-pick`."),
        RepoState::Reverting => ("Reverting", "Finish or abort with `git revert`."),
        RepoState::Bisecting => ("Bisecting", "Finish with `git bisect reset`."),
        RepoState::Clean => return Space::new().height(0).into(),
    };

    let warning = palette.warning;
    container(
        row![
            text(label).size(12.0).color(warning).font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),
            // hideGit cannot continue or abort until M5, and says so rather
            // than offering a button that does not work.
            text(detail).size(12.0).color(palette.muted),
        ]
        .spacing(10)
        .align_y(Center),
    )
    .width(Fill)
    .padding(Padding::from([6, 14]))
    .style(move |_| container::Style {
        background: Some(iced::Color { a: 0.15, ..warning }.into()),
        ..container::Style::default()
    })
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

fn vertical_rule<'a>(colour: iced::Color) -> Element<'a, RepoMessage> {
    container(Space::new().width(1))
        .height(Fill)
        .style(move |_| container::Style {
            background: Some(colour.into()),
            ..container::Style::default()
        })
        .into()
}
