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

/// drb1-r2b (order drb1-r2b, campaign drb1-race, the whitelisted
/// Tuning knob "search/fetch allowances and cap derivation"): how many
/// units of a meter ONE acquisition round may spend — the
/// round-allowance split.
///
/// The measured defect (runs-r3a loop seed-02, dr-1787328255): round 1
/// formed more queries than the 12-search allowance holds (2 survey-gap
/// queries + 10 frontier sub-questions), spent all 12, and the
/// between-rounds budget gate then refused the gap round entry before
/// it could ask anything — round-2 search_calls 0, gaps flat 2→2, no
/// fetch-list-2.json. On exactly the hardest questions (round-1
/// searches return little, every call burns) the loop's closure
/// mechanism — round N+1's gap-derived queries — starves.
///
/// Policy: a fair-share waterfall. The round may spend at most
/// `ceil(remaining / rounds_left)` of the meter, where `rounds_left`
/// counts the current round. Properties:
///
/// - no round but the last can exhaust the meter: the cap is strictly
///   below `remaining` whenever `rounds_left ≥ 2` and `remaining ≥ 2`,
///   so every later round — above all the gap round — enters with a
///   queryable allowance;
/// - the split degrades to (near-)equality at any max-rounds /
///   allowance pair: 12@3 → 4/4/4, 12@2 → 6/6, 4@3 → 2/1/1, 4@2 →
///   2/2 — a small allowance still hands every round at least one
///   query as long as the allowance lasts;
/// - the FINAL round (`rounds_left ≤ 1`) may spend everything left —
///   the R1 consume-the-remaining-budget stop rule's shape is intact
///   exactly where it belongs, on the last round;
/// - a degenerate allowance of 1 gives the opening round the unit
///   (ceil), never a structurally empty round — with fewer units than
///   rounds someone must go without, and the broadest round is the
///   better spender.
///
/// The decider itself is untouched: this caps how many times a round
/// ASKS, never what the ledger records — spent/remaining stay the real
/// consumption (the bank instruments read the truth, not a mask).
pub fn round_allowance_cap(remaining: u32, rounds_left: u32) -> u32 {
    if rounds_left <= 1 || remaining == 0 {
        remaining
    } else {
        remaining.div_ceil(rounds_left)
    }
}

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
    /// T6b pre-window slice: the run-scoped dead-fetch set — URLs
    /// whose fetch failed are refused for the rest of the run with no
    /// decider call and no re-spend (the task-56 shape).
    refused_urls: Vec<String>,
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
            refused_urls: Vec::new(),
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

    /// T6b pre-window slice: record a failed fetch's URL dead for the
    /// rest of the run. The in-memory set is updated FIRST (the gate
    /// holds for the live run even when the disk persist fails); a
    /// persist failure is returned for the caller to name — the dead
    /// record is best-effort across a resume, never silently dropped
    /// from the ledger.
    pub fn record_fetch_dead(&mut self, url: &str) -> Result<(), String> {
        if !self.refused_urls.iter().any(|u| u == url) {
            self.refused_urls.push(url.to_string());
        }
        self.persist()
    }

    /// Is this URL dead for the run — its fetch already failed?
    pub fn is_fetch_dead(&self, url: &str) -> bool {
        self.refused_urls.iter().any(|u| u == url)
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
            refused_urls: self.refused_urls.clone(),
        }
    }

    /// Remaining units on a meter (for the budget check between rounds).
    pub fn remaining(&self, family: &str, key: &str) -> u32 {
        self.remaining
            .get(&format!("{family}:{key}"))
            .copied()
            .unwrap_or(0)
    }

    /// Restore the decider from the run's journal (order
    /// deep-research-t3a). The ledger's ENTRIES are ground truth: the
    /// spent/remaining totals are REPLAYED from the allow decisions,
    /// and the stored totals must agree with the replay. Typed
    /// refusals: a missing/unreadable journal (a resume that spends
    /// blind is refused), a foreign ledger (run_id / charter_hash
    /// mismatch), an allowance that no longer matches the charter, an
    /// allow on a meter the charter never granted, spend beyond the
    /// grant, an unknown decision, and stored totals that disagree
    /// with the journal's own entries. The journal is never re-written
    /// by restore — continuity, not mutation.
    pub fn restore(
        run_id: &str,
        charter_hash: &str,
        expected_allowance: &HashMap<String, u32>,
        journal_path: &Path,
    ) -> Result<SpendDecider, String> {
        let raw = std::fs::read_to_string(journal_path).map_err(|e| {
            format!(
                "a resume that spends blind is refused: budget ledger {} is unreadable ({e})",
                journal_path.display()
            )
        })?;
        let ledger: BudgetLedger = serde_json::from_str(&raw).map_err(|e| {
            format!(
                "budget ledger at {} is malformed: {e}",
                journal_path.display()
            )
        })?;
        if ledger.icd != "budget_ledger" || ledger.version != super::icd::ICD_VERSION {
            return Err(format!(
                "budget ledger at {} is not a budget ledger (icd {:?}, version {}) — foreign or tampered",
                journal_path.display(),
                ledger.icd,
                ledger.version
            ));
        }
        if ledger.run_id != run_id {
            return Err(format!(
                "budget ledger at {} belongs to run {} — this run is {}, foreign ledger refuses",
                journal_path.display(),
                ledger.run_id,
                run_id
            ));
        }
        if ledger.charter_hash != charter_hash {
            return Err(format!(
                "budget ledger at {} carries a different charter hash — foreign or tampered",
                journal_path.display()
            ));
        }
        if ledger.allowance != *expected_allowance {
            return Err(format!(
                "budget ledger at {} allowance no longer matches the charter — tampered",
                journal_path.display()
            ));
        }

        // Replay the allow decisions. Refuse decisions are journaled but
        // spend nothing — they change no state and are skipped.
        let mut remaining = ledger.allowance.clone();
        let mut spent: HashMap<String, u32> = HashMap::new();
        for entry in &ledger.entries {
            if entry.decision != "allow" {
                if entry.decision == "refuse" {
                    continue;
                }
                return Err(format!(
                    "budget ledger at {} holds an unknown decision {:?} — tampered",
                    journal_path.display(),
                    entry.decision
                ));
            }
            let meter = format!("{}:{}", entry.family, entry.key);
            let have = remaining.get(&meter).copied().unwrap_or(0);
            if have == 0 {
                return Err(format!(
                    "budget ledger at {} allows {meter} which the charter never granted — tampered",
                    journal_path.display()
                ));
            }
            if entry.units > have {
                return Err(format!(
                    "budget ledger at {} spends {} units of {meter} beyond the grant — tampered",
                    journal_path.display(),
                    entry.units
                ));
            }
            remaining.insert(meter.clone(), have - entry.units);
            let s = spent.entry(meter).or_insert(0);
            *s += entry.units;
        }
        if ledger.spent != spent || ledger.remaining != remaining {
            return Err(format!(
                "budget ledger totals disagree with its own journal entries — tampered"
            ));
        }

        Ok(SpendDecider {
            run_id: run_id.to_string(),
            charter_hash: charter_hash.to_string(),
            initial_allowance: expected_allowance.clone(),
            remaining,
            spent,
            entries: ledger.entries,
            journal_path: journal_path.to_path_buf(),
            // T6b pre-window slice: the dead set is a FACT record (not
            // a spend decision) — replay carries it as stored; a
            // tampered set only ever refuses without spending
            // (fail-closed direction), so it needs no totals check.
            refused_urls: ledger.refused_urls,
        })
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

    fn allowance_5_2() -> HashMap<String, u32> {
        let mut allowance = HashMap::new();
        allowance.insert(format!("{FAMILY_WEB_SEARCH}:duckduckgo"), 5);
        allowance.insert(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 2);
        allowance
    }

    /// A hand-built honest ledger: one allow of 1 search unit.
    fn honest_ledger() -> BudgetLedger {
        BudgetLedger {
            icd: "budget_ledger".to_string(),
            version: super::super::icd::ICD_VERSION,
            run_id: "run-test".to_string(),
            charter_hash: "hash".to_string(),
            allowance: allowance_5_2(),
            entries: vec![BudgetEntry {
                family: FAMILY_WEB_SEARCH.to_string(),
                key: "duckduckgo".to_string(),
                units: 1,
                at_unix: 1,
                decision: "allow".to_string(),
                reason: None,
            }],
            spent: HashMap::from([("web-search:duckduckgo".to_string(), 1)]),
            remaining: HashMap::from([
                ("web-search:duckduckgo".to_string(), 4),
                ("web-fetch:pages".to_string(), 2),
            ]),
            refused_urls: Vec::new(),
        }
    }

    fn write_ledger(journal: &std::path::Path, ledger: &BudgetLedger) {
        std::fs::write(journal, serde_json::to_string_pretty(ledger).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn restore_replays_allow_entries_and_continues() {
        let tmp = std::env::temp_dir().join(format!("dr-budget-restore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let journal = tmp.join("budget-ledger.json");

        // A run that spent 3 search units (2 left), 1 fetch unit (1
        // left), and journaled one refusal.
        let mut d = decider(&tmp, 5, 2);
        for _ in 0..3 {
            assert!(d
                .allow(FAMILY_WEB_SEARCH, "duckduckgo", 1, 1)
                .await
                .unwrap()
                .allowed());
        }
        assert!(d
            .allow(FAMILY_WEB_FETCH, KEY_FETCH_PAGES, 1, 2)
            .await
            .unwrap()
            .allowed());
        let refused = d
            .allow(FAMILY_WEB_SEARCH, "duckduckgo", 3, 3)
            .await
            .unwrap();
        assert!(!refused.allowed()); // insufficient-allowance, journaled
        let entries_before = d.snapshot().entries.len();
        drop(d);

        let mut r = SpendDecider::restore("run-test", "hash", &allowance_5_2(), &journal)
            .expect("restore replays the ledger");
        // Replayed state: the live meters reflect the entries, not the
        // stored totals (which must agree with them — checked below).
        assert_eq!(r.remaining(FAMILY_WEB_SEARCH, "duckduckgo"), 2);
        assert_eq!(r.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES), 1);
        // Restore itself adds nothing to the journal (continuity, not
        // mutation) — the next spend lands on the SAME ledger.
        let after_restore = r.snapshot();
        assert_eq!(after_restore.entries.len(), entries_before);
        assert_eq!(after_restore.spent.get("web-search:duckduckgo"), Some(&3));
        // The run continues: 2 more search allows, then exhausted.
        assert!(r
            .allow(FAMILY_WEB_SEARCH, "duckduckgo", 1, 4)
            .await
            .unwrap()
            .allowed());
        assert!(r
            .allow(FAMILY_WEB_SEARCH, "duckduckgo", 1, 5)
            .await
            .unwrap()
            .allowed());
        let exhausted = r
            .allow(FAMILY_WEB_SEARCH, "duckduckgo", 1, 6)
            .await
            .unwrap();
        assert!(
            matches!(exhausted, SpendVerdict::Refuse { ref reason, .. } if reason == "no-allowance-or-exhausted")
        );
        // The continued spends journaled onto the same ledger file.
        let on_disk: BudgetLedger =
            serde_json::from_str(&std::fs::read_to_string(&journal).unwrap()).unwrap();
        assert!(on_disk.entries.len() > entries_before);
        assert_eq!(on_disk.spent.get("web-search:duckduckgo"), Some(&5));
        assert_eq!(on_disk.run_id, "run-test");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn restore_refuses_missing_journal() {
        let tmp = std::env::temp_dir().join(format!("dr-budget-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let e = SpendDecider::restore(
            "run-test",
            "hash",
            &allowance_5_2(),
            &tmp.join("budget-ledger.json"),
        )
        .err()
        .unwrap();
        assert!(e.contains("spends blind"), "{e}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn restore_refuses_foreign_ledger() {
        let tmp = std::env::temp_dir().join(format!("dr-budget-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let journal = tmp.join("budget-ledger.json");
        write_ledger(&journal, &honest_ledger());

        let e = SpendDecider::restore("run-other", "hash", &allowance_5_2(), &journal)
            .err()
            .unwrap();
        assert!(e.contains("foreign"), "{e}");
        let e = SpendDecider::restore("run-test", "other-hash", &allowance_5_2(), &journal)
            .err()
            .unwrap();
        assert!(e.contains("charter hash"), "{e}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn restore_refuses_tampered_ledger() {
        let tmp = std::env::temp_dir().join(format!("dr-budget-tampered-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let journal = tmp.join("budget-ledger.json");

        // Allowance that no longer matches the charter.
        write_ledger(&journal, &honest_ledger());
        let mut wrong = HashMap::new();
        wrong.insert(format!("{FAMILY_WEB_SEARCH}:duckduckgo"), 99);
        let e = SpendDecider::restore("run-test", "hash", &wrong, &journal)
            .err()
            .unwrap();
        assert!(e.contains("tampered"), "{e}");

        // Stored totals that disagree with the journal's own entries.
        let mut l = honest_ledger();
        l.spent.insert("web-search:duckduckgo".to_string(), 2); // entries say 1
        write_ledger(&journal, &l);
        let e = SpendDecider::restore("run-test", "hash", &allowance_5_2(), &journal)
            .err()
            .unwrap();
        assert!(e.contains("disagree"), "{e}");

        // An allow on a meter the charter never granted.
        let mut l = honest_ledger();
        l.entries.push(BudgetEntry {
            family: FAMILY_WEB_SEARCH.to_string(),
            key: "brave".to_string(),
            units: 1,
            at_unix: 2,
            decision: "allow".to_string(),
            reason: None,
        });
        write_ledger(&journal, &l);
        let e = SpendDecider::restore("run-test", "hash", &allowance_5_2(), &journal)
            .err()
            .unwrap();
        assert!(e.contains("never granted"), "{e}");

        // Spend beyond the grant.
        let mut l = honest_ledger();
        l.entries.push(BudgetEntry {
            family: FAMILY_WEB_SEARCH.to_string(),
            key: "duckduckgo".to_string(),
            units: 99,
            at_unix: 2,
            decision: "allow".to_string(),
            reason: None,
        });
        write_ledger(&journal, &l);
        let e = SpendDecider::restore("run-test", "hash", &allowance_5_2(), &journal)
            .err()
            .unwrap();
        assert!(e.contains("beyond"), "{e}");

        // An unknown decision.
        let mut l = honest_ledger();
        l.entries[0].decision = "maybe".to_string();
        write_ledger(&journal, &l);
        let e = SpendDecider::restore("run-test", "hash", &allowance_5_2(), &journal)
            .err()
            .unwrap();
        assert!(e.contains("unknown decision"), "{e}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
