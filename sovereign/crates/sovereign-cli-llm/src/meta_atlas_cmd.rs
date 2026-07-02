// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn meta-atlas` subcommand — build + inspect the
//! cross-corpus canonical meta-atlas.
//!
//! Move 5 Stage 3.
//!
//! Subcommands:
//!   - `build`       walk installed atlases, classify per-atom,
//!                   cluster by canonical_key, persist to
//!                   `~/.sovereign/meta-atlas/canonical_atoms.json`.
//!   - `list`        render meta-atoms; filter by `--key` and/or
//!                   `--axis=<inventory|argument|trace>`.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::enrichment::pipeline::ChatPrompt;
use corpus_engine::meta_atlas::bridge;
use corpus_engine::meta_atlas::{
    build_meta_atlas, default_meta_atlas_path, read_meta_atlas, write_meta_atlas, MetaAtlasFile,
    MetaAtom,
};
use corpus_engine::stream_axes::Articulation;

use crate::enrich_cmd::inference_client::DaemonInferenceClient;

pub async fn run_meta_atlas(args: &[String]) -> i32 {
    if args.is_empty() {
        print_help();
        return 1;
    }
    match args[0].as_str() {
        "build" => cmd_build(&args[1..]).await,
        "list" => cmd_list(&args[1..]).await,
        "align" => cmd_align(&args[1..]).await,
        "explain" => cmd_explain(&args[1..]).await,
        "probe" => cmd_probe(&args[1..]).await,
        "--help" | "-h" | "help" => {
            print_help();
            0
        }
        other => {
            eprintln!("Unknown meta-atlas subcommand: {other}");
            print_help();
            1
        }
    }
}

fn print_help() {
    println!(
        "svrn meta-atlas <subcommand> [args]\n\
        \n\
        Subcommands:\n\
          build               Walk installed atlases and build canonical_atoms.json.\n\
          list [--key=<>] [--axis=<inventory|argument|trace>]\n\
                              Render meta-atoms from the persisted file.\n\
          align [--dry-run] [--fresh] [--limit=N] [--k=N] [--right=<corpus>]\n\
                [--model=<id>] [--bank=<path>]\n\
                              Build the topic-to-topic bridge from a driver corpus\n\
                              (default: SEP eval-bank slugs) to a searchable right\n\
                              corpus (default: wikipedia). Resumable: a re-run\n\
                              continues from the last checkpoint; --fresh rebuilds.\n\
                              --dry-run prints proposed edges without persisting.\n\
          explain <concept>  Show the bridge edges touching a concept/topic.\n\
          probe <entity>     Fast: which bridge edges an entity/title resolves to\n\
                              via the runtime index (no daemon). Mirrors what\n\
                              `bridge_boost` sees at retrieval time.\n\
        \n\
        Persistence: ~/.sovereign/meta-atlas/{{canonical_atoms,bridge_edges}}.json"
    );
}

const ADJ_SYSTEM: &str =
    "You are a precise ontology-alignment judge. Reply with ONLY the JSON object.";

/// Box an adjudication call as the dyn future the `AdjudicateFn` seam
/// expects. A free fn (rather than an inline closure) so the
/// `Pin<Box<dyn Future + Send>>` coercion happens at an explicit return
/// boundary.
fn adjudicate_call(
    client: Arc<DaemonInferenceClient>,
    req: bridge::AdjudicationRequest,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<Output = corpus_engine::Result<Option<bridge::AdjudicationVerdict>>>
            + Send,
    >,
> {
    Box::pin(async move {
        let user = bridge::build_adjudication_prompt(&req);
        let prompt = ChatPrompt::new(ADJ_SYSTEM, user)
            .with_response_schema("bridge_relation", bridge::adjudication_schema())
            .with_temperature(0.0)
            .with_max_output_tokens(256);
        let text = client.complete(&prompt).await?;
        bridge::parse_adjudication_response(&text)
    })
}

/// `meta-atlas align` — build the topic-to-topic bridge. Corpus-agnostic
/// at the engine level; this command binds the first instantiation
/// (SEP driver → Wikipedia candidates) via flags.
async fn cmd_align(args: &[String]) -> i32 {
    let mut dry_run = false;
    let mut fresh = false;
    let mut limit: Option<usize> = None;
    let mut k = 20usize;
    let mut model = "primary".to_string();
    let mut base = "http://127.0.0.1:9741".to_string();
    let mut bank_path = "sovereign/bench/sep/questions.toml".to_string();
    let mut right_corpus = "wikipedia".to_string();

    for a in args {
        if a == "--dry-run" {
            dry_run = true;
        } else if a == "--fresh" {
            fresh = true;
        } else if a == "--help" || a == "-h" {
            println!("meta-atlas align [--dry-run] [--fresh] [--limit=N] [--k=N] [--right=<corpus>] [--model=<id>] [--base=<url>] [--bank=<path>]");
            println!("  Resumable: a re-run skips topics a prior checkpoint finished. --fresh rebuilds from scratch.");
            return 0;
        } else if let Some(v) = a.strip_prefix("--limit=") {
            limit = v.parse().ok();
        } else if let Some(v) = a.strip_prefix("--k=") {
            k = v.parse().unwrap_or(20);
        } else if let Some(v) = a.strip_prefix("--right=") {
            right_corpus = v.to_string();
        } else if let Some(v) = a.strip_prefix("--model=") {
            model = v.to_string();
        } else if let Some(v) = a.strip_prefix("--base=") {
            base = v.to_string();
        } else if let Some(v) = a.strip_prefix("--bank=") {
            bank_path = v.to_string();
        } else {
            eprintln!("meta-atlas align: unknown flag '{a}'");
            return 1;
        }
    }

    let indexes_dir = if let Ok(d) = std::env::var("SOVEREIGN_DATA_DIR") {
        PathBuf::from(d).join("indexes")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".sovereign").join("indexes")
    };

    // Pilot driver set = distinct expected_sources (slugs) in the eval bank.
    let bank = match crate::eval_cmd::bank::load_bank(std::path::Path::new(&bank_path)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: load bank {bank_path}: {e}");
            return 1;
        }
    };
    let mut slugs: Vec<String> = bank
        .questions
        .iter()
        .flat_map(|q| q.expected_sources.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if let Some(n) = limit {
        slugs.truncate(n);
    }

    // Build driver topics for slugs whose per-article atlas is on disk.
    let mut left_topics = Vec::new();
    let mut missing = 0usize;
    for slug in &slugs {
        let corpus_id = format!("sep-{slug}");
        let atlas_dir = indexes_dir.join(&corpus_id).join("atlas");
        if atlas_dir.join("atoms.json").exists() {
            left_topics.push(bridge::DriverTopic {
                corpus_id,
                topic_id: slug.clone(),
                atlas_dir,
            });
        } else {
            missing += 1;
        }
    }
    if left_topics.is_empty() {
        eprintln!(
            "error: no driver atlases found on disk for the {} bank slugs",
            slugs.len()
        );
        return 1;
    }

    // Detect a link graph for the right corpus (generic — not wiki-specific).
    let right_has_link_graph =
        corpus_engine::wikipedia_graph::WikipediaGraph::default_db_path(&indexes_dir, &right_corpus)
            .exists();

    eprintln!(
        "bridge align: {} driver topics ({} slugs without an atlas) · right={} (link_graph={}) · k={} · model={} · {}",
        left_topics.len(),
        missing,
        right_corpus,
        right_has_link_graph,
        k,
        model,
        if dry_run {
            "DRY RUN"
        } else if fresh {
            "persist · FRESH"
        } else {
            "persist · resume-capable"
        },
    );

    // EmbedFn — one client consumed into closures.
    let embed = match DaemonInferenceClient::new(base.clone(), model.clone(), "qwen3-embedding-0.6b")
    {
        Ok(c) => {
            let (embed, _chat) = c.into_closures();
            embed
        }
        Err(e) => {
            eprintln!("error: daemon client (embed): {e}\nIs the daemon running? Try `svrn daemon start`.");
            return 1;
        }
    };

    // AdjudicateFn — a second client kept for grammar-constrained complete().
    let adj_client = match DaemonInferenceClient::new(base.clone(), model.clone(), "qwen3-embedding-0.6b")
    {
        Ok(c) => Arc::new(c.with_max_output_tokens(256)),
        Err(e) => {
            eprintln!("error: daemon client (adjudicate): {e}");
            return 1;
        }
    };
    let adjudicate: bridge::AdjudicateFn =
        Arc::new(move |req| adjudicate_call(adj_client.clone(), req));

    let cfg = bridge::BridgeBuildConfig {
        indexes_dir,
        left_topics,
        right_corpus_id: right_corpus,
        right_has_link_graph,
        k_candidates: k,
        dry_run,
        fresh,
        edges_out: None,
    };

    match bridge::build_bridge(&cfg, embed, adjudicate).await {
        Ok(report) => {
            render_align_report(&report, dry_run);
            0
        }
        Err(e) => {
            eprintln!("error: bridge build failed: {e}");
            1
        }
    }
}

fn source_str(s: bridge::EdgeSource) -> &'static str {
    match s {
        bridge::EdgeSource::Deterministic => "det",
        bridge::EdgeSource::Adjudicated => "llm",
    }
}

fn render_edge(e: &bridge::BridgeEdge) {
    let sigs = e
        .signals_fired
        .iter()
        .map(|x| x.as_str())
        .collect::<Vec<_>>()
        .join(",");
    println!(
        "  {:8} {:>3} {:.2}  {}  →  {}",
        e.relation.as_str(),
        source_str(e.source),
        e.confidence,
        e.left.title,
        e.right.title
    );
    let rationale = e
        .rationale
        .as_deref()
        .map(|r| format!("  · {r}"))
        .unwrap_or_default();
    println!("           [{sigs}]{rationale}");
}

fn render_align_report(report: &bridge::BridgeBuildReport, dry_run: bool) {
    let s = &report.stats;
    println!();
    println!(
        "=== bridge align {} ===",
        if dry_run {
            "(dry run — nothing persisted)"
        } else {
            "(persisted)"
        }
    );
    println!(
        "driver topics: {}   candidates: {}   edges: {}",
        s.left_topics,
        s.candidates,
        report.edges.len()
    );
    if s.skipped_done > 0 {
        println!(
            "  resumed: {} topic(s) already done in a prior checkpoint (skipped)",
            s.skipped_done
        );
    }
    println!(
        "  auto-same: {}   adjudicated: {}   dropped: {}   errors: {}",
        s.auto_same, s.adjudicated, s.dropped, s.errors
    );
    if report.edges.is_empty() {
        println!("  (no edges proposed)");
        return;
    }
    println!();
    let mut edges: Vec<&bridge::BridgeEdge> = report.edges.iter().collect();
    edges.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for e in edges {
        render_edge(e);
    }
}

/// `meta-atlas probe <entity>` — fast, daemon-free check of exactly what
/// `bridge_boost` would resolve at retrieval time: load the runtime
/// `BridgeIndex` and `lookup` the surface form (which keys on titles AND
/// each topic's constituent entity keys). The tight iteration loop for
/// the entity-keying fix.
async fn cmd_probe(args: &[String]) -> i32 {
    let Some(surface) = args.iter().find(|a| !a.starts_with('-')).cloned() else {
        eprintln!("usage: meta-atlas probe <entity-or-title>");
        return 1;
    };
    let idx = match bridge::BridgeIndex::load(None) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: load bridge index: {e}");
            return 1;
        }
    };
    let hits = idx.lookup(&surface);
    println!(
        "\"{surface}\" → {} edge(s)  (bridge index: {} edges total)",
        hits.len(),
        idx.len()
    );
    for e in &hits {
        render_edge(e.as_ref());
    }
    0
}

/// `meta-atlas explain <concept>` — show the persisted bridge edges that
/// touch a concept (matched by case-insensitive title substring).
async fn cmd_explain(args: &[String]) -> i32 {
    let Some(concept) = args.iter().find(|a| !a.starts_with('-')).cloned() else {
        eprintln!("usage: meta-atlas explain <concept>");
        return 1;
    };
    let Some(path) = bridge::default_bridge_edges_path() else {
        eprintln!("error: $HOME unset");
        return 1;
    };
    let file = match bridge::read_bridge_edges(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "error: read {}: {e}\nRun `meta-atlas align` (without --dry-run) first.",
                path.display()
            );
            return 1;
        }
    };
    let needle = concept.to_lowercase();
    let keys: std::collections::BTreeSet<String> = file
        .topics_seen
        .iter()
        .filter(|t| t.title.to_lowercase().contains(&needle))
        .map(|t| t.key())
        .collect();
    if keys.is_empty() {
        println!(
            "no topic matching \"{concept}\" in the bridge ({} topics, {} edges).",
            file.topics_seen.len(),
            file.edges.len()
        );
        return 0;
    }
    for key in &keys {
        for e in file.edges_for(key) {
            render_edge(e);
        }
    }
    0
}

async fn cmd_build(args: &[String]) -> i32 {
    let mut out_path =
        default_meta_atlas_path().unwrap_or_else(|| PathBuf::from("./canonical_atoms.json"));
    let mut indexes_dir: Option<PathBuf> = None;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--out" => match iter.next() {
                Some(v) => out_path = PathBuf::from(v),
                None => {
                    eprintln!("--out requires a path");
                    return 1;
                }
            },
            "--indexes-dir" => match iter.next() {
                Some(v) => indexes_dir = Some(PathBuf::from(v)),
                None => {
                    eprintln!("--indexes-dir requires a path");
                    return 1;
                }
            },
            "--help" | "-h" => {
                println!("svrn meta-atlas build [--out <path>] [--indexes-dir <path>]");
                return 0;
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 1;
            }
        }
    }

    let indexes_dir = indexes_dir.unwrap_or_else(|| {
        sovereign_core::setup_config::SetupConfig::load()
            .map(|c| c.data.dir)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".sovereign")
            })
            .join("indexes")
    });

    eprintln!("meta-atlas: scanning {}", indexes_dir.display());
    let file = match build_meta_atlas(&indexes_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: build failed: {e}");
            return 1;
        }
    };

    print_diagnostics(&file);

    if let Err(e) = write_meta_atlas(&file, &out_path) {
        eprintln!("error: write {}: {e}", out_path.display());
        return 1;
    }
    eprintln!(
        "\nwrote {} ({} meta-atoms)",
        out_path.display(),
        file.atoms.len()
    );
    0
}

fn print_diagnostics(file: &MetaAtlasFile) {
    println!("Atlases seen: {}", file.atlases_seen.len());
    println!("{:<32} {:>10} {:<12}", "corpus", "entities", "stability");
    println!("{}", "─".repeat(64));
    for a in &file.atlases_seen {
        let stab = a
            .stability
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| "—".into());
        println!(
            "{:<32} {:>10} {:<12}",
            a.corpus_id, a.eligible_entities, stab
        );
    }
    println!("\nMeta-atoms: {}", file.atoms.len());

    // Articulation histogram across all anchors.
    let mut inv = 0usize;
    let mut arg = 0usize;
    let mut trc = 0usize;
    let mut ambig = 0usize;
    for atom in &file.atoms {
        for anchor in &atom.anchors {
            if anchor.articulation.is_ambiguous(0.05) {
                ambig += 1;
                continue;
            }
            match anchor.articulation.dominant() {
                Articulation::Inventory => inv += 1,
                Articulation::Argument => arg += 1,
                Articulation::Trace => trc += 1,
            }
        }
    }
    let total = inv + arg + trc + ambig;
    if total == 0 {
        return;
    }
    let pct = |n: usize| (n as f32 / total as f32) * 100.0;
    println!("\nArticulation histogram (per-anchor dominant):");
    println!("  inventory  {:>8}  ({:>5.1}%)", inv, pct(inv));
    println!("  argument   {:>8}  ({:>5.1}%)", arg, pct(arg));
    println!("  trace      {:>8}  ({:>5.1}%)", trc, pct(trc));
    if ambig > 0 {
        println!(
            "  ambiguous  {:>8}  ({:>5.1}%)  [flagged for review]",
            ambig,
            pct(ambig)
        );
    }
}

async fn cmd_list(args: &[String]) -> i32 {
    let mut path =
        default_meta_atlas_path().unwrap_or_else(|| PathBuf::from("./canonical_atoms.json"));
    let mut key_filter: Option<String> = None;
    let mut axis_filter: Option<Articulation> = None;
    let mut limit: usize = 40;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--path" => match iter.next() {
                Some(v) => path = PathBuf::from(v),
                None => {
                    eprintln!("--path requires an argument");
                    return 1;
                }
            },
            "--key" => match iter.next() {
                Some(v) => key_filter = Some(v.clone()),
                None => {
                    eprintln!("--key requires an argument");
                    return 1;
                }
            },
            "--axis" => match iter.next() {
                Some(v) => {
                    axis_filter = match v.as_str() {
                        "inventory" => Some(Articulation::Inventory),
                        "argument" => Some(Articulation::Argument),
                        "trace" => Some(Articulation::Trace),
                        other => {
                            eprintln!(
                                "--axis must be one of inventory|argument|trace, got {other}"
                            );
                            return 1;
                        }
                    };
                }
                None => {
                    eprintln!("--axis requires an argument");
                    return 1;
                }
            },
            "--limit" => match iter.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(n) => limit = n,
                None => {
                    eprintln!("--limit requires an unsigned integer");
                    return 1;
                }
            },
            "--help" | "-h" => {
                println!(
                    "svrn meta-atlas list [--key <name>] [--axis <inventory|argument|trace>] [--limit <N>]"
                );
                return 0;
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 1;
            }
        }
    }

    let file = match read_meta_atlas(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: read {}: {e}", path.display());
            eprintln!("hint: run `svrn meta-atlas build` first.");
            return 1;
        }
    };

    let matched: Vec<&MetaAtom> = file
        .atoms
        .iter()
        .filter(|m| {
            if let Some(k) = &key_filter {
                let needle = corpus_engine::atlas_canonical::lookup_key(k);
                if !needle.is_empty() && m.canonical_key != needle && !m.aliases.contains(&needle) {
                    return false;
                }
            }
            if let Some(axis) = axis_filter {
                if !m.anchors.iter().any(|a| a.articulation.dominant() == axis) {
                    return false;
                }
            }
            true
        })
        .collect();

    println!("Matched {} meta-atoms", matched.len());
    for atom in matched.iter().take(limit) {
        println!(
            "\n[{}] {} (aliases: {})",
            atom.canonical_key,
            atom.display,
            atom.aliases.iter().cloned().collect::<Vec<_>>().join(", ")
        );
        for anchor in &atom.anchors {
            let stab = anchor
                .stability
                .map(|s| s.as_str().to_string())
                .unwrap_or_else(|| "—".into());
            let articulation_dom = if anchor.articulation.is_ambiguous(0.05) {
                "ambiguous".to_string()
            } else {
                anchor.articulation.dominant().as_str().to_string()
            };
            println!(
                "    {:<28} articulation={:<10} stability={:<10} salience={:.2} chunk={}",
                anchor.corpus_id,
                articulation_dom,
                stab,
                anchor.salience,
                anchor.primary_chunk.chunk_id,
            );
        }
    }
    0
}
