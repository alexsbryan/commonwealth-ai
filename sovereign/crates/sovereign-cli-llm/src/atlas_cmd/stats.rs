// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas stats` — Move 6 P8 observability.
//!
//! Per-corpus inventory: atom count, doc count (from
//! `doc_to_atoms.json` sidecar), articulation histogram, stream
//! block (stability). Read-only; safe to run alongside live
//! ingest.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    atoms::AtomEnvelope, doc_to_atoms, writer::read_atlas_atoms,
};
use corpus_engine::stream_axes::{Articulation, ArticulationVector};

pub async fn run(args: &[String]) -> i32 {
    let mut filter: Option<String> = None;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--corpus" => match iter.next() {
                Some(v) => filter = Some(v.clone()),
                None => {
                    eprintln!("--corpus requires an argument");
                    return 1;
                }
            },
            "--all" => {}
            "--help" | "-h" => {
                println!(
                    "svrn atlas stats [--corpus <id>] [--all]\n\
                    \n\
                    Per-corpus atlas inventory: atom count, doc count,\n\
                    articulation distribution, stream block. Read-only."
                );
                return 0;
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 1;
            }
        }
    }

    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
        .join("indexes");

    let entries = match std::fs::read_dir(&indexes_dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("error: read_dir {}: {e}", indexes_dir.display());
            return 1;
        }
    };

    println!(
        "{:<32} {:>8} {:>6} {:>10} {:>9} {:>9} {:>9} {:>10}",
        "corpus", "atoms", "docs", "stability", "inv%", "arg%", "trc%", "doc_sidecar"
    );
    println!("{}", "─".repeat(96));

    let mut sorted: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    sorted.sort();

    for corpus_path in sorted {
        if !corpus_path.is_dir() {
            continue;
        }
        let corpus_id = match corpus_path.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.starts_with('.') && !n.starts_with('_') => n.to_string(),
            _ => continue,
        };
        if let Some(f) = &filter {
            if corpus_id != *f {
                continue;
            }
        }
        let atlas_dir = corpus_path.join("atlas");
        if !atlas_dir.join("atoms.json").is_file() {
            continue;
        }

        let atoms_file = match read_atlas_atoms(&atlas_dir) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("  ✗ {}: {e}", corpus_id);
                continue;
            }
        };
        let atoms_count = atoms_file.atoms.len();

        let (sidecar_status, doc_count) = match doc_to_atoms::read(&atlas_dir) {
            Ok(Some(f)) => ("present", f.len()),
            Ok(None) => ("absent", 0),
            Err(_) => ("error", 0),
        };

        let stability = read_corpus_stability(&corpus_path).unwrap_or_else(|| "—".into());

        let (inv_pct, arg_pct, trc_pct) = articulation_histogram(&atoms_file.atoms);

        println!(
            "{:<32} {:>8} {:>6} {:>10} {:>8.1}% {:>8.1}% {:>8.1}% {:>10}",
            corpus_id,
            format_count(atoms_count as u64),
            format_count(doc_count as u64),
            stability,
            inv_pct,
            arg_pct,
            trc_pct,
            sidecar_status,
        );
    }

    0
}

fn read_corpus_stability(corpus_dir: &std::path::Path) -> Option<String> {
    let s = std::fs::read_to_string(corpus_dir.join("_corpus_meta.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("stream")
        .and_then(|s| s.get("stability"))
        .and_then(|s| s.as_str())
        .map(String::from)
}

fn articulation_histogram(atoms: &[AtomEnvelope]) -> (f32, f32, f32) {
    if atoms.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut inv = 0usize;
    let mut arg = 0usize;
    let mut trc = 0usize;
    for env in atoms {
        let vec: ArticulationVector = corpus_engine::meta_atlas::classify_articulation(env, "");
        match vec.dominant() {
            Articulation::Inventory => inv += 1,
            Articulation::Argument => arg += 1,
            Articulation::Trace => trc += 1,
        }
    }
    let total = (inv + arg + trc) as f32;
    if total <= f32::EPSILON {
        return (0.0, 0.0, 0.0);
    }
    (
        (inv as f32 / total) * 100.0,
        (arg as f32 / total) * 100.0,
        (trc as f32 / total) * 100.0,
    )
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
