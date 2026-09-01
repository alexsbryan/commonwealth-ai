// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation-history atlas pipeline.
//!
//! Chat transcripts (claude.ai exports today; future: Slack, iMessage,
//! email threads via parallel extractors) are a different shape from
//! authored prose. Each chunk is a *turn-pair*:
//!
//! ```text
//! ### [2025-09-04 18:01] user
//! …user's question…
//! ### [2025-09-04 18:02] assistant
//! …assistant's reply…
//! ```
//!
//! Two structural facts drive every prompt divergence in this
//! pipeline:
//!
//! 1. **The user is the voice** — first-person `### [...] user`
//!    blocks. Their stances, decisions, plans, and questions are the
//!    load-bearing atoms; everything else exists to give them context.
//!    The user is NEVER a Person atom (same rule as obsidian_atlas's
//!    author-as-voice convention).
//! 2. **The assistant is a generation surface** — `### [...] assistant`
//!    blocks. The assistant is NEVER a Person atom. Its statements
//!    rarely matter for the user's atlas; when they do, they're
//!    tagged `attributed_to: "assistant"` so downstream filtering can
//!    isolate user-authored content.
//!
//! v1 is a forwarding wrapper over `literary_atlas`, diverging only at
//! Phase 1 — the layer where atom shape is decided. Phases 3-7 inherit
//! literary's calibration; we'll fork them lazily when the bench shows
//! they need it. Forking from day one (per obsidian_atlas's "cheaper
//! than forking later" rationale) keeps tuning commits scoped to this
//! file.
//!
//! Pipeline runs via `sovereign enrich init <corpus> --pipeline
//! conversation_atlas`. The recipe path (`conversations-anthropic`)
//! also drives in-line entity extraction via the `conversational`
//! domain; the two paths are complementary — in-line catches Person /
//! Org / Initiative quickly during ingest, the atlas pipeline runs
//! later for the full typed atom + edge graph.

use std::sync::Arc;

use super::literary_atlas::LiteraryAtlasPipeline;

/// Pipeline id exposed by the registry. Stable; the recipe + CLI
/// pass this string.
pub const PIPELINE_ID: &str = "conversation_atlas";

/// Conversation-flavored Phase 1 system preamble. Diverges from
/// `literary_atlas` (and from `obsidian_atlas`) in these load-bearing
/// places, each driven by the structural facts of chat transcripts:
///   1. Turn-block format is named upfront so the model knows the
///      `### [ts] user` / `### [ts] assistant` markers are
///      structural, not content.
///   2. The user (the voice behind `### [...] user`) is NEVER a
///      Person atom — same shape as obsidian's author/narrator rule.
///   3. The assistant (Claude / GPT / "the model") is NEVER a Person
///      atom — assistants are generation surfaces, not humans.
///   4. Timestamps and IDs (`2025-09-04`, `q3-2025`, `PR-1234`,
///      `commit abc1234`) are NEVER Person atoms — covers
///      conversation-specific artifact shapes that wouldn't appear in
///      authored prose.
///   5. Decisions / commitments are Claims with
///      `discourse_act: "commit"` — the "decision atom" the bench
///      needs for "when did I decide X and why" questions.
///   6. Attribution rules are spelled out exhaustively: user's
///      claims omit `attributed_to`; claims the user attributes to a
///      third party carry the third party's name; rare
///      assistant-authored claims carry `attributed_to: "assistant"`.
///      Load-bearing for the bench runner's attribution_mode filter.
///   7. Concept atom examples are drawn from working-professional
///      conversation vocab (`runway`, `burn rate`, `tech debt`,
///      `OKR alignment`, `prompt overlay`) instead of literary or
///      vault-essay examples.
static PHASE1_CONVERSATION_SYSTEM: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "conversation_atlas/phase1_system.md",
            include_str!("conversation_atlas_prompts/phase1_system.md"),
        )
    });

/// The conversation genre: chat transcripts, extracted under a Phase-1
/// ontology that treats the user as the voice and the assistant as a
/// generation surface. Everything downstream is the shared atlas machinery.
///
/// Until 2026-08-31 this was a `ConversationAtlasPipeline` wrapper holding 36
/// verbatim delegations to an inner `LiteraryAtlasPipeline` — 415 lines to say
/// what the three methods below say. The wrapper was not a style choice: a
/// delegate binds `self` to the INNER pipeline, so every method that reads
/// `self.phase1_system()` had to be copied to see this genre's prompt. It also
/// showed exactly the drift `configurable_atlas` predicted — the copy stopped
/// one method short of the terse Phase-1 retry, so a failed chapter was
/// re-extracted under the LITERARY prompt. That behaviour is preserved here
/// (the default `phase1_terse_system`) because a refactor may not change what
/// a model is sent; it is now visible as one defaulted method instead of
/// invisible in a missing copy.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConversationGenre;

impl super::genre::AtlasGenre for ConversationGenre {
    fn id(&self) -> &'static str {
        PIPELINE_ID
    }

    fn name(&self) -> &'static str {
        "Conversation history — atlas atom graph"
    }

    fn phase1_system(&self) -> &'static str {
        *PHASE1_CONVERSATION_SYSTEM
    }
}

/// The conversation atlas pipeline.
pub fn pipeline() -> LiteraryAtlasPipeline {
    LiteraryAtlasPipeline::with_genre(Arc::new(ConversationGenre))
}

#[cfg(test)]
mod tests {
    use super::super::super::trait_def::Pipeline;
    use super::*;

    #[test]
    fn id_and_name_diverge_from_literary() {
        let p = pipeline();
        assert_eq!(p.id(), "conversation_atlas");
        assert!(p.name().to_lowercase().contains("conversation"));
        let lit = LiteraryAtlasPipeline::new();
        // Same opt-ins as literary_atlas — flip here when bench
        // tells us conversations need different phase coverage.
        assert_eq!(p.runs_configuration_phase(), lit.runs_configuration_phase());
        assert_eq!(
            p.runs_phase6_atlas_classifier(),
            lit.runs_phase6_atlas_classifier()
        );
        assert_eq!(p.runs_phase6_holistic(), lit.runs_phase6_holistic());
    }

    #[test]
    fn phase1_diverges_from_literary_with_conversation_specific_rules() {
        // The conversation_atlas Phase 1 preamble names the seven
        // load-bearing divergences (see PHASE1_CONVERSATION_SYSTEM
        // comment block). Pin the rule strings so a future prompt
        // revision that removes them fails loudly.
        let p = pipeline();
        let lit = LiteraryAtlasPipeline::new();
        assert_ne!(p.phase1_system(), lit.phase1_system());
        let p1 = p.phase1_system();
        // 1. Turn-block format awareness.
        assert!(p1.contains("### [YYYY-MM-DD HH:MM] user"));
        assert!(p1.contains("### [YYYY-MM-DD HH:MM] assistant"));
        // 2. User-as-voice rule.
        assert!(p1.contains("user (the speaker behind `### [...] user` blocks) is NEVER a"));
        // 3. Assistant-as-non-person rule.
        assert!(p1.contains(
            "assistant (the speaker behind `### [...] assistant` blocks)\nis NEVER a Person atom"
        ));
        // 4. Timestamps / IDs.
        assert!(p1.contains("Years, dates, timestamps, and IDs are NEVER Person"));
        // 5. Decisions via discourse_act=commit.
        assert!(p1.contains("THIS IS THE DECISION ATOM"));
        // 6. Attribution rules.
        assert!(p1.contains("attributed_to: \"assistant\""));
        // 7. Working-professional concept vocab.
        assert!(p1.contains("runway"));
        assert!(p1.contains("burn rate"));
    }

    #[test]
    fn phase3_and_phase5_still_delegate_to_literary() {
        let p = pipeline();
        let lit = LiteraryAtlasPipeline::new();
        assert_eq!(p.phase3_system(), lit.phase3_system());
        assert_eq!(p.phase5_system(), lit.phase5_system());
    }
}
