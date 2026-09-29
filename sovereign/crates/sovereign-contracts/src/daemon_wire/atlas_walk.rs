// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas walk's echo on a chat response's metadata. Moved from
//! sovereign-core's `runtime/types.rs` (phase-b pb-cli-llm-bench-move) because
//! it is wire: svrn writes it and bench reads it, and bench names no core.

/// One node the atlas walk passed through, as a serde value.
///
/// Echo of `corpus_engine_atlas_reader::ground::MapNode`, which derives
/// no `Serialize` — the same reason `MetaAtlasHitEcho` exists on the bench
/// side: the measurement schema must not move when a walk internal does. The
/// two enum-typed fields (`kind`, `via`) come across as their `label()`
/// strings so a reader of the JSON needs no vocabulary crate.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AtlasWalkNodeEcho {
    /// The atlas this atom belongs to.
    pub atlas: String,
    /// The atom's id — the thing a study joins against. An echo whose nodes
    /// carry no atom id records that a walk happened and nothing about where
    /// it went.
    pub atom_id: String,
    pub name: String,
    /// `AtomType::label()` — "entity", "claim", "summary", …
    pub kind: String,
    /// The `entity_type` / claim subtype tag, or empty.
    pub subtype: String,
    /// 0 for a seed, 1 or 2 for a hop.
    pub hop: u8,
    /// `EdgeType::label()` for the edge followed to REACH this node. `None`
    /// for a seed.
    pub via: Option<String>,
    /// The node this one was reached from. `None` for a seed.
    pub from: Option<String>,
    /// Accumulated walk weight.
    pub score: f32,
}

/// The message-metadata key [`AtlasWalkEcho`] rides under.
///
/// One name for both ends of the wire (ARCH §8). The write is
/// `runtime/streaming.rs`; the read is `sovereign-cli-llm`'s
/// `eval_cmd::atlas_walk_meta`, in another crate — which is exactly where a
/// duplicated string literal goes stale silently, because a reader looking for
/// a key nobody writes returns "no walk" and no build, test or gate says a
/// word. Neither side spells it.
pub const ATLAS_WALK_META_KEY: &str = "atlas_walk";

/// The atlas walk's evidence PATH and its counters, as one serde value.
///
/// Until this type the walk's yield existed only as the `atlas-grounding:
/// fetch ledger` tracing event, and `svrn eval run` emits no `sovereign_core`
/// tracing at all — so every measurement of atlas reach was made by reading a
/// number the run could not produce. A value the pipeline carries out is
/// observable with no subscriber in the loop, which is the same argument
/// `StepLedger` already makes for the step's accounting.
///
/// `nodes` is the path (which atoms, reached how); the counters are the walk's
/// own ledger plus what the fetch did with the requests. Both are needed:
/// "the walk reached nothing" and "the walk reached nodes the fetch could not
/// resolve" produce the same `added` and are different facts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AtlasWalkEcho {
    /// `QuestionKind::as_str()` — the navigation row that was executed.
    pub kind: String,
    /// How `kind` came to be: `classified` (the centroid race won),
    /// `abstained` (no row cleared the gates — `kind` is the unfiltered
    /// row's shape, not a routing decision), `row_inert`, `no_classifier`,
    /// `classifier_unavailable`, `caller`. Without this the echo cannot
    /// distinguish "routed to thematic" from "abstained into the
    /// unfiltered walk" — which is the difference between a routing
    /// defect and a scatter defect (measured: the ANS K1 questions all
    /// abstained, 2026-09-22, and the echo said `thematic`).
    #[serde(default)]
    pub kind_source: String,
    /// The traversed nodes, highest walk weight first, capped at
    /// `MAP_NODE_CAP`. Empty when the walk reached nothing.
    pub nodes: Vec<AtlasWalkNodeEcho>,
    /// Seeds the walk actually started from.
    pub seeds: usize,
    /// Edges the walk followed.
    pub edges_followed: usize,
    /// Distinct atoms in the neighbourhood, seeds included — the pre-cap
    /// count, so a truncated `nodes` is detectable.
    pub nodes_reached: usize,
    /// Evidence requests the walk emitted.
    pub requests: usize,
    /// Summary nodes carried out for late append (rule R3).
    pub summaries_appended: usize,
    /// Chunks the fetch actually pushed into the pool.
    pub added: usize,
    /// Candidates the resolve step considered. `considered > added` is the
    /// fetch dropping, not the walk failing.
    pub considered: usize,
}
