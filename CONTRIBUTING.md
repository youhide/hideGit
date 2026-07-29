# Contributing to hideGit

Thanks for considering a contribution. hideGit is early — foundational decisions are still open
to argument, and a well-reasoned objection to an [ADR](./docs/adr/README.md) is as valuable as code.

By participating you agree to the [Code of Conduct](./CODE_OF_CONDUCT.md).

## Before you start

Read these first. They will answer most "why is it done this way" questions:

- [ARCHITECTURE.md](./docs/ARCHITECTURE.md) — crate boundaries, the `GitBackend` seam, concurrency model
- [ROADMAP.md](./docs/ROADMAP.md) — what is in scope right now versus deliberately deferred
- [The ADRs](./docs/adr/README.md) — the reasoning behind each major technical decision

If you want to change something an ADR decided, **open an issue proposing a superseding ADR**
before writing code. The decision may well be wrong; it just should not be reversed silently in
a pull request.

## Development environment

| Requirement | Notes |
|---|---|
| Rust 1.85+ | `rustup toolchain install stable` |
| `git` 2.30+ on `PATH` | Not optional — hideGit shells out to it. See [ADR-0002](./docs/adr/0002-git-backend-hybrid.md) |
| A GPU with Vulkan, Metal or OpenGL 3.3 | iced renders via `wgpu` |

On Linux you will also need the usual windowing and font development packages
(`libxkbcommon-dev`, `libwayland-dev`, `libfontconfig1-dev` or your distribution's equivalents).

No C toolchain is required. That is a deliberate consequence of choosing gitoxide over libgit2.

```sh
git clone https://github.com/<owner>/hideGit
cd hideGit
cargo run                       # opens the welcome screen
cargo run -- /path/to/a/repo    # opens a repository straight away
```

Set `HIDEGIT_LOG` to change what is logged — `HIDEGIT_LOG=hidegit=debug,hidegit_core=debug` logs
every `git` invocation with its full argument vector, which is usually what you want when
something behaves unexpectedly.

## Repository layout

```
crates/
  hidegit-core/    Git domain model and the GitBackend implementation.
                   No UI, no async runtime, no network. Testable headless.
  hidegit-forge/   Forge trait + GitHub implementation. Auth, PR polling.
  hidegit-ui/      iced views, widgets, theme, the commit-graph canvas.
  hidegit/         Binary. Wiring, configuration, logging.
docs/              Architecture, roadmap, feature specs, ADRs.
```

Dependencies point in one direction only: `hidegit` → `hidegit-ui` → `hidegit-core`, and
`hidegit-ui` → `hidegit-forge` → `hidegit-core`. **`hidegit-core` depends on neither iced nor
`hidegit-forge`.** A pull request that adds a UI or network dependency to `hidegit-core` will be
asked to move that code somewhere else — keeping the core free of both is what makes it testable
without a window or a token.

## Checks your pull request must pass

Run these locally before pushing; CI runs the same set on Linux, macOS and Windows.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

Clippy warnings are errors. If a lint is genuinely wrong for a piece of code, `#[allow(...)]` it
narrowly with a comment explaining why, rather than loosening the workspace configuration.

Anything touching the commit graph layout or the read backend should also be checked against the
benchmarks, which build a 100,000-commit repository and time the layout:

```sh
cargo bench -p hidegit-core
```

The numbers to beat are recorded in [COMMIT_GRAPH.md](./docs/COMMIT_GRAPH.md#performance). CI gains
a regression gate at M6; until then it is on you to look.

If you change `assets/icon.png`, regenerate everything derived from it and commit the result — the
Windows build reads the generated `.ico` at build time, so a checkout has to contain it:

```sh
cargo run -p xtask -- icons
```

See [assets/README.md](./assets/README.md) for what that produces and why each format exists.

## Testing expectations

| Layer | What is expected |
|---|---|
| `hidegit-core` | Unit tests against fixture repositories built by a test helper. Every `GitBackend` method has coverage, including the error paths. |
| `hidegit-forge` | HTTP interactions mocked. No test may reach the network or require a real token. |
| `hidegit-ui` | iced 0.14 supports headless testing — use it for state transitions and message handling. Rendering is not asserted pixel by pixel. |

Anything touching the commit graph layout needs a test. It is the component most likely to regress
subtly and the one users notice first — see [COMMIT_GRAPH.md](./docs/COMMIT_GRAPH.md).

## Working on the Git layer

Everything that reads a repository goes through `gix`. Everything that writes to a remote or
rewrites history goes through the system `git` binary. When adding a shell-out, follow the
invariants in [ARCHITECTURE.md](./docs/ARCHITECTURE.md#shelling-out-safely) — in particular:

- Never build a command by string concatenation. Pass arguments as a vector.
- Set `GIT_TERMINAL_PROMPT=0`. A subprocess that blocks on a hidden prompt is an app that hangs.
- Parse machine-readable formats (`--porcelain=v2`, `-z`), never human output.
- On failure, surface `stderr` to the user verbatim. Do not paraphrase Git's error messages.

## Adding a new forge (GitLab, Bitbucket, Gitea…)

Implement the `Forge` trait in `hidegit-forge`.
[ARCHITECTURE.md](./docs/ARCHITECTURE.md#forge-integration) documents the trait, what a new
provider must supply, and the constraints on authentication — most importantly that **no OAuth
client secret may be embedded in the binary**, because it would not be a secret in an open source
application.

New forges are post-1.0 by default. If you want to land one earlier, open an issue first so we can
agree the trait is stable enough to build against.

## Commits and pull requests

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(core): add rename detection to diff
fix(ui): keep graph scroll position after fetch
docs(adr): supersede 0002 with gix-native push
```

Scopes match crate names (`core`, `forge`, `ui`, `app`) or `docs`, `ci`, `build`.

For pull requests:

- One logical change per PR. Split refactors from behaviour changes — it makes review possible.
- Describe what a reviewer should check, not just what you did.
- Update the relevant document in `docs/` in the same PR. Documentation that lags the code is
  worse than no documentation, because people trust it.
- Draft PRs for work in progress are welcome, especially for anything architectural.

## Licensing of contributions

hideGit is [GPL-3.0](./LICENSE). By submitting a contribution you agree it is licensed under
GPL-3.0. Do not paste code from a proprietary product or from a project under an incompatible
license — including code produced by an AI assistant that you have reason to believe reproduces
such a source.
