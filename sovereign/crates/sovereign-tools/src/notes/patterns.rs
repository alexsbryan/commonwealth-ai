// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tool-call pattern matcher (Phase 7.1).
//!
//! Observes sequences and gaps in the agent's MCP tool calls and
//! writes a `source='observed'` note when a recognised pattern
//! fires. The audit's "Observed patterns" section reads these
//! notes — the user gets a paragraph like "you investigated the
//! impact of `process_request` before modifying it" without the
//! agent ever calling `note(...)` explicitly.
//!
//! ## Why a sliding window
//!
//! Each pattern looks at the last N tool calls. We keep the window
//! short (default 8) because:
//!
//! - Patterns describe a workflow shape, not a long-term trend; a
//!   noisy "lots of `callers` calls over an hour" mostly catches
//!   the agent looking around, not a discrete decision.
//! - Storage is `tool_call_log`, which is a 10k-row ring buffer.
//!   We don't want to scan it every tool call.
//!
//! ## "File write" proxies
//!
//! The patterns described in the plan (and the surrounding spec
//! doc) reference "file write" — the agent doing an Edit/Write
//! against the user's source tree. Those edits happen through
//! Claude Code's own toolchain and don't flow through MCP, so we
//! can't observe them directly. We use practical proxies:
//!
//! - `build` — the agent invoking `build` strongly implies a
//!   recent edit (otherwise the build would still be passing).
//! - `note` — the agent writing a note signals "I think I just
//!   made a decision worth recording," which is the structural
//!   moment a Phase 7.2 diff_extract would also pick up.
//!
//! Phase 7.2 lands a real diff-based extractor; until then the
//! proxies catch the most-common shapes well enough that the
//! audit's "non-empty floor" contract holds.
//!
//! ## Idempotency / dedup
//!
//! Each pattern firing produces one note. Patterns that fire on a
//! sequence ("blast → build") will re-fire if the agent does the
//! same dance in a fresh window — that's correct, those are two
//! discrete decisions. Pattern 3 (the gap pattern) is the
//! exception: it fires once when the agent *enters* the
//! "investigation without action" state and won't re-fire while
//! they're still in it. The matcher tracks state across calls via
//! the per-rule cooldown helpers below.

use std::collections::HashMap;
use std::sync::Arc;

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore, ToolCallLogRow};

/// Observation window — how many recent log rows we scan when
/// `observe()` runs. 8 is a deliberate trade-off:
///
/// - Long enough to catch reasonable "investigated, then acted"
///   sequences (typically 2–5 calls).
/// - Short enough that each `observe` call is cheap (one
///   `tool_call_log_rows(0, 8)` SQL query).
const WINDOW: usize = 8;

/// Cooldown for the "investigation without action" gap pattern —
/// don't fire it twice in a row inside the same dry spell. Counted
/// in *number of subsequent observe() calls*, not seconds: the
/// matcher fires on the call that crosses the threshold and stays
/// quiet until either an action breaks the streak or this many
/// further dry calls pass.
const ISOLATED_INVESTIGATION_COOLDOWN: u32 = 6;

/// What a recognised pattern wants to record. The matcher
/// returns these from `observe`; the caller (mcp_router) is
/// responsible for turning them into `NoteStore::write_note_with_source`
/// invocations. Splitting the I/O from the recognition keeps the
/// matcher pure and unit-testable without a real database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedPattern {
    /// Stable identifier for the rule. Useful for telemetry and
    /// for tests asserting "exactly this pattern fired."
    pub rule: PatternRule,
    /// Body of the note we'll write. Plain prose, ready for the
    /// audit's "Observed patterns" section.
    pub message: String,
    /// Tool names that contributed to the match — recorded in the
    /// note's `symbols` column so the audit can group by subject.
    pub tools: Vec<String>,
}

/// Rules implemented today. Stable identifiers keep telemetry
/// across versions even if we tweak the wording or window. Add
/// new rules by extending this enum and adding a corresponding
/// branch to [`ToolPatternMatcher::scan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatternRule {
    /// `blast` or `callers` followed by `build` inside the window
    /// — proxy for "investigated impact, then edited and built."
    InvestigateThenAct,
    /// `build` immediately preceded by another tool call, signal
    /// that the agent built after touching something. Currently
    /// merges into `InvestigateThenAct` when applicable; kept as a
    /// distinct rule slot so a future refinement can split them.
    BuildFollowsAction,
    /// `callers`, `callees`, or `symbols` runs of >= 3 with no
    /// `build` or `note` in between — a "looking around without
    /// committing" signal. Cooldown'd so it doesn't spam.
    IsolatedInvestigation,
    /// `spec` followed by `build` inside the window — proxy for
    /// "read the spec, then implemented and built it."
    SpecThenBuild,
    /// `notes` (the read-tool) followed by `note` (the write-tool)
    /// inside the window — the agent referenced an existing note
    /// when composing a new decision.
    NotesInformedDecision,
}

impl PatternRule {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvestigateThenAct => "investigate_then_act",
            Self::BuildFollowsAction => "build_follows_action",
            Self::IsolatedInvestigation => "isolated_investigation",
            Self::SpecThenBuild => "spec_then_build",
            Self::NotesInformedDecision => "notes_informed_decision",
        }
    }
}

/// Per-session cooldown state. Phase 7.1 uses this to suppress
/// the `IsolatedInvestigation` rule from firing repeatedly while
/// the agent stays in the same "looking around" mode.
#[derive(Debug, Default)]
struct SessionState {
    /// `(rule, calls_remaining_before_re_eligible)` for any rule
    /// currently on cooldown. When `calls_remaining` decrements to
    /// zero, the entry is removed and the rule can fire again.
    cooldowns: HashMap<PatternRule, u32>,
}

/// Stateful pattern matcher. One instance per running MCP server
/// (the embedded daemon's `mcp_router` and the standalone serve
/// each construct one). Holds an `Arc<NoteStore>` so callers can
/// shoot-and-forget through `observe_and_record`; tests use the
/// pure [`scan`](Self::scan) function which returns the matched
/// patterns without touching the DB.
pub struct ToolPatternMatcher {
    notes: Arc<NoteStore>,
    state: Arc<tokio::sync::Mutex<HashMap<String, SessionState>>>,
}

impl ToolPatternMatcher {
    pub fn new(notes: Arc<NoteStore>) -> Self {
        Self {
            notes,
            state: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Public re-export of [`scan`] used by Phase 7.3's
    /// `audit --recover` path. Same contract: pure, takes the
    /// session's rows newest-first and a mutable cooldown map,
    /// returns the recognised patterns. Recovery callers pass a
    /// fresh empty cooldown map so no rule is suppressed by stale
    /// in-process state.
    pub fn scan_for_recovery(
        rows_newest_first: &[&ToolCallLogRow],
        cooldowns: &mut HashMap<PatternRule, u32>,
    ) -> Vec<ObservedPattern> {
        Self::scan(rows_newest_first, cooldowns)
    }

    /// Pure scan: given the most-recent tool-call rows for one
    /// session (newest first — matches `tool_call_log_rows`'s
    /// natural order) and the current cooldown set, return the
    /// patterns that fire and an updated cooldown set. Caller is
    /// responsible for writing notes and storing the new state.
    ///
    /// Pulled out so tests don't need a real `NoteStore`.
    fn scan(
        rows_newest_first: &[&ToolCallLogRow],
        cooldowns: &mut HashMap<PatternRule, u32>,
    ) -> Vec<ObservedPattern> {
        // We reason in newest-last (chronological) order — easier to
        // describe "X then Y."
        let mut chronological: Vec<&ToolCallLogRow> = rows_newest_first.to_vec();
        chronological.reverse();

        // Successful calls only. An `error`/`empty_result` outcome
        // doesn't tell us the agent acted on the tool; skip it for
        // pattern matching but DO count it toward cooldown decrement
        // (otherwise an agent that errored its way through wouldn't
        // make progress on its `IsolatedInvestigation` window).
        let successful: Vec<&ToolCallLogRow> = chronological
            .iter()
            .copied()
            .filter(|r| r.outcome == "success")
            .collect();

        // Decrement cooldowns by 1 per scan, dropping any that hit zero.
        cooldowns.retain(|_, remaining| {
            if *remaining > 1 {
                *remaining -= 1;
                true
            } else {
                false
            }
        });

        let mut hits: Vec<ObservedPattern> = Vec::new();
        if successful.is_empty() {
            return hits;
        }

        // Most-recent tool name — drives most of the rules.
        let last = successful.last().expect("non-empty");
        let last_name = last.tool_name.as_str();

        // ── Rule 1 / 2: investigate → build, build follows action ──
        //
        // If the most-recent successful call is `build` and the
        // window contains an earlier `blast`, `callers`, `callees`,
        // or `symbols` call, fire InvestigateThenAct.
        if last_name == "build" {
            // Look at everything before the build.
            let before_build: Vec<&&ToolCallLogRow> = successful
                .iter()
                .take(successful.len().saturating_sub(1))
                .collect();
            let investigative: Vec<String> = before_build
                .iter()
                .filter(|r| matches_investigation(&r.tool_name))
                .map(|r| r.tool_name.clone())
                .collect();
            if !investigative.is_empty() {
                let mut tools: Vec<String> = investigative.to_vec();
                tools.push("build".into());
                tools.dedup();
                hits.push(ObservedPattern {
                    rule: PatternRule::InvestigateThenAct,
                    message: format!(
                        "Investigated impact ({}) before running `build`.",
                        investigative.join(", ")
                    ),
                    tools,
                });
            } else if !before_build.is_empty() {
                // BuildFollowsAction: there was *some* prior call,
                // just not an investigative one. Quieter signal.
                hits.push(ObservedPattern {
                    rule: PatternRule::BuildFollowsAction,
                    message: "Ran `build` after a previous tool call.".into(),
                    tools: vec!["build".into()],
                });
            }
        }

        // ── Rule 3: isolated investigation ─────────────────────────
        //
        // 3+ recent successful calls are all in {callers, callees,
        // symbols} and there's no `build` or `note` in the window.
        // Fires once per cooldown window so we don't spam the
        // observed-patterns section.
        let invest_count = successful
            .iter()
            .filter(|r| matches_investigation(&r.tool_name))
            .count();
        let action_count = successful
            .iter()
            .filter(|r| r.tool_name == "build" || r.tool_name == "note")
            .count();
        if invest_count >= 3
            && action_count == 0
            && !cooldowns.contains_key(&PatternRule::IsolatedInvestigation)
        {
            let invest_tools: Vec<String> = successful
                .iter()
                .filter(|r| matches_investigation(&r.tool_name))
                .map(|r| r.tool_name.clone())
                .collect::<Vec<_>>();
            // Keep the order; dedup runs of the same name.
            let mut deduped = Vec::new();
            for n in &invest_tools {
                if deduped.last() != Some(n) {
                    deduped.push(n.clone());
                }
            }
            hits.push(ObservedPattern {
                rule: PatternRule::IsolatedInvestigation,
                // Stable lesson text — the specific tools that fired vary run
                // to run and were previously interpolated into the message,
                // which fragmented one lesson into N near-duplicate rows. The
                // tools live in `tools` (→ the note's symbols) where the detail
                // belongs; the message stays identical so the store-level dedup
                // below can actually collapse re-emits.
                message: "Three or more code-intel calls with no `build` or `note` follow-up."
                    .to_string(),
                tools: deduped.clone(),
            });
            cooldowns.insert(
                PatternRule::IsolatedInvestigation,
                ISOLATED_INVESTIGATION_COOLDOWN,
            );
        }

        // ── Rule 4: spec → build ───────────────────────────────────
        if last_name == "build" {
            let saw_spec_before = successful
                .iter()
                .take(successful.len().saturating_sub(1))
                .any(|r| r.tool_name == "spec");
            if saw_spec_before {
                hits.push(ObservedPattern {
                    rule: PatternRule::SpecThenBuild,
                    message: "Read the spec, then ran `build`.".into(),
                    tools: vec!["spec".into(), "build".into()],
                });
            }
        }

        // ── Rule 5: notes-informed decision ────────────────────────
        if last_name == "note" {
            let saw_notes_before = successful
                .iter()
                .take(successful.len().saturating_sub(1))
                .any(|r| r.tool_name == "notes");
            if saw_notes_before {
                hits.push(ObservedPattern {
                    rule: PatternRule::NotesInformedDecision,
                    message: "Queried `notes` then wrote a new `note` — \
                              decision informed by prior recorded context."
                        .into(),
                    tools: vec!["notes".into(), "note".into()],
                });
            }
        }

        hits
    }

    /// Inspect the latest tool calls for `session_id`, fire any
    /// matched patterns, and write a `source='observed'` note for
    /// each. `feature_id` (if any) is taken from the request
    /// context so the note is scoped correctly.
    ///
    /// Errors are swallowed (logged at warn level) — pattern
    /// recording must never affect the outer tool-call response.
    pub async fn observe_and_record(&self, session_id: &str, feature_id: Option<&str>) {
        // Pull the recent rows for this session. Logging may have
        // happened on a different thread; use a generous since=0.
        let rows: Vec<ToolCallLogRow> = match self.notes.tool_call_log_rows(0, WINDOW).await {
            Ok(rs) => rs
                .into_iter()
                .filter(|r| r.session_id == session_id)
                .collect(),
            Err(e) => {
                tracing::warn!(
                    session_id,
                    error = %e,
                    "tool_pattern_matcher: failed to read tool_call_log; skipping observation"
                );
                return;
            }
        };
        let row_refs: Vec<&ToolCallLogRow> = rows.iter().collect();

        // Take the per-session cooldown state, run scan, store back.
        let mut state_lock = self.state.lock().await;
        let session_state = state_lock.entry(session_id.to_string()).or_default();
        let hits = Self::scan(&row_refs, &mut session_state.cooldowns);
        // Drop the lock before we touch the DB so a slow write
        // doesn't block other sessions' observation.
        drop(state_lock);

        for hit in hits {
            let scope = if feature_id.is_some() {
                NoteScope::Feature
            } else {
                NoteScope::Global
            };
            // Emitter idempotency. The `cooldowns` map above is per-session and
            // in-memory: it resets on a new session and on every daemon restart,
            // so on its own it re-files the same observed lesson indefinitely
            // (measured: 7 copies of the isolated-investigation reflection). The
            // store's own content_hash dedup can't catch this — the hash folds
            // in session_id, so cross-session re-emits hash differently. So we
            // ask the store directly: is there already an active observed
            // reflection with this exact content? If so, the lesson is recorded;
            // skip. A failed check is non-fatal — fall through and let the write
            // path's hash dedup be the backstop rather than dropping the note.
            match self
                .notes
                .has_active_note_with_content("reflection", &hit.message, NoteSource::Observed)
                .await
            {
                Ok(true) => {
                    tracing::debug!(
                        session_id,
                        rule = hit.rule.as_str(),
                        "tool_pattern_matcher: active observed reflection already present; skipping re-file"
                    );
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        session_id,
                        rule = hit.rule.as_str(),
                        error = %e,
                        "tool_pattern_matcher: observed-dup check failed; proceeding to write"
                    );
                }
            }
            // Record under kind="reflection" so existing readers
            // that filter by kind in the audit's "observed patterns"
            // section pick them up. The discriminator that the audit
            // uses is `source='observed'`, not the kind.
            if let Err(e) = self
                .notes
                .write_note_with_source(
                    "reflection",
                    &hit.message,
                    hit.tools.clone(),
                    Vec::new(),
                    session_id,
                    scope,
                    feature_id,
                    None,
                    NoteSource::Observed,
                    None,
                )
                .await
            {
                tracing::warn!(
                    session_id,
                    rule = hit.rule.as_str(),
                    error = %e,
                    "tool_pattern_matcher: failed to write observed note"
                );
            }
        }
    }
}

/// True for the read-only code-intel tools that count as
/// "investigation" for pattern 1 / 3.
fn matches_investigation(tool_name: &str) -> bool {
    matches!(tool_name, "blast" | "callers" | "callees" | "symbols")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(session: &str, tool: &str, outcome: &str, called_at: i64) -> ToolCallLogRow {
        ToolCallLogRow {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session.into(),
            tool_name: tool.into(),
            outcome: outcome.into(),
            called_at,
        }
    }

    /// Helper: scan a chronological-order vec (oldest → newest)
    /// the same way `observe_and_record` does, returning the hits
    /// without DB I/O.
    fn scan_chronological(
        chronological: &[ToolCallLogRow],
        cooldowns: &mut HashMap<PatternRule, u32>,
    ) -> Vec<ObservedPattern> {
        // tool_call_log_rows returns newest-first, so reverse here
        // to mirror what the production code receives.
        let mut newest_first: Vec<&ToolCallLogRow> = chronological.iter().collect();
        newest_first.reverse();
        ToolPatternMatcher::scan(&newest_first, cooldowns)
    }

    /// Pattern 1: `blast` followed by `build` fires
    /// `InvestigateThenAct` and includes both tool names.
    #[test]
    fn rule1_blast_then_build_fires_investigate_then_act() {
        let log = vec![
            row("s", "blast", "success", 10),
            row("s", "build", "success", 20),
        ];
        let mut cooldowns = HashMap::new();
        let hits = scan_chronological(&log, &mut cooldowns);
        assert_eq!(hits.len(), 1, "expected one pattern fire, got {hits:?}");
        assert_eq!(hits[0].rule, PatternRule::InvestigateThenAct);
        assert!(hits[0].tools.contains(&"blast".into()));
        assert!(hits[0].tools.contains(&"build".into()));
    }

    /// Pattern 2 (BuildFollowsAction): `build` after a non-investigative
    /// call (e.g. `note`) fires the quieter rule, not the louder one.
    #[test]
    fn rule2_build_after_note_fires_build_follows_action() {
        let log = vec![
            row("s", "note", "success", 10),
            row("s", "build", "success", 20),
        ];
        let mut cooldowns = HashMap::new();
        let hits = scan_chronological(&log, &mut cooldowns);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rule, PatternRule::BuildFollowsAction);
    }

    /// Pattern 3: 3+ callers/callees/symbols calls with no
    /// build/note fire IsolatedInvestigation once. Subsequent
    /// observation under the cooldown does NOT re-fire.
    #[test]
    fn rule3_isolated_investigation_fires_once_under_cooldown() {
        let log = vec![
            row("s", "callers", "success", 10),
            row("s", "callees", "success", 20),
            row("s", "symbols", "success", 30),
        ];
        let mut cooldowns = HashMap::new();
        let hits = scan_chronological(&log, &mut cooldowns);
        assert_eq!(hits.len(), 1, "first scan should fire once: {hits:?}");
        assert_eq!(hits[0].rule, PatternRule::IsolatedInvestigation);
        assert!(cooldowns.contains_key(&PatternRule::IsolatedInvestigation));

        // Run again with no new acting tool — cooldown blocks the
        // second fire.
        let hits2 = scan_chronological(&log, &mut cooldowns);
        assert!(
            hits2
                .iter()
                .all(|h| h.rule != PatternRule::IsolatedInvestigation),
            "cooldown should suppress re-firing of IsolatedInvestigation: {hits2:?}"
        );
    }

    /// Pattern 4: `spec` → `build` fires SpecThenBuild
    /// alongside (not instead of) the InvestigateThenAct check.
    /// Both can fire on the same window when both conditions hold.
    #[test]
    fn rule4_spec_then_build_fires_spec_then_build() {
        let log = vec![
            row("s", "spec", "success", 10),
            row("s", "build", "success", 20),
        ];
        let mut cooldowns = HashMap::new();
        let hits = scan_chronological(&log, &mut cooldowns);
        // SpecThenBuild fires; BuildFollowsAction may also fire
        // (build after spec) but that's a quieter co-occurrence we
        // don't suppress.
        assert!(
            hits.iter().any(|h| h.rule == PatternRule::SpecThenBuild),
            "expected SpecThenBuild in {hits:?}"
        );
    }

    /// Pattern 5: `notes` (read) → `note` (write) fires
    /// NotesInformedDecision, signalling the agent referenced
    /// existing context.
    #[test]
    fn rule5_notes_then_note_fires_notes_informed_decision() {
        let log = vec![
            row("s", "notes", "success", 10),
            row("s", "note", "success", 20),
        ];
        let mut cooldowns = HashMap::new();
        let hits = scan_chronological(&log, &mut cooldowns);
        assert!(
            hits.iter()
                .any(|h| h.rule == PatternRule::NotesInformedDecision),
            "expected NotesInformedDecision in {hits:?}"
        );
    }

    /// Errors don't trigger sequence patterns. The "isolated
    /// investigation" rule still notices them (we want to see
    /// "tried 3 things, all errored" too) — but the bias is to
    /// NOT spuriously fire investigate→act on a failed call.
    #[test]
    fn errored_calls_do_not_match_sequence_patterns() {
        let log = vec![
            row("s", "blast", "error", 10),
            row("s", "build", "success", 20),
        ];
        let mut cooldowns = HashMap::new();
        let hits = scan_chronological(&log, &mut cooldowns);
        // The errored `blast` is filtered out, so the build sees
        // only itself in the successful set — should fire the
        // BuildFollowsAction quieter rule, not InvestigateThenAct.
        assert!(
            !hits
                .iter()
                .any(|h| h.rule == PatternRule::InvestigateThenAct),
            "errored investigation should not trigger InvestigateThenAct: {hits:?}"
        );
    }

    /// Empty log → no patterns, no panic.
    #[test]
    fn empty_log_yields_no_patterns() {
        let log: Vec<ToolCallLogRow> = Vec::new();
        let mut cooldowns = HashMap::new();
        let hits = scan_chronological(&log, &mut cooldowns);
        assert!(hits.is_empty());
    }

    /// The cooldown decrements on each scan; once it expires the
    /// rule can fire again. We simulate that by running the scan
    /// repeatedly with the same window — the rule should re-fire
    /// after `ISOLATED_INVESTIGATION_COOLDOWN` empty scans.
    #[test]
    fn cooldown_decrements_and_eventually_re_enables() {
        let mut cooldowns = HashMap::new();
        cooldowns.insert(PatternRule::IsolatedInvestigation, 3);

        // Scan with a `build` in the window — no fires (cooldown
        // suppresses isolated, build absent doesn't matter), but
        // cooldown decrements.
        let log = vec![row("s", "callers", "success", 10)];
        for _ in 0..3 {
            scan_chronological(&log, &mut cooldowns);
        }
        assert!(
            !cooldowns.contains_key(&PatternRule::IsolatedInvestigation),
            "cooldown should have expired after 3 scans"
        );
    }

    // ─── observe_and_record (DB) integration ──────────────────────

    /// `observe_and_record` writes a `source='observed'` note
    /// when a pattern matches, and the note is reachable via the
    /// store's normal read APIs (filtered by source post-fetch
    /// since the existing reader doesn't take a source predicate
    /// directly).
    #[tokio::test]
    async fn observe_and_record_persists_observed_note() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());
        // Seed the log with a blast→build sequence.
        store
            .log_tool_call("sess-7", "blast", "success")
            .await
            .unwrap();
        store
            .log_tool_call("sess-7", "build", "success")
            .await
            .unwrap();

        let matcher = ToolPatternMatcher::new(Arc::clone(&store));
        matcher.observe_and_record("sess-7", None).await;

        // The store doesn't expose a source-filtered read directly;
        // query reflection-kind notes and filter to source=Observed.
        let rows = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 100, false)
            .await
            .unwrap();
        let observed: Vec<_> = rows
            .iter()
            .filter(|n| n.source == NoteSource::Observed.as_str())
            .collect();
        assert!(
            !observed.is_empty(),
            "expected at least one observed-source note after blast→build, got {} reflections",
            rows.len()
        );
        let body = observed[0].content.to_lowercase();
        assert!(
            body.contains("investigated") || body.contains("blast"),
            "observed note body should describe the pattern: {body}"
        );
    }
}
