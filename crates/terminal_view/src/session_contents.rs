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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TerminalView;
    use gpui::{AppContext as _, Entity, TestAppContext, UpdateGlobal, VisualTestContext};
    use settings::SettingsStore;
    use terminal::{Terminal, TerminalBuilder};
    use util::paths::PathStyle;

    fn set_restore_contents(restore_contents: bool, cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.terminal.get_or_insert_default().restore_contents =
                        Some(restore_contents);
                });
            })
        });
    }

    fn write(terminal: &Entity<Terminal>, bytes: &[u8], cx: &mut VisualTestContext) {
        terminal.update(cx, |terminal, cx| terminal.write_raw_output(bytes, cx));
        cx.run_until_parked();
        cx.update(|window, cx| terminal.update(cx, |terminal, cx| terminal.sync(window, cx)));
    }

    fn contents_to_save(
        view: &Entity<TerminalView>,
        cx: &mut VisualTestContext,
    ) -> Option<SaveContents> {
        view.update(cx, |view, cx| view.contents_to_save(cx))
    }

    #[gpui::test]
    async fn test_contents_to_save(cx: &mut TestAppContext) {
        let (project, workspace) = crate::tests::init_test(cx).await;
        let terminal = cx.new(|cx| {
            TerminalBuilder::new_display_only(
                Default::default(),
                settings::AlternateScroll::On,
                None,
                0,
                cx.background_executor(),
                PathStyle::local(),
            )
            .subscribe(cx)
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            TerminalView::new(
                terminal.clone(),
                workspace.downgrade(),
                Some(WorkspaceId::from_i64(1)),
                project.downgrade(),
                window,
                cx,
            )
        });
        write(
            &terminal,
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]633;E;ls\x07\x1b]133;C\x07\
              a.txt\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07",
            cx,
        );

        // Off by default: what was saved before is deleted.
        let contents = contents_to_save(&view, cx).unwrap();
        assert_eq!(contents.snapshot, None);
        assert_eq!(i64::from(contents.workspace_id), 1);
        assert_eq!(contents.item_id, view.entity_id().as_u64());

        set_restore_contents(true, cx);
        let snapshot = contents_to_save(&view, cx).unwrap().snapshot.unwrap();
        assert_eq!(snapshot.rows, ["$ ls", "a.txt"]);
        assert_eq!(snapshot.blocks.len(), 1);
        assert_eq!(snapshot.blocks[0].command.as_deref(), Some("ls"));

        // A restored terminal's save replaces the file it was restored from,
        // once.
        view.update(cx, |view, _| view.restored_from = Some(7));
        assert_eq!(contents_to_save(&view, cx).unwrap().replaces, Some(7));
        assert_eq!(contents_to_save(&view, cx).unwrap().replaces, None);

        // A running agent's half redrawn screen isn't saved, its command is.
        write(
            &terminal,
            b"claude\r\n\x1b]633;E;claude\x07\x1b]133;C\x07> Try \"fix the tests\"",
            cx,
        );
        let snapshot = contents_to_save(&view, cx).unwrap().snapshot.unwrap();
        assert_eq!(snapshot.rows, ["$ ls", "a.txt", "$ claude"]);

        // While a full screen program runs, what was saved before is kept.
        write(&terminal, b"\x1b[?1049h", cx);
        assert!(contents_to_save(&view, cx).is_none());

        // Saving when the test app quits would write to the real data dir.
        view.update(cx, |view, _| view.workspace_id = None);
    }

    #[test]
    fn test_saved_snapshot_reads_back() {
        let snapshot = SessionSnapshot {
            rows: vec!["$ ls".to_string(), "\x1b[0;31mred\x1b[0m".to_string()],
            blocks: vec![terminal::shell_integration::SavedCommandBlock {
                prompt_line: 0,
                input_line: Some(0),
                output_line: Some(1),
                end_line: Some(2),
                exit_code: Some(1),
                command: Some("ls".to_string()),
                working_directory: Some(PathBuf::from("/tmp")),
                duration_ms: Some(12),
            }],
        };
        let json = serde_json::to_vec(&snapshot).unwrap();
        let read: SessionSnapshot = serde_json::from_slice(&json).unwrap();
        assert_eq!(read, snapshot);
    }
}
