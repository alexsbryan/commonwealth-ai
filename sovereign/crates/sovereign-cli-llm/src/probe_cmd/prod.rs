// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn __probe` prod: every question drives the PRODUCTION KnowledgeQuery
//! retrieval pipeline in-process (`Runtime::retrieve_evidence` — context
//! build → the 19-step `kq_pipeline()` → merge → truncate) and the pool it
//! returns is written out unscored. No synthesis pass.

use sovereign_contracts::probe::{PoolEvidence, ProbeQuestion};

use super::pool_chunk;
use crate::chat_cmd::bootstrap::ChatSession;

/// Retrieve every question's pool in order. `isolate_corpora` scopes each
/// question's conversation; `limit` caps the pool.
pub(crate) async fn probe(
    session: &ChatSession,
    questions: &[ProbeQuestion],
    limit: usize,
    isolate_corpora: Option<&[String]>,
) -> Vec<PoolEvidence> {
    let mut rows = Vec::with_capacity(questions.len());
    for q in questions {
        rows.push(probe_question(session, q, limit, isolate_corpora).await);
    }
    rows
}

async fn probe_question(
    session: &ChatSession,
    q: &ProbeQuestion,
    limit: usize,
    isolate_corpora: Option<&[String]>,
) -> PoolEvidence {
    // Fresh conversation per question — same seeding pattern as the synth
    // path so `build_context` + the personal-scope filter see a real row,
    // and isolation scopes retrieval via `enabled_corpora`.
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let created_at = sovereign_core::time::unix_now();
    if let Err(e) = session
        .store
        .insert_empty_conversation(&conversation_id, created_at, None)
        .await
    {
        eprintln!(
            "  warn: prod-pipeline seed (insert) failed for {}: {e}",
            q.id
        );
    } else if let Some(corpora) = isolate_corpora {
        if let Err(e) = session
            .store
            .set_conversation_enabled_corpora(&conversation_id, Some(corpora.to_vec()))
            .await
        {
            eprintln!(
                "  warn: prod-pipeline seed (scope) failed for {}: {e}",
                q.id
            );
        }
    }

    let ev = match session
        .runtime
        .retrieve_evidence(&q.question, &conversation_id)
        .await
    {
        Ok(ev) => ev,
        Err(e) => {
            return PoolEvidence {
                id: q.id.clone(),
                error: Some(format!("retrieve_evidence: {e}")),
                chunks: Vec::new(),
                atlas_navigation: Vec::new(),
                embed_ms: 0,
                search_ms: 0,
                corpora_hit: Vec::new(),
                vector_eligible: true,
                unavailable_corpora: Vec::new(),
                atlas_walk: None,
            };
        }
    };

    // A walk echo that will not serialise errors the row, never drops into a
    // `None` that reads as "the walk did not run".
    let (atlas_walk, error) = match ev.atlas_walk.as_ref().map(serde_json::to_value) {
        None => (None, None),
        Some(Ok(v)) => (Some(v), None),
        Some(Err(e)) => (None, Some(format!("atlas walk echo: {e}"))),
    };
    let mut all_hits = ev.chunks;
    if all_hits.len() > limit {
        all_hits.truncate(limit);
    }
    let corpora_hit: Vec<String> = {
        let mut s: Vec<String> = all_hits.iter().map(|c| c.corpus_id.clone()).collect();
        s.sort();
        s.dedup();
        s
    };

    PoolEvidence {
        id: q.id.clone(),
        error,
        chunks: all_hits.iter().map(pool_chunk).collect(),
        atlas_navigation: Vec::new(),
        embed_ms: 0,
        search_ms: ev.search_ms,
        corpora_hit,
        vector_eligible: true,
        unavailable_corpora: ev.unavailable_corpora,
        atlas_walk,
    }
}
