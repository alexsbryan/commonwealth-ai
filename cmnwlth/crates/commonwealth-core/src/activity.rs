// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local Activity ledger — the glassbox record of what *this*
//! daemon did, in Sovereign's own vocabulary (tokens, embeddings,
//! chunks, queries, fetches).
//!
//! ## Why this is a sibling of, not part of, [`crate::contributions`]
//!
//! The contribution ledger answers "what did I provide *to the
//! mesh*?" — it is dimensional, gossip-replicated, and every variant
//! is a directed peer exchange. This module answers a different
//! question: "what is my daemon *doing*, and what resources is it
//! using — even as a mesh of one?" That covers heavy local work that
//! never crosses a peer boundary (ingesting and enriching an Obsidian
//! vault embeds thousands of chunks; a newsworthy tick fetches
//! articles) and so would never appear in the contribution ledger.
//!
//! Two structural consequences follow, and they are *why* this is a
//! separate type rather than extra `LedgerEventKind` variants:
//!
//! 1. **Local-first sovereignty.** Activity is the user's own usage.
//!    It is persisted under the `activity-private` namespace, which
//!    [`crate`'s sibling `commonwealth-state`] excludes from gossip
//!    structurally (see `peer_preferences::GOSSIP_EXCLUDED_APP_IDS`).
//!    Your token counts are yours; they never ride the wire.
//! 2. **Different unit of aggregation.** Contribution rolls up into
//!    per-*peer* `NodeContributions`. Activity rolls up into a single
//!    self-view [`ActivitySummary`] — per-corpus and per-dimension,
//!    not per-peer.
//!
//! Like the contribution ledger, aggregation here is a **pure
//! function** over an append-only event stream
//! ([`aggregate_activity`]): same events, same summary, every time.

use crate::ids::NodeId;

// The records live in `oicp_types::activity` since pb-mesh-exit-core; the
// aggregation below is the decision and stays. Re-exported so every path
// resolves.
pub use oicp_types::activity::*;

/// Collapse an append-only activity stream into a single
/// [`ActivitySummary`]. Pure function — same events, same summary.
///
/// `now_unix` is the upper bound; events older than `window_secs`
/// before it are dropped. There is no incremental state: rolling the
/// window forward simply re-runs this.
pub fn aggregate_activity(
    events: &[ActivityEvent],
    now_unix: u64,
    window_secs: u64,
) -> ActivitySummary {
    let cutoff = now_unix.saturating_sub(window_secs);
    let window_days = (window_secs / 86_400).max(1) as u32;
    let mut summary = ActivitySummary {
        window_days,
        ..Default::default()
    };

    // Helper: find-or-create the per-corpus bucket. Kept inline (not a
    // closure capturing `summary`) to avoid a borrow tangle.
    fn corpus_bucket<'a>(
        corpora: &'a mut Vec<CorpusActivity>,
        corpus_id: &str,
    ) -> &'a mut CorpusActivity {
        if let Some(pos) = corpora.iter().position(|c| c.corpus_id == corpus_id) {
            &mut corpora[pos]
        } else {
            corpora.push(CorpusActivity {
                corpus_id: corpus_id.to_string(),
                ..Default::default()
            });
            corpora.last_mut().unwrap()
        }
    }

    for ev in events.iter().filter(|e| e.timestamp >= cutoff) {
        match &ev.kind {
            ActivityEventKind::LocalInferenceServed {
                completion_tokens,
                wall_seconds,
                ..
            } => {
                summary.local_inference_requests += 1;
                summary.local_tokens_generated += completion_tokens;
                summary.local_inference_wall_seconds += wall_seconds;
            }
            ActivityEventKind::EmbeddingsServed {
                served_for,
                n_texts,
                ..
            } => {
                if served_for.is_peer() {
                    summary.embeddings.peer_requests += 1;
                    summary.embeddings.peer_units += n_texts;
                } else {
                    summary.embeddings.local_requests += 1;
                    summary.embeddings.local_units += n_texts;
                }
            }
            ActivityEventKind::LocalKnowledgeServed {
                chunks_returned, ..
            } => {
                summary.local_knowledge_queries += 1;
                summary.local_chunks_served += *chunks_returned as u64;
            }
            ActivityEventKind::ChunksIngested {
                corpus_id,
                chunks,
                duration_secs,
            } => {
                summary.total_chunks_ingested += chunks;
                let b = corpus_bucket(&mut summary.corpora, corpus_id);
                b.chunks_ingested += chunks;
                b.ingest_runs += 1;
                b.ingest_seconds += duration_secs;
            }
            ActivityEventKind::CorpusEnriched {
                corpus_id,
                atoms,
                duration_secs,
            } => {
                let b = corpus_bucket(&mut summary.corpora, corpus_id);
                b.enrich_runs += 1;
                b.enrich_atoms += atoms;
                b.enrich_seconds += duration_secs;
            }
            ActivityEventKind::NewsworthyFetched { articles, .. } => {
                summary.newsworthy_fetches += 1;
                summary.newsworthy_articles += articles;
            }
        }
    }

    // Stable order so the UI doesn't reshuffle corpus rows between
    // polls (HashMap iteration order would).
    summary
        .corpora
        .sort_by(|a, b| a.corpus_id.cmp(&b.corpus_id));
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(byte: u8) -> NodeId {
        NodeId::from_u128(byte as u128)
    }

    fn ev(ts: u64, kind: ActivityEventKind) -> ActivityEvent {
        ActivityEvent {
            node_id: nid(1),
            timestamp: ts,
            kind,
        }
    }

    #[test]
    fn empty_stream_is_zeroed_summary() {
        let s = aggregate_activity(&[], 1_000_000, 86_400);
        assert_eq!(s.local_inference_requests, 0);
        assert_eq!(s.total_chunks_ingested, 0);
        assert!(s.corpora.is_empty());
    }

    #[test]
    fn local_inference_tallies_completion_tokens_and_wall() {
        let now = 1_000_000;
        let events = vec![
            ev(
                now - 10,
                ActivityEventKind::LocalInferenceServed {
                    model_id: "qwen-9b".into(),
                    prompt_tokens: 500,
                    completion_tokens: 100,
                    wall_seconds: 2.0,
                },
            ),
            ev(
                now - 5,
                ActivityEventKind::LocalInferenceServed {
                    model_id: "qwen-9b".into(),
                    prompt_tokens: 300,
                    completion_tokens: 50,
                    wall_seconds: 1.0,
                },
            ),
        ];
        let s = aggregate_activity(&events, now, 86_400);
        assert_eq!(s.local_inference_requests, 2);
        assert_eq!(s.local_tokens_generated, 150);
        assert!((s.local_inference_wall_seconds - 3.0).abs() < 1e-6);
    }

    #[test]
    fn embeddings_split_peer_vs_local() {
        let now = 1_000_000;
        let events = vec![
            ev(
                now - 10,
                ActivityEventKind::EmbeddingsServed {
                    served_for: ServedFor::Peer { node_id: nid(2) },
                    n_texts: 64,
                    tokens: 4096,
                },
            ),
            ev(
                now - 5,
                ActivityEventKind::EmbeddingsServed {
                    served_for: ServedFor::Local,
                    n_texts: 8,
                    tokens: 512,
                },
            ),
        ];
        let s = aggregate_activity(&events, now, 86_400);
        assert_eq!(s.embeddings.peer_requests, 1);
        assert_eq!(s.embeddings.peer_units, 64);
        assert_eq!(s.embeddings.local_requests, 1);
        assert_eq!(s.embeddings.local_units, 8);
    }

    #[test]
    fn ingest_and_enrich_bucket_by_corpus() {
        let now = 1_000_000;
        let events = vec![
            ev(
                now - 30,
                ActivityEventKind::ChunksIngested {
                    corpus_id: "obsidian-vault".into(),
                    chunks: 3000,
                    duration_secs: 600,
                },
            ),
            ev(
                now - 20,
                ActivityEventKind::ChunksIngested {
                    corpus_id: "obsidian-vault".into(),
                    chunks: 21,
                    duration_secs: 10,
                },
            ),
            ev(
                now - 10,
                ActivityEventKind::CorpusEnriched {
                    corpus_id: "obsidian-vault".into(),
                    atoms: 450,
                    duration_secs: 900,
                },
            ),
        ];
        let s = aggregate_activity(&events, now, 86_400);
        assert_eq!(s.total_chunks_ingested, 3021);
        assert_eq!(s.corpora.len(), 1);
        let c = &s.corpora[0];
        assert_eq!(c.corpus_id, "obsidian-vault");
        assert_eq!(c.chunks_ingested, 3021);
        assert_eq!(c.ingest_runs, 2);
        assert_eq!(c.ingest_seconds, 610);
        assert_eq!(c.enrich_runs, 1);
        assert_eq!(c.enrich_atoms, 450);
        assert_eq!(c.enrich_seconds, 900);
    }

    #[test]
    fn events_outside_window_are_dropped() {
        let now = 1_000_000;
        let window = 86_400;
        let events = vec![
            ev(
                now - window - 1,
                ActivityEventKind::ChunksIngested {
                    corpus_id: "old".into(),
                    chunks: 9_999,
                    duration_secs: 1,
                },
            ),
            ev(
                now - 1,
                ActivityEventKind::ChunksIngested {
                    corpus_id: "fresh".into(),
                    chunks: 5,
                    duration_secs: 1,
                },
            ),
        ];
        let s = aggregate_activity(&events, now, window);
        assert_eq!(s.total_chunks_ingested, 5);
        assert_eq!(s.corpora.len(), 1);
        assert_eq!(s.corpora[0].corpus_id, "fresh");
    }

    #[test]
    fn aggregation_is_order_independent() {
        let now = 1_000_000;
        let events = vec![
            ev(
                now - 30,
                ActivityEventKind::NewsworthyFetched {
                    articles: 12,
                    portal_ingested: true,
                },
            ),
            ev(
                now - 20,
                ActivityEventKind::LocalKnowledgeServed {
                    corpus_id: "sep".into(),
                    chunks_returned: 8,
                },
            ),
            ev(
                now - 10,
                ActivityEventKind::ChunksIngested {
                    corpus_id: "z".into(),
                    chunks: 1,
                    duration_secs: 1,
                },
            ),
        ];
        let mut rev = events.clone();
        rev.reverse();
        assert_eq!(
            aggregate_activity(&events, now, 86_400),
            aggregate_activity(&rev, now, 86_400)
        );
    }
}
