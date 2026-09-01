// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live snapshot — the run-dir poller and the change-gated emit.
//!
//! Carved out of `deep_research_commands.rs` (ARCH §3.1).

use super::*;

/// Everything the live view shows, read from the run dir (the single state
/// source). `None` before the charter exists (the loop writes it first).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DrLiveSnapshot {
    pub(super) round: Option<u32>,
    pub(super) max_rounds: Option<u32>,
    pub(super) stage: String,
    pub(super) gaps: Vec<DrGap>,
    pub(super) budget: DrBudget,
    pub(super) consent: Option<DrConsent>,
}

/// Polls the run dir. `snapshot()` re-reads the artifacts on every call
/// (cheap — a handful of small JSON files); the caller decides whether the
/// snapshot CHANGED before emitting, so the channel stays lean.
pub(super) struct RunDirPoller {
    run_dir: PathBuf,
}

impl RunDirPoller {
    pub(super) fn new(run_dir: PathBuf) -> Self {
        Self { run_dir }
    }

    pub(super) fn report_md(&self) -> Option<PathBuf> {
        let p = self.run_dir.join("report.md");
        p.is_file().then_some(p)
    }

    pub(super) fn snapshot(&self) -> Option<DrLiveSnapshot> {
        let dir = &self.run_dir;
        // The charter (with the consent grant) is the first artifact; until
        // it exists there is no run state to show.
        let charter_raw = std::fs::read(dir.join("charter.json")).ok()?;
        let charter: Charter = serde_json::from_slice(&charter_raw).ok()?;

        // Round + stage, derived from which artifacts exist: the newest
        // gap-list-<round>.json names the current round; verdict-set.json
        // means the writing is being checked; report.md means done.
        let mut round: Option<u32> = None;
        let mut gaps: Vec<DrGap> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut lists: Vec<(u32, PathBuf)> = rd
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let rest = name.strip_prefix("gap-list-")?.strip_suffix(".json")?;
                    Some((rest.parse::<u32>().ok()?, e.path()))
                })
                .collect();
            lists.sort_by_key(|(r, _)| *r);
            if let Some((r, path)) = lists.last() {
                round = Some(*r);
                if let Ok(raw) = std::fs::read(path) {
                    if let Ok(list) = serde_json::from_slice::<GapList>(&raw) {
                        gaps = list
                            .gaps
                            .into_iter()
                            .map(|g| DrGap {
                                id: g.id,
                                text: g.text,
                            })
                            .collect();
                    }
                }
            }
        }
        let stage = (if self.report_md().is_some() {
            "done"
        } else if dir.join("verdict-set.json").is_file() {
            "checking"
        } else if round.is_some() {
            "rounding"
        } else {
            "planning"
        })
        .to_string();

        // Budget ledger — written from birth.
        let mut budget = DrBudget::default();
        if let Ok(raw) = std::fs::read(dir.join("budget-ledger.json")) {
            if let Ok(ledger) = serde_json::from_slice::<BudgetLedger>(&raw) {
                budget = DrBudget {
                    spent: ledger.spent.into_iter().collect(),
                    remaining: ledger.remaining.into_iter().collect(),
                };
            }
        }

        // Consent-grant status from the charter (live, not close-time).
        let max_rounds = charter.charter.max_rounds;
        let consent = charter.charter.consent.map(|c| DrConsent {
            release_floor: c.release_floor.as_str().to_string(),
            granted_at_unix: c.granted_at_unix,
        });

        Some(DrLiveSnapshot {
            round,
            max_rounds: Some(max_rounds),
            stage,
            gaps,
            budget,
            consent,
        })
    }
}

/// Emit a `Live` event only when the run dir actually moved. Returns
/// whether it did, which is what the heartbeat's quiet clock counts from —
/// so "nothing has changed for 4 minutes" is a measured fact rather than
/// an inference from an absent event.
pub(super) fn emit_if_changed(
    app: &AppHandle,
    channel: &str,
    snapshot: Option<DrLiveSnapshot>,
    last: &mut Option<DrLiveSnapshot>,
) -> bool {
    if snapshot.is_none() || *last == snapshot {
        return false;
    }
    *last = snapshot.clone();
    if let Some(s) = snapshot {
        let _ = app.emit(
            channel,
            DeepResearchRunEvent::Live {
                round: s.round,
                max_rounds: s.max_rounds,
                stage: s.stage,
                gaps: s.gaps,
                budget: s.budget,
                consent: s.consent,
            },
        );
    }
    true
}
