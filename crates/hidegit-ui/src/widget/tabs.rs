//! The repository tab bar.
//!
//! Absent entirely with one repository open. A tab bar showing a single tab is
//! chrome that tells you nothing — it costs a row of screen the graph could
//! have had, to say "you have one thing open", which you can see.
//!
//! Each tab carries the branch it is on as well as its name, because the reason
//! to keep two repositories open is usually that they are at different points
//! in different work, and the branch is what tells them apart at a glance.

use iced::widget::{Space, button, container, row, text};
use iced::{Center, Fill, Length};

use crate::Element;
use crate::format;
use crate::message::Message;
use crate::state::OpenRepo;
use crate::theme::Palette;

/// Wide enough for a repository name and a branch without either being cut to
/// nothing; narrow enough that eight tabs still fit a laptop screen.
const TAB_WIDTH: f32 = 190.0;

pub fn view<'a>(
    repos: &'a [OpenRepo],
    active: Option<usize>,
    palette: &'a Palette,
) -> Option<Element<'a, Message>> {
    // One repository needs no tabs to choose between.
    if repos.len() < 2 {
        return None;
    }

    let tabs = row(repos
        .iter()
        .enumerate()
        .map(|(index, repo)| tab(repo, index, active == Some(index), palette))
        .collect::<Vec<_>>())
    .spacing(1)
    .align_y(Center);

    Some(
        container(row![tabs, Space::new().width(Fill)].align_y(Center))
            .width(Fill)
            .style(move |_| container::Style {
                background: Some(palette.background.into()),
                ..container::Style::default()
            })
            .into(),
    )
}

fn tab<'a>(
    repo: &'a OpenRepo,
    index: usize,
    active: bool,
    palette: &'a Palette,
) -> Element<'a, Message> {
    // The name and the branch, because two checkouts of the same repository are
    // exactly the case tabs exist for and the name alone would not tell them
    // apart.
    let label = container(
        iced::widget::column![
            text(format::truncate(&repo.name(), TAB_WIDTH - 46.0))
                .size(12.0)
                .color(if active { palette.text } else { palette.muted }),
            text(format::truncate(&repo.head_label(), TAB_WIDTH - 46.0))
                .size(10.0)
                .color(palette.muted),
        ]
        .spacing(1),
    )
    .width(Length::Fixed(TAB_WIDTH - 30.0));

    let close = button(text("✕").size(10.0))
        .padding([2, 5])
        .style(move |_, status| button::Style {
            background: Some(
                match status {
                    button::Status::Hovered => palette.danger,
                    _ => iced::Color::TRANSPARENT,
                }
                .into(),
            ),
            text_color: match status {
                button::Status::Hovered => palette.background,
                _ => palette.muted,
            },
            border: iced::Border {
                radius: 3.0.into(),
                ..iced::Border::default()
            },
            ..button::Style::default()
        })
        .on_press(Message::CloseRepository(index));

    // The whole tab is the switch; the ✕ sits inside it as its own button.
    let body = button(container(label).padding([5, 8]))
        .padding(0)
        .style(move |_, status| button::Style {
            background: Some(
                match (active, status) {
                    (true, _) => palette.surface,
                    (false, button::Status::Hovered) => palette.selection_idle,
                    _ => iced::Color::TRANSPARENT,
                }
                .into(),
            ),
            text_color: palette.text,
            ..button::Style::default()
        })
        .on_press(Message::RepositorySelected(index));

    let border = palette.border;
    container(row![body, close].align_y(Center))
        .width(Length::Fixed(TAB_WIDTH))
        .style(move |_| container::Style {
            background: Some(
                if active {
                    palette.surface
                } else {
                    iced::Color::TRANSPARENT
                }
                .into(),
            ),
            // Only the active tab is outlined. Outlining every tab turns the bar
            // into a grid and hides which one you are looking at.
            border: iced::Border {
                color: if active {
                    border
                } else {
                    iced::Color::TRANSPARENT
                },
                width: 1.0,
                radius: iced::border::top_left(4).top_right(4),
            },
            ..container::Style::default()
        })
        .into()
}

/// The tab a `Cmd+<n>` press means, if there is one.
///
/// `Cmd+1` is the first tab. There is no `Cmd+0`, and a number past the last
/// tab does nothing rather than clamping to it: jumping somewhere unintended is
/// worse than nothing happening, especially when the keys are next to each
/// other.
pub fn tab_for_digit(digit: char, open: usize) -> Option<usize> {
    let n = digit.to_digit(10)? as usize;
    if n == 0 || n > open {
        return None;
    }
    Some(n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digits_map_to_tabs_from_one() {
        assert_eq!(tab_for_digit('1', 3), Some(0));
        assert_eq!(tab_for_digit('3', 3), Some(2));
    }

    #[test]
    fn a_digit_past_the_last_tab_does_nothing() {
        // Clamping to the last tab would move you somewhere you did not ask for,
        // and `4` and `3` are adjacent keys.
        assert_eq!(tab_for_digit('4', 3), None);
        assert_eq!(tab_for_digit('9', 0), None);
        // There is no zeroth tab.
        assert_eq!(tab_for_digit('0', 3), None);
        assert_eq!(tab_for_digit('x', 3), None);
    }
}
