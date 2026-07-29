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
#[derive(Debug, Clone, Copy)]
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
}

impl Palette {
    /// `hidegit-dark`, the default.
    pub const DARK: Self = Self {
        background: color!(0x16181d),
        surface: color!(0x1c1f26),
        border: color!(0x2a2f3a),
        text: color!(0xe6e8ec),
        muted: color!(0x8b93a3),
        accent: color!(0x4c8dff),
        success: color!(0x3fb950),
        warning: color!(0xd29922),
        danger: color!(0xf85149),
        lanes: [
            color!(0x4c8dff),
            color!(0x3fb950),
            color!(0xd29922),
            color!(0xbc8cff),
            color!(0x39c5cf),
            color!(0xf85149),
        ],
        added: color!(0x1b3a24),
        removed: color!(0x3c1618),
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
        let p = Palette::DARK;
        assert!(
            contrast(p.text, p.background) >= 4.5,
            "primary text on the background is {:.2}:1",
            contrast(p.text, p.background)
        );
        assert!(
            contrast(p.text, p.surface) >= 4.5,
            "primary text on a panel is {:.2}:1",
            contrast(p.text, p.surface)
        );
    }

    #[test]
    fn muted_text_meets_the_large_text_threshold() {
        let p = Palette::DARK;
        // Timestamps and hashes are secondary, so AA large (3:1) is the bar
        // they have to clear — not "whatever looks subtle enough".
        assert!(
            contrast(p.muted, p.background) >= 3.0,
            "muted text is {:.2}:1",
            contrast(p.muted, p.background)
        );
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
        let lanes = Palette::DARK.lanes;

        for protanopia in [false, true] {
            for (i, a) in lanes.iter().enumerate() {
                for b in lanes.iter().skip(i + 1) {
                    let d = distance(simulate(*a, protanopia), simulate(*b, protanopia));
                    assert!(
                        d > 0.05,
                        "two lane colours collapse to a distance of {d:.3} \
                         (protanopia: {protanopia}); adjacent lanes would be indistinguishable"
                    );
                }
            }
        }
    }

    #[test]
    fn lane_colours_cycle_rather_than_running_out() {
        let p = Palette::DARK;
        assert_eq!(p.lane(0), p.lanes[0]);
        assert_eq!(p.lane(6), p.lanes[0]);
        assert_eq!(p.lane(13), p.lanes[1]);
    }
}
