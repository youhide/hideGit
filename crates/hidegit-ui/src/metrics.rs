//! The layout scale: how much space goes between things, and how round a
//! corner is.
//!
//! Colour has been a system since M1 — twenty named slots, two hand-authored
//! palettes, and contrast asserted over both. Everything that is not colour was
//! not a system at all. Measured across this crate before this module existed:
//! **25 distinct `Padding` pairs** over 68 sites, twelve `spacing` values and
//! six border radii, with near-duplicate clusters — `[3,6]`, `[3,8]`, `[3,10]`
//! and `[3,12]` all in use, chosen a call site at a time.
//!
//! That is not a stylistic complaint. Values picked one at a time drift, and
//! nothing in the test suite notices: spacing and sizing were the crate's
//! largest blind spot, where any number could change and every one of nearly
//! nine hundred tests would still pass.
//!
//! # What the scale governs, and what it does not
//!
//! It governs **space between things** and **corner radii** — the numbers that
//! should agree across the interface because a reader compares them without
//! meaning to.
//!
//! It does not govern **a component's own dimensions**. `TAB_WIDTH` is a
//! decision about how much room a repository name needs; it belongs next to the
//! tab and reads better as a named local constant than as a step on a shared
//! ladder. The guard below leaves those alone.
//!
//! Type sizes are not here yet — that is its own step, and this module grows a
//! type scale when it arrives.

/// No space at all.
///
/// Worth a name rather than a bare `0`: most uses are stripping the padding
/// iced gives a `button` by default, which is a deliberate act rather than an
/// absence.
pub const NONE: f32 = 0.0;

/// A hairline. The seam between two tabs, or between the two stacked lines of
/// one label — enough to separate, not enough to read as a gap.
pub const HAIR: f32 = 1.0;

/// Barely apart. Rows in a dense list that are already separated by contrast.
pub const TIGHT: f32 = 2.0;

/// The vertical step for a row of text: enough to clear the descenders.
pub const SNUG: f32 = 4.0;

/// The default gap between two related things.
pub const BASE: f32 = 6.0;

/// The horizontal step for a control's padding: text wants more room beside it
/// than above it.
pub const ROOMY: f32 = 8.0;

/// Between groups rather than between items.
pub const WIDE: f32 = 12.0;

/// The inset of a panel or a dialog from its own edge.
pub const LOOSE: f32 = 16.0;

/// Corner radii.
///
/// Three steps, because the crate had six and no two of them meant anything
/// different. A control is [`SMALL`](radius::SMALL), a panel is
/// [`MEDIUM`](radius::MEDIUM), and a dialog — the only thing that floats over
/// everything else — is [`LARGE`](radius::LARGE).
pub mod radius {
    /// Buttons, badges, chips: anything you click or read a word inside.
    pub const SMALL: f32 = 4.0;

    /// A panel or a card that holds other things.
    pub const MEDIUM: f32 = 6.0;

    /// A dialog, which floats and therefore needs to read as a separate sheet.
    pub const LARGE: f32 = 8.0;
}

/// Type sizes.
///
/// Named for the job rather than the number, because the number is the part
/// that drifts: eight sizes were in use across 249 call sites, and six
/// file-local constants duplicated each other — `ITEM_SIZE = 13.0` written out
/// three separate times, `CODE_SIZE = 12.0` three more.
///
/// Two sizes are deliberately missing. `14.0` and `16.0` are each a *third*
/// spelling of "the title of something", one pixel from [`LEAD`](text::LEAD),
/// and the settings panel shows why that is not a naming problem: its header is
/// 14 while the option labels beneath it are 15, so the heading is smaller than
/// what it heads. Choosing what those sites should say is a decision about
/// hierarchy rather than a rename, so they keep their literals until it is made.
pub mod text {
    /// The smallest thing that is still a label: a tab's branch line, a glyph
    /// in a button.
    pub const MICRO: f32 = 10.0;

    /// Section headings and secondary text — the most common size in the
    /// interface, and the one that carries structure rather than content.
    pub const LABEL: f32 = 11.0;

    /// Anything monospaced, and the dense metadata that sits beside it: a diff,
    /// a blame gutter, a hash.
    pub const CODE: f32 = 12.0;

    /// Ordinary content. A list row, a commit summary, a sentence.
    pub const BODY: f32 = 13.0;

    /// A thing's own title: a dialog, a panel, the commit you are reading.
    pub const LEAD: f32 = 15.0;

    /// The two places that are meant to be looked at rather than read — the
    /// wordmark, and a device code somebody is copying into a browser.
    pub const DISPLAY: f32 = 28.0;
}

/// Files that have been moved onto the scale.
///
/// The guard below reads each one and fails if a raw number has come back, so
/// this list is the migration's own progress bar: a file joins it in the change
/// that converts it, and cannot quietly regress afterwards.
///
/// Paths are relative to `src/`. Compiled only under test, because that is the
/// only thing that reads it — it is a fixture that happens to double as the
/// clearest statement of how far this has got.
///
/// One list per scale, because the two migrate at different speeds: a file can
/// be entirely on the type scale while its paddings are still hand-picked, and
/// pretending otherwise would mean converting everything about a file at once
/// or nothing at all.
#[cfg(test)]
const ON_THE_LAYOUT_SCALE: &[&str] = &["widget/tabs.rs"];

/// Files whose every type size comes from [`text`].
#[cfg(test)]
const ON_THE_TYPE_SCALE: &[&str] = &[
    "screen/repository.rs",
    "widget/blame.rs",
    "widget/detail.rs",
    "widget/diff.rs",
    "widget/overlay.rs",
    "widget/resolver.rs",
    "widget/search.rs",
    "widget/sidebar.rs",
    "widget/staging.rs",
    "widget/tabs.rs",
];

/// Where a raw layout number is not allowed once a file is on the scale.
///
/// Deliberately not every number in the file. These three are the ones the
/// scale is *about* — a component's own width or height is its business, and
/// flagging it would push local decisions into a shared ladder they do not
/// belong on.
#[cfg(test)]
const LAYOUT_PROPERTIES: &[&str] = &[".padding(", ".spacing(", "radius:"];

/// Where a raw type size is not allowed once a file is on the type scale.
#[cfg(test)]
const TYPE_PROPERTIES: &[&str] = &[".size("];

#[cfg(test)]
mod tests {
    use super::*;

    /// The scale steps, smallest first.
    fn steps() -> Vec<f32> {
        vec![NONE, HAIR, TIGHT, SNUG, BASE, ROOMY, WIDE, LOOSE]
    }

    #[test]
    fn the_scale_climbs_and_never_repeats_itself() {
        // Two steps with the same value would be two names for one decision,
        // which is the state this module exists to end.
        let steps = steps();
        for pair in steps.windows(2) {
            assert!(
                pair[1] > pair[0],
                "the scale must increase: {} is not above {}",
                pair[1],
                pair[0]
            );
        }

        let radii = [radius::SMALL, radius::MEDIUM, radius::LARGE];
        for pair in radii.windows(2) {
            assert!(pair[1] > pair[0], "radii must increase too");
        }
    }

    #[test]
    fn every_step_is_a_whole_number_of_pixels() {
        // A half-pixel gap is rendered as either zero or one depending on where
        // it lands, which is how a scale stops being one.
        for step in steps() {
            assert_eq!(step.fract(), 0.0, "{step} is not a whole pixel");
        }
    }

    /// The offending fragments in one file, as `(line number, line)`.
    ///
    /// A line is an offender when it sets one of the governed properties *and*
    /// writes a digit while doing so, without naming the scale the digit should
    /// have come from. Lines with no number at all — `radius:
    /// iced::Border::default()` — are left alone, because they are not choosing
    /// a value.
    fn offenders(source: &str, properties: &[&str]) -> Vec<(usize, String)> {
        source
            .lines()
            .enumerate()
            .filter(|(_, line)| {
                properties.iter().any(|property| {
                    let Some(at) = line.find(property) else {
                        return false;
                    };
                    let rest = &line[at + property.len()..];
                    rest.bytes().any(|byte| byte.is_ascii_digit()) && !line.contains("metrics::")
                })
            })
            .map(|(number, line)| (number + 1, line.trim().to_owned()))
            .collect()
    }

    /// Asserts every file in `files` keeps off raw numbers for `properties`.
    fn guard(files: &[&str], properties: &[&str], scale: &str) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        for file in files {
            let source = std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| {
                panic!("{file} is listed as migrated but could not be read: {e}")
            });

            let offenders = offenders(&source, properties);
            assert!(
                offenders.is_empty(),
                "{file} is on the {scale} scale, so these lines may not choose their own numbers: {offenders:#?}"
            );
        }
    }

    #[test]
    fn migrated_files_carry_no_raw_layout_numbers() {
        // The mechanism that makes the rest of this migration testable at all.
        // Spacing has no other safety net: change a padding anywhere and every
        // other test in the crate still passes.
        guard(ON_THE_LAYOUT_SCALE, LAYOUT_PROPERTIES, "layout");
    }

    #[test]
    fn migrated_files_carry_no_raw_type_sizes() {
        guard(ON_THE_TYPE_SCALE, TYPE_PROPERTIES, "type");
    }

    #[test]
    fn the_guard_can_tell_a_raw_number_from_a_named_one() {
        // The guard is the only thing standing behind every later step of this
        // migration, so it gets a test of its own rather than being trusted
        // because it happens to pass over the files it is pointed at.
        let layout = |line: &str| offenders(line, LAYOUT_PROPERTIES);
        let sizes = |line: &str| offenders(line, TYPE_PROPERTIES);

        assert_eq!(layout("    .padding([3, 12])").len(), 1);
        assert_eq!(layout("    .spacing(6)").len(), 1);
        assert_eq!(layout("        radius: 3.0.into(),").len(), 1);
        assert_eq!(sizes("        .size(13.0)").len(), 1);

        assert!(layout("    .padding(metrics::SNUG)").is_empty());
        assert!(sizes("        .size(metrics::text::BODY)").is_empty());
        assert!(
            layout("        radius: metrics::radius::SMALL.into(),").is_empty(),
            "a named radius is the point of the scale"
        );
        assert!(
            layout("        radius: iced::Border::default().radius,").is_empty(),
            "a line that chooses no value is not choosing a wrong one"
        );
        assert!(
            layout("    .width(Length::Fixed(TAB_WIDTH))").is_empty(),
            "a component's own width is not on the scale"
        );
        assert!(
            sizes("    .padding([3, 12])").is_empty(),
            "the two scales are guarded separately, so one must not see the other's numbers"
        );
    }
}
