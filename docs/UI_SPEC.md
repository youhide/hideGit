# UI Specification

Screens, state, and interaction. iced follows the Elm architecture, so this document is organised
the way the code will be: `State`, `Message`, `update`, `view`.

Layout sketches are structural, not visual design.

## Contents

- [Principles](#principles)
- [Screen inventory](#screen-inventory)
- [Application state](#application-state)
- [Main window](#main-window)
- [Diff view](#diff-view)
- [Conflict resolver](#conflict-resolver)
- [PR panel](#pr-panel)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Theming](#theming)
- [Interaction rules](#interaction-rules)

## Principles

1. **The graph is the application.** History is what people reason about, so it gets the centre of
   the window and the most pixels. Everything else is support.
2. **Git's own vocabulary.** `stash`, `rebase`, `ref`, `HEAD`. Renaming Git concepts teaches people
   something they cannot use anywhere else.
3. **Never lie about state.** A repository mid-rebase does not offer "commit" as though nothing is
   happening. Disabled actions explain why they are disabled.
4. **Destructive actions are distinguishable.** Discard, hard reset and force push are visually and
   textually distinct from everything else, and say exactly what will be lost.
5. **Keyboard-complete.** Every action reachable by mouse is reachable by keyboard.
6. **Never block on I/O.** Anything slow shows progress and can be cancelled.

## Screen inventory

| Screen | Purpose | Milestone |
|---|---|---|
| Welcome | Open, clone, recent repositories | M1 |
| Main window | Graph, sidebar, detail — where all work happens | M1 |
| Staging view | Staged / changed / untracked / conflicted, and the selected file's diff | M2 |
| Diff view | Unified / side-by-side, hunk staging | M1 / M2 |
| Commit composer | Message, amend, sign-off | M2 |
| Conflict resolver | Three-pane resolution | M5 |
| PR panel | Pull request list and detail | M4 |
| Settings | Everything otherwise in TOML | M6 |
| Device-flow dialog | Code display, browser handoff, progress | M4 |

## Application state

```rust
struct App {
    screen:      Screen,               // Welcome | Repository
    repos:       Vec<OpenRepo>,        // multi-repo tabs (M6)
    active:      Option<usize>,
    config:      Config,
    theme:       Theme,
    toasts:      Vec<Toast>,
}

struct OpenRepo {
    path:        PathBuf,
    backend:     Arc<dyn GitBackend>,
    head:        Head,
    refs:        Refs,
    state:       RepoState,            // Clean | Merging | Rebasing | CherryPicking | Bisecting
    graph:       GraphView,            // commits + computed layout + viewport
    status:      WorktreeStatus,
    selection:   Selection,            // Commit(id) | WorkingDirectory | Stash(n)
    detail:      DetailPane,
    prs:         PrPanelState,
    pending:     Vec<Operation>,       // in-flight, cancellable
}
```

`RepoState` is consulted before rendering any action. It is the single source of truth for what is
currently legal.

Top-level messages:

```rust
enum Message {
    Repo(usize, RepoMessage),
    OpenRepository(PathBuf),
    CloseRepository(usize),
    ConfigChanged(Config),
    ToastDismissed(ToastId),
}

enum RepoMessage {
    // user intent
    Selected(Selection),
    GraphScrolled(Viewport),
    StagingRowSelected(StagingRow),     // which section, and where in it
    StageRequested(StageTarget),        // File | Files | Hunk | Lines
    CommitRequested(CommitDraft),
    CheckoutRequested(CheckoutTarget),
    PushRequested(PushSpec),
    // async results
    StatusLoaded(Result<StatusLoad, GitError>),   // status plus both diffs, in one unit
    CommitsLoaded(Result<Vec<Commit>, GitError>),
    DiffLoaded(Result<Diff, GitError>),
    OperationProgress(OperationId, Progress),
    OperationFinished(OperationId, Result<OperationOutcome, GitError>),
    RepositoryChanged,                  // filesystem watcher or completed operation
    // forge
    PrsUpdated(Result<Vec<PullRequest>, ForgeError>),
}
```

**`RepositoryChanged` is the refresh mechanism.** Every operation that mutates the repository ends
by emitting it, and it triggers a status and refs reload. One code path for "something changed",
rather than each operation remembering which views it invalidated.

## Main window

```
┌───────────────────────────────────────────────────────────────────────────────┐
│ ⟳ Fetch  ↓ Pull  ↑ Push    ⌥ Branch  ⌥ Stash        [main ▾]      ⚙  🔔 3     │
├──────────────────┬────────────────────────────────────────────────────────────┤
│ WORKING DIR   ●5 │  ●───────  feat: hunk staging          you      2m         │
│                  │  │ ●─────  fix: graph scroll jitter    you      1h         │
│ LOCAL            │  ●─┘                                                        │
│  ▸ main          │  │  ●────  chore: bump gix            dependabot 3h        │
│  ▸ feat/graph  ↑2│  ●──┘                                                       │
│                  │  │                                                          │
│ REMOTES          │  ●         Merge pull request #42     you       1d   ⑂main │
│  ▾ origin        │  ├─┐                                                        │
│    ▸ main        │  │ ●────── docs: architecture         you       1d         │
│                  │                                                             │
│ TAGS             ├────────────────────────────────────────────────────────────┤
│  ▸ v0.1.0        │ feat: hunk staging                                          │
│                  │ a3f9c21 · you · 2 minutes ago                               │
│ STASHES       2  │                                                             │
│                  │  M  crates/hidegit-ui/src/diff.rs        +48 −12            │
│ PULL REQUESTS    │  A  crates/hidegit-core/src/patch.rs     +91  −0            │
│  ⬤ #47 review    │                                                             │
│  ⬤ #45 CI failed │                                                             │
└──────────────────┴────────────────────────────────────────────────────────────┘
```

**Sidebar** — one tree, one mental model for "places I can jump to": working directory, local
branches, remotes, tags, stashes, pull requests. Ahead/behind indicators on branches. Counts on
section headers.

**Graph** — the centre. Virtualised: only visible rows are laid out and drawn. Refs are rendered as
badges on their commits. Selecting a row updates the detail pane. Full rendering rules in
[COMMIT_GRAPH.md](./COMMIT_GRAPH.md).

**Detail pane** — commit metadata and changed files when a commit is selected; the staging view
when the working directory is selected.

**Toolbar** — the operations reached constantly. Push shows ahead-count; the notification bell
shows unread PR alerts. Operations in flight replace their button with a progress indicator and a
cancel affordance.

**In-progress operations** get a persistent banner across the top: *"Rebasing feat/graph onto main
— 3 of 7 commits"* with Continue / Skip / Abort. It cannot be dismissed, because the repository
genuinely is in that state and hiding it is how people lose work.

## Staging view

Fills the detail pane when the sidebar's working-directory row is selected. Four sections down the
left, the selected file's diff on the right.

```
┌──────────────────────────────┬──────────────────────────────────┐
│ CONFLICTED                 1 │ M tracked.txt            +1 −1   │
│ ! shared.txt  both modified  ├──────────────────────────────────┤
│ STAGED                     3 │ @@ -1,3 +1,3 @@                  │
│ A .gitignore                 │   1   1   one                    │
│ R before.txt → after.txt     │   2     − two                    │
│ M tracked.txt                │       2 + TWO                    │
│ CHANGED                    2 │   3   3   three                  │
│ D doomed.txt                 │                                  │
│ M tracked.txt                │                                  │
│ UNTRACKED                  1 │                                  │
│ ? untracked.txt              │                                  │
└──────────────────────────────┴──────────────────────────────────┘
```

- **Conflicts sit at the top**, because nothing else in the working directory can be finished
  until they are resolved. Each names why it conflicts in Git's own words — "both modified",
  "deleted by them".
- **A file can appear in two sections at once.** Staged and then edited again, it is in both
  `STAGED` and `CHANGED`, and the two rows show *different diffs*: `HEAD` against the index, and
  the index against the working tree. Selecting a row therefore carries which section it came
  from, not just its path.
- **A rename is one row**, `before → after`, rather than a deletion and an addition that the
  reader has to pair up themselves.
- Status is carried by a glyph (`A`/`M`/`D`/`R`/`C`/`T`/`?`/`!`) as well as by colour, so the list
  reads without hue.
- Untracked files have no diff to show — nothing in the repository has ever seen them — so the
  pane says so rather than rendering every line as an addition against nothing.
- A clean working directory says *"Nothing to commit"* and what that means, per the empty-state
  rule below.

The sidebar badge counts every entry across all four sections, so a file that is both staged and
changed counts twice — the same way `git status` lists it under two headings.

Each row carries its own actions: `+` to stage, `−` to unstage, `✕` to discard. `Cmd+Backspace`
discards the open row. Discarding a *staged* change does nothing by keyboard — unstaging and then
destroying is two decisions, and one key must not stand for both.

**`Space` is not bound.** The spec's table lists it as stage/unstage, and it is deliberately absent
until M6's keyboard-navigation work. iced 0.14 keeps text-input focus inside the widget: it is
neither observable nor settable from the application, and wrapping a field in a `mouse_area` to
catch the click that grants focus swallows that click so the field never focuses at all. So a
global key binding cannot know whether the commit message field is being typed into until its
first keystroke has already been delivered — and `Space` is the one bare key whose leak would
stage a file. `j`/`k` remain bound because their leak only moves a highlight.

## Commit composer

Sits under the file lists, because it is about the whole commit rather than about whichever file is
open. Subject and description are separate fields; `Enter` in the subject commits, as does
`Cmd+Enter` from either.

- **Amend** starts from the message it is replacing, the way `git commit --amend` opens an editor
  already holding it, and stays available with nothing staged — rewording the last commit is a
  real thing to want.
- **Sign off** appends the trailer Git itself would.
- The Commit button says why it is unavailable rather than leaving it to be guessed: *"A summary is
  required"*, *"Nothing staged"*, *"A rebase in progress"*.
- A failed commit keeps the message. A rejected hook must not cost the user what they wrote.

## Confirmations and toasts

Both sit in a layer over the screen.

A **confirmation** is modal and names what will be lost — *"Changes to doomed.txt will be lost.
This cannot be undone."* — never a generic "are you sure?". Its accept button carries the verb
("Discard"), not "OK". Cancel comes first and unemphasised, because the safe choice should not be
the one that takes aim. While it is up it owns the keyboard: `Esc` cancels, `Enter` accepts, and
nothing else reaches the screen behind it.

A **toast** reports a failure and keeps Git's own stderr verbatim rather than paraphrasing it,
because that text is the most useful thing hideGit has to say when a command fails. Success is
silent: the refresh that follows an operation is its result, and a toast per click is noise.

## Diff view

Two modes, toggleable and remembered per user: **unified** and **side-by-side**.

```
┌─────────────────────────────────────────────────────────────────┐
│ crates/hidegit-core/src/patch.rs          +91 −0     [⇄] [⊞]    │
├─────────────────────────────────────────────────────────────────┤
│ @@ -0,0 +1,24 @@                              [Stage hunk]      │
│ + pub struct Patch {                                             │
│ +     pub file:  PathBuf,                                        │
│ +     pub hunks: Vec<Hunk>,                                      │
│ + }                                                              │
└─────────────────────────────────────────────────────────────────┘
```

- Hunk headers carry a stage/unstage action; the file header carries one for the whole file
- Clicking a changed line picks it out; the file header then offers those lines instead, because
  acting on the whole hunk would silently include what was left out
- A chosen line is marked twice: a bar in the margin and a brighter background. The background is
  *brightened*, not tinted toward the accent — the accent is a bright blue and the line backgrounds
  are very dark, so blending turns a removal purple and an addition teal, trading the added/removed
  reading away rather than adding to it
- `J`/`K` step between hunks, highlighting the one they land on
- Word-level intra-line highlighting on modified lines
- Whitespace-change toggle; large-file and binary-file placeholders rather than a hang
- Syntax highlighting deferred past M1 — correct diffing first

## Conflict resolver

Ships in [M5](./ROADMAP.md#m5--history-operations). The component that decides whether people trust
hideGit with a rebase.

```
┌──────────── OURS ────────────┬─── RESULT ───┬─────────── THEIRS ───────────┐
│ fn merge(&self) -> Result {  │  (editable)  │ fn merge(&self) -> Outcome { │
│     self.strategy.apply()    │              │     self.apply_strategy()    │
│ }                            │              │ }                            │
├──────────────────────────────┴──────────────┴──────────────────────────────┤
│ conflict 2 of 5   [Take ours] [Take theirs] [Take both] [Edit]  ‹ prev  next ›│
├────────────────────────────────────────────────────────────────────────────┤
│ Abort rebase                              Mark resolved · Continue rebase   │
└────────────────────────────────────────────────────────────────────────────┘
```

Rules:

- Per-conflict resolution, not per-file. Files with many conflicts are the common hard case.
- The result pane is directly editable — resolution is not limited to the three preset choices.
- Continue stays disabled until every conflict in every file is resolved, and says how many remain.
- **Abort restores the repository to exactly its prior state**, and says so before doing it.
- Navigation between conflicts and between files never loses a partial resolution.

## PR panel

A sidebar section, alongside branches, tags and stashes. The transport is described in
[ARCHITECTURE.md](./ARCHITECTURE.md#forge-integration).

- Open PRs where you are author, reviewer or assignee, grouped by role
- Per PR: number, title, review state, CI state, conflict state
- Selecting a PR shows its detail in the detail pane; opening it goes to the browser
- A stale-data marker when the last poll failed — a status indicator, never a dialog

Notifications are native OS notifications, individually toggleable, with per-repository enable and
quiet hours. Clicking one focuses hideGit with that PR selected.

| Event | Fires when | Default |
|---|---|---|
| `ReviewRequested` | You are added as a reviewer | on |
| `ReviewSubmitted` | Someone approves or requests changes on your PR | on |
| `PrCommented` | A new comment or review comment on your PR | on |
| `ChecksFailed` | CI transitions to failing on your PR | on |
| `ChecksPassed` | CI transitions to passing on your PR | off |
| `PrConflicting` | Your open PR becomes conflicted | on |
| `PrMerged` / `PrClosed` | A PR you authored is merged or closed | on |

Actions you take yourself never notify you, and multiple events in one poll collapse into a single
summary above a threshold.

## Keyboard shortcuts

`Cmd` on macOS, `Ctrl` elsewhere.

| Key | Action |
|---|---|
| `Cmd+O` | Open repository |
| `Cmd+Shift+O` | Clone repository |
| `Cmd+1` … `Cmd+9` | Switch repository tab |
| `Cmd+,` | Settings |
| **Navigation** | |
| `↑` / `↓` | Move selection in the focused list |
| `Tab` / `Shift+Tab` | Cycle panes: sidebar → graph → detail |
| `Cmd+F` | Search commits |
| `Cmd+P` | Command palette |
| `G` then `W` | Go to working directory |
| `G` then `B` | Branch switcher |
| **Working directory** | |
| `Space` | Stage / unstage selected file or hunk |
| `Cmd+Enter` | Commit |
| `Cmd+Shift+Enter` | Commit and push |
| `Cmd+Backspace` | Discard (always confirms) |
| **Remotes** | |
| `Cmd+Shift+F` | Fetch |
| `Cmd+Shift+P` | Pull |
| `Cmd+Shift+U` | Push |
| **Diff** | |
| `J` / `K` | Next / previous hunk |
| `Cmd+D` | Toggle unified ⇄ side-by-side |
| **Conflicts** | |
| `Cmd+]` / `Cmd+[` | Next / previous conflict |
| `Cmd+Shift+.` | Continue operation |

Shortcuts are user-remappable at M6. The scheme deliberately avoids fighting muscle memory built
in terminal Git tools.

## Theming

Dark is the default and is designed first. Light is a designed theme, not an inverted dark one.

```toml
[theme]
name = "hidegit-dark"

[theme.colors]
background = "#16181d"
surface    = "#1c1f26"
text       = "#e6e8ec"
muted      = "#8b93a3"
accent     = "#f65e17"
success    = "#3fb950"
warning    = "#d29922"
danger     = "#f85149"

# Graph lane colours, cycled. Must remain distinguishable
# under deuteranopia and protanopia — verified, not assumed.
lanes = ["#f65e17", "#3fb950", "#4c8dff", "#bc8cff", "#39c5cf", "#e36bb0"]
```

The accent is the orange from the ring in `assets/icon.png`, so the application
and its icon share one colour. It is used as drawn rather than lightened: it
reaches 5.13:1 as text on a panel, against the 5.15:1 of the blue it replaced.

Lane 0 follows the accent. Amber and red are not lane colours — they carry
meaning as `warning` and `danger`, and a lane painted the same red as a conflict
marker was a mixed signal. Removing them also keeps three warm lanes from
crowding an orange accent; the replacement set is measurably further apart than
the old one under both simulated deficiencies (minimum separation 0.063 against
0.056).

Constraints:

- Text contrast meets WCAG AA against its background in both themes.
- Lane colours are checked against common colour-vision deficiencies. A graph that is unreadable
  for 8% of men is a broken graph.
- Colour is never the only carrier of meaning — added/removed lines, CI state and conflict markers
  all have a shape or glyph as well.
- Custom themes are TOML files dropped in the config directory. A malformed theme falls back to
  the default with a warning; it never prevents startup.

## Interaction rules

**Destructive actions.** Discard, hard reset, force push, branch delete and stash drop each name
what will be lost — "Discard changes to 3 files? This cannot be undone." — and never rely on a
generic confirmation. Force push defaults to `--force-with-lease`; plain `--force` requires
deliberately selecting it.

**Long operations.** Anything that may exceed roughly 300ms shows progress with a real unit
(objects, commits, bytes) and a cancel button. Cancellation kills the subprocess and then reports
honestly if the repository was left mid-operation — including a stale `index.lock`, which is
reported rather than silently removed.

**Errors.** Recoverable errors appear inline where the action was attempted, with the action that
fixes them. Unexpected errors become a toast with a "copy details" action containing the argument
vector and Git's own stderr. Git's error messages are good; hideGit shows them rather than
paraphrasing.

**Empty states** carry the next action, not just an absence: no repositories → Open / Clone; no
PRs → connect GitHub, or "you have no open pull requests"; clean working directory → the last
commit.

**Drag and drop** (M5) on the graph performs merge and rebase. Every drop confirms with an explicit
description of the operation before anything runs — the discoverability of drag and drop is the
point, but not at the cost of an unintended rebase.
