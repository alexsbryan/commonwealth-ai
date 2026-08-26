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
    /// Verbatim passages the grounding gate RELEASED to ground this
    /// answer, in release order — the citation the reader can open.
    ///
    /// Empty on every turn that did not go through the citation path
    /// (abstentions, legacy-ladder releases, parametric answers). Not a
    /// duplicate of [`Holding`]: a holding is a CLAIM plus its basis and
    /// carries no passage, while these are the source spans themselves,
    /// each bound to the one chunk it was copied out of.
    #[serde(default)]
    pub citations: Vec<ReleasedCitation>,
}

impl EpistemicState {
    /// The ledger rows for a released turn, projected from the answer itself.
    ///
    /// This module's first line calls an [`EpistemicState`] *"the answer as a
    /// typed object"*, and since rung `nc-20-turn-adoption` the citation half
    /// of that claim is literal: the rows come from the
    /// [`kernel_types::Answer`]'s own citations — each already vouched for by
    /// the seal it was minted against — rather than from a second list
    /// assembled next to it.
    ///
    /// `headings` is index-parallel to [`kernel_types::Answer::citations`] and
    /// may be shorter; a missing entry reads as no heading, which is the same
    /// legitimate silence [`ReleasedCitation::locator`] already documents.
    /// A citation that cannot become an openable row is DROPPED — see
    /// [`ReleasedCitation::released`].
    pub fn citations_of(
        answer: &kernel_types::Answer,
        headings: &[Option<String>],
    ) -> Vec<ReleasedCitation> {
        answer
            .citations()
            .iter()
            .enumerate()
            .filter_map(|(i, c)| ReleasedCitation::released(c, headings.get(i).cloned().flatten()))
            .collect()
    }
}

/// One verbatim passage the gate released, with the handle needed to
/// open it in a reading surface.
///
/// WHY THIS EXISTS SEPARATELY FROM `Provenance::Corpus`. That variant
/// binds ONE `chunk_id` to one claim, and claims do not carry a passage
/// binding — the gate's [`Holding`]s are built per claim from text and a
/// support verdict, so attaching a chunk to them would attribute a
/// passage the claim was never individually verified against. Since
/// multi-quote citations became the default (2026-08-05) a released
/// citation routinely spans two chunks, which no single-`chunk_id`
/// field can honestly represent. The passage is the thing that has a
/// chunk; so the passage is what carries it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleasedCitation {
    /// The passage text as released, verbatim from the source.
    pub text: String,
    /// Human section heading for the passage ("CHAPTER VII"), when the
    /// corpus's chunk→section join resolves one. `None` is common and
    /// legitimate: no section structure, or an unjoined manifest. Never
    /// invented — a citation pointing at the wrong chapter is worse
    /// than one pointing nowhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    /// The passage this citation was copied out of — what a reading
    /// surface dereferences to open it.
    ///
    /// A row exists only when the quote matched as one contiguous run
    /// inside ONE chunk, which is the same condition that licenses a
    /// locator's chunk binding. A cross-boundary or partial match
    /// produces no citation row at all rather than one pointing
    /// somewhere plausible — refusal over guess, as the locator does.
    pub target: CitationTarget,
}

impl ReleasedCitation {
    /// Project one vouched-for [`kernel_types::Citation`] onto the wire.
    ///
    /// The ONE place a kernel citation becomes a ledger row (ARCH §10.6), and
    /// the only place [`CitationTarget`] is read back out of a
    /// [`kernel_types::Origin`]. Minted 2026-08-21, rung `nc-20-turn-adoption`:
    /// before it, the gate hand-filled this struct beside the answer's own
    /// citations and the two could disagree with nothing to catch it.
    ///
    /// `None` — a row that is not emitted at all — whenever the citation does
    /// not point into a corpus with a numeric chunk handle: a web fetch, an
    /// attachment, a tool output, or a corpus locator that is not a chunk id.
    /// That is the same refusal the field's own doc states for a cross-boundary
    /// match: a row here promises that clicking it opens the passage quoted,
    /// and a row that cannot keep the promise is worse than no row (§18.3).
    ///
    /// `heading` is the human section title ("CHAPTER VII"). It is a SEPARATE
    /// argument rather than something read off the citation because the kernel
    /// [`kernel_types::Locator`] is the machine handle — which span inside the
    /// document — and the two facts fail independently: a corpus with no
    /// section structure yields a handle and no heading, and a synthetic chunk
    /// yields neither.
    pub fn released(
        citation: &kernel_types::Citation,
        heading: Option<String>,
    ) -> Option<ReleasedCitation> {
        let kernel_types::Source::Corpus {
            corpus, locator, ..
        } = &citation.source().source
        else {
            return None;
        };
        Some(ReleasedCitation {
            text: citation.quote().to_string(),
            locator: heading,
            target: CitationTarget {
                corpus_id: corpus.as_str().to_string(),
                chunk_id: locator.as_str().parse().ok()?,
            },
        })
    }
}

/// The structurally-unique handle for one passage: `(corpus, chunk)`.
///
/// ONE definition of this pair (ARCH_PRINCIPLES §10.6). The gate carries
/// it alongside evidence while resolving quotes, and it ships on every
/// [`ReleasedCitation`]; both are the same handle, so they are the same
/// type rather than two structs that agree today.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CitationTarget {
    /// Corpus holding the passage.
    pub corpus_id: String,
    /// Stable chunk id within that corpus.
    pub chunk_id: u64,
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

/// The kind of demand facet. The deterministic v1 producer emits
/// Query/SubQuestion/Entity; the LLM demand plan (initiative I4) adds
/// Stance and Section — additive serde, so old ledgers still parse.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DemandFacet {
    /// The user's question itself — always present.
    Query,
    /// A sub-question from deterministic decomposition.
    SubQuestion,
    /// A named entity the question mentions.
    Entity,
    /// A pole of a stance contrast the demand planner detected — the
    /// answer should cover BOTH sides of a contested axis (I4).
    Stance,
    /// A document section the answer likely lives in (e.g. "criticism",
    /// "legacy") — from the demand planner's section terms (I4).
    Section,
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
