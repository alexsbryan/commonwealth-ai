// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pre-warm the RPC worker's tensor cache from a local GGUF — fully offline
//! (no network, no GPU). Hand a node the GGUF on a thumbdrive, run this, and the
//! worker serves with zero weight transfer when the cluster comes online.
//!
//! Usage: warm_cache --model <path.gguf> [--cache-dir <dir>]

use std::path::PathBuf;

use sovereign_inference::embedded::{default_cache_dir, warm_cache_from_gguf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut model: Option<PathBuf> = None;
    let mut cache_dir: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                i += 1;
                model = args.get(i).map(PathBuf::from);
            }
            "--cache-dir" => {
                i += 1;
                cache_dir = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let Some(model) = model else {
        eprintln!("Usage: warm_cache --model <path.gguf> [--cache-dir <dir>]");
        std::process::exit(2);
    };
    let cache_dir = cache_dir
        .or_else(default_cache_dir)
        .expect("no cache dir (set --cache-dir or HOME)");

    eprintln!(
        "warming RPC cache for {} → {}",
        model.display(),
        cache_dir.display()
    );
    let t0 = std::time::Instant::now();
    match warm_cache_from_gguf(&model, &cache_dir) {
        Ok(s) => {
            println!(
                "✓ {} tensors total, {} cacheable (>10MB): {} written ({:.2} GB), {} already present, in {:.1}s\n  cache dir: {}",
                s.tensors_total,
                s.tensors_cacheable,
                s.written,
                s.bytes_written as f64 / 1e9,
                s.already_present,
                t0.elapsed().as_secs_f64(),
                s.cache_dir.display(),
            );
        }
        Err(e) => {
            eprintln!("warm_cache failed: {e}");
            std::process::exit(1);
        }
    }
}
