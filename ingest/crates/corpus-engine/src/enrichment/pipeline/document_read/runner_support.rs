// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runner-only support for the declared document-reading path.

use std::path::Path;

use serde::Serialize;

use super::super::atlas::{SectionExtraction, SeedEntities};
use super::super::exemplar_bank::ExemplarBank;
use super::super::phase_cache::PhaseCache;
use super::super::pipelines::relation_focus::RelationFocus;
use super::super::types::{ChapterInput, Phase1Failure, PhaseFailureKind, PipelinePhase};
use crate::enrichment::ontology::OntologyPolicies;
use crate::error::{Error, Result};
use crate::types::EmbedFn;

pub async fn load_exemplar_bank(
    document_reading: bool,
    chapter_count: usize,
    path: &Path,
    embed: &EmbedFn,
) -> Result<Option<ExemplarBank>> {
    if document_reading {
        tracing::debug!(
            chapters = chapter_count,
            "phase1.document_read_mode_enabled"
        );
        Ok(None)
    } else {
        ExemplarBank::load_embedded(path, PipelinePhase::Questions, embed)
            .await
            .map(Some)
    }
}

pub fn phase1_inputs(
    document_reading: bool,
    cache: &PhaseCache,
    policies: &OntologyPolicies,
) -> Result<(Option<SeedEntities>, Option<RelationFocus>)> {
    let seed_opt: Option<SeedEntities> = if document_reading {
        None
    } else {
        cache.read(PipelinePhase::SeedExtraction)?
    };
    let focus = (!document_reading).then(|| RelationFocus::from_policies(policies));
    Ok((seed_opt, focus))
}

pub fn missing_source_documents(
    document_reading: bool,
    chapter: &ChapterInput,
) -> Option<Phase1Failure> {
    if !document_reading || !chapter.source_documents.is_empty() {
        return None;
    }
    let reason = format!(
        "declared document reading requires hydrated source documents for chapter `{}`",
        chapter.chapter_id
    );
    tracing::warn!(
        chapter = %chapter.chapter_id,
        "phase1.document_read_missing_source_documents"
    );
    Some(Phase1Failure {
        chapter_id: chapter.chapter_id.clone(),
        reason,
        raw_response_head: None,
        failure_kind: PhaseFailureKind::Skipped,
    })
}

pub fn cache_text<P: Serialize>(
    document_reading: bool,
    policies: &OntologyPolicies,
    chapter: &ChapterInput,
    line_classes: &str,
    prompt: &P,
) -> Result<String> {
    if document_reading {
        let prompt = serde_json::to_string(prompt).map_err(|error| {
            Error::Serialization(format!(
                "serialise declared document-read prompt for cache identity: {error}"
            ))
        })?;
        Ok(format!(
            "{}\0{}\0{}\0{}",
            super::contract_fingerprint(policies),
            super::context_fingerprint(chapter),
            line_classes,
            prompt
        ))
    } else {
        Ok(chapter.text.clone())
    }
}

pub fn validate_response(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    section_extraction: Option<&mut SectionExtraction>,
    response: &str,
) -> std::result::Result<(), Phase1Failure> {
    let validation = section_extraction
        .ok_or_else(|| {
            Error::Serialization("document-reading policy returned no SectionExtraction".into())
        })
        .and_then(|extraction| super::validate_and_stamp(chapter, policies, extraction));
    if let Err(error) = validation {
        let head = super::super::runner::truncate_response_head(response);
        let reason = format!("document-read validation error: {error}");
        tracing::warn!(
            chapter_id = %chapter.chapter_id,
            error = %error,
            "phase1.document_read_validation_failed"
        );
        return Err(Phase1Failure {
            chapter_id: chapter.chapter_id.clone(),
            reason,
            raw_response_head: head,
            failure_kind: PhaseFailureKind::ParseDrift,
        });
    }
    Ok(())
}
