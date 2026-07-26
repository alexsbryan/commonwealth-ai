// SPDX-License-Identifier: AGPL-3.0-or-later
//! Retrieval pipeline — chunk-fetch, atlas grounding, source
//! expansion, conversation-tiered briefing, and the `prepare_knowledge_context`
//! orchestrator that drives Runtime's synthesis paths.
//!
//! Everything in this module is `impl Runtime` — the methods access
//! Runtime's engine handles (`corpus_engine`, `wikipedia_graph`,
//! `meta_atlas`, `atlas_context_provider`, `conv_tiered_reader`,
//! `landscape_digests`, etc.) — so the natural split is by concern
//! (retrieval vs. system message vs. handler) within the same struct,
//! not by struct boundary.
//!
//! Split by concern into submodules (2026-07, pure move — every method is
//! still `impl Runtime`): see the `mod` declarations below. This file keeps
//! the orchestrating entry points (`prepare_knowledge_context`,
//! `retrieve_candidates`) and the small snapshot helpers they share.

mod atlas_grounding;
mod atom_enum;
mod boosts;
mod conv_tiered;
mod corpus_search;
mod history;
pub(crate) mod query_expansion;
mod raptor_grounding;
mod source_expansion;
mod turn_prepass;

use std::collections::HashMap;

use super::*;

use self::corpus_search::corpora_outside_seal;

impl Runtime {
    /// Snapshot the folder-metadata oracle. Returns an empty map
    /// when no oracle is wired (CLI fallback / tests), which makes
    /// every callee's `folder_metadata` lookup miss and so the
    /// pre-Phase-F label rendering applies. Folder-ingest v1 §6.3.
    pub(crate) async fn folder_metadata_snapshot(
        &self,
    ) -> std::collections::HashMap<String, crate::traits::FolderMetadata> {
        match &self.folder_metadata {
            Some(oracle) => oracle.folder_metadata().await,
            None => std::collections::HashMap::new(),
        }
    }
    /// Build the set of chunk titles whose Wikipedia source has at
    /// least one section flagged contested (`pov_count > 0` OR
    /// `section_type = "controversy"`). Used by
    /// `format_scored_chunks_with_kinds` to suffix `(contested)` on
    /// source labels. Returns an empty set when no graph is loaded —
    /// callers degrade gracefully.
    pub(crate) async fn contested_titles_for_chunks(
        &self,
        chunks: &[corpus_engine::ScoredChunk],
    ) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        let Some(graph) = self.wikipedia_graph.as_ref() else {
            return out;
        };
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for c in chunks {
            let Some(title) = c.title.clone() else {
                continue;
            };
            if !seen.insert(title.clone()) {
                continue;
            }
            if graph.has_contested_section(&title).await {
                out.insert(title);
            }
        }
        out
    }
    /// Retrieve the candidate chunk set the team-pipeline Curator
    /// will reduce — local + mesh search, atlas grounding, entity
    /// boost, optional decomposition, dedupe, reweight, multi-source
    /// expansion. Returns just the chunks; callers that also need
    /// provenance (search-method label, per-corpus source counts,
    /// peer attribution) currently re-call `prepare_knowledge_context`
    /// for the formatted shape.
    ///
    /// This is the Phase 2.5 seam from the situated-team plan
    /// (`/Users/user/.claude/plans/there-s-a-fast-slot-delightful-peach.md`).
    /// Implementation today is the minimal wrapper — runs the
    /// existing `prepare_knowledge_context` pipeline and discards
    /// the formatter output. Phase 4 wires this directly into
    /// `run_team_pipeline` and at that point the wrapper gets
    /// expanded into a real split so the wasted formatting work on
    /// the team-pipeline path is paid only once.
    pub(crate) async fn retrieve_candidates(
        &self,
        message: &str,
        context: &ConversationContext,
        intent: &Intent,
    ) -> Vec<corpus_engine::ScoredChunk> {
        let kc = self
            .prepare_knowledge_context(message, context, intent, None)
            .await;
        kc.chunks
    }
    /// Search all knowledge sources, build the prompt with retrieved context,
    /// and assemble provenance metadata. Shared between the streaming and
    /// non-streaming response paths so they cannot diverge.
    pub(crate) async fn prepare_knowledge_context(
        &self,
        message: &str,
        context: &ConversationContext,
        intent: &Intent,
        scope: Option<&str>,
    ) -> KnowledgeContext {
        // Document-attached messages are detected by the
        // `[Document attached: filename]` prefix. We only need to
        // know whether one is attached — the actual document
        // chunking has been moved to `DocumentOperationTool`
        // (routed via ComplexTask), so the parsed-out filename and
        // query text aren't consumed here. We still detect the
        // prefix to short-circuit the embed/search path; without
        // this, a stray document-attached message would burn an
        // embed call producing useless context.
        let attached_source: Option<String> = message
            .strip_prefix("[Document attached: ")
            .and_then(|rest| rest.find(']').map(|end| rest[..end].to_string()));

        // Run the DeepQuery retrieval pipeline — the ordered, traced
        // step list in `retrieval_pipeline::deep_pipeline()`: the shared
        // evidence-gathering head (local ∥ mesh retrieval → scope filter
        // → store search) → the shared core (boosts, expansions, noise
        // floor, grounding, merge) → the deep tail (truncate +
        // strategy-driven top-sources expansion). The per-step trace
        // rides the `retrieval.pipeline` target. Step ORDER is
        // bench-tuned data — pinned by golden tests in
        // retrieval_pipeline.rs.
        //
        // Document-attached turns short-circuit the corpus/mesh/atlas/
        // raptor/store steps (they're routed to ComplexTask and should
        // never reach this path) but keep the historical control flow
        // of running the entity/merge tail on the empty pool.
        let mut pipeline_state = PipelineState::new(
            message,
            context,
            intent,
            scope,
            Vec::new(),
            "DeepQuery",
            format!("{intent:?}"),
        );
        if attached_source.is_some() {
            tracing::debug!("prepare_knowledge_context called with attached document — skipping (should be ComplexTask)");
        } else {
            let retrieval_query = build_retrieval_query(message, context);
            if retrieval_query != message {
                tracing::debug!(
                    bare_chars = message.len(),
                    expanded_chars = retrieval_query.len(),
                    "retrieval: expanded follow-up query with prior user turns"
                );
            }
            pipeline_state.embedding = self
                .inference
                .embed_query(&retrieval_query)
                .await
                .unwrap_or_default();
        }
        deep_pipeline(attached_source.is_none())
            .run(self, &mut pipeline_state)
            .await;
        let PipelineState {
            chunks: mut all_chunks,
            peer_attribution,
            local_hits,
            ..
        } = pipeline_state;

        // Count mesh hits that survived dedupe so the search_method
        // label reflects what's actually in the prompt.
        let mesh_hits: usize = all_chunks
            .iter()
            .filter(|c| peer_attribution.contains_key(&c.corpus_id))
            .count();

        // 4. Provenance metadata.
        let installed_corpora = self.store.list_corpus_states().await.unwrap_or_default();
        let corpora_searched = !installed_corpora.is_empty() || self.corpus_engine.is_some();

        // Compose a human-readable label that describes *where* the
        // hits came from. This replaces the old hardcoded "LocalOnly"
        // string — the UI surface is unchanged (still a string in
        // `provenance.search_method`), but the content is now
        // truthful.
        let search_method = if all_chunks.is_empty() {
            if self.mesh_knowledge.is_some() {
                if corpora_searched {
                    Some("LocalAndMesh (no matches)".to_string())
                } else {
                    Some("Mesh (no matches)".to_string())
                }
            } else if corpora_searched {
                Some("LocalOnly (no matches)".to_string())
            } else {
                None
            }
        } else if mesh_hits > 0 && local_hits > 0 {
            Some("LocalAndMesh".to_string())
        } else if mesh_hits > 0 {
            Some("MeshOnly".to_string())
        } else {
            Some("LocalOnly".to_string())
        };

        let mut source_map: HashMap<String, usize> = HashMap::new();
        for c in &all_chunks {
            *source_map.entry(c.corpus_id.clone()).or_insert(0) += 1;
        }
        if all_chunks.is_empty() && corpora_searched {
            for cs in &installed_corpora {
                source_map.entry(cs.corpus_id.clone()).or_insert(0);
            }
        }
        let folder_meta_for_ctx = self.folder_metadata_snapshot().await;
        // Build the corpus-kind + display-category lookups before
        // the provenance components so the SourceSummary
        // `display_name` can render "Your conversations" for any
        // corpus declaring `[display] category = "conversation"`.
        // Catalog routing + Wikipedia editors' POV markers —
        // best-effort: `installed_indexes()` errors fall through to
        // defaults, so no callsite gates on the engine being
        // configured.
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
        let (sources, coverage) = build_provenance_components(
            &source_map,
            &peer_attribution,
            &folder_meta_for_ctx,
            if display_categories.is_empty() {
                None
            } else {
                Some(&display_categories)
            },
        );

        // 5. Build prompt with knowledge context.
        //
        // Use the EXPANDED budget here because `prepare_knowledge_context`
        // is the DeepQuery path and the multi-source expander above
        // may have appended depth-fetched chunks beyond the initial
        // top-K. The formatter takes chunks in order until the budget
        // is hit; if we kept `MAX_KNOWLEDGE_CHARS` (8000) the appended
        // depth chunks would never reach the prompt — which is the
        // exact failure mode v6 surfaced empirically (chunks_fact_score
        // climbed but answer-fact-score didn't, because the model
        // never saw the depth chunks).
        let contested_titles: std::collections::HashSet<String> =
            self.contested_titles_for_chunks(&all_chunks).await;
        let folder_meta = self.folder_metadata_snapshot().await;

        let history = format_history_as_prompt(context, 10);
        // Declared out here because the trace is BUILT inside the prompt
        // assembly below but CONSUMED by the DeepQuery grounding gate, which
        // only sees what rides out on the KnowledgeContext.
        let mut code_trace_out = String::new();
        let prompt = if !all_chunks.is_empty() {
            // Conv-tiered briefing — surface per-conversation RAPTOR
            // signposts ahead of the raw chunks when retrieval hit a
            // conversation corpus. No-op when no reader wired or no
            // conv-category chunks present. Spec
            // `sovereign/docs/specs/CONV_TIERED_PORT.md`.
            self.rerank_conv_chunks_via_ppr(message, &mut all_chunks, &display_categories)
                .await;
            // Late RAPTOR injection (SOVEREIGN_RAPTOR_LATE) — see the KQ path.
            // Appended post-rerank so leaf ranking is untouched. corpus_embedding
            // is block-local to the corpus-search arm and out of scope here, so
            // re-derive the SAME query embedding (build_retrieval_query →
            // embed_query) — isolates injection TIMING, not the embedding.
            if raptor_late_inject_enabled() {
                let late_emb = self
                    .inference
                    .embed_query(&build_retrieval_query(message, context))
                    .await
                    .unwrap_or_default();
                self.apply_raptor_grounding(
                    &late_emb,
                    &mut all_chunks,
                    "DeepQuery",
                    context.conversation.enabled_corpora.as_deref(),
                )
                .await;
            }
            let conv_briefing = self
                .build_conv_briefing_block(&all_chunks, &display_categories)
                .await;
            // Phase 3 (budget-sensor redesign): mirror the KQ path's
            // ctx-aware retrieval ceiling — this path previously
            // passed EXPANDED_KNOWLEDGE_CHARS unconditionally, blind
            // to the slot's window. Reserve the response budget plus
            // last turn's REAL measured system size (memo; 4096
            // static cushion on a conversation's first turn), then
            // hand the formatter the tighter of the two caps.
            let knowledge_char_budget = {
                let mut budget = EXPANDED_KNOWLEDGE_CHARS;
                if let Some(n_ctx) = self.inference.effective_context_size() {
                    let reserved_output = self.inference_config.max_tokens as u32;
                    let system_overhead = self
                        .last_assembly(&context.conversation.id)
                        .map(|m| m.system_tokens.saturating_add(256))
                        .unwrap_or(4096);
                    let available_chars = n_ctx
                        .saturating_sub(reserved_output)
                        .saturating_sub(system_overhead)
                        .saturating_mul(4) as usize;
                    if available_chars < budget {
                        tracing::info!(
                            n_ctx,
                            reserved_output,
                            system_overhead,
                            static_budget = budget,
                            ctx_aware_budget = available_chars,
                            "deep path: ctx-aware retrieval budget tighter than static cap"
                        );
                        budget = available_chars;
                    }
                }
                budget
            };
            let doc_context = format_scored_chunks_with_kinds(
                &all_chunks,
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
            // Code-intelligence-in-chat (Inc 2): mirror the KnowledgeQuery
            // augmentation on the DEEP path. Code questions route to DeepQuery
            // (REASONING), whose synthesis evidence is assembled here — so the
            // call-graph trace must be appended at this site too, not only at
            // knowledge_query.rs. Empty string (zero overhead) for non-code
            // corpora, so it is safe to run unconditionally. Twin injection.
            // Kept in scope (not consumed inline) so it can ride out on the
            // KnowledgeContext and reach the DeepQuery gate — see
            // `code_trace::trace_source_labels`.
            code_trace_out = crate::runtime::code_trace::build_code_trace_block(&all_chunks).await;
            let doc_context = if code_trace_out.is_empty() {
                doc_context
            } else {
                format!("{doc_context}\n\n{}", code_trace_out)
            };
            let knowledge_block = if conv_briefing.is_empty() {
                doc_context
            } else {
                format!("{conv_briefing}\n{doc_context}")
            };
            if history.is_empty() {
                format!("Relevant knowledge:\n{knowledge_block}\n\nUser: {message}\n\nAssistant:")
            } else {
                let short_history = format_history_as_prompt(context, 4);
                format!("{short_history}\n\nRelevant knowledge:\n{knowledge_block}\n\nAssistant:")
            }
        } else if history.is_empty() {
            message.to_string()
        } else {
            format!("{history}\n\nAssistant:")
        };

        // Seal audit (glassbox, ARCH §0.1/§9). When the conversation is scoped
        // to specific corpora (the `--isolate` seal / a corpus-pinned chat),
        // every retrieved chunk MUST belong to an allowed corpus (or its
        // `atlas:`-virtual / layer child). A chunk from outside the seal is a
        // cross-corpus bleed — log it loudly (with the offending corpora) so a
        // single live `--isolate` run confirms the seal holds end-to-end across
        // ALL injection paths, not just the ones audited statically.
        // `conversation-history` is exempt (prior turns, not a corpus source).
        if let Some(allow) = context.conversation.enabled_corpora.as_deref() {
            let bleed = corpora_outside_seal(&all_chunks, Some(allow));
            if bleed.is_empty() {
                tracing::info!(target: "retrieval.seal", allowed = ?allow, "DeepQuery: corpus seal intact");
            } else {
                tracing::warn!(
                    target: "retrieval.seal",
                    allowed = ?allow,
                    bleed = ?bleed,
                    "DeepQuery: cross-corpus bleed — chunks from corpora outside the conversation seal"
                );
            }
        }

        // 6. System message — layered confidence when knowledge is present.
        // Folder-ingest v1 §6.3: when a watched-folder corpus
        // contributed retrieval AND carries non-zero
        // failed_files/skipped_by_extension, append a one-line
        // "what I don't have" note so the synthesis is honest
        // about the user's coverage gap. Empty string when no
        // gaps — adds zero prompt overhead.
        let gap_note = build_coverage_gaps_note(&all_chunks, &folder_meta_for_ctx);
        // Budget reminder — same directive spliced into the
        // KnowledgeQuery synthesis routes. Tells the model how much
        // room it has so it picks a shape that lands within the
        // budget instead of opening a multi-section essay that gets
        // cut off mid-paragraph (the bug the cutoff chip surfaces on
        // the desktop side).
        // TEACHABLE P0 (rung 1): the active length lesson clamps the
        // directive target — the length DIRECTIVE only; the request's
        // max_tokens ceiling downstream is untouched. Same one-read
        // snapshot then drives the prompt block + transform + metadata.
        let active_lessons = crate::lessons::load_active_lessons(self.note_store.as_deref()).await;
        let (directive_target, lesson_length_applied) =
            active_lessons.adjust_soft_target(self.inference_config.max_tokens);
        let budget_note = crate::runtime::build_response_length_directive(directive_target);
        let system = if !all_chunks.is_empty() {
            // Synthesizer role builds the prompt body (SSOT). THINKING_DIRECTIVE
            // is a `<think>`-block contract — it guides the model's HIDDEN
            // reasoning channel; include it only when a think budget is
            // allocated, else a model with no `<think>` block would execute its
            // checklist in the OPEN. DeepQuery/Simple never take the comparison
            // shape.
            let base = crate::runtime::build_synthesis_system_prompt(
                false,
                &gap_note,
                self.inference_config.think_budget > 0,
                &budget_note,
            );
            self.build_primary_system_message(&base, context)
        } else {
            self.build_system_message(
                &format!(
                    "You are a helpful AI assistant. Respond concisely and accurately.\n\n{budget_note}"
                ),
                context,
            )
        };
        // TEACHABLE P0 (rung 4): K=1 prompt lesson, OUTERMOST — after
        // the custom-instructions layer `build_*_system_message`
        // appended. The relational witness arm in `handle_simple`
        // rebuilds its own system message and deliberately does NOT
        // re-add the block (witness voice is out of P0 scope).
        let mut system = system;
        if let Some(lesson) = &active_lessons.prompt {
            system.push_str("\n\n");
            system.push_str(&crate::lessons::render_lesson_block(
                &lesson.payload.prompt_form,
            ));
        }
        let turn_lessons = crate::runtime::types::TurnLessons::from_snapshot(
            &active_lessons,
            lesson_length_applied,
            active_lessons.prompt.is_some(),
        );

        // 7. Speed: the named intent→slot decision (one home for the
        // ladder; see `evidence::speed_for_retrieval_intent`).
        let speed =
            crate::runtime::evidence::speed_for_retrieval_intent(intent, !all_chunks.is_empty());

        // 8. Build chunk summaries for frontend source linking.
        // chunk_id and source_doc_id are emitted (when present) so the
        // desktop reading surface can deref a citation back to the
        // source chunk for in-app reading + atom-graph overlay.
        let retrieved_chunks = project_retrieved_chunks(&all_chunks);

        KnowledgeContext {
            chunks: all_chunks,
            prompt,
            code_trace: code_trace_out,
            system,
            speed,
            search_method,
            sources,
            retrieved_chunks,
            coverage,
            lessons: turn_lessons,
        }
    }
}
