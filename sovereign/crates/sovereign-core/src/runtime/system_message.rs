// SPDX-License-Identifier: AGPL-3.0-or-later
//! System-message builders + the upstream gates (`detect_contradiction`,
//! `maybe_collaborate`, `apply_post_stream_refinement`) and tool-narrowing
//! that the dispatch path consults before invoking inference.

use crate::traits::*;

use super::*;

impl Runtime {
    /// Build OICP requirements for non-Fast requests. Composes two
    /// sources — active skills' declared requirements and a set of
    /// intent-implied defaults — and takes the max per-capability so
    /// a skill can always refine beyond what the intent implies
    /// (e.g. `code-review` asking for `code=3` on a ComplexTask
    /// still keeps its `code=3`, and ComplexTask's `Instruction=3`
    /// merges on top).
    ///
    /// Why intent defaults matter: without them, a user asking
    /// "is free will compatible with determinism?" with no active
    /// skill would send a bare `CompletionRequest` with no OICP,
    /// and the mesh `MeshInferenceProvider` would have nothing to
    /// match against. DeepQuery carries a real capability signal
    /// ("reasoning-heavy") that the OICP layer should see.
    ///
    /// Returns `None` when neither source produces any requirements
    /// — e.g. SimpleQuery with no skill activation; the caller keeps
    /// the request local.
    /// Compose a v0.3 [`InferenceRequirements`] for an outbound turn.
    ///
    /// Skill declarations are the baseline — they encode domain
    /// knowledge about what the skill needs (hint, latency class,
    /// context/output envelopes, privacy). Intent-level defaults fill
    /// in properties the active skills didn't declare: a
    /// `DeepQuery` with no active skill still gets a sensible
    /// `LatencyClass::Extended` hint so peer schedulers pick the
    /// right kind of node.
    ///
    /// Merge rule: skill-declared properties win over intent defaults.
    /// This mirrors the pre-v0.3 behaviour where skills could never
    /// be silently downgraded.
    ///
    /// Returns `None` when neither an active skill nor the intent
    /// produces any routing signal — the caller keeps the request
    /// local with whatever default slot policy the runtime uses.
    pub(crate) fn build_oicp(&self, intent: &Intent) -> Option<crate::oicp::InferenceRequirements> {
        let from_skills = self.skills.inference_requirements();
        let from_intent = default_oicp_for_intent(intent);

        let skills_empty = from_skills.capability_hint.is_none()
            && from_skills.latency_class.is_none()
            && from_skills.context_tokens.is_none()
            && from_skills.max_output_tokens.is_none();
        if skills_empty && from_intent.is_none() {
            return None;
        }

        // Preserve the sharding value resolved by `SkillRegistry::
        // inference_requirements` — it defaults to `MeshAllowed` and
        // flips to `LocalOnly` only when an active skill has declared
        // it (e.g., `inner-work`). Rebuilding via
        // `InferenceRequirements::new()` would silently reset to
        // `LocalOnly` (the spec default) and block every cross-mesh
        // route, so we copy through explicitly.
        let sharding = from_skills.sharding();
        let mut out = crate::oicp::InferenceRequirements::new().with_sharding(sharding);

        // Hint: skill-declared wins; intent fills in.
        let hint = from_skills
            .capability_hint
            .clone()
            .or_else(|| from_intent.as_ref().and_then(|r| r.capability_hint.clone()));
        if let Some(h) = hint {
            out = out.with_hint(h);
        }

        // Latency class: skill-declared wins; intent fills in.
        let latency_class = from_skills
            .latency_class
            .or_else(|| from_intent.as_ref().and_then(|r| r.latency_class));
        if let Some(lc) = latency_class {
            out = out.with_latency_class(lc);
        }

        // Structural envelopes: skill-declared minimums survive; the
        // intent never imposes a context/output floor on its own.
        if let Some(ctx) = from_skills.context_tokens {
            out = out.with_context_tokens(ctx);
        }
        if let Some(mo) = from_skills.max_output_tokens {
            out = out.with_max_output_tokens(mo);
        }

        Some(out)
    }

    /// The session's current sharding posture — `MeshAllowed` by default,
    /// `LocalOnly` when an active skill (e.g. `inner-work`) has declared
    /// it. Reads the exact source `build_oicp` reads (§8), so a caller
    /// that builds its own envelope but has no `base_request` to derive
    /// posture from — the evidence-loop forced-choice judge — routes with
    /// the same privacy the synthesis turn would.
    pub(crate) fn session_sharding(&self) -> crate::oicp::ShardingPrivacy {
        self.skills.inference_requirements().sharding()
    }
    /// Build a system message that includes memory context.
    pub(crate) fn build_system_message(&self, base: &str, context: &ConversationContext) -> String {
        // Invariant check: the Runtime is required to splice
        // `knowledge_view_digests` after routing (via
        // `LandscapeDigestProvider::splice_landscape_digests`). If we
        // reach system-message assembly with the field still `None`,
        // something skipped the splice — most likely a new code path
        // that builds its own ConversationContext and went straight
        // to the LLM. Debug-builds panic loudly so the oversight is
        // caught in tests; release builds proceed without the digest.
        //
        // The guard is tolerant of the no-KnowledgeView configuration
        // (the field stays `None` when `Runtime::with_landscape_digests`
        // wasn't called — e.g. unit-test harnesses). We only assert
        // when a provider is installed.
        if self.landscape_digests.is_some() {
            context.debug_assert_routed();
        }

        let mut parts = vec![base.to_string()];

        let now_utc = chrono::Utc::now();
        parts.push(today_anchor_block(&now_utc.format("%Y-%m-%d").to_string()));

        // Tool-Mastery Layer 2 — tool dossier ambient context.
        // Spliced LATE in the system message (after conversation
        // history / memories / landscape) so it sits closest to
        // the user's in-flight message in the prompt. Empirically
        // the dossier is most influential at the tail of the
        // system block — earlier placements get buried under
        // intervening sections and the model under-weights them.
        // The splice itself happens after all the other sections
        // below; we store the now_utc here so the renderer sees
        // a single consistent "now" across the whole assembly.
        let now_unix = now_utc.timestamp();

        // Conversation history. CompletionRequest is a single
        // user-turn prompt+system shape — prior assistant/user turns
        // aren't natively threaded by the inference adapter. We
        // render the last few turns into the system message so the
        // model can resolve coreference and topic continuity across
        // follow-up questions. Without this, multi-turn answers
        // literally say "I'm having trouble identifying who 'he'
        // refers to" because the synthesis prompt sees only the
        // current user message. Surfaced by
        // sovereign/bench/wikipedia_learn 2026-05-17 smoke.
        // Age-aware per-message truncation: recent turns keep more
        // fidelity (coreference + topical anchor), older turns
        // compress. See `chars_for_message_age` in runtime.rs for the
        // tiered budget. Pre-PR-M2 this passed
        // `CONV_HISTORY_CHARS_PER_MSG` uniformly; new shape passes a
        // closure.
        // Retrieval-over-history (v5 2026-05-26). Render BEFORE the
        // visible-history block — anchor-then-recent shape matches
        // standard RAG layout (retrieved context first, recent
        // dialogue last, user question lands closest to attention).
        // Comes BEFORE format_conversation_history because that block
        // already ends with the current user message and the
        // retrieval anchor needs to precede it.
        if let Some(hits) = context
            .history_retrieval_hits
            .as_ref()
            .filter(|h| !h.is_empty())
        {
            let mut section = String::from(
                "Relevant earlier turns from this conversation (selected by similarity to your current message):\n",
            );
            for h in hits {
                section.push_str(&format!(
                    "— turn ~{} (similarity {:.2}):\n{}\n\n",
                    h.turn_index, h.similarity, h.content
                ));
            }
            parts.push(section.trim_end().to_string());
        }

        // Phase 3 (budget-sensor redesign): scale the age-caps by the
        // conversation's allocation — derived from last turn's REAL
        // measured demand vs the window. Identity (×1.0) in the
        // common case; under sustained overshoot the render shrinks
        // proportionally (floor 120 chars/message) so assembly fits
        // by construction instead of leaning on the trim ladder.
        let history_scale = self.allocation_for(&context.conversation.id).history_scale;
        if let Some(history) = format_conversation_history(
            &context.conversation.messages,
            CONV_HISTORY_TURNS,
            move |age| {
                ((crate::runtime::chars_for_message_age(age) as f32 * history_scale) as usize)
                    .max(120)
            },
            context.compacted_history.as_deref(),
        ) {
            parts.push(history);
        }

        // Memories are rendered in the active skill's voice register —
        // factual skills get a flat list (pre-existing format),
        // relational skills get three confidence-banded sections so
        // the model can render its three epistemic registers
        // (history / inference / guess) when surfacing user history.
        let register = context.turn_register();
        if let Some(mem_section) = memory::format_memories_for_prompt(&context.memories, register) {
            parts.push(mem_section);
        }

        if let Some(wm) = &context.working_memory {
            if let Some(goal) = &wm.current_goal {
                parts.push(format!("Current user goal: {goal}"));
            }
            if !wm.facts.is_empty() {
                parts.push(format!("Session context:\n- {}", wm.facts.join("\n- ")));
            }
        }

        // KnowledgeView landscape digests — the person's recurring
        // terrain (clusters, fault lines, open questions) that the
        // model reads before answering. Bounded at splice time by
        // `KnowledgeViewManager::splice_into`'s per-view token budget
        // (300 + 200 in v1).
        if let Some(digests) = context.knowledge_view_digests.as_ref() {
            for d in digests {
                let body = d.body.trim();
                if !body.is_empty() {
                    parts.push(body.to_string());
                }
            }
        }

        // R3 — Temporal tensions. Surfaced under a tentative heading
        // so the model treats them as cues (it decides whether to
        // surface, the system only ensures it has the option).
        // Empty Vec is the common case (factual skill / no memories
        // / no tension found) and renders nothing.
        if !context.temporal_tensions.is_empty() {
            parts.push(render_temporal_tensions(&context.temporal_tensions));
        }

        // Marathon-graceful M3 — cumulative web-source registry.
        // Renders all URLs the user has been shown via
        // `submit_information_search` across this conversation's
        // turns. Ordered by `last_referenced_turn` descending so the
        // model sees the most-recently-relevant sources first.
        // Capped at 20 entries to keep the system message bounded on
        // long conversations; older entries roll off silently.
        if let Some(searched) = &context.conversation.searched_sources {
            if !searched.is_empty() {
                let mut sorted: Vec<&crate::types::SearchedSourceEntry> = searched.iter().collect();
                sorted.sort_by(|a, b| b.last_referenced_turn.cmp(&a.last_referenced_turn));
                sorted.truncate(20);
                let mut block = String::from(
                    "Web sources gathered so far in this conversation (most recent first):\n",
                );
                for entry in sorted {
                    let title = if entry.title.trim().is_empty() {
                        "(no title)"
                    } else {
                        entry.title.trim()
                    };
                    block.push_str(&format!(
                        "- [{}] {} — first seen turn {}\n",
                        title, entry.url, entry.first_seen_turn,
                    ));
                }
                parts.push(block);
            }
        }

        // Tool-Mastery dossier — spliced LAST so it sits at the tail
        // of the system block, closest to the user's in-flight
        // message in the assembled prompt. Earlier placements get
        // buried under intervening sections; the tail position
        // maximises salience for the model's next action.
        if let Some(dossier) = &context.tool_dossier {
            parts.push(crate::dossier::render_tool_dossier(dossier, now_unix));
        }

        // User-authored standing instructions (global persona). The
        // OUTERMOST layer — appended after everything, including the tool
        // dossier — so it sits closest to the user's in-flight message and
        // carries the most weight on *how* to respond. It is layered ON
        // TOP of the situated prompt, never replacing it: the base
        // epistemic contract and all situated context above still hold.
        // `render_custom_instructions` returns `None` for an absent/empty
        // persona, so the assembled prompt is byte-identical to the
        // no-persona case (no stray section, no trailing separator).
        // Fully visible to the user in the desktop's ProvenancePanel,
        // which renders this final assembled string.
        if let Some(block) =
            render_custom_instructions(self.inference_config.custom_instructions.as_deref())
        {
            parts.push(block);
        }

        parts.join("\n\n")
    }
    /// Run the R3 temporal-tension pre-pass before prompt assembly.
    /// Implements principle 5 of the relational voice contract:
    /// surface contradictions across time. Active only when the
    /// resolved primary skill carries `register = "relational"`;
    /// no-op for factual skills so non-relational sessions pay
    /// zero inference cost.
    ///
    /// Soft-fail by design — a malformed classifier response, an
    /// inference error, or a transport hiccup must never block a
    /// turn. The model just doesn't get the tension-surfacing cue
    /// for this turn and continues normally.
    /// Build the per-turn tool catalog for a PRE-CLASSIFICATION call
    /// site (i.e. before the router has produced an intent). Mode-only
    /// narrowing applies — default-chat sees the full catalog so the
    /// router can reason across the broadest set of options; inner-
    /// work and recipe-author surface narrowing wins because the
    /// user explicitly entered those modes.
    ///
    /// Post-classification sites should use
    /// [`Self::narrow_tools_for_intent`] instead — that picks up the
    /// intent-derived narrowing too.
    /// Pre-classification tool narrow with an explicit `active_mode`
    /// override. Dispatch sites resolve the workspace tag from the
    /// conversation row (via [`Runtime::resolve_active_mode`]) and
    /// pass it here so the router sees the right catalog at
    /// classification time.
    ///
    /// History: a prior `narrow_tools_pre_classification` consulted
    /// `SkillRegistry::primary_skill_id_for_conversation` only — the
    /// in-process registry's notion of "active workspace skill". For
    /// surfaces that store the workspace tag on the conversation row
    /// (the desktop recipe-author workspace is the load-bearing case),
    /// the registry side returns `None`. The narrow then fell back to
    /// `Unrestricted` and the router classified "[Project state]…"
    /// turns as `SimpleAction { tool: "shell" }` — the 2026-05-23
    /// silent-misroute repro. Explicit `active_mode` plumbing closes
    /// that gap; default-chat callers pass `None` and fall back to
    /// the registry-side lookup, preserving prior behaviour.
    pub(crate) fn narrow_tools_pre_classification_for_mode(
        &self,
        active_mode: Option<&str>,
    ) -> Vec<ToolDescriptor> {
        let registry_mode = self.skills.primary_skill_id_for_conversation();
        let effective_mode_owned: Option<String> = match active_mode {
            Some(m) => Some(m.to_string()),
            None => registry_mode,
        };
        let register = effective_mode_owned
            .as_deref()
            .and_then(|id| self.skills.skill_by_id(id))
            .map(|s| s.inference.register)
            .unwrap_or_else(|| self.skills.primary_skill_register());
        let policy =
            crate::intent_policy::policy_for_mode_only(register, effective_mode_owned.as_deref());
        crate::intent_policy::narrow_tools(&self.tools.descriptors(), &policy)
    }
    /// Build the per-turn tool catalog given a CLASSIFIED intent.
    /// Used by handlers that have already received their `Intent`
    /// argument from the dispatch site (e.g. the retrieval-miss
    /// diversion in `handle_knowledge_query`). Routes through
    /// `intent_policy::policy_for` so mode + register + intent
    /// each get their say.
    pub(crate) fn narrow_tools_for_intent(&self, intent: &Intent) -> Vec<ToolDescriptor> {
        // Same policy-builder discipline as
        // `narrow_tools_pre_classification`: read register
        // directly from the mode rather than from a context that
        // may or may not have the policy stashed yet.
        let policy = crate::intent_policy::policy_for(
            intent,
            self.skills.primary_skill_register(),
            self.skills.primary_skill_id_for_conversation().as_deref(),
        );
        crate::intent_policy::narrow_tools(&self.tools.descriptors(), &policy)
    }
    /// Build a system message for Primary-slot (Speed::Slow) completions.
    /// Prepends the active skill's epistemic contract before the
    /// caller-supplied base text. Skills declaring `[inference]
    /// register = "relational"` (currently `inner-work` and
    /// `personal-assistant`) get `RELATIONAL_BASE_SYSTEM_PROMPT`
    /// instead of the default `PRIMARY_BASE_SYSTEM_PROMPT`. All other
    /// skills, and sessions with no active skill, keep the prior
    /// factual contract — non-relational behavior is unchanged.
    pub(crate) fn build_primary_system_message(
        &self,
        base: &str,
        context: &ConversationContext,
    ) -> String {
        let contract = epistemic_contract_for(context.turn_register());
        self.build_system_message(&format!("{contract}\n\n{base}"), context)
    }
    /// Build a Relational/witness system message using the COMPACT
    /// contract instead of the full one. Includes the FTS-retrieved
    /// memories (rendered in three confidence-banded sections) and
    /// any temporal tensions surfaced by the upstream pre-pass.
    ///
    /// Used by `handle_expressive_query` (Relational branch) and
    /// `handle_simple` (Relational + DeepQuery branch). The full
    /// `RELATIONAL_BASE_SYSTEM_PROMPT` is too heavy for a 9B
    /// fine-tune to converge through inside a 2048-token output
    /// budget — empirically (voice-eval scenario 10, 2026-05-01)
    /// the planning trace runs past 9.8KB without ever closing
    /// `</think>`. The compact form converges in 600-1200 tokens
    /// of planning and leaves room for a 200-400-token reply.
    /// Iter4: keyword heuristic for whether the current user turn is
    /// edge-of-competence (medical / legal / financial / credentialled
    /// professional). Used to gate the edge-clause addendum in
    /// `build_compact_relational_system_message` so the prompt
    /// doesn't overflow the 9B's budget on hard-mode rich-memory
    /// turns where the edge clause isn't load-bearing.
    pub(crate) fn looks_edge_of_competence(message: &str) -> bool {
        let lower = message.to_lowercase();
        // Medical
        const MEDICAL: &[&str] = &[
            "chest pain",
            "diagnosis",
            "diagnos",
            "symptom",
            "depress",
            "anxiety",
            "doctor",
            "physician",
            "therapist",
            "medication",
            "prescription",
            "dosage",
            "is it", // catches "is it depression?"-style phrasings
            "should i see",
            "should i go to",
            "ER",
            "emergency room",
        ];
        // Legal
        const LEGAL: &[&str] = &[
            "landlord",
            "lease",
            "tenant",
            "deposit",
            "evict",
            "lawyer",
            "attorney",
            "lawsuit",
            "sue ",
            "contract",
            "court",
            "rights",
            "legally",
            "legal",
            "jurisdiction",
        ];
        // Financial / regulated professional
        const FINANCIAL: &[&str] = &[
            "tax",
            "irs",
            "mortgage",
            "refinance",
            "401k",
            "ira",
            "bankruptcy",
            "audit",
        ];
        MEDICAL.iter().any(|m| lower.contains(m))
            || LEGAL.iter().any(|m| lower.contains(m))
            || FINANCIAL.iter().any(|m| lower.contains(m))
    }
    pub(crate) fn build_compact_relational_system_message(
        &self,
        context: &ConversationContext,
        user_message: &str,
    ) -> String {
        let mut s = String::with_capacity(RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT.len() + 1024);
        s.push_str(RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT);

        // Iter2: cap rendered memories at K=3. The retrieval upstream
        // returns top-5 by similarity; rendering all five gives the
        // 9B more threads to weave and reliably blows the length cap
        // (hard-mode H01/H04/H05/H08 regressed iter1 → iter0). Three
        // is empirically the sweet spot — enough recall to ground
        // the witness move, few enough threads to keep the reply
        // tight. The full retrieval result still flows through to
        // `detect_contradiction` (Pass A sees all 5).
        //
        // (Recall bench 2026-07-08 tested K=1 to kill cross-entry
        // welding — it BACKFIRED: confab rose and faithful recall
        // halved. The model embellishes even a single memory, and
        // fewer candidates starve correct recall. The lever is a
        // binding verifier, not fewer candidates — see
        // `memory_grounding` + the expressive handler's escalating
        // regeneration.)
        const PROMPT_RENDER_CAP: usize = 3;
        let render_slice: &[Memory] = if context.memories.len() > PROMPT_RENDER_CAP {
            &context.memories[..PROMPT_RENDER_CAP]
        } else {
            &context.memories[..]
        };
        if let Some(mem_section) =
            memory::format_memories_for_prompt(render_slice, SkillRegister::Relational)
        {
            s.push_str("\n\n");
            s.push_str(&mem_section);

            // Retrieval-handling discipline (borrowed from the
            // knowledge-grounding bench: an answer may only assert what
            // the retrieved evidence supports). The entries above are
            // pulled by SIMILARITY to the user's message, not by the
            // user pointing at them — so on an oblique callback the set
            // routinely contains adjacent-but-wrong notes, and
            // sometimes the right note is absent entirely. Without this
            // block the witness treats all rendered entries as
            // established fact and welds their details together
            // (measured: 56% confabulation on the recall bench,
            // 2026-07-08 — the model attributed one note's specifics to
            // a different callback, or asserted the nearest distractor
            // when the true memory hadn't been retrieved). The rule is
            // the memory analogue of citation grounding: match ONE,
            // quote nothing you can't point to, and prefer an honest
            // gap over a confident wrong memory.
            s.push_str(
                "\n\nHow to use those entries. They were retrieved by similarity to what the user \
                 just said — they are CANDIDATES, not a confirmed match, and the right memory may \
                 not be among them. Before you refer to any of them:\n\
                 \u{2022} Identify the ONE entry the user is actually pointing to. If none clearly \
                 matches what they said, tell them you don't have that specific memory and ask them \
                 to take you back to it — do NOT reach for the closest-sounding entry.\n\
                 \u{2022} State only what is literally written in the entry you matched. Never merge \
                 details from two entries, and never add a date, name, number, place, or fact that \
                 isn't there.\n\
                 \u{2022} A plain \"I don't have the detail of that\" always beats a confident wrong \
                 memory. Misremembering their past on their behalf is the one thing that breaks \
                 trust for good.",
            );
        }

        // Iter3: universal brevity anchor (no memory-count gate).
        //
        // Iter2 gated this on `render_slice.len() >= 2`, which left
        // single-memory and zero-memory turns unconstrained — and
        // those are where the 9B small actually elaborates the
        // most (base scenario 01: witness move clean for 200 chars,
        // then a 600-char wisdom-voice paragraph). The brevity
        // discipline applies to EVERY relational synthesis, not
        // just the rich-memory case.
        //
        // The wording also explicitly names the wisdom-voice tail
        // as the cut: empirically, the small model converges on a
        // correct witness move and THEN appends a wisdom-voice
        // paragraph ("This feels like it's reaching beyond what an
        // untrained observer can usefully evaluate…"). Telling it
        // to cut that paragraph is more direct than telling it to
        // be brief.
        s.push_str(
            "\n\nReply shape. The witness move is one specific \
             observation grounded in the record (or named gap) plus, \
             at most, one real hand-back question. With multiple \
             memories, pick the ONE detail that most changes the \
             answer — don't list. If your draft ends with a \
             wisdom-voice paragraph (\"this often happens when…\", \
             \"perhaps the question isn't…\", \"someone who listens \
             for patterns over months…\"), cut that paragraph: the \
             witness move was already finished. Three short \
             sentences beat three short paragraphs.",
        );

        // Iter4: edge-of-competence addendum, gated on a keyword
        // heuristic. The edge clause is load-bearing for medical /
        // legal / financial turns (where the 9B otherwise surveys
        // the domain) but adds 600+ characters to the system prompt
        // — and on hard-mode rich-memory turns that overflows the
        // 9B's output budget and triggers a `</think>` non-close
        // (iter4 hard small H05 = 10529-char planning trace dumped
        // pre-fix). Gate keeps the prompt lean unless the addendum
        // is doing real work.
        if Self::looks_edge_of_competence(user_message) {
            s.push_str(
                "\n\nEdge-of-competence (medical, legal, financial, \
                 credentialled-professional questions): name the edge \
                 in ONE sentence, name the right kind of person to \
                 ask, stop. Do NOT survey the domain — no lists of \
                 possible causes, no jurisdictional comparisons, no \
                 general-information paragraphs. If your draft \
                 contains domain facts you'd attribute to web \
                 sources or general knowledge, you've crossed the \
                 edge — cut back to the edge call.",
            );
        }

        if !context.temporal_tensions.is_empty() {
            s.push_str("\n\n");
            s.push_str(&render_temporal_tensions(&context.temporal_tensions));
        }

        // NOTE: skill.toml `[prompts] synthesis` is intentionally
        // NOT appended here. The relational floor + brevity anchor +
        // edge clause is already at the edge of what the chat model
        // can coherently hold (35B Darwin-Q6_K_L tested 2026-05-04
        // — appending the inner-work skill's ~1500-char synthesis
        // regressed right_calibration -0.91 and right_self_honesty
        // -0.91 on the inner-work bench). Tuning the relational
        // voice contract is done by editing the constants /
        // helpers in this module (RELATIONAL_EXPRESSIVE_SYSTEM_
        // PROMPT, this function's brevity / edge appends), not by
        // expanding skill.toml. Skills can still pin a register and
        // a planner via skill.toml; the [prompts] synthesis block
        // remains live for ComplexTask via executor::prompt_overrides.

        s
    }
    /// Multi-shot Pass A: detect whether the user's current message
    /// sits in clear factual tension with their prior memories. Run
    /// as a small structured-output Fast-slot call before the
    /// witness synthesis so the synthesis prompt can include an
    /// explicit "what may be missing" block when one is warranted.
    ///
    /// The motivation is variance: on the 9B fast slot the
    /// disagreement-as-inquiry move is hit-and-miss inside a single
    /// synthesis call (sometimes the model surfaces the prior,
    /// sometimes it just observes the current message). Decomposing
    /// the decision (Pass A: structured "is there a contradiction?")
    /// from the writing (Pass B: witness reply with the
    /// contradiction context already surfaced) makes the
    /// disagreement move deterministic when the evidence supports
    /// it.
    ///
    /// Soft-fails to `None` on inference error, JSON parse failure,
    /// or `contradiction=false` — the caller falls back to a
    /// single-shot witness reply without a contradiction cue. This
    /// keeps the relational path strictly additive: Pass A only ever
    /// improves the response, never blocks it.
    pub(crate) async fn detect_contradiction(
        &self,
        user_message: &str,
        memories: &[Memory],
    ) -> Option<ContradictionCheck> {
        if memories.is_empty() {
            return None;
        }
        let memory_text = memories
            .iter()
            .map(|m| format!("- {}", m.content))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "You are checking whether what the user just said sits in tension with \
             what they've said before in a way a witness would surface kindly.\n\
             \n\
             Prior memories about this person:\n{memory_text}\n\
             \n\
             User's current message:\n{user_message}\n\
             \n\
             Output JSON only: {{\"contradiction\": bool, \"prior_evidence\": \"...\", \
             \"current_claim\": \"...\"}}.\n\
             Set contradiction=true when EITHER:\n\
             (a) The user states something that factually conflicts with a prior \
             memory (e.g., \"I'm leaving this role\" then \"plan a growth roadmap \
             for this role\"); OR\n\
             (b) The user's current framing omits a pattern across recent memories \
             they appear to be unaware of (e.g., several memories of being short \
             with someone, followed by \"they blew up for absolutely no reason\").\n\
             Pure new emotional content with NO conflicting or omitted prior context \
             does NOT count.\n\
             When true: prior_evidence is ONE sentence quoting or summarising the \
             relevant memory or pattern; current_claim is ONE sentence summarising \
             what the user just said. When false, both strings can be empty."
        );

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "contradiction": {"type": "boolean"},
                "prior_evidence": {"type": "string"},
                "current_claim": {"type": "string"},
            },
            "required": ["contradiction", "prior_evidence", "current_claim"],
            "additionalProperties": false,
        });

        let mut req = CompletionRequest::new(&prompt).with_speed(Speed::Fast);
        req.max_tokens = Some(256);
        req.temperature = Some(0.0);
        req.structured_output = Some(schema);
        req.enable_thinking = Some(false);

        match self.inference.complete(&req).await {
            Ok(resp) => {
                // Same strip-think convention as the production path —
                // structured-output requests still pass through any
                // thinking-mode tag emission.
                let cleaned = crate::title::strip_thinking_response(&resp.text);
                match serde_json::from_str::<ContradictionCheck>(cleaned.trim()) {
                    Ok(c) if c.contradiction && !c.prior_evidence.is_empty() => Some(c),
                    Ok(_) => None,
                    Err(e) => {
                        tracing::debug!(
                            error = %e,
                            raw_chars = cleaned.len(),
                            "contradiction-check: parse failure, soft-fail to None"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "contradiction-check: inference soft-fail");
                None
            }
        }
    }
    /// Epistemic humility hook: audit the just-produced answer against
    /// its evidence and, if the model judges a specific external source
    /// would materially sharpen the answer, surface an
    /// [`InformationRequest`] card via the approval channel. If the
    /// user pastes content, re-synthesise the answer with that content
    /// folded in. Otherwise return the original response unchanged.
    ///
    /// **Pure-additive**: never makes the answer worse than the
    /// corpus-only baseline — any failure (inference error, parse
    /// failure, user skip) falls back to `response` unchanged. Gated
    /// by `InferenceConfig::auto_collaborate` (default on) so the
    /// whole path is a no-op when disabled.
    ///
    /// Callers pass `evidence` as a plain-text summary of whatever
    /// corpus material grounded the original answer. Empty string is
    /// acceptable (e.g. when corpus retrieval returned nothing).
    pub async fn maybe_collaborate(
        &self,
        conversation_id: &str,
        question: &str,
        response: &str,
        evidence: &str,
    ) -> String {
        // Synchronous (non-streaming) path: the routing-events
        // sink is wired but no live streaming session_id exists
        // here, so narration chips for the gap-check are skipped.
        // The user is awaiting a `Response` return rather than
        // staring at a streaming chat surface, so the chip is
        // less load-bearing on this path.
        //
        // Flatten the `RefinementOutcome` enum back to a plain
        // `String` so non-streaming callers (SimpleQuery,
        // ComplexTask, KnowledgeQuery sync, tauri command) keep
        // the original contract: any non-`Refined` outcome means
        // "use the original answer." The stuck-UI bug the enum
        // was introduced to fix only affects the streaming
        // post-stream refinement path, which dispatches the
        // typed outcome via `run_post_stream_refinement` below.
        match run_collaboration(
            self.inference.as_ref(),
            self.approval.as_ref(),
            &self.inference_config,
            conversation_id,
            question,
            response,
            evidence,
            None,
            None,
        )
        .await
        {
            crate::runtime::collaboration::RefinementOutcome::Refined(text) => text,
            crate::runtime::collaboration::RefinementOutcome::NotAttempted
            | crate::runtime::collaboration::RefinementOutcome::NoChange
            | crate::runtime::collaboration::RefinementOutcome::Failed { .. } => {
                response.to_string()
            }
        }
    }
    /// Post-stream refinement hook: runs the gap check against the
    /// already-streamed answer; if the user pastes content, overwrites
    /// the saved assistant message and emits a `message-refined` event
    /// so the UI can replace the bubble. Returns `Some(refined_text)`
    /// when refinement produced new content, `None` otherwise.
    ///
    /// Delegates to `run_post_stream_refinement` so the streaming
    /// spawn (which doesn't hold `&self`) and tests share one code
    /// path.
    pub async fn apply_post_stream_refinement(
        &self,
        conversation_id: &str,
        message_id: &str,
        question: &str,
        original_content: &str,
        evidence: &str,
        original_metadata: Option<serde_json::Value>,
    ) -> Option<String> {
        run_post_stream_refinement(
            self.inference.as_ref(),
            self.approval.as_ref(),
            self.store.as_ref(),
            &self.inference_config,
            conversation_id,
            message_id,
            question,
            original_content,
            evidence,
            original_metadata,
            // Test/CLI entrypoint: no live session_id available
            // here. The streaming-spawn path passes its own
            // routing_events + session_id so the user actually
            // sees the gap-check chips — and its own
            // RefinementGuard when the turn was gate-released.
            None,
            None,
            None,
        )
        .await
    }
}

/// Render the user's global "custom instructions" / persona as the
/// outermost system-prompt layer. Returns `None` for an absent or
/// whitespace-only persona so the assembled prompt is byte-identical to
/// the no-persona case (no stray section, no trailing separator).
/// Append-only by construction: the caller pushes this AFTER every
/// situated/tool section, so it augments the base contract, never
/// displaces it.
pub(crate) fn render_custom_instructions(ci: Option<&str>) -> Option<String> {
    let ci = ci?.trim();
    if ci.is_empty() {
        return None;
    }
    Some(format!(
        "The user has provided these standing instructions for how you \
         should respond. Honour them unless they conflict with a safety \
         or grounding rule above:\n{ci}"
    ))
}

#[cfg(test)]
mod custom_instructions_tests {
    use super::render_custom_instructions;

    #[test]
    fn none_persona_renders_nothing() {
        assert_eq!(render_custom_instructions(None), None);
    }

    #[test]
    fn empty_or_whitespace_persona_renders_nothing() {
        // The byte-identical guarantee: empty / whitespace yields no
        // section, so the assembled prompt matches the no-persona case.
        assert_eq!(render_custom_instructions(Some("")), None);
        assert_eq!(render_custom_instructions(Some("   \n\t ")), None);
    }

    #[test]
    fn nonempty_persona_is_rendered_and_trimmed() {
        let out = render_custom_instructions(Some("  Be concise.  "))
            .expect("non-empty persona must render");
        assert!(out.contains("Be concise."));
        // Trimmed — no leading/trailing whitespace from the raw field.
        assert!(!out.contains("  Be concise.  "));
        assert!(out.contains("standing instructions"));
    }
}
