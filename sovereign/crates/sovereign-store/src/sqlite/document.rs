// SPDX-License-Identifier: AGPL-3.0-or-later
//! `DocumentStore` impl — document chunks, FTS + embedding search.

use super::*;

#[async_trait]
impl DocumentStore for SqliteStateStore {
    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()> {
        let conn = self.conn.lock().await;
        for chunk in chunks {
            let embedding_blob = chunk
                .embedding
                .as_ref()
                .map(|v| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>());
            let (source_type_str, corpus_id) = chunk.source_type.to_db_columns();

            conn.execute(
                "INSERT OR REPLACE INTO documents (id, source, content, chunk_index, embedding, created_at, source_type, corpus_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    chunk.id,
                    chunk.source,
                    chunk.content,
                    chunk.chunk_index as i64,
                    embedding_blob,
                    chunk.created_at,
                    source_type_str,
                    corpus_id,
                ],
            )
            .map_err(map_db)?;
        }
        Ok(())
    }

    async fn search_documents(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<DocumentChunk>> {
        let conn = self.conn.lock().await;

        // Hybrid search: combine FTS5 text search with cosine similarity.
        // Collect results from both, deduplicate by ID, return top N.

        let mut results: std::collections::HashMap<String, (f32, DocumentChunk)> =
            std::collections::HashMap::new();

        // 1. FTS5 text search (always available, no embeddings needed).
        // Sanitize query into FTS5-safe keywords.
        let fts_query = sanitize_fts5_query(query_text);
        if !fts_query.is_empty() {
            let mut fts_stmt = conn
                .prepare(
                    "SELECT d.id, d.source, d.content, d.chunk_index, d.embedding, d.created_at, d.source_type, d.corpus_id
                     FROM documents d
                     JOIN documents_fts fts ON d.rowid = fts.rowid
                     WHERE documents_fts MATCH ?1 AND d.deleted_at IS NULL
                     LIMIT ?2",
                )
                .map_err(map_db)?;

            let fts_results: Vec<DocumentChunk> = fts_stmt
                .query_map(rusqlite::params![fts_query, (limit * 2) as i64], |row| {
                    let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                    let embedding = embedding_blob.map(|blob| {
                        blob.chunks(4)
                            .map(|c| {
                                let mut bytes = [0u8; 4];
                                bytes.copy_from_slice(c);
                                f32::from_le_bytes(bytes)
                            })
                            .collect::<Vec<f32>>()
                    });
                    let st: String = row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "user".to_string());
                    let cid: Option<String> = row.get(7)?;
                    Ok(DocumentChunk {
                        id: row.get(0)?,
                        source: row.get(1)?,
                        content: row.get(2)?,
                        chunk_index: row.get::<_, i64>(3)? as usize,
                        embedding,
                        created_at: row.get(5)?,
                        source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                        version: 0,
                        deleted_at: None,
                    })
                })
                .map_err(map_db)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap_or_default();

            for (i, chunk) in fts_results.into_iter().enumerate() {
                // FTS5 results get a score based on rank position (1.0 → 0.5).
                let score = 1.0 - (i as f32 * 0.05).min(0.5);
                results.insert(chunk.id.clone(), (score, chunk));
            }
        }

        // 2. Vector similarity search (if embeddings are available).
        if !query_embedding.is_empty() {
            let mut vec_stmt = conn
                .prepare(
                    "SELECT id, source, content, chunk_index, embedding, created_at, source_type, corpus_id
                     FROM documents WHERE embedding IS NOT NULL AND deleted_at IS NULL",
                )
                .map_err(map_db)?;

            let vector_results: Vec<(String, f32, DocumentChunk)> = vec_stmt
                .query_map([], |row| {
                    let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                    let embedding = embedding_blob.map(|blob| {
                        blob.chunks(4)
                            .map(|c| {
                                let mut bytes = [0u8; 4];
                                bytes.copy_from_slice(c);
                                f32::from_le_bytes(bytes)
                            })
                            .collect::<Vec<f32>>()
                    });
                    let id: String = row.get(0)?;
                    let st: String = row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "user".to_string());
                    let cid: Option<String> = row.get(7)?;
                    Ok((
                        id.clone(),
                        embedding.clone(),
                        DocumentChunk {
                            id,
                            source: row.get(1)?,
                            content: row.get(2)?,
                            chunk_index: row.get::<_, i64>(3)? as usize,
                            embedding,
                            created_at: row.get(5)?,
                            source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                            version: 0,
                            deleted_at: None,
                        },
                    ))
                })
                .map_err(map_db)?
                .filter_map(|r| r.ok())
                .filter_map(|(id, emb, chunk)| {
                    emb.map(|e| {
                        let sim = cosine_similarity(query_embedding, &e);
                        (id, sim, chunk)
                    })
                })
                .collect();

            for (id, sim, chunk) in vector_results {
                results
                    .entry(id)
                    .and_modify(|(score, _)| {
                        // Boost score if found by both methods.
                        *score = (*score + sim) / 2.0 + 0.1;
                    })
                    .or_insert((sim, chunk));
            }
        }

        // Sort by score descending, return top N.
        let mut sorted: Vec<(f32, DocumentChunk)> = results.into_values().collect();
        sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(sorted.into_iter().take(limit).map(|(_, c)| c).collect())
    }

    async fn search_documents_scored(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunk>> {
        // Reuse the same hybrid search logic but preserve scores.
        let conn = self.conn.lock().await;
        let mut results: std::collections::HashMap<String, (f32, DocumentChunk)> =
            std::collections::HashMap::new();

        let fts_query_scored = sanitize_fts5_query(query_text);
        if !fts_query_scored.is_empty() {
            let mut fts_stmt = conn
                .prepare(
                    "SELECT d.id, d.source, d.content, d.chunk_index, d.embedding, d.created_at, d.source_type, d.corpus_id
                     FROM documents d
                     JOIN documents_fts fts ON d.rowid = fts.rowid
                     WHERE documents_fts MATCH ?1 AND d.deleted_at IS NULL
                     LIMIT ?2",
                )
                .map_err(map_db)?;

            let fts_results: Vec<DocumentChunk> = fts_stmt
                .query_map(
                    rusqlite::params![fts_query_scored, (limit * 2) as i64],
                    |row| {
                        let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                        let embedding = embedding_blob.map(|blob| {
                            blob.chunks(4)
                                .map(|c| {
                                    let mut bytes = [0u8; 4];
                                    bytes.copy_from_slice(c);
                                    f32::from_le_bytes(bytes)
                                })
                                .collect::<Vec<f32>>()
                        });
                        let st: String = row
                            .get::<_, Option<String>>(6)?
                            .unwrap_or_else(|| "user".to_string());
                        let cid: Option<String> = row.get(7)?;
                        Ok(DocumentChunk {
                            id: row.get(0)?,
                            source: row.get(1)?,
                            content: row.get(2)?,
                            chunk_index: row.get::<_, i64>(3)? as usize,
                            embedding,
                            created_at: row.get(5)?,
                            source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                            version: 0,
                            deleted_at: None,
                        })
                    },
                )
                .map_err(map_db)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap_or_default();

            for (i, chunk) in fts_results.into_iter().enumerate() {
                let score = 1.0 - (i as f32 * 0.05).min(0.5);
                results.insert(chunk.id.clone(), (score, chunk));
            }
        }

        if !query_embedding.is_empty() {
            let mut vec_stmt = conn
                .prepare(
                    "SELECT id, source, content, chunk_index, embedding, created_at, source_type, corpus_id
                     FROM documents WHERE embedding IS NOT NULL AND deleted_at IS NULL",
                )
                .map_err(map_db)?;

            let vector_results: Vec<(String, f32, DocumentChunk)> = vec_stmt
                .query_map([], |row| {
                    let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                    let embedding = embedding_blob.map(|blob| {
                        blob.chunks(4)
                            .map(|c| {
                                let mut bytes = [0u8; 4];
                                bytes.copy_from_slice(c);
                                f32::from_le_bytes(bytes)
                            })
                            .collect::<Vec<f32>>()
                    });
                    let id: String = row.get(0)?;
                    let st: String = row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "user".to_string());
                    let cid: Option<String> = row.get(7)?;
                    Ok((
                        id.clone(),
                        embedding.clone(),
                        DocumentChunk {
                            id,
                            source: row.get(1)?,
                            content: row.get(2)?,
                            chunk_index: row.get::<_, i64>(3)? as usize,
                            embedding,
                            created_at: row.get(5)?,
                            source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                            version: 0,
                            deleted_at: None,
                        },
                    ))
                })
                .map_err(map_db)?
                .filter_map(|r| r.ok())
                .filter_map(|(id, emb, chunk)| {
                    emb.map(|e| {
                        let sim = cosine_similarity(query_embedding, &e);
                        (id, sim, chunk)
                    })
                })
                .collect();

            for (id, sim, chunk) in vector_results {
                results
                    .entry(id)
                    .and_modify(|(score, _)| {
                        *score = (*score + sim) / 2.0 + 0.1;
                    })
                    .or_insert((sim, chunk));
            }
        }

        let mut sorted: Vec<(f32, DocumentChunk)> = results.into_values().collect();
        sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(sorted
            .into_iter()
            .take(limit)
            .map(|(score, chunk)| ScoredChunk { chunk, score })
            .collect())
    }

    async fn get_chunks_by_source(&self, source: &str) -> Result<Vec<DocumentChunk>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, source, content, chunk_index, embedding, created_at, source_type, corpus_id
                 FROM documents WHERE source = ?1 AND deleted_at IS NULL ORDER BY chunk_index ASC",
            )
            .map_err(map_db)?;

        let chunks: Vec<DocumentChunk> = stmt
            .query_map(rusqlite::params![source], |row| {
                let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                let embedding = embedding_blob.map(|blob| {
                    blob.chunks(4)
                        .map(|c| {
                            let mut bytes = [0u8; 4];
                            bytes.copy_from_slice(c);
                            f32::from_le_bytes(bytes)
                        })
                        .collect::<Vec<f32>>()
                });
                let st: String = row
                    .get::<_, Option<String>>(6)?
                    .unwrap_or_else(|| "user".to_string());
                let cid: Option<String> = row.get(7)?;
                Ok(DocumentChunk {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    content: row.get(2)?,
                    chunk_index: row.get::<_, i64>(3)? as usize,
                    embedding,
                    created_at: row.get(5)?,
                    source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                    version: 0,
                    deleted_at: None,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(chunks)
    }

    async fn delete_chunks_by_corpus(&self, corpus_id: &str) -> Result<u64> {
        let conn = self.conn.lock().await;
        let ts = now();
        let count = conn
            .execute(
                "UPDATE documents SET deleted_at = ?2, version = ?2 WHERE corpus_id = ?1 AND deleted_at IS NULL",
                rusqlite::params![corpus_id, ts],
            )
            .map_err(map_db)?;
        Ok(count as u64)
    }

    async fn list_sources(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT source FROM documents WHERE deleted_at IS NULL ORDER BY source",
            )
            .map_err(map_db)?;

        let sources: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(sources)
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
