//! The settings panel.
//!
//! Everything here is also a key in `config.toml`, and the file stays the
//! source of truth: this screen edits it in place, keeping the comments and the
//! keys somebody wrote by hand. A setting reachable only by editing a file the
//! application never creates is a setting most people will never find — which
//! is what the light theme was until this existed.
//!
//! Changes apply as they are made. There is no OK button and nothing to
//! discard, because settings that only take effect on OK are settings people
//! are afraid to explore.

use iced::widget::{Space, button, checkbox, column, container, pick_list, row, scrollable, text};
use iced::{Center, Fill, Font, Length};

use crate::Element;
use crate::message::{AlertToggle, Message, QuietBound};
use crate::state::App;
use crate::theme::{Palette, Theme};

pub fn view<'a>(app: &'a App, palette: &'a Palette) -> Element<'a, Message> {
    let panel = container(
        column![
            header(palette),
            divider(palette),
            container(scrollable(body(app, palette)).height(Fill))
                .height(Fill)
                .padding([10, 16]),
            divider(palette),
            footer(app, palette),
        ]
        .height(Fill),
    )
    .width(Length::Fixed(560.0))
    .height(Length::Fixed(520.0))
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
    container(text("Settings").size(14.0).color(palette.text).font(Font {
        weight: iced::font::Weight::Semibold,
        ..Font::DEFAULT
    }))
    .padding([10, 16])
    .width(Fill)
    .into()
}

fn body<'a>(app: &'a App, palette: &'a Palette) -> Element<'a, Message> {
    let mut sections = column![section("Appearance", palette)].spacing(6);

    for (name, label) in [(Theme::DARK_NAME, "Dark"), (Theme::LIGHT_NAME, "Light")] {
        let chosen = app.theme.name == name;
        sections = sections.push(
            // A radio in behaviour if not in widget: exactly one theme is
            // active, and picking one is what unpicks the other.
            checkbox(chosen)
                .label(label)
                .size(15.0)
                .text_size(13.0)
                .on_toggle(move |_| Message::ThemeChosen(name.to_owned())),
        );
    }

    sections = sections.push(Space::new().height(14));
    sections = sections.push(section("Window", palette));
    sections = sections.push(
        checkbox(app.remember_geometry)
            .label("Reopen at the last size and position")
            .size(15.0)
            .text_size(13.0)
            .on_toggle(|_| Message::RememberGeometryToggled),
    );
    sections = sections.push(
        // Says what "no" means, which is otherwise a guess: the alternative is
        // not "wherever the window manager feels like", it is a fixed default.
        text("Off, hideGit opens at its default size, centred.")
            .size(11.0)
            .color(palette.muted),
    );

    sections = sections.push(Space::new().height(14));
    sections = sections.push(section("Pull request alerts", palette));

    let enabled = app.alerts.enabled;
    for which in AlertToggle::ALL {
        let on = which.get(&app.alerts);
        let master = which == AlertToggle::Enabled;

        // The per-event switches keep their own value while the master is off,
        // so turning notifications back on restores the set you chose rather
        // than a default: they go unavailable rather than being cleared.
        let live = master || enabled;
        let row = checkbox(on)
            .label(which.label())
            .size(15.0)
            .text_size(13.0)
            .on_toggle_maybe(live.then_some(move |_| Message::AlertToggled(which)));

        sections = sections.push(container(row).padding(if master { [0, 0] } else { [0, 14] }));

        if master {
            sections = sections.push(Space::new().height(6));
        }
    }

    sections = sections.push(Space::new().height(8));
    sections = sections.push(quiet_hours(app, palette));
    sections = sections.push(Space::new().height(14));
    sections = sections.push(section("Muted repositories", palette));
    sections = sections.push(muted(app, palette));

    sections.into()
}

/// The repositories that stay silent, and the ones that could.
///
/// Listed rather than typed into: the key is `owner/name` as the forge spells
/// it, and a name typed by hand that does not match silences nothing while
/// looking as though it does.
///
/// Shows every repository it can name — the open ones that have a forge remote,
/// plus anything already muted, which may well not be open. A muted entry that
/// vanished from the panel the moment you closed its tab would be a setting you
/// could not undo without editing the file.
fn muted<'a>(app: &'a App, palette: &'a Palette) -> Element<'a, Message> {
    let mut names: Vec<String> = app
        .repos
        .iter()
        .filter_map(|repo| repo.prs.repo.as_ref().map(ToString::to_string))
        .chain(app.alerts.muted.iter().cloned())
        .collect();
    names.sort();
    names.dedup();

    if names.is_empty() {
        return text(
            "Repositories appear here once one is open with a GitHub remote, \
             or is muted in config.toml.",
        )
        .size(11.0)
        .color(palette.muted)
        .into();
    }

    let live = app.alerts.enabled;
    let mut list = column![].spacing(2);

    for name in names {
        let on = app.alerts.muted.contains(&name);
        let key = name.clone();
        list = list.push(
            checkbox(on)
                .label(name)
                .size(15.0)
                .text_size(13.0)
                .on_toggle_maybe(
                    live.then_some(move |_| Message::RepositoryMuteToggled(key.clone())),
                ),
        );
    }

    list.into()
}

/// The window in which nothing is shown on the desktop.
///
/// Indented under the alerts it modifies, and unavailable when they are off:
/// a quiet window on an alert set that never fires is a setting with nothing to
/// act on, and offering it there suggests otherwise.
fn quiet_hours<'a>(app: &'a App, palette: &'a Palette) -> Element<'a, Message> {
    let quiet = &app.alerts.quiet_hours;
    let live = app.alerts.enabled;

    let switch = checkbox(quiet.enabled)
        .label("Quiet hours")
        .size(15.0)
        .text_size(13.0)
        .on_toggle_maybe(live.then_some(|_| Message::QuietHoursToggled));

    // Picked from a list rather than typed: an hour is one of twenty-four
    // values, and a text field would have to decide what "25" or "" means.
    let bounds = row![
        text("from").size(12.0).color(palette.muted),
        hour_picker(QuietBound::From, quiet.from, live && quiet.enabled, palette),
        text("to").size(12.0).color(palette.muted),
        hour_picker(QuietBound::To, quiet.to, live && quiet.enabled, palette),
    ]
    .spacing(8)
    .align_y(Center);

    let mut block = column![switch, container(bounds).padding([4, 22])].spacing(4);

    // Said once, where it applies. A window whose ends are equal covers nothing,
    // which is the only reading that does not silence either everything or
    // nothing depending on which comparison you write.
    if quiet.enabled && quiet.from == quiet.to {
        block = block.push(
            container(
                text("A window that starts and ends at the same hour silences nothing.")
                    .size(11.0)
                    .color(palette.warning),
            )
            .padding([0, 22]),
        );
    }

    block.into()
}

/// One end of the window, as a list of the twenty-four hours.
///
/// Rendered as plain text when it cannot be changed, rather than as a dropdown
/// that opens onto a choice it will not accept: the value still has to be
/// readable — it is what quiet hours would use once they are switched on.
fn hour_picker<'a>(
    bound: QuietBound,
    chosen: u8,
    live: bool,
    palette: &'a Palette,
) -> Element<'a, Message> {
    if !live {
        return text(Hour(chosen).to_string())
            .size(12.0)
            .color(palette.muted)
            .into();
    }

    let hours: Vec<Hour> = (0..24).map(Hour).collect();
    pick_list(hours, Some(Hour(chosen)), move |Hour(hour)| {
        Message::QuietHourChosen(bound, hour)
    })
    .text_size(12.0)
    .padding([3, 8])
    .into()
}

/// An hour of the day, shown the way a clock shows it.
///
/// A newtype purely for `Display`: `pick_list` renders with it, and a bare `u8`
/// would put "8" in a list where every other entry is two digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hour(u8);

impl std::fmt::Display for Hour {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:00", self.0)
    }
}

fn section<'a>(label: &'a str, palette: &'a Palette) -> Element<'a, Message> {
    text(label)
        .size(11.0)
        .color(palette.muted)
        .font(Font {
            weight: iced::font::Weight::Semibold,
            ..Font::DEFAULT
        })
        .into()
}

fn footer<'a>(app: &'a App, palette: &'a Palette) -> Element<'a, Message> {
    // The claim and its contradiction share a line, because they answer the
    // same question: did that toggle survive? Saying "saved" while it did not
    // is the whole bug — the switch flips either way, so the footer is the only
    // thing that can tell the difference.
    let (note, colour) = match &app.settings_error {
        None => (
            // Named so the file can be found and edited directly, which is
            // still the way to reach everything this screen does not cover.
            "Saved to config.toml as you change it.".to_owned(),
            palette.muted,
        ),
        Some(reason) => (format!("Not saved — {reason}"), palette.warning),
    };

    container(
        row![
            text(note).size(11.0).color(colour),
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
                .on_press(Message::SettingsDismissed),
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
