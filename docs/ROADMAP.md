# Roadmap

What gets built, in what order, and how we know each stage is finished.

Milestones are sequenced by dependency, not by date. Each has a **Done when** line that is a
behaviour someone can check, not a feeling of completeness.

| | Milestone | Theme |
|---|---|---|
| ✅ | [M0 — Foundation](#m0--foundation) | Decide and document |
| ✅ | [M1 — Scaffold & read-only viewer](#m1--scaffold--read-only-viewer) | See history |
| ✅ | [M2 — Working directory](#m2--working-directory) | Make commits |
| ✅ | [M3 — Branches & remotes](#m3--branches--remotes) | Daily driver |
| ✅ | [M4 — Pull request alerts](#m4--pull-request-alerts) | Forge integration |
| ✅ | [M5 — History operations](#m5--history-operations) | Stop dropping to a terminal |
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

**Status: complete, with one part of that bar unverified.** Everything in the scope above is
implemented, covered by tests against real repositories, and checked by eye. The one thing that has
*not* been verified is the last clause: **SSH with a passphrase and HTTPS with a credential helper**.
Every remote in the test suite is a bare repository on a local path, which needs no authentication,
and CI cannot hold a credential. `GIT_TERMINAL_PROMPT=0` makes a missing credential fail fast rather
than hang, and the failure is classified into an actionable `AuthError` — but that classification is
string matching against Git's stderr, exercised only against synthetic input. Saying M3 is done on
CI alone would be claiming something nobody has tried, so it is written down here instead: real
credentials remain a manual check.

Four things fell out of building it that the plan did not anticipate.

**`invalidate` was only half a cache invalidation.** It dropped the memoised commit walk, which is
what M1 and M2 needed, and left gitoxide's snapshot of `.git/config` alone. Every `git` command that
rewrites config — a branch rename, adding or removing a remote, `push --set-upstream` — therefore left
the read side describing the old file. The symptom was quiet rather than loud: after renaming a
branch its upstream vanished, and with it the `↑1` in the sidebar. `invalidate` now reopens the
handle. Nothing in M1 or M2 wrote config, so nothing had exposed it.

**`git switch` accepts exactly one reference after `--`.** It takes no paths for `--` to separate, so
passing a new branch name and a start point as two operands fails with "only one reference
expected". The name goes in `--create=<name>` instead. `git stash push --message` has the mirror-image
problem: unlike `git commit --file -` it does *not* read from stdin, and takes `-` as the literal
message. Both are the same lesson — Git's own commands are not uniform about where user text may go,
and the only way to find out is to run them.

**Pushing a renamed branch went to the wrong place.** The refspec used the local name on both sides,
so a branch whose upstream has a different name — precisely what renaming produces — quietly created a
*second* branch on the remote instead of updating the one it tracks. Nothing failed; there was simply
an extra branch. The destination now comes from the upstream.

**`git push --porcelain` was tried and reverted.** The machine format is the obvious choice and the
rule says to prefer it, but it also moves the failure detail off stderr: with it, a rejected push
leaves only `error: failed to push some refs` where plain `git push` writes
`! [rejected] main -> main (stale info)` and a hint saying what to do next. Since Git's own message is
the most useful thing hideGit has to say when a command fails, `push` reads the human summary. The
reasoning and the rejected alternatives are in
[ADR-0005](./adr/0005-progress-and-cancellation.md), which also records what cancelling a subprocess
does *not* solve on Windows.

Two smaller things worth writing down. Ahead/behind is deliberately not part of a refresh — a refresh
runs on every file save through the watcher, and this costs a commit walk per tracking branch — so it
loads on its own task and its absence for a branch that tracks nothing is meaningful rather than a
gap. And the correction to M2's retro: iced 0.14 focus is not observable, but it *is* settable, which
is why a prompt can open with the cursor already in its field. It does not unblock `Space`; knowing
where focus *is* remains the missing half.

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

**Status: complete, with the same shape of caveat M3 had.** Everything in the scope above is
implemented and covered by tests — HTTP mocked throughout, so no test needs a token or a network —
and the panel has been checked by eye. What has *not* been verified is the bar itself, because it
needs a real account: **a real review request producing a real notification**, and **a full day of
running without a rate-limit warning**. Every credential path is the same manual check M3 left
behind, and saying otherwise would be claiming something nobody has tried.

Four things fell out of building it that the plan did not anticipate.

**The documented polling design could not have worked.** It specified conditional REST requests with
`If-None-Match`, on the reasoning that a free `304` is what makes a one-minute interval affordable.
But a check run completing does not modify the pull request it belongs to — checks attach to a
*commit* — so the `ETag` stays valid for the entire life of a CI run and `ChecksFailed` never fires.
The one notification a developer actually waits for was the one the design could not deliver. The
gap only became visible when the event list and the transport were looked at together, which is an
argument for writing both down in the same document.
[ADR-0006](./adr/0006-poll-pull-requests-over-graphql.md) records the replacement and what it costs:
every poll now spends budget instead of riding a free `304`, and the query's nested page sizes
became a rate-limit decision rather than a display one.

**Two of the seven events arrive as an absence.** The poll asks for open pull requests, so a merge
and a close both look like a row disappearing — and `PrMerged` and `PrClosed` are separate events
the spec distinguishes. Each disappearance of a pull request *you wrote* therefore costs one extra
request to read its state. That is a handful a day rather than one per poll, and the alternative was
reporting both as the same thing.

**`UNKNOWN` is the ordinary answer, not an error.** GitHub recomputes mergeability after every push
and says `UNKNOWN` while it does, so `Unknown → Conflicting` is what *finished checking* looks like
rather than what *started conflicting* looks like. Collapsing it into two states would have fired
`PrConflicting` on every push. It is why `MergeState` has three variants and why the sidebar marks a
conflict only when one is known.

**The macOS notification story is about attribution, not delivery.** `notify-rust` goes through
`mac-notification-sys`, which credits whatever executable sent the notification: run from
`cargo run`, macOS raises its permission prompt naming *that binary* — observed here as
`"hidegit_forge-22ed2ace8b2e02be" would like to send you notifications` — rather than naming hideGit.
`show()` returns `Ok` either way, so nothing in the code can tell. The bundle is what fixes it, and
`cargo test -p hidegit-forge -- --ignored a_real_notification` is the manual check. That settles half
of the open question about whether a platform shim is needed; actionable buttons on the alert are
the other half and are still open.

One smaller thing. **`cargo run -p xtask -- bundle-macos` wraps an already-built release binary; it
does not build one.** A stale `target/release/hidegit` produces a bundle of the previous milestone
that looks entirely convincing, which cost a confused half-hour chasing a regression that did not
exist. `cargo build --release` first.

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

**Status: the bar is met; two scope items are deferred and named below.** The three-conflict rebase
is a test rather than a claim — `a_rebase_conflicting_on_three_commits_can_be_finished_here` starts
it, resolves each stop and finishes, and `aborting_a_rebase_part_way_restores_exactly_the_prior_state`
aborts *after* one resolution and asserts `HEAD`, the subjects, both files and the whole status came
back. The same path was driven by hand through the window: merge, conflict, resolve, continue; and
cherry-pick, conflict, abort.

Three things fell out of building it that the plan did not anticipate.

**`--` is the wrong separator for half of these commands.** It means *paths follow*, so
`git reset --hard -- HEAD~1` asks to reset a path named `HEAD~1` and fails with "Cannot do hard reset
with paths"; `git rev-parse` without `--verify` prints the marker back as though it were a revision.
Commands taking revisions and no paths use `--end-of-options`, which ends flag parsing without making
that claim. `merge`, `cherry-pick` and `revert` accept `--` and keep it. Which one a command wants is
not derivable from its signature — each was run to find out. [SECURITY.md](../SECURITY.md) records it,
because the reflex to reach for `--` everywhere is a security habit producing a correctness bug.

**`ours` and `theirs` are inverted for everything that replays a commit.** A rebase applies your
commits *onto* the upstream, so Git's `ours` is the branch being rebased onto and `theirs` is your own
commit. Resolving a rebase by taking "ours" therefore discards every commit it moves, silently — the
first version of the acceptance test did exactly that and ended with the three commits gone. The
resolver says so in the operations where it applies rather than swapping the panes, because swapping
would make hideGit disagree with `git status`, with every tutorial, and with the terminal someone
drops to when it goes wrong.

**Three commits against the same file do not produce three conflicts.** Resolving the first leaves
exactly the content the second expects, so the rest apply cleanly and the rebase stops once. The
acceptance fixture needed a *different* file per commit — which is worth knowing before writing any
test that counts conflicts.

**Deferred, deliberately, to M6.** Two scope items above are not built, and both are listed there
rather than quietly dropped:

- **Drag-and-drop on the graph** for merge and rebase. Deferred from here and **landed in M6**; see
  [UI_SPEC](./UI_SPEC.md#the-commit-graph) for the three rules that came out of building it.
- **The interactive rebase plan editor.** Deferred from here and **landed early in M6** — the entry
  stays because the reasoning still holds: it is a screen of its own rather than something bolted
  onto the resolver.

---

## M6 — Polish & release

Everything between "works" and "someone who does not write Rust can install it".

**Scope**

- Themes: dark and light, both designed rather than inverted; custom theme files. **Both shipped
  themes have landed** and `theme.name` in `config.toml` now selects between them — it had been read
  from disk and ignored since M1. A name that is not a theme falls back to dark **and says so on
  screen**, which is what [UI_SPEC](./UI_SPEC.md#theming) promised: the warning went only to stderr,
  so a typo in `theme.name` looked exactly like the setting being ignored again. Custom themes as
  TOML files are still to come
- Complete keyboard navigation; a discoverable shortcut reference. **Partly landed**: `Tab`, `Space`,
  `Cmd+Shift+Enter`, `Cmd+]` / `Cmd+[` and `Cmd+Shift+.` are bound, which closes the `Space` debt M2
  wrote down — focus turned out to be observable through a `find_focused` widget operation, which is
  not the signal M2 was reaching for. What is still missing is named in
  [UI_SPEC](./UI_SPEC.md#keyboard-shortcuts) rather than left looking implemented: the command
  palette, the `G` chords, repository tabs, and a shortcut reference someone can actually read
- **Drag-and-drop on the graph** for merge and rebase, with a confirmation step — deferred from
  [M5](#m5--history-operations). **Landed.** A press becomes a drag only past a threshold, so
  clicking a badge still selects; the drop opens the action sheet rather than running anything;
  and a drop between two branches neither of which is checked out says so instead of checking one
  out silently
- **The interactive rebase plan editor** — reorder, squash, fixup, edit, drop. **Landed.** The
  commits a rebase would replay are listed oldest first, which is todo order rather than the graph's;
  showing them newest-first would silently invert every reorder. All six verbs are on every row
  rather than behind a dropdown, since they are the whole vocabulary of an interactive rebase and a
  menu would make discovering `fixup` an act of exploration. Nothing runs until Start, so Cancel
  needs no confirmation. A plan Git would refuse — squashing the first step, dropping everything —
  disables Start and says which, because a greyed button explaining neither is a dead end
- Multi-repository tabs, with per-repository state preserved. **Landed.** The tab bar is absent with
  one repository open, because a bar showing a single tab costs a row of screen to say something you
  can already see. Opening a repository that is already open switches to its tab rather than opening
  a second copy — two tabs on one repository would each hold their own idea of its state. Closing
  lands on the neighbour rather than the last tab
- Settings UI covering everything currently in TOML. **Partly landed**: theme and every alert switch
  are on a `Cmd+,` panel, applied as they are changed and written back to `config.toml` **in place** —
  the file keeps its comments, its key order and any key hideGit does not own, because it is
  hand-edited and often lives in a dotfiles repository. **Quiet hours have landed** — a switch and
  the two ends of the window, picked from the twenty-four hours rather than typed, since a text field
  would have to decide what "25" means. They sit under the alerts they modify and go unavailable with
  them, and a window whose ends are equal says on the panel that it silences nothing, which is what
  `QuietHours::covers` decides and what somebody would otherwise have to guess. **Muting a repository
  has landed** as a list rather than a text field — the key is `owner/name` as the forge spells it, and
  a name typed by hand that does not match silences nothing while looking as though it does. The list
  names every open repository with a GitHub remote plus anything already muted, which may well not be
  open: an entry that vanished from the panel the moment you closed its tab would be a setting you
  could not undo without editing the file. It is kept sorted, because it is written to a file people
  read and diff. **`window.remember_geometry` has landed** too, saying on the panel what turning it
  off actually does — the default size, centred — rather than leaving that to be guessed.

  **The panel used to lie.** Writing in place means refusing to overwrite a file somebody is
  part-way through editing, and that refusal — along with an unreadable file, a config directory
  that cannot be created, and a system with no config directory at all — was a log line nobody sees
  in a GUI, under a footer that read "Saved to config.toml as you change it". The toggle flipped, the
  footer agreed, and the change was gone on restart. `save_settings` now reports why it declined and
  the footer says so instead
- Search: commits by message, author, hash — **landed**, on `Cmd+F`. One box searches every field,
  because people type a fragment and expect it found rather than classifying it first, and each hit
  says which field matched. The result reports whether the walk stopped at the limit: "these are the
  first matches" and "these are the matches" are different answers. Typing is **debounced**: a search
  is a walk of the whole history, and a ten-letter word typed at speed was ordering ten of them. The
  guard that stops a stale answer landing was already there and was doing its job — it just never
  stopped the work. File search within a commit is still to come
- Blame view. **Landed**, and it is the last `GitBackend` method that was returning
  `NotImplementedYet` — the trait declared its whole surface from M1 and is now entirely
  implemented. Rename detection had to be turned on explicitly: gitoxide leaves it off by default,
  and without it every line of a renamed file is attributed to the commit that moved it rather than
  the commit that wrote it, which answers the wrong question entirely
- Accessibility: focus order, contrast, screen-reader labels where iced supports them. **Partly
  landed, and the rest is blocked on the toolkit.** iced 0.14 has no accessibility surface at all —
  no AccessKit integration, no accessible trait, nothing a screen reader can read. That was checked
  against the crate sources rather than assumed, and it means "screen-reader labels where iced
  supports them" currently means nowhere. Do not plan that part until iced ships it.

  What is real and has landed: contrast is asserted for both palettes by the theme tests, and
  **keyboard reachability**. An action sheet can now be walked with `↑`/`↓` and chosen with `Enter` —
  every per-item action in the sidebar goes through a sheet, so a sheet that only answered the mouse
  put branch, tag, remote and stash actions behind one. `Tab` moves between a prompt's fields, which
  a two-field prompt like "Add a remote" needed and did not have.

  Still to do: a focus ring that is visible on every focusable widget, and an audit that every
  action has a keyboard route
- Crash reporting that is local and opt-in
- **Packaging:** signed and notarised `.dmg` (macOS), `.msi` (Windows), AppImage and Flatpak
  (Linux); an update-available check that never auto-installs

  Landed early, because an application icon is not something to bolt on at the end: the icon
  itself in every platform format, the Windows executable resource, a macOS `.app`
  (`cargo run -p xtask -- bundle-macos`), and a Linux `.desktop` entry with a hicolor icon set
  (`packaging/linux/install.sh`).

  **Downloadable archives have landed** — a universal macOS `.app`, a Windows `.exe` and a Linux
  tarball, built and published by tag from `.github/workflows/release.yml`. They are **unsigned**,
  deliberately and temporarily: waiting for a certificate would have meant no downloads at all,
  and every alternative to a warning dialog costs money this project does not have yet. What that
  means in practice, and what signing would take, is written down in [RELEASING.md](./RELEASING.md)
  rather than left for someone to rediscover.

  One thing that turned out not to be optional: the macOS bundle is **ad-hoc signed**. arm64 macOS
  will not execute a Mach-O carrying no signature at all, and `lipo` strips the signature the linker
  applies — so a universal build without that step would fail to launch on every Apple Silicon Mac,
  which is most of them. An ad-hoc signature is free and anonymous; it is not the notarisation that
  quiets Gatekeeper.

  What remains here is signing, notarisation, and the installers themselves — and they belong in the
  same piece of work, since an unsigned `.dmg` adds a step for the user without removing the warning
  it would have been justified by.
- Benchmarks in CI so a performance regression fails a build rather than being noticed by a user

**Done when:** a person who has never installed Rust can download an installer for their platform,
open a repository, commit, push, and receive a PR notification — and nothing on that path requires
a terminal. **That is 1.0.**

---

## Post-1.0

Breadth, once the core is solid. Not ordered.

| Area | Notes |
|---|---|
| **Submodules** | Status, update, init. Common enough to matter, awkward enough to deserve its own milestone. |
| **Worktrees** | Growing in use; fits naturally alongside multi-repo tabs. |
| **Git LFS** | Largely inherited from shelling out to `git`, but needs UI for pointer files and fetch state. |
| **Interactive rebase editor** | A richer one than M6's: a plan the graph itself can be dragged into, and `--autosquash`. |
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
