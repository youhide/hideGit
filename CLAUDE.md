# CLAUDE.md

Guidance for Claude Code and other AI assistants working in this repository.

## What this project is

hideGit is a cross-platform desktop Git client written in Rust with the [iced](https://iced.rs)
GUI toolkit, licensed GPL-3.0. Alongside the usual repository operations it provides pull request
alerts: native desktop notifications for review requests, CI status changes and newly conflicting
PRs.

**Current status: pre-alpha.** M1, M2 and M3 have landed: the workspace, CI, the `gix` read backend,
the commit graph, a read-only viewer, the working directory — status, staging by file, hunk and line,
discard, commit, a filesystem watcher — and everything that touches a remote: clone, branches, tags,
the stash, named remotes, and fetch, pull and push with progress and cancellation. History rewriting
is M5 and pull request alerts are M4. Check [ROADMAP.md](./docs/ROADMAP.md) before assuming a feature
exists, and check the code before referring to a module.

## Read before making architectural changes

| Document | Covers |
|---|---|
| [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) | Crate boundaries, `GitBackend`, forge integration, concurrency, errors |
| [docs/ROADMAP.md](./docs/ROADMAP.md) | What is in scope now versus deliberately deferred |
| [docs/adr/](./docs/adr/README.md) | Why each major decision was made |
| [docs/UI_SPEC.md](./docs/UI_SPEC.md) | Screens, state and message shapes, shortcuts |
| [docs/COMMIT_GRAPH.md](./docs/COMMIT_GRAPH.md) | Lane assignment algorithm |

If a change contradicts an ADR, propose a superseding ADR rather than reversing the decision in
passing.

## The rules that are easy to get wrong

**1. `gix` reads, the `git` CLI writes.**
Everything that reads a repository — log, status, diff, blame, refs, fetch — goes through `gix`.
Everything that writes to a remote or rewrites history — push, merge, rebase, pull, cherry-pick —
shells out to the system `git` binary. This is not a stopgap someone forgot to clean up: gitoxide
does not implement `push`, and delegating gives us credential helpers, hooks, submodules and LFS
for free. See [ADR-0002](./docs/adr/0002-git-backend-hybrid.md).

Do not add `git2`/libgit2 to the dependency tree. It was evaluated and rejected.

**2. `hidegit-core` has no UI and no network.**
It must not depend on `iced`, on `hidegit-forge`, or on an HTTP client. That constraint is what
lets the domain logic be tested without a window and without a token. Code that needs a widget or
a request belongs in `hidegit-ui` or `hidegit-forge`.

**3. Never build a `git` command as a shell string.** One documented exception, below.
Arguments go in a vector, no shell is spawned, `GIT_TERMINAL_PROMPT=0` is always set, machine
formats (`--porcelain=v2`, `-z`) are preferred over human output, and `stderr` is surfaced
to the user verbatim on failure. Branch names, paths and remote URLs come from untrusted
repositories. See [SECURITY.md](./SECURITY.md).

Arbitrary user text — a commit message, a branch name, a URL — goes on **stdin** or attached to its
option as `--opt=value`, never as a bare argument. Git's own commands are not uniform about which:
`git commit --file -` reads stdin, `git stash push --message` does not, and `git switch --create`
needs the name attached because `switch` accepts only one reference after `--`. Either shape keeps the
text one element of the argument vector.

The exception is `GIT_SEQUENCE_EDITOR`, which drives interactive rebase. Git runs every editor
through `sh -c`, so that variable is shell source whether hideGit likes it or not. It holds a
**constant string literal**, and the rebase plan reaches it as *data* in a second environment
variable the shell expands as a value. Nothing from the repository is ever concatenated into shell
code. See [ADR-0007](./docs/adr/0007-rebase-plan-through-the-environment.md); if you find yourself
building that string with `format!`, you are undoing the decision.

`--` is not the separator to reach for everywhere. It means *paths follow*, so on a command that
takes revisions and no paths it changes the request: `git reset --hard -- HEAD~1` asks to reset a
path named `HEAD~1`. Use `GitCommand::revisions`, which emits `--end-of-options`, for `reset`,
`rev-parse` and `rev-list`; `GitCommand::operands`, which emits `--`, for everything that takes
paths or refs — `merge`, `cherry-pick`, `revert`, `switch`. When adding a command, run it before
assuming which one it takes.

Three places read *human* output deliberately: `--progress` on stderr, which has no machine form, and
the fetch and push summaries. The push case is a documented exception to preferring machine formats —
`--porcelain` would move Git's actionable hint off stderr — and all three fail soft. See
[ADR-0005](./docs/adr/0005-progress-and-cancellation.md).

**4. Tokens go in the OS keychain, never in config, never in logs.**
No OAuth client secret may be embedded in the binary — this is open source, so it would not be
secret. Authentication is Device Flow.
See [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md#authentication-and-tokens).

**5. Blocking work never runs on the UI thread.**
`gix` calls and `git` subprocesses are blocking. They go through `Task::perform` onto a blocking
pool and return via a `Message`. A long operation that reports progress is a `Task::stream` — a
one-shot that ends when the work does — not a `Subscription`; PR polling is a long-lived
`Subscription` because it outlives any single request.

**6. `invalidate` reopens the gitoxide handle, it does not just clear a cache.**
gitoxide reads `.git/config` when a repository is opened and caches it, so any `git` command that
rewrites config — a branch rename, a remote change, `push --set-upstream` — would otherwise leave every
subsequent read describing the old file. The symptom is quiet: an upstream that silently disappears.

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo run
```

`xtask` is dev tooling, excluded from `default-members`, so lint it explicitly with `--workspace`.
It regenerates the icons and assembles the macOS bundle — see [assets/README.md](./assets/README.md):

```sh
cargo run -p xtask -- icons
cargo run -p xtask -- bundle-macos
```

Clippy warnings are errors. `cargo bench -p hidegit-core` times the graph layout against a
100,000-commit repository; the numbers to beat are in
[COMMIT_GRAPH.md](./docs/COMMIT_GRAPH.md#performance).

## Conventions

- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/), scoped by
  crate: `feat(core):`, `fix(ui):`, `docs(adr):`.
- Errors use `thiserror` in libraries. `hidegit-core` returns typed errors, never stringly-typed ones.
- Update the document in `docs/` that a change affects, in the same commit as the change.
- Public docs describe hideGit on its own terms and do not name competing products.

## When writing documentation here

Prefer specifics over adjectives. "60fps on a 100,000-commit repository" is useful; "blazingly
fast" is not. If a limitation exists — and the `push` gap is the obvious one — state it plainly in
the place a reader will hit it, rather than burying it.
