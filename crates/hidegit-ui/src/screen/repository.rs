//! The main window: toolbar, sidebar, graph and detail pane.

use hidegit_core::model::RepoState;
use hidegit_core::ops::SequenceControl;
use iced::widget::{Space, button, column, container, responsive, row, text, tooltip};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::message::{Message, RepoMessage};
use crate::state::{App, OpenRepo, Pane, ROW_HEIGHT};
use crate::theme::Palette;
use crate::widget::{detail, graph, sidebar};

/// The main window.
///
/// Emits top-level `Message` rather than `RepoMessage`, because the sidebar's row
/// actions raise application-level state — an action sheet, a prompt — as well as
/// addressing one repository. `index` is what lets a row wrap its own
/// `RepoMessage` up for later dispatch.
pub fn view<'a>(
    app: &'a App,
    repo: &'a OpenRepo,
    index: usize,
    palette: &'a Palette,
    cache: &'a iced::widget::canvas::Cache,
) -> Element<'a, Message> {
    let border = palette.border;
    let repo_message = move |m: RepoMessage| Message::Repo(index, m);

    let mut stack = column![toolbar(repo, palette).map(repo_message)];

    // A network operation in flight gets its own banner, above the one about
    // repository state: it is the thing that is happening right now.
    if let Some(pending) = &repo.pending {
        stack = stack.push(progress_banner(pending, palette).map(repo_message));
    }

    // A repository mid-operation says so, permanently and undismissably: the
    // repository genuinely is in that state, and hiding it is how people lose
    // work.
    if repo.state.is_in_progress() {
        stack = stack.push(
            operation_banner(repo.state, repo.status.conflicted.len(), palette).map(repo_message),
        );
    }

    stack = stack.push(divider(border).map(repo_message));
    // Which pane the keyboard is pointed at, said out loud. `Tab` cycles all
    // three; before this only the graph showed it, through the tint on its
    // selected row, so two of the three positions looked identical.
    let ring = |pane: Pane, element: Element<'a, Message>| -> Element<'a, Message> {
        let border = crate::theme::focus_ring(repo.focus == pane, palette);
        container(element)
            .style(move |_| container::Style {
                border,
                ..container::Style::default()
            })
            .into()
    };

    stack = stack.push(
        row![
            ring(Pane::Sidebar, sidebar::view(app, repo, index, palette)),
            vertical_rule(border).map(repo_message),
            column![
                // The portion goes on this container rather than inside
                // `graph_pane`, because `ring` wraps its child in a container
                // of its own and a container with no height is `Shrink` — a
                // `FillPortion` inside one never reaches the column that is
                // meant to divide the space. The detail below already had it
                // on the outside; this is the same shape, and the asymmetry is
                // what made the graph 20% of the window instead of 60%.
                container(ring(
                    Pane::Graph,
                    graph_pane(repo, palette, cache).map(repo_message)
                ))
                .height(Length::FillPortion(6))
                .width(Fill),
                divider(border).map(repo_message),
                container(ring(
                    Pane::Detail,
                    detail::view(repo, palette).map(repo_message)
                ))
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
            .height(Fill)
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
        .height(Fill)
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
            Space::new().width(Length::Fixed(16.0)),
            // Search has a button as well as `Cmd+F`. A shortcut is how you use
            // a thing you already know is there; a control is how you find out
            // it exists at all, and the toolbar is where the eye goes.
            action(
                "⌕ Search",
                "Search commits by summary, message, author or hash".to_owned(),
                Some(RepoMessage::SearchRequested),
                palette,
            ),
            remote_actions(repo, palette),
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

/// Fetch, Pull and Push — the operations reached constantly.
///
/// Absent entirely while something is in flight, because the banner underneath has
/// taken over saying what is happening and offering the only action that applies.
fn remote_actions<'a>(repo: &'a OpenRepo, palette: &'a Palette) -> Element<'a, RepoMessage> {
    if repo.pending.is_some() {
        return Space::new().width(0).into();
    }

    // No remote means nothing to fetch from or push to, and a button that cannot
    // work is worse than an absent one.
    let Some(remote) = repo.default_remote() else {
        return text("no remote").size(11.0).color(palette.muted).into();
    };

    // Every action that moves refs is unavailable mid-merge, and says so rather
    // than leaving it to be guessed.
    let blocked = (!repo.can_switch_branches())
        .then(|| format!("{} in progress", describe_state(repo.state)));

    let ahead = repo.head_ahead();
    let push_label = if ahead > 0 {
        format!("↑ Push {ahead}")
    } else {
        "↑ Push".to_owned()
    };

    let push_blocked = blocked.clone().or_else(|| match repo.push_target() {
        None => Some("Nothing to push from a detached HEAD".to_owned()),
        Some(_) => None,
    });

    row![
        action(
            "⟳ Fetch",
            blocked
                .clone()
                .unwrap_or_else(|| format!("Fetch every remote, pruning ({remote})")),
            blocked.is_none().then_some(RepoMessage::FetchRequested),
            palette,
        ),
        action(
            "↓ Pull",
            blocked
                .clone()
                .unwrap_or_else(|| "Fetch and integrate, the way your git config says".to_owned()),
            blocked.is_none().then_some(RepoMessage::PullRequested),
            palette,
        ),
        action(
            &push_label,
            push_blocked
                .clone()
                .unwrap_or_else(|| format!("Push to {remote}")),
            push_blocked
                .is_none()
                .then_some(RepoMessage::PushRequested {
                    force: hidegit_core::ops::ForceMode::None,
                }),
            palette,
        ),
    ]
    .spacing(4)
    .align_y(Center)
    .into()
}

/// A toolbar button, with the word behind it and the reason it is unavailable.
///
/// A disabled control keeps its place and carries *why* in its tooltip, which is
/// what `UI_SPEC.md` means by "disabled actions explain why they are disabled".
fn action<'a>(
    label: &str,
    hint: String,
    message: Option<RepoMessage>,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    let palette = *palette;
    let enabled = message.is_some();

    let mut control = button(
        container(text(label.to_owned()).size(12.0).color(if enabled {
            palette.text
        } else {
            palette.muted
        }))
        .padding(Padding::from([4, 9])),
    )
    .padding(0)
    .style(move |_, status| toolbar_style(palette, status));

    if let Some(message) = message {
        control = control.on_press(message);
    }

    tooltip(
        control,
        container(text(hint).size(11.0).color(palette.text))
            .padding(Padding::from([4, 6]))
            .style(move |_| container::Style {
                background: Some(palette.background.into()),
                border: iced::Border {
                    color: palette.border,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..container::Style::default()
            }),
        tooltip::Position::Bottom,
    )
    .into()
}

fn toolbar_style(palette: Palette, status: button::Status) -> button::Style {
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
            color: palette.border,
            width: 1.0,
            radius: 5.0.into(),
        },
        ..button::Style::default()
    }
}

/// What is happening, in a real unit, with the only action that applies.
///
/// A progress bar as well as the numbers: the bar is only drawn once a total is
/// known, because a bar that has to guess is exactly the indeterminate spinner
/// `UI_SPEC.md` rules out.
fn progress_banner<'a>(
    pending: &'a crate::state::Operation,
    palette: &'a Palette,
) -> Element<'a, RepoMessage> {
    let accent = palette.accent;
    let palette_copy = *palette;

    /// How wide the progress bar is, in logical pixels.
    const BAR: f32 = 120.0;

    let bar: Element<'a, RepoMessage> = match pending.fraction() {
        Some(fraction) => {
            // At least a sliver, so "0% and running" does not look like "nothing
            // is happening".
            let filled = (BAR * fraction).clamp(1.0, BAR);
            let track = palette.border;

            row![
                container(Space::new().height(3))
                    .width(Length::Fixed(filled))
                    .style(move |_| container::Style {
                        background: Some(accent.into()),
                        ..container::Style::default()
                    }),
                container(Space::new().height(3))
                    .width(Length::Fixed(BAR - filled))
                    .style(move |_| container::Style {
                        background: Some(track.into()),
                        ..container::Style::default()
                    }),
            ]
            .into()
        }
        None => Space::new().width(0).into(),
    };

    container(
        row![
            text(pending.label.as_str())
                .size(12.0)
                .color(accent)
                .font(Font {
                    weight: iced::font::Weight::Semibold,
                    ..Font::DEFAULT
                }),
            text(pending.detail()).size(12.0).color(palette.muted),
            bar,
            Space::new().width(Fill),
            button(
                container(text("Cancel").size(12.0).color(palette.text))
                    .padding(Padding::from([3, 10]))
            )
            .padding(0)
            .style(move |_, status| toolbar_style(palette_copy, status))
            .on_press(RepoMessage::OperationCancelled),
        ]
        .spacing(10)
        .align_y(Center),
    )
    .width(Fill)
    .padding(Padding::from([6, 14]))
    .style(move |_| container::Style {
        background: Some(iced::Color { a: 0.12, ..accent }.into()),
        ..container::Style::default()
    })
    .into()
}

/// The repository state in the words a person would use.
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

/// The persistent banner for a repository that is mid-operation.
///
/// `conflicted` is how many paths are still conflicted, which is what decides
/// whether continuing is offered: continuing is per operation, so it stays out
/// of reach until nothing anywhere is conflicted.
fn operation_banner<'a>(
    state: RepoState,
    conflicted: usize,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    let label = match state {
        RepoState::Merging => "Merging",
        RepoState::Rebasing => "Rebasing",
        RepoState::CherryPicking => "Cherry-picking",
        RepoState::Reverting => "Reverting",
        RepoState::Bisecting => "Bisecting",
        RepoState::Clean => return Space::new().height(0).into(),
    };

    let detail = if state == RepoState::Bisecting {
        // Bisect has its own vocabulary — `git bisect good`, not `--continue` —
        // and is not something hideGit drives. Saying so beats a button that
        // would send the wrong command.
        "Finish with `git bisect reset`.".to_owned()
    } else if conflicted > 0 {
        format!(
            "{conflicted} {} conflicted. Resolve {} to continue.",
            if conflicted == 1 {
                "file is"
            } else {
                "files are"
            },
            if conflicted == 1 { "it" } else { "them" },
        )
    } else {
        "Nothing is conflicted. Continue, or abort to go back.".to_owned()
    };

    let warning = palette.warning;
    let mut bar = row![
        text(label).size(12.0).color(warning).font(Font {
            weight: iced::font::Weight::Semibold,
            ..Font::DEFAULT
        }),
        text(detail).size(12.0).color(palette.muted),
    ]
    .spacing(10)
    .align_y(Center);

    // Bisect is the one state hideGit does not drive, so it gets no buttons.
    if state != RepoState::Bisecting {
        let muted = palette.muted;
        let text_colour = palette.text;
        let surface = palette.surface;
        let border = palette.border;
        let background = palette.background;

        let mut carry_on =
            button(text("Continue").size(11.0))
                .padding([3, 9])
                .style(move |_, status| button::Style {
                    background: Some(
                        match status {
                            button::Status::Disabled => surface,
                            _ => warning,
                        }
                        .into(),
                    ),
                    text_color: match status {
                        button::Status::Disabled => muted,
                        _ => background,
                    },
                    border: iced::Border {
                        radius: 3.0.into(),
                        ..iced::Border::default()
                    },
                    ..button::Style::default()
                });
        if conflicted == 0 {
            carry_on = carry_on.on_press(RepoMessage::SequenceControlRequested(
                SequenceControl::Continue,
            ));
        }

        bar = bar.push(Space::new().width(Fill));
        bar = bar.push(carry_on);
        bar = bar.push(
            button(text("Abort").size(11.0))
                .padding([3, 9])
                .style(move |_, _| button::Style {
                    background: Some(surface.into()),
                    text_color: text_colour,
                    border: iced::Border {
                        color: border,
                        width: 1.0,
                        radius: 3.0.into(),
                    },
                    ..button::Style::default()
                })
                .on_press(RepoMessage::SequenceControlRequested(
                    SequenceControl::Abort,
                )),
        );
    }

    container(bar)
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
