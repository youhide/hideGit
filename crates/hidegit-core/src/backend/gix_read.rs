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
    Blob, Branch, ChangeStatus, Commit, CommitDetail, Conflict, ConflictKind, Diff, DiffLine,
    DiffStats, DiffTarget, Divergence, FileChange, FileDiff, FileDiffContent, Head, Hunk,
    LfsPointer, LineKind, ObjectId, RefKind, RefName, ReflogEntry, Refs, Remote, RepoState,
    RevSpec, Signature, StashEntry, Submodule, Tag, Worktree, WorktreeStatus,
};
use crate::ops::{Blame, BlameLine, SearchField, SearchHit, SearchQuery, SearchResults};

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

/// The counterpart to [`to_id`], for handing an id back to gitoxide.
///
/// Infallible in practice: a domain [`ObjectId`] only ever holds a length Git
/// uses, and hex round-trips exactly.
fn to_gix_id(id: ObjectId) -> gix::ObjectId {
    gix::ObjectId::from_hex(id.to_hex().as_bytes()).expect("a domain id is always valid hex")
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

        // `origin/HEAD` is symbolic: a pointer at another ref, not a branch of
        // its own. Listing it would duplicate whatever it points at under a
        // name nobody checks out.
        let unpeeled = match reference.target() {
            gix::refs::TargetRef::Object(id) => id.to_owned(),
            gix::refs::TargetRef::Symbolic(target) => {
                tracing::debug!(ref_name = %name.full, target = %target.as_bstr(), "skipping symbolic reference");
                continue;
            }
        };

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

/// Ahead/behind for every local branch that has an upstream.
///
/// Two counted walks per branch, each hiding the other side: the commits a
/// branch has that its upstream does not, and the reverse. Bounded by how far the
/// two have actually drifted, because they share history up to their merge base —
/// which is what makes this cheap for the ordinary case of a branch a few commits
/// ahead, and why it is nonetheless kept out of [`refs`], which the filesystem
/// watcher rereads on every file save.
///
/// A branch whose upstream no longer exists — the remote branch was deleted and
/// the tracking ref pruned — is skipped rather than reported as infinitely ahead.
pub(crate) fn divergence(repo: &gix::Repository) -> Result<HashMap<String, Divergence>, GitError> {
    let refs = refs(repo)?;
    let mut out = HashMap::new();

    for branch in &refs.locals {
        let Some(upstream) = &branch.upstream else {
            continue;
        };
        // Resolved through the ref list already in hand rather than with another
        // `rev_parse`: a configured upstream whose ref is gone is a normal state
        // after a prune, not an error to propagate.
        let Some(target) = refs
            .remotes
            .iter()
            .find(|r| &r.name.full == upstream)
            .map(|r| r.target)
        else {
            tracing::debug!(
                branch = %branch.name.full,
                %upstream,
                "skipping ahead/behind for a branch whose upstream ref is gone"
            );
            continue;
        };

        if target == branch.target {
            out.insert(branch.name.full.clone(), Divergence::default());
            continue;
        }

        out.insert(
            branch.name.full.clone(),
            Divergence {
                ahead: count_excluding(repo, branch.target, target)?,
                behind: count_excluding(repo, target, branch.target)?,
            },
        );
    }

    Ok(out)
}

/// Every named remote, with its URLs.
///
/// Separate from [`Refs::remotes`], which holds remote-*tracking* branches: a
/// remote that has been added but never fetched has no tracking refs at all, and
/// leaving it out of the sidebar would be saying it does not exist.
pub(crate) fn remotes(repo: &gix::Repository) -> Result<Vec<Remote>, GitError> {
    let mut out = Vec::new();

    for name in repo.remote_names() {
        let name = name.as_bstr().to_str_lossy().into_owned();

        // A remote whose URL will not parse is still a remote the user configured,
        // and hiding it would make "why is my push failing" unanswerable. It is
        // listed with whatever is there.
        let Some(Ok(remote)) = repo.try_find_remote(name.as_str()) else {
            tracing::warn!(%name, "listing a remote whose configuration would not parse");
            out.push(Remote {
                name,
                fetch_url: String::new(),
                push_url: None,
            });
            continue;
        };

        let url = |direction| {
            remote
                .url(direction)
                .map(|url| url.to_bstring().to_str_lossy().into_owned())
        };
        let fetch_url = url(gix::remote::Direction::Fetch).unwrap_or_default();
        let push = url(gix::remote::Direction::Push);

        out.push(Remote {
            name,
            // Only set when it actually differs: gitoxide reports the fetch URL
            // for both directions when no `pushurl` is configured, and showing
            // the same string twice would imply a distinction that is not there.
            push_url: push.filter(|push| *push != fetch_url),
            fetch_url,
        });
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The stash, newest entry first.
///
/// A stash is not a branch: `refs/stash` points only at the most recent entry, and
/// the older ones live in that ref's *reflog*, which is what `stash@{n}` indexes
/// into. So this reads the reflog rather than walking commits — walking would
/// follow each entry's parents into ordinary history instead.
///
/// A repository that has never stashed has no `refs/stash` at all, which is an
/// empty list rather than an error.
pub(crate) fn stashes(repo: &gix::Repository) -> Result<Vec<StashEntry>, GitError> {
    let Some(reference) = repo
        .try_find_reference("refs/stash")
        .map_err(|e| GitError::gix("looking for the stash", e))?
    else {
        return Ok(Vec::new());
    };

    let mut platform = reference.log_iter();
    // Reverse order is newest first, which is the order `stash@{0}` means and the
    // order the sidebar shows.
    let Some(entries) = platform
        .rev()
        .map_err(|e| GitError::gix("reading the stash reflog", e))?
    else {
        // The ref exists but its log does not, which a hand-written ref can
        // produce. Nothing to list.
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for (index, entry) in entries.enumerate() {
        let line = match entry {
            Ok(line) => line,
            // One unreadable entry must not blank the whole list.
            Err(e) => {
                tracing::warn!(error = %e, index, "skipping unreadable stash entry");
                continue;
            }
        };

        let (branch, message) = parse_stash_subject(line.message.to_str_lossy().as_ref());
        out.push(StashEntry {
            index,
            id: to_id(&line.new_oid),
            message,
            // An owned reflog signature carries its time directly, unlike the
            // borrowed kind a commit hands out.
            time: to_time(line.signature.time),
            branch,
        });
    }

    Ok(out)
}

/// Whether an attributes file hands anything to Git LFS.
///
/// Looks for `filter=lfs`, which is the attribute that actually routes a file
/// through the clean and smudge filters — `diff=lfs` and `merge=lfs` are
/// written alongside it by `git lfs track`, but neither is what makes a file
/// stored as a pointer, and a repository could set them without LFS being
/// involved at all.
///
/// A missing file is not an error: most repositories have no `.gitattributes`,
/// and a repository with none tracks nothing with LFS.
pub(crate) fn declares_lfs(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };

    text.lines()
        .map(str::trim)
        // A comment mentioning the attribute is not a rule that sets it.
        .filter(|line| !line.starts_with('#'))
        .any(|line| {
            line.split_whitespace()
                .skip(1)
                .any(|attribute| attribute == "filter=lfs")
        })
}

/// Every checkout of this repository, the main one first.
///
/// gitoxide lists **linked** worktrees only — the main one is deliberately not
/// counted as linked — so it is prepended here. `git worktree list` shows it
/// first too, and leaving it out would make the list disagree with the command
/// the user already knows.
///
/// A bare repository has no main worktree at all, which is not an error and not
/// an empty entry: it simply has none, and may still have linked ones.
///
/// Costs a repository open per worktree, because `HEAD` is a property of the
/// checkout rather than of the registration. That is the whole reason to read
/// them: a branch checked out in one worktree cannot be checked out in another,
/// and without `HEAD` that rule is invisible.
pub(crate) fn worktrees(repo: &gix::Repository) -> Result<Vec<Worktree>, GitError> {
    /// Whether two paths name the same directory.
    ///
    /// Not `==`. gitoxide builds a linked worktree's git directory out of
    /// `common_dir`, which it leaves unnormalised, so the proxy for the
    /// worktree hideGit is *standing in* comes back as
    /// `…/worktrees/side/../../worktrees/side` while the repository itself
    /// reports `…/worktrees/side`. Comparing the strings says they are
    /// different checkouts, and the current one then never marks itself.
    ///
    /// Canonicalising touches the filesystem, which is why it falls back rather
    /// than unwrapping: a git directory that cannot be resolved is not a reason
    /// to fail listing the worktrees.
    fn same_dir(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        }
    }

    /// The main worktree, and whether it is the one in hand.
    ///
    /// `main_repo` opens a repository — config parse, ref store and all — and
    /// this runs on every file save through the watcher, so it is skipped when
    /// the repository in hand already *is* the main one, which is the case for
    /// every repository that has never had `git worktree add` run in it. The
    /// two directories being the same is exactly what "this is the main
    /// worktree" means.
    fn main_of(repo: &gix::Repository) -> Option<(gix::Repository, bool)> {
        if same_dir(repo.git_dir(), repo.common_dir()) {
            return Some((repo.clone(), true));
        }
        match repo.main_repo() {
            Ok(main) => Some((main, false)),
            // Not fatal: the linked worktrees are still worth listing, and a
            // repository whose main one cannot be opened is exactly the case
            // where knowing what else exists helps.
            Err(e) => {
                tracing::warn!(error = %e, "the main worktree would not open");
                None
            }
        }
    }

    let mut out = Vec::new();
    let here = repo.git_dir();

    if let Some((main, is_current)) = main_of(repo)
        && let Some(workdir) = main.workdir()
    {
        let path = workdir.to_path_buf();
        out.push(Worktree {
            is_current,
            head: head(&main).ok(),
            prunable: !path.is_dir(),
            path,
            is_main: true,
            // The main worktree cannot be locked. `git worktree lock` refuses
            // it, so `None` is the only honest answer.
            locked: None,
        });
    }

    let linked = repo
        .worktrees()
        .map_err(|e| GitError::gix("listing worktrees", e))?;

    for proxy in linked {
        let git_dir = proxy.git_dir().to_path_buf();
        let locked = proxy
            .is_locked()
            .then(|| proxy.lock_reason().unwrap_or_default().to_string());
        let base = proxy.base().ok();

        // The inaccessible variant on purpose: a registration whose directory
        // was deleted is precisely the entry worth showing, because it is still
        // holding a branch nothing can check out.
        let head = proxy
            .into_repo_with_possibly_inaccessible_worktree()
            .ok()
            .and_then(|repo| self::head(&repo).ok());

        let path = base.unwrap_or_else(|| git_dir.clone());
        out.push(Worktree {
            is_current: same_dir(&git_dir, here),
            prunable: !path.is_dir(),
            path,
            head,
            is_main: false,
            locked,
        });
    }

    Ok(out)
}

/// The submodules `.gitmodules` declares, in path order.
///
/// Two commits per entry, and they are read from different places on purpose.
/// `recorded` comes from the superproject's *index*, which is what
/// `git submodule status` compares against and what a commit would write.
/// `checked_out` comes from opening the nested repository and asking its own
/// `HEAD` — no amount of reading the superproject answers that, which is the
/// reason this method costs a repository open per submodule.
///
/// Nothing here is fatal per entry. A submodule whose URL will not parse, or
/// whose checkout is a directory Git cannot open, is still one the user
/// configured; it is listed with whatever could be read, because a list that
/// silently drops the broken entry makes "why is my submodule missing"
/// unanswerable.
pub(crate) fn submodules(repo: &gix::Repository) -> Result<Vec<Submodule>, GitError> {
    // `None` is a repository with no `.gitmodules` at all — the overwhelming
    // majority — and is an empty list rather than an error.
    let Some(found) = repo
        .submodules()
        .map_err(|e| GitError::gix("reading .gitmodules", e))?
    else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for submodule in found {
        let name = submodule.name().to_str_lossy().into_owned();

        let path = match submodule.path() {
            Ok(path) => gix::path::from_bstring(path),
            Err(e) => {
                tracing::warn!(%name, error = %e, "submodule with no readable path");
                PathBuf::from(&name)
            }
        };

        let url = submodule
            .url()
            .map(|url| url.to_bstring().to_str_lossy().into_owned())
            .unwrap_or_default();

        // `.` in `.gitmodules` means "whatever the superproject is on", which is
        // a rule rather than a branch name. It is spelled out rather than
        // passed through, because `.` in a branch column reads as a typo.
        let branch = submodule
            .branch()
            .ok()
            .flatten()
            .map(|branch| match branch {
                gix::submodule::config::Branch::CurrentInSuperproject => {
                    "the superproject's branch".to_owned()
                }
                gix::submodule::config::Branch::Name(name) => name.to_str_lossy().into_owned(),
            });

        let recorded = submodule.index_id().ok().flatten().map(|id| to_id(&id));

        // Gated on there being a worktree, which is not the same question as
        // the repository existing. `git submodule deinit` empties the checkout
        // but keeps the repository in `.git/modules`, so opening it still
        // succeeds and still answers with a `HEAD` — one that describes a
        // directory the user is looking at as empty. Git itself prints `-` for
        // that state, and so does this.
        let checked_out = match submodule.state() {
            Ok(state) if state.worktree_checkout => match submodule.open() {
                Ok(Some(nested)) => nested.head_id().ok().map(|id| to_id(&id.detach())),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(%name, error = %e, "submodule checkout would not open");
                    None
                }
            },
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(%name, error = %e, "submodule state would not read");
                None
            }
        };

        out.push(Submodule {
            name,
            path,
            url,
            branch,
            recorded,
            checked_out,
        });
    }

    // Path order rather than `.gitmodules` order: the file's order is whatever
    // sequence they were added in, and the sidebar shows a tree.
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The commits a rebase onto `onto` would replay, **oldest first**.
///
/// `onto..HEAD` — everything reachable from `HEAD` that `onto` cannot reach.
/// Oldest first because that is todo order: `git rebase --interactive` lists
/// the commit it applies first at the top, and a plan editor that showed them
/// newest-first like the graph would invert every reorder the user made.
///
/// An empty result is the ordinary answer for a branch with nothing to replay,
/// not an error.
pub(crate) fn rebase_preview(repo: &gix::Repository, onto: &str) -> Result<Vec<Commit>, GitError> {
    let head = match repo.head_id() {
        Ok(id) => id.detach(),
        // An unborn HEAD has nothing to rebase.
        Err(_) => return Ok(Vec::new()),
    };

    let onto_id = repo
        .rev_parse_single(onto)
        .map_err(|_| GitError::RefNotFound(onto.to_owned()))?
        .detach();

    let walk = repo
        .rev_walk([head])
        .with_hidden([onto_id])
        .all()
        .map_err(|e| GitError::gix("listing the commits a rebase would replay", e))?;

    let mut entries = Vec::new();
    for entry in walk {
        let info =
            entry.map_err(|e| GitError::gix("listing the commits a rebase would replay", e))?;
        let commit = repo
            .find_commit(info.id)
            .map_err(|e| GitError::gix("reading a commit a rebase would replay", e))?;
        entries.push(WalkEntry {
            id: to_id(&info.id),
            parents: commit.parent_ids().map(|p| to_id(&p)).collect(),
            time: commit.time().map(|t| t.seconds).unwrap_or_default(),
        });
    }
    // The walk yields newest first.
    entries.reverse();

    let refs = refs(repo)?;
    hydrate(repo, &entries, &refs)
}

/// Walks history looking for `query`, newest first.
///
/// Every field is searched — summary, body, author name and email, and the id
/// as a prefix — because people type a fragment and expect it found, not to
/// first classify it. The field that matched travels with the hit so the list
/// can say why a commit is in it.
///
/// The walk stops at the limit, and says so. A search with no matches still
/// walks the whole history: the cap bounds the result, not the work, and
/// pretending otherwise would report "no matches" for a search that simply gave
/// up early.
pub(crate) fn search(
    repo: &gix::Repository,
    query: &SearchQuery,
) -> Result<SearchResults, GitError> {
    let needle = query.text.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(SearchResults::default());
    }

    let entries = walk(repo, &RevSpec::All)?;
    let refs = refs(repo)?;

    let mut hits = Vec::new();
    let mut truncated = false;

    // Hydrated in batches: decoding every commit in a 100,000-commit history to
    // look at its message is the expensive part, and most searches match
    // something long before the end.
    for chunk in entries.chunks(512) {
        for commit in hydrate(repo, chunk, &refs)? {
            let Some(field) = matches(&commit, &needle) else {
                continue;
            };
            if hits.len() == query.limit {
                truncated = true;
                break;
            }
            hits.push(SearchHit { commit, field });
        }
        if truncated {
            break;
        }
    }

    Ok(SearchResults { hits, truncated })
}

/// Which field of `commit` contains `needle`, already lowercased.
///
/// Ordered by what a reader would consider the reason: a summary match is the
/// answer they wanted, and a hash match is the answer they get when they pasted
/// an id.
fn matches(commit: &Commit, needle: &str) -> Option<SearchField> {
    if commit.summary.to_lowercase().contains(needle) {
        return Some(SearchField::Summary);
    }
    if commit
        .body
        .as_deref()
        .is_some_and(|body| body.to_lowercase().contains(needle))
    {
        return Some(SearchField::Body);
    }
    if commit.author.name.to_lowercase().contains(needle)
        || commit.author.email.to_lowercase().contains(needle)
    {
        return Some(SearchField::Author);
    }
    // A prefix, not a substring: an id is looked up by its start, and a
    // substring match would put unrelated commits in the list whenever somebody
    // searched for a short hex string like `abc`.
    if commit.id.to_hex().starts_with(needle) {
        return Some(SearchField::Hash);
    }
    None
}

/// Who last touched each line of `path` as of `at`.
///
/// gitoxide reports blame as *hunks* — a run of consecutive lines introduced by
/// one commit — and this flattens them to one entry per line. The view needs
/// per-line answers anyway, and flattening here means the widget never has to
/// know that a hunk is a thing.
///
/// Line numbers are 1-based and count the file **as of `at`**, not as of the
/// commit that introduced each line: they are what a reader compares against
/// the file in front of them.
pub(crate) fn blame(repo: &gix::Repository, path: &Path, at: ObjectId) -> Result<Blame, GitError> {
    // Git stores paths with forward slashes whatever the platform, and a
    // Windows path arriving with backslashes silently matches nothing — the
    // blame comes back empty rather than failing, which is the worst shape of
    // wrong.
    let spec = path
        .to_str()
        .ok_or_else(|| GitError::RefNotFound(path.display().to_string()))?
        .replace('\\', "/");

    let outcome = repo
        .blame_file(
            spec.as_str().into(),
            to_gix_id(at),
            gix::repository::blame_file::Options {
                // Rename detection is off by default, and without it a renamed
                // file's whole history collapses to the rename: every line
                // shows the commit that moved the file rather than the commit
                // that wrote it, which is a blame that answers the wrong
                // question entirely.
                rewrites: Some(gix::diff::Rewrites::default()),
                ..Default::default()
            },
        )
        .map_err(|e| GitError::gix("blaming a file", e))?;

    let mut lines = Vec::new();
    for (entry, texts) in outcome.entries_with_lines() {
        for (offset, text) in texts.iter().enumerate() {
            lines.push(BlameLine {
                commit: to_id(&entry.commit_id),
                lineno: entry.start_in_blamed_file + offset as u32 + 1,
                // The terminator is dropped: a blame is read, never written
                // back, so unlike `conflict` — which reconstructs files and has
                // to preserve them exactly — carrying it would only mean every
                // caller trimming it again.
                text: text
                    .to_str_lossy()
                    .trim_end_matches(['\r', '\n'])
                    .to_owned(),
            });
        }
    }

    // gitoxide yields hunks in the order it resolved them, which is not file
    // order. A blame view reads top to bottom.
    lines.sort_by_key(|line| line.lineno);

    Ok(Blame { lines })
}

/// How many parents `id` has.
///
/// Deliberately not [`commit_detail`], which computes a full tree diff: telling
/// a fast-forward from a merge commit needs one field of the commit header, and
/// paying for a diff to read it would make every merge slower for nothing.
pub(crate) fn parent_count(repo: &gix::Repository, id: ObjectId) -> Result<usize, GitError> {
    let commit = repo
        .find_commit(to_gix_id(id))
        .map_err(|e| GitError::gix("reading a commit to count its parents", e))?;
    Ok(commit.parent_ids().count())
}

/// Reads up to `limit` reflog entries for `ref_name`, most recent first.
///
/// A reference with no log is not an error — a branch created by a tool that
/// does not write one, or a repository with `core.logAllRefUpdates` off, simply
/// has nothing to show — so it comes back empty rather than failing.
pub(crate) fn reflog(
    repo: &gix::Repository,
    ref_name: &str,
    limit: usize,
) -> Result<Vec<ReflogEntry>, GitError> {
    let Some(reference) = repo
        .try_find_reference(ref_name)
        .map_err(|e| GitError::gix("looking for a reference to read its reflog", e))?
    else {
        return Err(GitError::RefNotFound(ref_name.to_owned()));
    };

    let mut platform = reference.log_iter();
    // Newest first, which is the order `HEAD@{0}` means and the order the view
    // shows: the entry you want after a mistake is almost always the last one.
    let Some(entries) = platform
        .rev()
        .map_err(|e| GitError::gix("reading a reflog", e))?
    else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for (index, entry) in entries.enumerate() {
        if out.len() >= limit {
            break;
        }
        let line = match entry {
            Ok(line) => line,
            // One unreadable entry must not blank the whole log — the entries
            // around it are still what makes a mistake recoverable.
            Err(e) => {
                tracing::warn!(error = %e, index, ref_name, "skipping unreadable reflog entry");
                continue;
            }
        };

        out.push(ReflogEntry {
            index,
            old_id: to_id(&line.previous_oid),
            new_id: to_id(&line.new_oid),
            who: Signature {
                name: line.signature.name.to_str_lossy().trim().to_owned(),
                email: line.signature.email.to_str_lossy().trim().to_owned(),
                // An owned reflog signature carries its time directly, unlike
                // the borrowed kind a commit hands out.
                time: to_time(line.signature.time),
            },
            message: line.message.to_str_lossy().trim().to_owned(),
        });
    }

    Ok(out)
}

/// Pulls the branch and the message out of a stash reflog subject.
///
/// Git writes `WIP on <branch>: <sha> <summary>` when it invented the message, and
/// `On <branch>: <message>` when the user supplied one. Anything else — a reflog
/// written by another tool, or a wording change — is kept whole rather than
/// mangled, because a message shown verbatim is never wrong.
fn parse_stash_subject(subject: &str) -> (Option<String>, String) {
    for prefix in ["WIP on ", "On "] {
        if let Some(rest) = subject.strip_prefix(prefix)
            && let Some((branch, message)) = rest.split_once(": ")
        {
            return (Some(branch.to_owned()), message.trim().to_owned());
        }
    }
    (None, subject.trim().to_owned())
}

/// Is `ancestor` reachable from `descendant`?
///
/// Used to tell a fast-forward from an integration: after a pull, a new `HEAD`
/// that the old one is an ancestor of was fast-forwarded, and one it is not was
/// merged or rebased.
pub(crate) fn is_ancestor(
    repo: &gix::Repository,
    ancestor: ObjectId,
    descendant: ObjectId,
) -> Result<bool, GitError> {
    // Everything reachable from `ancestor` is painted unwanted by hiding
    // `descendant` exactly when `descendant` already contains all of it.
    Ok(count_excluding(repo, ancestor, descendant)? == 0)
}

/// How many commits `from` reaches that `hidden` does not.
fn count_excluding(
    repo: &gix::Repository,
    from: ObjectId,
    hidden: ObjectId,
) -> Result<usize, GitError> {
    let walk = repo
        .rev_walk([to_gix_id(from)])
        .with_hidden([to_gix_id(hidden)])
        .all()
        .map_err(|e| GitError::gix("counting ahead/behind", e))?;

    let mut count = 0;
    for entry in walk {
        entry.map_err(|e| GitError::gix("counting ahead/behind", e))?;
        count += 1;
    }
    Ok(count)
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
        DiffTarget::Staged => return worktree_diff(repo, Half::Staged),
        DiffTarget::Unstaged => return worktree_diff(repo, Half::Unstaged),
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
        if is_directory(&change) {
            continue;
        }
        files.push(to_file_diff(repo, change)?);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let stats = summarise(&files);
    Ok(Diff { files, stats })
}

fn path_from(location: &gix::bstr::BStr) -> PathBuf {
    PathBuf::from(location.to_str_lossy().into_owned())
}

/// What kind of thing a path holds, coarsely enough to answer "did the type
/// change?".
///
/// Git reports `T` for a regular file that became a symlink or a submodule, but
/// plain `M` for one that merely gained the executable bit — a permission
/// change is a modification, not a change of type. Comparing gitoxide's entry
/// kinds directly would conflate the two, because it separates `Blob` from
/// `BlobExecutable`.
#[derive(PartialEq, Eq)]
enum PathKind {
    File,
    Link,
    Submodule,
    Directory,
}

fn path_kind(mode: gix::object::tree::EntryMode) -> PathKind {
    use gix::object::tree::EntryKind as K;

    match mode.kind() {
        K::Blob | K::BlobExecutable => PathKind::File,
        K::Link => PathKind::Link,
        K::Commit => PathKind::Submodule,
        K::Tree => PathKind::Directory,
    }
}

/// Does this change describe a directory rather than a file?
///
/// A tree diff reports the directories along a changed file's path as changes
/// in their own right: a commit touching `a/b/c.rs` also reports `a` and
/// `a/b`. Git reports only the file, and so does this — a directory has no
/// text to show, so it would otherwise render as a binary file and inflate the
/// changed-file count.
fn is_directory(change: &gix::diff::tree_with_rewrites::Change) -> bool {
    use gix::diff::tree_with_rewrites::Change as C;

    match change {
        C::Addition { entry_mode, .. } | C::Deletion { entry_mode, .. } => entry_mode.is_tree(),
        // A directory replaced by a file of the same name is a real change and
        // stays; only tree-to-tree is the path-component noise above.
        C::Modification {
            previous_entry_mode,
            entry_mode,
            ..
        } => previous_entry_mode.is_tree() && entry_mode.is_tree(),
        // Rewrite tracking pairs blobs; it never reports a tree.
        C::Rewrite { .. } => false,
    }
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
            let status = if path_kind(previous_entry_mode) == path_kind(entry_mode) {
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

    assemble(
        path,
        status,
        load_side(repo, old_id)?,
        load_side(repo, new_id)?,
    )
}

/// Turns two loaded sides into a file's diff, or into the placeholder that
/// stands in for one.
fn assemble(
    path: PathBuf,
    status: ChangeStatus,
    old: Side,
    new: Side,
) -> Result<FileDiff, GitError> {
    let content = match (&old, &new) {
        (Side::TooLarge(bytes), _) | (_, Side::TooLarge(bytes)) => {
            FileDiffContent::TooLarge { bytes: *bytes }
        }
        (Side::Binary, _) | (_, Side::Binary) => FileDiffContent::Binary,
        // Checked before diffing rather than after. What Git stores for an
        // LFS-tracked file *is* the pointer, so the alternative is three lines
        // of `oid sha256:…` presented as though they were the change — which
        // is showing the plumbing and calling it the content.
        (Side::Text(old), Side::Text(new))
            if LfsPointer::parse(old).is_some() || LfsPointer::parse(new).is_some() =>
        {
            FileDiffContent::Lfs {
                old: LfsPointer::parse(old),
                new: LfsPointer::parse(new),
            }
        }
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

/// Which of the two working-directory diffs is being asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Half {
    /// `HEAD` against the index — what a commit would contain.
    Staged,
    /// The index against the working tree — what a commit would leave behind.
    Unstaged,
}

/// Diffs one half of the working directory.
///
/// The set of changed paths comes from [`status`] rather than from a second
/// traversal, so the staging view's file list and the diff it shows for a file
/// can never disagree about what changed. Content for each side is then loaded
/// per path: from a tree, from the index, or from disk.
fn worktree_diff(repo: &gix::Repository, half: Half) -> Result<Diff, GitError> {
    let status = status(repo)?;
    let changes = match half {
        Half::Staged => &status.staged,
        Half::Unstaged => &status.unstaged,
    };

    let head_tree = match repo.head_tree_id_or_empty() {
        Ok(id) => Some(
            repo.find_tree(id)
                .map_err(|e| GitError::gix("reading the HEAD tree", e))?,
        ),
        // An unborn HEAD has no tree; everything staged reads as an addition.
        Err(_) => None,
    };
    let index = repo
        .index_or_empty()
        .map_err(|e| GitError::gix("reading the index", e))?;

    let mut files = Vec::with_capacity(changes.len());
    for change in changes {
        // A rename's old content lives under its old path.
        let old_path = match &change.status {
            ChangeStatus::Renamed { from } | ChangeStatus::Copied { from } => from.as_path(),
            _ => change.path.as_path(),
        };

        let (old, new) = match half {
            Half::Staged => (
                tree_side(repo, head_tree.as_ref(), old_path)?,
                index_side(repo, &index, &change.path)?,
            ),
            Half::Unstaged => (
                index_side(repo, &index, old_path)?,
                disk_side(repo, &change.path)?,
            ),
        };

        files.push(assemble(
            change.path.clone(),
            change.status.clone(),
            old,
            new,
        )?);
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    let stats = summarise(&files);
    Ok(Diff { files, stats })
}

/// The content a path has in a tree, or nothing if it is absent from it.
fn tree_side(
    repo: &gix::Repository,
    tree: Option<&gix::Tree<'_>>,
    path: &Path,
) -> Result<Side, GitError> {
    let Some(tree) = tree else {
        return Ok(Side::Text(Vec::new()));
    };
    let entry = tree
        .clone()
        .peel_to_entry_by_path(path)
        .map_err(|e| GitError::gix("looking a path up in a tree", e))?;

    match entry {
        Some(entry) => load_side(repo, Some(entry.object_id())),
        None => Ok(Side::Text(Vec::new())),
    }
}

/// The content a path has in the index, or nothing if it is absent from it.
fn index_side(
    repo: &gix::Repository,
    index: &gix::index::File,
    path: &Path,
) -> Result<Side, GitError> {
    let rela = gix::path::to_unix_separators_on_windows(gix::path::into_bstr(path));
    match index.entry_by_path(rela.as_ref()) {
        Some(entry) => load_side(repo, Some(entry.id)),
        None => Ok(Side::Text(Vec::new())),
    }
}

/// The content a path has on disk, or nothing if it is no longer there.
fn disk_side(repo: &gix::Repository, path: &Path) -> Result<Side, GitError> {
    let absolute = repo
        .workdir()
        .ok_or_else(|| GitError::NotARepository(repo.git_dir().to_path_buf()))?
        .join(path);

    match std::fs::read(&absolute) {
        Ok(data) => Ok(classify(data)),
        // A deleted file has no content, which is exactly an empty new side.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Side::Text(Vec::new())),
        Err(e) => Err(GitError::Io(e)),
    }
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

    Ok(classify(object.into_blob().data.clone()))
}

/// Decides whether some bytes can be shown as text, by size and by content.
fn classify(data: Vec<u8>) -> Side {
    if data.len() as u64 > MAX_DIFF_BYTES {
        return Side::TooLarge(data.len() as u64);
    }
    // The same heuristic Git uses: a NUL byte in the first 8000 bytes.
    if data[..data.len().min(8000)].contains(&0) {
        return Side::Binary;
    }

    Side::Text(data)
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
            .map(|(kind, raw)| {
                // A line that does not end in a newline is the last line of its
                // side of a file that has none. That has to survive into the
                // model: a patch built without the `\ No newline at end of
                // file` marker silently appends one.
                let no_newline = !raw.ends_with(b"\n");
                let text = String::from_utf8_lossy(raw)
                    .trim_end_matches('\n')
                    .to_owned();
                match kind {
                    DiffLineKind::Context => {
                        let line = DiffLine {
                            kind: LineKind::Context,
                            old_lineno: Some(old_lineno),
                            new_lineno: Some(new_lineno),
                            text,
                            no_newline,
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
                            no_newline,
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
                            no_newline,
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

/// The working directory, as three lists plus whatever is conflicted.
///
/// One traversal produces both halves: `HEAD` against the index — what a commit
/// would contain — and the index against the working tree, alongside a
/// directory walk for untracked files. gitoxide runs them in parallel and so
/// emits them interleaved, which is why each list is sorted at the end: the UI
/// needs a stable order, and "whichever thread finished first" is not one.
///
/// A file modified in the index *and* changed again on disk appears in both
/// `staged` and `unstaged`. That is not double-counting — those are two
/// different diffs, and the staging view offers a different action for each.
pub(crate) fn status(repo: &gix::Repository) -> Result<WorktreeStatus, GitError> {
    let platform = repo
        .status(gix::progress::Discard)
        .map_err(|e| GitError::gix("reading the working tree status", e))?
        // Whole untracked directories collapse to the directory, the way
        // `git status` reports them, rather than listing every file beneath an
        // unopened `target/`.
        .untracked_files(gix::status::UntrackedFiles::Collapsed)
        // Rename detection on both halves. Without it a rename reads as a
        // delete plus an unrelated addition, and the diff for it is the whole
        // file twice over.
        .index_worktree_rewrites(Some(gix::diff::Rewrites::default()))
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Given(
            gix::diff::Rewrites::default(),
        ));

    let iter = platform
        .into_iter(std::iter::empty())
        .map_err(|e| GitError::gix("walking the working tree", e))?;

    let mut status = WorktreeStatus {
        state: repo_state(repo),
        ..WorktreeStatus::default()
    };

    for item in iter {
        let item = item.map_err(|e| GitError::gix("reading a status entry", e))?;

        match item {
            gix::status::Item::TreeIndex(change) => {
                status.staged.push(staged_change(&change));
            }
            gix::status::Item::IndexWorktree(item) => {
                worktree_item(item, &mut status);
            }
        }
    }

    status.staged.sort_by(|a, b| a.path.cmp(&b.path));
    status.unstaged.sort_by(|a, b| a.path.cmp(&b.path));
    status.untracked.sort();
    status.conflicted.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(status)
}

/// A difference between `HEAD` and the index — what a commit would record.
fn staged_change(change: &gix::diff::index::Change) -> FileChange {
    use gix::diff::index::Change as C;

    let (path, status) = match change {
        C::Addition { location, .. } => (location.as_ref(), ChangeStatus::Added),
        C::Deletion { location, .. } => (location.as_ref(), ChangeStatus::Deleted),
        C::Modification {
            location,
            previous_entry_mode,
            entry_mode,
            ..
        } => {
            let same_kind = match (
                previous_entry_mode.to_tree_entry_mode(),
                entry_mode.to_tree_entry_mode(),
            ) {
                (Some(before), Some(after)) => path_kind(before) == path_kind(after),
                // An index entry with no tree equivalent is a sparse
                // placeholder; there is no type to have changed.
                _ => true,
            };
            let status = if same_kind {
                ChangeStatus::Modified
            } else {
                ChangeStatus::TypeChange
            };
            (location.as_ref(), status)
        }
        C::Rewrite {
            source_location,
            location,
            copy,
            ..
        } => {
            let from = path_from(source_location.as_ref());
            let status = if *copy {
                ChangeStatus::Copied { from }
            } else {
                ChangeStatus::Renamed { from }
            };
            (location.as_ref(), status)
        }
    };

    FileChange {
        path: path_from(path),
        status,
    }
}

/// A difference between the index and the working tree, or a file the index has
/// never heard of.
fn worktree_item(item: gix::status::index_worktree::Item, status: &mut WorktreeStatus) {
    use gix::status::index_worktree::Item as I;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    match item {
        I::Modification {
            rela_path,
            status: entry_status,
            ..
        } => match entry_status {
            EntryStatus::Conflict { summary, .. } => status.conflicted.push(Conflict {
                path: path_from(rela_path.as_ref()),
                kind: conflict_kind(summary),
            }),
            EntryStatus::Change(Change::Removed) => status.unstaged.push(FileChange {
                path: path_from(rela_path.as_ref()),
                status: ChangeStatus::Deleted,
            }),
            EntryStatus::Change(Change::Type { .. }) => status.unstaged.push(FileChange {
                path: path_from(rela_path.as_ref()),
                status: ChangeStatus::TypeChange,
            }),
            EntryStatus::Change(Change::Modification { .. })
            | EntryStatus::Change(Change::SubmoduleModification(_)) => {
                status.unstaged.push(FileChange {
                    path: path_from(rela_path.as_ref()),
                    status: ChangeStatus::Modified,
                })
            }
            // A file staged with `--intent-to-add` has an index entry that
            // promises content the object database does not have yet. Nothing
            // has changed relative to that promise, so there is nothing to
            // report. `NeedsUpdate` is a cache hint, not a change at all.
            EntryStatus::IntentToAdd | EntryStatus::NeedsUpdate(_) => {}
        },
        I::DirectoryContents { entry, .. } => {
            if entry.status == gix::dir::entry::Status::Untracked {
                status.untracked.push(path_from(entry.rela_path.as_ref()));
            }
        }
        I::Rewrite {
            source,
            dirwalk_entry,
            copy,
            ..
        } => {
            let from = path_from(source.rela_path());
            status.unstaged.push(FileChange {
                path: path_from(dirwalk_entry.rela_path.as_ref()),
                status: if copy {
                    ChangeStatus::Copied { from }
                } else {
                    ChangeStatus::Renamed { from }
                },
            });
        }
    }
}

fn conflict_kind(summary: gix::status::plumbing::index_as_worktree::Conflict) -> ConflictKind {
    use gix::status::plumbing::index_as_worktree::Conflict as C;

    match summary {
        C::BothModified => ConflictKind::BothModified,
        C::BothAdded => ConflictKind::BothAdded,
        C::BothDeleted => ConflictKind::BothDeleted,
        C::DeletedByUs => ConflictKind::DeletedByUs,
        C::DeletedByThem => ConflictKind::DeletedByThem,
        C::AddedByUs => ConflictKind::AddedByUs,
        C::AddedByThem => ConflictKind::AddedByThem,
    }
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
