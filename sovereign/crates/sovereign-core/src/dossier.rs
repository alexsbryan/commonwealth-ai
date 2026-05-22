//! Tool-Mastery Framework — Layer 2: tool dossier (ambient context).
//!
//! A per-turn block of ambient context that puts the model on the
//! same page as a human collaborator would be after one minute of
//! orientation: which tools are even on the table, what they've
//! tried already in this conversation, and (eventually) what state
//! the workspace is in. Built as a Fast-slot pre-pass and spliced
//! into the system message by `runtime::build_system_message`.
//!
//! The shape mirrors the prior pre-pass/splice pattern shipped for
//! the today-anchor, landscape digests, and temporal tensions:
//!
//! 1. A pre-pass function ([`compute_tool_dossier`]) gathers data
//!    that the synthesis path will need.
//! 2. The result is stashed on `ConversationContext.tool_dossier`.
//! 3. The system-message builder renders the field if present.
//!
//! That keeps each layer's responsibility clean (gather vs. render)
//! and lets unit tests exercise the render path with a synthetic
//! dossier — no NoteStore or registry plumbing needed.
//!
//! Relational skills (`inner-work`) intentionally get *no* dossier
//! — reflective work is not tool-mediated and a "tools available"
//! list would prime the wrong frame. The gate lives at the
//! pre-pass call site so the splice never sees a dossier it has to
//! reject.

use corpus_engine::NoteStore;

use crate::intent_policy;
use crate::memory::{read_recent_tool_decisions, ToolDecisionOutcome};
use crate::registry::ToolRegistry;
use crate::skills::{Skill, SkillRegister};
use crate::types::{ToolDossier, ToolDossierEntry, ToolDossierOutcome};

/// Cap on `tools_available` entries rendered into the prompt.
/// Catalogs over this size truncate to the top N (by registration
/// order). 24 covers every bundled skill's narrowed catalog with
/// headroom; bigger means the dossier itself starts crowding the
/// system message.
pub const MAX_DOSSIER_TOOLS: usize = 24;

/// Cap on `outcome_history` entries pulled from the NoteStore.
/// Older outcomes drop off — the model needs the recent texture,
/// not the conversation's full history.
pub const MAX_DOSSIER_OUTCOMES: usize = 8;

/// Build a tool dossier for the active skill. Returns `None` for
/// the relational register (`inner-work`), preserving the
/// "reflective work has no tool dossier" invariant; otherwise
/// returns `Some(ToolDossier)` even when the outcome history is
/// empty (the model still benefits from the narrowed tool list).
pub async fn compute_tool_dossier(
    tools: &ToolRegistry,
    notes: Option<&NoteStore>,
    active_skill: Option<&Skill>,
    conversation_id: Option<&str>,
) -> Option<ToolDossier> {
    // Inner-work gate: relational mode doesn't get a dossier. Run
    // before any I/O so the NoteStore stays untouched on this
    // path.
    if matches!(
        active_skill.map(|s| s.inference.register),
        Some(SkillRegister::Relational)
    ) {
        tracing::debug!(
            skill = active_skill.map(|s| s.id.as_str()),
            "dossier:skip_relational"
        );
        return None;
    }

    // Mode-only narrowing — the dossier renders BEFORE the router
    // has classified an intent for this turn, so it shows the
    // surface-wide catalog (full for default chat, mode-narrowed
    // for recipe-author). Matches the policy the router itself
    // sees at `runtime::narrow_tools_pre_classification`.
    let mode_policy = intent_policy::policy_for_mode_only(
        active_skill
            .map(|s| s.inference.register)
            .unwrap_or(SkillRegister::Factual),
        active_skill.map(|s| s.id.as_str()),
    );
    let narrowed = intent_policy::narrow_tools(&tools.descriptors(), &mode_policy);

    let tools_available: Vec<ToolDossierEntry> = narrowed
        .into_iter()
        .take(MAX_DOSSIER_TOOLS)
        .map(|d| ToolDossierEntry {
            tool_id: d.id,
            description: d.description,
        })
        .collect();

    // Outcome history: best-effort. A NoteStore-missing or read-
    // failure path returns an empty Vec — the dossier still
    // renders its tools-available section.
    let outcome_history: Vec<ToolDossierOutcome> = match notes {
        Some(store) => {
            match read_recent_tool_decisions(store, conversation_id, MAX_DOSSIER_OUTCOMES).await {
                Ok(payloads) => payloads
                    .into_iter()
                    .map(|p| ToolDossierOutcome {
                        tool_id: p.tool_id,
                        outcome: p.outcome.as_str().to_string(),
                        reasoning: p.reasoning,
                        applied_at_unix: p.applied_at_unix,
                        summary: p.summary,
                        evidence_ids: p.evidence_ids,
                        turn_index: p.turn_index,
                    })
                    .collect(),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "dossier:read_outcomes_failed — rendering tools-only dossier"
                    );
                    Vec::new()
                }
            }
        }
        None => Vec::new(),
    };

    tracing::debug!(
        skill = active_skill.map(|s| s.id.as_str()),
        tools = tools_available.len(),
        outcomes = outcome_history.len(),
        "dossier:built"
    );

    Some(ToolDossier {
        active_skill_id: active_skill.map(|s| s.id.clone()),
        tools_available,
        outcome_history,
        ambient_state: None,
    })
}

/// Render the dossier into the system-message block. Stable shape:
/// two clearly-headered sections (tools + history) optionally
/// followed by ambient-state when wired. Empty `outcome_history`
/// renders the section as "no prior tool outcomes recorded this
/// conversation" so the model has an explicit "fresh start" signal
/// rather than guessing.
pub fn render_tool_dossier(dossier: &ToolDossier, now_unix: i64) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Section 1 — tools available.
    let mut tools_block = String::from("Tools available for this turn:");
    if dossier.tools_available.is_empty() {
        tools_block.push_str(
            "\n(no tools registered — the active skill expects pure-LLM reasoning)",
        );
    } else {
        for entry in &dossier.tools_available {
            let desc = entry.description.trim();
            if desc.is_empty() {
                tools_block.push_str(&format!("\n- {}", entry.tool_id));
            } else {
                tools_block.push_str(&format!("\n- {}: {desc}", entry.tool_id));
            }
        }
    }
    if let Some(skill_id) = &dossier.active_skill_id {
        tools_block.push_str(&format!("\n(narrowed for skill: {skill_id})"));
    }
    parts.push(tools_block);

    // Section 2 — outcome history this conversation.
    let mut history_block = String::from("Outcome history this conversation:");
    if dossier.outcome_history.is_empty() {
        history_block.push_str(
            "\n(no prior tool outcomes recorded — first tool call this conversation)",
        );
    } else {
        for outcome in &dossier.outcome_history {
            let age = format_age_since(now_unix, outcome.applied_at_unix);
            let reasoning = outcome.reasoning.trim();
            // Tier 1 result-memory rendering: when the outcome
            // carries a summary, render `→ outcome — "summary"`
            // so the model sees what came back at a glance. When
            // it also carries evidence_ids, append `[ev-Tn-...]`
            // so cross-turn citation is structurally addressable.
            let summary_part = outcome
                .summary
                .as_deref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| format!(" — \"{s}\""))
                .unwrap_or_default();
            let evidence_part = if outcome.evidence_ids.is_empty() {
                String::new()
            } else {
                format!(" {}", format_evidence_id_range(&outcome.evidence_ids))
            };
            let reasoning_part = if reasoning.is_empty() || !summary_part.is_empty() {
                // Reasoning is redundant once we have the summary
                // (both describe the same outcome) — skip to keep
                // the line scannable.
                String::new()
            } else {
                format!(" — {reasoning}")
            };
            history_block.push_str(&format!(
                "\n- {} → {} ({age}){summary_part}{evidence_part}{reasoning_part}",
                outcome.tool_id, outcome.outcome
            ));
        }
        // Tier 1 cross-turn citation guidance — only emitted when
        // at least one outcome in the history carries citable
        // evidence_ids. The model SHOULD reference past handles
        // rather than re-calling the tool for evidence it already
        // saw. This guidance is SHAPE-level (not bank vocabulary).
        let has_citable_history = dossier
            .outcome_history
            .iter()
            .any(|o| !o.evidence_ids.is_empty());
        if has_citable_history {
            history_block.push_str(
                "\n\nCross-turn citation: any [ev-Tn-NNNN] handle above \
                 is still addressable this turn — cite it directly in \
                 your answer the same way you'd cite an id from a tool \
                 call made on THIS turn. The runtime dereferences the \
                 handle to the original evidence row without you having \
                 to re-call the tool.",
            );
        }
        // SHAPE-level usage discipline. The dossier is most
        // valuable when the model SURFACES prior attempts to the
        // user — "I tried earlier and couldn't find X, so let me
        // be cautious about Y" beats silently retrying the same
        // approach. This is general advice (not bank-specific
        // vocabulary) and applies to any outcome history.
        let has_negative = dossier.outcome_history.iter().any(|o| {
            o.outcome == "no-results"
                || o.outcome == "stale"
                || o.outcome == "wrong-tool"
        });
        if has_negative {
            history_block.push_str(
                "\n\nGuidance (load-bearing, not optional): the user's \
                 next question is likely a follow-up to one of the above. \
                 If your honest answer would be similar to a prior \
                 unsuccessful one (no-results / stale / wrong-tool), \
                 open your reply by surfacing the prior attempt — e.g. \
                 \"I tried looking that up earlier and couldn't find it, \
                 so I expect the same is true here\" — rather than \
                 silently reasoning from first principles. The user is \
                 owed the continuity of what's already been tried.",
            );
        }
    }
    parts.push(history_block);

    // Section 3 — ambient state (optional, wired later).
    if let Some(state) = &dossier.ambient_state {
        let trimmed = state.trim();
        if !trimmed.is_empty() {
            parts.push(format!("Ambient workspace state:\n{trimmed}"));
        }
    }

    parts.join("\n\n")
}

/// Best-effort soft write of a tool-decision outcome. Hides the
/// NoteStore-missing case so the runtime call sites don't have to
/// thread `Option<&NoteStore>` checks; a `None` store is the test/
/// CLI path where outcomes aren't persisted at all.
///
/// `tracing::debug!` on failure — the dossier write is observational,
/// never on the user-facing critical path. ARCH §9.2 — degrade
/// gracefully and log so the operator can see it.
pub async fn record_tool_outcome(
    notes: Option<&NoteStore>,
    session_id: &str,
    conversation_id: Option<&str>,
    tool_id: &str,
    outcome: ToolDecisionOutcome,
    reasoning: &str,
    extras: crate::memory::ToolDecisionExtras,
) {
    let Some(store) = notes else {
        tracing::info!(
            tool_id,
            outcome = outcome.as_str(),
            "dossier:record_outcome_no_store"
        );
        return;
    };
    match crate::memory::write_tool_decision(
        store,
        session_id,
        conversation_id,
        tool_id,
        outcome,
        reasoning,
        extras,
    )
    .await
    {
        Ok(id) => tracing::debug!(
            tool_id,
            outcome = outcome.as_str(),
            note_id = %id,
            "dossier:record_outcome"
        ),
        Err(e) => tracing::debug!(
            tool_id,
            outcome = outcome.as_str(),
            error = %e,
            "dossier:record_outcome_failed"
        ),
    }
}

/// Compact "n seconds ago" / "n minutes ago" / "n hours ago" /
/// "earlier today" / "yesterday" formatter for the outcome-history
/// rendering. Keeps the dossier readable without dragging in a
/// localisation dep — the model only needs a temporal signal, not a
/// precise timestamp.
fn format_age_since(now_unix: i64, past_unix: i64) -> String {
    let secs = (now_unix - past_unix).max(0);
    if secs < 60 {
        return "just now".to_string();
    }
    let minutes = secs / 60;
    if minutes < 60 {
        return format!("{minutes} min ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours} hr ago");
    }
    let days = hours / 24;
    if days == 1 {
        return "yesterday".to_string();
    }
    format!("{days} days ago")
}

/// Render a list of `ev-Tn-NNNN` ids as a compact range when
/// contiguous (`[ev-T2-0000..0003]`), or a comma-separated list
/// when sparse (`[ev-T2-0001, ev-T2-0004]`). Empty input returns
/// empty string. Used by the outcome-history renderer to keep
/// per-row evidence inventory scannable.
///
/// Range detection requires all ids in the input to share the same
/// `Tn-` turn prefix and have monotonically-increasing zero-padded
/// indices. Mixed-turn input always renders as a flat list (no
/// cross-turn ranges).
fn format_evidence_id_range(ids: &[String]) -> String {
    if ids.is_empty() {
        return String::new();
    }
    if ids.len() == 1 {
        return format!("[{}]", ids[0]);
    }
    // Try contiguous-range rendering: same Tn prefix + consecutive
    // numeric suffix in input order.
    let parse = |id: &str| -> Option<(String, u32)> {
        // ev-T2-0001 → ("ev-T2-", 1)
        let last_dash = id.rfind('-')?;
        let prefix = &id[..=last_dash];
        let num = id[last_dash + 1..].parse::<u32>().ok()?;
        Some((prefix.to_string(), num))
    };
    let parsed: Option<Vec<(String, u32)>> = ids.iter().map(|s| parse(s)).collect();
    if let Some(parsed) = parsed {
        let first_prefix = &parsed[0].0;
        let same_prefix = parsed.iter().all(|(p, _)| p == first_prefix);
        let monotonic = parsed.windows(2).all(|w| w[1].1 == w[0].1 + 1);
        if same_prefix && monotonic {
            return format!(
                "[{}..{:04}]",
                ids.first().unwrap(),
                parsed.last().unwrap().1
            );
        }
    }
    // Fall back to flat comma-separated list.
    format!("[{}]", ids.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolDossier;

    #[test]
    fn evidence_id_range_empty_returns_empty_string() {
        assert_eq!(format_evidence_id_range(&[]), "");
    }

    #[test]
    fn evidence_id_range_single_returns_bracketed() {
        assert_eq!(
            format_evidence_id_range(&["ev-T2-0001".to_string()]),
            "[ev-T2-0001]"
        );
    }

    #[test]
    fn evidence_id_range_contiguous_renders_as_range() {
        let ids = vec![
            "ev-T2-0000".to_string(),
            "ev-T2-0001".to_string(),
            "ev-T2-0002".to_string(),
            "ev-T2-0003".to_string(),
        ];
        assert_eq!(format_evidence_id_range(&ids), "[ev-T2-0000..0003]");
    }

    #[test]
    fn evidence_id_range_sparse_renders_as_list() {
        let ids = vec![
            "ev-T2-0001".to_string(),
            "ev-T2-0004".to_string(),
        ];
        assert_eq!(
            format_evidence_id_range(&ids),
            "[ev-T2-0001, ev-T2-0004]"
        );
    }

    #[test]
    fn evidence_id_range_mixed_turn_renders_as_list() {
        // Cross-turn ids never collapse to a range — the Tn prefix
        // disagreement is load-bearing for the reader.
        let ids = vec![
            "ev-T2-0001".to_string(),
            "ev-T2-0002".to_string(),
            "ev-T3-0000".to_string(),
        ];
        assert_eq!(
            format_evidence_id_range(&ids),
            "[ev-T2-0001, ev-T2-0002, ev-T3-0000]"
        );
    }

    fn fake_dossier(
        skill: Option<&str>,
        tools: &[(&str, &str)],
        outcomes: &[(&str, &str, &str, i64)],
    ) -> ToolDossier {
        ToolDossier {
            active_skill_id: skill.map(str::to_string),
            tools_available: tools
                .iter()
                .map(|(id, desc)| ToolDossierEntry {
                    tool_id: (*id).into(),
                    description: (*desc).into(),
                })
                .collect(),
            outcome_history: outcomes
                .iter()
                .map(|(tool, outcome, reasoning, t)| ToolDossierOutcome {
                    tool_id: (*tool).into(),
                    outcome: (*outcome).into(),
                    reasoning: (*reasoning).into(),
                    applied_at_unix: *t,
                    summary: None,
                    evidence_ids: Vec::new(),
                    turn_index: 0,
                })
                .collect(),
            ambient_state: None,
        }
    }

    #[test]
    fn render_includes_both_sections_when_populated() {
        let now = 1_000_000;
        let dossier = fake_dossier(
            Some("codebase-navigator"),
            &[
                ("symbol_lookup", "Exact name match — always correct"),
                ("code_search", "Approximate semantic search"),
            ],
            &[
                ("symbol_lookup", "useful", "found EmbedFn at line 70", now - 120),
                ("code_search", "no-results", "no semantically similar code", now - 30),
            ],
        );
        let rendered = render_tool_dossier(&dossier, now);
        assert!(rendered.contains("Tools available for this turn:"));
        assert!(rendered.contains("symbol_lookup"));
        assert!(rendered.contains("code_search"));
        assert!(rendered.contains("(narrowed for skill: codebase-navigator)"));
        assert!(rendered.contains("Outcome history this conversation:"));
        assert!(rendered.contains("→ useful"));
        assert!(rendered.contains("→ no-results"));
    }

    #[test]
    fn render_empty_history_emits_first_call_marker() {
        let dossier = fake_dossier(Some("codebase-navigator"), &[("symbol_lookup", "")], &[]);
        let rendered = render_tool_dossier(&dossier, 0);
        assert!(rendered.contains("first tool call this conversation"));
    }

    #[test]
    fn render_empty_tools_emits_pure_llm_marker() {
        let dossier = fake_dossier(None, &[], &[]);
        let rendered = render_tool_dossier(&dossier, 0);
        assert!(rendered.contains("no tools registered"));
    }

    #[test]
    fn render_appends_ambient_state_when_present() {
        let mut dossier = fake_dossier(Some("codebase-navigator"), &[("symbol_lookup", "x")], &[]);
        dossier.ambient_state = Some("lint: passing\ntest: 42 passing".to_string());
        let rendered = render_tool_dossier(&dossier, 0);
        assert!(rendered.contains("Ambient workspace state:"));
        assert!(rendered.contains("lint: passing"));
    }

    #[test]
    fn format_age_brackets() {
        assert_eq!(format_age_since(100, 80), "just now");
        assert_eq!(format_age_since(1000, 800), "3 min ago");
        assert_eq!(format_age_since(10_000, 1_000), "2 hr ago");
        // 100k seconds ≈ 27.7 hours → 1 day → "yesterday"
        assert_eq!(format_age_since(200_000, 100_000), "yesterday");
        // 3-day span uses the plural form
        assert_eq!(format_age_since(300_000, 28_000), "3 days ago");
        assert_eq!(format_age_since(100, 200), "just now"); // future-clamped
    }
}
