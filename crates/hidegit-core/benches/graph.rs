//! Benchmarks for the read path and the graph layout, at 100,000 commits.
//!
//! The roadmap's target is 60fps scrolling on a repository that size. That is a
//! target, not a measurement, so this exists to turn it into one — from M1,
//! rather than discovering the truth at M6.
//!
//! The number that decides whether scrolling is smooth is
//! `visible_window_at_row_50000`: the work done per frame. Everything else here
//! happens once, when a repository is opened, and belongs off the UI thread
//! regardless of how fast it is.
//!
//! ```sh
//! cargo bench -p hidegit-core
//! ```

use std::collections::HashSet;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use hidegit_core::backend::{GitBackend, HybridBackend};
use hidegit_core::fixture::fixture;
use hidegit_core::graph::{CHECKPOINT_INTERVAL, Checkpoints, LaneState, layout, layout_window};
use hidegit_core::model::{Commit, LogPage, ObjectId, RevSpec};

/// How many commits the benchmarks run against.
const COMMITS: usize = 100_000;
/// How often the generated history forks and merges back.
const BRANCH_EVERY: usize = 50;
/// Rows a frame lays out: a tall viewport plus overscan on both sides.
const VIEWPORT: usize = 60 + 32;

/// Builds a real 100,000-commit repository and reads its history back.
///
/// Done once and shared, because generating it is the slow part and it is not
/// what any of these benchmarks measure.
fn history() -> Vec<Commit> {
    let repo = fixture().generate(COMMITS, BRANCH_EVERY).build();
    let backend = HybridBackend::open(repo.path()).expect("a generated fixture opens");

    backend
        .log(
            &RevSpec::All,
            LogPage {
                skip: 0,
                limit: COMMITS * 2,
            },
        )
        .expect("history is readable")
}

fn read_path(c: &mut Criterion) {
    let repo = fixture().generate(COMMITS, BRANCH_EVERY).build();
    let backend = HybridBackend::open(repo.path()).expect("a generated fixture opens");

    let mut group = c.benchmark_group("read");
    // These run once per repository, off the UI thread. Sample sizes are small
    // because each iteration walks a hundred thousand commits.
    group.sample_size(10);

    group.bench_function("walk_and_order_100k", |b| {
        b.iter(|| {
            backend.invalidate();
            black_box(backend.commit_count(&RevSpec::All).expect("counting works"))
        });
    });

    // What a file save costs now. The comparison that matters is against
    // `walk_and_order_100k` directly above: that walk is what every editor save
    // used to pay, because the watcher invalidated the memo whatever had
    // changed. A worktree change now reads the working directory and nothing
    // else.
    group.bench_function("status_after_a_file_save_100k", |b| {
        b.iter(|| black_box(backend.status().expect("status is readable")));
    });

    // The claim under test: paging is O(page), not O(total). `log` walks and
    // then slices, so it *looks* like every page pays for the whole history —
    // but the walk is memoised, so only the first one does. If these two
    // diverge, the memo has stopped working.
    group.bench_function("hydrate_a_deep_page_of_2000_at_row_50000", |b| {
        b.iter(|| {
            black_box(
                backend
                    .log(
                        &RevSpec::All,
                        LogPage {
                            skip: 50_000,
                            limit: 2_000,
                        },
                    )
                    .expect("a deep page is readable"),
            )
        });
    });

    group.bench_function("hydrate_one_page_of_2000", |b| {
        b.iter(|| {
            black_box(
                backend
                    .log(&RevSpec::All, LogPage::first(2_000))
                    .expect("a page is readable"),
            )
        });
    });

    group.finish();
}

fn graph_layout(c: &mut Criterion) {
    let commits = history();
    assert!(
        commits.len() >= COMMITS,
        "the generated history is smaller than the benchmark assumes"
    );

    let known: HashSet<ObjectId> = commits.iter().map(|c| c.id).collect();
    let checkpoints = Checkpoints::build(&commits, CHECKPOINT_INTERVAL);

    let mut group = c.benchmark_group("graph");

    group.sample_size(10);
    group.bench_function("layout_whole_history_100k", |b| {
        b.iter(|| black_box(layout(&commits)));
    });
    group.bench_function("build_checkpoints_100k", |b| {
        b.iter(|| black_box(Checkpoints::build(&commits, CHECKPOINT_INTERVAL)));
    });

    // The per-frame cost. A 60fps budget is 16.6ms for everything the frame
    // does, so this needs to be a small fraction of that.
    group.sample_size(100);
    group.bench_function("visible_window_at_row_50000", |b| {
        b.iter(|| {
            let (from, mut state) = checkpoints.resume_at(50_000);
            black_box(layout_window(
                &commits[from..50_000 + VIEWPORT],
                &known,
                &mut state,
            ))
        });
    });

    // The same work without checkpoints, which is what the UI would pay on
    // every frame if the O(n) pass were skipped. Kept as a benchmark rather
    // than a comment so the difference stays visible.
    group.sample_size(10);
    group.bench_function("visible_window_at_row_50000_without_checkpoints", |b| {
        b.iter(|| {
            let mut state = LaneState::new();
            black_box(layout_window(
                &commits[..50_000 + VIEWPORT],
                &known,
                &mut state,
            ))
        });
    });

    group.finish();
}

criterion_group!(benches, read_path, graph_layout);
criterion_main!(benches);
