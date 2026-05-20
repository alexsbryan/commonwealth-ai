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

use crate::memory::{read_recent_tool_decisions, ToolDecisionOutcome};
use crate::registry::ToolRegistry;
use crate::skills::{narrow_tools_for_skill, Skill};
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
    use crate::skills::SkillRegister;

    // Inner-work gate: relational skills don't get a dossier. Run
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

    let full_catalog = tools.descriptors();
    let narrowed = match active_skill {
        Some(skill) => narrow_tools_for_skill(&full_catalog, skill),
        None => full_catalog,
    };

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
            if reasoning.is_empty() {
                history_block.push_str(&format!(
                    "\n- {} → {} ({age})",
                    outcome.tool_id, outcome.outcome
                ));
            } else {
                history_block.push_str(&format!(
                    "\n- {} → {} ({age}) — {reasoning}",
                    outcome.tool_id, outcome.outcome
                ));
            }
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
) {
    let Some(store) = notes else {
        tracing::trace!(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolDossier;

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
