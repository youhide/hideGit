# Roadmap

What gets built, in what order, and how we know each stage is finished.

Milestones are sequenced by dependency, not by date. Each has a **Done when** line that is a
behaviour someone can check, not a feeling of completeness.

| | Milestone | Theme |
|---|---|---|
| ✅ | [M0 — Foundation](#m0--foundation) | Decide and document |
| ✅ | [M1 — Scaffold & read-only viewer](#m1--scaffold--read-only-viewer) | See history |
| ✅ | [M2 — Working directory](#m2--working-directory) | Make commits |
| ⬜ | [M3 — Branches & remotes](#m3--branches--remotes) | Daily driver |
| ⬜ | [M4 — Pull request alerts](#m4--pull-request-alerts) | Forge integration |
| ⬜ | [M5 — History operations](#m5--history-operations) | Stop dropping to a terminal |
| ⬜ | [M6 — Polish & release](#m6--polish--release) | 1.0 |
| ⬜ | [Post-1.0](#post-10) | Breadth |

---

## M0 — Foundation

**Status: complete.** Documentation and licensing, before any code.

- README, LICENSE (GPL-3.0), CONTRIBUTING, CODE_OF_CONDUCT, SECURITY
- [ARCHITECTURE](./ARCHITECTURE.md), this roadmap, [UI_SPEC](./UI_SPEC.md),
  [COMMIT_GRAPH](./COMMIT_GRAPH.md)
- [ADRs 0001–0004](./adr/README.md): GUI toolkit, Git backend, forge strategy, license
- `CLAUDE.md` for AI assistants; GitHub issue and PR templates

**Done when:** a contributor can read the repository and correctly answer what is being built, how
it is structured, and why each major technical choice was made — without asking.

---

## M1 — Scaffold & read-only viewer

The first runnable application. Reads repositories; cannot modify them. Everything here is `gix`,
so this milestone also validates the read half of
[the hybrid backend](./adr/0002-git-backend-hybrid.md).

**Status: complete.** Every item below is implemented and covered by tests, the performance target
is [measured](./COMMIT_GRAPH.md#performance) rather than claimed, and CI passes `fmt`, `clippy -D
warnings` and the test suite on Linux, macOS and Windows.

Signing off took two things the test suite could not supply. CI had never actually executed —
the workflow runs on `push` to `main` and on `pull_request`, and the branch had never been
pushed — so the cross-platform claim was untested until it went green on all three. And the
interface had never been reviewed by eye, which is how the one defect the tests missed was found:
a tree diff reports the directories along a changed file's path as changes of their own, so a
commit touching `a/b/c.rs` was shown as three modified files plus the file, each directory
rendering as binary. Every fixture committed at the repository root, so no test ever exercised a
nested path. Both are fixed; the regression test uses a nested path deliberately.

**Scope**

- Cargo workspace: `hidegit-core`, `hidegit-forge` (stub), `hidegit-ui`, `hidegit`
- CI on Linux, macOS and Windows: `fmt`, `clippy -D warnings`, `test`
- `GitBackend` trait with its read half implemented over `gix`
- Startup check for `git` on `PATH` and minimum version, with an actionable message
- Open a repository via native picker; recent-repository list
- **Commit graph** — virtualised, lanes, refs shown on their commits
- Commit detail: metadata, full message, changed files
- Diff viewer: unified and side-by-side, syntax-uncoloured to start
- Branch/tag/remote sidebar (read-only)
- Dark theme; window geometry persistence

**Explicitly not in scope:** anything that writes. No staging, no commit, no checkout.

**Done when:** you can open any local repository, scroll its full history, click any commit and
read its diff — and a 100,000-commit repository scrolls without visible stutter, measured, not
assumed. Laying out a visible window at row 50,000 measures at **52µs** against a 16.6ms frame
budget; opening a repository that size costs **1.01s** for the topological ordering pass, which is
the number to attack next.

---

## M2 — Working directory

The commit loop. After this, hideGit is useful for real work on a single branch.

**Scope**

- `status` with rename detection, respecting `.gitignore`
- Working-directory view: staged, unstaged, untracked, with counts
- Stage and unstage by file, by selection of files, and **by hunk**
- Line-level staging within a hunk
- Discard changes — file and hunk — behind an unmistakable confirmation
- Commit: message editor with subject/body separation, amend, sign-off
- Filesystem watcher driving automatic status refresh, debounced
- Conflicted-file detection and `RepoState` awareness (actions disabled mid-operation)

**Status: complete.** You can make a complete commit — including staging only part of a file —
without leaving the app, and the file list updates on its own when a file changes outside it.
Verified against real repositories rather than only in tests: staging one hunk of a two-hunk file
leaves the other in `git diff`, staging two lines of a four-line change leaves exactly the other
two, and `git add -p` produces the same index.

Two things fell out of building it that the plan did not anticipate.

`RepositoryChanged` used to reopen the repository, and opening *pushes a new entry* — so every
write appended a second copy of the repository and threw away the user's scroll position. It now
rereads in place, restoring the viewport by commit id. Left alone, the watcher would have made
that unbearable.

**`Space` is not bound to stage/unstage**, despite the shortcut table. iced 0.14 keeps text-input
focus inside the widget — not observable, not settable — and wrapping a field in a `mouse_area` to
catch the click that grants focus swallows that click, so the field never focuses at all. A global
binding therefore cannot know the commit message field is being typed into until its first
keystroke has already arrived, and `Space` is the one bare key whose leak would stage a file. It
waits for M6's keyboard-navigation work. `j`/`k` stay bound because their leak moves a highlight.

---

## M3 — Branches & remotes

Daily-driver capability for a normal, linear workflow. The first milestone that exercises the
**write** half of the hybrid backend, so it is where the CLI shell-out proves itself.

**Scope**

- Branch: create, checkout, rename, delete (with upstream-aware warnings)
- Tags: create (lightweight and annotated), delete, push
- Remotes: add, edit, remove; view tracking relationships
- **Fetch, pull, push** — including force-with-lease, prune, and push of new branches
- Progress reporting and cancellation for network operations
- Authentication delegated entirely to the user's Git credential helper
- Stash: create (with and without untracked), apply, pop, drop, view contents
- Ahead/behind indicators per branch
- Clone a repository from a URL

**Done when:** a full day of ordinary work — branch, commit, push, open a PR in the browser, pull
— happens without opening a terminal, on all three platforms, including over SSH with a passphrase
and over HTTPS with a credential helper.

---

## M4 — Pull request alerts

Design in [ARCHITECTURE.md](./ARCHITECTURE.md#forge-integration) and
[UI_SPEC.md](./UI_SPEC.md#pr-panel).

**Scope**

- GitHub authentication via OAuth Device Flow; PAT as fallback
- Token storage in the OS keychain; no secret embedded in the binary
- `Forge` trait finalised against the GitHub implementation
- PR panel: open PRs where you are author, reviewer or assignee, with review and CI state
- Background polling with `ETag` and rate-limit awareness, backoff on failure
- Native desktop notifications: review requested, review submitted, new comment on your PR, CI
  state changed, PR merged or closed, your PR started conflicting
- Per-event and per-repository notification preferences; quiet hours
- Offline behaviour: last known state shown, clearly marked stale, no error spam
- Open a PR in the browser; create a PR from the current branch

**Done when:** someone requests your review on GitHub and a native notification appears on your
desktop within the configured interval — with hideGit in the background, no browser open, and no
rate-limit warnings after a full day of running.

---

## M5 — History operations

History rewriting, and the conflict handling it requires. Conflict resolution is the hard part: a
client that cannot finish a conflicted rebase is not one you can start a rebase in.

**Scope**

- Merge, with fast-forward control and merge-commit message editing
- Rebase, including interactive: reorder, squash, fixup, edit, drop
- Cherry-pick and revert, single and multi-commit
- Reset: soft, mixed, hard, with an unambiguous explanation of each
- **Conflict resolution UI**: three-pane (ours / result / theirs), per-hunk resolution, mark
  resolved, continue or abort the in-progress operation
- Correct handling of operations interrupted mid-way, including a repository left mid-rebase by an
  external tool
- Drag-and-drop on the graph for merge and rebase, with a confirmation step
- Reflog view, and undo for the operations that support it

**Done when:** a rebase that hits conflicts on three separate commits can be started, resolved and
completed entirely inside hideGit — and aborting at any point restores the repository to exactly
its prior state.

---

## M6 — Polish & release

Everything between "works" and "someone who does not write Rust can install it".

**Scope**

- Themes: dark and light, both designed rather than inverted; custom theme files
- Complete keyboard navigation; a discoverable shortcut reference
- Multi-repository tabs, with per-repository state preserved
- Settings UI covering everything currently in TOML
- Search: commits by message, author, hash; file search within a commit
- Blame view
- Accessibility: focus order, contrast, screen-reader labels where iced supports them
- Crash reporting that is local and opt-in
- **Packaging:** signed and notarised `.dmg` (macOS), `.msi` (Windows), AppImage and Flatpak
  (Linux); an update-available check that never auto-installs

  Landed early, because an application icon is not something to bolt on at the end: the icon
  itself in every platform format, the Windows executable resource, an unsigned macOS `.app`
  (`cargo run -p xtask -- bundle-macos`), and a Linux `.desktop` entry with a hicolor icon set
  (`packaging/linux/install.sh`). What remains here is signing, notarisation, and the installers
  themselves.
- Benchmarks in CI so a performance regression fails a build rather than being noticed by a user

**Done when:** a person who has never installed Rust can download an installer for their platform,
open a repository, commit, push, and receive a PR notification — and nothing on that path requires
a terminal. **That is 1.0.**

---

## Post-1.0

Breadth, once the core is solid. Not ordered.

| Area | Notes |
|---|---|
| **GitLab & Bitbucket forges** | The first real test of whether the `Forge` trait generalises. Expect to revise it — merge requests are not pull requests. |
| **Submodules** | Status, update, init. Common enough to matter, awkward enough to deserve its own milestone. |
| **Worktrees** | Growing in use; fits naturally alongside multi-repo tabs. |
| **Git LFS** | Largely inherited from shelling out to `git`, but needs UI for pointer files and fetch state. |
| **Interactive rebase editor** | A richer plan editor than M5's. |
| **Internationalisation** | Scaffolding lands before 1.0 so this is not a retrofit; **PT-BR** is the first translation. |
| **Migrate operations back to `gix`** | As gitoxide lands `push` and a complete rebase workflow, methods move off the CLI one at a time, guarded by the `GitBackend` test suite. Conditions in [ADR-0002](./adr/0002-git-backend-hybrid.md). |
| **Plugin or scripting surface** | Only if a real need appears. A plugin API is a permanent compatibility commitment. |

## What is deliberately not planned

Saying no in advance is cheaper than saying it later:

- **A hosted sync service.** Configuration is a TOML file; sync it with whatever you already use.
  No server means no privacy question to answer.
- **An account system.** hideGit is fully usable with no account. Authentication exists only for
  forge features, and only for the forge you choose.
- **Telemetry.** No usage analytics, opt-in or otherwise.
- **A bundled Git.** The system `git` is the point — it carries your configuration, your credential
  helpers and your hooks.
