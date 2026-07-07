// SPDX-License-Identifier: AGPL-3.0-or-later
//! Situated-context renderer for the recipe-author skill.
//!
//! Spec §2 principle 1 — "situated before expressing" — requires the
//! agent to arrive at every turn already knowing the project's
//! charter, the recent decisions, the outstanding issues, and the
//! current corpus / test state. Static-at-session-start doesn't
//! satisfy this: decisions made *this* session must show up in
//! later turns of the same session.
//!
//! M1 ships the simplest version of this contract: the CLI driver
//! (`sovereign recipe-agent`) prepends a `[Project state]` block to
//! every user message before forwarding to the runtime. The block
//! is rebuilt per turn from on-disk + NoteStore state, capped at a
//! conservative character budget so a long-running project's
//! decision log doesn't crowd out the partner's actual message.
//!
//! M2/M3 will move the splice into the runtime's `build_context`
//! path so the desktop workspace doesn't have to replicate it; the
//! renderer here stays the single source of truth for block shape.

use sovereign_contracts::error::Result;
use sovereign_contracts::recipe::notes::NoteRow;

use super::decision_log::{DecisionAttribution, DecisionPayload};
use super::project::{ProjectSummary, RecipeProject};

/// Hard upper bound on the rendered block. Picked to leave plenty of
/// room (~80% of the typical chat-turn user message budget) for the
/// partner's actual question after the block is concatenated.
pub const MAX_SITUATED_CONTEXT_CHARS: usize = 3000;

/// How many recent feature-scoped notes to include verbatim before
/// the renderer falls back to a count summary. Keeps the dashboard's
/// "Recent decisions" card structurally aligned with the system
/// prompt.
pub const RECENT_DECISIONS_LIMIT: usize = 8;

/// Render the per-turn situated-context block for a recipe-author
/// project. The result is meant to be wrapped in a
/// `[Project state]\n…\n[Partner says]` envelope by the CLI driver
/// so the agent sees the project's frame before the partner's
/// words.
pub async fn render(project: &RecipeProject) -> Result<String> {
    let summary = project.read_summary()?;
    let recent = project
        .recent_feature_notes(RECENT_DECISIONS_LIMIT * 2)
        .await?;
    let mut block = String::new();
    write_header(&mut block, &summary, project);
    write_charter(&mut block, project).await?;
    write_corpus_state(&mut block, &summary);
    write_recent_decisions(&mut block, &recent);
    write_outstanding_issues(&mut block, &recent);
    write_capability_requests(&mut block, &recent);

    truncate_to_budget(block)
}

fn write_header(out: &mut String, summary: &ProjectSummary, project: &RecipeProject) {
    out.push_str("Project: ");
    out.push_str(&summary.title);
    out.push_str(" (feature_id=");
    out.push_str(project.feature_id());
    out.push_str(")\n");
}

async fn write_charter(out: &mut String, project: &RecipeProject) -> Result<()> {
    let row =
        match super::project::feature_row_for(project.feature_id(), project.features()).await? {
            Some(r) => r,
            None => return Ok(()),
        };
    let charter = row.charter_md.trim();
    if charter.is_empty() {
        return Ok(());
    }
    out.push_str("\nCharter:\n");
    out.push_str(&truncate_paragraph(charter, 600));
    out.push('\n');
    Ok(())
}

fn write_corpus_state(out: &mut String, summary: &ProjectSummary) {
    out.push_str("\nCorpus state:\n");
    match summary.recipe_id.as_ref() {
        Some(rid) => {
            out.push_str("- recipe: ");
            out.push_str(rid);
            out.push('\n');
        }
        None => out.push_str("- recipe: not yet drafted\n"),
    }
    if let Some(size) = summary.current_sample_size {
        out.push_str(&format!("- sample size: {size}\n"));
    }
    match (
        summary.last_test_status.as_ref(),
        summary.last_test_at.as_ref(),
    ) {
        (Some(status), Some(at)) => {
            out.push_str("- last test: ");
            out.push_str(status);
            out.push_str(" at ");
            out.push_str(at);
            out.push('\n');
        }
        _ => out.push_str("- last test: none yet\n"),
    }
}

fn write_recent_decisions(out: &mut String, notes: &[NoteRow]) {
    let decisions: Vec<&NoteRow> = notes
        .iter()
        .filter(|n| n.kind == "decision")
        .take(RECENT_DECISIONS_LIMIT)
        .collect();
    if decisions.is_empty() {
        return;
    }
    out.push_str("\nRecent decisions (newest first):\n");
    for (i, n) in decisions.iter().enumerate() {
        let payload: Option<DecisionPayload> = n
            .payload_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        let attribution = payload
            .as_ref()
            .and_then(|p| p.attribution)
            .map(attr_short)
            .unwrap_or("?");
        let kind = payload
            .as_ref()
            .map(|p| match p.decision_kind {
                super::decision_log::DecisionKind::SourceChoice => "source",
                super::decision_log::DecisionKind::ExtractionChoice => "extraction",
                super::decision_log::DecisionKind::SchemaChoice => "schema",
                super::decision_log::DecisionKind::DomainClarification => "clarification",
                super::decision_log::DecisionKind::DeferredQuestion => "deferred",
            })
            .unwrap_or("?");
        out.push_str(&format!(
            "{}. [{kind} · {attribution}] {}\n",
            i + 1,
            truncate_paragraph(&n.content, 240)
        ));
    }
}

fn write_outstanding_issues(out: &mut String, notes: &[NoteRow]) {
    let issues: Vec<&NoteRow> = notes
        .iter()
        .filter(|n| n.kind == "recipe_issue")
        .filter(|n| {
            // Status filter — only `open` issues appear in the
            // situated context. `resolved` / `won't_fix` shouldn't
            // crowd the per-turn prompt.
            n.payload_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(String::from))
                .map(|s| s == "open")
                .unwrap_or(true)
        })
        .collect();
    if issues.is_empty() {
        return;
    }
    out.push_str("\nOutstanding issues:\n");
    for n in issues.iter().take(6) {
        out.push_str("- ");
        out.push_str(&truncate_paragraph(&n.content, 200));
        out.push('\n');
    }
    if issues.len() > 6 {
        out.push_str(&format!("- (+{} more)\n", issues.len() - 6));
    }
}

fn write_capability_requests(out: &mut String, notes: &[NoteRow]) {
    let requests: Vec<&NoteRow> = notes
        .iter()
        .filter(|n| n.kind == "capability_request")
        .collect();
    if requests.is_empty() {
        return;
    }
    out.push_str("\nPending capability requests:\n");
    for n in requests.iter().take(4) {
        let summary = n
            .payload_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| {
                v.get("format_or_source")
                    .and_then(|s| s.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| "(no format declared)".into());
        out.push_str("- ");
        out.push_str(&summary);
        out.push('\n');
    }
}

fn attr_short(a: DecisionAttribution) -> &'static str {
    match a {
        DecisionAttribution::Partner => "partner",
        DecisionAttribution::AgentDefault => "agent",
        DecisionAttribution::Deferred => "deferred",
    }
}

fn truncate_paragraph(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim().replace('\n', " ");
    if trimmed.chars().count() <= max_chars {
        return trimmed;
    }
    let cut: String = trimmed.chars().take(max_chars - 1).collect();
    format!("{cut}…")
}

fn truncate_to_budget(s: String) -> Result<String> {
    if s.chars().count() <= MAX_SITUATED_CONTEXT_CHARS {
        return Ok(s);
    }
    let cut: String = s.chars().take(MAX_SITUATED_CONTEXT_CHARS - 32).collect();
    Ok(format!("{cut}\n[…context truncated]"))
}

/// Wrap the situated context block with the partner's message into
/// the canonical `[Project state] / [Partner says]` envelope. The
/// agent's system prompt mentions the `[Project state]` header by
/// name; keep this in sync with the wording in
/// `sovereign/skills/recipe-author/skill.toml`.
pub fn compose_envelope(situated: &str, partner_message: &str) -> String {
    format!("[Project state]\n{situated}\n\n[Partner says]\n{partner_message}",)
}

/// One-shot helper for the CLI driver: render + envelope in a
/// single call. Keeps the driver loop short.
pub async fn render_envelope(project: &RecipeProject, partner_message: &str) -> Result<String> {
    let block = render(project).await?;
    Ok(compose_envelope(&block, partner_message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::recipe_project_store::RecipeProjectStore;
    use crate::test_support::InMemoryRecipeNotes;
    use sovereign_contracts::recipe::notes::{NoteScope, NoteSource, RecipeNotes};

    use super::super::decision_log::{DecisionKind, DecisionPayload};

    async fn fresh() -> (
        RecipeProject,
        Arc<dyn RecipeNotes>,
        tempfile::TempDir,
        std::sync::MutexGuard<'static, ()>,
    ) {
        // HOME is process-global — hold the crate-wide lock for the
        // test's lifetime (see `recipe_author::home_test_lock`).
        let guard = crate::home_test_lock();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        let notes: Arc<dyn RecipeNotes> = Arc::new(InMemoryRecipeNotes::new());
        let features = Arc::new(RecipeProjectStore::open(&dir.path().join("features.db")).unwrap());
        let project = RecipeProject::new(
            "Federal case law (CourtListener)",
            "Build a corpus of federal published opinions over CourtListener \
             with a citation graph and a counsel-of-record investigation.",
            Arc::clone(&notes),
            Arc::clone(&features),
        )
        .await
        .unwrap();
        (project, notes, dir, guard)
    }

    #[tokio::test]
    async fn renders_with_charter_only_when_log_is_empty() {
        let (project, _notes, _dir, _guard) = fresh().await;
        let block = render(&project).await.unwrap();
        assert!(block.contains("Federal case law"));
        assert!(block.contains("Charter:"));
        assert!(block.contains("not yet drafted"));
        assert!(block.contains("none yet"));
    }

    #[tokio::test]
    async fn renders_recent_decisions_with_attribution_and_kind() {
        let (project, notes, _dir, _guard) = fresh().await;
        // Write three decisions through the same code path the tool
        // uses, so the payload schema is exercised end-to-end.
        for (i, kind) in [
            DecisionKind::SchemaChoice,
            DecisionKind::SourceChoice,
            DecisionKind::ExtractionChoice,
        ]
        .iter()
        .enumerate()
        {
            let payload = DecisionPayload {
                decision_kind: *kind,
                attribution: Some(if i == 0 {
                    DecisionAttribution::Partner
                } else {
                    DecisionAttribution::AgentDefault
                }),
                alternatives_considered: vec![],
            };
            let payload_json = serde_json::to_string(&payload).unwrap();
            notes
                .write_note_full(
                    "decision",
                    &format!("decision number {i}"),
                    Vec::new(),
                    Vec::new(),
                    "session-x",
                    NoteScope::Feature,
                    Some(project.feature_id()),
                    None,
                    NoteSource::Agent,
                    None,
                    Some(&payload_json),
                )
                .await
                .unwrap();
        }
        let block = render(&project).await.unwrap();
        assert!(block.contains("Recent decisions"), "block: {block}");
        // Newest-first ordering: write order was schema → source →
        // extraction; the reader returns newest first, so extraction
        // appears in row 1.
        assert!(block.contains("[extraction · agent]"));
        assert!(block.contains("[source · agent]"));
        assert!(block.contains("[schema · partner]"));
    }

    #[tokio::test]
    async fn truncates_at_budget() {
        let (project, notes, _dir, _guard) = fresh().await;
        let bulk = "x".repeat(8000);
        let payload = DecisionPayload {
            decision_kind: DecisionKind::ExtractionChoice,
            attribution: Some(DecisionAttribution::AgentDefault),
            alternatives_considered: vec![],
        };
        notes
            .write_note_full(
                "decision",
                &bulk,
                Vec::new(),
                Vec::new(),
                "session-x",
                NoteScope::Feature,
                Some(project.feature_id()),
                None,
                NoteSource::Agent,
                None,
                Some(&serde_json::to_string(&payload).unwrap()),
            )
            .await
            .unwrap();
        let block = render(&project).await.unwrap();
        assert!(block.chars().count() <= MAX_SITUATED_CONTEXT_CHARS);
    }

    #[test]
    fn envelope_wraps_situated_with_partner_message() {
        let envelope = compose_envelope("Project: X", "Hello there.");
        assert!(envelope.starts_with("[Project state]\n"));
        assert!(envelope.contains("[Partner says]\nHello there."));
    }
}
