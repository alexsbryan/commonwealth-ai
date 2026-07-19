// SPDX-License-Identifier: AGPL-3.0-or-later

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::skills::{MergedMemoryConfig, SkillRegister};
use crate::slot_policy::Workload;
use crate::traits::{InferenceProvider, MemoryScope, StateStore};
use crate::types::*;

use crate::time::unix_now as now;

/// Cosine similarity between two equally-sized embedding vectors.
/// Returns 0.0 when either norm is zero or lengths mismatch — the
/// caller treats that as "no signal" and falls back to FTS.
/// (pub(crate): also used by the grounding gate's attached-asset
/// claim search — third in-crate consumer of this shape.)
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Embedding-based memory recall — used on relational/witness paths
/// where keyword FTS misses the seed memories on abstract queries
/// (hard-mode H05: *"what kind of person am I?"* against concrete-
/// event memories shares zero keywords). Retrieves all live
/// memories, scores their content embeddings against the query by
/// cosine similarity, applies the same confidence-decay floor as
/// FTS, returns top-K.
///
/// Falls back to the FTS path on any embedding error (empty query,
/// dim mismatch, batch failure) so the caller never sees a hard
/// failure — the retrieval just degrades to keyword.
///
/// ## T1 persistent embeddings (tiered-retrieval memory-pool port)
///
/// Content embeddings are read from `Memory.embedding` when the
/// stored vector is usable — produced by the same
/// `embed_model_id()` the live provider reports, with the query's
/// dimensionality. Rows without a usable vector (pre-migration rows,
/// model swaps, providers that can't identify their embed model) are
/// batch re-embedded exactly as before and the result is written
/// back best-effort via `update_memory_embedding`, so the O(N)
/// re-embed cost is paid once per pool, not once per turn.
/// Retrieval-equivalent by construction: stored vectors come from
/// the identical document-side `embed_batch` call over the identical
/// content text.
pub async fn recall_relevant_memories_embed(
    inference: &dyn InferenceProvider,
    store: &dyn StateStore,
    scope: &MemoryScope,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    let scored =
        recall_relevant_memories_embed_scored(inference, store, scope, query, limit).await?;
    Ok(strip_embeddings(scored))
}

/// Strip the T1 vectors from rows leaving the recall path — callers
/// render content, and a 1024-float payload per row would bloat every
/// downstream clone/serialize (bench journals, prompt-context
/// plumbing) for no reader.
fn strip_embeddings(scored: Vec<(f32, Memory)>) -> Vec<Memory> {
    scored
        .into_iter()
        .map(|(_, mut m)| {
            m.embedding = None;
            m.embedding_model = None;
            m
        })
        .collect()
}

/// Score-carrying variant of [`recall_relevant_memories_embed`] —
/// same retrieval, but the caller also sees each row's final blended
/// similarity. The score is the confidence signal the LLM-pick gate
/// reads: a strong top score means the bi-encoder already resolved
/// the reference and no LLM assistance is worth its latency.
/// FTS-fallback rows carry score 0.0 (no cosine exists for them).
///
/// Public for the bench probes — gate calibration needs the exact
/// score landscape the gate sees, not a re-derivation.
pub async fn recall_relevant_memories_embed_scored(
    inference: &dyn InferenceProvider,
    store: &dyn StateStore,
    scope: &MemoryScope,
    query: &str,
    limit: usize,
) -> Result<Vec<(f32, Memory)>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Scope-filtered fetch — the wall is enforced before the embed
    // batch so we never embed memories the caller isn't allowed to
    // see. (Embedding scoped memories would leak content via the
    // inference provider's logging/telemetry even if we filtered the
    // result.)
    let all = store
        .get_all_memories_for_scope(scope)
        .await
        .unwrap_or_default();
    if all.is_empty() {
        return Ok(Vec::new());
    }

    let query_emb = match inference.embed_query(query).await {
        Ok(e) if !e.is_empty() => e,
        _ => {
            tracing::debug!("memory: embed recall — query embed failed, falling back to FTS");
            return Ok(store
                .get_relevant_memories_for_scope(scope, query, limit)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|m| (0.0, m))
                .collect());
        }
    };

    // Partition into rows with a usable stored T1 embedding vs rows
    // that need a fresh embed. "unknown" from `embed_model_id()`
    // means the provider cannot identify its embed model — never
    // match against it and never persist under it (a silent model
    // swap would mis-rank).
    let current_model = inference.embed_model_id();
    let model_known = current_model != "unknown";
    let mut embs: Vec<Option<Vec<f32>>> = Vec::with_capacity(all.len());
    let mut missing_idx: Vec<usize> = Vec::new();
    for (i, m) in all.iter().enumerate() {
        // "Attempted under the current model" = the stored
        // `embedding_model` equals the live model. Such a row has already
        // had its one embed attempt for this model: if that attempt
        // produced a usable vector we score it; if it produced an EMPTY /
        // wrong-length vector (content that is empty or unembeddable) we
        // skip it — but we do NOT re-embed it. Deriving "needs embed"
        // purely from vector presence re-embedded every unembeddable row
        // on EVERY recall turn, forever (a done-set derived from produced
        // output never converging — same class as the NER delta bug).
        // Only rows never attempted under the current model — or under an
        // "unknown" model we refuse to persist against — go into
        // `missing_idx`.
        let attempted_under_current =
            model_known && m.embedding_model.as_deref() == Some(current_model.as_str());
        let usable = attempted_under_current
            && m.embedding
                .as_ref()
                .is_some_and(|e| e.len() == query_emb.len());
        if usable {
            embs.push(m.embedding.clone());
        } else if attempted_under_current {
            // Tried under this exact model, no usable vector — the content
            // is unembeddable. It contributes nothing to cosine scoring;
            // skip it without re-embedding.
            embs.push(None);
        } else {
            embs.push(None);
            missing_idx.push(i);
        }
    }

    if !missing_idx.is_empty() {
        let texts: Vec<String> = missing_idx
            .iter()
            .map(|&i| all[i].content.clone())
            .collect();
        match inference.embed_batch(&texts).await {
            Ok(fresh) if fresh.len() == texts.len() => {
                for (&i, emb) in missing_idx.iter().zip(fresh) {
                    // Lazy backfill — best-effort: a failed write just
                    // means this row re-embeds next turn.
                    //
                    // Persist under the current model REGARDLESS of whether
                    // the vector is empty. An empty result means the content
                    // is unembeddable; stamping `embedding_model =
                    // current_model` anyway records that this row was
                    // ATTEMPTED under this model, so next turn it is
                    // recognised as done-but-empty (above) and not
                    // re-embedded forever. A later model swap
                    // (`embedding_model` != the new live model) correctly
                    // re-attempts. `model_known` still gates persistence: we
                    // never write under an "unknown" model we can't trust to
                    // rank against later.
                    if model_known {
                        if let Err(e) = store
                            .update_memory_embedding(&all[i].id, &emb, &current_model)
                            .await
                        {
                            tracing::debug!(
                                id = %all[i].id,
                                error = %e,
                                "memory: embed recall — embedding backfill write failed"
                            );
                        }
                    }
                    // An empty vector never contributes to cosine scoring —
                    // store None so the scoring loop's `emb?` skips it.
                    embs[i] = if emb.is_empty() { None } else { Some(emb) };
                }
            }
            _ => {
                tracing::debug!(
                    memories = missing_idx.len(),
                    "memory: embed recall — batch embed failed, falling back to FTS"
                );
                return Ok(store
                    .get_relevant_memories_for_scope(scope, query, limit)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| (0.0, m))
                    .collect());
            }
        }
    }
    let stored_hits = all.len() - missing_idx.len();

    // ── T3 tier-aware boost (memory RAPTOR; spec
    // TIERED_RETRIEVAL_MEMORIES.md) ────────────────────────────────
    //
    // A summary node ("ongoing grief for their father, hardest
    // through spring") embeds far closer to an oblique callback
    // ("that night in the spring") than any raw leaf does. When the
    // scope has a tree whose embeddings the live provider can vouch
    // for, cosine the query against each node summary; a matched
    // node lifts its member leaves to
    //
    //   score = max(leaf_cos, α·node_cos + (1−α)·leaf_cos)
    //
    // The blend (NOT a flat `α·node_cos`) is load-bearing: a flat
    // boost gives every member of the matched cluster the same
    // score, so the sought memory ties with its ~15 cluster
    // siblings and its rank inside the cluster is arbitrary. The
    // (1−α)·leaf term preserves within-cluster leaf ordering while
    // the α·node term lifts the whole cluster past unmatched-cluster
    // distractors. Empty node list (no tree, tiny pool,
    // non-persisting store, model swap) leaves scoring
    // byte-identical to flat T1.
    let alpha = std::env::var("SOVEREIGN_MEM_TIER_ALPHA")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .map(|a| a.clamp(0.0, 1.0))
        .unwrap_or(0.85);
    let mut tier_boost: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    if model_known {
        let nodes = store
            .list_mem_raptor_nodes(&scope.atlas_key())
            .await
            .unwrap_or_default();
        if !nodes.is_empty() {
            // Weak node matches carry no signal — don't let them
            // shuffle the tail of the ranking.
            const NODE_FLOOR: f32 = 0.30;
            // Only LEAF clusters boost. Higher-level nodes' evidence
            // converges on the whole pool (the root covers every
            // memory), so their boost lifts everything equally — zero
            // discriminating power, measured on the recall probe
            // (2026-07-08): the root matched every callback at ~0.4
            // and moved no ranks.
            let mut matched_nodes = 0usize;
            for node in nodes.iter().filter(|n| n.level == 0) {
                if node.embedding_model != current_model
                    || node.summary_embedding.len() != query_emb.len()
                {
                    continue;
                }
                let node_sim = cosine_similarity(&query_emb, &node.summary_embedding);
                if node_sim < NODE_FLOOR {
                    continue;
                }
                matched_nodes += 1;
                for mid in &node.evidence_memory_ids {
                    tier_boost
                        .entry(mid.clone())
                        .and_modify(|b| *b = b.max(node_sim))
                        .or_insert(node_sim);
                }
            }
            // Stored per-memory as the BEST matching node sim; the
            // blend against the leaf happens at scoring time below.
            tracing::debug!(
                nodes = nodes.len(),
                matched_nodes,
                boosted_memories = tier_boost.len(),
                alpha,
                scope = ?scope,
                "memory: embed recall — T3 tier boost active"
            );
        }
    }

    let now_ts = now();
    let in_scope = all.len();
    let mut scored: Vec<(f32, Memory)> = embs
        .into_iter()
        .zip(all)
        .filter_map(|(emb, m)| {
            // Same confidence-decay floor as FTS path
            // (sqlite::get_relevant_memories): drop memories whose
            // decayed confidence falls below 0.2.
            let months = (now_ts - m.last_used) as f64 / (30.0 * 86400.0);
            let decayed = m.confidence * 0.9_f64.powf(months.max(0.0));
            if decayed < 0.2 {
                return None;
            }
            let leaf = cosine_similarity(&query_emb, &emb?);
            let sim = tier_boost.get(&m.id).map_or(leaf, |node_sim| {
                leaf.max(alpha * node_sim + (1.0 - alpha) * leaf)
            });
            Some((sim, m))
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    tracing::debug!(
        in_scope,
        stored_embeddings = stored_hits,
        freshly_embedded = in_scope - stored_hits,
        returned = scored.len(),
        limit,
        scope = ?scope,
        top_ids = ?scored.iter().map(|(_, m)| m.id.as_str()).collect::<Vec<_>>(),
        "memory: embed recall — returning top-K by cosine"
    );
    Ok(scored)
}

/// Candidate pool handed to the cross-encoder when reranking is
/// active. Wider than the final window so the reranker can promote a
/// vocabulary-disjoint memory that bi-encoder cosine buried (the
/// measured failure mode: the sought memory at leaf-rank 42 in a
/// 0.42-cosine field). Env-tunable via `SOVEREIGN_MEM_RERANK_POOL`.
const MEM_RERANK_POOL_DEFAULT: usize = 16;

/// [`recall_relevant_memories_embed`] plus an optional cross-encoder
/// rerank pass over a widened candidate pool.
///
/// Opt-in by construction: when `rerank_fn` is `None` (no reranker
/// configured — the common deployment) or `SOVEREIGN_MEM_RERANK=0`,
/// this is byte-identical to the plain embed recall — same pool, same
/// scores, zero added cost. When active, the bi-encoder ranking
/// fetches `MEM_RERANK_POOL` candidates and the cross-encoder decides
/// the final top-`limit`; a rerank failure degrades to the bi-encoder
/// order, never to an error. The pass is timed and traced so the
/// witness-latency cost is always visible next to its benefit.
pub async fn recall_relevant_memories_embed_reranked(
    inference: &dyn InferenceProvider,
    store: &dyn StateStore,
    scope: &MemoryScope,
    query: &str,
    limit: usize,
    rerank_fn: Option<&corpus_engine::RerankFn>,
) -> Result<Vec<Memory>> {
    // LLM-pick stage (opt-in, `SOVEREIGN_MEM_PICK=1`) takes
    // precedence over the cross-encoder when both are enabled — it
    // measured strictly better on the recall fixture (2026-07-09):
    // pointwise 0.6B cross-encoders could not discriminate within a
    // journal of emotionally-adjacent entries, while an LLM reading
    // the pool solved the deep-buried callbacks.
    if std::env::var("SOVEREIGN_MEM_PICK").is_ok_and(|v| v == "1") {
        return recall_llm_picked(inference, store, scope, query, limit).await;
    }

    // Opt-IN (`SOVEREIGN_MEM_RERANK=1`), never inherited from the
    // corpus reranker's mere presence: measured on the recall probe
    // (2026-07-09, jina-reranker-v3-Q8), the cross-encoder DEMOTED
    // 5 of 6 correctly-retrieved plants out of the top-10 and added
    // ~420ms per recall (~10× the witness recall budget). The seam
    // stays for bench measurement of future rerankers; production
    // memory recall stays bi-encoder until one measures well here.
    let rerank_enabled =
        rerank_fn.is_some() && std::env::var("SOVEREIGN_MEM_RERANK").is_ok_and(|v| v == "1");
    let Some(rerank_fn) = rerank_fn.filter(|_| rerank_enabled) else {
        return recall_relevant_memories_embed(inference, store, scope, query, limit).await;
    };

    let pool_size = std::env::var("SOVEREIGN_MEM_RERANK_POOL")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(MEM_RERANK_POOL_DEFAULT)
        .max(limit);
    let pool = recall_relevant_memories_embed(inference, store, scope, query, pool_size).await?;
    if pool.len() <= 1 {
        return Ok(pool.into_iter().take(limit).collect());
    }

    let started = std::time::Instant::now();
    let docs: Vec<String> = pool.iter().map(|m| m.content.clone()).collect();
    match rerank_fn(query, docs).await {
        Ok(scores) if scores.len() == pool.len() => {
            let mut scored: Vec<(f32, Memory)> = scores.into_iter().zip(pool).collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            let top: Vec<Memory> = scored.into_iter().take(limit).map(|(_, m)| m).collect();
            tracing::debug!(
                pool = top.len().max(limit),
                rerank_ms = started.elapsed().as_millis() as u64,
                scope = ?scope,
                top_ids = ?top.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
                "memory: embed recall — cross-encoder rerank applied"
            );
            Ok(top)
        }
        Ok(scores) => {
            tracing::warn!(
                got = scores.len(),
                want = pool.len(),
                "memory: rerank returned wrong score count — keeping bi-encoder order"
            );
            Ok(pool.into_iter().take(limit).collect())
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "memory: rerank failed — keeping bi-encoder order"
            );
            Ok(pool.into_iter().take(limit).collect())
        }
    }
}

/// Candidate pool for the LLM-pick stage. Must reach past the hard
/// callbacks' bi-encoder depth (measured: the grief plant at
/// leaf-rank 42). Env-tunable via `SOVEREIGN_MEM_PICK_POOL`.
const MEM_PICK_POOL_DEFAULT: usize = 48;

/// Ambiguity gate: the picker fires only when the bi-encoder's
/// top1−top2 MARGIN falls below this. Measured on the recall fixture
/// (2026-07-09 probe, blended scores): the two callbacks the
/// bi-encoder misses have margin 0.000 — an exactly tied field —
/// while every solved callback has a dominant winner at margin
/// 0.022–0.131. An absolute-score gate cannot make this cut (solved
/// top1 spans 0.488–0.648, overlapping the ambiguous fields at
/// ~0.50) — an aborted A/B campaign proved that the expensive way.
/// A false FIRE costs only latency (the picker preserved every
/// bi-encoder-solved callback in all measured runs), so the default
/// sits just above the thinnest solved margin. Env-tunable via
/// `SOVEREIGN_MEM_PICK_MARGIN`.
const MEM_PICK_MARGIN_DEFAULT: f32 = 0.025;

/// LLM-pick recall (opt-in via `SOVEREIGN_MEM_PICK=1`): bi-encoder
/// pool of `MEM_PICK_POOL` candidates, then — ONLY when the field has
/// no dominant winner (margin gate) — one structured completion that
/// reads the pool and picks the entries the user is referring back
/// to. Picked entries lead, cosine order fills the remainder.
///
/// Cost model (measured 2026-07-09): the gate keeps confident recalls
/// at bi-encoder speed (~40ms); ambiguous recalls pay one LLM read of
/// ~48 snippets (~3.6s on the local 4B/35B) and in exchange the
/// deep-buried oblique callbacks become renderable (daughter rank
/// 7 → TOP-1 deterministically; grief reaches the rendered window
/// when picked). Any pick failure degrades to plain bi-encoder order.
async fn recall_llm_picked(
    inference: &dyn InferenceProvider,
    store: &dyn StateStore,
    scope: &MemoryScope,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    let pool_n = std::env::var("SOVEREIGN_MEM_PICK_POOL")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(MEM_PICK_POOL_DEFAULT)
        .max(limit);
    let margin_gate = std::env::var("SOVEREIGN_MEM_PICK_MARGIN")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(MEM_PICK_MARGIN_DEFAULT);

    let scored =
        recall_relevant_memories_embed_scored(inference, store, scope, query, pool_n).await?;
    if scored.is_empty() {
        return Ok(Vec::new());
    }
    let margin = if scored.len() >= 2 {
        scored[0].0 - scored[1].0
    } else {
        f32::MAX
    };
    if margin >= margin_gate || scored.len() <= limit {
        tracing::debug!(
            top_score = scored[0].0,
            margin,
            margin_gate,
            "memory: llm-pick gate — dominant bi-encoder winner, no LLM call"
        );
        return Ok(strip_embeddings(scored.into_iter().take(limit).collect()));
    }

    let started = std::time::Instant::now();
    let entries = scored
        .iter()
        .enumerate()
        .map(|(i, (_, m))| format!("[{i}] {}", m.content.chars().take(180).collect::<String>()))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "A person says to their journaling companion:\n\"{query}\"\n\n\
         They may be calling back to something specific they shared before. Below are their \
         stored journal memories. Pick the entries they are most likely referring back to.\n\n\
         {entries}\n\n\
         Reply with JSON only: {{\"indices\": [/* up to {limit} entry numbers, most likely \
         first */]}}"
    );
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "indices": { "type": "array", "items": { "type": "integer" }, "maxItems": limit }
        },
        "required": ["indices"],
        "additionalProperties": false
    });
    let mut req = CompletionRequest::new(&prompt).with_speed(Speed::Fast);
    req.structured_output = Some(schema);
    req.temperature = Some(0.0);
    req.max_tokens = Some(120);
    req.enable_thinking = Some(false);

    #[derive(serde::Deserialize)]
    struct Picked {
        #[serde(default)]
        indices: Vec<i64>,
    }
    let picked_idx: Vec<usize> = match inference.complete(&req).await {
        Ok(resp) => {
            let tail = crate::title::strip_thinking_response(&resp.text);
            let json = tail
                .find('{')
                .and_then(|s| tail.rfind('}').map(|e| &tail[s..=e]))
                .unwrap_or("");
            serde_json::from_str::<Picked>(json)
                .map(|p| {
                    p.indices
                        .into_iter()
                        .filter_map(|i| usize::try_from(i).ok())
                        .filter(|&i| i < scored.len())
                        .collect()
                })
                .unwrap_or_default()
        }
        Err(e) => {
            tracing::warn!(error = %e, "memory: llm-pick failed — keeping bi-encoder order");
            Vec::new()
        }
    };

    // Picked entries lead (in pick order), the rest follow in cosine
    // order — the witness's rendered window sees the picker's best
    // guesses first, and a bad pick can only reorder, never lose the
    // bi-encoder's own candidates.
    let mut order: Vec<usize> = picked_idx.clone();
    for i in 0..scored.len() {
        if !order.contains(&i) {
            order.push(i);
        }
    }
    let mut by_idx: Vec<Option<(f32, Memory)>> = scored.into_iter().map(Some).collect();
    let reordered: Vec<(f32, Memory)> = order
        .into_iter()
        .filter_map(|i| by_idx.get_mut(i).and_then(|slot| slot.take()))
        .take(limit)
        .collect();
    tracing::debug!(
        picked = picked_idx.len(),
        pick_ms = started.elapsed().as_millis() as u64,
        scope = ?scope,
        "memory: llm-pick applied"
    );
    Ok(strip_embeddings(reordered))
}

// ─── Working Memory Compression ───────────────────────────────

/// Compress recent conversation messages into a structured WorkingMemory.
/// Uses the Fast slot for low latency since this runs on every message.
pub async fn compress_working_memory(
    inference: &dyn InferenceProvider,
    messages: &[Message],
    previous: Option<&WorkingMemory>,
) -> Result<WorkingMemory> {
    tracing::debug!(
        messages = messages.len(),
        has_previous = previous.is_some(),
        "memory: compress_working_memory — begin"
    );

    if messages.len() < 2 {
        tracing::debug!("memory: compress_working_memory — not enough messages, skipping");
        return Ok(previous.cloned().unwrap_or(WorkingMemory {
            current_goal: None,
            facts: Vec::new(),
            active_documents: Vec::new(),
        }));
    }

    // Format last 8 messages.
    let recent: Vec<String> = messages
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            let mut end = m.content.len().min(300);
            while end > 0 && !m.content.is_char_boundary(end) {
                end -= 1;
            }
            format!("{role}: {}", &m.content[..end])
        })
        .collect();

    let mut context_prefix = String::new();
    if let Some(prev) = previous {
        if let Some(goal) = &prev.current_goal {
            context_prefix.push_str(&format!("Previous goal: {goal}\n"));
        }
        if !prev.facts.is_empty() {
            context_prefix.push_str(&format!(
                "Known facts: {}\n",
                prev.facts
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    }

    let prompt = format!(
        "{context_prefix}Conversation:\n{}\n\n\
         Produce a JSON object with:\n\
         - \"goal\": the user's current goal (string or null)\n\
         - \"facts\": array of short factual statements established so far\n\n\
         Respond with only the JSON object.",
        recent.join("\n")
    );

    // SLOT_POLICY §3 Housekeep: working-memory compression is advisory
    // context, not durable truth. Bundle supplies latency=Fast (shadow
    // Speed::Fast — pinned by `summarize_dropped_history_uses_fast_slot_only`).
    let mut request = CompletionRequest::for_workload(Workload::Housekeep, prompt)
        .with_system(
            "Extract the user's goal and key facts from the conversation. Respond with JSON only.",
        )
        .with_output_budget(200);
    request.temperature = Some(0.1);
    // POLICY-DEBT(SLOT_POLICY §3 Housekeep): the Housekeep bundle sets
    // think_budget=0, but this site historically left it at the config
    // default (None) and is NOT schema-constrained, so 0 could change
    // generation. Preserve None for P1 neutrality; P5 confirms the 0.
    request.think_budget = None;

    let response = inference.complete(&request).await?;
    let result = parse_working_memory(&response.text, previous);
    if let Ok(ref wm) = result {
        tracing::debug!(
            has_goal = wm.current_goal.is_some(),
            fact_count = wm.facts.len(),
            "memory: compress_working_memory — done"
        );
    }
    result
}

/// Parse working memory from LLM response, with fallback.
fn parse_working_memory(text: &str, previous: Option<&WorkingMemory>) -> Result<WorkingMemory> {
    // Try full JSON parse first.
    if let Some(json_str) = extract_json_object(text) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
            let goal = val
                .get("goal")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let facts: Vec<String> = val
                .get("facts")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            return Ok(WorkingMemory {
                current_goal: goal,
                facts,
                active_documents: Vec::new(),
            });
        }
    }

    // Fallback: return previous or empty.
    Ok(previous.cloned().unwrap_or(WorkingMemory {
        current_goal: None,
        facts: Vec::new(),
        active_documents: Vec::new(),
    }))
}

/// Extract a JSON object substring from text.
fn extract_json_object(text: &str) -> Option<String> {
    // Try ```json ... ``` fence.
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    // Try bare { ... }.
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return Some(text[start..=end].to_string());
            }
        }
    }
    None
}

// ─── Long-Term Memory Extraction ──────────────────────────────

/// Extract durable facts from a conversation for long-term storage.
/// Uses the Primary (Slow) slot for better extraction quality.
pub async fn extract_long_term_memories(
    inference: &dyn InferenceProvider,
    messages: &[Message],
    memory_rules: &MergedMemoryConfig,
) -> Result<Vec<Memory>> {
    tracing::debug!(
        messages = messages.len(),
        "memory: extract_long_term_memories — begin"
    );

    if messages.len() < 4 {
        tracing::debug!("memory: extract_long_term_memories — not enough messages, skipping");
        return Ok(Vec::new());
    }

    // Format conversation.
    let conversation_text: String = messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            format!("{role}: {}", &m.content[..m.content.len().min(500)])
        })
        .collect::<Vec<_>>()
        .join("\n");

    let addenda = if memory_rules.extraction_addenda.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nAdditional extraction rules:\n{}",
            memory_rules.extraction_addenda.join("\n")
        )
    };

    let prompt = format!(
        "Given this conversation, extract any durable facts about the user that would be \
         useful in future conversations. Only extract clearly true, persistently relevant \
         things (preferences, profession, location, tools used, etc.). Do not extract \
         transient requests or conversation-specific details.\n\n\
         Conversation:\n{conversation_text}{addenda}\n\n\
         Return a JSON array of strings, each a single fact. Return [] if none."
    );

    let request = CompletionRequest {
        prompt,
        system_message: Some(
            "You extract durable user facts from conversations. Respond with a JSON array of strings only."
                .to_string(),
        ),
        preferred_speed: Speed::Slow,
        max_tokens: Some(300),
        temperature: Some(0.3),
        structured_output: None,
            think_budget: None,
        top_k: None,
        top_p: None,
        oicp: None,
                tools: None,
                tool_choice: None,
                    model_id: None,
                    enable_thinking: None,
    sampling_mode: None,
    assistant_prefix: None,
    cmd_prefix: None,
    url_allowlist: None,
    evidence_id_allowlist: None,
    lark_grammar: None,
    };

    let response = inference.complete(&request).await?;
    let result = parse_extracted_memories(&response.text);
    if let Ok(ref memories) = result {
        tracing::info!(
            extracted = memories.len(),
            "memory: extract_long_term_memories — done"
        );
    }
    result
}

/// Parse extracted memories from LLM response.
fn parse_extracted_memories(text: &str) -> Result<Vec<Memory>> {
    let current_time = now();

    // Try to find JSON array.
    let json_str = if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            if end > start {
                &text[start..=end]
            } else {
                return Ok(Vec::new());
            }
        } else {
            return Ok(Vec::new());
        }
    } else {
        return Ok(Vec::new());
    };

    let facts: Vec<String> = serde_json::from_str(json_str).unwrap_or_default();

    Ok(facts
        .into_iter()
        .filter(|f| f.len() > 3)
        .map(|content| Memory {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            source: "conversation_extraction".to_string(),
            confidence: 1.0,
            created_at: current_time,
            last_used: current_time,
            version: current_time,
            deleted_at: None,
            source_conversation_id: None,
            source_skill_id: None,
            ..Default::default()
        })
        .collect())
}

// ─── Memory Prompt Injection ──────────────────────────────────

/// Confidence threshold for the "directly stated" register —
/// memories at or above this are presented as things the user
/// said in their own words.
const RELATIONAL_DIRECT_THRESHOLD: f64 = 0.85;

/// Confidence threshold for the "inferred" register — memories
/// between this and `RELATIONAL_DIRECT_THRESHOLD` are presented as
/// patterns read across conversations rather than verbatim claims.
/// Memories below this threshold land in the "tentative" band.
const RELATIONAL_INFER_THRESHOLD: f64 = 0.5;

/// Derive the epistemic band for a memory's stored confidence — the
/// SAME thresholds `format_relational` uses to band the prompt, so the
/// ledger and the prompt can never disagree about a recall's register.
pub(crate) fn band_for_confidence(confidence: f64) -> crate::types::MemoryBand {
    if confidence >= RELATIONAL_DIRECT_THRESHOLD {
        crate::types::MemoryBand::ToldDirectly
    } else if confidence >= RELATIONAL_INFER_THRESHOLD {
        crate::types::MemoryBand::Inferred
    } else {
        crate::types::MemoryBand::Tentative
    }
}

/// Format memories for injection into system prompts. The `register`
/// argument determines the surface shape:
///
/// * `Factual` — flat bulleted list under the heading "Known facts
///   about the user:". Pre-existing behavior; preserved for the
///   default voice contract.
/// * `Relational` — three confidence-banded sections that the model
///   can render into its three epistemic registers (history /
///   inference / guess). Memories whose `source_conversation_id` is
///   set get a `[YYYY-MM-DD]` prefix derived from `created_at`, so
///   the model can produce situated phrasing like "you told me on
///   2026-03-12 that…" instead of flat assertions.
///
/// Returns `None` when `memories` is empty.
pub fn format_memories_for_prompt(memories: &[Memory], register: SkillRegister) -> Option<String> {
    if memories.is_empty() {
        return None;
    }

    match register {
        SkillRegister::Factual => format_factual(memories),
        SkillRegister::Relational => format_relational(memories),
    }
}

fn format_factual(memories: &[Memory]) -> Option<String> {
    let items: Vec<String> = memories
        .iter()
        .map(|m| format!("- {}", m.content))
        .collect();
    Some(format!("Known facts about the user:\n{}", items.join("\n")))
}

fn format_relational(memories: &[Memory]) -> Option<String> {
    let mut directly: Vec<&Memory> = Vec::new();
    let mut inferred: Vec<&Memory> = Vec::new();
    let mut tentative: Vec<&Memory> = Vec::new();
    for m in memories {
        if m.confidence >= RELATIONAL_DIRECT_THRESHOLD {
            directly.push(m);
        } else if m.confidence >= RELATIONAL_INFER_THRESHOLD {
            inferred.push(m);
        } else {
            tentative.push(m);
        }
    }

    let mut sections: Vec<String> = Vec::new();
    if !directly.is_empty() {
        sections.push(format!(
            "What you've told me directly:\n{}",
            render_band(&directly).join("\n")
        ));
    }
    if !inferred.is_empty() {
        sections.push(format!(
            "What I've inferred from earlier conversations:\n{}",
            render_band(&inferred).join("\n")
        ));
    }
    if !tentative.is_empty() {
        sections.push(format!(
            "Tentative — flag these as guesses if you surface them:\n{}",
            render_band(&tentative).join("\n")
        ));
    }

    Some(sections.join("\n\n"))
}

fn render_band(memories: &[&Memory]) -> Vec<String> {
    memories
        .iter()
        .map(|m| {
            let date_prefix = m
                .source_conversation_id
                .as_ref()
                .and_then(|_| format_unix_date(m.created_at))
                .map(|d| format!("[{d}] "))
                .unwrap_or_default();
            let summary_prefix = match m.kind {
                MemoryKind::Summary => format!(
                    "[summary of {n} entries] ",
                    n = m.source_memory_ids.len().max(1)
                ),
                MemoryKind::Raw => String::new(),
            };
            // No per-entry confidence annotation: the three bands
            // already encode it, and a hand-read (2026-07-09) caught
            // the witness echoing "(confidence 0.85)" verbatim into a
            // user-facing reply — internals leaking through the one
            // surface that must feel human.
            format!("- {summary_prefix}{date_prefix}{}", m.content)
        })
        .collect()
}

/// Render a Unix timestamp (seconds) as `YYYY-MM-DD` in UTC.
/// Returns `None` for negative timestamps and timestamps that don't
/// resolve to a valid date — both treated as missing-date cases so
/// the renderer can fall through to an undated bullet.
fn format_unix_date(ts: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
}

// ─── Temporal Tension Detection ───────────────────────────────

/// Maximum number of memories considered in a single tension
/// pre-pass. Bounds the Quick-slot inference cost so adding the
/// pre-pass doesn't dominate per-turn latency. Picked at 5 because
/// the directly-stated band is by construction a small set per
/// retrieval call (top-K=5 in current production), and at K=5 the
/// classifier batch fits comfortably under the Fast slot's
/// 1024-token budget.
const MAX_TENSION_CANDIDATES: usize = 5;

/// Maximum char length of the user-message excerpt that's spliced
/// alongside a tension. Bounds the prompt size so a long pasted
/// passage doesn't bloat every turn's system prompt. The model
/// only needs the gist of what the user just said; the full
/// message is in the conversation history immediately below.
const TENSION_EXCERPT_CHAR_CAP: usize = 240;

/// JSON shape the Quick-slot classifier is asked to return, one
/// item per candidate memory.
#[derive(Debug, serde::Deserialize)]
struct TensionClassification {
    index: usize,
    relation: String,
}

/// Detect tensions between the user's current message and prior
/// directly-stated memories. Implements principle 5 ("surface
/// contradictions across time") of the relational voice contract.
///
/// Inputs:
/// * `inference` — provider used to make a single Fast-slot call.
/// * `current_message` — what the user just said.
/// * `memories` — the memories already loaded into the
///   conversation context (from FTS retrieval). Filtered here to
///   the directly-stated band (`confidence ≥ RELATIONAL_DIRECT_THRESHOLD`)
///   so guesses and inferences don't seed false-positive tensions.
///
/// Behaviour:
/// * Returns `Ok(Vec::new())` when there are no candidate memories
///   — common case for casual chat, costs zero inference.
/// * Issues one Fast-slot batched JSON-classifier call. Soft-fails
///   on parse error (returns empty rather than blocking the turn).
/// * Returns at most `MAX_TENSION_CANDIDATES` tensions.
///
/// The function is register-agnostic — the *caller* (the Runtime)
/// is responsible for skipping it for factual skills. Keeping the
/// gate in the caller avoids threading `SkillRegister` through
/// memory's public surface and keeps this fn unit-testable in
/// isolation.
pub async fn detect_temporal_tensions(
    inference: &dyn InferenceProvider,
    current_message: &str,
    memories: &[Memory],
) -> Result<Vec<TemporalTension>> {
    let candidates: Vec<&Memory> = memories
        .iter()
        .filter(|m| m.confidence >= RELATIONAL_DIRECT_THRESHOLD)
        .filter(|m| m.deleted_at.is_none())
        .take(MAX_TENSION_CANDIDATES)
        .collect();

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let listing = candidates
        .iter()
        .enumerate()
        .map(|(i, m)| {
            // Memory.content is user-derived but JSON-escape it
            // before splicing into the prompt — defensive against
            // quotes / newlines that would corrupt the listing.
            format!(
                "{{\"index\": {i}, \"memory\": {}}}",
                serde_json::to_string(&m.content).unwrap_or_else(|_| "\"\"".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");

    let prompt = format!(
        "You are a tension-detector for a situated conversation. Compare each \
prior memory the user expressed against the user's current message. Classify \
each pairing as exactly one of:\n\
- \"tension\" — the new statement materially contradicts the prior memory, OR \
describes the same subject in a way that would benefit from gentle surfacing \
(e.g., \"I'm leaving the job\" vs. \"I want to grow here\").\n\
- \"consistent\" — the new statement reinforces or naturally extends the prior memory.\n\
- \"neutral\" — the topics don't relate enough to evaluate.\n\n\
Bias toward \"neutral\" when uncertain. \"tension\" should be a deliberate \
flag, not a default.\n\n\
User's current message:\n{current_message}\n\n\
Prior memories (JSON):\n[\n{listing}\n]\n\n\
Reply with a JSON array, one entry per memory, in the original order:\n\
[{{\"index\": <i>, \"relation\": \"consistent|neutral|tension\"}}, ...]"
    );

    let schema = serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "index": { "type": "integer", "minimum": 0 },
                "relation": { "type": "string", "enum": ["consistent", "neutral", "tension"] }
            },
            "required": ["index", "relation"],
            "additionalProperties": false
        }
    });

    // Temporal-tension classification guards the durable memory store,
    // so it declares the ExtractDurable workload (Normal latency →
    // primary slot). It formerly ran on the Fast slot as a P1-neutrality
    // holdover; flipped to the mandated class (2026-07-08) because it is
    // a background task — latency buys nothing — and a corruption guard
    // deserves the stronger model. LocalOnly: internal machinery, never
    // offloaded. think_budget stays None (the bundle default), so the
    // only change is the slot, not the generation.
    let mut request = CompletionRequest::for_workload(Workload::ExtractDurable, &prompt);
    request.structured_output = Some(schema);
    request.max_tokens = Some(512);

    let response = inference.complete(&request).await?;
    let parsed = parse_tension_classifications(&response.text);

    let excerpt = excerpt_message(current_message);
    let tensions: Vec<TemporalTension> = parsed
        .into_iter()
        .filter(|item| item.relation == "tension")
        .filter_map(|item| {
            candidates.get(item.index).map(|m| TemporalTension {
                memory_id: m.id.clone(),
                prior_content: m.content.clone(),
                prior_created_at: m.created_at,
                prior_has_source_conversation: m.source_conversation_id.is_some(),
                current_excerpt: excerpt.clone(),
            })
        })
        .collect();

    Ok(tensions)
}

fn excerpt_message(msg: &str) -> String {
    if msg.chars().count() <= TENSION_EXCERPT_CHAR_CAP {
        msg.to_string()
    } else {
        let head: String = msg.chars().take(TENSION_EXCERPT_CHAR_CAP).collect();
        format!("{head}…")
    }
}

/// Parse the Quick-slot classifier's response. Soft-fail policy:
/// any deviation from the schema yields an empty `Vec`, NOT an
/// error — a malformed pre-pass response must never block a turn,
/// it just suppresses the tension-surfacing cue and the model
/// continues without it.
fn parse_tension_classifications(text: &str) -> Vec<TensionClassification> {
    // Try the raw text first — the structured_output path should
    // produce a clean JSON array directly.
    if let Ok(items) = serde_json::from_str::<Vec<TensionClassification>>(text.trim()) {
        return items;
    }
    // Fallback: extract bracketed array from a possibly-fenced or
    // prose-wrapped response.
    if let Some(arr) = extract_json_array(text) {
        if let Ok(items) = serde_json::from_str::<Vec<TensionClassification>>(&arr) {
            return items;
        }
    }
    Vec::new()
}

/// Extract a `[...]` JSON array from text that may contain code
/// fences or trailing prose. Mirrors `extract_json_object` but for
/// arrays.
fn extract_json_array(text: &str) -> Option<String> {
    if let Some(start) = text.find("```json") {
        let after = &text[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            if end > start {
                return Some(text[start..=end].to_string());
            }
        }
    }
    None
}

// ─── Contradiction Detection ──────────────────────────────────

/// Detect which existing memories are contradicted by a new fact.
/// Returns IDs of memories that should be deleted.
pub async fn detect_contradictions(
    inference: &dyn InferenceProvider,
    new_memory: &Memory,
    existing: &[Memory],
) -> Result<Vec<String>> {
    if existing.is_empty() {
        return Ok(Vec::new());
    }

    let numbered: String = existing
        .iter()
        .enumerate()
        .map(|(i, m)| format!("{}. {}", i + 1, m.content))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "New fact: \"{}\"\n\n\
         Existing facts:\n{numbered}\n\n\
         Which existing facts, if any, are CONTRADICTED by the new fact? \
         A contradiction means the new fact makes the old fact false. \
         Return a JSON array of the numbers of contradicted facts, or [] if none.",
        new_memory.content,
    );

    // Contradiction detection guards the durable memory store from
    // corruption, so it declares the ExtractDurable workload (Normal
    // latency → primary slot). It formerly ran on the Fast slot as a
    // P1-neutrality holdover; flipped to the mandated class (2026-07-08)
    // because it is a background task — latency buys nothing — and a
    // corruption guard deserves the stronger model. LocalOnly: internal
    // machinery, never offloaded. The ExtractDurable bundle's
    // think_budget is None, matching the prior value, so the flip
    // changes the slot, not the generation.
    let mut request = CompletionRequest::for_workload(Workload::ExtractDurable, prompt);
    request.system_message =
        Some("Identify contradictions. Respond with a JSON array of numbers only.".to_string());
    request.max_tokens = Some(50);
    request.temperature = Some(0.0);

    let response = inference.complete(&request).await?;

    // Parse array of indices.
    let indices: Vec<usize> = if let Some(start) = response.text.find('[') {
        if let Some(end) = response.text.rfind(']') {
            serde_json::from_str(&response.text[start..=end]).unwrap_or_default()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Map 1-based indices back to memory IDs.
    let ids: Vec<String> = indices
        .into_iter()
        .filter_map(|i| existing.get(i.wrapping_sub(1)).map(|m| m.id.clone()))
        .collect();

    Ok(ids)
}

// ─── Confidence Decay ─────────────────────────────────────────

/// Default uniform decay rate: 10% confidence loss per month.
/// Exposed so the Runtime's prune path can construct an explicit
/// `prune_decayed_memories_with_config` call when it has an entity
/// inventory available.
///
/// **Tuning history (2026)**: three of the retired research-shaped
/// skills (research-analyst, epistemic-research, collaborative-
/// research) declared `confidence_decay_per_month = 0.05` and
/// `prune_threshold = 0.1` — half the default decay rate, half the
/// default prune floor. The rationale was that research
/// conversations reference long-lived material (months/years of
/// context) and benefit from slower decay. When those skills were
/// retired in favour of intent-keyed policy, their values were NOT
/// promoted to defaults because (a) every conversation would slow
/// down its decay 2×, including ephemeral chitchat, and (b) the
/// values were one author's tuning, not bench-validated.
///
/// **Future work**: if telemetry shows users losing relevant
/// long-lived context too fast, consider either lowering the
/// global defaults toward 0.05/0.1 or letting mode TOMLs override
/// (inner-work in particular is a long-lived-context surface that
/// might benefit). The Skill struct still carries
/// `memory_rules.confidence_decay_per_month` and `prune_threshold`
/// fields for that case.
pub const DEFAULT_DECAY_RATE: f64 = 0.10;
/// Confidence floor below which a memory is dropped during prune.
pub const DEFAULT_PRUNE_THRESHOLD: f64 = 0.2;

/// Inventory of entity names that mark a memory as relationally /
/// strategically relevant. Memories whose `content` mentions any
/// inventory name decay at half the configured rate per
/// requirements §5 (relationship-weighted decay).
///
/// Names are stored lowercased + trimmed; matching is whole-word
/// case-insensitive (substring `Sarah` does NOT match `Sarahkov`).
/// The set is rebuilt from the personal + conversational atlas's
/// `atoms.json` files at the end of each enrichment cycle.
///
/// Relocated to `sovereign-contracts::types` (so the `LandscapeDigestProvider`
/// contract trait can name it); re-exported here at the historical
/// `sovereign_core::memory::EntityInventory` path.
pub use sovereign_contracts::types::EntityInventory;

/// Calculate decayed confidence for a memory based on time since last use.
/// `decay_rate` is the fraction lost per month (default 0.10 = 10%).
///
/// Convenience wrapper — no entity inventory, full decay applied.
pub fn apply_confidence_decay(memory: &Memory, now: i64) -> f64 {
    apply_confidence_decay_with_rate_and_inventory(memory, now, DEFAULT_DECAY_RATE, None)
}

/// Calculate decayed confidence with a custom decay rate.
///
/// Convenience wrapper — no entity inventory, full rate applied.
/// Use [`apply_confidence_decay_with_rate_and_inventory`] when an
/// entity inventory is available so relationship-weighted decay
/// kicks in.
pub fn apply_confidence_decay_with_rate(memory: &Memory, now: i64, decay_rate: f64) -> f64 {
    apply_confidence_decay_with_rate_and_inventory(memory, now, decay_rate, None)
}

/// Full-fat decay: rate halved when the memory mentions any name in
/// the inventory.
///
/// The fixed-half rule (not a separately configurable parameter) is
/// per requirements §5.2 — "a fixed ratio, not a configurable
/// parameter." A skill that overrides `confidence_decay_per_month`
/// to 15% sees entity-linked memories decay at 7.5%.
///
/// `inventory = None` short-circuits to the unweighted formula —
/// callers without an inventory loaded yet (first run, enrichment
/// disabled) keep the default 10%/month behaviour.
pub fn apply_confidence_decay_with_rate_and_inventory(
    memory: &Memory,
    now: i64,
    decay_rate: f64,
    inventory: Option<&EntityInventory>,
) -> f64 {
    let effective_rate = match inventory {
        Some(inv) if memory_mentions_any_entity(&memory.content, inv) => decay_rate / 2.0,
        _ => decay_rate,
    };
    let months_elapsed = (now - memory.last_used) as f64 / (30.0 * 86400.0);
    let retention = 1.0 - effective_rate.clamp(0.0, 1.0);
    memory.confidence * retention.powf(months_elapsed)
}

/// Whole-word case-insensitive substring check. `entities` is a set
/// of lowercased names; `content` is split on non-alphanumeric and
/// each token is compared.
///
/// Multi-word names (e.g. "Sarah Chen", "API migration") are
/// detected by joining adjacent tokens up to the longest matching
/// run — we walk the memory's tokens and try each prefix slice
/// against the inventory. Linear in `tokens.len() *
/// max_inventory_words`; entity names are typically 1–3 words and
/// memories are short enough that this is well below the noise
/// floor.
fn memory_mentions_any_entity(content: &str, entities: &EntityInventory) -> bool {
    if entities.is_empty() {
        return false;
    }
    let tokens: Vec<String> = content
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return false;
    }
    // Longest-match-wins: try 4-word, 3-word, 2-word, 1-word
    // windows. Most personal entities are 1–2 words; the cap at 4
    // covers full-name "First Middle Last Suffix" cases.
    const MAX_WINDOW: usize = 4;
    for start in 0..tokens.len() {
        let max_w = MAX_WINDOW.min(tokens.len() - start);
        for w in (1..=max_w).rev() {
            let candidate = tokens[start..start + w].join(" ");
            if entities.contains(&candidate) {
                return true;
            }
        }
    }
    false
}

/// Build an [`EntityInventory`] from a slice of raw entity names.
/// Names are lowercased + trimmed; empty strings are dropped.
/// Convenience for callers that have a `Vec<String>` from
/// `atoms.json` and want to feed it into the decay path.
pub fn entity_inventory_from_names<I, S>(names: I) -> EntityInventory
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .filter_map(|n| {
            let trimmed = n.as_ref().trim().to_lowercase();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect()
}

/// Prune memories with decayed confidence below threshold.
/// Uses default decay rate (10%/month) and prune threshold (0.2).
/// No entity inventory — decay is uniform.
pub async fn prune_decayed_memories(store: &dyn StateStore, now_ts: i64) -> Result<usize> {
    prune_decayed_memories_with_config(
        store,
        now_ts,
        DEFAULT_DECAY_RATE,
        DEFAULT_PRUNE_THRESHOLD,
        None,
    )
    .await
}

/// Prune memories with configurable decay rate, threshold, and an
/// optional entity inventory for relationship-weighted decay.
pub async fn prune_decayed_memories_with_config(
    store: &dyn StateStore,
    now_ts: i64,
    decay_rate: f64,
    prune_threshold: f64,
    inventory: Option<&EntityInventory>,
) -> Result<usize> {
    let all = store.get_all_memories().await?;
    let mut pruned = 0;

    for memory in &all {
        let decayed =
            apply_confidence_decay_with_rate_and_inventory(memory, now_ts, decay_rate, inventory);
        if decayed < prune_threshold {
            store.delete_memory(&memory.id).await?;
            pruned += 1;
        } else if (decayed - memory.confidence).abs() > 0.01 {
            store.update_memory_confidence(&memory.id, decayed).await?;
        }
    }

    Ok(pruned)
}

// ─── Save with Contradiction Check ────────────────────────────

/// Compute the T1 content embedding for a memory about to be saved,
/// when the provider can identify its embed model and the row doesn't
/// already carry one. Best-effort: an embed failure leaves the fields
/// `None` and recall lazy-backfills on first use. Uses the
/// document-side `embed_batch` — the SAME call recall uses on memory
/// contents — so stored and recall-computed vectors rank identically.
/// (NOT `embed_query`: instruction-aware embedders like
/// Qwen3-Embedding prefix queries and documents differently.)
pub async fn attach_content_embedding(inference: &dyn InferenceProvider, memory: &mut Memory) {
    if memory.embedding.is_some() {
        return;
    }
    let model = inference.embed_model_id();
    if model == "unknown" {
        return;
    }
    match inference
        .embed_batch(std::slice::from_ref(&memory.content))
        .await
    {
        Ok(mut embs) if embs.len() == 1 && !embs[0].is_empty() => {
            memory.embedding = Some(embs.remove(0));
            memory.embedding_model = Some(model);
        }
        _ => {
            tracing::debug!(
                id = %memory.id,
                "memory: write-path embed failed — recall will lazy-backfill"
            );
        }
    }
}

/// Save a new memory, first checking for duplicates and contradictions.
pub async fn save_with_contradiction_check(
    inference: &dyn InferenceProvider,
    store: &dyn StateStore,
    new_memory: Memory,
) -> Result<()> {
    let existing = store.get_all_memories().await?;

    // Check for exact duplicate content.
    let new_lower = new_memory.content.trim().to_lowercase();
    if existing
        .iter()
        .any(|m| m.content.trim().to_lowercase() == new_lower)
    {
        return Ok(());
    }

    // Detect and delete contradictions.
    let contradicted_ids = detect_contradictions(inference, &new_memory, &existing).await?;
    for id in &contradicted_ids {
        store.delete_memory(id).await?;
    }

    // T1 compute-on-write — AFTER the duplicate gate so a dropped
    // duplicate never costs an embed, and at the single funnel every
    // extracted memory passes through.
    let mut new_memory = new_memory;
    attach_content_embedding(inference, &mut new_memory).await;

    store.save_memory(&new_memory).await
}

// ─── Tool-decision memory (Tool-Mastery framework, Layer 3) ────

/// Closed set of outcomes recorded when an agent's tool invocation
/// resolves. Serialised via Serde's `kebab-case` rename so the
/// on-disk JSON reads as `"useful"` / `"stale"` / `"wrong-tool"` /
/// `"no-results"` — the same labels the dossier renders to the
/// model. Closed-set discipline per ARCH §2.1 — no stringly-typed
/// outcome elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolDecisionOutcome {
    /// Tool returned evidence the model used in its final answer.
    Useful,
    /// Tool returned evidence whose recency or coverage didn't fit
    /// the question (e.g. corpus snapshot predates the asked-about
    /// event). Drives the gap-check + INFORMATION REQUEST surface.
    Stale,
    /// Tool returned no usable evidence and the model picked the
    /// wrong tool for the question shape. The dossier surfaces this
    /// so the next turn's narrowed catalog can read past the
    /// previous misfire.
    WrongTool,
    /// Tool returned an empty result set entirely. Distinct from
    /// `Stale` — there's nothing in the index, not "the index is
    /// behind the world."
    NoResults,
}

impl ToolDecisionOutcome {
    /// Canonical wire-form string. Stable across versions because the
    /// dossier and any FTS lookups grep against these literal labels.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Useful => "useful",
            Self::Stale => "stale",
            Self::WrongTool => "wrong-tool",
            Self::NoResults => "no-results",
        }
    }
}

/// Structured payload stored on every `tool_decision` note. The
/// Layer 2 dossier deserialises a tail of these to render the
/// "outcome history this conversation" section. `conversation_id`
/// is optional so we can record decisions made outside a
/// conversation (e.g. cron-driven enrichment runs); the dossier
/// filters on it when present.
///
/// Tier 1 of the tool-framework expansion (2026) adds three
/// fields that turn the outcome history from a status log into
/// addressable memory: `summary` (one-line "what came back"),
/// `evidence_ids` (per-call ev-Tn-NNNN handles the model may
/// cite cross-turn), and `turn_index` (lets the dossier render
/// "[ev-T2-0001]" references that uniquely identify the source
/// turn). All three default to empty/None so pre-Tier-1 notes
/// continue to deserialise cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDecisionPayload {
    pub tool_id: String,
    pub outcome: ToolDecisionOutcome,
    pub reasoning: String,
    pub applied_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// One-line summary of what came back (Tier 1). For
    /// knowledge_lookup, the top-1 evidence title; for code-intel
    /// tools, the first symbol/file. Renders in the dossier as
    /// `→ outcome — "summary"`. `None` for pre-Tier-1 payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Per-call ev-Tn-NNNN handles returned by this tool invocation
    /// (Tier 1). The dossier renders them so a later turn can cite
    /// `[ev-T2-0001]` and the runtime can dereference without
    /// re-calling the tool. Empty when the tool doesn't return
    /// citation-shaped evidence (e.g. shell, file).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
    /// Zero-based turn index this outcome was recorded against
    /// (Tier 1). Lets the dossier renderer build T-prefixed
    /// handles and lets the cross-turn citation validator know
    /// which prior turn an `ev-Tn-NNNN` reference points at.
    /// Defaults to 0 for pre-Tier-1 payloads — the dossier
    /// renders those without T prefixes for back-compat.
    #[serde(default)]
    pub turn_index: usize,
}

/// Optional extras for `write_tool_decision` / `record_tool_outcome`.
/// Bundled in a single struct so the named-args API stays readable
/// while still admitting the Tier-1 cross-turn fields. Use
/// `ToolDecisionExtras::none()` from sites that don't have the
/// data — the dossier renders a degraded but well-formed entry.
#[derive(Debug, Clone, Default)]
pub struct ToolDecisionExtras {
    pub summary: Option<String>,
    pub evidence_ids: Vec<String>,
    pub turn_index: usize,
}

impl ToolDecisionExtras {
    /// Empty extras — degraded-but-valid for call sites that
    /// don't have summary/evidence/turn data (e.g. tests, legacy
    /// non-knowledge_lookup tools).
    pub fn none() -> Self {
        Self::default()
    }
}

/// Persist a tool-decision outcome into the NoteStore. Returns the
/// new note's id. `content` is a one-line human-readable summary so
/// `sovereign tools call notes --kinds=tool_decision` is readable
/// without parsing JSON; structured fields ride in `payload_json`
/// so the dossier reader doesn't have to re-parse free text.
pub async fn write_tool_decision(
    notes: &NoteStore,
    session_id: &str,
    conversation_id: Option<&str>,
    tool_id: &str,
    outcome: ToolDecisionOutcome,
    reasoning: &str,
    extras: ToolDecisionExtras,
) -> Result<String> {
    let payload = ToolDecisionPayload {
        tool_id: tool_id.to_string(),
        outcome,
        reasoning: reasoning.to_string(),
        applied_at_unix: now(),
        conversation_id: conversation_id.map(str::to_string),
        summary: extras.summary,
        evidence_ids: extras.evidence_ids,
        turn_index: extras.turn_index,
    };
    let payload_json = serde_json::to_string(&payload)?;
    let content = format!("{tool_id} → {} — {reasoning}", outcome.as_str());
    notes
        .write_note_full(
            "tool_decision",
            &content,
            vec![tool_id.to_string()],
            vec![],
            session_id,
            // SESSION, not Global, on purpose. Tool-decision rows are per-turn
            // operational telemetry read back only within a conversation (the
            // dossier). `notes_delta_since` gossips ONLY `scope = 'global'`, so
            // Session keeps this high-volume log LOCAL — it never floods the
            // mesh's durable channel. The dossier read (`read_recent_tool_decisions`
            // → `read_notes`, scope-agnostic default filter) is unaffected. Do
            // NOT change back to Global: it re-creates the telemetry firehose
            // that `notes rationalize` exists to clean up.
            NoteScope::Session,
            None,
            None,
            NoteSource::Agent,
            None,
            Some(&payload_json),
        )
        .await
        .map_err(|e| Error::Storage(e.to_string()))
}

/// Read recent tool-decision payloads. When `conversation_id` is
/// `Some`, returns only decisions tagged with that conversation;
/// `None` returns the global tail. Capped at `limit` items
/// post-filter. Over-fetches by 4× when filtering by conversation
/// so a sparse conversation still has a chance of returning
/// `limit` matches without paging.
pub async fn read_recent_tool_decisions(
    notes: &NoteStore,
    conversation_id: Option<&str>,
    limit: usize,
) -> Result<Vec<ToolDecisionPayload>> {
    let fetch_cap = if conversation_id.is_some() {
        limit.saturating_mul(4).max(limit).min(100)
    } else {
        limit.min(100)
    };
    let rows = notes
        .read_notes(
            None,
            &[],
            &[],
            &["tool_decision".to_string()],
            fetch_cap,
            false,
        )
        .await
        .map_err(|e| Error::Storage(e.to_string()))?;

    let mut decisions: Vec<ToolDecisionPayload> = rows
        .into_iter()
        .filter_map(|row| {
            row.payload_json
                .as_deref()
                .and_then(|p| serde_json::from_str::<ToolDecisionPayload>(p).ok())
        })
        .collect();

    if let Some(cid) = conversation_id {
        decisions.retain(|d| d.conversation_id.as_deref() == Some(cid));
    }

    decisions.truncate(limit);
    Ok(decisions)
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_with_content(content: &str) -> Memory {
        Memory {
            id: "1".to_string(),
            content: content.to_string(),
            source: "test".to_string(),
            confidence: 1.0,
            created_at: 0,
            last_used: 0,
            version: 0,
            deleted_at: None,
            source_conversation_id: None,
            source_skill_id: None,
            ..Default::default()
        }
    }

    #[test]
    fn confidence_decay_one_month() {
        let mem = mem_with_content("test");
        let one_month = 30 * 86400;
        let decayed = apply_confidence_decay(&mem, one_month);
        assert!((decayed - 0.9).abs() < 0.001);
    }

    // ── Relationship-weighted decay ───────────────────────────────

    #[test]
    fn entity_linked_memory_decays_at_half_rate() {
        // One month at default 10%/month: unweighted lands at 0.90,
        // entity-linked lands at 0.95.
        let mem = mem_with_content("Discussed Q3 strategy with Sarah Chen at the offsite.");
        let inventory = entity_inventory_from_names(["Sarah Chen", "Mike Torres"]);
        let one_month = 30 * 86400;

        let weighted =
            apply_confidence_decay_with_rate_and_inventory(&mem, one_month, 0.10, Some(&inventory));
        let unweighted =
            apply_confidence_decay_with_rate_and_inventory(&mem, one_month, 0.10, None);

        assert!((weighted - 0.95).abs() < 0.001, "weighted={weighted}");
        assert!((unweighted - 0.90).abs() < 0.001, "unweighted={unweighted}");
    }

    #[test]
    fn unmatched_memory_decays_at_full_rate_even_with_inventory() {
        let mem = mem_with_content("Just thinking about software architecture.");
        let inventory = entity_inventory_from_names(["Sarah Chen", "API migration"]);
        let one_month = 30 * 86400;
        let decayed =
            apply_confidence_decay_with_rate_and_inventory(&mem, one_month, 0.10, Some(&inventory));
        // No match → full decay → 0.90.
        assert!((decayed - 0.90).abs() < 0.001);
    }

    #[test]
    fn whole_word_match_does_not_match_substring_within_a_word() {
        // "Sarah" must not match "Sarahkov" (a different person).
        let mem = mem_with_content("Read about Sarahkov, the historian.");
        let inventory = entity_inventory_from_names(["Sarah"]);
        assert!(!memory_mentions_any_entity(&mem.content, &inventory));
    }

    #[test]
    fn match_is_case_insensitive() {
        let mem = mem_with_content("Brief chat with sarah about pricing.");
        let inventory = entity_inventory_from_names(["Sarah"]);
        assert!(memory_mentions_any_entity(&mem.content, &inventory));
    }

    #[test]
    fn multi_word_entity_name_matches() {
        let mem = mem_with_content("The API migration is on track for end of Q2.");
        let inventory = entity_inventory_from_names(["API migration"]);
        assert!(memory_mentions_any_entity(&mem.content, &inventory));
    }

    #[test]
    fn empty_inventory_short_circuits_to_full_decay() {
        let mem = mem_with_content("Sarah Chen mentioned the Q3 push.");
        let inventory: EntityInventory = EntityInventory::new();
        let one_month = 30 * 86400;
        let decayed =
            apply_confidence_decay_with_rate_and_inventory(&mem, one_month, 0.10, Some(&inventory));
        assert!((decayed - 0.90).abs() < 0.001);
    }

    #[test]
    fn skill_overridden_rate_is_halved_when_entity_matches() {
        // A skill with confidence_decay_per_month = 0.15 should see
        // entity-linked memories at 7.5% per month.
        let mem = mem_with_content("Sarah Chen flagged a budget concern.");
        let inventory = entity_inventory_from_names(["Sarah Chen"]);
        let one_month = 30 * 86400;
        let weighted =
            apply_confidence_decay_with_rate_and_inventory(&mem, one_month, 0.15, Some(&inventory));
        // Effective rate 0.075 → retention 0.925 → after 1 month 0.925.
        assert!((weighted - 0.925).abs() < 0.001, "weighted={weighted}");
    }

    #[test]
    fn confidence_decay_six_months() {
        let mem = Memory {
            id: "1".to_string(),
            content: "test".to_string(),
            source: "test".to_string(),
            confidence: 1.0,
            created_at: 0,
            last_used: 0,
            version: 0,
            deleted_at: None,
            source_conversation_id: None,
            source_skill_id: None,
            ..Default::default()
        };
        let six_months = 6 * 30 * 86400;
        let decayed = apply_confidence_decay(&mem, six_months);
        assert!((decayed - 0.531).abs() < 0.01);
    }

    #[test]
    fn confidence_decay_24_months_below_threshold() {
        let mem = Memory {
            id: "1".to_string(),
            content: "test".to_string(),
            source: "test".to_string(),
            confidence: 1.0,
            created_at: 0,
            last_used: 0,
            version: 0,
            deleted_at: None,
            source_conversation_id: None,
            source_skill_id: None,
            ..Default::default()
        };
        let two_years = 24 * 30 * 86400;
        let decayed = apply_confidence_decay(&mem, two_years);
        assert!(decayed < 0.2, "expected < 0.2, got {decayed}");
    }

    #[test]
    fn format_memories_empty_returns_none() {
        assert!(format_memories_for_prompt(&[], SkillRegister::Factual).is_none());
        assert!(format_memories_for_prompt(&[], SkillRegister::Relational).is_none());
    }

    #[test]
    fn factual_register_returns_pre_existing_flat_bullet_list() {
        let memories = vec![
            Memory {
                id: "1".to_string(),
                content: "User prefers Rust".to_string(),
                source: "test".to_string(),
                confidence: 1.0,
                created_at: 0,
                last_used: 0,
                version: 0,
                deleted_at: None,
                source_conversation_id: None,
                source_skill_id: None,
                ..Default::default()
            },
            Memory {
                id: "2".to_string(),
                content: "User is a backend engineer".to_string(),
                source: "test".to_string(),
                confidence: 1.0,
                created_at: 0,
                last_used: 0,
                version: 0,
                deleted_at: None,
                source_conversation_id: None,
                source_skill_id: None,
                ..Default::default()
            },
        ];
        let result = format_memories_for_prompt(&memories, SkillRegister::Factual).unwrap();
        assert!(result.contains("Known facts about the user:"));
        assert!(result.contains("- User prefers Rust"));
        assert!(result.contains("- User is a backend engineer"));
        // Banded headings must NOT appear in factual format.
        assert!(!result.contains("What you've told me directly"));
        assert!(!result.contains("What I've inferred"));
    }

    fn mem(
        id: &str,
        content: &str,
        confidence: f64,
        created_at: i64,
        source_conv: Option<&str>,
    ) -> Memory {
        Memory {
            id: id.to_string(),
            content: content.to_string(),
            source: "test".to_string(),
            confidence,
            created_at,
            last_used: created_at,
            version: 0,
            deleted_at: None,
            source_conversation_id: source_conv.map(|s| s.to_string()),
            source_skill_id: None,
            ..Default::default()
        }
    }

    #[test]
    fn relational_register_splits_into_three_confidence_bands() {
        // 2026-03-12 00:00:00 UTC = 1773273600
        let directly = mem(
            "d",
            "I want to leave the job",
            0.92,
            1_773_273_600,
            Some("c-mar"),
        );
        // 2026-04-08 00:00:00 UTC = 1775606400
        let inferred = mem(
            "i",
            "Work and meaning are linked for you",
            0.62,
            1_775_606_400,
            Some("c-apr"),
        );
        let tentative = mem("t", "You may be avoiding conflict with Mark", 0.35, 0, None);

        let result =
            format_memories_for_prompt(&[directly, inferred, tentative], SkillRegister::Relational)
                .unwrap();

        assert!(result.contains("What you've told me directly:"));
        assert!(result.contains("What I've inferred from earlier conversations:"));
        assert!(result.contains("Tentative — flag these as guesses"));
        assert!(result.contains("[2026-03-12]"));
        assert!(result.contains("[2026-04-08]"));
        // Per-entry confidence annotations must NOT render — the bands
        // carry the signal, and the witness echoed "(confidence 0.85)"
        // verbatim into a user-facing reply (hand-read, 2026-07-09).
        assert!(!result.contains("(confidence"));
        // The flat-list factual heading must NOT appear in relational format.
        assert!(!result.contains("Known facts about the user:"));
    }

    #[test]
    fn relational_register_omits_date_when_no_source_conversation() {
        let undated = mem("u", "User prefers Rust", 0.95, 1_773_273_600, None);
        let result = format_memories_for_prompt(&[undated], SkillRegister::Relational).unwrap();
        // Date should not appear because source_conversation_id is None,
        // even though created_at would resolve to a valid date.
        assert!(!result.contains("[2026-03-12]"));
        assert!(result.contains("- User prefers Rust"));
    }

    #[test]
    fn relational_register_skips_empty_bands() {
        let only_directly = mem("d", "I told you X", 0.95, 1_773_273_600, Some("c"));
        let result =
            format_memories_for_prompt(&[only_directly], SkillRegister::Relational).unwrap();
        // Only the band that has content should render.
        assert!(result.contains("What you've told me directly:"));
        assert!(!result.contains("What I've inferred"));
        assert!(!result.contains("Tentative —"));
    }

    #[test]
    fn relational_register_band_thresholds_are_exact() {
        // 0.85 — exactly on the directly threshold (inclusive).
        let m_85 = mem("a", "boundary directly", 0.85, 0, None);
        // 0.5 — exactly on the inferred threshold (inclusive).
        let m_50 = mem("b", "boundary inferred", 0.50, 0, None);
        // 0.4999... — just below inferred threshold.
        let m_49 = mem("c", "tentative", 0.49, 0, None);

        let result =
            format_memories_for_prompt(&[m_85, m_50, m_49], SkillRegister::Relational).unwrap();
        // The directly band lists "boundary directly".
        let directly_idx = result.find("What you've told me directly:").unwrap();
        let inferred_idx = result.find("What I've inferred").unwrap();
        let tentative_idx = result.find("Tentative —").unwrap();
        let directly_block = &result[directly_idx..inferred_idx];
        let inferred_block = &result[inferred_idx..tentative_idx];
        let tentative_block = &result[tentative_idx..];

        assert!(directly_block.contains("boundary directly"));
        assert!(inferred_block.contains("boundary inferred"));
        assert!(tentative_block.contains("tentative"));
    }

    #[test]
    fn parse_working_memory_valid_json() {
        let text = r#"{"goal": "build a web app", "facts": ["User knows Rust", "Using Axum"]}"#;
        let wm = parse_working_memory(text, None).unwrap();
        assert_eq!(wm.current_goal.as_deref(), Some("build a web app"));
        assert_eq!(wm.facts.len(), 2);
        assert_eq!(wm.facts[0], "User knows Rust");
    }

    #[test]
    fn parse_working_memory_json_fence() {
        let text = "Here is the result:\n```json\n{\"goal\": \"test\", \"facts\": []}\n```";
        let wm = parse_working_memory(text, None).unwrap();
        assert_eq!(wm.current_goal.as_deref(), Some("test"));
    }

    #[test]
    fn parse_working_memory_fallback() {
        let text = "I don't understand the request";
        let prev = WorkingMemory {
            current_goal: Some("previous goal".to_string()),
            facts: vec!["old fact".to_string()],
            active_documents: vec![],
        };
        let wm = parse_working_memory(text, Some(&prev)).unwrap();
        assert_eq!(wm.current_goal.as_deref(), Some("previous goal"));
    }

    #[test]
    fn parse_extracted_memories_valid() {
        let text = r#"["User prefers Rust", "User lives in Portland"]"#;
        let mems = parse_extracted_memories(text).unwrap();
        assert_eq!(mems.len(), 2);
        assert_eq!(mems[0].content, "User prefers Rust");
        assert_eq!(mems[0].confidence, 1.0);
        assert_eq!(mems[0].source, "conversation_extraction");
    }

    #[test]
    fn parse_extracted_memories_empty() {
        let text = "[]";
        let mems = parse_extracted_memories(text).unwrap();
        assert!(mems.is_empty());
    }

    #[test]
    fn parse_extracted_memories_garbage() {
        let text = "I found some facts about the user";
        let mems = parse_extracted_memories(text).unwrap();
        assert!(mems.is_empty());
    }

    // ─── R3: Temporal-tension detection ───────────────────────

    use crate::error::Error;
    use crate::traits::InferenceProvider;
    use crate::types::{CompletionResponse, Depth, ProviderCapabilities, Speed};
    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Minimal mock inference provider for the tension-detector
    /// tests. Returns whatever was preset; records the prompt the
    /// caller sent so the tests can pin the prompt shape.
    struct ScriptedInference {
        response_text: String,
        last_prompt: Mutex<Option<String>>,
    }

    impl ScriptedInference {
        fn new(response_text: &str) -> Self {
            Self {
                response_text: response_text.to_string(),
                last_prompt: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl InferenceProvider for ScriptedInference {
        async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
            *self.last_prompt.lock().unwrap() = Some(request.prompt.clone());
            Ok(CompletionResponse {
                text: self.response_text.clone(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "scripted".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented(
                "ScriptedInference: streaming unused".into(),
            ))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    fn relational_mem(
        id: &str,
        content: &str,
        confidence: f64,
        created_at: i64,
        source_conv: Option<&str>,
    ) -> Memory {
        Memory {
            id: id.to_string(),
            content: content.to_string(),
            source: "test".into(),
            confidence,
            created_at,
            last_used: created_at,
            version: 0,
            deleted_at: None,
            source_conversation_id: source_conv.map(|s| s.to_string()),
            source_skill_id: None,
            ..Default::default()
        }
    }

    /// Embed provider that records every text handed to `embed_batch`
    /// and treats empty/whitespace content as UNEMBEDDABLE (returns an
    /// empty vector for it) — the exact degenerate input that used to
    /// re-embed on every recall turn forever. Reports a KNOWN model id so
    /// backfills persist.
    struct CountingEmbed {
        model: String,
        batched: Mutex<Vec<String>>,
    }
    impl CountingEmbed {
        fn new() -> Self {
            Self {
                model: "test-embed-v1".into(),
                batched: Mutex::new(Vec::new()),
            }
        }
        fn embed_one(text: &str) -> Vec<f32> {
            if text.trim().is_empty() {
                vec![]
            } else {
                vec![1.0, 0.0, 0.0]
            }
        }
    }
    #[async_trait]
    impl InferenceProvider for CountingEmbed {
        async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
            Err(Error::NotImplemented("CountingEmbed: complete unused".into()))
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("CountingEmbed: stream unused".into()))
        }
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            Ok(Self::embed_one(text))
        }
        async fn embed_query(&self, _q: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0, 0.0, 0.0])
        }
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.batched.lock().unwrap().extend(texts.iter().cloned());
            Ok(texts.iter().map(|t| Self::embed_one(t)).collect())
        }
        fn embed_model_id(&self) -> String {
            self.model.clone()
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    // A memory whose content is unembeddable (empty vector) must be
    // stamped "attempted under the current model" on its first recall and
    // then NEVER re-embedded — otherwise it re-embeds on every single
    // recall turn forever (a done-set derived from produced output that
    // never converges).
    #[tokio::test]
    async fn unembeddable_memory_is_marked_and_not_re_embedded_every_recall() {
        use crate::traits::MemoryStore;
        use sovereign_store::sqlite::SqliteStateStore;
        let store = SqliteStateStore::open_in_memory().unwrap();

        let real = relational_mem("real", "the user prefers dark mode", 1.0, now(), None);
        let empty = relational_mem("empty", "   ", 1.0, now(), None);
        store.save_memory(&real).await.unwrap();
        store.save_memory(&empty).await.unwrap();

        let infer = CountingEmbed::new();
        let scope = MemoryScope::General;

        // First recall: neither memory has a stored embedding, so both are
        // embedded exactly once.
        let _ = recall_relevant_memories_embed(&infer, &store, &scope, "preferences", 10)
            .await
            .unwrap();
        {
            let batched = infer.batched.lock().unwrap();
            assert!(batched.iter().any(|t| t == "the user prefers dark mode"));
            assert!(batched.iter().any(|t| t.trim().is_empty()));
        }

        // The unembeddable memory is now stamped with the current model
        // even though its vector is empty — the marker that stops the loop.
        let after = store.get_all_memories_for_scope(&scope).await.unwrap();
        let stored_empty = after.iter().find(|m| m.id == "empty").unwrap();
        assert_eq!(stored_empty.embedding_model.as_deref(), Some("test-embed-v1"));

        // Second recall: the real memory has a usable vector, the empty one
        // is recognised as attempted-but-unembeddable — so NEITHER is
        // re-embedded. The batch record must not grow.
        let count_after_first = infer.batched.lock().unwrap().len();
        let _ = recall_relevant_memories_embed(&infer, &store, &scope, "preferences", 10)
            .await
            .unwrap();
        let count_after_second = infer.batched.lock().unwrap().len();
        assert_eq!(
            count_after_second, count_after_first,
            "no memory should be re-embedded on the second recall (convergence)"
        );
    }

    #[tokio::test]
    async fn detect_tensions_returns_empty_when_no_candidate_memories() {
        let infer = ScriptedInference::new("[]");
        let out = detect_temporal_tensions(&infer, "anything", &[])
            .await
            .unwrap();
        assert!(out.is_empty());
        // The provider must NOT have been called when there are no
        // candidates (zero-cost guarantee for casual chat).
        assert!(infer.last_prompt.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn detect_tensions_skips_low_confidence_memories() {
        // 0.6 < RELATIONAL_DIRECT_THRESHOLD (0.85) — should be filtered.
        let infer = ScriptedInference::new("[]");
        let mems = vec![relational_mem("a", "guess", 0.6, 0, None)];
        let out = detect_temporal_tensions(&infer, "anything", &mems)
            .await
            .unwrap();
        assert!(out.is_empty());
        // No directly-stated candidates → no inference call.
        assert!(infer.last_prompt.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn detect_tensions_returns_only_tension_classifications() {
        // Three candidates; classifier marks only the middle one as tension.
        let infer = ScriptedInference::new(
            r#"[
                {"index": 0, "relation": "consistent"},
                {"index": 1, "relation": "tension"},
                {"index": 2, "relation": "neutral"}
            ]"#,
        );
        let mems = vec![
            relational_mem("m0", "I love my job", 0.95, 1_773_273_600, Some("c1")),
            relational_mem(
                "m1",
                "I want to leave the job",
                0.92,
                1_773_273_600,
                Some("c2"),
            ),
            relational_mem("m2", "I cook on Sundays", 0.90, 0, None),
        ];
        let out = detect_temporal_tensions(&infer, "this is a place I want to grow", &mems)
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].memory_id, "m1");
        assert_eq!(out[0].prior_content, "I want to leave the job");
        assert!(out[0].prior_has_source_conversation);
        // Excerpt is the user message, possibly truncated; here it's short.
        assert_eq!(out[0].current_excerpt, "this is a place I want to grow");
    }

    #[tokio::test]
    async fn detect_tensions_soft_fails_on_garbage_response() {
        // Model output that doesn't parse as JSON — must NOT error,
        // just return empty so the turn proceeds.
        let infer = ScriptedInference::new("I'm not sure what you mean.");
        let mems = vec![relational_mem("m", "I told you X", 0.95, 0, Some("c"))];
        let out = detect_temporal_tensions(&infer, "current", &mems)
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn detect_tensions_handles_fenced_response() {
        let infer = ScriptedInference::new(
            "Here's the classification:\n```json\n[{\"index\": 0, \"relation\": \"tension\"}]\n```",
        );
        let mems = vec![relational_mem("m", "I told you X", 0.95, 0, Some("c"))];
        let out = detect_temporal_tensions(&infer, "current", &mems)
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].memory_id, "m");
    }

    #[tokio::test]
    async fn detect_tensions_truncates_long_messages() {
        let infer = ScriptedInference::new(r#"[{"index": 0, "relation": "tension"}]"#);
        let mems = vec![relational_mem("m", "prior", 0.95, 0, None)];
        let long_message = "x".repeat(500);
        let out = detect_temporal_tensions(&infer, &long_message, &mems)
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        // Excerpt cap is TENSION_EXCERPT_CHAR_CAP (240) + ellipsis.
        assert!(out[0].current_excerpt.chars().count() <= TENSION_EXCERPT_CHAR_CAP + 1);
        assert!(out[0].current_excerpt.ends_with('…'));
    }

    #[tokio::test]
    async fn detect_tensions_caps_at_max_candidates() {
        let infer = ScriptedInference::new(
            // Classifier asked to evaluate 5 (capped); we send 7.
            r#"[
                {"index": 0, "relation": "tension"},
                {"index": 1, "relation": "tension"},
                {"index": 2, "relation": "tension"},
                {"index": 3, "relation": "tension"},
                {"index": 4, "relation": "tension"}
            ]"#,
        );
        let mems: Vec<Memory> = (0..7)
            .map(|i| relational_mem(&format!("m{i}"), &format!("memory {i}"), 0.95, 0, None))
            .collect();
        let out = detect_temporal_tensions(&infer, "current", &mems)
            .await
            .unwrap();
        // At most MAX_TENSION_CANDIDATES (5), regardless of memories supplied.
        assert!(out.len() <= MAX_TENSION_CANDIDATES);
    }

    // ─── tool_decision memory ─────────────────────────────────

    async fn fresh_note_store() -> NoteStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.db");
        let store = NoteStore::open(&path).unwrap();
        // Leak the tempdir so it outlives the store handle (each test
        // builds its own; this isn't a daemon). Without this the
        // tempdir drops at scope-exit and the underlying SQLite file
        // is removed mid-test.
        std::mem::forget(dir);
        store
    }

    #[test]
    fn tool_decision_outcome_serde_round_trip_matches_kebab_labels() {
        for (variant, label) in [
            (ToolDecisionOutcome::Useful, "useful"),
            (ToolDecisionOutcome::Stale, "stale"),
            (ToolDecisionOutcome::WrongTool, "wrong-tool"),
            (ToolDecisionOutcome::NoResults, "no-results"),
        ] {
            let serialised = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialised, format!("\"{label}\""));
            let deserialised: ToolDecisionOutcome = serde_json::from_str(&serialised).unwrap();
            assert_eq!(deserialised, variant);
            assert_eq!(variant.as_str(), label);
        }
    }

    #[tokio::test]
    async fn write_then_read_tool_decision_round_trips_payload() {
        let store = fresh_note_store().await;

        let id = write_tool_decision(
            &store,
            "sess-mem-1",
            Some("conv-A"),
            "knowledge_lookup",
            ToolDecisionOutcome::NoResults,
            "corpus has no entry for M5 Mac Studio",
            ToolDecisionExtras::none(),
        )
        .await
        .unwrap();
        assert!(!id.is_empty());

        let recent = read_recent_tool_decisions(&store, Some("conv-A"), 10)
            .await
            .unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].tool_id, "knowledge_lookup");
        assert_eq!(recent[0].outcome, ToolDecisionOutcome::NoResults);
        assert_eq!(recent[0].reasoning, "corpus has no entry for M5 Mac Studio");
        assert_eq!(recent[0].conversation_id.as_deref(), Some("conv-A"));
    }

    #[tokio::test]
    async fn read_recent_filters_by_conversation_id() {
        let store = fresh_note_store().await;

        write_tool_decision(
            &store,
            "sess-mem-2",
            Some("conv-A"),
            "knowledge_lookup",
            ToolDecisionOutcome::Useful,
            "found it in the wiki corpus",
            ToolDecisionExtras::none(),
        )
        .await
        .unwrap();
        write_tool_decision(
            &store,
            "sess-mem-2",
            Some("conv-B"),
            "search",
            ToolDecisionOutcome::Stale,
            "results pre-dated the asked-about announcement",
            ToolDecisionExtras::none(),
        )
        .await
        .unwrap();
        write_tool_decision(
            &store,
            "sess-mem-2",
            None,
            "search",
            ToolDecisionOutcome::WrongTool,
            "should have used knowledge_lookup",
            ToolDecisionExtras::none(),
        )
        .await
        .unwrap();

        let a = read_recent_tool_decisions(&store, Some("conv-A"), 10)
            .await
            .unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].tool_id, "knowledge_lookup");

        let b = read_recent_tool_decisions(&store, Some("conv-B"), 10)
            .await
            .unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].outcome, ToolDecisionOutcome::Stale);

        let global = read_recent_tool_decisions(&store, None, 10).await.unwrap();
        assert_eq!(global.len(), 3, "no conversation filter returns the tail");
    }

    #[tokio::test]
    async fn read_recent_respects_limit() {
        let store = fresh_note_store().await;
        for i in 0..5 {
            write_tool_decision(
                &store,
                "sess-mem-3",
                Some("conv-X"),
                "knowledge_lookup",
                ToolDecisionOutcome::Useful,
                &format!("decision {i}"),
                ToolDecisionExtras::none(),
            )
            .await
            .unwrap();
        }
        let two = read_recent_tool_decisions(&store, Some("conv-X"), 2)
            .await
            .unwrap();
        assert_eq!(two.len(), 2);
    }

    #[tokio::test]
    async fn read_recent_returns_empty_for_unknown_conversation() {
        let store = fresh_note_store().await;
        write_tool_decision(
            &store,
            "sess-mem-4",
            Some("conv-real"),
            "search",
            ToolDecisionOutcome::Useful,
            "found in corpus",
            ToolDecisionExtras::none(),
        )
        .await
        .unwrap();
        let none = read_recent_tool_decisions(&store, Some("conv-missing"), 10)
            .await
            .unwrap();
        assert!(none.is_empty());
    }
}
