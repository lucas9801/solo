//! SOLO: snapshots of an agent's working directory, taken with git without
//! touching the user's branch, HEAD, index, stash list or object database.
//!
//! The work tree is added to a private index (seeded from the user's index,
//! so only changed and untracked files get hashed) and written as a tree to a
//! private object directory that borrows the repository's objects as
//! alternates. Directories that aren't in a repository get a private bare
//! repository as their git dir. Ignored files aren't part of a snapshot,
//! and neither are untracked files over [`MAX_UNTRACKED_FILE_BYTES`] (SOLO,
//! G51): a rollback leaves them alone.
//!
//! SOLO: Objects no kept snapshot needs are collected (see
//! [`SnapshotStore::collect_garbage`]): kept are what this process's turns
//! need, what other Solos using the store need (each lists its trees in
//! `live/`), and the private index. All stores together are capped at
//! [`MAX_TOTAL_BYTES`]: the least recently used are emptied first (G51).
//!
//! Everything here blocks on git; run it in the background.
#![allow(
    clippy::disallowed_methods,
    reason = "runs git synchronously, only from background tasks"
)]

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, anyhow};

/// SOLO: Whether `directory` is an absolute path on a local drive: not a
/// network share (`\\server\share`), and on Windows not a drive letter
/// mapped to one.
pub(crate) fn is_local_directory(directory: &Path) -> bool {
    if !directory.is_absolute() || terminal::shell_integration::is_network_path(directory) {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        use std::path::{Component, Prefix};
        use windows::Win32::Storage::FileSystem::GetDriveTypeW;
        use windows::core::PCWSTR;

        let letter = match directory.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
                _ => return false,
            },
            _ => return false,
        };
        let root: Vec<u16> = std::ffi::OsStr::new(&format!("{}:\\", letter as char))
            .encode_wide()
            .chain(Some(0))
            .collect();
        const DRIVE_REMOTE: u32 = 4;
        // SAFETY: `root` is a NUL-terminated wide string that outlives the call.
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
        drive_type != DRIVE_REMOTE
    }
    #[cfg(not(windows))]
    true
}

/// A tree object id.
pub(crate) type TreeId = String;

/// Directories that aren't repositories are only snapshotted up to this many
/// files: a home directory or a drive isn't a project.
const MAX_FILES_OUTSIDE_REPOSITORY: usize = 20_000;
/// A store nobody snapshotted with for this long is emptied the first time a
/// process opens it.
const MAX_STORE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// SOLO: A store holding more than this is emptied the first time a process
/// opens it too: snapshots aren't collected while in use (R3 L-G9).
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Touched by every snapshot: when the store was last used, by any Solo.
const LAST_USED_FILE: &str = "last-used";
/// Locked around git commands on a store, which Solos running side by side
/// (other channels, before and after an update) share.
const LOCK_FILE: &str = "lock";
/// SOLO: Untracked files bigger than this aren't snapshotted: an archive or a
/// build output left in the work tree would make the store grow by its size
/// (G51). Rolling back leaves them as they are.
pub(crate) const MAX_UNTRACKED_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// SOLO: Contents bigger than this aren't shown in a review (G51).
const MAX_SHOWN_BYTES: u64 = 16 * 1024 * 1024;
/// SOLO: What all the stores may hold together; past this, the least
/// recently used ones are emptied when a process first opens a store (G51).
const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// SOLO: Where each Solo using a store lists the trees it keeps, in
/// `<pid>.trees`, while it holds `<pid>.lock` locked (G51).
const LIVE_DIR: &str = "live";
/// SOLO: Marks a store that another store reads objects from (a linked
/// worktree's, see [`SnapshotStore::with_linked_worktrees`]): its objects
/// aren't collected, as the other store's trees aren't known here.
const BORROWED_FILE: &str = "borrowed";

/// Git commands that share a store's index run one at a time, also within
/// this process (file locks don't exclude threads holding other handles).
static GIT_LOCK: Mutex<()> = Mutex::new(());
/// SOLO: Stores opened by this process, and what it knows of each.
static STORES: LazyLock<Mutex<HashMap<PathBuf, StoreState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// SOLO: Whether this process checked the stores' total size yet.
static TOTAL_CHECKED: AtomicBool = AtomicBool::new(false);

/// SOLO: What this process knows of a store it opened (G51).
#[derive(Default)]
struct StoreState {
    /// The large files each snapshot left out, by tree.
    large_files: HashMap<TreeId, Vec<LargeFile>>,
    /// Trees kept for as long as they're pinned (to undo a rollback, say).
    pins: HashMap<TreeId, usize>,
    /// Trees snapshotted since the last collection, which a turn may not
    /// have taken yet.
    recent: HashSet<TreeId>,
    /// The trees kept by the last collection.
    kept: HashSet<TreeId>,
    /// `live/<pid>.lock`, held locked while this process runs.
    live_lock: Option<std::fs::File>,
}

fn with_state<R>(store_dir: &Path, f: impl FnOnce(&mut StoreState) -> R) -> R {
    let mut stores = STORES.lock().unwrap_or_else(|error| error.into_inner());
    f(stores.entry(store_dir.to_path_buf()).or_default())
}

/// SOLO: An untracked file a snapshot left out, as it was then (G51).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LargeFile {
    /// Relative to the work tree, with `/` separators.
    pub path: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// SOLO: The large files that changed from `before` to `after` (added,
/// removed, or written), sorted. No snapshot has them.
pub(crate) fn changed_large_files(before: &[LargeFile], after: &[LargeFile]) -> Vec<String> {
    let mut changed: Vec<String> = after
        .iter()
        .filter(|file| !before.contains(file))
        .chain(
            before
                .iter()
                .filter(|file| !after.iter().any(|other| other.path == file.path)),
        )
        .map(|file| file.path.clone())
        .collect();
    changed.sort();
    changed.dedup();
    changed
}

/// SOLO: Keeps a tree's objects from being collected until it's dropped.
#[derive(Debug)]
pub(crate) struct TreePin {
    store_dir: PathBuf,
    tree: TreeId,
}

impl Drop for TreePin {
    fn drop(&mut self) {
        with_state(&self.store_dir, |state| {
            if let Some(count) = state.pins.get_mut(&self.tree) {
                *count -= 1;
                if *count == 0 {
                    state.pins.remove(&self.tree);
                }
            }
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileChange {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChangedFile {
    /// Relative to the work tree, with `/` separators.
    pub path: String,
    pub change: FileChange,
}

/// Why a directory isn't snapshotted, to tell the user.
#[derive(Debug)]
pub(crate) enum SnapshotUnavailable {
    /// A drive or the home directory: not a project.
    TooBroad(PathBuf),
    /// Not in a repository, and more files than the limit.
    TooManyFiles { work_tree: PathBuf, limit: usize },
    /// On a network share, or not an absolute local path.
    NotLocal(PathBuf),
}

impl std::fmt::Display for SnapshotUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooBroad(work_tree) => write!(f, "{work_tree:?} is too broad to snapshot"),
            Self::TooManyFiles { work_tree, limit } => {
                write!(f, "{work_tree:?} has more than {limit} files")
            }
            Self::NotLocal(directory) => write!(f, "{directory:?} isn't on a local drive"),
        }
    }
}

impl std::error::Error for SnapshotUnavailable {}

/// SOLO: The work tree changed after the user was shown what a rollback
/// discards.
#[derive(Debug)]
pub(crate) struct WorkTreeChanged;

impl std::fmt::Display for WorkTreeChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "files changed since the rollback was shown")
    }
}

impl std::error::Error for WorkTreeChanged {}

/// SOLO: Restoring failed after it started changing files (one was in use,
/// say): some files are restored and some aren't. Restoring `undo` puts the
/// work tree back as it was before.
#[derive(Debug)]
pub(crate) struct PartlyRestored {
    pub undo: TreeId,
    pub error: anyhow::Error,
}

impl std::fmt::Display for PartlyRestored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.error)
    }
}

impl std::error::Error for PartlyRestored {}

#[derive(Debug)]
pub(crate) struct Restored {
    /// The state before restoring, to undo it.
    pub undo: TreeId,
    pub changed: Vec<ChangedFile>,
    /// SOLO: Files left as they are because they're inside a linked directory
    /// (a symlink or junction), which could lead out of the work tree.
    pub skipped: Vec<String>,
    /// SOLO: Large untracked files that changed, which no snapshot has: left
    /// as they are (G51).
    pub large_files: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct SnapshotStore {
    work_tree: PathBuf,
    git_dir: PathBuf,
    /// The repository's objects, when the work tree is a repository.
    alternate_objects: Option<PathBuf>,
    /// The user's index, to seed the private one.
    seed_index: Option<PathBuf>,
    store_dir: PathBuf,
    /// How many files a work tree outside a repository may have.
    max_files_outside_repository: usize,
    /// SOLO: How big an untracked file may be to be snapshotted.
    max_untracked_file_bytes: u64,
}

impl SnapshotStore {
    /// The store for the repository containing `directory`, or for
    /// `directory` itself when it isn't in one. Stores live in `storage_root`.
    pub(crate) fn open(directory: &Path, storage_root: &Path) -> Result<Self> {
        Self::open_inner(directory, storage_root, false)
    }

    /// SOLO: `borrowed`: another store reads this one's objects, so they're
    /// never collected. [`GIT_LOCK`] is held then.
    fn open_inner(directory: &Path, storage_root: &Path, borrowed: bool) -> Result<Self> {
        // SOLO: the directory can come from a report or the shell's OSC 7;
        // git on a network share would connect to another machine.
        if !is_local_directory(directory) {
            return Err(SnapshotUnavailable::NotLocal(directory.to_path_buf()).into());
        }
        let output = git_command(directory)
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ])
            .output()
            .context("running git")?;
        let repository = if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut lines = stdout.lines().map(|line| native_path(line.trim()));
            match (lines.next(), lines.next(), lines.next()) {
                (Some(top_level), Some(git_dir), Some(common_dir)) => {
                    Some((top_level, git_dir, common_dir))
                }
                _ => None,
            }
        } else {
            None
        };

        let (work_tree, git_dir, alternate_objects, seed_index) = match repository {
            // A repository whose git dir (a `.git` file's `gitdir:`) is on
            // another machine.
            Some((top_level, git_dir, common_dir))
                if ![&top_level, &git_dir, &common_dir]
                    .into_iter()
                    .all(|path| is_local_directory(path)) =>
            {
                return Err(SnapshotUnavailable::NotLocal(git_dir).into());
            }
            Some((top_level, git_dir, common_dir)) => (
                top_level,
                git_dir.clone(),
                Some(common_dir.join("objects")),
                Some(git_dir.join("index")),
            ),
            None => {
                let work_tree = directory
                    .canonicalize()
                    .with_context(|| format!("resolving {directory:?}"))?;
                let work_tree =
                    PathBuf::from(util::paths::SanitizedPath::new(&work_tree).as_path());
                if work_tree.parent().is_none() || dirs::home_dir().as_ref() == Some(&work_tree) {
                    return Err(SnapshotUnavailable::TooBroad(work_tree).into());
                }
                (work_tree, PathBuf::new(), None, None)
            }
        };

        let store_dir = storage_root.join(store_name(&work_tree));
        let first_open = {
            let mut stores = STORES
                .lock()
                .map_err(|_| anyhow!("snapshot stores lock poisoned"))?;
            let first_open = !stores.contains_key(&store_dir);
            stores.entry(store_dir.clone()).or_default();
            first_open
        };
        std::fs::create_dir_all(&store_dir).with_context(|| format!("creating {store_dir:?}"))?;
        if borrowed {
            std::fs::write(store_dir.join(BORROWED_FILE), b"")
                .with_context(|| format!("marking {store_dir:?}"))?;
        }
        let borrowed = borrowed || store_dir.join(BORROWED_FILE).exists();
        if first_open {
            // SOLO: this process uses the store from now on (G51).
            if let Err(error) = hold_live_lock(&store_dir) {
                log::warn!("failed to mark {store_dir:?} as in use: {error:#}");
            }
            if !TOTAL_CHECKED.swap(true, Ordering::SeqCst) {
                trim_stores(storage_root, MAX_TOTAL_BYTES);
            }
        }
        // SOLO: turns live as long as the process: what earlier sessions'
        // turns kept isn't needed anymore, unless another Solo still runs
        // with the store (G51). A borrowed store's objects may be needed by
        // another store's snapshots.
        if first_open
            && (!borrowed
                || is_older_than(&store_dir.join(LAST_USED_FILE), MAX_STORE_AGE)
                || holds_more_than(&store_dir.join("objects"), MAX_STORE_BYTES))
        {
            empty_unused_store(&store_dir)?;
        }
        std::fs::create_dir_all(store_dir.join("objects"))
            .with_context(|| format!("creating {store_dir:?}"))?;

        let git_dir = if alternate_objects.is_some() {
            git_dir
        } else {
            let private = store_dir.join("repository");
            if !private.join("HEAD").exists() {
                run(git_command(&store_dir)
                    .args(["init", "--bare", "--quiet"])
                    .arg(&private))?;
            }
            private
        };

        let store = Self {
            work_tree,
            git_dir,
            alternate_objects,
            seed_index,
            store_dir,
            max_files_outside_repository: MAX_FILES_OUTSIDE_REPOSITORY,
            max_untracked_file_bytes: MAX_UNTRACKED_FILE_BYTES,
        };
        // SOLO: and what only earlier sessions' turns kept goes when another
        // Solo keeps the rest (G51).
        if first_open
            && !borrowed
            && in_use_elsewhere(&store.store_dir)
            && let Err(error) = store.collect_garbage(Vec::new())
        {
            log::warn!(
                "failed to collect the snapshots in {:?}: {error:#}",
                store.store_dir
            );
        }
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn set_max_files_outside_repository(&mut self, max: usize) {
        self.max_files_outside_repository = max;
    }

    #[cfg(test)]
    pub(crate) fn set_max_untracked_file_bytes(&mut self, max: u64) {
        self.max_untracked_file_bytes = max;
    }

    /// SOLO: Where the store keeps its files.
    pub(crate) fn store_dir(&self) -> &Path {
        &self.store_dir
    }

    /// SOLO: Keeps `tree`'s objects until the pin is dropped (G51).
    pub(crate) fn pin(&self, tree: &str) -> TreePin {
        with_state(&self.store_dir, |state| {
            *state.pins.entry(tree.to_string()).or_default() += 1;
        });
        TreePin {
            store_dir: self.store_dir.clone(),
            tree: tree.to_string(),
        }
    }

    /// SOLO: The large untracked files `tree` left out, as far as this
    /// process knows (G51).
    pub(crate) fn large_files(&self, tree: &str) -> Vec<LargeFile> {
        with_state(&self.store_dir, |state| {
            state.large_files.get(tree).cloned().unwrap_or_default()
        })
    }

    pub(crate) fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    /// Where a file of a snapshot is in the work tree.
    pub(crate) fn abs_path(&self, path: &str) -> PathBuf {
        self.work_tree.join(native_path(path))
    }

    pub(crate) fn is_repository(&self) -> bool {
        self.alternate_objects.is_some()
    }

    /// Records the work tree as it is now.
    pub(crate) fn snapshot(&self) -> Result<TreeId> {
        Ok(self.snapshot_with_large_files()?.0)
    }

    /// SOLO: [`Self::snapshot`], with the large untracked files it left out.
    pub(crate) fn snapshot_with_large_files(&self) -> Result<(TreeId, Vec<LargeFile>)> {
        let _lock = GIT_LOCK
            .lock()
            .map_err(|_| anyhow!("snapshot lock poisoned"))?;
        let _store_lock = self.lock_store()?;
        self.snapshot_locked()
    }

    /// Excludes other processes from the store until the file is dropped.
    fn lock_store(&self) -> Result<std::fs::File> {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.store_dir.join(LOCK_FILE))
            .context("opening the snapshot store's lock")?;
        lock.lock().context("locking the snapshot store")?;
        Ok(lock)
    }

    fn snapshot_locked(&self) -> Result<(TreeId, Vec<LargeFile>)> {
        if let Err(error) = std::fs::write(self.store_dir.join(LAST_USED_FILE), b"") {
            log::warn!("failed to mark {:?} as used: {error}", self.store_dir);
        }
        let (tree, large_files) = self.snapshot_and_retry()?;
        // SOLO: remembered for rolling back, and kept until the next
        // collection at least (G51).
        let trees = with_state(&self.store_dir, |state| {
            if !large_files.is_empty() {
                let known = state.large_files.entry(tree.clone()).or_default();
                for file in &large_files {
                    known.retain(|known| known.path != file.path);
                    known.push(file.clone());
                }
            }
            state.recent.insert(tree.clone()).then(|| live_trees(state))
        });
        if let Some(trees) = trees {
            self.write_live_trees(&trees);
        }
        Ok((tree, large_files))
    }

    fn snapshot_and_retry(&self) -> Result<(TreeId, Vec<LargeFile>)> {
        match self.write_snapshot() {
            Ok(snapshot) => Ok(snapshot),
            // SOLO: the private index can name objects that are gone (a blob
            // staged in the user's index, since unstaged and pruned by
            // `git gc`); git trusts the index for files that look unchanged,
            // so every snapshot would fail. Starting the index over hashes
            // the files again (R3 M12).
            Err(error) => {
                log::warn!(
                    "snapshot of {:?} failed, starting its index over: {error:#}",
                    self.work_tree
                );
                let index = self.index_path();
                match std::fs::remove_file(&index) {
                    Ok(()) => self.write_snapshot(),
                    Err(remove) if remove.kind() == std::io::ErrorKind::NotFound => Err(error),
                    Err(remove) => {
                        Err(error.context(format!("failed to remove {index:?}: {remove}")))
                    }
                }
            }
        }
    }

    fn write_snapshot(&self) -> Result<(TreeId, Vec<LargeFile>)> {
        let index = self.index_path();
        if !index.exists() {
            if let Some(seed) = self.seed_index.as_ref().filter(|seed| seed.exists()) {
                std::fs::copy(seed, &index)
                    .with_context(|| format!("copying the index from {seed:?}"))?;
            } else if !self.is_repository() {
                let output = self
                    .command(&index)
                    .args(["ls-files", "--others", "--exclude-standard", "-z"])
                    .output()
                    .context("running git ls-files")?;
                let count = output.stdout.split(|byte| *byte == 0).count();
                if count > self.max_files_outside_repository {
                    return Err(SnapshotUnavailable::TooManyFiles {
                        work_tree: self.work_tree.clone(),
                        limit: self.max_files_outside_repository,
                    }
                    .into());
                }
            }
        }
        let large_files = self.large_untracked_files()?;
        // Files that can't be read (locked, say) are left as they were in the
        // last snapshot rather than failing it.
        let mut add = self.command(&index);
        add.args(["add", "--all", "--ignore-errors"]);
        let pathspecs = self.store_dir.join("pathspecs");
        if large_files.is_empty() {
            add.args(["--", "."]);
        } else {
            // SOLO: maybe too many to name on a command line (G51).
            let mut list = b".\0".to_vec();
            for file in &large_files {
                list.extend_from_slice(b":(exclude,literal)");
                list.extend_from_slice(file.path.as_bytes());
                list.push(0);
            }
            std::fs::write(&pathspecs, list).with_context(|| format!("writing {pathspecs:?}"))?;
            add.arg("--pathspec-from-file")
                .arg(&pathspecs)
                .arg("--pathspec-file-nul");
        }
        let output = add.output().context("running git add");
        std::fs::remove_file(&pathspecs).ok();
        let output = output?;
        if !output.status.success() {
            log::warn!(
                "git add for a snapshot of {:?} reported: {}",
                self.work_tree,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        if !large_files.is_empty() {
            // SOLO: and out of the index, if an earlier snapshot has them.
            let paths: Vec<u8> = large_files
                .iter()
                .flat_map(|file| file.path.as_bytes().iter().copied().chain([0]))
                .collect();
            self.run_with_input(
                &index,
                &["update-index", "--force-remove", "-z", "--stdin"],
                paths,
            )?;
            log::info!(
                "not snapshotting {} untracked files over {} MB in {:?}",
                large_files.len(),
                self.max_untracked_file_bytes / (1024 * 1024),
                self.work_tree
            );
        }
        let tree = run(self.command(&index).arg("write-tree"))?;
        let tree = String::from_utf8_lossy(&tree).trim().to_string();
        Ok((self.with_linked_worktrees(tree)?, large_files))
    }

    /// SOLO: The untracked files (not in the user's index) bigger than
    /// snapshots take, sorted (G51).
    fn large_untracked_files(&self) -> Result<Vec<LargeFile>> {
        // A missing index is an empty one: every file is untracked.
        let user_index = self
            .seed_index
            .clone()
            .unwrap_or_else(|| self.store_dir.join("no-index"));
        let output = run(self.command(&user_index).args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
        ]))?;
        let mut large = Vec::new();
        for path in output.split(|byte| *byte == 0) {
            let Ok(path) = std::str::from_utf8(path) else {
                continue;
            };
            if path.is_empty() {
                continue;
            }
            let Ok(metadata) = std::fs::symlink_metadata(self.abs_path(path)) else {
                continue;
            };
            if metadata.is_file() && metadata.len() > self.max_untracked_file_bytes {
                large.push(LargeFile {
                    path: path.to_string(),
                    size: metadata.len(),
                    modified: metadata.modified().ok(),
                });
            }
        }
        large.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(large)
    }

    /// SOLO: Removes the objects that none of these need: `live`, the trees
    /// pinned or snapshotted since the last collection, the trees other Solos
    /// using the store keep, and the private index. What's kept is packed
    /// into one pack; the repository's own objects aren't copied (G51).
    pub(crate) fn collect_garbage(&self, live: Vec<TreeId>) -> Result<()> {
        let _lock = GIT_LOCK
            .lock()
            .map_err(|_| anyhow!("snapshot lock poisoned"))?;
        let _store_lock = self.lock_store()?;
        self.collect_garbage_locked(live)
    }

    fn collect_garbage_locked(&self, live: Vec<TreeId>) -> Result<()> {
        let objects = self.store_dir.join("objects");
        if self.store_dir.join(BORROWED_FILE).exists()
            || objects.join("info").join("alternates").exists()
        {
            return Ok(());
        }
        let started = std::time::Instant::now();
        let mut trees: HashSet<TreeId> = live.into_iter().collect();
        with_state(&self.store_dir, |state| {
            trees.extend(state.pins.keys().cloned());
            trees.extend(state.recent.iter().cloned());
        });
        trees.extend(other_processes_trees(&self.store_dir));
        let index = self.index_path();
        if index.exists() {
            match run(self.command(&index).arg("write-tree")) {
                Ok(tree) => {
                    trees.insert(String::from_utf8_lossy(&tree).trim().to_string());
                }
                // It names objects that are gone: the next snapshot hashes
                // the files again.
                Err(_) => {
                    std::fs::remove_file(&index).ok();
                }
            }
        }

        // Those still there.
        let ids = |trees: &[TreeId]| -> Vec<u8> {
            trees
                .iter()
                .flat_map(|tree| tree.bytes().chain([b'\n']))
                .collect()
        };
        let trees: Vec<TreeId> = trees.into_iter().filter(|tree| !tree.is_empty()).collect();
        let checked = self.run_with_input(&index, &["cat-file", "--batch-check"], ids(&trees))?;
        let trees: Vec<TreeId> = String::from_utf8_lossy(&checked)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split(' ');
                let id = fields.next()?;
                (fields.next() == Some("tree")).then(|| id.to_string())
            })
            .collect();
        let reachable = self.run_with_input(
            &index,
            &["rev-list", "--objects", "--missing=allow-any", "--stdin"],
            ids(&trees),
        )?;
        let pack_dir = objects.join("pack");
        std::fs::create_dir_all(&pack_dir).with_context(|| format!("creating {pack_dir:?}"))?;
        let base = pack_dir.join("pack").to_string_lossy().into_owned();
        let new_pack = self.run_with_input(
            &index,
            &["pack-objects", "--local", "--non-empty", "-q", &base],
            reachable,
        )?;
        let new_pack = format!("pack-{}.", String::from_utf8_lossy(&new_pack).trim());

        // Everything else goes. A file another git has open stays until the
        // next time.
        let mut left = 0usize;
        for entry in std::fs::read_dir(&objects)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.len() == 2
                && name.chars().all(|c| c.is_ascii_hexdigit())
                && remove_tree(&entry.path()).is_err()
            {
                left += 1;
            }
        }
        for entry in std::fs::read_dir(&pack_dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("pack-")
                && !name.starts_with(&new_pack)
                && remove_read_only(&entry.path()).is_err()
            {
                left += 1;
            }
        }
        let kept = with_state(&self.store_dir, |state| {
            state.kept = trees.iter().cloned().collect();
            state.recent.clear();
            let kept = &state.kept;
            state.large_files.retain(|tree, _| kept.contains(tree));
            live_trees(state)
        });
        self.write_live_trees(&kept);
        log::info!(
            "collected the snapshots of {:?} in {:?}: {} trees kept, {} files in use left",
            self.work_tree,
            started.elapsed(),
            trees.len(),
            left
        );
        Ok(())
    }

    /// Runs git on the store with `input` on its stdin.
    fn run_with_input(&self, index: &Path, args: &[&str], input: Vec<u8>) -> Result<Vec<u8>> {
        let mut child = self
            .command(index)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        let mut stdin = child.stdin.take().context("no stdin for git")?;
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output();
        writer.join().ok();
        check(output?, &args.join(" "))
    }

    /// Lists this process's trees for other Solos using the store.
    fn write_live_trees(&self, trees: &[TreeId]) {
        let path = self
            .store_dir
            .join(LIVE_DIR)
            .join(format!("{}.trees", std::process::id()));
        let mut contents = trees.join("\n");
        contents.push('\n');
        if let Err(error) = std::fs::write(&path, contents) {
            log::warn!("failed to write {path:?}: {error}");
        }
    }

    /// SOLO: `tree` with the linked worktrees in this folder snapshotted in
    /// place of the nested repositories they'd be otherwise: an agent task
    /// across repositories works in a folder with one worktree of each, and
    /// what the agent changes in them is the turn's (R3 M3). Each worktree
    /// has its own store; this store reads their objects through
    /// `objects/info/alternates`. Repositories cloned into a folder (with a
    /// `.git` folder, not file) stay nested repositories, left alone.
    fn with_linked_worktrees(&self, tree: TreeId) -> Result<TreeId> {
        if self.is_repository() {
            return Ok(tree);
        }
        let listing = run(self
            .command(&self.index_path())
            .args(["ls-tree", "-r", "-z", &tree]))?;
        let worktrees: Vec<String> = listing
            .split(|byte| *byte == 0)
            .filter_map(|entry| {
                let (meta, path) = std::str::from_utf8(entry).ok()?.split_once('\t')?;
                meta.starts_with("160000 ").then(|| path.to_string())
            })
            .filter(|path| self.work_tree.join(path).join(".git").is_file())
            .collect();
        if worktrees.is_empty() {
            return Ok(tree);
        }
        let storage_root = self
            .store_dir
            .parent()
            .context("the snapshot store has no parent folder")?;
        let index = self.store_dir.join("worktrees-index");
        std::fs::remove_file(&index).ok();
        run(self.command(&index).args(["read-tree", &tree]))?;
        for path in &worktrees {
            let directory = self.work_tree.join(path);
            let snapshot =
                SnapshotStore::open_inner(&directory, storage_root, true).and_then(|store| {
                    anyhow::ensure!(store.is_repository(), "{directory:?} isn't a repository");
                    let _lock = store.lock_store()?;
                    let (tree, _) = store.snapshot_locked()?;
                    Ok((store, tree))
                });
            let (store, subtree) = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    log::warn!("not snapshotting the worktree {directory:?}: {error:#}");
                    continue;
                }
            };
            self.add_alternates(
                [store.store_dir.join("objects")]
                    .into_iter()
                    .chain(store.alternate_objects.clone()),
            )?;
            run(self
                .command(&index)
                .args(["update-index", "--force-remove", "--"])
                .arg(path))?;
            run(self
                .command(&index)
                .args(["read-tree", &format!("--prefix={path}/"), &subtree]))?;
        }
        let combined = run(self.command(&index).arg("write-tree"));
        std::fs::remove_file(&index).ok();
        Ok(String::from_utf8_lossy(&combined?).trim().to_string())
    }

    /// Adds `directories` to the object directories this store reads from.
    fn add_alternates(&self, directories: impl IntoIterator<Item = PathBuf>) -> Result<()> {
        let file = self
            .store_dir
            .join("objects")
            .join("info")
            .join("alternates");
        let mut lines: Vec<String> = std::fs::read_to_string(&file)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let before = lines.len();
        for directory in directories {
            let line = directory.to_string_lossy().replace('\\', "/");
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        if lines.len() != before {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&file, lines.join("\n") + "\n")
                .with_context(|| format!("writing {file:?}"))?;
        }
        Ok(())
    }

    /// Files that differ between two snapshots. Repositories nested in the
    /// work tree (a `git clone` into it, say) aren't files: they're left out.
    pub(crate) fn changed_files(&self, from: &str, to: &str) -> Result<Vec<ChangedFile>> {
        Ok(self.changes(from, to)?.0)
    }

    /// The files that differ between two snapshots, and the nested
    /// repositories (gitlinks) that do, with the files whose names aren't
    /// UTF-8 (see [`parse_raw_diff`]).
    fn changes(&self, from: &str, to: &str) -> Result<(Vec<ChangedFile>, Vec<String>)> {
        if from == to {
            return Ok((Vec::new(), Vec::new()));
        }
        let output = run(self.command(&self.index_path()).args([
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--raw",
            from,
            to,
        ]))?;
        Ok(parse_raw_diff(&output))
    }

    /// A file's contents in a snapshot, as it would be checked out.
    pub(crate) fn file_contents(&self, tree: &str, path: &str) -> Result<Vec<u8>> {
        run(self
            .command(&self.index_path())
            .args(["cat-file", "--filters"])
            .arg(format!("{tree}:{path}")))
    }

    /// SOLO: the contents of each `(tree, path)` as stored, for showing, with
    /// one git process for all of them rather than one each (R3 L-G9). `None`
    /// for one that can't be read. Unlike [`Self::file_contents`], line
    /// endings and other filters aren't applied: with `--filters`, git (2.37
    /// at least) reports the stored size for the filtered contents. The
    /// review compares them without line endings anyway.
    pub(crate) fn files_contents(
        &self,
        files: &[(String, String)],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        use std::io::{BufRead as _, Read as _, Write as _};
        let mut child = self
            .command(&self.index_path())
            .args(["cat-file", "--batch"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("running git cat-file --batch")?;
        // A path with a line break can't be asked for on a line.
        let asked: Vec<bool> = files
            .iter()
            .map(|(_, path)| !path.contains(['\n', '\r']))
            .collect();
        let mut input = String::new();
        for ((tree, path), asked) in files.iter().zip(&asked) {
            if *asked {
                input.push_str(&format!("{tree}:{path}\n"));
            }
        }
        let mut stdin = child.stdin.take().context("no stdin")?;
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let mut stdout = std::io::BufReader::new(child.stdout.take().context("no stdout")?);
        let mut contents = Vec::with_capacity(files.len());
        for asked in asked {
            if !asked {
                contents.push(None);
                continue;
            }
            let mut header = String::new();
            stdout.read_line(&mut header)?;
            // `<object> <type> <size>`, or `<name> missing`.
            let size = header
                .trim_end()
                .rsplit(' ')
                .next()
                .and_then(|size| size.parse::<usize>().ok())
                .filter(|_| !header.trim_end().ends_with(" missing"));
            let Some(size) = size else {
                contents.push(None);
                continue;
            };
            // SOLO: too big to show; not read into memory (G51).
            if size as u64 > MAX_SHOWN_BYTES {
                std::io::copy(
                    &mut (&mut stdout).take(size as u64 + 1),
                    &mut std::io::sink(),
                )?;
                contents.push(None);
                continue;
            }
            let mut bytes = vec![0; size + 1];
            stdout.read_exact(&mut bytes)?;
            bytes.pop();
            contents.push(Some(bytes));
        }
        writer.join().ok();
        child.wait().ok();
        Ok(contents)
    }

    /// Puts the work tree back as it was in `target`: changed files get their
    /// contents back, and files that weren't there are removed. The current
    /// state is snapshotted first, to undo it. If it fails partway, the
    /// error is [`PartlyRestored`], with the state to undo it to.
    pub(crate) fn restore(&self, target: &str) -> Result<Restored> {
        self.restore_checked(target, None)
    }

    /// SOLO: [`Self::restore`], unless the work tree changed since it was
    /// `shown` (snapshotted for the user to confirm what restoring
    /// discards): then nothing changes, and the error is [`WorkTreeChanged`].
    pub(crate) fn restore_if_unchanged(&self, target: &str, shown: &str) -> Result<Restored> {
        self.restore_checked(target, Some(shown))
    }

    fn restore_checked(&self, target: &str, shown: Option<&str>) -> Result<Restored> {
        let _lock = GIT_LOCK
            .lock()
            .map_err(|_| anyhow!("snapshot lock poisoned"))?;
        let _store_lock = self.lock_store()?;
        let (current, current_large) = self.snapshot_locked()?;
        if shown.is_some_and(|shown| shown != current) {
            return Err(WorkTreeChanged.into());
        }
        let (changed, nested_repositories) = self.changes(target, &current)?;

        // SOLO: git (for Windows) takes a junction for a directory, so files
        // through one are in snapshots; writing or deleting them would change
        // files outside the work tree. They're left alone, as are repositories
        // nested in the work tree, which aren't files to remove or check out
        // (R3 M3).
        let skipped: Vec<String> = changed
            .iter()
            .filter(|file| linked_ancestor(&self.abs_path(&file.path), &self.work_tree).is_some())
            .map(|file| file.path.clone())
            .chain(nested_repositories)
            .collect();
        if !skipped.is_empty() {
            log::warn!(
                "not restoring {} files in linked directories of {:?}: {:?}",
                skipped.len(),
                self.work_tree,
                skipped
            );
        }
        // SOLO: a large untracked file is in neither snapshot, or in one only
        // because it was small then: restoring that would lose what it is
        // now, which no snapshot has. It's left as it is (G51).
        let target_large = self.large_files(target);
        let large_paths: HashSet<&str> = current_large
            .iter()
            .chain(&target_large)
            .map(|file| file.path.as_str())
            .collect();
        let mut large_files = changed_large_files(&target_large, &current_large);
        large_files.extend(
            changed
                .iter()
                .filter(|file| large_paths.contains(file.path.as_str()))
                .map(|file| file.path.clone()),
        );
        large_files.sort();
        large_files.dedup();
        let is_skipped = |file: &&ChangedFile| {
            skipped.contains(&file.path) || large_paths.contains(file.path.as_str())
        };

        let to_check_out: Vec<&str> = changed
            .iter()
            .filter(|file| file.change != FileChange::Added && !is_skipped(file))
            .map(|file| file.path.as_str())
            .collect();
        let mut errors = Vec::new();
        if !to_check_out.is_empty() {
            let restore_index = self.store_dir.join("restore-index");
            let result = (|| {
                run(self.command(&restore_index).args(["read-tree", target]))?;
                let mut child = self
                    .command(&restore_index)
                    .args(["checkout-index", "--force", "-z", "--stdin"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .context("running git checkout-index")?;
                let mut stdin = child.stdin.take().context("no stdin for git")?;
                for path in &to_check_out {
                    stdin.write_all(path.as_bytes())?;
                    stdin.write_all(&[0])?;
                }
                drop(stdin);
                check(child.wait_with_output()?, "git checkout-index")
            })();
            if let Err(error) = std::fs::remove_file(&restore_index)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                log::warn!("failed to remove {restore_index:?}: {error}");
            }
            if let Err(error) = result {
                errors.push(error);
            }
        }

        // SOLO: files are changed by now, so the rest is still done, and the
        // state to undo it is returned even when something failed (R3 M2).
        for file in changed
            .iter()
            .filter(|file| file.change == FileChange::Added && !is_skipped(file))
        {
            let path = self.abs_path(&file.path);
            match std::fs::remove_file(&path) {
                Ok(()) => remove_empty_parents(&path, &self.work_tree),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    errors.push(anyhow::Error::new(error).context(format!("removing {path:?}")));
                }
            }
        }
        if let Some(first) = errors.first() {
            let error = if errors.len() == 1 {
                anyhow!("{first:#}")
            } else {
                anyhow!("{first:#} (and {} more errors)", errors.len() - 1)
            };
            log::error!(
                "restoring {:?} to {target} stopped partway; the state before is {current}: {errors:?}",
                self.work_tree
            );
            return Err(PartlyRestored {
                undo: current,
                error,
            }
            .into());
        }

        let changed = changed
            .into_iter()
            .filter(|file| !large_paths.contains(file.path.as_str()))
            .collect();
        Ok(Restored {
            undo: current,
            changed,
            skipped,
            large_files,
        })
    }

    fn index_path(&self) -> PathBuf {
        self.store_dir.join("index")
    }

    fn command(&self, index: &Path) -> Command {
        let mut command = git_command(&self.work_tree);
        command
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.work_tree)
            .env("GIT_INDEX_FILE", index)
            .env("GIT_OBJECT_DIRECTORY", self.store_dir.join("objects"));
        match &self.alternate_objects {
            Some(objects) => command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects),
            None => command.env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES"),
        };
        command
    }
}

/// Line endings aren't converted, so files are restored byte for byte, whatever
/// the user's `core.autocrlf` (only `.gitattributes` rules still apply).
const GIT_CONFIG: [&str; 10] = [
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.splitIndex=false",
    "-c",
    "core.safecrlf=false",
    "-c",
    "gc.auto=0",
];

fn native_path(path: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.replace('/', "\\"))
    } else {
        PathBuf::from(path)
    }
}

fn git_command(directory: &Path) -> Command {
    let mut command = util::command::new_std_command("git");
    command
        .current_dir(directory)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(GIT_CONFIG)
        .stdin(Stdio::null());
    command
}

fn run(command: &mut Command) -> Result<Vec<u8>> {
    let output = command
        .output()
        .with_context(|| format!("running {command:?}"))?;
    let name = command
        .get_args()
        .skip(GIT_CONFIG.len())
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    check(output, &name)
}

fn check(output: Output, name: &str) -> Result<Vec<u8>> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(anyhow!(
            "git {name} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// A gitlink's mode in a tree: a repository nested in the work tree.
const GITLINK_MODE: &[u8] = b"160000";

/// Parses `git diff-tree -z --raw` output (`:<mode> <mode> <id> <id>
/// <status>`, then the path) into the changed files and, apart, the paths to
/// leave alone: nested repositories (gitlinks) that changed, and (SOLO, R3
/// L-G6) files whose names aren't UTF-8, which Unix allows: written back as
/// lossy text, they'd name another file. Those are shown lossily.
fn parse_raw_diff(output: &[u8]) -> (Vec<ChangedFile>, Vec<String>) {
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut changed = Vec::new();
    let mut gitlinks = Vec::new();
    while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
        let meta: Vec<&[u8]> = meta
            .strip_prefix(b":")
            .unwrap_or(meta)
            .split(|byte| *byte == b' ')
            .collect();
        let Ok(path) = String::from_utf8(path.to_vec()) else {
            gitlinks.push(String::from_utf8_lossy(path).into_owned());
            continue;
        };
        if meta.iter().take(2).any(|mode| *mode == GITLINK_MODE) {
            gitlinks.push(path);
            continue;
        }
        let change = match meta.get(4).and_then(|status| status.first()) {
            Some(b'A') => FileChange::Added,
            Some(b'D') => FileChange::Deleted,
            _ => FileChange::Modified,
        };
        changed.push(ChangedFile { path, change });
    }
    (changed, gitlinks)
}

/// A directory name for the store of `work_tree`: readable, and unique by a hash.
fn store_name(work_tree: &Path) -> String {
    let path = work_tree.to_string_lossy();
    let readable: String = path
        .chars()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    // FNV-1a, which doesn't change between Rust versions like `DefaultHasher` may.
    let hash = path.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    });
    format!("{readable}-{hash:016x}")
}

/// Removes what an unused store holds, unless another process is using it
/// right now. The lock file stays: it may be open elsewhere.
fn empty_unused_store(store_dir: &Path) -> Result<()> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store_dir.join(LOCK_FILE))
        .context("opening the snapshot store's lock")?;
    if lock.try_lock().is_err() || in_use_elsewhere(store_dir) {
        return Ok(());
    }
    for name in ["objects", "repository", "index", "restore-index"] {
        let path = store_dir.join(name);
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = removed
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error).with_context(|| format!("removing old snapshots in {path:?}"));
        }
    }
    Ok(())
}

/// SOLO: The trees this process keeps in a store, for other Solos to see.
fn live_trees(state: &StoreState) -> Vec<TreeId> {
    let mut trees: Vec<TreeId> = state
        .kept
        .iter()
        .chain(&state.recent)
        .chain(state.pins.keys())
        .cloned()
        .collect();
    trees.sort();
    trees.dedup();
    trees
}

/// SOLO: Locks `live/<pid>.lock` in the store for as long as this process
/// runs, so other Solos know it uses the store (G51).
fn hold_live_lock(store_dir: &Path) -> Result<()> {
    let live = store_dir.join(LIVE_DIR);
    std::fs::create_dir_all(&live)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(live.join(format!("{}.lock", std::process::id())))?;
    file.try_lock()?;
    with_state(store_dir, |state| state.live_lock = Some(file));
    Ok(())
}

/// SOLO: The other Solos using the store, by the names of their files in
/// `live/`. The files of Solos that are gone are removed.
fn other_processes(store_dir: &Path) -> Vec<String> {
    let live = store_dir.join(LIVE_DIR);
    let own = std::process::id().to_string();
    let Ok(entries) = std::fs::read_dir(&live) else {
        return Vec::new();
    };
    let mut others = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "lock") {
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
        else {
            continue;
        };
        if stem == own {
            continue;
        }
        let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) else {
            // Being created or removed right now: in use.
            others.push(stem);
            continue;
        };
        if file.try_lock().is_err() {
            others.push(stem);
        } else {
            drop(file);
            std::fs::remove_file(&path).ok();
            std::fs::remove_file(live.join(format!("{stem}.trees"))).ok();
        }
    }
    others
}

fn in_use_elsewhere(store_dir: &Path) -> bool {
    !other_processes(store_dir).is_empty()
}

/// SOLO: The trees other Solos using the store keep.
fn other_processes_trees(store_dir: &Path) -> Vec<TreeId> {
    other_processes(store_dir)
        .into_iter()
        .filter_map(|stem| {
            std::fs::read_to_string(store_dir.join(LIVE_DIR).join(format!("{stem}.trees"))).ok()
        })
        .flat_map(|contents| {
            contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// SOLO: Empties the least recently used stores in `storage_root` that no
/// Solo uses (this one included) until they hold `max_bytes` at most (G51).
fn trim_stores(storage_root: &Path, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(storage_root) else {
        return;
    };
    let mut stores: Vec<(PathBuf, u64, Option<SystemTime>)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .map(|entry| {
            let path = entry.path();
            let size =
                directory_size(&path.join("objects")) + directory_size(&path.join("repository"));
            let used = std::fs::metadata(path.join(LAST_USED_FILE))
                .and_then(|metadata| metadata.modified())
                .ok();
            (path, size, used)
        })
        .collect();
    let mut total: u64 = stores.iter().map(|(_, size, _)| size).sum();
    if total <= max_bytes {
        return;
    }
    stores.sort_by_key(|(_, _, used)| *used);
    for (path, size, _) in stores {
        if total <= max_bytes {
            break;
        }
        let ours = STORES
            .lock()
            .map_or(true, |stores| stores.contains_key(&path));
        if ours {
            continue;
        }
        match empty_unused_store(&path) {
            Ok(()) => {
                let left = directory_size(&path.join("objects"))
                    + directory_size(&path.join("repository"));
                if left < size {
                    log::info!(
                        "emptied the snapshots in {path:?} ({} MB): all snapshots took more than {} MB",
                        (size - left) / (1024 * 1024),
                        max_bytes / (1024 * 1024)
                    );
                    total -= size - left;
                }
            }
            Err(error) => log::warn!("failed to empty {path:?}: {error:#}"),
        }
    }
}

fn directory_size(directory: &Path) -> u64 {
    let mut total = 0u64;
    let mut directories = vec![directory.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata().map_or(0, |metadata| metadata.len());
            }
        }
    }
    total
}

/// SOLO: Removes a file git made read-only, as it makes objects.
fn remove_read_only(path: &Path) -> std::io::Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        let mut permissions = metadata.permissions();
        if permissions.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(path, permissions).ok();
        }
    }
    std::fs::remove_file(path)
}

/// SOLO: Removes a directory of git's files, read-only ones too.
fn remove_tree(directory: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)?.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            remove_tree(&path)?;
        } else {
            remove_read_only(&path)?;
        }
    }
    std::fs::remove_dir(directory)
}

/// SOLO: whether the files under `directory` take more than `bytes`, counted
/// until they do (R3 L-G9).
fn holds_more_than(directory: &Path, bytes: u64) -> bool {
    let mut total = 0u64;
    let mut directories = vec![directory.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata().map_or(0, |metadata| metadata.len());
                if total > bytes {
                    return true;
                }
            }
        }
    }
    false
}

fn is_older_than(path: &Path, age: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|created| SystemTime::now().duration_since(created).ok())
        .is_some_and(|elapsed| elapsed > age)
}

/// Removes the directories left empty by removing `path`, up to `root`.
/// SOLO: The first directory on the way from `root` to `path` (`path` itself
/// excluded) that's a link: a symlink or, on Windows, a junction or another
/// reparse point. Writing or deleting `path` would follow it, maybe out of
/// `root`. A path outside `root`, or with `..`, counts as linked too.
pub(crate) fn linked_ancestor(path: &Path, root: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let Some((root, relative)) = path_in(path, root) else {
        return Some(path.to_path_buf());
    };
    let mut current = root;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        if !matches!(component, Component::Normal(_)) {
            return Some(current);
        }
        if components.peek().is_none() {
            break;
        }
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if is_link(&metadata) => return Some(current),
            Ok(_) => {}
            // Nothing there to follow.
            Err(_) => return None,
        }
    }
    None
}

/// `path` relative to `root`, with the root as `path` spells it. The two may
/// spell it differently: git gives the long, `/`-separated form while a path
/// from elsewhere can use another case or a short (8.3) name, like CI's temp
/// directory `C:\Users\RUNNER~1\...`. Then the root is the folder on the
/// path that is the same folder on disk; only folders above the repository
/// are resolved, so links inside it are still caught.
fn path_in(path: &Path, root: &Path) -> Option<(PathBuf, PathBuf)> {
    if let Ok(relative) = path.strip_prefix(root) {
        return Some((root.to_path_buf(), relative.to_path_buf()));
    }
    let root_on_disk = std::fs::canonicalize(root).ok()?;
    path.ancestors().skip(1).find_map(|ancestor| {
        let same = std::fs::canonicalize(ancestor).ok()? == root_on_disk;
        same.then(|| {
            let relative = path.strip_prefix(ancestor).ok()?;
            Some((ancestor.to_path_buf(), relative.to_path_buf()))
        })
        .flatten()
    })
}

fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

fn remove_empty_parents(path: &Path, root: &Path) {
    let mut directory = path.parent();
    while let Some(current) = directory {
        if current == root || !current.starts_with(root) {
            break;
        }
        // Fails, as it should, when the directory isn't empty.
        if std::fs::remove_dir(current).is_err() {
            break;
        }
        directory = current.parent();
    }
}

/// SOLO: Keeps a file from being replaced or deleted while it's held, as a
/// program that has it open does on Windows.
#[cfg(test)]
pub(crate) struct FileInUse {
    #[cfg(windows)]
    _file: std::fs::File,
    #[cfg(not(windows))]
    directory: PathBuf,
}

#[cfg(test)]
impl FileInUse {
    pub(crate) fn hold(path: &Path) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            let file = std::fs::OpenOptions::new()
                .read(true)
                // Others may read it, but not write, replace or delete it.
                .share_mode(1)
                .open(path)
                .expect("opening the file to hold");
            Self { _file: file }
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // Files in a read-only directory can't be replaced or deleted.
            let directory = path.parent().expect("a file in a directory").to_path_buf();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555))
                .expect("making the directory read-only");
            Self { directory }
        }
    }
}

#[cfg(all(test, not(windows)))]
impl Drop for FileInUse {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o755)).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git(directory: &Path, args: &[&str]) -> String {
        let output = git_command(directory)
            .args(["-c", "user.name=Solo", "-c", "user.email=solo@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn repository() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        let storage = temp.path().join("storage");
        fs::create_dir_all(repository.join("src")).unwrap();
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "core.autocrlf", "false"]);
        fs::write(repository.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(repository.join("src/b.txt"), "b\n").unwrap();
        fs::write(repository.join(".gitignore"), "ignored.txt\n").unwrap();
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);
        (temp, repository, storage)
    }

    fn sorted(mut changed: Vec<ChangedFile>) -> Vec<(String, FileChange)> {
        changed.sort_by(|a, b| a.path.cmp(&b.path));
        changed
            .into_iter()
            .map(|file| (file.path, file.change))
            .collect()
    }

    #[test]
    fn test_snapshot_leaves_the_repository_alone() {
        let (_temp, repository, storage) = repository();
        // Staged and unstaged changes, and a stash, that must survive.
        fs::write(repository.join("a.txt"), "one\ntwo\nthree\nstaged\n").unwrap();
        git(&repository, &["add", "a.txt"]);
        fs::write(repository.join("src/b.txt"), "b\nunstaged\n").unwrap();
        let head = git(&repository, &["rev-parse", "HEAD"]);
        let status = git(&repository, &["status", "--porcelain"]);
        let index_before = fs::read(repository.join(".git/index")).unwrap();
        let objects_before = git(&repository, &["count-objects", "-v"]);

        let store = SnapshotStore::open(&repository.join("src"), &storage).unwrap();
        assert!(store.is_repository());
        assert_eq!(
            store.work_tree().canonicalize().unwrap(),
            repository.canonicalize().unwrap()
        );
        let before = store.snapshot().unwrap();
        fs::write(repository.join("zz.txt"), "new\n").unwrap();
        let after = store.snapshot().unwrap();
        assert_ne!(before, after);

        assert_eq!(git(&repository, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            fs::read(repository.join(".git/index")).unwrap(),
            index_before
        );
        assert_eq!(git(&repository, &["count-objects", "-v"]), objects_before);
        assert_eq!(git(&repository, &["stash", "list"]), "");
        assert_eq!(
            git(&repository, &["status", "--porcelain"]),
            format!("{status}?? zz.txt\n")
        );
    }

    #[test]
    fn test_changed_files_and_contents() {
        let (_temp, repository, storage) = repository();
        fs::write(repository.join("untracked.txt"), "before\n").unwrap();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let before = store.snapshot().unwrap();
        assert_eq!(store.snapshot().unwrap(), before);

        fs::write(repository.join("a.txt"), "one\n2\nthree\n").unwrap();
        fs::remove_file(repository.join("src/b.txt")).unwrap();
        fs::write(repository.join("untracked.txt"), "after\n").unwrap();
        fs::create_dir_all(repository.join("new")).unwrap();
        fs::write(repository.join("new/c.txt"), "c\n").unwrap();
        fs::write(repository.join("ignored.txt"), "ignored\n").unwrap();
        let after = store.snapshot().unwrap();

        assert_eq!(
            sorted(store.changed_files(&before, &after).unwrap()),
            vec![
                ("a.txt".to_string(), FileChange::Modified),
                ("new/c.txt".to_string(), FileChange::Added),
                ("src/b.txt".to_string(), FileChange::Deleted),
                ("untracked.txt".to_string(), FileChange::Modified),
            ]
        );
        assert_eq!(
            store.file_contents(&before, "a.txt").unwrap(),
            b"one\ntwo\nthree\n"
        );
        assert_eq!(
            store.file_contents(&before, "untracked.txt").unwrap(),
            b"before\n"
        );
        assert_eq!(store.file_contents(&after, "new/c.txt").unwrap(), b"c\n");
        assert!(store.file_contents(&before, "new/c.txt").is_err());
        // All at once, with one git (R3 L-G9).
        assert_eq!(
            store
                .files_contents(&[
                    (before.clone(), "a.txt".to_string()),
                    (before.clone(), "new/c.txt".to_string()),
                    (after.clone(), "new/c.txt".to_string()),
                    (after, "x\ny".to_string()),
                    (before, "untracked.txt".to_string()),
                ])
                .unwrap(),
            vec![
                Some(b"one\ntwo\nthree\n".to_vec()),
                None,
                Some(b"c\n".to_vec()),
                None,
                Some(b"before\n".to_vec()),
            ]
        );
    }

    /// Stores are emptied when they hold too much (R3 L-G9).
    #[test]
    fn test_holds_more_than() {
        let temp = tempfile::tempdir().unwrap();
        let objects = temp.path().join("objects");
        std::fs::create_dir_all(objects.join("ab")).unwrap();
        std::fs::write(objects.join("ab").join("cd"), vec![0u8; 100]).unwrap();
        std::fs::write(objects.join("ef"), vec![0u8; 50]).unwrap();
        assert!(holds_more_than(&objects, 120));
        assert!(!holds_more_than(&objects, 150));
        assert!(!holds_more_than(&temp.path().join("missing"), 0));
    }

    #[test]
    fn test_restore_and_undo() {
        let (_temp, repository, storage) = repository();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let read = |path: &str| fs::read_to_string(repository.join(path)).ok();

        // Two turns.
        let first_turn = store.snapshot().unwrap();
        fs::write(repository.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        fs::create_dir_all(repository.join("new/deep")).unwrap();
        fs::write(repository.join("new/deep/c.txt"), "c\n").unwrap();
        let second_turn = store.snapshot().unwrap();
        fs::write(repository.join("a.txt"), "one\nTWO\nTHREE\n").unwrap();
        fs::remove_file(repository.join("src/b.txt")).unwrap();
        fs::write(repository.join("ignored.txt"), "ignored\n").unwrap();

        let restored = store.restore(&first_turn).unwrap();
        assert_eq!(read("a.txt").as_deref(), Some("one\ntwo\nthree\n"));
        assert_eq!(read("src/b.txt").as_deref(), Some("b\n"));
        assert_eq!(read("new/deep/c.txt"), None);
        assert!(!repository.join("new").exists());
        assert_eq!(read("ignored.txt").as_deref(), Some("ignored\n"));
        assert_eq!(store.snapshot().unwrap(), first_turn);
        assert_eq!(restored.changed.len(), 3);
        // The user's index and HEAD don't change: the restored files are clean.
        assert_eq!(git(&repository, &["status", "--porcelain"]), "");

        // A rollback confirmed on a state that changed since does nothing.
        let shown = store.snapshot().unwrap();
        fs::write(repository.join("a.txt"), "changed after it was shown\n").unwrap();
        let error = store
            .restore_if_unchanged(&second_turn, &shown)
            .unwrap_err();
        assert!(
            error.downcast_ref::<WorkTreeChanged>().is_some(),
            "{error:#}"
        );
        assert_eq!(
            read("a.txt").as_deref(),
            Some("changed after it was shown\n")
        );
        fs::write(repository.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        assert_eq!(store.snapshot().unwrap(), shown);

        // Undo.
        store.restore(&restored.undo).unwrap();
        assert_eq!(read("a.txt").as_deref(), Some("one\nTWO\nTHREE\n"));
        assert_eq!(read("src/b.txt"), None);
        assert_eq!(read("new/deep/c.txt").as_deref(), Some("c\n"));

        // Back to the second turn.
        store.restore(&second_turn).unwrap();
        assert_eq!(read("a.txt").as_deref(), Some("one\nTWO\nthree\n"));
        assert_eq!(read("src/b.txt").as_deref(), Some("b\n"));
    }

    /// A file in use stops restoring partway: what was restored stays so,
    /// and the error says how to undo it.
    #[test]
    fn test_restore_that_fails_partway_can_be_undone() {
        let (_temp, repository, storage) = repository();
        fs::create_dir_all(repository.join("held")).unwrap();
        fs::write(repository.join("held/h.txt"), "h\n").unwrap();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let read = |path: &str| fs::read_to_string(repository.join(path)).ok();

        let before = store.snapshot().unwrap();
        fs::write(repository.join("a.txt"), "after\n").unwrap();
        fs::write(repository.join("held/h.txt"), "after\n").unwrap();
        fs::write(repository.join("new.txt"), "new\n").unwrap();
        let after = store.snapshot().unwrap();

        let held = FileInUse::hold(&repository.join("held/h.txt"));
        let error = store.restore(&before).unwrap_err();
        let partly = error
            .downcast_ref::<PartlyRestored>()
            .unwrap_or_else(|| panic!("not partly restored: {error:#}"));
        assert_eq!(partly.undo, after);
        // The rest was restored all the same.
        assert_eq!(read("a.txt").as_deref(), Some("one\ntwo\nthree\n"));
        assert_eq!(read("new.txt"), None);
        assert_eq!(read("held/h.txt").as_deref(), Some("after\n"));

        // Once the file is free, undoing puts everything back.
        drop(held);
        store.restore(&partly.undo).unwrap();
        assert_eq!(read("a.txt").as_deref(), Some("after\n"));
        assert_eq!(read("new.txt").as_deref(), Some("new\n"));
        assert_eq!(read("held/h.txt").as_deref(), Some("after\n"));
        assert_eq!(store.snapshot().unwrap(), after);
    }

    /// A repository the agent cloned into the work tree is left alone by a
    /// rollback, which still restores the rest (R3 M3).
    #[test]
    fn test_restore_leaves_nested_repositories_alone() {
        let (_temp, repository, storage) = repository();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let read = |path: &str| fs::read_to_string(repository.join(path)).ok();
        let before = store.snapshot().unwrap();

        let nested = repository.join("vendor/lib");
        fs::create_dir_all(&nested).unwrap();
        git(&nested, &["init", "--quiet"]);
        fs::write(nested.join("lib.txt"), "lib\n").unwrap();
        git(&nested, &["add", "."]);
        git(&nested, &["commit", "--quiet", "-m", "lib"]);
        fs::write(repository.join("a.txt"), "changed\n").unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(
            sorted(store.changed_files(&before, &after).unwrap()),
            vec![("a.txt".to_string(), FileChange::Modified)]
        );

        let restored = store.restore(&before).unwrap();
        assert_eq!(restored.skipped, vec!["vendor/lib".to_string()]);
        assert_eq!(read("a.txt").as_deref(), Some("one\ntwo\nthree\n"));
        assert_eq!(read("vendor/lib/lib.txt").as_deref(), Some("lib\n"));
        store.restore(&restored.undo).unwrap();
        assert_eq!(read("a.txt").as_deref(), Some("changed\n"));
    }

    /// An agent task across repositories works in a folder of linked
    /// worktrees: what it changes in them is snapshotted, shown and rolled
    /// back like any file (R3 M3).
    #[test]
    fn test_linked_worktrees_in_a_folder_are_snapshotted() {
        let (temp, repository, storage) = repository();
        let group = temp.path().join("task");
        fs::create_dir_all(&group).unwrap();
        let worktree = group.join("repo");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "task",
                worktree.to_str().unwrap(),
            ],
        );
        fs::write(group.join("notes.txt"), "notes\n").unwrap();
        let store = SnapshotStore::open(&group, &storage).unwrap();
        assert!(!store.is_repository());
        let read = |path: &str| fs::read_to_string(group.join(path)).ok();
        let before = store.snapshot().unwrap();

        fs::write(worktree.join("a.txt"), "changed\n").unwrap();
        fs::write(worktree.join("src/new.txt"), "new\n").unwrap();
        fs::write(group.join("notes.txt"), "more notes\n").unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(
            sorted(store.changed_files(&before, &after).unwrap()),
            vec![
                ("notes.txt".to_string(), FileChange::Modified),
                ("repo/a.txt".to_string(), FileChange::Modified),
                ("repo/src/new.txt".to_string(), FileChange::Added),
            ]
        );
        assert_eq!(
            store.file_contents(&before, "repo/a.txt").unwrap(),
            b"one\ntwo\nthree\n"
        );

        let restored = store.restore(&before).unwrap();
        assert!(restored.skipped.is_empty(), "{:?}", restored.skipped);
        assert_eq!(read("repo/a.txt").as_deref(), Some("one\ntwo\nthree\n"));
        assert_eq!(read("repo/src/new.txt"), None);
        assert_eq!(read("notes.txt").as_deref(), Some("notes\n"));
        // The worktree's own repository doesn't see the rollback as changes.
        assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
        store.restore(&restored.undo).unwrap();
        assert_eq!(read("repo/a.txt").as_deref(), Some("changed\n"));
        assert_eq!(read("repo/src/new.txt").as_deref(), Some("new\n"));
    }

    /// The private index names a blob staged by the user, then unstaged and
    /// pruned: snapshots start the index over rather than fail from then on
    /// (R3 M12).
    #[test]
    fn test_snapshot_recovers_from_a_pruned_staged_blob() {
        let (_temp, repository, storage) = repository();
        fs::write(repository.join("a.txt"), "staged\n").unwrap();
        git(&repository, &["add", "a.txt"]);
        let blob = git(&repository, &["rev-parse", ":a.txt"]);
        let blob = blob.trim();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let first = store.snapshot().unwrap();

        // Unstaged, and the blob pruned from the repository.
        git(&repository, &["reset", "--quiet", "--", "a.txt"]);
        fs::remove_file(
            repository
                .join(".git/objects")
                .join(&blob[..2])
                .join(&blob[2..]),
        )
        .unwrap();
        fs::write(repository.join("src/b.txt"), "b changed\n").unwrap();

        let second = store.snapshot().unwrap();
        assert_ne!(first, second);
        assert_eq!(store.file_contents(&second, "a.txt").unwrap(), b"staged\n");
        assert_eq!(
            store.file_contents(&second, "src/b.txt").unwrap(),
            b"b changed\n"
        );
    }

    #[test]
    fn test_restore_keeps_line_endings() {
        let (_temp, repository, storage) = repository();
        git(&repository, &["config", "core.autocrlf", "true"]);
        fs::write(repository.join("crlf.txt"), "one\r\ntwo\r\n").unwrap();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let before = store.snapshot().unwrap();
        fs::write(repository.join("crlf.txt"), "one\r\nchanged\r\n").unwrap();
        assert_eq!(
            store.file_contents(&before, "crlf.txt").unwrap(),
            b"one\r\ntwo\r\n"
        );
        store.restore(&before).unwrap();
        assert_eq!(
            fs::read(repository.join("crlf.txt")).unwrap(),
            b"one\r\ntwo\r\n"
        );
    }

    /// Everything about the user's repository a snapshot or a restore must not
    /// change: branches, HEAD, index, stash and objects.
    fn repository_state(repository: &Path) -> Vec<String> {
        vec![
            git(repository, &["symbolic-ref", "HEAD"]),
            git(repository, &["for-each-ref"]),
            git(repository, &["stash", "list"]),
            git(repository, &["ls-files", "--stage"]),
            git(repository, &["count-objects", "-v"]),
            git(repository, &["worktree", "list", "--porcelain"]),
        ]
    }

    #[test]
    fn test_restore_leaves_branches_index_and_stash_alone() {
        let (_temp, repository, storage) = repository();
        git(&repository, &["branch", "other"]);
        fs::write(repository.join("a.txt"), "stashed\n").unwrap();
        git(
            &repository,
            &["stash", "push", "--quiet", "-m", "user stash"],
        );
        fs::write(repository.join("src/b.txt"), "b\nstaged\n").unwrap();
        git(&repository, &["add", "src/b.txt"]);
        let state = repository_state(&repository);
        let index = fs::read(repository.join(".git/index")).unwrap();

        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let before = store.snapshot().unwrap();
        fs::write(repository.join("a.txt"), "agent\n").unwrap();
        fs::write(repository.join("src/b.txt"), "b\nstaged\nagent\n").unwrap();
        fs::write(repository.join("new.txt"), "new\n").unwrap();
        store.snapshot().unwrap();
        let restored = store.restore(&before).unwrap();
        store.restore(&restored.undo).unwrap();
        store.restore(&before).unwrap();

        assert_eq!(repository_state(&repository), state);
        assert_eq!(fs::read(repository.join(".git/index")).unwrap(), index);
        assert_eq!(
            fs::read_to_string(repository.join("src/b.txt")).unwrap(),
            "b\nstaged\n"
        );
        assert_eq!(
            git(&repository, &["status", "--porcelain"]),
            "M  src/b.txt\n"
        );
    }

    #[test]
    fn test_restore_untracked_binary_and_large_files() {
        let (_temp, repository, storage) = repository();
        let binary: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        // Bigger than any buffer git or a pipe might use on the way.
        let large: Vec<u8> = (0..8 * 1024 * 1024u32)
            .map(|index| (index % 251) as u8)
            .collect();
        fs::write(repository.join("untracked.txt"), "untracked\n").unwrap();
        fs::write(repository.join("image.bin"), &binary).unwrap();
        fs::write(repository.join("large.bin"), &large).unwrap();
        fs::write(repository.join("lf-only.txt"), "no\r\nconversion\n").unwrap();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let before = store.snapshot().unwrap();

        fs::write(repository.join("untracked.txt"), "changed\n").unwrap();
        fs::write(repository.join("image.bin"), b"\0\0").unwrap();
        fs::remove_file(repository.join("large.bin")).unwrap();
        fs::write(repository.join("lf-only.txt"), "changed\n").unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(
            sorted(store.changed_files(&before, &after).unwrap()),
            vec![
                ("image.bin".to_string(), FileChange::Modified),
                ("large.bin".to_string(), FileChange::Deleted),
                ("lf-only.txt".to_string(), FileChange::Modified),
                ("untracked.txt".to_string(), FileChange::Modified),
            ]
        );

        let restored = store.restore(&before).unwrap();
        assert_eq!(
            fs::read_to_string(repository.join("untracked.txt")).unwrap(),
            "untracked\n"
        );
        assert_eq!(fs::read(repository.join("image.bin")).unwrap(), binary);
        assert_eq!(fs::read(repository.join("large.bin")).unwrap(), large);
        assert_eq!(
            fs::read(repository.join("lf-only.txt")).unwrap(),
            b"no\r\nconversion\n"
        );
        // Restored untracked files stay untracked.
        assert!(git(&repository, &["status", "--porcelain"]).contains("?? untracked.txt"),);

        store.restore(&restored.undo).unwrap();
        assert_eq!(fs::read(repository.join("image.bin")).unwrap(), b"\0\0");
        assert!(!repository.join("large.bin").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_restore_symlinks() {
        let (_temp, repository, storage) = repository();
        std::os::unix::fs::symlink("a.txt", repository.join("link")).unwrap();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let before = store.snapshot().unwrap();

        // The agent writes through the link, then replaces it with a file.
        fs::write(repository.join("link"), "through the link\n").unwrap();
        fs::remove_file(repository.join("link")).unwrap();
        fs::write(repository.join("link"), "a file now\n").unwrap();
        store.restore(&before).unwrap();

        let link = fs::symlink_metadata(repository.join("link")).unwrap();
        assert!(link.file_type().is_symlink());
        assert_eq!(
            fs::read_link(repository.join("link")).unwrap(),
            Path::new("a.txt")
        );
        assert_eq!(
            fs::read_to_string(repository.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
    }

    #[test]
    fn test_unused_stores_are_emptied_unless_in_use() {
        let (_temp, repository, storage) = repository();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let tree = store.snapshot().unwrap();
        assert!(store.store_dir.join(LAST_USED_FILE).exists());

        // Another Solo is snapshotting right now: nothing goes.
        let in_use = store.lock_store().unwrap();
        empty_unused_store(&store.store_dir).unwrap();
        assert!(store.store_dir.join("index").exists());
        drop(in_use);

        empty_unused_store(&store.store_dir).unwrap();
        assert!(!store.store_dir.join("index").exists());
        assert!(!store.store_dir.join("objects").exists());
        // The store works again once reopened.
        std::fs::create_dir_all(store.store_dir.join("objects")).unwrap();
        assert_eq!(store.snapshot().unwrap(), tree);
    }

    #[test]
    fn test_directory_outside_a_repository() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("plain");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("a.txt"), "a\n").unwrap();
        let store = SnapshotStore::open(&directory, &temp.path().join("storage")).unwrap();
        assert!(!store.is_repository());
        let before = store.snapshot().unwrap();
        fs::write(directory.join("a.txt"), "changed\n").unwrap();
        fs::write(directory.join("b.txt"), "b\n").unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(
            sorted(store.changed_files(&before, &after).unwrap()),
            vec![
                ("a.txt".to_string(), FileChange::Modified),
                ("b.txt".to_string(), FileChange::Added),
            ]
        );
        store.restore(&before).unwrap();
        assert_eq!(fs::read_to_string(directory.join("a.txt")).unwrap(), "a\n");
        assert!(!directory.join("b.txt").exists());
        assert!(!directory.join(".git").exists());
    }

    #[test]
    fn test_too_many_files_outside_a_repository() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("plain");
        fs::create_dir_all(&directory).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(directory.join(name), "a").unwrap();
        }
        let mut store = SnapshotStore::open(&directory, &temp.path().join("storage")).unwrap();
        store.set_max_files_outside_repository(2);
        let error = store.snapshot().unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SnapshotUnavailable>(),
            Some(SnapshotUnavailable::TooManyFiles { limit: 2, .. })
        ));
    }

    /// A junction the agent made to a folder outside the repository: git
    /// takes it for a directory, so the files through it show as added, and
    /// rolling back must not delete them (or write through it).
    #[cfg(windows)]
    #[test]
    fn test_restore_leaves_junctions_alone() {
        let (temp, repository, storage) = repository();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "keep me\n").unwrap();
        let other = temp.path().join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("secret.txt"), "other\n").unwrap();
        let junction = |link: &Path, target: &Path| {
            let status = Command::new("cmd")
                .arg("/c")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .stdout(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
        };

        let store = SnapshotStore::open(&repository, &storage).unwrap();
        let before = store.snapshot().unwrap();
        junction(&repository.join("link"), &outside);
        fs::write(repository.join("new.txt"), "new\n").unwrap();
        let after = store.snapshot().unwrap();
        // What git does with a junction (Git for Windows): it follows it.
        assert_eq!(
            sorted(store.changed_files(&before, &after).unwrap()),
            vec![
                ("link/secret.txt".to_string(), FileChange::Added),
                ("new.txt".to_string(), FileChange::Added),
            ]
        );
        assert_eq!(
            linked_ancestor(&repository.join("link").join("secret.txt"), &repository),
            Some(repository.join("link"))
        );
        assert_eq!(
            linked_ancestor(&repository.join("new.txt"), &repository),
            None
        );
        // The same repository spelled another way (case here; on CI, a
        // short 8.3 name) is no link.
        let respelled = PathBuf::from(repository.to_string_lossy().to_uppercase());
        assert_eq!(
            linked_ancestor(&respelled.join("new.txt"), &repository),
            None
        );
        assert_eq!(
            linked_ancestor(&respelled.join("link").join("secret.txt"), &repository),
            Some(respelled.join("link"))
        );

        let restored = store.restore(&before).unwrap();
        assert_eq!(restored.skipped, vec!["link/secret.txt".to_string()]);
        assert!(!repository.join("new.txt").exists());
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "keep me\n"
        );

        // The junction there before, and pointed elsewhere since: checking
        // the old contents out would overwrite the other folder's files.
        fs::remove_dir(repository.join("link")).unwrap();
        junction(&repository.join("link"), &outside);
        let with_link = store.snapshot().unwrap();
        fs::remove_dir(repository.join("link")).unwrap();
        junction(&repository.join("link"), &other);
        let restored = store.restore(&with_link).unwrap();
        assert_eq!(restored.skipped, vec!["link/secret.txt".to_string()]);
        assert_eq!(
            fs::read_to_string(other.join("secret.txt")).unwrap(),
            "other\n"
        );
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "keep me\n"
        );
        fs::remove_dir(repository.join("link")).unwrap();
    }

    #[test]
    fn test_network_directories_are_not_snapshotted() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("storage");
        let mut network = vec![Path::new("//attacker/share/repo"), Path::new("relative")];
        if cfg!(windows) {
            network.push(Path::new(r"\\attacker\share\repo"));
            network.push(Path::new(r"\\?\UNC\attacker\share\repo"));
        }
        for directory in network {
            let error = SnapshotStore::open(directory, &storage).unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<SnapshotUnavailable>(),
                    Some(SnapshotUnavailable::NotLocal(_))
                ),
                "{directory:?}: {error:#}"
            );
        }
        assert!(!storage.exists());
        assert!(is_local_directory(temp.path()));
    }

    /// Bytes that don't compress, as an archive's.
    fn noise(length: usize, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (0..length)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    /// Collects twice: the first keeps what was snapshotted since the last.
    fn collect(store: &SnapshotStore, live: &str) {
        store.collect_garbage(vec![live.to_string()]).unwrap();
        store.collect_garbage(vec![live.to_string()]).unwrap();
    }

    fn objects_size(store: &SnapshotStore) -> u64 {
        directory_size(&store.store_dir.join("objects"))
    }

    /// Large untracked files aren't snapshotted, and rolling back leaves
    /// them alone; tracked ones still are (G51).
    #[test]
    fn test_large_untracked_files_are_left_out() {
        let (_temp, repository, storage) = repository();
        let read = |path: &str| fs::read(repository.join(path)).ok();
        fs::write(repository.join("tracked.bin"), noise(1000, 1)).unwrap();
        git(&repository, &["add", "tracked.bin"]);
        git(&repository, &["commit", "--quiet", "-m", "tracked"]);
        fs::write(repository.join("grows.txt"), "small\n").unwrap();
        let mut store = SnapshotStore::open(&repository, &storage).unwrap();
        store.set_max_untracked_file_bytes(100 * 1024);
        let (before, before_large) = store.snapshot_with_large_files().unwrap();
        assert!(before_large.is_empty());
        let size_before = objects_size(&store);

        // The agent unpacks an archive, grows a small file past the limit,
        // and edits a tracked file and a small one.
        fs::create_dir_all(repository.join("out (1)")).unwrap();
        fs::write(repository.join("out (1)/archive.zip"), noise(600 * 1024, 2)).unwrap();
        fs::write(repository.join("grows.txt"), noise(200 * 1024, 3)).unwrap();
        fs::write(repository.join("tracked.bin"), noise(300 * 1024, 4)).unwrap();
        fs::write(repository.join("a.txt"), "changed\n").unwrap();
        let (after, after_large) = store.snapshot_with_large_files().unwrap();
        assert_eq!(
            after_large
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["grows.txt", "out (1)/archive.zip"]
        );
        assert_eq!(
            changed_large_files(&before_large, &after_large),
            vec!["grows.txt".to_string(), "out (1)/archive.zip".to_string()]
        );
        assert!(store.file_contents(&after, "out (1)/archive.zip").is_err());
        assert!(store.file_contents(&after, "grows.txt").is_err());
        assert_eq!(
            store.file_contents(&after, "tracked.bin").unwrap(),
            noise(300 * 1024, 4)
        );
        // The tracked file is in the store; the large ones aren't.
        let grown = objects_size(&store) - size_before;
        assert!(grown < 400 * 1024, "the store grew by {grown} bytes");
        // Unchanged, they're the same as before.
        let (again, again_large) = store.snapshot_with_large_files().unwrap();
        assert_eq!(again, after);
        assert!(changed_large_files(&after_large, &again_large).is_empty());

        // Rolling back restores the rest and leaves them as they are.
        let restored = store.restore(&before).unwrap();
        assert_eq!(
            restored.large_files,
            vec!["grows.txt".to_string(), "out (1)/archive.zip".to_string()]
        );
        assert_eq!(read("a.txt").unwrap(), b"one\ntwo\nthree\n");
        assert_eq!(read("tracked.bin").unwrap(), noise(1000, 1));
        assert_eq!(read("grows.txt").unwrap(), noise(200 * 1024, 3));
        assert_eq!(read("out (1)/archive.zip").unwrap(), noise(600 * 1024, 2));
        store.restore(&restored.undo).unwrap();
        assert_eq!(read("a.txt").unwrap(), b"changed\n");
        assert_eq!(read("tracked.bin").unwrap(), noise(300 * 1024, 4));

        // Back under the limit, a file is snapshotted again.
        fs::write(repository.join("grows.txt"), "small again\n").unwrap();
        let (small, small_large) = store.snapshot_with_large_files().unwrap();
        assert_eq!(small_large.len(), 1);
        assert_eq!(
            store.file_contents(&small, "grows.txt").unwrap(),
            b"small again\n"
        );
    }

    /// Outside a repository every file is untracked (G51).
    #[test]
    fn test_large_files_outside_a_repository_are_left_out() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("plain");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("a.txt"), "a\n").unwrap();
        fs::write(directory.join("big.bin"), noise(200 * 1024, 5)).unwrap();
        let mut store = SnapshotStore::open(&directory, &temp.path().join("storage")).unwrap();
        store.set_max_untracked_file_bytes(100 * 1024);
        let (tree, large) = store.snapshot_with_large_files().unwrap();
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].path, "big.bin");
        assert_eq!(store.file_contents(&tree, "a.txt").unwrap(), b"a\n");
        assert!(store.file_contents(&tree, "big.bin").is_err());
    }

    /// Objects that no kept tree needs are removed; the store stops growing
    /// however many snapshots are taken (G51).
    #[test]
    fn test_collect_garbage_keeps_only_live_trees() {
        let (_temp, repository, storage) = repository();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        const SIZE: usize = 64 * 1024;
        let mut trees = Vec::new();
        let mut sizes = Vec::new();
        for turn in 0..25u64 {
            fs::write(repository.join("data.bin"), noise(SIZE, 100 + turn)).unwrap();
            trees.push(store.snapshot().unwrap());
            // The last 5 are kept, as turns would be.
            let live: Vec<TreeId> = trees.iter().rev().take(5).cloned().collect();
            store.collect_garbage(live).unwrap();
            sizes.push(objects_size(&store));
        }
        // 5 kept, the index's tree (the last), and slack for the packs.
        let last = *sizes.last().unwrap();
        assert!(last < 6 * SIZE as u64 + 64 * 1024, "{sizes:?}");
        assert!(last <= sizes[10] + 8 * 1024, "{sizes:?}");
        for (ix, tree) in trees.iter().enumerate() {
            let contents = store.file_contents(tree, "data.bin");
            if ix >= 20 {
                assert_eq!(contents.unwrap(), noise(SIZE, 100 + ix as u64));
            } else {
                assert!(contents.is_err(), "turn {ix}'s contents are kept");
            }
        }
        // Files the repository has stay borrowed, not copied.
        assert_eq!(
            store.file_contents(&trees[24], "a.txt").unwrap(),
            b"one\ntwo\nthree\n"
        );
        // The index still works: unchanged files aren't hashed again.
        assert_eq!(store.snapshot().unwrap(), trees[24]);

        // A pinned tree is kept until unpinned.
        fs::write(repository.join("data.bin"), "pinned\n").unwrap();
        let pinned_tree = store.snapshot().unwrap();
        fs::write(repository.join("data.bin"), "later\n").unwrap();
        let later = store.snapshot().unwrap();
        let pin = store.pin(&pinned_tree);
        collect(&store, &later);
        assert_eq!(
            store.file_contents(&pinned_tree, "data.bin").unwrap(),
            b"pinned\n"
        );
        drop(pin);
        collect(&store, &later);
        assert!(store.file_contents(&pinned_tree, "data.bin").is_err());
        assert_eq!(store.file_contents(&later, "data.bin").unwrap(), b"later\n");
    }

    /// Trees another Solo keeps are kept while it runs (G51).
    #[test]
    fn test_collect_garbage_keeps_other_processes_trees() {
        let (_temp, repository, storage) = repository();
        let store = SnapshotStore::open(&repository, &storage).unwrap();
        fs::write(repository.join("theirs.txt"), "theirs\n").unwrap();
        let theirs = store.snapshot().unwrap();
        fs::write(repository.join("theirs.txt"), "ours\n").unwrap();
        let ours = store.snapshot().unwrap();
        collect(&store, &ours);
        assert!(store.file_contents(&theirs, "theirs.txt").is_err());

        fs::write(repository.join("theirs.txt"), "theirs\n").unwrap();
        assert_eq!(store.snapshot().unwrap(), theirs);
        fs::write(repository.join("theirs.txt"), "ours\n").unwrap();
        store.snapshot().unwrap();
        // Another Solo, holding its lock, keeps `theirs`.
        let live = store.store_dir.join(LIVE_DIR);
        fs::write(live.join("4000000001.trees"), format!("{theirs}\n")).unwrap();
        let other = fs::File::create(live.join("4000000001.lock")).unwrap();
        other.lock().unwrap();
        // It counts as using the store, which isn't emptied then.
        assert!(in_use_elsewhere(&store.store_dir));
        collect(&store, &ours);
        assert_eq!(
            store.file_contents(&theirs, "theirs.txt").unwrap(),
            b"theirs\n"
        );

        // Once it's gone, its files go and its trees aren't kept.
        drop(other);
        collect(&store, &ours);
        assert!(store.file_contents(&theirs, "theirs.txt").is_err());
        assert!(!live.join("4000000001.lock").exists());
        assert!(!live.join("4000000001.trees").exists());
        assert_eq!(store.file_contents(&ours, "theirs.txt").unwrap(), b"ours\n");
    }

    /// Past the total, the least recently used stores are emptied (G51).
    #[test]
    fn test_trim_stores_empties_the_least_recently_used() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let now = SystemTime::now();
        for (name, age_hours) in [("old", 48u64), ("middle", 24), ("new", 1)] {
            let store = root.join(name);
            fs::create_dir_all(store.join("objects").join("ab")).unwrap();
            fs::write(store.join("objects").join("ab").join("cd"), vec![0u8; 1000]).unwrap();
            let used = fs::File::create(store.join(LAST_USED_FILE)).unwrap();
            used.set_modified(now - Duration::from_secs(age_hours * 3600))
                .unwrap();
        }
        trim_stores(root, 2500);
        assert!(!root.join("old").join("objects").exists());
        assert!(root.join("middle").join("objects").exists());
        assert!(root.join("new").join("objects").exists());
        trim_stores(root, 5000);
        assert!(root.join("middle").join("objects").exists());
    }

    #[test]
    fn test_parse_raw_diff() {
        let zero = "0".repeat(40);
        let id = "1".repeat(40);
        let output = format!(
            ":100644 100644 {id} {id} M\0a b.txt\0\
             :000000 100644 {zero} {id} A\0new\0\
             :100644 000000 {id} {zero} D\0gone\0\
             :100644 120000 {id} {id} T\0link\0\
             :000000 160000 {zero} {id} A\0nested\0"
        );
        let mut output = output.into_bytes();
        // A name that isn't UTF-8 is left alone (R3 L-G6).
        output.extend(format!(":100644 100644 {id} {id} M\0").as_bytes());
        output.extend(b"caf\xe9.txt\0");
        let (changed, gitlinks) = parse_raw_diff(&output);
        assert_eq!(
            gitlinks,
            vec!["nested".to_string(), "caf\u{fffd}.txt".to_string()]
        );
        assert_eq!(
            changed,
            vec![
                ChangedFile {
                    path: "a b.txt".into(),
                    change: FileChange::Modified
                },
                ChangedFile {
                    path: "new".into(),
                    change: FileChange::Added
                },
                ChangedFile {
                    path: "gone".into(),
                    change: FileChange::Deleted
                },
                ChangedFile {
                    path: "link".into(),
                    change: FileChange::Modified
                },
            ]
        );
    }
}
