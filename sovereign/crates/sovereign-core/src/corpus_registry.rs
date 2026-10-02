// SPDX-License-Identifier: AGPL-3.0-or-later
//! Corpus registry reconciliation: ONE SOURCE, the engine.
//!
//! `corpus_state` is what `build_context` reads to derive the principal
//! ceiling and the prompt's installed list; `installed_indexes()` is what the
//! retrieval fan-out actually opens. When those diverge — measured
//! 2026-09-22: 0 rows against 58 on-disk indexes — the ceiling comes out
//! `Some([])` and Filter 5 fails CLOSED, refusing every local fan-out:
//! unscoped turns answered "No matching passages" from general knowledge on a
//! host holding the answer (b3f8a8000). Reconcile at boot, and again before
//! every turn that resolves a ceiling (`Runtime::principal_scope`): every
//! engine index without a row gets one, so the registry cannot lag the thing
//! it describes — a corpus ingested while the daemon runs included. Rows are
//! never DELETED here — engine-absent corpora keep theirs; removal is a corpus
//! operation, not this one's.

use std::collections::HashSet;

use corpus_index::source::CorpusReadPort;
use sovereign_contracts::traits::StateStore;
use sovereign_contracts::types::{CorpusState, CorpusVisibility};

/// Give every engine index that has no `corpus_state` row one. Returns
/// nothing: every outcome — rows added, a listing that failed, a save that
/// failed — is logged at this module's path (under the daemon's
/// `sovereign_core=info` allowlist entry), and none of them stops the boot
/// or the turn.
pub async fn reconcile_corpus_registry(engine: &dyn CorpusReadPort, store: &dyn StateStore) {
    let indexes = match engine.installed_indexes().await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "corpus registry reconcile skipped: engine index listing failed"
            );
            return;
        }
    };
    // A registry that could not be READ is not an empty one: treating it as
    // empty would re-save a row for every index and overwrite their install
    // times (ARCH principle 6). Skip and say so.
    let known: HashSet<String> = match store.list_corpus_states().await {
        Ok(rows) => rows.into_iter().map(|s| s.corpus_id).collect(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "corpus registry reconcile skipped: the registry could not be read"
            );
            return;
        }
    };
    // The decider, not a hand-read clock (clock-gate): one place says what
    // "now" is.
    let now = sovereign_time::unix_now();
    let today = chrono::Utc::now().date_naive().to_string();
    let (mut added, mut failed) = (0usize, 0usize);
    for info in indexes
        .into_iter()
        .filter(|i| !known.contains(&i.corpus_id))
    {
        let state = CorpusState {
            corpus_id: info.corpus_id.clone(),
            installed_at: now,
            source_date: today.clone(),
            chunks_count: info.chunk_count as i64,
            index_size_mb: (info.index_size_bytes / (1024 * 1024)) as i64,
            last_updated: now,
            version: now,
            deleted_at: None,
            vector_index_ready: info.vector_index_built,
            visibility: CorpusVisibility::Org,
        };
        match store.save_corpus_state(&state).await {
            Ok(()) => added += 1,
            Err(e) => {
                failed += 1;
                tracing::warn!(
                    corpus_id = %info.corpus_id,
                    error = %e,
                    "corpus registry: could not add a row for an engine index"
                );
            }
        }
    }
    if added > 0 || failed > 0 {
        tracing::info!(
            added,
            failed,
            "corpus registry reconciled from the engine's installed indexes"
        );
    } else {
        tracing::debug!(
            known = known.len(),
            "corpus registry already covers every engine index"
        );
    }
}
