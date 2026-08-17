//! Syntax highlighting for diffs.
//!
//! Deferred at M1 — correct diffing first — and this is the other end of that.
//! syntect through `iced::highlighter`, driven a line at a time rather than
//! through `text_editor`, because a diff is not a document: it is two documents
//! interleaved.
//!
//! **Two parsers, one per side.** syntect is stateful across lines — an open
//! string or block comment changes how the next line reads — so feeding it a
//! removed line and then the added line that replaced it corrupts both. The old
//! side sees context and removed lines; the new side sees context and added
//! ones. Each line is then coloured by the parser that belongs to its version
//! of the file.
//!
//! **It is still approximate, and deliberately so.** A hunk starts wherever the
//! diff starts, not at the top of the file, so a line inside a block comment
//! that opened fifty lines earlier is highlighted as code. Reading the whole
//! file to fix that means reading every blob of every commit anyone clicks on.
//! The colours are a reading aid; the text is the truth.
//!
//! **Every colour is lifted to WCAG AA.** Syntax themes dim comments on
//! purpose; measured against hideGit's row backgrounds, the worst colour in
//! every syntect theme that ships lands between 2.33:1 and 2.62:1 — below the
//! 4.5:1 `UI_SPEC` guarantees for text. Rather than drop the guarantee for the
//! one pane where people read for the longest, each colour that falls short is
//! moved along lightness, minimally, until it clears. Comments come out dimmer
//! than the code and still readable, which is the distinction they were carrying
//! anyway.
//!
//! **Context lines lose their muting.** They were dimmed to keep added and
//! removed lines forward; with highlighting on, every line carries its own
//! colours. What separates them is the marker glyph and the row background —
//! which is what `UI_SPEC` says has to carry that reading anyway, because colour
//! alone never does.

use std::ops::Range;
use std::path::Path;

use hidegit_core::model::LineKind;
use iced::advanced::text::Highlighter as _;
use iced::widget::text;
use iced::{Color, Font};

use crate::theme::Palette;

/// Above this many lines in one file's diff, the file is rendered plain.
///
/// syntect is not free, and the pane redraws on every frame it is scrolled. The
/// number is deliberately generous — a diff this size is already a wall of text
/// nobody reads line by line — and the cost of being wrong is colour, not
/// correctness.
pub const MAX_LINES: usize = 4_000;

/// The two parsers a diff needs, or nothing at all.
#[derive(Debug)]
pub struct Painter {
    sides: Option<Box<Sides>>,
    palette: Palette,
}

#[derive(Debug)]
struct Sides {
    old: iced::highlighter::Highlighter,
    new: iced::highlighter::Highlighter,
}

impl Painter {
    /// A painter for one file's diff.
    ///
    /// Plain — highlighting nothing — for a file with no extension to go on, or
    /// one big enough that colouring it would cost more than it is worth. An
    /// extension syntect does not know is not a failure: it resolves to plain
    /// text and every line comes back in one piece.
    pub fn new(path: &Path, lines: usize, palette: &Palette) -> Self {
        let token = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_lowercase();

        if token.is_empty() || lines > MAX_LINES {
            return Self {
                sides: None,
                palette: *palette,
            };
        }

        let settings = iced::highlighter::Settings {
            theme: theme_for(palette),
            token,
        };

        Self {
            sides: Some(Box::new(Sides {
                old: iced::highlighter::Highlighter::new(&settings),
                new: iced::highlighter::Highlighter::new(&settings),
            })),
            palette: *palette,
        }
    }

    /// A painter that colours nothing, for the callers that never had a file.
    pub fn plain() -> Self {
        Self {
            sides: None,
            palette: Palette::DARK,
        }
    }

    /// The coloured pieces of one line, in order and covering all of it.
    ///
    /// Every line has to be fed, in the diff's own order, even when its spans
    /// are thrown away: the parser's state after a line depends on having seen
    /// it. That is why a removed line is still pushed through the old side even
    /// though only the new side draws below it.
    pub fn line<'a>(
        &mut self,
        kind: LineKind,
        source: &'a str,
        fallback: Color,
    ) -> Vec<text::Span<'a, (), Font>> {
        let behind = self.behind(kind);
        let whole = || {
            vec![
                text::Span::new(source)
                    .color(fallback)
                    .font(Font::MONOSPACE),
            ]
        };

        let Some(sides) = &mut self.sides else {
            return whole();
        };

        // Both sides see a context line, and the one that does not own this
        // line has its answer dropped.
        let spans: Vec<(Range<usize>, Option<Color>, Option<Font>)> = match kind {
            LineKind::Context => {
                let _ = sides.old.highlight_line(source).count();
                collect(&mut sides.new, source)
            }
            LineKind::Removed => collect(&mut sides.old, source),
            LineKind::Added => collect(&mut sides.new, source),
        };

        if spans.is_empty() {
            return whole();
        }

        // syntect reports byte ranges into the line it was given, and they are
        // character-aligned for the valid UTF-8 a `String` always holds — so
        // this should never fire. It is here because the alternative to a check
        // is a panic in the middle of a diff, and lines come out of
        // repositories rather than out of this codebase.
        if spans
            .iter()
            .any(|(range, _, _)| source.get(range.clone()).is_none())
        {
            return whole();
        }

        spans
            .into_iter()
            .filter_map(|(range, colour, font)| {
                let piece = source.get(range)?;
                Some(
                    text::Span::new(piece)
                        .color(
                            colour
                                .map(|colour| readable(colour, behind))
                                .unwrap_or(fallback),
                        )
                        .font(font.unwrap_or(Font::MONOSPACE)),
                )
            })
            .collect()
    }
}

impl Painter {
    /// What a line of this kind is drawn on, which is what its colour has to be
    /// readable against.
    fn behind(&self, kind: LineKind) -> Color {
        match kind {
            LineKind::Added => self.palette.added,
            LineKind::Removed => self.palette.removed,
            LineKind::Context => self.palette.surface,
        }
    }
}

/// `colour`, moved as little as necessary to clear WCAG AA against `behind`.
///
/// Towards white on a dark row and towards black on a light one, by bisection:
/// contrast is monotonic along that path, so twenty steps land within a
/// thousandth of the least change that works. A colour already clearing the bar
/// is returned untouched, which is most of them.
fn readable(colour: Color, behind: Color) -> Color {
    if contrast(colour, behind) >= 4.5 {
        return colour;
    }

    let toward = if luminance(behind) < 0.5 {
        Color::WHITE
    } else {
        Color::BLACK
    };

    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..20 {
        let middle = f32::midpoint(low, high);
        if contrast(mix(colour, toward, middle), behind) >= 4.5 {
            high = middle;
        } else {
            low = middle;
        }
    }

    mix(colour, toward, high)
}

fn mix(from: Color, to: Color, amount: f32) -> Color {
    Color {
        r: from.r + (to.r - from.r) * amount,
        g: from.g + (to.g - from.g) * amount,
        b: from.b + (to.b - from.b) * amount,
        a: from.a,
    }
}

fn contrast(a: Color, b: Color) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    let (light, dark) = if a > b { (a, b) } else { (b, a) };
    (light + 0.05) / (dark + 0.05)
}

fn collect(
    highlighter: &mut iced::highlighter::Highlighter,
    source: &str,
) -> Vec<(Range<usize>, Option<Color>, Option<Font>)> {
    highlighter
        .highlight_line(source)
        .map(|(range, highlight)| (range, highlight.color(), highlight.font()))
        .collect()
}

/// Which syntect theme to colour with, decided from the palette rather than
/// from the theme's name.
///
/// A custom theme is a file somebody wrote; asking whether its background is
/// dark answers for every theme, including the ones that do not exist yet.
fn theme_for(palette: &Palette) -> iced::highlighter::Theme {
    if luminance(palette.background) < 0.5 {
        iced::highlighter::Theme::Base16Ocean
    } else {
        iced::highlighter::Theme::InspiredGitHub
    }
}

/// Relative luminance, per WCAG 2.1. The same formula the theme tests use.
fn luminance(colour: Color) -> f32 {
    let channel = |v: f32| {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(colour.r) + 0.7152 * channel(colour.g) + 0.0722 * channel(colour.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: &str = "let answer = 42; // and a comment";

    fn painter(name: &str) -> Painter {
        Painter::new(Path::new(name), 10, &Palette::DARK)
    }

    fn text_of(spans: &[text::Span<'_, (), Font>]) -> String {
        spans.iter().map(|span| span.text.as_ref()).collect()
    }

    #[test]
    fn a_known_extension_breaks_a_line_into_pieces() {
        let spans = painter("src/main.rs").line(LineKind::Added, RUST, Color::WHITE);

        assert!(spans.len() > 1, "the line was not highlighted at all");
        assert_eq!(text_of(&spans), RUST, "and every byte survived");
    }

    #[test]
    fn an_unknown_extension_falls_back_to_one_plain_piece() {
        // syntect resolves an unknown token to plain text rather than failing,
        // and a file with no extension never asks it anything.
        for name in ["notes.wibble", "Makefile", "src/main.rs.bak"] {
            let spans = painter(name).line(LineKind::Added, RUST, Color::WHITE);
            assert_eq!(text_of(&spans), RUST, "{name} lost text");
        }

        let spans = painter("Makefile").line(LineKind::Added, RUST, Color::WHITE);
        assert_eq!(spans.len(), 1, "nothing to go on, so nothing is coloured");
        assert_eq!(spans[0].color, Some(Color::WHITE), "the caller's colour");
    }

    #[test]
    fn a_diff_past_the_line_budget_is_not_coloured() {
        // The cost of being wrong here is colour, not correctness.
        let big = Painter::new(Path::new("src/main.rs"), MAX_LINES + 1, &Palette::DARK).line(
            LineKind::Added,
            RUST,
            Color::WHITE,
        );

        assert_eq!(big.len(), 1);
    }

    #[test]
    fn the_two_sides_do_not_corrupt_each_other() {
        // The reason there are two parsers. A removed line that opens a string
        // must not leave the added line below it inside one.
        let mut split = painter("src/main.rs");
        let _ = split.line(LineKind::Removed, "let s = \"unterminated", Color::WHITE);
        let after = split.line(LineKind::Added, RUST, Color::WHITE);

        let mut clean = painter("src/main.rs");
        let expected = clean.line(LineKind::Added, RUST, Color::WHITE);

        assert_eq!(
            after.len(),
            expected.len(),
            "the added line was read through the removed line's open string"
        );
    }

    #[test]
    fn a_line_with_multi_byte_characters_keeps_every_one() {
        // Byte ranges over a line that is not all ASCII. syntect aligns them to
        // characters, so this passes — and would stop passing the day it did
        // not, which is the only thing a test can honestly claim here.
        let line = "let emoji = \"🙂🙃\"; // ação, não reação";
        let spans = painter("src/main.rs").line(LineKind::Context, line, Color::WHITE);

        assert_eq!(text_of(&spans), line);
    }

    #[test]
    fn every_colour_the_diff_paints_clears_wcag_aa() {
        // Syntax themes dim comments on purpose: measured raw against these
        // backgrounds, the worst colour in every syntect theme that ships lands
        // between 2.33:1 and 2.62:1. `UI_SPEC` guarantees 4.5:1 for text, and
        // the diff is the pane people read for the longest, so the colours are
        // lifted rather than the guarantee dropped.
        let sample = [
            "use std::collections::BTreeMap;",
            "/// A doc comment.",
            "pub fn answer(n: u32) -> String {",
            "    let text = format!(\"{n} things\"); // trailing",
            "    if n > 0 { text } else { String::new() }",
            "}",
        ];

        for palette in [Palette::DARK, Palette::LIGHT] {
            for kind in [LineKind::Added, LineKind::Removed, LineKind::Context] {
                let mut painter = Painter::new(Path::new("src/main.rs"), 10, &palette);
                let behind = painter.behind(kind);

                for line in sample {
                    for span in painter.line(kind, line, palette.text) {
                        let colour = span.color.expect("every span is coloured");
                        assert!(
                            contrast(colour, behind) >= 4.5,
                            "{:?} on {kind:?} is {:.2}:1",
                            span.text,
                            contrast(colour, behind)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn lifting_a_colour_moves_it_as_little_as_it_can() {
        // A colour that already clears the bar is left exactly as the theme
        // drew it, which is most of them.
        let fine = Color::from_rgb(1.0, 1.0, 1.0);
        assert_eq!(readable(fine, Palette::DARK.surface), fine);

        // And one that does not is moved just past it, not to white.
        let dim = Color::from_rgb(0.25, 0.27, 0.3);
        let lifted = readable(dim, Palette::DARK.surface);
        assert!(contrast(lifted, Palette::DARK.surface) >= 4.5);
        assert!(
            contrast(lifted, Palette::DARK.surface) < 5.0,
            "moved further than it had to: {:.2}:1",
            contrast(lifted, Palette::DARK.surface)
        );
        assert_ne!(lifted, Color::WHITE);
    }

    #[test]
    fn the_syntax_theme_follows_the_palette_rather_than_its_name() {
        assert!(theme_for(&Palette::DARK).is_dark());
        assert!(!theme_for(&Palette::LIGHT).is_dark());
    }
}
