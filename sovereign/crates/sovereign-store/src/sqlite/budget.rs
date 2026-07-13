// SPDX-License-Identifier: AGPL-3.0-or-later
//! `BudgetStore` impl — per-backend search budgets.

use super::*;

#[async_trait]
impl BudgetStore for SqliteStateStore {
    async fn get_search_budget(&self, backend: &str) -> Result<Option<SearchBudget>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT backend, monthly_limit, used_this_month, reset_date
             FROM search_budget WHERE backend = ?1",
            rusqlite::params![backend],
            |row| {
                Ok(SearchBudget {
                    backend: row.get(0)?,
                    monthly_limit: row.get(1)?,
                    used_this_month: row.get(2)?,
                    reset_date: row.get(3)?,
                    version: 0,
                })
            },
        );

        match result {
            Ok(budget) => Ok(Some(budget)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(map_db(e)),
        }
    }

    async fn update_search_budget(&self, budget: &SearchBudget) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO search_budget (backend, monthly_limit, used_this_month, reset_date)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                budget.backend,
                budget.monthly_limit,
                budget.used_this_month,
                budget.reset_date,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }
}
