# Architecture

How hideGit is put together, and why. Decisions summarised here are argued in full in the
[ADRs](./adr/README.md).

**Status:** M1 through M5 have landed, so everything described here is code that exists — the reads,
the working-directory writes, the remote operations, the forge integration, and history rewriting:
merge, rebase, cherry-pick, revert, reset, the reflog and the conflict resolver. **Every `GitBackend` method is
implemented** as of M6, when `blame` — the last one — landed. `NotImplementedYet` stays in the error
type for whatever a later backend cannot do, but nothing produces it.

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
| `serde` + `toml` | 1 / 0.9 | Configuration, and custom theme files | M1 |
| `time` | 0.3 | Commit timestamps with their recorded offset | M1 |
| `tracing` | 0.1 | Structured logging | M1 |
| `thiserror` | 2 | Error types in libraries | M1 |
| `criterion` | 0.7 | Benchmarks (dev only) | M1 |
| `notify-debouncer-full` | 0.6 | Filesystem watching behind automatic status refresh | M2 |
| `async-trait` | 0.1 | `Forge`'s async methods, which have to be dyn-compatible | M4 |
| `octocrab` | 0.54 | GitHub API, and the device flow | M4 |
| `keyring` | 4 | OS keychain access for forge tokens | M4 |
| `notify-rust` | 4 | Native desktop notifications | M4 |
| `open` | 5 | Handing a URL to the platform's browser | M4 |
| `syntect` | via `iced/highlighter` | Syntax highlighting in the diff | M6 |

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
    fn remotes(&self) -> Result<Vec<Remote>, GitError>;
    fn stashes(&self) -> Result<Vec<StashEntry>, GitError>;
    fn divergence(&self) -> Result<HashMap<String, Divergence>, GitError>;
    fn blame(&self, path: &Path, at: ObjectId) -> Result<Blame, GitError>;
    fn invalidate(&self);

    // ---- write: git CLI -----------------------------------------------
    fn stage(&self, paths: &[&Path]) -> Result<(), GitError>;
    fn stage_patch(&self, patch: &Patch) -> Result<(), GitError>;
    fn unstage(&self, paths: &[&Path]) -> Result<(), GitError>;
    fn discard(&self, paths: &[&Path]) -> Result<(), GitError>;
    fn create_commit(&self, message: &str, opts: CommitOpts) -> Result<ObjectId, GitError>;
    fn checkout(&self, target: &CheckoutTarget) -> Result<(), GitError>;
    fn create_branch(&self, name: &str, from: &StartPoint) -> Result<(), GitError>;
    fn rename_branch(&self, from: &str, to: &str) -> Result<(), GitError>;
    fn delete_branch(&self, name: &str, force: bool) -> Result<(), GitError>;
    fn create_tag(&self, spec: &TagSpec) -> Result<(), GitError>;
    fn delete_tag(&self, name: &str) -> Result<(), GitError>;
    fn add_remote(&self, name: &str, url: &str) -> Result<(), GitError>;
    fn set_remote_url(&self, name: &str, url: &str) -> Result<(), GitError>;
    fn remove_remote(&self, name: &str) -> Result<(), GitError>;
    fn fetch(&self, remote: &str, opts: &FetchOpts,
             p: &dyn ProgressSink, c: &CancelToken) -> Result<FetchOutcome, GitError>;
    fn pull(&self, opts: &PullOpts,
            p: &dyn ProgressSink, c: &CancelToken) -> Result<PullOutcome, GitError>;
    fn push(&self, remote: &str, spec: &PushSpec,
            p: &dyn ProgressSink, c: &CancelToken) -> Result<PushOutcome, GitError>;
    fn stash(&self, op: &StashOp) -> Result<StashOutcome, GitError>;
    fn merge(&self, from: &str, opts: &MergeOpts) -> Result<MergeOutcome, GitError>; // M5
    fn rebase(&self, onto: &str, plan: &RebasePlan) -> Result<SequenceOutcome, GitError>;    // M5
    fn cherry_pick(&self, ids: &[ObjectId]) -> Result<SequenceOutcome, GitError>;    // M5
}
```

The whole surface is declared from M1 so the read/write split is visible in one file, and a method
whose milestone has not landed returns `GitError::NotImplementedYet { operation, milestone }`
rather than being absent.

**`clone` is deliberately not on the trait.** There is no repository for it to be a method *on*, and
keeping it out preserves what the trait means — the things you can ask of an already-open
repository. It is a free function, `clone_repository(url, into, progress, cancel)`.

Every write goes through one helper that does the two things a write owes the rest of the
application: it refuses to start while `index.lock` exists — reported, never deleted, because
whatever holds it may still be working — and it invalidates the read side so the next read sees what
just happened.

**Invalidating means reopening the gitoxide handle, not just clearing the walk cache.** gitoxide
reads `.git/config` when a repository is opened and caches it, so a `git` command that rewrites
config — renaming a branch, adding or removing a remote, `push --set-upstream` — leaves the handle
answering from the old file. The symptom is quiet rather than loud: a renamed branch appears to
track nothing, and its ahead/behind simply vanishes.

`divergence` is a separate read rather than a field on `Branch` because it costs a commit walk per
tracking branch, and `refs` is reread on every file save through the filesystem watcher. The UI
loads it on its own task for the same reason.

`log` is paged rather than limited because the graph only lays out the rows around the viewport. A
walk's topological order is memoised — it is the expensive part of drawing a graph and it does not
change until the repository does — and only the requested page is hydrated into full `Commit`
values. `invalidate` is what says that memo is stale.

**Both locks around those caches recover from poisoning rather than honouring it.** A `Mutex` is
poisoned when a thread panics while holding it, and every later `lock()` fails from then on. Every
read here runs on a blocking task the UI spawned, so honouring the poison would turn one panicked
task into a repository that fails *everything* asked of it for the life of the process — and quietly,
because the UI catches each panic individually and shows a toast rather than crashing. Recovery is
sound for the reason it usually is not: the guarded values are a memo and a repository handle, this
is safe Rust so neither can be left structurally invalid, and the worst a recovered entry can be is
stale — which is what `invalidate` already exists to clear. The GitHub client handle in
`hidegit-forge` is recovered the same way and for the same reason.

**A panic is written down.** `crash::install` sets a panic hook at startup that logs every panic and,
when `diagnostics.panic_reports` is on, writes a report to the data directory: version, platform,
message, location, backtrace. Nothing is sent anywhere and nothing names a repository, a branch or a
remote — a panic *message* can still carry a path if something formatted one into it, which the file
says rather than leaves to be assumed. The hook chains to the previous one, so `RUST_BACKTRACE` keeps
working.

They are called panic reports, not crash reports, because of the paragraph above: the UI catches
panics on blocking tasks and carries on. A report is evidence of a bug, not of a dead process.

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
and it is the first thing to revisit if fetch performance disappoints — though the reason it has not
been revisited is authentication rather than speed. See
[ADR-0005](./adr/0005-progress-and-cancellation.md), which records what the CLI side costs: one
human-output parser, and no structured progress.

## Shelling out safely

The subprocess boundary is the most security-sensitive part of hideGit, because branch names,
paths and remote URLs come from repositories that may have been cloned from anywhere. A single
helper wraps every invocation and enforces:

| Invariant | Reason |
|---|---|
| Arguments passed as a vector; **no shell is ever spawned** | Metacharacters in a branch name or path are never interpreted |
| `--` before operands wherever Git accepts it | A ref or path starting with `-` cannot be absorbed as a flag |
| `--end-of-options` instead, on commands that take revisions and no paths | On those, `--` means *paths follow*, which is a different and wrong request |
| `GIT_TERMINAL_PROMPT=0` | A subprocess blocking on a hidden prompt is an app that appears to hang |
| `GIT_OPTIONAL_LOCKS=0` for read-adjacent commands | Background invocations never contend for `index.lock` |
| `LC_ALL=C` | Git's output does not shift under the user's locale |
| Machine formats where they exist: `--porcelain=v2`, `-z`, `--null` | Human output is not a stable interface |
| Controlled environment, not inherited wholesale | Fewer surprises from the user's shell configuration |
| `stderr` surfaced to the user **verbatim** on failure | Git's error messages are good. Paraphrasing them destroys information |
| Every invocation logged at `debug` with its full argument vector | Bug reports become diagnosable |
| Arbitrary user text on **stdin**, or attached to its option as `--opt=value` | A commit message or a branch name can never become a separate argument, whatever it starts with |

That last row has two shapes because Git's own commands do. `git commit --file -` and
`git tag --annotate --file -` read from stdin; `git stash push --message` does not — it takes `-` as
the literal message — and `git switch --create` needs its name attached because `switch` accepts
exactly one reference after `--`. Either way the text is one element of the argument vector and can
never be reinterpreted.

The two separator rows differ for the same reason. `--` is Git's *options from paths* marker, and
on a command with no paths to separate it changes what is being asked: `git reset --hard -- HEAD~1`
requests a path-scoped reset and fails with "Cannot do hard reset with paths", and
`git rev-parse --end-of-options side` without `--verify` prints the marker back as though it were a
revision. `--end-of-options` ends flag parsing and says nothing about paths, so it is what `reset`,
`rev-parse` and `rev-list` get; `merge`, `cherry-pick` and `revert` accept `--` and keep it. Which
marker a command wants is not derivable from its signature — it was found by running each one.

### Long-running commands

Progress is parsed from `--progress` output on stderr, read incrementally on its own thread, and
cancellation kills the child process. A killed `git` may leave `index.lock` behind, so cancellation
checks for a stale lock and **reports** it rather than deleting it. The full reasoning, the rejected
alternatives, and the known gap on Windows are in
[ADR-0005](./adr/0005-progress-and-cancellation.md).

**One deliberate exception to preferring machine formats.** `git push --porcelain` puts a stable
tab-separated result on stdout — and moves the failure detail off stderr with it, leaving only
`error: failed to push some refs` where plain `git push` would have written
`! [rejected] main -> main (stale info)` and a hint saying what to do. Since Git's own message is
the most useful thing hideGit has to say when a command fails, `push` reads the human summary
instead. Both `fetch` and `push` summaries are parsed by hand and fail soft: an unrecognised line
costs a summary entry, never the operation. (`git fetch --porcelain` was never an option regardless —
it arrived in 2.41, past the 2.30 minimum.)

## Concurrency model

iced's `update` runs on the UI thread. Blocking it drops frames, so nothing blocking runs there.

| Work | Mechanism |
|---|---|
| `gix` calls (blocking) | `Task::perform` onto `tokio`'s blocking pool |
| `git` subprocesses | Run on the blocking pool, awaited off the UI thread |
| Long operations (clone, fetch, pull, push) | `Task::stream` yielding progress `Message`s and then the outcome; cancellable |
| PR polling | Schedule in `hidegit-forge`, `Subscription` in `hidegit-ui` |
| Keychain reads and writes | `spawn_blocking`, always — see below |
| Filesystem watching | `Subscription` over a debounced watcher, triggering status refresh |

The flow is uniform: `Message` → `update` returns a `Task` → work happens off-thread → completion
or failure arrives as another `Message`. A repository handle is cheap to clone and moves into the
task; the UI never holds a lock across an await.

A long operation is a **`Task::stream`, not a `Subscription`**: it is a one-shot that ends when the
work does, whereas a `Subscription` is for something that outlives any single request. The bridge is
a `ProgressSink` that pushes into a channel; the sink is moved into the blocking closure, so its
sender drops exactly when the work returns, which is what tells the stream to stop waiting and
collect the result. Operations carry a monotonic id, because a cancelled one's last report can arrive
after the operation that replaced it has started and must not redraw its banner.

**The keychain counts as blocking work, and it is easy to forget that it does.** `keyring` is
synchronous, and on macOS it can raise an authorisation dialog that waits for a human — so a call
made straight from an `async fn` stops that executor thread from serving anything else. It showed up
as a repository that never opened, because the keychain prompt at startup was still up and the task
that would have opened it never ran. Every keychain touch therefore goes through a `spawn_blocking`
helper inside `hidegit-forge`, and none of those helpers is public: there is no way to reach a token
store from async code without leaving the runtime alone.

Every operation that mutates the repository ends by emitting `RepositoryChanged`, which triggers a
refresh of status, refs, remotes and the stash. One code path for "something changed", rather than
each operation remembering to update the views it happens to affect. Ahead/behind is the one
exception: it rides on its own task, because a refresh runs on every file save and it costs a walk
per tracking branch.

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

/// A named remote, distinct from the remote-*tracking* branches in `Refs::remotes`:
/// one that has been added but never fetched has no tracking refs at all.
pub struct Remote {
    pub name: String,
    pub fetch_url: String,
    pub push_url: Option<String>,  // only when it differs
}

/// How far a branch has drifted from its upstream. Absent from the map entirely
/// when a branch tracks nothing, which is not the same as being level with a remote.
pub struct Divergence { pub ahead: usize, pub behind: usize }

/// One stash entry. `index` is the `n` in `stash@{n}`, which is what every stash
/// subcommand takes; `id` is a commit, which is what lets its contents be read
/// with an ordinary `DiffTarget::Commit`.
pub struct StashEntry {
    pub index: usize,
    pub id: ObjectId,
    pub message: String,
    pub time: OffsetDateTime,
    pub branch: Option<String>,
}
```

`RepoState` is not cosmetic. A repository mid-rebase must not offer "commit" as though nothing is
happening — the UI reads this to decide which actions are even available.

`DiffTarget::Staged` and `DiffTarget::Unstaged` take their set of changed paths from `status`
rather than from a traversal of their own, so the staging view's file list and the diff it shows
for a file cannot disagree about what changed.

### Building a patch

Staging part of a file means handing `git apply --cached` a patch, so a diff has to be able to
become patch text again. `hidegit_core::patch::serialize` does that, over a `Selection` of hunks
or of individual lines. Applying it in reverse is how unstaging and hunk-level discard are
expressed — one code path for all four.

Two things there are easy to get wrong and invisible when you do. A file whose last line has no
newline needs the `\ No newline at end of file` marker, or every partial stage silently appends
one; `DiffLine::no_newline` exists to carry that through the model. And the `@@` counts are
recomputed rather than copied, because a partial selection changes them: an unselected removal
becomes a context line, an unselected addition disappears, and a skipped hunk moves the new-side
start of every hunk after it. `tests/staging.rs` applies each case with the real `git apply` and
checks the index afterwards, because a patch that reads correctly but does not apply is worth
nothing.

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
pub trait Forge: Send + Sync + Debug {
    fn id(&self) -> ForgeId;
    fn detect(remote_url: &str) -> Option<RepoRef> where Self: Sized;

    async fn authenticate(&self, flow: AuthFlow) -> Result<Identity, ForgeError>;
    async fn current_user(&self) -> Result<Identity, ForgeError>;

    /// `since` carries the cursor from the previous poll.
    async fn pull_requests(&self, repo: &RepoRef, since: Option<PollCursor>)
        -> Result<PollResult<Vec<PullRequest>>, ForgeError>;
    async fn pull_request(&self, repo: &RepoRef, number: u64)
        -> Result<PullRequestDetail, ForgeError>;
    async fn create_pull_request(&self, repo: &RepoRef, draft: NewPullRequest)
        -> Result<PullRequest, ForgeError>;

    fn web_url(&self, repo: &RepoRef, target: WebTarget) -> String;
}

pub struct PollResult<T> {
    pub data:   Option<T>,     // None ⇒ unchanged since `cursor`
    pub cursor: PollCursor,    // opaque, provider-defined; fed into the next poll
    pub budget: RateBudget,    // remaining budget and reset time
}
```

`PollResult` is shaped this way because rate limits are not a detail a provider can hide — the poll
scheduler widens its interval on the budget, so it rides on every result rather than being asked
for separately.

**`PollCursor` is opaque rather than an `ETag` string.** The GitHub implementation polls over
GraphQL, which has no conditional requests, and always returns an empty cursor; a REST-based forge
would put an `ETag` in it. Keeping the shape provider-defined is what lets both exist behind one
trait. See [ADR-0006](./adr/0006-poll-pull-requests-over-graphql.md).

`AuthFlow::Device` carries a callback rather than returning twice, because the flow is not one
round trip: the user code has to reach the screen before polling for the token starts, and the
caller is what knows how to put it there. `authenticate` still returns once, with an `Identity`, so
nothing outside `hidegit-forge` has to know the flow has two halves.

### Detecting a forge repository

`RepoRef` is read out of a remote's fetch URL, covering every shape Git writes one in: `https`,
`http`, `git` and `ssh` URLs, and the scp-like `git@github.com:owner/repo.git`. That last form is
why the parser is hand-written rather than delegated to a URL crate — it has no scheme and its
colon separates a path rather than a port, so a conforming parser rejects it.

**Remote URLs come from repositories that may have been cloned from anywhere**, and an owner or
repository name read from one is interpolated into an API request. Names are therefore held to a
narrower character set than any provider's real rule, and anything unrecognised returns `None`
rather than being sanitised into something acceptable. A path that is not exactly two segments is
refused too: a GitLab subgroup path is three or more, and silently reading its last two would name
a repository that does not exist. Any credential embedded in the URL is dropped and never reaches
a `RepoRef`, which is logged and displayed.

### Authentication and tokens

Development builds can skip the keychain entirely with `HIDEGIT_NO_KEYCHAIN=1`, which makes every
store call answer `NoKeychain` — the same state as a machine that has none, rendered honestly as
"hideGit cannot store a token on this machine". It exists because macOS ties a keychain entry's
access list to the requesting binary's code signature: an unsigned bundle gets a fresh identity on
every build, so each launch raises the authorisation dialog again. The check is inside the store's
methods rather than at construction, so it governs any route to the keychain added later.

OAuth 2.0 Device Authorization Flow ([RFC 8628](https://datatracker.ietf.org/doc/html/rfc8628)),
with a personal access token as a first-class fallback for GitHub Enterprise and restricted
environments.

**No client secret is embedded.** hideGit is open source, so anything compiled in is public; the
device flow exists for public clients that cannot hold a secret. Introducing an embedded secret
would be a security bug. The *client identifier* is compiled in and is not one — it names the
application and authorises nothing.

Tokens are stored in the OS keychain via `keyring` — never in the config file, never in logs, never
sent anywhere but the provider's own API. If no keychain is available (a headless Linux session
with no Secret Service), forge features are disabled rather than falling back to a file. A token is
a `SecretString`, which redacts in both `Debug` and `Display`, so the promise survives somebody
adding a `#[derive(Debug)]` later.

### hideGit is registered as a GitHub App

Rather than an OAuth App, which is what ADR-0003 assumed without saying. The device flow, the
absence of a client secret and the personal-access-token fallback are all unchanged; two things a
GitHub App adds are not.

**Access is installation-scoped.** A valid token sees nothing in a repository the App has not been
installed on, and GitHub reports that as a `null` repository rather than as a permission error. So
"connected, but not installed here" is its own state — `ForgeError::NotInstalled`, carrying the
install URL — and the sidebar names it. An empty pull request list would say the opposite of the
truth.

**User tokens expire**, eight hours by default, and arrive with a refresh token. The keychain holds
the access token, the refresh token and the expiry together, and a token within a minute of expiry
is refreshed before it is used. If the App has expiry turned off, GitHub issues no refresh token,
the refresh path is never taken, and nothing has to be configured for either shape to work.

**GitHub Enterprise is not wired up.** A self-hosted instance puts REST on `/api/v3` and GraphQL on
`/api/graphql` — a different layout rather than a different hostname — and there is no
configuration surface to supply a host from. `Endpoint` carries the three bases apart so adding it
is a change in one place; detection alone only ever claims `github.com`, because sending a token to
a host hideGit merely guessed was GitHub is not a guess worth making.

### Polling

Only repositories currently open in hideGit are polled. **One GraphQL query per repository per
poll** returns every field the sidebar and every notification need — review decision, check rollup,
mergeable state — whatever the number of open pull requests.

This replaces an earlier design that used conditional REST requests and free `304`s. It had to go
because a check run completing does not modify the pull request it belongs to, so an `ETag` on the
pull request stays valid across the entire life of a CI run and `ChecksFailed` would never fire.
The full reasoning and the rejected alternatives are in
[ADR-0006](./adr/0006-poll-pull-requests-over-graphql.md), which also records why the query's
nested page sizes are a rate-limit decision rather than a display one.

| Condition | Interval |
|---|---|
| Default | 5 minutes |
| Window focused, PR panel open | 60 seconds |
| Application in background | 15 minutes |

The budget is read from the `rateLimit` block inside each response, and from
`x-ratelimit-remaining` / `x-ratelimit-reset` alongside it: below 20% remaining the interval
widens, below 5% polling stops until reset and says so in the UI. `Retry-After` is honoured
exactly. Network failures back off exponentially from 30s to a 30-minute ceiling with jitter, and
never produce a notification — a failed poll updates a status indicator.

**The scheduler lives in `hidegit-forge`; the `Subscription` that drives it lives in `hidegit-ui`.**
An interval is arithmetic and a transition is a comparison — neither needs a window, and both are
miserable to test through a toolkit. It is the same split `hidegit_core::watch` and
`hidegit-ui`'s `watcher` already use for the filesystem. The interval is part of the subscription's
identity, so a failure or a thin budget replaces the timer rather than being noticed on the next
tick.

Notifications fire on *transitions*, not on state, and the first poll after startup establishes a
baseline silently so launching the app never produces a burst of alerts for things already known.
Signing in as a different account resets that baseline for the same reason: every role changes, and
the change is not news about the pull requests. Events and their defaults are listed in
[UI_SPEC.md](./UI_SPEC.md#pr-panel).

**An ending arrives as an absence.** The poll asks only for open pull requests, so a merge and a
close both look like a row disappearing — and `PrMerged` and `PrClosed` are different events. Each
disappearance of a pull request *you wrote* therefore costs one extra request to read its state.
That is a handful a day rather than one per poll, and it is the alternative to reporting both as
the same thing.

**Delivery is behind a `Notifier` trait.** Nothing in CI can receive a notification — a Linux runner
has no notification daemon and a macOS runner has no bundle to send from — so everything that
*decides* to notify is tested against a recorder. On macOS `notify-rust` goes through
`mac-notification-sys`, which attributes a notification to whatever executable sent it: run from
`cargo run`, macOS raises its authorization prompt naming that binary rather than hideGit, and any
alert that follows is attributed to it. Checking alerts there therefore means
`cargo run -p xtask -- bundle-macos` and running the bundle, whose identifier is what makes the
notification say hideGit. `show()` returns `Ok` either way, so nothing in the code can detect the
difference — `cargo test -p hidegit-forge -- --ignored a_real_notification` is the manual check.

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
    Cancelled { stale_lock: Option<PathBuf> },     // what was asked for, not a failure
    Command { argv: Vec<String>, status: Option<i32>, stderr: String },
    Gix { context: &'static str, source: Box<dyn Error + Send + Sync> },
    Io(#[from] std::io::Error),
    NotImplementedYet { operation: &'static str, milestone: &'static str },
}
```

Some of these deserve emphasis:

- **`Conflict`** is a normal outcome of merge and rebase, not an error condition. It routes to the
  conflict resolution UI. Conflicts arising from a pull or a stash apply are the same: those methods
  return them as an *outcome* rather than as this error, because a wall of stderr in front of a state
  the user has to work in is the wrong shape.
- **`GitNotFound`** is checked once at startup, with a clear message pointing at the requirement,
  rather than surfacing as a mystery failure the first time someone pushes.
- **`Cancelled`** is what was asked for, so the UI reports it silently — unless `stale_lock` is set,
  which names an `index.lock` the killed `git` left behind. hideGit never deletes one.
- **`Auth`** is produced by classifying a failed network command's stderr, which is how a missing
  credential becomes an actionable message instead of a wall of text. Matching on Git's wording is a
  real maintenance cost, and anything unrecognised falls back to the verbatim `Command` error: a
  phrase we stop recognising degrades to "here is exactly what git said" rather than to a confident
  wrong diagnosis.
- **`Command`** carries the argument vector and raw stderr so a bug report contains what is needed
  to reproduce it. `status` is `None` when the process was killed by a signal.
- **`Gix`** boxes its source and names the operation, because gitoxide has no crate-wide error type
  — each operation defines its own.

## Configuration and state

`config.toml` is edited **in place** when the settings screen changes something: the document is
parsed with `toml_edit`, the handful of keys that screen owns are set, and everything else — comments,
key order, tables hideGit knows nothing about — is written back untouched. Round-tripping through
`serde` would strip every comment the first time somebody toggled a checkbox, and the file is meant
to be hand-edited and carried between machines. A file that will not parse is left alone rather than
replaced, because somebody is probably mid-edit.

Paths come from `directories`, so each platform gets its conventional location.

| What | Location | Format |
|---|---|---|
| Settings, repository list, alert preferences | Config dir | TOML — `AlertPrefs` is defined in `hidegit-forge`, so config and UI share one definition |
| Graph layout cache, avatars, forge response cache | Cache dir | Binary; safe to delete |
| Window geometry, recent repositories | Data dir | TOML |
| **Tokens** | **OS keychain** | Never a file |

Configuration is human-editable TOML on purpose. No hosted sync service: no server to run, and no
question about what hideGit does with your data — it does not have it.

Every config value has a working default. A missing or partially corrupt config file produces
defaults plus a warning, never a failure to start.

**Nothing waits for the exit to be written.** `state.toml` used to be written from one place — the
window-close handler — which quietly assumed quitting runs it. It does not: on macOS, `Cmd+Q` goes
through `terminate:` and closes the window directly, so `WindowEvent::CloseRequested` is never emitted
and the handler never runs. That is the ordinary way to quit a Mac application, not an edge case, and
it silently discarded the session's recent repositories. A kill or a panic reaches even less.

So the recents list is written when it changes — opening a repository is the only thing that changes
it — and window geometry, which arrives per frame while a window is dragged, is collected and flushed
on the application's one timer. Both go through a write that stages to a sibling file and `rename`s
over the target, because a write that happens while the application runs is a write that can be
interrupted part-way.

Geometry means size **and position**. It did not: `x` and `y` were read at startup to place the
window and written back unchanged, because only resizes were listened for. iced has a
`resize_events()` helper and no `move_events()` to match it, so the moves are filtered out of the
full window event stream — which is the whole reason the position was missed.

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
2. **Subprocess output is a parsing surface.** Mitigated by preferring machine-readable formats and
   pinning a minimum Git version, but it remains a real maintenance cost, and Git's porcelain formats
   do occasionally gain fields. Three places read *human* output deliberately: `--progress` on
   stderr, which has no machine form, and the fetch and push summaries, where
   [ADR-0005](./adr/0005-progress-and-cancellation.md) records why. All three fail soft — an
   unrecognised line costs a summary entry, never the operation.
3. **Cancelling a network operation on Windows may leave a helper process behind.** Killing a process
   there does not kill its children, and `git` spawns `git-remote-https` or an SSH client. Nothing in
   a hermetic test suite is slow enough to exercise this, so it is a known gap rather than a solved
   problem. ([ADR-0005](./adr/0005-progress-and-cancellation.md))
4. **Credential helpers and SSH passphrases are not covered by the test suite.** Every remote in it
   is a bare repository on a local path, which needs no authentication. `GIT_TERMINAL_PROMPT=0` makes
   a missing credential fail fast rather than hang, and the failure is classified into `AuthError` —
   but that classification is only ever exercised against synthetic stderr. Real credentials stay a
   manual check on a developer's machine.
5. **GitHub is the only forge, and no second one is planned.** The `Forge` trait still earns its
   place — it is the seam that keeps `hidegit-core` free of any network dependency, and it is what
   makes the UI's data model provider-neutral — but it is no longer a bet on a second provider
   arriving. A trait designed against one implementation usually needs adjusting when a second
   turns up, so anyone adding one should expect to revise it rather than to slot in underneath.
   **GitHub Enterprise is not wired up either** — a
   self-hosted instance puts REST on `/api/v3` and GraphQL on `/api/graphql`, and there is no
   configuration surface to name a host from. `Endpoint` carries the three bases apart so adding it
   is a change in one place.
6. **Pull request alerts have not been verified against a real account.** Every forge test mocks
   HTTP, which is what keeps the suite hermetic — and it means the milestone's own bar, a real
   review request producing a real notification, is a manual check. So is a full day of running
   without a rate-limit warning.
7. **A reply inside an existing review thread does not notify.** `PrCommented` watches a count of
   issue comments plus review threads, and a reply changes neither. Catching those would mean
   reading every thread on every pull request on every poll, which is the N+1
   [ADR-0006](./adr/0006-poll-pull-requests-over-graphql.md) exists to avoid.
8. **iced 0.14 is pre-1.0.** The final experimental release before 1.0, so a breaking upgrade is
   expected. Isolating iced types to `hidegit-ui` keeps that blast radius to one crate.
9. **Opening a very large repository takes about a second.** Ordering 100,000 commits
   topologically measures at 1.01s, and it happens before the first screen appears. Scrolling is
   fast once open — laying out a visible window costs 52µs — but the initial pass is real, and
   nothing yet shows progress during it. Numbers and method in
   [COMMIT_GRAPH.md](./COMMIT_GRAPH.md#performance).
