//! Commit graph layout: commits in, geometry out.
//!
//! A pure function. No I/O, no colours, no pixels — `hidegit-core` assigns
//! lane **indices** and `hidegit-ui` maps those to theme colours and draws
//! them. That boundary is what makes this testable: layout is compared against
//! handwritten expected output in tests that read as descriptions of a
//! history, with no window, no GPU and no screenshots.
//!
//! The algorithm is specified in `docs/COMMIT_GRAPH.md`; this is its
//! implementation, and the two are meant to stay in step.

use std::collections::HashSet;

use crate::model::{Commit, ObjectId};

/// Layout for one screenful (plus overscan) of history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphLayout {
    pub rows: Vec<GraphRow>,
    /// The highest number of lanes any row used — what drives how much
    /// horizontal space the graph column has to reserve.
    pub width: usize,
}

/// One commit's row, including everything drawn in its vertical band.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    pub commit: ObjectId,
    /// The column this commit's node sits in.
    pub lane: usize,
    pub kind: NodeKind,
    /// Whether a lane was already descending into this commit.
    ///
    /// Edges describe what leaves a node, so without this the top half of a
    /// node's own lane has nothing describing it and a renderer would leave a
    /// gap above every commit that is not a branch tip.
    pub incoming: bool,
    pub edges: Vec<Edge>,
}

/// A line segment crossing one row's vertical band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    /// Lane at the top of the band.
    pub from: usize,
    /// Lane at the bottom of the band.
    pub to: usize,
    pub role: EdgeRole,
}

/// What an edge means, which is also the order it is drawn in.
///
/// The variants are ordered so that sorting a row's edges puts the lines that
/// carry meaning on top of the pass-throughs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeRole {
    /// Straight through: the commit in this row is not involved.
    Continue,
    /// This commit down to its first parent.
    Parent,
    /// An additional parent of a merge commit.
    Merge,
    /// A lane terminating here, because the commit it was waiting for arrived.
    Close,
}

/// What kind of node sits on a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Normal,
    /// More than one parent.
    Merge,
    /// No parents at all.
    Root,
    /// Has parents, but none of them are loaded — the bottom of a window, or a
    /// shallow clone. Drawn with a fade rather than a line into nothing, so a
    /// partially loaded history does not look like a truncated one.
    Boundary,
}

/// The lanes currently waiting for a commit to arrive.
///
/// Each slot holds the id of the commit that lane is descending toward. This
/// is the entire state the algorithm carries between rows, which is what makes
/// windowed and incremental layout possible at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneState {
    lanes: Vec<Option<ObjectId>>,
}

impl LaneState {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many lanes are currently in use.
    pub fn width(&self) -> usize {
        self.lanes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lanes.iter().all(Option::is_none)
    }

    /// The leftmost free slot, reusing a hole before growing the vector so the
    /// graph does not drift right forever.
    fn allocate(&mut self, awaiting: ObjectId) -> usize {
        match self.lanes.iter().position(Option::is_none) {
            Some(i) => {
                self.lanes[i] = Some(awaiting);
                i
            }
            None => {
                self.lanes.push(Some(awaiting));
                self.lanes.len() - 1
            }
        }
    }

    fn awaiting(&self, id: ObjectId) -> impl Iterator<Item = usize> + '_ {
        self.lanes
            .iter()
            .enumerate()
            .filter(move |(_, l)| **l == Some(id))
            .map(|(i, _)| i)
    }

    /// Drops trailing empty slots so `width` tracks actual concurrency rather
    /// than the high-water mark of the whole history.
    fn trim(&mut self) {
        while matches!(self.lanes.last(), Some(None)) {
            self.lanes.pop();
        }
    }
}

/// Lays out a complete history.
///
/// Convenience over [`layout_window`] for when every commit is in hand: tests,
/// benchmarks, and small repositories.
pub fn layout(commits: &[Commit]) -> GraphLayout {
    let known: HashSet<ObjectId> = commits.iter().map(|c| c.id).collect();
    let mut state = LaneState::new();
    layout_window(commits, &known, &mut state)
}

/// Lays out one window of history, carrying lane state in and out.
///
/// `known` is every commit id that has been loaded, not just the ones in this
/// window. The distinction matters: a commit at the bottom of a window whose
/// parent sits in the next page is an ordinary node, whereas a commit whose
/// parents are genuinely absent — a shallow clone's boundary — is a
/// [`NodeKind::Boundary`] and must not have an edge drawn into nothing.
///
/// `state` is advanced as rows are produced, so calling this repeatedly over
/// consecutive windows produces exactly the layout a single call over their
/// concatenation would.
pub fn layout_window(
    commits: &[Commit],
    known: &HashSet<ObjectId>,
    state: &mut LaneState,
) -> GraphLayout {
    let mut rows = Vec::with_capacity(commits.len());
    let mut width = state.width();

    for commit in commits {
        let row = layout_row(commit, known, state);
        width = width.max(row_width(&row));
        width = width.max(state.width());
        rows.push(row);
    }

    GraphLayout { rows, width }
}

/// The widest lane index this row touches, as a count.
fn row_width(row: &GraphRow) -> usize {
    row.edges
        .iter()
        .flat_map(|e| [e.from, e.to])
        .chain(std::iter::once(row.lane))
        .max()
        .map_or(0, |max| max + 1)
}

/// One commit, in the five steps `docs/COMMIT_GRAPH.md` specifies.
fn layout_row(commit: &Commit, known: &HashSet<ObjectId>, state: &mut LaneState) -> GraphRow {
    let before = state.lanes.clone();
    let mut edges = Vec::new();

    // 1. Claim a lane. Every lane waiting for this commit converges here; the
    //    leftmost becomes the commit's own and the rest close into it.
    let waiting: Vec<usize> = state.awaiting(commit.id).collect();
    let incoming = !waiting.is_empty();
    let lane = match waiting.split_first() {
        Some((&first, rest)) => {
            for &other in rest {
                edges.push(Edge {
                    from: other,
                    to: first,
                    role: EdgeRole::Close,
                });
                state.lanes[other] = None;
            }
            first
        }
        // Nothing is waiting for it, so this commit is a tip.
        None => state.allocate(commit.id),
    };

    // Only parents that are actually loaded may be routed to.
    let mut parents = commit.parents.iter().filter(|p| known.contains(p)).copied();

    // 2. The first parent inherits this commit's lane, which is what keeps a
    //    mainline readable as a single vertical line.
    match parents.next() {
        Some(first) => {
            state.lanes[lane] = Some(first);
            edges.push(Edge {
                from: lane,
                to: lane,
                role: EdgeRole::Parent,
            });
        }
        // 5. No reachable parent: the lane is free for the next tip.
        None => state.lanes[lane] = None,
    }

    // 3. Additional parents, for merges. A parent some lane already awaits is
    //    joined rather than allocated, so two branches merging the same commit
    //    do not each grow a column.
    for parent in parents {
        let existing = state.awaiting(parent).next();
        let target = match existing {
            Some(existing) => existing,
            None => state.allocate(parent),
        };
        edges.push(Edge {
            from: lane,
            to: target,
            role: EdgeRole::Merge,
        });
    }

    // 4. Every lane still passing through emits a straight segment, so the
    //    renderer can draw this row's full band without consulting its
    //    neighbours.
    for (i, slot) in state.lanes.iter().enumerate() {
        let passed_through = i != lane
            && slot.is_some()
            && before.get(i).is_some_and(Option::is_some)
            && !waiting.contains(&i);
        if passed_through {
            edges.push(Edge {
                from: i,
                to: i,
                role: EdgeRole::Continue,
            });
        }
    }

    state.trim();

    let kind = if commit.parents.is_empty() {
        NodeKind::Root
    } else if !commit.parents.iter().any(|p| known.contains(p)) {
        NodeKind::Boundary
    } else if commit.parents.len() > 1 {
        NodeKind::Merge
    } else {
        NodeKind::Normal
    };

    // Draw pass-throughs first and the meaningful lines on top of them.
    edges.sort_by(|a, b| a.role.cmp(&b.role).then(a.from.cmp(&b.from)));

    GraphRow {
        commit: commit.id,
        lane,
        kind,
        incoming,
        edges,
    }
}

/// Saved lane state, so jumping to an arbitrary scroll position resumes from
/// nearby instead of replaying the layout from `HEAD`.
#[derive(Debug, Clone)]
pub struct Checkpoints {
    interval: usize,
    /// `(row index, state before that row)`, in increasing row order.
    saved: Vec<(usize, LaneState)>,
}

impl Checkpoints {
    /// Snapshots lane state every `interval` rows. A smaller interval costs
    /// memory; a larger one costs replay time on a long scroll jump.
    pub fn new(interval: usize) -> Self {
        assert!(interval > 0, "a checkpoint interval of zero saves nothing");
        Self {
            interval,
            saved: Vec::new(),
        }
    }

    /// Walks a whole history for its checkpoints, discarding the rows.
    ///
    /// This is the pass that makes scrolling to an arbitrary position cheap:
    /// afterwards, laying out a screenful costs a replay of at most `interval`
    /// rows instead of a replay from `HEAD`. It is O(n) and belongs off the UI
    /// thread.
    pub fn build(commits: &[Commit], interval: usize) -> Self {
        let known: HashSet<ObjectId> = commits.iter().map(|c| c.id).collect();
        let mut state = LaneState::new();
        let mut checkpoints = Self::new(interval);

        for (i, commit) in commits.iter().enumerate() {
            checkpoints.record(i, &state);
            let _ = layout_row(commit, &known, &mut state);
        }

        checkpoints
    }

    /// Records the state that precedes `row`, if `row` falls on the interval.
    pub fn record(&mut self, row: usize, state: &LaneState) {
        if row.is_multiple_of(self.interval) && self.saved.last().map(|(r, _)| *r) != Some(row) {
            self.saved.push((row, state.clone()));
        }
    }

    /// The nearest checkpoint at or before `row`, and the state to resume from.
    ///
    /// Falls back to row 0 with empty lanes, which is always correct — just
    /// slower.
    pub fn resume_at(&self, row: usize) -> (usize, LaneState) {
        self.saved
            .iter()
            .rev()
            .find(|(r, _)| *r <= row)
            .map(|(r, s)| (*r, s.clone()))
            .unwrap_or((0, LaneState::new()))
    }

    pub fn len(&self) -> usize {
        self.saved.len()
    }

    pub fn is_empty(&self) -> bool {
        self.saved.is_empty()
    }
}

/// Lays out a whole history while recording checkpoints along the way.
pub fn layout_with_checkpoints(commits: &[Commit], interval: usize) -> (GraphLayout, Checkpoints) {
    let known: HashSet<ObjectId> = commits.iter().map(|c| c.id).collect();
    let mut state = LaneState::new();
    let mut checkpoints = Checkpoints::new(interval);
    let mut rows = Vec::with_capacity(commits.len());
    let mut width = 0;

    for (i, commit) in commits.iter().enumerate() {
        checkpoints.record(i, &state);
        let row = layout_row(commit, &known, &mut state);
        width = width.max(row_width(&row));
        width = width.max(state.width());
        rows.push(row);
    }

    (GraphLayout { rows, width }, checkpoints)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    use crate::model::Signature;

    /// Builds a synthetic commit. Ids are `aa…`, `bb…` and so on, so a failure
    /// message names the commit the test's prose does.
    fn commit(name: char, parents: &[char]) -> Commit {
        let id = |c: char| {
            ObjectId::from_hex(&format!("{:02x}", c as u8).repeat(20)).expect("a valid hash")
        };
        let who = Signature {
            name: "test".into(),
            email: "test@hidegit.invalid".into(),
            time: OffsetDateTime::UNIX_EPOCH,
        };

        Commit {
            id: id(name),
            parents: parents.iter().copied().map(id).collect(),
            summary: name.to_string(),
            body: None,
            author: who.clone(),
            committer: who,
            time: OffsetDateTime::UNIX_EPOCH,
            refs: Vec::new(),
        }
    }

    /// The lane each row's node sits in.
    fn lanes(layout: &GraphLayout) -> Vec<usize> {
        layout.rows.iter().map(|r| r.lane).collect()
    }

    fn edges_of(layout: &GraphLayout, row: usize, role: EdgeRole) -> Vec<(usize, usize)> {
        layout.rows[row]
            .edges
            .iter()
            .filter(|e| e.role == role)
            .map(|e| (e.from, e.to))
            .collect()
    }

    #[test]
    fn linear_history_stays_in_one_column() {
        // C → B → A
        let history = [commit('C', &['B']), commit('B', &['A']), commit('A', &[])];
        let layout = layout(&history);

        assert_eq!(lanes(&layout), vec![0, 0, 0]);
        assert_eq!(layout.width, 1);
        assert_eq!(layout.rows[2].kind, NodeKind::Root);
        assert_eq!(edges_of(&layout, 0, EdgeRole::Parent), vec![(0, 0)]);
        assert!(edges_of(&layout, 2, EdgeRole::Parent).is_empty());
    }

    #[test]
    fn a_branch_and_merge_takes_a_second_lane_and_gives_it_back() {
        // The worked example from docs/COMMIT_GRAPH.md:
        //   A → B → C, where C merges E (first parent) and D, and D → E.
        let history = [
            commit('A', &['B']),
            commit('B', &['C']),
            commit('C', &['E', 'D']),
            commit('D', &['E']),
            commit('E', &[]),
        ];
        let layout = layout(&history);

        assert_eq!(lanes(&layout), vec![0, 0, 0, 1, 0]);
        assert_eq!(layout.width, 2);

        assert_eq!(layout.rows[2].kind, NodeKind::Merge);
        assert_eq!(
            edges_of(&layout, 2, EdgeRole::Merge),
            vec![(0, 1)],
            "the second parent takes a new lane to the right"
        );

        assert_eq!(
            edges_of(&layout, 4, EdgeRole::Close),
            vec![(1, 0)],
            "both lanes await E, so the right one closes into the left"
        );
        assert_eq!(layout.rows[4].kind, NodeKind::Root);
    }

    #[test]
    fn a_tip_has_no_incoming_lane_and_everything_else_does() {
        let history = [commit('A', &['B']), commit('B', &[]), commit('X', &[])];
        let layout = layout(&history);

        assert!(!layout.rows[0].incoming, "A is a branch tip");
        assert!(layout.rows[1].incoming, "B is descended into from A");
        assert!(!layout.rows[2].incoming, "X is an unrelated tip");
    }

    #[test]
    fn first_parent_history_holds_its_column_across_a_merge() {
        let history = [
            commit('M', &['A', 'B']),
            commit('A', &['R']),
            commit('B', &['R']),
            commit('R', &[]),
        ];
        let layout = layout(&history);

        assert_eq!(
            layout.rows[1].lane, layout.rows[0].lane,
            "the first parent inherits the merge commit's lane"
        );
        assert_ne!(layout.rows[2].lane, layout.rows[0].lane);
    }

    #[test]
    fn an_octopus_merge_routes_every_parent() {
        let history = [
            commit('M', &['A', 'B', 'C']),
            commit('A', &['R']),
            commit('B', &['R']),
            commit('C', &['R']),
            commit('R', &[]),
        ];
        let layout = layout(&history);

        assert_eq!(layout.rows[0].kind, NodeKind::Merge);
        assert_eq!(
            edges_of(&layout, 0, EdgeRole::Merge),
            vec![(0, 1), (0, 2)],
            "two additional parents, two new lanes"
        );
        assert_eq!(layout.width, 3);

        // All three converge on R, so two lanes close into the leftmost.
        assert_eq!(edges_of(&layout, 4, EdgeRole::Close), vec![(1, 0), (2, 0)]);
        assert_eq!(layout.rows[4].lane, 0);
    }

    #[test]
    fn multiple_roots_each_terminate_their_own_lane() {
        // Two independent histories, as a grafted or imported repository has.
        let history = [
            commit('A', &['B']),
            commit('X', &['Y']),
            commit('B', &[]),
            commit('Y', &[]),
        ];
        let layout = layout(&history);

        assert_eq!(lanes(&layout), vec![0, 1, 0, 1]);
        assert_eq!(layout.width, 2);
        assert_eq!(layout.rows[2].kind, NodeKind::Root);
        assert_eq!(layout.rows[3].kind, NodeKind::Root);
    }

    #[test]
    fn a_freed_lane_is_reused_before_the_graph_grows_rightward() {
        // A's lane frees at its root, and the next tip must reclaim it rather
        // than drifting right forever.
        let history = [
            commit('A', &['B']),
            commit('B', &[]),
            commit('X', &['Y']),
            commit('Y', &[]),
        ];
        let layout = layout(&history);

        assert_eq!(lanes(&layout), vec![0, 0, 0, 0]);
        assert_eq!(
            layout.width, 1,
            "width tracks concurrency, not history length"
        );
    }

    #[test]
    fn an_orphan_branch_shares_no_lane_with_the_mainline() {
        let history = [commit('A', &['B']), commit('Z', &[]), commit('B', &[])];
        let layout = layout(&history);

        assert_eq!(layout.rows[1].kind, NodeKind::Root);
        assert_ne!(
            layout.rows[1].lane, layout.rows[0].lane,
            "an orphan tip cannot take a lane the mainline is still using"
        );
    }

    #[test]
    fn criss_cross_merges_do_not_allocate_a_lane_per_edge() {
        // Two branches that each merge the other.
        let history = [
            commit('M', &['P', 'Q']),
            commit('P', &['A', 'B']),
            commit('Q', &['B', 'A']),
            commit('A', &['R']),
            commit('B', &['R']),
            commit('R', &[]),
        ];
        let layout = layout(&history);

        assert_eq!(
            layout.width, 3,
            "P and Q both reach A and B, and joining an awaited lane must not allocate"
        );
        assert_eq!(layout.rows[5].lane, 0, "everything converges on the root");
    }

    #[test]
    fn a_commit_whose_parents_are_not_loaded_is_a_boundary() {
        // A shallow clone: B's parent exists upstream but not here.
        let history = [commit('A', &['B']), commit('B', &['C'])];
        let layout = layout(&history);

        assert_eq!(layout.rows[0].kind, NodeKind::Normal);
        assert_eq!(layout.rows[1].kind, NodeKind::Boundary);
        assert!(
            edges_of(&layout, 1, EdgeRole::Parent).is_empty(),
            "never draw an edge to a commit that is not there"
        );
    }

    #[test]
    fn a_windows_layout_matches_the_corresponding_slice_of_a_full_layout() {
        let history = [
            commit('A', &['B']),
            commit('B', &['C', 'D']),
            commit('C', &['E']),
            commit('D', &['E']),
            commit('E', &['F']),
            commit('F', &[]),
        ];
        let full = layout(&history);
        let known: HashSet<ObjectId> = history.iter().map(|c| c.id).collect();

        let mut state = LaneState::new();
        let mut windowed = Vec::new();
        for chunk in history.chunks(2) {
            windowed.extend(layout_window(chunk, &known, &mut state).rows);
        }

        assert_eq!(
            windowed, full.rows,
            "windowing is an optimisation, and an optimisation that changes the output is a defect"
        );
    }

    #[test]
    fn resuming_from_a_checkpoint_equals_replaying_from_the_start() {
        let history: Vec<Commit> = (0..40u8)
            .map(|i| {
                let name = |n: u8| (b'A' + n % 26) as char;
                if i == 39 {
                    commit(name(i), &[])
                } else if i % 7 == 0 {
                    commit(name(i), &[name(i + 1), name(i + 2)])
                } else {
                    commit(name(i), &[name(i + 1)])
                }
            })
            .collect();

        let (full, checkpoints) = layout_with_checkpoints(&history, 8);
        assert!(!checkpoints.is_empty());

        let known: HashSet<ObjectId> = history.iter().map(|c| c.id).collect();
        let (row, mut state) = checkpoints.resume_at(20);
        let resumed = layout_window(&history[row..], &known, &mut state);

        assert_eq!(
            resumed.rows,
            full.rows[row..],
            "a checkpoint resume must produce exactly what a full replay does"
        );
    }

    #[test]
    fn the_same_history_always_produces_the_same_layout() {
        let history = [
            commit('A', &['B', 'C']),
            commit('B', &['D']),
            commit('C', &['D']),
            commit('D', &[]),
        ];

        let first = layout(&history);
        let second = layout(&history);

        assert_eq!(
            first, second,
            "determinism is what both the tests and a stable view across refreshes depend on"
        );
    }

    #[test]
    fn pass_through_lanes_are_emitted_so_a_row_can_be_drawn_alone() {
        let history = [
            commit('A', &['B', 'C']),
            commit('B', &['D']),
            commit('C', &['D']),
            commit('D', &[]),
        ];
        let layout = layout(&history);

        // Row 1 draws B in lane 0, while the lane waiting for C passes through.
        assert_eq!(edges_of(&layout, 1, EdgeRole::Continue), vec![(1, 1)]);
    }

    #[test]
    fn pass_throughs_are_drawn_before_the_lines_that_carry_meaning() {
        let history = [
            commit('A', &['B', 'C']),
            commit('B', &['D']),
            commit('C', &['D']),
            commit('D', &[]),
        ];
        let layout = layout(&history);

        for row in &layout.rows {
            let roles: Vec<EdgeRole> = row.edges.iter().map(|e| e.role).collect();
            let mut sorted = roles.clone();
            sorted.sort();
            assert_eq!(roles, sorted, "edges come out in drawing order");
        }
    }

    #[test]
    fn building_checkpoints_separately_matches_building_them_inline() {
        let history = [
            commit('A', &['B']),
            commit('B', &['C', 'D']),
            commit('C', &['E']),
            commit('D', &['E']),
            commit('E', &[]),
        ];

        let (_, inline) = layout_with_checkpoints(&history, 2);
        let standalone = Checkpoints::build(&history, 2);

        for row in 0..history.len() {
            assert_eq!(
                inline.resume_at(row).0,
                standalone.resume_at(row).0,
                "the row a resume starts from must not depend on how it was built"
            );
            assert_eq!(inline.resume_at(row).1, standalone.resume_at(row).1);
        }
    }

    #[test]
    fn an_empty_history_lays_out_to_nothing() {
        let layout = layout(&[]);
        assert!(layout.rows.is_empty());
        assert_eq!(layout.width, 0);
    }
}
