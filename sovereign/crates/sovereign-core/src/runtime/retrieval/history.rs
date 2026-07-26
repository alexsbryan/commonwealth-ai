// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation-history memory: dropped-history compaction
//! (turn-count + budget-pressure arms) and
//! retrieval-over-history.

use super::super::*;

/// The per-unit derived vectors that
/// [`Runtime::maybe_retrieve_relevant_history`] scores against the
/// current turn's query. Both are pure functions of the unit's
/// rendered body, so they are memoized across turns — see
/// [`Runtime::history_unit_memo`].
#[derive(Clone)]
pub(crate) struct HistoryUnitVectors {
    /// L2-normalized embedding of the unit body.
    pub(crate) embedding: Vec<f32>,
    /// Entities GLiNER extracted from the body. Empty when no
    /// extractor was wired at compute time — which is also the
    /// hybrid-scoring no-op, so a memo entry written pre-GLiNER stays
    /// correct for the pure-cosine path it was written under.
    pub(crate) entities: std::collections::HashSet<String>,
}

/// Ceiling on [`Runtime::history_unit_memo`] entries. One entry is an
/// embedding (~3-4 KB at typical dims) plus a small entity set, so
/// ~4096 caps the memo near 16 MB — far above any real per-process
/// working set of conversation pairs.
pub(crate) const HISTORY_UNIT_MEMO_CAP: usize = 4096;

/// Content key for a history unit. Hashing the BODY (not the turn
/// index) means a stale entry is impossible: if the rendered pair
/// changes for any reason — content edit, index-parity flip after an
/// odd-length turn — the key changes and we recompute.
fn history_unit_key(body: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    h.finish()
}

impl Runtime {
    /// Memo lookup for a batch of history-unit bodies: one slot per
    /// input, `Some` on hit. Returns each body's key alongside so the
    /// caller writes results back via
    /// [`Self::store_history_unit_vectors`] without re-hashing.
    fn history_unit_vectors(&self, bodies: &[String]) -> Vec<(u64, Option<HistoryUnitVectors>)> {
        let memo = self
            .history_unit_memo
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        bodies
            .iter()
            .map(|b| {
                let key = history_unit_key(b);
                (key, memo.get(&key).cloned())
            })
            .collect()
    }

    /// Record freshly computed vectors for one history unit. Over
    /// [`HISTORY_UNIT_MEMO_CAP`] we clear wholesale (same shape as
    /// `record_assembly`) — rolling eviction would need access
    /// ordering we don't track, and the cap is not a correctness knob.
    fn store_history_unit_vectors(&self, key: u64, vectors: HistoryUnitVectors) {
        let mut memo = self
            .history_unit_memo
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if memo.len() >= HISTORY_UNIT_MEMO_CAP && !memo.contains_key(&key) {
            tracing::debug!(
                cap = HISTORY_UNIT_MEMO_CAP,
                "runtime:history_retrieval.memo_cleared_at_cap"
            );
            memo.clear();
        }
        memo.insert(key, vectors);
    }

    /// Fold the dropped tail of the conversation into its persisted
    /// [`crate::conv_frame`] so the synthesis prompt keeps the entities,
    /// stated goals, and topical arc established outside the
    /// visible-history window.
    ///
    /// Activates only when `conversation.messages.len()` exceeds the
    /// visible-history window by at least
    /// `CONV_HISTORY_COMPACT_MIN_DROPPED` messages. The frame's prompt
    /// form is stored on `context.compacted_history` and read back by
    /// `format_conversation_history` at prompt-assembly time.
    ///
    /// # Cost shape (this is the point of the design)
    ///
    /// Every turn surfaces the frame; only turns past
    /// `CONV_COMPACT_FOLD_STRIDE` new dropped messages pay to update it,
    /// and an update's prompt carries the frame plus the new messages
    /// only — never the conversation. So the per-turn cost is flat in
    /// conversation length. Until 2026-07-26 it was not: the preamble
    /// lived in a `ConversationContext` field `build_context` reset to
    /// `None` every turn, so every turn re-summarised the ENTIRE dropped
    /// tail on the path the user waits on, and past ~168 dropped
    /// messages the unbounded prompt overran the slot.
    ///
    /// Soft-fail by design at every step: a store error, a parse
    /// failure, or an inference error leaves the previously stored frame
    /// in place and the synthesis path continues. Surfaced by
    /// `sovereign/bench/wikipedia_learn` 2026-05-17 marathon thread.
    pub(crate) async fn maybe_compact_dropped_history(
        &self,
        context: &mut ConversationContext,
        conversation_id: &str,
        // Optional because the compaction call fires earlier in the
        // streaming handler (line ~1355) than `self.sessions.begin`
        // (line ~1432), so the session_id isn't bound yet on the
        // critical path. Non-streaming and test callers don't have
        // a session at all. Emit the narration chip only when
        // Some; below that we still fire compaction + the
        // `runtime:compaction.budget_triggered` trace, just no chip.
        session_id: Option<&str>,
    ) {
        // v5 spike (2026-05-26): when retrieval-over-history is the
        // primary memory mechanism for old turns, the lossy-summary
        // compaction arm fights it (adds a re-summarised preamble
        // that competes with the retrieval block). Env-var off lets
        // bench A/B the two cleanly.
        if std::env::var("SOVEREIGN_COMPACTION_DISABLE")
            .ok()
            .as_deref()
            == Some("1")
        {
            tracing::debug!(conversation_id, "runtime:compaction.disabled_via_env");
            let _ = session_id;
            return;
        }
        // Load the running frame FIRST: it is both an input to the
        // pressure estimate (it rides every prompt) and the thing we
        // surface even on turns that decide not to fold.
        let stored = self
            .store
            .get_conversation_frame(conversation_id)
            .await
            .unwrap_or_else(|e| {
                tracing::debug!(error = %e, conversation_id,
                    "runtime:compaction.frame_read_failed — treating as cold");
                None
            });
        let frame = crate::conv_frame::parse(stored.as_deref());
        let covered = crate::conv_frame::covered_upto(&frame);
        let frame_prompt = frame.render_for_prompt();
        if !frame_prompt.trim().is_empty() {
            context.compacted_history = Some(frame_prompt.clone());
        }

        let total = context.conversation.messages.len();
        // Two-axis trigger (added 2026-05-25 in the
        // marathon-graceful pass):
        //   1. **Turn-count arm** (original): visible window has
        //      already overflowed, oldest messages are about to be
        //      dropped silently. This is the steady-state trigger on
        //      typical multi-turn chats.
        //   2. **Budget-pressure arm** (new): the conversation
        //      already exceeds `COMPACTION_PRESSURE_THRESHOLD * ctx`
        //      even with all turns visible. Catches the case where 6
        //      verbose turns on a tight slot would blow ctx before
        //      the turn-count arm fires.
        let turn_count_trigger = total > CONV_HISTORY_TURNS;
        let (budget_trigger, budget_pressure, budget_ctx) =
            self.estimate_compaction_pressure(context, &frame_prompt);
        if !turn_count_trigger && !budget_trigger {
            return;
        }

        // Pick the dropped window. Turn-count arm keeps its existing
        // shape (everything before `last 8`). Budget arm without the
        // turn-count arm drops just the oldest pair so the chat
        // shrinks one user/assistant pair at a time as pressure
        // climbs — leaves the recent context maximally intact.
        let dropped_end = if turn_count_trigger {
            total.saturating_sub(CONV_HISTORY_TURNS)
        } else {
            // Budget-only arm. Need ≥ 4 messages to drop a pair
            // without leaving the visible window degenerate.
            if total < 4 {
                return;
            }
            2
        };
        let dropped = &context.conversation.messages[..dropped_end];
        if dropped.len() < CONV_HISTORY_COMPACT_MIN_DROPPED {
            return;
        }

        if budget_trigger {
            tracing::debug!(
                turn_count_trigger,
                budget_trigger,
                budget_pressure,
                budget_ctx,
                dropped = dropped.len(),
                total,
                "runtime:compaction.budget_triggered"
            );
        }

        // Stride gate. The un-folded messages are the NEWEST dropped
        // ones, which the visible window only just released and which
        // retrieval-over-history can still surface verbatim — so waiting
        // costs little and saves two Housekeep calls in three.
        let new_count = dropped_end.saturating_sub(covered);
        if stored.is_some() && new_count < crate::runtime::CONV_COMPACT_FOLD_STRIDE {
            tracing::debug!(
                conversation_id,
                covered,
                dropped_end,
                new_count,
                stride = crate::runtime::CONV_COMPACT_FOLD_STRIDE,
                "runtime:compaction.reused_running_frame"
            );
            return;
        }

        // Bound the fold. Incremental folds are stride-sized; the cap
        // binds on a cold fold against an already-long conversation.
        // `covered.min(dropped_end)` guards the budget-only arm, which
        // can set a dropped edge BEHIND the watermark.
        let candidates = &context.conversation.messages[covered.min(dropped_end)..dropped_end];
        let (window, elided) =
            crate::context::fold_window(candidates, crate::runtime::CONV_COMPACT_MAX_FOLD_MSGS);

        tracing::debug!(
            conversation_id,
            cold = stored.is_none(),
            covered,
            dropped_end,
            fold_msgs = window.len(),
            elided,
            "runtime:compaction.folding"
        );

        match crate::conv_frame::fold(
            self.inference.as_ref(),
            stored.as_deref(),
            &window,
            elided,
            dropped_end,
        )
        .await
        {
            Ok(Some(rendered)) => {
                if let Err(e) = self
                    .store
                    .set_conversation_frame(conversation_id, &rendered)
                    .await
                {
                    // A lost write costs one cold fold next process, not
                    // this turn's memory — the frame is already in the
                    // context below.
                    tracing::warn!(error = %e, conversation_id,
                        "runtime:compaction.frame_write_failed");
                }
                let folded_frame = crate::conv_frame::parse(Some(&rendered));
                let prompt_form = folded_frame.render_for_prompt();
                if !prompt_form.trim().is_empty() {
                    context.compacted_history = Some(prompt_form);
                }
                // Glassbox the compaction so the user sees why their
                // chat surface changed shape. Gated below
                // `COMPACTION_CHIP_MIN_DROPPED = 3` — folding 2
                // messages would chip-spam on every long-chat turn.
                let folded_count = window.len();
                if folded_count >= crate::runtime::COMPACTION_CHIP_MIN_DROPPED {
                    if let Some(sid) = session_id {
                        self.routing_events
                            .emit_turn_narration(crate::types::TurnNarration {
                                session_id: sid.to_string(),
                                conversation_id: conversation_id.to_string(),
                                event: crate::types::NarrationEvent {
                                    phase: crate::types::NarrationPhase::ConversationFolded {
                                        turns: folded_count,
                                    },
                                    text: format!(
                                        "Folded {folded_count} earlier turns into my running notes on this conversation."
                                    ),
                                    elapsed_ms: 0,
                                },
                            })
                            .await;
                    }
                }
            }
            Ok(None) => {
                // The fold declined or failed to parse. `stored` is
                // already in `context.compacted_history` above, so the
                // conversation keeps the memory it had.
                tracing::debug!(
                    fold_msgs = window.len(),
                    "conv_frame: fold produced no update — keeping the stored frame"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    fold_msgs = window.len(),
                    "conv_frame: fold failed — keeping the stored frame"
                );
            }
        }
    }

    /// Estimate the conversation-history-side pressure on the slot's
    /// context window. Returns `(triggered, estimated_tokens,
    /// ctx_size)`. `triggered` is true iff the estimate crosses
    /// `COMPACTION_PRESSURE_THRESHOLD * ctx_size`. Returns
    /// `(false, 0, 0)` when the inference provider doesn't expose a
    /// concrete context window (remote-only forwarder) — the
    /// turn-count arm carries the trigger in that case.
    ///
    /// **NARROW SENSOR — KNOWN LIMITATION.** Walks only the
    /// components the runtime knows ABOUT BEFORE RETRIEVAL fires:
    ///   * visible conversation history (per-msg capped to
    ///     `CONV_HISTORY_CHARS_PER_MSG`),
    ///   * the compacted preamble we've already emitted on a prior
    ///     turn (if any — saves the call when the slot was already
    ///     hot),
    ///   * recalled memories (top-K, bounded).
    ///
    /// System message (persona + epistemic contract + thinking
    /// directive + tool dossier) and retrieval bundle are NOT
    /// measured — both fire later in the handler. The split is
    /// deliberate (compaction decides before retrieval runs and must
    /// not depend on retrieval state) but it makes this sensor
    /// systematically under-count when the system+retrieval terms
    /// are the dominant pressure source.
    ///
    /// Bench result (marathon_graceful 2026-05-26, three trials at
    /// PRESSURE_THRESHOLD ∈ {0.55, 0.7}): tuning the threshold
    /// against this narrow sensor monotonically regressed
    /// paraphrase-judge coverage (0.764 → 0.694 → 0.639). The
    /// thresholds that fire often enough to matter were firing
    /// when full-prompt was actually fine, triggering wasteful
    /// Fast-slot summarisation that lossy-compressed the preamble
    /// across multiple invocations. PRESSURE_THRESHOLD reverted to
    /// 0.9 (effective emergency-only); the architectural fix is a
    /// full-prompt sensor that takes (system_estimate,
    /// retrieval_estimate, history_estimate, response_reserve) —
    /// captured as a kind=todo note for the next iteration cycle.
    fn estimate_compaction_pressure(
        &self,
        context: &ConversationContext,
        frame_prompt: &str,
    ) -> (bool, u32, u32) {
        let Some(ctx_size) = self.inference.effective_context_size() else {
            return (false, 0, 0);
        };
        let threshold = (ctx_size as f32 * crate::runtime::COMPACTION_PRESSURE_THRESHOLD) as u32;

        let mut total: u32 = 0;
        // Visible conversation history: same per-msg truncate the
        // formatter applies. Use `count_tokens` on the truncated
        // body, not the full body — over-counting here would fire
        // compaction too aggressively.
        for msg in context.conversation.messages.iter() {
            let raw = &msg.content;
            let mut end = raw.len().min(CONV_HISTORY_CHARS_PER_MSG);
            while end > 0 && !raw.is_char_boundary(end) {
                end -= 1;
            }
            total = total.saturating_add(self.inference.count_tokens(&raw[..end]));
        }
        // The running conversation frame, in the form that actually
        // rides the prompt. Passed in by the caller, NOT read from
        // `context.compacted_history`: `build_context` rebuilds that
        // field as `None` every turn, so this branch contributed nothing
        // at all before the frame was persisted (2026-07-26) — it
        // measured a field that was structurally always empty at this
        // point in the turn.
        if !frame_prompt.is_empty() {
            total = total.saturating_add(self.inference.count_tokens(frame_prompt));
        }
        // Recalled memories — bounded at the FTS top-K but each can
        // carry 100-500 tokens.
        for mem in &context.memories {
            total = total.saturating_add(self.inference.count_tokens(&mem.content));
        }

        // Phase 2 (budget-sensor redesign): the component walk above
        // sees only history + memories + preamble — roughly a third
        // of the real prompt. System base, retrieval bundle, and the
        // response reservation are invisible to it, which is how an
        // 8k window could hard-fail at the engine while this sensor
        // read "no pressure". The assembly memo records what the LAST
        // turn's assembly actually demanded (pre-trim, including the
        // reservation); take it as a floor. First turn of a
        // conversation has no memo — one turn of the old blindness,
        // then converged.
        let real_floor = self
            .last_assembly(&context.conversation.id)
            .map(|m| m.input_tokens().saturating_add(m.reserved))
            .unwrap_or(0);
        if real_floor > total {
            tracing::debug!(
                component_estimate = total,
                real_floor,
                ctx_size,
                "compaction sensor: raising estimate to last turn's measured demand"
            );
            total = real_floor;
        }

        (total > threshold, total, ctx_size)
    }

    /// Retrieval-over-history spike (2026-05-26).
    ///
    /// Replaces — at least on the callback workload that crushed
    /// marathon_graceful T17-T20 — the lossy-summary mechanism with
    /// embedding-similarity retrieval over prior turns.
    ///
    /// Mechanism: embed each user+assistant pair *outside* the visible
    /// window, embed the current user message, cosine top-K (K=3),
    /// stash the hits on `context.history_retrieval_hits`. The renderer
    /// in `build_system_message` formats them as a "Relevant earlier
    /// turns:" prompt section.
    ///
    /// Gated on `SOVEREIGN_HISTORY_RETRIEVAL=1` for the spike phase.
    /// Off → no-op. On → runs after `maybe_compact_dropped_history`
    /// so the two can coexist during the A/B (the renderer will show
    /// both blocks if both fire — bench tells us which carries weight).
    ///
    /// Soft-fail by design: embed errors leave hits = None and the
    /// synthesis path continues on the existing compacted preamble +
    /// visible window.
    pub(crate) async fn maybe_retrieve_relevant_history(
        &self,
        context: &mut ConversationContext,
        user_message: &str,
        conversation_id: &str,
        // `Some` on the streaming surface (the call site sits below
        // `sessions.begin`, so a session exists); `None` for
        // non-streaming and test callers. Gates the narration chip
        // only — retrieval itself runs either way.
        session_id: Option<&str>,
    ) {
        // Default-on as of 2026-05-26 marathon_graceful spike outcome.
        // `SOVEREIGN_HISTORY_RETRIEVAL=0` disables for A/B compares.
        if std::env::var("SOVEREIGN_HISTORY_RETRIEVAL").ok().as_deref() == Some("0") {
            return;
        }
        tracing::debug!(
            messages_len = context.conversation.messages.len(),
            "runtime:history_retrieval.entry"
        );
        let messages = &context.conversation.messages;
        // Need at least one pair OLDER than the visible window. Visible
        // window is CONV_HISTORY_TURNS most recent messages. The
        // current user message is already pushed (runtime.rs:1386)
        // so subtract 1.
        if messages.len() <= crate::runtime::CONV_HISTORY_TURNS + 1 {
            return;
        }
        let dropped_end = messages
            .len()
            .saturating_sub(crate::runtime::CONV_HISTORY_TURNS + 1);
        let dropped = &messages[..dropped_end];

        // Build pair-shaped indexable units. Walk in (user, assistant)
        // pairs so each unit carries the question + its answer. Lone
        // trailing user message (if any) gets indexed alone.
        let mut units: Vec<(usize, String)> = Vec::new();
        let mut i = 0;
        while i < dropped.len() {
            let lead = &dropped[i];
            let body = if i + 1 < dropped.len() {
                let follow = &dropped[i + 1];
                format!(
                    "[{:?}] {}\n[{:?}] {}",
                    lead.role,
                    truncate_with_ellipsis(&lead.content, 600),
                    follow.role,
                    truncate_with_ellipsis(&follow.content, 600),
                )
            } else {
                format!(
                    "[{:?}] {}",
                    lead.role,
                    truncate_with_ellipsis(&lead.content, 600)
                )
            };
            units.push((i, body));
            i += 2;
        }
        if units.is_empty() {
            return;
        }

        // Cosine wants unit vectors. embed/embed_query already
        // normalize, but defend against unnormalized outputs from
        // custom providers — and normalize BEFORE memoizing so the
        // memo stores the scoring-ready form.
        let normalize = |v: &Vec<f32>| -> Vec<f32> {
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
            v.iter().map(|x| x / n).collect()
        };

        // Derive each candidate unit's vectors, taking memo hits where
        // we have them. The set of dropped pairs grows by one per turn
        // and each pair's body is stable, so without the memo turn N
        // pays N embeds (and N GLiNER extractions) — Θ(N²) over a
        // conversation, with the embed batch dominating pre-retrieval
        // latency past ~turn 20 on the 44-turn longhaul fixture.
        let unit_texts: Vec<String> = units.iter().map(|(_, b)| b.clone()).collect();
        let mut slots = self.history_unit_vectors(&unit_texts);
        let missing: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter(|(_, (_, v))| v.is_none())
            .map(|(i, _)| i)
            .collect();
        let memo_hits = slots.len() - missing.len();
        if !missing.is_empty() {
            // Embed the misses in a single batch. embed_batch falls
            // back to per-unit embed on providers that don't override
            // it.
            let miss_texts: Vec<String> = missing.iter().map(|&i| unit_texts[i].clone()).collect();
            let embeds = match self.inference.embed_batch(&miss_texts).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(error = %e, units = miss_texts.len(),
                        "runtime:history_retrieval.embed_batch_failed");
                    return;
                }
            };
            if embeds.len() != miss_texts.len() {
                tracing::warn!(
                    requested = miss_texts.len(),
                    returned = embeds.len(),
                    "runtime:history_retrieval.embed_batch_length_mismatch"
                );
                return;
            }
            for (&slot, emb) in missing.iter().zip(embeds) {
                // Entities are extracted here rather than per-turn for
                // the same reason as the embedding — `self.gliner` is
                // fixed for the Runtime's lifetime, so an entry
                // written under one extractor state stays valid.
                let entities: std::collections::HashSet<String> = self
                    .gliner
                    .as_ref()
                    .map(|g| g.extract_entities(&unit_texts[slot]).into_iter().collect())
                    .unwrap_or_default();
                let vectors = HistoryUnitVectors {
                    embedding: normalize(&emb),
                    entities,
                };
                self.store_history_unit_vectors(slots[slot].0, vectors.clone());
                slots[slot].1 = Some(vectors);
            }
        }
        tracing::debug!(
            units = slots.len(),
            memo_hits,
            computed = missing.len(),
            "runtime:history_retrieval.unit_vectors"
        );

        // Query enrichment (v5 tune): when the runtime extracted a
        // topic_context for this turn, append the topic + domain to
        // the embed-query text. Captures "switching back to <topic>"
        // semantics that bare follow-up phrasing misses (e.g.
        // T19 "And Linnaeus's framework — what part of his work
        // proved least durable?" embeds toward generic biology
        // unless we ride the topic_context anchor).
        let mut query_text = user_message.to_string();
        if let Some(tc) = context.topic_context.as_ref() {
            if let Some(t) = tc.topic.as_ref() {
                query_text.push_str("\n[topic: ");
                query_text.push_str(t);
                query_text.push(']');
            }
            if let Some(d) = tc.domain.as_ref() {
                query_text.push_str("\n[domain: ");
                query_text.push_str(d);
                query_text.push(']');
            }
        }
        let query_embed = match self.inference.embed_query(&query_text).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e,
                    "runtime:history_retrieval.embed_query_failed");
                return;
            }
        };

        let q_norm = normalize(&query_embed);

        // v7 entity-aware retrieval. When the runtime has a GLiNER
        // extractor wired, extract entities from the query (user
        // message + topic_context) and from each candidate pair, then
        // hybrid-score: 0.6·cosine + 0.4·jaccard. Fixes the v6 T17
        // failure mode where abstract callbacks ("church-and-science
        // theme") cosine-matched the wrong topic. GLiNER unavailable
        // → behaves exactly like v6 (pure cosine + MMR).
        const HYBRID_COSINE_WEIGHT: f32 = 0.6;
        const HYBRID_JACCARD_WEIGHT: f32 = 0.4;
        let query_entities: std::collections::HashSet<String> =
            if let Some(g) = self.gliner.as_ref() {
                g.extract_entities(&query_text).into_iter().collect()
            } else {
                std::collections::HashSet::new()
            };
        if !query_entities.is_empty() {
            tracing::debug!(
                entities = ?query_entities,
                "runtime:history_retrieval.query_entities"
            );
        }

        let jaccard =
            |a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>| -> f32 {
                if a.is_empty() || b.is_empty() {
                    return 0.0;
                }
                let inter = a.intersection(b).count() as f32;
                let union = a.union(b).count() as f32;
                if union == 0.0 {
                    0.0
                } else {
                    inter / union
                }
            };

        let mut scored: Vec<(usize, String, f32, Vec<f32>)> = Vec::with_capacity(units.len());
        for ((_, vectors), (idx, body)) in slots.into_iter().zip(units) {
            // Every slot was filled above or we returned early; treat a
            // gap as a bug worth seeing rather than silently scoring
            // the unit at zero.
            let Some(vectors) = vectors else {
                tracing::warn!(
                    turn_index = idx,
                    "runtime:history_retrieval.unit_vectors_missing"
                );
                continue;
            };
            let cos: f32 = vectors
                .embedding
                .iter()
                .zip(q_norm.iter())
                .map(|(a, b)| a * b)
                .sum();
            let sim = if self.gliner.is_some() {
                let j = jaccard(&query_entities, &vectors.entities);
                HYBRID_COSINE_WEIGHT * cos + HYBRID_JACCARD_WEIGHT * j
            } else {
                cos
            };
            scored.push((idx, body, sim, vectors.embedding));
        }

        // v6 tune: MMR (Maximal Marginal Relevance) selection.
        // v5 single trial regressed T20 -0.75 — cosine top-K picks
        // most-similar candidates, which on a "compare across Curie /
        // Linnaeus / Galileo" synthesis turn collapses onto whichever
        // topic dominates the topic_context (one bucket wins, two
        // missed). MMR optimises top-K = argmax λ·sim(d,q) −
        // (1−λ)·max sim(d, selected). λ=0.5 = balanced
        // relevance-vs-diversity. K stays 5, floor stays 0.30.
        const HISTORY_RETRIEVAL_TOP_K: usize = 5;
        const HISTORY_RETRIEVAL_SIM_FLOOR: f32 = 0.30;
        const HISTORY_RETRIEVAL_MMR_LAMBDA: f32 = 0.5;

        let mut candidates: Vec<(usize, String, f32, Vec<f32>)> = scored
            .into_iter()
            .filter(|(_, _, s, _)| *s >= HISTORY_RETRIEVAL_SIM_FLOOR)
            .collect();
        // Sort once descending by relevance for stable MMR seeding.
        candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        let mut selected: Vec<(usize, String, f32)> = Vec::with_capacity(HISTORY_RETRIEVAL_TOP_K);
        let mut selected_embeds: Vec<Vec<f32>> = Vec::with_capacity(HISTORY_RETRIEVAL_TOP_K);

        while selected.len() < HISTORY_RETRIEVAL_TOP_K && !candidates.is_empty() {
            let mut best_pos = 0;
            let mut best_score = f32::MIN;
            for (i, c) in candidates.iter().enumerate() {
                let max_sim_to_selected: f32 = selected_embeds
                    .iter()
                    .map(|s| s.iter().zip(c.3.iter()).map(|(a, b)| a * b).sum::<f32>())
                    .fold(0.0_f32, f32::max);
                let mmr = HISTORY_RETRIEVAL_MMR_LAMBDA * c.2
                    - (1.0 - HISTORY_RETRIEVAL_MMR_LAMBDA) * max_sim_to_selected;
                if mmr > best_score {
                    best_score = mmr;
                    best_pos = i;
                }
            }
            let (idx, body, sim, emb) = candidates.remove(best_pos);
            selected.push((idx, body, sim));
            selected_embeds.push(emb);
        }

        let hits: Vec<crate::types::HistoryRetrievalHit> = selected
            .into_iter()
            .map(
                |(turn_index, content, similarity)| crate::types::HistoryRetrievalHit {
                    turn_index,
                    content,
                    similarity,
                },
            )
            .collect();

        if hits.is_empty() {
            tracing::debug!(
                candidates = dropped.len() / 2,
                "runtime:history_retrieval.no_hits_above_floor"
            );
            return;
        }
        // Glassbox per-hit summary at debug. Captures the picks chosen
        // by hybrid (cosine·0.6 + jaccard·0.4) + MMR for post-mortem
        // analysis of "did retrieval surface the right earlier turn?"
        // RUST_LOG=sovereign_core::runtime::retrieval=debug to see it.
        let hit_summary: Vec<String> = hits
            .iter()
            .map(|h| format!("T{}@{:.2}", h.turn_index, h.similarity))
            .collect();
        tracing::debug!(
            hits = hits.len(),
            top_sim = hits[0].similarity,
            picked = %hit_summary.join(","),
            "runtime:history_retrieval.populated"
        );
        context.history_retrieval_hits = Some(hits);

        // Glassbox the recall. Until this chip existed, retrieval-over-
        // history was the one memory channel with NO user-visible
        // surface at all: the hits went straight into the system
        // prompt (`build_system_message`) and the chat said nothing,
        // so an answer that correctly called back to turn 3 was
        // indistinguishable from a lucky parametric guess — and a
        // missed callback looked like plain amnesia rather than a
        // similarity floor the user could work around by rephrasing.
        if let Some(sid) = session_id {
            let hits_ref = context
                .history_retrieval_hits
                .as_ref()
                .expect("hits were just stashed");
            let turn_indices: Vec<usize> = hits_ref.iter().map(|h| h.turn_index).collect();
            let top_similarity = hits_ref
                .iter()
                .map(|h| h.similarity)
                .fold(f32::MIN, f32::max);
            let turns = hits_ref.len();
            let text = if turns == 1 {
                "Recalled 1 earlier exchange from this conversation.".to_string()
            } else {
                format!("Recalled {turns} earlier exchanges from this conversation.")
            };
            self.routing_events
                .emit_turn_narration(crate::types::TurnNarration {
                    session_id: sid.to_string(),
                    conversation_id: conversation_id.to_string(),
                    event: crate::types::NarrationEvent {
                        phase: crate::types::NarrationPhase::ConversationRecall {
                            turns,
                            turn_indices,
                            top_similarity,
                        },
                        text,
                        elapsed_ms: 0,
                    },
                })
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use futures::Stream;

    use super::Runtime;
    use crate::error::{Error, Result};
    use crate::registry::ToolRegistry;
    use crate::skills::SkillRegistry;
    use crate::traits::InferenceProvider;
    use crate::types::{
        CompletionRequest, CompletionResponse, Conversation, ConversationContext, Depth, Message,
        ProviderCapabilities, Role, Speed,
    };
    use std::sync::Arc;

    /// Records every body handed to `embed_batch` so the test can
    /// assert on WHICH units were (re-)embedded, not just how many —
    /// and every `complete()` prompt, so the compaction tests can assert
    /// on how big the fold prompt actually got.
    struct CountingEmbed {
        batched: Mutex<Vec<Vec<String>>>,
        completions: Mutex<Vec<String>>,
    }

    impl CountingEmbed {
        fn new() -> Self {
            Self {
                batched: Mutex::new(Vec::new()),
                completions: Mutex::new(Vec::new()),
            }
        }
        fn completions(&self) -> Vec<String> {
            self.completions.lock().unwrap().clone()
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.batched.lock().unwrap().clone()
        }
        /// Deterministic pseudo-embedding: 4 dims driven by the body's
        /// bytes. Distinct bodies get distinct vectors, and every
        /// component is positive so cosine against the query clears
        /// the 0.30 floor — the test cares about embed CALLS, not
        /// ranking.
        fn embed_one(text: &str) -> Vec<f32> {
            let mut v = vec![1.0_f32; 4];
            for (i, b) in text.bytes().enumerate() {
                v[i % 4] += (b % 7) as f32 * 0.01;
            }
            v
        }
    }

    #[async_trait]
    impl InferenceProvider for CountingEmbed {
        async fn complete(&self, r: &CompletionRequest) -> Result<CompletionResponse> {
            self.completions.lock().unwrap().push(r.prompt.clone());
            Ok(CompletionResponse {
                text: r#"{"topics": "several numbered topics", "entities": "topic 0, topic 1"}"#
                    .to_string(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "counting".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
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
        async fn embed_query(&self, q: &str) -> Result<Vec<f32>> {
            Ok(Self::embed_one(q))
        }
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.batched.lock().unwrap().push(texts.to_vec());
            Ok(texts.iter().map(|t| Self::embed_one(t)).collect())
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    /// Read a conversation's persisted frame, parsed.
    async fn stored_frame(runtime: &Runtime, conversation_id: &str) -> sovereign_contracts::frame::Frame {
        let raw = runtime
            .store
            .get_conversation_frame(conversation_id)
            .await
            .expect("store read");
        assert!(raw.is_some(), "a fold must persist the frame");
        crate::conv_frame::parse(raw.as_deref())
    }

    fn runtime_with(inference: Arc<CountingEmbed>) -> Runtime {
        runtime_with_store(
            inference,
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
        )
    }

    fn runtime_with_store(
        inference: Arc<CountingEmbed>,
        store: Arc<sovereign_store::memory::InMemoryStateStore>,
    ) -> Runtime {
        Runtime::new(
            inference,
            Box::new(crate::stubs::PassthroughRouter),
            Box::new(crate::stubs::NoOpPlanner),
            Arc::new(ToolRegistry::new()),
            store,
            Arc::new(SkillRegistry::new()),
            Arc::new(crate::executor::AutoApprovalChannel),
            crate::types::InferenceConfig::default(),
        )
    }

    fn msg(i: usize, role: Role, content: String) -> Message {
        Message {
            id: format!("m{i}-{role:?}"),
            conversation_id: "conv-memo".to_string(),
            role,
            content,
            created_at: 1_700_000_000 + i as i64,
            metadata: None,
            version: 0,
        }
    }

    fn context_with_turns(pairs: usize) -> ConversationContext {
        let mut messages = Vec::new();
        for i in 0..pairs {
            messages.push(msg(
                i,
                Role::User,
                format!("question number {i} about topic {i}"),
            ));
            messages.push(msg(
                i,
                Role::Assistant,
                format!("answer number {i} covering topic {i}"),
            ));
        }
        ConversationContext {
            conversation: Conversation {
                id: "conv-memo".to_string(),
                title: None,
                messages,
                created_at: 0,
                updated_at: 0,
                version: 0,
                deleted_at: None,
                skill_id: None,
                enabled_corpora: None,
                searched_sources: None,
            },
            memories: Vec::new(),
            working_memory: None,
            installed_corpora: vec![],
            corpus_ceiling: None,
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
            temporal_tensions: Vec::new(),
            compacted_history: None,
            history_retrieval_hits: None,
            tool_dossier: None,
            intent_policy: None,
        }
    }

    /// Captures every narration the runtime emits so the recall tests
    /// can assert on the chip the user actually sees.
    #[derive(Default)]
    struct RecordingEvents {
        narrations: Mutex<Vec<crate::types::TurnNarration>>,
    }

    impl RecordingEvents {
        fn phases(&self) -> Vec<crate::types::NarrationPhase> {
            self.narrations
                .lock()
                .unwrap()
                .iter()
                .map(|n| n.event.phase.clone())
                .collect()
        }
    }

    #[async_trait]
    impl crate::traits::RoutingEventSink for RecordingEvents {
        async fn emit_interpretation_proposed(&self, _p: crate::types::InterpretationProposed) {}
        async fn emit_clarification_request(&self, _p: crate::types::ClarificationRequest) {}
        async fn emit_turn_narration(&self, payload: crate::types::TurnNarration) {
            self.narrations.lock().unwrap().push(payload);
        }
    }

    /// Recall must be VISIBLE. Before the `ConversationRecall` frame,
    /// retrieval-over-history spliced earlier turns into the prompt
    /// with no user-facing surface at all — a correct callback was
    /// indistinguishable from a lucky guess. The chip carries the
    /// turn indices and the best similarity so the claim is checkable.
    #[tokio::test]
    async fn recall_emits_a_narration_chip_naming_the_turns() {
        let inference = Arc::new(CountingEmbed::new());
        let events = Arc::new(RecordingEvents::default());
        let runtime = runtime_with(Arc::clone(&inference))
            .with_routing_events(Arc::clone(&events) as Arc<dyn crate::traits::RoutingEventSink>);

        let mut ctx = context_with_turns(12);
        runtime
            .maybe_retrieve_relevant_history(
                &mut ctx,
                "tell me about topic 0",
                "conv-memo",
                Some("session-1"),
            )
            .await;

        let hits = ctx
            .history_retrieval_hits
            .as_ref()
            .expect("retrieval produced hits");
        let phases = events.phases();
        assert_eq!(phases.len(), 1, "exactly one recall chip per turn");
        match &phases[0] {
            crate::types::NarrationPhase::ConversationRecall {
                turns,
                turn_indices,
                top_similarity,
            } => {
                assert_eq!(*turns, hits.len(), "chip counts the hits it narrates");
                assert_eq!(
                    turn_indices,
                    &hits.iter().map(|h| h.turn_index).collect::<Vec<_>>(),
                    "chip carries the turn indices, so the UI can anchor to them"
                );
                let best = hits.iter().map(|h| h.similarity).fold(f32::MIN, f32::max);
                assert!(
                    (*top_similarity - best).abs() < f32::EPSILON,
                    "chip reports the best similarity among the hits"
                );
            }
            other => panic!("expected ConversationRecall, got {other:?}"),
        }
    }

    /// The chip is gated on having a session to hang from — the
    /// non-streaming and test call paths pass `None` and must stay
    /// silent while retrieval itself still runs.
    #[tokio::test]
    async fn recall_without_a_session_still_retrieves_but_stays_silent() {
        let inference = Arc::new(CountingEmbed::new());
        let events = Arc::new(RecordingEvents::default());
        let runtime = runtime_with(Arc::clone(&inference))
            .with_routing_events(Arc::clone(&events) as Arc<dyn crate::traits::RoutingEventSink>);

        let mut ctx = context_with_turns(12);
        runtime
            .maybe_retrieve_relevant_history(&mut ctx, "tell me about topic 0", "conv-memo", None)
            .await;

        assert!(
            ctx.history_retrieval_hits.is_some(),
            "retrieval runs regardless of whether a session exists"
        );
        assert!(events.phases().is_empty(), "no session, no chip");
    }

    /// The memo's whole purpose: turn N must not re-embed the pairs
    /// turn N-1 already embedded. Without it each turn re-embeds every
    /// dropped pair — Θ(N²) embeds over a conversation.
    #[tokio::test]
    async fn history_units_are_embedded_once_across_turns() {
        let inference = Arc::new(CountingEmbed::new());
        let runtime = runtime_with(Arc::clone(&inference));

        // 12 pairs = 24 messages; the visible window keeps the last 9,
        // so the dropped region is messages[..15] → 8 pair-units.
        let mut ctx = context_with_turns(12);
        runtime
            .maybe_retrieve_relevant_history(&mut ctx, "tell me about topic 0", "conv-memo", None)
            .await;
        let first = inference.calls();
        assert_eq!(first.len(), 1, "one embed_batch on the cold turn");
        let cold_units = first[0].len();
        assert!(cold_units > 0, "cold turn embedded some units");
        assert!(
            ctx.history_retrieval_hits.is_some(),
            "retrieval produced hits, so scoring ran on the memoized vectors"
        );

        // Same conversation, same dropped region: every unit is a memo
        // hit, so embed_batch must not be called at all.
        let mut ctx2 = context_with_turns(12);
        runtime
            .maybe_retrieve_relevant_history(&mut ctx2, "and topic 3?", "conv-memo", None)
            .await;
        assert_eq!(
            inference.calls().len(),
            1,
            "second turn over the same pairs issued no new embed_batch"
        );
        assert!(ctx2.history_retrieval_hits.is_some());

        // Now grow the conversation turn by turn. Each turn pushes two
        // more messages out of the visible window, so at most TWO unit
        // bodies change: the pair that just entered the dropped region,
        // plus the message that was previously a lone trailing unit and
        // now has a partner (its rendered body — hence its key —
        // changes). Everything older is a memo hit.
        //
        // This is the property that kills the Θ(N²): per-turn embed
        // work is a small CONSTANT while the candidate set grows.
        const MAX_NEW_UNITS_PER_TURN: usize = 2;
        let mut naive_total = cold_units;
        for pairs in 13..=20 {
            let before = inference.calls().len();
            let mut ctx_n = context_with_turns(pairs);
            runtime
                .maybe_retrieve_relevant_history(&mut ctx_n, "topic 5 again", "conv-memo", None)
                .await;
            let calls = inference.calls();
            let units_this_turn = ctx_n
                .history_retrieval_hits
                .as_ref()
                .map(|_| pairs)
                .unwrap_or(0);
            naive_total += units_this_turn;
            for batch in calls.iter().skip(before) {
                assert!(
                    batch.len() <= MAX_NEW_UNITS_PER_TURN,
                    "turn at {pairs} pairs embedded {} units; the memo should leave \
                     at most {MAX_NEW_UNITS_PER_TURN} bodies changed",
                    batch.len()
                );
            }
        }
        let embedded_total: usize = inference.calls().iter().map(|c| c.len()).sum();
        assert!(
            embedded_total < naive_total / 2,
            "memoized embeds ({embedded_total}) must be far below the naive \
             re-embed-everything cost ({naive_total})"
        );
    }

    /// The compaction fold must cost the same at turn 40 as at turn 5.
    /// Before the rolling fold, every turn re-synthesised the preamble
    /// from the entire dropped tail: one Housekeep completion per turn
    /// whose prompt grew with the conversation, on the path the user
    /// waits on.
    #[tokio::test]
    async fn compaction_folds_incrementally_with_a_bounded_prompt() {
        let inference = Arc::new(CountingEmbed::new());
        let runtime = runtime_with(Arc::clone(&inference));

        // Cold fold at 8 pairs (16 messages, 8 dropped).
        let mut ctx = context_with_turns(8);
        runtime
            .maybe_compact_dropped_history(&mut ctx, "conv-memo", None)
            .await;
        assert_eq!(inference.completions().len(), 1, "cold fold ran");
        assert!(
            ctx.compacted_history.is_some(),
            "the cold fold populated the preamble"
        );
        let cold_prompt_len = inference.completions()[0].len();

        // Next turn: only 2 more messages dropped, below the stride, so
        // the running preamble is REUSED with no completion at all —
        // and it must still reach the prompt.
        let mut ctx2 = context_with_turns(9);
        runtime
            .maybe_compact_dropped_history(&mut ctx2, "conv-memo", None)
            .await;
        assert_eq!(
            inference.completions().len(),
            1,
            "a sub-stride turn must not pay for another fold"
        );
        assert!(
            ctx2.compacted_history.is_some(),
            "a turn that skips the fold must still carry the stored frame — \
             dropping it is the regression persistence exists to prevent"
        );

        // Run out to 40 pairs. Folds happen on the stride, and every
        // fold prompt stays near the cold-fold size instead of growing
        // with the conversation.
        for pairs in 10..=40 {
            let mut ctx_n = context_with_turns(pairs);
            runtime
                .maybe_compact_dropped_history(&mut ctx_n, "conv-memo", None)
                .await;
            assert!(
                ctx_n.compacted_history.is_some(),
                "every turn past the trigger carries a preamble ({pairs} pairs)"
            );
        }

        let prompts = inference.completions();
        let longest = prompts.iter().map(|p| p.len()).max().unwrap_or(0);
        assert!(
            longest < cold_prompt_len * 3,
            "fold prompts must stay bounded: longest {longest} vs cold {cold_prompt_len}"
        );
        // 33 turns past the cold fold at stride 6 (2 messages dropped
        // per turn) is ~11 folds, not 33.
        assert!(
            prompts.len() < 16,
            "stride should keep folds well below one-per-turn; got {}",
            prompts.len()
        );

        // The watermark tracks the dropped edge to within one stride —
        // it lags only by the messages deliberately left un-folded.
        // 40 pairs = 80 messages, visible window keeps 8.
        let dropped_edge = 80 - crate::runtime::CONV_HISTORY_TURNS;
        let frame = stored_frame(&runtime, "conv-memo").await;
        let covered = crate::conv_frame::covered_upto(&frame);
        assert!(
            covered <= dropped_edge
                && dropped_edge - covered < crate::runtime::CONV_COMPACT_FOLD_STRIDE,
            "watermark {covered} should trail the dropped edge {dropped_edge} by \
             less than one stride"
        );
        assert_eq!(
            crate::conv_frame::elided(&frame),
            0,
            "no fold window ever hit the cap here"
        );
        assert!(
            frame.body("Topics").is_some_and(|b| !b.trim().is_empty()),
            "the fold wrote the sections the model returned"
        );
    }

    /// A cold fold meeting an already-long conversation (fresh process,
    /// resumed thread) must cap the prompt rather than feed the whole
    /// tail to the model — that unbounded path is what walked off a
    /// silent cliff around 168 dropped messages.
    #[tokio::test]
    async fn cold_fold_caps_a_long_conversation_and_reports_the_gap() {
        let inference = Arc::new(CountingEmbed::new());
        let runtime = runtime_with(Arc::clone(&inference));

        // 120 pairs = 240 messages; 232 of them are dropped.
        let mut ctx = context_with_turns(120);
        runtime
            .maybe_compact_dropped_history(&mut ctx, "conv-long", None)
            .await;

        let prompts = inference.completions();
        assert_eq!(prompts.len(), 1, "one cold fold");
        // The fold read at most CONV_COMPACT_MAX_FOLD_MSGS messages, so
        // the prompt cannot mention every turn. Turn 60's body would
        // appear only if the cap were ignored.
        assert!(
            !prompts[0].contains("question number 60"),
            "the cap must exclude the middle of a long tail"
        );
        assert!(
            prompts[0].contains("question number 0"),
            "the head is kept — it carries how the conversation opened"
        );
        assert!(
            prompts[0].contains("never shown to you"),
            "the elision is stated in the prompt, so the model cannot present \
             a partial arc as complete"
        );

        let frame = stored_frame(&runtime, "conv-long").await;
        assert_eq!(
            crate::conv_frame::covered_upto(&frame),
            240 - crate::runtime::CONV_HISTORY_TURNS
        );
        assert!(
            crate::conv_frame::elided(&frame) > 0,
            "the gap is recorded in the frame, not forgotten"
        );
    }

    /// The frame is the persistence contract: a second Runtime over the
    /// same store must resume folding from the watermark instead of
    /// re-reading the conversation. This is what a process restart (or a
    /// desktop relaunch) actually does.
    #[tokio::test]
    async fn a_fresh_runtime_resumes_from_the_persisted_frame() {
        let store = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
        let first_inf = Arc::new(CountingEmbed::new());
        let first = runtime_with_store(Arc::clone(&first_inf), Arc::clone(&store));

        let mut ctx = context_with_turns(20);
        first
            .maybe_compact_dropped_history(&mut ctx, "conv-resume", None)
            .await;
        assert_eq!(first_inf.completions().len(), 1, "cold fold on process 1");

        // New Runtime, same store, same conversation, one turn later.
        let second_inf = Arc::new(CountingEmbed::new());
        let second = runtime_with_store(Arc::clone(&second_inf), Arc::clone(&store));
        let mut ctx2 = context_with_turns(21);
        second
            .maybe_compact_dropped_history(&mut ctx2, "conv-resume", None)
            .await;

        assert_eq!(
            second_inf.completions().len(),
            0,
            "the resumed process is inside the stride and must not re-fold — \
             re-summarising from scratch on every restart is the bug this \
             column exists to fix"
        );
        assert!(
            ctx2.compacted_history.is_some(),
            "and it still carries the frame the previous process wrote"
        );
    }
}
