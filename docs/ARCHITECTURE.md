# Architecture

How hideGit is put together, and why. Decisions summarised here are argued in full in the
[ADRs](./adr/README.md).

**Status:** M1 has landed, so the read half of this document describes code that exists. The write
half — everything from `stage` onward — is still design: those methods are declared and return
`NotImplementedYet` with the milestone they arrive in.

## Contents

- [Overview](#overview)
- [Crate layout](#crate-layout)
- [The `GitBackend` seam](#the-gitbackend-seam)
- [Shelling out safely](#shelling-out-safely)
- [Concurrency model](#concurrency-model)
- [Domain model](#domain-model)
- [Commit graph](#commit-graph)
- [Forge integration](#forge-integration)
- [Error taxonomy](#error-taxonomy)
- [Configuration and state](#configuration-and-state)
- [Testing strategy](#testing-strategy)
- [Known limits](#known-limits)

## Overview

Three ideas carry the design:

1. **A hybrid Git layer.** gitoxide for reads, the system `git` binary for writes. Not a
   compromise reached reluctantly — it buys both speed and correctness. See
   [ADR-0002](./adr/0002-git-backend-hybrid.md).
2. **A domain core with no dependencies on UI or network.** `hidegit-core` can be exercised
   headless, in CI, without a window or a token. That constraint is load-bearing; it is what keeps
   the graph algorithm and diff model testable.
3. **The Elm architecture, honestly applied.** iced gives `State`, `Message`, `update`, `view`.
   Every side effect is a `Task` or a `Subscription`. Nothing blocks the UI thread. Ever.

```
                      ┌─────────────────────────────────┐
                      │            hidegit              │  binary: wiring, config, logging
                      └────────────────┬────────────────┘
                                       │
                      ┌────────────────▼────────────────┐
                      │           hidegit-ui            │  iced: screens, widgets, theme
                      └───────┬─────────────────┬───────┘
                              │                 │
              ┌───────────────▼──────┐   ┌──────▼────────────────┐
              │    hidegit-core      │◄──│    hidegit-forge      │
              │  git domain + I/O    │   │  GitHub / PR alerts   │
              └──────┬────────┬──────┘   └───────────┬───────────┘
                     │        │                      │
                  ┌──▼──┐  ┌──▼───────┐         ┌────▼─────┐
                  │ gix │  │ git CLI  │         │ octocrab │
                  └─────┘  └──────────┘         └──────────┘
```

Dependencies point downward only. `hidegit-core` depends on neither `iced` nor `hidegit-forge`.

## Crate layout

| Crate | Responsibility | Must not depend on |
|---|---|---|
| `hidegit-core` | Git domain types, `GitBackend` and its hybrid implementation, commit graph layout, diff model | `iced`, `hidegit-forge`, any HTTP client |
| `hidegit-forge` | `Forge` trait, GitHub implementation, OAuth device flow, token storage, PR polling | `iced` |
| `hidegit-ui` | Screens, widgets, theme, the commit-graph canvas, keyboard handling | — |
| `hidegit` | Binary: CLI arguments, config loading, logging/tracing setup, window bootstrap | — |

The rule about `hidegit-core` is the one that will be under pressure. When a graph layout function
"just needs a colour" or a diff "just needs to know the viewport width", the answer is to return
data and let `hidegit-ui` decide, not to reach upward.

### Third-party crates

| Crate | Version | Used for | Since |
|---|---|---|---|
| `iced` | 0.14 | GUI toolkit | M1 |
| `gix` | 0.86 | All Git read operations | M1 |
| `tokio` | 1 | Blocking pool for Git work | M1 |
| `rfd` | 0.15 | Native file/folder pickers and startup dialogs | M1 |
| `directories` | 6 | Platform config, cache and data paths | M1 |
| `serde` + `toml` | 1 / 0.9 | Configuration | M1 |
| `time` | 0.3 | Commit timestamps with their recorded offset | M1 |
| `tracing` | 0.1 | Structured logging | M1 |
| `thiserror` | 2 | Error types in libraries | M1 |
| `criterion` | 0.7 | Benchmarks (dev only) | M1 |
| `octocrab` | 0.54 | GitHub API | M4 |
| `keyring` | — | OS keychain access for forge tokens | M4 |
| `notify-rust` | — | Native desktop notifications | M4 |

Versions are pinned in the workspace `Cargo.toml` and inherited by every crate, so a bump happens
in one place. Crates whose milestone has not arrived carry no version here — recording a number
for something no code imports produces documentation that is wrong before it is used.

`gix` is taken with its default features and **no network transport**: gitoxide is the read half
only, and everything that talks to a remote shells out to `git`.

**`git2`/libgit2 is deliberately absent.** It was evaluated and rejected; see
[ADR-0002](./adr/0002-git-backend-hybrid.md). Adding it back is a decision that needs a superseding ADR.

## The `GitBackend` seam

All Git access goes through one trait. There is exactly one implementation, `HybridBackend`, which
routes each method to either `gix` or the `git` binary.

```rust
pub trait GitBackend: Send + Sync + Debug {
    fn open(path: &Path) -> Result<Self, GitError> where Self: Sized;

    // ---- read: gix ----------------------------------------------------
    fn workdir(&self) -> &Path;
    fn git_dir(&self) -> &Path;
    fn head(&self) -> Result<Head, GitError>;
    fn refs(&self) -> Result<Refs, GitError>;
    fn repo_state(&self) -> Result<RepoState, GitError>;
    fn log(&self, spec: &RevSpec, page: LogPage) -> Result<Vec<Commit>, GitError>;
    fn commit_count(&self, spec: &RevSpec) -> Result<usize, GitError>;
    fn commit(&self, id: ObjectId) -> Result<CommitDetail, GitError>;
    fn diff(&self, target: &DiffTarget) -> Result<Diff, GitError>;
    fn read_blob(&self, id: ObjectId) -> Result<Blob, GitError>;
    fn status(&self) -> Result<WorktreeStatus, GitError>;
    fn blame(&self, path: &Path, at: ObjectId) -> Result<Blame, GitError>;  // M6
    fn invalidate(&self);

    // ---- write: git CLI -----------------------------------------------
    fn stage(&self, paths: &[&Path]) -> Result<(), GitError>;                        // M2
    fn stage_patch(&self, patch: &Patch) -> Result<(), GitError>;                    // M2
    fn unstage(&self, paths: &[&Path]) -> Result<(), GitError>;                      // M2
    fn discard(&self, paths: &[&Path]) -> Result<(), GitError>;                      // M2
    fn create_commit(&self, message: &str, opts: CommitOpts) -> Result<ObjectId, GitError>;  // M2
    fn checkout(&self, target: &CheckoutTarget) -> Result<(), GitError>;             // M3
    fn fetch(&self, remote: &str, p: &dyn ProgressSink) -> Result<FetchOutcome, GitError>;   // M3
    fn push(&self, remote: &str, spec: &PushSpec, p: &dyn ProgressSink) -> Result<(), GitError>;  // M3
    fn stash(&self, op: &StashOp) -> Result<StashOutcome, GitError>;                 // M3
    fn merge(&self, from: &str, opts: &MergeOpts) -> Result<MergeOutcome, GitError>; // M5
    fn rebase(&self, onto: &str, plan: &RebasePlan) -> Result<SequenceOutcome, GitError>;    // M5
    fn cherry_pick(&self, ids: &[ObjectId]) -> Result<SequenceOutcome, GitError>;    // M5
}
```

The whole surface is declared from M1 so the read/write split is visible in one file, and a method
whose milestone has not landed returns `GitError::NotImplementedYet { operation, milestone }`
rather than being absent. The types the write half takes are provisional: each is designed
properly in the milestone that implements it.

`log` is paged rather than limited because the graph only lays out the rows around the viewport. A
walk's topological order is memoised — it is the expensive part of drawing a graph and it does not
change until the repository does — and only the requested page is hydrated into full `Commit`
values. `invalidate` is what says that memo is stale.

**Topological order is computed by hideGit, not by gitoxide.** `gix` offers breadth-first and
date-ordered traversal but not `--topo-order`, so the date-ordered walk is corrected afterwards by
Kahn's algorithm with commit date as the tiebreak. Date order alone is not sufficient: clock skew
and rebases both produce commits whose timestamps predate their children, which would draw edges
pointing upward.

### Why a trait with one implementation

Not speculative abstraction. It exists for three concrete reasons:

1. **It is the migration path.** As gitoxide lands `push` and a complete rebase workflow, methods
   move from the CLI side to the `gix` side one at a time. The trait's test suite is the contract
   that says the move did not change behaviour.
2. **It makes the split auditable.** One file answers "what do we shell out for, and why is this
   one still on the CLI side?" Without the seam, the answer is scattered across the codebase.
3. **It enables a fake.** A `FakeBackend` lets `hidegit-ui` be tested against scripted repository
   states, including error states that are painful to produce with a real repository.

`fetch` sits on the CLI side despite gitoxide implementing it, because fetch and push share
credential handling and it is not worth having two authentication paths. That is a judgement call
and it is the first thing to revisit if fetch performance disappoints.

## Shelling out safely

The subprocess boundary is the most security-sensitive part of hideGit, because branch names,
paths and remote URLs come from repositories that may have been cloned from anywhere. A single
helper wraps every invocation and enforces:

| Invariant | Reason |
|---|---|
| Arguments passed as a vector; **no shell is ever spawned** | Metacharacters in a branch name or path are never interpreted |
| `--` before operands wherever Git accepts it | A ref or path starting with `-` cannot be absorbed as a flag |
| `GIT_TERMINAL_PROMPT=0` | A subprocess blocking on a hidden prompt is an app that appears to hang |
| `GIT_OPTIONAL_LOCKS=0` for read-adjacent commands | Background invocations never contend for `index.lock` |
| `LC_ALL=C` | Git's output does not shift under the user's locale |
| Machine formats only: `--porcelain=v2`, `-z`, `--null` | Human output is not a stable interface |
| Controlled environment, not inherited wholesale | Fewer surprises from the user's shell configuration |
| `stderr` surfaced to the user **verbatim** on failure | Git's error messages are good. Paraphrasing them destroys information |
| Every invocation logged at `debug` with its full argument vector | Bug reports become diagnosable |

Long-running commands stream progress by parsing `--progress` output on stderr and are cancellable
by killing the child process — with the caveat that a killed `git` may leave `index.lock` behind,
so cancellation checks for and reports a stale lock rather than silently deleting it.

## Concurrency model

iced's `update` runs on the UI thread. Blocking it drops frames, so nothing blocking runs there.

| Work | Mechanism |
|---|---|
| `gix` calls (blocking) | `Task::perform` onto `tokio`'s blocking pool |
| `git` subprocesses | Spawned async, awaited off the UI thread |
| Long operations (clone, fetch, push) | Channel-backed `Subscription` emitting progress `Message`s; cancellable |
| PR polling | Long-lived `Subscription` in `hidegit-forge` |
| Filesystem watching | `Subscription` over a debounced watcher, triggering status refresh |

The flow is uniform: `Message` → `update` returns a `Task` → work happens off-thread → completion
or failure arrives as another `Message`. A repository handle is cheap to clone and moves into the
task; the UI never holds a lock across an await.

Every operation that mutates the repository ends by emitting `RepositoryChanged`, which triggers a
refresh of status and refs. One code path for "something changed", rather than each operation
remembering to update the views it happens to affect.

## Domain model

`hidegit-core` owns plain data types with no `gix` types in their public signatures. Translation
happens at the backend boundary. This is what allows a method to move from CLI to gix — or gix to
CLI — without the rest of the application noticing.

```rust
pub struct Commit {
    pub id: ObjectId,
    pub parents: Vec<ObjectId>,
    pub summary: String,
    pub body: Option<String>,
    pub author: Signature,
    pub committer: Signature,
    pub time: OffsetDateTime,
    pub refs: Vec<RefName>,   // branches/tags pointing here
}

pub struct WorktreeStatus {
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<PathBuf>,
    pub conflicted: Vec<Conflict>,
    pub state: RepoState,  // Clean | Merging | Rebasing | CherryPicking | Reverting | Bisecting
}

pub struct Diff {
    pub files: Vec<FileDiff>,      // each with hunks, each hunk with lines
    pub stats: DiffStats,
}
```

`RepoState` is not cosmetic. A repository mid-rebase must not offer "commit" as though nothing is
happening — the UI reads this to decide which actions are even available.

`staged` and `unstaged` are two different diffs, not two halves of one list: `staged` is `HEAD`
against the index — what a commit would contain — and `unstaged` is the index against the working
tree. A file modified, staged, and then edited again appears in **both**, which is correct rather
than double-counted, because the staging view offers a different action for each. `change_count()`
adds all four lists for the sidebar badge and will therefore count such a file twice, the same way
a file listed under two headings in `git status` is read twice.

Both lists are sorted by path. gitoxide computes the two halves in parallel and emits them
interleaved, so the arrival order is "whichever thread finished first" — not something a list the
user reads should inherit.

## Commit graph

The visual centrepiece and the highest-risk component. It is split deliberately:

- **`hidegit-core`** computes the layout: given a topologically ordered commit list, assign each
  commit a lane, and each parent edge a route. A pure function — same input, same output, no I/O,
  fully unit-testable.
- **`hidegit-ui`** renders the resulting layout into an iced `canvas`, virtualised so only visible
  rows are drawn.

Target: 60fps scrolling on a 100,000-commit repository. Full algorithm, edge-routing rules and
test fixture strategy in [COMMIT_GRAPH.md](./COMMIT_GRAPH.md).

## Forge integration

`hidegit-forge` talks to hosting providers behind a `Forge` trait. GitHub is the only
implementation before 1.0; see [ADR-0003](./adr/0003-forge-github-first.md).

The trait is deliberately narrow — list pull requests, fetch one, create one, report poll state.
Anything beyond that opens the browser. Its data model is provider-neutral, so a GitLab merge
request is translated into a `PullRequest` at the boundary and `hidegit-ui` never branches on
provider.

```rust
#[async_trait]
pub trait Forge: Send + Sync {
    fn id(&self) -> ForgeId;
    fn detect(remote_url: &str) -> Option<RepoRef> where Self: Sized;

    async fn authenticate(&self, flow: AuthFlow) -> Result<Identity, ForgeError>;
    async fn current_user(&self) -> Result<Identity, ForgeError>;

    /// `since` carries the cache validator from the previous poll.
    async fn pull_requests(&self, repo: &RepoRef, since: Option<PollCursor>)
        -> Result<PollResult<Vec<PullRequest>>, ForgeError>;
    async fn pull_request(&self, repo: &RepoRef, number: u64)
        -> Result<PullRequestDetail, ForgeError>;
    async fn create_pull_request(&self, repo: &RepoRef, draft: NewPullRequest)
        -> Result<PullRequest, ForgeError>;

    fn web_url(&self, repo: &RepoRef, target: WebTarget) -> Url;
}

pub struct PollResult<T> {
    pub data:   Option<T>,     // None ⇒ unchanged since `cursor`
    pub cursor: PollCursor,    // ETag or equivalent, fed into the next poll
    pub budget: RateBudget,    // remaining requests and reset time
}
```

`PollResult` is shaped this way because rate limits and conditional requests are not details a
provider can hide — the poll scheduler needs both to behave.

### Authentication and tokens

OAuth 2.0 Device Authorization Flow ([RFC 8628](https://datatracker.ietf.org/doc/html/rfc8628)),
with a personal access token as a first-class fallback for GitHub Enterprise and restricted
environments.

**No client secret is embedded.** hideGit is open source, so anything compiled in is public; the
device flow exists for public clients that cannot hold a secret. Introducing an embedded secret
would be a security bug.

Tokens are stored in the OS keychain via `keyring` — never in the config file, never in logs, never
sent anywhere but the provider's own API. If no keychain is available (a headless Linux session
with no Secret Service), forge features are disabled rather than falling back to a file.

### Polling

Only repositories currently open in hideGit are polled. Every request sends `If-None-Match` with
the previous `ETag`; a `304 Not Modified` does not count against GitHub's rate limit, which is what
makes a short interval affordable at all.

| Condition | Interval |
|---|---|
| Default | 5 minutes |
| Window focused, PR panel open | 60 seconds |
| Application in background | 15 minutes |

`x-ratelimit-remaining` and `x-ratelimit-reset` are read on every response: below 20% remaining the
interval widens, below 5% polling stops until reset and says so in the UI. `Retry-After` is
honoured exactly. Network failures back off exponentially from 30s to a 30-minute ceiling with
jitter, and never produce a notification — a failed poll updates a status indicator.

Notifications fire on *transitions*, not on state, and the first poll after startup establishes a
baseline silently so launching the app never produces a burst of alerts for things already known.
Events and their defaults are listed in [UI_SPEC.md](./UI_SPEC.md#pr-panel).

## Error taxonomy

Libraries use `thiserror` and return typed errors. `hidegit-core` never returns a stringly-typed
error, because the UI needs to distinguish "this is recoverable and here is the button that fixes
it" from "report this".

```rust
pub enum GitError {
    NotARepository(PathBuf),
    GitNotFound,                                   // no `git` on PATH — actionable, not a crash
    GitTooOld { found: Version, required: Version },
    RefNotFound(String),
    Conflict(Vec<Conflict>),                       // expected outcome, not a failure
    IndexLocked(PathBuf),
    Auth(AuthError),
    Command { argv: Vec<String>, status: Option<i32>, stderr: String },
    Gix { context: &'static str, source: Box<dyn Error + Send + Sync> },
    Io(#[from] std::io::Error),
    NotImplementedYet { operation: &'static str, milestone: &'static str },
}
```

Three of these deserve emphasis:

- **`Conflict`** is a normal outcome of merge and rebase, not an error condition. It routes to the
  conflict resolution UI.
- **`GitNotFound`** is checked once at startup, with a clear message pointing at the requirement,
  rather than surfacing as a mystery failure the first time someone pushes.
- **`Command`** carries the argument vector and raw stderr so a bug report contains what is needed
  to reproduce it. `status` is `None` when the process was killed by a signal.
- **`Gix`** boxes its source and names the operation, because gitoxide has no crate-wide error type
  — each operation defines its own.

## Configuration and state

Paths come from `directories`, so each platform gets its conventional location.

| What | Location | Format |
|---|---|---|
| Settings, repository list, alert preferences | Config dir | TOML |
| Graph layout cache, avatars, forge response cache | Cache dir | Binary; safe to delete |
| Window geometry, recent repositories | Data dir | TOML |
| **Tokens** | **OS keychain** | Never a file |

Configuration is human-editable TOML on purpose. No hosted sync service: no server to run, and no
question about what hideGit does with your data — it does not have it.

Every config value has a working default. A missing or partially corrupt config file produces
defaults plus a warning, never a failure to start.

## Testing strategy

| Layer | Approach |
|---|---|
| `hidegit-core` | Unit tests against fixture repositories created by a test helper. Every `GitBackend` method covered including error paths. Graph layout tested against handwritten expected layouts. |
| `hidegit-forge` | HTTP mocked. No test touches the network or needs a real token. |
| `hidegit-ui` | iced 0.14's headless testing for state transitions and message handling. No pixel assertions. |
| Cross-platform | CI runs the suite on Linux, macOS and Windows, because the subprocess boundary genuinely differs. |

Fixture repositories are built programmatically rather than committed as binary blobs, so a test
reads as a description of the history it exercises.

## Known limits

Stated plainly, because a reader should meet these here rather than discover them mid-task.

1. **`git` must be on `PATH`.** gitoxide does not implement `push`, and merge/rebase have plumbing
   without a complete workflow layer. hideGit therefore requires the system `git` binary for those
   operations. Checked at startup with an actionable message.
   ([ADR-0002](./adr/0002-git-backend-hybrid.md))
2. **Subprocess output is a parsing surface.** Mitigated by using only machine-readable formats
   and pinning a minimum Git version, but it remains a real maintenance cost, and Git's porcelain
   formats do occasionally gain fields.
3. **GitHub only until post-1.0.** The `Forge` trait exists so GitLab and Bitbucket are additions
   rather than rewrites, but a trait designed against one implementation usually needs adjusting
   when the second arrives. Expect to revise it.
4. **iced 0.14 is pre-1.0.** The final experimental release before 1.0, so a breaking upgrade is
   expected. Isolating iced types to `hidegit-ui` keeps that blast radius to one crate.
5. **Opening a very large repository takes about a second.** Ordering 100,000 commits
   topologically measures at 1.01s, and it happens before the first screen appears. Scrolling is
   fast once open — laying out a visible window costs 52µs — but the initial pass is real, and
   nothing yet shows progress during it. Numbers and method in
   [COMMIT_GRAPH.md](./COMMIT_GRAPH.md#performance).
