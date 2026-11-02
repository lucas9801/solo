//! SOLO: terminals' contents and command blocks are saved to a file per
//! terminal and written into the terminal again when Solo restarts, like
//! Warp's session restoration. The shell itself starts afresh.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};
use terminal::shell_integration::SessionSnapshot;
use workspace::{ItemId, WorkspaceId};

/// Files of terminals not seen for this long are deleted.
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Per release channel, like the database the workspace ids come from.
fn sessions_dir() -> PathBuf {
    paths::data_dir()
        .join("terminal_sessions")
        .join(release_channel::RELEASE_CHANNEL.dev_name())
}

fn session_file(workspace_id: WorkspaceId, item_id: ItemId) -> PathBuf {
    sessions_dir().join(format!("{}-{item_id}.json", i64::from(workspace_id)))
}

/// What to save for one terminal.
pub(crate) struct SaveContents {
    pub workspace_id: WorkspaceId,
    pub item_id: ItemId,
    /// None deletes what was saved.
    pub snapshot: Option<SessionSnapshot>,
    /// An item whose saved contents this replaces, to be deleted.
    pub replaces: Option<ItemId>,
}

impl SaveContents {
    pub(crate) fn write(&self) -> Result<()> {
        let path = session_file(self.workspace_id, self.item_id);
        match &self.snapshot {
            Some(snapshot) => {
                std::fs::create_dir_all(sessions_dir())?;
                let json = serde_json::to_vec(snapshot)?;
                // Write a temporary file first, so quitting mid-write can't
                // leave a truncated one.
                let temp_path = path.with_extension("json.tmp");
                std::fs::write(&temp_path, json)?;
                std::fs::rename(&temp_path, &path)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
            None => remove(&path),
        }
        if let Some(replaced) = self.replaces
            && replaced != self.item_id
        {
            remove(&session_file(self.workspace_id, replaced));
        }
        Ok(())
    }
}

fn remove(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("failed to delete {}: {error}", path.display());
    }
}

pub(crate) fn load(workspace_id: WorkspaceId, item_id: ItemId) -> Option<SessionSnapshot> {
    let path = session_file(workspace_id, item_id);
    let json = std::fs::read(&path).ok()?;
    serde_json::from_slice(&json)
        .map_err(|error| log::warn!("failed to read {}: {error}", path.display()))
        .ok()
}

/// Deletes the files of terminals that haven't been saved for a long time,
/// or all of them when contents aren't restored (left from when they were).
/// Closed terminals delete theirs; this catches what was left behind, e.g.
/// by a crash. (Workspace cleanup can't be used: terminals in the panel and
/// the center are cleaned up separately, each pass knowing only its own.)
pub(crate) fn delete_stale(restore_contents: bool) {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let stale = !restore_contents
            || entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > MAX_AGE);
        if stale {
            remove(&entry.path());
        }
    }
}
