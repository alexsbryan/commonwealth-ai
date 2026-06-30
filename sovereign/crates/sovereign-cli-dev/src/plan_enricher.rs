// SPDX-License-Identifier: AGPL-3.0-or-later
//! Inference enrichment pass for `svrn project plan`.
//!
//! `plan_composer.rs` produces a deterministic structural skeleton:
//! one phase per H2 in `DESIGN.md`, each with the first non-empty
//! sentence as its body and no stop condition. That output is honest
//! about what the composer can compute from text alone — but it is
//! not what an operator wants to hand an autonomous coding agent.
//!
//! This module bridges that gap. For each non-skeleton phase, it
//! calls the local Bench_Darwin chat slot with:
//!   - the full `DESIGN.md` (so the model has project context),
//!   - the phase's H2 section body (the work *this* phase realizes),
//!   - the project's primary language (so the stop hint is shaped
//!     for the right test runner).
//!
//! In return it gets a strict JSON object (`body`, `stop_hint`) that
//! replaces the composer's placeholders. Failures fall back silently
//! to the composer's deterministic output — the plan still ships,
//! just with `(fill this in)` markers where enrichment couldn't
//! reach the daemon.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::plan_composer::ComposedPlanItem;

const ENRICH_TIMEOUT_S: u64 = 180;
const ENRICH_TEMPERATURE: f32 = 0.0;
const ENRICH_MAX_TOKENS: u32 = 1024;
const DESIGN_BODY_BUDGET: usize = 16 * 1024;
const SECTION_BODY_BUDGET: usize = 4 * 1024;

#[derive(Debug, Clone, Default)]
pub struct EnrichOutcome {
    pub enriched: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EnrichedFields {
    #[serde(default)]
    body: String,
    #[serde(default)]
    stop_hint: String,
}

/// Enrich each non-skeleton phase in `items` by calling the
/// daemon's chat slot. Mutates `items` in place. Returns a summary
/// the caller renders for the operator.
///
/// `sections` carries the full DESIGN.md H2 section bodies indexed
/// by heading; phase-N items match by exact heading.
pub async fn enrich(
    items: &mut [ComposedPlanItem],
    design_md: &str,
    sections: &[corpus_engine_atos::design_signals::Section],
    primary_language: Option<&str>,
    daemon_url: &str,
    model: &str,
) -> EnrichOutcome {
    let body_index: HashMap<String, &str> = sections
        .iter()
        .filter(|s| s.level == 2)
        .map(|s| (s.heading.clone(), s.body.as_str()))
        .collect();

    let truncated_design = truncate(design_md, DESIGN_BODY_BUDGET);
    let lang = primary_language.unwrap_or("unknown");

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(ENRICH_TIMEOUT_S))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("    \u{26a0} enrich: build reqwest client: {e}");
            return EnrichOutcome {
                enriched: 0,
                failed: items.len(),
                skipped: 0,
            };
        }
    };

    let mut outcome = EnrichOutcome::default();
    for item in items.iter_mut() {
        // Phase 0 (Skeleton) already has a real body + stop_hint from
        // the composer; no need to touch it.
        if item.phase == 0 {
            outcome.skipped += 1;
            continue;
        }
        let section_body = body_index
            .get(&item.title)
            .copied()
            .unwrap_or("(section body not found in DESIGN.md)");
        let prompt = build_prompt(
            item.phase,
            &item.title,
            &truncated_design,
            &truncate(section_body, SECTION_BODY_BUDGET),
            lang,
        );

        match call_once(&client, daemon_url, model, &prompt).await {
            Ok(fields) => {
                if !fields.body.trim().is_empty() {
                    item.body = fields.body.trim().to_string();
                }
                if !fields.stop_hint.trim().is_empty() {
                    item.stop_hint = Some(fields.stop_hint.trim().to_string());
                }
                outcome.enriched += 1;
            }
            Err(e) => {
                eprintln!(
                    "    \u{26a0} enrich: phase {} '{}' fell back to composer default ({})",
                    item.phase, item.title, e
                );
                outcome.failed += 1;
            }
        }
    }
    outcome
}

fn build_prompt(
    phase: u32,
    title: &str,
    design_md: &str,
    section_body: &str,
    lang: &str,
) -> String {
    format!(
        "You are filling in implementation details for ONE phase of a project plan that an autonomous coding agent will execute.\n\n\
         === Project DESIGN.md (full context) ===\n{design}\n\n\
         === Project primary language ===\n{lang}\n\n\
         === Phase to enrich ===\nPhase {phase}: {title}\n\n\
         Section excerpt from DESIGN.md (the H2 body that this phase realizes):\n{section}\n\n\
         Output ONE valid JSON object — no prose, no markdown fences:\n\
         {{\n\
         \x20 \"body\": \"2-4 plain-English sentences describing what code this phase delivers. Reference specific types, functions, files when DESIGN.md does. No headings or markdown lists — just prose.\",\n\
         \x20 \"stop_hint\": \"a single shell command that proves this phase is done. Use the project's standard test runner. For Rust prefer scoped tests like 'cargo test --test <name>' or 'cargo check -p <crate>' over a workspace-wide command. For TS/JS prefer 'npm test -- <pattern>'. Empty string if no obvious gate.\"\n\
         }}\n\
         \n\
         Important:\n\
         - The 'body' must be specific to THIS phase, not a restatement of the project goal. If the section excerpt is vague, say what code likely satisfies it given the broader DESIGN.md context.\n\
         - The 'stop_hint' must be executable. Don't write 'tests pass' — write the command that runs them.\n",
        design = design_md,
        lang = lang,
        phase = phase,
        title = title,
        section = section_body,
    )
}

async fn call_once(
    client: &reqwest::Client,
    daemon_url: &str,
    model: &str,
    prompt: &str,
) -> Result<EnrichedFields, String> {
    let url = format!("{}/v1/chat/completions", daemon_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "temperature": ENRICH_TEMPERATURE,
        "top_p": 1.0,
        "max_tokens": ENRICH_MAX_TOKENS,
        "messages": [
            {"role": "system", "content": "You output exactly one JSON object matching the requested schema. No prose. No markdown fences."},
            {"role": "user", "content": prompt},
        ],
    });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("daemon {status}: {}", truncate(&text, 512)));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse daemon response: {e}"))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    if content.is_empty() {
        return Err("empty content from daemon".into());
    }
    let stripped = strip_fences(&content);
    serde_json::from_str::<EnrichedFields>(&stripped)
        .map_err(|e| format!("parse enrich JSON: {e}; raw: {}", truncate(&content, 512)))
}

fn strip_fences(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    trimmed.to_string()
}

fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        let mut out = String::with_capacity(limit + 32);
        out.push_str(&s[..limit]);
        out.push_str("\n…(truncated)…");
        out
    }
}

/// Quick reachability probe — used by `cmd_plan` to decide whether to
/// even attempt enrichment. Returns true if `/v1/models` responds with
/// 2xx within a tight timeout.
pub async fn daemon_reachable(daemon_url: &str) -> bool {
    let url = format!("{}/v1/models", daemon_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(client.get(&url).send().await, Ok(r) if r.status().is_success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_fences_handles_json_block() {
        assert_eq!(
            strip_fences("```json\n{\"body\": \"x\"}\n```"),
            "{\"body\": \"x\"}"
        );
        assert_eq!(strip_fences("{\"body\": \"x\"}"), "{\"body\": \"x\"}");
    }

    #[test]
    fn build_prompt_carries_phase_and_title() {
        let p = build_prompt(3, "Forward compat", "DESIGN", "section body", "rust");
        assert!(p.contains("Phase 3: Forward compat"));
        assert!(p.contains("=== Project primary language ===\nrust"));
        assert!(p.contains("DESIGN"));
    }

    #[test]
    fn truncate_respects_limit() {
        let s = "x".repeat(50_000);
        let t = truncate(&s, 1_000);
        assert!(t.starts_with("xxxx"));
        assert!(t.contains("(truncated)"));
        assert!(t.len() < s.len());
    }
}
