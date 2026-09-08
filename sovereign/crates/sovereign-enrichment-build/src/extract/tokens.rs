// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-run token-spend snapshot — the WRITE half.
//!
//! The record, its schema constant and its reader live in
//! `corpus_engine::enrichment::tokens`, beside the enrichment workspace
//! layout they describe, so the corpus-status row and any status endpoint
//! can read the sidecar without reaching up into the orchestrator
//! (sv-surface rung 1). The writer stays HERE because it folds this
//! crate's `TokenUsageLedger` into the record — one schema, one reader
//! below, one writer above.

pub use corpus_engine::enrichment::tokens::{
    read_token_snapshot, TokenSpendRecord, TOKEN_SPEND_SCHEMA,
};

use crate::inference_client::TokenUsageLedger;

/// Atomically write a token-spend snapshot to `path`. Sibling `.tmp`
/// + rename so a crash mid-write can't leave a half-finished file.
pub fn write_token_snapshot(
    path: &std::path::Path,
    corpus_id: &str,
    started_at_ms: u64,
    ledger: &TokenUsageLedger,
) -> std::io::Result<()> {
    let snap = ledger.snapshot();
    let now_ms = sovereign_time::unix_millis();
    let record = TokenSpendRecord {
        schema_version: TOKEN_SPEND_SCHEMA,
        corpus_id: corpus_id.to_string(),
        calls: snap.calls,
        prompt_tokens: snap.prompt_tokens,
        completion_tokens: snap.completion_tokens,
        total_tokens: snap.total_tokens,
        started_at_ms,
        updated_at_ms: now_ms,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
