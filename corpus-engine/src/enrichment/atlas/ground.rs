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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use corpus_engine_vocab::ontology::{OntologyPolicies, WalkPolicy, SUMMARY_SEED_BUDGET};

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

mod report;
mod select;
mod summaries;

// The split is a FILE split, not an API split: every name below was in
// `ground.rs` before ei-5c and every `atlas::ground::…` path still resolves
// (ARCH §10.6 — a re-export, never a twin).
pub(crate) use report::Reach;
pub use report::{
    Degradation, Grounding, MapNode, MapSection, PolicySource, SummaryNode, WalkLedger,
};
pub use select::{
    admit_winner, navigation_policy_for, select_walk, Admission, RowInertReport, WalkSelection,
};
pub use summaries::{SourceYield, SummaryQuery, SummaryStage};

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
/// A filtered row asks the ANN table for [`seed_pool_size`] nearest atoms and
/// keeps the best that pass the filter and its quotas. Without it, a
/// row that seeds on `Configuration` gets zero seeds whenever the twelve
/// nearest atoms happen to be Claims — the filter would subtract rather than
/// select, and a policy-driven walk would ground LESS than the constant it
/// replaced.
///
/// The unfiltered row does not over-fetch, so the path
/// `apply_atlas_grounding` has always taken is unchanged by this file.
const SEED_OVERFETCH: usize = 4;

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
    // An inert row is its own degradation, with the row it fell to; the
    // generic "ran the unfiltered row" line would be wrong for it.
    if let Some(inert) = &selection.inert {
        degradations.push(Degradation::RowInert(inert.clone()));
    } else if selection.kind_source.is_degradation() {
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
        seed_pool_size(walk, max_seeds)
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

    // ── 2. The row's seed filter, then its per-kind quotas ──────────────
    //
    // Admission is by KIND (`seed_admits`); how many slots an admitted kind
    // may take is the row's [`SeedPolicy::budgets`]. A kind that declares a
    // quota draws from its own; every other kind shares `max_seeds` exactly as
    // before, so a quota can never take a slot from a kind that has none.
    //
    // This is the ei-7a defect's fix and it is a POLICY field, not a special
    // case for Summary: the pool is score-ordered, so before the quota the
    // only way to stop a reachable kind winning a leaf's slot was to make it
    // unreachable. Measured cost of having no such field: −7.5/66 on the SEP
    // subset A/B, which is what kept the retrieval-time injector alive.
    let mut seen_kinds: BTreeSet<AtomType> = BTreeSet::new();
    let seeds: Vec<(String, String, f32)> = if filtered {
        let mut kept = Vec::new();
        let mut budget_used: BTreeMap<AtomType, usize> = BTreeMap::new();
        let mut unbudgeted = 0usize;
        for (cid, aid, s) in candidates {
            let Some(graph) = graph_by_id.get(cid.as_str()) else {
                continue;
            };
            let Some(atom) = graph.atom(&aid) else {
                ledger.dropped_seed_kind += 1;
                continue;
            };
            let kind = atom.kind();
            seen_kinds.insert(kind);
            if !seed_admits(walk, *graph, kind, atom.subtype()) {
                ledger.dropped_seed_kind += 1;
                continue;
            }
            match walk.seed.budgets.get(&kind) {
                Some(&quota) => {
                    let used = budget_used.entry(kind).or_insert(0);
                    if *used < quota as usize {
                        *used += 1;
                        kept.push((cid, aid, s));
                    } else {
                        ledger.dropped_seed_budget += 1;
                    }
                }
                None => {
                    if unbudgeted < max_seeds {
                        unbudgeted += 1;
                        kept.push((cid, aid, s));
                    } else {
                        ledger.dropped_seed_budget += 1;
                    }
                }
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
    // The 5th element is the atlas's EXACT `section_id -> chunk ids` join,
    // captured here because this is the only place the graph that owns the
    // manifest is in scope. Filled once per key; empty for a `RowId` selector
    // and for a corpus whose join was never backfilled.
    let mut chunk_scores: HashMap<
        (EvidenceSite, ChunkSelector),
        (
            f32,
            String,
            Vec<String>,
            Vec<String>,
            Vec<u64>,
            Vec<(f32, String)>,
        ),
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
            let selector = ChunkSelector::parse(chunk_id);
            let section_rows = match &selector {
                ChunkSelector::Section(sec) => graph.section_chunk_ids(sec).to_vec(),
                ChunkSelector::RowId(_) => Vec::new(),
            };
            let key = (graph.site().clone(), selector);
            let entry = chunk_scores.entry(key).or_insert((
                0.0,
                preview.to_string(),
                Vec::new(),
                Vec::new(),
                section_rows,
                Vec::new(),
            ));
            entry.0 += reach.weight;
            if preview.len() > entry.1.len() {
                entry.1 = preview.to_string();
            }
            // Every citing atom's preview pins, not just the wordiest — see
            // `ChunkRequest::passage_previews`. Carried WITH the citing atom's
            // reach weight, because "which paragraph of this section did the
            // walk actually care about" is the walk's own ranking and summing
            // it away is what left the pin to whichever atom was wordiest.
            // Capped so one heavily-cited section cannot make the pin scan
            // unbounded.
            if !preview.is_empty() && !entry.5.iter().any(|(_, p)| p == preview) {
                entry.5.push((reach.weight, preview.to_string()));
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
            |(
                (site, selector),
                (score, preview, motivating, verbatim, section_rows, mut previews),
            )| {
                previews.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                previews.truncate(16);
                let previews: Vec<String> = previews.into_iter().map(|(_, p)| p).collect();
                ChunkRequest {
                    site,
                    selector,
                    passage_preview: preview,
                    passage_previews: previews,
                    score,
                    motivating_atoms: motivating,
                    verbatim_excerpts: verbatim,
                    section_rows,
                }
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
    // R3's cap IS the row's Summary seed quota — one number, one home
    // (ARCH §10.6). A Summary seed neither expands (R1) nor scores a leaf
    // (R2), so the count that may seed and the count that may be appended are
    // the same decision; they were two constants until ei-5c and could drift.
    // A row that declares no quota (the unfiltered row, a map written before
    // this field) gets the pre-registered one, named rather than unbounded.
    let summary_cap = walk
        .seed
        .budgets
        .get(&AtomType::Summary)
        .copied()
        .unwrap_or(SUMMARY_SEED_BUDGET) as usize;

    // ── R3': the row's SOURCES, composed into that one budget ───────────
    //
    // The atoms the walk reached are one source among the row's list, not the
    // whole supply. `compose` asks each listed source in the row's order for
    // the room that is left, dedupes on the summary's own content-derived id,
    // and stops at the budget — so however many sources a corpus composes
    // there is one producer of record. That is the rule whose absence cost
    // −7.5/66 when the walk and the retrieval-time injector both fed this
    // position and neither knew about the other.
    let (composed, served) = summaries::compose(
        &walk.summary_sources,
        summary_cap,
        question_embedding,
        graphs,
        &summaries,
    )
    .await;
    summaries = composed;
    ledger.summaries_appended = summaries.len();
    ledger.summary_sources_served = served;

    // ── 5. The map section ──────────────────────────────────────────────
    let map = report::build_map(&neighborhood, &graph_by_id);
    if map.truncated() {
        degradations.push(Degradation::MapTruncated {
            reached: map.reached,
        });
    }

    tracing::debug!(
        target: "retrieval_audit",
        kind = selection.kind.as_str(),
        kind_source = selection.kind_source.as_str(),
        row_inert = selection.inert.as_ref().map(|i| i.sentence()).unwrap_or_default(),
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
        dropped_seed_budget = ledger.dropped_seed_budget,
        seed_budgets = ?walk.seed.budgets,
        seed_kinds_unseen = ?ledger.seed_kinds_unseen,
        dropped_edge_kind = ledger.dropped_edge_kind,
        edges_followed = ledger.edges_followed,
        nodes_reached = ledger.nodes_reached,
        atoms_without_evidence = ledger.atoms_without_evidence,
        requests = ledger.requests,
        summary_seeds = ledger.summary_seeds,
        summary_expansions_suppressed = ledger.summary_expansions_suppressed,
        summaries_appended = ledger.summaries_appended,
        summary_sources = %summaries::yield_label(&ledger.summary_sources_served),
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

/// How many nearest atoms to ask the ANN table for, given the row.
///
/// A filtered row over-fetches so the filter SELECTS within a wider pool
/// instead of subtracting from a narrow one — without it a row that seeds on
/// `Configuration` gets nothing whenever the twelve nearest atoms are Claims.
///
/// The quota term is the ei-5c half and it is not cosmetic. A kind with a
/// [`SeedPolicy::budgets`] quota draws slots ON TOP of `max_seeds`, so the
/// pool has to be able to hold both; sizing it for `max_seeds` alone would let
/// a summary-dense atlas fill the fetched window with the very kind the quota
/// then refuses, and the leaves it was protecting would starve inside the
/// pool rather than at the filter. That would be the displacement this order
/// removed, reappearing one step upstream and invisible in the ledger —
/// `dropped_seed_budget` would count the refusals and nothing would count the
/// leaves that were never fetched.
///
/// Saturating throughout: a map may declare any `u32`, and a pool size that
/// wrapped would be a silent under-fetch.
fn seed_pool_size(walk: &WalkPolicy, max_seeds: usize) -> usize {
    let quota: usize = walk
        .seed
        .budgets
        .values()
        .fold(0usize, |a, &b| a.saturating_add(b as usize));
    max_seeds
        .saturating_add(quota)
        .saturating_mul(SEED_OVERFETCH)
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

#[cfg(test)]
mod tests;
