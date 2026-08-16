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

use iced::widget::{Space, button, checkbox, column, container, row, scrollable, text};
use iced::{Center, Fill, Font, Length};

use crate::Element;
use crate::message::{AlertToggle, Message};
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
            sections = sections.push(
                text("Quiet hours and muted repositories are in config.toml.")
                    .size(11.0)
                    .color(palette.muted),
            );
            sections = sections.push(Space::new().height(6));
        }
    }

    sections.into()
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
