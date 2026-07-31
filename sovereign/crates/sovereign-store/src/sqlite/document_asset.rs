// SPDX-License-Identifier: AGPL-3.0-or-later
//! `DocumentAssetStore` impl — document assets, skeletons, and
//! per-asset RAPTOR nodes.

use super::*;

// ─── DocumentAssetStore ──────────────────────────────────────

#[async_trait]
impl DocumentAssetStore for SqliteStateStore {
    async fn save_document_asset(&self, asset: &DocumentAsset) -> Result<()> {
        let conn = self.conn.lock().await;
        let state_json =
            serde_json::to_string(&asset.state).map_err(|e| Error::Storage(e.to_string()))?;
        let skeleton_json = asset
            .skeleton
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| Error::Storage(e.to_string()))?;
        let ingested_ts = asset.ingested_at.timestamp();
        let doc_type = serde_json::to_string(&asset.document_type)
            .map_err(|e| Error::Storage(e.to_string()))?;

        conn.execute(
            "INSERT OR REPLACE INTO document_assets
             (id, title, filename, file_size_mb, word_count, chunk_count,
              document_type, ingested_at, index_id, state_json, skeleton_json, owner)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                asset.id,
                asset.title,
                asset.filename,
                asset.file_size_mb,
                asset.word_count as i64,
                asset.chunk_count as i64,
                doc_type,
                ingested_ts,
                asset.index_id,
                state_json,
                skeleton_json,
                asset.owner,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn update_asset_state(&self, id: &str, state: &AssetState) -> Result<()> {
        let conn = self.conn.lock().await;
        let state_json = serde_json::to_string(state).map_err(|e| Error::Storage(e.to_string()))?;
        conn.execute(
            "UPDATE document_assets SET state_json = ?1 WHERE id = ?2",
            rusqlite::params![state_json, id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn save_asset_skeleton(
        &self,
        id: &str,
        skeleton: &DocumentSkeleton,
        document_type: &DocumentTypeTag,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let skeleton_json =
            serde_json::to_string(skeleton).map_err(|e| Error::Storage(e.to_string()))?;
        let doc_type_json =
            serde_json::to_string(document_type).map_err(|e| Error::Storage(e.to_string()))?;
        conn.execute(
            "UPDATE document_assets SET skeleton_json = ?1, document_type = ?2 WHERE id = ?3",
            rusqlite::params![skeleton_json, doc_type_json, id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_document_asset(&self, id: &str) -> Result<Option<DocumentAsset>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, title, filename, file_size_mb, word_count, chunk_count,
                    document_type, ingested_at, index_id, state_json, skeleton_json, owner
             FROM document_assets WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok(row_to_document_asset(row)),
        )
        .optional()
        .map_err(map_db)
    }

    async fn list_document_assets(&self) -> Result<Vec<DocumentAsset>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, title, filename, file_size_mb, word_count, chunk_count,
                        document_type, ingested_at, index_id, state_json, skeleton_json, owner
                 FROM document_assets
                 ORDER BY ingested_at DESC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map([], |row| Ok(row_to_document_asset(row)))
            .map_err(map_db)?;
        let mut assets = Vec::new();
        for row in rows {
            assets.push(row.map_err(map_db)?);
        }
        Ok(assets)
    }

    async fn delete_document_asset(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        // Cascade: document_conversations has ON DELETE CASCADE.
        // document_operations doesn't — clean up explicitly.
        conn.execute(
            "DELETE FROM document_operations WHERE asset_id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_db)?;
        conn.execute(
            "DELETE FROM document_assets WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn save_document_operation(
        &self,
        message_id: &str,
        asset_id: &str,
        operation: &DocumentAssetOperation,
        duration_ms: u64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let operation_json =
            serde_json::to_string(operation).map_err(|e| Error::Storage(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO document_operations
             (message_id, asset_id, operation_json, duration_ms)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![message_id, asset_id, operation_json, duration_ms as i64],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn save_raptor_nodes(&self, asset_id: &str, nodes: &[RaptorNode]) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM raptor_nodes WHERE asset_id = ?1",
            rusqlite::params![asset_id],
        )
        .map_err(map_db)?;
        for node in nodes {
            let children = serde_json::to_string(&node.children_node_ids)
                .map_err(|e| Error::Storage(e.to_string()))?;
            let direct_members: Option<String> = if node.direct_member_chunk_ids.is_empty() {
                None
            } else {
                Some(
                    serde_json::to_string(&node.direct_member_chunk_ids)
                        .map_err(|e| Error::Storage(e.to_string()))?,
                )
            };
            let evidence = serde_json::to_string(&node.evidence_chunk_ids)
                .map_err(|e| Error::Storage(e.to_string()))?;
            let quotes = serde_json::to_string(&node.quote_spans)
                .map_err(|e| Error::Storage(e.to_string()))?;
            let entities = serde_json::to_string(&node.primary_entities)
                .map_err(|e| Error::Storage(e.to_string()))?;
            tx.execute(
                "INSERT INTO raptor_nodes
                 (node_id, asset_id, level, summary,
                  summary_embedding, centroid_embedding,
                  children_node_ids, direct_member_chunk_ids,
                  evidence_chunk_ids, quote_spans, primary_entities,
                  cluster_coherence, created_at,
                  prompt_version, summarizer_model)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    node.node_id,
                    asset_id,
                    node.level as i64,
                    node.summary,
                    encode_f32_vec(&node.summary_embedding),
                    encode_f32_vec(&node.centroid_embedding),
                    children,
                    direct_members,
                    evidence,
                    quotes,
                    entities,
                    node.cluster_coherence as f64,
                    node.created_at.timestamp(),
                    node.prompt_version,
                    node.summarizer_model,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    async fn list_raptor_nodes(&self, asset_id: &str) -> Result<Vec<RaptorNode>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, level, summary, summary_embedding, centroid_embedding,
                        children_node_ids, direct_member_chunk_ids, evidence_chunk_ids,
                        quote_spans, primary_entities, cluster_coherence, created_at,
                        prompt_version, summarizer_model
                 FROM raptor_nodes
                 WHERE asset_id = ?1
                 ORDER BY level ASC, node_id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![asset_id], |row| {
                Ok(row_to_raptor_node(row))
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)??);
        }
        Ok(out)
    }

    async fn get_raptor_node(&self, node_id: &str) -> Result<Option<RaptorNode>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, level, summary, summary_embedding, centroid_embedding,
                        children_node_ids, direct_member_chunk_ids, evidence_chunk_ids,
                        quote_spans, primary_entities, cluster_coherence, created_at,
                        prompt_version, summarizer_model
                 FROM raptor_nodes
                 WHERE node_id = ?1",
            )
            .map_err(map_db)?;
        let mut rows = stmt
            .query_map(rusqlite::params![node_id], |row| {
                Ok(row_to_raptor_node(row))
            })
            .map_err(map_db)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(map_db)??)),
            None => Ok(None),
        }
    }

    async fn save_asset_motifs(&self, asset_id: &str, motifs: &[AssetMotif]) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM asset_motifs WHERE asset_id = ?1",
            rusqlite::params![asset_id],
        )
        .map_err(map_db)?;
        for motif in motifs {
            let occurrences = serde_json::to_string(&motif.occurrence_chunk_ids)
                .map_err(|e| Error::Storage(e.to_string()))?;
            tx.execute(
                "INSERT INTO asset_motifs
                 (asset_id, term, tf_idf_score, occurrence_chunk_ids, is_distinctive)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    asset_id,
                    motif.term,
                    motif.tf_idf_score as f64,
                    occurrences,
                    if motif.is_distinctive { 1 } else { 0 },
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    async fn list_asset_motifs(&self, asset_id: &str) -> Result<Vec<AssetMotif>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT term, tf_idf_score, occurrence_chunk_ids, is_distinctive
                 FROM asset_motifs
                 WHERE asset_id = ?1
                 ORDER BY is_distinctive DESC, tf_idf_score DESC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![asset_id], |row| {
                let term: String = row.get(0)?;
                let tf_idf_score: f64 = row.get(1)?;
                let occurrences: String = row.get(2)?;
                let is_distinctive: i64 = row.get(3)?;
                Ok((term, tf_idf_score, occurrences, is_distinctive))
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            let (term, score, occurrences_json, is_distinctive) = row.map_err(map_db)?;
            let occurrence_chunk_ids: Vec<u32> = serde_json::from_str(&occurrences_json)
                .map_err(|e| Error::Storage(e.to_string()))?;
            out.push(AssetMotif {
                term,
                tf_idf_score: score as f32,
                occurrence_chunk_ids,
                is_distinctive: is_distinctive != 0,
            });
        }
        Ok(out)
    }
}

fn row_to_raptor_node(row: &rusqlite::Row) -> Result<RaptorNode> {
    let node_id: String = row.get(0).map_err(map_db)?;
    let level: i64 = row.get(1).map_err(map_db)?;
    let summary: String = row.get(2).map_err(map_db)?;
    let summary_embedding_blob: Vec<u8> = row.get(3).map_err(map_db)?;
    let centroid_embedding_blob: Vec<u8> = row.get(4).map_err(map_db)?;
    let children_json: String = row.get(5).map_err(map_db)?;
    let direct_members_json: Option<String> = row.get(6).map_err(map_db)?;
    let evidence_json: String = row.get(7).map_err(map_db)?;
    let quotes_json: String = row.get(8).map_err(map_db)?;
    let entities_json: String = row.get(9).map_err(map_db)?;
    let cluster_coherence: f64 = row.get(10).map_err(map_db)?;
    let created_at_unix: i64 = row.get(11).map_err(map_db)?;
    let prompt_version: String = row.get(12).map_err(map_db)?;
    let summarizer_model: String = row.get(13).map_err(map_db)?;

    let children_node_ids: Vec<String> = serde_json::from_str(&children_json)
        .map_err(|e| Error::Storage(format!("raptor_nodes.children_node_ids: {e}")))?;
    let direct_member_chunk_ids: Vec<u32> = match direct_members_json {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| Error::Storage(format!("raptor_nodes.direct_member_chunk_ids: {e}")))?,
        None => Vec::new(),
    };
    let evidence_chunk_ids: Vec<u32> = serde_json::from_str(&evidence_json)
        .map_err(|e| Error::Storage(format!("raptor_nodes.evidence_chunk_ids: {e}")))?;
    let quote_spans: Vec<QuoteSpan> = serde_json::from_str(&quotes_json)
        .map_err(|e| Error::Storage(format!("raptor_nodes.quote_spans: {e}")))?;
    let primary_entities: Vec<String> = serde_json::from_str(&entities_json)
        .map_err(|e| Error::Storage(format!("raptor_nodes.primary_entities: {e}")))?;

    let created_at = chrono::DateTime::<chrono::Utc>::from_timestamp(created_at_unix, 0)
        .unwrap_or_else(chrono::Utc::now);

    Ok(RaptorNode {
        node_id,
        level: level as u8,
        summary,
        summary_embedding: decode_f32_vec(&summary_embedding_blob),
        centroid_embedding: decode_f32_vec(&centroid_embedding_blob),
        children_node_ids,
        direct_member_chunk_ids,
        evidence_chunk_ids,
        quote_spans,
        primary_entities,
        cluster_coherence: cluster_coherence as f32,
        created_at,
        prompt_version,
        summarizer_model,
    })
}

fn row_to_document_asset(row: &rusqlite::Row) -> DocumentAsset {
    let state_json: String = row.get(9).unwrap_or_else(|_| r#""Pending""#.to_string());
    let skeleton_json: Option<String> = row.get(10).ok().flatten();
    let doc_type_str: String = row.get(6).unwrap_or_else(|_| r#""Unknown""#.to_string());
    let ingested_ts: i64 = row.get(7).unwrap_or(0);

    DocumentAsset {
        id: row.get(0).unwrap_or_default(),
        title: row.get(1).unwrap_or_default(),
        filename: row.get(2).unwrap_or_default(),
        file_size_mb: row.get(3).unwrap_or(0.0),
        word_count: row.get::<_, i64>(4).unwrap_or(0) as usize,
        chunk_count: row.get::<_, i64>(5).unwrap_or(0) as usize,
        document_type: serde_json::from_str(&doc_type_str).unwrap_or(DocumentTypeTag::Unknown),
        ingested_at: chrono::DateTime::from_timestamp(ingested_ts, 0)
            .unwrap_or_else(chrono::Utc::now),
        index_id: row.get(8).unwrap_or_default(),
        skeleton: skeleton_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        state: serde_json::from_str(&state_json).unwrap_or(AssetState::Pending),
        owner: row.get::<_, Option<String>>(11).unwrap_or(None),
    }
}
