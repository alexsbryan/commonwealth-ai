// SPDX-License-Identifier: AGPL-3.0-or-later
//! The window reader (ONTOLOGY_METHOD §Reading): one call reads a whole
//! chapter, a window of documents in clock order (`window_manifest` in the
//! build crate), and names each particular once across them. Two claims in two
//! messages the model reads as one deal reach RESOLVE as one subject
//! ([`SubjectScope::Chapter`]), and RESOLVE decides only across windows.
//!
//! The model writes values and quotes; code keeps the contract. Every declared
//! field is answered, or unknown with a reason; a closed value is one of its
//! declared values; a reference is a record the document's own fields name, or
//! words the document holds; a field's evidence is in the claim's own
//! document. A claim whose quote is not in its document is refused by
//! validation (`validate_and_stamp`), never projected.

use std::borrow::Cow;
use std::collections::BTreeSet;

use serde_json::{json, Value};
use tracing::{debug, info, trace, warn};

use super::line_classes::LineClasses;
use super::pick::{candidates, Candidate};
use super::validation::{field_evidence_exists, metadata_strings};
use super::{
    DocumentReadClaim, DocumentReadField, DocumentReadOutcome, DocumentReadRefusal,
    DocumentReadStatus, SubjectScope,
};
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolution_records::decides;
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{
    AttrDecl, AttrFamily, OntologyPolicies, OntologyTypeDecl, SourceDecl, TypeIndex,
};
use crate::enrichment::pipeline::pipelines::literary::prepare_phase_json;
use crate::enrichment::pipeline::types::{ChapterInput, ChatPrompt};
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;
use crate::error::{Error, Result};
use crate::InferenceFn;

pub(super) const WINDOW_PHASE: &str = "document_window";
const PROMPT: &str = include_str!("window_prompt.md");
const SYSTEM: &str = "You read documents and write what an ontology describes, as JSON.";
/// The answer a window's call is given, its reasoning included: the engine
/// counts both against it. A window holds about 10k tokens
/// (`window_manifest::WINDOW_CHARS`), so prompt and answer fit 128k, the
/// context the system is held to (a 64GB machine on a capable local model,
/// operator 2026-10-14). The longest reasoning and answer on the worlds'
/// windows ran to ~47k tokens (runs/loop/reader-think-uncapped, 2026-10-14);
/// these two values are that arm's, so its reads are the default's.
const MAX_ANSWER_TOKENS: u32 = 64_000;
/// The reader reasons before it answers. Set on the prompt, so the answer
/// store keys a read by it (`asker::chat_key`). It leaves 16k of the answer
/// for the JSON on an engine that honours it; one that does not (DeepSeek)
/// is still held by `MAX_ANSWER_TOKENS`.
const THINKING_TOKENS: u32 = 48_000;

/// What the window reader reads from a declaration: the claim kinds it reads,
/// each with its subject type, and the types it names particulars of, each
/// once: those subjects, then every other type RESOLVE decides.
struct Contract<'a> {
    kinds: Vec<(&'a OntologyTypeDecl, &'a OntologyTypeDecl)>,
    subjects: Vec<&'a OntologyTypeDecl>,
}

impl<'a> Contract<'a> {
    fn of(policies: &'a OntologyPolicies) -> Self {
        let kinds: Vec<_> = policies
            .shape
            .types
            .iter()
            .filter(|t| super::schema::read_kind(t, policies))
            .filter_map(|t| Some((t, policies.type_decl(t.read_subject()?)?)))
            .collect();
        let index = TypeIndex::from_policies(policies);
        let mut subjects: Vec<&OntologyTypeDecl> = Vec::new();
        let decided = policies
            .shape
            .types
            .iter()
            .filter(|t| decides(&index, &t.name));
        for s in kinds.iter().map(|(_, s)| *s).chain(decided) {
            if !subjects.iter().any(|t| t.name == s.name) {
                subjects.push(s);
            }
        }
        Self { kinds, subjects }
    }

    /// Every type a read reference names, or a subject its metadata source
    /// names: the types a document lists its candidates of.
    fn named(
        &self,
        policies: &'a OntologyPolicies,
        index: &TypeIndex<'a>,
    ) -> Vec<&'a OntologyTypeDecl> {
        let mut names: BTreeSet<&str> = BTreeSet::new();
        for (kind, subject) in &self.kinds {
            let attrs = read_attrs(index, kind).into_iter().chain(
                subject_attrs(policies, index, &subject.name)
                    .into_iter()
                    .map(Cow::Borrowed),
            );
            for attr in attrs {
                // The target's name borrowed from its declaration: an owned
                // field (a relation's `to`) cannot lend its own.
                if let AttrFamily::Ref { of } = &attr.family {
                    if let Some(target) = policies.type_decl(of) {
                        names.insert(target.name.as_str());
                    }
                }
            }
            if matches!(subject.source, Some(SourceDecl::Metadata(_))) {
                names.insert(subject.name.as_str());
            }
        }
        names
            .into_iter()
            .filter_map(|n| policies.type_decl(n))
            .collect()
    }
}

fn read_attrs<'a>(index: &TypeIndex<'a>, kind: &'a OntologyTypeDecl) -> Vec<Cow<'a, AttrDecl>> {
    super::schema::statement_attributes(index, kind)
        .into_iter()
        .filter(|a| a.derived.is_none())
        .collect()
}

fn subject_attrs<'a>(
    policies: &'a OntologyPolicies,
    index: &TypeIndex<'a>,
    subject: &str,
) -> Vec<&'a AttrDecl> {
    super::schema::subject_read_attributes(policies, index, subject)
        .into_iter()
        .filter(|a| a.derived.is_none())
        .collect()
}

fn attr_line(attr: &AttrDecl) -> String {
    let kind = match &attr.family {
        AttrFamily::Text { .. } => "text".to_string(),
        AttrFamily::Quantity { unit: Some(u) } => format!("a number, in {u}"),
        AttrFamily::Quantity { unit: None } => "a number".to_string(),
        AttrFamily::Time { .. } => "a time".to_string(),
        AttrFamily::Ref { of } => format!("a {of}"),
    };
    let mut line = format!("  * {} ({kind})", attr.name);
    if !attr.description.is_empty() {
        line.push_str(&format!(": {}", attr.description));
    }
    if let AttrFamily::Text { values } = &attr.family {
        if !values.is_empty() {
            line.push_str(&format!(" One of: {}.", values.join(" | ")));
        }
    }
    line
}

/// The ontology as the prompt shows it, generated from the declaration alone.
fn ontology_text(
    contract: &Contract<'_>,
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
) -> String {
    let mut out = Vec::new();
    for (kind, subject) in &contract.kinds {
        out.push(format!(
            "- {} (a statement about one {}): {}",
            kind.name, subject.name, kind.description
        ));
        out.extend(read_attrs(index, kind).iter().map(|a| attr_line(a)));
    }
    for subject in &contract.subjects {
        out.push(format!("- {}: {}", subject.name, subject.description));
        if let Some(same) = &subject.identity_criterion {
            out.push(format!("  Same {} when: {same}", subject.name));
        }
        if matches!(subject.source, Some(SourceDecl::Metadata(_))) {
            out.push(format!(
                "  Named beside each document that names one; write it as one of those names."
            ));
        }
        out.extend(
            subject_attrs(policies, index, &subject.name)
                .into_iter()
                .map(attr_line),
        );
    }
    if !policies.prose.guidance.trim().is_empty() {
        out.push(format!("Guidance: {}", policies.prose.guidance.trim()));
    }
    out.join("\n")
}

/// The fields a document shows beside its text: the ones the declaration
/// reads, its document stamps and its metadata sources' fields.
fn shown_fields(policies: &OntologyPolicies) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    let mut add = |f: &str| {
        if !fields.iter().any(|x| x == f) {
            fields.push(f.to_string());
        }
    };
    if let Some(d) = &policies.change.document {
        for (_, f) in d.declared() {
            add(f);
        }
    }
    for t in &policies.shape.types {
        if let Some(SourceDecl::Metadata(src)) = &t.source {
            for f in &src.metadata {
                add(f);
            }
        }
    }
    fields
}

fn compose(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
    contract: &Contract<'_>,
    labels: &[String],
    classes: &LineClasses,
) -> ChatPrompt {
    let fields = shown_fields(policies);
    let named = contract.named(policies, index);
    let by_key = shown::labels(&chapter.source_documents, labels);
    let documents: String = chapter
        .source_documents
        .iter()
        .zip(labels)
        .map(|(doc, label)| {
            let mut head = vec![format!("=== {label} ===")];
            for f in &fields {
                if let Some(v) = doc.metadata().get(f) {
                    let v = metadata_strings(v).join(", ");
                    if !v.is_empty() {
                        head.push(format!("{f}: {v}"));
                    }
                }
            }
            let listed: Vec<String> = named
                .iter()
                .flat_map(|t| {
                    candidates(doc, policies, t, &[])
                        .shown
                        .into_iter()
                        .map(|c| c.shown(&t.name))
                })
                .collect();
            if !listed.is_empty() {
                head.push(format!("Named here: {}", listed.join("; ")));
            }
            let body = shown::shown_text(doc, classes, &by_key);
            format!("{}\n\n{body}\n", head.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let user = PROMPT
        .replace("{ontology}", &ontology_text(contract, policies, index))
        .replace("{documents}", &documents);
    let mut prompt = ChatPrompt::new(SYSTEM, user)
        .with_phase_id(WINDOW_PHASE)
        .with_max_output_tokens(MAX_ANSWER_TOKENS);
    prompt.thinking_tokens = Some(THINKING_TOKENS);
    prompt
}

/// What the reader asks, rendered from the declaration: a change to the
/// prompt or the ontology's rendering changes the read contract's fingerprint.
pub(super) fn contract_value(policies: &OntologyPolicies) -> Value {
    let index = TypeIndex::from_policies(policies);
    let contract = Contract::of(policies);
    json!({
        "reader": "window",
        "prompt": PROMPT,
        "ontology": ontology_text(&contract, policies, &index),
        "shown_fields": shown_fields(policies),
        "max_answer_tokens": MAX_ANSWER_TOKENS,
        "thinking_tokens": THINKING_TOKENS,
    })
}

/// Read `chapter` in one call and return the envelope `parse_response` reads.
pub(super) async fn read(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    classes: &LineClasses,
    infer: &InferenceFn,
) -> Result<String> {
    let index = TypeIndex::from_policies(policies);
    let contract = Contract::of(policies);
    if contract.kinds.is_empty() {
        return Err(Error::InvalidInput(
            "the window reader found no eligible declared claim kind".into(),
        ));
    }
    let labels: Vec<String> = (1..=chapter.source_documents.len())
        .map(|n| format!("d{n}"))
        .collect();
    let prompt = compose(chapter, policies, &index, &contract, &labels, classes);
    debug!(chapter = %chapter.chapter_id, documents = labels.len(), prompt_chars = prompt.user.len(), "document_read/window: asking");
    // What the model sees and says, whole, so a miss is diagnosed from the
    // read itself: the labels map d1..dn back to document keys.
    let keys: Vec<String> = chapter
        .source_documents
        .iter()
        .zip(&labels)
        .map(|(doc, label)| format!("{label}={}", doc.key()))
        .collect();
    trace!(chapter = %chapter.chapter_id, labels = %keys.join(" "), prompt = %prompt.user, "document_read/window: prompt");
    let raw = infer(&prompt, None).await?;
    trace!(chapter = %chapter.chapter_id, answer = %raw, "document_read/window: answer");
    // A failure keeps the answer's tail: it says whether the answer was cut
    // off at its budget or malformed.
    let failed = |why: String| {
        let tail: String = raw
            .chars()
            .rev()
            .take(240)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Error::Serialization(format!(
            "the window read's answer is not JSON: {why} ({} chars, ending {tail:?})",
            raw.len()
        ))
    };
    let parsed = prepare_phase_json(&raw, "phase 1 (window read)")
        .map_err(|e| failed(e.to_string()))
        .and_then(|json| answer::parse(&json, &chapter.chapter_id).map_err(failed))?;
    debug!(chapter = %chapter.chapter_id, repaired = parsed.repaired, dropped = parsed.dropped, "document_read/window: answer parsed");
    let owns: Vec<String> = chapter
        .source_documents
        .iter()
        .map(|doc| shown::own_text(doc, classes))
        .collect();
    let quoted: Vec<String> = chapter
        .source_documents
        .iter()
        .map(|doc| shown::quoted_text(doc, classes))
        .collect();
    let documents = envelope(
        chapter,
        policies,
        &index,
        &contract,
        &labels,
        &owns,
        &quoted,
        &parsed.value,
        parsed.dropped,
    );
    serde_json::to_string(&json!({ "documents": documents, "subjects": SubjectScope::Chapter }))
        .map_err(|e| Error::Serialization(format!("window reader envelope: {e}")))
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn fold(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn by() -> Option<SourcePrecision> {
    Some(SourcePrecision::new("reader_window", Precision::Unmeasured))
}

fn supported(value: Value, evidence: &str) -> DocumentReadField {
    DocumentReadField::Supported {
        value,
        evidence: evidence.to_string(),
        by: by(),
    }
}

fn unknown(reason: impl Into<String>) -> DocumentReadField {
    DocumentReadField::Unknown {
        reason: reason.into(),
    }
}

/// One field's reading in `document`: `value` as the model wrote it, cited by
/// `evidence` when that is in the document. A reference is a candidate the
/// document's fields name, words it holds, or a `particulars` label read as
/// its name, cited by `evidence` (`label_name`); nothing else is supported.
fn reading(
    attr: &AttrDecl,
    value: Option<&Value>,
    evidence: Option<&str>,
    document: &SourceDocument,
    named: &[(String, Vec<Candidate>)],
    particulars: &[Value],
    index: &TypeIndex<'_>,
) -> DocumentReadField {
    let Some(value) = value.filter(|v| !v.is_null() && !text(v).is_empty()) else {
        return unknown("the window read states no value");
    };
    let words = text(value);
    let cited = evidence.filter(|e| field_evidence_exists(document, e));
    match &attr.family {
        AttrFamily::Ref { of } => {
            refs::reference(of, &words, cited, document, named, particulars, index)
        }
        _ if cited.is_none() && !field_evidence_exists(document, &words) => unknown(format!(
            "{words:?} was read from a passage that is not in this document"
        )),
        AttrFamily::Text { values } if !values.is_empty() => {
            match values.iter().find(|v| fold(v) == fold(&words)) {
                Some(v) => supported(Value::String(v.clone()), cited.unwrap_or(&words)),
                None => unknown(format!("{words:?} is none of the declared values")),
            }
        }
        AttrFamily::Quantity { .. } => {
            let number = value.as_f64().or_else(|| {
                words
                    .chars()
                    .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                    .collect::<String>()
                    .parse::<f64>()
                    .ok()
            });
            match number.and_then(serde_json::Number::from_f64) {
                Some(n) => supported(Value::Number(n), cited.unwrap_or(&words)),
                None => unknown(format!("{words:?} is not a number")),
            }
        }
        AttrFamily::Text { .. } | AttrFamily::Time { .. } => {
            supported(Value::String(words.clone()), cited.unwrap_or(&words))
        }
    }
}

/// An identity field of a subject its metadata source names, read from the
/// record this document's fields name by the particular's `name`: the model
/// names a person, the source holds their address.
fn sourced_identity(
    subject: &OntologyTypeDecl,
    attr: &AttrDecl,
    name: &Value,
    index: &TypeIndex<'_>,
    named: &[(String, Vec<Candidate>)],
) -> Option<DocumentReadField> {
    if !matches!(subject.source, Some(SourceDecl::Metadata(_))) {
        return None;
    }
    let at = index
        .effective_identity(&subject.name)
        .iter()
        .position(|k| *k == attr.name)?;
    let name = text(name);
    let wanted: Vec<String> = refs::names_in(&name)
        .into_iter()
        .filter_map(fold_identity_value)
        .collect();
    named
        .iter()
        .filter(|(t, _)| *t == subject.name)
        .flat_map(|(_, cs)| cs)
        .find_map(|c| match c {
            Candidate::Record {
                identity,
                names,
                scalar,
                ..
            } if names
                .iter()
                .chain(identity.iter())
                .any(|n| fold_identity_value(n).is_some_and(|n| wanted.contains(&n))) =>
            {
                Some(supported(Value::String(identity.get(at)?.clone()), scalar))
            }
            _ => None,
        })
}

/// The sentence a quote stands in, from a document's own lines: the quote's
/// words found in order (whitespace and case aside), widened to the
/// sentence's ends (`.`, `!` or `?` before whitespace, a blank line, or the
/// text's ends), its whitespace flattened as a model writes a quote. Words
/// compare with their edge punctuation trimmed. A model
/// cites a span and code widens it, so no reader spends its reasoning copying
/// across a mail's line wraps: reasoned reads cut quotes at the wrap, 8 words
/// at the median against 12 unreasoned (runs/loop/reader-think-uncapped,
/// 2026-10-14). Longer than `MAX_SENTENCE` bytes, the quote's own lines are
/// cited instead. None when the quote's words are not in the text.
pub(super) fn sentence_of(text: &str, quote: &str) -> Option<String> {
    const MAX_SENTENCE: usize = 600;
    let words: Vec<(usize, &str)> = text
        .split_whitespace()
        .map(|w| (w.as_ptr() as usize - text.as_ptr() as usize, w))
        .collect();
    let want: Vec<&str> = quote.split_whitespace().collect();
    if want.is_empty() {
        return None;
    }
    // A span may stop inside a word's punctuation (".50" of ".50,"): words compare with it trimmed.
    let bare = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let at = words
        .windows(want.len())
        .position(|ws| ws.iter().zip(&want).all(|((_, a), b)| bare(a) == bare(b)))?;
    let start = words[at].0;
    let (last, w) = words[at + want.len() - 1];
    let end = last + w.len();
    let bytes = text.as_bytes();
    let stop = |i: usize| matches!(bytes[i], b'.' | b'!' | b'?');
    let mut a = start;
    // Byte steps: every place they stop is an ASCII byte or an end, so a char boundary.
    while a > 0
        && !(stop(a - 1) && bytes[a].is_ascii_whitespace())
        && !bytes[..a].ends_with(b"\n\n")
    {
        a -= 1;
    }
    let mut b = end;
    while b < text.len()
        && !(b > 0 && stop(b - 1) && bytes[b].is_ascii_whitespace())
        && !bytes[b..].starts_with(b"\n\n")
    {
        b += 1;
    }
    let (a, b) = if b - a > MAX_SENTENCE {
        (
            text[..start].rfind('\n').map_or(0, |i| i + 1),
            text[end..].find('\n').map_or(text.len(), |i| end + i),
        )
    } else {
        (a, b)
    };
    let span = text.get(a..b)?;
    Some(span.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// A particular's declared fields as the window read them, each supported only
/// by what `document` holds; an identity its metadata source names fills an
/// unknown one. One reading for a claim's subject and a particular of its own.
fn particular_fields(
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
    subject: &OntologyTypeDecl,
    particular: &Value,
    document: &SourceDocument,
    named: &[(String, Vec<Candidate>)],
    particulars: &[Value],
) -> std::collections::BTreeMap<String, DocumentReadField> {
    subject_attrs(policies, index, &subject.name)
        .into_iter()
        .map(|a| {
            let entry = &particular["fields"][&a.name];
            let (value, evidence) = match entry {
                Value::Object(o) if o.contains_key("value") => {
                    (o.get("value"), o.get("quote").map(text))
                }
                Value::Null => (None, None),
                other => (Some(other), None),
            };
            let cite = evidence.as_deref();
            let mut r = reading(a, value, cite, document, named, particulars, index);
            if matches!(r, DocumentReadField::Unknown { .. }) {
                r = sourced_identity(subject, a, &particular["name"], index, named).unwrap_or(r);
            }
            (a.name.clone(), r)
        })
        .collect()
}

/// One outcome per supplied document, in order, from the window's answer.
/// `owns` are the documents' own lines (`own_text`), each claim's quote
/// widened to its sentence in them (`sentence_of`); a claim whose quote is
/// instead in the lines its document quotes or embeds (`quoted`,
/// `quoted_text`) is refused, so only a document's own lines are cited;
/// `dropped` elements of the answer did not parse (`answer::parse`) and count
/// as lost.
#[allow(clippy::too_many_arguments)]
fn envelope(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
    contract: &Contract<'_>,
    labels: &[String],
    owns: &[String],
    quoted: &[String],
    answer: &Value,
    dropped: usize,
) -> Vec<DocumentReadOutcome> {
    let empty = Vec::new();
    let particulars = answer["particulars"].as_array().unwrap_or(&empty);
    let claims = answer["claims"].as_array().unwrap_or(&empty);
    let named_types = contract.named(policies, index);
    let mut outcomes: Vec<(DocumentReadOutcome, Vec<(String, Vec<Candidate>)>)> = chapter
        .source_documents
        .iter()
        .map(|doc| {
            let named = named_types
                .iter()
                .map(|t| (t.name.clone(), candidates(doc, policies, t, &[]).shown))
                .collect();
            (
                DocumentReadOutcome {
                    document_id: doc.key().to_string(),
                    status: DocumentReadStatus::Read,
                    reason: None,
                    claims: Vec::new(),
                    refused: Vec::new(),
                    particulars: Vec::new(),
                },
                named,
            )
        })
        .collect();
    let mut lost = dropped;
    let mut carried = 0usize;
    for claim in claims {
        let kind = text(&claim["kind"]);
        let Some(k) = labels.iter().position(|l| *l == text(&claim["document"])) else {
            lost += 1;
            warn!(chapter = %chapter.chapter_id, %kind, document = %claim["document"], "document_read/window: a claim names no document of the window; dropped");
            continue;
        };
        let document = &chapter.source_documents[k];
        let (outcome, named) = &mut outcomes[k];
        let refuse = |outcome: &mut DocumentReadOutcome, reason: String| {
            debug!(document = %outcome.document_id, %kind, %reason, "document_read/window: claim refused");
            outcome.refused.push(DocumentReadRefusal {
                kind: kind.clone(),
                reason,
            });
        };
        let Some((decl, subject)) = contract.kinds.iter().find(|(d, _)| d.name == kind) else {
            refuse(
                outcome,
                format!("`{kind}` is not a statement kind this recipe reads"),
            );
            continue;
        };
        let about = text(&claim["about"]);
        let Some(particular) = particulars.iter().find(|p| text(&p["ref"]) == about) else {
            refuse(
                outcome,
                format!("it is about `{about}`, which names no particular"),
            );
            continue;
        };
        if text(&particular["type"]) != subject.name {
            refuse(
                outcome,
                format!(
                    "`{about}` is a {}, not a {}",
                    text(&particular["type"]),
                    subject.name
                ),
            );
            continue;
        }
        let quote = text(&claim["quote"]);
        let quote = match sentence_of(&owns[k], &quote) {
            Some(sentence) => {
                if sentence != quote {
                    trace!(document = %outcome.document_id, %quote, %sentence, "document_read/window: a quote widened to its sentence");
                }
                sentence
            }
            None if sentence_of(&quoted[k], &quote).is_some() => {
                carried += 1;
                refuse(
                    outcome,
                    format!(
                        "its quote is in lines this document quotes or embeds from another, not its own: {quote:?}"
                    ),
                );
                continue;
            }
            None => quote,
        };
        let fields = read_attrs(index, decl)
            .iter()
            .map(|a| {
                let r = reading(
                    a,
                    claim["fields"].get(&a.name),
                    Some(&quote),
                    document,
                    named,
                    particulars,
                    index,
                );
                (a.name.clone(), r)
            })
            .collect();
        let subject_fields = particular_fields(
            policies,
            index,
            subject,
            particular,
            document,
            named,
            particulars,
        );
        let name = Some(text(&particular["name"]))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| about.clone());
        outcome.claims.push(DocumentReadClaim {
            kind: kind.clone(),
            content: quote.clone(),
            subject_type: subject.name.clone(),
            subject_local_ref: format!("{}/{about}", chapter.chapter_id),
            subject_name: name,
            speaker: None,
            evidence: quote,
            fields,
            subject_fields,
        });
    }
    particulars::unstated(
        chapter,
        policies,
        index,
        contract,
        particulars,
        claims,
        owns,
        &mut outcomes,
    );
    let read: usize = outcomes.iter().map(|(o, _)| o.claims.len()).sum();
    let refused: usize = outcomes.iter().map(|(o, _)| o.refused.len()).sum();
    info!(chapter = %chapter.chapter_id, documents = outcomes.len(), particulars = particulars.len(), claims = claims.len(), read, refused, carried, lost, "document_read/window: chapter read");
    outcomes
        .into_iter()
        .map(|(mut o, _)| {
            if o.claims.is_empty() && o.particulars.is_empty() {
                (o.status, o.reason) = if o.refused.is_empty() {
                    (
                        DocumentReadStatus::NothingApplicable,
                        Some(
                            "the window read states no declared statement in this document".into(),
                        ),
                    )
                } else {
                    (
                        DocumentReadStatus::CouldNotJudge,
                        Some(format!(
                            "every statement the window read here ({}) was refused",
                            o.refused.len()
                        )),
                    )
                };
            }
            o
        })
        .collect()
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;

#[path = "window_particulars.rs"]
mod particulars;

#[path = "window_text.rs"]
mod shown;

#[path = "window_answer.rs"]
mod answer;

#[path = "window_refs.rs"]
mod refs;
