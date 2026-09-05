// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-turn context pre-passes: evidence-id allowlist, tool
//! dossier, ambient field digests, temporal tensions.

use super::super::*;

impl Runtime {
    /// Gather the union of `ev-Tn-NNNN` handles emitted by prior
    /// `tool_decision` writes on this conversation, for sampler-side
    /// citation constraint (Tier 2 of tool-framework expansion).
    /// Returns `None` when the NoteStore isn't wired (CLI / test
    /// paths) or no prior decisions carried evidence ids — the
    /// caller's CompletionRequest stays unconstrained on the
    /// citation axis (Tier 1 prompt discipline is the only safety
    /// net on those turns).
    pub(crate) async fn gather_evidence_id_allowlist(
        &self,
        conversation_id: &str,
    ) -> Option<Vec<String>> {
        let notes = self.note_store.as_ref()?;
        let payloads = crate::memory::read_recent_tool_decisions(notes, Some(conversation_id), 32)
            .await
            .ok()?;
        let mut ids: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in payloads {
            for id in p.evidence_ids {
                if seen.insert(id.clone()) {
                    ids.push(id);
                }
            }
        }
        if ids.is_empty() {
            None
        } else {
            tracing::debug!(
                conversation_id,
                ev_id_count = ids.len(),
                "runtime: gathered evidence_id_allowlist from tool_decisions"
            );
            Some(ids)
        }
    }
    /// Tool-Mastery Layer 2 pre-pass. Computes the tool dossier
    /// (tools available + outcome history + ambient state) and
    /// stashes it on `context.tool_dossier` so
    /// `build_system_message` can splice it. Soft-fails: on any
    /// error or a relational skill the field stays `None` and the
    /// splice is a no-op — preserving today's behaviour for
    /// inner-work and for CLI/test harnesses that don't wire a
    /// NoteStore.
    pub(crate) async fn maybe_compute_tool_dossier(
        &self,
        context: &mut ConversationContext,
        conversation_id: &str,
    ) {
        let active_skill_id = self.skills.primary_skill_id_for_conversation();
        let active_skill = active_skill_id
            .as_deref()
            .and_then(|id| self.skills.skill_by_id(id))
            .cloned();
        if let Some(dossier) = crate::dossier::compute_tool_dossier(
            &self.tools,
            self.note_store.as_deref(),
            active_skill.as_ref(),
            Some(conversation_id),
        )
        .await
        {
            tracing::info!(
                conversation_id,
                skill = active_skill.as_ref().map(|s| s.id.as_str()),
                tools = dossier.tools_available.len(),
                outcomes = dossier.outcome_history.len(),
                has_note_store = self.note_store.is_some(),
                "dossier:computed_for_turn"
            );
            context.tool_dossier = Some(dossier);
        } else {
            tracing::info!(
                conversation_id,
                skill = active_skill.as_ref().map(|s| s.id.as_str()),
                has_note_store = self.note_store.is_some(),
                "dossier:skipped_for_turn"
            );
        }
    }
    /// Ambient field_model: for each corpus the turn is scoped to, read its
    /// ATLAS and splice a compact landscape digest into
    /// `context.knowledge_view_digests` — the same channel the
    /// system-prompt assembler renders (`system_message.rs`). This closes the
    /// "field_model is ambient for only 3 hardcoded views" gap: a turn scoped to
    /// sep / gutenberg / maple-house now gets THAT corpus's settled concerns, live
    /// tensions, and open questions ambiently, on every surface that builds this
    /// shared Runtime (bench, desktop, server) — not just the personal /
    /// conversational / institutional views the `KnowledgeViewManager` hardcodes.
    /// Because it lives in the shared runtime, bench and desktop gain it
    /// identically (the parity harness need not gate it — there's no seam to
    /// diverge).
    ///
    /// Runs AFTER `splice_landscape_digests`, so it APPENDS to (never clobbers)
    /// any view digests the provider produced. No-op when the turn is unscoped
    /// (`enabled_corpora` empty/None — we don't pay to scan every installed
    /// corpus's atlas) or the scoped corpus's atlas holds no `Question` atoms.
    ///
    /// ## ei-7b: the source moved, the renderer did not
    ///
    /// Until 2026-09-05 this read `field_skeleton.json` — a parallel artifact
    /// beside the index that only this function ever read. It now reads the
    /// atlas the rest of retrieval already reads, through
    /// `field_atoms::skeleton_from_atoms`, and hands the result to the SAME
    /// `render_landscape` (ARCH §10.6: one renderer, two sources; the digest
    /// text is pinned byte-identical across the two by
    /// `field_atoms::tests::digest_from_atoms_is_byte_identical_to_digest_from_the_v1_file`).
    ///
    /// Cost. The v1 read was one JSON per scoped corpus and so is this one, but
    /// `atoms.json` is a whole atlas where `field_skeleton.json` was one
    /// pipeline's output — on a large corpus that is a much bigger parse to pay
    /// on every turn. So the atlas's own census (`_summary.json`, already
    /// written by the atlas writer) is consulted first and the corpus is skipped
    /// when it reports zero `Question` atoms. A census that is ABSENT or STALE
    /// is not read as "no questions" (ARCH §18.3) — that falls through to the
    /// full read, which is correct and merely slower.
    pub(crate) async fn splice_ambient_field_digests(&self, context: &mut ConversationContext) {
        let Some(engine) = self.corpus_engine.as_ref() else {
            return;
        };
        let corpora: Vec<String> = match context.conversation.enabled_corpora.as_deref() {
            Some(c) if !c.is_empty() => c.to_vec(),
            _ => return,
        };
        // Per-corpus token budget for the ambient digest — small + prompt-bounded,
        // matching the KnowledgeViewManager's per-view budgets (300/200).
        const FIELD_DIGEST_BUDGET_TOKENS: usize = 250;
        let mut added: Vec<crate::types::LandscapeDigest> = Vec::new();
        for corpus_id in &corpora {
            let index = match engine.open_index_for_corpus(corpus_id).await {
                Ok(idx) => idx,
                Err(e) => {
                    tracing::debug!(corpus = %corpus_id, error = %e, "ambient field_model: open_index failed");
                    continue;
                }
            };
            let atlas_dir = index.path().join("atlas");
            if let Some(census) =
                corpus_engine::enrichment::atlas::read_current_atlas_summary(&atlas_dir)
            {
                let questions = census
                    .atom_counts
                    .get(&corpus_engine::enrichment::atlas::atoms::AtomType::Question)
                    .copied()
                    .unwrap_or(0);
                if questions == 0 {
                    tracing::debug!(
                        corpus = %corpus_id,
                        atoms = census.atom_count,
                        "ambient field_model: census says no Question atoms — skipped without reading the atlas"
                    );
                    continue;
                }
            }
            let atoms = match corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir) {
                Ok(f) => f.atoms,
                Err(e) => {
                    tracing::debug!(corpus = %corpus_id, error = %e, "ambient field_model: read_atlas_atoms failed");
                    continue;
                }
            };
            let skeleton =
                corpus_engine::enrichment::field_atoms::skeleton_from_atoms(corpus_id, &atoms);
            if skeleton.is_empty() {
                // `FieldSkeleton::is_empty` asks about CANONICAL questions
                // only, so an atlas whose questions are all `Open` renders
                // nothing here. That is unchanged from the v1 file — SEP's
                // skeleton carries 0 open questions and every per-article
                // `sep-<slug>` atlas carries only open ones — and it is left
                // alone deliberately: widening it would start splicing a
                // digest into per-article turns that never had one, which is a
                // behaviour change this order does not measure.
                tracing::debug!(
                    corpus = %corpus_id,
                    atoms = atoms.len(),
                    open_questions = skeleton.open_questions.len(),
                    "ambient field_model: atlas carries no canonical questions — nothing to splice"
                );
                continue;
            }
            let heading = format!("Field guide — {corpus_id}");
            let body = skeleton.render_landscape(&heading, FIELD_DIGEST_BUDGET_TOKENS);
            if !body.trim().is_empty() {
                added.push(crate::types::LandscapeDigest {
                    view_id: format!("field:{corpus_id}"),
                    body,
                });
            }
        }
        if added.is_empty() {
            return;
        }
        // Append to whatever `splice_landscape_digests` already set (Some on every
        // surface that wires the provider; `take().unwrap_or_default()` also keeps
        // the prompt-assembly `knowledge_view_digests.is_some()` invariant when no
        // provider ran — we always re-set Some below).
        let mut digests = context.knowledge_view_digests.take().unwrap_or_default();
        let field_count = added.len();
        digests.extend(added);
        context.set_landscape_digests(digests);
        // Glassbox: the same `retrieval_audit` channel the atom-enum /
        // atlas-grounding steps log to, so an operator can confirm the field
        // digest fired for this turn.
        tracing::info!(
            target: "retrieval_audit",
            scoped_corpora = corpora.len(),
            field_digests = field_count,
            "ambient field_model: spliced corpus field-skeleton digests"
        );
    }

    pub(crate) async fn maybe_splice_temporal_tensions(
        &self,
        context: &mut ConversationContext,
        user_message: &str,
    ) {
        if context.turn_register() != SkillRegister::Relational {
            return;
        }
        // Skip when there's nothing to compare against — common case
        // for casual chat under a relational skill (zero memories
        // retrieved by FTS), zero inference cost.
        if context.memories.is_empty() {
            return;
        }
        match memory::detect_temporal_tensions(
            self.inference.as_ref(),
            user_message,
            &context.memories,
        )
        .await
        {
            Ok(tensions) => {
                if !tensions.is_empty() {
                    tracing::debug!(
                        count = tensions.len(),
                        "runtime: temporal-tension pre-pass surfaced {} cue(s)",
                        tensions.len(),
                    );
                }
                context.temporal_tensions = tensions;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "runtime: temporal-tension pre-pass failed; continuing without",
                );
            }
        }
    }
}
