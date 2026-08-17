// SPDX-License-Identifier: AGPL-3.0-or-later
//! R8 — local synthesis: the draft, URL-constrained.
//!
//! The draft is produced through the port's constrained draft surface
//! (`ResearchPort::draft`) with the URL constraint enabled over the
//! window's source URLs — invented citations are structurally
//! impossible (the renderer then verifies every span anyway, the
//! always-on guarantee). The evidence is assembled into the prompt by
//! this code, never by the model.

use super::estate::ResearchPort;
use super::icd::{Draft, DraftCitation, EvidenceWindow, UrlConstraintPolicy};

/// Assemble the round's evidence text (chunk id → content) for the
/// prompt. Deterministic: chunks in window order, bounded by the
/// charter's window cap (the window was already capped at build).
pub fn evidence_block(window: &EvidenceWindow) -> String {
    let mut out = String::new();
    for chunk in &window.chunks {
        out.push_str(&format!(
            "[{}] {}",
            chunk.id,
            chunk.content.replace('\n', " ")
        ));
        out.push('\n');
    }
    out
}

/// The allowed citation set for the draft: the window's source URLs.
pub fn allowed_urls(window: &EvidenceWindow) -> Vec<String> {
    window.chunks.iter().map(|c| c.source_url.clone()).collect()
}

/// The draft's deterministic figure inventory (order deep-research-t1h,
/// H2 — pre-registered): per window chunk, its `figure_tokens` — the
/// ONE figure decider (mod.rs) — under a fixed header, so the model is
/// never left to volunteer the evidence's digits (the t1f residual:
/// keys whose figures sat in the window while the sub-questions did
/// not carry them; the t1g v1 flight: the window carried era figures
/// the draft's era-years restated). The inventory is code-enforced
/// into the PROMPT; the model's carrying of the figures into the
/// answer is measured by the battery, never assumed (§7.6). Empty
/// window → empty block (nothing to enumerate, nothing to invent).
pub fn figure_inventory(window: &EvidenceWindow) -> String {
    let mut out = String::new();
    let mut any = false;
    for chunk in &window.chunks {
        let tokens = super::figure_tokens(&chunk.content);
        if tokens.is_empty() {
            continue;
        }
        any = true;
        out.push_str(&format!("- [{}]: {}\n", chunk.id, tokens.join(", ")));
    }
    if !any {
        return String::new();
    }
    format!(
        "Figures present in the evidence (every evidence-supported figure must appear in the answer):\n{out}"
    )
}

/// Produce the round's draft through the constrained surface. Round 1
/// drafts from the estate answer alone; later rounds draft from the
/// evidence + the still-open gaps.
pub async fn draft_round(
    port: &dyn ResearchPort,
    run_id: &str,
    charter_hash: &str,
    round: u32,
    question: &str,
    evidence: &EvidenceWindow,
    open_gaps: &[String],
) -> Result<Draft, String> {
    let system = "You are a local research synthesist. Answer the question from the evidence provided. \
                  Cite every factual claim with [Source: URL] where the URL is one of the allowed sources. \
                  If the evidence cannot answer a part, say so explicitly rather than guessing."
        .to_string();
    let mut prompt = String::new();
    if round == 1 {
        prompt.push_str(&format!("Estate evidence:\n{}", evidence_block(evidence)));
    } else {
        prompt.push_str(&format!(
            "Evidence gathered so far:\n{}\n\nQuestion: {question}",
            evidence_block(evidence)
        ));
        if !open_gaps.is_empty() {
            prompt.push_str(
                "\n\nStill-open specifics to resolve (answer only if the evidence supports it):",
            );
            for gap in open_gaps {
                prompt.push_str(&format!("\n- {gap}"));
            }
        }
    }
    // The deterministic figure inventory (t1h — H2): the evidence's
    // figures are enumerated for the model, never left to the draft's
    // discretion. Both round shapes carry it.
    let inventory = figure_inventory(evidence);
    if !inventory.is_empty() {
        prompt.push_str(&format!("\n\n{inventory}"));
    }
    if evidence.chunks.is_empty() {
        prompt.push_str("\n\n(No evidence was retrieved this round. Say so plainly.)");
    }
    let urls = allowed_urls(evidence);
    let text = port
        .draft(&prompt, Some(&system), &urls)
        .await
        .map_err(|e| format!("draft failed: {e}"))?;
    let citations: Vec<DraftCitation> = evidence
        .chunks
        .iter()
        .map(|c| DraftCitation {
            evidence_id: c.id.clone(),
            url: c.source_url.clone(),
            custody: Some(c.custody.clone()),
        })
        .collect();
    Ok(Draft {
        icd: "draft".to_string(),
        version: super::icd::ICD_VERSION,
        run_id: run_id.to_string(),
        charter_hash: charter_hash.to_string(),
        round,
        provider: "port:draft".to_string(),
        url_constraint: UrlConstraintPolicy {
            enabled: true,
            layer: "sovereign-inference:UrlAllowlistConstraint".to_string(),
        },
        text,
        citations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deep_research::estate::{EstateListing, PortHit};
    use crate::types::Custody;
    use std::sync::{Arc, Mutex};

    /// A recording fake port: captures the prompt it was asked to
    /// complete. Everything else is unreachable — the test drives
    /// draft_round directly.
    struct RecordingPort {
        prompt: Arc<Mutex<Option<String>>>,
    }

    impl RecordingPort {
        fn new() -> Self {
            RecordingPort {
                prompt: Arc::new(Mutex::new(None)),
            }
        }
        fn last_prompt(&self) -> String {
            self.prompt.lock().unwrap().clone().unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl ResearchPort for RecordingPort {
        async fn estate_listing(&self, _c: &[String]) -> Result<EstateListing, String> {
            unimplemented!("unreachable: draft_round calls only draft")
        }
        async fn estate_search(
            &self,
            _c: &[String],
            _q: &str,
            _l: usize,
        ) -> Result<Vec<PortHit>, String> {
            unimplemented!("unreachable")
        }
        async fn web_search(&self, _b: &str, _q: &str, _l: usize) -> Result<Vec<PortHit>, String> {
            unimplemented!("unreachable")
        }
        async fn web_fetch(&self, _u: &str) -> Result<String, String> {
            unimplemented!("unreachable")
        }
        async fn terminal_poll(&self) -> Result<(), String> {
            Ok(())
        }
        async fn draft(
            &self,
            prompt: &str,
            _s: Option<&str>,
            _a: &[String],
        ) -> Result<String, String> {
            *self.prompt.lock().unwrap() = Some(prompt.to_string());
            Ok("draft".to_string())
        }
    }

    fn window() -> EvidenceWindow {
        EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: 1,
            run_id: "r".to_string(),
            charter_hash: "h".to_string(),
            round: 1,
            chunks: vec![super::super::icd::WindowChunk {
                id: "ev-1".to_string(),
                locator: "https://example.com/a".to_string(),
                source_url: "https://example.com/a".to_string(),
                custody: Custody::PublicWeb.as_str().to_string(),
                provenance_class: "known".to_string(),
                content: "The Meridian Bridge was completed in 1873.".to_string(),
                ingested_into: None,
                tags: Vec::new(),
            }],
            fetch_failures: Vec::new(),
            dedup_refused: Vec::new(),
            derived_custody: Custody::PublicWeb.as_str().to_string(),
        }
    }

    #[test]
    fn evidence_block_is_deterministic() {
        let w = window();
        let block = evidence_block(&w);
        assert!(block.contains("[ev-1] The Meridian Bridge"));
        assert_eq!(evidence_block(&w), evidence_block(&w));
    }

    #[test]
    fn allowed_urls_is_the_window() {
        assert_eq!(
            allowed_urls(&window()),
            vec!["https://example.com/a".to_string()]
        );
    }

    /// RED (order deep-research-t1h, H2 — draft figure-completeness,
    /// pre-registered in adversarial/pre-registration.md): "a
    /// window-held figure the plan's sub-questions missed enters the
    /// draft". The drafting surface must carry a deterministic figure
    /// inventory — figure_tokens per window chunk, the one decider —
    /// so the model is never left to volunteer the evidence's digits.
    /// The t1f residual: keys whose figures sat in the window while
    /// the draft's sub-questions did not carry them (20 Class-A keys,
    /// t1h-failure-taxonomy.md). Watched red: fails at HEAD — the
    /// prompt carries the evidence block with no inventory.
    #[tokio::test]
    async fn draft_prompt_carries_the_window_figure_inventory() {
        let mut w = window();
        w.chunks[0].content =
            "Gini coefficients in the largest metro areas exceeded 0.5469 in 2019.".to_string();
        let port = RecordingPort::new();
        draft_round(&port, "r", "h", 1, "How did cities change?", &w, &[])
            .await
            .unwrap();
        let prompt = port.last_prompt();
        assert!(
            prompt.contains("Figures present in the evidence"),
            "the draft prompt must carry the figure inventory: {prompt}"
        );
        assert!(
            prompt.contains("0.5469"),
            "the window's figure must be enumerated in the inventory: {prompt}"
        );
    }
}
