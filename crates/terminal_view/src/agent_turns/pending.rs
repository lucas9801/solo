//! SOLO: the text side of pending changes: what the user last accepted of a
//! file an agent changed, against the file now (G59).
//!
//! The rules come from the agent panel's action log
//! (`crates/action_log/src/action_log.rs`): someone else's edits that don't
//! touch the pending ones are taken into the accepted version
//! (`apply_non_conflicting_edits`), and pending edits the user committed are
//! accepted (`keep_committed_edits`). Here they work on whole texts, by line.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::snapshot::{ChangedFile, SnapshotStore, TreeId};

/// A version of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Version {
    /// There's no file.
    Absent,
    /// Text, normalized (see [`normalized_text`]).
    Text(Arc<str>),
    /// Not text (or too big to show): by its blob's id.
    Binary(String),
}

impl Version {
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Version::Text(text) => Some(text),
            Version::Absent => Some(""),
            Version::Binary(_) => None,
        }
    }

    /// A version from a file's contents.
    pub(crate) fn from_bytes(bytes: Vec<u8>, blob: impl FnOnce() -> Option<String>) -> Version {
        match String::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => Version::Text(normalized_text(text).into()),
            _ => match blob() {
                Some(blob) => Version::Binary(blob),
                None => Version::Binary(String::new()),
            },
        }
    }
}

/// What a terminal's agent changed in a file that the user hasn't accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingFile {
    /// What the user last accepted.
    pub accepted: Version,
    /// The file as the agent, or the review, last left it: what changed
    /// from there on is someone else's.
    pub left: Version,
    /// The first turn that changed it since it was accepted: `0` for one of
    /// an earlier session.
    pub first_turn: usize,
    /// When that turn started, if known.
    pub since: Option<SystemTime>,
    /// What changed while Solo was closed, in a terminal that kept running:
    /// the file as it was last recorded, and as Solo found it again. Its
    /// agent's, as far as can be told (G59).
    pub while_closed: Option<(Version, Version)>,
}

/// The turn number that stands for "while Solo was closed".
pub(crate) const WHILE_CLOSED: usize = usize::MAX;

impl PendingFile {
    /// Whether it has nothing pending as it was last left.
    pub(crate) fn is_accepted(&self) -> bool {
        self.accepted == self.left
    }
}

/// `accepted` with someone else's edits from `left` to `current` that don't
/// touch the pending ones (see [`apply_non_conflicting`]).
pub(crate) fn rebase(accepted: &Version, left: &Version, current: &Version) -> Version {
    if left == current {
        return accepted.clone();
    }
    if accepted == left {
        return current.clone();
    }
    match (accepted.text(), left.text(), current.text()) {
        (Some(accepted_text), Some(left), Some(current_text)) => {
            let rebased = apply_non_conflicting(accepted_text, left, current_text);
            text_version(rebased, accepted, current)
        }
        _ => accepted.clone(),
    }
}

/// [`keep_committed`] of versions; `committed` is `None` outside a
/// repository.
pub(crate) fn keep_committed_version(
    accepted: &Version,
    current: &Version,
    committed: Option<&Version>,
) -> Version {
    let Some(committed) = committed else {
        return accepted.clone();
    };
    if accepted == current || committed == accepted {
        return accepted.clone();
    }
    if committed == current {
        return current.clone();
    }
    match (accepted.text(), current.text(), committed.text()) {
        (Some(accepted_text), Some(current_text), Some(committed_text))
            if !matches!(accepted, Version::Absent) && !matches!(current, Version::Absent) =>
        {
            text_version(
                keep_committed(accepted_text, current_text, committed_text),
                accepted,
                current,
            )
        }
        _ => accepted.clone(),
    }
}

/// [`auto_keep`] of versions; `turn` is the turn's before and after, if it
/// changed the file.
pub(crate) fn auto_keep_version(
    accepted: &Version,
    current: &Version,
    turn: Option<(&Version, &Version)>,
) -> Version {
    let Some((before, after)) = turn else {
        return current.clone();
    };
    match (accepted.text(), current.text(), before.text(), after.text()) {
        (Some(accepted_text), Some(current_text), Some(before), Some(after))
            if !matches!(accepted, Version::Absent) && !matches!(current, Version::Absent) =>
        {
            text_version(
                auto_keep(accepted_text, current_text, Some((before, after))),
                accepted,
                current,
            )
        }
        // A created or deleted file is one edit, which the turn touched.
        _ => accepted.clone(),
    }
}

/// The version of a merged text: `accepted` or `current` when it's one of
/// them, so an absent file stays absent.
fn text_version(text: String, accepted: &Version, current: &Version) -> Version {
    if accepted.text() == Some(text.as_str()) {
        accepted.clone()
    } else if current.text() == Some(text.as_str()) {
        current.clone()
    } else {
        Version::Text(text.into())
    }
}

/// The versions of the objects named (`<tree>:<path>`, `HEAD:<path>`).
pub(crate) fn load_versions(store: &SnapshotStore, objects: &[String]) -> Result<Vec<Version>> {
    let ids = store.object_ids(objects)?;
    let present: Vec<String> = objects
        .iter()
        .zip(&ids)
        .filter(|(_, id)| id.is_some())
        .map(|(object, _)| object.clone())
        .collect();
    let mut contents = store.objects_contents(&present)?.into_iter();
    Ok(ids
        .into_iter()
        .map(|id| match id {
            None => Version::Absent,
            Some(id) => match contents.next().flatten() {
                Some(bytes) => Version::from_bytes(bytes, || Some(id)),
                None => Version::Binary(id),
            },
        })
        .collect())
}

/// The version of the file at `abs_path` now.
pub(crate) fn current_version(store: &SnapshotStore, abs_path: &Path) -> Version {
    match std::fs::read(abs_path) {
        Ok(bytes) => Version::from_bytes(bytes, || {
            store
                .hash_files(&[abs_path.to_path_buf()])
                .pop()
                .flatten()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Version::Absent,
        // Can't be read now (in use, say): unknown, so not something else.
        Err(_) => Version::Binary(String::new()),
    }
}

/// What a turn's end does to a terminal's pending changes.
pub(crate) struct TurnEnd {
    pub store: Arc<SnapshotStore>,
    pub before: TreeId,
    pub after: TreeId,
    pub number: usize,
    pub started_at: SystemTime,
    /// The files the agent changed in the turn.
    pub changed: Vec<ChangedFile>,
    /// Whether what earlier turns left pending is accepted now (see
    /// [`auto_keep`]): not while the review is open.
    pub auto_keep: bool,
}

/// The pending changes after `turn`: the files it changed are pending (from
/// before it, if they weren't); what changed between turns and doesn't
/// touch what's pending, and what was committed, is accepted; and with
/// `auto_keep`, what only earlier turns changed.
pub(crate) fn end_turn(
    files: &BTreeMap<String, PendingFile>,
    turn: &TurnEnd,
) -> Result<BTreeMap<String, PendingFile>> {
    let mut paths: Vec<&str> = files.keys().map(String::as_str).collect();
    for file in &turn.changed {
        if !files.contains_key(&file.path) {
            paths.push(&file.path);
        }
    }
    let repository = turn.store.is_repository();
    let mut objects = Vec::new();
    for path in &paths {
        objects.push(format!("{}:{path}", turn.before));
        objects.push(format!("{}:{path}", turn.after));
        if repository {
            objects.push(format!("HEAD:{path}"));
        }
    }
    let mut versions = load_versions(&turn.store, &objects)
        .context("reading the turn's files")?
        .into_iter();
    let mut result = BTreeMap::new();
    for path in paths {
        let (Some(before), Some(after)) = (versions.next(), versions.next()) else {
            break;
        };
        let committed = if repository { versions.next() } else { None };
        let changed = turn.changed.iter().any(|file| file.path == path);
        let mut file = match files.get(path) {
            Some(file) if changed => PendingFile {
                accepted: rebase(&file.accepted, &file.left, &before),
                left: after.clone(),
                ..file.clone()
            },
            Some(file) => PendingFile {
                accepted: rebase(&file.accepted, &file.left, &after),
                left: after.clone(),
                ..file.clone()
            },
            None => PendingFile {
                accepted: before.clone(),
                left: after.clone(),
                first_turn: turn.number,
                since: Some(turn.started_at),
                while_closed: None,
            },
        };
        file.accepted = keep_committed_version(&file.accepted, &after, committed.as_ref());
        if turn.auto_keep {
            file.accepted =
                auto_keep_version(&file.accepted, &after, changed.then_some((&before, &after)));
        }
        if !file.is_accepted() {
            result.insert(path.to_string(), file);
        }
    }
    Ok(result)
}

/// The pending changes with, if `take_in`, what the user did since they were
/// last left (on disk): edits that don't touch pending ones, and those
/// committed, are accepted. `locked` files (the agent is changing them) are
/// left as they are. With them, the files that differ from what's accepted.
pub(crate) fn reconcile(
    store: &SnapshotStore,
    files: &BTreeMap<String, PendingFile>,
    locked: &BTreeSet<String>,
    take_in: bool,
) -> (BTreeMap<String, PendingFile>, BTreeSet<String>) {
    let repository = store.is_repository();
    let paths: Vec<&String> = files.keys().collect();
    let committed: Vec<Version> = if repository && take_in {
        let objects: Vec<String> = paths.iter().map(|path| format!("HEAD:{path}")).collect();
        load_versions(store, &objects).unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut result = BTreeMap::new();
    let mut pending = BTreeSet::new();
    for (ix, path) in paths.into_iter().enumerate() {
        let file = &files[path];
        if locked.contains(path) {
            pending.insert(path.clone());
            result.insert(path.clone(), file.clone());
            continue;
        }
        let current = current_version(store, &store.abs_path(path));
        if !take_in {
            if file.accepted != current {
                pending.insert(path.clone());
            }
            result.insert(path.clone(), file.clone());
            continue;
        }
        let accepted = rebase(&file.accepted, &file.left, &current);
        let accepted = keep_committed_version(&accepted, &current, committed.get(ix));
        let file = PendingFile {
            accepted,
            left: current.clone(),
            ..file.clone()
        };
        if file.accepted != current {
            pending.insert(path.clone());
        }
        if !file.is_accepted() {
            result.insert(path.clone(), file);
        }
    }
    (result, pending)
}

/// The pending changes with what only turns before the last finished one
/// changed accepted, as that turn's end does (see [`end_turn`]); `last` is
/// that turn's before and after and the files it changed, if it changed
/// any. What the user did since the files were last left is taken in
/// first. `locked` files are left as they are.
pub(crate) fn auto_keep_now(
    store: &SnapshotStore,
    files: &BTreeMap<String, PendingFile>,
    locked: &BTreeSet<String>,
    last: Option<&(TreeId, TreeId, Vec<ChangedFile>)>,
) -> Result<BTreeMap<String, PendingFile>> {
    let paths: Vec<&String> = files.keys().collect();
    let mut objects = Vec::new();
    if let Some((before, after, _)) = last {
        for path in &paths {
            objects.push(format!("{before}:{path}"));
            objects.push(format!("{after}:{path}"));
        }
    }
    let mut turn_versions = load_versions(store, &objects)?.into_iter();
    let mut result = BTreeMap::new();
    for path in paths {
        let file = &files[path];
        let turn = last.map(|_| (turn_versions.next(), turn_versions.next()));
        if locked.contains(path) {
            result.insert(path.clone(), file.clone());
            continue;
        }
        let current = current_version(store, &store.abs_path(path));
        let accepted = rebase(&file.accepted, &file.left, &current);
        let changed_by_last =
            last.is_some_and(|(_, _, changed)| changed.iter().any(|file| file.path == *path));
        let accepted = match turn {
            Some((Some(before), Some(after))) if changed_by_last => {
                auto_keep_version(&accepted, &current, Some((&before, &after)))
            }
            _ => auto_keep_version(&accepted, &current, None),
        };
        let file = PendingFile {
            accepted,
            left: current,
            ..file.clone()
        };
        if !file.is_accepted() {
            result.insert(path.clone(), file);
        }
    }
    Ok(result)
}

/// How a terminal's pending changes are recorded, to be there after a
/// restart (in the snapshot store; see `SnapshotStore::lock`).
#[derive(Serialize, Deserialize)]
pub(crate) struct Record {
    pub version: u32,
    /// Where the agent works.
    pub directory: PathBuf,
    /// Trees with the blobs below, which keep them.
    pub trees: Vec<TreeId>,
    pub files: Vec<RecordedFile>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct RecordedFile {
    pub path: String,
    pub accepted: Option<RecordedVersion>,
    pub left: Option<RecordedVersion>,
    /// Milliseconds since the epoch.
    pub since: Option<u64>,
    /// What changed while Solo was closed: from, and to (absent files are
    /// `None`).
    #[serde(default)]
    pub while_closed: Option<(Option<RecordedVersion>, Option<RecordedVersion>)>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct RecordedVersion {
    pub blob: String,
    #[serde(default)]
    pub binary: bool,
}

/// Records `files` for the terminal `key`, or forgets it when there are
/// none. Returns the trees the record keeps.
pub(crate) fn save(
    store: &SnapshotStore,
    key: &str,
    directory: &Path,
    files: &BTreeMap<String, PendingFile>,
) -> Result<Vec<TreeId>> {
    let lock = store.lock()?;
    if files.is_empty() {
        lock.remove_record(key);
        return Ok(Vec::new());
    }
    // Each file's versions: accepted, left, and from and to while closed.
    let versions_of = |file: &PendingFile| -> [Option<Version>; 4] {
        let (from, to) = match &file.while_closed {
            Some((from, to)) => (Some(from.clone()), Some(to.clone())),
            None => (None, None),
        };
        [Some(file.accepted.clone()), Some(file.left.clone()), from, to]
    };
    let all: Vec<[Option<Version>; 4]> = files.values().map(versions_of).collect();
    let texts: Vec<&[u8]> = all
        .iter()
        .flatten()
        .flatten()
        .filter_map(|version| match version {
            Version::Text(text) => Some(text.as_bytes()),
            _ => None,
        })
        .collect();
    let mut blobs = lock.write_blobs(&texts)?.into_iter();
    let mut entries: [Vec<(String, String)>; 4] = Default::default();
    let mut recorded = Vec::new();
    for ((path, file), versions) in files.iter().zip(&all) {
        let mut out: [Option<RecordedVersion>; 4] = Default::default();
        for (kind, version) in versions.iter().enumerate() {
            let Some(version) = version else {
                continue;
            };
            let (blob, binary) = match version {
                Version::Absent => continue,
                Version::Text(_) => (blobs.next().context("a blob is missing")?, false),
                Version::Binary(blob) if blob.is_empty() => continue,
                Version::Binary(blob) => (blob.clone(), true),
            };
            entries[kind].push((path.clone(), blob.clone()));
            out[kind] = Some(RecordedVersion { blob, binary });
        }
        let [accepted, left, from, to] = out;
        recorded.push(RecordedFile {
            path: path.clone(),
            accepted,
            left,
            since: file.since.and_then(|since| {
                since
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .ok()
                    .map(|since| since.as_millis() as u64)
            }),
            while_closed: file.while_closed.as_ref().map(|_| (from, to)),
        });
    }
    let mut trees = Vec::new();
    for kind in &entries {
        if !kind.is_empty() {
            trees.push(lock.write_tree(kind)?);
        }
    }
    let record = Record {
        version: 1,
        directory: directory.to_path_buf(),
        trees: trees.clone(),
        files: recorded,
    };
    lock.write_record(key, &serde_json::to_vec(&record)?)?;
    Ok(trees)
}

/// The pending changes a record kept, with their versions read back.
pub(crate) fn load(
    store: &SnapshotStore,
    record: &Record,
) -> Result<BTreeMap<String, PendingFile>> {
    let recorded = |file: &RecordedFile| -> Vec<Option<RecordedVersion>> {
        let clone = |version: &Option<RecordedVersion>| {
            version.as_ref().map(|version| RecordedVersion {
                blob: version.blob.clone(),
                binary: version.binary,
            })
        };
        let mut versions = vec![clone(&file.accepted), clone(&file.left)];
        if let Some((from, to)) = &file.while_closed {
            versions.push(clone(from));
            versions.push(clone(to));
        }
        versions
    };
    let mut objects = Vec::new();
    for file in &record.files {
        for version in recorded(file).into_iter().flatten() {
            objects.push(version.blob);
        }
    }
    let mut contents = store.objects_contents(&objects)?.into_iter();
    let mut read = |version: &Option<RecordedVersion>| -> Result<Version> {
        let Some(version) = version else {
            return Ok(Version::Absent);
        };
        let bytes = contents.next().flatten();
        if version.binary {
            return Ok(Version::Binary(version.blob.clone()));
        }
        let bytes = bytes.with_context(|| format!("the blob {} is gone", version.blob))?;
        Ok(Version::from_bytes(bytes, || Some(version.blob.clone())))
    };
    let mut files = BTreeMap::new();
    for file in &record.files {
        let versions = recorded(file);
        let accepted = read(&versions[0])?;
        let left = read(&versions[1])?;
        let while_closed = if versions.len() == 4 {
            Some((read(&versions[2])?, read(&versions[3])?))
        } else {
            None
        };
        files.insert(
            file.path.clone(),
            PendingFile {
                accepted,
                left,
                first_turn: 0,
                since: file
                    .since
                    .map(|since| SystemTime::UNIX_EPOCH + Duration::from_millis(since)),
                while_closed,
            },
        );
    }
    Ok(files)
}

/// The pending changes restored after Solo was closed, with what changed on
/// disk meanwhile: in a terminal that kept running, that's its agent's, so
/// it's pending (not taken in as the user's) and marked as changed while
/// Solo was closed.
pub(crate) fn changed_while_closed(
    store: &SnapshotStore,
    files: BTreeMap<String, PendingFile>,
) -> BTreeMap<String, PendingFile> {
    files
        .into_iter()
        .map(|(path, mut file)| {
            let current = current_version(store, &store.abs_path(&path));
            if current != file.left {
                file.while_closed = Some((file.left.clone(), current.clone()));
                file.left = current;
            }
            (path, file)
        })
        .collect()
}
/// Rows `old` of one text became rows `new` of another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LineEdit {
    pub old: Range<u32>,
    pub new: Range<u32>,
}

impl LineEdit {
    fn old_len(&self) -> i64 {
        i64::from(self.old.end - self.old.start)
    }

    fn new_len(&self) -> i64 {
        i64::from(self.new.end - self.new.start)
    }
}

/// The line edits that turn `old` into `new`.
pub(crate) fn line_edits(old: &str, new: &str) -> Vec<LineEdit> {
    if old == new {
        return Vec::new();
    }
    language::line_diff(old, new)
        .into_iter()
        .map(|(old, new)| LineEdit { old, new })
        .collect()
}

/// Lines with their line breaks, as rows count them.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

fn rows<'a>(lines: &[&'a str], rows: &Range<u32>) -> Vec<&'a str> {
    let start = (rows.start as usize).min(lines.len());
    let end = (rows.end as usize).clamp(start, lines.len());
    lines[start..end].to_vec()
}

/// Text as a buffer holds it: without a byte order mark, with `\n` line
/// endings.
pub(crate) fn normalized_text(text: String) -> String {
    let mut text = match text.strip_prefix('\u{feff}') {
        Some(text) => text.to_string(),
        None => text,
    };
    language::LineEnding::normalize(&mut text);
    text
}

/// Whether two row ranges touch: they overlap, or an empty one (a point
/// where lines were removed or will be inserted) is at or inside the other.
pub(crate) fn touches(left: &Range<u32>, right: &Range<u32>) -> bool {
    if left.is_empty() || right.is_empty() {
        let (point, range) = if left.is_empty() {
            (left.start, right)
        } else {
            (right.start, left)
        };
        return range.start <= point && point <= range.end;
    }
    left.start < right.end && right.start < left.end
}

/// Where rows `range` of the old text are in the new one, through `edits`
/// (old → new). Edits that touch the range widen it to all they became.
fn map_forward(range: &Range<u32>, edits: &[LineEdit]) -> Range<u32> {
    let shift = |row: u32| -> u32 {
        let delta: i64 = edits
            .iter()
            .filter(|edit| edit.old.end <= row && !touches(&edit.old, range))
            .map(|edit| edit.new_len() - edit.old_len())
            .sum();
        (i64::from(row) + delta).max(0) as u32
    };
    let mut start = shift(range.start);
    let mut end = shift(range.end).max(start);
    for edit in edits.iter().filter(|edit| touches(&edit.old, range)) {
        start = start.min(edit.new.start);
        end = end.max(edit.new.end);
    }
    start..end
}

/// [`map_forward`] the other way: where rows of the new text were in the old.
fn map_backward(range: &Range<u32>, edits: &[LineEdit]) -> Range<u32> {
    let reversed: Vec<LineEdit> = edits
        .iter()
        .map(|edit| LineEdit {
            old: edit.new.clone(),
            new: edit.old.clone(),
        })
        .collect();
    map_forward(range, &reversed)
}

/// `accepted` with the edits someone else made from `left` to `current`
/// that don't touch the pending ones (`accepted` → `left`): they aren't for
/// the user to review (see the action log's `apply_non_conflicting_edits`).
pub(crate) fn apply_non_conflicting(accepted: &str, left: &str, current: &str) -> String {
    if left == current {
        return accepted.to_string();
    }
    if accepted == left {
        return current.to_string();
    }
    let pending = line_edits(accepted, left);
    let others = line_edits(left, current);
    let mut result: Vec<&str> = lines(accepted);
    let new_lines = lines(current);
    let mut pending_ix = 0;
    let mut other_ix = 0;
    let mut applied_delta = 0i64;
    let mut rebased_delta = 0i64;
    while other_ix < others.len() {
        let mut other = others[other_ix].clone();
        other_ix += 1;
        let mut conflict = false;
        // Past the pending edits before this one, or that it touches.
        while let Some(pending_edit) = pending.get(pending_ix) {
            if other.old.end < pending_edit.new.start
                || (!pending_edit.new.is_empty() && other.old.end == pending_edit.new.start)
            {
                break;
            } else if other.old.start > pending_edit.new.end
                || (!pending_edit.new.is_empty() && other.old.start == pending_edit.new.end)
            {
                rebased_delta += pending_edit.new_len() - pending_edit.old_len();
                pending_ix += 1;
            } else {
                conflict = true;
                if others.get(other_ix).is_some_and(|next| {
                    next.old.start < pending_edit.new.end && pending_edit.new.start < next.old.end
                }) {
                    other = others[other_ix].clone();
                    other_ix += 1;
                } else {
                    rebased_delta += pending_edit.new_len() - pending_edit.old_len();
                    pending_ix += 1;
                }
            }
        }
        if !conflict {
            let start = (i64::from(other.old.start) + applied_delta - rebased_delta).max(0) as usize;
            let end = (i64::from(other.old.end) + applied_delta - rebased_delta).max(0) as usize;
            let start = start.min(result.len());
            let end = end.clamp(start, result.len());
            result.splice(start..end, rows(&new_lines, &other.new));
            applied_delta += other.new_len() - other.old_len();
        }
    }
    result.concat()
}

/// `accepted` with the pending edits (`accepted` → `current`) that the user
/// committed as they are in `committed` (see the action log's
/// `keep_committed_edits`).
pub(crate) fn keep_committed(accepted: &str, current: &str, committed: &str) -> String {
    if current == committed {
        return committed.to_string();
    }
    let pending = line_edits(accepted, current);
    let committed_edits = line_edits(accepted, committed);
    let current_lines = lines(current);
    let committed_lines = lines(committed);
    let mut result: Vec<&str> = lines(accepted);
    let mut row_delta = 0i64;
    let mut pending = pending.into_iter().peekable();
    for committed_edit in committed_edits {
        while let Some(edit) = pending.peek() {
            if committed_edit.old == edit.old {
                if rows(&current_lines, &edit.new) == rows(&committed_lines, &committed_edit.new) {
                    let start = (i64::from(edit.old.start) + row_delta).max(0) as usize;
                    let end = (i64::from(edit.old.end) + row_delta).max(0) as usize;
                    let start = start.min(result.len());
                    let end = end.clamp(start, result.len());
                    result.splice(start..end, rows(&current_lines, &edit.new));
                    row_delta += edit.new_len() - edit.old_len();
                }
            } else if edit.old.start >= committed_edit.old.end {
                break;
            }
            pending.next();
        }
    }
    result.concat()
}

/// `accepted` with the pending edits (`accepted` → `current`) taken in,
/// except those `stays_pending` keeps.
pub(crate) fn accept_edits(
    accepted: &str,
    current: &str,
    stays_pending: impl Fn(&LineEdit) -> bool,
) -> String {
    let edits = line_edits(accepted, current);
    if edits.iter().all(|edit| !stays_pending(edit)) {
        return current.to_string();
    }
    let accepted_lines = lines(accepted);
    let current_lines = lines(current);
    let mut result = Vec::new();
    let mut row = 0u32;
    for edit in &edits {
        result.extend(rows(&accepted_lines, &(row..edit.old.start)));
        if stays_pending(edit) {
            result.extend(rows(&accepted_lines, &edit.old));
        } else {
            result.extend(rows(&current_lines, &edit.new));
        }
        row = edit.old.end;
    }
    result.extend(rows(&accepted_lines, &(row..accepted_lines.len() as u32)));
    result.concat()
}

/// The rows of `current` that a turn's edits (`before` → `after`) are at now.
fn turn_rows_now(before: &str, after: &str, current: &str) -> Vec<Range<u32>> {
    let since = line_edits(after, current);
    line_edits(before, after)
        .iter()
        .map(|edit| map_forward(&edit.new, &since))
        .collect()
}

/// What's accepted when a turn ends (or later, if the review was open then):
/// the pending edits of earlier turns, which had until now to be reviewed.
/// Those that touch the turn's own edits (`before` → `after`) stay pending;
/// a file the turn didn't change (`None`) has none left.
pub(crate) fn auto_keep(accepted: &str, current: &str, turn: Option<(&str, &str)>) -> String {
    let Some((before, after)) = turn else {
        return current.to_string();
    };
    let turn_rows = turn_rows_now(before, after, current);
    accept_edits(accepted, current, |edit| {
        turn_rows.iter().any(|rows| touches(&edit.new, rows))
    })
}

/// A turn's versions of a file: before it and when it ended.
pub(crate) struct TurnVersions<'a> {
    pub number: usize,
    pub before: &'a str,
    pub after: &'a str,
}

/// Which turns each pending edit (`accepted` → `current`) comes from, like
/// `git blame`: the edit is followed back through the turns, newest first,
/// and a turn whose own edits it touches is one. The rows are `current`'s.
pub(crate) fn edit_turns(
    accepted: &str,
    current: &str,
    turns_newest_first: &[TurnVersions],
) -> Vec<(Range<u32>, BTreeSet<usize>)> {
    let edits = line_edits(accepted, current);
    if edits.is_empty() {
        return Vec::new();
    }
    // For each turn, how its end became the next version, and its edits.
    let mut steps = Vec::with_capacity(turns_newest_first.len());
    let mut later = current;
    for turn in turns_newest_first {
        steps.push((
            turn.number,
            line_edits(turn.after, later),
            line_edits(turn.before, turn.after),
        ));
        later = turn.before;
    }
    edits
        .into_iter()
        .map(|edit| {
            let mut range = edit.new.clone();
            let mut turns = BTreeSet::new();
            for (number, since, own) in &steps {
                range = map_backward(&range, since);
                if own.iter().any(|own| touches(&own.new, &range)) {
                    turns.insert(*number);
                }
                range = map_backward(&range, own);
            }
            (edit.new, turns)
        })
        .collect()
}

/// The turns of the edits that touch rows `rows` of the current text.
pub(crate) fn turns_at(
    edit_turns: &[(Range<u32>, BTreeSet<usize>)],
    rows: &Range<u32>,
) -> BTreeSet<usize> {
    edit_turns
        .iter()
        .filter(|(edit_rows, _)| touches(edit_rows, rows))
        .flat_map(|(_, turns)| turns.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_touches() {
        assert!(touches(&(1..3), &(2..4)));
        assert!(!touches(&(1..3), &(3..4)));
        // A removal at row 3 touches the lines on both sides.
        assert!(touches(&(3..3), &(1..3)));
        assert!(touches(&(3..3), &(3..5)));
        assert!(!touches(&(3..3), &(4..5)));
    }

    #[test]
    fn test_apply_non_conflicting() {
        let accepted = "a\nb\nc\nd\ne\nf\n";
        // The agent changed b.
        let left = "a\nB\nc\nd\ne\nf\n";
        // Then the user changed e, and added a line at the end.
        let current = "a\nB\nc\nd\nE\nf\ng\n";
        assert_eq!(
            apply_non_conflicting(accepted, left, current),
            "a\nb\nc\nd\nE\nf\ng\n"
        );
        // An edit of the user's on the agent's line stays pending.
        let current = "a\nB2\nc\nd\ne\nf\n";
        assert_eq!(apply_non_conflicting(accepted, left, current), accepted);
        // Nothing pending: everything the user did is accepted.
        assert_eq!(
            apply_non_conflicting(accepted, accepted, current),
            current
        );
        // With lines added before the agent's edit.
        let left = "a\nb\nc\nd\nE\nf\n";
        let current = "x\ny\na\nb\nc\nd\nE\nf\n";
        assert_eq!(
            apply_non_conflicting(accepted, left, current),
            "x\ny\na\nb\nc\nd\ne\nf\n"
        );
    }

    #[test]
    fn test_keep_committed() {
        let accepted = "a\nb\nc\nd\ne\n";
        let current = "a\nB\nc\nd\nE\n";
        // The user committed the change to b, not the one to e.
        let committed = "a\nB\nc\nd\ne\n";
        assert_eq!(
            keep_committed(accepted, current, committed),
            "a\nB\nc\nd\ne\n"
        );
        // Committed as it is.
        assert_eq!(keep_committed(accepted, current, current), current);
        // Committed something else: nothing is accepted.
        assert_eq!(
            keep_committed(accepted, current, "a\nX\nc\nd\ne\n"),
            accepted
        );
    }

    #[test]
    fn test_auto_keep_keeps_what_the_turn_touched() {
        let accepted = "a\nb\nc\nd\ne\nf\ng\nh\n";
        // The first turn changed b, the second g.
        let first = "a\nB\nc\nd\ne\nf\ng\nh\n";
        let second = "a\nB\nc\nd\ne\nf\nG\nh\n";
        // At the second's end, b is accepted; g stays pending.
        assert_eq!(
            auto_keep(accepted, second, Some((first, second))),
            "a\nB\nc\nd\ne\nf\ng\nh\n"
        );
        // A turn that changed the line again keeps it pending.
        let again = "a\nB2\nc\nd\ne\nf\ng\nh\n";
        assert_eq!(auto_keep(accepted, again, Some((first, again))), accepted);
        // A turn that didn't change the file: all of it is accepted.
        assert_eq!(auto_keep(accepted, second, None), second);
    }

    #[test]
    fn test_edit_turns() {
        let accepted = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let first = "a\nB\nc\nd\ne\nf\ng\nh\n";
        // The user adds a line at the top between the turns.
        let between = "top\na\nB\nc\nd\ne\nf\ng\nh\n";
        let second = "top\na\nB\nc\nd\ne\nf\nG\nh\n";
        let current = second;
        let turns = [
            TurnVersions {
                number: 2,
                before: between,
                after: second,
            },
            TurnVersions {
                number: 1,
                before: accepted,
                after: first,
            },
        ];
        let blamed = edit_turns(accepted, current, &turns);
        let by_rows: Vec<(Range<u32>, Vec<usize>)> = blamed
            .iter()
            .map(|(rows, turns)| (rows.clone(), turns.iter().copied().collect()))
            .collect();
        assert_eq!(
            by_rows,
            vec![(0..1, vec![]), (2..3, vec![1]), (7..8, vec![2])]
        );
        assert_eq!(
            turns_at(&blamed, &(7..8)).into_iter().collect::<Vec<_>>(),
            vec![2]
        );
        // Changed by both turns.
        let second = "a\nB2\nc\nd\ne\nf\ng\nh\n";
        let turns = [
            TurnVersions {
                number: 2,
                before: first,
                after: second,
            },
            TurnVersions {
                number: 1,
                before: accepted,
                after: first,
            },
        ];
        let blamed = edit_turns(accepted, second, &turns);
        assert_eq!(blamed.len(), 1);
        assert_eq!(blamed[0].1.iter().copied().collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn test_accept_edits() {
        let accepted = "a\nb\nc\n";
        let current = "A\nb\nC\n";
        assert_eq!(
            accept_edits(accepted, current, |edit| edit.old.start == 0),
            "a\nb\nC\n"
        );
        assert_eq!(accept_edits(accepted, current, |_| false), current);
        assert_eq!(accept_edits(accepted, current, |_| true), accepted);
    }

    #[test]
    fn test_normalized_text() {
        assert_eq!(normalized_text("\u{feff}a\r\nb\r\n".into()), "a\nb\n");
    }
}
