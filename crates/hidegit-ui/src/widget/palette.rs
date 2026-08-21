//! The command palette.
//!
//! One box and a list: type a fragment, press Enter. It exists because a
//! shortcut you have not learnt yet and a menu you have to hunt through are the
//! two ways an action goes unused, and a palette is neither — you search for the
//! word you already have in mind.
//!
//! Commands are the same actions the rest of the interface dispatches, not a
//! second implementation of them. Each row carries the chord that also runs it,
//! where there is one, so using the palette teaches the shortcut rather than
//! competing with it.

use iced::widget::{Space, column, container, row, scrollable, text, text_input};
use iced::{Center, Fill, Font, Length};

use crate::Element;
use crate::message::{Message, RepoMessage};
use crate::state::{COMMAND_FIELD_ID, CommandPalette};
use crate::theme::Palette;
use crate::widget::common;

/// One entry.
///
/// `message` is a function rather than a value because the repository-scoped
/// commands need the active index, and because a command with nothing to act on
/// has to be able to say so — `None` keeps it out of the list rather than
/// offering a row that does nothing when pressed.
#[derive(Debug, Clone, Copy)]
pub struct Command {
    /// The name this command answers to in `config.toml`. Stable: renaming one
    /// silently unbinds whatever somebody had mapped to it.
    pub id: &'static str,
    pub title: &'static str,
    pub section: &'static str,
    /// The chord that also runs it, exactly as the shortcut reference spells
    /// it. Checked against that table by test, so the two cannot drift.
    pub chord: Option<&'static str>,
    pub message: fn(Option<usize>) -> Option<Message>,
}

/// The prompt `Cmd+Shift+O` raises, built in one place so the palette and the
/// binding cannot describe the clone dialog differently.
pub fn clone_prompt() -> Message {
    Message::PromptRequested(Box::new(crate::state::Prompt {
        kind: crate::state::PromptKind::Clone,
        title: "Clone a repository".to_owned(),
        confirm_label: "Choose a folder…".to_owned(),
        fields: vec![crate::state::PromptField::new(
            "URL",
            "https://github.com/owner/repo.git",
        )],
    }))
}

/// Turns a repository-scoped message into one, given somewhere to send it.
fn repo(active: Option<usize>, message: RepoMessage) -> Option<Message> {
    active.map(|index| Message::Repo(index, message))
}

pub const COMMANDS: &[Command] = &[
    Command {
        id: "open",
        title: "Open a repository",
        section: "Repositories",
        chord: Some("Cmd+O"),
        message: |_| Some(Message::OpenDialogRequested),
    },
    Command {
        id: "clone",
        title: "Clone a repository",
        section: "Repositories",
        chord: Some("Cmd+Shift+O"),
        message: |_| Some(clone_prompt()),
    },
    Command {
        id: "settings",
        title: "Settings",
        section: "Repositories",
        chord: Some("Cmd+,"),
        message: |_| Some(Message::SettingsRequested),
    },
    Command {
        id: "shortcuts",
        title: "Keyboard shortcuts",
        section: "Repositories",
        chord: Some("Cmd+/"),
        message: |_| Some(Message::ShortcutsRequested),
    },
    Command {
        id: "fetch",
        title: "Fetch every remote",
        section: "Remotes",
        chord: Some("Cmd+Shift+F"),
        message: |active| repo(active, RepoMessage::FetchRequested),
    },
    Command {
        id: "pull",
        title: "Pull",
        section: "Remotes",
        chord: Some("Cmd+Shift+P"),
        message: |active| repo(active, RepoMessage::PullRequested),
    },
    Command {
        id: "push",
        title: "Push",
        section: "Remotes",
        chord: Some("Cmd+Shift+U"),
        message: |active| {
            repo(
                active,
                RepoMessage::PushRequested {
                    force: hidegit_core::ops::ForceMode::None,
                },
            )
        },
    },
    Command {
        id: "commit",
        title: "Commit",
        section: "Working directory",
        chord: Some("Cmd+Enter"),
        message: |active| repo(active, RepoMessage::CommitRequested),
    },
    Command {
        id: "commit-and-push",
        title: "Commit and push",
        section: "Working directory",
        chord: Some("Cmd+Shift+Enter"),
        message: |active| repo(active, RepoMessage::CommitAndPushRequested),
    },
    Command {
        id: "discard",
        title: "Discard the selected changes",
        section: "Working directory",
        chord: Some("Cmd+Backspace"),
        message: |active| repo(active, RepoMessage::DiscardSelectedRequested),
    },
    Command {
        id: "search",
        title: "Search commits",
        section: "History",
        chord: Some("Cmd+F"),
        message: |active| repo(active, RepoMessage::SearchRequested),
    },
    Command {
        id: "diff-mode",
        title: "Toggle unified and side-by-side diff",
        section: "History",
        chord: Some("Cmd+D"),
        message: |active| repo(active, RepoMessage::DiffModeToggled),
    },
    Command {
        id: "continue",
        title: "Continue the operation in progress",
        section: "History",
        chord: Some("Cmd+Shift+."),
        message: |active| {
            repo(
                active,
                RepoMessage::SequenceControlRequested(hidegit_core::ops::SequenceControl::Continue),
            )
        },
    },
    Command {
        id: "refresh-pull-requests",
        title: "Refresh pull requests",
        section: "Pull requests",
        chord: None,
        message: |active| repo(active, RepoMessage::PrsRefreshRequested),
    },
    Command {
        id: "connect",
        title: "Connect to GitHub",
        section: "Pull requests",
        chord: None,
        message: |_| Some(Message::ConnectRequested),
    },
];

/// The commands a query matches, in table order.
///
/// Substring, case-insensitive, over the title. Not fuzzy: a fuzzy match that
/// puts "Discard the selected changes" under `push` because the letters appear
/// in order is worse than no match at all, and this list is fifteen rows, not
/// fifteen hundred.
pub fn matching(query: &str, active: Option<usize>) -> Vec<&'static Command> {
    let needle = query.trim().to_lowercase();

    COMMANDS
        .iter()
        .filter(|command| (command.message)(active).is_some())
        .filter(|command| needle.is_empty() || command.title.to_lowercase().contains(&needle))
        .collect()
}

pub fn view<'a>(
    state: &'a CommandPalette,
    active: Option<usize>,
    keymap: &'a crate::keymap::Keymap,
    palette: &'a Palette,
) -> Element<'a, Message> {
    let matches = matching(&state.query, active);

    let panel = container(
        column![
            container(
                text_input("Type a command…", &state.query)
                    .id(COMMAND_FIELD_ID)
                    .on_input(Message::PaletteQueryChanged)
                    .on_submit(Message::PaletteAccepted)
                    .size(14.0)
                    .padding(8)
            )
            .padding([8, 10]),
            common::divider(palette),
            container(results(&matches, state.selected, keymap, palette)).height(Fill),
        ]
        .height(Fill),
    )
    .width(Length::Fixed(620.0))
    .height(Length::Fixed(420.0))
    .style(move |_| container::Style {
        background: Some(palette.surface.into()),
        border: iced::Border {
            color: palette.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    });

    container(panel)
        .center(Fill)
        .style(move |_| container::Style {
            background: Some(crate::theme::scrim(palette).into()),
            ..container::Style::default()
        })
        .into()
}

fn results<'a>(
    matches: &[&'static Command],
    selected: usize,
    keymap: &'a crate::keymap::Keymap,
    palette: &'a Palette,
) -> Element<'a, Message> {
    if matches.is_empty() {
        return container(
            text("Nothing matches that.")
                .size(12.0)
                .color(palette.muted),
        )
        .padding(14)
        .into();
    }

    let mut list = column![].spacing(1).padding([6, 6]);
    let mut section = "";

    for (at, command) in matches.iter().enumerate() {
        if command.section != section {
            section = command.section;
            list = list.push(Space::new().height(6));
            list = list.push(
                container(text(section).size(10.0).color(palette.muted).font(Font {
                    weight: iced::font::Weight::Semibold,
                    ..Font::DEFAULT
                }))
                .padding([0, 8]),
            );
        }

        let chosen = at == selected;
        let mut line = row![text(command.title).size(13.0).color(palette.text)]
            .align_y(Center)
            .spacing(8);

        // The chord it answers to now, which is not its default if the file
        // moved it — or gave it one it never had.
        if let Some(chord) = keymap.chord_for(command) {
            line = line.push(Space::new().width(Fill));
            line = line.push(
                text(crate::widget::shortcuts::chord_label(chord))
                    .size(11.0)
                    .color(palette.muted)
                    .font(Font::MONOSPACE),
            );
        }

        list = list.push(container(line).width(Fill).padding([5, 8]).style(move |_| {
            container::Style {
                background: chosen.then(|| palette.selection.into()),
                border: iced::Border {
                    radius: 3.0.into(),
                    ..iced::Border::default()
                },
                ..container::Style::default()
            }
        }));
    }

    scrollable(list).height(Fill).into()
}
