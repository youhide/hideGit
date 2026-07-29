# Commit Graph

The commit graph is the centre of the application and the highest-risk component: the one most
likely to regress subtly, and the one users notice first when it does.

Ships in [M1](./ROADMAP.md#m1--scaffold--read-only-viewer).

## Contents

- [The split](#the-split)
- [Data model](#data-model)
- [Lane assignment](#lane-assignment)
- [Edge routing](#edge-routing)
- [Rendering](#rendering)
- [Performance](#performance)
- [Testing](#testing)
- [Known hard cases](#known-hard-cases)

## The split

| | Where | What |
|---|---|---|
| **Layout** | `hidegit-core` | Commits in, geometry out. Pure function. No I/O, no colours, no pixels. |
| **Rendering** | `hidegit-ui` | Geometry in, iced `canvas` out. Theme, colours, interaction. |

The boundary is what makes this testable. Layout is compared against handwritten expected output
in unit tests that read as descriptions of a history — no window, no GPU, no screenshots.

`hidegit-core` assigns lane **indices**, not colours. Mapping index → colour is a theme concern and
belongs in `hidegit-ui`.

## Data model

```rust
/// Layout for one screenful (plus overscan) of history.
pub struct GraphLayout {
    pub rows:  Vec<GraphRow>,
    pub width: usize,          // max lanes in use — drives horizontal reservation
}

pub struct GraphRow {
    pub commit:   ObjectId,
    pub lane:     usize,          // column this commit's node sits in
    pub kind:     NodeKind,       // Normal | Merge | Root | Boundary
    pub incoming: bool,           // a lane was already descending into this commit
    pub edges:    Vec<Edge>,      // everything drawn in this row's vertical band
}

pub struct Edge {
    pub from: usize,              // lane at the top of this row's band
    pub to:   usize,              // lane at the bottom
    pub role: EdgeRole,
}

pub enum EdgeRole {
    Continue,   // straight through: from == to, commit not involved
    Parent,     // this commit down to a parent
    Merge,      // an additional parent of a merge commit
    Close,      // a lane terminating here (its awaited commit arrived)
}
```

`incoming` exists because edges describe what *leaves* a node. Without it the top half of a node's
own lane has nothing describing it, and a renderer would leave a gap above every commit that is not
a branch tip.

`Boundary` marks a commit whose parents are outside the loaded window — it is drawn with a fade
rather than a line into nothing, so a partially loaded history does not look like a truncated one.

## Lane assignment

Commits arrive in display order: newest first, topologically ordered with a date tiebreak (`gix`
provides this; the layout does not re-sort).

The algorithm keeps a vector of **active lanes**, where each active lane holds the `ObjectId` of the
commit it is waiting to reach:

```
lanes: Vec<Option<ObjectId>>
```

For each commit `c`, in order:

1. **Claim a lane.**
   Find every lane awaiting `c`. If there are none, `c` is a tip: allocate the leftmost free lane
   (reusing a `None` slot before growing the vector, so the graph does not drift right forever).
   If there are several, the **leftmost becomes `c`'s lane** and the others close into it — this is
   a commit that several branches descend from, and each closing lane emits a `Close` edge.

2. **Route the first parent.**
   The first parent inherits `c`'s lane. `lanes[c.lane] = Some(first_parent)`, and a `Parent` edge
   runs straight down. Keeping first-parent history in a single column is what makes a mainline
   readable as a vertical line.

3. **Route additional parents** (merge commits only).
   For each remaining parent `p`:
   - If some lane already awaits `p`, emit a `Merge` edge from `c.lane` to that lane. Do not
     allocate.
   - Otherwise allocate the leftmost free lane, set it to await `p`, and emit a `Merge` edge to it.

4. **Emit pass-through edges.**
   Every lane still active and not otherwise involved in this row emits a `Continue` edge from its
   lane to itself, so the renderer can draw the row's full vertical band without looking at
   neighbouring rows.

5. **Free the lane if `c` is a root.**
   No parents: `lanes[c.lane] = None`. The slot becomes available to the next tip.

### Worked example

```
history                  lanes after each row       row output

●  A   (tip)             [A]                        A: lane 0
│                        [B]
●  B                     [C]                        B: lane 0
├─┐                      [C, D]                     C: lane 0, merge edge 0→1
● │  C                   [E, D]                     C: lane 0
│ ●  D                   [E, E]  ← both await E     D: lane 1
├─┘                      [E]                        E: lane 0, close edge 1→0
●  E                     [ ]                        E: lane 0
```

Row `C` is a merge: first parent `E` stays in lane 0, second parent `D` takes lane 1. Two rows
later, both lanes await `E`; `E` claims lane 0 (leftmost) and lane 1 closes into it.

### Properties this guarantees

- **Deterministic.** The same commit list always produces the same layout. Required for both
  testing and for a stable view across refreshes.
- **First-parent continuity.** A branch's mainline holds one column for as long as it exists.
- **Compact.** Lanes are reused as soon as they free, so width tracks actual concurrency rather
  than growing with history length.
- **Local.** Each row depends only on the lane state carried in — which is what makes incremental
  and windowed layout possible at all.

### Lane colouring

`hidegit-ui` maps lane index to a colour, cycling through the theme's `lanes` palette. Because lane
indices are reused, colour is not a stable identifier for a branch — it is a visual aid for
following a line across a screen. The palette must remain distinguishable under deuteranopia and
protanopia; see [UI_SPEC.md](./UI_SPEC.md#theming).

## Edge routing

Rendered geometry, all within a single row's vertical band:

- **Straight** — `from == to`. A vertical line through the band.
- **Diverging** — a merge to a lane on the right: leaves the node horizontally, then curves down
  into the target lane within the same band.
- **Converging** — a lane closing into the node: descends its own lane, then curves in.

Corners are quadratic curves rather than right angles — at typical row heights, right angles read
as visual noise at the density a graph reaches.

Edges are drawn **before** nodes, so nodes sit on top. Within a row, `Continue` edges draw first,
then `Parent`, then `Merge` and `Close`, so the lines that carry meaning are not obscured by
pass-throughs.

## Rendering

An iced `canvas` widget in `hidegit-ui`.

| Concern | Approach |
|---|---|
| Virtualisation | Only rows intersecting the viewport, plus overscan, are laid out and drawn |
| Row height | Fixed, so scroll position maps to a row index arithmetically |
| Ref badges | Branch and tag labels render to the right of the node, truncated with the full name on hover |
| Hit testing | Row index from the y coordinate; no per-node geometry search |
| Caching | The canvas re-renders only when the viewport or the underlying commit list changes |

A fixed row height is a deliberate constraint. Variable heights would allow richer rows but would
cost the arithmetic scroll mapping, which is what keeps scrolling a 100,000-commit history cheap.

## Performance

**Target: 60fps scrolling on a 100,000-commit repository.**

Measured, not assumed. `cargo bench -p hidegit-core` builds a 100,000-commit repository with
`git fast-import` and times the layout against it. On an Apple M4 Max, macOS 15, release build:

| What | Time | When it happens |
|---|---|---|
| **Lay out one visible window at row 50,000** | **52 µs** | Every frame — 0.3% of a 60fps budget |
| The same window, with checkpoints skipped | 23.9 ms | Would miss the frame budget outright |
| Walk and topologically order 100,000 commits | 1.01 s | Once, when a repository opens |
| Build checkpoints over 100,000 commits | 47.6 ms | Once per loaded page |
| Hydrate one 2,000-commit page into full commits | 14.8 ms | Once per loaded page |

The first row is the one that decides whether scrolling is smooth. The gap between it and the
second is the entire justification for checkpoints: without them the per-frame cost grows with how
far down the history you have scrolled, and by row 50,000 it is 450× over budget.

Everything else runs off the UI thread. The 1.01s ordering pass is the cost of opening a repository
that size — it is the first-screen latency, and the thing to attack if opening feels slow.

Strategies:

1. **Never lay out the whole history.** Layout runs over a window around the viewport. Lane state
   is carried forward, so scrolling extends an existing layout rather than recomputing one.
2. **Load commits in pages.** `gix` walks are streamed; scrolling toward older history requests the
   next page rather than blocking on a full traversal.
3. **Checkpoint lane state.** Snapshot the `lanes` vector every N rows so a jump to an arbitrary
   scroll position resumes from a nearby checkpoint instead of replaying from `HEAD`.
4. **Layout off the UI thread.** Like all `gix` work — `Task::perform`, results back as a `Message`.
5. **Cache the geometry, not the pixels.** `GraphLayout` for the current window is retained;
   redraws that do not move the viewport reuse it.

Benchmarks live in CI from M6 so a regression fails a build rather than being reported by a user.

## Testing

Fixture repositories are **built programmatically**, never committed as binary blobs, so a test
reads as a description of the history it exercises:

```rust
let repo = fixture()
    .commit("A")
    .branch("feature")
    .commit("B")
    .checkout("main")
    .commit("C")
    .merge("feature")
    .build();

let layout = layout_graph(&repo.commits());
assert_eq!(layout.rows[0].lane, 0);
assert_eq!(layout.width, 2);
```

Coverage required:

| Case | Why |
|---|---|
| Linear history | The baseline |
| Simple branch and merge | The common case |
| Octopus merge (3+ parents) | Rare, and the usual source of lane-allocation bugs |
| Multiple roots | Real, from grafted or imported repositories |
| Orphan branches | No common ancestor at all |
| Criss-cross merges | Two branches merging each other |
| Windowed layout | A window's layout must match the corresponding slice of a full layout |
| Checkpoint resume | Resuming from a checkpoint must equal replaying from the start |
| Determinism | Same input, same output, across runs |

The last three are the ones that catch real bugs. Windowing and checkpointing are optimisations,
and an optimisation that changes the output is a defect.

## Known hard cases

Stated so they are designed for rather than discovered:

- **Octopus merges.** More than two parents is legal and shows up in real repositories. The
  allocation step handles arbitrary parent counts, but the rendering gets crowded and needs a
  visual decision, not just a correct one.
- **Very wide history.** A repository with dozens of concurrent branches exceeds any sensible
  column budget. Beyond a threshold, lanes past the limit collapse into an overflow indicator
  rather than shrinking every column into illegibility.
- **Reordering on refresh.** New commits arriving at `HEAD` shift the layout. Scroll position must
  be anchored to a **commit id**, not a row index, or a background fetch silently moves the user's
  place.
- **Grafted and shallow clones.** Parents that do not exist locally are `Boundary` nodes, drawn as
  a fade. Never draw an edge to a commit that is not there.
- **Date-ordered versus topological.** Commit dates lie — clock skew and rebases both produce
  out-of-order timestamps. Layout consumes topological order and uses date only as a tiebreak;
  sorting by date alone produces a graph with edges pointing upward.
