# 0007 — Drive interactive rebase by handing the plan through the environment

- **Status:** Accepted
- **Date:** 2026-08-09

## Context

[ADR-0002](./0002-git-backend-hybrid.md) puts rebase on the `git` CLI side of the
backend. M5 has to drive `git rebase --interactive` from a plan the user built in
the UI — reorder, squash, fixup, edit, drop — and interactive rebase has no
non-interactive interface. It writes a todo list to a file, runs
`GIT_SEQUENCE_EDITOR` on it, and reads back whatever the editor left.

Three facts constrain the answer.

- **The sequence editor runs through a shell.** Git builds
  `sh -c '<editor> "$@"' <editor> <todo-path>`, so whatever `GIT_SEQUENCE_EDITOR`
  holds is shell source, not an argument vector. This is the one place in hideGit
  where a string is unavoidably interpreted by `sh`, and
  [rule 3](../../CLAUDE.md) otherwise forbids exactly that.
- **A rebase plan is full of untrusted text.** It names commits from a repository
  that may have been cloned from anywhere. Commit subjects routinely contain
  `` ` ``, `$`, `;` and quotes — this project's own history has such a subject,
  deliberately, in `crates/hidegit-core/tests/history.rs`.
- **`git rebase` with no todo is a different, simpler command.** A plain rebase
  needs no editor at all, so whatever is decided here applies only to the
  interactive case.

There is no flag that supplies a todo list directly. `--edit-todo` edits the todo
of a rebase already running, which is the same problem one step later.

## Decision

The plan travels as **data in an environment variable**, and `GIT_SEQUENCE_EDITOR`
is a **fixed string literal** that copies it into the file Git hands it:

```
HIDEGIT_REBASE_TODO = "pick <sha>\nsquash <sha>\ndrop <sha>\n"
GIT_SEQUENCE_EDITOR = printf "%s" "$HIDEGIT_REBASE_TODO" >
```

The shell code is a constant in the source. Nothing derived from the repository
is ever concatenated into it, so no commit id, action or subject can be parsed as
shell syntax — the variable is expanded by the shell as a value, which is the
one context where arbitrary bytes are inert.

Two details follow from the todo format itself:

- **Only the action and the full commit id are written.** Git accepts a trailing
  subject and ignores it, so leaving it out keeps untrusted text out of a
  line-oriented file for no loss.
- **`Reword` is written as `edit`.** Git's own `reword` opens `GIT_EDITOR` at that
  point in the sequence, and hideGit stubs the editor out so no invisible window
  can hang the task — which would make `reword` silently keep the old message,
  the one thing a reword must not do. `edit` stops after applying the commit and
  hands control back, so the new message comes from hideGit's editor via an
  amend.

## Alternatives considered

**Interpolate the plan into the editor command.** The obvious approach, and the
reason this ADR exists. It puts commit ids and actions through `sh`, so it is
only as safe as the quoting, and the quoting has to be right on every platform's
shell forever. Rejected: it converts untrusted repository content into code.

**Point `GIT_SEQUENCE_EDITOR` at the hideGit binary, re-executed with a flag.**
Clean in principle and what some Rust clients do. It still has to survive the
same `sh -c`, so the binary's own path needs quoting — and that path is chosen by
whoever installed the application, not by hideGit. It also means the application
must be able to find its own executable reliably, which is awkward under
`cargo run`, an AppImage, and a macOS bundle in three different ways. Rejected as
more moving parts for the same shell exposure.

**Write a helper script to a temporary file and point the editor at it.** Moves
the quoting problem to the script's path and adds a temporary file to clean up on
a code path that can be killed mid-way. Rejected.

**Implement rebase as a sequence of cherry-picks.** hideGit already has
`cherry_pick`, and rebase is conceptually that in a loop. It would avoid the
editor entirely. Rejected because it re-implements what `git rebase` already
handles — `--autosquash`, `--update-refs`, hooks, the reflog entries a rebase
writes, empty-commit policy, and the exact reachability rules for what "onto"
means — and it would diverge from the user's own `git rebase` in ways that are
invisible until they matter. Delegating is the whole reason writes shell out.

**Wait for gitoxide to implement rebase.** It does not yet, and M5 is not
blocked on it. [ADR-0002](./0002-git-backend-hybrid.md) already describes
migrating operations back as gitoxide lands them.

## Consequences

- The interactive rebase path depends on a POSIX shell being present. This is not
  a new dependency: Git runs *every* editor this way, so a Git that can open an
  editor at all can run this. Git for Windows ships its own `sh`.
- `printf` must exist as a shell builtin or on `PATH`. It is a POSIX builtin in
  `sh`, `bash`, `dash` and Git for Windows' `bash`.
- The plan has no size limit in practice, but it does live in the environment,
  which has one. A rebase plan large enough to hit `ARG_MAX` would be a plan over
  roughly a hundred thousand commits; if that ever becomes real, the fallback is
  a temporary file whose *path* is the only thing in the variable.
- `Reword` and `Edit` are the same todo verb, so the outcome cannot distinguish
  "stopped to reword" from "stopped to edit". The UI knows which it asked for,
  since it built the plan. If that stops being true — a rebase resumed after a
  restart, say — the plan will need to be persisted alongside it.
- Nothing here is verified against Windows beyond CI running the test suite. The
  shell is Git's own, so the risk is low, but it is a claim from a green CI run
  rather than from someone rebasing on Windows.
