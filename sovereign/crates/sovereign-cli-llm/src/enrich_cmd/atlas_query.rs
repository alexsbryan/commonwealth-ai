// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich atlas-query` — classify a natural-language question against
//! a resolved atlas, walk it, and print a cited brief. Two families:
//!
//! 1. **CallChain (Inc 5 — "talk to your architecture").** For a code atlas,
//!    seed an atom by NAME ("what does `matches` call") or by MEANING ("how does
//!    it check whether a version satisfies a requirement"), then BFS the
//!    `ScipStructural` call edges over the v2 CSR — callees by default, callers
//!    with `--callers` — and narrate the chain in call order with depth
//!    indentation and `[dyn-dispatch]` markers. Lives on
//!    [`AtlasGraph::call_chain`]; the Inc-7 chat path reuses the same method.
//! 2. **Classifier variants (legacy).** "Who is X" / "tensions" / "trajectory"
//!    / … route through the prose classifier + traversal engine unchanged.
//!
//! The NAMED CallChain and every classifier variant are pure Rust (no LLM). The
//! CONCEPTUAL CallChain embeds the question once (the only model call) to seed
//! by meaning, preferring the persistent ANN table and cosine-falling-back.

use corpus_engine::atlas_traversal::{
    assemble_brief, classify_query, engine::AtlasView, traverse, QueryPlan,
};
use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_edges, AtomEnvelope, ATLAS_DIRNAME,
};
use sovereign_core::atlas_context::{
    open_and_attach_ann_seed_table, render_call_chain_brief, seed_atom_by_meaning, AtlasGraph,
    CallDirection,
};

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use crate::eval_cmd::runner::{load_atlas_context, AtlasLoadFilter};
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

/// Per-node fanout cap on the CallChain BFS — a hot symbol referencing dozens of
/// callees can't explode the chain. Matches the code-atlas builder's
/// `MAX_CALLEE_FANOUT`.
const CALL_FANOUT: usize = 12;
/// Default BFS depth when `--depth` is omitted.
const DEFAULT_DEPTH: usize = 3;

const HELP: Help = Help {
    command: "svrn enrich atlas-query",
    summary: "Classify + traverse a question against a resolved atlas (CallChain for code).",
    sections: &[
        HelpSection::Usage(
            "svrn enrich atlas-query <corpus-id> \"<question>\" [--depth N] [--callers] [--json]",
        ),
        HelpSection::Flags(&[
            (
                "--depth N",
                "CallChain BFS depth (default 3, capped at 5).",
            ),
            (
                "--callers",
                "Walk CALLERS (who calls / where used) instead of the default CALLEES.",
            ),
            (
                "--json",
                "Emit the structured result (CallChainResult or TraversalResult) as JSON.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich atlas-query semver-self-atlas \"what does the matches function call\" --depth 3",
                "Named CallChain — BFS the scip call edges from `matches`, callees.",
            ),
            (
                "svrn enrich atlas-query semver-self-atlas \"how does it check whether a version satisfies a requirement\"",
                "Conceptual CallChain — embed the question, ANN-seed an atom, then trace.",
            ),
            (
                "svrn enrich atlas-query bk \"Who is Alyosha?\"",
                "Classifier variant — entity lookup over a prose atlas.",
            ),
        ]),
        HelpSection::Notes(
            "CallChain needs the v2 store (atoms.lance + edges.csr) — the only backend \
             that carries edge provenance, so scip call edges can be told from \
             containment. Conceptual seeding needs an embedding model and, for best \
             results, a backfilled ANN table (`svrn atlas backfill-ann <corpus> \
             --atlas-depth structural --atlas-min-description-chars 1`).",
        ),
    ],
};

pub async fn cmd_atlas_query(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    // Globals first (so the conceptual path can build an embedding session),
    // then our positionals + flags from what's left.
    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let parsed = match parse_args(&rest) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    let atlas_dir = paths::index_root(&parsed.corpus_id).join(ATLAS_DIRNAME);
    if !atlas_dir.exists() {
        eprintln!(
            "error: no atlas at {}. Build/install the atlas for `{}` first.",
            atlas_dir.display(),
            parsed.corpus_id
        );
        return 1;
    }

    // Atoms + edges drive the classifier (and the legacy traversal); the graph
    // drives the CallChain BFS.
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: reading {}/atoms.json: {e}.", atlas_dir.display());
            return 1;
        }
    };
    let edges_file = read_atlas_edges(&atlas_dir).map(|f| f.edges).unwrap_or_default();

    let mut entities = Vec::new();
    let mut events = Vec::new();
    let mut states = Vec::new();
    let mut relations = Vec::new();
    let mut claims = Vec::new();
    let mut questions = Vec::new();
    let mut configurations = Vec::new();
    let mut positions = Vec::new();
    let mut oppositions = Vec::new();
    for a in atoms_file.atoms {
        match a {
            AtomEnvelope::Entity(x) => entities.push(x),
            AtomEnvelope::Event(x) => events.push(x),
            AtomEnvelope::State(x) => states.push(x),
            AtomEnvelope::Relation(x) => relations.push(x),
            AtomEnvelope::Claim(x) => claims.push(x),
            AtomEnvelope::Question(x) => questions.push(x),
            AtomEnvelope::Configuration(x) => configurations.push(x),
            AtomEnvelope::Position(x) => positions.push(x),
            AtomEnvelope::Opposition(x) => oppositions.push(x),
            AtomEnvelope::ArgumentReconstruction(_) | AtomEnvelope::Asset(_) => {}
        }
    }

    // Route: an explicit call intent (keywords / `--callers`), or a question the
    // prose classifier can't place, becomes a CallChain. Everything the
    // classifier DOES place (who-is / tensions / trajectory / …) stays legacy.
    let call_dir = detect_call_intent(&parsed.query, parsed.callers);
    let legacy_plan = classify_query(&parsed.query, &entities);
    let do_callchain = call_dir.is_some() || matches!(legacy_plan, QueryPlan::Unknown { .. });

    if do_callchain {
        return run_call_chain(&globals, &parsed, &atlas_dir, call_dir).await;
    }

    // Legacy classifier-driven traversal + brief.
    let view = AtlasView {
        entities: &entities,
        events: &events,
        states: &states,
        relations: &relations,
        claims: &claims,
        questions: &questions,
        configurations: &configurations,
        edges: &edges_file,
        positions: &positions,
        oppositions: &oppositions,
    };
    let result = traverse(&legacy_plan, view);
    if parsed.as_json {
        match serde_json::to_string_pretty(&result) {
            Ok(s) => {
                println!("{s}");
                0
            }
            Err(e) => {
                eprintln!("error: serialising traversal result: {e}");
                1
            }
        }
    } else {
        println!("{}", assemble_brief(&result).to_text());
        0
    }
}

/// CallChain branch — load the graph, seed (named → else conceptual), BFS, and
/// render a cited brief.
async fn run_call_chain(
    globals: &crate::chat_cmd::config::ChatGlobals,
    parsed: &ParsedQuery,
    atlas_dir: &std::path::Path,
    call_dir: Option<CallDirection>,
) -> i32 {
    let direction = call_dir.unwrap_or(CallDirection::Callees);

    let mut graph = match AtlasGraph::load_from_disk(&parsed.corpus_id, atlas_dir) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: loading atlas graph for {}: {e}", parsed.corpus_id);
            return 1;
        }
    };
    // NAMED seed first unless the question is conceptual ("how does it …"), in
    // which case prefer the embedding seed so an accidental token match can't
    // hijack it. Either way the other is the fallback.
    let named = graph.resolve_symbol_seed(&parsed.query);
    let prefer_conceptual = is_how_question(&parsed.query);

    let (seed_id, how): (Option<String>, &str) = if !prefer_conceptual && named.is_some() {
        (named, "named")
    } else {
        match conceptual_seed(globals, &parsed.corpus_id, atlas_dir, &parsed.query, &mut graph).await
        {
            Some((id, score)) => {
                eprintln!("atlas-query: conceptual seed `{id}` (cosine {score:.3})");
                (Some(id), "conceptual")
            }
            None => {
                if named.is_some() {
                    (named, "named (conceptual seed unavailable)")
                } else {
                    (None, "none")
                }
            }
        }
    };

    let Some(seed_id) = seed_id else {
        eprintln!(
            "atlas-query: could not seed a CallChain for `{}` — no symbol named and \
             no conceptual seed (needs an embedding model / ANN table).",
            parsed.query
        );
        return 1;
    };
    eprintln!(
        "atlas-query: CallChain seed={seed_id} ({how}), direction={:?}, depth={}",
        direction, parsed.depth
    );

    let result = graph.call_chain(&seed_id, direction, parsed.depth, CALL_FANOUT);

    if parsed.as_json {
        match serde_json::to_string_pretty(&result) {
            Ok(s) => {
                println!("{s}");
                0
            }
            Err(e) => {
                eprintln!("error: serialising call chain: {e}");
                1
            }
        }
    } else {
        println!("{}", render_call_chain_brief(&result));
        0
    }
}

/// Conceptual seed: build an embedding session, embed the question, attach the
/// ANN seed table to `graph` (so the BFS and the seed share one graph), and pick
/// the nearest atom. Loads the embedding bag only as a cosine fallback when the
/// corpus isn't backfilled. `None` on any failure (caller falls back to named).
async fn conceptual_seed(
    globals: &crate::chat_cmd::config::ChatGlobals,
    corpus_id: &str,
    atlas_dir: &std::path::Path,
    query: &str,
    graph: &mut AtlasGraph,
) -> Option<(String, f32)> {
    let session = match build_session(globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atlas-query: conceptual seed needs a model but build_session failed: {e}");
            return None;
        }
    };
    // Attach the ANN seed table on THIS runtime (the held lancedb::Table is
    // queried below); rebind so the same graph backs the BFS.
    let attached = open_and_attach_ann_seed_table(corpus_id, atlas_dir, graph.clone()).await;
    *graph = attached;

    let embedding = match session.inference.embed_query(query).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("atlas-query: embed query failed: {e}");
            return None;
        }
    };

    // Cosine fallback bag — only when there's no ANN table. Permissive filter so
    // the bag covers every embeddable atom (best seed recall).
    let ctx = if graph.has_ann_seed_table() {
        None
    } else {
        let filter = AtlasLoadFilter {
            min_description_chars: 1,
            depth_allowlist: Vec::new(),
            max_entries: None,
            include_claims: true,
            include_tensions: false,
            include_configurations: false,
        };
        match load_atlas_context(&session, corpus_id, 8, &filter).await {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("atlas-query: no ANN table and embedding bag load failed: {e}");
                None
            }
        }
    };

    seed_atom_by_meaning(&embedding, graph, ctx.as_ref()).await
}

/// Detect a CallChain intent + its direction from the question text (or the
/// `--callers` flag). `None` → no call intent (route to the legacy classifier).
fn detect_call_intent(query: &str, force_callers: bool) -> Option<CallDirection> {
    if force_callers {
        return Some(CallDirection::Callers);
    }
    let q = query.to_lowercase();
    // CALLERS — who reaches this symbol. Checked first (some phrases share the
    // word "call" with the callees set).
    const CALLERS: &[&str] = &[
        "what calls",
        "who calls",
        "callers of",
        "called by",
        "used by",
        "where is",
        "where's",
        "where are",
    ];
    if CALLERS.iter().any(|k| q.contains(k)) || (q.contains("used") && q.contains("where")) {
        return Some(CallDirection::Callers);
    }
    // CALLEES — what this symbol reaches / how it works.
    const CALLEES: &[&str] = &[
        "call",
        "calls",
        "invoke",
        "invokes",
        "how does",
        "how do ",
        "how is",
        "how are",
        "how can",
        "how to",
        "trace",
        "flow",
        "what does",
        "uses",
        "depends on",
    ];
    if CALLEES.iter().any(|k| q.contains(k)) {
        return Some(CallDirection::Callees);
    }
    None
}

/// A conceptual "how does it work" question — prefer the embedding seed so a
/// stray token match can't hijack the chain.
fn is_how_question(query: &str) -> bool {
    let q = query.to_lowercase();
    q.starts_with("how ")
        || q.contains("how does")
        || q.contains("how do ")
        || q.contains("how is ")
        || q.contains("how are ")
        || q.contains("how can")
        || q.contains("how to ")
}

#[derive(Debug)]
struct ParsedQuery {
    corpus_id: String,
    query: String,
    depth: usize,
    callers: bool,
    as_json: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedQuery, String> {
    let mut corpus_id: Option<String> = None;
    let mut query: Option<String> = None;
    let mut depth = DEFAULT_DEPTH;
    let mut callers = false;
    let mut as_json = false;
    let mut positional_count = 0;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--json" => as_json = true,
            "--callers" => callers = true,
            "--depth" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--depth needs a value".to_string())?;
                depth = v
                    .parse()
                    .map_err(|_| format!("--depth: not a positive integer: {v}"))?;
                if depth == 0 {
                    return Err("--depth must be > 0".to_string());
                }
                i += 2;
                continue;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => match positional_count {
                0 => {
                    corpus_id = Some(other.to_string());
                    positional_count += 1;
                }
                1 => {
                    query = Some(other.to_string());
                    positional_count += 1;
                }
                _ => return Err(format!("unexpected positional argument: {other}")),
            },
        }
        i += 1;
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let query = query.ok_or_else(|| "missing <query>".to_string())?;
    if query.trim().is_empty() {
        return Err("query must be non-empty".to_string());
    }
    Ok(ParsedQuery {
        corpus_id,
        query,
        depth,
        callers,
        as_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_corpus_id_and_query() {
        let p = parse_args(&["bk".into(), "Who is Alyosha?".into()]).unwrap();
        assert_eq!(p.corpus_id, "bk");
        assert_eq!(p.query, "Who is Alyosha?");
        assert_eq!(p.depth, DEFAULT_DEPTH);
        assert!(!p.callers);
        assert!(!p.as_json);
    }

    #[test]
    fn parse_args_parses_depth_callers_json() {
        let p = parse_args(&[
            "semver".into(),
            "what calls matches".into(),
            "--depth".into(),
            "2".into(),
            "--callers".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(p.depth, 2);
        assert!(p.callers);
        assert!(p.as_json);
    }

    #[test]
    fn parse_args_rejects_missing_query() {
        let err = parse_args(&["bk".into()]).unwrap_err();
        assert!(err.contains("query"));
    }

    #[test]
    fn parse_args_rejects_zero_depth() {
        let err =
            parse_args(&["bk".into(), "q".into(), "--depth".into(), "0".into()]).unwrap_err();
        assert!(err.contains("> 0"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["bk".into(), "q".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn call_intent_direction_detection() {
        assert_eq!(
            detect_call_intent("what does the matches function call", false),
            Some(CallDirection::Callees)
        );
        assert_eq!(
            detect_call_intent("what calls matches", false),
            Some(CallDirection::Callers)
        );
        assert_eq!(
            detect_call_intent("where is Version used", false),
            Some(CallDirection::Callers)
        );
        assert_eq!(
            detect_call_intent("how does it check a requirement", false),
            Some(CallDirection::Callees)
        );
        // `--callers` forces direction regardless of phrasing.
        assert_eq!(
            detect_call_intent("trace the flow", true),
            Some(CallDirection::Callers)
        );
        // A prose who-is question is not a call intent.
        assert_eq!(detect_call_intent("Who is Alyosha?", false), None);
    }

    #[test]
    fn how_questions_prefer_conceptual_seed() {
        assert!(is_how_question("how does it parse a version requirement"));
        assert!(is_how_question("How is a comparator evaluated?"));
        assert!(!is_how_question("what does matches call"));
    }
}
