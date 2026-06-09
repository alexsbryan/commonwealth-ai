// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign reading-diag` — headless validator for the glass-box
//! reading-surface chain.
//!
//! The desktop's reading surface stitches together six data flows:
//!   1. Search returns chunks with stable `chunk_id`s.
//!   2. `CorpusIndex::neighbors` resolves a chunk to its prev/center/next
//!      siblings within `source_doc_id`.
//!   3. `atlas_traversal::detect_atom_spans` finds atom mentions in the
//!      cited chunk's text using the section_id ↔ chunk projection.
//!   4. The atom card endpoint returns one-hop edges + cross-corpus links.
//!   5. The atom-elsewhere endpoint resolves section ids back to chunk ids.
//!   6. The "ask about this passage" preamble survives into the runtime
//!      and the LLM answer references the focused passage.
//!
//! Bugs in any of these manifest in the desktop as silent UI weirdness
//! (wrong popover, no underlines, panel rows that don't jump). This
//! command runs the same code paths the Tauri commands wrap and prints
//! a tree-shaped report so you can bisect end-to-end without the UI
//! loop.
//!
//! **Companion command:** `sovereign chat inspect "<query>"` already
//! handles the *retrieval* side — per-corpus hits with scores, dim
//! eligibility, etc. Use it when "all citations came from the wrong
//! corpus." Use `reading-diag` when "the citation deref returned
//! nothing" or "atom spans are empty for a chunk that obviously
//! mentions Alyosha."

use std::collections::HashMap;
use std::sync::Arc;

use corpus_engine::atlas_traversal::{detect_atom_spans, AtomSpan};
use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_cross_corpus_edges, read_atlas_edges, AtomEnvelope, AtomId, Edge,
};
use corpus_engine::{CorpusEngine, ScoredChunk};

/// Local error/result alias — keeps the harness independent of any
/// particular workspace error type. Errors here are diagnostic
/// strings printed to the operator, not propagated programmatically.
type DiagResult<T> = std::result::Result<T, String>;
use serde_json::json;

use crate::chat_cmd::bootstrap::{build_session, ChatSession};
use crate::chat_cmd::config::parse_globals;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign reading-diag",
    summary:
        "Validate the desktop reading-surface chain end-to-end without the UI.",
    sections: &[
        HelpSection::Usage(
            "sovereign reading-diag query \"<question>\" [--corpus <id>] [--limit N] [--max-spans N] [--format text|json]",
        ),
        HelpSection::Flags(&[
            ("--corpus <id>",     "Restrict retrieval to a single corpus_id (default: every installed)."),
            ("--limit <N>",       "How many top citations to walk through the deref chain (default: 3)."),
            ("--max-spans <N>",   "Cap atom_spans printed per chunk (default: 6)."),
            ("--max-related <N>", "Cap related-atom rows on the atom card (default: 6)."),
            ("--no-atoms",        "Skip atom-layer validation (just chunk + neighbors)."),
            ("--format text|json","Output format (default: text)."),
            ("--help, -h",        "Show this message."),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign reading-diag query \"Is free will compatible with determinism?\"",
                "Run a philosophy question across every installed corpus and walk the top-3 deref chains. Surfaces the 'wrong corpus winning' bug + each citation's atom-layer health.",
            ),
            (
                "sovereign reading-diag query \"Who is Alyosha?\" --corpus brothers_karamazov --limit 1",
                "Validate that a known-good citation in BK derefs cleanly: chunk + neighbors + atom_spans + atom card + elsewhere section→chunk resolution.",
            ),
            (
                "sovereign reading-diag query \"...\" --format json | jq '.citations[].atom_spans'",
                "JSON output for piping into ad-hoc assertions.",
            ),
        ]),
        HelpSection::Notes(
            "Calls into the corpus engine + atlas readers DIRECTLY (no Tauri, no \
             daemon HTTP) — same code the desktop's `read_get_chunk_neighbors` / \
             `read_get_atom_card` Tauri commands wrap. If `chat inspect` is \
             green and `reading-diag` is red, the bug is in the reading-surface \
             chain (chunk_id plumbing, neighbor projection, AtomSpan detector, \
             section→chunk resolver). If both are red, the bug is upstream in \
             retrieval.",
        ),
    ],
};

#[derive(Copy, Clone, Debug)]
enum OutputFormat {
    Text,
    Json,
}

struct CmdArgs {
    question: String,
    corpus_filter: Option<String>,
    limit: usize,
    max_spans: usize,
    max_related: usize,
    skip_atoms: bool,
    format: OutputFormat,
}

pub async fn run(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }

    // First arg is the subcommand. Today we only have `query`; named
    // explicitly so future scenarios (`reading-diag scenario <toml>`,
    // `reading-diag chunk <corpus> <id>`) can land without breaking
    // the call shape.
    let (sub, rest) = (args[0].as_str(), &args[1..]);
    match sub {
        "query" => run_query(rest).await,
        other => {
            eprintln!("error: unknown subcommand `{other}`. Try `query`.");
            help::print(&HELP);
            2
        }
    }
}

async fn run_query(args: &[String]) -> i32 {
    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let parsed = match parse_query_args(&rest) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            help::print(&HELP);
            return 2;
        }
    };

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    match run_diag(&session, &parsed).await {
        Ok(report) => {
            match parsed.format {
                OutputFormat::Text => print_text(&report),
                OutputFormat::Json => print_json(&report),
            }
            0
        }
        Err(e) => {
            eprintln!("reading-diag failed: {e}");
            1
        }
    }
}

fn parse_query_args(args: &[String]) -> DiagResult<CmdArgs> {
    let mut question: Option<String> = None;
    let mut corpus_filter: Option<String> = None;
    let mut limit: usize = 3;
    let mut max_spans: usize = 6;
    let mut max_related: usize = 6;
    let mut skip_atoms = false;
    let mut format = OutputFormat::Text;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" => {
                i += 1;
                corpus_filter = args.get(i).cloned();
                if corpus_filter.is_none() {
                    return Err("--corpus needs a value".into());
                }
            }
            "--limit" => {
                i += 1;
                limit = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| "--limit needs a number".to_string())?;
            }
            "--max-spans" => {
                i += 1;
                max_spans = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| "--max-spans needs a number".to_string())?;
            }
            "--max-related" => {
                i += 1;
                max_related = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| "--max-related needs a number".to_string())?;
            }
            "--no-atoms" => {
                skip_atoms = true;
            }
            "--format" => {
                i += 1;
                format = match args.get(i).map(String::as_str) {
                    Some("text") => OutputFormat::Text,
                    Some("json") => OutputFormat::Json,
                    Some(other) => {
                        return Err(format!("--format expects text|json, got `{other}`"));
                    }
                    None => return Err("--format needs a value".into()),
                };
            }
            "--help" | "-h" => {
                help::print(&HELP);
                std::process::exit(0);
            }
            arg if question.is_none() => question = Some(arg.to_string()),
            extra => return Err(format!("unexpected argument `{extra}`")),
        }
        i += 1;
    }

    let question = question
        .ok_or_else(|| "missing question. Usage: reading-diag query \"<text>\"".to_string())?;
    Ok(CmdArgs {
        question,
        corpus_filter,
        limit,
        max_spans,
        max_related,
        skip_atoms,
        format,
    })
}

// ─── Report shape ────────────────────────────────────────────

#[derive(Debug)]
struct DiagReport {
    question: String,
    embed_dims: usize,
    per_corpus: Vec<CorpusBucket>,
    citations: Vec<CitationDeref>,
    summary: Summary,
}

#[derive(Debug)]
struct CorpusBucket {
    corpus_id: String,
    chunk_count: u64,
    embedding_dims: usize,
    dim_match: bool,
    /// `true` when this corpus would actually appear in chat
    /// retrieval — Knowledge or Catalog kind. Code-kind corpora are
    /// filtered out by `Runtime::search_corpus_indexes` before merge,
    /// so we mirror that to avoid presenting a false-positive
    /// "wrong corpus winning" picture.
    chat_eligible: bool,
    /// `true` when this corpus's hits used the hybrid (vector + FTS)
    /// path. `false` means FTS-only, whose raw BM25 scores are NOT
    /// on the same [0,1] RRF scale as hybrid scores — comparing
    /// them in a merge is exactly the "wrong corpus wins" failure
    /// mode `chat inspect` users see when a small or dim-mismatched
    /// corpus floats to the top.
    hybrid: bool,
    hits: usize,
    top_score: Option<f32>,
    skip_reason: Option<&'static str>,
}

#[derive(Debug)]
struct CitationDeref {
    rank: usize,
    corpus_id: String,
    chunk_id: Option<u64>,
    title: Option<String>,
    score: f32,
    /// Cross-corpus comparable signal: cosine distance from query
    /// embedding to chunk's stored embedding (`1 - cos_sim`,
    /// lower = better, range `[0, 2]`). When this is populated for
    /// every citation, the rank shown is the *vector-distance*
    /// rank, not the RRF score rank.
    vector_distance: Option<f32>,
    chunk_id_ok: bool,
    snippet: String,
    neighbors: NeighborStatus,
    atom_spans: AtomSpansStatus,
    first_atom_card: Option<AtomCardStatus>,
    elsewhere: Option<ElsewhereStatus>,
}

#[derive(Debug)]
struct NeighborStatus {
    attempted: bool,
    found: bool,
    prev_count: usize,
    next_count: usize,
    failure: Option<String>,
}

#[derive(Debug)]
struct AtomSpansStatus {
    attempted: bool,
    section_id: Option<String>,
    span_count: usize,
    sample_spans: Vec<SampleSpan>,
    /// Number of spans whose `&text[start..end] != surface_form`
    /// — should always be zero. Non-zero = byte-offset bug.
    invalid_offsets: usize,
    failure: Option<String>,
}

#[derive(Debug)]
struct SampleSpan {
    atom_id: String,
    atom_type: String,
    surface_form: String,
    span_start: usize,
    span_end: usize,
    /// `&text[start..end]` — should equal `surface_form`.
    actual_slice: String,
}

#[derive(Debug)]
struct AtomCardStatus {
    atom_id: String,
    atom_type: String,
    canonical_name: String,
    description_chars: usize,
    related_count: usize,
    cross_corpus_count: usize,
    sample_related: Vec<RelatedSummary>,
}

#[derive(Debug)]
struct RelatedSummary {
    atom_id: String,
    atom_type: String,
    canonical_name: String,
    edge_type: String,
    role: String,
}

#[derive(Debug)]
struct ElsewhereStatus {
    section_count: usize,
    chunks_resolved: usize,
    /// `chunks_resolved / section_count`. Below 0.6 is a smell —
    /// either the chunker isn't stamping section_id or the atom
    /// evidence references sections that no chunk carries.
    resolution_rate: f64,
    sample_unresolved: Vec<String>,
}

#[derive(Debug)]
struct Summary {
    citations_total: usize,
    citations_with_chunk_id: usize,
    citations_with_neighbors: usize,
    citations_with_any_atom_span: usize,
    invalid_byte_offsets: usize,
    avg_elsewhere_resolution: Option<f64>,
}

// ─── Run ─────────────────────────────────────────────────────

async fn run_diag(session: &ChatSession, args: &CmdArgs) -> DiagResult<DiagReport> {
    let embedding = session
        .inference
        .embed_query(&args.question)
        .await
        .map_err(|e| format!("embed query: {e}"))?;

    let indexes = session
        .corpus_engine
        .installed_indexes()
        .await
        .map_err(|e| format!("list installed indexes: {e}"))?;

    let mut per_corpus: Vec<CorpusBucket> = Vec::new();
    let mut all_hits: Vec<(String, ScoredChunk)> = Vec::new();
    for info in indexes.iter().filter(|i| {
        args.corpus_filter
            .as_deref()
            .is_none_or(|f| i.corpus_id == f)
    }) {
        let dim_match = info.embedding_dimensions == embedding.len();
        // Mirror Runtime::search_corpus_indexes: drop Code-kind
        // corpora before any chat-style merge. Catalog stays
        // because the runtime keeps it (with a separate evidence
        // tier downstream); reading-surface won't deref a catalog
        // hit since chat's catalog-aware synthesis path doesn't
        // emit them as citations, but the count is informative.
        let chat_eligible = matches!(
            info.kind,
            corpus_engine::CorpusKind::Knowledge | corpus_engine::CorpusKind::Catalog
        );
        let skip_reason = if !chat_eligible {
            Some("Code-kind corpus — runtime filters out of chat retrieval")
        } else {
            None
        };

        if !chat_eligible {
            // Don't open or search Code corpora — record a row in
            // per_corpus so the user sees them, but don't mix their
            // scores into the merge.
            per_corpus.push(CorpusBucket {
                corpus_id: info.corpus_id.clone(),
                chunk_count: info.chunk_count,
                embedding_dims: info.embedding_dimensions,
                dim_match,
                chat_eligible,
                hybrid: false,
                hits: 0,
                top_score: None,
                skip_reason,
            });
            continue;
        }

        let query_vec: &[f32] = if dim_match { &embedding } else { &[] };
        let hybrid = dim_match && !embedding.is_empty();
        let idx = match session.corpus_engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(e) => {
                eprintln!(
                    "warning: open_index for {} failed: {e}; skipping",
                    info.corpus_id
                );
                continue;
            }
        };
        let hits = match idx
            .search(query_vec, &args.question, args.limit.max(1))
            .await
        {
            Ok(h) => h,
            Err(e) => {
                eprintln!(
                    "warning: search for {} failed: {e}; skipping",
                    info.corpus_id
                );
                continue;
            }
        };
        per_corpus.push(CorpusBucket {
            corpus_id: info.corpus_id.clone(),
            chunk_count: info.chunk_count,
            embedding_dims: info.embedding_dimensions,
            dim_match,
            chat_eligible,
            hybrid,
            hits: hits.len(),
            top_score: hits.first().map(|h| h.score),
            skip_reason: None,
        });
        for h in hits {
            all_hits.push((info.corpus_id.clone(), h));
        }
    }

    // Merge across corpora. Two sort keys, layered:
    //
    //   1. Primary: `vector_distance` (asc) when populated. This is
    //      the apples-to-apples cross-corpus signal — raw cosine
    //      distance from the query embedding, comparable across any
    //      corpora that share the same embedding model. Solves the
    //      RRF-saturation bug where a small corpus's top-1 hit
    //      beats a large corpus's semantically-better answer because
    //      the small one happened to land at rank-1 in BOTH vector
    //      and FTS while the large one only landed at rank-1 in one.
    //
    //   2. Fallback: RRF `score` (desc) for chunks where
    //      `vector_distance` is None — FTS-only paths, mesh hits,
    //      synthetic atlas chunks. Keeps mixed-source results sane.
    //
    // Hits with vector_distance always rank above hits without (a
    // None vector distance is treated as +infinity so it loses to
    // any real distance).
    all_hits.sort_by(|a, b| {
        let av = a.1.vector_distance;
        let bv = b.1.vector_distance;
        match (av, bv) {
            (Some(ad), Some(bd)) => ad.partial_cmp(&bd).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => {
                b.1.score
                    .partial_cmp(&a.1.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        }
    });
    all_hits.truncate(args.limit);

    let mut citations: Vec<CitationDeref> = Vec::new();
    for (rank, (corpus_id, chunk)) in all_hits.into_iter().enumerate() {
        let deref = walk_citation(rank + 1, &corpus_id, &chunk, &session.corpus_engine, args).await;
        citations.push(deref);
    }

    let summary = summarize(&citations);

    Ok(DiagReport {
        question: args.question.clone(),
        embed_dims: embedding.len(),
        per_corpus,
        citations,
        summary,
    })
}

async fn walk_citation(
    rank: usize,
    corpus_id: &str,
    chunk: &ScoredChunk,
    engine: &Arc<CorpusEngine>,
    args: &CmdArgs,
) -> CitationDeref {
    let chunk_id = chunk.chunk_id;
    let chunk_id_ok = chunk_id.is_some();

    let snippet: String = chunk
        .content
        .chars()
        .take(160)
        .collect::<String>()
        .replace('\n', " ");

    // Neighbors — only attempt when chunk_id present.
    let neighbors = match chunk_id {
        None => NeighborStatus {
            attempted: false,
            found: false,
            prev_count: 0,
            next_count: 0,
            failure: Some("chunk_id missing on search result".into()),
        },
        Some(id) => match engine.open_index_for_corpus(corpus_id).await {
            Err(e) => NeighborStatus {
                attempted: true,
                found: false,
                prev_count: 0,
                next_count: 0,
                failure: Some(format!("open_index: {e}")),
            },
            Ok(idx) => match idx.neighbors(id, 1).await {
                Err(e) => NeighborStatus {
                    attempted: true,
                    found: false,
                    prev_count: 0,
                    next_count: 0,
                    failure: Some(format!("neighbors: {e}")),
                },
                Ok(None) => NeighborStatus {
                    attempted: true,
                    found: false,
                    prev_count: 0,
                    next_count: 0,
                    failure: Some("neighbors returned None — center chunk not found by id".into()),
                },
                Ok(Some(window)) => NeighborStatus {
                    attempted: true,
                    found: true,
                    prev_count: window.prev.len(),
                    next_count: window.next.len(),
                    failure: None,
                },
            },
        },
    };

    let atom_spans = if args.skip_atoms {
        AtomSpansStatus {
            attempted: false,
            section_id: None,
            span_count: 0,
            sample_spans: Vec::new(),
            invalid_offsets: 0,
            failure: None,
        }
    } else {
        compute_atom_spans(corpus_id, chunk, engine, args).await
    };

    let (first_atom_card, elsewhere) = if args.skip_atoms || atom_spans.span_count == 0 {
        (None, None)
    } else {
        let first_atom_id = atom_spans.sample_spans[0].atom_id.clone();
        let card = compute_atom_card(corpus_id, &first_atom_id, engine, args).await;
        let els = compute_elsewhere(corpus_id, &first_atom_id, engine).await;
        (card, els)
    };

    CitationDeref {
        rank,
        corpus_id: corpus_id.to_string(),
        chunk_id,
        title: chunk.title.clone(),
        score: chunk.score,
        vector_distance: chunk.vector_distance,
        chunk_id_ok,
        snippet,
        neighbors,
        atom_spans,
        first_atom_card,
        elsewhere,
    }
}

async fn compute_atom_spans(
    corpus_id: &str,
    chunk: &ScoredChunk,
    engine: &Arc<CorpusEngine>,
    args: &CmdArgs,
) -> AtomSpansStatus {
    let installed = match engine.installed_indexes().await {
        Ok(i) => i,
        Err(e) => {
            return AtomSpansStatus {
                attempted: true,
                section_id: None,
                span_count: 0,
                sample_spans: Vec::new(),
                invalid_offsets: 0,
                failure: Some(format!("installed_indexes: {e}")),
            };
        }
    };
    let entry = match installed.iter().find(|i| i.corpus_id == corpus_id) {
        Some(e) => e,
        None => {
            return AtomSpansStatus {
                attempted: true,
                section_id: None,
                span_count: 0,
                sample_spans: Vec::new(),
                invalid_offsets: 0,
                failure: Some(format!("corpus `{corpus_id}` not in installed_indexes")),
            };
        }
    };
    let atlas_dir = entry.path.join("atlas");
    if !atlas_dir.exists() {
        return AtomSpansStatus {
            attempted: true,
            section_id: chunk.metadata.get("section_id").cloned(),
            span_count: 0,
            sample_spans: Vec::new(),
            invalid_offsets: 0,
            failure: Some("no atlas/ dir for this corpus — atom layer disabled".into()),
        };
    }
    let atoms = match read_atlas_atoms(&atlas_dir) {
        Ok(f) => f.atoms,
        Err(e) => {
            return AtomSpansStatus {
                attempted: true,
                section_id: chunk.metadata.get("section_id").cloned(),
                span_count: 0,
                sample_spans: Vec::new(),
                invalid_offsets: 0,
                failure: Some(format!("read atoms: {e}")),
            };
        }
    };

    // section_id sourcing: ScoredChunk.metadata is HashMap<String,
    // String> populated from the chunk's metadata JSON. Sectioned
    // chunker writes section_id there. If absent → atom layer no-ops.
    let section_id = chunk.metadata.get("section_id").cloned();
    let spans = detect_atom_spans(&chunk.content, section_id.as_deref(), &atoms);
    let mut invalid_offsets = 0;
    let sample_spans: Vec<SampleSpan> = spans
        .iter()
        .take(args.max_spans)
        .map(|s| sample_from_span(&chunk.content, s))
        .inspect(|s| {
            if s.actual_slice != s.surface_form {
                invalid_offsets += 1;
            }
        })
        .collect();

    AtomSpansStatus {
        attempted: true,
        section_id,
        span_count: spans.len(),
        sample_spans,
        invalid_offsets,
        failure: None,
    }
}

fn sample_from_span(text: &str, s: &AtomSpan) -> SampleSpan {
    let actual_slice = if s.span_start <= s.span_end && s.span_end <= text.len() {
        text.get(s.span_start..s.span_end)
            .unwrap_or("<bad utf8 boundary>")
            .to_string()
    } else {
        "<out of bounds>".to_string()
    };
    SampleSpan {
        atom_id: s.atom_id.clone(),
        atom_type: s.atom_type.to_string(),
        surface_form: s.surface_form.clone(),
        span_start: s.span_start,
        span_end: s.span_end,
        actual_slice,
    }
}

async fn compute_atom_card(
    corpus_id: &str,
    atom_id: &str,
    engine: &Arc<CorpusEngine>,
    args: &CmdArgs,
) -> Option<AtomCardStatus> {
    let atlas_dir = atlas_dir_for_corpus(engine, corpus_id).await?;
    let atoms = read_atlas_atoms(&atlas_dir).ok()?.atoms;
    let target = AtomId::from_raw(atom_id.to_string());
    let atom = atoms.iter().find(|a| *a.id() == target)?;
    let edges = read_atlas_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    let cross = read_atlas_cross_corpus_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();

    let (atom_type, canonical_name, description) = atom_brief(atom);
    let related: Vec<&Edge> = edges
        .iter()
        .filter(|e| e.source == target || e.target == target)
        .collect();
    let cross_count = cross
        .iter()
        .filter(|e| e.edge.source == target || e.edge.target == target)
        .count();

    let mut sample_related: Vec<RelatedSummary> = Vec::new();
    let by_id: HashMap<&AtomId, &AtomEnvelope> = atoms.iter().map(|a| (a.id(), a)).collect();
    for e in related.iter().take(args.max_related) {
        let (other_id, role) = if e.source == target {
            (&e.target, "source")
        } else {
            (&e.source, "target")
        };
        let Some(other) = by_id.get(other_id) else {
            continue;
        };
        let (other_type, other_name, _) = atom_brief(other);
        sample_related.push(RelatedSummary {
            atom_id: other_id.as_str().to_string(),
            atom_type: other_type.into(),
            canonical_name: other_name,
            edge_type: format!("{:?}", e.edge_type).to_lowercase(),
            role: role.to_string(),
        });
    }

    Some(AtomCardStatus {
        atom_id: target.as_str().to_string(),
        atom_type: atom_type.into(),
        canonical_name,
        description_chars: description.chars().count(),
        related_count: related.len(),
        cross_corpus_count: cross_count,
        sample_related,
    })
}

async fn compute_elsewhere(
    corpus_id: &str,
    atom_id: &str,
    engine: &Arc<CorpusEngine>,
) -> Option<ElsewhereStatus> {
    let atlas_dir = atlas_dir_for_corpus(engine, corpus_id).await?;
    let atoms = read_atlas_atoms(&atlas_dir).ok()?.atoms;
    let target = AtomId::from_raw(atom_id.to_string());
    let atom = atoms.iter().find(|a| *a.id() == target)?;
    let evidence = atom_evidence_section_ids(atom);
    let mut unique: Vec<String> = Vec::new();
    {
        let mut seen = std::collections::HashSet::new();
        for s in &evidence {
            if seen.insert(s.clone()) {
                unique.push(s.clone());
            }
        }
    }
    let index = engine.open_index_for_corpus(corpus_id).await.ok()?;
    let resolved = index
        .resolve_sections_to_chunks(&unique)
        .await
        .unwrap_or_default();
    let chunks_resolved = resolved.len();
    let section_count = unique.len();
    let resolution_rate = if section_count == 0 {
        0.0
    } else {
        chunks_resolved as f64 / section_count as f64
    };
    let sample_unresolved: Vec<String> = unique
        .iter()
        .filter(|s| !resolved.contains_key(s.as_str()))
        .take(5)
        .cloned()
        .collect();

    Some(ElsewhereStatus {
        section_count,
        chunks_resolved,
        resolution_rate,
        sample_unresolved,
    })
}

// ─── Atom helpers (mirror reading_http) ──────────────────────

async fn atlas_dir_for_corpus(
    engine: &Arc<CorpusEngine>,
    corpus_id: &str,
) -> Option<std::path::PathBuf> {
    let installed = engine.installed_indexes().await.ok()?;
    let entry = installed.iter().find(|i| i.corpus_id == corpus_id)?;
    let atlas_dir = entry.path.join("atlas");
    if atlas_dir.exists() {
        Some(atlas_dir)
    } else {
        None
    }
}

fn atom_brief(atom: &AtomEnvelope) -> (&'static str, String, String) {
    match atom {
        AtomEnvelope::Entity(e) => ("entity", e.canonical_name.clone(), e.description.clone()),
        AtomEnvelope::Event(e) => ("event", e.description.clone(), e.description.clone()),
        AtomEnvelope::State(s) => (
            "state",
            s.label.clone(),
            format!("State of {}: {}", s.entity_id.as_str(), s.label),
        ),
        AtomEnvelope::Relation(r) => ("relation", r.label.clone(), r.label.clone()),
        AtomEnvelope::Claim(c) => ("claim", c.content.clone(), c.content.clone()),
        AtomEnvelope::Question(q) => ("question", q.content.clone(), q.content.clone()),
        AtomEnvelope::Configuration(c) => ("configuration", c.label.clone(), c.description.clone()),
        AtomEnvelope::ArgumentReconstruction(a) => (
            "argument",
            a.name.clone(),
            format!(
                "{} (P1..P{}, conclusion present: {})",
                a.name,
                a.premises.len(),
                !a.conclusion.is_empty()
            ),
        ),
        AtomEnvelope::Position(p) => ("position", p.canonical_name.clone(), p.content.clone()),
        AtomEnvelope::Opposition(o) => (
            "opposition",
            o.canonical_label.clone(),
            if o.framing.is_empty() {
                format!("{} vs {}", o.left_label, o.right_label)
            } else {
                o.framing.clone()
            },
        ),
        AtomEnvelope::Asset(a) => (
            "asset",
            if a.original_filename.is_empty() {
                format!("{} asset", a.asset_kind)
            } else {
                a.original_filename.clone()
            },
            format!(
                "{} bytes, sha256:{}",
                a.size,
                &a.sha256[..16.min(a.sha256.len())]
            ),
        ),
    }
}

fn atom_evidence_section_ids(atom: &AtomEnvelope) -> Vec<String> {
    match atom {
        AtomEnvelope::Entity(e) => vec![e.first_appearance.chunk_id.clone()],
        AtomEnvelope::Event(e) => {
            let mut out = vec![e.section_position.section_id.clone()];
            out.extend(e.evidence.iter().map(|c| c.chunk_id.clone()));
            out
        }
        AtomEnvelope::State(s) => s.evidence.iter().map(|c| c.chunk_id.clone()).collect(),
        AtomEnvelope::Relation(r) => r.evidence.iter().map(|c| c.chunk_id.clone()).collect(),
        AtomEnvelope::Claim(c) => c.evidence.iter().map(|cr| cr.chunk_id.clone()).collect(),
        AtomEnvelope::Question(q) => q.raised_at.iter().map(|c| c.chunk_id.clone()).collect(),
        AtomEnvelope::Configuration(c) => c.evidence.iter().map(|cr| cr.chunk_id.clone()).collect(),
        AtomEnvelope::ArgumentReconstruction(a) => {
            let mut out = vec![a.section_position.section_id.clone()];
            out.extend(a.evidence.iter().map(|c| c.chunk_id.clone()));
            out
        }
        AtomEnvelope::Position(_) | AtomEnvelope::Opposition(_) => {
            unreachable!("typed atoms wired in Gap B Stage 4")
        }
        AtomEnvelope::Asset(_) => Vec::new(),
    }
}

// ─── Summary + output ────────────────────────────────────────

fn summarize(citations: &[CitationDeref]) -> Summary {
    let mut s = Summary {
        citations_total: citations.len(),
        citations_with_chunk_id: 0,
        citations_with_neighbors: 0,
        citations_with_any_atom_span: 0,
        invalid_byte_offsets: 0,
        avg_elsewhere_resolution: None,
    };
    let mut elsewhere_rates: Vec<f64> = Vec::new();
    for c in citations {
        if c.chunk_id_ok {
            s.citations_with_chunk_id += 1;
        }
        if c.neighbors.found && (c.neighbors.prev_count + c.neighbors.next_count) > 0 {
            s.citations_with_neighbors += 1;
        }
        if c.atom_spans.span_count > 0 {
            s.citations_with_any_atom_span += 1;
        }
        s.invalid_byte_offsets += c.atom_spans.invalid_offsets;
        if let Some(e) = &c.elsewhere {
            elsewhere_rates.push(e.resolution_rate);
        }
    }
    if !elsewhere_rates.is_empty() {
        s.avg_elsewhere_resolution =
            Some(elsewhere_rates.iter().sum::<f64>() / elsewhere_rates.len() as f64);
    }
    s
}

fn print_text(report: &DiagReport) {
    println!("══════════════════════════════════════════════════════════════");
    println!("query   : {}", report.question);
    println!("embed   : {} dims", report.embed_dims);
    println!("══════════════════════════════════════════════════════════════");
    println!();

    println!("CORPUS DISTRIBUTION (mirrors Runtime::search_corpus_indexes filtering)");
    if report.per_corpus.is_empty() {
        println!("  (no installed corpora matched the filter)");
    } else {
        let mut has_fts_only = false;
        let mut has_hybrid = false;
        for c in &report.per_corpus {
            let dim_tag = if c.dim_match { "✓dims" } else { "✗dims" };
            let mode_tag = if !c.chat_eligible {
                "skip "
            } else if c.hybrid {
                has_hybrid = true;
                "hybrid"
            } else {
                has_fts_only = true;
                "fts  "
            };
            let top = c
                .top_score
                .map(|s| format!("top={s:.3}"))
                .unwrap_or_else(|| "no hits".into());
            let reason = c
                .skip_reason
                .map(|r| format!("  ({r})"))
                .unwrap_or_default();
            println!(
                "  {:32} {:>7} chunks  {dim_tag}  {mode_tag}  {:>3} hits  {top}{reason}",
                c.corpus_id, c.chunk_count, c.hits,
            );
        }
        if has_fts_only && has_hybrid {
            println!();
            println!(
                "  ⚠ score-scale warning: FTS-only and hybrid scores are NOT comparable.\n  \
                   FTS returns raw BM25 (often > 1.0); hybrid returns RRF-style ([0, 1]).\n  \
                   Merging them by score is the canonical 'wrong corpus wins' bug."
            );
        }
    }
    println!();

    println!(
        "DEREF CHAIN (top {} citations after merge)",
        report.summary.citations_total
    );
    if report.citations.is_empty() {
        println!("  (no citations to walk)");
        return;
    }
    for c in &report.citations {
        let title = c.title.as_deref().unwrap_or("<untitled>");
        let cid = c
            .chunk_id
            .map(|i| i.to_string())
            .unwrap_or_else(|| "—".into());
        let chunk_tag = if c.chunk_id_ok { "✓" } else { "✗" };
        let dist_tag = c
            .vector_distance
            .map(|d| format!("vec_dist={d:.3}"))
            .unwrap_or_else(|| "vec_dist=—".into());
        println!();
        println!(
            "  [{:>2}] {dist_tag}  rrf={:.3}  {}  ({}, chunk_id={cid} {chunk_tag})",
            c.rank, c.score, title, c.corpus_id
        );
        println!("       snippet: {}", c.snippet);

        // Neighbors
        if c.neighbors.attempted {
            if c.neighbors.found {
                println!(
                    "       neighbors: ✓  prev={} next={}",
                    c.neighbors.prev_count, c.neighbors.next_count
                );
            } else {
                println!(
                    "       neighbors: ✗  ({})",
                    c.neighbors.failure.as_deref().unwrap_or("unknown")
                );
            }
        } else {
            println!("       neighbors: skipped");
        }

        // Atom spans
        let spans = &c.atom_spans;
        if spans.attempted {
            let sec = spans.section_id.as_deref().unwrap_or("none");
            let off_tag = if spans.invalid_offsets == 0 {
                "✓".to_string()
            } else {
                format!("✗ {} bad", spans.invalid_offsets)
            };
            if let Some(failure) = &spans.failure {
                println!("       atom_spans: ✗  ({failure})");
            } else {
                println!(
                    "       atom_spans: {} found  section={sec}  offsets:{off_tag}",
                    spans.span_count
                );
                for s in &spans.sample_spans {
                    let mismatch = if s.actual_slice == s.surface_form {
                        ""
                    } else {
                        "  ⚠ slice ≠ surface_form"
                    };
                    println!(
                        "         · {} ({}) [{}..{}] \"{}\"{mismatch}",
                        s.atom_id, s.atom_type, s.span_start, s.span_end, s.surface_form
                    );
                }
            }
        }

        // Atom card
        if let Some(card) = &c.first_atom_card {
            println!(
                "       first atom: {} ({}) — {}  related={} cross={}",
                card.atom_id,
                card.atom_type,
                card.canonical_name,
                card.related_count,
                card.cross_corpus_count
            );
            for r in &card.sample_related {
                println!(
                    "           ↪ {} ({}) {}—{}→ {}",
                    r.atom_id, r.atom_type, r.role, r.edge_type, r.canonical_name
                );
            }
        }

        // Elsewhere
        if let Some(e) = &c.elsewhere {
            let rate_pct = (e.resolution_rate * 100.0) as u32;
            let rate_tag = if e.resolution_rate >= 0.6 {
                "✓"
            } else {
                "⚠ low"
            };
            println!(
                "       elsewhere: {} sections, {} resolved to chunks ({rate_pct}% {rate_tag})",
                e.section_count, e.chunks_resolved
            );
            if !e.sample_unresolved.is_empty() {
                println!(
                    "           unresolved sample: {}",
                    e.sample_unresolved.join(", ")
                );
            }
        }
    }

    // Summary footer
    let s = &report.summary;
    println!();
    println!("══════════════════════════════════════════════════════════════");
    println!("SUMMARY");
    println!(
        "  citations:           {}/{} carry chunk_id",
        s.citations_with_chunk_id, s.citations_total
    );
    println!(
        "  neighbors found:     {}/{}",
        s.citations_with_neighbors, s.citations_total
    );
    println!(
        "  atom spans present:  {}/{}",
        s.citations_with_any_atom_span, s.citations_total
    );
    println!(
        "  invalid byte offsets:{}",
        if s.invalid_byte_offsets == 0 {
            "  none ✓".to_string()
        } else {
            format!("  {} ✗", s.invalid_byte_offsets)
        }
    );
    if let Some(rate) = s.avg_elsewhere_resolution {
        println!("  avg elsewhere rate:  {:.0}%", rate * 100.0);
    }
}

fn print_json(report: &DiagReport) {
    let payload = json!({
        "query": report.question,
        "embed_dims": report.embed_dims,
        "corpus_distribution": report.per_corpus.iter().map(|c| json!({
            "corpus_id": c.corpus_id,
            "chunk_count": c.chunk_count,
            "embedding_dims": c.embedding_dims,
            "dim_match": c.dim_match,
            "hits": c.hits,
            "top_score": c.top_score,
        })).collect::<Vec<_>>(),
        "citations": report.citations.iter().map(|c| json!({
            "rank": c.rank,
            "corpus_id": c.corpus_id,
            "chunk_id": c.chunk_id,
            "chunk_id_ok": c.chunk_id_ok,
            "title": c.title,
            "rrf_score": c.score,
            "vector_distance": c.vector_distance,
            "snippet": c.snippet,
            "neighbors": {
                "attempted": c.neighbors.attempted,
                "found": c.neighbors.found,
                "prev_count": c.neighbors.prev_count,
                "next_count": c.neighbors.next_count,
                "failure": c.neighbors.failure,
            },
            "atom_spans": {
                "attempted": c.atom_spans.attempted,
                "section_id": c.atom_spans.section_id,
                "span_count": c.atom_spans.span_count,
                "invalid_offsets": c.atom_spans.invalid_offsets,
                "failure": c.atom_spans.failure,
                "sample": c.atom_spans.sample_spans.iter().map(|s| json!({
                    "atom_id": s.atom_id,
                    "atom_type": s.atom_type,
                    "surface_form": s.surface_form,
                    "span_start": s.span_start,
                    "span_end": s.span_end,
                    "actual_slice": s.actual_slice,
                    "byte_offsets_ok": s.actual_slice == s.surface_form,
                })).collect::<Vec<_>>(),
            },
            "first_atom_card": c.first_atom_card.as_ref().map(|card| json!({
                "atom_id": card.atom_id,
                "atom_type": card.atom_type,
                "canonical_name": card.canonical_name,
                "description_chars": card.description_chars,
                "related_count": card.related_count,
                "cross_corpus_count": card.cross_corpus_count,
                "sample_related": card.sample_related.iter().map(|r| json!({
                    "atom_id": r.atom_id,
                    "atom_type": r.atom_type,
                    "canonical_name": r.canonical_name,
                    "edge_type": r.edge_type,
                    "role": r.role,
                })).collect::<Vec<_>>(),
            })),
            "elsewhere": c.elsewhere.as_ref().map(|e| json!({
                "section_count": e.section_count,
                "chunks_resolved": e.chunks_resolved,
                "resolution_rate": e.resolution_rate,
                "sample_unresolved": e.sample_unresolved,
            })),
        })).collect::<Vec<_>>(),
        "summary": {
            "citations_total": report.summary.citations_total,
            "citations_with_chunk_id": report.summary.citations_with_chunk_id,
            "citations_with_neighbors": report.summary.citations_with_neighbors,
            "citations_with_any_atom_span": report.summary.citations_with_any_atom_span,
            "invalid_byte_offsets": report.summary.invalid_byte_offsets,
            "avg_elsewhere_resolution": report.summary.avg_elsewhere_resolution,
        },
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );
}
