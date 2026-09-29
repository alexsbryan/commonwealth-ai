// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code's notes rail: what wires `notes.db` beyond the file (phase-b
//! pb-notes-memory; phase-b-30 Group 4, phase-b-33). The gossip sink, the
//! ingest poller, the tier backfill and TTL sweep, and the T1/T2 hooks,
//! origin id and roster moved here from svrn's daemon boot with the store
//! they serve, so they run wherever code runs: alone, and composed into the
//! stock binary. The rail dials cw-rails' KV the way the work atlas does
//! ([`super::atlas::atlas_kv`]); the host supplies only what it alone has.

use std::sync::Arc;

use corpus_engine_notes::{NotePropagationEvent, NoteStore};
use corpus_index::types::{EmbedFn, GlinerFn, NodeRoster};
use kernel_types::NodeId;
use sovereign_contracts::peer::{Convergence, ReplicatedKv, SoloConvergence};

/// What the host hands code's notes rail. Every field defaults to code
/// alone: no embed slot (T1 dark), no GLiNER (T2 on author symbols and files
/// only), this root's node id, no roster (authors render as raw ids), and a
/// convergence recorder nobody else reads.
#[derive(Default)]
pub struct NotesRail {
    /// T1: the host's embed slot, for semantic recall over notes.
    pub embed: Option<EmbedFn>,
    /// T2: the host's GLiNER session, for entity rows on note writes.
    pub gliner: Option<GlinerFn>,
    /// This node's id; `None` is [`super::atlas::atlas_node_id`].
    pub node_id: Option<NodeId>,
    /// The mesh roster that names note authors.
    pub roster: Option<NodeRoster>,
    /// The recorder the host's `/status` reads.
    pub convergence: Option<Arc<dyn Convergence>>,
}

/// Wire `notes` with the host's hooks and start the rail over cw-rails' KV.
pub fn wire(notes: &Arc<NoteStore>, rail: NotesRail) {
    let NotesRail {
        embed,
        gliner,
        node_id,
        roster,
        convergence,
    } = rail;
    match embed {
        Some(embed) => match notes.set_embed_fn(embed) {
            Err(e) => tracing::warn!(target = "notes", error = e, "notes: embed_fn already set"),
            Ok(()) => tracing::info!(
                target = "notes",
                "notes: T1 embed_fn wired to the host's embed slot"
            ),
        },
        None => tracing::info!(
            target = "notes",
            "notes: no embed slot; T1 semantic recall is off"
        ),
    }
    match gliner {
        Some(gliner) => match notes.set_gliner_fn(gliner) {
            Err(e) => tracing::warn!(target = "notes", error = e, "notes: gliner_fn already set"),
            Ok(()) => tracing::info!(
                target = "notes",
                "notes: T2 gliner_fn wired to the host's GLiNER session"
            ),
        },
        None => tracing::info!(
            target = "notes",
            "notes: GLiNER not loaded; T2 will use author-supplied symbols/files only"
        ),
    }
    let node_id = node_id.unwrap_or_else(super::atlas::atlas_node_id);
    // Stamp outbound propagation events with this node id. `content_hash`
    // is the dedup key on the gossip wire, so this field is informational,
    // surfaced in the audit display.
    if let Err(e) = notes.set_origin_node_id(node_id.to_string()) {
        tracing::warn!(
            target = "notes",
            error = e,
            "notes: origin_node_id already set — wiring race?"
        );
    }
    // The reading half of the same identity: whose name a reader sees on
    // the notes coming back, including gossiped ones from peers.
    match roster {
        Some(roster) => {
            let self_name = roster.self_name().unwrap_or("<unnamed>").to_string();
            match notes.set_node_roster(roster) {
                Err(e) => tracing::warn!(
                    target = "notes",
                    error = e,
                    "notes: node_roster already set"
                ),
                Ok(()) => {
                    tracing::debug!(target = "notes", self_node = %node_id, self_name = %self_name,
                    "notes: node roster wired — authors resolve to mesh names")
                }
            }
        }
        None => tracing::debug!(target = "notes", self_node = %node_id,
            "notes: no mesh roster — note authors will render as raw node ids"),
    }
    let convergence = convergence.unwrap_or_else(|| Arc::new(SoloConvergence::new()));
    let kv: Arc<dyn ReplicatedKv> = Arc::new(super::atlas::atlas_kv());
    wire_note_propagation_sink(
        Arc::clone(notes),
        Arc::clone(&kv),
        node_id,
        Arc::clone(&convergence),
    );
    spawn_notes_tier_backfill(Arc::clone(notes));
    spawn_notes_ingest_poller(kv, Arc::clone(notes), node_id, convergence);
}

/// Wire NoteStore's outbound propagation sink to publish notes via the mesh store.
pub fn wire_note_propagation_sink(
    notes_store: Arc<NoteStore>,
    peer_store: Arc<dyn ReplicatedKv>,
    self_node_id: NodeId,
    convergence: Arc<dyn Convergence>,
) {
    // ── NoteStore propagation wiring ─────────────────────────────
    //
    // Now that `mesh_store` is live, wire NoteStore's outbound
    // sink to publish global non-private notes via app_id="notes"
    // (and private notes via "notes-private", which is
    // structurally gossip-excluded — see
    // the mesh's own `GOSSIP_EXCLUDED_APP_IDS`).
    let mesh_for_sink = Arc::clone(&peer_store);
    let self_id_for_sink = self_node_id;
    let sink: corpus_index::types::PropagationSinkFn =
        Arc::new(move |ev: &NotePropagationEvent| {
            // Everything this sink sees rides the public `notes`
            // namespace, tombstones included, so peers converge to the
            // deleted state. There is no private branch to take:
            // `NoteStore` gates BOTH sink call sites on `!private`
            // (`notes.rs:1841` and `:1936`), so a private note never
            // reaches here. Until cw-lift 2b this was an `if
            // ev.tombstone` whose two arms both evaluated to "notes",
            // which read as a private path that does not exist.
            let app_id = sovereign_contracts::peer::NOTES_APP_ID;
            // Receipt stamp (order commons-fluency fix 3): the wire
            // copy carries the publication clock — the moment THIS
            // sink's set() accepted it — which is the origin end of
            // the two-sided receipt. The original event is left
            // untouched; the store stamps its row from the return
            // value below. One timestamp, two uses: the wire copy and
            // the liveness stamp (fix 9) share it, so `/status`'s
            // convergence age can never disagree with the receipt.
            let sent_at = sovereign_time::unix_now();
            let mut wired = ev.clone();
            wired.sent_at = Some(sent_at);
            // This `to_vec` is the wire. Since order
            // `mesh-scale-t1-notes` it cannot emit the note's
            // embedding whatever `ev` holds — `NotePropagationEvent`
            // serializes that field as `null` unconditionally — which
            // took a gossiped note from a measured 16.1 KB to ~1.6 KB
            // and the 8 MiB push limit from ~520 notes to ~5,300
            // (research/scale-analysis/
            // MESH_SCALE_100_USERS_1000_CORPORA.md §8.3.1). Peers
            // re-embed the content in their own model space at ingest.
            match serde_json::to_vec(&wired) {
                Ok(bytes) => {
                    match mesh_for_sink.set(
                        app_id,
                        &ev.content_hash,
                        bytes.into(),
                        self_id_for_sink,
                    ) {
                        // `Ok(_)`: the bool is "did the value change"
                        // (a re-publish of the same hash reports
                        // false) — either way the note IS on the mesh,
                        // which is what the receipt means.
                        Ok(_) => {
                            tracing::debug!(
                                target = "notes",
                                content_hash = %ev.content_hash,
                                tombstone = ev.tombstone,
                                sent_at = wired.sent_at,
                                "notes: propagated"
                            );
                            // Liveness stamp (fix 9): the origin's
                            // publish path just succeeded — `/status`
                            // reads this as the convergence age.
                            convergence.record_outbound_publish_success(sent_at);
                            true
                        }
                        Err(e) => {
                            tracing::warn!(
                                target = "notes",
                                error = %e,
                                content_hash = %ev.content_hash,
                                "notes: mesh propagation sink set() failed"
                            );
                            false
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        target = "notes",
                        error = %e,
                        "notes: failed to serialize propagation event"
                    );
                    false
                }
            }
        });
    if let Err(e) = notes_store.set_propagation_sink(sink) {
        tracing::warn!(
            target = "notes",
            error = e,
            "notes: propagation_sink already set"
        );
    } else {
        tracing::info!(
            target = "notes",
            "notes: propagation_sink wired to the replicated KV (app_id=notes)"
        );
    }
}

/// Spawn the one-shot pre-T1/T2 note tier-artifact backfill (embeddings + entities).
pub fn spawn_notes_tier_backfill(notes_store: Arc<NoteStore>) {
    // One-shot tier-artifact backfill: pre-T1/T2 notes (anything
    // written before `embed_fn`/`gliner_fn` were wired) get
    // embeddings + entity rows on a background task so the
    // existing notes corpus benefits from semantic recall +
    // related-notes lookup immediately, not only when re-written.
    // Runs once per daemon start. Best-effort: rows that error
    // skip + pick up on the next start.
    //
    // Since order `mesh-scale-t1-notes` this is also the recovery
    // path for gossiped notes: the wire no longer carries vectors, so
    // `ingest_remote_notes` re-embeds each remote note locally, and
    // any it could not embed (embed slot down) lands here with no
    // `note_embeddings` row — outside the cosine pool, never blended
    // unembedded, until this pass picks it up. One-shot is the reason
    // the poller warns when its deferred count is non-zero.
    let notes_for_backfill = Arc::clone(&notes_store);
    // Supervised one-shot: best-effort + idempotent per the contract
    // above (rows that error skip + pick up next start) —
    // DAEMON_RESILIENCE.md P0.4.
    host_kit::supervise::spawn_supervised("notes_tier_backfill", move || {
        let notes_for_backfill = Arc::clone(&notes_for_backfill);
        async move {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let report = notes_for_backfill.backfill_tier_artifacts(0).await;
            if report.embeddings_backfilled > 0 || report.entities_backfilled > 0 {
                tracing::info!(
                    target = "notes",
                    embeddings = report.embeddings_backfilled,
                    entities = report.entities_backfilled,
                    embed_skipped = report.embed_skipped,
                    entity_skipped = report.entity_skipped,
                    "notes: tier-artifact backfill done"
                );
            }
        }
    });

    // TTL sweep — this is what keeps the store all-signal WITHOUT anyone running
    // `notes rationalize` by hand. Operational-exhaust kinds (tool_decision,
    // checkpoint…) age out on their own: first sweep ~30s after boot, then every
    // 24h. Tombstone (not delete) so it's resurrection-proof and, for any legacy
    // Global telemetry, gossips the removal to peers. TTL tunable via
    // SOVEREIGN_NOTES_EPHEMERAL_TTL_DAYS (default 30; <=0 disables).
    let notes_for_ttl = Arc::clone(&notes_store);
    // Supervised: the sweep is tombstone-idempotent; a panic must not
    // silently end TTL hygiene (DAEMON_RESILIENCE.md P0.4).
    host_kit::supervise::spawn_supervised("notes_ttl_sweep", move || {
        let notes_for_ttl = Arc::clone(&notes_for_ttl);
        async move {
            let ttl_days: i64 = std::env::var("SOVEREIGN_NOTES_EPHEMERAL_TTL_DAYS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(30);
            if ttl_days <= 0 {
                tracing::info!(
                    target = "notes",
                    "notes: ephemeral TTL sweep disabled (ttl_days<=0)"
                );
                return;
            }
            let ttl_secs = ttl_days * 86_400;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                match notes_for_ttl.purge_expired_ephemeral(ttl_secs).await {
                    Ok(n) if n > 0 => tracing::info!(
                        target = "notes",
                        swept = n,
                        ttl_days,
                        "notes: TTL sweep tombstoned expired ephemeral notes"
                    ),
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(target = "notes", error = %e, "notes: TTL sweep failed")
                    }
                }
            }
        }
    });
}

/// Spawn the poller that bridges inbound gossip note entries into `NoteStore`.
pub fn spawn_notes_ingest_poller(
    peer_store: Arc<dyn ReplicatedKv>,
    notes_store: Arc<NoteStore>,
    self_node_id: NodeId,
    convergence: Arc<dyn Convergence>,
) {
    // Ingest poller: bridge inbound replicated-KV entries (merged from
    // the ring) into `NoteStore::ingest_remote_notes`. The KV
    // doesn't expose a merge-callback today — periodic scan is
    // the path of least resistance. `ingest_remote_notes` is
    // idempotent (content_hash dedup) so re-reads cost nothing.
    //
    // Cadence: 10s, matching the gossip push-pull cadence. Skips
    // entries whose `origin` is `self_node_id` (those are notes
    // WE published; reingesting via our own sink would be a no-op
    // but wastes a JSON roundtrip).
    let mesh_for_poller = Arc::clone(&peer_store);
    let notes_for_poller = Arc::clone(&notes_store);
    let self_id_for_poller = self_node_id;
    let convergence_for_poller = Arc::clone(&convergence);
    // Supervised: ingest is content-hash idempotent; a panic must not
    // silently stop cross-peer note convergence (DAEMON_RESILIENCE.md
    // P0.4).
    host_kit::supervise::spawn_supervised("notes_ingest_poller", move || {
        let mesh_for_poller = Arc::clone(&mesh_for_poller);
        let notes_for_poller = Arc::clone(&notes_for_poller);
        let self_id_for_poller = self_id_for_poller;
        let convergence_for_poller = Arc::clone(&convergence_for_poller);
        async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let entries =
                    match mesh_for_poller.scan(sovereign_contracts::peer::NOTES_APP_ID, "") {
                        Ok(e) => e,
                        Err(err) => {
                            tracing::debug!(
                                target = "notes",
                                error = %err,
                                "notes: ingest poller scan failed"
                            );
                            continue;
                        }
                    };
                let mut events: Vec<NotePropagationEvent> = Vec::new();
                for entry in entries {
                    if entry.origin == self_id_for_poller {
                        continue;
                    }
                    match serde_json::from_slice::<NotePropagationEvent>(&entry.value) {
                        Ok(ev) => events.push(ev),
                        Err(e) => {
                            tracing::warn!(
                                target = "notes",
                                key = %entry.key,
                                error = %e,
                                "notes: ingest poller could not decode entry; skipping"
                            );
                        }
                    }
                }
                if events.is_empty() {
                    continue;
                }
                match notes_for_poller.ingest_remote_notes(events).await {
                    Ok(report) => {
                        // Liveness stamp (fix 9): a peer batch was
                        // applied — `/status` reads this as the
                        // inbound convergence age. Stamped on ANY Ok:
                        // a deduplicated-only batch still proves the
                        // scan→decode→apply loop ran.
                        convergence_for_poller
                            .record_inbound_ingest_success(sovereign_time::unix_now());
                        if report.inserted > 0 || report.tombstoned > 0 || report.forked > 0 {
                            tracing::info!(
                                target = "notes",
                                inserted = report.inserted,
                                tombstoned = report.tombstoned,
                                forked = report.forked,
                                deduplicated = report.deduplicated,
                                rejected = report.rejected,
                                // Order `mesh-scale-t1-notes`. These
                                // three answer, from the daemon log
                                // alone: did the peers' notes get an
                                // embedding in OUR model space
                                // (`recomputed`), are any sitting
                                // outside the cosine pool waiting on
                                // the backfill (`deferred`), and is
                                // some peer still on a pre-strip build
                                // shipping vectors we throw away
                                // (`foreign_discarded`)?
                                embeddings_recomputed = report.embeddings_recomputed,
                                embeddings_deferred = report.embeddings_deferred,
                                foreign_discarded = report.foreign_embeddings_discarded,
                                "notes: ingest poller converged batch"
                            );
                        }
                        if report.embeddings_deferred > 0 {
                            // Not an error — the note IS stored and IS
                            // readable by keyword. But it is invisible
                            // to semantic recall until
                            // `backfill_tier_artifacts` runs, and that
                            // is a one-shot at daemon start, so a
                            // non-zero count here that persists means
                            // the embed slot is down.
                            tracing::warn!(
                                target = "notes",
                                deferred = report.embeddings_deferred,
                                "notes: remote notes stored without a local embedding; \
                                 excluded from semantic recall until the tier backfill \
                                 runs (next daemon start)"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            target = "notes",
                            error = %e,
                            "notes: ingest_remote_notes failed"
                        );
                    }
                }
            }
        }
    });
}
