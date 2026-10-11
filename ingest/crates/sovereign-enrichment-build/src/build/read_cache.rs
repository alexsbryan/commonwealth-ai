// SPDX-License-Identifier: AGPL-3.0-or-later
//! Contract-aware reuse of declared document reads in the canonical build cache.

use crate::config::EnrichConfig;
use std::path::Path;

pub(super) fn extract_cache_matches_document_reads(
    cache_path: &Path,
    corpus: &str,
) -> std::result::Result<(), String> {
    let cfg = EnrichConfig::require(corpus)
        .map_err(|error| format!("cannot check declared read cache identity: {error}"))?;
    let bytes = std::fs::read(cache_path).map_err(|error| {
        format!(
            "cannot read Phase-1 cache {}: {error}",
            cache_path.display()
        )
    })?;
    let raw: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "cannot parse Phase-1 cache {}: {error}",
            cache_path.display()
        )
    })?;
    let has_document_reads = raw
        .get("questions_by_chapter")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|chapters| {
            chapters.iter().any(|chapter| {
                chapter
                    .get("section_extraction")
                    .and_then(|section| section.get("document_read"))
                    .is_some_and(|read| !read.is_null())
            })
        });
    let policies = cfg
        .ontology
        .as_ref()
        .map(|spec| spec.policies())
        .unwrap_or_default();
    if !policies.reads_documents() {
        return if has_document_reads {
            Err("cached Phase-1 output was produced by a declared reading this declaration no longer asks for".into())
        } else {
            Ok(())
        };
    }
    let (chapters, _) = crate::corpus_io::rebuild_corpus_state(&cfg)
        .map_err(|error| format!("cannot hydrate current declared read inputs: {error}"))?;
    let output: corpus_engine::enrichment::pipeline::Phase1Output = serde_json::from_slice(&bytes)
        .map_err(|error| {
            format!(
                "cannot parse Phase-1 cache {}: {error}",
                cache_path.display()
            )
        })?;
    corpus_engine::enrichment::pipeline::document_read::phase1_cache_matches(
        &chapters, &output, &policies,
    )
}
