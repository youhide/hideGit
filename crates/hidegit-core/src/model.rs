//! The domain model.
//!
//! These are plain data types with no `gix` types in their public signatures.
//! Translation happens at the backend boundary, which is what allows a method
//! to move from the CLI to gix — or back — without the rest of the application
//! noticing. See `docs/ARCHITECTURE.md#domain-model`.

use std::fmt;
use std::path::PathBuf;

use time::OffsetDateTime;

/// A Git object hash.
///
/// Stores the raw bytes rather than a hex string: comparison and hashing are
/// what the graph layout does constantly, and doing them on 20 bytes is
/// cheaper than on 40 characters. Both SHA-1 and SHA-256 repositories are
/// representable, so a future object-format migration is not a breaking change
/// to every type that carries an id.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId {
    bytes: [u8; 32],
    len: u8,
}

impl ObjectId {
    /// Builds an id from raw hash bytes.
    ///
    /// Returns `None` unless the length is one Git actually uses (20 bytes for
    /// SHA-1, 32 for SHA-256).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 20 && bytes.len() != 32 {
            return None;
        }
        let mut buf = [0u8; 32];
        buf[..bytes.len()].copy_from_slice(bytes);
        Some(Self {
            bytes: buf,
            len: bytes.len() as u8,
        })
    }

    /// Parses a full hex hash. Abbreviated hashes are not accepted — resolving
    /// those needs the object database, so it belongs on the backend.
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 40 && hex.len() != 64 {
            return None;
        }
        let mut buf = [0u8; 32];
        for (i, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            let s = std::str::from_utf8(pair).ok()?;
            buf[i] = u8::from_str_radix(s, 16).ok()?;
        }
        Some(Self {
            bytes: buf,
            len: (hex.len() / 2) as u8,
        })
    }

    /// The raw hash bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// The full hash as lowercase hex.
    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(self.len as usize * 2);
        for byte in self.as_bytes() {
            use fmt::Write as _;
            let _ = write!(s, "{byte:02x}");
        }
        s
    }

    /// The first `n` hex characters, for display. Not guaranteed unique — that
    /// is a repository-wide question and this type does not have the database.
    pub fn short(self, n: usize) -> String {
        let mut hex = self.to_hex();
        hex.truncate(n.min(self.len as usize * 2));
        hex
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.short(8))
    }
}

/// An author or committer identity with the timestamp it was recorded at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub time: OffsetDateTime,
}

/// What kind of thing a reference points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    LocalBranch,
    RemoteBranch,
    Tag,
    /// `HEAD` itself, and other pseudo-refs.
    Special,
}

/// A reference, carrying both the full name and the name to show a user.
///
/// The full name is kept because it is what disambiguates: a local branch and
/// a tag may both be called `release`, and only `refs/heads/release` versus
/// `refs/tags/release` says which is which.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RefName {
    pub kind: RefKind,
    /// The fully qualified name, e.g. `refs/remotes/origin/main`.
    pub full: String,
    /// The name to display, e.g. `origin/main`.
    pub short: String,
}

/// A branch and, if it has one, the upstream it tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: RefName,
    pub target: ObjectId,
    /// Full name of the upstream ref, when one is configured.
    pub upstream: Option<String>,
}

/// How far a branch has drifted from the upstream it tracks.
///
/// Deliberately not a field on [`Branch`]: computing it costs a commit walk per
/// branch, and [`crate::GitBackend::refs`] runs on every file save through the
/// filesystem watcher. Ahead/behind only changes when a ref moves, so it is read
/// separately and cached.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Divergence {
    /// Commits on the branch that the upstream does not have.
    pub ahead: usize,
    /// Commits on the upstream that the branch does not have.
    pub behind: usize,
}

impl Divergence {
    /// True when the branch and its upstream point at the same commit.
    pub fn is_in_sync(self) -> bool {
        self.ahead == 0 && self.behind == 0
    }

    /// True when both sides have commits the other does not — a push will be
    /// rejected and a pull will have to merge or rebase.
    pub fn has_diverged(self) -> bool {
        self.ahead > 0 && self.behind > 0
    }
}

/// A named remote, as `git remote` lists it.
///
/// Distinct from the remote-tracking branches in [`Refs::remotes`]: a remote
/// that has been added but never fetched has no tracking refs at all, and
/// leaving it out of the sidebar would be saying it does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub fetch_url: String,
    /// Set only when `remote.<name>.pushurl` differs from the fetch URL.
    pub push_url: Option<String>,
}

impl Remote {
    /// The remote in one line, for a heading.
    ///
    /// Names both URLs when they differ, because "why did my push go somewhere
    /// else" is exactly the question a separate `pushurl` answers.
    pub fn url_summary(&self) -> String {
        match &self.push_url {
            Some(push) => format!("{} → {} (push {push})", self.name, self.fetch_url),
            None => format!("{} → {}", self.name, self.fetch_url),
        }
    }
}

/// A submodule, as the superproject records it.
///
/// A submodule is two facts that can disagree: the commit the superproject's
/// index points at, and the commit the nested checkout is actually on. Keeping
/// both — rather than one "is it up to date" boolean — is what lets the UI say
/// *which* commit is wrong, which is the whole difficulty of submodules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submodule {
    /// The `.gitmodules` section name. Usually the path, but not necessarily:
    /// it survives a `git mv`, which is why Git keys configuration on it.
    pub name: String,
    /// Where it sits in the superproject's worktree, relative to the root.
    pub path: PathBuf,
    /// The URL `.gitmodules` records. Empty rather than absent when the entry
    /// has none — a submodule with no URL is broken, not missing.
    pub url: String,
    /// The branch `.gitmodules` names, for the submodules that track one.
    pub branch: Option<String>,
    /// The commit the superproject's index points at. `None` for an entry in
    /// `.gitmodules` with nothing staged at its path, which is what a
    /// half-removed submodule looks like.
    pub recorded: Option<ObjectId>,
    /// Where the nested checkout's own `HEAD` is. `None` when there is no
    /// checkout to ask.
    pub checked_out: Option<ObjectId>,
}

impl Submodule {
    /// What `git submodule status` would print in its first column.
    ///
    /// The same three states, deliberately: someone who has read `-`, `+` or a
    /// space in a terminal should not have to learn a second vocabulary here.
    pub fn state(&self) -> SubmoduleState {
        match (self.recorded, self.checked_out) {
            (_, None) => SubmoduleState::Uninitialised,
            (Some(recorded), Some(checked_out)) if recorded != checked_out => SubmoduleState::Moved,
            _ => SubmoduleState::Current,
        }
    }
}

/// Whether a submodule's checkout agrees with the superproject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmoduleState {
    /// No checkout on disk — cloning the superproject does not clone these, so
    /// this is the state every submodule starts in. `git submodule status`
    /// prints `-`.
    Uninitialised,
    /// Checked out at the commit the superproject records. `git submodule
    /// status` prints a space.
    Current,
    /// Checked out at some other commit, so committing the superproject now
    /// would move the recorded pointer. `git submodule status` prints `+`.
    Moved,
}

/// One entry on the stash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    /// Position from the top, which is the `n` in the `stash@{n}` that every
    /// stash subcommand takes.
    pub index: usize,
    /// The commit the entry is stored as. A stash is a commit, so its contents
    /// are readable with an ordinary [`DiffTarget::Commit`].
    pub id: ObjectId,
    /// What to show in a list: the user's own message when they gave one, and
    /// otherwise the `WIP on …` line Git wrote.
    pub message: String,
    pub time: OffsetDateTime,
    /// The branch the stash was made on, when the reflog subject names one.
    pub branch: Option<String>,
}

/// One entry in a reference's reflog.
///
/// The reflog is how a rewritten history stays recoverable: every operation in
/// M5 that moves a branch leaves an entry naming where it was, and that old
/// commit stays reachable until it is garbage-collected. It is the difference
/// between a hard reset being frightening and being reversible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    /// Position from the top, which is the `n` in `HEAD@{n}`.
    pub index: usize,
    /// What the reference pointed at *before* this entry's operation.
    pub old_id: ObjectId,
    /// What it pointed at after.
    pub new_id: ObjectId,
    /// Who made the change, and when.
    pub who: Signature,
    /// Git's own description — `commit: …`, `rebase (finish): …`, `reset: …`.
    ///
    /// Shown verbatim rather than re-worded: the vocabulary is Git's, and a
    /// user who searches for what it says will find Git's documentation.
    pub message: String,
}

/// A tag. Annotated tags carry their own object; lightweight ones do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: RefName,
    /// The commit the tag ultimately resolves to.
    pub target: ObjectId,
    pub annotated: bool,
}

/// Every reference in a repository, grouped the way the sidebar shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refs {
    pub locals: Vec<Branch>,
    pub remotes: Vec<Branch>,
    pub tags: Vec<Tag>,
}

impl Refs {
    /// Every ref pointing at `id`, for the badges drawn on a graph row.
    pub fn pointing_at(&self, id: ObjectId) -> Vec<RefName> {
        let locals = self.locals.iter().chain(&self.remotes);
        locals
            .filter(|b| b.target == id)
            .map(|b| b.name.clone())
            .chain(
                self.tags
                    .iter()
                    .filter(|t| t.target == id)
                    .map(|t| t.name.clone()),
            )
            .collect()
    }
}

/// Where `HEAD` currently points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// A branch that exists but has no commits yet — a freshly initialised
    /// repository. Not an error, and the UI has to render it.
    Unborn { name: RefName },
    /// Attached to a branch.
    Branch { name: RefName, target: ObjectId },
    /// Detached at a specific commit.
    Detached { target: ObjectId },
}

impl Head {
    /// The commit `HEAD` resolves to, or `None` in an unborn repository.
    pub fn target(&self) -> Option<ObjectId> {
        match self {
            Head::Unborn { .. } => None,
            Head::Branch { target, .. } | Head::Detached { target } => Some(*target),
        }
    }
}

/// A commit as the graph and the sidebar need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: ObjectId,
    pub parents: Vec<ObjectId>,
    /// The first line of the message.
    pub summary: String,
    /// Everything after the first blank line, when there is any.
    pub body: Option<String>,
    pub author: Signature,
    pub committer: Signature,
    /// The commit time, duplicated out of `committer` because ordering and
    /// display both reach for it constantly.
    pub time: OffsetDateTime,
    /// Branches and tags pointing at this commit.
    pub refs: Vec<RefName>,
}

impl Commit {
    /// A commit with more than one parent.
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// A commit plus the file list that only the detail pane needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetail {
    pub commit: Commit,
    pub changes: Vec<FileChange>,
    pub stats: DiffStats,
}

/// What happened to one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    /// `from` is the path the file had before.
    Renamed {
        from: PathBuf,
    },
    Copied {
        from: PathBuf,
    },
    /// A regular file became a symlink, or gained/lost the executable bit.
    TypeChange,
}

impl ChangeStatus {
    /// The single-letter code Git uses, for a compact file list.
    pub fn code(&self) -> char {
        match self {
            ChangeStatus::Added => 'A',
            ChangeStatus::Modified => 'M',
            ChangeStatus::Deleted => 'D',
            ChangeStatus::Renamed { .. } => 'R',
            ChangeStatus::Copied { .. } => 'C',
            ChangeStatus::TypeChange => 'T',
        }
    }
}

/// One changed path in a status or commit listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub status: ChangeStatus,
}

/// A path with unresolved conflict markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub path: PathBuf,
    pub kind: ConflictKind,
}

/// Why a path is conflicted. The UI wording differs for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides changed the contents.
    BothModified,
    BothAdded,
    /// Deleted on one side, modified on the other.
    DeletedByUs,
    DeletedByThem,
    AddedByUs,
    AddedByThem,
    /// Both sides deleted the path, but left different states behind it.
    BothDeleted,
}

/// What operation, if any, the repository is in the middle of.
///
/// Not cosmetic: a repository mid-rebase must not offer "commit" as though
/// nothing is happening. The UI reads this to decide which actions are legal
/// at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RepoState {
    #[default]
    Clean,
    Merging,
    Rebasing,
    CherryPicking,
    Reverting,
    Bisecting,
}

impl RepoState {
    /// True when an operation is in progress and needs to be continued or
    /// aborted before anything else is allowed.
    pub fn is_in_progress(self) -> bool {
        !matches!(self, RepoState::Clean)
    }
}

/// The working directory as the staging view shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<PathBuf>,
    pub conflicted: Vec<Conflict>,
    pub state: RepoState,
}

impl WorktreeStatus {
    /// Total number of entries, for the badge on the sidebar heading.
    pub fn change_count(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.untracked.len() + self.conflicted.len()
    }

    pub fn is_clean(&self) -> bool {
        self.change_count() == 0
    }
}

/// What a diff is being computed between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTarget {
    /// A commit against its first parent. A root commit diffs against nothing,
    /// so every file reads as added.
    Commit(ObjectId),
    /// An arbitrary pair of commits.
    Range { from: ObjectId, to: ObjectId },
    /// Index against `HEAD` — what a commit would contain.
    Staged,
    /// Working tree against the index — what has not been staged.
    Unstaged,
}

/// A whole diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub files: Vec<FileDiff>,
    pub stats: DiffStats,
}

/// One file's changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    pub status: ChangeStatus,
    pub content: FileDiffContent,
}

/// Why a file has no hunks, when it has none.
///
/// Binary and oversized files get a placeholder rather than an attempt to
/// render them: the alternative is a hang, and a hang is worse than an honest
/// "not shown".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDiffContent {
    Text { hunks: Vec<Hunk> },
    Binary,
    TooLarge { bytes: u64 },
}

/// A contiguous run of changed lines with its surrounding context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// The `@@ … @@` line, kept verbatim so the view does not reconstruct it.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// One line inside a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line number on the left-hand side, absent on added lines.
    pub old_lineno: Option<u32>,
    /// Line number on the right-hand side, absent on removed lines.
    pub new_lineno: Option<u32>,
    /// The line's content, without its leading `+`/`-`/space marker and
    /// without a trailing newline.
    pub text: String,
    /// This line ends the file and the file does not end with a newline.
    ///
    /// Kept because a patch has to say so — the `\ No newline at end of file`
    /// marker. Dropping it means a patch built from this diff silently appends
    /// a newline to the file it is applied to.
    pub no_newline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffStats {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// Raw object contents, for the diff viewer and future blame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub id: ObjectId,
    pub bytes: Vec<u8>,
}

impl Blob {
    /// Whether Git would treat this content as binary.
    ///
    /// The same heuristic Git uses: a NUL byte in the first 8000 bytes.
    pub fn is_binary(&self) -> bool {
        let window = &self.bytes[..self.bytes.len().min(8000)];
        window.contains(&0)
    }
}

/// Which commits a history walk should visit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RevSpec {
    /// From `HEAD` only.
    Head,
    /// From every local branch, remote branch and tag, so branches that are
    /// not checked out still appear in the graph.
    All,
    /// From one named ref.
    Ref(String),
    /// From one commit.
    Commit(ObjectId),
}

/// Which slice of history to return.
///
/// History is paged rather than loaded whole: the graph only lays out the
/// rows around the viewport, and a 100,000-commit repository must not block
/// on a full traversal to draw its first screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogPage {
    /// Commits to skip from the start of the walk.
    pub skip: usize,
    /// Maximum number of commits to return.
    pub limit: usize,
}

impl LogPage {
    pub fn first(limit: usize) -> Self {
        Self { skip: 0, limit }
    }

    /// The page immediately after this one.
    pub fn next(self) -> Self {
        Self {
            skip: self.skip + self.limit,
            limit: self.limit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_id_round_trips_through_hex() {
        let hex = "a3f9c2100000000000000000000000000000beef";
        let id = ObjectId::from_hex(hex).expect("valid sha-1 hex");
        assert_eq!(id.to_hex(), hex);
        assert_eq!(id.short(7), "a3f9c21");
        assert_eq!(id.as_bytes().len(), 20);
    }

    #[test]
    fn object_id_accepts_sha256_lengths() {
        let hex = "a".repeat(64);
        let id = ObjectId::from_hex(&hex).expect("valid sha-256 hex");
        assert_eq!(id.as_bytes().len(), 32);
        assert_eq!(id.to_hex(), hex);
    }

    #[test]
    fn object_id_rejects_abbreviated_and_malformed_input() {
        assert!(ObjectId::from_hex("a3f9c21").is_none());
        assert!(ObjectId::from_hex(&"z".repeat(40)).is_none());
        assert!(ObjectId::from_bytes(&[0u8; 16]).is_none());
    }

    #[test]
    fn sha1_and_sha256_ids_of_the_same_prefix_are_distinct() {
        let short = ObjectId::from_hex(&"0".repeat(40)).unwrap();
        let long = ObjectId::from_hex(&"0".repeat(64)).unwrap();
        assert_ne!(short, long, "length must participate in equality");
    }

    #[test]
    fn blob_binary_detection_matches_gits_heuristic() {
        let text = Blob {
            id: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
            bytes: b"fn main() {}\n".to_vec(),
        };
        assert!(!text.is_binary());

        let binary = Blob {
            id: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
            bytes: vec![0x7f, 0x45, 0x4c, 0x46, 0x00, 0x01],
        };
        assert!(binary.is_binary());
    }

    #[test]
    fn a_nul_past_the_first_8000_bytes_does_not_count() {
        let mut bytes = vec![b'a'; 9000];
        bytes[8500] = 0;
        let blob = Blob {
            id: ObjectId::from_hex(&"0".repeat(40)).unwrap(),
            bytes,
        };
        assert!(!blob.is_binary());
    }

    #[test]
    fn divergence_distinguishes_being_behind_from_having_diverged() {
        let synced = Divergence::default();
        assert!(synced.is_in_sync());
        assert!(!synced.has_diverged());

        let behind = Divergence {
            ahead: 0,
            behind: 3,
        };
        assert!(!behind.is_in_sync());
        assert!(
            !behind.has_diverged(),
            "being behind fast-forwards; it is not a divergence"
        );

        let diverged = Divergence {
            ahead: 2,
            behind: 3,
        };
        assert!(diverged.has_diverged());
    }

    #[test]
    fn repo_state_gates_actions_only_when_an_operation_is_running() {
        assert!(!RepoState::Clean.is_in_progress());
        assert!(RepoState::Rebasing.is_in_progress());
        assert!(RepoState::Merging.is_in_progress());
    }

    #[test]
    fn a_submodule_state_is_the_column_git_submodule_status_prints() {
        let one = ObjectId::from_hex(&"a".repeat(40)).expect("valid hex");
        let other = ObjectId::from_hex(&"b".repeat(40)).expect("valid hex");

        let submodule = |recorded, checked_out| Submodule {
            name: "vendor/lib".to_owned(),
            path: PathBuf::from("vendor/lib"),
            url: "https://example.invalid/lib.git".to_owned(),
            branch: None,
            recorded,
            checked_out,
        };

        assert_eq!(
            submodule(Some(one), Some(one)).state(),
            SubmoduleState::Current
        );
        assert_eq!(
            submodule(Some(one), Some(other)).state(),
            SubmoduleState::Moved
        );
        assert_eq!(
            submodule(Some(one), None).state(),
            SubmoduleState::Uninitialised
        );
        assert_eq!(
            submodule(None, None).state(),
            SubmoduleState::Uninitialised,
            "an entry with nothing staged and nothing checked out has no commit to disagree about"
        );
        assert_eq!(
            submodule(None, Some(one)).state(),
            SubmoduleState::Current,
            "a submodule staged for removal is not a submodule that moved"
        );
    }
}
