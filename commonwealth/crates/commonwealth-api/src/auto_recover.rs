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
    /// Canonical `<corpus>/` exists on disk but does NOT carry a
    /// corpus-engine `_corpus_meta.json`. Another subsystem owns
    /// the directory — for code corpora the daemon stores
    /// `scip_graph.db` at `<canonical>/scip_graph.db` while the
    /// chunk data lives in `<canonical>-partition-local/`.
    /// Attempting `merge_partitions_into_canonical` from here would
    /// trip `promote_single_shard: refusing to overwrite existing`,
    /// so we short-circuit. The cooldown is NOT stamped — discovery
    /// is cheap and re-evaluating each tick costs nothing.
    CanonicalDirectoryReserved,
    /// No `<corpus>-partition-*/` dirs on disk — nothing to merge.
    NotEnoughPartitions,
    /// The previous attempt was within the cooldown window. Caller
    /// should fall back to the original behaviour (e.g. emit the
    /// dispatcher's WARN).
    InCooldown,
    /// Local partitions don't cover every shard the recipe expects.
    /// Producing a canonical from this state would silently advertise
    /// the corpus as complete while missing content from the
    /// uncovered shards. Skip the merge entirely; a peer with fuller
    /// coverage (mirroring more partitions locally) will produce the
    /// canonical, and gossip will pick it up here on the next tick.
    ///
    /// Discovered in the wild: RuggedFox completed a 17-shard ingest
    /// while another peer completed a 31-shard ingest. Each peer
    /// mirrored a stub of the other (meta exists, chunks were never
    /// actually pulled). RuggedFox's auto_recover merged its real
    /// partition with the stub, producing a 17/38-shard canonical
    /// that nevertheless advertised `hosted_corpora={"wikipedia"}`.
    /// Query routing then started returning results that silently
    /// omitted 21 shards of content. The fix: refuse to merge when
    /// coverage is incomplete; the peer with full coverage produces
    /// the canonical instead.
    ///
    /// This variant does NOT stamp the cooldown — re-evaluation on
    /// the next 30s tick is cheap, and bailing fast lets recovery
    /// fire as soon as a peer-pull lands missing shards locally.
    IncompleteCoverage {
        covered: usize,
        total: usize,
        missing: Vec<usize>,
    },
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
    let canonical_dir = index_dir.join(corpus_id);
    let canonical_meta = canonical_dir.join("_corpus_meta.json");
    if canonical_meta.exists() {
        return RecoveryOutcome::AlreadyHasCanonical;
    }
    // The canonical-named directory exists but doesn't carry our
    // meta. Another subsystem (SCIP code-graph for code corpora —
    // `<canonical>/scip_graph.db` is created by daemon_cmd at
    // startup and code_walk during atlas builds) owns the
    // directory. The merge engine will refuse to overwrite, so
    // skip up-front rather than emit ERROR + WARN every tick.
    if canonical_dir.exists() {
        return RecoveryOutcome::CanonicalDirectoryReserved;
    }

    // Discovery: any `<corpus>-partition-*/` with a meta file?
    // Walk every partition meta to compute (a) partition count and
    // (b) the union of `processed_shards` + max `total_shards`
    // across all of them. The union → coverage check below decides
    // whether merging here would produce a complete or partial
    // canonical.
    let prefix = format!("{corpus_id}-partition-");
    let mut partition_count = 0usize;
    let mut shard_union: std::collections::BTreeSet<usize> =
        std::collections::BTreeSet::new();
    let mut total_shards: Option<usize> = None;
    if let Ok(entries) = std::fs::read_dir(index_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !name_str.starts_with(&prefix) {
                continue;
            }
            let meta_path = entry.path().join("_corpus_meta.json");
            if !meta_path.exists() {
                continue;
            }
            partition_count += 1;
            // Read processed_shards + total_shards out of the meta
            // JSON directly — cheaper than opening a CorpusIndex
            // handle just for these fields, and discovery-time
            // failure modes (corrupt JSON) are recoverable.
            let raw = std::fs::read_to_string(&meta_path).unwrap_or_default();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                // Defense in depth against the conversations-personal
                // race (auto_ingest.rs gates on active_ingests, but
                // any future caller of this function — manual CLI
                // merge, recovery from a different loop — gets the
                // same protection here). A partition with
                // `ingestion_in_progress=true` is being actively
                // written; merging it produces a zero-chunk canonical
                // and then the in-flight pipeline trips over the
                // consumed partition meta.
                if v["ingestion_in_progress"].as_bool() == Some(true) {
                    tracing::debug!(
                        corpus = %corpus_id,
                        partition = %entry.path().display(),
                        "auto_recover: partition has ingestion_in_progress=true — skipping merge attempt"
                    );
                    return RecoveryOutcome::NotEnoughPartitions;
                }
                if let Some(arr) = v["processed_shards"].as_array() {
                    for s in arr.iter().filter_map(|x| x.as_u64()) {
                        shard_union.insert(s as usize);
                    }
                }
                if let Some(n) = v["total_shards"].as_u64() {
                    let n = n as usize;
                    total_shards = Some(total_shards.map_or(n, |m| m.max(n)));
                }
            }
        }
    }
    if partition_count == 0 {
        return RecoveryOutcome::NotEnoughPartitions;
    }

    // Coverage gate. If the recipe stamped `total_shards` on any
    // partition AND our local union doesn't cover all shards, refuse
    // to merge. Producing a 17/38 canonical that advertises as
    // complete is worse than no canonical at all — query routing
    // would silently miss the uncovered shards' content.
    //
    // No cooldown stamp on this path — it's a precondition failure
    // that re-resolves quickly when a peer-pull fills missing
    // shards locally; bailing fast and re-checking next tick is
    // exactly what we want.
    if let Some(n) = total_shards {
        if shard_union.len() < n {
            let missing: Vec<usize> =
                (0..n).filter(|s| !shard_union.contains(s)).collect();
            tracing::warn!(
                corpus = %corpus_id,
                covered = shard_union.len(),
                total = n,
                missing = ?missing,
                "auto_recover: refusing to merge — local partitions cover only {} \
                 of {} shards. Waiting for a peer with fuller coverage to \
                 produce canonical (or for missing shards to land locally \
                 via collaborate-pull). Manual override available via \
                 `sovereign corpus merge-partitions {}` (CLI confirms partial \
                 coverage explicitly).",
                shard_union.len(),
                n,
                corpus_id,
            );
            return RecoveryOutcome::IncompleteCoverage {
                covered: shard_union.len(),
                total: n,
                missing,
            };
        }
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
            // Self-heal hook for the alignment recipe: when the
            // merged canonical is a mutable_merge corpus, materialize
            // its rows back to ~/.claude/. project() is a no-op for
            // every other corpus (it re-checks the policy) so this is
            // safe to call unconditionally.
            if let Some(home) = dirs::home_dir() {
                let canonical_path = index_dir.join(corpus_id);
                match corpus_engine::alignment_projector::project(
                    &canonical_path,
                    &home,
                )
                .await
                {
                    Ok(p) => {
                        if p.wrote > 0
                            || p.skipped_local_newer > 0
                            || p.swept_incoming > 0
                        {
                            tracing::info!(
                                corpus = %corpus_id,
                                wrote = p.wrote,
                                skipped_local_newer = p.skipped_local_newer,
                                skipped_unsafe_path = p.skipped_unsafe_path,
                                swept_incoming = p.swept_incoming,
                                "auto_recover: alignment projection complete"
                            );
                        }
                    }
                    Err(e) => tracing::warn!(
                        corpus = %corpus_id,
                        error = %e,
                        "auto_recover: alignment projection failed; merge stands"
                    ),
                }
            }
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

    /// Regression: code corpora put the SCIP call-graph at
    /// `<canonical>/scip_graph.db` while their chunk data lives in
    /// `<canonical>-partition-local/`. auto_recover must NOT try to
    /// merge in that case — `promote_single_shard` would fail every
    /// tick with `refusing to overwrite existing`. Observed in the
    /// daemon log (May 2026) for corpora `commonwealth`,
    /// `commonwealth-ai`, `corpus-engine`, `sovereign`.
    #[tokio::test]
    async fn skips_when_canonical_dir_holds_only_scip_graph() {
        let dir = tempfile::tempdir().unwrap();
        // Canonical dir present, no _corpus_meta.json — only
        // scip_graph.db (placeholder file is enough; we never read
        // its content here).
        let canonical = dir.path().join("commonwealth");
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("scip_graph.db"), b"sqlite-stub").unwrap();
        // A real partition with chunks would normally live next to it.
        let partition = dir.path().join("commonwealth-partition-local");
        std::fs::create_dir_all(&partition).unwrap();
        std::fs::write(
            partition.join("_corpus_meta.json"),
            r#"{"processed_shards":[0]}"#,
        )
        .unwrap();

        let outcome = try_recover_stranded_partitions(dir.path(), "commonwealth").await;
        assert!(
            matches!(outcome, RecoveryOutcome::CanonicalDirectoryReserved),
            "expected CanonicalDirectoryReserved, got {outcome:?}",
        );
        // The canonical was untouched; SCIP file still there.
        assert!(canonical.join("scip_graph.db").exists());
        // No cooldown stamp on this path — re-evaluation each tick
        // is cheap and lets recovery resume the moment the SCIP-
        // owned directory goes away (e.g. after a `sovereign
        // corpus remove` flow).
        let outcome2 = try_recover_stranded_partitions(dir.path(), "commonwealth").await;
        assert!(matches!(outcome2, RecoveryOutcome::CanonicalDirectoryReserved));
    }

    #[tokio::test]
    async fn refuses_merge_when_local_partitions_dont_cover_all_shards() {
        // Reproduces the RuggedFox scenario: two partition dirs
        // exist locally, one is real (claims processed_shards [0,
        // 1, 2]), the other is a stub (no chunks, empty
        // processed_shards). Recipe-stamped total_shards = 5.
        // Local union covers 3 of 5 — auto_recover must refuse to
        // merge.
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("foo-partition-real");
        let p2 = dir.path().join("foo-partition-stub");
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::create_dir_all(&p2).unwrap();
        std::fs::write(
            p1.join("_corpus_meta.json"),
            r#"{"processed_shards":[0,1,2],"total_shards":5}"#,
        )
        .unwrap();
        // Stub partition: no processed_shards stamped, total_shards
        // either matches or is absent. We test with both stamped
        // values matching to confirm the union is what gates.
        std::fs::write(
            p2.join("_corpus_meta.json"),
            r#"{"processed_shards":[],"total_shards":5}"#,
        )
        .unwrap();

        let outcome = try_recover_stranded_partitions(dir.path(), "foo").await;
        match outcome {
            RecoveryOutcome::IncompleteCoverage {
                covered,
                total,
                missing,
            } => {
                assert_eq!(covered, 3);
                assert_eq!(total, 5);
                assert_eq!(missing, vec![3usize, 4]);
            }
            other => panic!("expected IncompleteCoverage; got {:?}", other),
        }

        // Critical: no canonical was produced.
        assert!(!dir.path().join("foo").exists());

        // Cooldown was NOT stamped on this path — a follow-up call
        // should re-evaluate (and bail again, since the on-disk
        // state is unchanged).
        let outcome2 = try_recover_stranded_partitions(dir.path(), "foo").await;
        assert!(
            matches!(outcome2, RecoveryOutcome::IncompleteCoverage { .. }),
            "second call should re-evaluate, not be cooldown-blocked; got {:?}",
            outcome2
        );
    }

    #[tokio::test]
    async fn proceeds_when_total_shards_unstamped() {
        // Older partitions written before the `total_shards` field
        // landed don't have it stamped. The coverage gate must NOT
        // trip on those — we have no signal to know whether
        // coverage is partial or complete, so fall through to the
        // merge attempt (which has its own embedding-model and
        // dimension preflights).
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("foo-partition-aaaa");
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::write(
            p1.join("_corpus_meta.json"),
            r#"{"processed_shards":[0,1]}"#,
        )
        .unwrap();

        // Reach the merge attempt — will fail because the meta is
        // junk for actual merge purposes (no embedding model, no
        // chunks table). What matters is we did NOT short-circuit
        // with IncompleteCoverage.
        let unique_corpus = format!("foo_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_micros());
        // Re-create with the unique corpus prefix to avoid cooldown
        // collision with other tests.
        let p1u = dir.path().join(format!("{unique_corpus}-partition-aaaa"));
        std::fs::create_dir_all(&p1u).unwrap();
        std::fs::write(
            p1u.join("_corpus_meta.json"),
            r#"{"processed_shards":[0,1]}"#,
        )
        .unwrap();

        let outcome = try_recover_stranded_partitions(dir.path(), &unique_corpus).await;
        assert!(
            !matches!(outcome, RecoveryOutcome::IncompleteCoverage { .. }),
            "must NOT short-circuit with IncompleteCoverage when total_shards is absent; got {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn proceeds_when_local_coverage_is_complete() {
        // Recipe says total_shards=3 and our local partitions
        // cover [0,1,2]. The coverage gate must pass; subsequent
        // failure (junk meta for merge purposes) is fine — we just
        // need to confirm IncompleteCoverage didn't fire.
        let dir = tempfile::tempdir().unwrap();
        let unique_corpus = format!("complete_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_micros());
        let p1 = dir.path().join(format!("{unique_corpus}-partition-aaaa"));
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::write(
            p1.join("_corpus_meta.json"),
            r#"{"processed_shards":[0,1,2],"total_shards":3}"#,
        )
        .unwrap();

        let outcome = try_recover_stranded_partitions(dir.path(), &unique_corpus).await;
        assert!(
            !matches!(outcome, RecoveryOutcome::IncompleteCoverage { .. }),
            "complete local coverage must not trigger IncompleteCoverage; got {:?}",
            outcome
        );
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

    /// Regression: a partition with `ingestion_in_progress=true` is
    /// being actively written; auto_recover must refuse to merge it
    /// rather than racing the embed pipeline. Reproduced by the
    /// conversations-personal install on 2026-05-17 — 180 chunks
    /// embedded but never landed because auto_recover ran mid-ingest
    /// and consumed the partition. The auto_ingest scheduler now
    /// gates on AppState.active_ingests, but this defense lives
    /// inside `try_recover_stranded_partitions` itself so any
    /// alternate caller (manual CLI merge, future recovery loops)
    /// is also protected.
    #[tokio::test]
    async fn refuses_merge_when_partition_ingestion_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let unique_corpus = format!("ing_{}", std::process::id());
        let partition = dir
            .path()
            .join(format!("{unique_corpus}-partition-node-abcd"));
        std::fs::create_dir_all(&partition).unwrap();
        std::fs::write(
            partition.join("_corpus_meta.json"),
            r#"{"ingestion_in_progress":true,"processed_shards":[]}"#,
        )
        .unwrap();

        let outcome = try_recover_stranded_partitions(dir.path(), &unique_corpus).await;
        assert!(
            matches!(outcome, RecoveryOutcome::NotEnoughPartitions),
            "in-progress partition should short-circuit as NotEnoughPartitions \
             (cheap, deterministic, re-evaluates on next tick); got {:?}",
            outcome
        );
        // Partition was NOT consumed by the failed-merge path.
        assert!(partition.join("_corpus_meta.json").exists());
    }
}
