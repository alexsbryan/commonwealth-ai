// SPDX-License-Identifier: AGPL-3.0-or-later
//! Non-streaming turn dispatch — `handle_message` → `handle_turn`,
//! plus conversation seeding and the stream-drain adapter that
//! converts a streaming surface into a `Response`. Extracted
//! verbatim from `runtime.rs` in the 2026-06-10 decomposition.

use std::sync::Arc;

use futures::StreamExt;

use crate::context::build_context;
use crate::error::{Error, Result};
use crate::memory;
use crate::skills::SkillRegister;

use super::*;

impl Runtime {
    #[tracing::instrument(
        name = "runtime.handle_message",
        skip(self, message),
        fields(conversation_id = %conversation_id, message_chars = message.len())
    )]
    pub async fn handle_message(&self, message: &str, conversation_id: &str) -> Result<Response> {
        // Save the user message first so `handle_turn` sees it in the
        // conversation history during context building and routing.
        let user_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::User,
            content: message.to_string(),
            created_at: now(),
            metadata: None,
            version: now(),
        };
        self.store.save_message(&user_msg).await?;

        // Tag the conversation with the active skill on first message
        // (idempotent — see the streaming-path equivalent).
        if let Some(skill_id) = self.skills.primary_skill_id_for_conversation() {
            if let Err(e) = self
                .store
                .set_conversation_skill_if_unset(conversation_id, &skill_id)
                .await
            {
                tracing::debug!(
                    conversation_id,
                    error = %e,
                    "failed to tag conversation with skill_id; continuing"
                );
            }
        }

        self.handle_turn(message, conversation_id).await
    }

    /// Seed an empty conversation row with an optional workspace skill
    /// tag BEFORE the first message — the daemon `/v1/conversations`
    /// surface's analog of the desktop "new chat" flow
    /// (`commands/conversation.rs`). Setting `skill_id = "recipe-author"`
    /// here is what makes [`Self::handle_message_any`] route the
    /// conversation into the recipe-author agent loop. INSERT-OR-IGNORE:
    /// a no-op if the row already exists.
    pub async fn seed_conversation(
        &self,
        id: &str,
        created_at: i64,
        skill_id: Option<&str>,
    ) -> Result<()> {
        self.store
            .insert_empty_conversation(id, created_at, skill_id)
            .await
    }

    /// Non-streaming entry that honours workspace agent-loops. A
    /// conversation tagged `recipe-author` runs the long-lived tool
    /// loop (the same dispatch the desktop streaming path uses at
    /// [`Self::handle_message_stream`]), drained to a single
    /// [`Response`]; every other conversation falls through to the
    /// standard [`Self::handle_message`] turn chain, unchanged. The
    /// daemon conversation API calls this so a headless caller reaches
    /// the real recipe-author loop rather than a side-channel.
    pub async fn handle_message_any(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        if self.resolve_active_mode(conversation_id).await.as_deref()
            == Some(crate::intent_policy::MODE_RECIPE_AUTHOR)
        {
            return self
                .handle_message_stream_drain(message, conversation_id)
                .await;
        }
        self.handle_message(message, conversation_id).await
    }

    /// Drive the streaming turn pipeline and drain it into a single
    /// [`Response`]. Reuses [`Self::handle_message_stream`] wholesale —
    /// context build, user-message persistence, routing, and the
    /// workspace agent-loop dispatch — so a non-streaming caller gets
    /// identical behaviour to the desktop streaming surface without
    /// re-implementing any of it.
    pub async fn handle_message_stream_drain(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        use futures::StreamExt;
        let StreamHandle {
            message_id,
            mut stream,
        } = self.handle_message_stream(message, conversation_id).await?;
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            text.push_str(&item?);
        }
        Ok(Response {
            message: Message {
                id: message_id,
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: text,
                created_at: now(),
                metadata: Some(
                    serde_json::json!({ "intent": "RecipeAuthor", "via": "stream_drain" }),
                ),
                version: now(),
            },
            task: None,
            metrics: None,
        })
    }

    /// Run a conversation turn assuming the user message has **already** been
    /// saved as the latest message in the conversation.
    ///
    /// Callers that need to save the user message with custom metadata — for
    /// example the `ask_document` Tauri command which tags the message with
    /// the attached asset id — can call this entry point directly. The
    /// runtime pipeline (context build, working-memory compression, topic
    /// context, routing, synthesis, auto-title) then proceeds identically
    /// to [`Self::handle_message`].
    ///
    /// Build-context reads all existing messages from the store, so the
    /// pre-saved user message is included in the in-memory context without
    /// the caller having to push it explicitly.
    #[tracing::instrument(
        name = "runtime.handle_turn",
        skip(self, message),
        fields(conversation_id = %conversation_id, message_chars = message.len())
    )]
    pub async fn handle_turn(&self, message: &str, conversation_id: &str) -> Result<Response> {
        let turn_start = std::time::Instant::now();
        let has_doc_prefix = message.starts_with("[Document attached: ");
        tracing::info!(has_doc_prefix, "runtime: turn begin");

        // PR2e — same oversize guard the streaming path applies.
        // The `[Document attached: ...]` prefix path is exempt — that
        // one is designed for long inputs and runs through the
        // map-reduce pipeline, not the Fast-slot turn chain.
        if !has_doc_prefix && message.len() > MAX_TURN_MESSAGE_CHARS {
            tracing::warn!(
                message_chars = message.len(),
                limit = MAX_TURN_MESSAGE_CHARS,
                "runtime:oversize_message rejected (non-streaming)"
            );
            return Err(Error::InvalidInput(OVERSIZE_MESSAGE_HINT.to_string()));
        }

        // 1. Build context from store (use message text for memory retrieval).
        //    The user message is already persisted so it shows up here.
        let mut context = build_context(self.store.as_ref(), conversation_id, message).await?;
        tracing::debug!(
            messages = context.conversation.messages.len(),
            memories = context.memories.len(),
            installed_corpora = context.installed_corpora.len(),
            has_document_session = context.document_session.is_some(),
            "runtime: context built"
        );

        // Iter5: per-stage timing. We accumulate millisecond costs
        // upstream of dispatch and then attach them to the response
        // metrics if the handler populated metrics (witness paths
        // only). Stages we don't instrument (build_context FTS,
        // working-memory compression, topic context, KV digests)
        // are sub-100ms in practice — the relational latency
        // budget lives in routing, memory recall, Pass A, tensions,
        // and synthesis.
        let mut upstream_metrics = RuntimeMetrics::default();

        // 1a. Embedding-based memory recall on relational/witness paths.
        // FTS keyword retrieval misses concrete-event memories on
        // abstract self-referential queries (hard-mode H05:
        // *"what kind of person am I?"* shares zero keywords with
        // *"I left my last job because the team was burning out"*).
        // Re-rank/replace `context.memories` via cosine over batched
        // embeddings. Falls back to the FTS list on any error.
        if context.turn_register() == SkillRegister::Relational {
            let recall_start = std::time::Instant::now();
            let scope = crate::traits::MemoryScope::from_conversation_skill(
                context.conversation.skill_id.as_deref(),
            );
            match memory::recall_relevant_memories_embed(
                self.inference.as_ref(),
                self.store.as_ref(),
                &scope,
                message,
                5,
            )
            .await
            {
                Ok(top) if !top.is_empty() => {
                    tracing::debug!(
                        before = context.memories.len(),
                        after = top.len(),
                        "runtime: memories overridden via embedding recall"
                    );
                    context.memories = top;
                }
                _ => {}
            }
            upstream_metrics.memory_recall_ms = Some(recall_start.elapsed().as_millis() as u64);
        }

        // 1b. Compress working memory from conversation history (now including
        //     the latest user message — gives working-memory extraction a
        //     crisper view of current intent).
        let working_memory_start = std::time::Instant::now();
        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        upstream_metrics.working_memory_ms =
            Some(working_memory_start.elapsed().as_millis() as u64);
        context.working_memory = working_memory;

        // 1c. Update topic context for turn-aware routing. Latest user
        //     message is part of the extraction input — see the
        //     streaming-path equivalent comment above for rationale.
        let topic_context_start = std::time::Instant::now();
        let topic_context = crate::context::update_topic_context(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.topic_context.as_ref(),
            context.document_session.as_ref(),
            Some(message),
        )
        .await
        .ok();
        upstream_metrics.topic_context_ms = Some(topic_context_start.elapsed().as_millis() as u64);
        context.topic_context = topic_context;

        // 2. Route.
        //
        // Pre-classification narrowing (mode-only). See the
        // streaming-path comment for rationale; this keeps the two
        // dispatch surfaces symmetric so a turn classified the same
        // way sees the same tool catalog regardless of the
        // streaming/non-streaming distinction. Resolve the conv-tag
        // mode here so the narrow picks up the recipe-author catalog
        // (registry-side lookup misses workspace tags stored only on
        // the conversation row).
        let early_active_mode = self.resolve_active_mode(conversation_id).await;
        let tool_descriptors =
            self.narrow_tools_pre_classification_for_mode(early_active_mode.as_deref());
        let routing_start = std::time::Instant::now();
        let classification = self
            .router
            .classify(message, &context, &tool_descriptors)
            .await?;
        upstream_metrics.routing_ms = Some(routing_start.elapsed().as_millis() as u64);
        upstream_metrics.routing_breakdown = classification.timing.clone();

        // Same policy-apply + QuerySession hookup as the streaming
        // path. See handle_message_stream for context. PR1 dispatcher
        // only reaches MoveKind::Commit; PR2 will branch.
        let policy = decide_policy(&classification, &self.confidence_thresholds);
        tracing::debug!(
            tier = ?policy.tier,
            move_kind = ?policy.move_kind,
            primary_intent = ?classification.primary.intent,
            confidence = classification.primary.confidence,
            thresholds_high = policy.thresholds_used.high,
            thresholds_moderate = policy.thresholds_used.moderate,
            "router:policy_applied"
        );

        self.sessions.sweep_expired();
        let skill_id = self.skills.primary_skill_id_for_conversation();
        let (_session_id, _cancel_token) = self.sessions.begin(
            conversation_id.to_string(),
            skill_id,
            message.to_string(),
            classification.clone(),
            policy.clone(),
        );

        // Build the per-turn IntentPolicy on the non-streaming path
        // too, with the same shape as the streaming dispatch. See
        // that block for the contract; this stays symmetric so a
        // turn classified the same way sees the same policy on
        // either dispatch surface.
        let raw_intent = classification.primary.intent.clone();
        // Reuse the early resolution from the pre-classification
        // narrow site above — same conversation, same answer.
        let active_mode = early_active_mode.clone();
        let declared_register = active_mode
            .as_deref()
            .and_then(|id| self.skills.skill_by_id(id))
            .map(|s| s.inference.register)
            .unwrap_or_default();
        let intent_policy = crate::intent_policy::policy_for(
            &raw_intent,
            declared_register,
            active_mode.as_deref(),
        );
        let intent = intent_policy
            .effective_intent
            .clone()
            .unwrap_or_else(|| raw_intent.clone());
        context.intent_policy = Some(intent_policy);
        let coarse_intent = classification.coarse_intent.clone();
        let self_assessment = classification.self_assessment.clone();
        let scope = classification.scope.clone();

        tracing::info!(
            intent = ?intent,
            coarse = ?coarse_intent,
            self_assessment = ?self_assessment,
            scope = ?scope,
            active_mode = ?active_mode,
            tier = ?policy.tier,
            "runtime: routed"
        );

        // Recipe-author workspace dispatch on the non-streaming path
        // (mesh peer, OICP caller, CLI). Symmetric with the streaming
        // dispatch above — same handler, drained into a Response.
        if matches!(
            active_mode.as_deref(),
            Some(crate::intent_policy::MODE_RECIPE_AUTHOR)
                | Some(crate::intent_policy::MODE_WORKFLOW_AUTHOR)
        ) {
            let skill_id = active_mode
                .as_deref()
                .unwrap_or(crate::intent_policy::MODE_RECIPE_AUTHOR);
            tracing::info!(
                intent = ?intent,
                skill_id,
                "runtime: dispatching authoring workspace turn to agent loop (non-stream)"
            );
            return self
                .handle_recipe_author_turn(
                    skill_id,
                    message,
                    conversation_id,
                    &context,
                    &tool_descriptors,
                )
                .await;
        }

        // PR2 — Ask on the non-streaming path. Same semantics as
        // `handle_ask_move_stream`: save a placeholder assistant
        // message with clarification metadata, emit the event, return
        // a Response without running synthesis.
        if matches!(policy.move_kind, MoveKind::Ask) {
            return self
                .handle_ask_move_turn(message, conversation_id, &_session_id, &classification)
                .await;
        }
        // PR2 — Propose on the non-streaming path. Emit the banner
        // event before falling through to synthesis. Redirect from
        // the non-streaming path is a PR2c concern (the desktop runs
        // on the streaming path; CLI users who want to redirect can
        // send a new turn).
        if matches!(policy.move_kind, MoveKind::Propose) {
            let interpretation = format_interpretation(
                message,
                &classification.primary.intent,
                classification.rationale.as_deref(),
            );
            let alternatives = classification
                .alternatives
                .iter()
                .map(|a| ProposedAlternative {
                    label: label_for_intent(&a.intent),
                    intent_hint: intent_hint(&a.intent),
                })
                .collect();
            self.routing_events
                .emit_interpretation_proposed(InterpretationProposed {
                    session_id: _session_id.clone(),
                    conversation_id: conversation_id.to_string(),
                    interpretation,
                    alternatives,
                    confidence: classification.primary.confidence,
                })
                .await;
        }

        // 2b. Splice KnowledgeView landscape digests (same hook as
        // handle_message_stream). No-op when
        // `Runtime::with_landscape_digests` wasn't called at build
        // time. See the streaming path for rationale on `active_skill`.
        if let Some(provider) = &self.landscape_digests {
            // Conversation-tag-driven active skill (2026-05-24
            // redesign): the digest suppression should follow the
            // surface that owns the conversation, not registry state.
            let active_skill = self.resolve_active_mode(conversation_id).await;
            provider
                .splice_landscape_digests(&mut context, active_skill.as_deref())
                .await;
        }

        // 2c. R3 — temporal tension pre-pass. Mirror of the
        // streaming path: active for relational skills only,
        // zero-cost no-op for factual skills.
        let tensions_start = std::time::Instant::now();
        self.maybe_splice_temporal_tensions(&mut context, message)
            .await;
        upstream_metrics.tensions_ms = Some(tensions_start.elapsed().as_millis() as u64);

        // 2d. Tool-Mastery Layer 2 — compute the dossier. Same
        // pattern as the streaming path: pre-pass populates the
        // field, `build_system_message` splices it.
        self.maybe_compute_tool_dossier(&mut context, conversation_id)
            .await;

        // When a legacy [Document attached: ...] prefix is used, bypass the
        // planner entirely and route to the map-reduce document_operation path.
        if let Some(rest) = message.strip_prefix("[Document attached: ") {
            if let Some(end) = rest.find(']') {
                let source = rest[..end].to_string();
                let user_query = rest[end + 1..].trim().to_string();
                tracing::info!(
                    source = %source,
                    user_query_chars = user_query.len(),
                    "runtime: dispatching to handle_document_operation"
                );
                let result = self
                    .handle_document_operation(&source, &user_query, conversation_id)
                    .await;
                tracing::info!(
                    success = result.is_ok(),
                    total_latency_ms = turn_start.elapsed().as_millis() as u64,
                    "runtime: turn end (document_operation)"
                );
                return result;
            }
        }

        // ── Attached-document branch (the new path, sovereign decision 7693f16b) ──
        //
        // When this conversation has an active `DocumentSession`, the
        // user has attached a document and the answer probably lives in
        // it. Bypass intent classification + corpus-shaped retrieval
        // entirely and dispatch through a `ReasonWithTools`-style loop
        // over `[attached_doc_search, knowledge_lookup, web_fetch]`.
        // The model picks which tool to call (and how many times).
        //
        // The book-report bench (2026-05-20) surfaced the failure mode
        // this fixes: a question about Conrad's novel was classified as
        // `KnowledgeQuery` → corpus retrieval → 32 chunks from
        // `sep`+`wikipedia`, zero from the attached novel → answer about
        // the 2005 London bombings. The KQ handler doesn't consult the
        // tool catalog, so registering `attached_doc_search` alone
        // didn't change behaviour. Branching here is what makes the
        // tool actually fire.
        if context.document_session.is_some() {
            tracing::info!(
                conversation_id,
                "runtime: dispatching to handle_attached_doc_turn (document_session present)"
            );
            let result = self
                .handle_attached_doc_turn(message, conversation_id)
                .await;
            tracing::info!(
                success = result.is_ok(),
                total_latency_ms = turn_start.elapsed().as_millis() as u64,
                "runtime: turn end (attached_doc)"
            );
            return result;
        }

        // ── Team-pipeline gate (Phase 4 of the situated-team plan) ──
        //
        // Symmetric to the streaming-path gate at ~line 5087. When
        // `SOVEREIGN_TEAM_PIPELINE` is on AND the intent is one the
        // orchestrator handles, route through `run_team_pipeline`,
        // drain the Presenter stream into a single string, and
        // synthesize a `Response`. This is the entry point that
        // `voice_eval` exercises (it calls `handle_message` →
        // `handle_turn`, not the streaming path), so without this
        // branch flipping the kill-switch has no effect on the
        // bench harness.
        if crate::pipeline::is_team_pipeline_enabled()
            && matches!(
                intent,
                Intent::SimpleQuery
                    | Intent::DeepQuery
                    | Intent::KnowledgeQuery
                    | Intent::ComparisonQuery
                    | Intent::ExpressiveQuery
            )
        {
            tracing::info!(
                intent = ?intent,
                "team-pipeline: kill-switch enabled — routing turn through orchestrator (non-streaming path)"
            );
            let candidates = self.retrieve_candidates(message, &context, &intent).await;
            let register = context.turn_register();
            let witness_grounding = build_witness_grounding(&context, register);
            let inputs = crate::pipeline::TeamPipelineInputs {
                provider: Arc::clone(&self.inference),
                message,
                classification: &classification,
                register,
                candidates,
                max_tokens: crate::pipeline::DEFAULT_TEAM_PIPELINE_MAX_TOKENS,
                judge_enabled: true,
                witness_grounding,
            };
            let sink: Arc<dyn crate::pipeline::NarrationSink> =
                Arc::new(crate::pipeline::RoutingEventNarrationSink {
                    inner: Arc::clone(&self.routing_events),
                });
            let mut output = crate::pipeline::run_team_pipeline(
                inputs,
                sink,
                _session_id.clone(),
                conversation_id.to_string(),
            )
            .await?;

            // Drain the Presenter token stream into a single string.
            // Errors mid-stream produce a partial response — log and
            // continue rather than failing the whole turn, since the
            // user (or bench) still gets whatever was produced.
            let mut raw_text = String::new();
            while let Some(chunk) = output.stream.next().await {
                match chunk {
                    Ok(token) => raw_text.push_str(&token),
                    Err(e) => {
                        tracing::warn!(error = %e, "team-pipeline (non-stream): mid-stream error");
                        break;
                    }
                }
            }
            // iter4: strip mechanical artifacts from the Presenter
            // output here, in code, instead of asking the LLM to do
            // it (which iter1–iter3 showed caused the small Fast
            // slot to narrate the cleanup task instead of executing
            // it). The desktop streaming path applies the same
            // helper post-stream so users see clean text too — see
            // `pipeline::presenter::strip_presenter_artifacts`.
            let full_text = crate::pipeline::presenter::strip_presenter_artifacts(&raw_text);

            let total_turn_ms = turn_start.elapsed().as_millis() as u64;
            let assistant_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: full_text,
                created_at: now(),
                metadata: None,
                version: now(),
            };
            self.store.save_message(&assistant_msg).await?;

            let mut metrics = upstream_metrics.clone();
            metrics.total_turn_ms = Some(total_turn_ms);

            tracing::info!(
                dispatch = "team_pipeline",
                total_latency_ms = total_turn_ms,
                "runtime: turn end (team-pipeline non-stream)"
            );
            return Ok(Response {
                message: assistant_msg,
                task: None,
                metrics: Some(metrics),
            });
        }

        // 3. Dispatch based on intent.
        // ComparisonQuery rides the same retrieval+synthesis path as
        // KnowledgeQuery; the difference is in (a) the OICP envelope
        // (Fast latency_class → fast slot) and (b) the comparison-aware
        // synthesis prompt branch built downstream by intent matching.
        // MetalingualQuery has its own handler — source-anchored
        // retrieval against a filtered corpus subset, distinct from
        // KnowledgeQuery's broad retrieval. ConationQuery,
        // CommissiveQuery, and ExpressiveQuery each have dedicated
        // situated handlers. Conation/Commissive still operate on
        // prior-turn / notes-store. ExpressiveQuery also operates
        // situated, but its Relational branch now consumes the
        // upstream FTS retrieval (`context.memories`) + any
        // temporal tensions so the witness contract can execute
        // its contradiction-across-time moves.
        let dispatch = match intent {
            Intent::ComplexTask => "handle_complex_task",
            Intent::KnowledgeQuery | Intent::ComparisonQuery => "handle_knowledge_query",
            Intent::CodeQuery => "handle_code_query",
            Intent::MetalingualQuery => "handle_metalingual_query",
            Intent::ConationQuery => "handle_conation_query",
            Intent::CommissiveQuery => "handle_commissive_query",
            Intent::ExpressiveQuery => "handle_expressive_query",
            _ => "handle_simple",
        };
        tracing::info!(dispatch, "runtime: dispatching");

        let result = match intent {
            Intent::ComplexTask => {
                self.handle_complex_task(message, conversation_id, &context, &tool_descriptors)
                    .await
            }
            Intent::KnowledgeQuery | Intent::ComparisonQuery => {
                self.handle_knowledge_query(
                    message,
                    conversation_id,
                    &context,
                    &intent,
                    coarse_intent,
                    self_assessment,
                    classification.rationale.clone(),
                )
                .await
            }
            Intent::CodeQuery => {
                self.handle_code_query(
                    message,
                    conversation_id,
                    &context,
                    coarse_intent,
                    self_assessment,
                    classification.rationale.clone(),
                )
                .await
            }
            Intent::MetalingualQuery => {
                self.handle_metalingual_query(message, conversation_id, &context)
                    .await
            }
            Intent::ConationQuery => {
                self.handle_conation_query(message, conversation_id, &context)
                    .await
            }
            Intent::CommissiveQuery => {
                self.handle_commissive_query(message, conversation_id, &context)
                    .await
            }
            Intent::ExpressiveQuery => {
                self.handle_expressive_query(message, conversation_id, &context)
                    .await
            }
            _ => {
                self.handle_simple(
                    message,
                    conversation_id,
                    &context,
                    &intent,
                    coarse_intent,
                    self_assessment,
                    classification.rationale.clone(),
                    scope.as_deref(),
                )
                .await
            }
        };

        // Iter5: stitch upstream timings into the handler's metrics
        // when the witness path was active. Handlers fill in
        // pass_a_ms / synthesis_ms; we add routing / recall / tensions
        // here so the report sees the full waterfall.
        // Iter6: also stitch routing_breakdown, working_memory,
        // topic_context, and total_turn_ms.
        let total_turn_ms = turn_start.elapsed().as_millis() as u64;
        let result = result.map(|mut r| {
            if let Some(m) = r.metrics.as_mut() {
                m.routing_ms = upstream_metrics.routing_ms;
                m.routing_breakdown = upstream_metrics.routing_breakdown.clone();
                m.memory_recall_ms = upstream_metrics.memory_recall_ms;
                m.working_memory_ms = upstream_metrics.working_memory_ms;
                m.topic_context_ms = upstream_metrics.topic_context_ms;
                m.tensions_ms = upstream_metrics.tensions_ms;
                m.total_turn_ms = Some(total_turn_ms);
            }
            r
        });

        tracing::info!(
            dispatch,
            success = result.is_ok(),
            total_latency_ms = turn_start.elapsed().as_millis() as u64,
            "runtime: turn end"
        );
        result
    }
}
