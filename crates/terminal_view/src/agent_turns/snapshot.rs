//! SOLO: snapshots of an agent's working directory, taken with git without
//! touching the user's branch, HEAD, index, stash list or object database.
//!
//! The work tree is added to a private index (seeded from the user's index,
//! so only changed and untracked files get hashed) and written as a tree to a
//! private object directory that borrows the repository's objects as
//! alternates. Directories that aren't in a repository get a private bare
//! repository as their git dir. Ignored files aren't part of a snapshot.
//!
//! Everything here blocks on git; run it in the background.
#![allow(
    clippy::disallowed_methods,
    reason = "runs git synchronously, only from background tasks"
)]

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, anyhow, bail};

/// A tree object id.
pub(crate) type TreeId = String;

/// Directories that aren't repositories are only snapshotted up to this many
/// files: a home directory or a drive isn't a project.
const MAX_FILES_OUTSIDE_REPOSITORY: usize = 20_000;
/// A store nobody snapshotted with for this long is emptied the first time a
/// process opens it.
const MAX_STORE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Touched by every snapshot: when the store was last used, by any Solo.
const LAST_USED_FILE: &str = "last-used";
/// Locked around git commands on a store, which Solos running side by side
/// (other channels, before and after an update) share.
const LOCK_FILE: &str = "lock";

/// Git commands that share a store's index run one at a time, also within
/// this process (file locks don't exclude threads holding other handles).
static GIT_LOCK: Mutex<()> = Mutex::new(());
/// Stores opened by this process, which keep their objects.
static OPENED_STORES: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

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

#[derive(Debug)]
pub(crate) struct Restored {
    /// The state before restoring, to undo it.
    pub undo: TreeId,
    pub changed: Vec<ChangedFile>,
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
}

impl SnapshotStore {
    /// The store for the repository containing `directory`, or for
    /// `directory` itself when it isn't in one. Stores live in `storage_root`.
    pub(crate) fn open(directory: &Path, storage_root: &Path) -> Result<Self> {
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
                    bail!("{work_tree:?} is too broad to snapshot");
                }
                (work_tree, PathBuf::new(), None, None)
            }
        };

        let store_dir = storage_root.join(store_name(&work_tree));
        let first_open = OPENED_STORES
            .lock()
            .map_err(|_| anyhow!("snapshot stores lock poisoned"))?
            .insert(store_dir.clone());
        std::fs::create_dir_all(&store_dir).with_context(|| format!("creating {store_dir:?}"))?;
        if first_open && is_older_than(&store_dir.join(LAST_USED_FILE), MAX_STORE_AGE) {
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

        Ok(Self {
            work_tree,
            git_dir,
            alternate_objects,
            seed_index,
            store_dir,
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

    fn snapshot_locked(&self) -> Result<TreeId> {
        if let Err(error) = std::fs::write(self.store_dir.join(LAST_USED_FILE), b"") {
            log::warn!("failed to mark {:?} as used: {error}", self.store_dir);
        }
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
                if count > MAX_FILES_OUTSIDE_REPOSITORY {
                    bail!(
                        "{:?} has more than {MAX_FILES_OUTSIDE_REPOSITORY} files",
                        self.work_tree
                    );
                }
            }
        }
        // Files that can't be read (locked, say) are left as they were in the
        // last snapshot rather than failing it.
        let output = self
            .command(&index)
            .args(["add", "--all", "--ignore-errors", "--", "."])
            .output()
            .context("running git add")?;
        if !output.status.success() {
            log::warn!(
                "git add for a snapshot of {:?} reported: {}",
                self.work_tree,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let tree = run(self.command(&index).arg("write-tree"))?;
        Ok(String::from_utf8_lossy(&tree).trim().to_string())
    }

    /// Files that differ between two snapshots.
    pub(crate) fn changed_files(&self, from: &str, to: &str) -> Result<Vec<ChangedFile>> {
        if from == to {
            return Ok(Vec::new());
        }
        let output = run(self.command(&self.index_path()).args([
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-status",
            from,
            to,
        ]))?;
        Ok(parse_name_status(&output))
    }

    /// A file's contents in a snapshot, as it would be checked out.
    pub(crate) fn file_contents(&self, tree: &str, path: &str) -> Result<Vec<u8>> {
        run(self
            .command(&self.index_path())
            .args(["cat-file", "--filters"])
            .arg(format!("{tree}:{path}")))
    }

    /// Puts the work tree back as it was in `target`: changed files get their
    /// contents back, and files that weren't there are removed. The current
    /// state is snapshotted first, to undo it.
    pub(crate) fn restore(&self, target: &str) -> Result<Restored> {
        let _lock = GIT_LOCK
            .lock()
            .map_err(|_| anyhow!("snapshot lock poisoned"))?;
        let _store_lock = self.lock_store()?;
        let current = self.snapshot_locked()?;
        let changed = self.changed_files(target, &current)?;

        let to_check_out: Vec<&str> = changed
            .iter()
            .filter(|file| file.change != FileChange::Added)
            .map(|file| file.path.as_str())
            .collect();
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
            result?;
        }

        for file in changed
            .iter()
            .filter(|file| file.change == FileChange::Added)
        {
            let path = self.abs_path(&file.path);
            match std::fs::remove_file(&path) {
                Ok(()) => remove_empty_parents(&path, &self.work_tree),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("removing {path:?}"));
                }
            }
        }

        Ok(Restored {
            undo: current,
            changed,
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

/// Parses `git diff-tree -z --name-status` output.
fn parse_name_status(output: &[u8]) -> Vec<ChangedFile> {
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut changed = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        let change = match status.first() {
            Some(b'A') => FileChange::Added,
            Some(b'D') => FileChange::Deleted,
            _ => FileChange::Modified,
        };
        changed.push(ChangedFile {
            path: String::from_utf8_lossy(path).into_owned(),
            change,
        });
    }
    changed
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
    if lock.try_lock().is_err() {
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

fn is_older_than(path: &Path, age: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|created| SystemTime::now().duration_since(created).ok())
        .is_some_and(|elapsed| elapsed > age)
}

/// Removes the directories left empty by removing `path`, up to `root`.
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
    fn test_parse_name_status() {
        assert_eq!(
            parse_name_status(b"M\0a b.txt\0A\0new\0D\0gone\0T\0link\0"),
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
