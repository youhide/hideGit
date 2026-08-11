<p align="center">
  <img src="./assets/generated/icon-512.png" alt="" width="128" height="128">
</p>

# hideGit

A cross-platform desktop Git client written in Rust, with pull request alerts built in.

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](./LICENSE)
[![Status: pre-alpha](https://img.shields.io/badge/status-pre--alpha-orange.svg)](./docs/ROADMAP.md)

> **Pre-alpha, and now a daily driver for ordinary work.** You can clone a repository or open one,
> scroll its history, read any commit's diff, stage by file, by hunk or by individual line, commit,
> branch, stash, and fetch, pull and push — with progress you can watch and cancel. Sign in to
> GitHub and open pull requests appear in the sidebar with their review and CI state, with desktop
> notifications when one of them changes. History rewriting works too: merge, rebase, cherry-pick,
> revert and reset, with a three-pane conflict resolver — a rebase that conflicts on three separate
> commits can be finished without opening a terminal.
>
> Interactive rebase works too: reorder, squash, fixup, edit and drop from a plan editor where
> nothing runs until you start it.
>
> Two caveats worth knowing before you rely on it. Pushing over SSH with a passphrase, or
> over HTTPS with a credential helper, has not been verified — it should work, since hideGit hands
> those operations to your own `git`, but every remote in the test suite is a local path. And on
> macOS, notifications are attributed to whatever binary sent them, so run the bundle from
> `cargo run -p xtask -- bundle-macos` rather than `cargo run` if you want them to say hideGit.
>
> See [ROADMAP](./docs/ROADMAP.md) for what ships when, and
> [ARCHITECTURE](./docs/ARCHITECTURE.md) for how it is built.

---

## What it does

hideGit visualises a repository's history and lets you work in it — stage, commit, branch, push,
resolve conflicts — and tells you when something needs your attention on a pull request.

Where it needs to say no, it says so and shows you Git's own words. A checkout that would overwrite
your changes fails with the list of files rather than stashing them somewhere you did not ask for; a
rejected push shows the hint that tells you what to do about it; a deletion that Git refuses stays
refused rather than being retried with `--force`.

- **Commit graph as the primary view.** History is the thing you reason about, so it gets the
  centre of the window. Target: 60fps scrolling on a 100,000-commit repository.
- **Pull request alerts.** A native desktop notification when a review is requested, when CI turns
  red, or when your open PR starts conflicting.
- **Native rendering.** [iced][iced] draws on the GPU via `wgpu`. No web view.
- **Reads on [gitoxide][gix].** History traversal, status and diff run on a pure-Rust Git
  implementation, so no C toolchain is needed to build.
- **Your Git configuration is the one that applies.** Push, merge and rebase run through your
  system `git`, so credential helpers, hooks, submodules and LFS behave as you configured them.

## Features

| | Feature | Milestone |
|---|---|---|
| ✅ | Commit graph, commit details, file tree, diff viewer | [M1](./docs/ROADMAP.md#m1--scaffold--read-only-viewer) |
| ✅ | Stage / unstage by file, hunk and line, discard, commit, amend | [M2](./docs/ROADMAP.md#m2--working-directory) |
| ✅ | Clone, branches, tags, stash, remotes, fetch / pull / push with progress | [M3](./docs/ROADMAP.md#m3--branches--remotes) |
| ✅ | GitHub PR alerts + native notifications | [M4](./docs/ROADMAP.md#m4--pull-request-alerts) |
| ✅ | Merge, rebase, cherry-pick, revert, reset, conflict resolution UI | [M5](./docs/ROADMAP.md#m5--history-operations) |
| 🟡 | Interactive rebase editor ✅, drag-and-drop, themes, keyboard navigation, installers | [M6](./docs/ROADMAP.md#m6--polish--release) |
| ⬜ | GitLab & Bitbucket, submodules, worktrees, LFS, PT-BR translation | [Post-1.0](./docs/ROADMAP.md#post-10) |

Legend: ⬜ planned · 🟡 in progress · ✅ shipped

## Requirements

**Git must be installed and on your `PATH`.**

This is a deliberate architectural choice, not an oversight. hideGit reads repositories with
gitoxide, but delegates operations that write to a remote or rewrite history — `push`, `merge`,
`rebase` — to your system `git`. gitoxide does not implement `push` yet, and delegating means
hideGit inherits your credential helpers, hooks, submodules, Git LFS and SSH agent configuration
exactly as you have them configured, rather than reimplementing each one imperfectly.

The full reasoning, including how this changes as gitoxide matures, is in
[ADR-0002](./docs/adr/0002-git-backend-hybrid.md).

| | Minimum |
|---|---|
| Git | 2.30 or newer, on `PATH` |
| Rust | 1.88 or newer (to build from source) |
| Platforms | macOS 11+, Windows 10+, Linux with Vulkan / OpenGL 3.3 |

## Installing

Pre-built installers (`.dmg`, `.msi`, AppImage, Flatpak) arrive at
[M6](./docs/ROADMAP.md#m6--polish--release). Until then, build from source — and if you want hideGit
to behave like an installed application rather than a binary in `target/`, see
[Installing locally](#installing-locally) below.

## Building from source

```sh
git clone https://github.com/youhide/hideGit
cd hideGit
cargo run --release                      # opens the welcome screen
cargo run --release -- /path/to/a/repo   # opens a repository straight away
```

No C toolchain is needed — a consequence of choosing gitoxide over libgit2. On Linux you will also
need the usual windowing and font development packages (`libxkbcommon-dev`, `libwayland-dev`,
`libfontconfig1-dev`, or your distribution's equivalents).

### Installing locally

Neither of these is signed, and neither is a package. They exist so that hideGit gets a real
application icon and launcher entry while the actual installers are still ahead at
[M6](./docs/ROADMAP.md#m6--polish--release).

**macOS.** A window icon does nothing here — macOS has no per-window icons and reads the Dock icon
from an application bundle — so there is a task that builds one:

```sh
cargo build --release
cargo run -p xtask -- bundle-macos       # target/release/bundle/hideGit.app
```

Being unsigned, Gatekeeper will want a right-click → Open the first time.

**Linux.** Installs the binary, a `.desktop` entry and the hicolor icon set. `PREFIX` defaults to
`/usr/local`; point it at `~/.local` to avoid needing root, and pass `uninstall` to reverse it:

```sh
cargo build --release
PREFIX=~/.local ./packaging/linux/install.sh
```

The `.desktop` entry matters more than it sounds: Wayland ignores window icons and identifies an
application by matching its `app_id` against an installed entry, so on Wayland this is what puts the
icon in the overview and the dock.

**Windows.** The icon is linked into `hidegit.exe` at build time, so `cargo build --release` is
enough for Explorer, the taskbar and Alt-Tab to show it. There is nothing else to install.

## Built with

| | |
|---|---|
| [iced](https://iced.rs) `0.14` | GUI toolkit — the Elm architecture, GPU-rendered |
| [gitoxide (`gix`)](https://github.com/GitoxideLabs/gitoxide) `0.86` | Git read operations, in pure Rust |
| system `git` | Push, merge, rebase — see [ADR-0002](./docs/adr/0002-git-backend-hybrid.md) |
| [octocrab](https://github.com/XAMPPRocky/octocrab) `0.54` | GitHub API client, and the device flow ([M4](./docs/ROADMAP.md#m4--pull-request-alerts)) |
| [keyring](https://github.com/open-source-cooperative/keyring-rs) `4` | Tokens in the OS keychain — never in a file, never in a log |
| [notify-rust](https://github.com/hoodie/notify-rust) `4` | Native desktop notifications |

## Documentation

| Document | What it covers |
|---|---|
| [ARCHITECTURE](./docs/ARCHITECTURE.md) | Crate layout, the `GitBackend` seam, forge integration, concurrency, known limits |
| [ROADMAP](./docs/ROADMAP.md) | Milestones M0–M6 with acceptance criteria |
| [UI_SPEC](./docs/UI_SPEC.md) | Screens, state and message shapes, keyboard shortcuts, theming |
| [COMMIT_GRAPH](./docs/COMMIT_GRAPH.md) | Lane assignment algorithm and rendering |
| [ADRs](./docs/adr/README.md) | Why each major technical decision was made |

## Contributing

Contributions are welcome — the project is early enough that foundational decisions are still
open to argument. Start with [CONTRIBUTING.md](./CONTRIBUTING.md), and read
[the ADRs](./docs/adr/README.md) before proposing a change to an architectural choice.

By participating you agree to the [Code of Conduct](./CODE_OF_CONDUCT.md).
Security issues: please follow [SECURITY.md](./SECURITY.md) rather than opening a public issue.

## License

[GPL-3.0](./LICENSE) © hideGit contributors.

[gix]: https://github.com/GitoxideLabs/gitoxide
[iced]: https://iced.rs
