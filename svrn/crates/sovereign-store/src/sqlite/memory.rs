// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MemoryStore` impl — memories, FTS + embedding retrieval,
//! compaction fields, and the mem-RAPTOR node upsert.

use super::*;

/// Column list used by every `memories` SELECT that needs the full
/// Memory shape (including compaction fields). Kept as a constant so
/// the row-reading helper and the SQL strings stay in lockstep — if
/// you add a column to the projection, update [`row_to_memory_full`]
/// in the same edit.
const MEMORY_FULL_COLUMNS: &str = "id, content, source, confidence, created_at, last_used, \
     source_conversation_id, source_skill_id, \
     kind, source_memory_ids, superseded_by, \
     embedding, embedding_model";

/// Read a row produced by a SELECT whose projection matches
/// [`MEMORY_FULL_COLUMNS`] (13 columns) into a `Memory`. Honors the
/// compaction-fields defaults (Raw / empty / None) when the row
/// predates the compaction migration — sqlite returns NULL for those
/// columns on unmigrated rows; `Option::get` collapses NULL to None
/// and we coerce to the documented defaults below. Same for the T1
/// embedding pair: NULL blob → `None`, and recall lazy-backfills.
fn row_to_memory_full(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    let kind_str: Option<String> = row.get(8)?;
    let kind = match kind_str.as_deref() {
        Some("summary") => sovereign_core::types::MemoryKind::Summary,
        _ => sovereign_core::types::MemoryKind::Raw,
    };
    let source_memory_ids_json: Option<String> = row.get(9)?;
    let source_memory_ids: Vec<String> = source_memory_ids_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let embedding_blob: Option<Vec<u8>> = row.get(11)?;
    Ok(Memory {
        id: row.get(0)?,
        content: row.get(1)?,
        source: row.get(2)?,
        confidence: row.get(3)?,
        created_at: row.get(4)?,
        last_used: row.get(5)?,
        version: 0,
        deleted_at: None,
        source_conversation_id: row.get(6)?,
        source_skill_id: row.get(7)?,
        kind,
        source_memory_ids,
        superseded_by: row.get(10)?,
        embedding: embedding_blob.as_deref().map(decode_f32_vec),
        embedding_model: row.get(12)?,
    })
}

#[async_trait]
impl MemoryStore for SqliteStateStore {
    async fn save_memory(&self, memory: &Memory) -> Result<()> {
        {
            let conn = self.conn.lock().await;
            let kind_str = match memory.kind {
                sovereign_core::types::MemoryKind::Raw => "raw",
                sovereign_core::types::MemoryKind::Summary => "summary",
            };
            let source_memory_ids_json =
                serde_json::to_string(&memory.source_memory_ids).unwrap_or_else(|_| "[]".into());
            let embedding_blob: Option<Vec<u8>> = memory.embedding.as_deref().map(encode_f32_vec);
            conn.execute(
                "INSERT OR REPLACE INTO memories
                   (id, content, source, confidence, created_at, last_used,
                    source_conversation_id, source_skill_id,
                    kind, source_memory_ids, superseded_by,
                    embedding, embedding_model)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    memory.id,
                    memory.content,
                    memory.source,
                    memory.confidence,
                    memory.created_at,
                    memory.last_used,
                    memory.source_conversation_id,
                    memory.source_skill_id,
                    kind_str,
                    source_memory_ids_json,
                    memory.superseded_by,
                    embedding_blob,
                    memory.embedding_model,
                ],
            )
            .map_err(map_db)?;
        }
        // Post-commit observer notification. Lock dropped above so the
        // observer cannot deadlock on a store read from inside its handler.
        self.fire_observer(|o| o.on_memory_written(&memory.id));
        Ok(())
    }

    async fn get_relevant_memories(&self, context: &str, limit: usize) -> Result<Vec<Memory>> {
        if context.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.conn.lock().await;
        let current_time = now();

        let fts_context = sanitize_fts5_query(context);
        if fts_context.is_empty() {
            return Ok(Vec::new());
        }
        tracing::debug!(
            input_chars = context.len(),
            fts_query = %fts_context,
            "memory:fts_match query"
        );

        let sql = format!(
            "SELECT {cols} \
             FROM memories m \
             JOIN memories_fts fts ON m.rowid = fts.rowid \
             WHERE memories_fts MATCH ?1 \
               AND m.deleted_at IS NULL \
               AND m.superseded_by IS NULL \
             LIMIT ?2",
            cols = MEMORY_FULL_COLUMNS
                .split(", ")
                .map(|c| format!("m.{c}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;

        let memories: Vec<Memory> = stmt
            .query_map(
                rusqlite::params![fts_context, (limit * 3) as i64],
                row_to_memory_full,
            )
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_default();

        // Apply confidence decay and filter.
        let mut scored: Vec<(f64, Memory)> = memories
            .into_iter()
            .filter_map(|m| {
                let months = (current_time - m.last_used) as f64 / (30.0 * 86400.0);
                let decayed = m.confidence * 0.9_f64.powf(months);
                if decayed >= 0.2 {
                    Some((decayed, m))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // Touch returned memories.
        for (_, mem) in &scored {
            let _ = conn.execute(
                "UPDATE memories SET last_used = ?2 WHERE id = ?1",
                rusqlite::params![mem.id, current_time],
            );
        }

        Ok(scored.into_iter().map(|(_, m)| m).collect())
    }

    async fn get_all_memories_for_scope(
        &self,
        scope: &sovereign_core::MemoryScope,
    ) -> Result<Vec<Memory>> {
        let conn = self.conn.lock().await;
        // Filter at the SQL layer — the inner-work wall is a privacy
        // contract; in-process filtering would still load scoped rows
        // through the observer hooks and any future replication
        // transport. This route ensures we never even read scoped
        // bytes when serving a general query.
        let (where_clause, scope_param): (&str, Option<String>) = match scope {
            sovereign_core::MemoryScope::General => (
                "WHERE deleted_at IS NULL \
                   AND superseded_by IS NULL \
                   AND source_skill_id IS NULL",
                None,
            ),
            sovereign_core::MemoryScope::Scoped(id) => (
                "WHERE deleted_at IS NULL \
                   AND superseded_by IS NULL \
                   AND source_skill_id = ?1",
                Some(id.clone()),
            ),
        };
        let sql = format!(
            "SELECT {cols} FROM memories {where_clause}",
            cols = MEMORY_FULL_COLUMNS,
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let memories: Vec<Memory> = if let Some(id) = scope_param {
            stmt.query_map(rusqlite::params![id], row_to_memory_full)
                .map_err(map_db)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_db)?
        } else {
            stmt.query_map([], row_to_memory_full)
                .map_err(map_db)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_db)?
        };
        Ok(memories)
    }

    async fn get_relevant_memories_for_scope(
        &self,
        scope: &sovereign_core::MemoryScope,
        context_query: &str,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        if context_query.is_empty() {
            return Ok(Vec::new());
        }
        let fts_context = sanitize_fts5_query(context_query);
        if fts_context.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().await;
        let current_time = now();

        // Same SQL-level wall as get_all_memories_for_scope — the FTS
        // path has to honor the same invariant or scoped memories
        // could leak through the keyword fallback.
        let (scope_clause, scope_param): (&str, Option<String>) = match scope {
            sovereign_core::MemoryScope::General => ("AND m.source_skill_id IS NULL", None),
            sovereign_core::MemoryScope::Scoped(id) => {
                ("AND m.source_skill_id = ?3", Some(id.clone()))
            }
        };
        let cols = MEMORY_FULL_COLUMNS
            .split(", ")
            .map(|c| format!("m.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {cols} \
             FROM memories m \
             JOIN memories_fts fts ON m.rowid = fts.rowid \
             WHERE memories_fts MATCH ?1 \
               AND m.deleted_at IS NULL \
               AND m.superseded_by IS NULL \
               {scope_clause} \
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let raw: Vec<Memory> = if let Some(id) = scope_param {
            stmt.query_map(
                rusqlite::params![fts_context, (limit * 3) as i64, id],
                row_to_memory_full,
            )
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_default()
        } else {
            stmt.query_map(
                rusqlite::params![fts_context, (limit * 3) as i64],
                row_to_memory_full,
            )
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_default()
        };

        // Same confidence-decay floor + last-used touch as the
        // unscoped path so callers can swap freely.
        let mut scored: Vec<(f64, Memory)> = raw
            .into_iter()
            .filter_map(|m| {
                let months = (current_time - m.last_used) as f64 / (30.0 * 86400.0);
                let decayed = m.confidence * 0.9_f64.powf(months);
                if decayed >= 0.2 {
                    Some((decayed, m))
                } else {
                    None
                }
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        for (_, mem) in &scored {
            let _ = conn.execute(
                "UPDATE memories SET last_used = ?2 WHERE id = ?1",
                rusqlite::params![mem.id, current_time],
            );
        }
        Ok(scored.into_iter().map(|(_, m)| m).collect())
    }

    async fn get_all_memories(&self) -> Result<Vec<Memory>> {
        let conn = self.conn.lock().await;
        let sql = format!(
            "SELECT {cols} \
             FROM memories \
             WHERE deleted_at IS NULL AND superseded_by IS NULL",
            cols = MEMORY_FULL_COLUMNS,
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;

        let memories: Vec<Memory> = stmt
            .query_map([], row_to_memory_full)
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(memories)
    }

    async fn delete_memory(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let ts = now();
        conn.execute(
            "UPDATE memories SET deleted_at = ?2, version = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id, ts],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn update_memory_confidence(&self, id: &str, confidence: f64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE memories SET confidence = ?2 WHERE id = ?1",
            rusqlite::params![id, confidence],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn update_memory_embedding(
        &self,
        id: &str,
        embedding: &[f32],
        model: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE memories SET embedding = ?2, embedding_model = ?3 WHERE id = ?1",
            rusqlite::params![id, encode_f32_vec(embedding), model],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn save_mem_raptor_nodes(
        &self,
        scope_key: &str,
        nodes: &[MemRaptorNodeRow],
    ) -> Result<()> {
        // Atomic replace — mirrors `save_conv_raptor_nodes` so a
        // partial builder crash never leaves a half-built tree.
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM mem_raptor_nodes WHERE scope = ?1",
            rusqlite::params![scope_key],
        )
        .map_err(map_db)?;
        for node in nodes {
            exec_upsert_mem_raptor_node(&tx, node).map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    async fn upsert_mem_raptor_node(&self, node: &MemRaptorNodeRow) -> Result<()> {
        let conn = self.conn.lock().await;
        exec_upsert_mem_raptor_node(&conn, node).map_err(map_db)?;
        Ok(())
    }

    async fn delete_mem_raptor_node(&self, node_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM mem_raptor_nodes WHERE node_id = ?1",
            rusqlite::params![node_id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn list_mem_raptor_nodes(&self, scope_key: &str) -> Result<Vec<MemRaptorNodeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, scope, level, summary,
                        summary_embedding, centroid_embedding,
                        children_node_ids, direct_member_memory_ids,
                        evidence_memory_ids, primary_entities,
                        cluster_coherence, embedding_model, created_at,
                        parent_node_id, cf_n, cf_ls, cf_ss,
                        ph_mean, ph_cum, ph_min, n_since_summary,
                        radius_at_summary
                 FROM mem_raptor_nodes
                 WHERE scope = ?1
                 ORDER BY level DESC, node_id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![scope_key], |r| {
                let parse = |s: String| serde_json::from_str::<Vec<String>>(&s).unwrap_or_default();
                let cf_ls_blob: Option<Vec<u8>> = r.get(15)?;
                Ok(MemRaptorNodeRow {
                    node_id: r.get(0)?,
                    scope: r.get(1)?,
                    level: r.get::<_, i64>(2)? as u8,
                    summary: r.get(3)?,
                    summary_embedding: decode_f32_vec(r.get::<_, Vec<u8>>(4)?.as_slice()),
                    centroid_embedding: decode_f32_vec(r.get::<_, Vec<u8>>(5)?.as_slice()),
                    children_node_ids: parse(r.get(6)?),
                    direct_member_memory_ids: parse(r.get(7)?),
                    evidence_memory_ids: parse(r.get(8)?),
                    primary_entities: parse(r.get(9)?),
                    cluster_coherence: r.get::<_, f64>(10)? as f32,
                    embedding_model: r.get(11)?,
                    created_at: r.get(12)?,
                    parent_node_id: r.get(13)?,
                    cf_n: r.get(14)?,
                    cf_ls: cf_ls_blob
                        .as_deref()
                        .map(decode_f32_vec)
                        .unwrap_or_default(),
                    cf_ss: r.get(16)?,
                    ph_mean: r.get(17)?,
                    ph_cum: r.get(18)?,
                    ph_min: r.get(19)?,
                    n_since_summary: r.get(20)?,
                    radius_at_summary: r.get::<_, f64>(21)? as f32,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    async fn delete_mem_raptor_nodes_for_scope(&self, scope_key: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM mem_raptor_nodes WHERE scope = ?1",
            rusqlite::params![scope_key],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn touch_memory(&self, id: &str, timestamp: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE memories SET last_used = ?2 WHERE id = ?1",
            rusqlite::params![id, timestamp],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn list_memories_for_conversation(&self, conversation_id: &str) -> Result<Vec<Memory>> {
        let conn = self.conn.lock().await;
        let sql = format!(
            "SELECT {cols} \
             FROM memories \
             WHERE source_conversation_id = ?1 \
               AND deleted_at IS NULL \
               AND superseded_by IS NULL \
             ORDER BY created_at ASC",
            cols = MEMORY_FULL_COLUMNS,
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let memories: Vec<Memory> = stmt
            .query_map(rusqlite::params![conversation_id], row_to_memory_full)
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;
        Ok(memories)
    }

    async fn mark_superseded(&self, memory_id: &str, summary_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE memories SET superseded_by = ?2 WHERE id = ?1",
            rusqlite::params![memory_id, summary_id],
        )
        .map_err(map_db)?;
        Ok(())
    }
}

/// Single-row REPLACE for `mem_raptor_nodes` — shared by the batch
/// save (inside its transaction) and the incremental single-node
/// upsert, so the column list exists exactly once.
fn exec_upsert_mem_raptor_node(
    conn: &rusqlite::Connection,
    node: &MemRaptorNodeRow,
) -> rusqlite::Result<usize> {
    let json = |v: &Vec<String>| serde_json::to_string(v).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT OR REPLACE INTO mem_raptor_nodes
            (node_id, scope, level, summary,
             summary_embedding, centroid_embedding,
             children_node_ids, direct_member_memory_ids,
             evidence_memory_ids, primary_entities,
             cluster_coherence, embedding_model, created_at,
             parent_node_id, cf_n, cf_ls, cf_ss,
             ph_mean, ph_cum, ph_min, n_since_summary,
             radius_at_summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
        rusqlite::params![
            node.node_id,
            node.scope,
            node.level as i64,
            node.summary,
            encode_f32_vec(&node.summary_embedding),
            encode_f32_vec(&node.centroid_embedding),
            json(&node.children_node_ids),
            json(&node.direct_member_memory_ids),
            json(&node.evidence_memory_ids),
            json(&node.primary_entities),
            node.cluster_coherence as f64,
            node.embedding_model,
            node.created_at,
            node.parent_node_id,
            node.cf_n,
            if node.cf_ls.is_empty() {
                None
            } else {
                Some(encode_f32_vec(&node.cf_ls))
            },
            node.cf_ss,
            node.ph_mean,
            node.ph_cum,
            node.ph_min,
            node.n_since_summary,
            node.radius_at_summary as f64,
        ],
    )
}
