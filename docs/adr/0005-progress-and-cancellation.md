# 0005 — Progress and cancellation by parsing stderr and killing the child

- **Status:** Accepted
- **Date:** 2026-07-29

## Context

[ADR-0002](./0002-git-backend-hybrid.md) put fetch, pull and push on the `git` CLI
side of the backend. M3 had to make those three report progress and be
cancellable, because [UI_SPEC](../UI_SPEC.md#interaction-rules) requires anything
that may exceed roughly 300ms to show progress in a real unit and offer a cancel
button.

A subprocess gives neither for free.

- Progress exists only as `--progress` output on **stderr**, in a human format
  that Git rewrites in place with a bare carriage return rather than a newline.
- There is no cancellation protocol at all. A `git` process stops when it is
  killed.
- Killing `git` mid-write can leave `index.lock` behind, and a stale lock stops
  every subsequent operation until something deals with it.

Two facts constrain the alternatives. `hidegit-core` must not depend on an async
runtime — that is what lets the domain logic be tested headless, and it is why
`ProgressSink` was declared as a trait object back in M1. And `git` must not be
allowed to block the UI thread, which the concurrency model already handles by
running every invocation on a blocking pool.

## Decision

Progress and cancellation are built on the subprocess boundary, in
`GitCommand::run_streaming`:

1. **stderr is read incrementally**, on its own thread, with chunks delivered over
   a channel that the caller polls with a timeout. Lines are split on both `\n`
   and `\r`, and each is offered to a hand-written parser for
   `[remote: ]<phase>: <n>% (<done>/<total>)`. Everything is also accumulated
   verbatim, so a failure still carries Git's own words.
2. **stdout is drained on a second thread**, because a command that fills the pipe
   buffer while the main thread reads stderr would deadlock.
3. **Cancellation is a `CancelToken`** — an `Arc<AtomicBool>` in `hidegit-core`,
   with no runtime attached. The reader loop checks it between chunks; on
   cancellation it kills the child, waits for it, and returns
   `GitError::Cancelled`.
4. **A leftover `index.lock` is reported, never deleted.** `Cancelled` carries the
   path when one is found. hideGit did not create it and cannot know whether the
   process that holds it is still working.

The UI adapts the sink to a `Task::stream`: a `ProgressSink` implementation pushes
into a channel, the blocking work runs on the blocking pool as usual, and the
stream yields progress messages and then the outcome. Operations carry a
monotonic id so a cancelled one's late messages cannot redraw the banner of the
operation that replaced it.

## Alternatives considered

**Use gitoxide's own `fetch`.** gitoxide implements fetch, with structured
progress and no parsing at all — genuinely better on both counts. Rejected because
fetch and push would then have two different authentication paths: gitoxide's
credential handling for fetch, and the user's credential helpers, SSH agent and
`GIT_ASKPASS` for push. ADR-0002 already flags this as a judgement call and as
the first thing to revisit if fetch performance disappoints; the reason it has not
been revisited is authentication, not speed.

Note the asymmetry this creates and its cost: hideGit parses Git's human output in
exactly one place, and that place is a maintenance surface. It is mitigated by
failing soft — an unrecognised line costs a summary entry, never the operation —
but it is a real cost, accepted deliberately.

**Poll `git` for progress some other way.** There is no other way. `--progress` on
stderr is the interface.

**Use `--porcelain` where it exists.** `git push --porcelain` puts a stable
tab-separated result on stdout, and the general rule in
[ARCHITECTURE](../ARCHITECTURE.md#shelling-out-safely) is that machine formats are
always preferred. It was tried and reverted, because `--porcelain` also moves the
*failure detail* off stderr: asked to push a stale lease, plain `git push` writes
`! [rejected] main -> main (stale info)` plus a hint saying what to do next, while
`--porcelain` leaves stderr holding only `error: failed to push some refs`. Since
Git's own message is the most useful thing hideGit has to say when a command
fails, losing it costs more than parsing the summary does. `git fetch --porcelain`
would not have been available anyway — it arrived in 2.41, past the 2.30 minimum.

**Delete a stale `index.lock` automatically.** Rejected. The lock may belong to a
live process, and removing one that does corrupts the index. Reporting it is worse
UX and better behaviour.

**Cancel by closing the pipes and letting `git` notice.** Unreliable: a fetch
stalled on a network read notices nothing, which is precisely the case where
Cancel matters most.

**A channel instead of a `ProgressSink` trait object.** Would put a specific
channel type — and in practice an async runtime — into `hidegit-core`, breaking
the constraint that makes the crate testable without one.

## Consequences

**What this buys.** Fetch, pull, push and clone all report in real units and stop
when asked, over any transport the user's `git` supports, with their own
credential helpers. One mechanism covers all four, and the same
`run_streaming` serves any future long operation — a rebase in M5 included.

**What it costs.**

- One human-output parser to maintain. Git's wording is not an interface and will
  change. Every parser here fails soft and is tested against captured output, so a
  change costs a summary line rather than correctness.
- Three threads per streamed command: the caller's, plus one each for stdout and
  stderr. Acceptable for an operation the user explicitly started; it would not be
  for something run in a loop.
- Cancellation resolution is bounded by the channel poll interval (50ms), not
  instant.

**What has to be revisited.**

- **Windows.** Killing a process there does not kill its children, and `git` spawns
  helpers — `git-remote-https`, an SSH client. A cancelled fetch may leave one
  behind. CI runs the suite on Windows, but no test exercises cancellation of a
  *slow* operation, because nothing in a hermetic suite is slow. This is a known
  gap, not a solved problem.
- **The credential-helper and SSH-passphrase paths cannot be tested hermetically.**
  Every remote in the test suite is a bare repository on a local path, which needs
  no authentication. `GIT_TERMINAL_PROMPT=0` means a missing credential fails fast
  rather than hanging, and `classify_remote_failure` turns that into an
  `AuthError` — but that classification is string matching on Git's stderr and is
  only ever exercised against synthetic input. Real credentials stay a manual
  check.
- **If gitoxide grows credential-helper support** good enough to replace the CLI
  for fetch *and* push, this ADR should be superseded rather than amended: the
  stderr parsing would go away entirely, which is a different design and not a
  tweak to this one.
