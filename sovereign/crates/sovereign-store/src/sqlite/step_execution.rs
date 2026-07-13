// SPDX-License-Identifier: AGPL-3.0-or-later
//! `StepExecutionStore` impl — per-step execution records for replay.

use super::*;

#[async_trait]
impl StepExecutionStore for SqliteStateStore {
    async fn record_started(&self, execution: &StepExecution) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO step_executions
               (id, task_id, step_id, tool_id, status, idempotency_key,
                summary, anomalies, started_at, ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                execution.id,
                execution.task_id,
                execution.step_id as i64,
                execution.tool_id,
                execution.status.as_str(),
                execution.idempotency_key,
                execution.summary,
                execution.anomalies,
                execution.started_at,
                execution.ended_at,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn mark_completed(
        &self,
        execution_id: &str,
        summary: Option<String>,
        anomalies: Option<String>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE step_executions
                SET status = 'completed', summary = ?2, anomalies = ?3, ended_at = ?4
              WHERE id = ?1",
            rusqlite::params![execution_id, summary, anomalies, now()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn mark_failed(&self, execution_id: &str, message: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE step_executions
                SET status = 'failed', anomalies = ?2, ended_at = ?3
              WHERE id = ?1",
            rusqlite::params![execution_id, message, now()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn find_execution(&self, idempotency_key: &str) -> Result<Option<StepExecution>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, task_id, step_id, tool_id, status, idempotency_key,
                    summary, anomalies, started_at, ended_at
               FROM step_executions
              WHERE idempotency_key = ?1
           ORDER BY started_at DESC
              LIMIT 1",
            rusqlite::params![idempotency_key],
            |row| {
                let sid: i64 = row.get(2)?;
                let status_str: String = row.get(4)?;
                Ok(StepExecution {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    step_id: sid as usize,
                    tool_id: row.get(3)?,
                    status: ExecutionStatus::from_db(&status_str),
                    idempotency_key: row.get(5)?,
                    summary: row.get(6)?,
                    anomalies: row.get(7)?,
                    started_at: row.get(8)?,
                    ended_at: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(map_db)
    }
}
