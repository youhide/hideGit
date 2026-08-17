//! User-remapped shortcuts.
//!
//! A layer in front of the built-in bindings rather than a replacement for
//! them. `config.toml` maps a command to a chord:
//!
//! ```toml
//! [shortcuts]
//! push = "Cmd+U"
//! refresh-pull-requests = "Cmd+R"
//! ```
//!
//! The commands are the ones the command palette lists, by `id`. That is a
//! deliberate boundary rather than an accident: those are the actions with a
//! name, and rebinding them cannot break the interface. Navigation — the
//! arrows, `Tab`, `J`/`K`, `Space`, the chord prefix and the keys a panel owns
//! while it is up — stays fixed, because those are how you get *out* of things,
//! and a config file that can strand you inside a panel is a config file that
//! can lock you out of the application.
//!
//! Rebinding a command **moves** it: its default chord stops working, or the
//! two would both fire and the remap would look ignored.

use std::collections::BTreeSet;
use std::sync::Arc;

use iced::keyboard::{Key, Modifiers};

use crate::message::Message;
use crate::widget::palette::{COMMANDS, Command};

/// A line of `[shortcuts]` that could not be used, and why.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Problem {
    /// The key as written in the file.
    pub action: String,
    pub reason: String,
}

/// One binding the user asked for.
#[derive(Debug, Clone)]
struct Bound {
    command: &'static Command,
    /// As written, and the only thing hashed: two keymaps are the same keymap
    /// when the file said the same thing.
    chord: String,
    key: Key,
    modifiers: Modifiers,
}

/// What `[shortcuts]` in `config.toml` asked for.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bound: Vec<Bound>,
    /// Commands whose default chord must stop working, because the user moved
    /// them somewhere else.
    moved: BTreeSet<&'static str>,
    pub problems: Vec<Problem>,
}

impl std::hash::Hash for Keymap {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // The identity of a keymap is what the file said. Hashing the parsed
        // keys as well would be the same information twice, and `Modifiers` is
        // not hashable anyway.
        for binding in &self.bound {
            binding.command.id.hash(state);
            binding.chord.hash(state);
        }
    }
}

impl Keymap {
    /// Reads the `action = "chord"` pairs, keeping the ones that make sense and
    /// reporting the ones that do not.
    ///
    /// Nothing here refuses to start. A typo in a shortcut is worth saying out
    /// loud and worth ignoring; it is not worth a window that does not open.
    pub fn parse<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut map = Self::default();
        let built_in = crate::widget::shortcuts::built_in_chords();

        for (action, chord) in entries {
            let Some(command) = COMMANDS.iter().find(|c| c.id == action) else {
                map.problems.push(Problem {
                    action: action.to_owned(),
                    reason: format!(
                        "there is no command called “{action}”. The names are the ones \
                         in the command palette."
                    ),
                });
                continue;
            };

            let Some((key, modifiers)) = crate::widget::shortcuts::parse(chord) else {
                map.problems.push(Problem {
                    action: action.to_owned(),
                    reason: format!(
                        "“{chord}” is not a chord. They are written like \
                         \"Cmd+Shift+U\" — Cmd, Ctrl and Shift, then one key."
                    ),
                });
                continue;
            };

            if let Some(taken) = map
                .bound
                .iter()
                .find(|b| b.key == key && b.modifiers == modifiers)
            {
                map.problems.push(Problem {
                    action: action.to_owned(),
                    reason: format!("“{chord}” is already bound to {}.", taken.command.id),
                });
                continue;
            }

            // Honoured, and said out loud. An explicit line in a config file
            // beats a default — but taking `J` for Push silently would leave
            // hunk navigation gone with nothing to connect it to.
            if built_in.contains(&chord) && command.chord != Some(chord) {
                map.problems.push(Problem {
                    action: action.to_owned(),
                    reason: format!(
                        "“{chord}” is also a built-in binding, which this now replaces."
                    ),
                });
            }

            if command.chord.is_some_and(|default| default != chord) {
                map.moved.insert(command.id);
            }

            map.bound.push(Bound {
                command,
                chord: chord.to_owned(),
                key,
                modifiers,
            });
        }

        map
    }

    /// The message this press means, if the user bound it.
    ///
    /// Checked before the built-in bindings, so an explicit line in the file
    /// wins over a default.
    pub fn resolve(
        &self,
        key: &Key,
        modifiers: Modifiers,
        active: Option<usize>,
    ) -> Option<Message> {
        self.bound
            .iter()
            .find(|b| b.key == *key && b.modifiers == modifiers)
            .and_then(|b| (b.command.message)(active))
    }

    /// Whether this press is the default chord of a command the user moved, and
    /// should therefore no longer do anything.
    pub fn moved_away(&self, key: &Key, modifiers: Modifiers) -> bool {
        self.moved.iter().any(|id| {
            COMMANDS
                .iter()
                .find(|c| c.id == *id)
                .and_then(|c| c.chord)
                .and_then(crate::widget::shortcuts::parse)
                .is_some_and(|(k, m)| k == *key && m == modifiers)
        })
    }

    /// The chord a command answers to now, for the reference and the palette.
    pub fn chord_for(&self, command: &Command) -> Option<&str> {
        self.bound
            .iter()
            .find(|b| b.command.id == command.id)
            .map(|b| b.chord.as_str())
            .or(command.chord)
    }

    /// What to print where a built-in chord is shown, given the user may have
    /// moved the command that answers to it.
    ///
    /// A reference that still prints `Cmd+Shift+U` after Push was rebound is
    /// the drift the reference exists to prevent, one config file later.
    pub fn shown_instead_of(&self, default: &str) -> Option<&str> {
        let command = COMMANDS.iter().find(|c| c.chord == Some(default))?;
        self.bound
            .iter()
            .find(|b| b.command.id == command.id && b.chord != default)
            .map(|b| b.chord.as_str())
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::key::{Key, Named};

    fn press(chord: &str) -> (Key, Modifiers) {
        crate::widget::shortcuts::parse(chord).expect("a chord the tests wrote")
    }

    #[test]
    fn a_remapped_command_answers_to_its_new_chord() {
        let map = Keymap::parse([("push", "Cmd+U")]);
        let (key, modifiers) = press("Cmd+U");

        assert!(
            matches!(
                map.resolve(&key, modifiers, Some(0)),
                Some(Message::Repo(
                    0,
                    crate::message::RepoMessage::PushRequested { .. }
                ))
            ),
            "the new chord pushes"
        );
        assert!(map.problems.is_empty(), "{:?}", map.problems);
    }

    #[test]
    fn a_remapped_command_stops_answering_to_the_old_one() {
        // Both firing would make the remap look ignored: the old chord still
        // works, so nothing appears to have changed.
        let map = Keymap::parse([("push", "Cmd+U")]);
        let (key, modifiers) = press("Cmd+Shift+U");

        assert!(map.moved_away(&key, modifiers));
        assert!(map.resolve(&key, modifiers, Some(0)).is_none());
    }

    #[test]
    fn a_command_bound_to_the_chord_it_already_had_moves_nothing() {
        let map = Keymap::parse([("push", "Cmd+Shift+U")]);
        let (key, modifiers) = press("Cmd+Shift+U");

        assert!(!map.moved_away(&key, modifiers), "it did not move");
        assert!(map.resolve(&key, modifiers, Some(0)).is_some());
    }

    #[test]
    fn a_command_with_no_chord_of_its_own_can_be_given_one() {
        // Refreshing pull requests ships without a binding. Being able to bind
        // it is half the point of the file.
        let map = Keymap::parse([("refresh-pull-requests", "Cmd+R")]);
        let (key, modifiers) = press("Cmd+R");

        assert!(matches!(
            map.resolve(&key, modifiers, Some(0)),
            Some(Message::Repo(
                0,
                crate::message::RepoMessage::PrsRefreshRequested
            ))
        ));
        assert!(map.problems.is_empty(), "{:?}", map.problems);
    }

    #[test]
    fn a_name_that_is_not_a_command_is_reported_rather_than_ignored() {
        // Silently dropping it is the failure the settings screen exists to
        // stop: the file says one thing, the application does another, and
        // nothing connects the two.
        let map = Keymap::parse([("shove", "Cmd+U")]);

        assert_eq!(map.problems.len(), 1);
        assert_eq!(map.problems[0].action, "shove");
        assert!(
            map.problems[0].reason.contains("no command"),
            "{:?}",
            map.problems[0]
        );
    }

    #[test]
    fn a_chord_that_is_not_one_is_reported() {
        for bad in ["Meta+U", "", "Cmd+Whatever"] {
            let map = Keymap::parse([("push", bad)]);
            assert_eq!(map.problems.len(), 1, "“{bad}” was accepted");
        }
    }

    #[test]
    fn two_commands_cannot_take_the_same_chord() {
        // First wins, and the second is told why it did not. Silently letting
        // the later line win would make the order of a TOML table load-bearing.
        let map = Keymap::parse([("push", "Cmd+U"), ("pull", "Cmd+U")]);
        let (key, modifiers) = press("Cmd+U");

        assert!(matches!(
            map.resolve(&key, modifiers, Some(0)),
            Some(Message::Repo(
                0,
                crate::message::RepoMessage::PushRequested { .. }
            ))
        ));
        assert_eq!(map.problems.len(), 1);
        assert_eq!(map.problems[0].action, "pull");

        // And the line that lost changes nothing at all: Pull still answers to
        // the chord it shipped with. A rejected remap that still moved the
        // command would take the binding away and give nothing back.
        let (key, modifiers) = press("Cmd+Shift+P");
        assert!(!map.moved_away(&key, modifiers), "Pull kept its own chord");
    }

    #[test]
    fn taking_a_chord_that_is_already_a_built_in_binding_is_honoured_and_said() {
        // An explicit line beats a default. Taking `J` silently would leave
        // hunk navigation gone with nothing to connect it to.
        let map = Keymap::parse([("push", "J")]);
        let (key, modifiers) = press("J");

        assert!(
            map.resolve(&key, modifiers, Some(0)).is_some(),
            "the file wins"
        );
        assert_eq!(map.problems.len(), 1, "and is told what it replaced");
    }

    #[test]
    fn a_binding_still_needs_something_to_act_on() {
        let map = Keymap::parse([("push", "Cmd+U")]);
        let (key, modifiers) = press("Cmd+U");

        assert!(map.resolve(&key, modifiers, None).is_none());
    }

    #[test]
    fn a_chord_parses_case_insensitively_and_names_the_keys_it_knows() {
        assert_eq!(
            crate::widget::shortcuts::parse("Cmd+Shift+U"),
            crate::widget::shortcuts::parse("Cmd+Shift+u")
        );
        assert_eq!(
            crate::widget::shortcuts::parse("Ctrl+O"),
            crate::widget::shortcuts::parse("Cmd+o"),
            "the file may spell it either way"
        );
        assert_eq!(
            crate::widget::shortcuts::parse("Enter"),
            Some((Key::Named(Named::Enter), Modifiers::default()))
        );
    }
}
