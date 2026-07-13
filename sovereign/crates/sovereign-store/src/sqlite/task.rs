// SPDX-License-Identifier: AGPL-3.0-or-later
//! `TaskStore` impl — task rows with plan/result JSON.

use super::*;

#[async_trait]
impl TaskStore for SqliteStateStore {
    async fn save_task(&self, task: &Task) -> Result<()> {
        let conn = self.conn.lock().await;
        let plan_json =
            serde_json::to_string(&task.plan).map_err(|e| Error::Serialization(e.to_string()))?;
        let state_json = serde_json::to_string(&task.completed_steps)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        let status_str = match task.status {
            TaskStatus::Running => "running",
            TaskStatus::Paused => "paused",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
        };

        conn.execute(
            "INSERT OR REPLACE INTO tasks (id, conversation_id, goal, plan, state, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                task.id,
                task.conversation_id,
                task.goal,
                plan_json,
                state_json,
                status_str,
                task.created_at,
                task.updated_at,
            ],
        )
        .map_err(map_db)?;

        Ok(())
    }

    async fn get_task(&self, id: &str) -> Result<Task> {
        let conn = self.conn.lock().await;

        conn.query_row(
            "SELECT id, conversation_id, goal, plan, state, status, created_at, updated_at
             FROM tasks WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                let plan_json: String = row.get(3)?;
                let state_json: String = row.get(4)?;
                let status_str: String = row.get(5)?;

                Ok(Task {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    goal: row.get(2)?,
                    plan: serde_json::from_str(&plan_json).unwrap_or_else(|_| Plan {
                        id: String::new(),
                        goal: String::new(),
                        steps: Vec::new(),
                        edges: Vec::new(),
                    }),
                    completed_steps: serde_json::from_str(&state_json).unwrap_or_default(),
                    status: match status_str.as_str() {
                        "running" => TaskStatus::Running,
                        "paused" => TaskStatus::Paused,
                        "completed" => TaskStatus::Completed,
                        _ => TaskStatus::Failed,
                    },
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                    version: 0,
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("Task {id}")),
            other => map_db(other),
        })
    }
}
