use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{json, Value};

use crate::enrichment::ontology::OntologyPolicies;
use crate::enrichment::pipeline::atlas::SectionExtraction;
use crate::enrichment::pipeline::runner::Phase1CheckpointEntry;
use crate::enrichment::pipeline::types::{ChapterInput, ExtractedQuestion, Phase1Output};

use super::{schema, validation};

/// Whether a cached read is valid for one current chapter and extraction contract.
pub fn cache_matches_chapter(
    chapter: &ChapterInput,
    extraction: &SectionExtraction,
    policies: &OntologyPolicies,
) -> std::result::Result<(), String> {
    let read = extraction
        .document_read
        .as_ref()
        .ok_or_else(|| "cached Phase-1 section has no declared document read".to_string())?;
    let expected_context = context_fingerprint(chapter);
    if read.context_fingerprint != expected_context {
        return Err("cached document author, metadata, identity, or body changed".into());
    }
    let mut checked = extraction.clone();
    validation::validate_and_stamp(chapter, policies, &mut checked)
        .map_err(|error| format!("cached document read is no longer valid: {error}"))?;
    Ok(())
}

/// Validate a full cached Phase-1 output against the current document inputs.
pub fn phase1_cache_matches(
    chapters: &[ChapterInput],
    output: &Phase1Output,
    policies: &OntologyPolicies,
) -> std::result::Result<(), String> {
    if !policies.document_reading {
        if output.questions_by_chapter.iter().any(|chapter| {
            chapter
                .section_extraction
                .as_ref()
                .is_some_and(|section| section.document_read.is_some())
        }) {
            return Err("cached Phase-1 output was produced with document_reading enabled".into());
        }
        return Ok(());
    }
    let mut by_id: HashMap<&str, &ExtractedQuestion> = HashMap::new();
    for extracted in &output.questions_by_chapter {
        if by_id.insert(&extracted.chapter_id, extracted).is_some() {
            return Err(format!(
                "cached Phase-1 output repeats chapter `{}`",
                extracted.chapter_id
            ));
        }
    }
    if by_id.len() != chapters.len() {
        return Err(format!(
            "cached Phase-1 output has {} chapter(s), current input has {}",
            by_id.len(),
            chapters.len()
        ));
    }
    for chapter in chapters {
        let extracted = by_id
            .get(chapter.chapter_id.as_str())
            .ok_or_else(|| format!("cached Phase-1 output omits `{}`", chapter.chapter_id))?;
        let section = extracted.section_extraction.as_ref().ok_or_else(|| {
            format!(
                "cached chapter `{}` has no SectionExtraction",
                chapter.chapter_id
            )
        })?;
        cache_matches_chapter(chapter, section, policies)
            .map_err(|error| format!("cached chapter `{}`: {error}", chapter.chapter_id))?;
    }
    Ok(())
}

/// Success checkpoints are reusable only when their accountable read still fits.
pub fn checkpoint_processed_ids(
    entries: &[Phase1CheckpointEntry],
    chapters: &[ChapterInput],
    policies: &OntologyPolicies,
) -> HashSet<String> {
    if !policies.document_reading {
        let read_successes: BTreeSet<String> = entries
            .iter()
            .filter_map(|entry| match entry {
                Phase1CheckpointEntry::Success {
                    chapter_id,
                    extracted,
                } if extracted
                    .section_extraction
                    .as_ref()
                    .is_some_and(|section| section.document_read.is_some()) =>
                {
                    Some(chapter_id.clone())
                }
                _ => None,
            })
            .collect();
        return entries
            .iter()
            .map(|entry| entry.chapter_id().to_string())
            .filter(|chapter| !read_successes.contains(chapter))
            .collect();
    }
    let mut latest_success = HashMap::<String, ExtractedQuestion>::new();
    for entry in entries {
        match entry {
            Phase1CheckpointEntry::Success {
                chapter_id,
                extracted,
            } => {
                latest_success.insert(chapter_id.clone(), extracted.clone());
            }
            Phase1CheckpointEntry::Failure { .. } => {}
        }
    }
    chapters
        .iter()
        .filter_map(|chapter| {
            let extracted = latest_success.get(&chapter.chapter_id)?;
            let section = extracted.section_extraction.as_ref()?;
            cache_matches_chapter(chapter, section, policies)
                .ok()
                .map(|()| chapter.chapter_id.clone())
        })
        .collect()
}

/// Refuse to finalize a checkpoint containing a legacy or stale document read.
pub fn validate_checkpoint(
    entries: &[Phase1CheckpointEntry],
    chapters: &[ChapterInput],
    policies: &OntologyPolicies,
) -> std::result::Result<(), String> {
    if !policies.document_reading {
        if entries.iter().any(|entry| match entry {
            Phase1CheckpointEntry::Success { extracted, .. } => extracted
                .section_extraction
                .as_ref()
                .is_some_and(|section| section.document_read.is_some()),
            Phase1CheckpointEntry::Failure { .. } => false,
        }) {
            return Err(
                "checkpoint contains declared document reads but document_reading is disabled"
                    .into(),
            );
        }
        return Ok(());
    }
    let by_id: HashMap<&str, &ChapterInput> = chapters
        .iter()
        .map(|chapter| (chapter.chapter_id.as_str(), chapter))
        .collect();
    let mut latest_success = HashMap::<String, ExtractedQuestion>::new();
    for entry in entries {
        if let Phase1CheckpointEntry::Success {
            chapter_id,
            extracted,
        } = entry
        {
            latest_success.insert(chapter_id.clone(), extracted.clone());
        }
    }
    if latest_success.is_empty() {
        return Err("checkpoint has no successful declared document reads to finalize".into());
    }
    for (chapter_id, extracted) in latest_success {
        let chapter = by_id.get(chapter_id.as_str()).ok_or_else(|| {
            format!("checkpoint chapter `{chapter_id}` is not in the current manifest")
        })?;
        let section = extracted.section_extraction.as_ref().ok_or_else(|| {
            format!("checkpoint chapter `{chapter_id}` has no document-read extraction")
        })?;
        cache_matches_chapter(chapter, section, policies)
            .map_err(|error| format!("checkpoint chapter `{chapter_id}`: {error}"))?;
    }
    Ok(())
}

/// Hashes the policy-derived prompt/schema only. The runner adds hydrated
/// context, the serialized prompt, configured model id, and prompt version to
/// its section-cache key. Neither this fingerprint nor that key carries a
/// provider weight fingerprint or runtime sampling settings; parsed checkpoint
/// reuse is therefore contract-and-context qualified, not model-qualified.
pub fn contract_fingerprint(policies: &OntologyPolicies) -> String {
    blake3::hash(
        &serde_json::to_vec(&schema::contract_value(policies))
            .expect("serialising ontology read contract is infallible"),
    )
    .to_hex()
    .to_string()
}

/// Fingerprint every document input that reaches the dedicated prompt.
pub fn context_fingerprint(chapter: &ChapterInput) -> String {
    let documents: Vec<Value> = chapter
        .source_documents
        .iter()
        .map(|document| {
            json!({
                "id": document.key(),
                "source_doc_id": document.source_doc_id(),
                "title": document.title(),
                "url": document.url(),
                "metadata": document.metadata(),
                "body": document.raw_body(),
            })
        })
        .collect();
    blake3::hash(
        &serde_json::to_vec(&json!({
            "chapter_id": chapter.chapter_id,
            "documents": documents,
        }))
        .expect("serialising hydrated document context is infallible"),
    )
    .to_hex()
    .to_string()
}
