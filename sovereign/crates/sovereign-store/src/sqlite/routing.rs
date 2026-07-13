// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RoutingStore` impl — routing decision log + redirect signals.

use super::*;

#[async_trait]
impl RoutingStore for SqliteStateStore {
    async fn log_routing(
        &self,
        message_hash: &str,
        classified_as: &str,
        latency_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO routing_log (message_hash, classified_as, latency_ms, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![message_hash, classified_as, latency_ms, now()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn log_routing_meta(
        &self,
        message_hash: &str,
        coarse_intent: &str,
        self_assessment: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE routing_log SET coarse_intent = ?1, self_assessment = ?2
             WHERE id = (
                 SELECT id FROM routing_log WHERE message_hash = ?3
                 ORDER BY created_at DESC LIMIT 1
             )",
            rusqlite::params![coarse_intent, self_assessment, message_hash],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_routing_corrections(&self, limit: usize) -> Result<Vec<RoutingCorrection>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT message_hash, classified_as, was_correct, created_at
                 FROM routing_log WHERE was_correct = 0
                 ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(map_db)?;

        let corrections: Vec<RoutingCorrection> = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(RoutingCorrection {
                    message_hash: row.get(0)?,
                    classified_as: row.get(1)?,
                    was_correct: row.get::<_, bool>(2)?,
                    created_at: row.get(3)?,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(corrections)
    }

    async fn mark_routing_correct(&self, message_hash: &str, was_correct: bool) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE routing_log SET was_correct = ?2 WHERE message_hash = ?1",
            rusqlite::params![message_hash, was_correct],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn mark_routing_redirected(&self, message_hash: &str, redirect_to: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE routing_log SET was_redirected = 1, redirect_to = ?2 \
             WHERE message_hash = ?1",
            rusqlite::params![message_hash, redirect_to],
        )
        .map_err(map_db)?;
        Ok(())
    }
}
