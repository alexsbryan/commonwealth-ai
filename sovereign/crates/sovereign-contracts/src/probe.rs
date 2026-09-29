// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's retrieval probe — what svrn says about its own internals, for a
//! bench to judge.
//!
//! `svrn __probe` runs ONE internal stage (the router's classifier,
//! the production retrieval pipeline, a raw index search, or an
//! attached-document turn) over plain question text and writes a
//! [`ProbeEvidence`]. It carries no bank, no
//! expectation and no score: svrn describes itself, bench owns banks and
//! verdicts (ARCH principle 12; phase-b-58). Both sides name this one type, so
//! the file between them has one shape (principle 8).

use serde::{Deserialize, Serialize};

use crate::traits::CorpusUnavailable;

mod attached;
mod resources;

pub use attached::{
    AttachedEvidence, AttachedProbe, AttachedSource, AttachedTurn, StateTransition,
};
pub use resources::{CallRecord, PhaseBucket, PhaseResources, ResourceReport};

/// Which internal stage a probe runs. A closed set: the spelling on the
/// command line is [`ProbeMode::as_str`] on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeMode {
    /// The router's classifier alone — no retrieval, no synthesis.
    Routing,
    /// The production KnowledgeQuery retrieval pipeline, no synthesis.
    Prod,
    /// A raw hybrid search of the named corpus's indexes, reranked as the
    /// runtime is configured, optionally widened by an atlas walk.
    Retrieve,
    /// A full attached-document turn per question, through a minted
    /// `DocumentSession`, over an asset the probe ingests or reuses.
    Attached,
}

impl ProbeMode {
    /// Every mode, in command-line order.
    pub const ALL: [ProbeMode; 4] = [
        ProbeMode::Routing,
        ProbeMode::Prod,
        ProbeMode::Retrieve,
        ProbeMode::Attached,
    ];

    /// The command-line spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeMode::Routing => "routing",
            ProbeMode::Prod => "prod",
            ProbeMode::Retrieve => "retrieve",
            ProbeMode::Attached => "attached",
        }
    }

    /// Parse the command-line spelling; `None` for anything else.
    pub fn parse(s: &str) -> Option<ProbeMode> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// What `svrn __probe --request <file>` reads: which stage, over which
/// questions, with which retrieval knobs. The session config (daemon, data
/// dir, models, temperature) rides on svrn's own global flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeRequest {
    /// The stage to run.
    pub mode: ProbeMode,
    /// The questions, answered in this order.
    pub questions: Vec<ProbeQuestion>,
    /// The corpus the session is sealed to, and the one whose indexes the
    /// retrieve probe searches. Empty = every installed corpus.
    pub corpus: String,
    /// Pool cap per question (prod, retrieve).
    pub limit: usize,
    /// Prod: scope each question's conversation to `corpus` alone.
    pub isolate: bool,
    /// Atlases to load (prod, retrieve); the retrieve probe walks them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas: Option<AtlasProbe>,
    /// Attached: the asset and how to build it. Required in that mode,
    /// ignored in the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<AttachedProbe>,
}

/// The atlases a probe loads, and how it filters and seeds them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasProbe {
    /// Atlas corpus ids, each loaded independently.
    pub corpus_ids: Vec<String>,
    /// Atom matches kept per question.
    pub top_k: usize,
    /// Atoms with a shorter description are dropped.
    pub min_description_chars: usize,
    /// `enrichment_depth` allowlist; empty = any depth.
    pub depth_allowlist: Vec<String>,
    /// Cap on atoms embedded; `None` = unlimited.
    pub max_entries: Option<usize>,
    /// Atom kinds surfaced beyond entities (`claim`, `tension`,
    /// `configuration`), lowercase.
    pub include_kinds: Vec<String>,
    /// How the atlas walk seeds.
    pub seed: SeedMode,
}

/// How the atlas walk seeds: `--atlas-seed cosine|ann`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedMode {
    /// v1: exact cosine over the in-memory embedding bag + `resolve_atom_id`.
    Cosine,
    /// v2: ANN over each corpus's persistent vector column — atom-ids returned
    /// directly, no per-query resolve. Requires the corpus to be backfilled.
    Ann,
}

/// One question handed to the probe: an id to key the answer by, and the
/// text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeQuestion {
    /// The caller's id, echoed back on the evidence row.
    pub id: String,
    /// The question text, exactly as a user would type it.
    pub question: String,
}

/// Everything a probe run observed, one row per question in input order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ProbeEvidence {
    /// [`ProbeMode::Routing`].
    Routing {
        /// One classification per question.
        rows: Vec<RoutingEvidence>,
    },
    /// [`ProbeMode::Prod`].
    Prod {
        /// One evidence pool per question.
        rows: Vec<PoolEvidence>,
        /// Retrieval-pipeline ledger violations the probe process counted
        /// over the whole run: a step that could not account for its
        /// candidates. Non-zero means the pools are not trustworthy.
        ledger_violations: u64,
    },
    /// [`ProbeMode::Retrieve`].
    Retrieve {
        /// One evidence pool per question.
        rows: Vec<PoolEvidence>,
    },
    /// [`ProbeMode::Attached`].
    Attached(Box<AttachedEvidence>),
}

impl ProbeEvidence {
    /// The stage this evidence came from.
    pub fn mode(&self) -> ProbeMode {
        match self {
            ProbeEvidence::Routing { .. } => ProbeMode::Routing,
            ProbeEvidence::Prod { .. } => ProbeMode::Prod,
            ProbeEvidence::Retrieve { .. } => ProbeMode::Retrieve,
            ProbeEvidence::Attached(_) => ProbeMode::Attached,
        }
    }
}

/// The classifier's decision on one question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingEvidence {
    /// [`ProbeQuestion::id`].
    pub id: String,
    /// `Some(why)` when the classifier failed; the fields below are then
    /// empty and make no claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The intent's wire slug (`Intent::row().slug`).
    pub intent: String,
    /// The raw coarse label the classifier produced, when it produced one.
    pub coarse_intent: Option<String>,
    /// The classifier's confidence in `intent`.
    pub confidence: f32,
    /// The classifier's one-clause justification, when it gave one.
    pub rationale: Option<String>,
    /// Wall time of the classify call.
    pub latency_ms: u64,
}

/// The evidence pool retrieval assembled for one question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolEvidence {
    /// [`ProbeQuestion::id`].
    pub id: String,
    /// `Some(why)` when the stage failed before producing a pool; `chunks`
    /// is then empty and makes no claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The pool, in rank order, as the stage returned it.
    pub chunks: Vec<PoolChunk>,
    /// Atlas atoms the question's embedding matched — a navigation
    /// snapshot, not evidence. Empty unless the retrieve probe loaded atlases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub atlas_navigation: Vec<PoolChunk>,
    /// Wall time of the query embed (0 where the stage embeds internally).
    pub embed_ms: u64,
    /// Wall time of the search.
    pub search_ms: u64,
    /// Distinct corpora that contributed, in the order the stage met them.
    pub corpora_hit: Vec<String>,
    /// False when the query embedding matched no index's dimension, so the
    /// search ran full-text only.
    pub vector_eligible: bool,
    /// Corpora in scope that could not serve this question. A judge must
    /// not score a pool assembled without one of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable_corpora: Vec<CorpusUnavailable>,
    /// The production pipeline's atlas-walk echo, as svrn serialises it.
    /// `None` = the walk did not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas_walk: Option<serde_json::Value>,
}

/// One retrieved chunk, with the fields a judge reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolChunk {
    /// The corpus it came from.
    pub corpus_id: String,
    /// The source document's title, when the chunk carries one.
    pub title: Option<String>,
    /// The source URL, when the corpus carries one.
    pub url: Option<String>,
    /// The ranker's score.
    pub score: f32,
    /// The chunk text the stage returned, in full.
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_round_trips_its_spelling() {
        for m in ProbeMode::ALL {
            assert_eq!(ProbeMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(ProbeMode::parse("synth"), None);
    }

    #[test]
    fn evidence_names_its_mode_on_the_wire() {
        let ev = ProbeEvidence::Prod {
            rows: vec![],
            ledger_violations: 2,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["mode"], "prod");
        assert_eq!(v["ledger_violations"], 2);
        let back: ProbeEvidence = serde_json::from_value(v).unwrap();
        assert!(matches!(
            back,
            ProbeEvidence::Prod {
                ledger_violations: 2,
                ..
            }
        ));
    }

    /// The attached mode's request names its asset source by kind, and its
    /// evidence carries the build and the turns under `mode: attached`.
    #[test]
    fn attached_request_and_evidence_round_trip() {
        let req = ProbeRequest {
            mode: ProbeMode::Attached,
            questions: vec![ProbeQuestion {
                id: "q1".into(),
                question: "Who is Verloc?".into(),
            }],
            corpus: String::new(),
            limit: 0,
            isolate: false,
            atlas: None,
            attached: Some(AttachedProbe {
                source: AttachedSource::Reuse {
                    asset_id: "a1".into(),
                },
                enrich_model: None,
                no_gliner: true,
                rebuild_skeleton: false,
                rebuild_raptor: true,
                warm_atlas: false,
                lane: "bench book-report".into(),
            }),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["mode"], "attached");
        assert_eq!(v["attached"]["source"]["kind"], "reuse");
        assert_eq!(v["attached"]["source"]["asset_id"], "a1");
        let back: ProbeRequest = serde_json::from_value(v).unwrap();
        assert!(back.attached.unwrap().rebuild_raptor);

        let ev = ProbeEvidence::Attached(Box::new(AttachedEvidence {
            assets: vec![],
            asset: None,
            chat_model: "m".into(),
            enrich_model: "m".into(),
            attach_ms: 7,
            transitions: vec![StateTransition {
                ms_since_attach: 7,
                phase: "failed".into(),
                detail: serde_json::json!({ "error": "boom" }),
            }],
            terminal_phase: "failed".into(),
            chunks: vec![],
            rows: vec![AttachedTurn {
                id: "q1".into(),
                error: Some("runtime: down".into()),
                answer: String::new(),
                metadata: None,
                narration: vec![],
                latency_ms: 3,
                question_embedding: vec![0.5],
            }],
            resources: ResourceReport {
                phases: vec![],
                totals: PhaseBucket::default(),
                models_seen: vec![],
                calls: vec![],
            },
        }));
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["mode"], "attached");
        assert_eq!(v["transitions"][0]["detail"]["error"], "boom");
        let back: ProbeEvidence = serde_json::from_value(v).unwrap();
        assert_eq!(back.mode(), ProbeMode::Attached);
        let ProbeEvidence::Attached(back) = back else {
            unreachable!()
        };
        assert_eq!(back.rows[0].error.as_deref(), Some("runtime: down"));
        assert_eq!(back.rows[0].question_embedding, vec![0.5]);
    }
}
