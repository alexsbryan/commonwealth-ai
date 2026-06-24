// SPDX-License-Identifier: AGPL-3.0-or-later
//! KnowledgeQuery dispatch + its retrieval-miss diversion path.
//!
//! `prepare_knowledge_query_plan` is the load-bearing setup that
//! both the non-streaming `handle_knowledge_query` and the streaming
//! KQ branch in `handle_message_stream_with_classification` consume
//! — single source of truth for retrieval, expansion, and routing.
//!
//! The `handle_retrieval_miss_*` trio fires when retrieval came back
//! dispersed; it suppresses synthesis and surfaces a clarification
//! card instead of letting the model fabricate against noise.

use std::collections::HashMap;

use crate::error::Result;
use crate::traits::*;

use super::super::*;

impl Runtime {
    /// PR5 — post-retrieval Ask diversion. Fires when retrieval ran
    /// successfully but produced dispersed noise (see
    /// `EvidenceShape::is_off_target`). Classification was high
    /// enough to commit, but synthesis against off-target evidence
    /// is exactly the shape that produces confident fabrication —
    /// so we suppress synthesis and show the user their options
    /// instead: answer from general knowledge (explicit opt-in),
    /// search the web (if tool available), or rephrase.
    ///
    /// Returns a closed stream with a placeholder message carrying
    /// a `clarification` metadata field — same shape the regular
    /// Ask path uses, so the existing `ClarificationCard` renders
    /// it without any UI-layer changes.
    pub(crate) async fn handle_retrieval_miss_stream(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        shape: &EvidenceShape,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<StreamHandle> {
        let message_id = uuid::Uuid::new_v4().to_string();

        // Build options aimed at the miss: let the user opt in to
        // parametric synthesis, web-search if available, or
        // rephrase. Three options max — the ClarificationCard also
        // offers a free-text fallback, so adding more options would
        // just be clutter.
        let mut options: Vec<ClarificationOption> = Vec::new();
        options.push(ClarificationOption {
            label: "Answer from general knowledge (may be inaccurate)".to_string(),
            follow_up: original_message.to_string(),
            intent_hint: "simple_query".to_string(),
        });
        if let Some(web_tool) = tool_descriptors
            .iter()
            .find(|t| t.name.contains("web_search") || t.name == "search")
        {
            options.push(ClarificationOption {
                label: "Search the web".to_string(),
                follow_up: original_message.to_string(),
                intent_hint: format!("simple_action:{}", web_tool.id),
            });
        }
        options.push(ClarificationOption {
            label: "Rephrase — I'll try again".to_string(),
            // Empty follow_up signals "wait for user input" — the UI
            // surfaces the clarification card's free-text box.
            follow_up: original_message.to_string(),
            intent_hint: "deep_query".to_string(),
        });

        let question = format!(
            "I searched {} sources but nothing looked relevant to \"{}\". \
             How would you like me to proceed?",
            shape.distinct_sources,
            truncate_with_ellipsis(original_message, 80),
        );

        let clarification_payload = ClarificationRequest {
            session_id: session_id.to_string(),
            conversation_id: conversation_id.to_string(),
            question: question.clone(),
            options: options.clone(),
        };

        let placeholder_body = "I didn't find anything relevant in your installed knowledge bases \
             for that question. Rather than guess, I'd like to check how you'd \
             like me to proceed."
            .to_string();
        let metadata = serde_json::json!({
            // The user's intent was a knowledge query; it merely missed.
            // Carry it so the turn satisfies the provenance contract (every
            // turn names an intent) — a retrieval miss is still a knowledge
            // turn, not an intent-less blank.
            "intent": "knowledge_query",
            "move_kind": "ask",
            "retrieval_missed": true,
            "documents_found": shape.count,
            "distinct_sources": shape.distinct_sources,
            "clarification": {
                "session_id": session_id,
                "question": question,
                "options": options,
            },
        });
        let assistant_msg = Message {
            id: message_id.clone(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: placeholder_body.clone(),
            created_at: now(),
            metadata: Some(metadata),
            version: 0,
        };
        self.store.save_message(&assistant_msg).await?;

        self.routing_events
            .emit_clarification_request(clarification_payload)
            .await;

        tracing::info!(
            session_id,
            conversation_id,
            distinct_sources = shape.distinct_sources,
            retrieval_count = shape.count,
            "routing:retrieval_miss — synthesis suppressed, clarification requested"
        );

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(1);
        let _ = tx.send(Ok(placeholder_body)).await;
        drop(tx);

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(StreamHandle {
            message_id,
            stream: Box::pin(stream),
        })
    }
    /// Test-only entry point that directly invokes
    /// `handle_retrieval_miss_stream`. Integration tests can't
    /// easily drive the full KnowledgeQuery pipeline (no corpora in
    /// the harness), so this exposes the diversion method for
    /// isolated verification.
    pub async fn invoke_retrieval_miss_stream_for_test(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        shape: &EvidenceShape,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<StreamHandle> {
        self.handle_retrieval_miss_stream(
            original_message,
            conversation_id,
            session_id,
            shape,
            tool_descriptors,
        )
        .await
    }
    /// PR5 — non-streaming sibling of `handle_retrieval_miss_stream`.
    /// Same suppression + clarification semantics; returns a
    /// Response carrying the placeholder body + metadata so CLI /
    /// server callers get a consistent behavior.
    pub(crate) async fn handle_retrieval_miss_response(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        shape: &EvidenceShape,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<Response> {
        let message_id = uuid::Uuid::new_v4().to_string();

        let mut options: Vec<ClarificationOption> = Vec::new();
        options.push(ClarificationOption {
            label: "Answer from general knowledge (may be inaccurate)".to_string(),
            follow_up: original_message.to_string(),
            intent_hint: "simple_query".to_string(),
        });
        if let Some(web_tool) = tool_descriptors
            .iter()
            .find(|t| t.name.contains("web_search") || t.name == "search")
        {
            options.push(ClarificationOption {
                label: "Search the web".to_string(),
                follow_up: original_message.to_string(),
                intent_hint: format!("simple_action:{}", web_tool.id),
            });
        }
        options.push(ClarificationOption {
            label: "Rephrase — I'll try again".to_string(),
            follow_up: original_message.to_string(),
            intent_hint: "deep_query".to_string(),
        });

        let question = format!(
            "I searched {} sources but nothing looked relevant to \"{}\". \
             How would you like me to proceed?",
            shape.distinct_sources,
            truncate_with_ellipsis(original_message, 80),
        );

        let clarification_payload = ClarificationRequest {
            session_id: session_id.to_string(),
            conversation_id: conversation_id.to_string(),
            question: question.clone(),
            options: options.clone(),
        };

        let placeholder_body = "I didn't find anything relevant in your installed knowledge bases \
             for that question. Rather than guess, I'd like to check how you'd \
             like me to proceed."
            .to_string();
        let metadata = serde_json::json!({
            // The user's intent was a knowledge query; it merely missed.
            // Carry it so the turn satisfies the provenance contract (every
            // turn names an intent) — a retrieval miss is still a knowledge
            // turn, not an intent-less blank.
            "intent": "knowledge_query",
            "move_kind": "ask",
            "retrieval_missed": true,
            "documents_found": shape.count,
            "distinct_sources": shape.distinct_sources,
            "clarification": {
                "session_id": session_id,
                "question": question,
                "options": options,
            },
        });
        let assistant_msg = Message {
            id: message_id,
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: placeholder_body,
            created_at: now(),
            metadata: Some(metadata),
            version: 0,
        };
        self.store.save_message(&assistant_msg).await?;
        let response_msg = assistant_msg.clone();

        self.routing_events
            .emit_clarification_request(clarification_payload)
            .await;

        tracing::info!(
            session_id,
            conversation_id,
            distinct_sources = shape.distinct_sources,
            retrieval_count = shape.count,
            "routing:retrieval_miss — synthesis suppressed (non-streaming)"
        );

        Ok(Response {
            message: response_msg,
            task: None,
            metrics: None,
        })
    }

    /// Build the complete synthesis plan for a KnowledgeQuery turn:
    /// retrieval + evidence-shape routing + source-cohesion expansion +
    /// request construction + metadata for the UI (retrieved_chunks
    /// summaries, source_map, result_quality).
    ///
    /// Shared between [`Self::handle_knowledge_query`] (non-streaming)
    /// and the streaming KQ branch in
    /// [`Self::handle_message_stream`] so both paths cannot diverge
    /// in how they search, expand, or build the request.
    pub(crate) async fn prepare_knowledge_query_plan(
        &self,
        message: &str,
        context: &ConversationContext,
        intent: &Intent,
        scope: Option<&str>,
    ) -> KnowledgeQueryPlan {
        tracing::info!(
            message_chars = message.len(),
            "handle_knowledge_query: begin"
        );

        // 1. Embed the query using the query-side function (applies
        //    instruction prefix for asymmetric models like Qwen3-Embedding).
        //
        //    Follow-up turns get the prior-user-turn topic anchor
        //    folded in via `build_retrieval_query` so the embedded
        //    text isn't just "What did he publish in 1905?" — which
        //    matches no Einstein chunk. BM25 leg below still sees
        //    the bare message. See sovereign/bench/wikipedia_learn.
        let t_search = std::time::Instant::now();
        let retrieval_query = build_retrieval_query(message, context);
        if retrieval_query != message {
            tracing::debug!(
                bare_chars = message.len(),
                expanded_chars = retrieval_query.len(),
                "retrieval: expanded follow-up query with prior user turns"
            );
        }
        // Captured for retrieval_audit turn_summary at end of fn.
        let topic_for_audit: Option<String> = context
            .topic_context
            .as_ref()
            .and_then(|tc| tc.topic.clone());
        let prior_messages_for_audit = context.conversation.messages.len();
        let query_preview_for_audit = truncate_with_ellipsis(message, 120).to_string();
        let retrieval_query_preview_for_audit =
            truncate_with_ellipsis(&retrieval_query, 160).to_string();
        let embedding = self
            .inference
            .embed_query(&retrieval_query)
            .await
            .unwrap_or_default();

        // 2. Run the KnowledgeQuery retrieval pipeline — the ordered,
        //    traced step list in `retrieval_pipeline::kq_pipeline()`:
        //    the shared evidence-gathering head (local ∥ mesh retrieval
        //    → scope filter → store search) → the shared core (boosts,
        //    expansions, noise floor, grounding, merge) → the KQ
        //    truncate tail. The per-step trace rides the
        //    `retrieval.pipeline` target. The step ORDER is bench-tuned
        //    data — pinned by golden tests in retrieval_pipeline.rs.
        let mut pipeline_state = PipelineState::new(
            message,
            context,
            intent,
            scope,
            embedding,
            "KnowledgeQuery",
            "KnowledgeQuery".to_string(),
        );
        kq_pipeline().run(self, &mut pipeline_state).await;
        let PipelineState {
            chunks,
            embedding,
            hot_corpora,
            entities,
            meta_atlas_hits,
            ..
        } = pipeline_state;

        let search_ms = t_search.elapsed().as_millis() as u64;
        // Per-corpus breakdown at this checkpoint — paired with the
        // graph-walk trace upstream so any drop between the two is
        // visible (ARCH §0.1 glassbox).
        let post_truncate_per_corpus: std::collections::BTreeMap<String, usize> = {
            let mut m: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for c in &chunks {
                *m.entry(c.corpus_id.clone()).or_insert(0) += 1;
            }
            m
        };
        tracing::info!(
            chunks_found = chunks.len(),
            search_ms,
            per_corpus = ?post_truncate_per_corpus,
            "handle_knowledge_query: corpus search done (per-corpus survivors)"
        );


        // 4a. Empty results path — answer from parametric knowledge.
        //
        // v29 attempted to gate this on (entities non-empty +
        // meta_atlas empty) to emit refusal prose for fabricated-
        // entity questions. The behavior change is correct (don't
        // hallucinate biographies of made-up people) but the bench
        // can't measure it — the model's refusal vocab ("not
        // certain", "no record") doesn't always align with the
        // bench's expected_facts list ("no information", "couldn't
        // find"). Reverted to the simpler general-knowledge prompt
        // until bench fixtures are updated to a broader refusal-
        // vocabulary expected set.
        if chunks.is_empty() {
            tracing::info!("KnowledgeQuery: no chunks — answering from parametric knowledge");
            let corpora = context.installed_corpora_display();
            let prompt = format!(
                "The user asked: \"{message}\"\n\n\
                 A search of the installed sources ({corpora}) found nothing \
                 relevant, so you have no evidence from the user's own material. \
                 Your reply already opens by noting that — so continue straight \
                 into the substance and do NOT restate that opening (no echoing \
                 it, no second caveat).\n\n\
                 If the question has a genuine general-knowledge answer (a public \
                 fact, a concept, how something works), give it concisely and \
                 directly.\n\
                 If instead it asks for a specific detail that could only come \
                 from a particular document, dataset, or codebase you don't have, \
                 do NOT invent it — say in one short sentence that you don't have \
                 that material, then offer one concrete next step. Never \
                 fabricate names, numbers, code, identifiers, commands, or URLs.\n\
                 Keep it brief and warm: a few sentences at most, no preamble and \
                 no meta-commentary about source limitations."
            );
            let request = CompletionRequest {
                prompt,
                system_message: None,
                // Intentional pin (not a routing decision): empty
                // retrieval means there is no evidence shape to
                // resolve — this is the 300-token general-knowledge
                // fallback and always belongs on the fast slot.
                preferred_speed: Speed::Fast,
                max_tokens: Some(300),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                model_id: None,
                enable_thinking: None,
                sampling_mode: None,
                // This path is DEFINITIONALLY parametric — zero chunks
                // retrieved — so the provenance caveat is committed
                // structurally, not requested via the prompt above
                // (instruction compliance measured ~60% on the fast
                // slot; this was the holdout bank's whole honesty gap,
                // 0.64 vs a 0.91 counterfactual). The KQ stream spawn
                // emits the prefix as visible text.
                assistant_prefix: Some(
                    crate::runtime::prompts::GK_CAVEAT_PREFIX.to_string(),
                ),
                cmd_prefix: None,
                url_allowlist: None,
                evidence_id_allowlist: None,
                lark_grammar: None,
            };
            return KnowledgeQueryPlan {
                request,
                chunks: Vec::new(),
                gate_entity_anchored: false,
                doc_context: String::new(),
                shape: compute_evidence_shape(&[], message),
                route: SynthesisRoute::FastFocused,
                // Empty retrieval is the strongest case for asking the
                // user to supply something — keep the gap check on.
                gap_check_enabled: true,
                search_ms,
                retrieved_chunks: Vec::new(),
                source_map: HashMap::new(),
                result_quality: "empty",
                // Parametric request — tiny prompt, can't overflow.
                prompt_budget_note: None,
                folder_meta: std::collections::HashMap::new(),
                meta_atlas_hits,
            };
        }

        // 4b. Evidence-shape routing.
        let shape = compute_evidence_shape(&chunks, message);
        // ComparisonQuery is a bounded contrast — pin to FastFocused
        // regardless of evidence shape. The whole point of the split
        // is to keep these off the primary slot; letting the evidence
        // shape escalate to PrimarySynthesis would defeat that.
        //
        // v32 attempted "junk-retrieval escalation": shape.is_off_target()
        // + entities non-empty → force PrimarySynthesis + parametric-
        // authorization system message. Net negative on marathon
        // variance test because the marathon T3 question ("What did
        // she contribute that was genuinely new?") has no extracted
        // proper-noun entity, so the escalation didn't fire on the
        // canonical failure case. The real variance lever is upstream
        // (why don't title-expand'd Lovelace chunks survive merge?),
        // not in synth routing. Reverted.
        // Enumeration turns (atom-enum fired) pin PrimarySynthesis. The
        // directed set is many low-cosine entity chunks, so the evidence
        // shape reads as weak/single-focus and route_from_evidence picks
        // FastFocused — which runs synth on the FAST slot and emits a
        // per-passage "let me scan the documents…" narration that burns
        // the whole token budget before the LIST is ever written (fatal:
        // the counterparty turn rambled through 3 irrelevant passages and
        // never enumerated, despite 16 energy-company chunks pinned in
        // front). Enumeration IS multi-source breadth — pin it to the
        // primary slot so it writes the clean list. Gated: `has_atom_enum`
        // is only true when SOVEREIGN_ATOM_ENUM is on AND a set question
        // fired, so non-enumeration turns and other corpora are untouched.
        let has_atom_enum = chunks.iter().any(|c| {
            c.metadata
                .get("source")
                .map(|s| s == "atom-enum")
                .unwrap_or(false)
        });
        // Single capability decision. The route ladder lives in one place —
        // evidence.rs::resolve_synthesis_route, pure + unit-tested against the
        // legacy truth table — so "why did THIS query hit the fast/primary
        // slot?" is answerable at one site (it was mis-identified three times
        // when this was inlined). `decision.reason` is surfaced in the trace.
        let decision = resolve_synthesis_route(&intent, has_atom_enum, &shape);
        let route = decision.route;
        // MECE operation axis (QUERY_TAXONOMY_MECE.md) — emitted alongside the
        // legacy route for glassbox legibility. Naming-only today: nothing
        // routes on `operation` yet (Step 2 will wire effort → tier); the
        // `route` field remains the load-bearing decision.
        let operation = operation_of(&intent, has_atom_enum);
        tracing::info!(
            count = shape.count,
            top1 = shape.top1_score,
            median = shape.median_score,
            median_ratio = shape.median_ratio,
            top_source_repeat = shape.top_source_repeat_count,
            distinct_sources = shape.distinct_sources,
            title_match = shape.title_match,
            top_source = %shape.top_source_label,
            route = ?route,
            reason = ?decision.reason,
            role = "synthesizer",
            tier = ?decision.tier,
            operation = ?operation,
            "KnowledgeQuery: evidence-shape routing decision"
        );

        // 4c. Cohesion expansion. Two flavors based on retrieval shape:
        //
        //   - **Single-source dominance** (FastFocused route + ≥2
        //     top-source repeats): the question landed clearly on one
        //     document. Pull all chunks from that document by title;
        //     keep 2 grounding chunks for breadth. (`expand_from_dominant_source`)
        //   - **Multi-article synthesis** (PrimarySynthesis route, ≥2
        //     distinct titled sources): the question requires combining
        //     evidence from several articles. Pull
        //     `EXPANSION_MULTI_PER_SOURCE` chunks from each of the top
        //     `EXPANSION_MULTI_SOURCE_GROUPS` source documents. This
        //     directly addresses the chunks_fact_score gap where
        //     retrieval lands on the right articles but only contributes
        //     1-2 chunks per source — synthesis ends up sparse.
        //     (`expand_from_top_sources`)
        //
        // Either expansion path uses the `EXPANDED_KNOWLEDGE_CHARS`
        // budget; the formatter trims to fit if the expanded set is
        // larger than 8000 chars.
        // v23 attempted "title-expand authoritative — skip all
        // expansion". v24 then tried fetch-by-title to compensate.
        // Both regressed fact_recall because auto-expansion was
        // providing useful depth coverage from non-title-expand
        // articles. Restored to v22 behavior: title-expand chunks
        // get reserved (see reserve_chunks_per_entity above), and
        // downstream expanders still fire normally to deepen
        // coverage of the dominant article. The cost is occasional
        // displacement of title-expand chunks (T2/T7/T8 v22) but
        // the net is +19pt fact, +22pt src vs baseline.
        // Which expander to run. Intent-aware: comparisons take breadth,
        // never the single-dominant-source collapse (which would strip a
        // contrast down to one side — see `decide_expansion_strategy`).
        let (expansion_strategy, expansion_reason) =
            decide_expansion_strategy(intent, route, &shape);
        tracing::info!(
            target: "retrieval_audit",
            event = "expansion_decision",
            intent = ?intent,
            route = ?route,
            strategy = ?expansion_strategy,
            reason = expansion_reason,
            top_source_repeat = shape.top_source_repeat_count,
            distinct_sources = shape.distinct_sources,
            "retrieval_audit: expansion_decision"
        );
        let expansion_kind: &'static str;
        let (mut chunks, mut knowledge_char_budget, expansion_fired) = match expansion_strategy {
            ExpansionStrategy::DominantSource => {
                expansion_kind = "dominant_source";
                let (expanded, _from_source, _grounding, _dropped) =
                    self.expand_from_dominant_source(chunks, &shape).await;
                (expanded, EXPANDED_KNOWLEDGE_CHARS, true)
            }
            ExpansionStrategy::TopSources => {
                let (expanded, sources_expanded, _total) =
                    self.expand_from_top_sources(chunks).await;
                // Only count as "fired" when the expander actually pulled
                // from ≥ 2 sources — otherwise we're back to the initial
                // chunk set and the prompt budget should reflect that.
                if sources_expanded >= 2 {
                    expansion_kind = "top_sources";
                    (expanded, EXPANDED_KNOWLEDGE_CHARS, true)
                } else {
                    expansion_kind = "top_sources_noop";
                    (expanded, MAX_KNOWLEDGE_CHARS, false)
                }
            }
            ExpansionStrategy::NoExpansion => {
                expansion_kind = "none";
                (chunks, MAX_KNOWLEDGE_CHARS, false)
            }
        };

        // Pre-flight retrieval-bundle budget. The configured
        // `knowledge_char_budget` (8000 / 16000 chars) is the
        // *upper* bound the formatter respects, but it's blind to the
        // slot's actual context window — see the 2026-05-25 repro
        // where a 39KB synth prompt blew an 8192 ctx primary slot at
        // `clamp_max_tokens` before the formatter's budget even
        // mattered. Compute a ctx-aware ceiling here and pass the
        // tighter of the two to the formatter:
        //
        //   1. `effective_context_size()` — what the slot actually
        //      allocated (post llama-cpp padding). `None` on remote
        //      providers (no local slot to budget against) — fall
        //      through to the formatter's static cap.
        //   2. Reserve `inference_config.max_tokens` for the response.
        //   3. Reserve a `SYSTEM_OVERHEAD_TOKEN_RESERVE` cushion for
        //      `KNOWLEDGE_SYNTHESIS_SYSTEM` + `THINKING_DIRECTIVE` +
        //      epistemic contract + persona + memories + tool dossier.
        //      The persona/memory tail varies by turn; the empirical
        //      max during marathon eval is ~5000 tokens, so 4096 is
        //      a conservative-but-honest cushion that leaves the
        //      synthesis prompt the bulk of the window.
        //   4. Convert the remaining token budget to chars via the
        //      project's standard ~4 chars/token heuristic.
        //
        // When `effective_context_size()` returns `None` (remote
        // provider, deterministic test stubs) or the ctx-aware budget
        // is at least as generous as the static cap, the formatter
        // sees `knowledge_char_budget` unchanged — pre-fix behaviour.
        const SYSTEM_OVERHEAD_TOKEN_RESERVE: u32 = 4096;
        const CHARS_PER_TOKEN: u32 = 4;
        let original_budget = knowledge_char_budget;
        if let Some(n_ctx) = self.inference.effective_context_size() {
            let reserved_output = self.inference_config.max_tokens as u32;
            // Phase 3 (budget-sensor redesign): when the assembly memo
            // has last turn's REAL system-message size for this
            // conversation, use it instead of the static cushion —
            // the 4096 guess under-reserves on long threads (history
            // + memories ride the system message and grow per turn)
            // and over-reserves on fresh ones. Small pad on top for
            // turn-over-turn growth; static cushion remains the
            // first-turn fallback.
            let system_overhead = self
                .last_assembly(&context.conversation.id)
                .map(|m| m.system_tokens.saturating_add(256))
                .unwrap_or(SYSTEM_OVERHEAD_TOKEN_RESERVE);
            let available_tokens = n_ctx
                .saturating_sub(reserved_output)
                .saturating_sub(system_overhead);
            let available_chars = available_tokens.saturating_mul(CHARS_PER_TOKEN) as usize;
            if available_chars < knowledge_char_budget {
                tracing::info!(
                    n_ctx,
                    reserved_output,
                    system_overhead_reserve = system_overhead,
                    chunk_count = chunks.len(),
                    static_budget = knowledge_char_budget,
                    ctx_aware_budget = available_chars,
                    "trim_bundle_to_budget: ctx-aware retrieval budget tighter than static cap — formatter will drop lowest-score chunks until under budget"
                );
                knowledge_char_budget = available_chars;
            }
        }
        let _ = original_budget; // silence unused warning when ctx-aware branch never fires

        // Naturalistic audit — post-expansion composition. After the
        // dominant-source or top-sources expander has had its say,
        // what's actually in the prompt? T11 (marathon) showed 12
        // Atanasoff-Berry chunks at this layer despite a 10-cap at
        // merge — that's `expand_from_dominant_source` honestly doing
        // its job after a *wrong* article won evidence-shape dominance.
        // The fix lives in shape signals, not the expander, so this
        // log lets us see exactly when the wrong dominant survives.
        // (Glassbox: paired with graph-walk + post-truncate traces
        // upstream per ARCH §0.1; drops inside the expander surface
        // here as a delta against the merge-cap total.)
        {
            use std::collections::HashMap;
            let mut by_corpus: HashMap<String, usize> = HashMap::new();
            let mut by_article: HashMap<(String, String), usize> = HashMap::new();
            for c in &chunks {
                *by_corpus.entry(c.corpus_id.clone()).or_insert(0) += 1;
                *by_article
                    .entry((c.corpus_id.clone(), c.title.clone().unwrap_or_default()))
                    .or_insert(0) += 1;
            }
            let mut corpus_pairs: Vec<(String, usize)> = by_corpus.into_iter().collect();
            corpus_pairs.sort_by(|a, b| b.1.cmp(&a.1));
            let mut article_pairs: Vec<((String, String), usize)> =
                by_article.into_iter().collect();
            article_pairs.sort_by(|a, b| b.1.cmp(&a.1));
            let article_top: Vec<(String, String, usize)> = article_pairs
                .into_iter()
                .take(5)
                .map(|((cid, t), n)| (cid, t, n))
                .collect();
            tracing::info!(
                target: "retrieval_audit",
                event = "post_expansion",
                kind = expansion_kind,
                fired = expansion_fired,
                single_source_expansion = expansion_kind == "dominant_source",
                reason = expansion_reason,
                total = chunks.len(),
                by_corpus = ?corpus_pairs,
                top5_articles = ?article_top,
                "retrieval_audit: post_expansion"
            );
        }

        // 4d. Build prompt. Retrieved content first, question last —
        // keeps the model from reasoning purely from training weights
        // during its <think> phase (when Primary path is taken).
        //
        // Build a `corpus_id → CorpusKind` map so catalog hits route
        // into a separate evidence tier — the synthesis prompt
        // (`KNOWLEDGE_SYNTHESIS_SYSTEM`) has dedicated guidance for
        // them. Best-effort: if `installed_indexes()` errors we fall
        // back to no-kinds formatting (pre-catalog behaviour).
        let (kinds, display_categories): (
            std::collections::HashMap<String, corpus_engine::CorpusKind>,
            std::collections::HashMap<String, String>,
        ) = if let Some(engine) = &self.corpus_engine {
            let mut kinds_map = std::collections::HashMap::new();
            let mut display_map = std::collections::HashMap::new();
            for info in engine.installed_indexes().await.unwrap_or_default() {
                if let Some(d) = &info.display {
                    if let Some(cat) = &d.category {
                        display_map.insert(info.corpus_id.clone(), cat.clone());
                    }
                }
                kinds_map.insert(info.corpus_id, info.kind);
            }
            (kinds_map, display_map)
        } else {
            Default::default()
        };
        // Surface Wikipedia editors' POV/controversy flags as
        // `(contested)` markers on the source label. Best-effort:
        // graph absent → empty set → no markers, behaviour
        // unchanged.
        let contested_titles: std::collections::HashSet<String> =
            self.contested_titles_for_chunks(&chunks).await;
        let folder_meta = self.folder_metadata_snapshot().await;
        self.rerank_conv_chunks_via_ppr(message, &mut chunks, &display_categories)
            .await;
        // Late RAPTOR injection (SOVEREIGN_RAPTOR_LATE): inject summaries AFTER
        // the full leaf pipeline (reweight → … → ppr-rerank) so they cannot
        // perturb leaf retrieval/ranking — QA-neutral by construction.
        // Appended at the END (not reserved to front) so the prompt char budget
        // serves leaf chunks first; the summaries fill remaining budget, which
        // DeepQuery's larger budget admits (where summary intent lives). Reuses
        // the same `embedding` as the early path so the A/B isolates TIMING.
        if raptor_late_inject_enabled() {
            self.apply_raptor_grounding(
                &embedding,
                &mut chunks,
                "KnowledgeQuery",
                context.conversation.enabled_corpora.as_deref(),
            )
            .await;
        }
        // 4d-agentic. Bounded agentic evidence loop (prototype, env-gated
        // SOVEREIGN_AGENTIC_KQ=1). When the evidence fails a fast
        // forced-choice sufficiency check, the model formulates 1-3
        // world-grounded queries and the SAME kq_pipeline runs per
        // query; new chunks are deduped, hard-sealed to the
        // conversation's corpora, and appended. Placed HERE — after the
        // dominance-expansion and rerank — so round-2 evidence feeds
        // the prompt and metadata directly (the v1 prototype sat
        // upstream and the expansion rebuilt the set without it).
        // Contested-markers above are computed from round-0 chunks
        // only; acceptable for the prototype. Degrades to a no-op on
        // any judge or formulation failure. See runtime/evidence_loop.rs.
        let mut agentic_still_insufficient = false;
        // `agentic_entity_anchored` tracks whether the agentic LOOP ran a real
        // round and found the question in-world — it stays false when the loop
        // degrades/skips, which is correct for the `:826` "a second pass already
        // ran" note. The GATE's entity-anchored verdict is computed separately
        // (deterministically) at plan construction, so it does NOT inherit this
        // variable's loop-success semantics. See the construction below.
        let mut agentic_entity_anchored = false;
        let mut agentic_corpus_anchored = true;
        if crate::runtime::evidence_loop::agentic_kq_enabled() {
            let (merged, still_insufficient, entity_anchored, corpus_anchored) = self
                .agentic_evidence_round(message, chunks, context, intent, scope)
                .await;
            chunks = merged;
            agentic_still_insufficient = still_insufficient;
            agentic_entity_anchored = entity_anchored;
            agentic_corpus_anchored = corpus_anchored;
        }
        let conv_briefing = self
            .build_conv_briefing_block(&chunks, &display_categories)
            .await;
        let doc_context = format_scored_chunks_with_kinds(
            &chunks,
            knowledge_char_budget,
            Some(&kinds),
            if contested_titles.is_empty() {
                None
            } else {
                Some(&contested_titles)
            },
            if folder_meta.is_empty() {
                None
            } else {
                Some(&folder_meta)
            },
            if display_categories.is_empty() {
                None
            } else {
                Some(&display_categories)
            },
        );
        let knowledge_block = if conv_briefing.is_empty() {
            doc_context.clone()
        } else {
            format!("{conv_briefing}\n{doc_context}")
        };
        let corpus_display = context.installed_corpora_display();
        let prompt = format!(
            "RETRIEVED FROM {corpus_display}:\n\n{knowledge_block}\n\n\
             ════════════════════════════════════\n\n\
             Question: {message}"
        );

        // 4e. Request shape varies by route.
        // Folder-ingest v1 §6.3: a "what I don't have" note appended
        // to the synthesis system message when matched folder
        // corpora carry non-zero failed/skipped counts. Empty
        // string when there's nothing to disclose, so the prompt
        // overhead is zero in the common case.
        let mut gap_note = build_coverage_gaps_note(&chunks, &folder_meta);
        if agentic_still_insufficient {
            // The agentic loop fired, ran its targeted second retrieval
            // pass, and the evidence STILL fails the sufficiency judge.
            // Tell the synthesis model — a model that knows the search
            // already came back empty abstains; one that doesn't treats
            // the near-miss pile as license to answer (measured
            // 2026-06-11: 3 absent-question abstentions became
            // confident fabrications without this note).
            //
            // Two strengths. For in-world (entity-anchored) questions
            // the "general knowledge" escape is closed outright:
            // outside knowledge structurally cannot supply facts about
            // the corpus's own world, and the measured failure mode is
            // confabulation wearing the GK-caveat format ("from
            // general knowledge: The Professor's real name is Dr.
            // Verloc"). World-general questions keep the GK path —
            // caveated parametric answers there (capital of Australia)
            // are the desired behavior.
            if agentic_entity_anchored {
                gap_note.push_str(
                    "\n\nEVIDENCE CHECK: a targeted second retrieval pass already \
                     ran for this question and did not surface decisive evidence. \
                     This question asks about the world inside your sources; \
                     outside 'general knowledge' cannot supply facts about it. If \
                     the passages do not directly state the asked-for fact, answer \
                     that the sources do not state it — never substitute a guess \
                     or an outside-knowledge claim.",
                );
            } else {
                gap_note.push_str(
                    "\n\nEVIDENCE CHECK: a targeted second retrieval pass already \
                     ran for this question and did not surface decisive evidence. \
                     If the passages do not directly state the asked-for fact, say \
                     plainly that the available sources do not contain it — do not \
                     bridge the gap with a confident guess.",
                );
            }
        }
        // The slot is THE route's slot — both arms below consume this
        // instead of re-stating a Speed literal that could drift from
        // the arm it sits in.
        let route_speed = route.to_speed();
        let mut request = match route {
            SynthesisRoute::FastFocused => {
                // Comparison-shape contrast — append the directive that
                // pins the model to a bounded axes structure rather
                // than the open-ended essay shape.
                let budget_note = crate::runtime::build_response_length_directive(
                    FAST_KNOWLEDGE_MAX_TOKENS as usize,
                );
                // Synthesizer role builds the prompt body (SSOT). FastFocused
                // forces think_budget=0 → no THINKING_DIRECTIVE. The
                // Comparison-shape directive pins the bounded-axes structure.
                // provenance_emphasis: the no-thinking fast slot skips the
                // mid-prompt provenance rule — restate it end-positioned
                // (see PROVENANCE_DIRECTIVE).
                let base = crate::runtime::build_synthesis_system_prompt_with_provenance(
                    matches!(intent, Intent::ComparisonQuery),
                    &gap_note,
                    false,
                    &budget_note,
                    true,
                );
                let system = self.build_system_message(&base, context);
                CompletionRequest {
                    prompt,
                    system_message: Some(system),
                    preferred_speed: route_speed,
                    max_tokens: Some(FAST_KNOWLEDGE_MAX_TOKENS as usize),
                    temperature: Some(self.inference_config.temperature),
                    think_budget: Some(0),
                    structured_output: None,
                    top_k: self.inference_config.top_k,
                    top_p: None,
                    // oicp=None lets the wire layer auto-derive
                    // latency_class=Fast from preferred_speed (per the
                    // OICP-native fast-slot routing landed in v19).
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
                }
            }
            SynthesisRoute::PrimarySynthesis => {
                // Give synthesis a generous output budget so a thorough answer
                // COMPLETES instead of truncating mid-sentence at the general
                // cap (finish_reason=Length, released as-is — see synth.truncation
                // telemetry). The enforce() ladder protects the output reservation
                // by trimming evidence FIRST, so a larger reservation just gives
                // the answer the room — the 16k context easily holds it. A
                // truncated answer is wrong regardless of grounding; the length
                // directive still steers conciseness. Env-tunable for calibration
                // against the truncation telemetry, default 4096.
                let synth_max = self.inference_config.max_tokens.max(
                    std::env::var("SOVEREIGN_SYNTHESIS_OUTPUT_FLOOR")
                        .ok()
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(4096),
                );
                let budget_note = crate::runtime::build_response_length_directive(synth_max);
                // Synthesizer role builds the prompt body (SSOT). THINKING_DIRECTIVE
                // is a `<think>`-block contract — include it only when a think
                // budget is allocated. Comparison routes to FastFocused, never here.
                let base = crate::runtime::build_synthesis_system_prompt(
                    false,
                    &gap_note,
                    self.inference_config.think_budget > 0,
                    &budget_note,
                );
                let system = self.build_primary_system_message(&base, context);
                CompletionRequest {
                    prompt,
                    system_message: Some(system),
                    preferred_speed: route_speed,
                    max_tokens: Some(synth_max),
                    temperature: Some(self.inference_config.temperature),
                    think_budget: Some(self.inference_config.think_budget),
                    structured_output: None,
                    top_k: self.inference_config.top_k,
                    top_p: None,
                    oicp: self.build_oicp(&Intent::KnowledgeQuery),
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
                }
            }
        };

        // Structural general-knowledge caveat. When the agentic loop
        // judged the evidence insufficient after its second retrieval
        // round AND the question shares no vocabulary with any enabled
        // corpus's atlas (topically foreign — "capital of Canada"
        // against a novel), whatever the model answers is parametric
        // memory and must say so. The caveat is COMMITTED via
        // assistant_prefix — instruction-based caveats measured ~60%
        // compliance on the 4B (3/5 OOD omissions on the 2026-06-11
        // holdout run = honesty 0.64 vs 0.91). The streaming layer
        // emits the same prefix as visible text.
        if agentic_still_insufficient && !agentic_corpus_anchored {
            request.assistant_prefix =
                Some(crate::runtime::prompts::GK_CAVEAT_PREFIX.to_string());
            tracing::info!(
                target: "agentic_kq",
                "agentic_kq: foreign-topic insufficiency — GK caveat prefix committed"
            );
        }

        // Phase-1 prompt-budget guard: assembled input + response
        // reservation must fit the window — the engine's "Prompt too
        // long" rejection is a terminal, user-facing error loop
        // (note 2cd9227e). Degradation ladder in `prompt_budget`;
        // the note rides the plan into message metadata.
        let prompt_budget_note = match self.inference.effective_context_size() {
            Some(ctx) => {
                let (outcome, measured) = crate::runtime::prompt_budget::enforce(
                    &mut request,
                    &|s| self.inference.count_tokens(s),
                    ctx,
                );
                // Phase 2: record pre-trim DEMAND for the compaction
                // sensor + next-turn allocator.
                self.record_assembly(&context.conversation.id, measured);
                match outcome {
                    crate::runtime::prompt_budget::BudgetOutcome::Trimmed { note } => Some(note),
                    _ => None,
                }
            }
            None => None,
        };

        // 4f. Build retrieved_chunks summaries for the UI citation
        // expander. Same shape `prepare_knowledge_context` produces so
        // the frontend renders both paths identically.
        let retrieved_chunks = project_retrieved_chunks(&chunks);

        let mut source_map: HashMap<String, usize> = HashMap::new();
        for c in &chunks {
            *source_map.entry(c.corpus_id.clone()).or_insert(0) += 1;
        }

        // Gap check ALWAYS fires on the KnowledgeQuery path.
        //
        // Humility principle: the corpus is the source of truth and
        // we have to query it before deciding whether external
        // lookup is warranted — but we also have to honestly check
        // whether the answer we synthesised actually addresses the
        // question. Retrieval shape (FastFocused vs PrimarySynthesis)
        // is a SYNTHESIS-routing decision, not an answer-quality
        // signal. The 2026-05-19 M5-Mac-Studio failure pinned this:
        // 12 chunks clustered tightly on `wikipedia::Mac (computer)`
        // because of title overlap with "Mac Studio", route was
        // FastFocused, gap check was skipped — and the model
        // produced "no reliable info" with no INFORMATION REQUEST
        // surfaced. The gap check is the LLM-based judge of "is
        // this answer grounded in the evidence?"; it has to run
        // regardless of how concentrated the evidence looked.
        //
        // Cost: one small-LLM call (~1s) per FastFocused turn,
        // post-stream so user-visible latency is unaffected.
        let gap_check_enabled = true;

        let result_quality = if expansion_fired {
            "focused"
        } else if matches!(route, SynthesisRoute::PrimarySynthesis) {
            "synthesis"
        } else {
            "routed"
        };

        let _ = expansion_fired; // logged by expand_from_dominant_source already

        let folder_meta = self.folder_metadata_snapshot().await;

        // Naturalistic audit — turn_summary. One structured line per
        // synthesis turn so a grep on `retrieval_audit` reconstructs
        // the full story: query, topic anchor, hot-corpora histogram,
        // entities extracted, evidence-shape decision, expansion kind,
        // and the final per-corpus + per-article composition that the
        // synthesizer will see. Pairs with the `corpus_results`,
        // `post_merge`, and `post_expansion` events emitted earlier
        // for the same turn — match by query preview or by event order.
        {
            use std::collections::HashMap;
            let mut by_corpus: HashMap<String, usize> = HashMap::new();
            for c in &chunks {
                *by_corpus.entry(c.corpus_id.clone()).or_insert(0) += 1;
            }
            let mut corpus_pairs: Vec<(String, usize)> = by_corpus.into_iter().collect();
            corpus_pairs.sort_by(|a, b| b.1.cmp(&a.1));
            let hot_pairs: Vec<(String, usize)> = {
                let mut v: Vec<(String, usize)> =
                    hot_corpora.iter().map(|(k, v)| (k.clone(), *v)).collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                v
            };
            tracing::info!(
                target: "retrieval_audit",
                event = "turn_summary",
                intent = ?intent,
                route = ?route,
                query = %query_preview_for_audit,
                expanded_query = %retrieval_query_preview_for_audit,
                topic = ?topic_for_audit,
                prior_messages = prior_messages_for_audit,
                hot_corpora = ?hot_pairs,
                entities = ?entities,
                meta_atlas_hits = meta_atlas_hits.len(),
                expansion_kind,
                expansion_fired,
                final_chunks = chunks.len(),
                final_by_corpus = ?corpus_pairs,
                top_source = %shape.top_source_label,
                top_source_repeat = shape.top_source_repeat_count,
                distinct_sources = shape.distinct_sources,
                title_match = shape.title_match,
                top1 = shape.top1_score,
                median = shape.median_score,
                "retrieval_audit: turn_summary"
            );
        }

        crate::runtime::grounding::dbg(&format!(
            "[KQDIAG] plan build: agentic_entity_anchored={agentic_entity_anchored}"
        ));
        // The gate's entity-anchored verdict is computed DETERMINISTICALLY here
        // (question keywords vs the corpus gazetteer + deictic), NOT taken from
        // `agentic_entity_anchored`. That variable reflects whether the agentic
        // LOOP ran, and every degrade/skip/sufficient early return in
        // `agentic_evidence_round` hardcodes it false — so sourcing the gate from
        // it left the GK-caveat exemption OPEN on the fast streaming/desktop
        // route (and whenever the sufficiency judge failed), releasing "from
        // general knowledge: …" fabrications about corpus entities unverified.
        // Entity-anchoring is a property of the question + corpus, independent of
        // the loop's success; deictic questions close the exemption the same way.
        // Computed before the struct so the `&chunks` borrow ends before the move.
        let gate_entity_anchored = crate::runtime::evidence_loop::compute_entity_anchored(
            message,
            context.conversation.enabled_corpora.as_deref(),
            &chunks,
        ) || crate::runtime::evidence_loop::question_is_corpus_deictic(message);
        crate::runtime::grounding::dbg(&format!(
            "[KQDIAG] gate_entity_anchored(deterministic)={gate_entity_anchored} loop_value={agentic_entity_anchored}"
        ));
        KnowledgeQueryPlan {
            request,
            chunks,
            gate_entity_anchored,
            doc_context,
            shape,
            route,
            gap_check_enabled,
            search_ms,
            retrieved_chunks,
            source_map,
            result_quality,
            prompt_budget_note,
            folder_meta,
            meta_atlas_hits,
        }
    }

    /// Handle KnowledgeQuery (and ComparisonQuery): search corpus-engine
    /// LanceDB indexes → inject into prompt → synthesize. The intent
    /// pins the plan's synthesis route — ComparisonQuery always rides
    /// FastFocused regardless of evidence shape.
    pub(crate) async fn handle_knowledge_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        intent: &Intent,
        coarse_intent: Option<String>,
        self_assessment: Option<String>,
        routing_trigger: Option<String>,
    ) -> Result<Response> {
        let plan = self
            .prepare_knowledge_query_plan(message, context, intent, None)
            .await;

        // PR5 — non-streaming retrieval-miss diversion. Mirrors the
        // streaming path: dispersed noise → suppress synthesis +
        // surface clarification instead of confabulating.
        if plan.shape.is_off_target() {
            let session_id = self
                .sessions
                .latest_for_conversation(conversation_id)
                .map(|s| s.id)
                .unwrap_or_default();
            // Post-classification site: intent IS in hand here, so
            // we apply full intent-keyed narrowing. The
            // retrieval-miss handler renders an INFORMATION
            // REQUEST card; surfacing the intent-appropriate tool
            // catalog lets the model offer accurate "next-step"
            // affordances to the user.
            let tool_descriptors = self.narrow_tools_for_intent(intent);
            tracing::info!(
                distinct_sources = plan.shape.distinct_sources,
                retrieval_count = plan.shape.count,
                "routing:retrieval_miss — non-streaming diversion"
            );
            return self
                .handle_retrieval_miss_response(
                    message,
                    conversation_id,
                    &session_id,
                    &plan.shape,
                    &tool_descriptors,
                )
                .await;
        }

        let completion = self.inference.complete(&plan.request).await?;

        // Production grounding gate — non-streaming sibling of the
        // held-stream gate in streaming.rs; same shared ladder
        // (grounding::gate_answer: single-claim verify→retry→abstain
        // for short answers, per-claim audit→rewrite→annotate for
        // long-form). No hold needed here: nothing was sent yet.
        let mut grounding_gate_meta: Option<serde_json::Value> = None;
        // Domain-managed corpora (governance, proxy-voting) take their own
        // calibrated gate surface; else the general KnowledgeQuery gate.
        let gate_surface =
            self.kq_gate_surface(context.conversation.enabled_corpora.as_deref());
        let completion_text = if gate_surface.enabled() && !plan.chunks.is_empty() {
            // The turn's sealed evidence universe; claim search
            // sealed to the conversation's corpora.
            let gate_evidence = crate::runtime::grounding::EvidenceContext {
                chunks: crate::runtime::grounding::gate_evidence_chunks(&plan.chunks),
                searcher: Some(std::sync::Arc::new(self.claim_searcher(
                    context.conversation.enabled_corpora.as_deref(),
                    &plan.chunks,
                )) as _),
                entity_anchored: plan.gate_entity_anchored,
                top_similarity: None,
            };
            let outcome = crate::runtime::grounding::gate_answer(
                &self.inference,
                message,
                completion.text.clone(),
                &gate_evidence,
                &plan.request,
                &gate_surface.profile(),
            )
            .await;
            grounding_gate_meta = Some(outcome.meta);
            outcome.text
        } else {
            completion.text.clone()
        };

        let final_content = if plan.gap_check_enabled {
            // Humility principle: always run the gap check on KQ
            // paths. The retrieval-shape route is a synthesis-
            // routing decision, not an answer-quality signal. See
            // the matching block in the streaming KQ path + the
            // long-form note at `prepare_knowledge_query_plan`.
            tracing::debug!(
                route = ?plan.route,
                top_source = %plan.shape.top_source_label,
                "KnowledgeQuery: running gap check"
            );
            self.maybe_collaborate(
                conversation_id,
                message,
                &completion_text,
                &plan.doc_context,
            )
            .await
        } else {
            completion_text.clone()
        };

        // Post-synthesis guardrail: demote any quoted span that isn't
        // verbatim-present in the evidence we showed the model, so a
        // composite / fabricated quotation can't reach the user framed
        // as a real one. Runs after the gap check so it also covers a
        // refined answer. Empty doc_context (parametric path) is a
        // no-op — nothing to verify against. See
        // `quote_verification::verify_answer_against_evidence`.
        let final_content = {
            let v = crate::quote_verification::verify_answer_against_evidence(
                &final_content,
                &plan.doc_context,
            );
            if v.demoted_count > 0 {
                tracing::warn!(
                    demoted = v.demoted_count,
                    verified = v.verified_count,
                    "knowledge_query: post-synthesis guardrail demoted unverified quotations"
                );
            }
            v.rewritten
        };

        let (sources_for_prov, coverage_for_prov) = build_provenance_components(
            &plan.source_map,
            &std::collections::HashMap::new(),
            &plan.folder_meta,
            // KnowledgeQueryPlan doesn't yet carry a display-category
            // lookup. See the matching note in the streaming path
            // above — DeepQuery is where the conversation-label
            // rename fires for v1.
            None,
        );
        let provenance = ResponseProvenance {
            intent: "KnowledgeQuery".to_string(),
            search_method: Some("CorpusEngine".to_string()),
            sources: sources_for_prov,
            inference_backend: completion.model_id.clone(),
            oicp_match: completion
                .oicp_meta
                .as_ref()
                .and_then(|m| m.match_quality.as_ref())
                .map(|q| format!("{q:?}")),
            total_latency_ms: completion.latency_ms,
            tokens_used: completion.tokens_used,
            coarse_intent,
            self_assessment,
            routing_trigger,
            coverage: coverage_for_prov,
            // Non-streaming KQ path: CompletionResponse now carries
            // the provider-observed finish_reason + completion_tokens
            // (Phase 1 of the cutoff-legibility plan). Read from the
            // response so the desktop chip lights up on length
            // truncation here too, not just on streaming surfaces.
            finish_reason: completion.finish_reason.clone(),
            max_tokens_budget: Some(self.inference_config.max_tokens),
            completion_tokens: completion.completion_tokens,
            // Ctx-budget glassbox — paired with `tokens_used` so the
            // desktop chat bubble can render `N / M (X%)` and brighten
            // as the cap approaches. `None` on remote-only providers
            // (no local slot to read from).
            context_window: self.inference.effective_context_size(),
        };

        // PR3 — grounded next-step offers. Look up the most recent
        // session for this conversation (handle_turn created one
        // right before dispatching here); fall back to a synthetic
        // id when the session isn't present (e.g. legacy test
        // harnesses that don't wire the session store).
        let session_id = self
            .sessions
            .latest_for_conversation(conversation_id)
            .map(|s| s.id)
            .unwrap_or_default();
        let had_dominant_source = plan.shape.top_source_repeat_count >= 2;
        let retrieval_missed = plan.shape.is_off_target();
        let top_source_title = if plan.shape.top_source_key.1.is_empty() {
            None
        } else {
            Some(plan.shape.top_source_key.1.clone())
        };
        let offers = build_next_step_offers(&OfferContext {
            user_message: message,
            top_source_title: top_source_title.as_deref(),
            had_dominant_source,
            retrieved_chunks: &plan.retrieved_chunks,
            session_id: &session_id,
            retrieval_missed,
        });
        let offers_json = serde_json::to_value(&offers).unwrap_or_default();

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: final_content,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "model": completion.model_id,
                "tokens": completion.tokens_used,
                "latency_ms": completion.latency_ms,
                "intent": "knowledge_query",
                "documents_found": plan.chunks.len(),
                "search_ms": plan.search_ms,
                "result_quality": plan.result_quality,
                "provenance": provenance,
                "retrieved_chunks": plan.retrieved_chunks,
                "prompt_budget": plan.prompt_budget_note,
                "next_steps": offers_json,
                "grounding_gate": grounding_gate_meta,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;
        self.spawn_auto_title(conversation_id);

        Ok(Response {
            message: assistant_msg,
            task: None,
            metrics: None,
        })
    }
}
