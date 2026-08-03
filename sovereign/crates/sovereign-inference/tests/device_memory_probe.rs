// SPDX-License-Identifier: AGPL-3.0-or-later
//! Measurement probe for `llama_cpp_4::fit::get_device_memory_data` — the
//! three-term (model / context / compute) per-device projection the fit gates
//! want instead of their hand-rolled proxies (`weights/8` KV, `× headroom`).
//!
//! This probe GATES the RPC_BLOCK_SPLIT fix (note 16fc9204): the projection
//! loads the model with `no_alloc` and frees it before returning, so it
//! SHOULD be cheap — but it had never been measured on a ~155 GB sharded
//! GGUF, and if it is slow it cannot sit in the warm/load path. Ignored by
//! default (needs real weights). Run by hand:
//!
//! ```sh
//! SOVEREIGN_PROBE_GGUF=sovereign/models/DeepSeek-V4-Flash-0731-GGUF/UD-Q4_K_XL/DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00001-of-00005.gguf \
//!   cargo test -p sovereign-inference --test device_memory_probe -- --ignored --nocapture
//! ```

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Instant;

#[test]
#[ignore = "needs real GGUF weights — see module doc"]
fn measure_get_device_memory_data_cost() {
    let Some(path) = std::env::var_os("SOVEREIGN_PROBE_GGUF").map(PathBuf::from) else {
        panic!("set SOVEREIGN_PROBE_GGUF to the first shard of the GGUF to probe");
    };
    assert!(path.exists(), "no such file: {}", path.display());

    let _backend = llama_cpp_4::llama_backend::LlamaBackend::init().expect("backend init");

    // Mirror the distributed child's real load shape: full offload, the
    // primary's context, the operator-tuned ubatch.
    let mparams = llama_cpp_4::model::params::LlamaModelParams::default().with_n_gpu_layers(999);
    let cparams = llama_cpp_4::context::params::LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(32_768))
        .with_n_ubatch(2_048);

    // Three trials: the first pays any cold page-cache cost of the GGUF
    // headers, the rest show the warm cost the load path would actually pay.
    for trial in 1..=3 {
        let t0 = Instant::now();
        let report = llama_cpp_4::fit::get_device_memory_data(
            &path,
            &mparams,
            &cparams,
            llama_cpp_sys_4::GGML_LOG_LEVEL_ERROR,
        )
        .expect("device memory query");
        let elapsed = t0.elapsed();
        const MIB: f64 = 1024.0 * 1024.0;
        println!(
            "trial {trial}: {:?} — n_gpu_layers={} n_ctx_train={} n_expert={}",
            elapsed,
            report.hyperparams.n_gpu_layers,
            report.hyperparams.n_ctx_train,
            report.hyperparams.n_expert,
        );
        for (i, e) in report.entries.iter().enumerate() {
            println!(
                "  dev{i}: total={:.0} MiB free={:.0} MiB | model={:.0} MiB context={:.0} MiB compute={:.0} MiB",
                e.total as f64 / MIB,
                e.free as f64 / MIB,
                e.model as f64 / MIB,
                e.context as f64 / MIB,
                e.compute as f64 / MIB,
            );
        }
    }
}
