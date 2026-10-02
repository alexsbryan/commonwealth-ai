// SPDX-License-Identifier: AGPL-3.0-or-later
//! `pool_resolved`'s ledger, in the pipeline's unit. Beside `atlas_grounding.rs`
//! under `#[path]` so the step file stays under the ARCH §3.1 band.

use super::pool_resolved;
use crate::runtime::retrieval_ledger::{ledger_violations, DropReason, StepKind};
use corpus_engine_atlas_reader::context::ChunkRequest;
use corpus_engine_atlas_reader::evidence_site::{ChunkSelector, EvidenceSite};
use corpus_engine_atlas_reader::resolve::{resolve_evidence, EvidenceFetcher};
use corpus_index::{index::ChunkProvenance, types::ScoredChunk};

fn passage(i: u64) -> ScoredChunk {
    ScoredChunk {
        content: format!("passage {i}"),
        title: Some("abduction".into()),
        url: None,
        corpus_id: "sep".into(),
        score: 0.9,
        metadata: Default::default(),
        chunk_id: Some(i),
        source_doc_id: None,
        vector_distance: Some(0.1),
        provenance: ChunkProvenance::acquired_from_estate("sep"),
    }
}

/// A searched Section request: its one search returns two passages and a
/// repeat of the first.
struct TwoAndARepeat;

impl EvidenceFetcher for TwoAndARepeat {
    async fn by_row(&self, _: &kernel_types::CorpusId, _: u64) -> Option<ScoredChunk> {
        None
    }
    async fn by_search(&self, _: &kernel_types::CorpusId, _: &str, _: usize) -> Vec<ScoredChunk> {
        vec![passage(1), passage(2), passage(1)]
    }
}

fn searched_section(excerpts: Vec<String>) -> ChunkRequest {
    ChunkRequest {
        site: EvidenceSite::derive("sep-abduction"),
        selector: ChunkSelector::Section("sec_0001".into()),
        passage_preview: "preview".into(),
        score: 1.0,
        motivating_atoms: vec!["atom".into()],
        verbatim_excerpts: excerpts,
        passage_previews: Vec::new(),
        section_rows: Vec::new(),
    }
}

/// The step's ledger is in CHUNKS, the pipeline's unit. One request that
/// yields two passages plus a duplicate is three candidates: two added, one
/// dropped as a duplicate, and the identity balances. Failing input: report
/// `considered` in requests (`resolve.considered`, 1 here), which is the SEP
/// lane's `delta=9 considered=8` violation.
#[tokio::test]
async fn one_section_request_yielding_several_passages_balances() {
    let requests = vec![searched_section(Vec::new())];
    let (fetched, resolve) = resolve_evidence(&requests, 12, None, &TwoAndARepeat).await;
    let mut pool: Vec<ScoredChunk> = Vec::new();
    let led = pool_resolved(&mut pool, fetched, &resolve);
    assert_eq!(pool.len(), 2);
    assert_eq!(led.considered, Some(3));
    assert_eq!(led.accounted.get(&DropReason::Duplicate), Some(&1));
    let violations = ledger_violations(StepKind::Injector, pool.len() as i64, &led);
    assert!(violations.is_empty(), "{violations:?}: {led:?}");
}

/// The pool's own de-duplication is a named drop, not a silent one. An
/// excerpt longer than the pool key's 80 characters makes the two passages
/// one key once the highlights are prepended. Failing input: drop the
/// `pool_duplicate` term.
#[tokio::test]
async fn the_pool_de_duplication_is_recorded_as_a_duplicate() {
    let requests = vec![searched_section(vec!["x".repeat(100)])];
    let (fetched, resolve) = resolve_evidence(&requests, 12, None, &TwoAndARepeat).await;
    let mut pool: Vec<ScoredChunk> = Vec::new();
    let led = pool_resolved(&mut pool, fetched, &resolve);
    assert_eq!(pool.len(), 1);
    assert_eq!(led.considered, Some(3));
    assert_eq!(led.accounted.get(&DropReason::Duplicate), Some(&2));
    let violations = ledger_violations(StepKind::Injector, pool.len() as i64, &led);
    assert!(violations.is_empty(), "{violations:?}: {led:?}");
}
