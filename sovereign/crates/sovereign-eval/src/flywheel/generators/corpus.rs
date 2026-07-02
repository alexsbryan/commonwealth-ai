// SPDX-License-Identifier: AGPL-3.0-or-later
//! I1 — the autonomous corpus self-supervision generator (FR-IN-1: no human in
//! the loop).
//!
//! PRESENT probes are mined from `Claim` atoms — the agent should retrieve and
//! answer, citing the corpus. ABSENT (should-abstain) probes come from one of
//! two sources ([`AbsentSource`]):
//!
//!   * **CuratedBank** — lift the absent questions from a curated chaos bank.
//!     The bootstrap path that exercises the abstain register before the slice
//!     exists.
//!   * **HeldOutSlice** — mine claims from a withheld, enriched-but-UNINDEXED
//!     document slice. The answer provably exists (a real mined claim) but
//!     provably isn't retrievable (its doc isn't indexed), so honest abstention
//!     is verifiably correct WITH a real witness — and the same indexed/withheld
//!     split is exactly the before/after corpus state I4 (delta) flips.
//!
//! Generation is deterministic: the query is a fixed template over the claim
//! (no LLM rephrase), so a `(n, seed)` battery is reproducible bit-for-bit.

use std::path::{Path, PathBuf};

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;

use crate::chaos_monkey::{ChaosBank, QuestionType};
use crate::flywheel::mining::{mine_claims, MinedClaim};
use crate::flywheel::probe::{chaos_to_probe, AbsentKind, Oracle, Probe, ProbeSource};
use crate::flywheel::Generator;

/// Where the should-abstain probes come from.
#[derive(Debug, Clone)]
pub enum AbsentSource {
    /// Present-only run (no absent probes).
    None,
    /// Lift the absent questions from a curated chaos bank (bootstrap).
    CuratedBank(PathBuf),
    /// Mine claims from a withheld, enriched-but-unindexed slice (Phase 3).
    /// `withheld` is the corpus root holding the withheld docs' `atlas/atoms.json`.
    HeldOutSlice { withheld: PathBuf },
}

#[derive(Debug, Clone)]
pub struct CorpusGenerator {
    pub absent: AbsentSource,
}

impl Default for CorpusGenerator {
    fn default() -> Self {
        Self {
            absent: AbsentSource::None,
        }
    }
}

/// Deterministic interrogative framing of a mined claim. No inference — a fixed
/// template keeps the generator pure and `(n, seed)`-reproducible.
///
/// Phrased as a direct yes/no factual question ("Is it true that …?") so the
/// router sends it to the grounded knowledge-retrieval handler. A live smoke
/// proved that a meta framing ("According to the corpus, is the following
/// accurate …") embeds near the MetalingualQuery / ComplexTask exemplars and
/// misroutes to the tool-planning path (it even triggered a web search) — the
/// probe never reaches the grounded register it is meant to test.
fn claim_query(content: &str) -> String {
    let c = content.trim().trim_end_matches(['.', '!', '?']).trim();
    // Lowercase the leading char for natural "Is it true that <claim>?" phrasing.
    let body = {
        let mut chars = c.chars();
        match chars.next() {
            Some(f) => format!("{}{}", f.to_lowercase(), chars.as_str()),
            None => String::new(),
        }
    };
    format!("Is it true that {body}?")
}

impl CorpusGenerator {
    fn present_probes(&self, n: usize, seed: u64, corpus: &Path) -> Vec<Probe> {
        let mut claims = mine_claims(corpus, true);
        claims.sort_by(|a, b| a.id.cmp(&b.id));
        let mut rng = StdRng::seed_from_u64(seed);
        claims.shuffle(&mut rng);
        claims.truncate(n.min(claims.len()));
        claims
            .iter()
            .filter_map(|c| {
                // Witness keywords come from the CLAIM CONTENT (clean, normalized),
                // NOT the quotable_excerpt — a live run showed raw excerpts can be
                // corrupted source fragments ("ca rgo", "ccomonwealth-daemon") whose
                // tokens no correct answer contains, turning every Present probe into
                // a false Confab. The excerpt is still kept as the supporting_quote.
                let kws = salient_terms(&c.content, 3);
                if kws.is_empty() {
                    return None; // no usable witness → skip (stay fair by construction)
                }
                Some(Probe {
                    id: format!("i1:{}", c.id),
                    query: claim_query(&c.content),
                    qtype: QuestionType::Present,
                    oracle: Oracle::Witness {
                        gold_keywords: kws,
                        supporting_quote: Some(c.excerpt.clone()),
                        distractor_quote: None,
                    },
                    source: ProbeSource::I1Corpus,
                    note: format!("mined Claim atom {}", c.id),
                })
            })
            .collect()
    }

    fn absent_probes(&self, n: usize, seed: u64) -> Vec<Probe> {
        match &self.absent {
            AbsentSource::None => Vec::new(),
            AbsentSource::CuratedBank(path) => match ChaosBank::load(path) {
                Ok(bank) => bank
                    .questions
                    .iter()
                    .filter(|q| q.qtype.is_absent())
                    .take(n)
                    .map(chaos_to_probe)
                    .collect(),
                Err(e) => {
                    eprintln!("[i1] could not load curated absent bank {path:?}: {e}");
                    Vec::new()
                }
            },
            AbsentSource::HeldOutSlice { withheld } => {
                // Mine claims from the withheld (unindexed) slice. The answer
                // provably exists but isn't retrievable → honest abstention,
                // with the mined keywords as the held_out_witness (leak detector).
                let mut claims = mine_claims(withheld, true);
                claims.sort_by(|a, b| a.id.cmp(&b.id));
                let mut rng = StdRng::seed_from_u64(seed ^ 0x5151_5151);
                claims.shuffle(&mut rng);
                claims.truncate(n.min(claims.len()));
                claims
                    .iter()
                    .map(|c: &MinedClaim| Probe {
                        id: format!("i1-absent:{}", c.id),
                        query: claim_query(&c.content),
                        qtype: QuestionType::AbsentAdjacent,
                        oracle: Oracle::Absent {
                            held_out_witness: Some(salient_terms(&c.content, 3)),
                            kind: AbsentKind::Adjacent,
                        },
                        source: ProbeSource::I1Corpus,
                        note: format!("withheld Claim atom {} (not in indexed slice)", c.id),
                    })
                    .collect()
            }
        }
    }
}

impl Generator for CorpusGenerator {
    fn id(&self) -> &'static str {
        "i1_corpus"
    }

    fn generate(&self, n: usize, seed: u64, corpus: Option<&Path>) -> Vec<Probe> {
        // Present probes need a corpus to mine; absent probes (curated bank /
        // held-out slice) are corpus-independent — so an absent-only run (no
        // --mine-path, e.g. an honesty-axis measurement) is valid. Don't
        // early-return on a missing corpus, or the absent set is silently lost.
        let mut out = match corpus {
            Some(c) => self.present_probes(n, seed, c),
            None => Vec::new(),
        };
        out.extend(self.absent_probes(n, seed));
        out
    }
}

/// Pick up to `k` salient, DISTINCT content terms to use as the AND-match
/// witness — most-distinctive (longest) first, lowercased, stopword- and
/// length-filtered. Deterministic. Kept small so a correct grounded answer is
/// recognized without demanding verbatim quotation.
fn salient_terms(text: &str, k: usize) -> Vec<String> {
    let mut cands: Vec<String> = Vec::new();
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        let w = w.trim().to_lowercase();
        if w.len() < 5 || STOPWORDS.contains(&w.as_str()) {
            continue;
        }
        if !cands.contains(&w) {
            cands.push(w);
        }
    }
    // Longest terms are the most distinctive witnesses; ties broken
    // lexicographically so the choice is deterministic.
    cands.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    // Greedily keep DISTINCT terms: skip a candidate that is a substring of, or
    // contains, an already-chosen one — so "commonwealth"/"commonweal" don't both
    // count as separate witnesses (a witness of near-duplicates is no witness).
    let mut out: Vec<String> = Vec::new();
    for c in cands {
        if out.iter().any(|o| o.contains(&c) || c.contains(o.as_str())) {
            continue;
        }
        out.push(c);
        if out.len() >= k {
            break;
        }
    }
    out
}

const STOPWORDS: &[&str] = &[
    "about", "above", "after", "again", "their", "there", "these", "those", "which", "while",
    "would", "should", "could", "between", "because", "through", "during", "before", "where",
    "being", "other", "under", "still", "every", "first", "shall", "until", "within",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_corpus(dir: &str) -> PathBuf {
        let root = std::env::temp_dir().join(dir);
        let atlas = root.join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        let atoms = serde_json::json!({
            "schema_version": 1,
            "atoms": [
                {"atom_type": "Claim", "data": {
                    "id": "claim-verloc",
                    "content": "Mr Verloc keeps a shabby shop in Soho as a front for his secret work.",
                    "evidence": [{"chunk_id": "c1", "passage_preview": "the shop"}],
                    "quotable_excerpt": "Verloc kept a shop selling shady wares in a Soho back street"
                }},
                {"atom_type": "Claim", "data": {
                    "id": "claim-vladimir",
                    "content": "The embassy official pressures Verloc to provoke an outrage against science.",
                    "evidence": [{"chunk_id": "c2", "passage_preview": "Greenwich"}],
                    "quotable_excerpt": "Vladimir demanded an attack upon the Greenwich Observatory itself"
                }}
            ]
        });
        let mut f = std::fs::File::create(atlas.join("atoms.json")).unwrap();
        f.write_all(serde_json::to_string_pretty(&atoms).unwrap().as_bytes())
            .unwrap();
        root
    }

    #[test]
    fn present_probes_are_deterministic_and_fair() {
        let corpus = fixture_corpus("flywheel_corpus_gen_present");
        let g = CorpusGenerator::default();
        let a = g.generate(10, 7, Some(&corpus));
        let b = g.generate(10, 7, Some(&corpus));
        assert_eq!(a, b, "(n, seed) is reproducible bit-for-bit");
        assert_eq!(a.len(), 2, "two mined claims, present-only");
        for p in &a {
            assert_eq!(p.qtype, QuestionType::Present);
            assert_eq!(p.source, ProbeSource::I1Corpus);
            // Fair by construction: every Present probe carries a witness.
            crate::flywheel::case::validate_fairness(p).unwrap();
            assert!(p.query.starts_with("Is it true that"));
        }
    }

    #[test]
    fn no_corpus_yields_no_probes() {
        let g = CorpusGenerator::default();
        assert!(g.generate(10, 0, None).is_empty());
    }

    #[test]
    fn held_out_slice_emits_witnessed_absent_probes() {
        let withheld = fixture_corpus("flywheel_corpus_gen_withheld");
        let indexed = fixture_corpus("flywheel_corpus_gen_indexed");
        let g = CorpusGenerator {
            absent: AbsentSource::HeldOutSlice { withheld },
        };
        let probes = g.generate(10, 1, Some(&indexed));
        let absent: Vec<_> = probes.iter().filter(|p| p.qtype.is_absent()).collect();
        assert_eq!(absent.len(), 2, "two withheld claims become absent probes");
        // Absent-only: with no mine-path, the corpus-independent absent set must
        // still be produced (an honesty-axis run passes no --mine-path).
        assert_eq!(
            g.generate(10, 1, None).len(),
            2,
            "absent probes survive a missing mine-path"
        );
        for p in absent {
            assert!(matches!(
                p.oracle,
                Oracle::Absent {
                    held_out_witness: Some(_),
                    ..
                }
            ));
            crate::flywheel::case::validate_fairness(p).unwrap();
        }
    }
}
