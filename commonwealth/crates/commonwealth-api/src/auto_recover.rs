//! Reactive recovery for stranded corpus partitions.
//!
//! Why this exists
//! ---------------
//! The `corpus_collaborate` dispatcher in `commonwealth-api` already
//! has a recovery path that re-fires a stalled queue-mode merge when
//! the queue drained but no canonical was produced — see
//! `routes_internal::spawn_queue_merge`. That recovery requires a
//! handoff blob in `mesh_store` to point at, and the blob carries
//! the merge_leader assignment.
//!
//! The MeshStore is **in-memory** on the daemon (see
//! `sovereign-mesh::daemon::start_daemon`), so every restart wipes
//! it. Handoff blobs only re-appear via gossip from a peer that
//! still holds them. If no peer in the mesh has the blob anymore —
//! observed in the wild on a long-running Wikipedia ingest after
//! both peers restarted — the queue-mode recovery path
//! `find_local_handoff_for_corpus` returns `None` and the
//! dispatcher logs:
//!
//! ```text
//! corpus_collaborate: queue drained but no canonical index and no
//! local handoff found — peer must re-trigger from a node that holds
//! the handoff blob
//! ```
//!
//! …and the corpus deadlocks. The data is on disk: every shard's
//! chunks are in some `<corpus>-partition-*/` directory locally
//! (this node's own work + every peer-pulled mirror). What's
//! missing is the merge step that consolidates them into a
//! canonical `<corpus>/`.
//!
//! `auto_recover` plugs that gap. When the dispatcher's WARN site
//! is about to fire, we instead try the local on-disk merge via
//! `corpus_engine::merge_partitions_into_canonical`. If it
//! succeeds, the canonical exists, `installed_indexes()` will pick
//! it up on the next tick, and `hosted_corpora` gossip will
//! re-advertise. If it fails (no partitions, embedding mismatch,
//! canonical exists), we fall back to the WARN — same outcome as
//! before, just one more option exhausted.
//!
//! Cooldown
//! --------
//! Recovery is expensive on Wikipedia-scale corpora — the
//! `build_indexes` phase is tens of minutes of IVF-PQ + FTS work.
//! A per-corpus 5-minute cooldown prevents a thundering-herd of
//! recovery attempts when, say, the dispatcher fires repeatedly
//! while the merge is still running. The cooldown is module-local
//! state (a `Mutex<HashMap>` behind `OnceLock`); it's lost on
//! daemon restart, which is fine — restart already implies a
//! human-driven event.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 5-minute cooldown between recovery attempts for the same corpus.
/// Prevents a stuck dispatcher (firing every 30s) from launching
/// multiple concurrent merges. The actual merge holds an
/// `active_ingests` slot via `spawn_corpus_install`, so concurrency
/// can't actually happen even without the cooldown — this is
/// belt-and-suspenders against a future code path that calls
/// `try_recover_stranded_partitions` more directly.
const RECOVERY_COOLDOWN: Duration = Duration::from_secs(5 * 60);

fn cooldown_table() -> &'static Mutex<HashMap<String, Instant>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Outcome categories for telemetry + caller flow control.
#[derive(Debug, Clone)]
pub enum RecoveryOutcome {
    /// Canonical `<corpus>/` already exists — no recovery needed.
    AlreadyHasCanonical,
    /// No `<corpus>-partition-*/` dirs on disk — nothing to merge.
    NotEnoughPartitions,
    /// The previous attempt was within the cooldown window. Caller
    /// should fall back to the original behaviour (e.g. emit the
    /// dispatcher's WARN).
    InCooldown,
    /// Recovery merge produced a built canonical with the supplied
    /// chunk count and shard coverage.
    Recovered {
        chunks: u64,
        shards_covered: usize,
    },
    /// Recovery attempted but failed; caller should fall back. The
    /// error is already logged at `error!` level.
    Failed(String),
}

/// Attempt to merge all `<corpus>-partition-*/` directories under
/// `index_dir` into a canonical `<corpus>/`. See module-level
/// docs for the motivating scenario.
///
/// Synchronous interface (`async fn` because the underlying
/// `merge_partitions_into_canonical` is async). The caller is
/// expected to be on the dispatcher's request task — this function
/// itself does not spawn. If you want fire-and-forget behaviour,
/// wrap the call in `tokio::spawn`.
///
/// Stamps the cooldown on every attempt (success OR failure) so a
/// repeated WARN doesn't immediately retry. `AlreadyHasCanonical`
/// and `NotEnoughPartitions` short-circuit before the cooldown
/// stamp — they're cheap, deterministic checks that should always
/// re-evaluate fresh.
pub async fn try_recover_stranded_partitions(
    index_dir: &Path,
    corpus_id: &str,
) -> RecoveryOutcome {
    // Cheap pre-checks first — these don't consume the cooldown.
    let canonical_meta = index_dir.join(corpus_id).join("_corpus_meta.json");
    if canonical_meta.exists() {
        return RecoveryOutcome::AlreadyHasCanonical;
    }

    // Discovery: any `<corpus>-partition-*/` with a meta file?
    let prefix = format!("{corpus_id}-partition-");
    let mut partition_count = 0;
    if let Ok(entries) = std::fs::read_dir(index_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !name_str.starts_with(&prefix) {
                continue;
            }
            if entry.path().join("_corpus_meta.json").exists() {
                partition_count += 1;
            }
        }
    }
    if partition_count == 0 {
        return RecoveryOutcome::NotEnoughPartitions;
    }

    // Cooldown gate. Skip if a recent attempt fired (success or
    // failure — both stamp the cooldown).
    {
        let table = cooldown_table().lock().expect("cooldown mutex poisoned");
        if let Some(last) = table.get(corpus_id) {
            if last.elapsed() < RECOVERY_COOLDOWN {
                tracing::debug!(
                    corpus = %corpus_id,
                    last_attempt_secs = last.elapsed().as_secs(),
                    "auto_recover: in cooldown — skipping"
                );
                return RecoveryOutcome::InCooldown;
            }
        }
    }

    tracing::info!(
        corpus = %corpus_id,
        partition_count,
        "auto_recover: attempting stranded-partition merge into canonical"
    );

    // Tracing-only progress callback for the daemon path. The
    // human-readable variant lives in the CLI; here every phase
    // boundary becomes a structured info! event for journalctl /
    // sovereign log scraping.
    let progress: std::sync::Arc<
        dyn Fn(corpus_engine::MergePhaseProgress) + Send + Sync,
    > = {
        let corpus_id = corpus_id.to_string();
        std::sync::Arc::new(move |phase| {
            let corpus = corpus_id.clone();
            match phase {
                corpus_engine::MergePhaseProgress::DiscoveryComplete {
                    partition_count,
                } => tracing::info!(
                    %corpus,
                    partition_count,
                    "auto_recover: discovery complete"
                ),
                corpus_engine::MergePhaseProgress::MergeComplete {
                    chunks_merged,
                    chunks_deduped,
                } => tracing::info!(
                    %corpus,
                    chunks_merged,
                    chunks_deduped,
                    "auto_recover: chunk-merge phase complete"
                ),
                corpus_engine::MergePhaseProgress::MetaStamped => tracing::info!(
                    %corpus,
                    "auto_recover: canonical meta stamped"
                ),
                corpus_engine::MergePhaseProgress::BuildSubPhase { done, total } => {
                    tracing::info!(
                        %corpus,
                        done,
                        total,
                        "auto_recover: build_indexes sub-phase"
                    )
                }
                corpus_engine::MergePhaseProgress::Complete => tracing::info!(
                    %corpus,
                    "auto_recover: canonical built and marked complete"
                ),
            }
        })
    };

    let outcome = match corpus_engine::merge_partitions_into_canonical(
        index_dir,
        corpus_id,
        Some(progress),
    )
    .await
    {
        Ok(report) => {
            tracing::info!(
                corpus = %corpus_id,
                chunks = report.chunks_merged,
                chunks_input = report.chunks_input,
                chunks_deduped = report.chunks_input.saturating_sub(report.chunks_merged),
                shards_covered = report.shard_union.len(),
                total_shards = ?report.total_shards,
                "auto_recover: stranded-partition merge SUCCEEDED — canonical \
                 will be picked up by installed_indexes() and re-advertised \
                 in hosted_corpora gossip"
            );
            RecoveryOutcome::Recovered {
                chunks: report.chunks_merged,
                shards_covered: report.shard_union.len(),
            }
        }
        Err(e) => {
            tracing::error!(
                corpus = %corpus_id,
                error = %e,
                "auto_recover: stranded-partition merge FAILED — falling back to no-recovery WARN"
            );
            RecoveryOutcome::Failed(e.to_string())
        }
    };

    // Stamp the cooldown so a repeated dispatcher WARN doesn't
    // retry the same merge immediately.
    {
        let mut table = cooldown_table().lock().expect("cooldown mutex poisoned");
        table.insert(corpus_id.to_string(), Instant::now());
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_already_has_canonical_when_canonical_exists() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_meta = dir.path().join("foo").join("_corpus_meta.json");
        std::fs::create_dir_all(canonical_meta.parent().unwrap()).unwrap();
        std::fs::write(&canonical_meta, "{}").unwrap();

        let outcome = try_recover_stranded_partitions(dir.path(), "foo").await;
        assert!(matches!(outcome, RecoveryOutcome::AlreadyHasCanonical));
    }

    #[tokio::test]
    async fn returns_not_enough_partitions_when_no_partition_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = try_recover_stranded_partitions(dir.path(), "absent").await;
        assert!(matches!(outcome, RecoveryOutcome::NotEnoughPartitions));
    }

    #[tokio::test]
    async fn cooldown_blocks_consecutive_attempts_for_same_corpus() {
        // Use a corpus_id that we know will fail (no real partition
        // contents) — the failure path stamps the cooldown, and a
        // second call inside the window should return InCooldown.
        let dir = tempfile::tempdir().unwrap();
        // Materialise a partition meta-file with junk JSON so
        // discovery passes the partition_count check but the actual
        // merge errors. (We need partition_count > 0 to reach the
        // cooldown stamp.)
        let partition_dir = dir.path().join("xyz-partition-aaaa");
        std::fs::create_dir_all(&partition_dir).unwrap();
        std::fs::write(partition_dir.join("_corpus_meta.json"), "{}").unwrap();

        // First call: should fall through to merge attempt and fail
        // (junk meta). Stamps the cooldown.
        let unique_corpus = format!("xyz_{}", std::process::id());
        // Need to re-create the partition dir with the unique name.
        let real_partition = dir
            .path()
            .join(format!("{unique_corpus}-partition-aaaa"));
        std::fs::create_dir_all(&real_partition).unwrap();
        std::fs::write(real_partition.join("_corpus_meta.json"), "{}").unwrap();

        let first = try_recover_stranded_partitions(dir.path(), &unique_corpus).await;
        assert!(
            matches!(first, RecoveryOutcome::Failed(_)),
            "first attempt with junk meta should fail; got {:?}",
            first
        );

        let second = try_recover_stranded_partitions(dir.path(), &unique_corpus).await;
        assert!(
            matches!(second, RecoveryOutcome::InCooldown),
            "second attempt inside cooldown should be blocked; got {:?}",
            second
        );
    }
}
