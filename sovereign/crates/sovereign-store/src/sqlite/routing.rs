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
        conversation_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO routing_log
                 (message_hash, classified_as, latency_ms, created_at, conversation_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![message_hash, classified_as, latency_ms, now(), conversation_id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    /// Same most-recent-row selection as `log_routing_meta` below:
    /// `message_hash` is not unique, so the UPDATE must name the row
    /// this turn's `log_routing` just inserted.
    async fn log_routing_policy_intent(
        &self,
        message_hash: &str,
        policy_intent: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE routing_log SET policy_intent = ?1
             WHERE id = (
                 SELECT id FROM routing_log WHERE message_hash = ?2
                 ORDER BY created_at DESC LIMIT 1
             )",
            rusqlite::params![policy_intent, message_hash],
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The override lands on THIS turn's row, not on an older turn that
    /// happened to ask the same question.
    ///
    /// `message_hash` is not unique, which is why the UPDATE names one row
    /// rather than every row with the hash. Drop the
    /// `ORDER BY created_at DESC LIMIT 1` and the first assertion goes red:
    /// the older conversation's row picks up an override it never had.
    ///
    /// `routing_log.created_at` is SECONDS (`sovereign_time::unix_now`), so
    /// the two rows are separated in time deliberately — otherwise they tie
    /// and the test would be asserting SQLite's tie-break.
    #[tokio::test]
    async fn policy_intent_lands_on_the_turn_that_was_overridden() {
        let store = SqliteStateStore::open_in_memory().expect("in-memory store");
        let hash = "same-question-twice";

        store
            .log_routing(hash, "SimpleQuery", 10, Some("conv-older"))
            .await
            .expect("older row");
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        store
            .log_routing(hash, "SimpleQuery", 10, Some("conv-newer"))
            .await
            .expect("newer row");

        store
            .log_routing_policy_intent(hash, "ComplexTask")
            .await
            .expect("record the override");

        let rows: Vec<(String, Option<String>)> = {
            let conn = store.conn.lock().await;
            let mut stmt = conn
                .prepare(
                    "SELECT conversation_id, policy_intent FROM routing_log \
                     WHERE message_hash = ?1 ORDER BY id ASC",
                )
                .expect("prepare");
            let out = stmt
                .query_map(rusqlite::params![hash], |r| Ok((r.get(0)?, r.get(1)?)))
                .expect("query")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("collect");
            out
        };

        assert_eq!(
            rows,
            vec![
                ("conv-older".to_string(), None),
                ("conv-newer".to_string(), Some("ComplexTask".to_string())),
            ],
            "only the turn that was overridden carries policy_intent"
        );
    }
}
