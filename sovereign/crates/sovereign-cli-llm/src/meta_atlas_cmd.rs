// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign meta-atlas` subcommand — build + inspect the
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

use corpus_engine::meta_atlas::{
    build_meta_atlas, default_meta_atlas_path, read_meta_atlas, write_meta_atlas, MetaAtlasFile,
    MetaAtom,
};
use corpus_engine::stream_axes::Articulation;

pub async fn run_meta_atlas(args: &[String]) -> i32 {
    if args.is_empty() {
        print_help();
        return 1;
    }
    match args[0].as_str() {
        "build" => cmd_build(&args[1..]).await,
        "list" => cmd_list(&args[1..]).await,
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
        "sovereign meta-atlas <subcommand> [args]\n\
        \n\
        Subcommands:\n\
          build               Walk installed atlases and build canonical_atoms.json.\n\
          list [--key=<>] [--axis=<inventory|argument|trace>]\n\
                              Render meta-atoms from the persisted file.\n\
        \n\
        Persistence: ~/.sovereign/meta-atlas/canonical_atoms.json"
    );
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
                println!("sovereign meta-atlas build [--out <path>] [--indexes-dir <path>]");
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
                    "sovereign meta-atlas list [--key <name>] [--axis <inventory|argument|trace>] [--limit <N>]"
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
            eprintln!("hint: run `sovereign meta-atlas build` first.");
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
