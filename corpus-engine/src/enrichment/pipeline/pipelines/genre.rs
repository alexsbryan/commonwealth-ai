// SPDX-License-Identifier: AGPL-3.0-or-later
//! `AtlasGenre` — what actually differs between one atlas pipeline and another.
//!
//! # Why this exists
//!
//! The 7-phase atlas machinery is genre-agnostic: Phases 3–7 cluster, name and
//! find tensions over whatever atoms Phase 1 produced. A genre is therefore a
//! Phase-1 ontology plus a handful of strategy choices — not a pipeline.
//!
//! `configurable_atlas` already reached that conclusion and wrote it down:
//!
//! > Rather than duplicate ~26 trait-method delegations (which would silently
//! > diverge the day the atlas pipeline gains a new phase), `LiteraryAtlasPipeline`
//! > carries an optional `CustomOntology` […] leaving every downstream phase
//! > identical.
//!
//! The prebuilt genres predate that realization and are wrappers instead. A
//! wrapper cannot work here, and the reason is mechanical: `compose_phase1`
//! reads its prompt through `self.phase1_system()`, so a wrapper delegating to
//! `self.inner` gets the INNER's prompt. Every method that transitively reads
//! an overridden one has to be copied verbatim to rebind `self` — which is why
//! `conversation_atlas` held a byte-identical copy of literary's
//! `compose_phase1`, and why its terse Phase-1 RETRY silently ran under the
//! literary prompt (the copy stopped one method short). That is the divergence
//! the note above predicted, observed.
//!
//! So: one `impl Pipeline`, over a small trait of what genuinely varies. Every
//! method here has a default that reproduces the literary genre, so a new genre
//! declares only its differences and cannot inherit a stale copy of anything.
//!
//! `CustomOntology` (recipe-driven) and the prebuilt genres are now the same
//! mechanism rather than two — ARCH §10.6, one decider.

use super::super::atlas::SeedEntities;
use super::super::atlas::SeedStrategy;
use super::super::exemplar_bank::Exemplar;
use super::super::types::Phase1ChapterResult;
use super::super::types::{ChapterInput, ChatPrompt, Vocabulary};
use crate::enrichment::atlas::analysis::{CandidateContent, CorpusShape, TensionStrategy};

/// What one atlas genre changes about the shared 7-phase machinery.
///
/// Defaults reproduce the literary genre exactly. Override only what differs.
pub trait AtlasGenre: Send + Sync + std::fmt::Debug + 'static {
    /// Pipeline id as the registry and the recipe validator see it.
    fn id(&self) -> &'static str;

    /// Human-facing pipeline name.
    fn name(&self) -> &'static str;

    /// Phase-1 extraction system prompt — the ontology. The one field every
    /// genre must supply, because it is what a genre IS.
    fn phase1_system(&self) -> &'static str;

    /// System prompt for the terse Phase-1 retry after a failed chapter.
    ///
    /// Defaults to the literary terse prompt, which is what the prebuilt
    /// genres have always used (see the module note — for `conversation` that
    /// was an accident of the wrapper, preserved here deliberately so this
    /// refactor changes no bytes sent to a model; fixing it is its own change).
    fn phase1_terse_system(&self) -> &'static str {
        *super::literary_atlas::PHASE1_ATLAS_SYSTEM_TERSE
    }

    /// Genre vocabulary, or `None` to inherit the literary pipeline's.
    fn vocabulary(&self) -> Option<&Vocabulary> {
        None
    }

    /// Whether the literary-framed Phase-1b coverage top-up applies. A genre
    /// whose atoms are not literary should say no rather than be asked about
    /// characters.
    fn runs_phase1b_coverage(&self) -> bool {
        true
    }

    /// How Stage-1a seeds are obtained.
    fn seed_strategy(&self) -> SeedStrategy {
        SeedStrategy::Llm
    }

    /// Does this genre run Phase 8, the interpretive-configuration
    /// rollup? Default `true` — the literary genre's behaviour, which is
    /// what every genre had before Phase 8 became an opt-in. A
    /// recipe-ontology genre answers from `derive.configurations`
    /// (ONTOLOGY_MIGRATION §P4).
    fn runs_configuration_phase(&self) -> bool {
        true
    }

    /// How Phase-6 finds candidate tension pairs.
    fn tension_strategy(&self) -> TensionStrategy {
        TensionStrategy::Graph
    }

    /// The same question, asked of a corpus whose shape has been measured.
    ///
    /// The selector is a DERIVED facet, not a declared one
    /// (ONTOLOGY_PRIMITIVES §2 axis 5), so the genre gets the corpus's
    /// shape and returns what it wants to run. The default ignores the
    /// shape and answers [`tension_strategy`](Self::tension_strategy) — a
    /// prebuilt genre's selector is a property of the genre, not of the
    /// material it was pointed at, and I2 says a genre that declares
    /// nothing behaves as it did.
    fn derive_tension_strategy(&self, _shape: &CorpusShape) -> TensionStrategy {
        self.tension_strategy()
    }

    /// Compose the genre's own Phase-1 prompt. `None` — the default — uses the
    /// shared atlas Phase 1 (the section-extraction schema under this genre's
    /// [`phase1_system`](Self::phase1_system)), which is what a genre that
    /// differs only in ontology wants.
    ///
    /// Override when the genre's Phase 1 emits a DIFFERENT SHAPE, not just a
    /// different ontology — `engineering` extracts a flat claims envelope, so
    /// it brings its own body, schema and [`parse_phase1`](Self::parse_phase1).
    fn compose_phase1(
        &self,
        _chapter: &ChapterInput,
        _exemplars: &[&Exemplar],
        _seed: Option<&SeedEntities>,
    ) -> Option<ChatPrompt> {
        None
    }

    /// Compose the genre's terse Phase-1 retry. `None` uses the shared retry
    /// under [`phase1_terse_system`](Self::phase1_terse_system).
    fn compose_phase1_terse(&self, _chapter: &ChapterInput) -> Option<ChatPrompt> {
        None
    }

    /// Parse a Phase-1 response under the genre's own schema. `None` uses the
    /// shared section-extraction parse. A genre that overrides
    /// [`compose_phase1`](Self::compose_phase1) with a different schema MUST
    /// override this too, or it will parse its own output with the wrong reader.
    fn parse_phase1(&self, _response: &str) -> Option<crate::error::Result<Phase1ChapterResult>> {
        None
    }

    /// The genre's own Phase-6 conflict classifier, when the literary frame
    /// ("narrative tension between characters") is the wrong unit of analysis.
    /// `None` keeps the literary frame.
    fn compose_phase6_classifier(&self, _content: &CandidateContent) -> Option<ChatPrompt> {
        None
    }
}

/// The baked literary genre — every default, nothing overridden. It is the
/// reference implementation of "no divergence".
#[derive(Debug, Clone, Copy, Default)]
pub struct LiteraryGenre;

impl AtlasGenre for LiteraryGenre {
    fn id(&self) -> &'static str {
        super::literary_atlas::PIPELINE_ID
    }

    fn name(&self) -> &'static str {
        "Literary — atlas atom graph"
    }

    fn phase1_system(&self) -> &'static str {
        *super::literary_atlas::PHASE1_ATLAS_SYSTEM
    }
}
