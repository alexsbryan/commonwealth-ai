// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the walk SAYS ABOUT ITSELF — the ledger, the degradations, the map
//! section, and the result that carries them.
//!
//! Split out of `ground.rs` by order ei-5c, along the seam the module's own
//! doc already draws: `ground.rs` performs the three steps, and everything
//! here is what a reader is handed afterwards. None of it is on the hot path
//! — [`WalkLedger`] is counters, [`Degradation`] is sentences, [`MapSection`]
//! is a capped list for a human — so keeping it beside the BFS made a
//! 1,815-line file out of a 600-line walk (ARCH §3.1).
//!
//! The rule for what lands here: if the walk DECIDES it, it is in `ground.rs`;
//! if the walk REPORTS it, it is here. [`Reach`] sits on that line and comes
//! here, because "how was this node reached" is the fact the map renders.
//!
//! Re-exported wholesale from [`super`], so every existing
//! `atlas::ground::WalkLedger` path still resolves (ARCH §10.6 — a re-export,
//! never a twin).

use std::collections::HashMap;

use crate::atlas_traversal::question_kind::{KindScore, KindSource};

use super::super::atoms::AtomType;
use super::super::context::ChunkRequest;
use super::super::edges::EdgeType;
use super::super::evidence_site::EvidenceSite;
use super::super::provider::AtlasProvider;
use super::{WalkSelection, MAP_NODE_CAP};
use corpus_engine_vocab::ontology::QuestionKind;

/// Which map decided the walk — recorded because a mixed-corpus query has
/// several atlases and only one can supply the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicySource {
    /// One atlas declared a navigation section; its id is here.
    Declared(String),
    /// No atlas in scope declared one, so the pre-registered table
    /// (`EPISTEMIC_INDEX.md` §2.2) applies. This is the normal case for
    /// every atlas written before ei-2-map.
    PreRegistered,
}

impl PolicySource {
    pub fn label(&self) -> String {
        match self {
            PolicySource::Declared(id) => format!("declared by {id}"),
            PolicySource::PreRegistered => "pre-registered defaults".to_string(),
        }
    }
}

/// One idea node the walk passed through — the map section's unit.
#[derive(Debug, Clone)]
pub struct MapNode {
    /// The atlas this atom belongs to.
    pub atlas: String,
    pub atom_id: String,
    pub name: String,
    pub kind: AtomType,
    /// The `entity_type` / claim subtype tag, or empty.
    pub subtype: String,
    /// 0 for a seed, 1 or 2 for a hop.
    pub hop: u8,
    /// The edge kind followed to REACH this node. `None` for a seed.
    pub via: Option<EdgeType>,
    /// The node this one was reached from. `None` for a seed.
    pub from: Option<String>,
    /// Accumulated walk weight: seed cosine × edge weight × confidence ×
    /// hop decay.
    pub score: f32,
}

/// The nodes and edges a walk traversed, for the reader.
///
/// `EPISTEMIC_INDEX.md` §4: `ask` returns cited passages **and a map
/// section** — "the idea nodes traversed, their kinds, and the edges
/// followed, so the answer can be connected and the ledger is visible".
#[derive(Debug, Clone, Default)]
pub struct MapSection {
    /// Traversed nodes, highest walk weight first, capped at
    /// [`MAP_NODE_CAP`].
    pub nodes: Vec<MapNode>,
    /// How many nodes the walk actually reached, before the cap.
    pub reached: usize,
}

impl MapSection {
    /// Was the node list cut down to fit the cap?
    pub fn truncated(&self) -> bool {
        self.reached > self.nodes.len()
    }

    /// The distinct edge kinds followed, in traversal order of first use.
    pub fn edge_kinds(&self) -> Vec<EdgeType> {
        let mut seen = Vec::new();
        for n in &self.nodes {
            if let Some(e) = n.via {
                if !seen.contains(&e) {
                    seen.push(e);
                }
            }
        }
        seen
    }
}

/// Why a walk yielded what it yielded. Every field is a decision the walk
/// made, not a measurement of the world (ARCH §9).
#[derive(Debug, Clone, Default)]
pub struct WalkLedger {
    /// Graphs offered to the walk.
    pub graphs: usize,
    /// Graphs that carry an ANN seed table. A graph without one contributes
    /// name-match seeds only, and none at all when no atom bag was loaded.
    pub graphs_with_ann: usize,
    /// Atom bags offered — the name-match seed source. Zero is legitimate
    /// (`corpus-mcp` serves without one) and is a NAMED degradation, not a
    /// quiet loss of half the seeding.
    pub bags: usize,
    /// Seed candidates before the row's kind filter.
    pub seed_candidates: usize,
    /// Seeds the walk actually started from.
    pub seeds: usize,
    /// Candidates the row's `seed.kinds` / `entity_types` / `declared`
    /// filter rejected.
    pub dropped_seed_kind: usize,
    /// Seed kinds the row lists that no candidate carried. The spec's "the
    /// walker skips absent kinds and says so in the ledger" — said about the
    /// SEED POOL, which is what the walk can observe without a scan.
    pub seed_kinds_unseen: Vec<AtomType>,
    /// Edges the row's `walk` list excluded.
    pub dropped_edge_kind: usize,
    /// Edges the walk followed.
    pub edges_followed: usize,
    /// Distinct atoms in the neighbourhood, seeds included.
    pub nodes_reached: usize,
    /// Evidence requests emitted.
    pub requests: usize,
    /// Neighbourhood atoms that carried no evidence anchor at all — they can
    /// never become a citation, and a walk that reaches only these looks
    /// exactly like a walk that reached nothing.
    pub atoms_without_evidence: usize,
    /// Seeds whose kind is [`Grain::Summary`](kernel_types::Grain::Summary).
    /// The order's third done-when: "atlas_retrieval shows Summary seeds in
    /// the yield ledger" is this counter.
    pub summary_seeds: usize,
    /// Times a Summary was reached and NOT expanded from — rule R1. Counted
    /// so the hold-out is visible at `tracing=debug` rather than being a
    /// silent branch (principle 1): a walk where this is 0 on a corpus that
    /// has Summary atoms is a walk where the hold-out did not fire.
    pub summary_expansions_suppressed: usize,
    /// Summary nodes carried out for late append — rule R3.
    pub summaries_appended: usize,
    /// Candidates the row's per-kind seed QUOTA refused
    /// ([`SeedPolicy::budgets`]) — admitted by kind, and then dropped because
    /// that kind's quota was already full. Distinct from
    /// [`Self::dropped_seed_kind`], which is the kind filter saying no: this
    /// one is the walk saying "yes, but not at another kind's expense", and
    /// the two would be indistinguishable in one counter.
    pub dropped_seed_budget: usize,
}

/// A named thing that was NOT available, stated rather than defaulted
/// (principle 6, ARCH §18.3). `ask` puts every one of these in its result
/// text; `apply_atlas_grounding` logs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Degradation {
    /// No graph in scope carries `atlas/atoms_ann.lance`. Seeding falls back
    /// to name-match over the bag, and to nothing at all without a bag.
    NoSeedTable,
    /// No atom bag was loaded, so no name-match seeds. The ANN table alone
    /// still seeds.
    NoAtomBag,
    /// The question could not be classified; the walk ran the unfiltered row.
    Unclassified(KindSource),
    /// The row lists seed kinds that nothing in the seed pool carried.
    SeedKindsUnseen(Vec<AtomType>),
    /// The walk reached atoms but none carried an evidence anchor.
    NoEvidenceAnchors,
    /// The map section was cut to [`MAP_NODE_CAP`].
    MapTruncated { reached: usize },
}

impl Degradation {
    /// One sentence, for a result body or a log line.
    pub fn sentence(&self) -> String {
        match self {
            Degradation::NoSeedTable => "no ANN seed table on any atlas in scope — the walk \
                 seeded by name match only; run `svrn atlas backfill-ann <corpus>`"
                .to_string(),
            Degradation::NoAtomBag => {
                "no atom bag loaded — name-match seeding was unavailable and the walk seeded \
                 from the ANN table alone"
                    .to_string()
            }
            Degradation::Unclassified(src) => format!(
                "question kind {} — the walk ran the unfiltered row (no seed-kind or \
                 edge-kind filter, 2 hops)",
                src.as_str()
            ),
            Degradation::SeedKindsUnseen(kinds) => format!(
                "the map's row seeds on {}, which nothing in the seed pool carried",
                kinds
                    .iter()
                    .map(|k| k.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Degradation::NoEvidenceAnchors => {
                "the walk reached idea nodes but none carried an evidence anchor, so nothing \
                 can be cited"
                    .to_string()
            }
            Degradation::MapTruncated { reached } => {
                format!("the map section shows the top {MAP_NODE_CAP} of {reached} nodes reached")
            }
        }
    }
}

/// Everything one walk produced.
#[derive(Debug, Clone)]
pub struct SummaryNode {
    /// The Summary atom this came from.
    pub atom_id: String,
    /// Where the summarised chunks live — the same site a
    /// [`ChunkRequest`] carries, so a consumer attributes a summary to
    /// the same corpus it would attribute a chunk to.
    pub site: EvidenceSite,
    /// The paraphrase. Never quotable: this is
    /// [`Grain::Summary`](kernel_types::Grain::Summary) text.
    pub text: String,
    /// The walk weight that reached it. Orders the summaries among
    /// THEMSELVES; deliberately never compared against a
    /// [`ChunkRequest::score`], because the two are not on one scale and
    /// putting them on one is the whole failure this design avoids.
    pub score: f32,
}

pub struct Grounding {
    /// Evidence requests, highest score first. Hand these to
    /// [`resolve_evidence`].
    pub requests: Vec<ChunkRequest>,
    /// Summary-grain nodes the walk reached, highest weight first, for
    /// LATE append by the consumer — rule R3.
    ///
    /// A separate field from [`Self::requests`] on purpose, and this is
    /// the load-bearing line of the whole port. Summaries do not consume
    /// [`Self::budget`], are never sorted against leaf requests, and are
    /// appended after the leaf pipeline has finished — the position that
    /// was MEASURED QA-neutral (SEP sources 86% vs an 85% no-RAPTOR
    /// baseline, 2026-06-08) after the pre-merge position measured −14
    /// points. Put them in `requests` and that result is re-opened.
    pub summaries: Vec<SummaryNode>,
    /// The row that was executed, and where it came from.
    pub kind: QuestionKind,
    pub kind_source: KindSource,
    /// The classifier's raw scores, when one ran — for the log line that
    /// makes a borderline abstain reviewable.
    pub kind_score: Option<KindScore>,
    pub policy_source: PolicySource,
    /// How many chunks the resolve step may keep — the row's `budget`.
    pub budget: usize,
    pub map: MapSection,
    pub ledger: WalkLedger,
    pub degradations: Vec<Degradation>,
}

impl Grounding {
    /// An empty result that still says why. Never a bare `Vec::new()`: a
    /// caller must be able to tell "nothing was relevant" from "the walk
    /// could not run".
    pub(super) fn empty(selection: &WalkSelection, degradations: Vec<Degradation>) -> Self {
        Self {
            requests: Vec::new(),
            summaries: Vec::new(),
            kind: selection.kind,
            kind_source: selection.kind_source,
            kind_score: selection.kind_score,
            policy_source: selection.policy_source.clone(),
            budget: 0,
            map: MapSection::default(),
            ledger: WalkLedger::default(),
            degradations,
        }
    }
}

/// Where a node was reached from, and how.
#[derive(Debug, Clone)]
pub(crate) struct Reach {
    pub(crate) weight: f32,
    pub(crate) hop: u8,
    pub(crate) via: Option<EdgeType>,
    pub(crate) from: Option<String>,
}

pub(super) fn build_map(
    neighborhood: &HashMap<(String, String), Reach>,
    graph_by_id: &HashMap<&str, &dyn AtlasProvider>,
) -> MapSection {
    let mut nodes: Vec<MapNode> = Vec::with_capacity(neighborhood.len().min(MAP_NODE_CAP));
    let mut ordered: Vec<(&(String, String), &Reach)> = neighborhood.iter().collect();
    ordered.sort_by(|a, b| {
        b.1.weight
            .partial_cmp(&a.1.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Ties broken by id so the map is deterministic across runs —
            // a map that reorders between two identical queries is not
            // evidence of anything.
            .then_with(|| a.0.cmp(b.0))
    });
    for ((atlas_id, atom_id), reach) in ordered.iter().take(MAP_NODE_CAP) {
        let Some(graph) = graph_by_id.get(atlas_id.as_str()) else {
            continue;
        };
        let Some(atom) = graph.atom(atom_id) else {
            continue;
        };
        nodes.push(MapNode {
            atlas: atlas_id.clone(),
            atom_id: atom_id.clone(),
            name: atom.name().to_string(),
            kind: atom.kind(),
            subtype: atom.subtype().to_string(),
            hop: reach.hop,
            via: reach.via,
            from: reach.from.clone(),
            score: reach.weight,
        });
    }
    MapSection {
        nodes,
        reached: neighborhood.len(),
    }
}
