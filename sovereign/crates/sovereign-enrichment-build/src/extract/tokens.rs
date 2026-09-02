// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-run token-spend snapshot.
//!
//! A small versioned sidecar (`TOKEN_SPEND_SCHEMA`) written beside a run so a
//! later pass can report what extraction cost without re-reading the run file.

use std::fs;

/// Phase D2 — persisted token-spend record at `<workspace>/_tokens.json`.
/// Schema kept stable so the corpus-status display + future
/// `/internal/atlas/status` endpoint can deserialise the same file
/// without coordination.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenSpendRecord {
    pub schema_version: u32,
    pub corpus_id: String,
    pub calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Wall-clock start of the extract run that wrote this record
    /// (Unix ms). Reset every run — this is per-run spend, not
    /// lifetime-of-corpus spend, because Phase 1 caches and
    /// `--resume` make lifetime accounting non-trivial.
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
}

const TOKEN_SPEND_SCHEMA: u32 = 1;

/// Atomically write a token-spend snapshot to `path`. Sibling `.tmp`
/// + rename so a crash mid-write can't leave a half-finished file.
pub fn write_token_snapshot(
    path: &std::path::Path,
    corpus_id: &str,
    started_at_ms: u64,
    ledger: &crate::inference_client::TokenUsageLedger,
) -> std::io::Result<()> {
    let snap = ledger.snapshot();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
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

/// Read the persisted token-spend record from `path`. Returns
/// `None` if the file is missing, malformed, or has a future
/// schema. Used by the corpus-status display + atlas status
/// endpoint.
pub fn read_token_snapshot(path: &std::path::Path) -> Option<TokenSpendRecord> {
    let raw = std::fs::read_to_string(path).ok()?;
    let record: TokenSpendRecord = serde_json::from_str(&raw).ok()?;
    if record.schema_version != TOKEN_SPEND_SCHEMA {
        return None;
    }
    Some(record)
}
