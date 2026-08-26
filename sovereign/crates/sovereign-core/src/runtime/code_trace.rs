// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code-intelligence-in-chat — the runtime evidence augmentation (Inc 2 slice 2b).
//!
//! When retrieval surfaces a code-intel *summary* chunk (written by
//! `corpus_engine::enrichment::code_intel::store`, which tags every such chunk
//! `source = "code_intel_summary"` in its metadata JSON), this module renders a
//! call-graph trace for the matched symbol and appends it to the synthesis
//! evidence, so the model can answer "how does X work / connect to Y" with the
//! real call graph rather than guessing from prose.
//!
//! **Two graph sources, preferring the richer one (Inc 7).** When the owning
//! corpus has a v2 code *atlas* (an [`AtlasGraph`] that loads on the Lance
//! backend — `atoms.lance` + `edges.csr`, the only backend carrying
//! `ScipStructural` call-edge provenance), we narrate the **N-hop CallChain**
//! ([`AtlasGraph::call_chain`], callees, depth 3) from the seed symbol — the
//! connected flow, not just the immediate neighbours. Strictly best-effort: no
//! v2 atlas / no call edges / any error falls back to the original **1-hop**
//! caller/callee trace over the corpus's `scip_graph.db`
//! (`corpus-engine-scip`), and a corpus with neither simply contributes no
//! block. Either way the block keeps the same `Call-graph trace for `X`` handle
//! the synthesis directive already consumes, so the chat handlers need no change.
//!
//! The chat answer path is single-shot retrieve -> synthesize, so this is a
//! *deterministic* augmentation, NOT an agentic tool-loop.
//!
//! Lean by construction: depends only on `corpus-engine-scip` (a SQLite reader
//! over `scip_graph.db`), never on the tree-sitter grammars that BUILT the
//! graph. That read/write split is the whole point — the chat runtime ships in
//! every binary, and dragging 5 grammar crates into it to *read* a graph would
//! be backwards. The grammars stay confined to the indexing path that writes the
//! db (the CLI `enrich`/`code index` verbs and the daemon watcher).
//!
//! Best-effort throughout: a missing/corrupt db, a corpus with no graph, or a
//! symbol with no recorded edges all yield no trace and never disturb the answer.

use std::path::PathBuf;

use corpus_engine::ScoredChunk;
use corpus_engine_scip::{build_symbol_trace, render_trace, ScipGraph};

use crate::atlas_context::{render_call_chain_brief, AtlasGraph, CallChainResult, CallDirection};

/// Metadata marker `code_intel::store::insert_chunk_for` stamps on every
/// summary chunk. The per-chunk detector — precise (only actual summary chunks
/// get traced) and independent of whether the corpus is tagged `CorpusKind::Code`.
const CODE_INTEL_SOURCE: &str = "code_intel_summary";

/// Cap on distinct symbols traced per turn. Bounds prompt cost and reflects the
/// §5 hardening finding "never trust retrieval rank-1": we attach traces for the
/// top few code hits as evidence (letting the model + graph disambiguate),
/// rather than betting the answer on chunk #1 alone.
const MAX_TRACED_SYMBOLS: usize = 3;

/// Multi-hop CallChain depth for the chat evidence block (Inc 7). Matches
/// `enrich atlas-query`'s `DEFAULT_DEPTH` so chat and the CLI narrate the same
/// chain. `AtlasGraph::call_chain` clamps to `1..=5`.
const CHAIN_DEPTH: usize = 3;

/// Per-node fanout cap on the chain BFS — a hot symbol referencing dozens of
/// callees can't explode the block. Matches `atlas_query::CALL_FANOUT`.
const CHAIN_FANOUT: usize = 12;

/// A code-intel hit distilled from one retrieved chunk's metadata.
struct CodeHit {
    corpus_id: String,
    /// Short symbol name — the key `find_callers` matches on.
    symbol: String,
    /// SCIP descriptor — the key `find_callees_qualified` needs (may be empty).
    qualified_name: String,
}

/// `<data>/indexes/<corpus_id>` — the per-corpus index root, matching the
/// daemon's writer (`sovereign-cli-dev/project_cmd.rs`) and the atlas reader
/// (`runtime/evidence_loop.rs`). The scip db and the v2 atlas dir are siblings
/// under it; one canonical layout, several readers.
fn index_root(corpus_id: &str) -> PathBuf {
    sovereign_contracts::rebrand::data_dir()
        .join("indexes")
        .join(corpus_id)
}

/// `<data>/indexes/<corpus_id>/scip_graph.db` — the 1-hop trace's SQLite graph.
fn scip_db_path(corpus_id: &str) -> PathBuf {
    index_root(corpus_id).join("scip_graph.db")
}

/// `<data>/indexes/<corpus_id>/atlas` — the v2 code-atlas dir (`atoms.lance` +
/// `edges.csr`), mirroring `runtime/evidence_loop.rs`'s resolution exactly.
fn atlas_dir_path(corpus_id: &str) -> PathBuf {
    index_root(corpus_id).join("atlas")
}

/// Distill the distinct code-intel summary hits from a retrieved chunk set, in
/// retrieval order, deduped by qualified name (falling back to symbol), capped
/// at [`MAX_TRACED_SYMBOLS`]. Pure — unit-tested without a graph.
fn code_hits(chunks: &[ScoredChunk]) -> Vec<CodeHit> {
    let mut seen = std::collections::HashSet::new();
    let mut hits = Vec::new();
    for c in chunks {
        if c.metadata.get("source").map(String::as_str) != Some(CODE_INTEL_SOURCE) {
            continue;
        }
        let symbol = c.metadata.get("symbol").cloned().unwrap_or_default();
        if symbol.is_empty() {
            continue;
        }
        let qualified_name = c
            .metadata
            .get("qualified_name")
            .cloned()
            .unwrap_or_default();
        let key = if qualified_name.is_empty() {
            symbol.clone()
        } else {
            qualified_name.clone()
        };
        if !seen.insert(key) {
            continue;
        }
        hits.push(CodeHit {
            corpus_id: c.corpus_id.clone(),
            symbol,
            qualified_name,
        });
        if hits.len() >= MAX_TRACED_SYMBOLS {
            break;
        }
    }
    hits
}

/// Group hits by corpus, preserving first-seen order, so each `scip_graph.db` is
/// opened exactly once even when several traced symbols share a corpus.
fn group_by_corpus(hits: Vec<CodeHit>) -> Vec<(String, Vec<CodeHit>)> {
    let mut grouped: Vec<(String, Vec<CodeHit>)> = Vec::new();
    for hit in hits {
        if let Some((_, bucket)) = grouped.iter_mut().find(|(c, _)| *c == hit.corpus_id) {
            bucket.push(hit);
        } else {
            grouped.push((hit.corpus_id.clone(), vec![hit]));
        }
    }
    grouped
}

/// Build the call-graph evidence block for any code-intel summary chunks in the
/// retrieved set. Returns an empty string when there are none — zero overhead
/// for the common (non-code) corpus, so it is safe to call unconditionally.
///
/// Per traced symbol it prefers the **multi-hop CallChain** off the corpus's v2
/// code atlas ([`v2_chain_block`]) and falls back to the **1-hop SCIP trace**
/// ([`build_symbol_trace`]/[`render_trace`]) when there's no v2 atlas, no call
/// edges for that seed, or any error — the fallback is per-hit, so a leaf
/// symbol can take the 1-hop path while a connected one takes the chain. Both
/// shapes carry the `Call-graph trace for `X`` handle the synthesis directive
/// consumes, so the chat handlers are unchanged.
pub async fn build_code_trace_block(chunks: &[ScoredChunk]) -> String {
    let hits = code_hits(chunks);
    if hits.is_empty() {
        return String::new();
    }

    let mut blocks = Vec::new();
    let mut chain_hits = 0usize; // glassbox: used the multi-hop atlas chain
    let mut one_hop_hits = 0usize; // vs. fell back to the 1-hop scip trace
    for (corpus_id, hits) in group_by_corpus(hits) {
        // Load the v2 code atlas once per corpus (best-effort). The load reads
        // `atoms.lance` resident and bridges async→sync on a scoped thread, so
        // offload it from the async worker rather than blocking it on the join.
        let atlas = {
            let corpus = corpus_id.clone();
            tokio::task::spawn_blocking(move || load_v2_code_atlas(&corpus))
                .await
                .ok()
                .flatten()
        };
        // Open the scip db lazily — only when a hit actually needs the fallback.
        let mut scip: Option<Option<ScipGraph>> = None;
        for hit in &hits {
            // 1) Preferred: the connected N-hop call chain from the v2 atlas.
            if let Some(block) = atlas.as_ref().and_then(|g| v2_chain_block(g, hit)) {
                blocks.push(block);
                chain_hits += 1;
                continue;
            }
            // 2) Fallback: today's 1-hop caller/callee trace over scip_graph.db.
            let graph = scip.get_or_insert_with(|| open_scip_graph(&corpus_id));
            let Some(graph) = graph.as_ref() else {
                continue;
            };
            match build_symbol_trace(graph, &hit.symbol, &hit.qualified_name).await {
                Ok(trace) => {
                    blocks.push(render_trace(&trace));
                    one_hop_hits += 1;
                }
                Err(e) => tracing::debug!(
                    target: "runtime.code_trace",
                    symbol = %hit.symbol,
                    error = %e,
                    "build_symbol_trace failed; skipping",
                ),
            }
        }
    }

    if blocks.is_empty() {
        return String::new();
    }
    // Glassbox: operators can see the feature fire and which graph each hit used.
    tracing::info!(
        target: "runtime.code_trace",
        chain = chain_hits,
        one_hop = one_hop_hits,
        "injected call-graph evidence into synthesis (multi-hop chain preferred, 1-hop fallback)",
    );
    let mut out = String::from(
        "CODE CALL-GRAPH (resolved from the indexed source — use it to trace how the \
         code connects across call hops; a `dyn-dispatch` marker is a trait/dynamic \
         boundary where a text search would miss the link):\n\n",
    );
    out.push_str(&blocks.join("\n"));
    out
}

/// The `Call-graph trace for `X`` handles present in a rendered trace block.
///
/// The grounding gate verifies a released answer against a sealed evidence
/// universe built from the retrieved CHUNKS. The call-graph block is injected
/// into the synthesis prompt but was never added to that universe, so every
/// fact the model took from it — the callers, their file:line sites, the
/// dyn-dispatch boundaries — came back flagged *"could not be confirmed against
/// your sources"*. That is exactly backwards: SCIP edges are compiler-resolved
/// ground truth, the most reliable evidence in the turn, while the thing the
/// gate DID trust was prose. (Contrast the deliberate RAPTOR exclusion in
/// `gate_evidence_chunks`: that excludes *abstractive LLM paraphrase*. A call
/// graph is the opposite kind of artifact.)
///
/// Callers pair these labels with the block body so a `[Source: Call-graph
/// trace for `X`]` citation resolves and the claims verify.
pub fn trace_source_labels(block: &str) -> Vec<String> {
    block
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("Call-graph trace for ")?;
            let sym = rest.trim_end_matches(':').trim().trim_matches('`');
            if sym.is_empty() {
                None
            } else {
                // Both spellings: the backticked handle as rendered, and the
                // bare symbol, because models cite it either way (observed:
                // `[Source: Call-graph trace for gate_answer]`, no backticks).
                Some([
                    format!("Call-graph trace for `{sym}`"),
                    format!("Call-graph trace for {sym}"),
                ])
            }
        })
        .flatten()
        .collect()
}

/// Load the v2 code atlas for a corpus — the Lance store (`atoms.lance` +
/// `edges.csr`) that records the `ScipStructural` edge provenance `call_chain`
/// needs. `load_from_disk` returns `Ok` only for a v2 store, so a successful
/// load is always call-edge-capable; failure routes the hit to the 1-hop
/// fallback. Best-effort: missing dir / unreadable store → `None`, never a
/// panic. Synchronous (call from a blocking context); see the `spawn_blocking`
/// wrapper at the call site.
fn load_v2_code_atlas(corpus_id: &str) -> Option<AtlasGraph> {
    let atlas_dir = atlas_dir_path(corpus_id);
    if !atlas_dir.exists() {
        return None;
    }
    let graph = match AtlasGraph::load_from_disk(corpus_id, &atlas_dir) {
        Ok(g) => g,
        Err(e) => {
            tracing::debug!(
                target: "runtime.code_trace",
                corpus = %corpus_id,
                error = %e,
                "atlas load failed; 1-hop fallback",
            );
            return None;
        }
    };
    Some(graph)
}

/// Open a corpus's `scip_graph.db` for the 1-hop fallback. `None` (with a debug
/// trace) when the db is absent or unreadable — best-effort, like the rest.
fn open_scip_graph(corpus_id: &str) -> Option<ScipGraph> {
    let path = scip_db_path(corpus_id);
    if !path.exists() {
        tracing::debug!(
            target: "runtime.code_trace",
            corpus = %corpus_id,
            "no scip_graph.db; cannot build a 1-hop trace",
        );
        return None;
    }
    match ScipGraph::open(&path, corpus_id) {
        Ok(g) => Some(g),
        Err(e) => {
            tracing::debug!(
                target: "runtime.code_trace",
                corpus = %corpus_id,
                error = %e,
                "scip_graph.db open failed; cannot build a 1-hop trace",
            );
            None
        }
    }
}

/// Build the multi-hop CallChain block for one code hit off the v2 atlas. Seeds
/// by the recorded qualified name (a whole-name match in `resolve_symbol_seed`,
/// the strongest), falling back to the short symbol; then BFSs the callees to
/// [`CHAIN_DEPTH`]. `None` when the seed can't be resolved or the chain is a
/// singleton (see [`format_chain_block`]) — the caller then uses the 1-hop trace.
fn v2_chain_block(graph: &AtlasGraph, hit: &CodeHit) -> Option<String> {
    let query = if hit.qualified_name.is_empty() {
        hit.symbol.as_str()
    } else {
        hit.qualified_name.as_str()
    };
    let seed_id = graph.resolve_symbol_seed(query)?;
    let result = graph.call_chain(&seed_id, CallDirection::Callees, CHAIN_DEPTH, CHAIN_FANOUT);
    format_chain_block(&hit.symbol, &result)
}

/// Format a [`CallChainResult`] as a `Call-graph trace for `X`` evidence block —
/// the SAME handle the 1-hop [`render_trace`] emits and the synthesis directive
/// cites (`[Source: Call-graph trace for `X`]`), so the model treats a chain and
/// a 1-hop trace identically. `None` for a **singleton** chain (just the seed —
/// a leaf symbol, or a seed with no `ScipStructural` callees), which adds nothing
/// over the 1-hop trace (the latter also surfaces callers); the caller falls
/// back there. Pure — unit-tested without a graph.
fn format_chain_block(symbol: &str, result: &CallChainResult) -> Option<String> {
    if result.nodes.len() <= 1 {
        return None;
    }
    let mut block = format!("Call-graph trace for `{symbol}`:\n");
    block.push_str(&render_call_chain_brief(result));
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate seals the trace into its evidence universe using these labels;
    /// if they don't match what the model actually cites, every call-graph
    /// fact comes back "could not be confirmed".
    #[test]
    fn trace_labels_cover_both_citation_spellings() {
        let block = "CODE CALL-GRAPH (resolved from the indexed source):\n\n\
                     Call-graph trace for `gate_answer`:\n  callers: handle_simple\n\
                     Call-graph trace for `gate_held_answer`:\n  callers: stream_turn\n";
        let labels = trace_source_labels(block);
        assert!(labels.contains(&"Call-graph trace for `gate_answer`".to_string()));
        // Observed live: the model drops the backticks when citing.
        assert!(labels.contains(&"Call-graph trace for gate_answer".to_string()));
        assert!(labels.contains(&"Call-graph trace for `gate_held_answer`".to_string()));
        assert_eq!(labels.len(), 4);
    }

    #[test]
    fn trace_labels_empty_for_a_non_code_turn() {
        assert!(trace_source_labels("").is_empty());
        assert!(trace_source_labels("just some prose about gating").is_empty());
    }
    use crate::atlas_context::CallChainNode;
    use std::collections::HashMap;

    fn chunk(corpus: &str, meta: &[(&str, &str)]) -> ScoredChunk {
        let metadata: HashMap<String, String> = meta
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ScoredChunk {
            content: "summary text".to_string(),
            title: None,
            url: None,
            corpus_id: corpus.to_string(),
            score: 1.0,
            metadata,
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
            // Fixture chunk: nothing acquired it (TOPOLOGY §10 rung 9.1).
            provenance: corpus_engine::index::ChunkProvenance::manufactured("test_fixture"),
        }
    }

    #[test]
    fn only_code_intel_summary_chunks_become_hits() {
        let chunks = vec![
            // A normal prose chunk — ignored.
            chunk("wiki", &[("source", "document")]),
            // A code-intel summary — picked up.
            chunk(
                "codecorpus",
                &[
                    ("source", "code_intel_summary"),
                    ("symbol", "route_query"),
                    ("qualified_name", "crate::router::route_query"),
                ],
            ),
        ];
        let hits = code_hits(&chunks);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol, "route_query");
        assert_eq!(hits[0].qualified_name, "crate::router::route_query");
        assert_eq!(hits[0].corpus_id, "codecorpus");
    }

    #[test]
    fn dedups_by_qualified_name_and_caps() {
        // Same symbol twice (two chunks for one symbol) collapses to one hit;
        // four distinct symbols cap at MAX_TRACED_SYMBOLS.
        let mut chunks = vec![
            chunk(
                "c",
                &[
                    ("source", "code_intel_summary"),
                    ("symbol", "a"),
                    ("qualified_name", "crate::a"),
                ],
            ),
            chunk(
                "c",
                &[
                    ("source", "code_intel_summary"),
                    ("symbol", "a"),
                    ("qualified_name", "crate::a"),
                ],
            ),
        ];
        for name in ["b", "d", "e", "f"] {
            chunks.push(chunk(
                "c",
                &[
                    ("source", "code_intel_summary"),
                    ("symbol", name),
                    ("qualified_name", "crate::x"),
                ],
            ));
        }
        // Note: b/d/e/f share qualified_name "crate::x" so they dedup to ONE; a
        // is distinct -> 2 distinct keys total, under the cap.
        let hits = code_hits(&chunks);
        assert_eq!(hits.len(), 2, "deduped by qualified_name");
        assert_eq!(hits[0].symbol, "a");
    }

    #[test]
    fn caps_at_max_traced_symbols() {
        // Ten chunks with distinct qualified names -> ten distinct hits, capped.
        let chunks: Vec<ScoredChunk> = (0..10)
            .map(|i| {
                let metadata: HashMap<String, String> = [
                    ("source".to_string(), "code_intel_summary".to_string()),
                    ("symbol".to_string(), format!("s{i}")),
                    ("qualified_name".to_string(), format!("crate::s{i}")),
                ]
                .into_iter()
                .collect();
                ScoredChunk {
                    content: String::new(),
                    title: None,
                    url: None,
                    corpus_id: "c".to_string(),
                    score: 1.0,
                    metadata,
                    chunk_id: None,
                    source_doc_id: None,
                    vector_distance: None,
                    // Fixture chunk: nothing acquired it (TOPOLOGY §10 rung 9.1).
                    provenance: corpus_engine::index::ChunkProvenance::manufactured("test_fixture"),
                }
            })
            .collect();
        let hits = code_hits(&chunks);
        assert_eq!(hits.len(), MAX_TRACED_SYMBOLS);
    }

    #[tokio::test]
    async fn empty_when_no_code_chunks() {
        let chunks = vec![chunk("wiki", &[("source", "document")])];
        assert!(build_code_trace_block(&chunks).await.is_empty());
    }

    // ── Inc 7: multi-hop chain block formatting + fallback gate ─────────────

    fn node(name: &str, depth: usize, via: Option<&str>, dyn_dispatch: bool) -> CallChainNode {
        CallChainNode {
            atom_id: format!("entity-{name}"),
            name: name.to_string(),
            subtype: "function".to_string(),
            description: String::new(),
            chunk_id: String::new(),
            depth,
            via: via.map(str::to_string),
            via_dyn_dispatch: dyn_dispatch,
        }
    }

    fn chain(nodes: Vec<CallChainNode>) -> CallChainResult {
        CallChainResult {
            corpus_id: "c".to_string(),
            direction: CallDirection::Callees,
            max_depth: CHAIN_DEPTH,
            nodes,
            truncated: false,
        }
    }

    #[test]
    fn multi_hop_chain_becomes_a_call_graph_trace_block() {
        // A genuine multi-hop chain (seed → depth-1 → depth-2) renders under the
        // canonical `Call-graph trace for `X`` handle, with the brief's call-order
        // narration and `→` arrows (which only depth>0 nodes get).
        let result = chain(vec![
            node("semver::matches", 0, None, false),
            node(
                "semver::eval::matches_req",
                1,
                Some("entity-semver::matches"),
                false,
            ),
            node(
                "semver::is_empty",
                2,
                Some("entity-semver::eval::matches_req"),
                true,
            ),
        ]);
        let block = format_chain_block("matches", &result).expect("multi-hop chain → block");
        assert!(
            block.starts_with("Call-graph trace for `matches`:\n"),
            "must keep the handle the synthesis directive consumes, got:\n{block}"
        );
        assert!(
            block.contains("Call chain —"),
            "carries the multi-hop brief header"
        );
        assert!(block.contains('→'), "depth>0 arrow proves it is multi-hop");
        assert!(block.contains("semver::eval::matches_req"));
        assert!(block.contains("[dyn-dispatch]"), "trait boundary flagged");
    }

    #[test]
    fn singleton_chain_returns_none_so_caller_uses_1hop() {
        // Seed only (a leaf, or a seed with no scip callees): the chain adds
        // nothing over the 1-hop trace, so format_chain_block declines and the
        // caller falls back. This is the "no call edges → 1-hop" gate.
        let result = chain(vec![node("leaf", 0, None, false)]);
        assert!(format_chain_block("leaf", &result).is_none());
        assert!(
            format_chain_block("leaf", &chain(vec![])).is_none(),
            "miss → None too"
        );
    }

    #[tokio::test]
    async fn no_atlas_and_no_scip_yields_no_block() {
        // A code-intel hit for a corpus that has neither a v2 atlas nor a
        // scip_graph.db: best-effort produces nothing and never panics (proving
        // the v2→1-hop→nothing chain of fallbacks degrades gracefully).
        let chunks = vec![chunk(
            "definitely-not-a-real-corpus-7f3a",
            &[
                ("source", "code_intel_summary"),
                ("symbol", "foo"),
                ("qualified_name", "crate::foo"),
            ],
        )];
        assert!(build_code_trace_block(&chunks).await.is_empty());
    }

    /// Integration check against the REAL on-disk v2 code atlas built by the
    /// Inc-5 work (`semver-self-atlas`). Skips when the corpus isn't installed
    /// under the resolved data dir (CI without `~/.svrnmesh`), so it's a no-op
    /// there but an end-to-end proof on a developer box: chunk → load v2 atlas →
    /// seed `semver::matches` → BFS callees → multi-hop block.
    #[tokio::test]
    async fn semver_self_atlas_yields_multi_hop_chain() {
        let corpus = "semver-self-atlas";
        if !atlas_dir_path(corpus).join("atoms.lance").exists() {
            eprintln!("skipping semver_self_atlas_yields_multi_hop_chain: {corpus} v2 atlas not installed");
            return;
        }
        let chunks = vec![chunk(
            corpus,
            &[
                ("source", "code_intel_summary"),
                ("symbol", "matches"),
                ("qualified_name", "semver::matches"),
            ],
        )];
        let block = build_code_trace_block(&chunks).await;
        assert!(
            !block.is_empty(),
            "expected a code-trace block for {corpus}"
        );
        assert!(
            block.contains("Call-graph trace for `matches`"),
            "expected the canonical handle, got:\n{block}"
        );
        assert!(
            block.contains("Call chain —"),
            "expected the multi-hop brief (not the 1-hop trace), got:\n{block}"
        );
        assert!(
            block.contains('→'),
            "expected depth>0 nodes — a genuine multi-hop chain, got:\n{block}"
        );
    }
}
