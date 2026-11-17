use db::{
    query,
    sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
    sqlez_macros::sql,
};
use workspace::{ItemId, WorkspaceDb, WorkspaceId};

/// The address of each open browser tab, to reopen it after a restart. Only the
/// address is kept: no history, form contents or scroll position.
pub struct BrowserDb(ThreadSafeConnection);

impl Domain for BrowserDb {
    const NAME: &str = stringify!(BrowserDb);

    const MIGRATIONS: &[&str] = &[sql!(
        CREATE TABLE browser_tabs (
            workspace_id INTEGER,
            item_id INTEGER UNIQUE,

            url TEXT,

            PRIMARY KEY(workspace_id, item_id),
            FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
            ON DELETE CASCADE
        ) STRICT;
    )];
}

db::static_connection!(BrowserDb, [WorkspaceDb]);

impl BrowserDb {
    query! {
        pub async fn save_url(
            item_id: ItemId,
            workspace_id: WorkspaceId,
            url: String
        ) -> Result<()> {
            INSERT OR REPLACE INTO browser_tabs(item_id, workspace_id, url)
            VALUES (?, ?, ?)
        }
    }

    query! {
        pub fn get_url(item_id: ItemId, workspace_id: WorkspaceId) -> Result<Option<String>> {
            SELECT url
            FROM browser_tabs
            WHERE item_id = ? AND workspace_id = ?
        }
    }
}
