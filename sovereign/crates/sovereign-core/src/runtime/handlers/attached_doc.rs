//! Attached-doc dispatch — the ReasonWithTools loop over a single
//! attached document. Iterates inference + tool calls (search/triangulate)
//! against the DocumentAsset until the model issues a `final` or hits
//! the iteration cap.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;

use crate::error::Result;
use crate::traits::*;
use crate::types::*;

use super::super::*;

impl Runtime {
    /// Handle a turn on a conversation with an attached document.
    ///
    /// Dispatches the turn through a `ReasonWithTools`-style loop over
    /// a fixed catalog `[attached_doc_search, knowledge_lookup,
    /// web_fetch]`. The model decides whether the answer is in the
    /// attached text (call `attached_doc_search`), in the corpus
    /// (`knowledge_lookup`), on the web (`web_fetch`), or some
    /// combination. Replaces the legacy parallel `DocumentAssetManager`
    /// router that mis-routed factual questions about an attached novel
    /// to `OffTopic` and sent them to the general corpus.
    ///
    /// Why a hand-rolled loop instead of `Executor::execute_reason_with_tools`:
    /// the Executor doesn't yet emit `ToolInvocationStart` / `Complete`
    /// narration — those frames are the load-bearing diagnostic for
    /// "did the model pick the doc tool". Inlining the loop keeps the
    /// narration-emission contract local to this handler; future cleanup
    /// can lift it into the Executor.
    pub(crate) async fn handle_attached_doc_turn(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        // ── Fixed tool catalog ────────────────────────────────────
        // Order matters for the model: list `attached_doc_search`
        // first to bias toward the most direct path. The model can
        // still pick the others when the question demands it.
        let available_tools: &[&str] =
            &["attached_doc_search", "knowledge_lookup", "web_fetch"];
        const MAX_ITERATIONS: usize = 4;

        let turn_start = std::time::Instant::now();

        // ── Build tool descriptions for the system prompt ────────
        let mut tool_descs: Vec<String> = Vec::with_capacity(available_tools.len());
        for id in available_tools {
            if let Ok(t) = self.tools.get(*id) {
                let d = t.descriptor();
                tool_descs.push(format!("- {} (id: {}): {}", d.name, d.id, d.description));
            }
        }

        // ── Resolve the attached asset + its skeleton ────────────
        // `DocumentSession.source` carries the asset id (per the
        // bench's `dispatch_question` and decision note 7693f16b).
        // The skeleton field carries the model-extracted overview,
        // main entities (ranked, with kind labels), and structural
        // moments. Without this briefing the model formulates queries
        // in the question's vocabulary — e.g. searching Conrad for
        // "the bomber" when the chunks call him "Stevie". With it,
        // the model picks up character / concept vocabulary on the
        // first query.
        let (briefing, briefing_entity_names) =
            self.build_attached_doc_briefing(conversation_id).await;

        // Fetch the asset's full chunk content + RAPTOR verbatim
        // spans up-front. Used by `verify_quotes_in_answer` below to
        // demote any quoted span in the model's final answer that
        // isn't substring-present in the document. The pre-fetch keeps
        // the verification path hot — we never want to skip it for
        // perf reasons. Best-effort: failures leave the verification
        // surface empty, which means the model's answer is returned
        // unchanged (graceful degradation).
        let (verification_chunks, verification_verbatim_spans) =
            self.fetch_quote_verification_surface(conversation_id).await;

        // Question-conditional RAG-into-briefing was tested and
        // *hurt* the bench score on book-report Tier 1 (28% vs
        // 71% baseline). Embedding the full user question with
        // its interrogative structure ("what / where / who / when")
        // surfaced early-chapter introductory chunks, not the
        // load-bearing later passages. The model's own targeted
        // entity queries (e.g. "Chief Inspector Heat velvet collar")
        // out-perform the full-question embedding by a wide
        // margin. Kept the helper for future experiments (e.g.
        // embedding a *summary* of the question rather than the
        // full question) but not on the hot path.
        let prefetch_block = String::new();

        if std::env::var("SOVEREIGN_DEBUG_BRIEFING").is_ok() {
            eprintln!(
                "[attached_doc] briefing: {} chars, {} entities; prefetch: {} chars",
                briefing.len(),
                briefing_entity_names.len(),
                prefetch_block.len(),
            );
            // Dump the entity-association section so we can see which
            // chunks K-NN-per-entity surfaced.
            if let Some(start) = briefing.find("**High entity-association chunks**") {
                let tail = &briefing[start..];
                eprintln!("[attached_doc] association section:\n{}", tail);
            } else {
                eprintln!("[attached_doc] association section: <empty> (no chunks associated with ≥2 entities)");
            }
        }

        let system_prompt = format!(
            "The user has attached a document to this conversation. {briefing}{prefetch_block}\
             \n\nYou have these tools:\n\n\
             {tool_descs}\n\n\
             Emit a tool call as a single line, exact format:\n\
             <tool_call>{{\"tool\":\"<id>\",\"query\":\"<search terms>\"}}</tool_call>\n\n\
             ## How to query effectively\n\
             **Phrase queries in the document's vocabulary, not the question's.** RAG retrieval scores chunks by similarity to your query, so paraphrasing the question abstractly will miss chunks that describe the same event in the document's specific words. The briefing above lists the document's actual entities and the phrases the document uses about them; lift query terms from there.\n\
             \n\
             **Your first query must use at least one entity name from the briefing.** Do not start with abstract question-words. Pick the entity in the briefing whose role most overlaps the question, and query with its name plus a concrete noun from its quote sample.\n\
             \n\
             **Plan for multiple queries.** Most questions have more than one layer of evidence — a partial hit on the first query usually means there's a richer passage you haven't found yet, often anchored on a *different* entity. Before answering, run at least 2 queries against different entities from the briefing. Stop only when you've explored the obvious angles or hit {MAX_ITERATIONS}.\n\
             \n\
             ## Rules\n\
             - When the question references the attached document, the user's text, \"this book\", \"this paper\", \"the document\", or a specific phrase that would live in the attachment, call `attached_doc_search` FIRST.\n\
             - If a search returns 0 passages, switch vocabulary (try a different character name from the briefing, or a phrase from one of the quote samples).\n\
             - **Citations are evidence, not decoration.** Never write `[passage N]`, `[Source: X]`, or any other citation marker unless that citation appeared verbatim in a tool result above this line. Do not invent passage numbers. If retrieval came up empty, do not fabricate citations to make your answer look grounded — say plainly that you couldn't find supporting passages.\n\
             - **HIT vs context chunks.** Tool results mark each chunk as either `HIT` (a direct embedding match for your query — load-bearing evidence) or `context` (a neighbour chunk surrounding a HIT, providing narrative flow). Cite HIT chunks for direct factual claims; use context chunks to understand setup and consequences. Don't quote a context chunk as if it were retrieved evidence for a different question.\n\
             - **Pretraining recall is not document evidence.** This document may be one you've seen during training (famous book, common paper, viral article). The user attached *their* copy because the specific text matters — paraphrasing from memory drops the exact wording, may use a different edition, and may be subtly wrong. If you find yourself writing facts that didn't come from a retrieved passage, mark them clearly: `(from general knowledge of this work, not from retrieved text — may differ from your attached copy)`. Default to refusal over training-data fluency.\n\
             - When you do have passages, write your final answer with [Source] citations that map to the actual labels the tool returned. Mark any claim you can't verify against retrieved passages as `[unverified]`.\n\
             - Maximum {MAX_ITERATIONS} tool calls per turn.\n",
            tool_descs = tool_descs.join("\n"),
        );

        // Conversation is held as a typed segment list rather than a
        // raw String so we can compress superseded tool results out
        // of every prefill after the first. Prefill cost dominates
        // per-question latency (~120s of 150s on the book-report
        // bench); keeping all four tool results verbatim in every
        // iteration's prompt is mostly wasted compute, since the
        // model issues refined queries on the same tool by turn 3-4
        // and the early result becomes dead weight.
        //
        // Compression rule: for each `tool_id`, keep the most recent
        // `MAX_TOOL_RESULTS_KEPT_PER_TOOL` results in full; replace
        // older ones with a one-line `(superseded — N passages, content
        // dropped)` marker. The model still sees that it queried,
        // sees the query text, and sees how much came back — it just
        // doesn't re-prefill the chunk content on every turn.
        let conversation_header =
            format!("{system_prompt}\n\n---\n\nUser: {message}\n\nAssistant:");
        let mut conversation_segments: Vec<AttachedDocSegment> = Vec::new();
        let mut iterations: usize = 0;
        let mut tool_ids_invoked: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut total_chunks: usize = 0;
        let mut search_method_parts: Vec<String> = Vec::new();
        // Distinct (lowercased, trimmed) query strings the model has
        // actually issued this turn. Used to enforce the
        // "explore-multiple-angles" rule structurally — the prompt asks
        // for it, but on the book-report bench the model retrieved 3
        // passages on its first query and stopped, missing the other
        // half of the answer that lived in a different chunk. Tracking
        // distinct queries lets the runtime push back when the model
        // tries to bail after one angle.
        let mut distinct_queries: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        // Minimum queries we expect before accepting a final answer.
        // Empirically the variance between runs at MIN=2 was large
        // (book-report Tier-1 swung 42-71% across identical-config
        // runs) because the model's choice of which two entities to
        // query was the single biggest random factor. Raising to 3
        // forces broader coverage; the gate's forcing message also
        // calls out unused entities from the briefing explicitly so
        // the third query targets a specifically-untouched angle.
        const MIN_DISTINCT_QUERIES: usize = 3;

        // Resolve session_id for narration emission. The bench / desktop
        // create a session via `self.sessions.begin(...)` in
        // `handle_turn` before reaching this handler, so this is non-empty
        // on the production path. Missing session is non-fatal — narration
        // just won't surface to the UI.
        let session_id: String = self
            .sessions
            .latest_for_conversation(conversation_id)
            .map(|s| s.id)
            .unwrap_or_default();

        loop {
            let conversation =
                render_attached_doc_conversation(&conversation_header, &conversation_segments);
            let request = CompletionRequest {
                prompt: conversation,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: Some(self.inference_config.max_tokens),
                temperature: Some(self.inference_config.temperature),
                structured_output: None,
                think_budget: Some(self.inference_config.think_budget),
                top_k: self.inference_config.top_k,
                top_p: None,
                oicp: self.build_oicp(&Intent::ComplexTask),
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

            let completion = self.inference.complete(&request).await?;
            let response_text = completion.text.trim().to_string();

            // ── Tool call? Parse + dispatch ──────────────────────
            if let Some((tool_id, query)) = parse_tool_call_inline(&response_text) {
                distinct_queries.insert(query.trim().to_lowercase());
                // Surface "calling X" immediately on TWO surfaces:
                // (a) the in-session narration log — what the bench
                //     + the chat-history transcript read after the
                //     turn completes;
                // (b) the routing-events sink — what the desktop UI
                //     subscribes to for live "Searching for X…" chips.
                // Both bypass the 1.5s suppression / 3-event cap in
                // `try_emit_narration` per the explicit contract on
                // `NarrationPhase::ToolInvocation*`.
                let call_id = uuid::Uuid::new_v4().to_string();
                let summary = format!(
                    "{tool_id}: \"{}\"",
                    truncate_for_chip(&query, 60)
                );
                let start_phase = NarrationPhase::ToolInvocationStart {
                    call_id: call_id.clone(),
                    tool_id: tool_id.clone(),
                    summary: summary.clone(),
                };
                let start_text = format!("Searching via {tool_id} for \"{query}\"");
                self.sessions.force_push_narration(
                    &session_id,
                    start_phase.clone(),
                    start_text.clone(),
                );
                let elapsed_ms = turn_start.elapsed().as_millis() as u64;
                self.routing_events
                    .emit_turn_narration(TurnNarration {
                        session_id: session_id.clone(),
                        conversation_id: conversation_id.to_string(),
                        event: NarrationEvent {
                            phase: start_phase,
                            text: start_text,
                            elapsed_ms,
                        },
                    })
                    .await;

                let (tool_result_text, ok, result_summary) = match self.tools.get(&tool_id) {
                    Ok(_) => {
                        let params = serde_json::json!({"query": query});
                        let tool_ctx = ToolContext {
                            conversation_id: conversation_id.to_string(),
                            task_id: None,
                            working_directory: None,
                            in_reasoning_loop: true,
                            agent_session_token: None,
                            turn_index: 0,
                        };
                        match self.tools.call_cached(&tool_id, &params, &tool_ctx).await {
                            Ok(StepOutput::Text(t)) => {
                                let chunks = t.matches("[Source").count();
                                total_chunks += chunks;
                                if std::env::var("SOVEREIGN_DEBUG_BRIEFING").is_ok() {
                                    let preview: String = t.chars().take(500).collect();
                                    eprintln!(
                                        "[attached_doc] query={:?} chunks={} preview:\n{}\n---",
                                        query, chunks, preview
                                    );
                                }
                                (
                                    t.clone(),
                                    true,
                                    format!("Retrieved {chunks} passage(s)"),
                                )
                            }
                            Ok(StepOutput::Json(v)) => {
                                let txt = v
                                    .get("answer")
                                    .and_then(|a| a.as_str())
                                    .unwrap_or("(no answer field)")
                                    .to_string();
                                (txt, true, "Retrieved JSON payload".to_string())
                            }
                            Ok(_) => (
                                "(no results)".to_string(),
                                true,
                                "Empty result".to_string(),
                            ),
                            Err(e) => (
                                format!("Tool error: {e}"),
                                false,
                                format!("Failed: {e}"),
                            ),
                        }
                    }
                    Err(_) => (
                        format!("Tool '{tool_id}' not available."),
                        false,
                        "Unknown tool".to_string(),
                    ),
                };

                let complete_phase = NarrationPhase::ToolInvocationComplete {
                    call_id: call_id.clone(),
                    tool_id: tool_id.clone(),
                    ok,
                    result_summary: result_summary.clone(),
                };
                self.sessions.force_push_narration(
                    &session_id,
                    complete_phase.clone(),
                    result_summary.clone(),
                );
                let elapsed_complete_ms = turn_start.elapsed().as_millis() as u64;
                self.routing_events
                    .emit_turn_narration(TurnNarration {
                        session_id: session_id.clone(),
                        conversation_id: conversation_id.to_string(),
                        event: NarrationEvent {
                            phase: complete_phase,
                            text: result_summary.clone(),
                            elapsed_ms: elapsed_complete_ms,
                        },
                    })
                    .await;

                tool_ids_invoked.insert(tool_id.clone());
                if !search_method_parts.contains(&tool_id) {
                    search_method_parts.push(tool_id.clone());
                }

                let thinking = response_text
                    .split("<tool_call>")
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let passage_count = tool_result_text.matches("[Source").count();
                conversation_segments.push(AttachedDocSegment::ToolCall {
                    thinking,
                    tool_id: tool_id.clone(),
                    query: query.clone(),
                    result: tool_result_text,
                    passage_count,
                });

                iterations += 1;
                if iterations >= MAX_ITERATIONS {
                    conversation_segments.push(AttachedDocSegment::FinalCue(
                        " You've used all available searches. Synthesize the final \
                         answer from what you have. Cite passages with [Source]; mark \
                         anything you couldn't verify against retrieved text as \
                         [unverified].\n\nAssistant:"
                            .to_string(),
                    ));
                    break;
                }
                continue;
            }

            // ── No tool call: this is the model's final answer ──
            //
            // Two structural gates run here. Each addresses a different
            // failure mode the bench surfaced.
            //
            // **(1) No-retrieval gate.** If the model wants to
            // synthesize without ever successfully retrieving a
            // passage, that's exactly the failure mode that
            // contaminates answers on famous documents — the model
            // has the doc in pretraining and will recite it (often
            // subtly wrong, often out of date, often a different
            // version than the user's attached copy). Refuse;
            // push a forcing turn demanding query or admission.
            if total_chunks == 0 && iterations < MAX_ITERATIONS {
                tracing::warn!(
                    iterations,
                    "attached_doc: model emitted final answer with 0 retrieved chunks — forcing another query"
                );
                conversation_segments.push(AttachedDocSegment::Gate {
                    thinking: response_text.trim().to_string(),
                    gate_text:
                        "You have NOT retrieved any passages from the attached document yet. Citing passages or stating document-specific facts without retrieval is exactly the pretraining-fallback failure the user asked you to avoid. You MUST either: (a) emit another <tool_call> using an entity name from the briefing, or (b) state plainly that you could not find this in the attached document. Do not fabricate `[passage N]` or `[Source: …]` markers. Try again now."
                            .to_string(),
                });
                iterations += 1;
                continue;
            }

            // **(2) Triangulation gate.** A single retrieval often
            // surfaces one angle and misses the other half of the
            // answer that lives in a different chunk. The bench's
            // stevie_address_label question is the canonical case —
            // Heat's velvet-collar discovery (one part of the book)
            // and Winnie's address-label sewing (a different part)
            // are both required; one query finds at most one of
            // them. Force at least two *distinct* queries before
            // accepting a final answer, unless the model has
            // exhausted iterations trying.
            if distinct_queries.len() < MIN_DISTINCT_QUERIES
                && iterations < MAX_ITERATIONS
            {
                tracing::warn!(
                    distinct = distinct_queries.len(),
                    iterations,
                    "attached_doc: model tried to finalise after fewer than {MIN_DISTINCT_QUERIES} distinct queries — forcing another angle"
                );
                // Enumerate entities from the briefing that haven't
                // yet appeared in any query. The model's biggest
                // source of run-to-run variance is which entity it
                // picks — naming the unused ones explicitly turns
                // the prompt's soft "try a different entity" into a
                // concrete shortlist the model can pick from.
                let unused_entities: Vec<&str> = briefing_entity_names
                    .iter()
                    .filter(|name| {
                        let lower = name.to_lowercase();
                        !distinct_queries.iter().any(|q| q.contains(&lower))
                    })
                    .take(6)
                    .map(|s| s.as_str())
                    .collect();
                let unused_hint = if unused_entities.is_empty() {
                    String::new()
                } else {
                    format!(
                        " Your next <tool_call> MUST contain one of these entity names: {}. Pick whichever fits a different aspect of the question and INCLUDE THE NAME VERBATIM in the query. Don't paraphrase the question — name the entity.",
                        unused_entities.join(", "),
                    )
                };
                conversation_segments.push(AttachedDocSegment::Gate {
                    thinking: response_text.trim().to_string(),
                    gate_text: format!(
                        "You've explored {n} angle(s) so far ({required} required) and your queries all use question-vocabulary, not document-vocabulary. The chunks holding the answer use the document's own entity names — querying \"identification evidence\" again will return the same chunks; you need to try a name.{unused_hint}",
                        n = distinct_queries.len(),
                        required = MIN_DISTINCT_QUERIES,
                    ),
                });
                iterations += 1;
                continue;
            }

            let verified = crate::quote_verification::verify_quotes(
                &response_text,
                &verification_chunks,
                &verification_verbatim_spans,
                crate::quote_verification::DEFAULT_MIN_QUOTE_CHARS,
            );
            if verified.demoted_count > 0 {
                tracing::warn!(
                    demoted = verified.demoted_count,
                    verified = verified.verified_count,
                    "attached_doc: post-generation guardrail demoted unverified quotations"
                );
            }
            return self
                .package_attached_doc_response(
                    conversation_id,
                    &verified.rewritten,
                    &completion,
                    &tool_ids_invoked,
                    total_chunks,
                    iterations,
                    &search_method_parts,
                )
                .await;
        }

        // ── Cap hit: force one more completion to synthesize ────
        let final_conversation =
            render_attached_doc_conversation(&conversation_header, &conversation_segments);
        let final_request = CompletionRequest {
            prompt: final_conversation,
            system_message: None,
            preferred_speed: Speed::Slow,
            max_tokens: Some(self.inference_config.max_tokens),
            temperature: Some(self.inference_config.temperature),
            structured_output: None,
            think_budget: Some(self.inference_config.think_budget),
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp: self.build_oicp(&Intent::ComplexTask),
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
        let final_completion = self.inference.complete(&final_request).await?;
        let mut final_text = final_completion.text.trim().to_string();
        // Last-resort honesty gate. If the iteration cap was hit
        // without a single successful retrieval, the model is about
        // to produce an answer drawn entirely from pretraining
        // (recall of a famous document) or fabricated. Replace it
        // with a structured refusal that names what the model tried
        // and why it's not answering — the user can then adjust the
        // question, re-attach, or accept that the document doesn't
        // address what they asked.
        if total_chunks == 0 {
            tracing::warn!(
                iterations,
                "attached_doc: cap hit with 0 retrieved chunks — emitting refusal"
            );
            final_text = format!(
                "I tried {iterations} search(es) of the attached document and didn't find passages that address your question. \
                 Rather than answer from general knowledge — which would likely diverge from the specific text you attached — I'm flagging this as not answerable from the attached document.\n\n\
                 If you think the answer is in there, try rephrasing the question using a character name, place, or specific phrase you recall from the text, and I'll search again.",
            );
        }
        let verified = crate::quote_verification::verify_quotes(
            &final_text,
            &verification_chunks,
            &verification_verbatim_spans,
            crate::quote_verification::DEFAULT_MIN_QUOTE_CHARS,
        );
        if verified.demoted_count > 0 {
            tracing::warn!(
                demoted = verified.demoted_count,
                verified = verified.verified_count,
                "attached_doc: post-generation guardrail (cap-hit path) demoted unverified quotations"
            );
        }
        self.package_attached_doc_response(
            conversation_id,
            &verified.rewritten,
            &final_completion,
            &tool_ids_invoked,
            total_chunks,
            iterations,
            &search_method_parts,
        )
        .await
    }
    /// Build a "Document briefing" block for the system prompt: the
    /// document's title, type, overview, top-N ranked entities, and
    /// the first few structural moments. Falls back to a minimal
    /// "(briefing unavailable)" string when the asset can't be
    /// resolved or the skeleton hasn't been built yet — the handler
    /// then runs without a briefing, which is degraded but still
    /// correct (the model just queries blind).
    ///
    /// **Tier-gating is implicit** via the per-section emptiness
    /// checks (`skeleton.overview.trim().is_empty()`,
    /// `skeleton.segments.is_empty()`, `raptor_nodes.is_empty()`,
    /// `distinctive.is_empty()`, etc.). At `MultiHopReady`, T3-only
    /// fields (overview, segments, RAPTOR nodes, motifs) are still
    /// empty so their sections skip themselves; T2 fields
    /// (main_entities, entity_index, action_atoms,
    /// structural_moments) are populated and surface normally. As T3
    /// completes mid-conversation each new turn picks up a richer
    /// briefing — no explicit state-check needed in this function.
    ///
    /// Why this matters: the chunks the tool retrieves use the
    /// document's vocabulary (character names, concept terms), not
    /// the question's vocabulary. The book-report bench (2026-05-21)
    /// surfaced the failure mode — a question about "the bomber" hit
    /// 0/1 on RAG queries because Conrad's chunks talk about
    /// "Stevie". Putting ranked entities in the system prompt fixes
    /// the vocabulary mismatch at the source: the model now formulates
    /// queries with the words the chunks actually contain.
    /// Pre-fetch the verification surface for `verify_quotes` calls
    /// at the end of `handle_attached_doc_turn`. Returns
    /// `(chunk_contents, raptor_verbatim_spans)`.
    ///
    /// The chunk contents are every chunk for the attached asset's
    /// source key; verification matches answer quotes against
    /// substring presence anywhere in the document. RAPTOR quote_spans
    /// are passed as `extra_verbatim_spans` so spans that cross
    /// chunk boundaries (or live in RAPTOR node text not directly
    /// chunked) still verify.
    ///
    /// Failures fall back to empty vecs — the verification function
    /// then becomes a no-op, leaving the answer unchanged. Better to
    /// ship an unmodified answer than to crash on a transient store
    /// read.
    pub(crate) async fn fetch_quote_verification_surface(
        &self,
        conversation_id: &str,
    ) -> (Vec<String>, Vec<String>) {
        let session = match self
            .store
            .get_document_session_by_conversation(conversation_id)
            .await
        {
            Ok(Some(s)) => s,
            _ => return (Vec::new(), Vec::new()),
        };
        let asset = match self.store.get_document_asset(&session.source).await {
            Ok(Some(a)) => a,
            _ => return (Vec::new(), Vec::new()),
        };
        let chunks = self
            .store
            .get_chunks_by_source(&asset.source_key())
            .await
            .unwrap_or_default();
        let chunk_contents: Vec<String> = chunks.into_iter().map(|c| c.content).collect();
        let raptor_nodes = self.store.list_raptor_nodes(&asset.id).await.unwrap_or_default();
        let verbatim_spans: Vec<String> = raptor_nodes
            .iter()
            .flat_map(|n| n.quote_spans.iter().map(|q| q.text.clone()))
            .collect();
        (chunk_contents, verbatim_spans)
    }
    pub(crate) async fn build_attached_doc_briefing(
        &self,
        conversation_id: &str,
    ) -> (String, Vec<String>) {
        let session = match self
            .store
            .get_document_session_by_conversation(conversation_id)
            .await
        {
            Ok(Some(s)) => s,
            _ => return ("(no attached document briefing available)".to_string(), Vec::new()),
        };
        let asset = match self.store.get_document_asset(&session.source).await {
            Ok(Some(a)) => a,
            _ => return ("(no attached document briefing available)".to_string(), Vec::new()),
        };

        let mut s = String::new();
        let mut entity_names: Vec<String> = Vec::new();
        s.push_str(&format!(
            "\n\n## Attached document\n\
             **Title:** {}\n\
             **Type:** {}\n\
             **Length:** {} words across {} chunks\n",
            asset.title,
            asset.document_type.label(),
            asset.word_count,
            asset.chunk_count,
        ));

        // Skeleton may be `None` when the ingest hasn't reached the
        // BuildingSkeleton phase yet. Without a skeleton the briefing
        // degrades to title + type only — still useful, just not
        // vocabulary-priming.
        let Some(skeleton) = asset.skeleton.as_ref() else {
            s.push_str(
                "\n(structural skeleton not yet built — query the document directly)\n",
            );
            return (s, entity_names);
        };

        if !skeleton.overview.trim().is_empty() {
            s.push_str(&format!("\n**Overview:** {}\n", skeleton.overview.trim()));
        }

        // Top entities — ranked by presence_rate. Cap at 12 so a
        // long-character-list document doesn't blow the prompt
        // budget, but wide enough that mid-prominence entities
        // (often the load-bearing ones for a specific question)
        // make the cut. Include the kind label AND one quote
        // sample so the model picks up the document's actual
        // phrasing, not just the entity name. The book-report
        // bench (2026-05-21) surfaced the failure mode this
        // addresses: with just names, the model still queries
        // in question-vocabulary ("physical evidence") instead
        // of document-vocabulary ("velvet collar", "address
        // label"). Sample quotes are what carry the phrasing
        // signal across.
        if !skeleton.main_entities.is_empty() {
            s.push_str(
                "\n**Main entities** (use these names + phrasings in queries — they're what the document actually says):\n",
            );
            for ent in skeleton.main_entities.iter().take(12) {
                entity_names.push(ent.name.clone());
                let quote_hint = skeleton
                    .entity_index
                    .get(&ent.name)
                    .and_then(|app| app.quote_samples.first())
                    .map(|q| {
                        let cleaned = q
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ");
                        let trimmed: String = cleaned.chars().take(140).collect();
                        format!(" — e.g. \"{trimmed}…\"")
                    })
                    .unwrap_or_default();
                s.push_str(&format!(
                    "- {} ({}, presence {:.0}%){}\n",
                    ent.name,
                    ent.kind.label(),
                    ent.presence_rate * 100.0,
                    quote_hint,
                ));
            }
        }

        if !skeleton.structural_moments.is_empty() {
            // Fetch the asset's chunks once so we can splice the raw
            // passage of each load-bearing structural moment directly
            // into the briefing. Without this, the briefing tells the
            // model *what* the moment is in abstract prose
            // ("Heat seizes the velvet collar despite orders") but
            // doesn't give the model the document's actual phrasing.
            // Including ~280 chars of the moment's chunk content
            // primes the model with the right vocabulary AND gives it
            // direct evidence to cite, without needing the retrieval
            // tool to surface that chunk at query time.
            //
            // Cost trade-off: 8 moments × ~280 chars = ~2.2KB added
            // to the system prompt. That's a small fraction of an
            // 8K-context model and pays off heavily in fact recall.
            let chunks = self
                .store
                .get_chunks_by_source(&asset.source_key())
                .await
                .unwrap_or_default();
            let chunk_by_index: std::collections::HashMap<usize, &str> = chunks
                .iter()
                .map(|c| (c.chunk_index, c.content.as_str()))
                .collect();

            s.push_str("\n**Structural moments** (load-bearing passages with the document's actual text — cite these chunks directly when relevant):\n");
            for m in skeleton.structural_moments.iter().take(8) {
                let v = serde_json::to_value(m).unwrap_or(serde_json::Value::Null);
                let label = v
                    .get("label")
                    .or_else(|| v.get("description"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("(moment)");
                let chunk_idx = v.get("chunk_index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let passage = chunk_by_index
                    .get(&chunk_idx)
                    .map(|content| {
                        let cleaned: String = content
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ");
                        let snippet: String = cleaned.chars().take(280).collect();
                        format!("\n    > [chunk {chunk_idx}] {snippet}…")
                    })
                    .unwrap_or_default();
                s.push_str(&format!("- {label}{passage}\n"));
            }

            // ── Segment map (scene/section index) ──────────────
            //
            // The ingest LLM walked adjacent chunks and grouped
            // them into `DocumentSegment`s (scene in fiction,
            // section in a paper, procedure in a manual). We
            // experimented with using these as retrieval-time
            // expansion units; that hurt aggregate bench scores
            // (T1 mech -18 from the prompt-budget squeeze
            // needed to bound 70-chunk segments). Reverted —
            // but the segment labels themselves are valuable
            // as a scene index for query formulation: a model
            // asking about "the bomb-handover scene" can see
            // the actual segment title + chunk range and form
            // a more targeted retrieval query.
            //
            // Cap at 24 segments to keep the briefing bounded;
            // longer documents have more scenes than any single
            // question needs surfaced.
            if !skeleton.segments.is_empty() {
                s.push_str("\n**Scene/section map** (titles the ingest LLM assigned to coherent multi-chunk units — use these to formulate scene-targeted queries):\n");
                for seg in skeleton.segments.iter().take(24) {
                    s.push_str(&format!(
                        "- chunks {}..={} — \"{}\"\n",
                        seg.chunk_start, seg.chunk_end, seg.title,
                    ));
                }
            }

            // ── Entity-association chunks (embedding-based) ─────
            //
            // The skeleton's structural_moments are LLM-curated and
            // miss chunks that are narratively dense but didn't get
            // tagged as "major turning points". Fill the gap by
            // running a K-NN-per-entity probe: for each top entity,
            // embed its name and pull top-3 chunks from the asset.
            // Chunks that surface for ≥2 different entities are
            // "co-associated" with multiple characters — a
            // structural signal for multi-character scenes.
            //
            // Why embedding-based, not substring matching:
            // substring scanning of chunk content for entity-name
            // tokens looks like a useful heuristic but it's
            // exactly the pattern the project retired in the 7-
            // substring-heuristic refactor: brittle (no word
            // boundaries — "Heat" matches "Heath"), document-
            // dependent ("Heat" in a thermodynamics paper is a
            // common noun not a character), and locale-fragile
            // (the `tok.len() > 3` filter that excluded "Mr"
            // also excludes legitimate names). Embedding similarity
            // is the project's preferred substitute — same signal,
            // no false-positive risk on out-of-domain attachments.
            //
            // Cost: 8 embed calls + 8 search_documents calls per
            // briefing build. ~1s at current daemon throughput.
            // Acceptable for the bench loop; a future change can
            // move this work to ingest time and persist it on the
            // skeleton so it's amortised across turns.
            let top_entity_names: Vec<String> = skeleton
                .main_entities
                .iter()
                .take(8)
                .map(|e| e.name.clone())
                .collect();
            let mut chunk_associations: std::collections::HashMap<usize, Vec<String>> =
                std::collections::HashMap::new();
            for entity_name in &top_entity_names {
                let entity_embedding = match self.inference.embed(entity_name).await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // Bump K to 64 to compete past chunks from other
                // assets in the store — same source-filter limitation
                // as the question-conditional prefetch helper.
                let raw = match self
                    .store
                    .search_documents(&entity_embedding, entity_name, 64)
                    .await
                {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let asset_top: Vec<&crate::types::DocumentChunk> = raw
                    .iter()
                    .filter(|c| c.source == asset.source_key())
                    .take(3)
                    .collect();
                for c in asset_top {
                    chunk_associations
                        .entry(c.chunk_index)
                        .or_default()
                        .push(entity_name.clone());
                }
            }
            // Filter to chunks associated with ≥2 entities and rank
            // by association count, then by chunk_index ascending
            // (narrative order on ties). Cap at 6.
            let mut density_ranked: Vec<(usize, usize, &crate::types::DocumentChunk)> = Vec::new();
            for (chunk_idx, entities) in &chunk_associations {
                if entities.len() >= 2 {
                    if let Some(chunk_ref) = chunks.iter().find(|c| c.chunk_index == *chunk_idx) {
                        density_ranked.push((entities.len(), *chunk_idx, chunk_ref));
                    }
                }
            }
            density_ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            if !density_ranked.is_empty() {
                s.push_str("\n**High entity-association chunks** (chunks that surface for multiple main entities under embedding similarity — often multi-character scenes):\n");
                for (count, chunk_idx, chunk_ref) in density_ranked.iter().take(6) {
                    let cleaned: String = chunk_ref
                        .content
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    let snippet: String = cleaned.chars().take(320).collect();
                    let entities = chunk_associations.get(chunk_idx).cloned().unwrap_or_default();
                    s.push_str(&format!(
                        "- [chunk {chunk_idx}, associated with {count} entities: {}] {snippet}…\n",
                        entities.join(", "),
                    ));
                }
            }
        }

        // ── RAPTOR atlas section (briefing v2 augmentation) ─────
        //
        // Appended to (not replacing) the skeleton-based briefing
        // during the transition. When raptor_nodes are populated
        // for this asset, surface:
        //   1. Mid-level node summaries with their evidence chunk
        //      ranges — these are scene/section-scale signposts
        //      RAPTOR built by clustering chunks then summarizing.
        //      Each node carries verbatim quote_spans (hallucination-
        //      detector-safe) and a transitive list of evidence
        //      chunks the model can fetch via attached_doc_search.
        //   2. Distinctive motifs from the TF-IDF + LLM-classified
        //      index — direct retrieval handles for recurring
        //      words/phrases (e.g. "incurious" in Conrad) that the
        //      RAPTOR abstraction loses but the bench's T4 tier
        //      relies on.
        //   3. Tool-usage guidance for the granularity contract.
        //
        // The legacy skeleton section above stays in place as the
        // fallback path: it's load-bearing today, and RAPTOR is
        // additive while the new path proves itself against the
        // bench. Once Phase 7 validates, the legacy section can be
        // trimmed in a follow-up.
        // ── Position pointers ───────────────────────────────────
        //
        // Deterministic retrieval handles independent of clustering
        // quality. Embedding clusters carve by topic similarity, not
        // by document position; a question about "how does it end"
        // or "what is the document's premise" wants a position-
        // anchored handle that clustering doesn't reliably provide.
        // Position pointers give the model a deterministic "the
        // opening/conclusion lives HERE" range regardless of the
        // cluster shape and regardless of document type.
        //
        // The shape of "opening" and "ending" generalises:
        //   - narrative: setup vs resolution
        //   - argument: thesis vs conclusion
        //   - paper: introduction+methodology vs results+discussion
        //   - chronicle: earliest vs latest entries
        //   - manual: overview vs appendices+references
        // The model is left to interpret in context — the briefing
        // describes positions, not document-type-specific roles.
        let total_chunks = asset.chunk_count as u32;
        if total_chunks > 20 {
            let opening_end = (total_chunks / 20).clamp(5, 50);
            let ending_start = total_chunks.saturating_sub((total_chunks / 10).clamp(10, 100));
            s.push_str(&format!(
                "\n**Position pointers.** Opening: chunks 0..{opening_end}. Ending: chunks {ending_start}..{}.\n",
                total_chunks - 1,
            ));
        }

        let raptor_nodes = self
            .store
            .list_raptor_nodes(&asset.id)
            .await
            .unwrap_or_default();
        if !raptor_nodes.is_empty() {
            // Pick the mid-level layer to surface. Heuristic:
            // - If we have nodes at level ≥ 1, the highest non-empty
            //   level is the "root layer" — skip it (it'll just be
            //   1-4 nodes summarising the whole doc).
            // - One level below the root is the scene/section layer
            //   the briefing wants.
            // - If only level-0 nodes exist (degenerate small doc),
            //   surface them directly.
            let max_level = raptor_nodes.iter().map(|n| n.level).max().unwrap_or(0);
            let target_level = if max_level >= 2 {
                max_level - 1
            } else if max_level == 1 {
                1
            } else {
                0
            };

            // Coherence-weighted ordering — tight clusters earn their
            // briefing slot ahead of looser ones.
            let mut surfaceable: Vec<&crate::types::RaptorNode> = raptor_nodes
                .iter()
                .filter(|n| n.level == target_level)
                .collect();
            surfaceable.sort_by(|a, b| {
                b.cluster_coherence
                    .partial_cmp(&a.cluster_coherence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            if !surfaceable.is_empty() {
                // Bench run #2 (2026-05-22) surfaced the failure mode
                // this section's wording fixes: rendering the
                // LLM-generated `node.summary` as a top-level claim
                // ("Winnie flees with Ossipon to Paris...") led the
                // model to treat the paraphrase as ground truth,
                // anchor on its (sometimes wrong) details, and query
                // down false leads. T1 `winnie_fate` regressed 100% →
                // 0% on this exact failure: the briefing's
                // post-Verloc-murder summary mentioned a train/wedding
                // ring path that doesn't exist in Conrad's text, so
                // the model queried for "wedding ring Winnie" and
                // missed the actual chunk 957 newspaper-notice.
                //
                // Fix: the section now surfaces only *verifiable*
                // signal — chunk range, primary entities, and a
                // single verbatim quote span per cluster. The
                // LLM-generated `summary` is dropped from the
                // briefing entirely. Summaries still drive
                // node-level *query matching* (their embeddings sit
                // in raptor_nodes.summary_embedding for tool-side
                // retrieval), but the model never sees their text
                // in the system prompt, so it can't misread a
                // paraphrase as a fact.
                s.push_str("\n**Cluster signposts** — embedding-grouped passage ranges in the document. Use the chunk ranges and entity hints to formulate `attached_doc_search` queries; the inline `>` snippets are verbatim and safe to quote:\n");
                for node in surfaceable.iter().take(8) {
                    let chunk_min = node.evidence_chunk_ids.iter().min().copied().unwrap_or(0);
                    let chunk_max = node.evidence_chunk_ids.iter().max().copied().unwrap_or(0);
                    let entities_label = if node.primary_entities.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " — entities: {}",
                            node.primary_entities
                                .iter()
                                .take(4)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", "),
                        )
                    };
                    s.push_str(&format!(
                        "- chunks {chunk_min}..{chunk_max}{entities_label}\n",
                    ));
                    if let Some(q) = node.quote_spans.first() {
                        let trimmed: String = q.text.chars().take(180).collect();
                        s.push_str(&format!("    > [chunk {}] {trimmed}…\n", q.chunk_id));
                    }
                }
            }
        }

        let motifs = self
            .store
            .list_asset_motifs(&asset.id)
            .await
            .unwrap_or_default();
        let distinctive: Vec<&crate::types::AssetMotif> =
            motifs.iter().filter(|m| m.is_distinctive).collect();
        if !distinctive.is_empty() {
            s.push_str("\n**Recurring motifs** (distinctive lexical recurrences — query `attached_doc_search` with the exact term to retrieve all occurrences):\n");
            for m in distinctive.iter().take(12) {
                let chunks_preview: Vec<String> = m
                    .occurrence_chunk_ids
                    .iter()
                    .take(5)
                    .map(|c| c.to_string())
                    .collect();
                let more = if m.occurrence_chunk_ids.len() > 5 {
                    format!(", +{} more", m.occurrence_chunk_ids.len() - 5)
                } else {
                    String::new()
                };
                s.push_str(&format!(
                    "- \"{}\" — chunks {}{}\n",
                    m.term,
                    chunks_preview.join(", "),
                    more,
                ));
            }
        }

        if !raptor_nodes.is_empty() || !distinctive.is_empty() || total_chunks > 20 {
            s.push_str(
                "\n**Briefing is pointers, not facts.** Only the inline `>` lines are verbatim. \
                 Quote only from retrieved chunks. If a query is thin, also try: a verbatim \
                 motif term, a distinctive word you remember from the chunks, or a query \
                 aimed at the OPENING/ENDING range.\n",
            );
        }

        (s, entity_names)
    }

    /// Embed the user's question and prefetch top-K chunks from the
    /// attached document. Returned as a system-prompt-shaped block
    /// the caller splices in.
    ///
    /// **Off the hot path (book-report 2026-05-21):** retained for
    /// future experiments (e.g. embedding a *summary* of the
    /// question rather than the question itself). When tested on
    /// the bench, embedding the full user question with its
    /// interrogative scaffold ("what / where / who / when")
    /// surfaced early-chapter introductory chunks rather than the
    /// later load-bearing passages — net regression vs the model's
    /// targeted entity-name queries. The helper stays compiled but
    /// unused so a future variant doesn't have to re-derive it.
    pub(crate) async fn build_question_conditional_passages(
        &self,
        conversation_id: &str,
        message: &str,
    ) -> String {
        let debug = std::env::var("SOVEREIGN_DEBUG_BRIEFING").is_ok();
        let session = match self
            .store
            .get_document_session_by_conversation(conversation_id)
            .await
        {
            Ok(Some(s)) => s,
            other => {
                if debug { eprintln!("[prefetch] no session: {:?}", other.is_ok()); }
                return String::new();
            }
        };
        let asset = match self.store.get_document_asset(&session.source).await {
            Ok(Some(a)) => a,
            other => {
                if debug { eprintln!("[prefetch] no asset for source={}: {:?}", session.source, other.is_ok()); }
                return String::new();
            }
        };
        let asset_source_key = asset.source_key();

        let embedding = match self.inference.embed_query(message).await {
            Ok(v) => v,
            Err(e) => {
                if debug { eprintln!("[prefetch] embed failed: {e}"); }
                return String::new();
            }
        };
        // K=64 here (vs K=16 in the tool path) because `search_documents`
        // doesn't accept a source filter — when the user has had the
        // same document ingested twice (a real-world case: re-attach
        // after editing, plus the bench's iterative re-ingests), the
        // top-K embedding hits often come entirely from the OTHER
        // asset. Bumping K gives the current asset's chunks a chance
        // to make it through the post-filter. Cost is bounded — 64
        // chunks × a few hundred bytes is still trivial.
        let chunks = match self
            .store
            .search_documents(&embedding, message, 64)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                if debug { eprintln!("[prefetch] search_documents failed: {e}"); }
                return String::new();
            }
        };
        if debug {
            let mut source_counts: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for c in &chunks {
                *source_counts.entry(c.source.as_str()).or_insert(0) += 1;
            }
            eprintln!(
                "[prefetch] embed_dim={} total_chunks={} source_counts={:?} asset_source_key={}",
                embedding.len(),
                chunks.len(),
                source_counts,
                asset_source_key,
            );
        }
        let relevant: Vec<&crate::types::DocumentChunk> = chunks
            .iter()
            .filter(|c| c.source == asset_source_key)
            .take(6)
            .collect();
        if relevant.is_empty() {
            return String::new();
        }

        let mut s = String::new();
        s.push_str("\n\n**Passages most relevant to the user's question** (embedded the full question and ran RAG; the model can cite these chunks immediately):\n");
        for c in &relevant {
            let cleaned: String = c
                .content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let snippet: String = cleaned.chars().take(350).collect();
            if debug {
                eprintln!("[prefetch] chunk {}: {}", c.chunk_index, snippet.chars().take(100).collect::<String>());
            }
            s.push_str(&format!(
                "- [chunk {}] {}…\n",
                c.chunk_index,
                snippet,
            ));
        }
        s
    }
    /// Save the assistant message + build the `Response` for an
    /// attached-doc turn. Pulled out of `handle_attached_doc_turn`
    /// because the loop has two exit points (model wrote a final
    /// answer, or we hit the iteration cap and forced one) and
    /// duplicating the bookkeeping at each invited drift.
    pub(crate) async fn package_attached_doc_response(
        &self,
        conversation_id: &str,
        text: &str,
        completion: &CompletionResponse,
        tool_ids_invoked: &std::collections::BTreeSet<String>,
        total_chunks: usize,
        iterations: usize,
        search_method_parts: &[String],
    ) -> Result<Response> {
        let search_method = if search_method_parts.is_empty() {
            Some(format!("AttachedDoc ({iterations} iterations, no tools)"))
        } else {
            Some(format!(
                "AttachedDoc ({iterations} iterations: {tools})",
                tools = search_method_parts.join(", "),
            ))
        };

        let provenance = ResponseProvenance {
            intent: "AttachedDoc".to_string(),
            search_method,
            sources: tool_ids_invoked
                .iter()
                .map(|id| SourceSummary {
                    origin: id.clone(),
                    count: 0,
                    from_peer: None,
                    display_name: None,
                })
                .collect(),
            inference_backend: completion.model_id.clone(),
            oicp_match: completion
                .oicp_meta
                .as_ref()
                .and_then(|m| m.match_quality.as_ref())
                .map(|q| format!("{q:?}")),
            total_latency_ms: completion.latency_ms,
            tokens_used: completion.tokens_used,
            coarse_intent: None,
            self_assessment: None,
            routing_trigger: None,
            coverage: None,
            finish_reason: completion.finish_reason.clone(),
            max_tokens_budget: Some(self.inference_config.max_tokens),
            completion_tokens: completion.completion_tokens,
        };

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: text.to_string(),
            created_at: now(),
            metadata: Some(serde_json::json!({
                "intent": "AttachedDoc",
                "iterations": iterations,
                "tools_invoked": tool_ids_invoked.iter().cloned().collect::<Vec<_>>(),
                "retrieved_chunks_total": total_chunks,
                "provenance": provenance,
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
