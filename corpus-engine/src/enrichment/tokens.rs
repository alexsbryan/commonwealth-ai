// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-run token-spend snapshot — the READ half.
//!
//! A small versioned sidecar (`TOKEN_SPEND_SCHEMA`) written beside a tier-2
//! extract run so a later pass can report what extraction cost without
//! re-reading the run file. The record and its reader live HERE, beside the
//! enrichment workspace layout they describe, because two surfaces that
//! owe nothing to each other read the same file: the corpus-status row
//! (this crate's `engine::status`) and the tier-2 extract pass's own
//! reporting. The WRITER stays in `sovereign-enrichment-build`, beside the
//! `TokenUsageLedger` it folds — one schema, one reader, one writer, three
//! homes that each own exactly their half (sv-surface rung 1; before this
//! the read lived above the layout it parses and only the CLI's private
//! status walk could reach it).

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

/// The one schema version both halves agree on. The writer mints it; the
/// reader refuses a record that carries any other.
pub const TOKEN_SPEND_SCHEMA: u32 = 1;

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
