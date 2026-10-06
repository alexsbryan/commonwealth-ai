//! Proposers for RESOLVE: domain-free retrieval that bounds which open records
//! the model is shown (ONTOLOGY_METHOD.md §Invariants 3). A proposer never
//! decides; the caller measures its recall against gold.

use std::collections::HashMap;

use tracing::debug;

use super::{Document, DocumentResolution};

/// The records of the documents most like this one: TF-IDF cosine over word
/// tokens of title and body, against every document already resolved.
#[derive(Debug)]
pub struct SimilarDocuments {
    /// How many of the most similar documents contribute their records.
    pub neighbours: usize,
    /// At most this many candidates are shown. Both are cost knobs.
    pub max_candidates: usize,
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
    pub fn new(neighbours: usize, max_candidates: usize) -> Self {
        Self {
            neighbours,
            max_candidates,
            df: HashMap::new(),
            seen: Vec::new(),
        }
    }

    /// Candidate record ids for `doc`, best first.
    pub fn propose(&self, doc: Document<'_>) -> Vec<String> {
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
                (cos > 0.0).then_some((cos, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(self.neighbours);
        let mut out: Vec<String> = Vec::new();
        for &(_, i) in &scored {
            for r in &self.seen[i].records {
                if out.len() < self.max_candidates && !out.contains(r) {
                    out.push(r.clone());
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
                })
                .collect(),
        }
    }

    #[test]
    fn the_nearest_document_offers_its_records_first_and_the_cap_holds() {
        let mut p = SimilarDocuments::new(2, 3);
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
                },
                &resolved(id, r),
            );
        }
        let q = Document {
            id: "q",
            title: Some("Salisbury shooting"),
            body: "The girl shot in Salisbury on Sunday has been named.",
        };
        let got = p.propose(q);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(
            got[..2]
                .iter()
                .all(|r| ["r0", "r1", "r3", "r4"].contains(&r.as_str())),
            "{got:?}"
        );
        assert!(!got.contains(&"r2".to_string()), "{got:?}");
    }

    #[test]
    fn nothing_seen_proposes_nothing() {
        let p = SimilarDocuments::new(3, 10);
        assert!(p
            .propose(Document {
                id: "q",
                title: None,
                body: "anything"
            })
            .is_empty());
    }
}
