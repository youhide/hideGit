# hideGit

A cross-platform desktop Git client written in Rust, with pull request alerts built in.

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](./LICENSE)
[![Status: pre-alpha](https://img.shields.io/badge/status-pre--alpha-orange.svg)](./docs/ROADMAP.md)

> **Pre-alpha.** There is no application to run yet. The repository currently holds the
> architecture and roadmap that the implementation will follow. See [ROADMAP](./docs/ROADMAP.md)
> for what ships when, and [ARCHITECTURE](./docs/ARCHITECTURE.md) for how it is built.

---

## What it does

hideGit visualises a repository's history and lets you work in it — stage, commit, branch, push,
resolve conflicts — and tells you when something needs your attention on a pull request.

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
| ⬜ | Commit graph, commit details, file tree, diff viewer | [M1](./docs/ROADMAP.md#m1--scaffold--read-only-viewer) |
| ⬜ | Stage / unstage by file and by hunk, commit, amend | [M2](./docs/ROADMAP.md#m2--working-directory) |
| ⬜ | Branches, tags, stash, fetch / pull / push, remotes | [M3](./docs/ROADMAP.md#m3--branches--remotes) |
| ⬜ | GitHub PR alerts + native notifications | [M4](./docs/ROADMAP.md#m4--pull-request-alerts) |
| ⬜ | Merge, rebase, cherry-pick, revert, conflict resolution UI | [M5](./docs/ROADMAP.md#m5--history-operations) |
| ⬜ | Themes, keyboard navigation, multi-repo tabs, installers | [M6](./docs/ROADMAP.md#m6--polish--release) |
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
| Rust | 1.85 or newer (to build from source) |
| Platforms | macOS 11+, Windows 10+, Linux with Vulkan / OpenGL 3.3 |

## Installing

Pre-built installers (`.dmg`, `.msi`, AppImage, Flatpak) arrive at
[M6](./docs/ROADMAP.md#m6--polish--release). Until then, build from source.

## Building from source

```sh
git clone https://github.com/<owner>/hideGit
cd hideGit
cargo run --release
```

> The Cargo workspace does not exist yet — it is the first deliverable of
> [M1](./docs/ROADMAP.md#m1--scaffold--read-only-viewer). These commands will work once it lands.

## Built with

| | |
|---|---|
| [iced](https://iced.rs) `0.14` | GUI toolkit — the Elm architecture, GPU-rendered |
| [gitoxide (`gix`)](https://github.com/GitoxideLabs/gitoxide) `0.86` | Git read operations, in pure Rust |
| system `git` | Push, merge, rebase — see [ADR-0002](./docs/adr/0002-git-backend-hybrid.md) |
| [octocrab](https://github.com/XAMPPRocky/octocrab) `0.54` | GitHub API client, for PR alerts |

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
