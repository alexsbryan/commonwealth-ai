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
    use crate::types::Custody;

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
}
