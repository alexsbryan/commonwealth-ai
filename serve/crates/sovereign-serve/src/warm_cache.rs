// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-serve warm-cache`, spelled `svrn mesh warm-cache` through the
//! dispatcher. Moved from sovereign-cli-mesh (phase-b-22): it touches
//! serve's weights and nothing of cmnwlth's.

/// `svrn mesh warm-cache <gguf> [--cache-dir <dir>]`
///
/// Pre-seed the RPC worker's tensor cache from a local GGUF — fully offline (no
/// network, no GPU). When the cluster later serves this model, the host's
/// tensor-hash requests are all cache hits and zero weight bytes cross the wire.
/// The companion to a thumbdrive'd GGUF: distribute the model offline, run this
/// on each worker, and a metered/throttled link never carries the weights.
pub(crate) async fn cmd_warm_cache(args: &[String]) -> i32 {
    let mut model: Option<std::path::PathBuf> = None;
    let mut cache_dir: Option<std::path::PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cache-dir" => {
                i += 1;
                cache_dir = args.get(i).map(std::path::PathBuf::from);
            }
            "--help" | "-h" => {
                eprintln!("Usage: svrn mesh warm-cache <model.gguf> [--cache-dir <dir>]");
                eprintln!();
                eprintln!("  Pre-seeds the RPC tensor cache from a local GGUF so a mesh worker");
                eprintln!("  serves this model with ZERO weight transfer over the network.");
                eprintln!("  Fully offline — no network, no GPU. Default cache dir:");
                eprintln!("  ~/.svrnmesh/rpc-cache (matches the in-process worker).");
                return 0;
            }
            s if model.is_none() && !s.starts_with('-') => {
                model = Some(std::path::PathBuf::from(s));
            }
            other => {
                eprintln!("Unknown arg: {other}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(model) = model else {
        eprintln!("Usage: svrn mesh warm-cache <model.gguf> [--cache-dir <dir>]");
        return 2;
    };
    let cache_dir = match cache_dir.or_else(sovereign_inference::embedded::default_cache_dir) {
        Some(d) => d,
        None => {
            eprintln!(
                "no cache dir: SOVEREIGN_RPC_CACHE_DIR is off/0/empty (caching \
                 disabled), or HOME is unset. Pass --cache-dir to warm one anyway."
            );
            return 1;
        }
    };
    eprintln!(
        "warming RPC cache for {} → {}",
        model.display(),
        cache_dir.display()
    );
    let t0 = std::time::Instant::now();
    match sovereign_inference::embedded::warm_cache_from_gguf(&model, &cache_dir) {
        Ok(s) => {
            println!(
                "✓ {}/{} tensors cacheable (>10MB): {} written ({:.2} GB), {} already present — {:.1}s",
                s.tensors_cacheable,
                s.tensors_total,
                s.written,
                s.bytes_written as f64 / 1e9,
                s.already_present,
                t0.elapsed().as_secs_f64(),
            );
            println!("  cache dir: {}", s.cache_dir.display());
            0
        }
        Err(e) => {
            eprintln!("warm-cache failed: {e}");
            1
        }
    }
}
