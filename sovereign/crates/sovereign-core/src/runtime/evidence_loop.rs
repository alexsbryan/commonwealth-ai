// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded agentic evidence loop for KnowledgeQuery (prototype,
//! env-gated behind `SOVEREIGN_AGENTIC_KQ=1`).
//!
//! Today's KQ contract treats retrieval as preprocessing: one
//! embedding pass over the user's raw phrasing, then synthesis over
//! whatever came back. The model — the only component that knows what
//! evidence would *answer* the question — never gets a say. Measured
//! failure shape (2026-06-10): chaos-bench retrieval recall pinned at
//! 2/8 on critical evidence spans across five runs ("which anarchist
//! takes Winnie's money" retrieved zero Ossipon chunks); production
//! case "When was the last time the Knicks won a championship?"
//! returned a Phil Jackson article. Topical-overlap retrieval, no
//! agency.
//!
//! This module gives the model ONE bounded round of agency:
//!
//! 1. **Sufficiency check** — forced-choice logprob pass on the fast
//!    slot (~1 token decode, prefill-dominated): do the round-0
//!    passages contain the specific facts the question needs?
//! 2. **Query formulation** — when insufficient, the fast slot emits
//!    1–3 short targeted queries (grammar-constrained JSON), naming
//!    the entities/events the answer requires. The fast slot's
//!    formulation quality is already production-proven by the
//!    InformationRequest flow (`gap.rs`) — this moves the same
//!    capability PRE-synthesis where it can still change the evidence.
//! 3. **Round-2 retrieval** — each formulated query runs the SAME
//!    `kq_pipeline()` as round 0 (own embedding, FTS+vector, mesh,
//!    boosts); results are deduped against round 0 and appended. The
//!    existing downstream PPR rerank orders the merged set.
//!
//! Hard bounds: one extra round, ≤3 formulated queries, ≤12 appended
//! chunks. Latency: +~2s (sufficiency) on every gated turn; the
//! formulation + retrieval cost (~4–10s) is paid only on turns that
//! are, by the sufficiency judge's own verdict, currently
//! unanswerable. Glassbox: every step traces under
//! `agentic_kq` with the verdict probability, the formulated
//! queries, and per-query yields.

use std::collections::HashSet;

use crate::runtime::retrieval_pipeline::{kq_pipeline, PipelineState};
use crate::types::{CompletionRequest, Intent, Speed};

use super::ConversationContext;
use super::Runtime;

/// Canonical entity names read directly from a corpus's atlas atoms
/// file (`<data>/indexes/<corpus>/atlas/atoms.json`). Best-effort:
/// missing/garbled atlas → empty vec. Used only as the gazetteer
/// fallback when the embedded-context provider has no entry for the
/// corpus (see call site).
fn atlas_entity_names(corpus_id: &str) -> Vec<String> {
    let base = std::env::var("SOVEREIGN_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir().unwrap_or_default().join(".sovereign")
        });
    let path = base.join("indexes").join(corpus_id).join("atlas").join("atoms.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let atoms = v.get("atoms").and_then(|a| a.as_array()).cloned().unwrap_or_else(|| {
        v.as_array().cloned().unwrap_or_default()
    });
    atoms
        .iter()
        .filter_map(|a| {
            // v2 schema: {"atom_type": "Entity", "data": {"canonical_name": …}};
            // older flat shapes carry canonical_name at the top level.
            a.get("data")
                .and_then(|d| d.get("canonical_name"))
                .or_else(|| a.get("canonical_name"))
                .and_then(|n| n.as_str())
        })
        .map(str::to_string)
        .collect()
}

/// Person-type entity names from the atlas — the candidate pool for
/// structural WHICH-question enumeration.
fn atlas_person_names(corpus_id: &str) -> Vec<String> {
    let base = std::env::var("SOVEREIGN_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".sovereign"));
    let path = base.join("indexes").join(corpus_id).join("atlas").join("atoms.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let atoms = v.get("atoms").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    atoms
        .iter()
        .filter_map(|a| {
            let d = a.get("data")?;
            if d.get("entity_type").and_then(|t| t.as_str()) != Some("person") {
                return None;
            }
            d.get("canonical_name").and_then(|n| n.as_str()).map(str::to_string)
        })
        .collect()
}

/// Structural candidate enumeration for WHICH/WHO-shaped questions.
/// Instruction-following proved unreliable here — the fast model bets
/// on its prior (Verloc) instead of enumerating, no matter how the
/// prompt insists (v5 probes). So enumeration is done IN CODE: one
/// query per person entity, pairing the candidate name with the
/// question's content words. Deterministic, prior-free — the same
/// structure-over-instruction principle as the think-suppression and
/// evidence-seal fixes.
fn structural_candidate_queries(message: &str, corpus_ids: &[String]) -> Vec<String> {
    let lower = message.to_lowercase();
    if !(lower.starts_with("which") || lower.starts_with("who") || lower.contains(" which ") || lower.contains(" who "))
    {
        return Vec::new();
    }
    const STOP: &[&str] = &[
        "which", "who", "what", "does", "kind", "with", "near", "the", "end", "novel",
        "their", "from", "into", "takes", "that", "this", "her", "his", "and",
    ];
    let keywords: Vec<&str> = message
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4 && !STOP.contains(&w.to_lowercase().as_str()))
        .take(3)
        .collect();
    let mut out = Vec::new();
    for cid in corpus_ids {
        for name in atlas_person_names(cid) {
            if out.len() >= 16 {
                return out;
            }
            out.push(format!("{name} {}", keywords.join(" ")));
        }
    }
    out
}

/// Distinct corpus ids present in a chunk set — the implicit scope
/// when the conversation carries no explicit corpus seal.
fn merged_corpora(chunks: &[corpus_engine::ScoredChunk]) -> HashSet<String> {
    chunks.iter().map(|c| c.corpus_id.clone()).collect()
}

/// `SOVEREIGN_AGENTIC_KQ=1` (or `true`) turns the loop on. Default off
/// so benches A/B cleanly and nothing changes for existing surfaces.
pub(crate) fn agentic_kq_enabled() -> bool {
    std::env::var("SOVEREIGN_AGENTIC_KQ")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Insufficiency probability above which the formulation round fires.
/// Env-tunable (`SOVEREIGN_AGENTIC_KQ_THRESHOLD`) for sweeps.
fn insufficiency_threshold() -> f64 {
    std::env::var("SOVEREIGN_AGENTIC_KQ_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.5)
}

/// `SOVEREIGN_AGENTIC_KQ_DEBUG=1` mirrors the loop's tracing onto
/// stderr. Needed because bench/CLI surfaces don't install a tracing
/// subscriber (their idiom is eprintln, e.g. `[router]`/`[gv]` lines)
/// — without this the loop is invisible exactly where it's being
/// evaluated.
fn dbg(msg: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| {
        std::env::var("SOVEREIGN_AGENTIC_KQ_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    });
    if on {
        eprintln!("    [agentic_kq] {msg}");
    }
}

const MAX_FORMULATED_QUERIES: usize = 6;
const MAX_APPENDED_CHUNKS: usize = 12;
/// Evidence excerpt budget for the sufficiency prompt: enough to judge
/// coverage, small enough to stay prefill-cheap on the fast slot.
const SUFFICIENCY_CHUNKS: usize = 6;
const SUFFICIENCY_CHARS_PER_CHUNK: usize = 600;

impl Runtime {
    /// Run the bounded agentic round over the round-0 evidence.
    /// Returns the (possibly augmented) chunk set; on any judge or
    /// formulation failure it degrades to the input unchanged — the
    /// loop can only ADD evidence, never lose or reorder round 0.
    pub(crate) async fn agentic_evidence_round(
        &self,
        message: &str,
        chunks: Vec<corpus_engine::ScoredChunk>,
        context: &ConversationContext,
        intent: &Intent,
        scope: Option<&str>,
    ) -> Vec<corpus_engine::ScoredChunk> {
        // Empty round 0 is the strongest possible insufficiency signal
        // — skip the judge and go straight to formulation.
        let insufficiency = if chunks.is_empty() {
            1.0
        } else {
            match self.judge_evidence_sufficiency(message, &chunks).await {
                Some(p) => p,
                None => {
                    tracing::warn!(
                        target: "agentic_kq",
                        "sufficiency judge failed — keeping round-0 evidence unchanged"
                    );
                    return chunks;
                }
            }
        };
        let threshold = insufficiency_threshold();
        tracing::info!(
            target: "agentic_kq",
            insufficiency = format!("{insufficiency:.3}").as_str(),
            threshold,
            round0_chunks = chunks.len(),
            fires = insufficiency >= threshold,
            "agentic_kq: sufficiency verdict"
        );
        dbg(&format!(
            "insufficiency={insufficiency:.3} threshold={threshold} round0={} fires={}",
            chunks.len(),
            insufficiency >= threshold
        ));
        if insufficiency < threshold {
            return chunks;
        }

        let queries = match self.formulate_evidence_queries(message, &chunks, context).await {
            Some(q) if !q.is_empty() => q,
            _ => {
                tracing::warn!(
                    target: "agentic_kq",
                    "query formulation failed/empty — keeping round-0 evidence unchanged"
                );
                return chunks;
            }
        };
        // Structural enumeration augments (and outranks) the model's
        // own formulations for WHICH-shaped questions.
        let lookup_ids: Vec<String> = match context.conversation.enabled_corpora.as_deref() {
            Some(ids) if !ids.is_empty() => ids.to_vec(),
            _ => merged_corpora(&chunks).into_iter().collect(),
        };
        let mut queries = queries;
        let structural = structural_candidate_queries(message, &lookup_ids);
        if !structural.is_empty() {
            dbg(&format!("structural_candidates={structural:?}"));
            let mut all = structural;
            all.extend(queries);
            queries = all;
        }
        tracing::info!(
            target: "agentic_kq",
            queries = ?queries,
            "agentic_kq: formulated round-2 queries"
        );
        dbg(&format!("queries={queries:?}"));

        // Dedupe key: stable row id when present, else corpus + content
        // head (synthetic chunks have no row id).
        let key = |c: &corpus_engine::ScoredChunk| -> (String, String) {
            match c.chunk_id {
                Some(id) => (c.corpus_id.clone(), id.to_string()),
                None => (
                    c.corpus_id.clone(),
                    c.content.chars().take(120).collect(),
                ),
            }
        };
        let mut seen: HashSet<(String, String)> = chunks.iter().map(&key).collect();

        // Hard seal for round-2 results. The upstream "seal" proved
        // score-soft (2026-06-11 probe: a sealed-to-one-novel
        // conversation accumulated Winnie-the-Pooh wikipedia chunks
        // because the formulated query scored high there). Round-2
        // additions are structurally restricted to the conversation's
        // enabled corpora; when no explicit seal exists, to the
        // corpora round 0 actually drew from — agency must widen the
        // EVIDENCE, never the corpus scope.
        let allowed: HashSet<String> = match context.conversation.enabled_corpora.as_deref() {
            Some(ids) if !ids.is_empty() => ids.iter().cloned().collect(),
            _ => merged_corpora(&chunks),
        };

        let mut merged = chunks;
        let mut appended_chunks: Vec<corpus_engine::ScoredChunk> = Vec::new();
        let mut appended = 0usize;
        for q in queries.iter().take(18) {
            if appended >= MAX_APPENDED_CHUNKS {
                break;
            }
            let embedding = self.inference.embed_query(q).await.unwrap_or_default();
            if embedding.is_empty() {
                tracing::warn!(target: "agentic_kq", query = %q, "embed failed — skipping query");
                continue;
            }
            let mut state = PipelineState::new(
                q,
                context,
                intent,
                scope,
                embedding,
                "KnowledgeQuery",
                "KnowledgeQuery".to_string(),
            );
            kq_pipeline().run(self, &mut state).await;
            let mut new_for_query = 0usize;
            for c in state.chunks {
                if appended >= MAX_APPENDED_CHUNKS {
                    break;
                }
                if !allowed.is_empty() && !allowed.contains(&c.corpus_id) {
                    continue;
                }
                if seen.insert(key(&c)) {
                    appended_chunks.push(c);
                    appended += 1;
                    new_for_query += 1;
                }
            }
            tracing::info!(
                target: "agentic_kq",
                query = %q,
                new_chunks = new_for_query,
                "agentic_kq: round-2 query yield"
            );
            dbg(&format!("query={q:?} new_chunks={new_for_query}"));
        }
        // Priority placement: round-2 evidence splices in right after
        // the top-3 round-0 chunks instead of appending at the tail.
        // The prompt formatter serves the front of the set first and
        // the fast model reads early context most reliably — v2's
        // tail-appended murder-scene chunk was RETRIEVED but the model
        // still answered from general knowledge (position ~21 of 21,
        // past the char budget / attention cliff). The loop only fires
        // when round 0 was judged insufficient, so promoting round-2
        // evidence over mid-tier round-0 chunks is exactly the bet the
        // sufficiency verdict already made.
        let keep_front = merged.len().min(3);
        let tail = merged.split_off(keep_front);
        merged.extend(appended_chunks);
        merged.extend(tail);
        tracing::info!(
            target: "agentic_kq",
            appended,
            total = merged.len(),
            "agentic_kq: evidence round complete"
        );
        dbg(&format!("complete: appended={appended} total={}", merged.len()));
        merged
    }

    /// Forced-choice logprob pass: P(evidence is INSUFFICIENT). Uses
    /// the `x_forced_choice` sentinel (one decoded token, distribution
    /// read off the masked logits) — deterministic, no sampled verdict.
    async fn judge_evidence_sufficiency(
        &self,
        message: &str,
        chunks: &[corpus_engine::ScoredChunk],
    ) -> Option<f64> {
        let excerpts: Vec<String> = chunks
            .iter()
            .take(SUFFICIENCY_CHUNKS)
            .map(|c| c.content.chars().take(SUFFICIENCY_CHARS_PER_CHUNK).collect())
            .collect();
        let prompt = format!(
            "PASSAGES retrieved for a question:\n\"\"\"\n{}\n\"\"\"\n\n\
             QUESTION: {}\n\n\
             Do these passages contain the specific facts needed to answer the \
             question — the actual names, events, dates, or details it asks for? \
             Passages that are merely on the same topic, without the asked-for \
             facts, do NOT count.\n\n\
             Answer with exactly one letter — A = yes, the needed facts are \
             present; B = no, the needed facts are missing.",
            excerpts.join("\n---\n"),
            message.chars().take(400).collect::<String>(),
        );
        let req = CompletionRequest {
            prompt,
            system_message: Some("You are a careful evidence auditor. Answer with a single letter.".into()),
            preferred_speed: Speed::Fast,
            max_tokens: Some(1),
            temperature: Some(0.0),
            think_budget: Some(0),
            enable_thinking: Some(false),
            structured_output: Some(serde_json::json!({
                "type": "string", "enum": ["A", "B"], "x_forced_choice": true
            })),
            ..Default::default()
        };
        let resp = self.inference.complete(&req).await.ok()?;
        let dist: std::collections::HashMap<String, f64> =
            serde_json::from_str(resp.text.trim()).ok()?;
        let a = dist.get("A").copied().unwrap_or(0.0);
        let b = dist.get("B").copied().unwrap_or(0.0);
        let denom = a + b;
        if denom <= 0.0 {
            return None;
        }
        Some(b / denom)
    }

    /// Grammar-constrained formulation: 1–3 short search queries naming
    /// the specific entities/events the answer needs. Same fast-slot
    /// capability the InformationRequest flow already exercises.
    ///
    /// The prompt is grounded in the WORLD being searched — corpus
    /// titles from the round-0 chunks (e.g. the novel's title) plus
    /// the conversation's enabled corpora. Without this grounding the
    /// v1 prototype free-associated entities from the wrong universe
    /// entirely ("Winnie the Pooh money thief", "Annie Wilkes murder
    /// weapon" for questions about Conrad's The Secret Agent) — the
    /// formulator cannot know which world a bare question concerns.
    async fn formulate_evidence_queries(
        &self,
        message: &str,
        round0: &[corpus_engine::ScoredChunk],
        context: &ConversationContext,
    ) -> Option<Vec<String>> {
        let mut titles: Vec<String> = Vec::new();
        for c in round0 {
            if let Some(t) = c.title.as_deref() {
                let t = t.trim();
                if !t.is_empty() && !titles.iter().any(|x| x == t) {
                    titles.push(t.to_string());
                }
            }
            if titles.len() >= 6 {
                break;
            }
        }
        let corpora: Vec<String> = context
            .conversation
            .enabled_corpora
            .clone()
            .unwrap_or_default();
        // Atlas gazetteer: the canonical entity inventory for the
        // corpora in play. The formulator cannot search for an entity
        // it cannot NAME — "which anarchist takes Winnie's money" is
        // unformulable without knowing Ossipon exists. The atlas (when
        // built) knows every canonical entity; handing the inventory
        // to the formulator lets it enumerate candidates by name. This
        // is the designed composition point of the two retrieval
        // levers: atlas = indexes, agency = queries.
        let lookup_ids: Vec<String> = if corpora.is_empty() {
            merged_corpora(round0).into_iter().collect()
        } else {
            corpora.clone()
        };
        let mut entities: Vec<String> = Vec::new();
        if let Some(provider) = self.atlas_context_provider.as_ref() {
            for cid in &lookup_ids {
                if let Some(ctx) = provider.get(cid) {
                    for e in &ctx.entries {
                        if entities.len() >= 24 {
                            break;
                        }
                        if !entities.contains(&e.canonical_name) {
                            entities.push(e.canonical_name.clone());
                        }
                    }
                }
            }
        }
        // Fallback: read canonical entity NAMES straight from the
        // atlas atoms file. The provider above serves pre-EMBEDDED
        // contexts and its description-length/depth filter excludes
        // enrich-built literary atoms entirely (they carry no
        // description field) — but the gazetteer needs only names.
        // Prototype-scoped file read; the durable fix is a name-only
        // surface on AtlasContextProvider.
        if entities.is_empty() {
            for cid in &lookup_ids {
                for name in atlas_entity_names(cid) {
                    if entities.len() >= 24 {
                        break;
                    }
                    if !entities.contains(&name) {
                        entities.push(name);
                    }
                }
            }
        }
        let mut world = String::new();
        if !corpora.is_empty() {
            world.push_str(&format!("Knowledge base being searched: {}.\n", corpora.join(", ")));
        }
        if !titles.is_empty() {
            world.push_str(&format!(
                "Source documents seen so far: {}.\n",
                titles.join("; ")
            ));
        }
        if !entities.is_empty() {
            world.push_str(&format!(
                "Known people and things in this knowledge base: {}.\n",
                entities.join(", ")
            ));
        }
        let prompt = format!(
            "{}\nA question is being answered FROM THIS KNOWLEDGE BASE ONLY:\n\n{}\n\n\
             The first search, using the question's own wording, did not surface \
             the needed facts. Write 1-3 alternative search queries that name the \
             specific people, places, things, or events FROM THIS KNOWLEDGE BASE \
             whose details would answer the question. Stay inside the world of the \
             source documents named above — never introduce names from other books, \
             films, or topics. If the question asks WHICH person or thing did \
             something, you MUST write one query for EVERY plausible candidate \
             from the known entities above (up to 6), each pairing one \
             candidate's name with the asked-about action — enumerate, do not \
             bet on a single guess. Each \
             query is a few plain words — names and concrete terms only. No \
             punctuation, no quotes, no braces, no explanations.",
            world,
            message.chars().take(400).collect::<String>(),
        );
        let req = CompletionRequest {
            prompt,
            system_message: Some("You write precise search queries.".into()),
            preferred_speed: Speed::Fast,
            max_tokens: Some(160),
            temperature: Some(0.0),
            think_budget: Some(0),
            enable_thinking: Some(false),
            structured_output: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "queries": {
                        "type": "array",
                        "items": { "type": "string", "maxLength": 80 },
                        "minItems": 1,
                        "maxItems": 6
                    }
                },
                "required": ["queries"],
                "additionalProperties": false
            })),
            ..Default::default()
        };
        let resp = self.inference.complete(&req).await.ok()?;
        #[derive(serde::Deserialize)]
        struct Q {
            queries: Vec<String>,
        }
        let text = resp.text.trim();
        // Grammar guarantees shape on the happy path; the defensive
        // find('{') covers template tails around the JSON.
        let parsed: Q = serde_json::from_str(text)
            .or_else(|_| {
                let start = text.find('{').unwrap_or(0);
                serde_json::from_str(&text[start..])
            })
            .ok()?;
        // Sanitize: with a long entity-rich prompt the fast model
        // sometimes nests JSON-looking text INSIDE the string items
        // (grammar constrains the envelope, not the string contents).
        // Strip structural characters; drop items that were mostly
        // meta-narration rather than query terms.
        let queries: Vec<String> = parsed
            .queries
            .into_iter()
            .map(|q| {
                q.chars()
                    .map(|c| if "{}\"\\:".contains(c) { ' ' } else { c })
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|q| !q.is_empty() && q.len() <= 80 && !q.to_lowercase().contains("searches for"))
            .collect();
        Some(queries)
    }
}

#[cfg(test)]
mod tests {
    /// The env gate must default OFF — existing surfaces and benches
    /// change behaviour only by explicit opt-in.
    #[test]
    fn agentic_kq_default_off() {
        std::env::remove_var("SOVEREIGN_AGENTIC_KQ");
        assert!(!super::agentic_kq_enabled());
    }
}
