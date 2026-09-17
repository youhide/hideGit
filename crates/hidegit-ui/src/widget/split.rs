//! The dividers you can drag.
//!
//! A hairline is one pixel wide, and one pixel is not something a pointer can
//! be expected to hit. So a draggable divider is a strip
//! [`GRAB`](self::GRAB) pixels across with the hairline drawn down the middle
//! of it: what you see is the same rule [`common::divider`](crate::widget::common::divider)
//! draws everywhere else, and what you can grab is the part either side of it.
//!
//! It emits [`LayoutMessage`], and nothing else — the screen that places it
//! decides how that reaches `update`. That is what lets the same divider sit in
//! the main window, which speaks `Message`, and inside the working directory,
//! which speaks `RepoMessage`: both map it.

use iced::widget::{Space, container, mouse_area};
use iced::{Fill, Length, mouse};

use crate::Element;
use crate::layout::Split;
use crate::message::LayoutMessage;
use crate::metrics;
use crate::theme::Palette;

/// How much of the divider is pointer target rather than hairline.
///
/// Seven pixels: three either side of the rule. Small enough that the panes
/// keep the space they had to within a few pixels, large enough to hit without
/// aiming — the macOS window-resize border is about this wide, and it is the
/// closest thing to a platform answer.
pub const GRAB: f32 = 7.0;

/// A divider between two columns: drag it left and right.
pub fn vertical<'a>(
    split: Split,
    extent: f32,
    dragging: bool,
    palette: &Palette,
) -> Element<'a, LayoutMessage> {
    handle(
        split,
        extent,
        dragging,
        palette,
        (Length::Fixed(GRAB), Fill),
        (Length::Fixed(metrics::HAIR), Fill),
        mouse::Interaction::ResizingColumn,
    )
}

/// A divider between two rows: drag it up and down.
pub fn horizontal<'a>(
    split: Split,
    extent: f32,
    dragging: bool,
    palette: &Palette,
) -> Element<'a, LayoutMessage> {
    handle(
        split,
        extent,
        dragging,
        palette,
        (Fill, Length::Fixed(GRAB)),
        (Fill, Length::Fixed(metrics::HAIR)),
        mouse::Interaction::ResizingRow,
    )
}

#[allow(clippy::too_many_arguments)]
fn handle<'a>(
    split: Split,
    extent: f32,
    dragging: bool,
    palette: &Palette,
    (width, height): (Length, Length),
    (rule_width, rule_height): (Length, Length),
    interaction: mouse::Interaction,
) -> Element<'a, LayoutMessage> {
    // The rule brightens while it is being dragged, and only then. Without it
    // there is no way to tell a drag that was picked up from a press that
    // missed — the panes either side move, but not until the pointer does.
    let colour = if dragging {
        palette.accent
    } else {
        palette.border
    };

    let rule = container(Space::new())
        .width(rule_width)
        .height(rule_height)
        .style(move |_| container::Style {
            background: Some(colour.into()),
            ..container::Style::default()
        });

    mouse_area(
        container(rule)
            .width(width)
            .height(height)
            .center_x(width)
            .center_y(height),
    )
    .interaction(interaction)
    .on_press(LayoutMessage::Grabbed(split, extent))
    // The pointer usually leaves the divider during a drag, so the release that
    // ends one arrives through the subscription rather than here. This is for
    // the press that never became a drag at all.
    .on_release(LayoutMessage::Released)
    .on_double_click(LayoutMessage::Reset(split))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    /// A divider carries no text, so it cannot be found the way the rest of the
    /// render tests find things. What can be asserted is what pressing it says,
    /// which is the whole of its job.
    fn pressed(element: Element<'_, LayoutMessage>, at: (f32, f32)) -> Vec<LayoutMessage> {
        let mut ui = iced_test::simulator(element);
        ui.point_at(at);
        let _ = ui.simulate(iced_test::simulator::click());

        ui.into_messages().collect()
    }

    #[test]
    fn pressing_a_divider_grabs_it_and_carries_the_space_it_divides() {
        let palette = Theme::default().palette;

        // Three pixels in: the hairline is one pixel of a seven-pixel strip, so
        // this is a press that hits the target and misses the rule.
        let messages = pressed(
            vertical(Split::Sidebar, 1440.0, false, &palette),
            (3.0, 100.0),
        );

        assert_eq!(
            messages.first(),
            Some(&LayoutMessage::Grabbed(Split::Sidebar, 1440.0)),
            "the extent is measured by the view, so the grab has to carry it"
        );
    }

    #[test]
    fn a_press_beside_the_divider_grabs_nothing() {
        let palette = Theme::default().palette;

        // Deliberately outside the strip: the grab area is generous, not
        // unbounded, and a divider that swallowed presses meant for the pane
        // beside it would be worse than a thin one.
        let messages = pressed(
            vertical(Split::Sidebar, 1440.0, false, &palette),
            (40.0, 100.0),
        );

        assert!(messages.is_empty(), "got {messages:?}");
    }

    #[test]
    fn a_horizontal_divider_is_grabbed_across_its_thickness_rather_than_its_length() {
        let palette = Theme::default().palette;

        let messages = pressed(
            horizontal(Split::Detail, 900.0, true, &palette),
            (100.0, 3.0),
        );

        assert_eq!(
            messages.first(),
            Some(&LayoutMessage::Grabbed(Split::Detail, 900.0))
        );
    }
}
