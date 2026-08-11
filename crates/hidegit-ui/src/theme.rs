//! Colours.
//!
//! Dark is the default and is designed first; a light theme lands in M6 as a
//! designed theme rather than an inverted dark one. iced's own `Palette` covers
//! background, text and the semantic accents; the extras hideGit needs — a
//! surface tone, a muted text tone and the graph lane colours — live here.
//!
//! Two constraints, from `docs/UI_SPEC.md#theming`:
//!
//! - text contrast meets WCAG AA against its background;
//! - lane colours stay distinguishable under deuteranopia and protanopia. A
//!   graph that is unreadable for 8% of men is a broken graph, so colour is
//!   never the only carrier of meaning — node shape distinguishes merges,
//!   roots and boundaries independently of hue.

use iced::{Color, color};

/// Everything the UI paints with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub background: Color,
    /// Panels sitting on the background: sidebar, detail pane, toolbar.
    pub surface: Color,
    /// The line between panels.
    pub border: Color,
    pub text: Color,
    /// Secondary text: timestamps, hashes, counts.
    pub muted: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    /// Lane colours, cycled by index. Lane indices are reused as lanes free,
    /// so a colour identifies a line on screen, not a branch.
    pub lanes: [Color; 6],
    /// Diff line backgrounds.
    pub added: Color,
    pub removed: Color,
    /// The selected row, while its pane has keyboard focus.
    ///
    /// An opaque colour per theme rather than the accent at some alpha, because
    /// an alpha tuned on a dark background does not transfer: the same wash that
    /// reads as a glow over near-black reads as a stain over near-white, and the
    /// accent is a warm orange while the light page is a cool grey. Each theme
    /// picks the colour that works on *its* background.
    pub selection: Color,
    /// The selected row while the pane does not have focus. Still findable,
    /// visibly not the thing the keyboard is pointed at.
    pub selection_idle: Color,
}

impl Palette {
    /// `hidegit-dark`, the default.
    pub const DARK: Self = Self {
        background: color!(0x16181d),
        surface: color!(0x1c1f26),
        border: color!(0x2a2f3a),
        text: color!(0xe6e8ec),
        muted: color!(0x8b93a3),
        // The orange from the ring in `assets/icon.png`. It clears the same
        // contrast bar the blue it replaced did — 5.13:1 on a panel against
        // 5.15:1 — so the brand colour is used as drawn, not lightened.
        accent: color!(0xf65e17),
        success: color!(0x3fb950),
        warning: color!(0xd29922),
        danger: color!(0xf85149),
        // Amber and red are deliberately absent: they stay semantic, and a
        // lane painted the same red as a conflict was always a mixed signal.
        // Dropping them also keeps three warm lanes from crowding the accent.
        lanes: [
            color!(0xf65e17),
            color!(0x3fb950),
            color!(0x4c8dff),
            color!(0xbc8cff),
            color!(0x39c5cf),
            color!(0xe36bb0),
        ],
        added: color!(0x1b3a24),
        removed: color!(0x3c1618),
        // What the accent at 22% and 8% over the background used to composite
        // to, kept exactly so this change moves nothing in the dark theme.
        selection: color!(0x47271c),
        selection_idle: color!(0x281e1d),
    };

    /// `hidegit-light`, designed rather than inverted.
    ///
    /// The dark theme uses the brand orange exactly as drawn, because it
    /// cleared the contrast bar there. On a near-white panel the same colour
    /// reaches only 3.21:1, so light darkens it — the same decision applied
    /// honestly rather than the same *hex* applied stubbornly. Every colour
    /// here was picked against the numbers the tests assert, not by eye.
    ///
    /// The background is a soft grey and panels are near-white, so a panel is
    /// still the raised surface it is in dark. Inverting dark's relationship —
    /// white page, grey panels — would make every panel read as sunken.
    pub const LIGHT: Self = Self {
        background: color!(0xf0f1f4),
        surface: color!(0xfafbfc),
        border: color!(0xd8dbe0),
        text: color!(0x1c1f26),
        muted: color!(0x5c6470),
        accent: color!(0xb8410a),
        success: color!(0x1a7f37),
        warning: color!(0x9a6700),
        danger: color!(0xcf222e),
        // The same six hues in the same order as dark, darkened to sit on a
        // near-white panel. Their separation under both simulated deficiencies
        // is 0.061, against dark's 0.063 — measured, not assumed.
        lanes: [
            color!(0xb8410a),
            color!(0x1a7f37),
            color!(0x0550ae),
            color!(0x8250df),
            color!(0x106e75),
            color!(0xbf3989),
        ],
        added: color!(0xe6ffec),
        removed: color!(0xffebe9),
        // Warm and very low chroma. The accent washed over this page at the
        // dark theme's alpha gives a muddy salmon — readable, measurably, but
        // the kind of thing that makes a light theme look like an afterthought.
        // The idle one is neutral grey: without focus there is no reason for it
        // to carry the brand colour at all.
        selection: color!(0xf3e7e0),
        selection_idle: color!(0xe9eaed),
    };

    /// The colour for a lane index, cycling through the palette.
    pub fn lane(&self, index: usize) -> Color {
        self.lanes[index % self.lanes.len()]
    }
}

/// The active theme.
///
/// A struct rather than an enum because custom themes are TOML files dropped in
/// the config directory (M6), and a malformed one falls back to the default
/// with a warning rather than preventing startup.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub palette: Palette,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "hidegit-dark".to_owned(),
            palette: Palette::DARK,
        }
    }
}

impl Theme {
    pub const DARK_NAME: &'static str = "hidegit-dark";
    pub const LIGHT_NAME: &'static str = "hidegit-light";

    /// The built-in theme with this name.
    ///
    /// `None` for anything else, which the caller reports and falls back from —
    /// a typo in a config file must not stop the application starting. Custom
    /// themes as TOML files are still to come; this is the pair that ships.
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            Self::DARK_NAME => Some(Self {
                name: name.to_owned(),
                palette: Palette::DARK,
            }),
            Self::LIGHT_NAME => Some(Self {
                name: name.to_owned(),
                palette: Palette::LIGHT,
            }),
            _ => None,
        }
    }

    /// Translates into the theme iced's own widgets style themselves from.
    pub fn to_iced(&self) -> iced::Theme {
        iced::Theme::custom(
            self.name.clone(),
            iced::theme::Palette {
                background: self.palette.background,
                text: self.palette.text,
                primary: self.palette.accent,
                success: self.palette.success,
                warning: self.palette.warning,
                danger: self.palette.danger,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both shipped palettes.
    ///
    /// Every constraint runs over both, because light was added to a suite that
    /// only tested dark — and a light theme whose contrast nobody checked is
    /// exactly the washed-out inversion the spec says not to ship.
    fn palettes() -> [(&'static str, Palette); 2] {
        [("dark", Palette::DARK), ("light", Palette::LIGHT)]
    }

    /// Relative luminance, per WCAG 2.1.
    fn luminance(c: Color) -> f32 {
        let channel = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
    }

    fn contrast(a: Color, b: Color) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        let (light, dark) = if a > b { (a, b) } else { (b, a) };
        (light + 0.05) / (dark + 0.05)
    }

    #[test]
    fn body_text_meets_wcag_aa_against_both_backgrounds() {
        for (theme, p) in palettes() {
            assert!(
                contrast(p.text, p.background) >= 4.5,
                "{theme}: primary text on the background is {:.2}:1",
                contrast(p.text, p.background)
            );
            assert!(
                contrast(p.text, p.surface) >= 4.5,
                "{theme}: primary text on a panel is {:.2}:1",
                contrast(p.text, p.surface)
            );
        }
    }

    #[test]
    fn secondary_text_clears_the_large_text_threshold() {
        // Timestamps, hashes and counts are small but they are still read.
        for (theme, p) in palettes() {
            assert!(
                contrast(p.muted, p.background) >= 3.0,
                "{theme}: secondary text on the background is {:.2}:1",
                contrast(p.muted, p.background)
            );
            assert!(
                contrast(p.muted, p.surface) >= 3.0,
                "{theme}: secondary text on a panel is {:.2}:1",
                contrast(p.muted, p.surface)
            );
        }
    }

    #[test]
    fn the_semantic_colours_are_readable_as_text() {
        // The staging view paints file paths in these: staged in `success`,
        // unstaged in `warning`, conflicted in `danger`. They stopped being
        // decoration the moment they carried a word, so they have to clear the
        // large-text threshold on the panel they sit on.
        //
        // `accent` is in here because the sidebar and the commit detail pane
        // paint text with it too — it is not only a selection highlight.
        for (theme, p) in palettes() {
            for (name, colour) in [
                ("accent", p.accent),
                ("success", p.success),
                ("warning", p.warning),
                ("danger", p.danger),
            ] {
                assert!(
                    contrast(colour, p.surface) >= 3.0,
                    "{theme}: {name} on a panel is {:.2}:1",
                    contrast(colour, p.surface)
                );
            }
        }
    }

    #[test]
    fn lane_colours_are_readable_on_the_panel_they_sit_on() {
        // A lane is a line and a node, not text — but an unreadable lane is an
        // unreadable graph, and light's near-white panel is where the dark
        // theme's lane colours would have quietly disappeared.
        for (theme, p) in palettes() {
            for (index, colour) in p.lanes.iter().enumerate() {
                assert!(
                    contrast(*colour, p.surface) >= 3.0,
                    "{theme}: lane {index} on a panel is {:.2}:1",
                    contrast(*colour, p.surface)
                );
            }
        }
    }

    /// Simulates how a colour appears under a red-green colour vision
    /// deficiency, using the Viénot–Brettel–Mollon reduction.
    fn simulate(c: Color, protanopia: bool) -> (f32, f32, f32) {
        // Linearise, project onto the dichromat's plane, and stay in linear
        // space — only relative distance matters here.
        let lin = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (lin(c.r), lin(c.g), lin(c.b));

        if protanopia {
            (
                0.1121 * r + 0.8853 * g - 0.0005 * b,
                0.1127 * r + 0.8897 * g - 0.0001 * b,
                0.0045 * r + 0.0000 * g + 1.0019 * b,
            )
        } else {
            (
                0.2920 * r + 0.7054 * g - 0.0003 * b,
                0.2934 * r + 0.7089 * g + 0.0000 * b,
                -0.0195 * r + 0.0333 * g + 1.0011 * b,
            )
        }
    }

    fn distance(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
    }

    #[test]
    fn lane_colours_stay_apart_under_deuteranopia_and_protanopia() {
        for (theme, p) in palettes() {
            for protanopia in [false, true] {
                for (i, a) in p.lanes.iter().enumerate() {
                    for b in p.lanes.iter().skip(i + 1) {
                        let d = distance(simulate(*a, protanopia), simulate(*b, protanopia));
                        assert!(
                            d > 0.05,
                            "{theme}: two lane colours collapse to a distance of {d:.3} \
                             (protanopia: {protanopia}); adjacent lanes would be \
                             indistinguishable"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn lane_colours_cycle_rather_than_running_out() {
        for (_, p) in palettes() {
            assert_eq!(p.lane(0), p.lanes[0]);
            assert_eq!(p.lane(6), p.lanes[0]);
            assert_eq!(p.lane(13), p.lanes[1]);
        }
    }

    #[test]
    fn the_two_shipped_themes_are_reachable_by_name() {
        assert_eq!(
            Theme::by_name(Theme::DARK_NAME)
                .expect("dark ships")
                .palette,
            Palette::DARK
        );
        assert_eq!(
            Theme::by_name(Theme::LIGHT_NAME)
                .expect("light ships")
                .palette,
            Palette::LIGHT
        );
        // A typo in a config file must not stop the application starting, so
        // the caller gets `None` to fall back from rather than a panic.
        assert!(Theme::by_name("hidegit-solarized").is_none());
    }

    #[test]
    fn light_is_not_dark_inverted() {
        // The cheap way to ship a light theme is to flip the dark one, which
        // reliably produces muddy semantics and lanes nobody can tell apart.
        // A panel stays *raised* in both: lighter than the page in light, and
        // lighter than the page in dark too.
        assert!(luminance(Palette::LIGHT.surface) > luminance(Palette::LIGHT.background));
        assert!(luminance(Palette::DARK.surface) > luminance(Palette::DARK.background));

        // And the brand orange is darkened rather than reused: as drawn it
        // reaches only 3.21:1 on light's panel, which is below the bar for the
        // text it is used for.
        assert_ne!(Palette::LIGHT.accent, Palette::DARK.accent);
    }
}
