//! The epistemic ledger — the answer as a typed object.
//!
//! Every answer turn produces an [`EpistemicState`]: a structured
//! account of what the question needed ([`Demand`]s), what the answer
//! asserts and on what basis ([`Holding`]s), what remains uncovered
//! ([`Gap`]s, each carrying acquisition conjectures), and a derived
//! [`TurnVerdict`]. The ledger is assembled by DETERMINISTIC collation
//! of judgments the turn already computes (grounding-gate claim
//! verdicts, memory recall bands, general-knowledge signals) — never
//! by an additional model pass — and persists on
//! `Message.metadata.epistemic_state`. Design:
//! `sovereign/docs/EPISTEMIC_STATE.md`.
//!
//! Wire-stability: these types are serialized into message metadata
//! and read by the desktop/mobile projections. Additive changes only;
//! bump [`EpistemicState::version`] when readers must opt in.

use serde::{Deserialize, Serialize};

/// Schema version stamped on every assembled ledger.
pub const EPISTEMIC_STATE_VERSION: u32 = 1;

/// The typed epistemic account of one answer turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EpistemicState {
    /// Ledger schema version ([`EPISTEMIC_STATE_VERSION`]).
    pub version: u32,
    /// What the question needed — the coverage contract.
    #[serde(default)]
    pub demands: Vec<Demand>,
    /// What the answer asserts, per claim, with basis + verification.
    #[serde(default)]
    pub holdings: Vec<Holding>,
    /// Demands no holding covers: the honest residue, with conjecture.
    #[serde(default)]
    pub gaps: Vec<Gap>,
    /// Derived from holdings + gaps by a pure function — never
    /// asserted by a model (design invariant I2).
    pub verdict: TurnVerdict,
}

/// One facet of what the question needs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Demand {
    /// The kind of facet this demand represents.
    pub facet: DemandFacet,
    /// The facet text (the query itself, a sub-question, or an
    /// entity's surface form).
    pub text: String,
    /// How far the evidence pool got toward covering this facet.
    pub covered: CoverageLevel,
}

/// The kind of demand facet. Deterministic v1 facets only; stance and
/// section facets arrive with the LLM demand plan (initiative I4).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DemandFacet {
    /// The user's question itself — always present.
    Query,
    /// A sub-question from deterministic decomposition.
    SubQuestion,
    /// A named entity the question mentions.
    Entity,
}

/// How far the evidence pool got toward covering a demand.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoverageLevel {
    /// A holding (a verified claim) cites evidence matching this facet.
    Supported,
    /// The pool contains matching evidence but no holding cites it.
    Retrieved,
    /// Nothing in the pool matches this facet.
    Absent,
}

/// One claim the answer asserts, with its basis and verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Holding {
    /// The claim text (from the grounding gate's claim extractor).
    pub claim: String,
    /// Where the claim's support comes from.
    pub provenance: Provenance,
    /// What verification the claim survived.
    pub verification: Verification,
}

/// The basis of a holding — a closed set (ARCH §2.1): no claim ships
/// without one of these, and rendering paths match on this enum so a
/// memory recall can never render as document evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Supported by installed corpus evidence.
    Corpus {
        /// Corpus the supporting evidence lives in, when attributable
        /// to ONE corpus (single-corpus pools — the common sealed
        /// notebook case). `None` = verified against a multi-corpus
        /// pool; per-claim corpus binding arrives with claim-level
        /// search retention (initiative I2).
        corpus_id: Option<String>,
        /// Chunk id within the corpus (`(corpus_id, chunk_id)` is the
        /// structurally-unique citation handle). `None` when the gate
        /// verified against the pool without a single-chunk binding.
        chunk_id: Option<u64>,
    },
    /// Recalled from the user-memory pool, with its honesty band.
    Memory {
        /// Epistemic band derived from the memory's confidence.
        band: MemoryBand,
        /// Id of the memory entry recalled.
        entry_id: String,
    },
    /// Asserted from the model's parametric knowledge, not sources.
    GeneralKnowledge,
    /// Produced by a deterministic tool (e.g. the numeric audit's
    /// tool-emitted figures) — the system, not the model, originated it.
    ToolDerived {
        /// Name of the originating tool.
        tool: String,
    },
}

/// Epistemic band of a recalled memory, derived from stored
/// confidence at recall time (thresholds live beside the memory
/// formatter — one derivation, prompt and ledger agree).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryBand {
    /// The user stated this directly.
    ToldDirectly,
    /// Inferred from earlier conversations.
    Inferred,
    /// Low-confidence — must be flagged as a guess if surfaced.
    Tentative,
}

/// What verification a holding survived. `FailOpen` is honesty about
/// the memory verifier's deliberate availability posture (design
/// decision D5): the recall shipped without confirmation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// A verifier confirmed this claim against sealed evidence.
    Verified,
    /// The gate's first check failed; the claim was revised/annotated.
    FailedOnce,
    /// The verifier errored or declined and the claim shipped unchecked.
    FailOpen,
    /// No verifier ran on this claim (e.g. un-gated surfaces).
    Unverified,
}

/// A demand the evidence never covered, with acquisition conjecture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Gap {
    /// Index into [`EpistemicState::demands`] of the uncovered demand.
    pub demand_idx: usize,
    /// Human-readable statement of what's missing.
    pub statement: String,
    /// Whether the whole topic or just this claim is uncovered.
    pub coverage: GapCoverage,
    /// Ranked acquisition conjectures — where the user could fetch
    /// what would fill this gap. Structurally catalog-only (design
    /// invariant I4): never model-invented.
    #[serde(default)]
    pub routes: Vec<AcquisitionRoute>,
}

/// The cross-corpus coverage verdict behind a gap, from the
/// nearest-chunk-cosine probe over installed corpora.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GapCoverage {
    /// No installed corpus has any region near this topic — the
    /// remedy is acquiring a source, not rephrasing.
    TopicUncovered,
    /// An installed corpus covers the topic but not this specific
    /// claim — the remedy is a deeper source or the web.
    ClaimUncovered,
}

/// One acquisition conjecture: a concrete place the user could go to
/// fetch the missing knowledge. Every variant maps to an affordance
/// the product already ships (Library catalog, Add-sheet connectors,
/// web search, paste).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionRoute {
    /// Install a catalog recipe corpus.
    InstallRecipe {
        /// Recipe id in the registry catalog.
        recipe_id: String,
        /// Human display name of the recipe.
        name: String,
    },
    /// Connect a local folder as a knowledge source.
    ConnectFolder,
    /// Connect an Obsidian vault.
    ConnectVault,
    /// Import assistant-conversation exports.
    ImportConversations,
    /// Search the web with the suggested queries.
    WebSearch {
        /// Suggested search queries.
        queries: Vec<String>,
    },
    /// Ask the user to provide a document of the named kind.
    ProvideDocument {
        /// What kind of document would satisfy the gap
        /// (e.g. "a primary source", "the filing itself").
        kind: String,
    },
}

/// The turn-level verdict, derived purely from holdings + gaps.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnVerdict {
    /// Every holding is corpus-backed and verified.
    Grounded,
    /// Holdings mix bases (corpus + memory/general-knowledge/tool),
    /// or verification was partial.
    Mixed,
    /// The answer rests on recalled memory.
    MemoryRecall,
    /// The answer rests on parametric general knowledge.
    GeneralKnowledge,
    /// Retrieved evidence was used but no verifier ran on this turn
    /// (un-gated surface) — honesty about the absence of a check, not
    /// a judgment against the answer.
    Unverified,
    /// No supported holdings — the honest abstention state; its gaps
    /// carry the conjectures.
    CannotKnowFromHere,
}
