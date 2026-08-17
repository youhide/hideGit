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
| Welcome | Open, clone, recent repositories | M1 / M3 |
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
    stashes:     Vec<StashEntry>,
    remotes:     Vec<Remote>,          // configured, whether fetched or not
    divergence:  HashMap<String, Divergence>,   // ahead/behind, its own task
    selection:   Selection,            // Commit(id) | WorkingDirectory | Stash(n)
    detail:      DetailPane,
    prs:         PrPanelState,
    pending:     Option<Operation>,    // one at a time, cancellable
}
```

`pending` is one operation rather than a list: the toolbar replaces its buttons with the banner while
something is in flight, so two fetches racing for the same refs is not a state to support. It carries a
monotonic id, because a cancelled operation's last report can arrive after the one that replaced it has
started and must not redraw its banner.

`divergence` is a separate map loaded by its own task rather than a field on each `Branch`, because it
costs a commit walk per tracking branch and a refresh runs on every file save through the watcher. A
branch that tracks nothing is **absent** from it, which is different from being level with a remote and
renders differently.

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
branches, remotes, tags, stashes, submodules, worktrees, pull requests. Counts on section headers.

Every row carries its own controls, revealed as a glyph rather than hidden behind a menu bar — the
same idiom the staging rows use for `+`, `−` and `✕` — plus a `⋯` that opens its action sheet.
Section headings carry a `+` for the thing they hold: a new branch, a remote, a tag.

**Remotes are two levels**: the named remote, then the branches on it. Every *configured* remote
appears, fetched or not, because one that has been added and never fetched has no tracking refs and
grouping by ref name alone would say it does not exist. It shows "not fetched" rather than an empty
space.

**Ahead/behind has three states that must stay distinct.** A branch that tracks nothing shows nothing
at all, because `↑0 ↓0` would claim an upstream that does not exist. A branch level with its upstream
also shows nothing — the absence *is* the "no news" state, and a column of zeroes is noise. Only real
drift gets arrows, and only the non-zero side of it.

`LOCAL`, `REMOTES` and `TAGS` render even when empty, because their heading carries the `+` that
creates the first one and "you have no remotes" is a true and useful thing to read. `STASHES` does not:
a stash is made out of the working directory rather than from a heading, so stashing is offered from
the working-directory row instead, and only when there is something to stash. `SUBMODULES` follows
the same rule, for the same reason — a submodule comes from a `.gitmodules` somebody committed, not
from a heading — and it is absent on the overwhelming majority of repositories.

**`WORKTREES` appears only when there is more than one**, which is not the `STASHES` rule with a
different number. Every repository has a worktree — the one being looked at — so a section listing
exactly it would be a heading over a line the whole window already says. A *second* checkout is the
fact worth showing.

**The branch a worktree holds is the load-bearing half of that row.** A branch checked out in one
worktree cannot be checked out in another, so the row is the answer to a checkout that is being
refused. The right-hand column carries that branch — or Git's own `detached at <hash>` — except when
the worktree is locked or its directory is gone, which are states to act on and take the column
instead. Both reasons are in the tooltip along with the full path; the row shows the directory name,
because a path does not fit in 230px and its useful end is the last component.

A worktree whose directory is gone is **listed, not hidden**. It still holds its branch until
somebody runs `git worktree prune`, so hiding it would leave the refusal it causes with no visible
cause anywhere. `▸` marks the checkout being looked at, the same marker the local-branch row uses
for the branch `HEAD` is on.

Neither worktree rows nor submodule rows are selectable: another checkout, like another repository,
is not a place in *this* history to jump to.

**A worktree is made from the branch row, not from the `WORKTREES` heading.** A worktree is made
*out of* a branch, and the heading is absent on exactly the repositories where somebody wants a
second checkout — the same reason stashing is offered from the working-directory row rather than
from a `STASHES` heading. The branch sheet's "Check out in a new worktree…" asks for a directory
with the platform's own picker, then checks out into a folder named after the branch's last segment
inside it: checking out straight into a directory chosen for other reasons is how a home folder
acquires a `.git`, and `feat/graph` is not a directory name.

**A branch another worktree holds offers neither a checkout nor a second worktree.** Git refuses both
outright, and a control that always fails is worse than an absent one. The rest of that branch's
actions — merge, rebase, rename, delete — are unaffected and stay.

**A worktree row carries a `⋯` only where an action would work.** Three rows have none: the **main**
worktree, which cannot be removed; the **current** one, because `git worktree remove` will not take
the directory you are standing in; and a **locked** one, because locking it is how the user said
"leave this alone" and unlocking is how that is undone — hideGit does not offer a route past a
decision they already made.

The rest offer one action, and which one depends on whether the directory is still there. A live
worktree offers **Remove**, marked destructive, behind a confirmation that names the branch coming
back — "it stays, and becomes checkoutable here again" is the reassurance that makes the dialog safe
to accept. A worktree whose directory is gone offers **Clear the stale registration** instead, with
no confirmation: there is no directory left to lose, and "Remove" would be the wrong verb for the
only operation that would work.

**A submodule row says which of the two pointers is wrong.** A submodule is two commits that can
disagree: the one the superproject's index records, and the one the nested checkout is actually on.
The row carries Git's own column — `-`, a space, `+`, exactly what `git submodule status` prints —
and then names the commits. In sync it shows one hash; moved, it shows `recorded → checked out`,
because a single hash would leave the reader asking which of the two it was; never checked out, it
says "not initialised" rather than showing the recorded hash, which would claim something is there.

It carries a `⋯` **only when there is something to do**. A submodule already at the recorded commit
has no action that would change anything, and a control that does nothing is worse than no control —
so that row is informational, with its URL and state behind the tooltip. The other two offer one
action each: "Set up and check out" for the one with no checkout, "Return it to the recorded commit"
for the one that moved. Neither is marked destructive: `git submodule update` refuses rather than
discarding when the nested checkout has uncommitted work, and the commits it moves off stay in the
nested repository's own reflog.

There is still no *selection*. A submodule is not a place in this repository's history to jump to; it
is a pointer at another repository's, and clicking one has nothing to show in the detail pane.

**An update that succeeded and changed nothing is reported.** `git submodule update` exits 0 for a
submodule it left exactly as it found it, so "the operation succeeded" is not the same claim as "the
submodule is now current". The second one is what the user is owed, and a toast says so by name when
it is not true.

**Graph** — the centre. Virtualised: only visible rows are laid out and drawn. Refs are rendered as
badges on their commits. Selecting a row updates the detail pane. Full rendering rules in
[COMMIT_GRAPH.md](./COMMIT_GRAPH.md).

**The scrollbar is draggable, and it takes the click before the rows do.** A strip along the right
edge wider than the bar it draws answers to a press — an 8px target is one people miss, and missing
it would select whatever commit sits behind it. Dragging past the bottom edge keeps scrolling,
because that is how anyone reaches the end. Clicking the track jumps the thumb under the pointer and
then drags from its middle, so the page follows the cursor rather than leaping once and stopping.
The thumb has a minimum height: at a hundred thousand commits its proportional size is under a
pixel, and a thumb nobody can grab is the same as no thumb.

**Detail pane** — commit metadata and changed files when a commit is selected; the staging view
when the working directory is selected.

The file list carries a filter box. A commit that touches two hundred files is a scroll to find the
one you came for, and the file list is the one place in the interface where the thing you want is
already named on screen. Matching is substring over the path, case-insensitive. While a filter is on
the count says both numbers — *"3 of 42 file(s)"* — because "3 file(s)" over a commit that touched
forty-two is a quiet lie. Nothing matching says so rather than leaving an empty column, which reads
as a commit that changed nothing.

Rows address the commit's own file list, not the filtered one: an index taken from the visible list
would open whichever file happened to sit in that position. The filter is cleared when the selection
changes, since one written against the last commit's files would hide most of the next one's with
nothing on screen saying why. Typing into the box takes the keyboard the same way the commit composer
does, and clicking a row gives it back.

**Toolbar** — the operations reached constantly. Push shows its ahead-count; the notification bell
shows unread PR alerts. A control that cannot work is unavailable and carries *why* in its tooltip —
"A merge in progress", "Nothing to push from a detached HEAD" — rather than leaving that to be
guessed. With no remote configured at all the three buttons are replaced by the words "no remote",
because a button that cannot work is worse than an absent one.

**A network operation in flight** replaces the toolbar's buttons with a banner: the phase, a real unit
(*"Receiving objects 42/100"*), a progress bar, and Cancel. The bar is drawn only once a total is
known — a bar that has to guess is exactly the indeterminate spinner ruled out below — so before the
first report the banner says "starting…" rather than inventing a number. One operation at a time per
repository.

**Cancel only asks.** The worker notices, kills the subprocess, and reports back like any other
ending, so the banner clears in exactly one place; clearing it on the click would claim the operation
had stopped before it had. Afterwards a cancellation is silent, because it is what was asked for —
unless the killed `git` left `index.lock` behind, which is named, because hideGit will not delete it.

**A clone** gets the same banner above the whole screen, including the welcome screen it was started
from, because there is no repository yet to put it in.

**Opening a repository** gets a banner in the same place, for the same reason: there is no
repository to hang it off yet, and the window is showing the welcome screen or a different
repository entirely. It names the repository and the step — *"Counting commits… hideGit"* — because
two paths given on the command line open at the same time, and "Counting commits…" twice says
nothing about which.

Named steps, no bar. Nothing knows how long counting a hundred thousand commits will take, and a bar
that has to guess is the indeterminate spinner ruled out above. The steps are Opening, reading
branches and tags, reading the working directory, counting commits, reading history — counting is
where the wait is: the walk-and-order pass measures **1.19 s over a hundred thousand commits**. No
Cancel either: the read is already off the UI thread, and stopping half way would leave a tab that
is neither open nor closed.

The banner clears **after** the repository is on screen, not when its result arrives — the other
order blanks it a frame early.

**In-progress repository state** — mid-merge, mid-rebase — gets a second, persistent banner below
that one: *"Rebasing feat/graph onto main — 3 of 7 commits"* with Continue / Skip / Abort (M5). It
cannot be dismissed, because the repository genuinely is in that state and hiding it is how people
lose work.

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

**`Space` asks about focus before it acts**, and the story of why is worth keeping.

It was unbound through M2 and M5. iced 0.14 keeps text-input focus inside the widget, `on_input` is
the only signal it offers, and wrapping a field in a `mouse_area` to catch the click that grants
focus swallows that click so the field never focuses at all. A global binding therefore could not
know the commit message field was being typed into until its first keystroke had already arrived —
and `Space` is the one bare key whose leak would stage a file. `j`/`k` stayed bound because their
leak only moves a highlight.

The conclusion was wrong, and M6 found out how. Focus is not observable through `on_input`, but it
*is* observable through a `find_focused` widget operation — a different mechanism, not a harder push
on the same one. `Space` now runs that query and acts only when nothing holds focus.

The operation **ignores widgets with no id**, which is why the composer's two fields carry
`COMPOSER_FIELD_IDS`, and why the file filter above a commit's changed files carries one too.
Without them the query answers "nothing focused" while a message is being typed, and the guard is
worse than useless — it looks like it works.

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

## Layers over the screen

Four of them, stacked in this order: toasts, then the action sheet, then the prompt, then the
confirmation — which goes last because it is what a sheet's destructive item raises, so it has to be
able to sit over the sheet that raised it.

A **confirmation** is modal and names what will be lost — *"Changes to doomed.txt will be lost.
This cannot be undone."* — never a generic "are you sure?". Its accept button carries the verb
("Discard"), not "OK". Cancel comes first and unemphasised, because the safe choice should not be
the one that takes aim. While it is up it owns the keyboard: `Esc` cancels, `Enter` accepts, and
nothing else reaches the screen behind it.

An **action sheet** is the list of things that can be done to one item, titled with the item rather
than with a question — the user knows what they clicked, and repeating "what would you like to do?"
wastes the one line that could name it. Destructive items are distinguishable by colour *and* by a
glyph. `Esc` dismisses; `Enter` does nothing, because a list one of whose items may be "Delete" must
have no default action. Choosing an item closes the sheet as the action goes out, so it cannot end up
sitting over a toast reporting that the action failed.

**It is a centred card, not a positioned context menu.** iced 0.14 gives a `button`'s `on_press` no
cursor coordinates and has no popover widget, so anchoring one where the click happened would mean a
custom widget with its own overlay layer. A sheet says the same thing, works from the keyboard, and
is one mechanism for branches, remote branches, remotes, tags and stashes.

A **prompt** is a modal that collects text before acting — a branch name, a remote's URL, a stash
message. It is a sibling of the confirmation rather than a variant of it, because a confirmation's
action is fixed and an action that depends on what the user types cannot be. `Esc` dismisses, `Enter`
accepts, and its primary button is the accent rather than the danger colour so creating a branch does
not wear the same red as discarding one. Every field is required except a stash's message, which Git
will invent.

A **toast** reports a failure and keeps Git's own stderr verbatim rather than paraphrasing it,
because that text is the most useful thing hideGit has to say when a command fails. Success is
silent: the refresh that follows an operation is its result, and a toast per click is noise. There is
one exception — a push that was *partly* refused says so, because a push that appears to have worked
and did not is exactly the lie the silence rule exists to avoid.

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
- **A file stored with Git LFS gets a placeholder too, and says what it can.** What Git stores for an
  LFS-tracked file *is* the pointer — three lines naming an object and a size — so a diff of one is a
  diff of the plumbing. hideGit recognises the pointer and reports the size instead: `1.0 KiB →
  4.0 KiB` when both sides are pointers, `unchanged` when the object is the same, `changed` when two
  different files happen to be the same length, and "now tracked by LFS" / "no longer tracked by LFS"
  when a file moves into or out of it — a change of *storage* rather than of content, which a
  three-line pointer diff would say neither of. This needs no `git-lfs` installed: the pointer is a
  text file, and recognising it is reading, not tooling
- **Opening a repository that uses LFS without `git-lfs` installed raises a toast naming the tool.**
  Said on the way in, because the symptom arrives before the question does: pointer text where the
  content should be reads as corruption, and naming what is missing is the whole fix. Read once, at
  open — neither half of the question changes while a repository is on screen, and a toast that came
  back on every file save would be worse than what it reports
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

**A pull request is listed once, under its strongest role** — author, then reviewer, then assignee
— because listing one you wrote *and* were assigned to under two headings would make the section's
count disagree with what is under it. Having already reviewed something keeps it under *awaiting
your review*: otherwise approving it drops it the instant you act, and there is nowhere left to see
that checks later failed on it.

**Check and review state are glyphs in fixed positions, not colours.** Passing and failing would
otherwise differ only in hue. "No checks configured" shows nothing at all, which is a different
thing from "no check has reported yet" and must not get the same marker. A conflict marker appears
only when a pull request is *known* to conflict — GitHub computes mergeability lazily, and marking
its "still checking" answer would be a guess the user acts on.

**The section is absent when no remote names a forge repository.** A repository whose only remote is
a path on disk has no pull requests to have, which is not the same as having none — and every remote
in hideGit's own test suite is exactly that. Where several remotes qualify, `origin` wins: in a fork
configured with `origin` for your copy and `upstream` for the project, yours is the one you are
working in.

**Four states that look alike as an absence, and mean opposite things**, so each gets its own row
with its own next action: not signed in → Connect; signed in but the app is not installed on this
repository → Install, with the URL; installed with nothing open → "no open pull requests"; the last
poll failed → a stale marker *above the previous result*, which stays on screen. A network blip must
not read as every pull request having been closed.

**No keychain, no forge features.** On a machine with no credential store there is nothing to retry
and nothing to dismiss, so it is not a toast — the panel says so where somebody would look for pull
requests, which is where the question comes up.

Notifications are native OS notifications, individually toggleable, with per-repository enable and
quiet hours. Clicking one focuses hideGit with that PR selected.

Preferences live in `config.toml` under `[alerts]`, and every value has a working default, so the
section need not exist:

```toml
[alerts]
enabled = true          # the master switch; the panel keeps working either way
muted   = ["owner/noisy"]   # repositories to stay silent about, by owner/name

[alerts.events]
checks_passed = true    # off by default; see the table below

[alerts.quiet_hours]
enabled = true
from = 22               # local hours, `from` inclusive and `to` exclusive
to   = 8                # `from > to` wraps midnight, which is the usual case
```

Toggles are named fields rather than a map keyed by event name, so a misspelled setting is an error
rather than one that silently turns nothing on.

| Event | Fires when | Default |
|---|---|---|
| `ReviewRequested` | You are added as a reviewer | on |
| `ReviewSubmitted` | Someone approves or requests changes on your PR | on |
| `PrCommented` | A new comment, or a new review thread, on your PR | on |
| `ChecksFailed` | CI transitions to failing on your PR | on |
| `ChecksPassed` | CI transitions to passing on your PR | off — every other event needs your attention; a build going green is the *absence* of a problem |
| `PrConflicting` | Your open PR becomes conflicted | on |
| `PrMerged` / `PrClosed` | A PR you authored is merged or closed | on |

Actions you take yourself never notify you, and multiple events in one poll collapse into a single
summary above a threshold.

**One gap worth knowing about.** `PrCommented` fires on a change in the number of issue comments
plus review threads, so a *reply inside an existing review thread* does not produce a notification.
Catching those would mean reading every thread on every pull request on every poll, which is the
N+1 that [ADR-0006](./adr/0006-poll-pull-requests-over-graphql.md) exists to avoid. A new comment
and a new review thread both do notify.

## Keyboard shortcuts

`Cmd` on macOS, `Ctrl` elsewhere.

| Key | Action |
|---|---|
| `Cmd+O` | Open repository |
| `Esc` | Close the device-code dialog — the sign-in continues in the background |
| `Cmd+Shift+O` | Clone repository — checked before the unshifted `O`, or it would open a picker |
| `Cmd+1` … `Cmd+9` | Switch repository tab. Past the last tab does nothing rather than clamping to it |
| `Cmd+,` | Settings |
| `Cmd+/` | This table, on screen. Modified deliberately: a bare `?`, which terminal tools use, would hit the same hazard `Space` did and open the reference instead of typing the first character of a commit message |
| **Navigation** | |
| `↑` / `↓` | Move selection in the focused list |
| `PageUp` / `PageDown` | Move by twenty |
| `Tab` / `Shift+Tab` | Cycle panes: sidebar → graph → detail |
| `Cmd+F` | Search commits — also a toolbar button, since a shortcut is how you use a thing you already know exists |
| `Cmd+P` | Command palette. Substring match over the command titles, not fuzzy: a fuzzy match that puts "Discard the selected changes" under `push` because the letters appear in order is worse than no match, and the list is fifteen rows |
| `G` then `W` | Go to the working directory |
| `G` then `B` | Branch switcher — **not built**: the chord mechanism exists, the switcher it would open does not |
| **Working directory** | |
| `Space` | Stage / unstage the selected file |
| `Cmd+Enter` | Commit |
| `Cmd+Shift+Enter` | Commit and push |
| `Cmd+Backspace` | Discard (always confirms) |
| **Remotes** | |
| `Cmd+Shift+F` | Fetch every remote, pruning |
| `Cmd+Shift+P` | Pull |
| `Cmd+Shift+U` | Push |

All three carry a modifier deliberately: the guard that stops bare keys reaching the screen while a
text field has focus only lets modified keys through, and `Cmd+Shift+U` is wanted precisely while a
commit message is being written. They are matched case-insensitively, because with Shift held the
character iced reports is the shifted one on most layouts and hard-coding either case would make the
binding depend on the keyboard.
| **Diff** | |
| `J` / `K` | Next / previous hunk |
| `Cmd+D` | Toggle unified ⇄ side-by-side |
| **Conflicts** | |
| `Cmd+]` / `Cmd+[` | Next / previous conflict |
| `Cmd+Shift+.` | Continue operation |

**Focus.** `Tab` cycles sidebar → graph → detail, and the pane that has the keyboard carries a
one-pixel outline in the accent colour. Only the colour changes with focus, never the width: a
border that appears takes a pixel from the pane it is on, and text that reflows as focus moves is
worse than no ring. The ring clears the 3:1 WCAG bar for non-text against both pane backgrounds,
asserted by test.

`↑` / `↓` act on the pane that has focus. In the graph they move the commit selection; in the
staging view they move the file row — the row `Space` stages, which before this could only be moved
with the mouse. With nothing selected the first press enters the list rather than stepping into its
second row. The sidebar has no row model, so focusing it shows the ring and nothing else; its
entries are reached by clicking or through the command palette.

**Chords.** `G` on its own arms the next key and does nothing else. A binding function that maps one
press to one message cannot remember the press before it, so the pending prefix is state: `G` returns
a message that records it, and the key after resolves against it. Anything that does not complete a
chord **cancels** rather than falling through to its ordinary binding — after `G`, a stray `J` must
not step a hunk and leave the chord armed for the key after that. The prefix is cleared only by the
key that completes or cancels it, never by an unrelated message, so a poll landing between `G` and
`W` does not eat the chord. `G` is bare, so it is guarded by the same rule as `J` and `K`: typing
"Go to sleep" into a commit message arms nothing.

`Space` is the one binding that cannot be decided from the key alone. iced keeps text-input focus
inside the widget, and the `editing` flag that guards every other bare key is only set once a
keystroke has *arrived* — so clicking into the commit message and pressing `Space` would stage a
file. It therefore asks first, with a `find_focused` widget operation, and acts only when nothing
holds focus. That operation **ignores widgets with no id**, which is why the composer's two fields
carry `COMPOSER_FIELD_IDS`: without them the query would answer "nothing focused" while a message
was being typed, and the guard would be worse than useless — it would look like it worked.

M2 deferred `Space` on the grounds that focus was not observable at all. That was true of the
`on_input` signal it was reaching for, and not of the widget operation, which is the fix.

**Remapping.** `[shortcuts]` in `config.toml` maps a command to a chord:

```toml
[shortcuts]
push = "Cmd+U"
refresh-pull-requests = "Cmd+R"    # a command that ships with no chord at all
```

The commands are the ones the command palette lists, by name. That boundary is deliberate:
navigation — the arrows, `Tab`, `J`/`K`, `Space`, the chord prefix and the keys a panel owns while it
is up — stays fixed, because those are how you get *out* of things, and a file that can strand you
inside a panel can lock you out of the application.

Rebinding a command **moves** it: its default chord stops working, or both would fire and the remap
would look ignored. An explicit line beats a built-in binding, and taking one is reported rather than
done in silence — losing `J` with nothing to connect it to is worse than being told. Two commands
cannot share a chord; the first wins and the second is told why, so the order of a TOML table is not
load-bearing. A name that is not a command and a chord that will not parse are reported on screen and
ignored, never fatal. Both the reference and the palette print the chord a command answers to *now*.

The scheme deliberately avoids fighting muscle memory built
in terminal Git tools. Rows marked **not built** are bindings this table has promised since M1 and
that nothing dispatches yet; they are listed rather than removed so the gap is visible.

**This table is on `Cmd+/`, and a test keeps the two in step.** The reference panel is a table in
`widget/shortcuts.rs` rather than prose, and a test parses every chord in it, feeds each to the
binding function, and compares the two sets **in both directions** — a row for a binding that does
not exist fails, and so does a binding added without a row. A reference that drifts is worse than
none, because it is believed.

Each palette row carries the chord that also runs it, and a second test asserts every chord the
palette prints is one the reference lists — so the palette teaches shortcuts rather than competing
with them. A command with nothing to act on is absent from the list rather than present and inert:
a row that does nothing when pressed reads as the application being broken.

That test found three bindings nobody chose. `↑`, `↓`, `PageUp`, `PageDown` and `Tab` were matched
on the key alone, so `Cmd+↑` moved the selection and `Cmd+Tab` cycled panes — on macOS both mean
something else entirely. And a panel that owns the keyboard only owned the unmodified half of it, so
`Cmd+Esc` closed the search and `Cmd+Enter` jumped to a commit from inside it. All of them are now
guarded; a panel that owns the keyboard owns all of it.

## Theming

Dark is the default and is designed first. Light is a designed theme, not an inverted dark one —
`hidegit-light` ships alongside it, selected with `theme.name` in `config.toml`.

What "not inverted" means concretely, since it is easy to say and easy to skip:

- **The brand orange is darkened for light.** As drawn it reaches only 3.21:1 on light's near-white
  panel, below the bar for the text it is used for. Dark uses it as drawn *because it clears the bar
  there*; light applies the same rule and gets a different hex.
- **A panel stays raised in both.** Light is a grey page with near-white panels, not a white page
  with grey ones — inverting that relationship makes every panel read as sunken.
- **Selection colours live in the palette, not as an alpha over the accent.** An alpha tuned on a
  dark background does not transfer: the wash that reads as a glow over near-black reads as a stain
  over near-white, and a warm accent over a cool grey page composites to a muddy pink. Each theme
  names its own `selection` and `selection_idle`.
- **Every contrast constraint below is asserted for both palettes.** They were asserted for dark
  only until light arrived, which is exactly how an unchecked light theme ships.

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
- Diff text is syntax-highlighted, with every colour lifted to WCAG AA — see below.
- Custom themes are TOML files dropped in a `themes` directory beside `config.toml`, one per
  theme. A malformed theme is skipped with its reason shown on screen; it never prevents startup.

A theme is named by its file — `themes/zinc.toml` is `theme.name = "zinc"` — so two files cannot
claim the same name, and a file may not take a name that ships. Colours are `#rrggbb`, or
`#rrggbbaa` where something sits over what is behind it:

```toml
# themes/zinc.toml — every key is optional.
based_on = "hidegit-light"    # defaults to hidegit-dark
accent = "#0550ae"
selection = "#e9eaed"
lanes = ["#0550ae", "#1a7f37", "#8250df", "#106e75", "#bf3989", "#b8410a"]
```

Every colour not named is inherited from `based_on`. A palette has eighteen of them, and requiring
all eighteen would make changing the accent a twenty-line file with eighteen chances to be rejected
over a colour the author never cared about. `lanes` is all six or none — accepting four and
repeating them would quietly change how many lines the graph can tell apart.

An unrecognised key is an error, not a shrug: a file with `acccent` in it and no complaint looks
exactly like hideGit ignoring the setting, which is the failure the settings screen exists to stop.
The shipped palettes are not overridable in place; `based_on` is how you start from one.

**Syntax highlighting** is syntect, through `iced::highlighter`, driven a line at a time. A diff is
not a document — it is two documents interleaved — so there are **two parsers, one per side**: the
old side sees context and removed lines, the new side sees context and added ones. Feeding one
parser a removed line and then the added line replacing it leaves both wrong.

It is approximate at the top of a hunk, deliberately. A hunk starts where the diff starts, not at
the top of the file, so a line inside a block comment that opened fifty lines earlier reads as code.
Fixing that means reading every blob of every commit anyone clicks on. The colours are a reading
aid; the text is the truth.

Every colour is lifted to 4.5:1 against the row it sits on. Syntax themes dim comments on purpose:
measured against hideGit's diff backgrounds, the worst colour in each of the five syntect themes
lands between 2.33:1 and 2.62:1 — below what this document guarantees for text. Rather than drop the
guarantee for the pane people read longest, each colour that falls short is moved along lightness by
bisection until it clears, and no further. Which syntect theme is used follows the palette's
background luminance, not the theme's name, so a custom theme gets the right one too.

Context lines lose their muting, because every line now carries its own colours. What separates
added from removed is the marker glyph and the row background — which is what has to carry it
anyway, since colour alone never does. A file whose extension syntect does not know, or a diff over
4,000 lines, renders plain.

## Interaction rules

**Destructive actions.** Discard, hard reset, force push, branch delete, tag delete, remote removal and
stash drop each name what will be lost — "Discard changes to 3 files? This cannot be undone." — and
never rely on a generic confirmation. Force push defaults to `--force-with-lease`, and its
confirmation says what the lease protects; plain `--force` requires deliberately selecting it and says
that someone else's commits would become unreachable.

**A refusal is surfaced, never worked around.** Deleting an unmerged branch is refused by Git and the
refusal is shown; hideGit does not retry with `--force` behind the user's back. A checkout blocked by
local changes fails with Git's own message naming the files; hideGit does not stash on the user's
behalf, because that moves their work somewhere they never asked for. Losing commits is always
something the user chose.

**An action that cannot work is absent, not present and refusing.** Checkout is not offered for the
branch you are standing on, Delete is not offered for it either, and neither is offered for the only
branch in the repository.

**Long operations.** Anything that may exceed roughly 300ms shows progress with a real unit
(objects, commits, bytes) and a cancel button. Cancellation kills the subprocess and then reports
honestly if the repository was left mid-operation — including a stale `index.lock`, which is
reported rather than silently removed. See
[ADR-0005](./adr/0005-progress-and-cancellation.md).

**Errors.** Recoverable errors appear inline where the action was attempted, with the action that
fixes them. Unexpected errors become a toast with a **Copy details** action containing the argument
vector and Git's own stderr — or, for a forge failure, the provider's own message. Git's error
messages are good; hideGit shows them rather than paraphrasing. The copy action matters more than it
looks: an `iced` `text` is not selectable, so without it the one thing a bug report needs can be read
on screen and taken nowhere. Copying does not dismiss the toast.

**Empty states** carry the next action, not just an absence: no repositories → Open / Clone; no
PRs → connect GitHub, or "you have no open pull requests"; clean working directory → the last
commit.

**Drag and drop** on the graph performs merge and rebase. Drag a branch badge onto another and the
action sheet opens naming both branches on every entry; nothing runs until one is chosen. The
discoverability of the gesture is the point, but not at the cost of an unintended rebase.

Three things fall out of that:

- A press only becomes a drag once the pointer has travelled a few pixels, so clicking a badge still
  selects its commit. Without the threshold, a slightly unsteady click would arm a merge.
- Both operations act on the branch that is **checked out**, so the direction of the drag does not
  change what is on offer — only which two branches were named. The title records the drag as it
  happened; the entries name both branches in full, because a gesture is exactly the thing whose
  direction people misread.
- Dropping between two branches neither of which is checked out says so rather than checking one out
  silently. Tags are not draggable at all: no operation merges or rebases onto one, and offering the
  gesture would promise it.
