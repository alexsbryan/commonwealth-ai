// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas step's pool admission and its ledger. Beside `atlas_grounding.rs`
//! under `#[path]` so the step file stays under the ARCH §3.1 band.

use crate::runtime::text_utils::truncate_chars;

/// Pool the resolver's chunks and state the step's ledger in CHUNKS, the one
/// unit the pipeline's identity (`added + dropped == considered`) is written
/// in. The resolver counts `considered` in requests, and a Section request
/// resolved by search yields several chunks, so reporting it unconverted
/// failed the identity on SEP (`delta=9 considered=8`). Considered here is
/// every chunk the resolver handed back or dropped as a duplicate, plus each
/// request that yielded nothing, counted once under its reason.
pub(super) fn pool_resolved(
    chunks: &mut Vec<corpus_index::types::ScoredChunk>,
    fetched: Vec<corpus_engine_atlas_reader::resolve::ResolvedChunk>,
    resolve: &corpus_engine_atlas_reader::resolve::ResolveLedger,
) -> crate::runtime::retrieval_ledger::StepLedger {
    use crate::runtime::retrieval_ledger::{DropReason, StepLedger};
    let fetched_n = fetched.len();
    let mut pool_duplicate = 0usize;
    let mut seen_in_pool: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in fetched {
        let key = format!(
            "{}|{}",
            r.chunk.title.clone().unwrap_or_default(),
            truncate_chars(&r.chunk.content, 80)
        );
        if seen_in_pool.insert(key) {
            chunks.push(r.chunk);
        } else {
            pool_duplicate += 1;
        }
    }
    let considered = fetched_n
        + resolve.duplicate
        + resolve.out_of_scope
        + resolve.unresolvable
        + resolve.title_mismatch
        + resolve.unattempted;
    StepLedger::injected(considered)
        .drop(DropReason::OutOfScope, resolve.out_of_scope)
        .drop(DropReason::EvidenceUnresolvable, resolve.unresolvable)
        .drop(DropReason::TitleMismatch, resolve.title_mismatch)
        .drop(DropReason::Duplicate, resolve.duplicate + pool_duplicate)
        // Requests past the fetch budget were never attempted. They are a
        // DECISION, not a failure, and the accounting identity requires them
        // named.
        .drop(DropReason::BudgetExhausted, resolve.unattempted)
}
