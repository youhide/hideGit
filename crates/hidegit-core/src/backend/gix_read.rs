//! The read half of the hybrid backend, implemented over gitoxide.
//!
//! Nothing here shells out. Everything that reads a repository — refs, log,
//! commit, diff, blob — runs in-process on `gix`, which is what makes the read
//! path fast enough to lay out a 100,000-commit graph and what removes the C
//! toolchain from the build. See `docs/adr/0002-git-backend-hybrid.md`.

use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{Algorithm, InternedInput, UnifiedDiff, sources::byte_lines};

use crate::error::GitError;
use crate::model::{
    Blob, Branch, ChangeStatus, Commit, CommitDetail, Diff, DiffLine, DiffStats, DiffTarget,
    FileChange, FileDiff, FileDiffContent, Head, Hunk, LineKind, ObjectId, RefKind, RefName, Refs,
    RepoState, RevSpec, Signature, Tag,
};

/// Files past this size get a placeholder instead of a diff.
///
/// Rendering a multi-megabyte file line by line is a hang, and a hang is worse
/// than an honest "not shown".
const MAX_DIFF_BYTES: u64 = 5 * 1024 * 1024;

/// Lines of context around each hunk, matching `git diff`'s default.
const DIFF_CONTEXT: u32 = 3;

/// One commit as the traversal sees it: enough to order the graph, without
/// paying to decode messages and identities for commits nobody has scrolled to.
#[derive(Debug, Clone)]
pub(crate) struct WalkEntry {
    pub(crate) id: ObjectId,
    pub(crate) parents: Vec<ObjectId>,
    pub(crate) time: i64,
}

/// Opens the repository containing `path`, searching upward.
pub(crate) fn open(path: &Path) -> Result<gix::ThreadSafeRepository, GitError> {
    match gix::ThreadSafeRepository::discover(path) {
        Ok(repo) => Ok(repo),
        Err(gix::discover::Error::Discover(_)) => Err(GitError::NotARepository(path.to_path_buf())),
        Err(e) => Err(GitError::gix("opening the repository", e)),
    }
}

fn to_id(id: &gix::hash::oid) -> ObjectId {
    ObjectId::from_bytes(id.as_bytes()).expect("gix only produces sha-1 and sha-256 hashes")
}

fn to_ref_name(name: &gix::refs::FullNameRef) -> RefName {
    let kind = match name.category() {
        Some(gix::refs::Category::LocalBranch) => RefKind::LocalBranch,
        Some(gix::refs::Category::RemoteBranch) => RefKind::RemoteBranch,
        Some(gix::refs::Category::Tag) => RefKind::Tag,
        _ => RefKind::Special,
    };

    RefName {
        kind,
        full: name.as_bstr().to_str_lossy().into_owned(),
        short: name.shorten().to_str_lossy().into_owned(),
    }
}

/// Converts a Git timestamp into an offset-aware one.
///
/// Git records seconds since the epoch plus the committer's UTC offset, and
/// both are needed: displaying a commit in the reader's timezone loses the
/// information that it was made at 2am local time.
fn to_time(time: gix::date::Time) -> time::OffsetDateTime {
    let offset = time::UtcOffset::from_whole_seconds(time.offset).unwrap_or(time::UtcOffset::UTC);
    time::OffsetDateTime::from_unix_timestamp(time.seconds)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .to_offset(offset)
}

fn to_signature(sig: gix::actor::SignatureRef<'_>) -> Signature {
    Signature {
        name: sig.name.to_str_lossy().trim().to_owned(),
        email: sig.email.to_str_lossy().trim().to_owned(),
        time: sig
            .time()
            .map(to_time)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH),
    }
}

pub(crate) fn head(repo: &gix::Repository) -> Result<Head, GitError> {
    let head = repo.head().map_err(|e| GitError::gix("reading HEAD", e))?;

    if head.is_unborn() {
        // A freshly initialised repository. Not an error: the UI has to render
        // it, and the branch name is the one the first commit will create.
        let name = head
            .referent_name()
            .map(to_ref_name)
            .unwrap_or_else(|| RefName {
                kind: RefKind::LocalBranch,
                full: "refs/heads/main".to_owned(),
                short: "main".to_owned(),
            });
        return Ok(Head::Unborn { name });
    }

    let target = head
        .id()
        .map(|id| to_id(&id))
        .ok_or_else(|| GitError::RefNotFound("HEAD".to_owned()))?;

    Ok(match head.referent_name() {
        Some(name) => Head::Branch {
            name: to_ref_name(name),
            target,
        },
        None => Head::Detached { target },
    })
}

pub(crate) fn refs(repo: &gix::Repository) -> Result<Refs, GitError> {
    let platform = repo
        .references()
        .map_err(|e| GitError::gix("listing references", e))?;
    let iter = platform
        .all()
        .map_err(|e| GitError::gix("listing references", e))?;

    let mut out = Refs::default();

    for reference in iter {
        let mut reference = match reference {
            Ok(r) => r,
            // A single broken ref must not blank the whole sidebar.
            Err(e) => {
                tracing::warn!(error = %e, "skipping unreadable reference");
                continue;
            }
        };

        let name = to_ref_name(reference.name());
        let unpeeled = reference.target().id().to_owned();

        let peeled = match reference.peel_to_id() {
            Ok(id) => to_id(&id),
            Err(e) => {
                tracing::warn!(ref_name = %name.full, error = %e, "skipping unpeelable reference");
                continue;
            }
        };

        match name.kind {
            RefKind::LocalBranch => {
                let upstream = repo
                    .branch_remote_tracking_ref_name(
                        reference.name(),
                        gix::remote::Direction::Fetch,
                    )
                    .and_then(|r| r.ok())
                    .map(|name| name.as_bstr().to_str_lossy().into_owned());
                out.locals.push(Branch {
                    name,
                    target: peeled,
                    upstream,
                });
            }
            RefKind::RemoteBranch => out.remotes.push(Branch {
                name,
                target: peeled,
                upstream: None,
            }),
            RefKind::Tag => {
                // An annotated tag has its own object, so the ref points at a
                // tag rather than straight at the commit.
                let annotated = to_id(&unpeeled) != peeled;
                out.tags.push(Tag {
                    name,
                    target: peeled,
                    annotated,
                });
            }
            RefKind::Special => {}
        }
    }

    out.locals.sort_by(|a, b| a.name.short.cmp(&b.name.short));
    out.remotes.sort_by(|a, b| a.name.short.cmp(&b.name.short));
    out.tags.sort_by(|a, b| a.name.short.cmp(&b.name.short));

    Ok(out)
}

pub(crate) fn repo_state(repo: &gix::Repository) -> RepoState {
    use gix::state::InProgress;

    match repo.state() {
        None => RepoState::Clean,
        Some(state) => match state {
            InProgress::Merge => RepoState::Merging,
            InProgress::Rebase | InProgress::RebaseInteractive => RepoState::Rebasing,
            InProgress::ApplyMailbox | InProgress::ApplyMailboxRebase => RepoState::Rebasing,
            InProgress::CherryPick | InProgress::CherryPickSequence => RepoState::CherryPicking,
            InProgress::Revert | InProgress::RevertSequence => RepoState::Reverting,
            InProgress::Bisect => RepoState::Bisecting,
        },
    }
}

/// Resolves a [`RevSpec`] to the commits a walk should start from.
fn tips(repo: &gix::Repository, spec: &RevSpec) -> Result<Vec<gix::ObjectId>, GitError> {
    match spec {
        RevSpec::Head => match repo.head_id() {
            Ok(id) => Ok(vec![id.detach()]),
            // An unborn HEAD has no history yet; an empty walk, not an error.
            Err(_) => Ok(Vec::new()),
        },
        RevSpec::All => {
            let refs = refs(repo)?;
            let mut ids: Vec<gix::ObjectId> = refs
                .locals
                .iter()
                .chain(&refs.remotes)
                .map(|b| b.target)
                .chain(refs.tags.iter().map(|t| t.target))
                .filter_map(|id| gix::ObjectId::from_hex(id.to_hex().as_bytes()).ok())
                .collect();

            // HEAD may be detached and therefore reachable from no ref at all.
            if let Ok(head) = repo.head_id() {
                ids.push(head.detach());
            }

            ids.sort();
            ids.dedup();
            Ok(ids)
        }
        RevSpec::Ref(name) => {
            let id = repo
                .rev_parse_single(name.as_str())
                .map_err(|_| GitError::RefNotFound(name.clone()))?;
            Ok(vec![id.detach()])
        }
        RevSpec::Commit(id) => {
            let oid = gix::ObjectId::from_hex(id.to_hex().as_bytes())
                .map_err(|_| GitError::RefNotFound(id.to_hex()))?;
            Ok(vec![oid])
        }
    }
}

/// Walks `spec` and returns every reachable commit in display order.
///
/// The order is topological with commit date as the tiebreak — a commit always
/// precedes its parents. gitoxide offers date order and breadth-first order but
/// not topological order, so the date-ordered walk here is corrected by a
/// Kahn-style pass afterwards. Date order alone is not enough: clock skew and
/// rebases both produce commits whose timestamps predate their children, which
/// would draw edges pointing upward.
pub(crate) fn walk(repo: &gix::Repository, spec: &RevSpec) -> Result<Vec<WalkEntry>, GitError> {
    let tips = tips(repo, spec)?;
    if tips.is_empty() {
        return Ok(Vec::new());
    }

    let walk = repo
        .rev_walk(tips)
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ))
        .all()
        .map_err(|e| GitError::gix("walking history", e))?;

    let mut entries = Vec::new();
    for info in walk {
        let info = info.map_err(|e| GitError::gix("walking history", e))?;
        entries.push(WalkEntry {
            id: to_id(&info.id),
            parents: info.parent_ids.iter().map(|p| to_id(p)).collect(),
            time: info.commit_time.unwrap_or_default(),
        });
    }

    Ok(topological_order(entries))
}

/// Reorders a date-ordered walk so every commit precedes its parents.
///
/// Kahn's algorithm over the child→parent edges, with a max-heap on
/// `(commit time, id)` deciding between commits that are simultaneously ready.
/// The id participates so the result is deterministic: the same input always
/// produces the same layout, which both the tests and a stable view across
/// refreshes depend on.
fn topological_order(entries: Vec<WalkEntry>) -> Vec<WalkEntry> {
    let n = entries.len();
    let index: HashMap<ObjectId, usize> =
        entries.iter().enumerate().map(|(i, e)| (e.id, i)).collect();

    // How many commits in this set have `entries[i]` as a parent.
    let mut children = vec![0usize; n];
    for entry in &entries {
        for parent in &entry.parents {
            if let Some(&i) = index.get(parent) {
                children[i] += 1;
            }
        }
    }

    let mut ready: BinaryHeap<(i64, ObjectId, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(i, _)| children[*i] == 0)
        .map(|(i, e)| (e.time, e.id, i))
        .collect();

    let mut ordered = Vec::with_capacity(n);
    let mut emitted = vec![false; n];

    while let Some((_, _, i)) = ready.pop() {
        emitted[i] = true;
        for parent in &entries[i].parents {
            if let Some(&j) = index.get(parent) {
                children[j] -= 1;
                if children[j] == 0 {
                    ready.push((entries[j].time, entries[j].id, j));
                }
            }
        }
        ordered.push(entries[i].clone());
    }

    // Defensive: a cycle would leave commits unemitted. Git histories are
    // acyclic, but a corrupt repository must still render rather than silently
    // lose rows.
    if ordered.len() != n {
        tracing::warn!(
            expected = n,
            ordered = ordered.len(),
            "history did not sort topologically; appending the remainder in date order"
        );
        for (i, entry) in entries.into_iter().enumerate() {
            if !emitted[i] {
                ordered.push(entry);
            }
        }
    }

    ordered
}

/// Loads the full commit objects for one page of a walk.
///
/// Separated from [`walk`] because ordering the graph needs only ids, parents
/// and timestamps, while messages and identities are what a screenful of rows
/// actually displays. Decoding those for 100,000 commits to draw 40 of them is
/// the difference between a fast first screen and a stall.
pub(crate) fn hydrate(
    repo: &gix::Repository,
    entries: &[WalkEntry],
    refs: &Refs,
) -> Result<Vec<Commit>, GitError> {
    entries
        .iter()
        .map(|entry| {
            let oid = gix::ObjectId::from_hex(entry.id.to_hex().as_bytes())
                .map_err(|_| GitError::RefNotFound(entry.id.to_hex()))?;
            let commit = repo
                .find_commit(oid)
                .map_err(|e| GitError::gix("reading a commit", e))?;

            let message = commit
                .message()
                .map_err(|e| GitError::gix("decoding a commit message", e))?;
            let author = commit
                .author()
                .map_err(|e| GitError::gix("decoding a commit author", e))?;
            let committer = commit
                .committer()
                .map_err(|e| GitError::gix("decoding a commit committer", e))?;
            let committer = to_signature(committer);

            Ok(Commit {
                id: entry.id,
                parents: entry.parents.clone(),
                summary: message.summary().to_str_lossy().into_owned(),
                body: message
                    .body
                    .map(|b| b.to_str_lossy().trim().to_owned())
                    .filter(|b| !b.is_empty()),
                author: to_signature(author),
                time: committer.time,
                committer,
                refs: refs.pointing_at(entry.id),
            })
        })
        .collect()
}

pub(crate) fn commit_detail(
    repo: &gix::Repository,
    id: ObjectId,
) -> Result<CommitDetail, GitError> {
    let refs = refs(repo)?;
    let oid = gix::ObjectId::from_hex(id.to_hex().as_bytes())
        .map_err(|_| GitError::RefNotFound(id.to_hex()))?;
    let commit = repo
        .find_commit(oid)
        .map_err(|e| GitError::gix("reading a commit", e))?;

    let entry = WalkEntry {
        id,
        parents: commit.parent_ids().map(|p| to_id(&p)).collect(),
        time: commit.time().map(|t| t.seconds).unwrap_or_default(),
    };
    let commit = hydrate(repo, std::slice::from_ref(&entry), &refs)?
        .pop()
        .expect("hydrating one entry yields one commit");

    let diff = diff(repo, &DiffTarget::Commit(id))?;
    let changes = diff
        .files
        .iter()
        .map(|f| FileChange {
            path: f.path.clone(),
            status: f.status.clone(),
        })
        .collect();

    Ok(CommitDetail {
        commit,
        changes,
        stats: diff.stats,
    })
}

pub(crate) fn read_blob(repo: &gix::Repository, id: ObjectId) -> Result<Blob, GitError> {
    let oid = gix::ObjectId::from_hex(id.to_hex().as_bytes())
        .map_err(|_| GitError::RefNotFound(id.to_hex()))?;
    let object = repo
        .find_object(oid)
        .map_err(|e| GitError::gix("reading a blob", e))?;

    Ok(Blob {
        id,
        bytes: object.into_blob().data.clone(),
    })
}

pub(crate) fn diff(repo: &gix::Repository, target: &DiffTarget) -> Result<Diff, GitError> {
    let (old, new) = match target {
        DiffTarget::Commit(id) => {
            let oid = gix::ObjectId::from_hex(id.to_hex().as_bytes())
                .map_err(|_| GitError::RefNotFound(id.to_hex()))?;
            let commit = repo
                .find_commit(oid)
                .map_err(|e| GitError::gix("reading a commit", e))?;
            // A root commit diffs against nothing, so every file reads as
            // added — which is what `git show` does too.
            let parent = commit.parent_ids().next().map(|p| p.detach());
            (parent, Some(commit.id().detach()))
        }
        DiffTarget::Range { from, to } => {
            let from = gix::ObjectId::from_hex(from.to_hex().as_bytes())
                .map_err(|_| GitError::RefNotFound(from.to_hex()))?;
            let to = gix::ObjectId::from_hex(to.to_hex().as_bytes())
                .map_err(|_| GitError::RefNotFound(to.to_hex()))?;
            (Some(from), Some(to))
        }
        DiffTarget::Staged | DiffTarget::Unstaged => {
            return Err(super::not_implemented(
                "diffing the working directory",
                "M2",
            ));
        }
    };

    let old_tree = match old {
        Some(id) => Some(
            repo.find_commit(id)
                .map_err(|e| GitError::gix("reading a commit", e))?
                .tree()
                .map_err(|e| GitError::gix("reading a tree", e))?,
        ),
        None => None,
    };
    let new_tree = match new {
        Some(id) => Some(
            repo.find_commit(id)
                .map_err(|e| GitError::gix("reading a commit", e))?
                .tree()
                .map_err(|e| GitError::gix("reading a tree", e))?,
        ),
        None => None,
    };

    let changes = repo
        .diff_tree_to_tree(old_tree.as_ref(), new_tree.as_ref(), None)
        .map_err(|e| GitError::gix("diffing two trees", e))?;

    let mut files = Vec::with_capacity(changes.len());
    for change in changes {
        files.push(to_file_diff(repo, change)?);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let stats = summarise(&files);
    Ok(Diff { files, stats })
}

fn path_from(location: &gix::bstr::BStr) -> PathBuf {
    PathBuf::from(location.to_str_lossy().into_owned())
}

fn to_file_diff(
    repo: &gix::Repository,
    change: gix::diff::tree_with_rewrites::Change,
) -> Result<FileDiff, GitError> {
    use gix::diff::tree_with_rewrites::Change as C;

    let (path, status, old_id, new_id) = match change {
        C::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            // A new symlink or submodule has no text to show.
            let _ = entry_mode;
            (
                path_from(location.as_ref()),
                ChangeStatus::Added,
                None,
                Some(id),
            )
        }
        C::Deletion { location, id, .. } => (
            path_from(location.as_ref()),
            ChangeStatus::Deleted,
            Some(id),
            None,
        ),
        C::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let status = if previous_entry_mode.kind() == entry_mode.kind() {
                ChangeStatus::Modified
            } else {
                ChangeStatus::TypeChange
            };
            (
                path_from(location.as_ref()),
                status,
                Some(previous_id),
                Some(id),
            )
        }
        C::Rewrite {
            source_location,
            source_id,
            location,
            id,
            copy,
            ..
        } => {
            let from = path_from(source_location.as_ref());
            let status = if copy {
                ChangeStatus::Copied { from }
            } else {
                ChangeStatus::Renamed { from }
            };
            (
                path_from(location.as_ref()),
                status,
                Some(source_id),
                Some(id),
            )
        }
    };

    let old = load_side(repo, old_id)?;
    let new = load_side(repo, new_id)?;

    let content = match (&old, &new) {
        (Side::TooLarge(bytes), _) | (_, Side::TooLarge(bytes)) => {
            FileDiffContent::TooLarge { bytes: *bytes }
        }
        (Side::Binary, _) | (_, Side::Binary) => FileDiffContent::Binary,
        (Side::Text(old), Side::Text(new)) => FileDiffContent::Text {
            hunks: hunks(old, new)?,
        },
    };

    Ok(FileDiff {
        path,
        status,
        content,
    })
}

/// One side of a file diff, after the checks that decide whether it can be
/// rendered as text at all.
enum Side {
    Text(Vec<u8>),
    Binary,
    TooLarge(u64),
}

fn load_side(repo: &gix::Repository, id: Option<gix::ObjectId>) -> Result<Side, GitError> {
    let Some(id) = id else {
        return Ok(Side::Text(Vec::new()));
    };

    let object = repo
        .find_object(id)
        .map_err(|e| GitError::gix("reading a blob", e))?;

    if object.kind != gix::object::Kind::Blob {
        // A submodule entry points at a commit, not a blob; there is no text.
        return Ok(Side::Binary);
    }

    let data = object.into_blob().data.clone();

    if data.len() as u64 > MAX_DIFF_BYTES {
        return Ok(Side::TooLarge(data.len() as u64));
    }
    // The same heuristic Git uses: a NUL byte in the first 8000 bytes.
    if data[..data.len().min(8000)].contains(&0) {
        return Ok(Side::Binary);
    }

    Ok(Side::Text(data))
}

/// Collects unified-diff hunks into the domain model.
#[derive(Default)]
struct HunkCollector {
    hunks: Vec<Hunk>,
}

impl ConsumeHunk for HunkCollector {
    type Out = Vec<Hunk>;

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        let mut old_lineno = header.before_hunk_start;
        let mut new_lineno = header.after_hunk_start;

        let lines = lines
            .iter()
            .map(|(kind, text)| {
                let text = String::from_utf8_lossy(text)
                    .trim_end_matches('\n')
                    .to_owned();
                match kind {
                    DiffLineKind::Context => {
                        let line = DiffLine {
                            kind: LineKind::Context,
                            old_lineno: Some(old_lineno),
                            new_lineno: Some(new_lineno),
                            text,
                        };
                        old_lineno += 1;
                        new_lineno += 1;
                        line
                    }
                    DiffLineKind::Remove => {
                        let line = DiffLine {
                            kind: LineKind::Removed,
                            old_lineno: Some(old_lineno),
                            new_lineno: None,
                            text,
                        };
                        old_lineno += 1;
                        line
                    }
                    DiffLineKind::Add => {
                        let line = DiffLine {
                            kind: LineKind::Added,
                            old_lineno: None,
                            new_lineno: Some(new_lineno),
                            text,
                        };
                        new_lineno += 1;
                        line
                    }
                }
            })
            .collect();

        self.hunks.push(Hunk {
            old_start: header.before_hunk_start,
            old_lines: header.before_hunk_len,
            new_start: header.after_hunk_start,
            new_lines: header.after_hunk_len,
            header: format!(
                "@@ -{},{} +{},{} @@",
                header.before_hunk_start,
                header.before_hunk_len,
                header.after_hunk_start,
                header.after_hunk_len
            ),
            lines,
        });

        Ok(())
    }

    fn finish(self) -> Self::Out {
        self.hunks
    }
}

fn hunks(old: &[u8], new: &[u8]) -> Result<Vec<Hunk>, GitError> {
    let input = InternedInput::new(byte_lines(old), byte_lines(new));
    let diff = gix::diff::blob::diff_with_slider_heuristics(Algorithm::Histogram, &input);

    UnifiedDiff::new(
        &diff,
        &input,
        HunkCollector::default(),
        ContextSize::symmetrical(DIFF_CONTEXT),
    )
    .consume()
    .map_err(GitError::Io)
}

fn summarise(files: &[FileDiff]) -> DiffStats {
    let mut stats = DiffStats {
        files_changed: files.len(),
        ..DiffStats::default()
    };

    for file in files {
        if let FileDiffContent::Text { hunks } = &file.content {
            for hunk in hunks {
                for line in &hunk.lines {
                    match line.kind {
                        LineKind::Added => stats.insertions += 1,
                        LineKind::Removed => stats.deletions += 1,
                        LineKind::Context => {}
                    }
                }
            }
        }
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u8, parents: &[u8], time: i64) -> WalkEntry {
        let hex = |b: u8| format!("{b:02x}").repeat(20);
        WalkEntry {
            id: ObjectId::from_hex(&hex(id)).unwrap(),
            parents: parents
                .iter()
                .map(|p| ObjectId::from_hex(&hex(*p)).unwrap())
                .collect(),
            time,
        }
    }

    fn ids(entries: &[WalkEntry]) -> Vec<String> {
        entries.iter().map(|e| e.id.short(2)).collect()
    }

    #[test]
    fn a_commit_always_precedes_its_parents() {
        // 3 → 2 → 1, but 2's timestamp lies and predates 1's, the way a
        // rebased or clock-skewed commit does.
        let entries = vec![entry(3, &[2], 300), entry(1, &[], 200), entry(2, &[1], 100)];

        let ordered = topological_order(entries);
        assert_eq!(ids(&ordered), vec!["03", "02", "01"]);
    }

    #[test]
    fn date_breaks_ties_between_independent_branches() {
        // Two tips over a shared root: the newer tip comes first.
        let entries = vec![entry(1, &[3], 100), entry(2, &[3], 500), entry(3, &[], 50)];

        let ordered = topological_order(entries);
        assert_eq!(ids(&ordered), vec!["02", "01", "03"]);
    }

    #[test]
    fn ordering_is_deterministic_when_time_and_topology_both_tie() {
        let entries = vec![entry(1, &[], 100), entry(2, &[], 100), entry(3, &[], 100)];

        let first = ids(&topological_order(entries.clone()));
        let second = ids(&topological_order(entries));
        assert_eq!(first, second);
    }

    #[test]
    fn a_merge_is_emitted_before_both_of_its_parents() {
        let entries = vec![
            entry(4, &[2, 3], 400),
            entry(2, &[1], 300),
            entry(3, &[1], 350),
            entry(1, &[], 100),
        ];

        let ordered = ids(&topological_order(entries));
        let pos = |s: &str| ordered.iter().position(|o| o == s).unwrap();

        assert!(pos("04") < pos("02"));
        assert!(pos("04") < pos("03"));
        assert!(pos("02") < pos("01"));
        assert!(pos("03") < pos("01"));
    }

    #[test]
    fn every_commit_survives_the_reordering() {
        let entries = vec![
            entry(1, &[2], 100),
            entry(2, &[3], 90),
            entry(3, &[], 80),
            entry(9, &[], 70),
        ];

        let ordered = topological_order(entries);
        assert_eq!(ordered.len(), 4, "an orphan tip must not be dropped");
    }
}
