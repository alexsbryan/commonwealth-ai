// SPDX-License-Identifier: AGPL-3.0-or-later
//! `PermissionStore` impl — tool permission grants.

use super::*;

#[async_trait]
impl PermissionStore for SqliteStateStore {
    async fn get_permission(&self, tool_id: &str, scope: &str) -> Result<Option<bool>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT granted FROM permissions WHERE tool_id = ?1 AND scope = ?2",
            rusqlite::params![tool_id, scope],
            |row| row.get::<_, bool>(0),
        );

        match result {
            Ok(granted) => Ok(Some(granted)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(map_db(e)),
        }
    }

    async fn set_permission(&self, tool_id: &str, scope: &str, granted: bool) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO permissions (tool_id, scope, granted, granted_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![tool_id, scope, granted, now()],
        )
        .map_err(map_db)?;
        Ok(())
    }
}
