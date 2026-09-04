// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ask` — the one tool that composes the layers, so the client does not have
//! to (`EPISTEMIC_INDEX.md` §4).
//!
//! `corpus_search`, `atoms_lookup` and `corpus_ontology` each expose one
//! layer, and a client that wants a connected answer has to compose three
//! calls and know why. `ask` embeds once, runs tier 1, walks the atlas under
//! the corpus's own navigation policy, resolves the walk's evidence, and
//! returns cited passages **plus a map section** — the idea nodes traversed,
//! their kinds, and the edges followed.
//!
//! # What is here and what is not
//!
//! No walk logic is here. The walk is `corpus_engine::enrichment::atlas::
//! ground`, the same function `sovereign-core`'s `apply_atlas_grounding`
//! calls; this file supplies the two fetches ([`IndexEvidenceFetcher`]) and
//! the rendering. That is the whole point of ei-4: one walk, two hosts. A
//! second walk written here would be exactly the §10.6 smell.
//!
//! No generation is here either, and that is the spec's design: `ask` hands
//! the model on the OTHER side of the MCP wire its evidence and its map. This
//! host holds no chat client and takes no dependency that carries one.
//!
//! # Degradations are text, not silence
//!
//! Every reason the answer is thinner than it could be — no seed table, no
//! atom bag, an unclassified question, a truncated map — is a sentence in the
//! result body and a field in `structuredContent`. A caller must never have to
//! infer from a short list that something was missing (§18.3).

use corpus_engine::enrichment::atlas::ground::{Degradation, Grounding, MapNode};
use corpus_engine::enrichment::atlas::{EvidenceFetcher, ResolvedChunk};
use corpus_engine::CorpusId;
use corpus_engine::{CorpusIndex, ScoredChunk};
use serde_json::{json, Value};

use crate::tools::ToolOutcome;

/// How much of a passage the text rendering carries. The structured half
/// carries all of it; the text half is what a model reads inline.
const PASSAGE_CHARS: usize = 600;

/// The walk's two fetches, over one open [`CorpusIndex`].
///
/// The host serves a fixed set of corpora and holds their handles, so a
/// request whose site names a corpus this host does not serve resolves to
/// nothing — reported by the resolve ledger as unresolvable, never by opening
/// an index the host was not asked to serve.
pub struct IndexEvidenceFetcher<'a> {
    /// `(corpus id, open handle)` for every corpus this host serves.
    pub indexes: Vec<(&'a str, &'a CorpusIndex)>,
}

impl IndexEvidenceFetcher<'_> {
    fn index_for(&self, corpus: &CorpusId) -> Option<&CorpusIndex> {
        self.indexes
            .iter()
            .find(|(id, _)| *id == corpus.as_str())
            .map(|(_, ix)| *ix)
    }
}

impl EvidenceFetcher for IndexEvidenceFetcher<'_> {
    async fn by_row(&self, corpus: &CorpusId, row: u64) -> Option<ScoredChunk> {
        let Some(index) = self.index_for(corpus) else {
            eprintln!(
                "corpus-mcp: ask: chunk {row} lives in `{corpus}`, which this host does not serve"
            );
            return None;
        };
        match index.acquire_chunks(&[row]).await {
            Ok(chunks) => chunks.into_iter().next(),
            Err(e) => {
                // NAMED, not swallowed (§18.3). Both arms end as
                // `ResolveLedger::unresolvable`, but "the row is not there"
                // and "the index errored" are different facts and only this
                // line carries the second one. An `Err` collapsed into an
                // empty result is how the desktop updater hid two bugs for
                // weeks.
                eprintln!("corpus-mcp: ask: fetching chunk {row} from `{corpus}` failed: {e}");
                None
            }
        }
    }

    async fn by_search(&self, corpus: &CorpusId, query: &str, limit: usize) -> Vec<ScoredChunk> {
        let Some(index) = self.index_for(corpus) else {
            eprintln!(
                "corpus-mcp: ask: evidence lives in `{corpus}`, which this host does not serve"
            );
            return Vec::new();
        };
        // Full-text only: the walk already decided WHICH passage it wants and
        // gave its preview verbatim, so a second vector race would re-rank
        // against the question rather than find the named passage. An empty
        // embedding disables the vector leg inside `search`.
        match index.search(&[], query, limit).await {
            Ok(hits) => hits,
            Err(e) => {
                eprintln!("corpus-mcp: ask: searching `{corpus}` for evidence failed: {e}");
                Vec::new()
            }
        }
    }
}

/// One row of the map section, as the client sees it.
fn map_row(n: &MapNode) -> Value {
    json!({
        "atlas": n.atlas,
        "atom_id": n.atom_id,
        "name": n.name,
        "kind": n.kind.label(),
        "subtype": n.subtype,
        "hop": n.hop,
        "via_edge": n.via.map(|e| format!("{e:?}")),
        "from_atom": n.from,
        "score": n.score,
    })
}

/// Render one `ask` result.
///
/// Pure — no I/O, no `Server` — so the shape of the answer is unit-testable
/// without an endpoint or an index, which is how every other assertion in
/// this crate is written (`ontology_outcome`).
pub fn render(
    question: &str,
    tier1: &[ScoredChunk],
    grounded: &[ResolvedChunk],
    grounding: &Grounding,
    atlas_present: bool,
) -> ToolOutcome {
    let mut text = String::new();
    let mut passages = Vec::new();

    // ── Cited passages: tier 1 first, then what the walk added ──────────
    //
    // The walk's own chunks are marked, because "the graph brought this back"
    // is the claim this whole architecture makes and a reader must be able to
    // check it against the map below.
    let mut rank = 0usize;
    for h in tier1 {
        rank += 1;
        push_passage(&mut text, &mut passages, rank, h, &[], "search");
    }
    for r in grounded {
        rank += 1;
        push_passage(
            &mut text,
            &mut passages,
            rank,
            &r.chunk,
            &r.motivating_atoms,
            "walk",
        );
    }
    if passages.is_empty() {
        text.push_str(&format!("No passages matched `{question}`.\n"));
    }

    // ── The map section ─────────────────────────────────────────────────
    let mut map_text = String::new();
    if atlas_present {
        map_text.push_str(&format!(
            "\n--- map ({}) ---\n{} nodes traversed",
            grounding.kind.as_str(),
            grounding.map.reached
        ));
        let edges = grounding.map.edge_kinds();
        if edges.is_empty() {
            map_text.push_str(", no edges followed");
        } else {
            map_text.push_str(&format!(
                ", edges followed: {}",
                edges
                    .iter()
                    .map(|e| format!("{e:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        map_text.push('\n');
        for n in &grounding.map.nodes {
            map_text.push_str(&format!(
                "  {kind}{sub} {name} (hop {hop}{via}, {score:.3})\n",
                kind = n.kind.label(),
                sub = if n.subtype.is_empty() {
                    String::new()
                } else {
                    format!("/{}", n.subtype)
                },
                name = n.name,
                hop = n.hop,
                via = n
                    .via
                    .map(|e| format!(" via {e:?}"))
                    .unwrap_or_else(|| " seed".to_string()),
                score = n.score,
            ));
        }
    } else {
        map_text.push_str(
            "\n--- map ---\nthis corpus has no atlas on disk, so there is no map to walk; \
             the passages above are full-text and vector search only\n",
        );
    }
    text.push_str(&map_text);

    // ── Degradations ────────────────────────────────────────────────────
    let mut degradations: Vec<String> = grounding
        .degradations
        .iter()
        .map(Degradation::sentence)
        .collect();
    if !atlas_present {
        degradations.push(
            "no atlas directory for this corpus — run `svrn enrich build <corpus>` to index \
             its ideas"
                .to_string(),
        );
    }
    if !degradations.is_empty() {
        text.push_str("\n--- degraded ---\n");
        for d in &degradations {
            text.push_str(&format!("  - {d}\n"));
        }
    }

    ToolOutcome {
        text: text.trim_end().to_string(),
        is_error: false,
        structured: Some(json!({
            "question": question,
            "passages": passages,
            "map": {
                "question_kind": grounding.kind.as_str(),
                "question_kind_source": grounding.kind_source.as_str(),
                "policy": grounding.policy_source.label(),
                "nodes_reached": grounding.map.reached,
                "nodes": grounding.map.nodes.iter().map(map_row).collect::<Vec<_>>(),
                "edge_kinds": grounding
                    .map
                    .edge_kinds()
                    .iter()
                    .map(|e| format!("{e:?}"))
                    .collect::<Vec<_>>(),
                "truncated": grounding.map.truncated(),
            },
            "walk": {
                "graphs": grounding.ledger.graphs,
                "graphs_with_seed_table": grounding.ledger.graphs_with_ann,
                "seeds": grounding.ledger.seeds,
                "seed_candidates": grounding.ledger.seed_candidates,
                "dropped_seed_kind": grounding.ledger.dropped_seed_kind,
                "dropped_edge_kind": grounding.ledger.dropped_edge_kind,
                "edges_followed": grounding.ledger.edges_followed,
                "requests": grounding.ledger.requests,
                "budget": grounding.budget,
            },
            "degradations": degradations,
        })),
    }
}

fn push_passage(
    text: &mut String,
    rows: &mut Vec<Value>,
    rank: usize,
    c: &ScoredChunk,
    motivating: &[String],
    origin: &str,
) {
    let title = c.title.as_deref().unwrap_or("(untitled)");
    let url = c.url.as_deref().unwrap_or("-");
    text.push_str(&format!(
        "[{rank}] {title} — {url} (corpus {corpus}, {origin}, score {score:.4})\n{body}\n\n",
        corpus = c.corpus_id,
        score = c.score,
        body = truncate(&c.content, PASSAGE_CHARS),
    ));
    rows.push(json!({
        "rank": rank,
        "origin": origin,
        "corpus_id": c.corpus_id,
        "title": c.title,
        "url": c.url,
        "score": c.score,
        "chunk_id": c.chunk_id,
        "source_doc_id": c.source_doc_id,
        "content": c.content,
        "motivating_atoms": motivating,
    }));
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::ground::{
        MapSection, PolicySource, WalkLedger, WalkSelection,
    };
    use corpus_engine_vocab::atoms::AtomType;
    use corpus_engine_vocab::ontology::{NavigationPolicy, QuestionKind};

    fn chunk(title: &str, body: &str) -> ScoredChunk {
        ScoredChunk {
            content: body.into(),
            title: Some(title.into()),
            url: Some("file:///book".into()),
            corpus_id: "bk".into(),
            score: 0.5,
            metadata: Default::default(),
            chunk_id: Some(7),
            source_doc_id: None,
            vector_distance: None,
            provenance: corpus_engine::index::ChunkProvenance::acquired_from_estate("bk"),
        }
    }

    fn node(name: &str, hop: u8) -> MapNode {
        MapNode {
            atlas: "bk".into(),
            atom_id: format!("atom-{name}"),
            name: name.into(),
            kind: AtomType::Entity,
            subtype: "concept".into(),
            hop,
            via: (hop > 0).then_some(corpus_engine::enrichment::atlas::EdgeType::Involves),
            from: (hop > 0).then(|| "atom-seed".to_string()),
            score: 0.9 - (hop as f32) * 0.1,
        }
    }

    fn grounding(nodes: Vec<MapNode>, degradations: Vec<Degradation>) -> Grounding {
        let sel = WalkSelection::named(
            QuestionKind::Thematic,
            &NavigationPolicy::default(),
            PolicySource::PreRegistered,
        );
        let reached = nodes.len();
        Grounding {
            requests: Vec::new(),
            kind: sel.kind,
            kind_source: sel.kind_source,
            kind_score: None,
            policy_source: sel.policy_source,
            budget: 12,
            map: MapSection { nodes, reached },
            ledger: WalkLedger::default(),
            degradations,
        }
    }

    /// The shape §4 asks for: cited passages AND a map section naming the
    /// nodes, their kinds and the edges followed. Failing input: drop the
    /// map block from `render`.
    #[test]
    fn a_walked_answer_carries_passages_and_a_map() {
        let tier1 = vec![chunk("Book I", "the elder Zossima received pilgrims")];
        let walked = vec![ResolvedChunk {
            chunk: chunk("Book I", "an ill-natured buffoon and nothing more"),
            motivating_atoms: vec!["atom-buffoonery".into()],
            walk_score: 1.4,
        }];
        let g = grounding(vec![node("eldership", 0), node("buffoonery", 1)], vec![]);
        let out = render("what is this about", &tier1, &walked, &g, true);
        assert!(!out.is_error);
        assert!(out.text.contains("--- map (thematic) ---"), "{}", out.text);
        assert!(
            out.text.contains("entity/concept eldership"),
            "{}",
            out.text
        );
        assert!(out.text.contains("via Involves"), "{}", out.text);

        let s = out.structured.unwrap();
        assert_eq!(s["passages"].as_array().unwrap().len(), 2);
        // The walk's passage is MARKED as the walk's, and names the idea it
        // is evidence for — that link is what makes the map checkable.
        assert_eq!(s["passages"][0]["origin"], "search");
        assert_eq!(s["passages"][1]["origin"], "walk");
        assert_eq!(s["passages"][1]["motivating_atoms"][0], "atom-buffoonery");
        assert_eq!(s["map"]["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(s["map"]["edge_kinds"][0], "Involves");
        assert_eq!(s["map"]["question_kind"], "thematic");
    }

    /// A corpus with no atlas says so, in the body and in the structured
    /// half — it does not return an empty map that reads like a walk that
    /// found nothing (§18.3). Failing input: render an empty map section
    /// when `atlas_present` is false.
    #[test]
    fn no_atlas_is_reported_not_rendered_as_an_empty_walk() {
        let tier1 = vec![chunk("Book I", "some passage")];
        let g = grounding(vec![], vec![]);
        let out = render("what is this about", &tier1, &[], &g, false);
        assert!(out.text.contains("no atlas on disk"), "{}", out.text);
        let s = out.structured.unwrap();
        let degs = s["degradations"].as_array().unwrap();
        assert!(
            degs.iter()
                .any(|d| d.as_str().unwrap().contains("no atlas")),
            "{degs:?}"
        );
    }

    /// Every walk degradation reaches the caller as a sentence. Failing
    /// input: log them instead of rendering them.
    #[test]
    fn walk_degradations_reach_the_caller() {
        let g = grounding(
            vec![node("theme", 0)],
            vec![
                Degradation::NoSeedTable,
                Degradation::SeedKindsUnseen(vec![AtomType::Configuration]),
            ],
        );
        let out = render("q", &[], &[], &g, true);
        assert!(out.text.contains("--- degraded ---"), "{}", out.text);
        assert!(out.text.contains("backfill-ann"), "{}", out.text);
        assert!(out.text.contains("configuration"), "{}", out.text);
        assert_eq!(
            out.structured.unwrap()["degradations"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    /// Nothing at all is still an answer that says so, with the map section
    /// intact — never an empty body.
    #[test]
    fn an_empty_result_still_says_so() {
        let g = grounding(vec![], vec![]);
        let out = render("nothing matches this", &[], &[], &g, true);
        assert!(out.text.contains("No passages matched"), "{}", out.text);
        assert!(!out.is_error, "an empty answer is not an error");
        assert_eq!(
            out.structured.unwrap()["passages"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }
}
