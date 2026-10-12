// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compiled reader (ONTOLOGY_METHOD §Reading, the wave plan): the
//! declaration compiled into levels of closed questions about a document,
//! particulars first. Every question opens with the same prefix, the
//! document's declared facts and its numbered lines (`waves_forms.rs`), and
//! each round of a level asks every job's next questions as one forced-choice
//! wave (`waves_jobs.rs`), so the document is read off one shared prefix.
//!
//! Level 1, at the document: each type RESOLVE decides is counted and each of
//! its particulars pointed at where it is first named (About's question); each
//! type a read reference targets and no table holds whole, each particular it
//! names (Mention's, enumerated instead of asked line by line); each claim kind
//! whose subject no particular stands for, each claim it states (Locate's,
//! enumerated). Level 2, per particular: its identifying fields (Choose, Point
//! at the line that states it then its words, Pick among what the document
//! offers); each act about it, two wordings and a cited sentence (`act.rs`'s
//! rule); each claim of a kind about it, pointed at; per level-1 claim, its
//! fields and subject. Level 3, per claim found in level 2: its other fields.
//! The answers assemble into the claims and particulars RESOLVE reads, as the
//! passes reader's do (`passes.rs`).
//!
//! Chosen by `SOVEREIGN_READER=waves` (declared, experiment): the passes
//! reader stays the default until the compiled reader is measured against it.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tracing::{debug, info};

use super::about::{self, About, Count, Particular};
use super::act::{self, Reading};
use super::ask::opening;
use super::folded::Folded;
use super::line_classes::LineClasses;
use super::mention::{self, Mention};
use super::passes::{lines, mentioned, numbered, FieldPlan, Line, Plan};
use super::waves_forms::{self as forms, Target, Who};
use super::waves_jobs::{run, Doc, Enumerate, Job, Spent};
use super::waves_readings::{ActRead, Field};
use super::{pick, prefill};
use super::{DocumentReadClaim, DocumentReadField, DocumentReadOutcome, DocumentReadStatus};
use crate::enrichment::atlas::resolve_records::{ClosedAttr, LABELS, NONE};
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{OntologyPolicies, OntologyTypeDecl};
use crate::enrichment::pipeline::types::ChapterInput;
use crate::error::{Error, Result};
use crate::InferenceFn;

/// Whether the compiled reader reads documents (`SOVEREIGN_READER=waves`).
pub(super) fn chosen() -> bool {
    std::env::var("SOVEREIGN_READER").is_ok_and(|v| v == "waves")
}

/// What each level asks, compiled from the contract alone (a pure function
/// of it: renaming every type and attribute renames the questions only).
pub(super) struct Levels<'a> {
    /// Types RESOLVE decides: counted, their particulars pointed at.
    pub(super) decided: Vec<&'a OntologyTypeDecl>,
    /// Types a read reference targets that the text names: enumerated.
    pub(super) mentioned: Vec<&'a OntologyTypeDecl>,
    /// Kinds asked line by line in the passes reader whose subject is not
    /// decided: their claims enumerated at the document.
    pub(super) at_document: Vec<usize>,
    /// Such kinds whose subject is decided: their claims enumerated per
    /// particular (at the document when it is about none).
    pub(super) per_particular: Vec<usize>,
    /// Kinds a protocol rule names: read as acts per particular.
    pub(super) acts: Vec<usize>,
}

impl<'a> Levels<'a> {
    pub(super) fn of(plan: &Plan<'a>) -> Self {
        let decided_name = |n: &str| plan.decided.iter().any(|d| d.name == n);
        let mentioned = plan
            .picked
            .iter()
            .copied()
            .filter(|t| mentioned(t) && !decided_name(&t.name))
            .collect();
        let located = plan.located();
        Self {
            decided: plan.decided.clone(),
            mentioned,
            at_document: located
                .iter()
                .copied()
                .filter(|&k| !decided_name(&plan.kinds[k].subject.name))
                .collect(),
            per_particular: located
                .iter()
                .copied()
                .filter(|&k| decided_name(&plan.kinds[k].subject.name))
                .collect(),
            acts: (0..plan.kinds.len())
                .filter(|&k| !plan.kinds[k].keys.is_empty())
                .collect(),
        }
    }
}

/// Read every supplied document of `chapter` by the compiled plan.
pub async fn read(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    classes: &LineClasses,
    infer: &InferenceFn,
) -> Result<String> {
    let plan = Plan::of(policies);
    if plan.kinds.is_empty() {
        return Err(Error::InvalidInput(
            "the compiled reader found no eligible declared claim kind".into(),
        ));
    }
    let levels = Levels::of(&plan);
    let mut documents = Vec::with_capacity(chapter.source_documents.len());
    let mut spent = Spent::default();
    for document in &chapter.source_documents {
        documents.push(
            read_document(
                document, &plan, &levels, policies, classes, infer, &mut spent,
            )
            .await,
        );
    }
    info!(
        chapter = %chapter.chapter_id,
        documents = documents.len(),
        claims = documents.iter().map(|d| d.claims.len()).sum::<usize>(),
        calls = spent.questions,
        rounds = spent.rounds,
        reader = "waves",
        "document_read/passes: chapter read"
    );
    serde_json::to_string(&json!({ "documents": documents })).map_err(|error| {
        Error::Serialization(format!(
            "compiled reader envelope cannot be serialized: {error}"
        ))
    })
}

/// A claim being assembled: its kind, what it is about, its lines (indices),
/// and its readings as they come in.
struct Stated {
    kind: usize,
    particular: Option<(usize, usize)>,
    first: usize,
    last: usize,
    keyed: BTreeMap<String, Reading>,
    fields: BTreeMap<String, DocumentReadField>,
    subject_fields: Option<BTreeMap<String, DocumentReadField>>,
}

/// Which side of a claim, or which particular, a field job reads for.
#[derive(Clone, Copy)]
enum For {
    Particular(usize, usize),
    Claim(usize),
    Subject(usize),
}

#[allow(clippy::too_many_arguments)]
async fn read_document(
    document: &SourceDocument,
    plan: &Plan<'_>,
    levels: &Levels<'_>,
    policies: &OntologyPolicies,
    classes: &LineClasses,
    infer: &InferenceFn,
    spent: &mut Spent,
) -> DocumentReadOutcome {
    let id = document.key().to_string();
    let raw = document.raw_body();
    let lines = lines(&raw);
    let outcome = |status, reason: String| DocumentReadOutcome {
        document_id: id.clone(),
        status,
        reason: Some(reason),
        claims: Vec::new(),
        refused: Vec::new(),
        particulars: Vec::new(),
    };
    if lines.is_empty() {
        return outcome(
            DocumentReadStatus::NothingApplicable,
            "the document has no text".into(),
        );
    }
    let facts = prefill::facts(document, policies);
    let texts: Vec<&str> = lines.iter().map(|l| &raw[l.start..l.end]).collect();
    let mut asked_mask = vec![true; lines.len()];
    for (i, carried) in classes.classes(&id, &texts).into_iter().enumerate() {
        if let Some(carried) = carried {
            debug!(document = %id, line = lines[i].n, class = carried.class.label(), from = carried.from, "document_read/waves: line not asked");
            asked_mask[i] = false;
        }
    }
    let view = numbered(&raw, &lines);
    let mut d = Doc {
        infer,
        document: &id,
        prefix: opening(&facts, &view),
        body: &raw,
        lines: &lines,
        asked: lines
            .iter()
            .zip(&asked_mask)
            .filter(|(_, a)| **a)
            .map(|(l, _)| l)
            .collect(),
        units: act::units(&raw, &lines, &asked_mask),
        candidates: BTreeMap::new(),
    };
    let mut cost = Spent::default();

    // Level 1: what the document holds, at the document.
    let mut l1: Vec<Job> = Vec::new();
    for t in &levels.decided {
        l1.push(Job::Enumerate(Enumerate::new(
            Target::Decided(t),
            None,
            t.name.clone(),
        )));
    }
    for t in &levels.mentioned {
        l1.push(Job::Enumerate(Enumerate::new(
            Target::Mentioned(t),
            None,
            format!("{} named", t.name),
        )));
    }
    for &k in &levels.at_document {
        let decl = plan.kinds[k].decl;
        l1.push(Job::Enumerate(Enumerate::new(
            Target::Kind(decl),
            None,
            decl.name.clone(),
        )));
    }
    run(&d, &mut l1, "particulars", &mut cost).await;
    let enumerated: Vec<Enumerate> = l1
        .into_iter()
        .filter_map(|j| match j {
            Job::Enumerate(e) => Some(e),
            _ => None,
        })
        .collect();
    if !enumerated.is_empty() && enumerated.iter().all(|e| !e.answered) {
        spent.questions += cost.questions;
        spent.rounds += cost.rounds;
        return outcome(
            DocumentReadStatus::CouldNotJudge,
            format!("every level-1 question ({}) was refused", cost.questions),
        );
    }
    let (decided, rest) = enumerated.split_at(levels.decided.len());
    let (named, at_document) = rest.split_at(levels.mentioned.len());
    let abouts: Vec<About> = decided
        .iter()
        .map(|e| About {
            // A count never answered (the level ended at its round bound first) is unjudged, never "not about".
            count: e.count.unwrap_or(Count::Unjudged),
            particulars: e.found.clone(),
            complete: e.complete,
        })
        .collect();
    let mentions: Vec<Mention> = named
        .iter()
        .flat_map(|e| {
            let of = e.target.decl().name.clone();
            e.found.iter().map(move |p| Mention {
                of: of.clone(),
                line: p.line,
                text: p.words.clone(),
            })
        })
        .collect();
    d.candidates = pick::per_type(document, policies, &plan.picked, &mentions);
    for t in plan.subjects_for(&(0..plan.kinds.len()).collect::<Vec<_>>(), policies) {
        d.candidates
            .entry(t.name.clone())
            .or_insert_with(|| pick::candidates(document, policies, t, &[]));
    }

    // Level 2: per particular, and per claim level 1 found.
    let mut stated: Vec<Stated> = Vec::new();
    let mut l2: Vec<Job> = Vec::new();
    let mut l2_for: Vec<Option<For>> = Vec::new();
    for (t, (of, about)) in levels.decided.iter().zip(&abouts).enumerate() {
        let Some(kind) = plan.kinds.iter().find(|k| k.subject.name == of.name) else {
            continue;
        };
        for (p, named) in about.particulars.iter().enumerate() {
            for field in &kind.subject_fields {
                l2.push(Job::Field(Field::new(
                    field,
                    particular(of, named),
                    named.span.clone(),
                    format!("{} {}", of.name, named.local_ref()),
                )));
                l2_for.push(Some(For::Particular(t, p)));
            }
        }
    }
    for &k in &levels.acts {
        let kind = &plan.kinds[k];
        let Some(t) = levels
            .decided
            .iter()
            .position(|o| o.name == kind.subject.name)
        else {
            continue;
        };
        let found = &abouts[t].particulars;
        let referents: Vec<(Option<(usize, usize)>, Option<&Particular>)> = if found.len() > 1 {
            (0..found.len())
                .map(|p| (Some((t, p)), Some(&found[p])))
                .collect()
        } else {
            vec![(found.first().map(|_| (t, 0)), None)]
        };
        let keys: Vec<&ClosedAttr> = kind
            .fields
            .iter()
            .filter_map(|f| match f {
                FieldPlan::Choose(c) if kind.keys.contains(&c.name) => Some(c),
                _ => None,
            })
            .collect();
        for (r, (at, named)) in referents.into_iter().enumerate() {
            let who = match named {
                Some(p) => particular(kind.subject, p),
                None => Who::It { of: kind.subject },
            };
            l2.push(Job::Act(ActRead::new(
                k,
                at,
                named.cloned(),
                who,
                keys.clone(),
                kind.subject,
                format!("{} r{r}", kind.decl.name),
            )));
            l2_for.push(None);
        }
    }
    let mut per_particular: Vec<(usize, Option<(usize, usize)>)> = Vec::new();
    for &k in &levels.per_particular {
        let kind = &plan.kinds[k];
        let t = levels
            .decided
            .iter()
            .position(|o| o.name == kind.subject.name)
            .expect("per-particular kinds are about a decided type");
        let found = &abouts[t].particulars;
        if found.is_empty() {
            l2.push(Job::Enumerate(Enumerate::new(
                Target::Kind(kind.decl),
                None,
                kind.decl.name.clone(),
            )));
            l2_for.push(None);
            per_particular.push((k, None));
        }
        for (p, named) in found.iter().enumerate() {
            l2.push(Job::Enumerate(Enumerate::new(
                Target::Kind(kind.decl),
                Some(particular(kind.subject, named)),
                format!("{} of {}", kind.decl.name, named.local_ref()),
            )));
            l2_for.push(None);
            per_particular.push((k, Some((t, p))));
        }
    }
    for (e, &k) in at_document.iter().zip(&levels.at_document) {
        for claim in &e.found {
            let s = stated.len();
            stated.push(Stated::new(k, None, claim.line - 1, claim.line - 1));
            claim_jobs(plan, &stated[s], s, &d, true, &mut l2, &mut l2_for);
        }
    }
    run(&d, &mut l2, "readings", &mut cost).await;

    // What level 2 read: identities, acts, claims per particular.
    let mut identities: BTreeMap<(usize, usize), BTreeMap<String, DocumentReadField>> =
        BTreeMap::new();
    for (t, about) in abouts.iter().enumerate() {
        for p in 0..about.particulars.len() {
            identities.insert((t, p), BTreeMap::new());
        }
    }
    let mut kind_enums = per_particular.into_iter();
    let mut l3: Vec<Job> = Vec::new();
    let mut l3_for: Vec<Option<For>> = Vec::new();
    for (job, to) in l2.into_iter().zip(l2_for) {
        match (job, to) {
            (Job::Field(f), Some(to)) => file(&mut identities, &mut stated, to, f.read),
            (Job::Act(a), _) => {
                let Some(cited) = a.cited.clone() else {
                    continue;
                };
                let s = stated.len();
                let mut st = Stated::new(a.kind, a.particular, cited.start, cited.end - 1);
                st.keyed = a.read;
                stated.push(st);
                claim_jobs(
                    plan,
                    &stated[s],
                    s,
                    &d,
                    a.particular.is_none(),
                    &mut l3,
                    &mut l3_for,
                );
            }
            (Job::Enumerate(e), _) => {
                let (k, at) = kind_enums
                    .next()
                    .expect("one per-particular enumeration per entry");
                for claim in &e.found {
                    let s = stated.len();
                    stated.push(Stated::new(k, at, claim.line - 1, claim.line - 1));
                    claim_jobs(plan, &stated[s], s, &d, at.is_none(), &mut l3, &mut l3_for);
                }
            }
            (Job::Field(_), None) => {}
        }
    }

    // Level 3: the fields of what level 2 found.
    run(&d, &mut l3, "claims", &mut cost).await;
    for (job, to) in l3.into_iter().zip(l3_for) {
        if let (Job::Field(f), Some(to)) = (job, to) {
            file(&mut identities, &mut stated, to, f.read);
        }
    }
    spent.questions += cost.questions;
    spent.rounds += cost.rounds;

    // Assembly: claims, then the particulars no claim is about, then mentions.
    let folded = Folded::of(&raw);
    let mut claims = Vec::with_capacity(stated.len());
    for st in &stated {
        let kind = &plan.kinds[st.kind];
        let (first, last) = (&lines[st.first], &lines[st.last]);
        let evidence = raw[first.start..last.end].to_string();
        let local_ref = match st.particular {
            Some((t, p)) => abouts[t].particulars[p].local_ref(),
            None => format!("l{}-{}", first.n, last.n),
        };
        let mut fields = st.fields.clone();
        for (key, reading) in &st.keyed {
            fields.insert(key.clone(), reading.field(&evidence));
        }
        let subject_fields = match st.particular {
            Some(key) => identities[&key].clone(),
            // No subject field job ran only when the kind declares no subject field.
            None => st.subject_fields.clone().unwrap_or_default(),
        };
        let at = folded.span(first.start..last.end);
        claims.push(DocumentReadClaim {
            kind: kind.decl.name.clone(),
            content: folded.text[at].to_string(),
            subject_type: kind.subject.name.clone(),
            subject_name: format!("{} {local_ref}", kind.subject.name),
            subject_local_ref: local_ref,
            speaker: None,
            evidence,
            fields,
            subject_fields,
        });
    }
    let stated_about: Vec<Option<(usize, usize)>> = stated.iter().map(|s| s.particular).collect();
    let about_doc = about::Doc {
        infer,
        facts: &facts,
        document: &id,
        body: &raw,
        lines: &lines,
        view: &view,
    };
    let mut particulars = about::unstated(
        &about_doc,
        &levels.decided,
        &abouts,
        &stated_about,
        identities,
    );
    particulars.extend(mention::particulars(
        document,
        policies,
        &raw,
        &lines,
        &levels.mentioned,
        &mentions,
    ));
    info!(document = %id, questions = cost.questions, rounds = cost.rounds, claims = claims.len(), particulars = particulars.len(), "document_read/waves: document read");
    if claims.is_empty() && particulars.is_empty() {
        return outcome(
            DocumentReadStatus::NothingApplicable,
            format!(
                "the compiled reader found nothing declared ({} questions)",
                cost.questions
            ),
        );
    }
    DocumentReadOutcome {
        document_id: id,
        status: DocumentReadStatus::Read,
        reason: None,
        claims,
        refused: Vec::new(),
        particulars,
    }
}

impl Stated {
    fn new(kind: usize, particular: Option<(usize, usize)>, first: usize, last: usize) -> Self {
        Self {
            kind,
            particular,
            first,
            last,
            keyed: BTreeMap::new(),
            fields: BTreeMap::new(),
            subject_fields: None,
        }
    }
}

fn particular<'a>(of: &'a OntologyTypeDecl, p: &Particular) -> Who<'a> {
    Who::Particular {
        of,
        line: p.line,
        words: p.words.clone(),
    }
}

/// The field jobs a claim needs: each of its kind's fields an act did not
/// read, and, when no particular stands for its subject, its subject's.
fn claim_jobs<'a>(
    plan: &'a Plan<'a>,
    st: &Stated,
    s: usize,
    d: &Doc<'_>,
    own_subject: bool,
    jobs: &mut Vec<Job<'a>>,
    to: &mut Vec<Option<For>>,
) {
    let kind = &plan.kinds[st.kind];
    let (first, last) = (&d.lines[st.first], &d.lines[st.last]);
    let who = Who::Claim {
        kind: kind.decl,
        lines: (first.n, last.n),
        text: d.body[first.start..last.end].to_string(),
    };
    let item = format!("{} l{}-{}", kind.decl.name, first.n, last.n);
    for field in &kind.fields {
        if field.names().iter().all(|n| st.keyed.contains_key(*n)) && !st.keyed.is_empty() {
            continue;
        }
        jobs.push(Job::Field(Field::new(
            field,
            who.clone(),
            first.start..last.end,
            item.clone(),
        )));
        to.push(Some(For::Claim(s)));
    }
    if own_subject {
        for field in &kind.subject_fields {
            jobs.push(Job::Field(Field::new(
                field,
                who.clone(),
                first.start..last.end,
                item.clone(),
            )));
            to.push(Some(For::Subject(s)));
        }
    }
}

fn file(
    identities: &mut BTreeMap<(usize, usize), BTreeMap<String, DocumentReadField>>,
    stated: &mut [Stated],
    to: For,
    read: Vec<(String, DocumentReadField)>,
) {
    match to {
        For::Particular(t, p) => identities.entry((t, p)).or_default().extend(read),
        For::Claim(s) => stated[s].fields.extend(read),
        For::Subject(s) => stated[s]
            .subject_fields
            .get_or_insert_with(BTreeMap::new)
            .extend(read),
    }
}

/// What the compiled reader asks, rendered on a fixed sample: any change to a
/// question's wording changes this value, and so the contract fingerprint
/// that keys cached reads (`schema::contract_value`).
pub(super) fn contract_value(policies: &OntologyPolicies) -> Value {
    let plan = Plan::of(policies);
    let levels = Levels::of(&plan);
    let body = "first line\nsecond line";
    let ls: Vec<Line> = lines(body);
    let probe = OntologyTypeDecl {
        name: "type".into(),
        description: "description".into(),
        identity_criterion: Some("criterion".into()),
        ..Default::default()
    };
    let attr = ClosedAttr {
        name: "attribute".into(),
        description: String::new(),
        values: vec!["value".into()],
    };
    let who = Who::Particular {
        of: &probe,
        line: 1,
        words: "first".into(),
    };
    let window = forms::window(body, &ls.iter().collect::<Vec<_>>());
    let prefix = opening(prefill::FACTS_HEADER, &numbered(body, &ls));
    let q = |suffix: String| forms::question(&prefix, &suffix, &["A", NONE]);
    let samples = [
        q(forms::count(Target::Decided(&probe))),
        q(forms::count(Target::Mentioned(&probe))),
        q(forms::count(Target::Kind(&probe))),
        q(forms::line(
            Target::Decided(&probe),
            None,
            &[(1, "first".into())],
            &window,
            &["A", "B", NONE],
        )),
        q(forms::line(
            Target::Kind(&probe),
            Some(&who),
            &[],
            &window,
            &["A", "B", NONE],
        )),
        q(forms::choose(&who, &attr, &["A", NONE])),
        q(forms::act(&who, &attr, 0, &["A", NONE])),
        q(forms::act(&who, &attr, 1, &["A", NONE])),
        q(forms::word_start(
            "head\n",
            "Line 1: \"first line\"",
            &["first", "line"],
            "what",
            &["A", "B", NONE],
        )),
        q(forms::word_end(
            "head\n",
            "Line 1: \"first line\"",
            &["first", "line"],
            "what",
            &["A", "B"],
        )),
        q(forms::cite("Fact: f.", &[], &[])),
    ];
    json!({
        "reader": "waves",
        "max_particulars": about::MAX_PARTICULARS,
        "labels": LABELS.len(),
        "levels": [levels.decided.len(), levels.mentioned.len(), levels.at_document.len(), levels.per_particular.len(), levels.acts.len()],
        "questions": samples.iter().map(|p| json!([p.system, p.user])).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
#[path = "waves_tests.rs"]
mod tests;
