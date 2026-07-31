// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation tiered-retrieval persistence — `ConvTieredReader`
//! impl plus the inherent skeleton / RAPTOR-node / motif / vault-theme /
//! chunk-entity methods the enrichment provider writes through.

use super::*;

#[async_trait::async_trait]
impl ConvTieredReader for SqliteStateStore {
    async fn list_conv_skeletons_for_corpus(
        &self,
        corpus_id: &str,
        conv_uuids: &[String],
    ) -> sovereign_core::error::Result<Vec<ConvSkeletonRow>> {
        SqliteStateStore::list_conv_skeletons_for_corpus(self, corpus_id, conv_uuids).await
    }

    async fn list_conv_raptor_nodes(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> sovereign_core::error::Result<Vec<ConvRaptorNodeRow>> {
        SqliteStateStore::list_conv_raptor_nodes(self, corpus_id, conv_uuid).await
    }

    async fn list_corpus_raptor_nodes(
        &self,
        corpus_id: &str,
        min_level: i64,
    ) -> sovereign_core::error::Result<Vec<ConvRaptorNodeRow>> {
        SqliteStateStore::list_corpus_raptor_nodes(self, corpus_id, min_level).await
    }

    async fn corpus_raptor_version(&self, corpus_id: &str) -> sovereign_core::error::Result<i64> {
        SqliteStateStore::corpus_raptor_version(self, corpus_id).await
    }

    async fn list_chunk_entities_for_conv(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> sovereign_core::error::Result<Vec<sovereign_core::conv_tiered::ChunkEntityRow>> {
        SqliteStateStore::list_chunk_entities_for_conv(self, corpus_id, conv_uuid).await
    }

    async fn get_chunk_entity_progress(
        &self,
        corpus_id: &str,
    ) -> sovereign_core::error::Result<Option<sovereign_core::conv_tiered::ChunkEntityProgressRow>>
    {
        SqliteStateStore::get_chunk_entity_progress(self, corpus_id).await
    }

    async fn list_vault_themes_for_corpus(
        &self,
        corpus_id: &str,
    ) -> sovereign_core::error::Result<Vec<sovereign_core::conv_tiered::VaultThemeRow>> {
        SqliteStateStore::list_vault_themes_for_corpus(self, corpus_id).await
    }
}

//
// Persistence surface for the per-conversation T2/T3 enrichment
// output. The `TieredEnrichmentProvider` impl in `sovereign-tools`
// holds an `Arc<SqliteStateStore>` and writes through these methods;
// corpus-engine never touches the store directly (no dep on
// sovereign-store).

impl SqliteStateStore {
    /// Upsert the per-conv skeleton row. `state` is one of
    /// `ConvTieredState::as_str()`; future-proofed to bare string so
    /// the provider can write a custom error sub-state without a
    /// schema change.
    pub async fn save_conv_skeleton(&self, row: &ConvSkeletonRow) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO conv_skeletons
                (corpus_id, conv_uuid, state, skeleton_json, overview,
                 segments_json, chunk_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(corpus_id, conv_uuid) DO UPDATE SET
                state         = excluded.state,
                skeleton_json = excluded.skeleton_json,
                overview      = excluded.overview,
                segments_json = excluded.segments_json,
                chunk_count   = excluded.chunk_count,
                updated_at    = excluded.updated_at",
            rusqlite::params![
                row.corpus_id,
                row.conv_uuid,
                row.state,
                row.skeleton_json,
                row.overview,
                row.segments_json,
                row.chunk_count,
                row.updated_at,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    /// Read the state row for one conversation. Returns `None` if the
    /// tiered pass has never run for `(corpus_id, conv_uuid)`.
    pub async fn get_conv_skeleton(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Option<ConvSkeletonRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT corpus_id, conv_uuid, state, skeleton_json, overview,
                        segments_json, chunk_count, updated_at
                 FROM conv_skeletons
                 WHERE corpus_id = ?1 AND conv_uuid = ?2",
                rusqlite::params![corpus_id, conv_uuid],
                |r| {
                    Ok(ConvSkeletonRow {
                        corpus_id: r.get(0)?,
                        conv_uuid: r.get(1)?,
                        state: r.get(2)?,
                        skeleton_json: r.get(3)?,
                        overview: r.get(4)?,
                        segments_json: r.get(5)?,
                        chunk_count: r.get(6)?,
                        updated_at: r.get(7)?,
                    })
                },
            )
            .ok();
        Ok(row)
    }

    /// Record the content fingerprint of a conversation/note's last
    /// SUCCESSFUL enrichment. The provider calls this only on the Ready
    /// path (after skeleton + RAPTOR nodes persist), so a stored hash
    /// means "fully built from exactly this content". Upsert (one row per
    /// `(corpus_id, conv_uuid)`): a content-changed re-enrich overwrites
    /// the prior hash. Read back by [`Self::get_conv_content_hash`] for
    /// the conversation runner's skip-already-built check on re-import.
    pub async fn record_conv_content_hash(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        content_hash: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO conv_content_hash (corpus_id, conv_uuid, content_hash)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(corpus_id, conv_uuid) DO UPDATE SET
                content_hash = excluded.content_hash",
            rusqlite::params![corpus_id, conv_uuid, content_hash],
        )
        .map_err(map_db)?;
        Ok(())
    }

    /// The content fingerprint stored for a conversation/note by the last
    /// successful enrichment, or `None` if it was never enriched (or was
    /// enriched before this marker existed). `None` makes the runner
    /// re-enrich — the fail-safe direction (never wrongly skip).
    pub async fn get_conv_content_hash(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let hash = conn
            .query_row(
                "SELECT content_hash FROM conv_content_hash
                 WHERE corpus_id = ?1 AND conv_uuid = ?2",
                rusqlite::params![corpus_id, conv_uuid],
                |r| r.get::<_, String>(0),
            )
            .ok();
        Ok(hash)
    }

    /// Upsert the user's summary correction for one note. One active
    /// correction per note (PK `(corpus_id, conv_uuid)`) — re-flagging
    /// supersedes. `status` is `"pending"` on flag, flipped to
    /// `"applied"` by [`set_correction_status`] once the guided
    /// re-enrich lands. Part of the summary-revision loop
    /// (`docs/specs/SUMMARY_REVISION_LOOP.md`).
    pub async fn upsert_summary_correction(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        correction_hint: Option<&str>,
        original_summary: Option<&str>,
        status: &str,
        created_at: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO conv_summary_corrections
                (corpus_id, conv_uuid, correction_hint, original_summary,
                 status, created_at, applied_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
             ON CONFLICT(corpus_id, conv_uuid) DO UPDATE SET
                correction_hint  = excluded.correction_hint,
                original_summary = excluded.original_summary,
                status           = excluded.status,
                created_at       = excluded.created_at,
                applied_at       = NULL",
            rusqlite::params![
                corpus_id,
                conv_uuid,
                correction_hint,
                original_summary,
                status,
                created_at,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    /// Read the active correction for one note, or `None` if the user
    /// has never flagged it. The enrichment provider consults this on
    /// EVERY rebuild (`enrich_conversation`) to re-inject the hint so
    /// the correction persists; the desktop reads it to render the
    /// "revised by you" badge.
    pub async fn get_active_correction(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Option<SummaryCorrectionRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT corpus_id, conv_uuid, correction_hint, original_summary,
                        status, created_at, applied_at
                 FROM conv_summary_corrections
                 WHERE corpus_id = ?1 AND conv_uuid = ?2",
                rusqlite::params![corpus_id, conv_uuid],
                |r| {
                    Ok(SummaryCorrectionRow {
                        corpus_id: r.get(0)?,
                        conv_uuid: r.get(1)?,
                        correction_hint: r.get(2)?,
                        original_summary: r.get(3)?,
                        status: r.get(4)?,
                        created_at: r.get(5)?,
                        applied_at: r.get(6)?,
                    })
                },
            )
            .ok();
        Ok(row)
    }

    /// Flip a correction's lifecycle status (e.g. `pending` →
    /// `applied`) after the guided re-enrich completes. No-op if the
    /// note has no correction row.
    pub async fn set_correction_status(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        status: &str,
        applied_at: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE conv_summary_corrections
                SET status = ?3, applied_at = ?4
              WHERE corpus_id = ?1 AND conv_uuid = ?2",
            rusqlite::params![corpus_id, conv_uuid, status, applied_at],
        )
        .map_err(map_db)?;
        Ok(())
    }

    /// Replace the RAPTOR node set for one conversation. Atomic
    /// delete + insert in one transaction — mirrors the attached-doc
    /// `save_raptor_nodes` semantics so a partial provider crash
    /// doesn't leave a half-built tree on disk.
    pub async fn save_conv_raptor_nodes(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        nodes: &[ConvRaptorNodeRow],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_raptor_nodes WHERE corpus_id = ?1 AND conv_uuid = ?2",
            rusqlite::params![corpus_id, conv_uuid],
        )
        .map_err(map_db)?;
        for node in nodes {
            tx.execute(
                "INSERT INTO conv_raptor_nodes
                    (node_id, corpus_id, conv_uuid, level, summary,
                     summary_embedding, centroid_embedding,
                     children_node_ids, direct_member_chunk_ids,
                     evidence_chunk_ids, quote_spans, primary_entities,
                     cluster_coherence, created_at,
                     prompt_version, summarizer_model)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                rusqlite::params![
                    node.node_id,
                    node.corpus_id,
                    node.conv_uuid,
                    node.level,
                    node.summary,
                    encode_f32_vec(&node.summary_embedding),
                    encode_f32_vec(&node.centroid_embedding),
                    node.children_node_ids_json,
                    node.direct_member_chunk_ids_json,
                    node.evidence_chunk_ids_json,
                    node.quote_spans_json,
                    node.primary_entities_json,
                    node.cluster_coherence,
                    node.created_at,
                    node.prompt_version,
                    node.summarizer_model,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Replace the motif set for one conversation. Same atomicity
    /// rationale as `save_conv_raptor_nodes`.
    pub async fn save_conv_motifs(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        motifs: &[ConvMotifRow],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_motifs WHERE corpus_id = ?1 AND conv_uuid = ?2",
            rusqlite::params![corpus_id, conv_uuid],
        )
        .map_err(map_db)?;
        for motif in motifs {
            tx.execute(
                "INSERT INTO conv_motifs
                    (corpus_id, conv_uuid, term, tf_idf_score,
                     occurrence_chunk_ids, is_distinctive)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    motif.corpus_id,
                    motif.conv_uuid,
                    motif.term,
                    motif.tf_idf_score,
                    motif.occurrence_chunk_ids_json,
                    if motif.is_distinctive { 1i64 } else { 0i64 },
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Read every RAPTOR node for one conversation, ordered by level
    /// descending then by `node_id` so the briefing layer sees root
    /// summaries first (the top-of-tree paraphrase that anchors
    /// reading order) and leaf clusters last. Level-0 leaves carry
    /// `direct_member_chunk_ids`; higher levels carry only
    /// `evidence_chunk_ids` (the transitive subtree union).
    pub async fn list_conv_raptor_nodes(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Vec<ConvRaptorNodeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, corpus_id, conv_uuid, level, summary,
                        summary_embedding, centroid_embedding,
                        children_node_ids, direct_member_chunk_ids,
                        evidence_chunk_ids, quote_spans, primary_entities,
                        cluster_coherence, created_at,
                        prompt_version, summarizer_model
                 FROM conv_raptor_nodes
                 WHERE corpus_id = ?1 AND conv_uuid = ?2
                 ORDER BY level DESC, node_id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id, conv_uuid], |r| {
                Ok(ConvRaptorNodeRow {
                    node_id: r.get(0)?,
                    corpus_id: r.get(1)?,
                    conv_uuid: r.get(2)?,
                    level: r.get(3)?,
                    summary: r.get(4)?,
                    summary_embedding: decode_f32_vec(r.get::<_, Vec<u8>>(5)?.as_slice()),
                    centroid_embedding: decode_f32_vec(r.get::<_, Vec<u8>>(6)?.as_slice()),
                    children_node_ids_json: r.get(7)?,
                    direct_member_chunk_ids_json: r.get(8)?,
                    evidence_chunk_ids_json: r.get(9)?,
                    quote_spans_json: r.get(10)?,
                    primary_entities_json: r.get(11)?,
                    cluster_coherence: r.get(12)?,
                    created_at: r.get(13)?,
                    prompt_version: r.get(14)?,
                    summarizer_model: r.get(15)?,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Every RAPTOR node for a corpus at or above `min_level`
    /// (`min_level = 0` = all nodes incl. leaves; `1` = section/doc
    /// summaries only). The corpus-wide collapsed-tree pool for
    /// query-time cosine grounding (`Runtime::apply_raptor_grounding`).
    /// Mirrors `list_conv_raptor_nodes` but drops the `conv_uuid`
    /// predicate.
    pub async fn list_corpus_raptor_nodes(
        &self,
        corpus_id: &str,
        min_level: i64,
    ) -> Result<Vec<ConvRaptorNodeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, corpus_id, conv_uuid, level, summary,
                        summary_embedding, centroid_embedding,
                        children_node_ids, direct_member_chunk_ids,
                        evidence_chunk_ids, quote_spans, primary_entities,
                        cluster_coherence, created_at,
                        prompt_version, summarizer_model
                 FROM conv_raptor_nodes
                 WHERE corpus_id = ?1 AND level >= ?2
                 ORDER BY level DESC, conv_uuid ASC, node_id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id, min_level], |r| {
                Ok(ConvRaptorNodeRow {
                    node_id: r.get(0)?,
                    corpus_id: r.get(1)?,
                    conv_uuid: r.get(2)?,
                    level: r.get(3)?,
                    summary: r.get(4)?,
                    summary_embedding: decode_f32_vec(r.get::<_, Vec<u8>>(5)?.as_slice()),
                    centroid_embedding: decode_f32_vec(r.get::<_, Vec<u8>>(6)?.as_slice()),
                    children_node_ids_json: r.get(7)?,
                    direct_member_chunk_ids_json: r.get(8)?,
                    evidence_chunk_ids_json: r.get(9)?,
                    quote_spans_json: r.get(10)?,
                    primary_entities_json: r.get(11)?,
                    cluster_coherence: r.get(12)?,
                    created_at: r.get(13)?,
                    prompt_version: r.get(14)?,
                    summarizer_model: r.get(15)?,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Newest `created_at` across a corpus's RAPTOR nodes, or 0 when none.
    /// A cheap `MAX` aggregate over the `idx_conv_raptor_nodes_conv_level`
    /// `corpus_id` prefix — the build-version source for the
    /// `raptor_summaries.lance` freshness gate, avoiding the full-table BLOB
    /// decode the brute-force grounding scan performs.
    pub async fn corpus_raptor_version(&self, corpus_id: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        let v: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(created_at), 0) FROM conv_raptor_nodes WHERE corpus_id = ?1",
                rusqlite::params![corpus_id],
                |r| r.get(0),
            )
            .map_err(map_db)?;
        Ok(v)
    }

    /// Wipe every RAPTOR node for a single source_doc inside a
    /// corpus, without touching the rest of the vault. Used by the
    /// incremental sweeper: when a note's chunk set changes, the
    /// caller wants to drop the stale RAPTOR before
    /// `save_conv_raptor_nodes` re-writes it. Returns the number of
    /// rows actually deleted so the caller can log a skipped-doc
    /// short-circuit.
    pub async fn delete_conv_raptor_nodes_for_source(
        &self,
        corpus_id: &str,
        source_doc_id: &str,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM conv_raptor_nodes
                 WHERE corpus_id = ?1 AND conv_uuid = ?2",
                rusqlite::params![corpus_id, source_doc_id],
            )
            .map_err(map_db)?;
        Ok(deleted)
    }

    /// All `conv_uuid`s for a corpus whose `conv_skeletons.state` is
    /// `'Ready'`. Used by the vault-wide synthesis pass to enumerate
    /// the per-note RAPTOR trees that should feed the cross-note
    /// theme clustering. Returns deterministically-ordered uuids
    /// (`ORDER BY conv_uuid ASC`) so the synthesis input is stable
    /// across re-runs.
    pub async fn list_ready_source_doc_ids_for_corpus(
        &self,
        corpus_id: &str,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT conv_uuid
                 FROM conv_skeletons
                 WHERE corpus_id = ?1 AND state = 'Ready'
                 ORDER BY conv_uuid ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id], |r| r.get::<_, String>(0))
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Atomically replace the vault-wide synthesis themes for one
    /// corpus. Like `save_conv_raptor_nodes`, the entire prior theme
    /// set is deleted in the same transaction so a re-synthesis pass
    /// observably swaps the briefing's "Vault themes" block as one
    /// commit — never partial.
    pub async fn save_vault_themes(
        &self,
        corpus_id: &str,
        themes: &[sovereign_core::conv_tiered::VaultThemeRow],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM vault_themes WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        for theme in themes {
            tx.execute(
                "INSERT INTO vault_themes
                    (corpus_id, theme_id, summary, summary_embedding,
                     member_source_doc_ids_json, cluster_coherence,
                     created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    theme.corpus_id,
                    theme.theme_id,
                    theme.summary,
                    encode_f32_vec(&theme.summary_embedding),
                    theme.member_source_doc_ids_json,
                    theme.cluster_coherence,
                    theme.created_at,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// All vault-wide synthesis themes for one corpus, ordered by
    /// `cluster_coherence DESC`. Empty when the synthesis pass has
    /// not run yet — caller (the briefing layer) treats empty as
    /// "no vault-wide block, fall through to per-note signposts".
    pub async fn list_vault_themes_for_corpus(
        &self,
        corpus_id: &str,
    ) -> Result<Vec<sovereign_core::conv_tiered::VaultThemeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, theme_id, summary, summary_embedding,
                        member_source_doc_ids_json, cluster_coherence,
                        created_at
                 FROM vault_themes
                 WHERE corpus_id = ?1
                 ORDER BY cluster_coherence DESC, theme_id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id], |r| {
                Ok(sovereign_core::conv_tiered::VaultThemeRow {
                    corpus_id: r.get(0)?,
                    theme_id: r.get(1)?,
                    summary: r.get(2)?,
                    summary_embedding: decode_f32_vec(r.get::<_, Vec<u8>>(3)?.as_slice()),
                    member_source_doc_ids_json: r.get(4)?,
                    cluster_coherence: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Wipe every vault-wide theme for a corpus. Called by the
    /// disable-enrichment teardown path so the briefing stops
    /// referencing themes that no longer reflect the current vault
    /// state.
    pub async fn delete_vault_themes_for_corpus(&self, corpus_id: &str) -> Result<usize> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM vault_themes WHERE corpus_id = ?1",
                rusqlite::params![corpus_id],
            )
            .map_err(map_db)?;
        Ok(deleted)
    }

    /// Bulk read for `(state, overview, chunk_count)` triples across
    /// many conversations. Avoids a per-conv round-trip when the
    /// briefing builder is selecting which convs to surface. Drops
    /// conv_uuids that have no row (briefing layer treats those as
    /// "no tiered enrichment yet").
    pub async fn list_conv_skeletons_for_corpus(
        &self,
        corpus_id: &str,
        conv_uuids: &[String],
    ) -> Result<Vec<ConvSkeletonRow>> {
        if conv_uuids.is_empty() {
            return Ok(Vec::new());
        }
        // SQLite has a default 999 parameter limit; cap the IN-list
        // size to stay well under. Briefing layer caps at top-8 anyway,
        // so this floor is purely defensive.
        let max_in_list = conv_uuids.len().min(500);
        let placeholders: Vec<&str> = (0..max_in_list).map(|_| "?").collect();
        let mut placeholder_list = String::from("?,");
        placeholder_list.push_str(&placeholders.join(","));
        let sql = format!(
            "SELECT corpus_id, conv_uuid, state, skeleton_json, overview,
                    segments_json, chunk_count, updated_at
             FROM conv_skeletons
             WHERE corpus_id = ?1 AND conv_uuid IN ({})",
            placeholders.join(",")
        );
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(max_in_list + 1);
        params.push(&corpus_id);
        for uuid in conv_uuids.iter().take(max_in_list) {
            params.push(uuid);
        }
        let rows = stmt
            .query_map(params.as_slice(), |r| {
                Ok(ConvSkeletonRow {
                    corpus_id: r.get(0)?,
                    conv_uuid: r.get(1)?,
                    state: r.get(2)?,
                    skeleton_json: r.get(3)?,
                    overview: r.get(4)?,
                    segments_json: r.get(5)?,
                    chunk_count: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Enumerate every conv corpus that has at least one row in
    /// `conv_skeletons`, together with per-state counts + max
    /// `updated_at`. Used by the desktop Atlas index
    /// (`atlas_list_conv_corpora`) to render the "Conversations"
    /// group alongside atoms.json-backed corpora.
    ///
    /// Returns one tuple per corpus_id: `(corpus_id, total,
    /// max_updated_at, per_state)`. Empty when no tiered enrichment
    /// has ever run.
    pub async fn list_conv_corpora_with_state_buckets(
        &self,
    ) -> Result<Vec<(String, u64, i64, Vec<(String, u64)>)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, state, COUNT(*) as n, MAX(updated_at) as max_ts
                 FROM conv_skeletons
                 GROUP BY corpus_id, state
                 ORDER BY corpus_id ASC, state ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(map_db)?;
        let mut by_corpus: std::collections::BTreeMap<String, (Vec<(String, u64)>, i64)> =
            std::collections::BTreeMap::new();
        for row in rows {
            let (corpus_id, state, n, ts) = row.map_err(map_db)?;
            let entry = by_corpus.entry(corpus_id).or_default();
            entry.0.push((state, n as u64));
            entry.1 = entry.1.max(ts);
        }
        Ok(by_corpus
            .into_iter()
            .map(|(corpus_id, (per_state, max_ts))| {
                let total: u64 = per_state.iter().map(|(_, n)| *n).sum();
                (corpus_id, total, max_ts, per_state)
            })
            .collect())
    }

    /// Paginated list of conversations in one corpus, optionally
    /// filtered by case-insensitive substring on `overview`. Returns
    /// the page slice + total matching count.
    pub async fn list_conversations_paginated(
        &self,
        corpus_id: &str,
        filter: Option<&str>,
        offset: u64,
        limit: u64,
    ) -> Result<(Vec<ConvSkeletonRow>, u64)> {
        let conn = self.conn.lock().await;
        let filter_clause = if filter.is_some() {
            "AND COALESCE(overview, '') LIKE ?2"
        } else {
            ""
        };
        // Total count first.
        let count_sql =
            format!("SELECT COUNT(*) FROM conv_skeletons WHERE corpus_id = ?1 {filter_clause}");
        let total: i64 = if let Some(f) = filter {
            let needle = format!("%{}%", f.replace('%', "\\%").replace('_', "\\_"));
            conn.query_row(&count_sql, rusqlite::params![corpus_id, needle], |r| {
                r.get(0)
            })
            .map_err(map_db)?
        } else {
            conn.query_row(&count_sql, rusqlite::params![corpus_id], |r| r.get(0))
                .map_err(map_db)?
        };

        // Page itself, ordered by updated_at DESC then conv_uuid for
        // stable pagination. SQLite supports OFFSET on indexed sorts,
        // but for very large corpora a keyset cursor (last-seen
        // updated_at, conv_uuid) would be preferable; deferred until
        // anyone hits a 100k+ conv corpus.
        let page_sql = format!(
            "SELECT corpus_id, conv_uuid, state, skeleton_json, overview,
                    segments_json, chunk_count, updated_at
             FROM conv_skeletons
             WHERE corpus_id = ?1 {filter_clause}
             ORDER BY updated_at DESC, conv_uuid ASC
             LIMIT ?{} OFFSET ?{}",
            if filter.is_some() { 3 } else { 2 },
            if filter.is_some() { 4 } else { 3 },
        );
        let mut stmt = conn.prepare(&page_sql).map_err(map_db)?;
        let map_row = |r: &rusqlite::Row<'_>| {
            Ok(ConvSkeletonRow {
                corpus_id: r.get(0)?,
                conv_uuid: r.get(1)?,
                state: r.get(2)?,
                skeleton_json: r.get(3)?,
                overview: r.get(4)?,
                segments_json: r.get(5)?,
                chunk_count: r.get(6)?,
                updated_at: r.get(7)?,
            })
        };
        let rows_result = if let Some(f) = filter {
            let needle = format!("%{}%", f.replace('%', "\\%").replace('_', "\\_"));
            stmt.query_map(
                rusqlite::params![corpus_id, needle, limit as i64, offset as i64],
                map_row,
            )
        } else {
            stmt.query_map(
                rusqlite::params![corpus_id, limit as i64, offset as i64],
                map_row,
            )
        }
        .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows_result {
            out.push(row.map_err(map_db)?);
        }
        Ok((out, total as u64))
    }

    /// Replace all chunk_entities rows for one conversation. Writes
    /// inside a transaction so concurrent reads see either the prior
    /// or the new set, never a half-applied state. `rows.len()` is
    /// also the natural progress increment for the batch CLI.
    pub async fn save_chunk_entities_for_conv(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        rows: &[sovereign_core::conv_tiered::ChunkEntityRow],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM chunk_entities WHERE corpus_id = ?1 AND conv_uuid = ?2",
            rusqlite::params![corpus_id, conv_uuid],
        )
        .map_err(map_db)?;
        for row in rows {
            tx.execute(
                "INSERT OR REPLACE INTO chunk_entities
                    (corpus_id, chunk_id, text, label, char_start,
                     char_end, score, conv_uuid, extracted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    row.corpus_id,
                    row.chunk_id as i64,
                    row.text,
                    row.label,
                    row.char_start,
                    row.char_end,
                    row.score,
                    row.conv_uuid,
                    row.extracted_at,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Tear down all tiered-enrichment rows for one corpus. Used by
    /// `LocalCorpusManager::disable_enrichment` to clean up
    /// `conv_raptor_nodes` / `conv_motifs` / `conv_skeletons` /
    /// `chunk_entities` / `chunk_entity_progress` so re-enabling on
    /// the same corpus starts from a clean slate.
    ///
    /// One transaction so partial teardown isn't possible — either
    /// the corpus has tiered data or it doesn't.
    pub async fn delete_tiered_for_corpus(&self, corpus_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_raptor_nodes WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_motifs WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_skeletons WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.execute(
            "DELETE FROM chunk_entities WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.execute(
            "DELETE FROM chunk_entity_progress WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Bulk-write chunk_entities rows scoped by chunk_id (no conv
    /// grouping). Idempotent via PRIMARY KEY collision → REPLACE.
    /// Used by non-conv corpora that don't have a `conv_uuid` to
    /// group on.
    pub async fn save_chunk_entities(
        &self,
        rows: &[sovereign_core::conv_tiered::ChunkEntityRow],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        for row in rows {
            tx.execute(
                "INSERT OR REPLACE INTO chunk_entities
                    (corpus_id, chunk_id, text, label, char_start,
                     char_end, score, conv_uuid, extracted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    row.corpus_id,
                    row.chunk_id as i64,
                    row.text,
                    row.label,
                    row.char_start,
                    row.char_end,
                    row.score,
                    row.conv_uuid,
                    row.extracted_at,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Distinct `chunk_id` values that produced at least one
    /// `chunk_entities` row for a corpus (i.e. ENTITY-BEARING chunks
    /// only), returned as a `HashSet`. Empty set when no extraction has
    /// run yet for the corpus.
    ///
    /// DO NOT use this as the "already processed" set for an incremental
    /// NER delta. A chunk that GliNER finds no entities in writes no
    /// `chunk_entities` row, so this query omits every entity-less chunk
    /// (headers, code blocks, short lines). Deriving "done" from it makes
    /// those chunks look unprocessed forever — they stay in the delta and
    /// re-NER on every pass, pinning CPU and never converging (the
    /// 2026-07-16 vault-enrichment bug). The correct done-set is
    /// [`Self::list_ner_processed_chunk_ids`], which unions this with the
    /// `chunk_ner_processed` marker table. This method is retained only
    /// for callers that genuinely want the entity-bearing subset.
    pub async fn list_extracted_chunk_ids_for_corpus(
        &self,
        corpus_id: &str,
    ) -> Result<std::collections::HashSet<u64>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT DISTINCT chunk_id FROM chunk_entities WHERE corpus_id = ?1")
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id], |r| r.get::<_, i64>(0))
            .map_err(map_db)?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row.map_err(map_db)? as u64);
        }
        Ok(out)
    }

    /// Record that NER has been run on these chunks, regardless of
    /// whether any entities were found. This is the durable "processed"
    /// marker the incremental delta needs: a chunk that yields zero
    /// entities writes no `chunk_entities` row, so without this it would
    /// look unprocessed forever and be re-run on every pass. Idempotent
    /// (INSERT OR IGNORE) so re-recording an already-marked chunk is a
    /// no-op — safe to call once per note after its NER batch completes.
    pub async fn record_ner_processed_chunks(
        &self,
        corpus_id: &str,
        chunk_ids: &[u64],
    ) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        for &chunk_id in chunk_ids {
            tx.execute(
                "INSERT OR IGNORE INTO chunk_ner_processed (corpus_id, chunk_id)
                 VALUES (?1, ?2)",
                rusqlite::params![corpus_id, chunk_id as i64],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// The set of chunk ids that have been NER-processed for a corpus —
    /// the UNION of chunks that produced an entity (`chunk_entities`) and
    /// chunks explicitly marked processed-but-empty (`chunk_ner_processed`).
    /// This is the correct "already done" set for the incremental delta:
    /// `list_extracted_chunk_ids_for_corpus` alone omits entity-less
    /// chunks, so the delta never converged and NER re-ran them every
    /// pass. The union with `chunk_entities` means corpora enriched
    /// before the marker table existed still recognise their entity-
    /// bearing chunks as done without a re-run — only the entity-less
    /// tail is re-processed once, and then marked.
    pub async fn list_ner_processed_chunk_ids(
        &self,
        corpus_id: &str,
    ) -> Result<std::collections::HashSet<u64>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT chunk_id FROM chunk_entities WHERE corpus_id = ?1
                 UNION
                 SELECT chunk_id FROM chunk_ner_processed WHERE corpus_id = ?1",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id], |r| r.get::<_, i64>(0))
            .map_err(map_db)?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row.map_err(map_db)? as u64);
        }
        Ok(out)
    }

    /// Aggregate one entity's footprint inside a corpus. Drives the
    /// desktop's Atlas-view entity drawer. Match is case-insensitive
    /// on `text` (so "Borges" + "borges" + "BORGES" fold into one
    /// row); per-label breakdown surfaces homonyms ("Swift"
    /// Person vs "SWIFT" Organization) without merging.
    ///
    /// `co_limit` caps co-occurring entities; `conv_limit` caps the
    /// top-conv list. Pass small values (~20) — the drawer shows the
    /// head only; the full list is reserved for the "expand" tail.
    pub async fn aggregate_entity(
        &self,
        corpus_id: &str,
        text: &str,
        co_limit: usize,
        conv_limit: usize,
    ) -> Result<sovereign_core::conv_tiered::EntityAggregateRow> {
        use sovereign_core::conv_tiered::{
            CoOccurringEntity, EntityAggregateRow, EntityConvHit, EntityLabelCount,
        };
        let conn = self.conn.lock().await;

        // Canonical display form: pick the most-common surface-form
        // variant inside the corpus. Ties broken by alphabetical so
        // the answer is deterministic across re-queries.
        let canonical: String = conn
            .query_row(
                "SELECT text FROM chunk_entities
                 WHERE corpus_id = ?1 AND text = ?2 COLLATE NOCASE
                 GROUP BY text
                 ORDER BY COUNT(*) DESC, text ASC
                 LIMIT 1",
                rusqlite::params![corpus_id, text],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(map_db)?
            .unwrap_or_else(|| text.to_string());

        // Label breakdown.
        let mut labels_stmt = conn
            .prepare(
                "SELECT label, COUNT(*) AS n
                 FROM chunk_entities
                 WHERE corpus_id = ?1 AND text = ?2 COLLATE NOCASE
                 GROUP BY label
                 ORDER BY n DESC, label ASC",
            )
            .map_err(map_db)?;
        let labels: Vec<EntityLabelCount> = labels_stmt
            .query_map(rusqlite::params![corpus_id, text], |r| {
                Ok(EntityLabelCount {
                    label: r.get::<_, String>(0)?,
                    count: r.get::<_, i64>(1)?,
                })
            })
            .map_err(map_db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_db)?;
        drop(labels_stmt);

        // Scalar counts in one round-trip.
        let (mention_count, conv_count, chunk_count): (i64, i64, i64) = conn
            .query_row(
                "SELECT
                    COUNT(*),
                    COUNT(DISTINCT conv_uuid),
                    COUNT(DISTINCT chunk_id)
                 FROM chunk_entities
                 WHERE corpus_id = ?1 AND text = ?2 COLLATE NOCASE",
                rusqlite::params![corpus_id, text],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(map_db)?;

        // Top convs by mention count. NULL conv_uuid rows (non-conv
        // corpora) are filtered out here so the drawer's "where it
        // appears" list never tries to link to a missing conv.
        let mut conv_stmt = conn
            .prepare(
                "SELECT conv_uuid, COUNT(*) AS n
                 FROM chunk_entities
                 WHERE corpus_id = ?1
                   AND text = ?2 COLLATE NOCASE
                   AND conv_uuid IS NOT NULL
                 GROUP BY conv_uuid
                 ORDER BY n DESC, conv_uuid ASC
                 LIMIT ?3",
            )
            .map_err(map_db)?;
        let top_convs: Vec<EntityConvHit> = conv_stmt
            .query_map(rusqlite::params![corpus_id, text, conv_limit as i64], |r| {
                Ok(EntityConvHit {
                    conv_uuid: r.get::<_, String>(0)?,
                    mention_count: r.get::<_, i64>(1)?,
                })
            })
            .map_err(map_db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_db)?;
        drop(conv_stmt);

        // Co-occurring entities: chunks that contain the seed
        // entity, with every OTHER entity in those chunks bucketed
        // by `(text, label)`. The self-join keys on chunk_id so
        // intra-chunk neighbours are counted, not inter-chunk
        // collisions.
        let mut co_stmt = conn
            .prepare(
                "SELECT other.text, other.label, COUNT(DISTINCT other.chunk_id) AS shared
                 FROM chunk_entities AS seed
                 JOIN chunk_entities AS other
                   ON other.corpus_id = seed.corpus_id
                  AND other.chunk_id = seed.chunk_id
                 WHERE seed.corpus_id = ?1
                   AND seed.text = ?2 COLLATE NOCASE
                   AND NOT (other.text = ?2 COLLATE NOCASE)
                 GROUP BY other.text, other.label
                 ORDER BY shared DESC, other.text ASC
                 LIMIT ?3",
            )
            .map_err(map_db)?;
        let co_occurring: Vec<CoOccurringEntity> = co_stmt
            .query_map(rusqlite::params![corpus_id, text, co_limit as i64], |r| {
                Ok(CoOccurringEntity {
                    text: r.get::<_, String>(0)?,
                    label: r.get::<_, String>(1)?,
                    shared_chunk_count: r.get::<_, i64>(2)?,
                })
            })
            .map_err(map_db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_db)?;

        Ok(EntityAggregateRow {
            corpus_id: corpus_id.to_string(),
            text: canonical,
            labels,
            mention_count,
            conv_count,
            chunk_count,
            top_convs,
            co_occurring,
        })
    }

    /// Read every `chunk_entities` row for one conversation.
    /// Returned in `(chunk_id ASC, char_start ASC)` order.
    pub async fn list_chunk_entities_for_conv(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Vec<sovereign_core::conv_tiered::ChunkEntityRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, chunk_id, text, label, char_start,
                        char_end, score, conv_uuid, extracted_at
                 FROM chunk_entities
                 WHERE corpus_id = ?1 AND conv_uuid = ?2
                 ORDER BY chunk_id ASC, char_start ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id, conv_uuid], |r| {
                Ok(sovereign_core::conv_tiered::ChunkEntityRow {
                    corpus_id: r.get(0)?,
                    chunk_id: r.get::<_, i64>(1)? as u64,
                    text: r.get(2)?,
                    label: r.get(3)?,
                    char_start: r.get(4)?,
                    char_end: r.get(5)?,
                    score: r.get(6)?,
                    conv_uuid: r.get(7)?,
                    extracted_at: r.get(8)?,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Upsert per-corpus extraction progress. Drives the CLI's
    /// progress bar + the desktop's "entity extraction running"
    /// badge.
    pub async fn upsert_chunk_entity_progress(
        &self,
        row: &sovereign_core::conv_tiered::ChunkEntityProgressRow,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO chunk_entity_progress
                (corpus_id, chunks_processed, chunks_total,
                 mentions_extracted, last_chunk_id, started_at,
                 updated_at, finished_at, state, model_id, threshold,
                 labels_json, error_msg)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(corpus_id) DO UPDATE SET
                chunks_processed = excluded.chunks_processed,
                chunks_total = excluded.chunks_total,
                mentions_extracted = excluded.mentions_extracted,
                last_chunk_id = excluded.last_chunk_id,
                updated_at = excluded.updated_at,
                finished_at = excluded.finished_at,
                state = excluded.state,
                model_id = excluded.model_id,
                threshold = excluded.threshold,
                labels_json = excluded.labels_json,
                error_msg = excluded.error_msg",
            rusqlite::params![
                row.corpus_id,
                row.chunks_processed,
                row.chunks_total,
                row.mentions_extracted,
                row.last_chunk_id,
                row.started_at,
                row.updated_at,
                row.finished_at,
                row.state,
                row.model_id,
                row.threshold,
                row.labels_json,
                row.error_msg,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    pub async fn get_chunk_entity_progress(
        &self,
        corpus_id: &str,
    ) -> Result<Option<sovereign_core::conv_tiered::ChunkEntityProgressRow>> {
        let conn = self.conn.lock().await;
        Ok(conn
            .query_row(
                "SELECT corpus_id, chunks_processed, chunks_total,
                        mentions_extracted, last_chunk_id, started_at,
                        updated_at, finished_at, state, model_id,
                        threshold, labels_json, error_msg
                 FROM chunk_entity_progress
                 WHERE corpus_id = ?1",
                rusqlite::params![corpus_id],
                |r| {
                    Ok(sovereign_core::conv_tiered::ChunkEntityProgressRow {
                        corpus_id: r.get(0)?,
                        chunks_processed: r.get(1)?,
                        chunks_total: r.get(2)?,
                        mentions_extracted: r.get(3)?,
                        last_chunk_id: r.get(4)?,
                        started_at: r.get(5)?,
                        updated_at: r.get(6)?,
                        finished_at: r.get(7)?,
                        state: r.get(8)?,
                        model_id: r.get(9)?,
                        threshold: r.get(10)?,
                        labels_json: r.get(11)?,
                        error_msg: r.get(12)?,
                    })
                },
            )
            .ok())
    }

    /// Inventory of conv states for a corpus — used by ops tools to
    /// answer "how far has the tiered pass progressed across this
    /// import?". Returns `(state, count)` pairs.
    pub async fn count_conv_skeletons_by_state(
        &self,
        corpus_id: &str,
    ) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT state, COUNT(*) FROM conv_skeletons
                 WHERE corpus_id = ?1 GROUP BY state ORDER BY state",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }
}
