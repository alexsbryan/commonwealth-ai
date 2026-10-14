//! Particulars the window's reader named that no statement of the window is
//! about. Each reaches RESOLVE as a statement of its own
//! (`DocumentReadParticular`, as the passes reader's `about::unstated` sends
//! them), on the first document whose own lines name it, so a person only
//! ever spoken of is a record, decided across windows by RESOLVE like any
//! other. Failing input, named: saltgrass (2026-10-14), whose window reader
//! named nine characters, the killer among them, of which four became records:
//! the window envelope sent claims only.

use std::collections::BTreeSet;

use serde_json::Value;
use tracing::{debug, trace};

use super::super::pick::Candidate;
use super::super::{DocumentReadOutcome, DocumentReadParticular};
use super::{particular_fields, text, Contract};
use crate::enrichment::ontology::{OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::types::ChapterInput;

/// Add every unstated particular of a statement subject type to the outcome
/// of the first document whose own lines name it: by its name, or else by a
/// field's quote, and that line is its evidence. One no own line names cites
/// nothing and is dropped, traced; one of a type no statement kind is about
/// is left to the references that name it, traced.
#[allow(clippy::too_many_arguments)]
pub(super) fn unstated(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
    contract: &Contract<'_>,
    particulars: &[Value],
    claims: &[Value],
    owns: &[String],
    outcomes: &mut [(DocumentReadOutcome, Vec<(String, Vec<Candidate>)>)],
) {
    let stated: BTreeSet<String> = claims.iter().map(|c| text(&c["about"])).collect();
    for p in particulars {
        let local = text(&p["ref"]);
        if local.is_empty() || stated.contains(&local) {
            continue;
        }
        let ty = text(&p["type"]);
        let name = text(&p["name"]);
        let Some(subject) = contract.subjects.iter().copied().find(|s| s.name == ty) else {
            debug!(chapter = %chapter.chapter_id, particular = %local, %ty, %name, "document_read/window: no statement kind is about this type; its references name it");
            continue;
        };
        let mut cues = vec![name.clone()];
        if let Some(fields) = p["fields"].as_object() {
            cues.extend(fields.values().filter_map(|f| f.get("quote")).map(text));
        }
        let found = owns.iter().enumerate().find_map(|(k, own)| {
            cues.iter()
                .filter(|c| !c.trim().is_empty())
                .find_map(|c| line_naming(own, c))
                .map(|line| (k, line))
        });
        let Some((k, evidence)) = found else {
            debug!(chapter = %chapter.chapter_id, particular = %local, %name, "document_read/window: no own line of the window names this particular; dropped");
            continue;
        };
        let document = &chapter.source_documents[k];
        let (outcome, named) = &mut outcomes[k];
        let subject_fields = particular_fields(policies, index, subject, p, document, named);
        trace!(document = %outcome.document_id, particular = %local, %name, %evidence, "document_read/window: an unstated particular sent as its own statement");
        outcome.particulars.push(DocumentReadParticular {
            subject_type: ty,
            subject_local_ref: format!("{}/{local}", chapter.chapter_id),
            subject_name: if name.is_empty() { local } else { name },
            evidence,
            subject_fields,
            words: None,
        });
    }
}

/// The first line of `own` holding `cue`, case and whitespace aside.
fn line_naming(own: &str, cue: &str) -> Option<String> {
    let fold = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let cue = fold(cue);
    own.lines()
        .find(|l| fold(l).contains(&cue))
        .map(|l| l.trim().to_string())
}
