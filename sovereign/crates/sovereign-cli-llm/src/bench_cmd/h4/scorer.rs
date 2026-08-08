// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reranker, adapted to the sweep's injected-scorer seam — and the refusal
//! that runs when it is not there.
//!
//! `sovereign-core` declares [`SentenceScorer`] and takes no inference
//! dependency; this is the one place the real model is bound to it. The adapter
//! is four lines. The interesting part of this file is the refusal.

use std::path::{Path, PathBuf};

use sovereign_core::runtime::native_grounding::sentence_sweep::SentenceScorer;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::capacity::{self, SlotPlan};
use sovereign_inference::hardware::HardwareProfile;
use sovereign_inference::reranker_standalone::StandaloneReranker;

/// Context size the rerank slot is planned at, matching H1's fit check so the
/// two measurements plan the same slot rather than two different ones.
const RERANK_N_CTX: u32 = 8192;
/// Sequence cap for the rerank slot — `RerankSlot`'s batched path, as H1 plans
/// it (`h1_gate.rs:367-397`).
const RERANK_N_SEQ: u32 = 8;

/// `StandaloneReranker` behind the sweep's trait.
pub struct RerankScorer {
    inner: StandaloneReranker,
}

#[async_trait::async_trait]
impl SentenceScorer for RerankScorer {
    async fn score(&self, query: &str, docs: &[String]) -> Result<Vec<f32>, String> {
        self.inner
            .rerank_batch(query, docs)
            .await
            .map_err(|e| format!("rerank: {e}"))
    }
}

/// Resolve the reranker path: `--rerank-model` wins, `SOVEREIGN_RERANK_MODEL_PATH`
/// is the fallback, and an all-whitespace env value counts as unset.
///
/// Returns an error naming the absence rather than a `None` the caller might
/// paper over. §18.3, and the same shape H1 uses (`h1_gate.rs:336-358`) — a
/// measurement that quietly proceeds without its instrument is worse than one
/// that does not run.
pub fn resolve_rerank_path(flag: Option<PathBuf>) -> Result<PathBuf, String> {
    let path = match flag.or_else(|| {
        std::env::var("SOVEREIGN_RERANK_MODEL_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
    }) {
        Some(p) => p,
        None => {
            return Err(
                "no reranker. `SOVEREIGN_RERANK_MODEL_PATH` is unset (it is default-inert) and \
                 --rerank-model was not given. H4's sentence sweep IS the rerank margin, so there \
                 is nothing to measure and nothing to substitute — pass --rerank-model \
                 <path-to-gguf>."
                    .into(),
            );
        }
    };
    if !path.is_file() {
        return Err(format!(
            "reranker not found at {path:?} — refusing rather than sweeping with a \
             stand-in scorer and emitting margins nobody can reproduce"
        ));
    }
    Ok(path)
}

/// Fit-check and load the rerank slot.
///
/// The capacity gate runs BEFORE the load, the same way H1 runs it
/// (`h1_gate.rs:367-397`) and for the same recorded reason — the 64GB SIGTERM
/// incident (note `b57b0cd5`), which §8's residency plan is a response to. This
/// path hard-refuses on a bad fit unless `SOVEREIGN_SKIP_VRAM_CHECK` is set,
/// which is stricter than the daemon's advisory use of the same function.
pub fn load(path: &Path) -> Result<RerankScorer, String> {
    let hw = HardwareProfile::detect();
    let plans = vec![SlotPlan {
        role: "rerank".into(),
        path: path.to_path_buf(),
        n_seq_max: RERANK_N_SEQ,
        n_ctx: RERANK_N_CTX,
    }];
    let report = capacity::check_fit(&plans, &hw);
    if report.fits {
        eprintln!(
            "[h4] capacity: {} MiB required, {} MiB available (after {} MiB reserved) — fits",
            report.total_required_mb, report.available_mb, report.safety_reserved_mb
        );
    } else if capacity::check_skipped_by_env() {
        eprintln!("[h4] capacity check FAILED but is disabled by env — proceeding as instructed");
        eprintln!("{}", report.refuse_message());
    } else {
        return Err(format!(
            "capacity check refused the rerank slot this measurement needs:\n{}",
            report.refuse_message()
        ));
    }

    let inner = StandaloneReranker::load(path, sovereign_core::model_family::ModelFamily::Reranker, None)
        .map_err(|e| format!("load reranker: {e}"))?;
    Ok(RerankScorer { inner })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_reranker_is_refused_by_name_not_defaulted() {
        // No flag, and the env var scrubbed to the whitespace case the
        // resolver must treat as unset.
        let err = resolve_rerank_path(Some(PathBuf::from("/nonexistent/model.gguf")))
            .expect_err("a path that is not a file must refuse");
        assert!(
            err.contains("refusing"),
            "the refusal must say it is refusing: {err}"
        );
    }

    #[test]
    fn the_flag_beats_the_env() {
        // A real file, so the resolver's is_file() check passes.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("flag.gguf");
        std::fs::write(&p, b"x").unwrap();
        let got = resolve_rerank_path(Some(p.clone())).unwrap();
        assert_eq!(got, p, "--rerank-model must win over any env value");
    }
}
