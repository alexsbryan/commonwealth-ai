// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation-history memory: dropped-history compaction
//! (turn-count + budget-pressure arms) and
//! retrieval-over-history.

use super::super::*;

impl Runtime {
    /// Summarize the dropped tail of the conversation so the
    /// synthesis prompt retains an anchor for entities and topics
    /// established outside the visible-history window.
    ///
    /// Activates only when `conversation.messages.len()` exceeds the
    /// visible-history window by at least
    /// `CONV_HISTORY_COMPACT_MIN_DROPPED` messages. The summary is
    /// stored on `context.compacted_history` and read back by
    /// `format_conversation_history` at prompt-assembly time.
    ///
    /// Soft-fail by design: a parse failure or an inference error
    /// leaves `compacted_history = None` and the synthesis path
    /// continues on just the visible window. Surfaced by
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
            self.estimate_compaction_pressure(context);
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

        match crate::context::summarize_dropped_history(self.inference.as_ref(), dropped).await {
            Ok(summary @ Some(_)) => {
                context.compacted_history = summary;
                // Glassbox the compaction so the user sees why their
                // chat surface changed shape. Gated below
                // `COMPACTION_CHIP_MIN_DROPPED = 3` — folding 2
                // messages would chip-spam on every long-chat turn.
                let dropped_count = dropped.len();
                if dropped_count >= crate::runtime::COMPACTION_CHIP_MIN_DROPPED {
                    if let Some(sid) = session_id {
                        self.routing_events
                            .emit_turn_narration(crate::types::TurnNarration {
                                session_id: sid.to_string(),
                                conversation_id: conversation_id.to_string(),
                                event: crate::types::NarrationEvent {
                                    phase: crate::types::NarrationPhase::GapCheckFired,
                                    text: format!(
                                        "Folded {dropped_count} earlier turns into a summary to keep context fresh."
                                    ),
                                    elapsed_ms: 0,
                                },
                            })
                            .await;
                    }
                }
            }
            Ok(None) => {
                tracing::debug!(
                    dropped = dropped.len(),
                    "context: summarize_dropped_history returned None — falling back to visible window only"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    dropped = dropped.len(),
                    "context: summarize_dropped_history failed — falling back to visible window only"
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
    fn estimate_compaction_pressure(&self, context: &ConversationContext) -> (bool, u32, u32) {
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
        // Pre-existing compacted preamble (from a prior turn on this
        // conversation). It rides every prompt until the slot
        // unloads.
        if let Some(s) = &context.compacted_history {
            total = total.saturating_add(self.inference.count_tokens(s));
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

        // Embed the candidate units in a single batch + the query
        // separately. embed_batch falls back to per-unit embed on
        // providers that don't override it.
        let unit_texts: Vec<String> = units.iter().map(|(_, b)| b.clone()).collect();
        let unit_embeds = match self.inference.embed_batch(&unit_texts).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, units = units.len(),
                    "runtime:history_retrieval.embed_batch_failed");
                return;
            }
        };
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

        // Cosine score. embed/embed_query already normalize, but defend
        // against unnormalized outputs from custom providers.
        let normalize = |v: &Vec<f32>| -> Vec<f32> {
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
            v.iter().map(|x| x / n).collect()
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

        let gliner = self.gliner.clone();
        let scored: Vec<(usize, String, f32, Vec<f32>)> = unit_embeds
            .into_iter()
            .zip(units)
            .map(|(emb, (idx, body))| {
                let e_norm = normalize(&emb);
                let cos: f32 = e_norm.iter().zip(q_norm.iter()).map(|(a, b)| a * b).sum();
                let sim = if let Some(g) = gliner.as_ref() {
                    let pair_ents: std::collections::HashSet<String> =
                        g.extract_entities(&body).into_iter().collect();
                    let j = jaccard(&query_entities, &pair_ents);
                    HYBRID_COSINE_WEIGHT * cos + HYBRID_JACCARD_WEIGHT * j
                } else {
                    cos
                };
                (idx, body, sim, e_norm)
            })
            .collect();

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
    }
}
