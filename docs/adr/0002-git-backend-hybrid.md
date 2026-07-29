# 0002 — Hybrid Git backend: gitoxide for reads, `git` CLI for writes

- **Status:** Accepted
- **Date:** 2026-07-29

The load-bearing decision in this project. It determines the performance ceiling, the build
requirements, and the one thing users must install before hideGit works.

## Context

A Git client needs a Git implementation. In Rust there are three realistic options — libgit2
bindings, gitoxide, or driving the `git` binary — and the choice is constrained by one hard fact
established during evaluation.

**gitoxide does not implement `push`.**

Verified against gitoxide's [`crate-status.md`][status] in July 2026. The picture at `gix` 0.86:

| Operation | Status in gitoxide |
|---|---|
| Fetch, clone (incl. shallow, bundles, negotiation) | Complete |
| Status (index ↔ worktree, rename tracking, untracked) | Complete |
| Diff (blob and tree, rename detection) | Complete |
| Blame (incl. worktree changes, rename tracking) | Complete |
| Commit creation | Complete |
| Credentials (`git credential`, helper cascade) | Complete |
| Worktree checkout | Complete |
| Merge | Plumbing exists; full workflow in progress |
| Rebase | Backends exist; plumbing incomplete |
| Cherry-pick | Plumbing exists (single and sequence) |
| Reset, stash | Partial |
| **Push** | **Not implemented** |

The read story is excellent and, on history traversal and status over large repositories,
measurably faster than libgit2 — which is precisely what the commit graph needs. The write story
stops short of the one operation no Git client can omit.

Meanwhile, the things a client must get right beyond raw Git operations are substantial:
credential helpers (including OS keychain helpers and enterprise SSO helpers), hooks,
`.gitattributes` filters, submodules, Git LFS, and SSH agent integration. Each is a source of
"works for me, fails for that one user" bugs, and each is already solved — correctly, and
consistently with the user's existing configuration — by the `git` binary on their machine.

## Decision

**A single `GitBackend` trait with one hybrid implementation.**

- **Reads go through `gix`:** log, status, diff, blame, refs, object access.
- **Writes go through the system `git` binary:** push, merge, rebase, pull, cherry-pick, revert,
  reset, checkout, stash, staging and commit.

`fetch` is placed on the CLI side despite gitoxide implementing it, because fetch and push share
credential handling and maintaining two authentication paths is not worth the gain. That is a
judgement call and the first thing to revisit if fetch performance disappoints.

**`git` is therefore a hard runtime requirement** (2.30 or newer, on `PATH`), checked at startup
with an actionable message rather than surfacing as a mystery failure at first push.

Every shell-out passes arguments as a vector — no shell is ever spawned — sets
`GIT_TERMINAL_PROMPT=0` and `LC_ALL=C`, parses only machine formats (`--porcelain=v2`, `-z`), and
surfaces `stderr` verbatim on failure. Full invariants in
[ARCHITECTURE.md](../ARCHITECTURE.md#shelling-out-safely).

## Alternatives considered

**git2 (libgit2 bindings), version 0.21.** The obvious choice: mature, complete, threadsafe,
covering every operation hideGit needs including push, merge and rebase. Rejected for three
reasons.

First, it reintroduces a C toolchain. Building requires a working C compiler and either a vendored
libgit2 or a system one; cross-compilation and reproducible builds both get meaningfully harder,
and every contributor pays the cost on first build.

Second, it is slower than gitoxide on exactly the operations that dominate this application's
workload — history traversal and status on large repositories. Given a target of 60fps on a
100,000-commit graph, the read path is not the place to accept a known-slower option.

Third — and least obvious — libgit2 does not use the user's Git configuration in the way users
expect. Credential helpers, hooks and LFS need reimplementation or explicit wiring, and the result
is a client that behaves subtly differently from the `git` the user already has configured. That
class of divergence produces bug reports that are extremely hard to reproduce.

**gitoxide only, deferring push and merge.** The purist option: entirely pure Rust, no external
binary, no C. Rejected because a Git client that cannot push is not a Git client — it is a
repository inspector. Shipping M3 would be blocked on upstream work with no committed timeline,
and the project would be unusable as a daily driver for an indefinite period. Waiting on someone
else's roadmap for a core feature is not a plan.

**`git` CLI only, for everything.** Simple, maximally compatible, always correct. Rejected on
performance: rendering a commit graph means walking history continuously as the user scrolls, and
paying process-spawn cost plus output parsing on every window is not compatible with the 60fps
target. Reads are where gitoxide's advantage is largest, and giving it up loses the main
performance argument for the project.

**gix reads + git2 writes.** Avoids the external binary while keeping fast reads. Rejected because
it brings back the C toolchain — the main cost of libgit2 — while *also* carrying two independent
Git object models in one codebase, and still leaves credential helpers, hooks and LFS to be
reimplemented. It pays both prices and collects only one of the benefits.

## Consequences

**What this buys**

- The fastest available read path, which is where this application spends its time.
- **No C toolchain.** `cargo build` works on a clean machine; cross-compilation stays simple.
- Credential helpers, hooks, `.gitattributes` filters, submodules, Git LFS and SSH agent support
  work exactly as the user has already configured them — inherited, not reimplemented.
- Push and rebase behaviour is, by construction, identical to the user's `git`. When something
  goes wrong the user can reproduce it on their own command line.
- Git's own error messages reach the user intact. They are better than anything we would write.

**What this costs**

- **`git` must be installed.** Acceptable for this audience — a Git client's users have Git — but
  it is a genuine deployment constraint and is stated in the README's requirements, not buried.
- **Output parsing is a maintenance surface.** Mitigated by using only machine-readable formats
  and pinning a minimum version, but porcelain formats do occasionally gain fields.
- **Process spawn cost per write operation.** Negligible relative to the network round-trip these
  operations involve, but it is real, and likely to be most noticeable on Windows.
- **Cancellation is messier.** Killing a `git` subprocess can leave `index.lock` behind. hideGit
  detects and reports a stale lock rather than silently deleting it.
- **Two mental models in the codebase.** Contributors need to know which side of the seam they are
  on. The `GitBackend` trait exists partly so that one file answers the question.

## Migration path

This decision is expected to be superseded. gitoxide is under active development and the gaps are
being closed.

Because everything goes through `GitBackend`, methods can move from the CLI side to the `gix` side
**one at a time**, with the trait's test suite as the contract that behaviour did not change.

A method moves when all of the following hold:

1. The gitoxide implementation is complete, not plumbing-only.
2. It integrates with the credential helper cascade, so authentication is unchanged for users.
3. Hooks fire the way the `git` binary fires them — a client that silently skips `pre-push` is
   worse than one that shells out.
4. The `GitBackend` test suite passes against the gix implementation with no test modifications.
5. Behaviour is verified on all three platforms.

`push` is the one to watch. When gitoxide lands it and it meets those conditions, the external
`git` requirement can be reconsidered — and if enough operations migrate, the requirement could be
dropped entirely, which would be a superseding ADR.

Until then, this stands.

[status]: https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md
