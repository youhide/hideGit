//! The pieces every screen needs and none of them should own.
//!
//! A rule between two things is not a design decision any one widget gets to
//! make, but it was made twelve times: nine horizontal rules under three
//! spellings — `divider` eight times and `horizontal_rule` once — and three
//! vertical ones. Six of the nine were the same nine lines character for
//! character.
//!
//! They could not simply be shared, for two reasons that are worth naming
//! because they are the reason this took a module rather than a deletion.
//!
//! **They disagreed about their argument.** Seven took `&Palette` and five took
//! an `iced::Color` the caller had already pulled out of one. The colour form
//! leaves each call site deciding what colour a rule is, which is exactly the
//! decision that should not be made twelve times.
//!
//! **They disagreed about their message type.** Some returned
//! `Element<'a, Message>` and some `Element<'a, RepoMessage>`, which reads like
//! a real obstacle and is not: a rule emits nothing, so the message type is
//! free and these are generic over it.

use iced::widget::{Space, container, text};
use iced::{Fill, Length};

use crate::Element;
use crate::metrics;
use crate::theme::Palette;

/// A hairline across the full width.
///
/// Always `palette.border`, because a rule that is any other colour is not a
/// rule — it is a band, and a band is a different thing with a different job.
pub fn divider<'a, M: 'a>(palette: &Palette) -> Element<'a, M> {
    rule(palette, Fill, Length::Fixed(metrics::HAIR))
}

/// A hairline down the full height, for splitting a row into columns.
pub fn vertical_rule<'a, M: 'a>(palette: &Palette) -> Element<'a, M> {
    rule(palette, Length::Fixed(metrics::HAIR), Fill)
}

fn rule<'a, M: 'a>(palette: &Palette, width: Length, height: Length) -> Element<'a, M> {
    let colour = palette.border;
    container(Space::new())
        .width(width)
        .height(height)
        .style(move |_| container::Style {
            background: Some(colour.into()),
            ..container::Style::default()
        })
        .into()
}

/// What a pane shows when it holds nothing yet.
///
/// Three of these existed — `empty` in the diff, `placeholder` in the detail
/// pane and a third `placeholder` in staging — rendering the same centred muted
/// line three ways.
///
/// `UI_SPEC.md` asks empty states to "carry the next action, not just an
/// absence", and most of these do not yet. Giving them one is a separate change;
/// what this does is make there be one place to give it to.
pub fn empty<'a, M: 'a>(message: impl text::IntoFragment<'a>, palette: &Palette) -> Element<'a, M> {
    centred(message, palette)
}

/// An empty state that carries the next action, which `UI_SPEC.md` asks all of
/// them to.
///
/// The action is a button rather than a sentence telling you where to click.
/// "The working directory matches the last commit" is a description; a button
/// that shows you that commit is the thing the description was standing in for.
pub fn empty_offering<'a, M: Clone + 'a>(
    message: impl text::IntoFragment<'a>,
    label: impl text::IntoFragment<'a>,
    action: M,
    palette: &Palette,
) -> Element<'a, M> {
    let muted = palette.muted;
    let palette = *palette;

    container(
        iced::widget::column![
            text(message).size(metrics::text::BODY).color(muted),
            iced::widget::button(text(label).size(metrics::text::BODY))
                .padding(iced::Padding::from([metrics::SNUG, metrics::WIDE]))
                .style(move |_, status| button::quiet(palette, status))
                .on_press(action),
        ]
        .spacing(metrics::WIDE)
        .align_x(iced::Center),
    )
    .width(Fill)
    .height(Fill)
    .center(Fill)
    .into()
}

/// What a pane shows while it is still reading.
///
/// The same picture as [`empty`] and deliberately not the same function. A pane
/// with nothing in it and a pane that has not finished looking are different
/// states, and the difference is about to matter: an empty state is getting the
/// next action, and "Loading…" has no next action to offer. Two of the seven
/// call sites this replaced were this state wearing the other one's name.
pub fn loading<'a, M: 'a>(
    message: impl text::IntoFragment<'a>,
    palette: &Palette,
) -> Element<'a, M> {
    centred(message, palette)
}

fn centred<'a, M: 'a>(message: impl text::IntoFragment<'a>, palette: &Palette) -> Element<'a, M> {
    let muted = palette.muted;
    container(text(message).size(metrics::text::BODY).color(muted))
        .width(Fill)
        .height(Fill)
        .center(Fill)
        .into()
}

/// How a button is drawn, by what pressing it does.
///
/// Four designs for "the primary button" existed at once and disagreed about
/// every part of it: the text was `Color::WHITE` in three places and
/// `palette.background` in two, the radius was 6 in three and 3 in two, hover
/// was 0.85, 0.9 or the border colour depending where you looked.
///
/// The white text was not merely inconsistent. Measured against WCAG 2.1, white
/// on the dark theme's accent is **3.21:1** and on its danger **3.35:1** —
/// under the 4.5:1 that button labels need — while `palette.background` clears
/// it in both themes. That is the whole argument for naming these: the contrast
/// tests covered body text, secondary text, the semantic colours as text and
/// the graph lanes, but nothing checked the text *on* a filled button, because
/// no palette slot describes it.
///
/// These take `Palette` by value rather than by reference, unlike everything
/// else here. A style is a closure the widget keeps, so it has to own what it
/// paints with; that is where the crate's two argument conventions come from.
pub mod button {
    use iced::widget::button;

    use crate::metrics;
    use crate::theme::{self, Palette};

    /// The button that does the thing the screen is about.
    pub fn primary(palette: Palette, status: button::Status) -> button::Style {
        filled(palette, palette.accent, status)
    }

    /// The button that destroys something.
    ///
    /// Separate from [`primary`] because `UI_SPEC.md` requires destructive
    /// actions to be distinguishable, which only works if creating a branch
    /// does not wear the same colour as deleting one.
    pub fn danger(palette: Palette, status: button::Status) -> button::Style {
        filled(palette, palette.danger, status)
    }

    /// The button that is not the answer: Cancel, and the toolbar.
    ///
    /// Outlined rather than bare. `UI_SPEC.md` wants Cancel unemphasised, which
    /// an outline does not undo — it only makes the target visible before the
    /// pointer is over it.
    pub fn quiet(palette: Palette, status: button::Status) -> button::Style {
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
            text_color: match status {
                button::Status::Disabled => palette.muted,
                _ => palette.text,
            },
            border: iced::Border {
                color: palette.border,
                width: metrics::HAIR,
                radius: metrics::radius::SMALL.into(),
            },
            ..button::Style::default()
        }
    }

    /// A button filled with `base`, in every state it can be in.
    fn filled(palette: Palette, base: iced::Color, status: button::Status) -> button::Style {
        // Disabled is an opaque pair rather than the fill at an alpha: what an
        // alpha lands on depends on whatever is behind the button, so its
        // contrast cannot be stated, let alone asserted.
        let (background, text_color) = match status {
            button::Status::Disabled => (palette.border, palette.muted),
            button::Status::Hovered | button::Status::Pressed => (
                iced::Color {
                    a: theme::HOVERED,
                    ..base
                },
                palette.background,
            ),
            button::Status::Active => (base, palette.background),
        };

        button::Style {
            background: Some(background.into()),
            text_color,
            border: iced::Border {
                radius: metrics::radius::SMALL.into(),
                ..iced::Border::default()
            },
            ..button::Style::default()
        }
    }
}

#[cfg(test)]
mod tests {
    /// Every view file, so the guard below cannot be outrun by a new one.
    fn view_sources() -> Vec<(String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();

        for directory in ["widget", "screen"] {
            let entries = std::fs::read_dir(root.join(directory)).expect("a readable source tree");
            for entry in entries {
                let path = entry.expect("a readable entry").path();
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let name = path
                    .file_name()
                    .expect("a named file")
                    .to_string_lossy()
                    .into_owned();
                if directory == "widget" && name == "common.rs" {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("a readable source file");
                out.push((format!("{directory}/{name}"), source));
            }
        }

        out
    }

    #[test]
    fn no_view_file_draws_its_own_rule_or_button() {
        // Nine horizontal rules and three vertical ones existed before this
        // module, six of them identical character for character. Deleting them
        // is easy; keeping them deleted is what needs a test, because the next
        // one is always cheaper to write than to find.
        //
        // Discovered by reading the directory rather than from a list, so a
        // widget added tomorrow is covered without anyone remembering to add
        // it.
        let mut offenders = Vec::new();

        for (name, source) in view_sources() {
            // The rules, the five button roles that were duplicated, and the
            // two hardcoded colours. `Color::WHITE` earned its place here: it
            // was the label colour on three primary buttons, at 3.21:1 on the
            // dark theme, and it was also painted *over* two of them from the
            // button's content where the style could not correct it.
            for shape in [
                "fn divider",
                "fn vertical_rule",
                "fn horizontal_rule",
                "fn placeholder",
                "fn accent_style",
                "fn danger_style",
                "fn quiet_style",
                "fn primary_style",
                "fn secondary_style",
                "Color::WHITE",
                "Color::BLACK",
            ] {
                if source.contains(shape) {
                    offenders.push(format!("{name} defines its own `{shape}`"));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "a rule, a button role and a colour all belong to the palette or to \
             `widget::common`: {offenders:#?}"
        );
    }

    /// Every filled button, in the states whose text has to be readable.
    fn readable_states() -> [iced::widget::button::Status; 3] {
        use iced::widget::button::Status;
        [Status::Active, Status::Hovered, Status::Pressed]
    }

    #[test]
    fn a_button_label_is_readable_on_the_button() {
        // The gap this closes. Contrast was asserted for body text, secondary
        // text, the semantic colours used *as* text, and the graph lanes —
        // never for a label on a filled button, because no palette slot
        // describes that pairing.
        //
        // It was failing. `Color::WHITE` on the dark theme's accent is 3.21:1
        // and on its danger 3.35:1, both under the 4.5:1 a button label needs,
        // and three of the five primary buttons used it.
        use super::button;

        for (name, palette) in crate::theme::tests::palettes() {
            for status in readable_states() {
                for (role, style) in [
                    ("primary", button::primary(palette, status)),
                    ("danger", button::danger(palette, status)),
                ] {
                    let background = match style.background {
                        Some(iced::Background::Color(colour)) => colour,
                        other => panic!("{name}/{role}: expected a filled button, got {other:?}"),
                    };
                    let ratio = crate::theme::tests::contrast(style.text_color, background);
                    assert!(
                        ratio >= 4.5,
                        "{name}/{role} in {status:?} is {ratio:.2}:1, under the 4.5:1 a label needs"
                    );
                }
            }
        }
    }

    #[test]
    fn a_destructive_button_never_wears_the_ordinary_one_s_colour() {
        // `UI_SPEC.md` requires destructive actions to be distinguishable, and
        // nothing checked it: painting `danger` with `accent` passed every test
        // in the crate.
        use super::button;
        use iced::widget::button::Status;

        for (name, palette) in crate::theme::tests::palettes() {
            let ordinary = button::primary(palette, Status::Active).background;
            let destructive = button::danger(palette, Status::Active).background;
            assert_ne!(
                ordinary, destructive,
                "{name}: deleting a branch must not look like creating one"
            );
        }
    }

    #[test]
    fn the_guard_reads_every_view_file() {
        // A guard that quietly read nothing would pass forever.
        let sources = view_sources();
        assert!(
            sources.len() > 10,
            "expected the whole view tree, got {}",
            sources.len()
        );
        assert!(
            sources
                .iter()
                .any(|(name, _)| name == "screen/repository.rs"),
            "the screens are view files too"
        );
        assert!(
            !sources.iter().any(|(name, _)| name == "widget/common.rs"),
            "this module is where a rule is allowed to be defined"
        );
    }
}
