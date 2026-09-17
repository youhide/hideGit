//! The native menu bar.
//!
//! macOS puts a menu bar on screen whether an application asks for one or not.
//! hideGit never asked, so what was up there was winit's fallback: an
//! application menu with About, Services, Hide and Quit, every one of them
//! naming the *executable* — "About hidegit", lowercase — and nothing else at
//! all. No File, no Edit, so no Copy or Paste in a menu, and no way to discover
//! that `Cmd+O` opens a repository except by already knowing.
//!
//! # The menu is built from the command palette's table
//!
//! [`COMMANDS`] already holds every action with a name: its id, its title, the
//! chord that runs it, and the message it sends. The palette reads it, the
//! shortcut reference is checked against it, `[shortcuts]` in `config.toml`
//! remaps by its ids — and now the menu bar is assembled from it too.
//!
//! That is the whole reason this file is as short as it is, and the reason
//! nothing here can drift: a menu entry is an `id` from that table, so a menu
//! item whose action, title or shortcut disagreed with the palette's would have
//! to disagree with itself. A test holds that every id named below exists.
//!
//! # What is not here
//!
//! **Windows and Linux.** `muda` draws menus on both, but attaching one needs a
//! native window handle that iced 0.14 does not hand out, and on Linux it needs
//! a GTK window hideGit does not have — it is winit and wgpu all the way down.
//! Neither platform *expects* an application to own the screen's menu bar the
//! way macOS does, so the cost of going without is much lower there. See
//! [ADR-0009](../../../docs/adr/0009-native-menu-bar.md).
//!
//! The table below and the three functions that read it are portable, and their
//! tests run on every platform CI covers — an id that stopped naming a command
//! is worth catching on all three. Only macOS builds a menu out of them, so
//! there they are dead code by construction rather than by mistake, which is
//! what the `allow` says.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use hidegit_ui::Message;
use hidegit_ui::widget::palette::COMMANDS;

/// One line of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// A command from the palette's table, named by the id it answers to in
    /// `config.toml`.
    Command(&'static str),
    /// A label and the page it opens in the browser.
    Url(&'static str, &'static str),
    /// A rule between two groups of entries.
    Separator,
    /// Something the platform draws and handles itself — Copy, Quit, Minimise.
    ///
    /// These are deliberately *not* wired to hideGit messages. Cut and Paste
    /// belong to the operating system's editing machinery, and reimplementing
    /// them to route through `update` would get their behaviour subtly wrong in
    /// every text field at once.
    Standard(Standard),
}

/// A menu item the platform owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum Standard {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    CloseWindow,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Minimize,
    Zoom,
    BringAllToFront,
}

/// A top-level menu.
#[derive(Debug, Clone, Copy)]
pub struct Menu {
    /// What the menu bar shows. The first menu's title is replaced by the
    /// application's own name on macOS, which is why this one says `hideGit`
    /// and is not expected to be read from here.
    pub title: &'static str,
    pub entries: &'static [Entry],
}

/// The menu bar, as data.
///
/// Ordered the way macOS orders a menu bar, because a menu that puts its own
/// ideas where Edit belongs is worse than no menu: the first submenu is the
/// application menu, Edit is second-from-left after File, and Window and Help
/// are last.
pub const BAR: &[Menu] = &[
    Menu {
        title: "hideGit",
        entries: &[
            Entry::Standard(Standard::About),
            Entry::Separator,
            Entry::Command("settings"),
            Entry::Separator,
            Entry::Standard(Standard::Services),
            Entry::Separator,
            Entry::Standard(Standard::Hide),
            Entry::Standard(Standard::HideOthers),
            Entry::Standard(Standard::ShowAll),
            Entry::Separator,
            Entry::Standard(Standard::Quit),
        ],
    },
    Menu {
        title: "File",
        entries: &[
            Entry::Command("open"),
            Entry::Command("clone"),
            Entry::Separator,
            Entry::Standard(Standard::CloseWindow),
        ],
    },
    Menu {
        title: "Edit",
        entries: &[
            Entry::Standard(Standard::Undo),
            Entry::Standard(Standard::Redo),
            Entry::Separator,
            Entry::Standard(Standard::Cut),
            Entry::Standard(Standard::Copy),
            Entry::Standard(Standard::Paste),
            Entry::Standard(Standard::SelectAll),
        ],
    },
    Menu {
        title: "View",
        entries: &[
            Entry::Command("search"),
            Entry::Command("diff-mode"),
            Entry::Separator,
            Entry::Command("shortcuts"),
        ],
    },
    Menu {
        title: "Repository",
        entries: &[
            Entry::Command("fetch"),
            Entry::Command("pull"),
            Entry::Command("push"),
            Entry::Separator,
            Entry::Command("commit"),
            Entry::Command("commit-and-push"),
            Entry::Command("discard"),
            Entry::Separator,
            Entry::Command("continue"),
        ],
    },
    Menu {
        title: "Pull Requests",
        entries: &[
            Entry::Command("connect"),
            Entry::Command("refresh-pull-requests"),
        ],
    },
    Menu {
        title: "Window",
        entries: &[
            Entry::Standard(Standard::Minimize),
            Entry::Standard(Standard::Zoom),
            Entry::Separator,
            Entry::Standard(Standard::BringAllToFront),
        ],
    },
    Menu {
        title: "Help",
        entries: &[Entry::Url(
            "hideGit on GitHub",
            "https://github.com/youhide/hideGit",
        )],
    },
];

/// What choosing this entry does, given which repository is in front.
///
/// `None` means the entry has nothing to do right now — every repository
/// command is `None` with no repository open — and that is also what decides
/// whether it is greyed out. One rule, applied to both, so an item can never be
/// enabled and inert.
pub fn message(id: &str, active: Option<usize>) -> Option<Message> {
    if let Some(url) = id.strip_prefix("url:") {
        return Some(Message::OpenUrl(url.to_owned()));
    }

    COMMANDS
        .iter()
        .find(|command| command.id == id)
        .and_then(|command| (command.message)(active))
}

/// Whether the entry can do anything at all right now.
pub fn enabled(id: &str, active: Option<usize>) -> bool {
    message(id, active).is_some()
}

/// The title and shortcut an entry shows, with the user's own remapping in
/// front of the built-in chord — the same order the shortcut reference uses.
///
/// A menu accelerator is taken by the operating system *before* the window sees
/// the key, so a menu that advertised the default chord of a command somebody
/// had moved would not merely be wrong on screen: it would shadow their
/// remapping with the binding they had replaced.
pub fn label(
    id: &str,
    keymap: &hidegit_ui::keymap::Keymap,
) -> Option<(&'static str, Option<String>)> {
    let command = COMMANDS.iter().find(|command| command.id == id)?;
    let chord = keymap
        .chord_for(command)
        .map(str::to_owned)
        .or_else(|| command.chord.map(str::to_owned));

    Some((command.title, chord))
}

#[cfg(target_os = "macos")]
pub use platform::Bar;

#[cfg(target_os = "macos")]
mod platform {
    use muda::{AboutMetadata, MenuItem, PredefinedMenuItem, Submenu, accelerator::Accelerator};

    use super::{BAR, Entry, Standard};

    /// The menu bar, once it is on screen.
    ///
    /// Holds its own items so their enabled state can be kept honest: what a
    /// command can do changes as repositories open and close, and a menu that
    /// learned the answer once would offer Push with nothing open.
    pub struct Bar {
        /// Kept because dropping a `muda::Menu` takes the menu off the screen
        /// with it.
        _menu: muda::Menu,
        items: Vec<(&'static str, MenuItem)>,
    }

    /// Hand-written because none of muda's types implement `Debug`, and the
    /// workspace warns on a public type that does not.
    impl std::fmt::Debug for Bar {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Bar")
                .field(
                    "items",
                    &self.items.iter().map(|(id, _)| id).collect::<Vec<_>>(),
                )
                .finish()
        }
    }

    impl Bar {
        /// Builds the bar and hands it to the application.
        ///
        /// **Must run on the main thread, after the window exists.** A menu is
        /// attached to `NSApp`, and `NSApp` is created by the event loop — this
        /// is called from `update`, which iced runs on the main thread, in
        /// answer to the window having opened.
        pub fn install(
            keymap: &hidegit_ui::keymap::Keymap,
            active: Option<usize>,
        ) -> Result<Self, muda::Error> {
            let menu = muda::Menu::new();
            let mut items = Vec::new();

            for section in BAR {
                let submenu = Submenu::new(section.title, true);

                for entry in section.entries {
                    match entry {
                        Entry::Separator => {
                            submenu.append(&PredefinedMenuItem::separator())?;
                        }
                        Entry::Standard(standard) => {
                            submenu.append(&predefined(*standard))?;
                        }
                        Entry::Url(label, url) => {
                            let item = MenuItem::with_id(format!("url:{url}"), *label, true, None);
                            submenu.append(&item)?;
                            items.push((*label, item));
                        }
                        Entry::Command(id) => {
                            let Some((title, chord)) = super::label(id, keymap) else {
                                continue;
                            };
                            // A chord that will not parse costs the shortcut,
                            // not the item: the command is still reachable from
                            // the menu, and the log says which one was dropped.
                            let accelerator = chord.and_then(|chord| {
                                chord.parse::<Accelerator>().ok().or_else(|| {
                                    tracing::warn!(%chord, id, "no menu accelerator for this chord");
                                    None
                                })
                            });

                            let item = MenuItem::with_id(
                                *id,
                                title,
                                super::enabled(id, active),
                                accelerator,
                            );
                            submenu.append(&item)?;
                            items.push((id, item));
                        }
                    }
                }

                menu.append(&submenu)?;
            }

            menu.init_for_nsapp();

            Ok(Self { _menu: menu, items })
        }

        /// Greys out what cannot be done right now.
        ///
        /// Called after every message rather than when something specific
        /// changes: it is a handful of boolean writes against items already in
        /// memory, and the alternative is a list of "what might have changed
        /// this" that goes stale the first time a command is added.
        pub fn sync(&self, active: Option<usize>) {
            for (id, item) in &self.items {
                let enabled = super::enabled(id, active);
                if item.is_enabled() != enabled {
                    item.set_enabled(enabled);
                }
            }
        }
    }

    fn predefined(standard: Standard) -> PredefinedMenuItem {
        match standard {
            Standard::About => PredefinedMenuItem::about(
                Some("About hideGit"),
                Some(AboutMetadata {
                    name: Some("hideGit".to_owned()),
                    version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                    copyright: Some("GPL-3.0-only © hideGit contributors".to_owned()),
                    website: Some("https://github.com/youhide/hideGit".to_owned()),
                    ..AboutMetadata::default()
                }),
            ),
            // `None` takes the platform's own wording, which is already
            // translated and already says the right thing.
            Standard::Services => PredefinedMenuItem::services(None),
            Standard::Hide => PredefinedMenuItem::hide(Some("Hide hideGit")),
            Standard::HideOthers => PredefinedMenuItem::hide_others(None),
            Standard::ShowAll => PredefinedMenuItem::show_all(None),
            Standard::Quit => PredefinedMenuItem::quit(Some("Quit hideGit")),
            Standard::CloseWindow => PredefinedMenuItem::close_window(None),
            Standard::Undo => PredefinedMenuItem::undo(None),
            Standard::Redo => PredefinedMenuItem::redo(None),
            Standard::Cut => PredefinedMenuItem::cut(None),
            Standard::Copy => PredefinedMenuItem::copy(None),
            Standard::Paste => PredefinedMenuItem::paste(None),
            Standard::SelectAll => PredefinedMenuItem::select_all(None),
            Standard::Minimize => PredefinedMenuItem::minimize(None),
            Standard::Zoom => PredefinedMenuItem::zoom(None),
            Standard::BringAllToFront => PredefinedMenuItem::bring_all_to_front(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_ids() -> Vec<&'static str> {
        BAR.iter()
            .flat_map(|menu| menu.entries)
            .filter_map(|entry| match entry {
                Entry::Command(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_menu_entry_names_a_command_that_exists() {
        // The menu holds ids rather than actions, so a command renamed in the
        // palette would leave a menu item that silently does nothing.
        for id in command_ids() {
            assert!(
                COMMANDS.iter().any(|command| command.id == id),
                "the menu names “{id}”, which is not a command"
            );
        }
    }

    #[test]
    fn no_command_is_in_two_menus() {
        // Two items for one action is two places to keep right, and the second
        // is the one nobody updates.
        let ids = command_ids();
        for id in &ids {
            assert_eq!(
                ids.iter().filter(|other| *other == id).count(),
                1,
                "“{id}” appears in the menu bar twice"
            );
        }
    }

    #[test]
    fn a_repository_command_is_dead_until_a_repository_is_open() {
        // What greys the item out. `open` needs nothing; `push` needs somewhere
        // to push from.
        assert!(enabled("open", None));
        assert!(!enabled("push", None));
        assert!(enabled("push", Some(0)));
    }

    #[test]
    fn the_help_entry_opens_the_page_it_names() {
        let Some(Message::OpenUrl(url)) = message("url:https://github.com/youhide/hideGit", None)
        else {
            panic!("the help entry did not open a page");
        };

        assert_eq!(url, "https://github.com/youhide/hideGit");
    }

    #[test]
    fn an_entry_shows_the_chord_the_user_remapped_it_to() {
        // A menu accelerator is taken before the window sees the key, so
        // advertising the default chord of a moved command would shadow the
        // remapping with the binding it replaced.
        let keymap = hidegit_ui::keymap::Keymap::parse([("push", "Cmd+U")]);

        let (title, chord) = label("push", &keymap).expect("push is a command");

        assert_eq!(title, "Push");
        assert_eq!(chord.as_deref(), Some("Cmd+U"));
    }

    #[test]
    fn an_entry_falls_back_to_the_chord_it_ships_with() {
        let (_, chord) = label("push", &hidegit_ui::keymap::Keymap::default()).unwrap();

        assert_eq!(chord.as_deref(), Some("Cmd+Shift+U"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn every_chord_in_the_menu_is_one_the_platform_can_take() {
        // `Cmd+,`, `Cmd+/`, `Cmd+Enter` and `Cmd+Backspace` are all in the
        // table, and all four are places a parser gives up. A chord that does
        // not parse costs the shortcut and keeps the item, which is the right
        // failure — but it should not be happening to anything that ships.
        let keymap = hidegit_ui::keymap::Keymap::default();

        for id in command_ids() {
            let Some((_, Some(chord))) = label(id, &keymap) else {
                continue;
            };

            assert!(
                chord.parse::<muda::accelerator::Accelerator>().is_ok(),
                "“{chord}”, the shortcut for “{id}”, is not one a menu can show"
            );
        }
    }
}
