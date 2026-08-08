// SPDX-License-Identifier: AGPL-3.0-or-later
//! H4 deliverable 2 — the sentence-margin sweep that replaces the twice-run
//! longform claim ladder.
//!
//! `NATIVE_GROUNDING.md` §5 H4: *"Sentence-split (the lossless splitter at
//! `surgical.rs:42` survives), score each sentence against the sealed evidence
//! with the reranker (~23 ms/pair, batched); sentences under the calibrated
//! margin with fabrication-shaped content (the existing deterministic vetoes at
//! `judge.rs:890,974` survive as cheap structural checks) get the surgical
//! Delete/Fix treatment."*
//!
//! This module builds the **measurement** half of that: split, score, and flag.
//! It decides nothing. There is no threshold in this file — the margin floor is
//! calibrated in deliverable 4 and committed beside the code that reads it, so
//! that a naked threshold cannot enter through the instrument (principle 2).
//! What lands here is one row per sentence carrying a margin, a span
//! resolution, and the structural vetoes, and a caller is free to apply
//! whatever floor its committed curve justifies.
//!
//! # Everything reused, nothing re-derived
//!
//! | What | Where it comes from |
//! |---|---|
//! | sentence split | `surgical::split_sentences` (`surgical.rs:42`) — lossless: `.concat()` is the input |
//! | span resolution | [`super::span_resolver::resolve_span`] (deliverable 1) |
//! | fabricated-name veto | `judge::absent_name_attribution` (`judge.rs:890`) |
//! | fabricated-identifier veto | `judge::absent_identifier_attribution` (`judge.rs:974`) |
//! | the margin fold | `max_i margin(sentence, chunk_i)`, the same shape H1's `answerability` uses over the same k ≤ 8 pool |
//!
//! # Why the scorer is a trait
//!
//! `sovereign-core` does not depend on `sovereign-inference`, and a measurement
//! is a bad reason to make it start. [`SentenceScorer`] is the seam: the bench
//! harness passes the real `StandaloneReranker`, the tests pass a deterministic
//! fake. The consequence that matters is that the determinism pin below runs
//! with **no model on disk** — so a machine that cannot load the reranker can
//! still prove the sweep is a pure function of its inputs.
//!
//! # What is deterministic and what is not
//!
//! [`SweepResult::sentences`] is a pure function of `(question, answer,
//! chunks)` and the scorer's outputs. [`SweepResult::elapsed_ms`] is wall time
//! and is deliberately NOT part of `SentenceRow`'s equality — it is the H4
//! gate's audit-latency measurement (§7.3 H4's second bar), and mixing a clock
//! into the determinism pin would make the pin unfalsifiable.

use serde::{Deserialize, Serialize};

use super::span_resolver::{resolve_span, SpanResolution};

/// Evidence pool cap for the margin fold.
///
/// Eight, matching H1's `answerability(q) = max_i margin(q, chunk_i)` over
/// `k ≤ 8` (§5 H1) — one number for "how much evidence a rerank fold looks at",
/// not two. When a turn retrieved more than this, the FIRST `EVIDENCE_K_CAP`
/// chunks are kept: chunk order out of retrieval is relevance order, so the cap
/// drops the tail rather than a random sample, and [`SweepResult::k_cap_applied`]
/// reports that it happened rather than letting it pass silently.
pub const EVIDENCE_K_CAP: usize = 8;

/// Batch scorer for (sentence, chunk) pairs — the injected reranker seam.
#[async_trait::async_trait]
pub trait SentenceScorer: Send + Sync {
    /// One margin per doc, in the order the docs were given.
    ///
    /// Returning a vector of a different length than `docs` is a contract
    /// violation and the sweep treats it as a scorer failure rather than
    /// silently zipping the short prefix.
    async fn score(&self, query: &str, docs: &[String]) -> Result<Vec<f32>, String>;
}

/// A deterministic structural flag from the shipped vetoes. Cheap: no model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "veto", rename_all = "snake_case")]
pub enum Veto {
    /// The sentence attributes something to a *named source artifact* whose
    /// name appears nowhere in the evidence (`judge.rs:890`).
    AbsentNameAttribution {
        /// The offending name, as the veto reported it.
        name: String,
    },
    /// The sentence makes a claim about a code/structure artifact naming an
    /// identifier absent from the whole evidence pool (`judge.rs:974`).
    AbsentIdentifierAttribution {
        /// The offending identifier, as the veto reported it.
        identifier: String,
    },
}

impl Veto {
    /// Stable label for artifacts. One name per veto.
    pub fn label(&self) -> &'static str {
        match self {
            Self::AbsentNameAttribution { .. } => "absent_name_attribution",
            Self::AbsentIdentifierAttribution { .. } => "absent_identifier_attribution",
        }
    }
}

/// One sentence of the answer, swept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SentenceRow {
    /// Position in the lossless split. `rows.map(text).concat()` is the answer.
    pub index: usize,
    /// The sentence exactly as the splitter produced it — trailing whitespace
    /// included, because losslessness is what lets a caller rebuild the answer
    /// byte-for-byte after a Delete/Fix.
    pub text: String,
    /// `max_i margin(sentence, chunk_i)` over the k-capped pool.
    ///
    /// `None` is **could-not-judge**, never a low score: either the sentence
    /// had no scoreable content (a whitespace-only split artifact) or the
    /// evidence pool was empty. A caller applying a floor must not read `None`
    /// as "below the floor" — §18.3, and the reason this is an `Option` rather
    /// than a sentinel `f32`.
    pub margin: Option<f32>,
    /// Which chunk carried the max margin, when there was one.
    pub best_chunk: Option<usize>,
    /// Whether this sentence's own text resolves against the evidence
    /// (deliverable 1). Structural, deterministic, and free.
    pub span: SpanResolution,
    /// Structural vetoes that fired. Empty is the common case.
    pub vetoes: Vec<Veto>,
}

impl SentenceRow {
    /// True when the sentence carries content worth judging. Whitespace-only
    /// split artifacts are carried (losslessness) but are not claims.
    pub fn is_scoreable(&self) -> bool {
        !self.text.trim().is_empty()
    }
}

/// The sweep's output for one turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepResult {
    /// One row per sentence, in order. The deterministic part.
    pub sentences: Vec<SentenceRow>,
    /// How many (sentence, chunk) pairs were actually scored — the instrument's
    /// own accounting, so a report can state its cost rather than estimate it.
    pub scored_pairs: usize,
    /// Size of the evidence pool AFTER the k cap.
    pub evidence_chunks: usize,
    /// True when the turn retrieved more than [`EVIDENCE_K_CAP`] chunks and the
    /// tail was dropped. Reported, never silent.
    pub k_cap_applied: bool,
    /// Wall time for the whole sweep. NOT part of the determinism pin — this is
    /// the §7.3 H4 audit-latency measurement.
    pub elapsed_ms: u128,
}

impl SweepResult {
    /// Sentences that carry content, i.e. the ones a floor applies to.
    pub fn scoreable(&self) -> impl Iterator<Item = &SentenceRow> {
        self.sentences.iter().filter(|s| s.is_scoreable())
    }

    /// Rebuild the answer from the rows — the losslessness contract, callable.
    pub fn rejoined(&self) -> String {
        self.sentences.iter().map(|s| s.text.as_str()).collect()
    }
}

/// Sweep one turn: split the answer, score every sentence against the sealed
/// evidence, and ride the deterministic vetoes along.
///
/// `question` is carried for the caller's records and for future scorers that
/// condition on it; the margin fold deliberately scores **sentence against
/// chunk**, not question against chunk, because H4 is asking "is this claim
/// supported?" and not "is this turn answerable?" (that is H1's question, and
/// it has its own instrument).
///
/// Errors only when the scorer errors. An empty evidence pool is not an error:
/// every row comes back with `margin: None` and a `NoEvidence` span, which is
/// the honest report of a turn that retrieved nothing.
pub async fn sweep(
    question: &str,
    answer: &str,
    chunks: &[String],
    scorer: &dyn SentenceScorer,
) -> Result<SweepResult, String> {
    let _ = question;
    let started = std::time::Instant::now();

    let k_cap_applied = chunks.len() > EVIDENCE_K_CAP;
    let pool: Vec<String> = chunks.iter().take(EVIDENCE_K_CAP).cloned().collect();
    let hay_lower = pool.join(" ").to_lowercase();

    let mut sentences = Vec::new();
    let mut scored_pairs = 0usize;

    for (index, text) in super::super::surgical::split_sentences(answer)
        .into_iter()
        .enumerate()
    {
        let trimmed = text.trim().to_string();
        let scoreable = !trimmed.is_empty() && !pool.is_empty();

        let (margin, best_chunk) = if scoreable {
            let margins = scorer.score(&trimmed, &pool).await?;
            if margins.len() != pool.len() {
                return Err(format!(
                    "scorer returned {} margins for {} chunks (sentence {index}) — refusing to \
                     zip a short prefix, because a silently truncated fold is a wrong number \
                     that looks right",
                    margins.len(),
                    pool.len()
                ));
            }
            scored_pairs += margins.len();
            // Max fold, first index wins a tie so the address is deterministic.
            let mut best = (0usize, margins[0]);
            for (i, &m) in margins.iter().enumerate().skip(1) {
                if m > best.1 {
                    best = (i, m);
                }
            }
            (Some(best.1), Some(best.0))
        } else {
            (None, None)
        };

        // Structural flags: free, deterministic, and computed even for
        // sentences the scorer skipped — a fabricated name is a fabricated
        // name whether or not there was a pool to fold over.
        let span = resolve_span(&trimmed, &pool);
        let mut vetoes = Vec::new();
        if !trimmed.is_empty() && !hay_lower.is_empty() {
            if let Some(name) = super::super::judge::absent_name_attribution(&trimmed, &hay_lower) {
                vetoes.push(Veto::AbsentNameAttribution { name });
            }
            if let Some(identifier) =
                super::super::judge::absent_identifier_attribution(&trimmed, &hay_lower)
            {
                vetoes.push(Veto::AbsentIdentifierAttribution { identifier });
            }
        }

        sentences.push(SentenceRow {
            index,
            text,
            margin,
            best_chunk,
            span,
            vetoes,
        });
    }

    Ok(SweepResult {
        sentences,
        scored_pairs,
        evidence_chunks: pool.len(),
        k_cap_applied,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scorer with no model and no randomness: the margin is a function of
    /// the pair's text. Deterministic by construction, which is the point —
    /// the determinism pin must not depend on a GGUF being on this disk.
    struct FakeScorer;

    #[async_trait::async_trait]
    impl SentenceScorer for FakeScorer {
        async fn score(&self, query: &str, docs: &[String]) -> Result<Vec<f32>, String> {
            Ok(docs
                .iter()
                .map(|d| {
                    // Word overlap, scaled — monotone in shared content, so the
                    // max fold picks the chunk a human would pick.
                    let q: Vec<&str> = query.split_whitespace().collect();
                    let hit = q
                        .iter()
                        .filter(|w| d.to_lowercase().contains(&w.to_lowercase()))
                        .count();
                    hit as f32 / q.len().max(1) as f32
                })
                .collect())
        }
    }

    /// A scorer that breaks its contract — returns fewer margins than docs.
    struct ShortScorer;

    #[async_trait::async_trait]
    impl SentenceScorer for ShortScorer {
        async fn score(&self, _query: &str, _docs: &[String]) -> Result<Vec<f32>, String> {
            Ok(vec![0.5])
        }
    }

    fn chunks() -> Vec<String> {
        vec![
            "Karl Yundt giggled grimly, and Comrade Alexander Ossipon sat near Mr Verloc."
                .to_string(),
            "Sir Ethelred received the Assistant Commissioner in the small hours.".to_string(),
        ]
    }

    const ANSWER: &str = "Karl Yundt giggled grimly. Sir Ethelred received the Assistant \
                          Commissioner. Vladimir Stepanovich Haldin was never there.";

    // ── the splitter's contract survives the sweep ───────────────────────────

    #[tokio::test]
    async fn the_split_is_lossless_through_the_sweep() {
        let r = sweep("q", ANSWER, &chunks(), &FakeScorer).await.unwrap();
        assert_eq!(
            r.rejoined(),
            ANSWER,
            "rows must rebuild the answer byte-for-byte, or Delete/Fix cannot use them"
        );
    }

    // ── determinism (§7.4: HARD verdicts come from deterministic facets) ─────

    #[tokio::test]
    async fn the_sweep_is_a_pure_function_of_its_inputs() {
        let a = sweep("q", ANSWER, &chunks(), &FakeScorer).await.unwrap();
        let b = sweep("q", ANSWER, &chunks(), &FakeScorer).await.unwrap();
        assert_eq!(
            a.sentences, b.sentences,
            "two sweeps of one frozen fixture diverged"
        );
        assert_eq!(a.scored_pairs, b.scored_pairs);
    }

    // ── the margin fold ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn each_sentence_folds_to_its_own_best_chunk() {
        let r = sweep("q", ANSWER, &chunks(), &FakeScorer).await.unwrap();
        let rows: Vec<_> = r.scoreable().collect();
        assert_eq!(rows.len(), 3, "three content sentences");
        assert_eq!(rows[0].best_chunk, Some(0), "Yundt lives in chunk 0");
        assert_eq!(rows[1].best_chunk, Some(1), "Ethelred lives in chunk 1");
        assert!(
            rows[0].margin.unwrap() > rows[2].margin.unwrap(),
            "a supported sentence must outscore an invented one: {:?} vs {:?}",
            rows[0].margin,
            rows[2].margin
        );
    }

    #[tokio::test]
    async fn the_pool_is_capped_and_says_so() {
        let many: Vec<String> = (0..12).map(|i| format!("chunk number {i}")).collect();
        let r = sweep("q", "One sentence.", &many, &FakeScorer)
            .await
            .unwrap();
        assert!(r.k_cap_applied, "12 > EVIDENCE_K_CAP must be reported");
        assert_eq!(r.evidence_chunks, EVIDENCE_K_CAP);
        assert_eq!(r.scored_pairs, EVIDENCE_K_CAP, "one sentence x k chunks");
    }

    // ── absence is reported, never defaulted (§18.3) ─────────────────────────

    #[tokio::test]
    async fn an_empty_pool_yields_could_not_judge_not_a_zero_margin() {
        let r = sweep("q", ANSWER, &[], &FakeScorer).await.unwrap();
        for row in r.scoreable() {
            assert_eq!(
                row.margin, None,
                "no evidence must read as could-not-judge, not as margin 0.0"
            );
            assert_eq!(
                row.span,
                SpanResolution::Unverified {
                    reason: super::super::span_resolver::UnverifiedReason::NoEvidence
                }
            );
        }
        assert_eq!(r.scored_pairs, 0, "nothing was scored, and the count says so");
    }

    #[tokio::test]
    async fn a_scorer_that_breaks_its_contract_is_refused_not_zipped() {
        let err = sweep("q", ANSWER, &chunks(), &ShortScorer)
            .await
            .expect_err("a short margin vector must fail the sweep");
        assert!(
            err.contains("refusing to zip"),
            "the refusal must name what it refused: {err}"
        );
    }

    // ── the structural flags ride along ──────────────────────────────────────

    #[tokio::test]
    async fn the_span_resolution_rides_along_per_sentence() {
        let r = sweep("q", ANSWER, &chunks(), &FakeScorer).await.unwrap();
        let rows: Vec<_> = r.scoreable().collect();
        assert!(
            rows[0].span.is_resolved(),
            "a verbatim sentence must resolve: {:?}",
            rows[0].span
        );
        assert_eq!(
            rows[2].span,
            SpanResolution::Unverified {
                reason: super::super::span_resolver::UnverifiedReason::NotFound
            },
            "the invented sentence must be typed Unverified"
        );
    }

    async fn veto_labels(answer: &str) -> Vec<&'static str> {
        sweep("q", answer, &chunks(), &FakeScorer)
            .await
            .unwrap()
            .sentences
            .iter()
            .flat_map(|s| s.vetoes.iter().map(|v| v.label()))
            .collect()
    }

    #[tokio::test]
    async fn a_fabricated_source_attribution_is_vetoed() {
        // `absent_name_attribution` (judge.rs:890) fires on an ARTIFACT word
        // ("email") plus an adjacent Capitalized NAME PAIR that appears
        // nowhere in the evidence. A single capitalized word is deliberately
        // not enough — the veto was tightened after a soak fused list items
        // like "Hamilton, Madison" into a fictitious person.
        let vetoes = veto_labels("The email from Josiah Hargreaves confirms the shipment.").await;
        assert!(
            vetoes.contains(&"absent_name_attribution"),
            "the shipped veto must ride along; saw {vetoes:?}"
        );
    }

    #[tokio::test]
    async fn the_veto_does_not_fire_on_a_name_the_evidence_carries() {
        // Same artifact word, same two-word-name shape — the only difference
        // is that this pair IS in the evidence. Without this case the test
        // above would pass against a veto that fires on everything.
        let vetoes = veto_labels("The passage where Karl Yundt giggled is the one meant.").await;
        assert!(
            vetoes.is_empty(),
            "a real name in real evidence must not be flagged; saw {vetoes:?}"
        );
    }

    #[tokio::test]
    async fn a_clean_sentence_draws_no_vetoes() {
        let r = sweep("q", "Karl Yundt giggled grimly.", &chunks(), &FakeScorer)
            .await
            .unwrap();
        assert!(
            r.sentences.iter().all(|s| s.vetoes.is_empty()),
            "a supported sentence must not be flagged"
        );
    }

    #[tokio::test]
    async fn no_threshold_lives_in_this_module() {
        // Guarding principle 2 structurally: if a floor ever gets hard-coded
        // here, this test is where the next reader finds out. The sweep may
        // report `None`, but it may never report a DECISION.
        let r = sweep("q", ANSWER, &chunks(), &FakeScorer).await.unwrap();
        let json = serde_json::to_string(&r.sentences).unwrap();
        for banned in ["\"passes\"", "\"fails\"", "\"verdict\"", "\"grounded\":"] {
            assert!(
                !json.contains(banned),
                "the sweep emitted a decision ({banned}) — the floor belongs in the \
                 calibrated gate, beside its committed curve"
            );
        }
    }
}
