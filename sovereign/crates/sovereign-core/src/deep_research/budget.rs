// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ONE run-scoped, fail-closed spend decider (budget-decider.md §2).
//!
//! The loop's search + fetch paths consult this decider only. Fail-closed
//! table:
//!
//! | situation | verdict |
//! |---|---|
//! | no allowance record for the family/key | refuse |
//! | journal write fails (ledger not persistable) | refuse — never spend blind |
//! | unknown family / unknown key | refuse |
//! | allowance exhausted (== 0) | refuse |
//! | allowance remaining (> 0) | allow, then decrement + journal |
//!
//! The journal is the `budget-ledger.json` ICD (icd-schemas.md §7),
//! appended synchronously before the spend executes — an allowance unit
//! is consumed by the attempt, recorded first. Run-scoped: the ledger is
//! bound to one run; the run charter's allowance seeds it at run start.

use super::icd::{BudgetEntry, BudgetLedger};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The meter families this build knows. An unknown family refuses.
pub const FAMILY_WEB_SEARCH: &str = "web-search";
pub const FAMILY_WEB_FETCH: &str = "web-fetch";
/// The t2b frontier-judge family (order deep-research-t2a, R-6).
/// INERT until t2b wires the judge dispatch: the family is declared
/// (so the closed set is complete and a t2b spend compiles against
/// the ONE decider) but nothing in t2a calls it and no allowance is
/// ever seeded for it — an attempted spend refuses
/// `no-allowance-or-exhausted`, the fail-closed default.
pub const FAMILY_FRONTIER_KEY: &str = "frontier-key";

/// The fetch meter's one key.
pub const KEY_FETCH_PAGES: &str = "pages";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendVerdict {
    Allow {
        family: String,
        key: String,
        units: u32,
    },
    Refuse {
        family: String,
        key: String,
        units: u32,
        reason: String,
    },
}

impl SpendVerdict {
    pub fn allowed(&self) -> bool {
        matches!(self, SpendVerdict::Allow { .. })
    }
}

/// The run-scoped fail-closed decider. Owns the allowance and the
/// journal; persisted as the `budget-ledger.json` ICD.
pub struct SpendDecider {
    run_id: String,
    charter_hash: String,
    /// The original allowance (the ICD's `allowance` — what was granted).
    initial_allowance: HashMap<String, u32>,
    /// The live remaining balance (mutated by allow).
    remaining: HashMap<String, u32>,
    spent: HashMap<String, u32>,
    entries: Vec<BudgetEntry>,
    journal_path: PathBuf,
}

impl SpendDecider {
    /// Create the decider from the charter's allowance. The journal is
    /// written to `journal_path` (the run dir's `budget-ledger.json`);
    /// a journal that cannot be written refuses every allow from the
    /// start — never spend blind.
    pub fn new(
        run_id: &str,
        charter_hash: &str,
        allowance: HashMap<String, u32>,
        journal_path: &Path,
    ) -> Result<SpendDecider, String> {
        let mut d = SpendDecider {
            run_id: run_id.to_string(),
            charter_hash: charter_hash.to_string(),
            initial_allowance: allowance.clone(),
            remaining: allowance,
            spent: HashMap::new(),
            entries: Vec::new(),
            journal_path: journal_path.to_path_buf(),
        };
        // Refuse-at-construction if the journal is not writable: an
        // unjournalable run is a run that cannot spend (fail-closed).
        d.persist()
            .map_err(|e| format!("budget journal unwritable: {e}"))?;
        Ok(d)
    }

    /// The one decision: allow-then-decrement or refuse, both journaled
    /// before the caller spends. A journal failure refuses (never spend
    /// blind).
    pub async fn allow(
        &mut self,
        family: &str,
        key: &str,
        units: u32,
        at_unix: i64,
    ) -> Result<SpendVerdict, String> {
        if family != FAMILY_WEB_SEARCH
            && family != FAMILY_WEB_FETCH
            && family != FAMILY_FRONTIER_KEY
        {
            return Ok(SpendVerdict::Refuse {
                family: family.to_string(),
                key: key.to_string(),
                units,
                reason: "unknown-family".to_string(),
            });
        }
        if family == FAMILY_WEB_FETCH && key != KEY_FETCH_PAGES {
            return Ok(SpendVerdict::Refuse {
                family: family.to_string(),
                key: key.to_string(),
                units,
                reason: "unknown-key".to_string(),
            });
        }
        let meter = format!("{family}:{key}");
        let allowed = *self.remaining.get(&meter).unwrap_or(&0);
        if allowed == 0 {
            let verdict = SpendVerdict::Refuse {
                family: family.to_string(),
                key: key.to_string(),
                units,
                reason: "no-allowance-or-exhausted".to_string(),
            };
            self.journal(&verdict, at_unix)?;
            return Ok(verdict);
        }
        if allowed < units {
            let verdict = SpendVerdict::Refuse {
                family: family.to_string(),
                key: key.to_string(),
                units,
                reason: "insufficient-allowance".to_string(),
            };
            self.journal(&verdict, at_unix)?;
            return Ok(verdict);
        }
        // Allow, then decrement — the journal records the spend before
        // it executes.
        self.remaining.insert(meter.clone(), allowed - units);
        let spent = self.spent.entry(meter.clone()).or_insert(0);
        *spent += units;
        let verdict = SpendVerdict::Allow {
            family: family.to_string(),
            key: key.to_string(),
            units,
        };
        self.journal(&verdict, at_unix)?;
        Ok(verdict)
    }

    fn journal(&mut self, verdict: &SpendVerdict, at_unix: i64) -> Result<(), String> {
        let (family, key, units, decision, reason) = match verdict {
            SpendVerdict::Allow { family, key, units } => {
                (family.clone(), key.clone(), *units, "allow", None)
            }
            SpendVerdict::Refuse {
                family,
                key,
                units,
                reason,
            } => (
                family.clone(),
                key.clone(),
                *units,
                "refuse",
                Some(reason.clone()),
            ),
        };
        self.entries.push(BudgetEntry {
            family,
            key,
            units,
            at_unix,
            decision: decision.to_string(),
            reason,
        });
        self.persist()
    }

    fn persist(&self) -> Result<(), String> {
        let ledger = self.snapshot();
        let json = serde_json::to_string_pretty(&ledger)
            .map_err(|e| format!("budget ledger serialize: {e}"))?;
        std::fs::write(&self.journal_path, json)
            .map_err(|e| format!("budget ledger write {}: {e}", self.journal_path.display()))
    }

    /// The ICD snapshot: original allowance + journal + spent/remaining.
    pub fn snapshot(&self) -> BudgetLedger {
        BudgetLedger {
            icd: "budget_ledger".to_string(),
            version: super::icd::ICD_VERSION,
            run_id: self.run_id.clone(),
            charter_hash: self.charter_hash.clone(),
            allowance: self.initial_allowance.clone(),
            entries: self.entries.clone(),
            spent: self.spent.clone(),
            remaining: self.remaining.clone(),
        }
    }

    /// Remaining units on a meter (for the budget check between rounds).
    pub fn remaining(&self, family: &str, key: &str) -> u32 {
        self.remaining
            .get(&format!("{family}:{key}"))
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decider(tmp: &std::path::Path, search: u32, fetch: u32) -> SpendDecider {
        let mut allowance = HashMap::new();
        allowance.insert(format!("{FAMILY_WEB_SEARCH}:duckduckgo"), search);
        allowance.insert(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), fetch);
        SpendDecider::new(
            "run-test",
            "hash",
            allowance,
            &tmp.join("budget-ledger.json"),
        )
        .expect("journal writable")
    }

    #[tokio::test]
    async fn fail_closed_table() {
        let tmp = std::env::temp_dir().join(format!("dr-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mut d = decider(&tmp, 2, 1);

        // Missing meter → refuse (no allowance record).
        let v = d.allow(FAMILY_WEB_SEARCH, "brave", 1, 0).await.unwrap();
        assert_eq!(
            v,
            SpendVerdict::Refuse {
                family: FAMILY_WEB_SEARCH.to_string(),
                key: "brave".to_string(),
                units: 1,
                reason: "no-allowance-or-exhausted".to_string()
            }
        );
        // Unknown family → refuse.
        let v = d.allow("no-such-family", "o3", 1, 0).await.unwrap();
        assert!(matches!(v, SpendVerdict::Refuse { ref reason, .. } if reason == "unknown-family"));
        // The t2b frontier-key family is DECLARED but INERT (order
        // deep-research-t2a, R-6): no allowance is ever seeded for
        // it in t2a, so an attempted spend refuses fail-closed —
        // the same no-allowance verdict as any unseeded meter.
        let v = d
            .allow(FAMILY_FRONTIER_KEY, "frontier-judge", 1, 0)
            .await
            .unwrap();
        assert!(
            matches!(v, SpendVerdict::Refuse { ref reason, .. } if reason == "no-allowance-or-exhausted")
        );
        // Unknown fetch key → refuse.
        let v = d
            .allow(FAMILY_WEB_FETCH, "unknown-key", 1, 0)
            .await
            .unwrap();
        assert!(matches!(v, SpendVerdict::Refuse { ref reason, .. } if reason == "unknown-key"));
        // Allow then decrement.
        let v = d
            .allow(FAMILY_WEB_SEARCH, "duckduckgo", 1, 0)
            .await
            .unwrap();
        assert!(v.allowed());
        assert_eq!(d.remaining(FAMILY_WEB_SEARCH, "duckduckgo"), 1);
        // Exhausted → refuse.
        let v = d
            .allow(FAMILY_WEB_SEARCH, "duckduckgo", 1, 0)
            .await
            .unwrap();
        assert!(v.allowed());
        let v = d
            .allow(FAMILY_WEB_SEARCH, "duckduckgo", 1, 0)
            .await
            .unwrap();
        assert!(
            matches!(v, SpendVerdict::Refuse { ref reason, .. } if reason == "no-allowance-or-exhausted")
        );
        // Over-units → refuse.
        let v = d
            .allow(FAMILY_WEB_FETCH, KEY_FETCH_PAGES, 2, 0)
            .await
            .unwrap();
        assert!(
            matches!(v, SpendVerdict::Refuse { ref reason, .. } if reason == "insufficient-allowance")
        );

        // The journal is persisted as the ICD.
        let ledger: BudgetLedger =
            serde_json::from_str(&std::fs::read_to_string(tmp.join("budget-ledger.json")).unwrap())
                .unwrap();
        assert_eq!(ledger.icd, "budget_ledger");
        // Six journaled decisions: the "brave" no-allowance refusal,
        // the inert frontier-key no-allowance refusal, two duckduckgo
        // allows, the exhausted refusal, and the
        // insufficient-allowance refusal. The unknown-family and
        // unknown-key refusals are programmer-error guards, not spend
        // decisions — they are refused before the journal, deliberately.
        assert_eq!(ledger.entries.len(), 6);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
