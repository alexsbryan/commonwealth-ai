//! Proposers for RESOLVE: declared structure and domain-free retrieval that
//! bound which open records the model is shown, each record with the reasons
//! it was offered for (ONTOLOGY_METHOD.md §Invariants 3). A proposer never
//! decides; the caller measures its recall against gold.

use std::collections::{BTreeMap, HashMap};

use tracing::debug;

use super::{Document, DocumentResolution, Proposal, Reason, Statement};
use crate::enrichment::alignment::cosine_similarity;
use crate::enrichment::ontology::DocumentStamp;

/// The proposers a declaration selects, offering one list: the records a
/// document's fields name, then those of its declared thread (when the recipe
/// declares `change.document.thread`), then those nearest its statements in
/// meaning (when the run has an embedder), then those of similar documents.
/// A record several offer carries every reason.
#[derive(Debug)]
pub struct Proposers {
    named: Option<FieldNamed>,
    thread: Option<SameThread>,
    nearest: Option<Nearest>,
    similar: SimilarDocuments,
}

impl Proposers {
    pub fn new(declares_thread: bool, similar: SimilarDocuments) -> Self {
        Self {
            named: None,
            thread: declares_thread.then(SameThread::default),
            nearest: None,
            similar,
        }
    }

    /// With the records nearest each statement in meaning offered too.
    pub fn with_nearest(mut self, nearest: Nearest) -> Self {
        self.nearest = Some(nearest);
        self
    }

    /// With the records a document's fields name offered first.
    pub fn with_named(mut self, named: FieldNamed) -> Self {
        self.named = Some(named);
        self
    }

    pub fn propose(&self, doc: Document<'_>, statements: &[Statement]) -> Vec<Proposal> {
        let mut out = self
            .named
            .as_ref()
            .map_or_else(Vec::new, |n| n.propose(doc));
        let thread = self
            .thread
            .as_ref()
            .map_or_else(Vec::new, |t| t.propose(doc));
        let nearest = self
            .nearest
            .as_ref()
            .map_or_else(Vec::new, |n| n.propose(doc, statements));
        for p in thread
            .into_iter()
            .chain(nearest)
            .chain(self.similar.propose(doc))
        {
            match out.iter_mut().find(|q| q.record == p.record) {
                Some(q) => q.reasons.extend(p.reasons),
                None => out.push(p),
            }
        }
        out
    }

    pub fn observe(&mut self, doc: Document<'_>, resolution: &DocumentResolution) {
        if let Some(t) = &mut self.thread {
            t.observe(doc, resolution);
        }
        if let Some(n) = &mut self.nearest {
            n.observe(resolution);
        }
        self.similar.observe(doc, resolution);
    }

    /// What a run's summary names as its proposer.
    pub fn describe(&self) -> String {
        let s = &self.similar;
        format!(
            "{}{}{}similar_documents(neighbours {}, max_candidates {}, min_similarity {})",
            if self.named.is_some() {
                "field_named + "
            } else {
                ""
            },
            if self.thread.is_some() {
                "same_thread + "
            } else {
                ""
            },
            self.nearest
                .as_ref()
                .map_or_else(String::new, |n| format!("nearest(k {}) + ", n.k)),
            s.neighbours,
            s.max_candidates,
            s.min_similarity
        )
    }
}

/// The records a document's own fields name, by document key: a metadata
/// source's records (`resolution_sources` participants), each with the fields
/// naming it. Declared structure, never cut to a cost cap; the only proposer
/// that offers a record before any statement has reached it.
#[derive(Debug, Default)]
pub struct FieldNamed {
    by_doc: HashMap<String, Vec<(String, String)>>,
}

impl FieldNamed {
    /// `by_doc`: document key -> (record id, field) for each field naming one.
    pub fn new(by_doc: HashMap<String, Vec<(String, String)>>) -> Self {
        Self { by_doc }
    }

    pub fn propose(&self, doc: Document<'_>) -> Vec<Proposal> {
        let mut out: Vec<Proposal> = Vec::new();
        for (record, field) in self.by_doc.get(doc.id).into_iter().flatten() {
            let reason = Reason::NamedInField {
                field: field.clone(),
            };
            match out.iter_mut().find(|p| p.record == *record) {
                Some(p) if !p.reasons.contains(&reason) => p.reasons.push(reason),
                Some(_) => {}
                None => out.push(Proposal {
                    record: record.clone(),
                    reasons: vec![reason],
                }),
            }
        }
        debug!(
            document = doc.id,
            records = out.len(),
            "atlas/resolve: records the document's fields name"
        );
        out
    }
}

/// The records nearest a document's statements in meaning: for each
/// statement, the `k` records of its type whose card lies nearest the
/// statement's words and lines by cosine under the run's embedder. A record
/// is carded by what it is shown as (`Record::shown`) when seeded, else by
/// the words of the statement that opened it. Retrieval, never a decision:
/// `k` is a cost knob, and the one proposer that offers a record no field of
/// the document names and no statement of its thread holds, as when a note
/// names a company before any message from it arrives.
#[derive(Debug, Default)]
pub struct Nearest {
    k: usize,
    /// Statement id -> its vector.
    statements: HashMap<String, Vec<f32>>,
    /// Record id -> its card's vector, in the order the records came.
    records: Vec<(String, Vec<f32>)>,
}

impl Nearest {
    pub fn new(
        k: usize,
        statements: HashMap<String, Vec<f32>>,
        records: Vec<(String, Vec<f32>)>,
    ) -> Self {
        Self {
            k,
            statements,
            records,
        }
    }

    pub fn propose(&self, doc: Document<'_>, statements: &[Statement]) -> Vec<Proposal> {
        let mut best: HashMap<&str, f32> = HashMap::new();
        for s in statements {
            let Some(v) = self.statements.get(&s.id) else {
                debug!(document = doc.id, statement = %s.id, "atlas/resolve: a statement with no vector; nearest offers it nothing");
                continue;
            };
            let mut ranked: Vec<(&str, f32)> = self
                .records
                .iter()
                .map(|(id, r)| (id.as_str(), cosine_similarity(v, r)))
                .collect();
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
            for (id, similarity) in ranked.into_iter().take(self.k) {
                let at = best.entry(id).or_insert(similarity);
                *at = at.max(similarity);
            }
        }
        let mut out: Vec<Proposal> = best
            .into_iter()
            .map(|(record, similarity)| Proposal {
                record: record.to_string(),
                reasons: vec![Reason::Nearest { similarity }],
            })
            .collect();
        out.sort_by(|a, b| {
            b.nearness()
                .total_cmp(&a.nearness())
                .then(a.record.cmp(&b.record))
        });
        debug!(
            document = doc.id,
            offered = out.len(),
            records = self.records.len(),
            k = self.k,
            "atlas/resolve: records nearest the document's statements"
        );
        out
    }

    /// A record a statement opened is carded by that statement's words.
    pub fn observe(&mut self, resolution: &DocumentResolution) {
        for o in &resolution.outcomes {
            let Some(record) = o.outcome.record() else {
                continue;
            };
            if record != o.statement || self.records.iter().any(|(id, _)| id == record) {
                continue;
            }
            if let Some(v) = self.statements.get(record) {
                self.records.push((record.to_string(), v.clone()));
            }
        }
    }
}

/// The records holding statements of documents in this document's declared
/// thread, in the order they were opened. Every one is offered: declared
/// structure is never cut to a cost cap.
#[derive(Debug, Default)]
pub struct SameThread {
    records: HashMap<String, Vec<String>>,
}

impl SameThread {
    pub fn propose(&self, doc: Document<'_>) -> Vec<Proposal> {
        let Some(thread) = doc.stamp(DocumentStamp::Thread) else {
            return Vec::new();
        };
        let out: Vec<Proposal> = self
            .records
            .get(thread)
            .into_iter()
            .flatten()
            .map(|r| Proposal {
                record: r.clone(),
                reasons: vec![Reason::SameThread],
            })
            .collect();
        debug!(
            document = doc.id,
            thread,
            candidates = out.len(),
            "atlas/resolve propose: same thread"
        );
        out
    }

    pub fn observe(&mut self, doc: Document<'_>, resolution: &DocumentResolution) {
        let Some(thread) = doc.stamp(DocumentStamp::Thread) else {
            return;
        };
        let held = self.records.entry(thread.to_string()).or_default();
        for r in resolution
            .outcomes
            .iter()
            .filter_map(|o| o.outcome.record())
        {
            if !held.iter().any(|h| h == r) {
                held.push(r.to_string());
            }
        }
    }
}

/// The records of the documents most like this one: TF-IDF cosine over word
/// tokens of title and body, against every document already resolved.
#[derive(Debug)]
pub struct SimilarDocuments {
    /// How many of the most similar documents contribute their records.
    pub neighbours: usize,
    /// At most this many candidates are shown. Both are cost knobs.
    pub max_candidates: usize,
    /// A document less alike than this offers nothing. Chosen with no model
    /// on every example's train fold; 0 offers every neighbour.
    pub min_similarity: f32,
    df: HashMap<String, u32>,
    seen: Vec<Seen>,
}

#[derive(Debug)]
struct Seen {
    id: String,
    /// Ordered, so every sum over it runs in one order: f32 addition is not
    /// associative, and summed in a HashMap's per-process order two documents
    /// of one wording scored 1.0 against each other in one run and a ulp under
    /// in the next, flipping which neighbours were offered (uv-support,
    /// 2026-10-09: the same build gave different records on two runs).
    tf: BTreeMap<String, f32>,
    records: Vec<String>,
}

/// The similar-documents proposer every adopted run was measured with
/// (resolve-prereg.md §Knobs; min_similarity .1, since at .2 ward's recall fell
/// from .698 to .581). The atlas build runs it; `resolve-statements` defaults to it.
pub const NEIGHBOURS: usize = 3;
pub const MAX_CANDIDATES: usize = 12;
pub const MIN_SIMILARITY: f32 = 0.1;

impl SimilarDocuments {
    pub fn new(neighbours: usize, max_candidates: usize, min_similarity: f32) -> Self {
        Self {
            neighbours,
            max_candidates,
            min_similarity,
            df: HashMap::new(),
            seen: Vec::new(),
        }
    }

    /// Candidate records for `doc`, best first, each with the similarity of
    /// the document it came through.
    pub fn propose(&self, doc: Document<'_>) -> Vec<Proposal> {
        let q = self.weigh(&term_counts(doc));
        let qn = norm(&q);
        let mut scored: Vec<(f32, usize)> = self
            .seen
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let d = self.weigh(&s.tf);
                let dot: f32 = q.iter().filter_map(|(t, w)| Some(w * d.get(t)?)).sum();
                let cos = dot / (qn * norm(&d)).max(f32::MIN_POSITIVE);
                (cos > 0.0 && cos >= self.min_similarity).then_some((cos, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(self.neighbours);
        let mut out: Vec<Proposal> = Vec::new();
        for &(similarity, i) in &scored {
            for r in &self.seen[i].records {
                if out.len() < self.max_candidates && !out.iter().any(|p| &p.record == r) {
                    out.push(Proposal {
                        record: r.clone(),
                        reasons: vec![Reason::SimilarDocument { similarity }],
                    });
                }
            }
        }
        debug!(
            document = doc.id,
            neighbours = ?scored.iter().map(|&(c, i)| (self.seen[i].id.as_str(), c)).collect::<Vec<_>>(),
            candidates = out.len(),
            "atlas/resolve propose: similar documents"
        );
        out
    }

    /// Remember a resolved document and the records its statements landed in.
    pub fn observe(&mut self, doc: Document<'_>, resolution: &DocumentResolution) {
        let tf = term_counts(doc);
        for t in tf.keys() {
            *self.df.entry(t.clone()).or_insert(0) += 1;
        }
        let mut records: Vec<String> = Vec::new();
        for o in &resolution.outcomes {
            if let Some(r) = o.outcome.record() {
                if !records.iter().any(|x| x == r) {
                    records.push(r.to_string());
                }
            }
        }
        self.seen.push(Seen {
            id: doc.id.to_string(),
            tf,
            records,
        });
    }

    fn weigh(&self, tf: &BTreeMap<String, f32>) -> BTreeMap<String, f32> {
        let n = self.seen.len() as f32 + 1.0;
        tf.iter()
            .map(|(t, c)| {
                let df = *self.df.get(t).unwrap_or(&0) as f32;
                (t.clone(), c * (((n + 1.0) / (df + 1.0)).ln() + 1.0))
            })
            .collect()
    }
}

fn term_counts(doc: Document<'_>) -> BTreeMap<String, f32> {
    let mut tf = BTreeMap::new();
    let text = doc.title.into_iter().chain([doc.body]);
    for part in text {
        for t in part
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.chars().count() > 1)
        {
            *tf.entry(t.to_lowercase()).or_insert(0.0) += 1.0;
        }
    }
    tf
}

fn norm(v: &BTreeMap<String, f32>) -> f32 {
    v.values().map(|w| w * w).sum::<f32>().sqrt()
}

#[cfg(test)]
mod tests {
    use super::super::{Outcome, StatementOutcome};
    use super::*;

    fn resolved(doc: &str, records: &[&str]) -> DocumentResolution {
        DocumentResolution {
            document: doc.into(),
            candidates: vec![],
            calls: 0,
            vetoed: 0,
            unread: 0,
            necessary: Default::default(),
            outcomes: records
                .iter()
                .map(|r| StatementOutcome {
                    statement: format!("{doc}/{r}"),
                    outcome: Outcome::Decided(super::super::Decision::Opened {
                        record: r.to_string(),
                        cite: None,
                    }),
                    choice: None,
                    by: Vec::new(),
                })
                .collect(),
            settles: false,
        }
    }

    #[test]
    fn the_nearest_document_offers_its_records_first_and_the_cap_holds() {
        let mut p = SimilarDocuments::new(2, 3, 0.0);
        let docs = [
            ("a", "A shooting in Salisbury left a girl dead on Sunday."),
            ("b", "Council votes on the harbour budget in Portsmouth."),
            (
                "c",
                "Police in Salisbury arrested a man after Sunday's shooting.",
            ),
        ];
        let recs: [&[&str]; 3] = [&["r0", "r1"], &["r2"], &["r3", "r4"]];
        for ((id, body), r) in docs.iter().zip(recs) {
            p.observe(
                Document {
                    id,
                    title: None,
                    body,
                    stamps: &[],
                },
                &resolved(id, r),
            );
        }
        let q = Document {
            id: "q",
            title: Some("Salisbury shooting"),
            body: "The girl shot in Salisbury on Sunday has been named.",
            stamps: &[],
        };
        let got = p.propose(q);
        assert_eq!(got.len(), 3, "{got:?}");
        let got: Vec<String> = got.into_iter().map(|p| p.record).collect();
        assert!(
            got[..2]
                .iter()
                .all(|r| ["r0", "r1", "r3", "r4"].contains(&r.as_str())),
            "{got:?}"
        );
        assert!(!got.contains(&"r2".to_string()), "{got:?}");
    }

    fn threaded<'a>(id: &'a str, thread: Option<&'a str>, body: &'a str) -> Document<'a> {
        let stamps: Vec<(DocumentStamp, String)> = thread
            .map(|t| (DocumentStamp::Thread, t.to_string()))
            .into_iter()
            .collect();
        Document {
            id,
            title: None,
            body,
            stamps: Vec::leak(stamps),
        }
    }

    #[test]
    fn the_declared_thread_offers_first_and_a_record_both_offer_carries_both_reasons() {
        let mut p = Proposers::new(true, SimilarDocuments::new(3, 10, 0.0));
        let a = threaded("a", Some("7"), "Install fails on Windows with a long path.");
        p.observe(a, &resolved("a", &["r0"]));
        let b = threaded("b", Some("9"), "Lockfile drift after upgrade.");
        p.observe(b, &resolved("b", &["r1"]));
        let q = threaded(
            "q",
            Some("9"),
            "Install fails on Windows when the path is long.",
        );
        let got = p.propose(q, &[]);
        assert_eq!(got[0].record, "r1", "{got:?}");
        assert_eq!(got[0].reasons[0], Reason::SameThread, "{got:?}");
        let r0 = got.iter().find(|x| x.record == "r0").expect("similar");
        assert!(!r0.same_thread() && r0.similarity() > 0.0, "{got:?}");

        let mut bare = Proposers::new(false, SimilarDocuments::new(3, 10, 0.0));
        bare.observe(b, &resolved("b", &["r1"]));
        assert!(bare.propose(q, &[]).iter().all(|x| !x.same_thread()));
    }

    #[test]
    fn a_document_below_the_similarity_floor_offers_nothing() {
        let mut p = SimilarDocuments::new(3, 10, 0.99);
        p.observe(
            threaded("a", None, "Install fails on Windows."),
            &resolved("a", &["r0"]),
        );
        assert!(p
            .propose(threaded("q", None, "Install fails on Linux."))
            .is_empty());
    }

    /// Documents of one wording score exactly alike against a query, so the
    /// earliest seen wins a tie in every run (the sums run in one order).
    #[test]
    fn documents_of_one_wording_tie_exactly_and_the_earliest_is_offered() {
        let words: Vec<String> = (0..60)
            .map(|i| format!("w{i} ").repeat(i % 7 + 1))
            .collect();
        let body: &'static str = Box::leak(words.concat().into_boxed_str());
        let ids: Vec<String> = (0..40).map(|i| format!("d{i}")).collect();
        let mut p = SimilarDocuments::new(40, 40, 0.0);
        for (i, id) in ids.iter().enumerate() {
            p.observe(threaded(id, None, body), &resolved(id, &[&format!("r{i}")]));
        }
        let got = p.propose(threaded("q", None, body));
        let sims: Vec<f32> = got.iter().map(Proposal::similarity).collect();
        assert_eq!(sims.len(), 40);
        assert!(
            sims.windows(2).all(|w| w[0].to_bits() == w[1].to_bits()),
            "{sims:?}"
        );
        assert_eq!(got[0].record, "r0");
    }

    #[test]
    fn nothing_seen_proposes_nothing() {
        let p = SimilarDocuments::new(3, 10, 0.0);
        assert!(p
            .propose(Document {
                id: "q",
                title: None,
                body: "anything",
                stamps: &[],
            })
            .is_empty());
    }

    /// A token-bag stand-in for the run's embedder: one dimension per word of
    /// `vocab`, so cosine ranks by the words two texts share.
    fn bag(text: &str, vocab: &[&str]) -> Vec<f32> {
        let words: Vec<String> = text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .map(str::to_string)
            .collect();
        vocab
            .iter()
            .map(|v| words.iter().filter(|w| w == v).count() as f32)
            .collect()
    }

    /// The stand-in ranks as its contract says, so the ranking test below
    /// measures the proposer and not the double.
    #[test]
    fn the_token_bag_double_ranks_by_shared_words() {
        let vocab = ["linden", "court", "trust", "elm", "row"];
        let q = bag("Linden Court Trust rang", &vocab);
        let near = bag("name: Linden Court Trust", &vocab);
        let far = bag("name: Elm Row", &vocab);
        assert!(cosine_similarity(&q, &near) > cosine_similarity(&q, &far));
        assert_eq!(cosine_similarity(&q, &far), 0.0);
    }

    /// With more records than `k`, a statement is offered its `k` nearest
    /// records by card, nearest first, and never a record sharing nothing with
    /// it: retrieval bounds the question, every seeded record a candidate.
    #[test]
    fn the_nearest_records_are_offered_when_a_type_has_more_than_k() {
        let names = [
            "Linden Court Trust",
            "Elm Row Joinery",
            "Maple Lane Mill",
            "Hazel Yard Works",
            "Rowan Quay Stores",
            "Aspen Gate Supply",
            "Cedar Hill Timber",
            "Willow Bank Crafts",
            "Birch Hall Trust",
            "Alder Court Joinery",
            "Larch Moor Sawmill",
            "Holly Row Market",
        ];
        let vocab: Vec<String> = names
            .iter()
            .flat_map(|n| n.split_whitespace())
            .map(str::to_lowercase)
            .collect();
        let vocab: Vec<&str> = vocab.iter().map(String::as_str).collect();
        let records: Vec<(String, Vec<f32>)> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("c{i}"), bag(&format!("name: {n}"), &vocab)))
            .collect();
        let statement = Statement {
            id: "s1".into(),
            start: 0,
            end: 0,
            keys: Default::default(),
            read: Default::default(),
        };
        let mut statements = HashMap::new();
        statements.insert(
            "s1".to_string(),
            bag(
                "The charity Linden Court Trust rang this afternoon.",
                &vocab,
            ),
        );
        let nearest = Nearest::new(3, statements, records);
        let got = nearest.propose(
            Document {
                id: "d1",
                title: None,
                body: "",
                stamps: &[],
            },
            &[statement],
        );
        let offered: Vec<&str> = got.iter().map(|p| p.record.as_str()).collect();
        assert_eq!(offered.len(), 3, "{offered:?}");
        assert_eq!(
            offered[0], "c0",
            "Linden Court Trust is nearest: {offered:?}"
        );
        assert!(
            !offered.contains(&"c2"),
            "a record sharing no word was offered: {offered:?}"
        );
        assert!(got.iter().all(|p| p.nearness() > 0.0));
    }
}
