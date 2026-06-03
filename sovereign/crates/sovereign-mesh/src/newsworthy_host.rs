//! Concrete `NewsworthyHost` impl for the embedded Commonwealth daemon.
//!
//! Bridges between corpus-engine's host-agnostic
//! [`corpus_engine::update::newsworthy_watcher::NewsworthyHost`] trait
//! and the live mesh state held by `commonwealth_api::AppState`.
//!
//! - Mesh-state queries (`is_leader`, `is_owner_of`) read the
//!   `Arc<RwLock<Mesh>>` carried on `AppStateInner` and run the
//!   answer through `commonwealth_core::partition::{is_leader,
//!   is_owner}`.
//! - KV operations forward to `MeshStore` directly. SQLite is
//!   blocking-friendly under tokio's full runtime; the existing
//!   `peer_preferences` store does the same and ships in production
//!   today.
//!
//! The watcher itself never sees `MeshStore`, `Mesh`, or `NodeId` —
//! the trait keeps `corpus-engine` free of any Commonwealth dependency
//! per the architectural seam in §6 of `SYSTEM_OVERVIEW.md`.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use commonwealth_api::state::AppState;
use commonwealth_core::contributions::{LedgerEvent, LedgerEventKind};
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::NodeStatus;
use commonwealth_core::partition;
use commonwealth_state::MeshStore;
use corpus_engine::error::{Error as CorpusError, Result as CorpusResult};
use corpus_engine::update::newsworthy_watcher::{CommittedDocs, NewsworthyHost};

pub struct MeshNewsworthyHost {
    app_state: AppState,
    /// The corpus this watcher is responsible for. Used to restrict
    /// leader/owner election to peers that have advertised the corpus
    /// in their most recent `StorageSnapshot` ledger event — without
    /// this gate, the lowest-NodeId peer wins leadership even when
    /// they haven't installed the corpus, leaving the leader role
    /// orphaned and no chunks ever ingested.
    target_corpus_id: String,
}

impl MeshNewsworthyHost {
    pub fn new(app_state: AppState, target_corpus_id: impl Into<String>) -> Self {
        Self {
            app_state,
            target_corpus_id: target_corpus_id.into(),
        }
    }

    fn mesh_store(&self) -> &Arc<MeshStore> {
        &self.app_state.inner.mesh_store
    }

    fn self_node_id(&self) -> NodeId {
        self.app_state.self_node_id()
    }

    /// Snapshot the current online member set. Online = `Online` *or*
    /// `Busy` *or* `Away` — i.e. anything that isn't `Offline`. The
    /// inference scheduler treats Busy/Away the same way for leader
    /// election, so we follow suit to keep behaviour consistent across
    /// daemons.
    async fn online_members(&self) -> Vec<NodeId> {
        let mesh = self.app_state.inner.mesh.read().await;
        mesh.members
            .iter()
            .filter(|(_, m)| m.status != NodeStatus::Offline)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Intersection of `online_members()` and the set of peers whose
    /// most recent gossiped `StorageSnapshot` contains
    /// `self.target_corpus_id`. Self is included whenever the local
    /// engine reports the corpus installed (no need to wait a full
    /// `STORAGE_SNAPSHOT_INTERVAL` round-trip to see our own
    /// advertisement land in the ledger).
    ///
    /// If the local engine has the corpus and the ledger walk yields
    /// nothing else, the return is `[self]` and the watcher runs as
    /// solo leader — the right behaviour when peers haven't yet
    /// installed or haven't gossiped since boot. Fallback semantics
    /// here matter: the alternative (returning all online members on
    /// ledger error) would re-introduce the bug we're fixing — a
    /// peer with the lowest NodeId but no install winning the leader
    /// role and silently doing nothing.
    async fn online_members_holding_target(&self) -> Vec<NodeId> {
        let online = self.online_members().await;
        if online.is_empty() {
            return Vec::new();
        }

        let self_id = self.self_node_id();
        let mut holders: Vec<NodeId> = Vec::new();

        // Self: check local engine directly — fastest source of truth.
        if let Some(engine) = self.app_state.inner.corpus_engine.clone() {
            match engine.installed_indexes().await {
                Ok(list) => {
                    if list
                        .iter()
                        .any(|i| i.corpus_id == self.target_corpus_id && !i.is_shard)
                    {
                        holders.push(self_id);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "newsworthy.host: installed_indexes failed; excluding self from leader pool"
                    );
                }
            }
        }

        // Peers: walk the contribution ledger for the latest
        // StorageSnapshot per node, accept those whose snapshot lists
        // `target_corpus_id`. Snapshots are gossiped hourly, so a
        // freshly-installed peer may not show up for up to an hour —
        // acceptable for a daily watcher tick.
        let events: Vec<LedgerEvent> = match self.app_state.inner.contribution_emitter.events() {
            Ok(ev) => ev,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "newsworthy.host: contribution_emitter.events failed; \
                     leader pool falls back to self-only"
                );
                return holders;
            }
        };

        let mut latest_per_node: HashMap<NodeId, (&LedgerEvent, &Vec<(String, f64)>)> =
            HashMap::new();
        for ev in &events {
            if ev.node_id == self_id {
                continue; // self handled above
            }
            if let LedgerEventKind::StorageSnapshot { corpora } = &ev.kind {
                let entry = latest_per_node.entry(ev.node_id);
                match entry {
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert((ev, corpora));
                    }
                    std::collections::hash_map::Entry::Occupied(mut o) => {
                        if ev.timestamp > o.get().0.timestamp {
                            o.insert((ev, corpora));
                        }
                    }
                }
            }
        }

        for (node_id, (_, corpora)) in latest_per_node {
            if !online.contains(&node_id) {
                continue;
            }
            if corpora.iter().any(|(id, _)| id == &self.target_corpus_id) {
                holders.push(node_id);
            }
        }

        holders
    }
}

#[async_trait::async_trait]
impl NewsworthyHost for MeshNewsworthyHost {
    fn self_node_id_str(&self) -> String {
        self.self_node_id().to_string()
    }

    async fn is_leader(&self) -> bool {
        let pool = self.online_members_holding_target().await;
        partition::is_leader(self.self_node_id(), &pool)
    }

    async fn is_owner_of(&self, partition_key: &str) -> bool {
        let pool = self.online_members_holding_target().await;
        partition::is_owner(self.self_node_id(), partition_key, &pool)
    }

    fn store_get(&self, app_id: &str, key: &str) -> CorpusResult<Option<Vec<u8>>> {
        match self
            .mesh_store()
            .get(app_id, key)
            .map_err(|e| CorpusError::Database(format!("MeshStore.get: {e}")))?
        {
            Some(entry) => Ok(Some(entry.value.to_vec())),
            None => Ok(None),
        }
    }

    fn store_set(&self, app_id: &str, key: &str, value: Vec<u8>) -> CorpusResult<()> {
        self.mesh_store()
            .set(app_id, key, Bytes::from(value), self.self_node_id())
            .map(|_| ())
            .map_err(|e| CorpusError::Database(format!("MeshStore.set: {e}")))
    }

    fn store_scan(&self, app_id: &str, prefix: &str) -> CorpusResult<Vec<(String, Vec<u8>)>> {
        let entries = self
            .mesh_store()
            .scan(app_id, prefix)
            .map_err(|e| CorpusError::Database(format!("MeshStore.scan: {e}")))?;
        Ok(entries
            .into_iter()
            .map(|e| (e.key, e.value.to_vec()))
            .collect())
    }

    fn store_delete(&self, app_id: &str, key: &str) -> CorpusResult<bool> {
        self.mesh_store()
            .delete(app_id, key)
            .map_err(|e| CorpusError::Database(format!("MeshStore.delete: {e}")))
    }

    /// Schedule a structural atlas rebuild for each affected corpus.
    /// Spawned on a detached tokio task so the watcher's tick body
    /// isn't blocked on the rebuild — wikipedia (1.85M chunks) reads
    /// in ~30s-2min and we don't want that on the critical path.
    ///
    /// Cadence: the watcher fires this hook at most once per tick
    /// (default 24h), so no extra throttling is layered here. If the
    /// rebuild takes longer than the tick interval — unlikely at
    /// metadata-only structure_first speeds — back-to-back ticks
    /// would overlap and the second would block on Lance file
    /// contention until the first finished.
    fn on_chunks_committed(&self, affected: &[(String, &'static str)]) {
        let Some(engine) = self.app_state.inner.corpus_engine.clone() else {
            tracing::warn!(
                affected_count = affected.len(),
                "newsworthy.atlas_rebuild_skipped — no corpus_engine on AppState; refreshed chunks landed but atlas stays stale"
            );
            return;
        };
        let indexes_dir = engine.index_dir().to_path_buf();
        // structure_first doesn't read recipes (metadata-only walk);
        // pass the indexes_dir as a stand-in for recipes_dir to
        // satisfy the engine constructor inside `rebuild_structural_atlas`.
        let recipes_dir = indexes_dir.clone();
        let work: Vec<(String, &'static str)> = affected.to_vec();
        tokio::spawn(async move {
            for (corpus_id, role) in &work {
                let started = std::time::Instant::now();
                tracing::info!(
                    corpus_id = %corpus_id,
                    role = %role,
                    "newsworthy.atlas_rebuild_start"
                );
                let outcome = sovereign_tools::atlas_postinstall::rebuild_structural_atlas(
                    corpus_id,
                    indexes_dir.clone(),
                    recipes_dir.clone(),
                )
                .await;
                match outcome {
                    sovereign_tools::atlas_postinstall::StructuralAtlasOutcome::Built {
                        atoms_path,
                        elapsed_secs,
                        ..
                    } => tracing::info!(
                        corpus_id = %corpus_id,
                        role = %role,
                        atoms_path = %atoms_path.display(),
                        elapsed_secs,
                        wall_ms = started.elapsed().as_millis() as u64,
                        "newsworthy.atlas_rebuild_complete"
                    ),
                    sovereign_tools::atlas_postinstall::StructuralAtlasOutcome::AlreadyPresent {
                        atoms_path,
                    } => tracing::warn!(
                        corpus_id = %corpus_id,
                        role = %role,
                        atoms_path = %atoms_path.display(),
                        "newsworthy.atlas_rebuild_skipped — rebuild_structural_atlas returned AlreadyPresent which shouldn't happen with force=true; investigate"
                    ),
                    sovereign_tools::atlas_postinstall::StructuralAtlasOutcome::Failed { reason } => {
                        tracing::warn!(
                            corpus_id = %corpus_id,
                            role = %role,
                            reason,
                            "newsworthy.atlas_rebuild_failed — atlas stays at last known state until next tick retries"
                        );
                    }
                }
            }
        });
    }

    /// Move 6 P5.a.1: per-doc incremental atlas update.
    ///
    /// When `SOVEREIGN_ATLAS_INCREMENTAL=1` and the corpus's atlas
    /// already carries content-hash atom IDs, run an incremental path
    /// instead of a full rebuild:
    ///
    ///   1. Query LanceDB for chunks whose `source_doc_id` is in the
    ///      tick's `doc_ids`.
    ///   2. Aggregate into `AggregatedArticle` records via the same
    ///      helper `ingest()` uses.
    ///   3. `extract_atoms_for_articles` produces an `AtomsDelta`
    ///      with `upserted_docs` keyed by article title.
    ///   4. `apply_atom_delta` rewrites `atoms.json` +
    ///      `doc_to_atoms.json` + `edges.json` atomically.
    ///   5. `rebuild_for_corpus` refreshes meta-atlas anchors for
    ///      this corpus only (O(target_atoms) vs O(all atoms)).
    ///
    /// Failure modes (env unset, atoms.json missing, sequential-id
    /// atlas pre-migration, any read/write error) fall through to
    /// the legacy full-rebuild path. Spawned on tokio so the
    /// watcher's tick body returns promptly — the work load is
    /// LanceDB scan + per-doc atom extraction, dominated by the
    /// LanceDB scan on the parent corpus.
    fn on_chunks_committed_with_docs(&self, committed: &[CommittedDocs]) {
        let incremental_enabled = std::env::var("SOVEREIGN_ATLAS_INCREMENTAL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        for c in committed {
            tracing::info!(
                corpus_id = %c.corpus_id,
                role = %c.role,
                doc_count = c.doc_ids.len(),
                incremental_enabled,
                "newsworthy.atlas_delta_received"
            );
        }

        // Portal-role corpora MUST go through `apply_incremental` even
        // when the global incremental flag is off — the legacy
        // full-rebuild path runs `structure_first`, which collapses a
        // portal page into one Entity-of-type-article and produces a
        // useless atlas surface. Per-bullet extraction only lives on the
        // incremental path today (see `apply_incremental`'s `role =
        // "portal"` branch). Refresh-role work (the parent wikipedia
        // corpus) keeps the old fallback because structure_first is
        // correct for it and a full rebuild over millions of articles
        // is intentionally gated behind the env flag.
        let (incremental_work, legacy_work): (Vec<_>, Vec<_>) = committed
            .iter()
            .cloned()
            .partition(|c| incremental_enabled || c.role == "portal");

        if !legacy_work.is_empty() {
            let legacy: Vec<(String, &'static str)> = legacy_work
                .iter()
                .map(|c| (c.corpus_id.clone(), c.role))
                .collect();
            self.on_chunks_committed(&legacy);
        }

        if incremental_work.is_empty() {
            return;
        }

        let Some(engine) = self.app_state.inner.corpus_engine.clone() else {
            tracing::warn!(
                committed_count = committed.len(),
                "newsworthy.atlas_delta_skipped — no corpus_engine on AppState"
            );
            return;
        };
        let indexes_dir = engine.index_dir().to_path_buf();
        let work = incremental_work;
        tokio::spawn(async move {
            for c in &work {
                let outcome = apply_incremental(
                    engine.clone(),
                    indexes_dir.clone(),
                    c.corpus_id.clone(),
                    c.role,
                    c.doc_ids.clone(),
                )
                .await;
                match outcome {
                    Ok(()) => {}
                    Err(reason) if c.role == "portal" => {
                        // Portal-role fallback: wipe + rebuild via the
                        // per-bullet strategy. The legacy
                        // `rebuild_structural_atlas` path runs
                        // structure_first, which would re-write the
                        // single-Entity-of-type-article garbage. The
                        // wipe path here handles the common bailout
                        // (atoms.json carries pre-migration
                        // sequential-id atoms) by clearing the atlas
                        // dir so the next apply_incremental sees a
                        // clean slate it can populate with
                        // content-hash atoms via newsworthy_events.
                        tracing::warn!(
                            corpus_id = %c.corpus_id,
                            role = %c.role,
                            doc_count = c.doc_ids.len(),
                            reason,
                            "newsworthy.atlas_portal_wipe_and_rebuild — incremental bailed for portal corpus; wiping atlas + retrying with newsworthy_events"
                        );
                        if let Err(e) = wipe_atlas_dir(&indexes_dir, &c.corpus_id) {
                            tracing::warn!(
                                corpus_id = %c.corpus_id,
                                error = %e,
                                "newsworthy.atlas_portal_wipe_failed — atlas stays at last known state"
                            );
                            continue;
                        }
                        // Retry on the clean slate. Second failure is
                        // logged; no further fallback for portal
                        // because the legacy path is structurally
                        // wrong for this corpus shape.
                        let retry = apply_incremental(
                            engine.clone(),
                            indexes_dir.clone(),
                            c.corpus_id.clone(),
                            c.role,
                            c.doc_ids.clone(),
                        )
                        .await;
                        if let Err(e) = retry {
                            tracing::warn!(
                                corpus_id = %c.corpus_id,
                                role = %c.role,
                                error = %e,
                                "newsworthy.atlas_portal_retry_failed — atlas left empty until next tick"
                            );
                        }
                    }
                    Err(reason) => {
                        // Refresh-role (parent wikipedia) fallback —
                        // structure_first IS correct here, so the
                        // legacy full-rebuild remains the right path.
                        tracing::warn!(
                            corpus_id = %c.corpus_id,
                            role = %c.role,
                            doc_count = c.doc_ids.len(),
                            reason,
                            "newsworthy.atlas_incremental_fallback — falling back to full rebuild"
                        );
                        let recipes_dir = indexes_dir.clone();
                        let started = std::time::Instant::now();
                        let res = sovereign_tools::atlas_postinstall::rebuild_structural_atlas(
                            &c.corpus_id,
                            indexes_dir.clone(),
                            recipes_dir,
                        )
                        .await;
                        match res {
                            sovereign_tools::atlas_postinstall::StructuralAtlasOutcome::Built {
                                atoms_path,
                                elapsed_secs,
                                ..
                            } => tracing::info!(
                                corpus_id = %c.corpus_id,
                                role = %c.role,
                                atoms_path = %atoms_path.display(),
                                elapsed_secs,
                                wall_ms = started.elapsed().as_millis() as u64,
                                "newsworthy.atlas_rebuild_complete"
                            ),
                            sovereign_tools::atlas_postinstall::StructuralAtlasOutcome::AlreadyPresent {
                                ..
                            } => {}
                            sovereign_tools::atlas_postinstall::StructuralAtlasOutcome::Failed {
                                reason,
                            } => tracing::warn!(
                                corpus_id = %c.corpus_id,
                                role = %c.role,
                                reason,
                                "newsworthy.atlas_rebuild_failed"
                            ),
                        }
                    }
                }
            }
        });
    }
}

/// Move 6 P5.a.1 incremental computation. Returns `Err(reason)` if
/// the caller should fall back to a full rebuild; `Ok(())` on
/// success (or on no-op when the delta carried no doc_ids).
async fn apply_incremental(
    engine: std::sync::Arc<corpus_engine::engine::CorpusEngine>,
    indexes_dir: std::path::PathBuf,
    corpus_id: String,
    role: &'static str,
    doc_ids: Vec<String>,
) -> Result<(), String> {
    use corpus_engine::enrichment::atlas::atoms_delta::apply_atom_delta;
    use corpus_engine::enrichment::atlas::strategies::newsworthy_events::extract_atoms_for_portal_chunks;
    use corpus_engine::enrichment::atlas::strategies::structure_first::{
        aggregate_articles_from_chunks, extract_atoms_for_articles, StructureFirstConfig,
    };
    use corpus_engine::enrichment::atlas::writer::{read_atlas_atoms, ATLAS_DIRNAME};
    use corpus_engine::meta_atlas::rebuild_for_corpus;

    if doc_ids.is_empty() {
        return Ok(());
    }

    let started = std::time::Instant::now();
    let atlas_dir = indexes_dir.join(&corpus_id).join(ATLAS_DIRNAME);

    // Pre-flight: only run the incremental path against an atlas
    // that's already migrated to content-hash ids. Sequential-id
    // atlases mix with content-hash atoms badly (apply_atom_delta
    // would leave the legacy atoms orphaned).
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => return Err(format!("read atoms.json at {}: {e}", atlas_dir.display())),
    };
    if !atoms_file.atoms.is_empty()
        && !atoms_file
            .atoms
            .iter()
            .all(|env| env.id().is_content_hash())
    {
        return Err(
            "atoms.json contains sequential-id atoms; run `sovereign atlas migrate-ids` first"
                .to_string(),
        );
    }
    let atoms_before = atoms_file.atoms.len();
    drop(atoms_file);

    // Query LanceDB for the tick's chunks.
    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("open_index_for_corpus({corpus_id}): {e}"))?;
    let chunks = index
        .chunks_by_source_doc_ids(&doc_ids)
        .await
        .map_err(|e| format!("chunks_by_source_doc_ids({} ids): {e}", doc_ids.len()))?;
    let chunk_count = chunks.len();

    // Strategy dispatch keyed off the watcher-supplied role.
    //
    // `portal` → wikipedia-newsworthy daily Portal:Current_events pages.
    //   Each chunk IS a single event bullet — extract per-bullet Event
    //   atoms + wikilink Entity placeholders via `newsworthy_events`.
    //
    // `refresh` → the parent `wikipedia` corpus's tracked-window
    //   articles. Each chunk is a section of a real article — keep the
    //   structure_first one-Entity-per-article shape.
    //
    // Any future role falls back to structure_first; new roles should
    // add their dispatch branch here together with the extractor that
    // matches the corpus's chunk shape.
    let (delta_atoms, delta_edges, articles_count) = if role == "portal" {
        let delta = extract_atoms_for_portal_chunks(&chunks, &corpus_id);
        let event_count = delta
            .atoms_delta
            .upserted_docs
            .iter()
            .filter(|(d, _)| d != "_placeholders")
            .map(|(_, atoms)| atoms.len())
            .sum::<usize>();
        (delta.atoms_delta, delta.edges, event_count)
    } else {
        let agg = aggregate_articles_from_chunks(&chunks);
        let cfg = StructureFirstConfig {
            source_corpus_id: corpus_id.clone(),
            ..Default::default()
        };
        let delta = extract_atoms_for_articles(&agg.articles, &corpus_id, &cfg);
        (delta.atoms_delta, delta.edges, agg.articles.len())
    };
    // edges already live inside delta_atoms.added_edges; drop the
    // separate handle to silence dead-code warnings on the `portal`
    // branch where we don't apply edges twice.
    let _ = delta_edges;

    // Apply.
    let summary = apply_atom_delta(&atlas_dir, delta_atoms)
        .map_err(|e| format!("apply_atom_delta({}): {e}", atlas_dir.display()))?;

    // Meta-atlas: refresh anchors for this corpus only.
    let meta_outcome = match rebuild_for_corpus(&indexes_dir, &corpus_id, None) {
        Ok(_) => "ok",
        Err(e) => {
            tracing::warn!(
                corpus_id = %corpus_id,
                role = %role,
                error = %e,
                "newsworthy.atlas_meta_partial_rebuild_failed — meta-atlas anchors may lag until next full build"
            );
            "failed"
        }
    };

    tracing::info!(
        corpus_id = %corpus_id,
        role = %role,
        doc_count = doc_ids.len(),
        chunk_count,
        articles_aggregated = articles_count,
        atoms_before = summary.atoms_before,
        atoms_after = summary.atoms_after,
        atoms_added = summary.atoms_added,
        atoms_removed = summary.atoms_removed,
        docs_upserted = summary.docs_upserted,
        meta_atlas = meta_outcome,
        wall_ms = started.elapsed().as_millis() as u64,
        atoms_before_query = atoms_before,
        "newsworthy.atlas_incremental_complete"
    );
    Ok(())
}

/// Delete every file inside the corpus's atlas dir, leaving the
/// directory itself in place. Used by the portal-role fallback when
/// `apply_incremental` bails on a sequential-id atoms.json or any
/// other unrecoverable pre-existing-shape error: a clean slate lets
/// the next `apply_incremental` pass populate the atlas via the
/// per-bullet `newsworthy_events` strategy with content-hash ids,
/// which is the shape every downstream reader expects.
///
/// `Ok(())` is returned when the dir didn't exist (nothing to wipe)
/// or when every file was removed. Any walk/IO error short-circuits
/// — the caller logs and skips the retry rather than rebuilding on
/// a half-wiped directory.
fn wipe_atlas_dir(indexes_dir: &std::path::Path, corpus_id: &str) -> Result<(), String> {
    use corpus_engine::enrichment::atlas::writer::ATLAS_DIRNAME;
    let atlas_dir = indexes_dir.join(corpus_id).join(ATLAS_DIRNAME);
    if !atlas_dir.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(&atlas_dir)
        .map_err(|e| format!("read_dir {}: {e}", atlas_dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir entry {}: {e}", atlas_dir.display()))?;
        let path = entry.path();
        if path.is_file() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("remove_file {}: {e}", path.display()))?;
        }
    }
    Ok(())
}
