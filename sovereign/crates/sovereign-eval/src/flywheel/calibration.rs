// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **calibration set**: (question, chunks, answerable?) pairs mined from a
//! corpus's own atlas, with mechanically-derived labels.
//!
//! `NATIVE_GROUNDING.md §7.1` fixes three data roles and forbids mixing them:
//! calibration (fit thresholds — flywheel-mined, volume), development
//! (`saltgrass*`), and test (`secret_agent`, touched only at phase gates).
//! This module builds the first role. H1's answerability scorer, H2's
//! clustering floor and every τ this initiative picks are fitted here and
//! nowhere else.
//!
//! **The label is mechanical, and both sides are checked, not assumed:**
//!
//!   * `answerable = true` — the claim's own supporting evidence passage is in
//!     the chunk pool. True by construction (the pair is built around it).
//!   * `answerable = false` — the SAME question over a pool the passage was
//!     withheld from, *and* the deterministic witness kernel finds no leak:
//!     `gold_match(pool, witness)` must be false, or the pair is dropped. This
//!     is the flywheel's `held_out_witness` leak detector
//!     (`generators/corpus.rs:146`) applied pairwise, and it is what makes an
//!     "absent" label a fact rather than a hope.
//!
//! Emitting both sides from ONE claim is deliberate: the two rows differ only
//! in the evidence pool, so a scorer cannot win by reading the question alone.
//!
//! **Determinism.** No RNG, no clock, no map iteration order: claims are sorted
//! by atom id and the distractor pool for claim *i* is the next `k-1` excerpts
//! by rotation. The same corpus and `k` yield byte-identical output, which is
//! the property a calibration artifact needs to be re-derivable from its
//! committed report.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::flywheel::det_checks::gold_match;
use crate::flywheel::generators::corpus::{claim_query, salient_terms};
use crate::flywheel::mining::mine_claims_bounded;

/// Corpora that are DEV or TEST data under §7.1 and must never be mined into a
/// calibration set. Enforced structurally, at the entry point, rather than left
/// to whoever types the command (ARCH §7 — make it unforgettable).
pub const RESERVED_CORPORA: &[&str] = &["chaos-secret-agent", "chaos-saltgrass"];

/// One labeled calibration pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationPair {
    pub id: String,
    pub corpus_id: String,
    pub question: String,
    /// The evidence pool the answerability scorer sees.
    pub chunks: Vec<String>,
    /// The label. See the module docs for how each side is established.
    pub answerable: bool,
    /// The deterministic witness for the underlying claim (AND-match terms).
    pub witness: Vec<String>,
    /// The atom this pair descends from — the audit trail back to the corpus.
    pub source_claim: String,
    /// Glassbox, NOT a filter: does the witness literally appear in the pool?
    /// On an answerable pair this can be `false` (the supporting passage
    /// paraphrases the claim), and a consumer that mistakes `gold_match` for
    /// the label needs to see that before it trusts one.
    pub witness_in_pool: bool,
}

/// What a mining run produced, and what it refused to produce.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MineReport {
    pub corpus_id: String,
    pub claims_mined: usize,
    pub pairs_answerable: usize,
    pub pairs_absent: usize,
    /// Absent pairs dropped because the withheld pool leaked the witness — the
    /// fairness contract doing its job. A large number here means the corpus's
    /// passages repeat themselves, not that the miner failed.
    pub absent_dropped_leaky: usize,
    /// Answerable pairs whose witness does not literally appear in their pool.
    pub answerable_witness_absent: usize,
}

/// Mine `limit` claims from `corpus_root` and emit up to two pairs per claim.
///
/// `k` is the pool size (chunks per pair). Returns the pairs plus the report;
/// a caller that only prints the pairs would be hiding the drops.
pub fn mine_calibration_pairs(
    corpus_id: &str,
    corpus_root: &Path,
    limit: usize,
    k: usize,
) -> Result<(Vec<CalibrationPair>, MineReport), String> {
    if RESERVED_CORPORA.contains(&corpus_id) {
        return Err(format!(
            "`{corpus_id}` is a dev/test bank corpus under NATIVE_GROUNDING §7.1 — mining it into \
             a calibration set would contaminate the split the whole measurement rests on"
        ));
    }
    let k = k.max(2);
    let mut claims = mine_claims_bounded(corpus_root, true, limit);
    claims.sort_by(|a, b| a.id.cmp(&b.id));
    let n = claims.len();
    let mut report = MineReport {
        corpus_id: corpus_id.to_string(),
        claims_mined: n,
        ..Default::default()
    };
    if n < k {
        return Err(format!(
            "`{corpus_id}` yielded {n} mined claim(s) at {corpus_root:?} — fewer than the pool size \
             {k}, so no honest distractor pool exists. Mine a larger corpus, or lower --pool."
        ));
    }

    let mut out = Vec::new();
    for (i, c) in claims.iter().enumerate() {
        let witness = salient_terms(&c.content, 3);
        if witness.is_empty() {
            continue; // no usable witness → cannot label either side fairly
        }
        let question = claim_query(&c.content);
        // Deterministic distractor pool: the next k-1 excerpts by rotation.
        let others: Vec<String> = (1..k)
            .map(|off| claims[(i + off) % n].excerpt.clone())
            .collect();

        // ── answerable: the claim's own evidence is in the pool ──
        let mut pool = others.clone();
        pool.insert(i % k.min(pool.len() + 1), c.excerpt.clone());
        let witness_in_pool = gold_match(&pool.join(" \n "), &witness);
        if !witness_in_pool {
            report.answerable_witness_absent += 1;
        }
        out.push(CalibrationPair {
            id: format!("cal:{corpus_id}:{}:present", c.id),
            corpus_id: corpus_id.to_string(),
            question: question.clone(),
            chunks: pool,
            answerable: true,
            witness: witness.clone(),
            source_claim: c.id.clone(),
            witness_in_pool,
        });
        report.pairs_answerable += 1;

        // ── absent: same question, evidence withheld, leak-checked ──
        let leaked =
            gold_match(&others.join(" \n "), &witness) || others.iter().any(|o| o == &c.excerpt);
        if leaked {
            report.absent_dropped_leaky += 1;
            continue;
        }
        out.push(CalibrationPair {
            id: format!("cal:{corpus_id}:{}:absent", c.id),
            corpus_id: corpus_id.to_string(),
            question,
            chunks: others,
            answerable: false,
            witness,
            source_claim: c.id.clone(),
            witness_in_pool: false,
        });
        report.pairs_absent += 1;
    }
    Ok((out, report))
}

// ───────────────────────────── contamination ─────────────────────────────

/// A calibration set that overlaps the dev or test banks is not a calibration
/// set — it is a leak, and every threshold fitted on it is unfalsifiable.
///
/// The convention is the one `research/verifier-v0/scripts/contamination_pass.py`
/// uses and the GPT-3/Llama dedup literature established: **13-word shingles**
/// over `[a-z0-9]+` tokens, lowercased. A collision means the calibration text
/// shares a ≥13-word verbatim span with bank text.
///
/// Two differences from the Python, both deliberate and both narrowing:
///   * it holds the shingles themselves rather than 8-byte digests — the index
///     here is three small TOML banks, not 100k benchmark documents, so there
///     is no reason to accept even a theoretical hash collision;
///   * its test corpus is OUR banks. The Python script indexes external
///     benchmarks (LLM-AggreFact, FaithBench) and answers a different
///     question — "did a public benchmark leak into our training stream?" —
///     which still needs `research/verifier-v0/data/` present to run.
const SHINGLE_N: usize = 13;

fn shingles(text: &str) -> HashSet<String> {
    let words: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    if words.len() < SHINGLE_N {
        return HashSet::new();
    }
    words
        .windows(SHINGLE_N)
        .map(|w| w.join(" "))
        .collect::<HashSet<String>>()
}

/// One collision, kept with enough context to adjudicate it by hand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collision {
    pub pair_id: String,
    pub bank: String,
    pub shared_span: String,
}

/// The committed artifact that says whether a calibration set is usable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContaminationReport {
    pub shingle_n: usize,
    pub pairs_scanned: usize,
    /// Bank path → number of 13-gram shingles indexed from it.
    pub banks_indexed: Vec<(String, usize)>,
    pub collisions: Vec<Collision>,
    /// `true` iff nothing collided. The gate: a contaminated calibration set is
    /// not "mostly fine".
    pub clean: bool,
}

/// Scan `pairs` against the dev/test bank files at `bank_paths`.
pub fn contamination_pass(
    pairs: &[CalibrationPair],
    bank_paths: &[std::path::PathBuf],
) -> Result<ContaminationReport, String> {
    let mut index: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut banks_indexed = Vec::new();
    for p in bank_paths {
        let text = std::fs::read_to_string(p).map_err(|e| format!("read bank {p:?}: {e}"))?;
        let sh = shingles(&text);
        banks_indexed.push((p.display().to_string(), sh.len()));
        for s in sh {
            index.entry(s).or_insert_with(|| p.display().to_string());
        }
    }
    if index.is_empty() {
        return Err(
            "no shingles indexed from any bank — an empty index would report every set clean"
                .into(),
        );
    }
    let mut collisions = Vec::new();
    for pair in pairs {
        let mut text = pair.question.clone();
        for c in &pair.chunks {
            text.push('\n');
            text.push_str(c);
        }
        for s in shingles(&text) {
            if let Some(bank) = index.get(&s) {
                collisions.push(Collision {
                    pair_id: pair.id.clone(),
                    bank: bank.clone(),
                    shared_span: s,
                });
                break; // one witness per pair is enough to condemn it
            }
        }
    }
    Ok(ContaminationReport {
        shingle_n: SHINGLE_N,
        pairs_scanned: pairs.len(),
        banks_indexed,
        clean: collisions.is_empty(),
        collisions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(n: usize) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("calibration_fixture_{n}"));
        let atlas = root.join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        let atoms = serde_json::json!({
            "atoms": (1..=n).map(|i| serde_json::json!({
                "atom_type": "Claim",
                "data": {
                    "id": format!("claim-{i:03}"),
                    "content": format!("Zorbulax {i} governs the tidal reckoning of the seventh harbour."),
                    "evidence": [{"chunk_id": format!("c{i}"), "passage_preview": format!("Passage {i}: the reckoning of tides in that harbour was long attributed to a distant authority.")}]
                }
            })).collect::<Vec<_>>()
        });
        std::fs::File::create(atlas.join("atoms.json"))
            .unwrap()
            .write_all(serde_json::to_string(&atoms).unwrap().as_bytes())
            .unwrap();
        root
    }

    #[test]
    fn emits_a_labeled_pair_on_each_side_of_every_claim() {
        let root = fixture(6);
        let (pairs, report) = mine_calibration_pairs("fixture", &root, usize::MAX, 3).unwrap();
        assert_eq!(report.claims_mined, 6);
        assert_eq!(report.pairs_answerable, 6);
        assert_eq!(report.pairs_absent + report.absent_dropped_leaky, 6);
        // Every answerable pool contains its own claim's evidence; no absent
        // pool does. That is the label, and it is checkable.
        for p in &pairs {
            let own = pairs
                .iter()
                .find(|q| q.source_claim == p.source_claim && q.answerable)
                .map(|q| q.chunks.clone())
                .unwrap();
            assert!(!own.is_empty());
            if !p.answerable {
                assert!(
                    !gold_match(&p.chunks.join(" \n "), &p.witness),
                    "an absent pair whose pool matches the witness is mislabeled: {}",
                    p.id
                );
            }
        }
    }

    #[test]
    fn mining_is_byte_identical_across_repeats() {
        let root = fixture(7);
        let a = mine_calibration_pairs("fixture", &root, usize::MAX, 4)
            .unwrap()
            .0;
        let b = mine_calibration_pairs("fixture", &root, usize::MAX, 4)
            .unwrap()
            .0;
        assert_eq!(
            a, b,
            "the miner must be reproducible from (corpus, k) alone"
        );
    }

    #[test]
    fn dev_and_test_corpora_are_refused_structurally() {
        let root = fixture(5);
        for id in RESERVED_CORPORA {
            let err = mine_calibration_pairs(id, &root, usize::MAX, 3).unwrap_err();
            assert!(err.contains("§7.1"), "refusal must name the rule: {err}");
        }
    }

    #[test]
    fn a_thin_corpus_is_an_error_not_a_silently_tiny_set() {
        let root = fixture(2);
        let err = mine_calibration_pairs("fixture", &root, usize::MAX, 8).unwrap_err();
        assert!(err.contains("fewer than the pool size"), "{err}");
    }

    #[test]
    fn contamination_catches_a_planted_verbatim_span() {
        let bank = std::env::temp_dir().join("calibration_contam_bank.toml");
        let span =
            "the reckoning of tides in that harbour was long attributed to a distant authority";
        std::fs::write(
            &bank,
            format!("question = \"{span} and more words follow here\"\n"),
        )
        .unwrap();

        let clean_pair = CalibrationPair {
            id: "clean".into(),
            corpus_id: "fixture".into(),
            question: "Is it true that something entirely unrelated happened elsewhere?".into(),
            chunks: vec!["a passage sharing no long verbatim span with the bank at all".into()],
            answerable: true,
            witness: vec!["unrelated".into()],
            source_claim: "claim-x".into(),
            witness_in_pool: false,
        };
        let dirty_pair = CalibrationPair {
            id: "dirty".into(),
            chunks: vec![format!("{span} and more words follow here")],
            ..clean_pair.clone()
        };
        let rep = contamination_pass(&[clean_pair, dirty_pair], &[bank]).unwrap();
        assert!(!rep.clean);
        assert_eq!(rep.collisions.len(), 1);
        assert_eq!(rep.collisions[0].pair_id, "dirty");
    }

    #[test]
    fn an_empty_index_is_an_error_not_a_clean_bill() {
        // A contamination pass with nothing indexed would call everything
        // clean — the exact silent-pass shape ARCH §18.3 forbids.
        let empty = std::env::temp_dir().join("calibration_contam_empty.toml");
        std::fs::write(&empty, "x = 1\n").unwrap();
        let err = contamination_pass(&[], &[empty]).unwrap_err();
        assert!(err.contains("no shingles indexed"), "{err}");
    }
}
