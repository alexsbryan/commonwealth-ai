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
//!     the chunk pool. Established by VERBATIM anchor containment, not by
//!     construction: the passage is the one the atom's quoted fragment was cut
//!     from, and a claim whose fragment is found nowhere is dropped.
//!   * `answerable = false` — the SAME question over a pool the passage was
//!     withheld from, and *both* leak checks clear:
//!       1. `gold_match(pool, witness)` is false — the flywheel's
//!          `held_out_witness` detector (`generators/corpus.rs:146`) applied
//!          pairwise;
//!       2. no pool passage contains the claim's quoted evidence verbatim
//!          ([`crate::flywheel::passages::PassageStore::anchors_present_in`]).
//!     Check 2 exists because check 1 goes vacuous exactly where it matters:
//!     the witness comes from the claim's paraphrased content, so on a
//!     paraphrased claim it cannot fire at all (measured: 6 of 10 answerable
//!     pools on brothers-karamazov-book-1 contain their evidence verbatim but
//!     not their witness terms). Together they are what makes an "absent"
//!     label a fact rather than a hope.
//!
//! Emitting both sides from ONE claim is deliberate: the two rows differ only
//! in the evidence pool, so a scorer cannot win by reading the question alone.
//!
//! **The pool is made of REAL PASSAGES.** Every chunk in a pair is a chunk
//! the retrieval layer could actually return — resolved out of the corpus's
//! chunk store by [`crate::flywheel::passages`], median 627 characters on
//! the SEP substrate. It is not the atom's `passage_preview`, which is a
//! ~25-character fragment. The first version of this file shipped those
//! fragments as the pool, and its own committed report recorded the
//! consequence: `answerable_witness_absent: 13` out of 13 answerable pairs
//! — not one "answerable" pool contained its own answer. A threshold fitted
//! on that set would have been fitted on noise.
//!
//! A claim whose anchor does not resolve to a real passage is **dropped**
//! and counted in [`MineReport::claims_unresolved`]. There is no
//! nearest-chunk fallback (ARCH §18.3).
//!
//! **Distractors are same-document, on purpose.** The pool for a pair is
//! drawn from the *same article*, by rotation from the evidence passage.
//! Cross-article distractors would make the negatives trivially separable
//! on topic — and topic is exactly what `top_cosine` already measures and
//! what H1 must beat by measuring containment instead (§5 H1: the
//! "~0.75 in-topic thin" failure). A calibration set whose negatives are
//! off-topic cannot tell those two signals apart.
//!
//! **Both pools are size `k`.** The absent pool is not the answerable pool
//! minus its evidence: that would make pool SIZE a label leak.
//!
//! **Determinism.** No RNG, no clock, no map iteration order: claims are
//! sorted by atom id, passages by chunk id, and the distractor pool for a
//! claim is taken by rotation from its evidence passage's position. The same
//! (corpus, chunk store, `k`) yields byte-identical output, which is the
//! property a calibration artifact needs to be re-derivable from its
//! committed report.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::flywheel::det_checks::gold_match;
use crate::flywheel::generators::corpus::{claim_query, salient_terms};
use crate::flywheel::mining::mine_claims_bounded;
use crate::flywheel::passages::PassageStore;

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
    /// The evidence pool the answerability scorer sees — REAL passage text
    /// from the corpus's chunk store, in pool order.
    pub chunks: Vec<String>,
    /// The chunk-store row id of each pool member, positionally aligned with
    /// `chunks`. Glassbox: every passage in the pool has a real address, so
    /// a disputed pair can be re-read from the corpus.
    pub chunk_ids: Vec<u64>,
    /// Index into `chunks` of the claim's own supporting passage. `Some` on
    /// every answerable pair (that IS the label) and `None` on every absent
    /// one.
    pub evidence_index: Option<usize>,
    /// The chunk store the passages came from, and the document filter used
    /// — per-pair provenance, because a multi-corpus pool mixes them.
    pub passage_source: String,
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
    /// Absent pairs dropped because the withheld pool contained the claim's
    /// own quoted evidence verbatim. The stronger of the two leak checks —
    /// it fires on paraphrased claims, where the witness check cannot.
    pub absent_dropped_anchor_leak: usize,
    /// Answerable pairs whose witness does not literally appear in their pool.
    ///
    /// NOT a defect and NOT a label problem: the witness terms come from the
    /// claim's model-written content, and a source passage routinely supports
    /// a claim without repeating its vocabulary. The label rests on anchor
    /// containment, which is verbatim. Kept glassbox so a consumer that
    /// mistakes `gold_match` for the label sees the gap before it trusts one.
    pub answerable_witness_absent: usize,
    /// Claims dropped because none of their anchors could be found in any
    /// real passage of the document. NOT a failure of the corpus: the atom's
    /// excerpt is sometimes a paraphrase rather than a quotation, and a
    /// paraphrase cannot prove containment. The alternative — attaching the
    /// topically-nearest chunk — would mislabel the pair silently.
    pub claims_unresolved: usize,
    /// Passages available in the document these claims were mined from.
    pub passages_available: usize,
    /// The chunk store the passages came from.
    pub passage_source: String,
}

/// Mine `limit` claims from `corpus_root` and emit up to two pairs per claim,
/// with pools built from `passages` — the document's REAL chunks.
///
/// `k` is the pool size (chunks per pair). Returns the pairs plus the report;
/// a caller that only prints the pairs would be hiding the drops.
///
/// # Errors
/// Refuses on a reserved (dev/test) corpus, on a document with fewer
/// passages than the pool size, and on an atlas that yields no claims.
pub fn mine_calibration_pairs(
    corpus_id: &str,
    corpus_root: &Path,
    passages: &PassageStore,
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
    let passage_source = match &passages.doc_filter {
        Some(d) => format!("{}#{d}", passages.corpus_id),
        None => passages.corpus_id.clone(),
    };
    let mut claims = mine_claims_bounded(corpus_root, true, limit);
    claims.sort_by(|a, b| a.id.cmp(&b.id));
    let n = claims.len();
    let np = passages.len();
    let mut report = MineReport {
        corpus_id: corpus_id.to_string(),
        claims_mined: n,
        passages_available: np,
        passage_source: passage_source.clone(),
        ..Default::default()
    };
    if n == 0 {
        return Err(format!(
            "`{corpus_id}` yielded 0 mined claims at {corpus_root:?} — an empty atlas produces an \
             empty calibration set, which would report clean and measure nothing"
        ));
    }
    // The distractors come from the PASSAGE store now, not from other
    // claims, so it is the passage count that has to clear the pool size.
    if np < k {
        return Err(format!(
            "`{corpus_id}` resolves to {np} passage(s) in `{passage_source}` — fewer than the pool \
             size {k}, so no honest distractor pool exists. Mine a larger document, or lower \
             --pool."
        ));
    }

    let mut out = Vec::new();
    for c in &claims {
        let witness = salient_terms(&c.content, 3);
        if witness.is_empty() {
            continue; // no usable witness → cannot label either side fairly
        }
        // Find the REAL passage this claim's evidence was cut from. No
        // fallback: an unresolved claim is dropped, not approximated.
        let Some(ev) = passages.resolve(&c.anchors) else {
            report.claims_unresolved += 1;
            continue;
        };
        let question = claim_query(&c.content);
        let all = passages.passages();

        // ── answerable: the claim's own evidence passage IS in the pool ──
        // Deterministic same-document distractors: the k-1 passages
        // following the evidence passage, by rotation.
        let mut idxs: Vec<usize> = Vec::with_capacity(k);
        idxs.push(ev);
        for off in 1..k {
            idxs.push((ev + off) % np);
        }
        // The evidence passage sits at its rotation position rather than
        // always first, so pool POSITION is not a label leak either.
        let evidence_index = ev % k;
        idxs.swap(0, evidence_index);
        let pool: Vec<String> = idxs.iter().map(|&i| all[i].text.clone()).collect();
        let pool_ids: Vec<u64> = idxs.iter().map(|&i| all[i].chunk_id).collect();
        let witness_in_pool = gold_match(&pool.join(" \n "), &witness);
        if !witness_in_pool {
            report.answerable_witness_absent += 1;
        }
        out.push(CalibrationPair {
            id: format!("cal:{corpus_id}:{}:present", c.id),
            corpus_id: corpus_id.to_string(),
            question: question.clone(),
            chunks: pool,
            chunk_ids: pool_ids,
            evidence_index: Some(evidence_index),
            passage_source: passage_source.clone(),
            answerable: true,
            witness: witness.clone(),
            source_claim: c.id.clone(),
            witness_in_pool,
        });
        report.pairs_answerable += 1;

        // ── absent: same question, evidence withheld, leak-checked ──
        // Same size k as the answerable pool (size is not a signal), taken
        // by continuing the rotation past the evidence passage.
        let absent_idxs: Vec<usize> = (1..=k).map(|off| (ev + off) % np).collect();
        if absent_idxs.contains(&ev) {
            // Only reachable when np <= k, which the guard above excludes;
            // stated so the invariant is visible rather than implied.
            report.absent_dropped_leaky += 1;
            continue;
        }
        let absent_pool: Vec<String> = absent_idxs.iter().map(|&i| all[i].text.clone()).collect();
        // TWO leak checks, because either alone lets a fake negative through:
        //   * the witness AND-match — catches a pool that states the answer
        //     in the claim's own vocabulary;
        //   * anchor containment — catches a pool that contains the claim's
        //     quoted evidence verbatim. This one is the stronger of the two
        //     and it is the one that can fire when the witness terms are a
        //     paraphrase the source prose never uses (the common case: 6 of
        //     10 answerable pools on brothers-karamazov-book-1).
        if gold_match(&absent_pool.join(" \n "), &witness) {
            report.absent_dropped_leaky += 1;
            continue;
        }
        if passages.anchors_present_in(&c.anchors, &absent_idxs) {
            report.absent_dropped_anchor_leak += 1;
            continue;
        }
        out.push(CalibrationPair {
            id: format!("cal:{corpus_id}:{}:absent", c.id),
            corpus_id: corpus_id.to_string(),
            question,
            chunks: absent_pool,
            chunk_ids: absent_idxs.iter().map(|&i| all[i].chunk_id).collect(),
            evidence_index: None,
            passage_source: passage_source.clone(),
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

    /// The fixture's shape mirrors the real substrate: each claim carries a
    /// short `passage_preview` that is a VERBATIM fragment of exactly one
    /// real passage, and the passages share topical vocabulary so a
    /// distractor is a plausible neighbour rather than an obvious miss.
    fn fixture(n: usize) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("calibration_fixture_{n}"));
        let atlas = root.join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        let atoms = serde_json::json!({
            "atoms": (1..=n).map(|i| serde_json::json!({
                "atom_type": "Claim",
                "data": {
                    "id": format!("claim-{i:03}"),
                    "content": format!("Quorlibet{i} presides over the marmoreal ascendancy of district {i}."),
                    "evidence": [{
                        "chunk_id": format!("sec_{i:04}"),
                        "passage_preview": preview(i),
                    }]
                }
            })).collect::<Vec<_>>()
        });
        std::fs::File::create(atlas.join("atoms.json"))
            .unwrap()
            .write_all(serde_json::to_string(&atoms).unwrap().as_bytes())
            .unwrap();
        root
    }

    fn preview(i: usize) -> String {
        format!("came at last to preside over the marmoreal ascendancy of district {i}")
    }

    fn passage(i: usize) -> String {
        format!(
            "Chronicle {i}. It was in that season that Quorlibet{i} {}, by a right nobody \
             disputed at the time.",
            preview(i)
        )
    }

    /// One passage per claim, in chunk-id order.
    fn store(n: usize) -> PassageStore {
        PassageStore::from_rows(
            "fixture-chunks",
            (1..=n).map(|i| (i as u64, passage(i))).collect(),
        )
    }

    #[test]
    fn emits_a_labeled_pair_on_each_side_of_every_claim() {
        let root = fixture(6);
        let (pairs, report) = mine_calibration_pairs("fixture", &root, &store(6), usize::MAX, 3)
            .unwrap();
        assert_eq!(report.claims_mined, 6);
        assert_eq!(report.claims_unresolved, 0);
        assert_eq!(report.pairs_answerable, 6);
        assert_eq!(
            report.pairs_absent + report.absent_dropped_leaky + report.absent_dropped_anchor_leak,
            6
        );
        assert_eq!(report.passages_available, 6);
        for p in &pairs {
            // Both pools are size k: pool size cannot leak the label.
            assert_eq!(p.chunks.len(), 3, "{}", p.id);
            assert_eq!(p.chunk_ids.len(), p.chunks.len(), "{}", p.id);
            if p.answerable {
                // The answerable pool holds the claim's OWN passage, at the
                // position the pair records — the label, checkable.
                let ev = p.evidence_index.expect("answerable pair names its evidence");
                let want = p.source_claim.trim_start_matches("claim-").parse::<usize>().unwrap();
                assert_eq!(p.chunks[ev], passage(want), "{}", p.id);
                assert_eq!(p.chunk_ids[ev], want as u64, "{}", p.id);
            } else {
                assert!(p.evidence_index.is_none(), "{}", p.id);
                assert!(
                    !gold_match(&p.chunks.join(" \n "), &p.witness),
                    "an absent pair whose pool matches the witness is mislabeled: {}",
                    p.id
                );
            }
        }
    }

    #[test]
    fn an_answerable_pool_actually_contains_its_own_answer() {
        // The regression this whole change exists for. The preview-fragment
        // miner produced `answerable_witness_absent: 13` on 13 answerable
        // pairs (see the committed contamination report it shipped with):
        // not one "answerable" pool contained its own answer, so the label
        // was fiction and any AUROC computed on it was noise.
        let root = fixture(6);
        let (_, report) = mine_calibration_pairs("fixture", &root, &store(6), usize::MAX, 3)
            .unwrap();
        assert_eq!(
            report.answerable_witness_absent, 0,
            "every answerable pool must literally contain its claim's witness terms"
        );
    }

    #[test]
    fn the_evidence_passage_is_not_always_at_position_zero() {
        // Position must not be a label leak either: a scorer that always
        // read chunk 0 would score perfectly on a set that pinned it there.
        let root = fixture(6);
        let (pairs, _) = mine_calibration_pairs("fixture", &root, &store(6), usize::MAX, 3)
            .unwrap();
        let positions: HashSet<usize> = pairs
            .iter()
            .filter_map(|p| p.evidence_index)
            .collect();
        assert!(
            positions.len() > 1,
            "evidence sat at exactly one pool position across every pair: {positions:?}"
        );
    }

    #[test]
    fn an_absent_pool_holding_the_evidence_verbatim_is_dropped_even_when_the_witness_cannot_tell() {
        // The case the witness check CANNOT catch, and the reason the anchor
        // leak check exists. A seventh passage repeats claim 4's quoted
        // evidence but never names `Quorlibet4`, so:
        //   * `gold_match(pool, witness)` stays FALSE — the witness needs all
        //     three terms and `quorlibet4` is absent;
        //   * the pool nonetheless contains, verbatim, the very sentence the
        //     claim was extracted from.
        // Shipping that as a negative would teach the scorer that the answer
        // being present means "absent".
        let root = fixture(6);
        let mut rows: Vec<(u64, String)> = (1..=6).map(|i| (i as u64, passage(i))).collect();
        rows.push((
            7,
            format!(
                "Chronicle 7. Another chronicler recorded that the regent {}, though under a \
                 wholly different name.",
                preview(4)
            ),
        ));
        let leaky = PassageStore::from_rows("fixture-chunks", rows);
        let (pairs, report) =
            mine_calibration_pairs("fixture", &root, &leaky, usize::MAX, 3).unwrap();

        assert_eq!(
            report.absent_dropped_anchor_leak, 1,
            "the verbatim-evidence leak must be caught"
        );
        let absent_4 = pairs
            .iter()
            .find(|p| p.source_claim == "claim-004" && !p.answerable);
        assert!(absent_4.is_none(), "claim-004 must contribute no absent pair");
        // ...and the witness check really was blind to it: claim 4's absent
        // pool does not AND-match its witness.
        let claim_4_witness = pairs
            .iter()
            .find(|p| p.source_claim == "claim-004")
            .map(|p| p.witness.clone())
            .expect("claim-004 still yields its answerable pair");
        let leaked_pool = [passage(5), passage(6), format!(
            "Chronicle 7. Another chronicler recorded that the regent {}, though under a wholly \
             different name.",
            preview(4)
        )]
        .join(" \n ");
        assert!(
            !gold_match(&leaked_pool, &claim_4_witness),
            "if the witness check could see this leak, the anchor check would be redundant"
        );
    }

    #[test]
    fn mining_is_byte_identical_across_repeats() {
        let root = fixture(7);
        let a = mine_calibration_pairs("fixture", &root, &store(7), usize::MAX, 4)
            .unwrap()
            .0;
        let b = mine_calibration_pairs("fixture", &root, &store(7), usize::MAX, 4)
            .unwrap()
            .0;
        assert_eq!(
            a, b,
            "the miner must be reproducible from (corpus, chunk store, k) alone"
        );
    }

    #[test]
    fn dev_and_test_corpora_are_refused_structurally() {
        let root = fixture(5);
        for id in RESERVED_CORPORA {
            let err = mine_calibration_pairs(id, &root, &store(5), usize::MAX, 3).unwrap_err();
            assert!(err.contains("§7.1"), "refusal must name the rule: {err}");
        }
    }

    #[test]
    fn a_thin_passage_store_is_an_error_not_a_silently_tiny_set() {
        let root = fixture(6);
        // Six claims, but only two passages to build a pool of 8 from.
        let err = mine_calibration_pairs("fixture", &root, &store(2), usize::MAX, 8).unwrap_err();
        assert!(err.contains("fewer than the pool size"), "{err}");
    }

    #[test]
    fn an_empty_atlas_is_an_error_not_an_empty_clean_set() {
        let root = std::env::temp_dir().join("calibration_fixture_empty_atlas");
        std::fs::create_dir_all(root.join("atlas")).unwrap();
        std::fs::write(root.join("atlas").join("atoms.json"), r#"{"atoms":[]}"#).unwrap();
        let err = mine_calibration_pairs("fixture", &root, &store(6), usize::MAX, 3).unwrap_err();
        assert!(err.contains("0 mined claims"), "{err}");
    }

    #[test]
    fn a_claim_whose_anchor_does_not_resolve_is_dropped_and_counted() {
        // The passages are real and topically identical, but none of them
        // contains claim 4's quoted fragment. The honest outcome is a drop,
        // NOT a pair built around the nearest-looking chunk.
        let root = fixture(6);
        let mut rows: Vec<(u64, String)> = (1..=6).map(|i| (i as u64, passage(i))).collect();
        rows[3].1 = "Chronicle 4. A season passed in which the marmoreal ascendancy of the \
                     district went entirely unremarked by anyone at all."
            .to_string();
        let holed = PassageStore::from_rows("fixture-chunks", rows);
        let (pairs, report) =
            mine_calibration_pairs("fixture", &root, &holed, usize::MAX, 3).unwrap();
        assert_eq!(report.claims_unresolved, 1);
        assert_eq!(report.pairs_answerable, 5);
        assert!(
            !pairs.iter().any(|p| p.source_claim == "claim-004"),
            "an unresolved claim must contribute no pairs at all"
        );
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
            chunk_ids: vec![1],
            evidence_index: Some(0),
            passage_source: "fixture-chunks".into(),
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
