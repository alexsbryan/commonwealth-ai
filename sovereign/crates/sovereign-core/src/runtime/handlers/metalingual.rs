// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MetalingualQuery` dispatch — source-anchored vocabulary lookup.
//!
//! Distinct from KnowledgeQuery: filters retrieval to the source the
//! locator points to ("according to SEP" → only sep; "in this
//! codebase" → only the code corpora, resolved by
//! [`corpus_engine::IndexInfo::is_code_corpus`] rather than by the
//! `CorpusKind::Code` tag, which repo corpora deliberately never carry).
//! When the locator names a source that isn't indexed locally, we surface
//! the gap explicitly rather than falling through to general knowledge —
//! silent confabulation against the wrong source is exactly what this
//! carve-out prevents.
//!
//! The one exception is a handoff, not a fallback: an in-system locator
//! that finds no vocabulary match escalates to `handle_code_query`, which
//! searches the same source the user named with the call graph in play.

use crate::error::Result;
use crate::types::*;

use super::super::{
    cap_chunks_per_article, cross_corpus_sort_cmp, format_scored_chunks_with_kinds, now,
    parse_metalingual_locator, reweight_by_query_relevance, MetalingualLocator, Runtime,
    FAST_KNOWLEDGE_MAX_TOKENS, KNOWLEDGE_SYNTHESIS_SYSTEM, KQ_MERGED_LIMIT, KQ_PER_CORPUS_LIMIT,
    MAX_CHUNKS_PER_ARTICLE_AT_MERGE, MAX_KNOWLEDGE_CHARS,
};

impl Runtime {
    /// Handle MetalingualQuery: source-anchored vocabulary lookup.
    ///
    /// Distinct from KnowledgeQuery: rather than retrieving across all
    /// installed knowledge corpora, this filters to the source the
    /// locator points to ("according to SEP" → only sep; "in this
    /// codebase" → only Code corpora; "earlier in this conversation"
    /// → conversation-history corpus). Synthesis is fast slot with a
    /// source-attribution-heavy prompt — the answer says how *that
    /// source* uses the term, not a generic dictionary entry.
    ///
    /// Empty-state behaviour is intentional: when the locator points
    /// to a source that isn't indexed, we surface that explicitly and
    /// suggest the operator command to enable it. We do *not* fall
    /// through to general knowledge retrieval — that would defeat the
    /// whole point of the metalingual carve-out (silent confabulation
    /// against the wrong source).
    ///
    /// One escalation is allowed ahead of that dead end: an in-system
    /// locator with code corpora available but no vocabulary match hands
    /// off to `handle_code_query` once, stamped `escalated_from` in the
    /// response metadata. It searches the source the user actually named,
    /// so the carve-out's guarantee is intact — what it removes is the
    /// asymmetry where a structural question landing on this route got
    /// nothing at all while the reverse misroute cost only precision.
    pub(crate) async fn handle_metalingual_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        // The locator the ROUTER committed on, when it committed on
        // one. `None` = no routing-level verdict to honour, so parse
        // the message. Threading this is what lets the semantic
        // locator tier work at all: those queries carry none of the
        // literal markers `parse_metalingual_locator` looks for, so
        // re-parsing here would discard the router's decision and send
        // the turn off to search corpora (the 2026-07-25 gap-probe
        // failure, one layer down).
        locator_hint: Option<MetalingualLocator>,
    ) -> Result<Response> {
        let from_router = locator_hint.is_some();
        let locator = locator_hint.unwrap_or_else(|| parse_metalingual_locator(message));
        tracing::info!(
            ?locator,
            from_router,
            "MetalingualQuery: resolved locator"
        );

        // Which locators point at *our own code*. Both the retrieval scope
        // below and the empty-state escalation key off this, so they cannot
        // disagree about what "in this codebase" covers.
        let in_system = matches!(
            &locator,
            MetalingualLocator::SystemCode
                | MetalingualLocator::Ambient
                | MetalingualLocator::Unknown
        );

        // Resolve the codebase by CAPABILITY, not by tag.
        //
        // This used to be `kind_filter = Some(CorpusKind::Code)`, which
        // matched **zero corpora on every real install**: repo corpora
        // deliberately ship as `knowledge`-kind (chat retrieval admits only
        // `Knowledge | Catalog`, so a `Code` tag would delete them from chat —
        // see `IndexInfo::is_code_corpus`). The result was that every "in this
        // codebase" question fell to the `no_source` empty state below while a
        // fully indexed repo with a SCIP graph sat right beside it. Measured
        // 2026-07-25: `commonwealth-ai`, 41,691 chunks, `kind=knowledge`,
        // `scip_graph.db` present — and metalingual retrieved nothing from it.
        //
        // `code_corpus_ids()` is the same resolver `handle_code_query` uses, so
        // the two routes now agree on what the codebase IS and differ only in
        // what they do with it: metalingual asks what a term *means*, CodeQuery
        // asks where it lives and what calls it.
        let code_ids: Vec<String> = if in_system {
            self.code_corpus_ids().await
        } else {
            Vec::new()
        };

        // Resolve locator → (kind_filter, name_match).
        let (kind_filter, name_match): (Option<corpus_engine::CorpusKind>, Option<String>) =
            match &locator {
                MetalingualLocator::SystemCode
                | MetalingualLocator::Ambient
                | MetalingualLocator::Unknown => {
                    // Scoped via `code_ids` below rather than by kind. An
                    // ambient locator ("what does X mean here") resolves the
                    // same way — in a dev chat that is nearly always the
                    // codebase, and when no code corpus exists the scope is
                    // empty and the empty-state message handles it.
                    (None, None)
                }
                MetalingualLocator::Conversation => {
                    // Unused on this path — the Conversation locator
                    // does NOT search corpora (see the direct-route
                    // branch below). Kept so the match stays total.
                    //
                    // This previously resolved to `Some("conversation")`,
                    // matching a Knowledge corpus whose id contains
                    // "conversation". No such corpus is ever installed,
                    // so the branch was dead: it always returned zero
                    // chunks and fell through to the `no_source` message
                    // — even when routing was correct. Measured
                    // 2026-07-25 (gap-probe run, 0/5 recall on
                    // "what was the very first topic I asked about").
                    (None, None)
                }
                MetalingualLocator::NamedSource(name) => (None, Some(name.clone())),
            };

        // Intersect the code scope with any explicit conversation scope, so a
        // user who narrowed the notebook to one corpus keeps that narrowing.
        // An empty intersection means the user scoped away from every code
        // corpus — honour that and let the empty state explain, rather than
        // silently searching corpora they excluded.
        let scope: Option<Vec<String>> = if in_system {
            Some(match context.conversation.enabled_corpora.as_deref() {
                Some(enabled) => {
                    let allowed: std::collections::HashSet<&str> =
                        enabled.iter().map(String::as_str).collect();
                    code_ids
                        .iter()
                        .filter(|c| allowed.contains(c.as_str()))
                        .cloned()
                        .collect()
                }
                None => code_ids.clone(),
            })
        } else {
            None
        };
        if in_system {
            tracing::info!(
                target: "runtime.metalingual",
                corpora = ?scope,
                "MetalingualQuery: scoping retrieval to code corpora"
            );
        }

        let locator_phrase = match &locator {
            MetalingualLocator::SystemCode => "this codebase".to_string(),
            MetalingualLocator::Conversation => "this conversation".to_string(),
            MetalingualLocator::NamedSource(n) => n.clone(),
            MetalingualLocator::Ambient | MetalingualLocator::Unknown => "this system".to_string(),
        };

        // The Conversation locator's source IS the conversation. Take
        // the direct route to the turns already in `context` instead of
        // searching installed corpora — a question about this thread is
        // not a retrieval problem, and the similarity channels can't
        // serve it anyway: ordinal asks ("what did I ask FIRST") have no
        // semantic neighbour to match, so every candidate scores below
        // the retrieval floor. Order is the signal here, so the
        // relevance reweight/sort below is deliberately NOT applied.
        // The running conversation frame — the distilled record of the
        // turns that have already scrolled out of the visible window
        // (`conv_frame`: topics, entities, stated goals, commitments,
        // open threads). It is the honest answer to "what do you
        // remember about this chat": the raw turns in `context` are
        // only what survived the window, while the frame is what the
        // system actually carries forward. Loaded for the Conversation
        // locator only — no other locator is asking about this thread.
        let conversation_frame: Option<String> =
            if matches!(locator, MetalingualLocator::Conversation) {
                match self.store.get_conversation_frame(conversation_id).await {
                    Ok(raw) => {
                        let rendered = crate::conv_frame::parse(raw.as_deref()).render_for_prompt();
                        let rendered = rendered.trim().to_string();
                        (!rendered.is_empty()).then_some(rendered)
                    }
                    Err(e) => {
                        // Soft-fail: the turns below still answer most
                        // conversation questions. Losing the frame
                        // degrades recall, it doesn't break the turn.
                        tracing::debug!(error = %e, conversation_id,
                            "metalingual: conversation frame unreadable");
                        None
                    }
                }
            } else {
                None
            };

        let mut chunks = if matches!(locator, MetalingualLocator::Conversation) {
            conversation_turns_as_chunks(&context.conversation.messages)
        } else {
            let embedding = self
                .inference
                .embed_query(message)
                .await
                .unwrap_or_default();
            let mut chunks = self
                .search_corpora_filtered(
                    &embedding,
                    message,
                    KQ_PER_CORPUS_LIMIT,
                    kind_filter,
                    name_match.as_deref(),
                    "MetalingualQuery",
                    // In-system locators carry the code scope; every other
                    // locator keeps the conversation's own scope untouched.
                    scope
                        .as_deref()
                        .or(context.conversation.enabled_corpora.as_deref()),
                    context.corpus_ceiling.as_deref(),
                )
                .await;

            // Reweight + sort + cap mirror KnowledgeQuery's conditioning so
            // chunk quality is on the same scale.
            reweight_by_query_relevance(&mut chunks, message);
            chunks.sort_by(cross_corpus_sort_cmp);
            let mut chunks = cap_chunks_per_article(chunks, MAX_CHUNKS_PER_ARTICLE_AT_MERGE);
            chunks.truncate(KQ_MERGED_LIMIT);
            chunks
        };

        // A conversation whose turns have all scrolled out of the
        // window yields zero chunks but may still have a frame — that
        // is precisely the case the frame exists for, so an available
        // frame keeps the turn on the synthesis path instead of the
        // "couldn't find that reference" bail below.
        if chunks.is_empty() && conversation_frame.is_none() {
            // ── Recoverable boundary, before the honest dead end. ──
            //
            // The metalingual/code split is a k-NN boundary between two
            // clusters that share the same in-system locator ("in this
            // codebase" appears on both sides); what separates them is the
            // interrogative shape — what a term MEANS versus WHERE it lives
            // and what calls it. Novel phrasings will land on the wrong side
            // of that boundary, and the cost of doing so used to be
            // catastrophic in exactly one direction: a structural question
            // that landed here got no answer at all, while the reverse
            // (a vocabulary question landing on the code route) merely
            // returned a slightly over-scoped but correct one.
            //
            // So when the user pointed at our own code and this route found
            // nothing there, hand off to the code route once rather than dead
            // ending. That is NOT the parametric fallback the carve-out
            // forbids — it searches the very source the user named, harder.
            // A misroute now costs latency instead of the answer.
            if in_system && !code_ids.is_empty() {
                tracing::info!(
                    target: "runtime.metalingual",
                    ?locator,
                    code_corpora = code_ids.len(),
                    "MetalingualQuery: no vocabulary match in the codebase; escalating to CodeQuery"
                );
                let mut escalated = self
                    .handle_code_query(
                        message,
                        conversation_id,
                        context,
                        None,
                        None,
                        Some("metalingual_empty_escalation".to_string()),
                    )
                    .await?;
                // Stamp the handoff so it is legible in the transcript and the
                // provenance panel — an escalation nobody can see is magic,
                // and magic is what makes routing bugs take three sessions to
                // find.
                let note = serde_json::json!({
                    "escalated_from": "MetalingualQuery",
                    "escalation_reason": format!(
                        "in-system locator {:?}, 0 vocabulary matches, {} code corpus(es) available",
                        locator,
                        code_ids.len()
                    ),
                });
                match escalated.message.metadata.as_mut() {
                    Some(serde_json::Value::Object(map)) => {
                        if let serde_json::Value::Object(extra) = note {
                            map.extend(extra);
                        }
                    }
                    _ => escalated.message.metadata = Some(note),
                }
                return Ok(escalated);
            }

            // No indexed source matches the locator. Surface the gap
            // honestly — the alternative (parametric fallback) is
            // exactly the failure mode that motivated this carve-out.
            let empty_message = match &locator {
                MetalingualLocator::SystemCode => {
                    "I read this as a question about *this codebase*, but I don't \
                     have a code corpus indexed locally. Run `sovereign code \
                     index <path>` against the relevant repo to enable in-system \
                     vocabulary lookups, then ask again.\n\n\
                     If you meant something else by \"in this codebase\", let me \
                     know — I can re-route to general knowledge retrieval."
                        .to_string()
                }
                MetalingualLocator::Conversation => {
                    "I read this as a question about something we discussed \
                     earlier in this conversation, but I couldn't find that \
                     reference. Could you quote or paraphrase the part you're \
                     asking about?"
                        .to_string()
                }
                MetalingualLocator::NamedSource(n) => format!(
                    "I read this as a question about how `{n}` uses the term, \
                     but I don't have a corpus matching `{n}` indexed locally. \
                     Run `sovereign corpus install <id>` (or the relevant \
                     ingest recipe) and ask again. Available corpora: \
                     {corpora}.",
                    corpora = context.installed_corpora_display()
                ),
                MetalingualLocator::Ambient | MetalingualLocator::Unknown => {
                    "I read this as a question about how *this system* uses \
                     the term, but I couldn't find a matching internal source. \
                     Could you tell me which source you meant — the codebase, \
                     a specific corpus, our notes?"
                        .to_string()
                }
            };
            let response_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: empty_message,
                created_at: now(),
                metadata: Some(serde_json::json!({
                    "intent": "MetalingualQuery",
                    "locator": format!("{:?}", locator),
                    "result_quality": "no_source",
                })),
                version: 0,
            };
            return Ok(Response {
                message: response_msg,
                task: None,
                metrics: None,
            });
        }

        // Build the synthesis prompt — emphasise that the answer
        // describes how the located source uses the term, and that
        // citations should attribute claims to the source.
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
        let folder_meta = self.folder_metadata_snapshot().await;
        self.rerank_conv_chunks_via_ppr(message, &mut chunks, &display_categories)
            .await;
        let conv_briefing = self
            .build_conv_briefing_block(&chunks, &display_categories)
            .await;
        let doc_context = format_scored_chunks_with_kinds(
            &chunks,
            MAX_KNOWLEDGE_CHARS,
            Some(&kinds),
            None,
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
        // Code-intelligence-in-chat (Inc 2): append call-graph traces for any
        // code-intel summary hits — the same augmentation the KnowledgeQuery and
        // DeepQuery paths run. A "this codebase" metalingual query that found a
        // code corpus lands here. Empty string (zero overhead) for non-code
        // corpora, so it is safe to run unconditionally.
        let doc_context = {
            let code_trace = crate::runtime::code_trace::build_code_trace_block(&chunks).await;
            if code_trace.is_empty() {
                doc_context
            } else {
                format!("{doc_context}\n\n{code_trace}")
            }
        };
        let knowledge_block = if conv_briefing.is_empty() {
            doc_context
        } else {
            format!("{conv_briefing}\n{doc_context}")
        };
        // Frame FIRST: it is the summary of everything the verbatim
        // turns below no longer contain, and an ordinal or "what have
        // we covered" question is answered by the frame's Topics /
        // Stated goals / Open threads sections rather than by any one
        // retrieved turn.
        let knowledge_block = match &conversation_frame {
            Some(frame) => format!(
                "MY RUNNING NOTES ON THIS CONVERSATION (folded from turns that \
                 scrolled out of view — this is what I carry forward):\n\
                 {frame}\n\n{knowledge_block}"
            ),
            None => knowledge_block,
        };
        // The instruction splits on locator because the two question
        // shapes are genuinely different. A source-anchored lookup
        // ("what does SEP mean by X") wants vocabulary-in-that-source.
        // A question about THIS thread wants recall: what was said,
        // by whom, and in what ORDER — "what did I ask first" is
        // answered by position, not by similarity, and the generic
        // "how does <source> use the term(s)" framing pushed the model
        // toward defining a word nobody asked about.
        let instruction = if matches!(locator, MetalingualLocator::Conversation) {
            "Answer from this conversation itself — my running notes above plus the \
             turns quoted below them. Quote the actual words that were used and say \
             which turn they came from. Order is meaningful: if the question asks what \
             came first, earliest, or at the start, answer from the order of the turns \
             shown, not from whichever turn looks most related. If the material above \
             does not contain the answer, say that plainly — never invent an exchange \
             that isn't there."
                .to_string()
        } else {
            format!(
                "Answer how *{locator_phrase}* uses the term(s) in this question. \
                 Quote and cite source titles. If the retrieved passages don't \
                 cover the term, say so explicitly — do not substitute generic \
                 knowledge. Source attribution is the whole point of this answer."
            )
        };
        let prompt = format!(
            "RETRIEVED FROM {locator_phrase}:\n\n{knowledge_block}\n\n\
             ════════════════════════════════════\n\n\
             Question: {message}\n\n\
             {instruction}"
        );
        // Structural honesty + attribution (contract principle 4 —
        // structure over instruction, the GK-caveat lesson): the
        // opening is decode-COMMITTED via assistant_prefix, not
        // requested in the prompt. Two shapes:
        //   - the question quotes term(s) and NONE appears in the
        //     retrieved material → commit the term-absent caveat (the
        //     prompt's "say so explicitly" was instruction-only,
        //     ~60% compliance class);
        //   - otherwise → commit the source-anchored opening so the
        //     answer is structurally about the located source.
        // assistant_prefix is decode-commit only — prepended to the
        // returned text below, same contract as GK_CAVEAT_PREFIX.
        let asked_terms = quoted_terms(message);
        let any_term_present = asked_terms.iter().any(|t| {
            let tl = t.to_lowercase();
            // The frame counts as retrieved material: a term the model
            // can see in the running notes must not draw a "does not
            // appear" caveat committed at decode time.
            let in_frame = conversation_frame
                .as_ref()
                .is_some_and(|f| f.to_lowercase().contains(&tl));
            in_frame
                || chunks
                    .iter()
                    .any(|c| c.content.to_lowercase().contains(&tl))
        });
        let committed_prefix: String = if !asked_terms.is_empty() && !any_term_present {
            format!(
                "The term {} does not appear in the material I retrieved from {locator_phrase}. ",
                asked_terms
                    .iter()
                    .map(|t| format!("\"{t}\""))
                    .collect::<Vec<_>>()
                    .join(" / ")
            )
        } else {
            format!("In {locator_phrase}: ")
        };
        let system = self.build_system_message(KNOWLEDGE_SYNTHESIS_SYSTEM, context);
        let request = CompletionRequest {
            prompt,
            system_message: Some(system),
            preferred_speed: Speed::Fast,
            max_tokens: Some(FAST_KNOWLEDGE_MAX_TOKENS as usize),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(0),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: Some(committed_prefix.clone()),
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            prompt_shape: None,
            stable_prefix_len: None,
        };

        let completion = self.inference.complete(&request).await?;
        // Prefix is decode-commit only; quote guardrail runs on the
        // full released text against exactly the evidence the model
        // saw (graceful no-op when knowledge_block is empty).
        let full_text = format!("{committed_prefix}{}", completion.text);
        let verified =
            crate::quote_verification::verify_answer_against_evidence(&full_text, &knowledge_block);
        if verified.demoted_count > 0 {
            tracing::warn!(
                demoted = verified.demoted_count,
                "metalingual: quote guardrail demoted unverified quotations"
            );
        }
        let sources: Vec<String> = chunks
            .iter()
            .filter_map(|c| c.title.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let response_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: verified.rewritten,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "intent": "MetalingualQuery",
                "locator": format!("{:?}", locator),
                "sources": sources,
                "chunks_used": chunks.len(),
                // Whether the running conversation frame was part of
                // the evidence — the difference between "answered from
                // turns still in view" and "answered from what I
                // folded away", which is otherwise unrecoverable after
                // the turn.
                "conversation_frame_used": conversation_frame.is_some(),
            })),
            version: 0,
        };
        Ok(Response {
            message: response_msg,
            task: None,
            metrics: None,
        })
    }
}

/// Terms the question explicitly quotes ('x', "x", `x`) — the things a
/// metalingual question is ABOUT. Pure; drives the structural
/// term-absent caveat above.
fn quoted_terms(message: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for quote in ['\'', '"', '`'] {
        let mut parts = message.split(quote);
        // Odd-indexed segments are inside quotes — but only when the
        // CLOSING quote exists (an unpaired apostrophe in "What's"
        // must not turn the rest of the sentence into a term).
        parts.next();
        while let Some(inside) = parts.next() {
            if parts.next().is_none() {
                break; // unpaired — not a quoted term
            }
            let t = inside.trim();
            if t.len() >= 2 && t.len() <= 60 && !t.contains('\n') {
                terms.push(t.to_string());
            }
        }
    }
    terms
}

/// Head and tail message counts kept when a conversation is too long to
/// render whole. The head is what ordinal questions ("what did I ask
/// FIRST", "how did this start") reach for; the tail is what "what have
/// we covered" and recency-flavoured asks need. The middle is elided
/// with an explicit marker rather than silently dropped — a summary the
/// user can't see the seams of is worse than a visible gap.
const CONV_LOCATOR_HEAD_MSGS: usize = 8;
const CONV_LOCATOR_TAIL_MSGS: usize = 12;

/// Per-message char cap in the rendered turn list. Generous relative to
/// the visible-history tiers because this path renders FEWER messages
/// (head+tail, not every turn) and the question is usually *about* an
/// early turn's content.
const CONV_LOCATOR_CHARS_PER_MSG: usize = 400;

/// Render the conversation's own turns as evidence chunks, in turn
/// order, each labelled with its ordinal.
///
/// This is the evidence surface for [`MetalingualLocator::Conversation`].
/// It exists because a question *about this thread* is not a retrieval
/// problem: the three conversation-memory channels (visible window,
/// compacted preamble, similarity retrieval over dropped pairs) are all
/// keyed on content similarity or recency, so an ordinal ask has no
/// semantic neighbour to match and every candidate lands under the
/// retrieval floor. Measured 2026-07-25: 0/5 recall of "what was the
/// very first topic I asked about" at conversation depths 10-42, with
/// `no_hits_above_floor` at four of the five depths.
///
/// The ordinal label is the load-bearing part — it is the only place in
/// the prompt where turn ORDER is stated explicitly, which is what lets
/// the model answer "first"/"then"/"after that" at all.
fn conversation_turns_as_chunks(msgs: &[Message]) -> Vec<corpus_engine::ScoredChunk> {
    let total = msgs.len();

    // Which indices to render: whole thread when short, else head+tail.
    let elide = total > CONV_LOCATOR_HEAD_MSGS + CONV_LOCATOR_TAIL_MSGS;
    let keep: Vec<usize> = if elide {
        (0..CONV_LOCATOR_HEAD_MSGS)
            .chain(total - CONV_LOCATOR_TAIL_MSGS..total)
            .collect()
    } else {
        (0..total).collect()
    };

    let mut out: Vec<corpus_engine::ScoredChunk> = Vec::with_capacity(keep.len() + 1);
    let mut prev: Option<usize> = None;
    for (rank, &i) in keep.iter().enumerate() {
        // Visible seam where the middle was dropped.
        if let Some(p) = prev {
            if i != p + 1 {
                out.push(conv_chunk(
                    "(elided)".to_string(),
                    format!("[turns {}-{} not shown]", p + 2, i),
                    rank,
                ));
            }
        }
        prev = Some(i);

        let m = &msgs[i];
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };
        let mut end = m.content.len().min(CONV_LOCATOR_CHARS_PER_MSG);
        while end > 0 && !m.content.is_char_boundary(end) {
            end -= 1;
        }
        let body = if end < m.content.len() {
            format!("{}...", &m.content[..end])
        } else {
            m.content.clone()
        };
        // 1-based ordinal: "turn 1" is the conversation's opening message.
        out.push(conv_chunk(format!("Turn {} ({role})", i + 1), body, rank));
    }
    out
}

/// One rendered turn as a pseudo-chunk. `corpus_id` names the source in
/// the user's terms so citations read "this conversation", not a corpus
/// id that doesn't exist. Scores descend with turn order so that any
/// downstream stable sort preserves the sequence — order is the signal
/// on this path, not relevance.
fn conv_chunk(title: String, content: String, rank: usize) -> corpus_engine::ScoredChunk {
    corpus_engine::ScoredChunk {
        content,
        title: Some(title),
        url: None,
        corpus_id: "this conversation".to_string(),
        score: 1.0 - (rank as f32) * 1e-3,
        metadata: std::collections::HashMap::new(),
        chunk_id: None,
        source_doc_id: None,
        vector_distance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        conversation_turns_as_chunks, quoted_terms, CONV_LOCATOR_CHARS_PER_MSG,
        CONV_LOCATOR_HEAD_MSGS, CONV_LOCATOR_TAIL_MSGS,
    };
    use crate::types::{Message, Role};

    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::Stream;

    use crate::error::{Error, Result};
    use crate::registry::ToolRegistry;
    use crate::runtime::Runtime;
    use crate::skills::SkillRegistry;
    use crate::traits::InferenceProvider;
    use crate::types::{
        CompletionRequest, CompletionResponse, Conversation, ConversationContext, Depth,
        ProviderCapabilities, Speed,
    };

    /// Records the synthesis prompt so a test can assert on what the
    /// model was actually shown — the only honest way to check "did
    /// the frame reach the prompt?".
    struct RecordingInference {
        prompts: Mutex<Vec<String>>,
    }

    impl RecordingInference {
        fn new() -> Self {
            Self {
                prompts: Mutex::new(Vec::new()),
            }
        }
        fn last_prompt(&self) -> String {
            self.prompts
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("a synthesis call was made")
        }
    }

    #[async_trait]
    impl InferenceProvider for RecordingInference {
        async fn complete(&self, r: &CompletionRequest) -> Result<CompletionResponse> {
            self.prompts.lock().unwrap().push(r.prompt.clone());
            Ok(CompletionResponse {
                text: "we opened on orbital mechanics.".to_string(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "recording".into(),
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
            Err(Error::NotImplemented("unused".into()))
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
        async fn embed_query(&self, _q: &str) -> Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
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

    fn runtime_with(
        inference: Arc<RecordingInference>,
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

    fn context_with(messages: Vec<Message>) -> ConversationContext {
        ConversationContext {
            conversation: Conversation {
                id: "conv-frame".to_string(),
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

    fn stored_frame() -> String {
        let mut frame = crate::conv_frame::parse(None);
        frame.set_body("Topics", "orbital mechanics, then Hohmann transfers".into());
        frame.set_body("Entities", "Kepler, Hohmann".into());
        frame.set_body("Stated goals", "user wants short answers".into());
        frame.render()
    }

    /// "What do you remember about this conversation?" must be answered
    /// from the RUNNING FRAME, not only from whichever turns happen to
    /// still be in the window. The frame is the distillation of
    /// everything the window already dropped, so a conversation long
    /// enough for the question to be interesting is exactly the case
    /// where the raw turns can't answer it.
    #[tokio::test]
    async fn conversation_locator_puts_the_running_frame_in_the_prompt() {
        let inference = Arc::new(RecordingInference::new());
        let store = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
        let runtime = runtime_with(Arc::clone(&inference), Arc::clone(&store));
        crate::traits::ConversationStore::set_conversation_frame(
            &*store,
            "conv-frame",
            &stored_frame(),
        )
        .await
        .expect("frame persisted");

        let ctx = context_with(vec![Message {
            id: "m0".into(),
            conversation_id: "conv-frame".into(),
            role: Role::User,
            content: "and what about aerobraking?".into(),
            created_at: 1_700_000_000,
            metadata: None,
            version: 0,
        }]);

        runtime
            .handle_metalingual_query(
                "what have we discussed in this conversation?",
                "conv-frame",
                &ctx,
                None,
            )
            .await
            .expect("handler succeeded");

        let prompt = inference.last_prompt();
        assert!(
            prompt.contains("MY RUNNING NOTES ON THIS CONVERSATION"),
            "frame block missing from prompt:\n{prompt}"
        );
        assert!(
            prompt.contains("Hohmann transfers"),
            "frame body missing from prompt:\n{prompt}"
        );
        assert!(
            prompt.contains("Order is meaningful"),
            "conversation locator must use the recall instruction, not the \
             vocabulary-lookup one:\n{prompt}"
        );
    }

    /// A conversation whose turns have all rolled out of view still has
    /// a frame — and that is precisely when the frame matters. The
    /// empty-chunks bail ("could you quote the part you're asking
    /// about?") must not fire over a frame we can read.
    #[tokio::test]
    async fn frame_alone_keeps_the_turn_off_the_empty_state_bail() {
        let inference = Arc::new(RecordingInference::new());
        let store = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
        let runtime = runtime_with(Arc::clone(&inference), Arc::clone(&store));
        crate::traits::ConversationStore::set_conversation_frame(
            &*store,
            "conv-frame",
            &stored_frame(),
        )
        .await
        .expect("frame persisted");

        let response = runtime
            .handle_metalingual_query(
                "what have we discussed in this conversation?",
                "conv-frame",
                // No messages at all — zero chunks from the turns.
                &context_with(Vec::new()),
                None,
            )
            .await
            .expect("handler succeeded");

        assert!(
            !response.message.content.contains("couldn't find that reference"),
            "frame-only turn took the empty-state bail: {}",
            response.message.content
        );
        assert_eq!(
            response.message.metadata.as_ref().and_then(|m| m
                .get("conversation_frame_used")
                .and_then(|v| v.as_bool())),
            Some(true),
            "metadata must record that the frame was the evidence"
        );
    }

    /// The semantic locator tier's whole value depends on this: the
    /// router commits "this is about our conversation" on a message
    /// carrying NONE of the nine literal markers, and the handler must
    /// honour that verdict instead of re-parsing the same string and
    /// reaching a different answer. Without the hint this message
    /// parses to `Ambient` and goes looking through code corpora.
    #[tokio::test]
    async fn router_locator_verdict_beats_the_string_parse() {
        let message = "what was the first thing I asked?";
        assert_eq!(
            crate::runtime::parse_metalingual_locator(message),
            crate::runtime::MetalingualLocator::Unknown,
            "fixture must carry no literal marker, else this proves nothing"
        );

        let inference = Arc::new(RecordingInference::new());
        let store = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
        let runtime = runtime_with(Arc::clone(&inference), Arc::clone(&store));

        let ctx = context_with(vec![
            Message {
                id: "m0".into(),
                conversation_id: "conv-frame".into(),
                role: Role::User,
                content: "how do ion thrusters work?".into(),
                created_at: 1_700_000_000,
                metadata: None,
                version: 0,
            },
            Message {
                id: "m1".into(),
                conversation_id: "conv-frame".into(),
                role: Role::Assistant,
                content: "they accelerate ionised propellant electrostatically.".into(),
                created_at: 1_700_000_001,
                metadata: None,
                version: 0,
            },
        ]);

        let response = runtime
            .handle_metalingual_query(
                message,
                "conv-frame",
                &ctx,
                crate::runtime::locator_hint_from_coarse(Some(
                    crate::runtime::COARSE_CONVERSATION_LOCATOR_EMBED,
                )),
            )
            .await
            .expect("handler succeeded");

        assert_eq!(
            response
                .message
                .metadata
                .as_ref()
                .and_then(|m| m.get("locator"))
                .and_then(|v| v.as_str()),
            Some("Conversation"),
            "handler must record the router's locator, not its own re-parse"
        );
        let prompt = inference.last_prompt();
        assert!(
            prompt.contains("ion thrusters"),
            "the thread's own turns must be the evidence:\n{prompt}"
        );
    }

    #[test]
    fn quoted_terms_extracts_each_quote_style() {
        assert_eq!(
            quoted_terms("How is 'sovereignty' used in this codebase?"),
            vec!["sovereignty"]
        );
        assert_eq!(
            quoted_terms("What does `EmbedFn` mean here?"),
            vec!["EmbedFn"]
        );
        assert_eq!(
            quoted_terms("Define \"mesh seal\" as the docs use it"),
            vec!["mesh seal"]
        );
    }

    #[test]
    fn quoted_terms_ignores_unquoted_and_degenerate() {
        assert!(quoted_terms("How is sovereignty used here?").is_empty());
        assert!(quoted_terms("What's a y?").is_empty()); // apostrophe noise stays out
    }

    fn msg(i: usize, role: Role, content: &str) -> Message {
        Message {
            id: format!("m{i}"),
            conversation_id: "c".to_string(),
            role,
            content: content.to_string(),
            created_at: i as i64,
            metadata: None,
            version: 0,
        }
    }

    fn thread(pairs: usize) -> Vec<Message> {
        let mut v = Vec::new();
        for p in 0..pairs {
            v.push(msg(v.len(), Role::User, &format!("question {p}")));
            v.push(msg(v.len(), Role::Assistant, &format!("answer {p}")));
        }
        v
    }

    #[test]
    fn short_thread_renders_every_turn_in_order_with_ordinals() {
        let chunks = conversation_turns_as_chunks(&thread(3));
        assert_eq!(chunks.len(), 6, "no elision below the head+tail budget");
        assert_eq!(chunks[0].title.as_deref(), Some("Turn 1 (user)"));
        assert_eq!(chunks[0].content, "question 0");
        assert_eq!(chunks[1].title.as_deref(), Some("Turn 2 (assistant)"));
        assert_eq!(chunks[5].title.as_deref(), Some("Turn 6 (assistant)"));
        // Order is the signal: scores must descend so any stable
        // downstream sort preserves the sequence.
        assert!(chunks.windows(2).all(|w| w[0].score > w[1].score));
        assert!(chunks.iter().all(|c| c.corpus_id == "this conversation"));
    }

    /// The regression this whole path exists for: at depth, the FIRST
    /// turn must survive into the evidence. Similarity retrieval scored
    /// it under the floor at every depth measured (2026-07-25).
    #[test]
    fn long_thread_keeps_the_opening_turn_and_marks_the_seam() {
        let msgs = thread(30); // 60 messages, well past head+tail
        let chunks = conversation_turns_as_chunks(&msgs);

        assert_eq!(chunks[0].title.as_deref(), Some("Turn 1 (user)"));
        assert_eq!(chunks[0].content, "question 0");

        // The seam is a real chunk so the elision is visible in the
        // prompt rather than silently swallowed. Its CONTENT names the
        // range, because that is the text the model actually reads.
        let elided: Vec<_> = chunks
            .iter()
            .filter(|c| c.title.as_deref() == Some("(elided)"))
            .collect();
        assert_eq!(elided.len(), 1, "exactly one visible seam");
        assert_eq!(elided[0].content, "[turns 9-48 not shown]");

        assert_eq!(
            chunks.len(),
            CONV_LOCATOR_HEAD_MSGS + CONV_LOCATOR_TAIL_MSGS + 1
        );
        assert_eq!(chunks.last().unwrap().title.as_deref(), Some("Turn 60 (assistant)"));
    }

    #[test]
    fn long_messages_are_truncated_not_dropped() {
        let long = "x".repeat(CONV_LOCATOR_CHARS_PER_MSG * 3);
        let chunks = conversation_turns_as_chunks(&[msg(0, Role::User, &long)]);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.ends_with("..."));
        assert_eq!(chunks[0].content.len(), CONV_LOCATOR_CHARS_PER_MSG + 3);
    }
}
