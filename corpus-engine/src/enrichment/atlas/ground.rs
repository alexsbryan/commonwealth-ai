// SPDX-License-Identifier: AGPL-3.0-or-later
//! The grounding walk — ONE implementation, driven by the map.
//!
//! `EPISTEMIC_INDEX.md` §1's Walk row, §2.2, §7 step 4. The mechanism was
//! never missing: [`super::context::atlas_navigate_ann`] has walked the typed
//! graph since ATLAS_STORAGE_V2. What was missing is that the walk's POLICY —
//! which atom kinds to seed on, which edges to follow, how far, how much
//! evidence to keep — was four constants in `sovereign-core`'s retrieval glue,
//! reachable by exactly one caller, tunable by nobody, and invisible to the
//! atlas that the walk was over.
//!
//! So the policy moves into the atlas's own declaration
//! ([`NavigationPolicy`], written by every pipeline since ei-2-map) and the
//! walk moves down here, where `corpus-mcp` can reach it without taking a
//! dependency the boundary-gate forbids. `sovereign-core`'s
//! `apply_atlas_grounding` and `corpus-mcp`'s `ask` are then two callers of
//! one walk rather than one walk and one absence.
//!
//! # The three steps, and what each is allowed to decide
//!
//! 1. **Which row.** [`select_walk`] classifies the question onto a
//!    [`QuestionKind`] by centroid over the map's own exemplars
//!    (`atlas_traversal::question_kind`), and reads that kind's
//!    [`WalkPolicy`]. An abstain runs [`WalkPolicy::unfiltered`] — the status
//!    quo ante, written down as data — and the ledger says which happened.
//! 2. **The walk.** [`ground`] seeds, filters the seeds by the row's kinds,
//!    expands the row's edge kinds for the row's hops, and aggregates the
//!    neighbourhood's evidence into scored [`ChunkRequest`]s. It performs no
//!    I/O beyond the ANN query and touches no index.
//! 3. **The resolve.** [`resolve_evidence`] turns requests into chunks
//!    against a caller-supplied [`EvidenceFetcher`] — two methods, because
//!    the two callers fetch very differently (a `CorpusIndex` handle here, a
//!    whole lane-scoped retrieval pipeline there) and only the FETCH differs.
//!    Every decision between a request and a chunk — scope, budget, title
//!    filter, duplicate — is made once, here.
//!
//! # Glassbox (ARCH §9)
//!
//! Every seed, hop, drop and budget decision is a field of [`WalkLedger`],
//! emitted at `debug` under the `retrieval_audit` target and returned to the
//! caller so `ask` can render it. A zero yield always says WHICH zero it is;
//! that distinction is what the SEP evidence-site defect hid behind for
//! months (see [`super::evidence_site`]).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::atlas_traversal::question_kind::{shared_classifier, KindScore, KindSource};
use crate::types::EmbedFn;
use corpus_engine_vocab::ontology::{NavigationPolicy, OntologyPolicies, QuestionKind, WalkPolicy};

use super::atoms::AtomType;
use super::context::{
    atom_verbatim_excerpt, contains_whole_word, cosine, edge_weight, AtlasContext, ChunkRequest,
};
use super::edges::EdgeType;
use super::evidence_site::{ChunkSelector, EvidenceSite};
use super::provider::AtlasProvider;

// The resolve step is the walk's other half and callers reach it through the
// same door: `ground::{ground, resolve_evidence}` rather than two module
// paths for one pipeline.
pub use super::resolve::{resolve_evidence, EvidenceFetcher, ResolveLedger, ResolvedChunk};

/// How many nodes of the traversed neighbourhood the map section carries.
///
/// The map is for a READER (`ask` renders it, a log prints it), not for the
/// fetch, so it is capped rather than complete: 64 nodes is more than any
/// answer cites and small enough that a two-hop walk over a million-atom
/// atlas cannot turn the result into a dump. Truncation is reported in the
/// ledger — a short map and a small neighbourhood must not look alike.
pub const MAP_NODE_CAP: usize = 64;

/// Over-fetch factor for a KIND-FILTERED seed pool.
///
/// A filtered row asks the ANN table for `max_seeds * SEED_OVERFETCH` nearest
/// atoms and keeps the best `max_seeds` that pass the filter. Without it, a
/// row that seeds on `Configuration` gets zero seeds whenever the twelve
/// nearest atoms happen to be Claims — the filter would subtract rather than
/// select, and a policy-driven walk would ground LESS than the constant it
/// replaced.
///
/// The unfiltered row does not over-fetch, so the path
/// `apply_atlas_grounding` has always taken is unchanged by this file.
const SEED_OVERFETCH: usize = 4;

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

/// The most summaries one walk carries out.
///
/// 8, which is not a fresh guess: it is `SOVEREIGN_RAPTOR_TOP_M`'s
/// shipped default in the injector this replaces
/// (`raptor_grounding.rs`). Carrying the number across means the volume
/// of late-appended summary text is unchanged by the port, so a lane
/// delta is attributable to the WALK reaching them rather than to more
/// of them arriving (§18.4 — validate the instrument before the result).
pub const SUMMARY_APPEND_CAP: usize = 8;

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
    fn empty(selection: &WalkSelection, degradations: Vec<Degradation>) -> Self {
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

/// The atlas ids that could hold atoms citing a chunk — the chunk → atlas id
/// derivation, in ONE place.
///
/// This is the INVERSE of [`EvidenceSite`]: that type answers "given an
/// atlas, which corpus holds its chunks", and this answers "given a chunk,
/// which atlases might describe it". It lived as `format!("{}-{}",
/// corpus_id, title)` inline in `sovereign-core`'s retrieval glue — a format
/// string that silently encoded SEP's per-article layout as a universal rule,
/// which is the same conflation `evidence_site` exists to prevent, standing
/// in the other direction.
///
/// Returns the self-hosted candidate (the chunk's own corpus) always, plus
/// the per-article candidate when the chunk carries a title. A caller drops
/// the candidates that have no atlas; both are cheap to test and neither may
/// be guessed at the call site.
pub fn candidate_atlas_ids(corpus_id: &str, title: Option<&str>) -> Vec<String> {
    let mut out = vec![corpus_id.to_string()];
    if let Some(t) = title
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != corpus_id)
    {
        // The per-article child of this corpus. `EvidenceSite::derive` reads
        // this same shape back the other way, so the two agree by
        // construction: `derive("sep-freewill").chunk_corpus() == "sep"`.
        //
        // The `t != corpus_id` guard is not hypothetical. Every chunk of
        // `brothers-karamazov-book-1` carries `title =
        // "brothers-karamazov-book-1"` (its bank file says so: that is why
        // its `expected_sources` are empty), so the inline format string this
        // function replaced minted
        // `brothers-karamazov-book-1-brothers-karamazov-book-1` on EVERY
        // literary query — an atlas id that cannot exist, warmed and then
        // dropped, once per retrieved chunk. Suppressing it removes a probe,
        // never a candidate: an atlas named `<corpus>-<corpus>` would require
        // an article inside corpus X titled X.
        out.push(format!("{corpus_id}-{t}"));
    }
    out
}

/// Which navigation map governs this walk.
///
/// The first graph in scope that DECLARED one wins; otherwise the
/// pre-registered table. A mixed-corpus query is the ambiguous case the spec
/// left open, and it is resolved by declaration-beats-default rather than by
/// merging two maps into a third that neither corpus wrote.
///
/// Note `AtlasGraph::ontology()` is `Some` only for a corpus that declared
/// TYPES (`with_ontology` drops the rest), so a built-in pipeline's atlas
/// reaches the pre-registered table here even when its `ontology.json` is on
/// disk — which is correct, because those files carry no `navigation`
/// override either.
pub fn navigation_policy_for(graphs: &[&dyn AtlasProvider]) -> (NavigationPolicy, PolicySource) {
    for g in graphs {
        if let Some(p) = g.ontology() {
            return (
                p.navigation.clone(),
                PolicySource::Declared(g.atlas_corpus_id().to_string()),
            );
        }
    }
    (NavigationPolicy::default(), PolicySource::PreRegistered)
}

/// One decision about HOW to walk, taken once: which row, from whose map,
/// on what evidence.
///
/// Bundled rather than passed as five arguments because they are one
/// decision and a caller must not be able to pair a `Thematic` kind with the
/// `Tension` row, or report a declared policy source for the defaults.
#[derive(Debug, Clone)]
pub struct WalkSelection {
    pub kind: QuestionKind,
    pub walk: WalkPolicy,
    pub kind_source: KindSource,
    /// The classifier's raw scores, when one ran — what makes a borderline
    /// abstain reviewable instead of mysterious.
    pub kind_score: Option<KindScore>,
    pub policy_source: PolicySource,
}

impl WalkSelection {
    /// The row a caller that ALREADY knows the kind wants — an `ask` argument,
    /// a test. Skips the embedder entirely and records that a caller, not a
    /// centroid, decided.
    pub fn named(kind: QuestionKind, policy: &NavigationPolicy, source: PolicySource) -> Self {
        Self {
            kind,
            walk: policy.walk(kind).clone(),
            kind_source: KindSource::Caller,
            kind_score: None,
            policy_source: source,
        }
    }

    /// The unfiltered row under a named reason — the one place
    /// "unclassified" is turned into a walk.
    fn unfiltered(
        kind_source: KindSource,
        kind_score: Option<KindScore>,
        source: PolicySource,
    ) -> Self {
        Self {
            kind: QuestionKind::Thematic,
            walk: WalkPolicy::unfiltered(),
            kind_source,
            kind_score,
            policy_source: source,
        }
    }

    /// One line naming the row and why it was chosen — for `ask`'s result
    /// text and for the log.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "{} ({}, {}): {} hops, budget {}",
            self.kind.as_str(),
            self.kind_source.as_str(),
            self.policy_source.label(),
            self.walk.hops,
            self.walk.budget
        );
        if let Some(k) = self.kind_score {
            s.push_str(&format!(" [sim {:.3}, margin {:.3}]", k.sim, k.margin));
        }
        s
    }
}

/// Classify the question and read its row.
///
/// Separated from [`ground`] so a caller that already KNOWS the kind uses
/// [`WalkSelection::named`] and skips the embedder, and so the classification
/// decision has one home.
pub async fn select_walk(
    question_embedding: &[f32],
    policy: &NavigationPolicy,
    policy_source: PolicySource,
    embed: Option<&EmbedFn>,
) -> WalkSelection {
    let Some(embed) = embed else {
        return WalkSelection::unfiltered(KindSource::ClassifierUnavailable, None, policy_source);
    };
    let Some(classifier) = shared_classifier(policy, embed).await else {
        // Two reasons, distinguished: the map declared no exemplars at all,
        // or the embedder failed. `classifiable()` answers which.
        let src = if policy.classifiable().is_empty() {
            KindSource::NoClassifier
        } else {
            KindSource::ClassifierUnavailable
        };
        return WalkSelection::unfiltered(src, None, policy_source);
    };
    match classifier.classify(question_embedding) {
        (Some(kind), score) => WalkSelection {
            kind,
            walk: policy.walk(kind).clone(),
            kind_source: KindSource::Classified,
            kind_score: score,
            policy_source,
        },
        (None, score) => WalkSelection::unfiltered(KindSource::Abstained, score, policy_source),
    }
}

/// Where a node was reached from, and how.
#[derive(Debug, Clone)]
struct Reach {
    weight: f32,
    hop: u8,
    via: Option<EdgeType>,
    from: Option<String>,
}

/// **The walk.** Seed, expand, aggregate — driven by one [`WalkPolicy`] row.
///
/// `atlases` is the atom BAG, used only for name-match seeding; passing an
/// empty slice is legitimate and named as [`Degradation::NoAtomBag`]. This is
/// what lets `corpus-mcp` walk from an `AtlasGraph` alone rather than paying
/// `load_atlas_context`'s per-call re-embed of every entity.
///
/// Async only because the ANN query awaits; the BFS stays sync over resident
/// atoms and the `edges.csr` mmap — the "hot BFS stays sync" invariant.
pub async fn ground(
    question: &str,
    question_embedding: &[f32],
    atlases: &[&AtlasContext],
    graphs: &[&dyn AtlasProvider],
    selection: &WalkSelection,
    max_seeds: usize,
) -> Grounding {
    let walk = &selection.walk;
    let mut degradations = Vec::new();
    if selection.kind_source.is_degradation() {
        degradations.push(Degradation::Unclassified(selection.kind_source));
    }
    if question_embedding.is_empty() || graphs.is_empty() {
        return Grounding::empty(selection, degradations);
    }

    let graph_by_id: HashMap<&str, &dyn AtlasProvider> =
        graphs.iter().map(|g| (g.atlas_corpus_id(), *g)).collect();

    let mut ledger = WalkLedger {
        graphs: graphs.len(),
        graphs_with_ann: graphs.iter().filter(|g| g.has_ann_seed_table()).count(),
        bags: atlases.len(),
        ..Default::default()
    };
    if ledger.graphs_with_ann == 0 {
        degradations.push(Degradation::NoSeedTable);
    }
    if atlases.is_empty() {
        degradations.push(Degradation::NoAtomBag);
    }

    // ── 1. Seeds ────────────────────────────────────────────────────────
    //
    // 1a. Vector seeds — each graph's ANN table returns nearest atom-ids
    // directly, re-scored with the canonical cosine so the BFS sees stable
    // weights. One global pool. A FILTERED row over-fetches so the filter
    // selects within a wider pool instead of subtracting from a narrow one.
    let filtered = seed_filter_is_active(walk);
    let ann_k = if filtered {
        max_seeds.saturating_mul(SEED_OVERFETCH)
    } else {
        max_seeds
    };
    let mut scored: Vec<(f32, String, String)> = Vec::new();
    for graph in graphs {
        let Some(ann) = graph.ann_seed_table() else {
            continue;
        };
        match ann.nearest_with_vectors(question_embedding, ann_k).await {
            Ok(hits) => {
                for (atom_id, vector) in hits {
                    scored.push((
                        cosine(question_embedding, &vector),
                        graph.atlas_corpus_id().to_string(),
                        atom_id,
                    ));
                }
            }
            Err(e) => tracing::warn!(
                target: "retrieval_audit",
                corpus = graph.atlas_corpus_id(),
                "ground: ANN nearest failed ({e}); corpus contributes name-match seeds only"
            ),
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(ann_k);

    // 1b. Name-match seeds over the bag — force-seeds every atom literally
    // named in the question, which a single embedding cannot rank for a
    // compound question. Unchanged from `atlas_navigate_ann`; skipped
    // entirely when no bag was loaded.
    let q_lower = question.to_lowercase();
    let mut name_seeds: Vec<(f32, String, String)> = Vec::new();
    for ctx in atlases {
        if !graph_by_id.contains_key(ctx.atlas_corpus_id.as_str()) {
            continue;
        }
        for entry in &ctx.entries {
            if entry.atom_id.is_empty() {
                continue;
            }
            let name = entry.canonical_name.trim();
            if name.len() < 4 {
                continue;
            }
            let name_lower = name.to_lowercase();
            let mut hit = contains_whole_word(&q_lower, &name_lower);
            if !hit {
                if let Some(last) = name_lower.split_whitespace().last() {
                    if last.len() >= 4 && last != name_lower {
                        hit = contains_whole_word(&q_lower, last);
                    }
                }
            }
            if !hit {
                if let Some(rest) = entry.embed_text.strip_prefix("[Argument: ") {
                    if let Some(end) = rest.find(']') {
                        let arg_name = rest[..end].trim().to_lowercase();
                        if arg_name.len() >= 4 {
                            let toks: Vec<&str> = arg_name.split_whitespace().collect();
                            for w in toks.windows(2) {
                                let phrase = format!("{} {}", w[0], w[1]);
                                if phrase.len() >= 6 && q_lower.contains(&phrase) {
                                    hit = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            if !hit {
                continue;
            }
            let s = cosine(question_embedding, &entry.embedding).max(0.6);
            name_seeds.push((s, ctx.atlas_corpus_id.clone(), entry.atom_id.clone()));
        }
    }

    // Merge, dedup by (atlas, atom), keep the max score. Name additions are
    // an intentional broadening beyond `max_seeds`.
    let mut merged: HashMap<(String, String), f32> = HashMap::new();
    for (s, cid, aid) in scored.into_iter().chain(name_seeds) {
        merged
            .entry((cid, aid))
            .and_modify(|e| {
                if s > *e {
                    *e = s;
                }
            })
            .or_insert(s);
    }
    let mut candidates: Vec<(String, String, f32)> = merged
        .into_iter()
        .filter(|((cid, _), _)| graph_by_id.contains_key(cid.as_str()))
        .map(|((cid, aid), s)| (cid, aid, s))
        .collect();
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    ledger.seed_candidates = candidates.len();

    // ── 2. The row's seed filter ────────────────────────────────────────
    let mut seen_kinds: BTreeSet<AtomType> = BTreeSet::new();
    let seeds: Vec<(String, String, f32)> = if filtered {
        let mut kept = Vec::new();
        for (cid, aid, s) in candidates {
            let Some(graph) = graph_by_id.get(cid.as_str()) else {
                continue;
            };
            let Some(atom) = graph.atom(&aid) else {
                ledger.dropped_seed_kind += 1;
                continue;
            };
            seen_kinds.insert(atom.kind());
            if seed_admits(walk, *graph, atom.kind(), atom.subtype()) {
                if kept.len() < max_seeds {
                    kept.push((cid, aid, s));
                }
            } else {
                ledger.dropped_seed_kind += 1;
            }
        }
        // Which of the row's listed kinds nothing in the pool carried. Said
        // about the pool, which is what the walk observed — not about the
        // atlas, which would need a scan.
        ledger.seed_kinds_unseen = walk
            .seed
            .kinds
            .iter()
            .copied()
            .filter(|k| !seen_kinds.contains(k))
            .collect();
        if !ledger.seed_kinds_unseen.is_empty() {
            degradations.push(Degradation::SeedKindsUnseen(
                ledger.seed_kinds_unseen.clone(),
            ));
        }
        kept
    } else {
        candidates
    };
    ledger.seeds = seeds.len();

    // ── 3. BFS over the row's edge kinds, for the row's hops ────────────
    let max_hops = walk.hops as usize;
    let edge_allowed: Option<&[EdgeType]> = (!walk.walk.is_empty()).then_some(walk.walk.as_slice());
    let mut neighborhood: HashMap<(String, String), Reach> = HashMap::new();
    for (atlas_id, atom_id, seed_score) in &seeds {
        let Some(graph) = graph_by_id.get(atlas_id.as_str()) else {
            continue;
        };
        let key = (atlas_id.clone(), atom_id.clone());
        let entry = neighborhood.entry(key).or_insert(Reach {
            weight: 0.0,
            hop: 0,
            via: None,
            from: None,
        });
        if *seed_score > entry.weight {
            entry.weight = *seed_score;
        }

        // ── R1: a Summary is a TERMINUS, never a route ──────────────────
        //
        // It is in the neighbourhood (so §5's map shows it and §4 carries
        // its text), and the BFS stops there. A summary's `EvidenceFor`
        // edges fan out over its whole subtree, so expanding from one pulls
        // that entire subtree into the neighbourhood at hop 1 — dozens of
        // leaf atoms admitted for no reason except that one paraphrase
        // matched the question. That is the displacement mechanism, and
        // this is where it is refused.
        if is_summary_grain(*graph, atom_id) {
            ledger.summary_seeds += 1;
            ledger.summary_expansions_suppressed += 1;
            continue;
        }

        let mut frontier: Vec<(String, f32)> = vec![(atom_id.clone(), *seed_score)];
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(atom_id.clone());
        let decay = 0.6_f32;

        for hop in 1..=max_hops {
            let hop_decay = decay.powi(hop as i32);
            let mut next_frontier: Vec<(String, f32)> = Vec::new();
            for (current_id, current_score) in &frontier {
                let mut consider =
                    |neighbor_id: &str, edge_type: EdgeType, conf: f32, out: &mut Vec<_>| {
                        if visited.contains(neighbor_id) {
                            return;
                        }
                        if let Some(allowed) = edge_allowed {
                            if !allowed.contains(&edge_type) {
                                ledger.dropped_edge_kind += 1;
                                return;
                            }
                        }
                        let w = edge_weight(edge_type);
                        if w <= 0.0 {
                            return;
                        }
                        let neighbor_score = current_score * w * conf * hop_decay;
                        if neighbor_score < 0.05 {
                            return;
                        }
                        ledger.edges_followed += 1;
                        let key = (atlas_id.clone(), neighbor_id.to_string());
                        let entry = neighborhood.entry(key).or_insert(Reach {
                            weight: 0.0,
                            hop: hop as u8,
                            via: Some(edge_type),
                            from: Some(current_id.clone()),
                        });
                        if neighbor_score > entry.weight {
                            entry.weight = neighbor_score;
                            entry.hop = hop as u8;
                            entry.via = Some(edge_type);
                            entry.from = Some(current_id.clone());
                        }
                        visited.insert(neighbor_id.to_string());
                        // R1 again, for a Summary reached through an edge
                        // rather than seeded: it stays in `neighborhood`
                        // (already inserted above) and does not join the
                        // next frontier.
                        if is_summary_grain(*graph, neighbor_id) {
                            ledger.summary_expansions_suppressed += 1;
                            return;
                        }
                        out.push((neighbor_id.to_string(), neighbor_score));
                    };
                for edge in graph.edges_from(current_id) {
                    consider(
                        edge.target,
                        edge.edge_type,
                        edge.confidence,
                        &mut next_frontier,
                    );
                }
                for edge in graph.edges_to(current_id) {
                    consider(
                        edge.source,
                        edge.edge_type,
                        edge.confidence,
                        &mut next_frontier,
                    );
                }
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }
    }
    ledger.nodes_reached = neighborhood.len();

    // ── 4. Aggregate evidence into requests ─────────────────────────────
    //
    // Keyed by the SITE (not a bare slug) so two atlases sharing an article
    // name under different parents cannot collide, and by the parsed
    // SELECTOR so the addressing scheme travels with the request instead of
    // being re-derived downstream.
    let mut chunk_scores: HashMap<
        (EvidenceSite, ChunkSelector),
        (f32, String, Vec<String>, Vec<String>),
    > = HashMap::new();
    let mut summaries: Vec<SummaryNode> = Vec::new();
    for ((atlas_id, atom_id), reach) in &neighborhood {
        let Some(graph) = graph_by_id.get(atlas_id.as_str()) else {
            continue;
        };
        // ── R2: a Summary's reach NEVER scores a leaf request ───────────
        //
        // The loop below does `entry.0 += reach.weight` for every chunk an
        // atom cites, and `requests` is then sorted by that sum. A Summary
        // cites its entire subtree, so letting it accumulate here adds one
        // weight to dozens of chunks at once and re-ranks the evidence pool
        // toward whichever subtree happened to match — the −14pt SEP
        // source-coverage regression of 2026-06-08, reproduced inside the
        // walk. Its text leaves by the other door (R3) instead.
        //
        // Note this also keeps Summary atoms out of `atoms_without_evidence`:
        // an edges-omitted Summary (every SEP article but
        // `computational-complexity`, whose tree columns are absent from the
        // published artifact) is not a walk that failed to find anchors.
        if is_summary_grain(*graph, atom_id) {
            let text = summary_text(*graph, atom_id);
            if !text.is_empty() {
                summaries.push(SummaryNode {
                    atom_id: atom_id.clone(),
                    site: graph.site().clone(),
                    text,
                    score: reach.weight,
                });
            }
            continue;
        }
        let evidence = graph.atom_evidence(atom_id);
        if evidence.is_empty() {
            ledger.atoms_without_evidence += 1;
            continue;
        }
        let verbatim = atom_verbatim_excerpt(*graph, atom_id);
        for ev in evidence {
            let chunk_id = ev.chunk_id().trim();
            if chunk_id.is_empty() {
                continue;
            }
            let preview = ev.passage_preview().trim();
            let key = (graph.site().clone(), ChunkSelector::parse(chunk_id));
            let entry = chunk_scores.entry(key).or_insert((
                0.0,
                preview.to_string(),
                Vec::new(),
                Vec::new(),
            ));
            entry.0 += reach.weight;
            if preview.len() > entry.1.len() {
                entry.1 = preview.to_string();
            }
            entry.2.push(atom_id.clone());
            if let Some(line) = verbatim.as_ref() {
                if !entry.3.iter().any(|existing| existing == line) {
                    entry.3.push(line.clone());
                }
            }
        }
    }
    if ledger.nodes_reached > 0 && ledger.atoms_without_evidence == ledger.nodes_reached {
        degradations.push(Degradation::NoEvidenceAnchors);
    }

    let mut requests: Vec<ChunkRequest> = chunk_scores
        .into_iter()
        .map(
            |((site, selector), (score, preview, motivating, verbatim))| ChunkRequest {
                site,
                selector,
                passage_preview: preview,
                score,
                motivating_atoms: motivating,
                verbatim_excerpts: verbatim,
            },
        )
        .collect();
    requests.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ledger.requests = requests.len();

    // R3: highest walk weight first, capped. Ordered among THEMSELVES only —
    // never merged into `requests`.
    summaries.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    summaries.truncate(SUMMARY_APPEND_CAP);
    ledger.summaries_appended = summaries.len();

    // ── 5. The map section ──────────────────────────────────────────────
    let map = build_map(&neighborhood, &graph_by_id);
    if map.truncated() {
        degradations.push(Degradation::MapTruncated {
            reached: map.reached,
        });
    }

    tracing::debug!(
        target: "retrieval_audit",
        kind = selection.kind.as_str(),
        kind_source = selection.kind_source.as_str(),
        policy = %selection.policy_source.label(),
        hops = walk.hops,
        budget = walk.budget,
        seed_kinds = ?walk.seed.kinds,
        edge_kinds = ?walk.walk,
        graphs = ledger.graphs,
        graphs_with_ann = ledger.graphs_with_ann,
        bags = ledger.bags,
        seed_candidates = ledger.seed_candidates,
        seeds = ledger.seeds,
        dropped_seed_kind = ledger.dropped_seed_kind,
        seed_kinds_unseen = ?ledger.seed_kinds_unseen,
        dropped_edge_kind = ledger.dropped_edge_kind,
        edges_followed = ledger.edges_followed,
        nodes_reached = ledger.nodes_reached,
        atoms_without_evidence = ledger.atoms_without_evidence,
        requests = ledger.requests,
        summary_seeds = ledger.summary_seeds,
        summary_expansions_suppressed = ledger.summary_expansions_suppressed,
        summaries_appended = ledger.summaries_appended,
        "ground: walk ledger"
    );

    Grounding {
        requests,
        summaries,
        kind: selection.kind,
        kind_source: selection.kind_source,
        kind_score: selection.kind_score,
        policy_source: selection.policy_source.clone(),
        budget: walk.budget as usize,
        map,
        ledger,
        degradations,
    }
}

/// The paraphrase carried by a Summary atom, for the late append (R3).
///
/// Reads the projected `content` column, which
/// [`projection`](super::projection) fills from `Summary.text`, and falls
/// back to a payload parse for a record projected before that column carried
/// this kind. Empty means "nothing to append", and the caller drops the node
/// rather than appending a blank — absence reported, never defaulted.
fn summary_text(graph: &dyn AtlasProvider, atom_id: &str) -> String {
    let Some(view) = graph.atom(atom_id) else {
        return String::new();
    };
    if !view.content().is_empty() {
        return view.content().to_string();
    }
    match view.atom_envelope() {
        Some(crate::enrichment::atlas::atoms::AtomEnvelope::Summary(s)) => s.text,
        _ => String::new(),
    }
}

/// Is this atom summary-grain — derived text that may orient retrieval but
/// may not be quoted, and may not score a leaf?
///
/// One line, but it is the enforcement point for the ei-7a hold-out, so it
/// has a name and a single definition. The DECISION itself lives on
/// [`AtomType::grain`] in `corpus-engine-vocab` (one decider, ARCH §10.6);
/// this only asks the store for the kind. An atom the store cannot produce
/// is NOT treated as a summary — the permissive answer is the leaf one, and
/// a missing atom is already counted as a dropped seed elsewhere.
fn is_summary_grain(graph: &dyn AtlasProvider, atom_id: &str) -> bool {
    graph
        .atom(atom_id)
        .map(|a| a.kind().grain() == kernel_types::Grain::Summary)
        .unwrap_or(false)
}

/// Does this row filter its seeds at all? An unfiltered row seeds on whatever
/// the ANN and the name match return, which is what the pre-policy glue did.
fn seed_filter_is_active(walk: &WalkPolicy) -> bool {
    !walk.seed.kinds.is_empty() || walk.seed.declared
}

/// Does one candidate atom pass the row's seed filter?
fn seed_admits(
    walk: &WalkPolicy,
    graph: &dyn AtlasProvider,
    kind: AtomType,
    subtype: &str,
) -> bool {
    if walk.seed.kinds.contains(&kind) {
        // An `Entity` seed narrowed by `entity_types` matches on the
        // `entity_type` tag, which is what `AtomView::subtype` carries for an
        // Entity. An empty list means any entity.
        if kind == AtomType::Entity && !walk.seed.entity_types.is_empty() {
            if walk
                .seed
                .entity_types
                .iter()
                .any(|t| t.as_str_repr() == subtype)
            {
                return true;
            }
        } else {
            return true;
        }
    }
    // The enumeration row: the corpus's own declared types and their
    // subtypes. `is_subtype_of` is inert for an undeclared corpus, so this
    // arm cannot fire where there is nothing declared.
    if walk.seed.declared {
        if let Some(policies) = graph.ontology() {
            return declares(policies, graph, subtype);
        }
    }
    false
}

fn declares(policies: &OntologyPolicies, graph: &dyn AtlasProvider, subtype: &str) -> bool {
    policies
        .shape
        .types
        .iter()
        .any(|t| t.name == subtype || graph.is_subtype_of(subtype, &t.name))
}

fn build_map(
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

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_vocab::taxonomy::EntityType;

    use crate::enrichment::atlas::ann_store::AnnSeedTable;
    use crate::enrichment::atlas::context::{AtlasEntry, AtomView, EdgeView, EvidenceRef};
    use crate::enrichment::atlas::projection::AtomRecord;
    use corpus_engine_vocab::atoms::ChunkRef;
    use corpus_engine_vocab::edges::EdgeProvenance;

    // ── The displacement fixture (§18.1) ────────────────────────────────
    //
    // One rollup atom whose evidence is five chunks, and five ordinary
    // atoms with one chunk each. The rollup matches the question better
    // than any single leaf does — which is the NORMAL case for a rollup,
    // not a contrived one: a paraphrase of a whole region is written to
    // be about the region.
    //
    // Everything below is shared by the two tests that follow. They
    // differ in ONE field — the rollup atom's `kind` — and that field is
    // the whole hold-out. Keeping both directions as tests is deliberate:
    // a guard whose failing input is not itself run is a guard nobody has
    // watched fail (ARCH §18.1), and "I disabled it once locally" is not
    // evidence anybody can re-check.

    const ROLLUP_CHUNKS: [&str; 5] = ["c1", "c2", "c3", "c4", "c5"];
    const LEAF_CHUNKS: [&str; 5] = ["c6", "c7", "c8", "c9", "c10"];

    struct FixtureAtlas {
        site: EvidenceSite,
        atoms: std::collections::HashMap<String, AtomRecord>,
        edges: Vec<(String, String, EdgeType)>,
    }

    fn record(id: &str, kind: AtomType, content: &str, chunks: &[&str]) -> AtomRecord {
        AtomRecord {
            id: id.to_string(),
            kind,
            name: String::new(),
            label: String::new(),
            content: content.to_string(),
            subtype: String::new(),
            description: String::new(),
            excerpt: String::new(),
            confidence: 0.0,
            salience: 0.0,
            aliases: Vec::new(),
            participants: Vec::new(),
            evidence: chunks
                .iter()
                .map(|c| ChunkRef::new(*c, Some(format!("preview of {c}"))))
                .collect(),
            payload: Vec::new(),
        }
    }

    /// `rollup_kind` is the ONLY variable. `Summary` is the shipped
    /// behaviour; any leaf-grain kind reproduces the pre-ei-7a mechanism.
    fn fixture(rollup_kind: AtomType) -> FixtureAtlas {
        let mut atoms = std::collections::HashMap::new();
        atoms.insert(
            "rollup".to_string(),
            record(
                "rollup",
                rollup_kind,
                "a paraphrase of the whole region",
                &ROLLUP_CHUNKS,
            ),
        );
        let mut edges = Vec::new();
        for (i, chunk) in LEAF_CHUNKS.iter().enumerate() {
            let id = format!("leaf{i}");
            atoms.insert(
                id.clone(),
                record(&id, AtomType::Claim, "an ordinary claim", &[chunk]),
            );
            // The rollup cites the leaves it summarises. This is what makes
            // the hazard structural rather than incidental: EvidenceFor is
            // exactly the edge the port introduces, and it fans out over the
            // whole subtree by construction.
            edges.push(("rollup".to_string(), id, EdgeType::EvidenceFor));
        }
        FixtureAtlas {
            site: EvidenceSite::derive("fixture"),
            atoms,
            edges,
        }
    }

    impl AtlasProvider for FixtureAtlas {
        fn atlas_corpus_id(&self) -> &str {
            "fixture"
        }
        fn site(&self) -> &EvidenceSite {
            &self.site
        }
        fn atom(&self, atom_id: &str) -> Option<AtomView<'_>> {
            self.atoms.get(atom_id).map(AtomView::new)
        }
        fn atom_evidence(&self, atom_id: &str) -> Vec<EvidenceRef<'_>> {
            self.atoms
                .get(atom_id)
                .map(|a| a.evidence.iter().map(EvidenceRef::new).collect())
                .unwrap_or_default()
        }
        fn edges_from(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
            self.edges
                .iter()
                .filter(|(s, _, _)| s == atom_id)
                .map(|(s, t, k)| EdgeView {
                    source: s.as_str(),
                    target: t.as_str(),
                    edge_type: *k,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                })
                .collect()
        }
        fn edges_to(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
            self.edges
                .iter()
                .filter(|(_, t, _)| t == atom_id)
                .map(|(s, t, k)| EdgeView {
                    source: s.as_str(),
                    target: t.as_str(),
                    edge_type: *k,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                })
                .collect()
        }
        fn ann_seed_table(&self) -> Option<&std::sync::Arc<AnnSeedTable>> {
            None
        }
        fn ontology(&self) -> Option<&OntologyPolicies> {
            None
        }
    }

    /// The bag that seeds the walk by name match. The rollup's embedding
    /// IS the question (cosine 1.0); the leaves are further away. A
    /// rollup outscoring its own leaves is the ordinary case.
    fn fixture_bag() -> AtlasContext {
        let mut entries = vec![AtlasEntry {
            atom_id: "rollup".to_string(),
            canonical_name: "rollup".to_string(),
            embed_text: "rollup".to_string(),
            embedding: vec![1.0, 0.0],
        }];
        for i in 0..LEAF_CHUNKS.len() {
            entries.push(AtlasEntry {
                atom_id: format!("leaf{i}"),
                canonical_name: format!("leaf{i}"),
                embed_text: format!("leaf{i}"),
                embedding: vec![0.5, 0.866],
            });
        }
        AtlasContext {
            atlas_corpus_id: "fixture".to_string(),
            entries,
            top_k: 32,
        }
    }

    async fn walk_fixture(rollup_kind: AtomType) -> Grounding {
        let atlas = fixture(rollup_kind);
        let bag = fixture_bag();
        // Names every atom so seeding is deterministic and does not depend
        // on an ANN table the fixture deliberately does not have.
        let question = "rollup leaf0 leaf1 leaf2 leaf3 leaf4";
        ground(
            question,
            &[1.0, 0.0],
            &[&bag],
            &[&atlas as &dyn AtlasProvider],
            &WalkSelection::unfiltered(KindSource::Abstained, None, PolicySource::PreRegistered),
            12,
        )
        .await
    }

    fn requested_chunks(g: &Grounding) -> Vec<String> {
        g.requests
            .iter()
            .map(|r| r.selector.as_str().to_string())
            .collect()
    }

    /// THE FAILING INPUT, kept runnable. With the rollup carrying a
    /// LEAF-grain kind — which is exactly what a RAPTOR summary was
    /// before `AtomType::Summary` existed — its weight lands on all five
    /// of the chunks it covers, and those chunks outrank every leaf. This
    /// is the −14pt SEP source-coverage mechanism (2026-06-08) reproduced
    /// deterministically, with no model and no corpus.
    ///
    /// If this test ever goes green-by-accident (no displacement), the
    /// sibling test below is proving nothing and both need re-deriving.
    #[tokio::test]
    async fn a_leaf_grain_rollup_displaces_the_leaf_chunks_it_covers() {
        let g = walk_fixture(AtomType::Claim).await;
        let chunks = requested_chunks(&g);
        let top: Vec<&String> = chunks.iter().take(ROLLUP_CHUNKS.len()).collect();
        for c in ROLLUP_CHUNKS {
            assert!(
                top.iter().any(|t| t.as_str() == c),
                "rollup chunk {c} should have crowded the top; got {chunks:?}"
            );
        }
        assert!(
            g.summaries.is_empty(),
            "a leaf-grain atom is not carried as a summary"
        );
        assert_eq!(g.ledger.summary_seeds, 0);
    }

    /// THE GUARD. Same fixture, same scores, same edges — the rollup is
    /// `Summary` grain. R2 keeps its weight off every leaf request, so the
    /// five leaf chunks are the whole request list and the rollup's text
    /// leaves by the other door (R3) instead.
    #[tokio::test]
    async fn a_summary_seed_cannot_displace_a_leaf_chunk() {
        let g = walk_fixture(AtomType::Summary).await;
        let chunks = requested_chunks(&g);

        for c in ROLLUP_CHUNKS {
            assert!(
                !chunks.iter().any(|r| r == c),
                "summary-covered chunk {c} entered the requests; the hold-out did not fire \
                 ({chunks:?})"
            );
        }
        for c in LEAF_CHUNKS {
            assert!(
                chunks.iter().any(|r| r == c),
                "leaf chunk {c} was displaced ({chunks:?})"
            );
        }

        // R3: the text is carried, not dropped — the capability survives
        // the hold-out. Losing it would be the other way to pass this test
        // and is not a fix.
        assert_eq!(
            g.summaries.len(),
            1,
            "the summary is carried for late append"
        );
        assert_eq!(g.summaries[0].atom_id, "rollup");
        assert!(g.summaries[0].text.contains("paraphrase"));

        // R1 + the ledger: the hold-out is VISIBLE, not silent.
        assert_eq!(g.ledger.summary_seeds, 1);
        assert!(
            g.ledger.summary_expansions_suppressed >= 1,
            "R1 never fired: {:?}",
            g.ledger
        );
        assert_eq!(g.ledger.summaries_appended, 1);
    }

    /// The chunk → atlas id derivation, in both shapes, and its agreement
    /// with `EvidenceSite`'s reading in the other direction. Failing input:
    /// drop the self-hosted candidate, or emit the child for a titleless
    /// chunk.
    #[test]
    fn candidate_atlas_ids_covers_both_layouts_and_agrees_with_evidence_site() {
        let ids = candidate_atlas_ids("sep", Some("freewill"));
        assert_eq!(ids, vec!["sep".to_string(), "sep-freewill".to_string()]);
        // The inverse holds: the child id reads back to the parent corpus.
        assert_eq!(
            EvidenceSite::derive("sep-freewill").chunk_corpus().as_str(),
            "sep"
        );

        // A chunk with no title has exactly one candidate — its own corpus.
        assert_eq!(
            candidate_atlas_ids("wikipedia", None),
            vec!["wikipedia".to_string()]
        );
        assert_eq!(
            candidate_atlas_ids("wikipedia", Some("   ")),
            vec!["wikipedia".to_string()]
        );
        // …and a chunk titled after its own corpus yields ONE candidate, not
        // a `bk-1-bk-1` that addresses nothing. This is the literary shape,
        // not a corner case: every chunk of `brothers-karamazov-book-1` is
        // titled with its corpus id.
        assert_eq!(
            candidate_atlas_ids("bk-1", Some("bk-1")),
            vec!["bk-1".to_string()]
        );
    }

    /// The unfiltered row admits everything and does NOT over-fetch — the
    /// path `apply_atlas_grounding` has always taken. Failing input: make
    /// `seed_filter_is_active` true for the unfiltered row.
    #[test]
    fn the_unfiltered_row_is_the_status_quo_ante() {
        let w = WalkPolicy::unfiltered();
        assert!(!seed_filter_is_active(&w));
        assert!(w.walk.is_empty(), "no edge filter");
        assert_eq!(w.hops, 2);
        assert_eq!(w.budget, corpus_engine_vocab::ontology::DEFAULT_BUDGET);
    }

    /// Every classified row DOES filter, or the map is not driving anything.
    #[test]
    fn every_pre_registered_row_filters_its_seeds() {
        let p = NavigationPolicy::default();
        for (kind, w) in p.rows() {
            assert!(
                seed_filter_is_active(w),
                "{} declares no seed filter",
                kind.as_str()
            );
        }
    }

    /// The enumeration row seeds on declared types only — so it can never
    /// fire on a corpus that declared nothing, which is the I5 gate.
    #[test]
    fn the_enumeration_row_is_inert_without_a_declaration() {
        let p = NavigationPolicy::default();
        let w = p.walk(QuestionKind::Enumeration);
        assert!(w.seed.declared && w.seed.kinds.is_empty());
        assert_eq!(w.hops, 0, "enumeration lists its seeds, it does not walk");
    }

    /// The thematic row narrows `Entity` to concepts and leaves
    /// `Configuration` unnarrowed — the two arms of `seed_admits`.
    #[test]
    fn an_entity_type_narrows_only_the_entity_seed() {
        let p = NavigationPolicy::default();
        let w = p.walk(QuestionKind::Thematic);
        assert_eq!(w.seed.entity_types, vec![EntityType::Concept]);
        assert!(w.seed.kinds.contains(&AtomType::Configuration));
        // `Configuration` carries whatever subtype it likes and still seeds;
        // `Entity` must be a concept.
        assert_eq!(EntityType::Concept.as_str_repr(), "concept");
    }

    /// Every degradation renders a sentence a reader can act on — none is an
    /// empty string or a bare enum name.
    #[test]
    fn every_degradation_says_something_actionable() {
        let all = [
            Degradation::NoSeedTable,
            Degradation::NoAtomBag,
            Degradation::Unclassified(KindSource::Abstained),
            Degradation::SeedKindsUnseen(vec![AtomType::Configuration]),
            Degradation::NoEvidenceAnchors,
            Degradation::MapTruncated { reached: 900 },
        ];
        for d in all {
            let s = d.sentence();
            assert!(s.len() > 20, "{d:?} -> {s:?}");
            assert!(s.chars().next().unwrap().is_lowercase() || s.starts_with("the"));
        }
        assert!(Degradation::SeedKindsUnseen(vec![AtomType::Configuration])
            .sentence()
            .contains("configuration"));
    }

    /// No graphs is an EMPTY grounding that still names why — never a bare
    /// vec the caller reads as "nothing was relevant".
    #[tokio::test]
    async fn a_walk_with_no_graphs_says_why_it_is_empty() {
        let g = ground(
            "what is this about",
            &[0.1, 0.2],
            &[],
            &[],
            &WalkSelection::unfiltered(KindSource::Abstained, None, PolicySource::PreRegistered),
            12,
        )
        .await;
        assert!(g.requests.is_empty());
        assert!(g
            .degradations
            .contains(&Degradation::Unclassified(KindSource::Abstained)));
    }

    /// An empty embedding cannot seed, and says so rather than returning an
    /// unexplained empty list.
    #[tokio::test]
    async fn an_empty_embedding_cannot_seed() {
        let g = ground(
            "anything",
            &[],
            &[],
            &[],
            &WalkSelection::named(
                QuestionKind::Thematic,
                &NavigationPolicy::default(),
                PolicySource::PreRegistered,
            ),
            12,
        )
        .await;
        assert!(g.requests.is_empty());
        assert_eq!(g.ledger.seeds, 0);
    }

    /// With no classifier available the walk runs the unfiltered row and the
    /// source says which — it does not silently pick a kind.
    #[tokio::test]
    async fn no_embedder_means_the_unfiltered_row_named_as_such() {
        let sel = select_walk(
            &[0.1, 0.2],
            &NavigationPolicy::default(),
            PolicySource::PreRegistered,
            None,
        )
        .await;
        assert_eq!(sel.kind, QuestionKind::Thematic);
        assert_eq!(sel.walk, WalkPolicy::unfiltered());
        assert_eq!(sel.kind_source, KindSource::ClassifierUnavailable);
        assert!(sel.kind_source.is_degradation());
        assert!(sel.kind_score.is_none());
        // …and it SAYS so, in one line a reader can act on.
        assert!(sel.describe().contains("classifier-unavailable"));
    }

    /// A map that carries no exemplars anywhere yields `NoClassifier`, which
    /// is a DIFFERENT fact from an unreachable embedder.
    #[tokio::test]
    async fn a_map_with_no_exemplars_is_distinguished_from_a_dead_embedder() {
        let mut policy = NavigationPolicy::default();
        for row in [
            &mut policy.thematic,
            &mut policy.trajectory,
            &mut policy.tension,
            &mut policy.enumeration,
            &mut policy.lookup,
        ] {
            row.exemplars.clear();
        }
        let embed: EmbedFn = std::sync::Arc::new(|_: &str| {
            Box::pin(async { Ok(vec![1.0_f32, 0.0]) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = crate::Result<Vec<f32>>> + Send>,
                >
        });
        let sel = select_walk(
            &[1.0, 0.0],
            &policy,
            PolicySource::PreRegistered,
            Some(&embed),
        )
        .await;
        assert_eq!(sel.kind_source, KindSource::NoClassifier);
        assert_eq!(sel.walk, WalkPolicy::unfiltered());
    }

    /// No declared navigation anywhere means the pre-registered table, and
    /// the source says so rather than looking like a corpus's own choice.
    #[test]
    fn an_undeclared_scope_uses_the_pre_registered_table() {
        let (policy, src) = navigation_policy_for(&[]);
        assert_eq!(policy, NavigationPolicy::default());
        assert_eq!(src, PolicySource::PreRegistered);
        assert_eq!(src.label(), "pre-registered defaults");
    }
}
