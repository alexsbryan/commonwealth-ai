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

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// One discrete unit of local daemon work. Append-only; the `node_id`
/// is always *this* node (activity is never about a peer's work — a
/// peer's work for us is `contributions`, and our work is local).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActivityEvent {
    /// Origin: always the local node. Carried for symmetry with
    /// `LedgerEvent` and so the on-disk record is self-describing.
    pub node_id: NodeId,
    /// Unix seconds when the work completed.
    pub timestamp: u64,
    pub kind: ActivityEventKind,
}

/// Who a served unit of work was for. Embeddings and inference served
/// over the daemon's HTTP surface can be driven either by a mesh peer
/// (a node with no embed model of its own) or by a local OpenAI-API
/// client (a CLI tool, an editor plugin). The split matters in the UI:
/// "served to the mesh" vs "served to you, locally."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "actor", rename_all = "snake_case")]
pub enum ServedFor {
    /// A local OpenAI-API client on this machine (no `X-Node-Id`).
    Local,
    /// A mesh peer, identified by node id.
    Peer { node_id: NodeId },
}

impl ServedFor {
    pub fn is_peer(&self) -> bool {
        matches!(self, ServedFor::Peer { .. })
    }
}

/// The closed set of local-activity dimensions. Per ARCH_PRINCIPLES
/// §2.1 each variant is a genuinely distinct kind of work; do not
/// overload one by stuffing a new payload into it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ActivityEventKind {
    /// The daemon served an inference completion to a *local* API
    /// client (requester had no `X-Node-Id`). Peer-served inference
    /// is the contribution ledger's `InferenceServed`; this is the
    /// local counterpart the contribution gate drops.
    LocalInferenceServed {
        model_id: String,
        prompt_tokens: u64,
        completion_tokens: u64,
        wall_seconds: f64,
    },
    /// The daemon served embeddings over `/v1/embeddings`. Recorded
    /// for both peer and local callers (`served_for`). This is the
    /// dimension that was previously invisible: a peer with no embed
    /// model drives this and nothing recorded it.
    EmbeddingsServed {
        served_for: ServedFor,
        /// Number of input texts embedded in the request.
        n_texts: u64,
        /// Approximate input tokens (the embeddings handler's
        /// `Usage.prompt_tokens`).
        tokens: u64,
    },
    /// The daemon answered a knowledge query for a *local* API client.
    /// Peer-served knowledge is the contribution ledger's
    /// `KnowledgeQueryServed`; this is the local counterpart.
    LocalKnowledgeServed {
        corpus_id: String,
        chunks_returned: u32,
    },
    /// A corpus ingest completed on this machine — `chunks` chunks
    /// were extracted, embedded, and indexed. This is the headline
    /// "your Obsidian import did real work" signal.
    ChunksIngested {
        corpus_id: String,
        chunks: u64,
        duration_secs: u64,
    },
    /// A corpus enrichment pass completed (atlas / RAPTOR / atom
    /// extraction). Heavy local inference work, distinct from the
    /// raw ingest embed pass above.
    CorpusEnriched {
        corpus_id: String,
        /// Atoms / nodes produced, when the pipeline reports it.
        atoms: u64,
        duration_secs: u64,
    },
    /// A wikipedia-newsworthy freshness tick fetched articles.
    NewsworthyFetched {
        articles: u64,
        portal_ingested: bool,
    },
}

/// A served-work tally split by who it was for (mesh peer vs local
/// client). Three orthogonal counts so the UI can say "N requests, M
/// texts, K tokens" without inferring one from another.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ServedTally {
    pub local_requests: u64,
    pub peer_requests: u64,
    /// Unit count (tokens for inference, texts for embeddings).
    pub local_units: u64,
    pub peer_units: u64,
}

/// Per-corpus local ingest + enrich activity within the window.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CorpusActivity {
    pub corpus_id: String,
    pub chunks_ingested: u64,
    pub ingest_runs: u64,
    pub ingest_seconds: u64,
    pub enrich_runs: u64,
    pub enrich_atoms: u64,
    pub enrich_seconds: u64,
}

/// The single self-view rollup of local activity over a window. This
/// is the activity counterpart to `NodeContributions`, but folded to
/// one node (always this one) and organised by dimension rather than
/// by peer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ActivitySummary {
    pub window_days: u32,
    // Inference the daemon served to local API clients.
    pub local_inference_requests: u64,
    pub local_tokens_generated: u64,
    pub local_inference_wall_seconds: f64,
    // Embeddings served over /v1/embeddings (peer + local).
    pub embeddings: ServedTally,
    // Knowledge served to local API clients.
    pub local_knowledge_queries: u64,
    pub local_chunks_served: u64,
    // Per-corpus ingest + enrichment work done on this machine.
    pub corpora: Vec<CorpusActivity>,
    pub total_chunks_ingested: u64,
    // Newsworthy freshness fetches.
    pub newsworthy_fetches: u64,
    pub newsworthy_articles: u64,
}

/// Default activity window. Shorter than the contribution ledger's 30
/// days — "what has my daemon been up to lately" is a more
/// immediate question than peer-fairness accounting, and a 7-day
/// window keeps the totals legible.
pub const DEFAULT_ACTIVITY_WINDOW_DAYS: u32 = 7;

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
