//! The keyboard shortcut reference.
//!
//! Shortcuts that are only in a document nobody opens are shortcuts nobody
//! uses. This is the same table as `docs/UI_SPEC.md#keyboard-shortcuts`, on
//! `Cmd+/`, next to the thing it describes.
//!
//! The table is data rather than prose because it is checked: a test parses
//! every chord here, feeds it to `shortcut()`, and asserts the two sets agree
//! in **both** directions — a row for a binding that does not exist fails, and
//! so does a binding added without a row. A reference that quietly drifts out
//! of date is worse than none, because it is believed.

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Center, Fill, Font, Length};

use crate::Element;
use crate::message::Message;
use crate::theme::Palette;

/// Where a binding applies.
///
/// Most act on the screen. A few belong to a panel that owns the keyboard while
/// it is up, and asking whether `↓` is bound has no single answer without this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Bound on the ordinary screen, with a repository open.
    Screen,
    /// Bound only while the search panel has the keyboard.
    Search,
    /// Bound only while the command palette has the keyboard.
    Command,
    /// Bound only while a panel — settings, or this one — is up.
    Panel,
}

#[derive(Debug, Clone, Copy)]
pub struct Binding {
    /// What the panel prints. Written with `Cmd`; rendered as `Ctrl` off macOS.
    pub shown: &'static str,
    /// The chords this row stands for, for the test that keeps it honest.
    /// Usually exactly one, and then it is `shown` verbatim.
    pub chords: &'static [&'static str],
    pub what: &'static str,
    pub context: Context,
}

impl Binding {
    const fn one(shown: &'static str, chords: &'static [&'static str], what: &'static str) -> Self {
        Self {
            shown,
            chords,
            what,
            context: Context::Screen,
        }
    }

    const fn in_context(mut self, context: Context) -> Self {
        self.context = context;
        self
    }
}

#[derive(Debug)]
pub struct Group {
    pub title: &'static str,
    pub bindings: &'static [Binding],
}

/// Every binding, grouped the way `UI_SPEC` groups them.
///
/// Bindings the spec lists as **not built** — the command palette, the `G`
/// chords — are deliberately absent. A reference that lists a key which does
/// nothing teaches people the application is broken.
pub const REFERENCE: &[Group] = &[
    Group {
        title: "Repositories",
        bindings: &[
            Binding::one("Cmd+O", &["Cmd+O"], "Open a repository"),
            Binding::one("Cmd+Shift+O", &["Cmd+Shift+O"], "Clone a repository"),
            Binding::one(
                "Cmd+1 … Cmd+9",
                &[
                    "Cmd+1", "Cmd+2", "Cmd+3", "Cmd+4", "Cmd+5", "Cmd+6", "Cmd+7", "Cmd+8", "Cmd+9",
                ],
                "Switch repository tab",
            ),
            Binding::one("Cmd+,", &["Cmd+,"], "Settings"),
            Binding::one("Cmd+/", &["Cmd+/"], "This list"),
            Binding::one("Cmd+P", &["Cmd+P"], "Command palette"),
        ],
    },
    Group {
        title: "Navigation",
        bindings: &[
            Binding::one("↑ / ↓", &["Up", "Down"], "Move the selection"),
            Binding::one("PageUp / PageDown", &["PageUp", "PageDown"], "Move by 20"),
            Binding::one(
                "Tab / Shift+Tab",
                &["Tab", "Shift+Tab"],
                "Cycle panes: sidebar → graph → detail",
            ),
            Binding::one("Cmd+F", &["Cmd+F"], "Search commits"),
        ],
    },
    Group {
        title: "Working directory",
        bindings: &[
            Binding::one("Space", &["Space"], "Stage or unstage the selected file"),
            Binding::one("Cmd+Enter", &["Cmd+Enter"], "Commit"),
            Binding::one("Cmd+Shift+Enter", &["Cmd+Shift+Enter"], "Commit and push"),
            Binding::one("Cmd+Backspace", &["Cmd+Backspace"], "Discard — always asks"),
        ],
    },
    Group {
        title: "Remotes",
        bindings: &[
            Binding::one(
                "Cmd+Shift+F",
                &["Cmd+Shift+F"],
                "Fetch every remote, pruning",
            ),
            Binding::one("Cmd+Shift+P", &["Cmd+Shift+P"], "Pull"),
            Binding::one("Cmd+Shift+U", &["Cmd+Shift+U"], "Push"),
        ],
    },
    Group {
        title: "Diff",
        bindings: &[
            Binding::one("J / K", &["J", "K"], "Next / previous hunk"),
            Binding::one("Cmd+D", &["Cmd+D"], "Unified ⇄ side-by-side"),
        ],
    },
    Group {
        title: "Conflicts",
        bindings: &[
            Binding::one(
                "Cmd+] / Cmd+[",
                &["Cmd+]", "Cmd+["],
                "Next / previous conflict",
            ),
            // Two chords for one key: with Shift held a US layout reports
            // `>`, and the binding accepts both rather than depending on the
            // keyboard.
            Binding::one(
                "Cmd+Shift+.",
                &["Cmd+Shift+.", "Cmd+Shift+>"],
                "Continue the operation",
            ),
        ],
    },
    Group {
        title: "While the search panel is up",
        bindings: &[
            Binding::one("↑ / ↓", &["Up", "Down"], "Move through the results")
                .in_context(Context::Search),
            Binding::one("Enter", &["Enter"], "Go to the selected commit")
                .in_context(Context::Search),
            Binding::one("Esc", &["Esc"], "Close the search").in_context(Context::Search),
        ],
    },
    Group {
        title: "While the command palette is up",
        bindings: &[
            Binding::one("↑ / ↓", &["Up", "Down"], "Move through the commands")
                .in_context(Context::Command),
            Binding::one("Enter", &["Enter"], "Run the selected command")
                .in_context(Context::Command),
            Binding::one("Esc", &["Esc"], "Close the palette").in_context(Context::Command),
        ],
    },
    Group {
        title: "While a panel is up",
        bindings: &[Binding::one("Esc", &["Esc"], "Close it").in_context(Context::Panel)],
    },
];

/// `Cmd` on macOS, `Ctrl` everywhere else — as the platform spells it, not as
/// the source does.
pub fn chord_label(shown: &str) -> String {
    if cfg!(target_os = "macos") {
        shown.to_owned()
    } else {
        shown.replace("Cmd", "Ctrl")
    }
}

pub fn view<'a>(palette: &'a Palette) -> Element<'a, Message> {
    let panel = container(
        column![
            header(palette),
            divider(palette),
            container(scrollable(body(palette)).height(Fill))
                .height(Fill)
                .padding([10, 16]),
            divider(palette),
            footer(palette),
        ]
        .height(Fill),
    )
    .width(Length::Fixed(560.0))
    .height(Length::Fixed(560.0))
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
            background: Some(
                iced::Color {
                    a: 0.75,
                    ..palette.background
                }
                .into(),
            ),
            ..container::Style::default()
        })
        .into()
}

fn header<'a>(palette: &'a Palette) -> Element<'a, Message> {
    container(
        text("Keyboard shortcuts")
            .size(14.0)
            .color(palette.text)
            .font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),
    )
    .padding([10, 16])
    .width(Fill)
    .into()
}

fn body<'a>(palette: &'a Palette) -> Element<'a, Message> {
    let mut sections = column![].spacing(4);

    for group in REFERENCE {
        sections = sections.push(Space::new().height(8));
        sections = sections.push(
            text(group.title)
                .size(11.0)
                .color(palette.muted)
                .font(Font {
                    weight: iced::font::Weight::Semibold,
                    ..Font::DEFAULT
                }),
        );

        for binding in group.bindings {
            sections = sections.push(
                row![
                    // Fixed rather than shrink-to-fit, so the descriptions line
                    // up down the panel instead of stepping in and out.
                    container(
                        text(chord_label(binding.shown))
                            .size(12.0)
                            .color(palette.accent)
                            .font(Font::MONOSPACE)
                    )
                    .width(Length::Fixed(170.0)),
                    text(binding.what).size(12.0).color(palette.text),
                ]
                .align_y(Center)
                .spacing(10),
            );
        }
    }

    sections.into()
}

fn footer<'a>(palette: &'a Palette) -> Element<'a, Message> {
    container(
        row![
            text("Remappable shortcuts are still to come.")
                .size(11.0)
                .color(palette.muted),
            Space::new().width(Fill),
            button(text("Done").size(11.0))
                .padding([5, 14])
                .style(move |_, status| button::Style {
                    background: Some(
                        match status {
                            button::Status::Hovered => palette.border,
                            _ => palette.surface,
                        }
                        .into(),
                    ),
                    text_color: palette.text,
                    border: iced::Border {
                        color: palette.border,
                        width: 1.0,
                        radius: 3.0.into(),
                    },
                    ..button::Style::default()
                })
                .on_press(Message::ShortcutsDismissed),
        ]
        .align_y(Center)
        .spacing(8),
    )
    .padding([8, 16])
    .width(Fill)
    .into()
}

fn divider<'a>(palette: &Palette) -> Element<'a, Message> {
    let border = palette.border;
    container(Space::new().height(Length::Fixed(1.0)))
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(border.into()),
            ..container::Style::default()
        })
        .into()
}
