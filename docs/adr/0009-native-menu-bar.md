# 0009 — A native menu bar on macOS, built from the command palette's table

- **Status:** Accepted
- **Date:** 2026-09-17

## Context

macOS puts a menu bar on screen for every application, asked for or not. hideGit
never asked, so what was up there was winit's fallback: one menu, containing
About, Services, Hide, Hide Others, Show All and Quit — and nothing else. No
File, no Edit, therefore no Copy and no Paste in a menu, and no way to find out
that `Cmd+O` opens a repository except by already knowing it or by opening the
command palette, which is itself only reachable by a shortcut.

Every item in that fallback also named the *executable* rather than the
application: "About hidegit", lowercase, in an application whose own name has
been a deliberate decision since M1.

iced 0.14 has no menu API at all. It draws its own widgets into a wgpu surface,
and a menu bar is not a widget — on macOS it belongs to `NSApp`, which the event
loop owns.

Three constraints shaped the answer.

- **The workspace forbids `unsafe`.** `unsafe_code = "forbid"` is set at the
  workspace root, and `forbid` cannot be lifted for a module. Writing the
  `NSMenu` directly through `objc2` — which is already in the dependency tree
  underneath winit, so it would cost nothing to add — is therefore not available
  without weakening a lint that protects everything else.
- **Shortcuts are remappable.** `[shortcuts]` in `config.toml` moves a command to
  a different chord, and moving it makes the default stop working. A menu
  accelerator is taken by the operating system *before* the window sees the key,
  so a menu advertising a default chord for a command somebody had moved would
  not merely be wrong on screen: it would resurrect the binding they replaced.
- **There is already a table of every named action.** `widget::palette::COMMANDS`
  holds an id, a title, a chord and a message for each. The command palette reads
  it, the shortcut reference is tested against it, and `[shortcuts]` remaps by its
  ids.

## Decision

**The menu bar is built from `COMMANDS`, by id, through `muda`, on macOS only.**

A menu entry is one of four things: an id from that table, a URL, a separator, or
an item the platform owns (Copy, Quit, Minimise). The bar itself is a `const`
in `crates/hidegit/src/menu.rs`, which is the whole of the layout decision.

Three consequences follow from building it that way, and they are the reason for
it:

- **An item's title, action and shortcut cannot disagree with the palette's**,
  because there is only one of each. A test asserts every id in the bar exists.
- **Enabled state has one rule.** An entry is enabled exactly when its command
  yields a message for the repository in front — which is what "Push with nothing
  open" already meant. The item cannot be enabled and inert.
- **The accelerator is the user's.** It comes from `Keymap::chord_for` first and
  the built-in chord second, the same order the shortcut reference shows.

`muda` is a macOS-only dependency with `default-features = false`: its defaults
are gtk3 and libxdo, which are how it draws menus on Linux and which would put
forty packages into the lockfile for a platform this does not run on.

The menu is installed from `update`, in answer to `window::open_events()`. Both
halves of that matter: iced runs `update` on the main thread, and the window
having opened is what proves `NSApp` exists. A failure to build it is logged and
otherwise ignored — losing a menu bar costs discoverability, and refusing to
start costs everything.

Menu clicks arrive on muda's own channel, which is not a future. One thread
parks on it for the life of the process and forwards what it gets; the
alternative is a timer, and a timer that wakes thirty times a second to find
nothing is exactly what [ADR-0005](./0005-progress-and-cancellation.md) avoids
elsewhere.

## What this does not do

**Windows and Linux keep no menu bar.** muda draws one on both, but attaching it
needs a native window handle iced 0.14 does not expose, and on Linux it needs a
GTK window hideGit does not have — it is winit and wgpu the whole way down.
Neither platform expects an application to own a screen-wide menu bar the way
macOS does, so going without costs less there. This is stated in
`menu.rs`, in `UI_SPEC.md` and in the roadmap rather than left to be discovered.

**Nothing moves out of the interface and into the menu.** Every command in the
bar is reachable exactly as it was — by chord, by the command palette, or by a
control on screen. The menu is a third way to the same messages, which is what
makes it safe to have and what makes it useless to depend on.

## Alternatives considered

**Write the `NSMenu` with `objc2` directly.** No new dependency, since winit
already pulls it in, and complete control. Rejected: it needs `unsafe`, which the
workspace forbids outright, and the blast radius of that lint reaching across
four crates is worth more than a menu.

**Draw a menu bar as iced widgets, inside the window.** Portable, themeable, and
tested by the same render suite as everything else. Rejected for macOS: a
menu bar inside the window is not where a macOS user looks, and the screen's real
menu bar would still say "About hidegit" beside it. It remains the obvious answer
for Windows and Linux if either turns out to want one.

**Wait for iced to grow a menu API.** It may; 0.14 is pre-alpha's toolkit and
breaking releases are expected ([ADR-0001](./0001-gui-toolkit-iced.md)). Rejected
as a plan: the table this is built from is iced-independent, so whatever iced
ships later replaces `platform::Bar` — about a hundred lines — and nothing else.

## Consequences

- One new dependency, on one platform, behind one module.
- The menu, the palette and the shortcut reference are three views of one table,
  and a command added to that table appears in the palette and the reference
  automatically — the menu still has to be told, which is the seam a test covers.
- `Cmd+O`, `Cmd+F` and the rest are now taken by the menu on macOS before the
  window sees them. They dispatch the same messages the keyboard bindings did, so
  the behaviour is unchanged; a chord the parser cannot read loses its
  accelerator and keeps its menu item, with a warning naming it.
