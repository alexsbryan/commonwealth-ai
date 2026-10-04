// SPDX-License-Identifier: AGPL-3.0-or-later
//! Chat-activity rollup — `summarize_chat_activity`, the read-side
//! aggregation over persisted `ResponseProvenance` message metadata.

use super::*;

impl SqliteStateStore {
    /// Aggregate the user's own chat usage over the last `window_secs`.
    /// Reads `metadata["provenance"]` off every assistant message in
    /// the window and totals tokens generated, chunks retrieved (per
    /// corpus, local vs peer), turns, and per-model usage. Messages
    /// that predate provenance, or whose metadata fails to parse, are
    /// skipped — a best-effort read, never an error for the caller.
    pub async fn summarize_chat_activity(&self, window_secs: i64) -> Result<ChatActivitySummary> {
        use std::collections::BTreeMap;

        let cutoff = now().saturating_sub(window_secs);
        let window_days = (window_secs / 86_400).max(1) as u32;

        // Accumulate into ordered maps so the materialized Vecs are
        // stable between polls (a HashMap would reshuffle the UI rows).
        let mut corpus_local: BTreeMap<String, u64> = BTreeMap::new();
        let mut corpus_peer: BTreeMap<String, u64> = BTreeMap::new();
        let mut models: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        let mut turns: u64 = 0;
        let mut tokens_generated: u64 = 0;
        let mut chunks_retrieved: u64 = 0;

        {
            let conn = self.conn.lock().await;
            let mut stmt = conn
                .prepare(
                    "SELECT metadata FROM messages
                     WHERE role = 'assistant' AND created_at >= ?1
                       AND metadata IS NOT NULL",
                )
                .map_err(map_db)?;
            let rows = stmt
                .query_map(rusqlite::params![cutoff], |row| {
                    row.get::<_, Option<String>>(0)
                })
                .map_err(map_db)?;

            // Collect provenance inside the locked scope (per the
            // MappedRows-lifetime invariant: never return the iterator
            // across the conn/stmt block boundary).
            for meta in rows.flatten() {
                let Some(meta) = meta else { continue };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&meta) else {
                    continue;
                };
                // Provenance is nested under `metadata["provenance"]`
                // (see the runtime handlers that build message metadata).
                let Some(prov_value) = value.get("provenance") else {
                    continue;
                };
                let Ok(prov) = serde_json::from_value::<ResponseProvenance>(prov_value.clone())
                else {
                    continue;
                };

                turns += 1;
                // Prefer the explicit completion-token count; fall back
                // to total `tokens_used` for messages that lack it.
                let gen = prov
                    .completion_tokens
                    .map(|t| t as u64)
                    .unwrap_or(prov.tokens_used as u64);
                tokens_generated += gen;

                let m = models.entry(prov.inference_backend.clone()).or_default();
                m.0 += 1;
                m.1 += gen;

                for s in &prov.sources {
                    chunks_retrieved += s.count as u64;
                    let bucket = if s.from_peer.is_some() {
                        &mut corpus_peer
                    } else {
                        &mut corpus_local
                    };
                    *bucket.entry(s.origin.clone()).or_insert(0) += s.count as u64;
                }
            }
        }

        let mut by_corpus: Vec<ChatCorpusUsage> = Vec::new();
        for (origin, chunks) in corpus_local {
            by_corpus.push(ChatCorpusUsage {
                origin,
                chunks,
                from_peer: false,
            });
        }
        for (origin, chunks) in corpus_peer {
            by_corpus.push(ChatCorpusUsage {
                origin,
                chunks,
                from_peer: true,
            });
        }
        let by_model = models
            .into_iter()
            .map(|(model, (turns, tokens_generated))| ChatModelUsage {
                model,
                turns,
                tokens_generated,
            })
            .collect();

        Ok(ChatActivitySummary {
            window_days,
            turns,
            tokens_generated,
            chunks_retrieved,
            by_corpus,
            by_model,
        })
    }
}
