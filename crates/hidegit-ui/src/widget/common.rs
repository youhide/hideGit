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

use iced::widget::{Space, container};
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
    fn no_view_file_draws_its_own_rule() {
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
            for shape in ["fn divider", "fn vertical_rule", "fn horizontal_rule"] {
                if source.contains(shape) {
                    offenders.push(format!("{name} defines its own `{shape}`"));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "a rule belongs to `widget::common`: {offenders:#?}"
        );
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
