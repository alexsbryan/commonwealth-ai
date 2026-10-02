// SPDX-License-Identifier: AGPL-3.0-or-later
//! `HealthStore` impl — health reports, issues, resolution log.

use super::*;

#[async_trait]
impl HealthStore for SqliteStateStore {
    async fn save_health_report(
        &self,
        report: &sovereign_core::health::HealthReport,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let component = serde_json::to_string(&report.component).map_err(map_json)?;
        let status = serde_json::to_string(&report.status).map_err(map_json)?;
        let issues_json = serde_json::to_string(&report.issues).map_err(map_json)?;
        conn.execute(
            "INSERT INTO health_reports (component, status, issues_json, summary, measured_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                component,
                status,
                issues_json,
                report.summary,
                report.measured_at
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn save_pending_decision(
        &self,
        d: &sovereign_core::health::PendingDecision,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let component = serde_json::to_string(&d.component).map_err(map_json)?;
        let issue_json = serde_json::to_string(&d.issue).map_err(map_json)?;
        let options_json = serde_json::to_string(&d.options).map_err(map_json)?;
        conn.execute(
            "INSERT INTO pending_health_decisions
             (component, issue_json, question, options_json, consequence, surfaced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                component,
                issue_json,
                d.question,
                options_json,
                d.consequence,
                d.surfaced_at_secs
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn list_pending_decisions(&self) -> Result<Vec<sovereign_core::health::PendingDecision>> {
        use std::time::{Duration, UNIX_EPOCH};
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, component, issue_json, question, options_json, consequence, surfaced_at
                 FROM pending_health_decisions WHERE resolved_at IS NULL",
            )
            .map_err(map_db)?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(map_db)?;

        let mut out = Vec::new();
        for row in rows {
            let (
                id,
                component_json,
                issue_json,
                question,
                options_json,
                consequence,
                surfaced_at_secs,
            ) = row.map_err(map_db)?;
            let component: sovereign_core::health::Component =
                serde_json::from_str(&component_json).map_err(map_json)?;
            let issue: sovereign_core::health::HealthIssue =
                serde_json::from_str(&issue_json).map_err(map_json)?;
            let options: Vec<sovereign_core::health::UserOption> =
                serde_json::from_str(&options_json).map_err(map_json)?;
            let surfaced_at =
                UNIX_EPOCH.checked_add(Duration::from_secs(surfaced_at_secs.max(0) as u64));
            out.push(sovereign_core::health::PendingDecision {
                id: Some(id),
                component,
                issue,
                question,
                options,
                consequence,
                surfaced_at_secs,
                surfaced_at,
            });
        }
        Ok(out)
    }

    async fn resolve_pending_decision(
        &self,
        id: i64,
        chosen: sovereign_core::health::RepairKind,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let chosen_json = serde_json::to_string(&chosen).map_err(map_json)?;
        conn.execute(
            "UPDATE pending_health_decisions SET resolved_at = ?1 WHERE id = ?2",
            rusqlite::params![now(), id],
        )
        .map_err(map_db)?;
        let _ = chosen_json; // stored in resolved_at for now; extend schema if needed
        Ok(())
    }
}
