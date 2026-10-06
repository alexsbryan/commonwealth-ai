//! Proposers for RESOLVE: declared structure and domain-free retrieval that
//! bound which open records the model is shown, each record with the reasons
//! it was offered for (ONTOLOGY_METHOD.md §Invariants 3). A proposer never
//! decides; the caller measures its recall against gold.

use std::collections::HashMap;

use tracing::debug;

use super::{Document, DocumentResolution, Proposal, Reason};
use crate::enrichment::ontology::DocumentStamp;

/// The proposers a declaration selects, offering one list: the records of the
/// declared thread first (when the recipe declares `change.document.thread`),
/// then those of similar documents. A record both offer carries both reasons.
#[derive(Debug)]
pub struct Proposers {
    thread: Option<SameThread>,
    similar: SimilarDocuments,
}

impl Proposers {
    pub fn new(declares_thread: bool, similar: SimilarDocuments) -> Self {
        Self {
            thread: declares_thread.then(SameThread::default),
            similar,
        }
    }

    pub fn propose(&self, doc: Document<'_>) -> Vec<Proposal> {
        let mut out = self
            .thread
            .as_ref()
            .map_or_else(Vec::new, |t| t.propose(doc));
        for p in self.similar.propose(doc) {
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
        self.similar.observe(doc, resolution);
    }

    /// What a run's summary names as its proposer.
    pub fn describe(&self) -> String {
        let s = &self.similar;
        format!(
            "{}similar_documents(neighbours {}, max_candidates {}, min_similarity {})",
            if self.thread.is_some() {
                "same_thread + "
            } else {
                ""
            },
            s.neighbours,
            s.max_candidates,
            s.min_similarity
        )
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
    tf: HashMap<String, f32>,
    records: Vec<String>,
}

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

    fn weigh(&self, tf: &HashMap<String, f32>) -> HashMap<String, f32> {
        let n = self.seen.len() as f32 + 1.0;
        tf.iter()
            .map(|(t, c)| {
                let df = *self.df.get(t).unwrap_or(&0) as f32;
                (t.clone(), c * (((n + 1.0) / (df + 1.0)).ln() + 1.0))
            })
            .collect()
    }
}

fn term_counts(doc: Document<'_>) -> HashMap<String, f32> {
    let mut tf = HashMap::new();
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

fn norm(v: &HashMap<String, f32>) -> f32 {
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
            outcomes: records
                .iter()
                .map(|r| StatementOutcome {
                    statement: format!("{doc}/{r}"),
                    outcome: Outcome::Decided(super::super::Decision::Opened {
                        record: r.to_string(),
                        cite: None,
                    }),
                    choice: None,
                })
                .collect(),
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
        let got = p.propose(q);
        assert_eq!(got[0].record, "r1", "{got:?}");
        assert_eq!(got[0].reasons[0], Reason::SameThread, "{got:?}");
        let r0 = got.iter().find(|x| x.record == "r0").expect("similar");
        assert!(!r0.same_thread() && r0.similarity() > 0.0, "{got:?}");

        let mut bare = Proposers::new(false, SimilarDocuments::new(3, 10, 0.0));
        bare.observe(b, &resolved("b", &["r1"]));
        assert!(bare.propose(q).iter().all(|x| !x.same_thread()));
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
}
