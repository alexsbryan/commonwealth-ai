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
//! One structural stage runs alongside the model's own formulations,
//! in code rather than in the model (structure-over-instruction):
//! atlas event/relation atoms whose pronoun-resolved statements
//! overlap the question's content words are injected directly as
//! evidence (`atlas_atom_matches`). The loop also classifies the
//! question as in-world (entity-anchored) or world-general, which
//! drives the caller's choice of insufficiency note.
//!
//! Hard bounds: one extra round, ≤6 formulated queries, ≤12 appended
//! chunks. Latency: +~2s (sufficiency) on every gated turn; the
//! formulation + retrieval cost (~4–10s) is paid only on turns that
//! are, by the sufficiency judge's own verdict, currently
//! unanswerable. Glassbox: every step traces under
//! `agentic_kq` with the verdict probability, the formulated
//! queries, and per-query yields.

use std::collections::HashSet;

use crate::runtime::retrieval_pipeline::{kq_pipeline, PipelineState};
use crate::slot_policy::Workload;
use crate::types::{CompletionRequest, Intent, Speed};

use super::ConversationContext;
use super::Runtime;

mod anchoring;
pub(crate) use anchoring::*;

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
        // Mirror to tracing too: a detached daemon discards stderr, so the loop
        // was invisible in daemon.err. Default target (`sovereign_core::…`)
        // matches the daemon's crate-scoped filter. (2026-06-18 glassbox fix.)
        tracing::info!("[agentic_kq] {msg}");
    }
}

const MAX_FORMULATED_QUERIES: usize = 6;
const MAX_APPENDED_CHUNKS: usize = 12;
/// Evidence excerpt budget for the sufficiency prompt. Was 6×600 chars to stay
/// prefill-cheap, but that truncated answers deep in a chunk: measured 2026-06-18,
/// the science answer ("the sacrosanct fetish of to-day is science") sat at char
/// 1902 of a 2023-char chunk ranked 7th, so the judge saw a 600-char slice of the
/// top 6 chunks, never saw the answer, declared the evidence insufficient, and the
/// abstain note fired — even though full synthesis answered it correctly. The
/// judge must see the evidence it is judging. Defaults raised to 12×2000 (the full
/// round-0 set, full chunks); env-tunable so the accuracy↔prefill-throughput trade
/// can be swept without a rebuild. A more accurate verdict also avoids firing
/// round-2 (formulation + extra retrieval) when round-0 was already sufficient, so
/// the larger prefill is partly self-funding.
fn sufficiency_chunks() -> usize {
    std::env::var("SOVEREIGN_SUFFICIENCY_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12)
}
fn sufficiency_chars_per_chunk() -> usize {
    std::env::var("SOVEREIGN_SUFFICIENCY_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000)
}

impl Runtime {
    /// Run the bounded agentic round over the round-0 evidence.
    /// Returns the (possibly augmented) chunk set; on any judge or
    /// formulation failure it degrades to the input unchanged — the
    /// loop can only ADD evidence, never lose or reorder round 0.
    /// Returns `(chunks, still_insufficient, entity_anchored, corpus_anchored)`.
    ///
    /// `still_insufficient` — the loop fired, did its round, and the
    /// merged evidence STILL fails the sufficiency judge (or round 2
    /// found nothing new). The caller surfaces this to the synthesis
    /// prompt: a model that knows the targeted search came back empty
    /// abstains; one that doesn't treats the near-miss pile as
    /// license to answer. Measured 2026-06-11 full bank: the loop
    /// converted 3 absent-question abstentions into confident
    /// fabrications precisely because synthesis never learned that
    /// round 2 failed.
    ///
    /// `entity_anchored` — the question names entities from the
    /// corpus's own world (atlas gazetteer match), so "general
    /// knowledge" structurally cannot answer it; see
    /// `question_is_entity_anchored`.
    pub(crate) async fn agentic_evidence_round(
        &self,
        message: &str,
        chunks: Vec<corpus_engine::ScoredChunk>,
        context: &ConversationContext,
        intent: &Intent,
        scope: Option<&str>,
    ) -> (Vec<corpus_engine::ScoredChunk>, bool, bool, bool) {
        dbg(&format!(
            "agentic loop ENTERED: round0_chunks={} (judge next)",
            chunks.len()
        ));
        // Empty round 0 is the strongest possible insufficiency signal
        // — skip the judge and go straight to formulation.
        let insufficiency = if chunks.is_empty() {
            1.0
        } else {
            match self.judge_evidence_sufficiency(message, &chunks).await {
                Some(p) => p,
                None => {
                    // Fail-FORWARD: if we cannot confirm sufficiency, treat the
                    // evidence as insufficient and run round-2 — which only ADDS
                    // deduped evidence, never removes. The old default returned
                    // round-0 unchanged, so ANY judge flakiness silently disabled
                    // recall. Recall-safe default: when unsure, retrieve more.
                    tracing::warn!(
                        target: "agentic_kq",
                        "sufficiency judge failed — treating as INSUFFICIENT (fail-forward to round-2)"
                    );
                    dbg(
                        "agentic loop: sufficiency judge FAILED (None) → fail-forward, run round-2",
                    );
                    1.0
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
            return (chunks, false, false, true);
        }

        // Corpora the atom-matching stage may read atlases from, and
        // the question's lexical view — both also feed the in-world
        // (entity-anchored) verdict the caller uses to pick the right
        // insufficiency note.
        let lookup_ids: Vec<String> = match context.conversation.enabled_corpora.as_deref() {
            // Intersect the user's selection with the principal-scoped
            // installed set, so a forged `enabled_corpora` can't widen
            // retrieval into a forbidden (another principal's Private) corpus.
            Some(ids) if !ids.is_empty() => {
                let scoped: std::collections::HashSet<&str> = context
                    .installed_corpora
                    .iter()
                    .map(String::as_str)
                    .collect();
                ids.iter()
                    .filter(|id| scoped.contains(id.as_str()))
                    .cloned()
                    .collect()
            }
            _ => merged_corpora(&chunks).into_iter().collect(),
        };
        let kw = question_keywords(message);
        let entity_anchored = question_is_entity_anchored(&kw, &lookup_ids);
        let corpus_anchored = entity_anchored || question_is_corpus_anchored(&kw, &lookup_ids);
        // Lexical-only matching, deliberately. A v10 probe tried a
        // cosine-similarity fallback (floor 0.5) for synonymy gaps and
        // it matched WRONG atoms at 0.52–0.59 — short-text embedding
        // similarity is dominated by entity-name overlap, not answer
        // relevance — and any nonzero match defeats the zero-atom skip
        // below, re-arming the fabrication path it exists to close.
        // Precision over recall here; the grounding gate downstream is
        // the recall safety net.
        let atom_matches: Vec<(String, Vec<(String, Vec<String>, usize)>)> = lookup_ids
            .iter()
            .map(|cid| (cid.clone(), atlas_atom_matches(cid, &kw)))
            .collect();
        let atom_count: usize = atom_matches.iter().map(|(_, m)| m.len()).sum();
        dbg(&format!(
            "entity_anchored={entity_anchored} atom_matches={atom_count}"
        ));

        // In-world question, and the semantic index has nothing for it:
        // an entity-anchored question whose content words match NO atlas
        // atom is, with high probability, asking for a fact the corpus
        // never states (the atlas catalogued every event/relation at
        // enrichment time). Round-2 chunk retrieval can only return
        // near-miss passages about the named entity — and the measured
        // effect of appending those (2026-06-11 full banks ×2) is
        // flipping honest abstentions into confident fabrications: the
        // near-miss pile reads as license to answer, and no prompt note
        // (generic OR in-world-strengthened) stopped a 4B from guessing
        // over it. So the append round is structurally SKIPPED — the
        // fabrication fuel never reaches the prompt, round-0 stands as
        // retrieved, and the caller's in-world note still tells the
        // model the targeted search found nothing. Cheaper too: no
        // formulation call, no per-query retrieval.
        if entity_anchored && atom_count == 0 {
            tracing::info!(
                target: "agentic_kq",
                "agentic_kq: in-world question with zero atlas-atom support — append round skipped"
            );
            dbg("in-world + zero atoms: append round skipped (anti-fabrication)");
            return (chunks, true, true, true);
        }

        let queries = match self
            .formulate_evidence_queries(message, &chunks, context)
            .await
        {
            Some(q) if !q.is_empty() => q,
            _ => {
                tracing::warn!(
                    target: "agentic_kq",
                    "query formulation failed/empty — keeping round-0 evidence unchanged"
                );
                // The loop FIRED (round 0 judged insufficient) and
                // produced nothing — synthesis should know.
                return (chunks, true, entity_anchored, corpus_anchored);
            }
        };
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
                None => (c.corpus_id.clone(), c.content.chars().take(120).collect()),
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
            // Same principal-scope intersection as `lookup_ids` above: a
            // forged `enabled_corpora` cannot widen round-2 evidence into a
            // forbidden corpus.
            Some(ids) if !ids.is_empty() => {
                let scoped: HashSet<&str> = context
                    .installed_corpora
                    .iter()
                    .map(String::as_str)
                    .collect();
                ids.iter()
                    .filter(|id| scoped.contains(id.as_str()))
                    .cloned()
                    .collect()
            }
            _ => merged_corpora(&chunks),
        };

        let mut merged = chunks;
        let mut appended_chunks: Vec<corpus_engine::ScoredChunk> = Vec::new();
        let mut appended = 0usize;

        // Atlas atom matches first — they carry pronoun-resolved
        // event/relation statements extracted at enrichment time, so
        // when one matches the question's content words it is usually
        // the single most decisive piece of evidence available (and
        // costs nothing: an in-code keyword scan over a small file).
        // NOTE: a v6 probe tried pooling per-candidate BM25 hits by
        // raw score instead; cross-query FTS scores are dominated by
        // each query's rarest term (IDF favours the rarest CANDIDATE,
        // not the right one) — same composition trap `ScoredChunk`'s
        // own docs warn about for cross-corpus scores. Don't revive it.
        for (cid, matches) in &atom_matches {
            for (desc, previews, hits) in matches {
                if appended >= MAX_APPENDED_CHUNKS {
                    break;
                }
                let title = merged
                    .iter()
                    .find(|c| &c.corpus_id == cid)
                    .and_then(|c| c.title.clone());
                let mut content = format!("Knowledge-atlas record for this corpus: {desc}");
                if !previews.is_empty() {
                    content.push_str(&format!(
                        " [supporting passage: \"{}\"]",
                        previews.join("\" / \"")
                    ));
                }
                let chunk = corpus_engine::ScoredChunk {
                    content,
                    title,
                    url: None,
                    corpus_id: cid.clone(),
                    score: 1.0,
                    metadata: std::collections::HashMap::from([(
                        "atlas_atom".to_string(),
                        "true".to_string(),
                    )]),
                    chunk_id: None,
                    source_doc_id: None,
                    vector_distance: None,
                };
                if seen.insert(key(&chunk)) {
                    dbg(&format!("atlas atom hits={hits} desc={desc:?}"));
                    tracing::info!(
                        target: "agentic_kq",
                        corpus = %cid,
                        keyword_hits = hits,
                        "agentic_kq: atlas atom match injected"
                    );
                    appended_chunks.push(chunk);
                    appended += 1;
                }
            }
        }

        for q in queries.iter().take(MAX_FORMULATED_QUERIES) {
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
        // Post-loop verdict: did round 2 actually fix the gap? Re-run
        // the same forced-choice judge over the merged front (which now
        // contains the appended evidence). One extra ~1s call, paid
        // only on fired turns; the verdict drives the caller's
        // insufficiency note to the synthesis prompt.
        let still_insufficient = if appended == 0 {
            true
        } else {
            match self.judge_evidence_sufficiency(message, &merged).await {
                Some(p) => p >= threshold,
                None => false,
            }
        };
        tracing::info!(
            target: "agentic_kq",
            appended,
            total = merged.len(),
            still_insufficient,
            "agentic_kq: evidence round complete"
        );
        dbg(&format!(
            "complete: appended={appended} total={} still_insufficient={still_insufficient}",
            merged.len()
        ));
        (merged, still_insufficient, entity_anchored, corpus_anchored)
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
            .take(sufficiency_chunks())
            .map(|c| {
                c.content
                    .chars()
                    .take(sufficiency_chars_per_chunk())
                    .collect()
            })
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
            system_message: Some(
                "You are a careful evidence auditor. Answer with a single letter.".into(),
            ),
            // PRIMARY tier, not Fast. The `x_forced_choice` logit-distribution
            // sentinel is NOT honored on the fast slot — it returns a sampled
            // token ("\"B") instead of the {A,B} distribution, so the parse below
            // fails and the loop silently degrades to round-0. This is the
            // documented "recall pinned at 2/8" failure: the judge never worked.
            // The gate's `forced_choice_ab` runs on primary for the same reason
            // (the fast slot's support distributions are squashed). One
            // prefill-dominated forced-choice per KQ is affordable for recall.
            preferred_speed: Speed::Slow,
            // SLOT_POLICY §7: OICP envelope instead of a `model_id:
            // "primary"` pin (a latent privacy hole — see grounding/
            // judge.rs). No `base_request` here, so posture comes from
            // the session's skills exactly as `build_oicp` derives it.
            oicp: Some(Workload::Judge.requirements(self.session_sharding())),
            max_tokens: Some(1),
            temperature: Some(0.0),
            think_budget: Some(0),
            enable_thinking: Some(false),
            structured_output: Some(serde_json::json!({
                "type": "string", "enum": ["A", "B"], "x_forced_choice": true
            })),
            ..Default::default()
        };
        let resp = match self.inference.complete(&req).await {
            Ok(r) => r,
            Err(e) => {
                dbg(&format!("sufficiency judge: complete() error: {e}"));
                return None;
            }
        };
        let dist: std::collections::HashMap<String, f64> =
            match serde_json::from_str(resp.text.trim()) {
                Ok(d) => d,
                Err(e) => {
                    dbg(&format!(
                        "sufficiency judge: parse error ({e}) on resp={:?}",
                        resp.text.chars().take(80).collect::<String>()
                    ));
                    return None;
                }
            };
        let a = dist.get("A").copied().unwrap_or(0.0);
        let b = dist.get("B").copied().unwrap_or(0.0);
        let denom = a + b;
        if denom <= 0.0 {
            dbg("sufficiency judge: A+B logprob mass is 0 → None");
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
            world.push_str(&format!(
                "Knowledge base being searched: {}.\n",
                corpora.join(", ")
            ));
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
        // SLOT_POLICY §3 Housekeep: alternative-query generation for
        // re-retrieval — schema-constrained, consumed by the loop.
        let mut req = CompletionRequest::for_workload(Workload::Housekeep, prompt)
            .with_system("You write precise search queries.")
            .with_output_budget(160);
        req.temperature = Some(0.0);
        req.enable_thinking = Some(false);
        req.structured_output = Some(serde_json::json!({
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
        }));
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
            .filter(|q| {
                !q.is_empty() && q.len() <= 80 && !q.to_lowercase().contains("searches for")
            })
            .collect();
        Some(queries)
    }
}

#[cfg(test)]
mod tests {
    use super::question_is_corpus_deictic;

    #[test]
    fn corpus_deictic_catches_story_year_class() {
        assert!(question_is_corpus_deictic("In what year is the story set?"));
        assert!(question_is_corpus_deictic(
            "What year is this document from?"
        ));
        assert!(question_is_corpus_deictic("Who wrote the novel?"));
        assert!(!question_is_corpus_deictic(
            "What is the capital of Canada?"
        ));
        assert!(!question_is_corpus_deictic(
            "How do I reverse a linked list in Python?"
        ));
    }

    /// The env gate must default OFF — existing surfaces and benches
    /// change behaviour only by explicit opt-in.
    #[test]
    fn agentic_kq_default_off() {
        std::env::remove_var("SOVEREIGN_AGENTIC_KQ");
        assert!(!super::agentic_kq_enabled());
    }

    fn chunk(corpus_id: &str) -> corpus_engine::ScoredChunk {
        corpus_engine::ScoredChunk {
            content: String::new(),
            title: None,
            url: None,
            corpus_id: corpus_id.to_string(),
            score: 1.0,
            metadata: std::collections::HashMap::new(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn catalog_only_closes_the_gk_exemption() {
        use corpus_engine::CorpusKind;
        let kinds = std::collections::HashMap::from([
            ("wikipedia-catalog".to_string(), CorpusKind::Catalog),
            ("sep".to_string(), CorpusKind::Knowledge),
        ]);
        // All hits catalog → catalog-only (steps 373/519/535).
        assert!(super::retrieval_is_catalog_only(
            &[chunk("wikipedia-catalog"), chunk("wikipedia-catalog")],
            &kinds,
        ));
        // A single full-text hit means the body can ground an answer → NOT
        // catalog-only, keep the honest GK path.
        assert!(!super::retrieval_is_catalog_only(
            &[chunk("wikipedia-catalog"), chunk("sep")],
            &kinds,
        ));
        // Empty retrieval is owned by the zero-chunk GK-caveat path, not here.
        assert!(!super::retrieval_is_catalog_only(&[], &kinds));
        // Unknown corpus (kinds map empty / index list errored) → not catalog,
        // falls back to prior behaviour.
        assert!(!super::retrieval_is_catalog_only(
            &[chunk("wikipedia-catalog")],
            &std::collections::HashMap::new(),
        ));
    }

    fn titled(corpus_id: &str, title: &str) -> corpus_engine::ScoredChunk {
        let mut c = chunk(corpus_id);
        c.title = Some(title.to_string());
        c
    }

    #[test]
    fn retrieved_title_anchors_the_specific_question() {
        // Step 179: MIXED retrieval — one tangential full-text body chunk rides
        // along with the catalog title, so `retrieval_is_catalog_only` is false,
        // yet the question names exactly the retrieved title. The title anchor
        // must fire here (this is the whole point of #10).
        let mixed = [
            titled("wikipedia-catalog", "1926 Darlington by-election"),
            titled("wikipedia", "Darlington"), // tangential full-text distractor
            titled("wikipedia-catalog", "1866 New Brunswick general election"),
            titled("wikipedia-catalog", "List of United Kingdom by-elections"),
        ];
        assert!(super::question_anchors_retrieved_title(
            "Who won the 1926 Darlington by-election?",
            &mixed,
        ));
    }

    #[test]
    fn generic_shared_word_does_not_anchor() {
        // The distractor titles share ONLY "election" with the question — a
        // single generic word must not anchor (else every election question
        // over-gates). Drop the exact title so only the coattail remains.
        let distractors_only = [
            titled("wikipedia-catalog", "1866 New Brunswick general election"),
            titled("wikipedia-catalog", "List of United Kingdom by-elections"),
            titled("wikipedia", "Electoral reform"),
        ];
        assert!(!super::question_anchors_retrieved_title(
            "Who won the 1926 Darlington by-election?",
            &distractors_only,
        ));
    }

    #[test]
    fn world_general_question_with_no_matching_title_does_not_anchor() {
        // Control (photosynthesis-class): a grounded/world-general question whose
        // retrieved titles are topically unrelated must keep the honest GK path.
        let unrelated = [
            titled("wikipedia", "Cellular respiration"),
            titled("wikipedia", "Calvin cycle"),
        ];
        assert!(!super::question_anchors_retrieved_title(
            "Who won the 1926 Darlington by-election?",
            &unrelated,
        ));
        // A one-significant-word title cannot anchor (below MIN_TITLE_SIG).
        assert!(!super::question_anchors_retrieved_title(
            "Tell me about Darlington.",
            &[titled("wikipedia", "Darlington")],
        ));
        // No titles at all (bodies only) → nothing to anchor on.
        assert!(!super::question_anchors_retrieved_title(
            "Who won the 1926 Darlington by-election?",
            &[chunk("wikipedia-catalog")],
        ));
    }
}
