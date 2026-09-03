// ---- audit_window: the derived bound (GR-4) ----

/// covers: GR-4
#[test]
fn the_audit_window_is_derived_from_the_retrieved_leaf_set_not_a_constant() {
    // The failure this guards is the one the removed `max_chunks: 8`
    // produced: a literal that stops governing, so the auditor sees less
    // evidence than the drafter did and rejects claims it cannot reach.
    // Measured 2026-08-13: 32 of 57 failed claims had their support past
    // the eighth leaf chunk.
    let small = super::audit_window(3);
    let large = super::audit_window(30);
    assert_eq!(small, 3, "the window is the retrieved leaf set, not a cap");
    assert_eq!(large, 30, "the window is the retrieved leaf set, not a cap");
    assert_ne!(
        small, large,
        "a constant window would return the same bound for 3 and 30 leaf chunks"
    );

    // Monotone across the range a real turn spans: any reintroduced ceiling
    // shows up here as two adjacent inputs sharing a window.
    let mut previous = super::audit_window(1);
    for n in 2..=64 {
        let w = super::audit_window(n);
        assert_eq!(w, n, "window at {n} leaf chunks must equal the leaf set");
        assert!(w > previous, "window must grow with the leaf set at {n}");
        previous = w;
    }

    // The only non-identity case, and the reason `max(1)` is there at all:
    // the claim loop's take() bound must stay non-zero on empty evidence.
    assert_eq!(super::audit_window(0), 1);
}

// ---- project_verdict: the four-valued projection (ARCH §18.1) ----

#[test]
fn a_gate_that_never_ran_is_could_not_judge_not_supported() {
    // The regression this function exists for. `verify_grounding`
    // returns vp=0.0 from three paths that never checked anything;
    // before 2026-08-19 every one of them rendered as `Supported`.
    for vp in [0.0, 0.5, 1.0] {
        assert_eq!(
            super::project_verdict(Some(vp), false, 0.9, None),
            sovereign_contracts::types::GateJudgeVerdict::CouldNotJudge,
            "vp={vp} from an unmeasured path must never be a verdict about the answer"
        );
    }
}

#[test]
fn a_measured_vp_still_reads_against_tau() {
    assert_eq!(
        super::project_verdict(Some(0.0), true, 0.9, None),
        sovereign_contracts::types::GateJudgeVerdict::Supported
    );
    assert_eq!(
        super::project_verdict(Some(0.89), true, 0.9, None),
        sovereign_contracts::types::GateJudgeVerdict::Supported
    );
    assert_eq!(
        super::project_verdict(Some(0.9), true, 0.9, None),
        sovereign_contracts::types::GateJudgeVerdict::Unsupported
    );
    assert_eq!(
        super::project_verdict(Some(1.0), true, 0.9, None),
        sovereign_contracts::types::GateJudgeVerdict::Unsupported
    );
}

#[test]
fn without_a_vp_the_per_claim_ladder_speaks() {
    assert_eq!(
        super::project_verdict(None, true, 0.9, Some(true)),
        sovereign_contracts::types::GateJudgeVerdict::Supported
    );
    assert_eq!(
        super::project_verdict(None, true, 0.9, Some(false)),
        sovereign_contracts::types::GateJudgeVerdict::Unsupported
    );
    // Neither a vp nor a claim verdict: nothing judged anything.
    assert_eq!(
        super::project_verdict(None, true, 0.9, None),
        sovereign_contracts::types::GateJudgeVerdict::CouldNotJudge
    );
}

#[test]
fn an_absent_outcome_field_preserves_prior_behaviour() {
    // Rows written before `claim_check_outcome` existed default to
    // measured, so history is not silently reclassified.
    assert_eq!(
        super::project_verdict(Some(0.0), true, 0.9, None),
        sovereign_contracts::types::GateJudgeVerdict::Supported
    );
}
use super::*;

use crate::error::{Error, Result};
use crate::types::CompletionResponse;
use crate::types::{Depth, ProviderCapabilities};
use futures::Stream;
use std::pin::Pin;

fn chunk_with(corpus_id: &str, chunk_id: Option<u64>) -> corpus_engine::ScoredChunk {
    corpus_engine::ScoredChunk {
        content: "text".into(),
        title: None,
        url: None,
        corpus_id: corpus_id.into(),
        score: 1.0,
        metadata: std::collections::HashMap::new(),
        chunk_id,
        source_doc_id: None,
        vector_distance: None,
        // Fixture chunk: nothing acquired it (TOPOLOGY §10 rung 9.1).
        provenance: corpus_engine::index::ChunkProvenance::manufactured("test_fixture"),
    }
}

/// A target is a pure projection of what retrieval already knows — and
/// half a handle is worse than none, because a chunk id is unique only
/// WITHIN a corpus. A row missing either half would be a citation the
/// reading surface fails to deref at click time, after the reader has
/// already been told it is openable.
#[test]
fn a_target_needs_both_halves_of_the_handle() {
    let targets = gate_evidence_targets(&[
        chunk_with("chaos-saltgrass", Some(41)),
        // Synthetic / atlas-virtual chunk: no stable row id.
        chunk_with("chaos-saltgrass", None),
        // Corpus id absent — the chunk id alone resolves nothing.
        chunk_with("   ", Some(9)),
    ]);
    assert_eq!(targets.len(), 3, "targets stay PARALLEL to chunks");
    let first = targets[0].as_ref().expect("a real chunk is openable");
    assert_eq!(first.corpus_id, "chaos-saltgrass");
    assert_eq!(first.chunk_id, 41);
    assert_eq!(targets[1], None, "no row id, nothing to open");
    assert_eq!(
        targets[2], None,
        "a chunk id without a corpus resolves nothing"
    );
}

/// Targets are index-parallel to `chunks` and must survive the same
/// summary filter, or a click opens a passage the reader never saw.
/// This is the alignment argument `chunk_locators` documents, with a
/// worse failure mode.
#[test]
fn targets_stay_aligned_with_the_chunks_they_name() {
    let chunks = vec![
        chunk_with("ledger", Some(1)),
        chunk_with("ledger", Some(2)),
        chunk_with("ledger", Some(3)),
    ];
    let parts = gate_evidence_with_sources(&chunks);
    assert_eq!(parts.chunk_targets.len(), parts.chunks.len());
    for (i, t) in parts.chunk_targets.iter().enumerate() {
        assert_eq!(
            t.as_ref().map(|t| t.chunk_id),
            Some(i as u64 + 1),
            "slot {i} names the wrong chunk"
        );
    }
}

/// Prompt-routing mock for the gate's judge calls: claim
/// extraction returns a fixed claim; every forced-choice support
/// check returns `support` (as a logprob A/B distribution).
struct GateMock {
    support: bool,
}

#[async_trait::async_trait]
impl crate::traits::InferenceProvider for GateMock {
    async fn complete(
        &self,
        request: &crate::types::CompletionRequest,
    ) -> Result<CompletionResponse> {
        // P4-D contract: every judge call routed through this mock must
        // carry the OICP Judge envelope and NOT the old
        // `model_id: "primary"` pin (a latent privacy hole). This is the
        // capture-stub assertion — it fires on the real gate paths the
        // tests below drive (claim extraction + forced-choice support).
        assert!(
            request.model_id.is_none(),
            "P4-D: judge request must not pin model_id; got {:?}",
            request.model_id
        );
        let judge_oicp = request
            .oicp
            .as_ref()
            .expect("P4-D: judge request must carry an OICP Judge envelope");
        assert_eq!(
            judge_oicp.effective_latency_class(),
            crate::oicp::LatencyClass::Normal,
            "P4-D: Judge envelope latency class"
        );
        let text = if request
            .structured_output
            .as_ref()
            .map(|s| s.to_string().contains("x_forced_choice"))
            .unwrap_or(false)
        {
            if self.support {
                r#"{"A": 0.98, "B": 0.02}"#.to_string()
            } else {
                r#"{"A": 0.02, "B": 0.98}"#.to_string()
            }
        } else if request.prompt.contains("single central factual claim") {
            "The shop is located on Crescent Lane.".to_string()
        } else if request.prompt.contains("List the SPECIFIC factual claims") {
            // Longform per-claim extractor (gate_longform's audit).
            "The shop is located on Crescent Lane.\nThe shop sells loose-leaf tea.".to_string()
        } else if request
            .prompt
            .contains("Compare the ANSWER against the EVIDENCE")
        {
            // Specifics scan: nothing unsupported — keeps the
            // longform progress tests pinned to the claim loop.
            "NONE".to_string()
        } else {
            "unexpected synthesis call".to_string()
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "gate-mock".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &crate::types::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("GateMock: no streaming".into()))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: crate::types::Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

/// Mock for the retry-decline path: the first verify extracts a claim
/// and judges it unsupported (forcing the retry), the retry synthesis
/// returns a pure decline, and the re-verify extracts NO_CLAIM from it.
struct RetryDeclineMock;

#[async_trait::async_trait]
impl crate::traits::InferenceProvider for RetryDeclineMock {
    async fn complete(
        &self,
        request: &crate::types::CompletionRequest,
    ) -> Result<CompletionResponse> {
        let p = &request.prompt;
        let text = if request
            .structured_output
            .as_ref()
            .map(|s| s.to_string().contains("x_forced_choice"))
            .unwrap_or(false)
        {
            // Forced-choice support judge: unsupported.
            r#"{"A": 0.02, "B": 0.98}"#.to_string()
        } else if p.contains("single central factual claim") {
            if p.contains("I don't have reliable information") {
                // Re-extraction over the retry's decline text.
                "NO_CLAIM".to_string()
            } else {
                "The shop is located on Crescent Lane.".to_string()
            }
        } else {
            // The retry synthesis itself → a pure decline.
            "I don't have reliable information on this.".to_string()
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "gate-mock".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &crate::types::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented(
            "RetryDeclineMock: no streaming".into(),
        ))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: crate::types::Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

/// Mock for the NO_CLAIM extraction path: the claim extractor finds
/// nothing to audit (a decline or an unextractable reply), so the verify
/// ladder waves the text through as a NO_CLAIM release.
struct NoClaimGateMock;

#[async_trait::async_trait]
impl crate::traits::InferenceProvider for NoClaimGateMock {
    async fn complete(
        &self,
        request: &crate::types::CompletionRequest,
    ) -> Result<CompletionResponse> {
        assert!(
            request.model_id.is_none(),
            "P4-D: judge request must not pin model_id; got {:?}",
            request.model_id
        );
        Ok(CompletionResponse {
            text: "NO_CLAIM".to_string(),
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "gate-mock".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &crate::types::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented(
            "NoClaimGateMock: no streaming".into(),
        ))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: crate::types::Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

fn refinement_evidence() -> EvidenceContext {
    EvidenceContext {
        // These tests exercise the INCUMBENT ladder; a verdict here
        // would route them down the typed path and stop them testing
        // what they are named for.
        native_verdict: None,
        chunks: vec!["The shop sits on Harbour Row, by the quay.".to_string()],
        source_labels: Vec::new(),
        chunk_labels: Vec::new(),
        chunk_locators: Vec::new(),
        chunk_targets: Vec::new(),
        chunk_sources: Vec::new(),
        // Unstamped fixture — the pre-custody shape (custody.md §1);
        // these tests exercise the incumbent ladder, not custody.
        chunk_custodies: Vec::new(),
        chunk_urls: Vec::new(),
        searcher: None,
        entity_anchored: false,
        top_similarity: None,
    }
}

/// Custody refusal (custody.md §4, red R-3): a MIXED universe — at
/// least one stamped chunk plus an unstamped late append (sealed /
/// pinned evidence has no source row) — must refuse BEFORE any
/// judge call, and the funnel's ledger must record the unknown row.
///
/// The red's fixture trajectory points at the unstamped path in a
/// later wave (its refusal assertion is conditional on unknown
/// provenance appearing in a live turn); THIS test binds the
/// refusal structurally now — a refusal no test has watched fire is
/// not a refusal (ARCH §18.5).
#[tokio::test]
async fn unknown_provenance_in_mixed_universe_refuses() {
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(GateMock { support: true });
    let profile = GateSurface::Refinement.profile();
    let mut evidence = refinement_evidence();
    evidence
        .chunks
        .push("A recalled turn from this conversation.".to_string());
    evidence.chunk_custodies = vec![
        Some(crate::types::Custody::Personal),
        None, // the sealed/pinned late append — no stamp
    ];
    let outcome = gate_answer(
        &inference,
        "Where is the shop?",
        "The shop is on Harbour Row.".to_string(),
        &evidence,
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("refused_unknown_custody")
    );
    assert!(
        outcome.answer.text().starts_with("I couldn't confirm"),
        "the refusal must be an abstention-shaped release"
    );
    // The funnel's ledger records BOTH rows: the stamped chunk as
    // known, the late append as the unknown that triggered.
    let ledger = outcome
        .meta
        .get("chunk_custody")
        .and_then(|v| v.as_array())
        .expect("a stamped universe must carry the custody ledger");
    assert_eq!(ledger.len(), 2);
    assert_eq!(
        ledger[0].get("provenance_class").and_then(|v| v.as_str()),
        Some("known")
    );
    assert_eq!(
        ledger[0].get("custody").and_then(|v| v.as_str()),
        Some("personal")
    );
    assert_eq!(
        ledger[1].get("provenance_class").and_then(|v| v.as_str()),
        Some("unknown")
    );
}

#[test]
fn claim_budget_scales_with_length_within_bounds() {
    // Short answers keep the floor (the surface's min_claims).
    assert_eq!(claim_budget(0, 4), 4);
    assert_eq!(claim_budget(500, 4), 4);
    assert_eq!(claim_budget(2_399, 4), 4, "under 4*600 stays at floor");
    // The empirical fabrication distribution now scales meaningfully — at
    // the old 900/claim these got budget 4-9 (3630 got NO lift).
    assert_eq!(
        claim_budget(3_630, 4),
        6,
        "3630-char fabrication -> 6, not 4"
    );
    assert_eq!(claim_budget(4_550, 4), 7, "4550-char fabrication -> 7");
    // Very long answers are capped so per-claim judge latency stays bounded.
    assert_eq!(
        claim_budget(8_571, 4),
        10,
        "8571-char essay -> capped at 10"
    );
    assert_eq!(claim_budget(usize::MAX, 4), 10);
    // The floor is the surface's min, not a hardcoded 4.
    assert_eq!(claim_budget(500, 1), 1);
}

/// The cap is a RATIO of the audited claims, which is what its own comment
/// has always reasoned about. The two rows that name the old defect are the
/// first two assertions in each block: under the pre-2026-08-13 absolute cap
/// of 3, `(4 failed, 10 audited)` DECLINED and `(3 failed, 3 audited)` was
/// ADMITTED — both backwards.
#[test]
fn surgical_cap_is_a_minority_of_the_audited_claims() {
    let r = SURGICAL_MAX_FAILED_RATIO;

    // Longform, mostly grounded: surgery is available. This is the case the
    // absolute cap structurally excluded.
    assert!(
        surgery_admits(4, 10, r),
        "10-claim answer, 60% grounded — surgery, not full re-synthesis"
    );
    assert!(surgery_admits(5, 10, r), "exactly half is not a majority");
    assert!(!surgery_admits(6, 10, r), "6 of 10 IS most — decline");

    // Short answers tighten, which is the same correction in the other
    // direction: all three of three claims failing is 100% broken.
    assert!(
        !surgery_admits(3, 3, r),
        "every claim failed — the draft is broken, re-synthesise"
    );
    assert!(surgery_admits(1, 3, r), "1 of 3 is a minority");
    assert!(!surgery_admits(2, 3, r), "2 of 3 IS most — decline");

    // Odd counts round toward declining (integer majority).
    assert!(surgery_admits(3, 7, r));
    assert!(!surgery_admits(4, 7, r));

    // Degenerate inputs never reach surgery: nothing to repair, or nothing
    // audited to take a ratio of.
    assert!(!surgery_admits(0, 10, r), "no failures — caller releases");
    assert!(
        !surgery_admits(1, 0, r),
        "no audited claims — no denominator"
    );
    assert!(
        !surgery_admits(2, 1, r),
        "synthetic sweep findings can exceed the audited count; that declines"
    );

    // The knob's two forcing positions, which the calibration harness and
    // the negative-case demonstration both rely on.
    assert!(
        !surgery_admits(1, 100, 0.0),
        "ratio 0 forces full re-synthesis"
    );
    assert!(surgery_admits(100, 100, 1.0), "ratio 1 forces surgery");
}

/// The knob is read in exactly one place and a bad value cannot silently
/// move the cap.
#[test]
fn surgical_ratio_knob_rejects_out_of_range_values() {
    let d = SURGICAL_MAX_FAILED_RATIO;
    assert!((d - 0.5).abs() < f64::EPSILON, "0.5 == what 'most' means");

    // Unset -> the derived default.
    assert_eq!(parse_failed_ratio(None), d);
    // Valid values pass through, including both forcing positions.
    assert_eq!(parse_failed_ratio(Some("0")), 0.0);
    assert_eq!(parse_failed_ratio(Some("1.0")), 1.0);
    assert_eq!(parse_failed_ratio(Some(" 0.25 ")), 0.25);
    // Unparseable or out of range -> the default, never an arbitrary clamp
    // and never a silently-widened cap (#6: absence is reported, and the
    // warn! above is the report).
    for bad in ["", "3", "-0.1", "1.1", "half", "0.5.0", "NaN"] {
        assert_eq!(parse_failed_ratio(Some(bad)), d, "bad input {bad:?}");
    }
}

#[test]
fn answer_declines_skips_honest_abstentions_only() {
    // The exact short-band answers the specifics scan flagged as GOOD-but-
    // FLAGGED on 2026-07-01 — all honest abstentions the guard must SKIP so
    // it never wastes a corrective retry re-abstaining them.
    for decline in [
            "I don't have reliable information on the specific four authors listed for Chapter E.",
            "I am not certain of the value of `SWAP_THRESHOLD`. The provided sources do not contain this.",
            "The provided knowledge base sources do not contain this specific constant or file.",
            "I looked through the 12 passages your sources turned up for this, but none of them actually cover it — so I'd rather not guess.",
            "Based on the provided knowledge base, I do not have information regarding a character named \"Winnie\".",
            "The provided Rust snippets do not contain any assignment to a variable named `b`.",
            grounded_abstention("x", 12).as_str(),
        ] {
            assert!(answer_declines(decline), "should skip decline: {decline:?}");
        }
    // Real ASSERTING short answers the guard MUST scan — including the two
    // confirmed fabrications the guard exists to catch.
    for assert_ans in [
            "David Hart\n\nGrounded in the source: \"David Hart, Chief Operations Officer, Knowledge Process Software\"",
            "The most important thing is what Tokei does: it shows file-level stats (`--files`) and sorting (`--sort`).",
            "The three operations are index_stats, extract_shard, and merge_shards.",
        ] {
            assert!(!answer_declines(assert_ans), "should scan assertion: {assert_ans:?}");
        }
}

/// P1's arm identity (A1), at the one seam where the typed verdict
/// used to change a turn's ACTION.
///
/// Both directions of the retired shortcut are asserted, because both
/// were divergences: a confident answer under a typed `Abstain` used
/// to be reclassified as an abstention, and a prose decline under a
/// typed `Answer` used to escape reclassification. The prose strings
/// are chosen so the zoo disagrees with the verdict in each case —
/// a string the zoo already agreed with would make this pass for the
/// wrong reason.
#[test]
fn the_typed_verdict_no_longer_changes_a_turns_action_in_either_direction() {
    let confident_prose = "The harbormaster was found by Tabb Orrison at first light.";
    let declining_prose = "The sources do not contain that detail.";
    assert!(
        !released_pure_decline(confident_prose),
        "precondition: the zoo must NOT see this as a decline"
    );
    assert!(
        released_pure_decline(declining_prose),
        "precondition: the zoo must WANT to reclassify this"
    );
    // The signature carries no verdict at all: enforcement is absent
    // structurally, so there is no flag-on variant of this call to
    // diverge (ARCH §7 — make it structural, not remembered).
    assert_eq!(abstention_action(confident_prose), None);
    assert_eq!(
        abstention_action(declining_prose),
        Some("abstained_decline")
    );
}

/// **GR-12 — the short path's retry is not tombstonable the way the
/// longform ladder is, and the reason is a structural asymmetry, not
/// appetite.**
///
/// Note 4350f44d (2026-08-14): a tombstone order proposed removing the
/// short-path retry machinery alongside the longform repair ladder.
/// Nothing in the tree asserted why the two are different, so the next
/// order would have proposed it again — and this file's exit table is
/// where the difference actually lives.
///
/// The longform ladder can ANNOTATE: three of its exits carry
/// `GateReach::Flawed`, releasing an audited draft with its failed claims
/// marked, so retiring the repair machinery there still leaves the reader
/// an answer. The short path has no `Flawed` exit and cannot be given one
/// — a single-claim answer whose one claim failed has nothing left to mark,
/// the claim IS the answer. So on that path the retry is the ONLY producer
/// of an exit that both follows a failed verdict and still reaches the user
/// with an answer. Delete it and every post-failure door is `Declined`:
/// the gate stops being a quality lever and becomes an availability one.
#[test]
fn the_short_paths_retry_is_the_only_door_from_a_failed_claim_back_to_an_answer() {
    // (a) THE ASYMMETRY, as the exit table states it.
    let longform_flawed: Vec<&str> = [
        ACT_ANNOTATED_MARKED,
        ACT_ANNOTATED_NO_RETRY,
        ACT_ANNOTATED_REWRITE_ERROR,
        ACT_REWRITE_ANNOTATED,
    ]
    .iter()
    .filter(|a| a.reach == GateReach::Flawed)
    .map(|a| a.id)
    .collect();
    assert_eq!(
        longform_flawed.len(),
        4,
        "the longform ladder's annotate exits are what make ITS repair \
             machinery removable — a reader still gets the draft with its \
             failures marked. Found: {longform_flawed:?}"
    );

    const SHORT_PATH: &[GateAction] = &[
        ACT_RELEASED,
        ACT_RETRY_RELEASED,
        ACT_RETRY_RELEASED_SPECIFICS,
        ACT_RETRY_RELEASED_UNVERIFIED,
        ACT_ABSTAINED,
        ACT_ABSTAINED_NO_RETRY,
        ACT_ABSTAINED_WEAK_EVIDENCE,
        ACT_ABSTAINED_DECLINE,
        ACT_ABSTAINED_RETRY_ERROR,
        ACT_ABSTAINED_SPECIFICS,
        ACT_JUDGE_FAILED_OPEN,
    ];
    let short_flawed: Vec<&str> = SHORT_PATH
        .iter()
        .filter(|a| a.reach == GateReach::Flawed)
        .map(|a| a.id)
        .collect();
    assert!(
        short_flawed.is_empty(),
        "the short path grew an annotate exit ({short_flawed:?}). If a \
             single-claim answer can now be released with its one claim marked, \
             this asymmetry is gone and the retry's irreducibility argument has \
             to be re-derived rather than assumed"
    );

    // (b) THE STATE the caller requires: after a failed first verdict, the
    // only short-path exits that still hand the reader an answer.
    let answering_after_failure: Vec<&str> = SHORT_PATH
        .iter()
        .filter(|a| a.id != ACT_RELEASED.id && a.id != ACT_JUDGE_FAILED_OPEN.id)
        .filter(|a| a.reach == GateReach::Held)
        .map(|a| a.id)
        .collect();
    assert_eq!(
        answering_after_failure,
        vec!["retry_released", "retry_released_specifics"],
        "these are the retry's exits and nothing else produces them"
    );

    // (c) THE PRODUCER. Each of those states has exactly one production
    // site. Delete the retry and the count goes to zero HERE, naming the
    // state that went missing — rather than the build going quiet because
    // an unused `const` is not an error.
    const SRC: &str = include_str!("mod.rs");
    // (2026-09-02 split) the producers live across the gate module
    // family now; the guard's subject is the FAMILY, not one file.
    const INNER_SRC: &str = include_str!("inner.rs");
    const GATE_SRC: &str = include_str!("gate.rs");
    let prod: String = SRC
        .split("\n#[cfg(test)]")
        .next()
        .unwrap_or(SRC)
        .to_string()
        + INNER_SRC
        + GATE_SRC;
    for state in ["ACT_RETRY_RELEASED", "ACT_RETRY_RELEASED_SPECIFICS"] {
        // Every mention outside the constant's own declaration and outside
        // comments. Counted this way rather than by matching one assignment
        // shape, because the two exits are produced differently — one binds
        // `action`, the other is handed to `release_as` — and a guard that
        // knew only the first shape would sit green through the second
        // exit's removal.
        let sites: Vec<&str> = prod
            .lines()
            .map(str::trim_start)
            .filter(|t| {
                // Whole-name match: `ACT_RETRY_RELEASED` is a PREFIX of
                // `ACT_RETRY_RELEASED_UNVERIFIED` and of
                // `_SPECIFICS`, so a bare `contains` would let a sibling
                // exit vouch for a door that had been removed.
                let names = t.match_indices(state).any(|(i, _)| {
                    t[i + state.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| c != '_' && !c.is_ascii_alphanumeric())
                });
                names
                    && !t.starts_with("//")
                    && !t.starts_with(&format!("pub(crate) const {state}"))
            })
            .collect();
        assert!(
            !sites.is_empty(),
            "`{state}` has no producer left in production code. It is a door \
                 the short path cannot do without: there is no annotate exit to \
                 fall back on, so removing the retry does not simplify the \
                 ladder — it converts every failed single-claim turn into a \
                 refusal, and the reader loses the answer rather than the badge"
        );
    }
}

/// **GR-11 — the decline-recognition set decides while the typed
/// disposition reads `not_computed`, and neither may be retired in favour
/// of the other yet.**
///
/// Note df357e58 (2026-08-14): `answer_declines`, `released_pure_decline`
/// and the refusal-opener list were listed for retirement on the argument
/// that the typed disposition supersedes them. It does not — not yet. P1
/// made H1 telemetry, so on the overwhelming majority of turns the typed
/// field is the `not_computed` sentinel, and a field that does not decide
/// cannot be the only decider. Retiring the zoo against it would leave a
/// pure decline released as an ANSWER: the ledger would derive
/// `Unverified` instead of `CannotKnowFromHere`, the coverage probe would
/// never fire, and a genuine knowledge gap would mis-route as
/// `ClaimUncovered` (bench/gap_check/DECISION.md bug 2, observed on
/// `ood-australia-capital` over 10 retrieved distractors).
///
/// The sibling test above pins that the typed verdict does not CHANGE an
/// action. This one pins the other side, which is what the retirement
/// order would have broken: the action is still reached with the typed
/// field carrying nothing at all, and the decline guard's own condition
/// cannot read it.
#[test]
fn a_decline_is_still_recognised_while_the_typed_disposition_is_uncomputed() {
    const DECLINE: &str = "The sources do not contain that detail.";

    // The turn as it actually ships on a flag-off (or no-instrument) arm:
    // H1 produced nothing, so `with_native_verdict` writes the sentinel
    // into BOTH keys — and the action beside them is still the zoo's.
    let meta = with_native_verdict(
        serde_json::json!({
            "action": abstention_action(DECLINE)
                .expect("the zoo must recognise a pure decline"),
        }),
        None,
    );
    assert_eq!(
        meta["native_decision"], NATIVE_VERDICT_NOT_COMPUTED,
        "precondition: the typed disposition must be UNCOMPUTED here, or \
             this test is not exercising the case the retirement order missed"
    );
    assert_eq!(
        meta["native_answerability"], NATIVE_VERDICT_NOT_COMPUTED,
        "precondition: no score either"
    );
    assert_eq!(
        meta["action"], "abstained_decline",
        "a pure decline must be reclassified as an abstention even though \
             the typed disposition decided nothing — the zoo is the decider on \
             both arms until P3c"
    );

    // Structural: the decline guard's own condition must not consult the
    // typed verdict. Routing recognition through it is a one-token edit at
    // the call site (`&& native.is_some()`) that no pure-function test can
    // see, so the condition is read here directly. `include_str!` resolves
    // relative to THIS file (ARCH §7 — structural, not remembered).
    const SRC: &str = include_str!("inner.rs");
    let prod = SRC;
    let i = prod
        .find("let reclassify = ")
        .expect("the decline guard is gone — re-point this guard");
    let j = prod[i..]
        .find(".flatten();")
        .expect("the decline guard's condition no longer ends where this guard expects");
    let condition = &prod[i..i + j];
    assert!(
        !condition.contains("native"),
        "the decline guard reads H1's verdict. P1 retired the typed \
             shortcut in BOTH directions because it made the flag change a \
             turn's action, which is what A1's arm-identity kill forbids — and \
             while the disposition reads `not_computed` it would simply stop \
             recognising declines:\n{condition}"
    );
}

#[test]
fn released_pure_decline_separates_declines_from_caveated_answers() {
    // The P0 target shape: a provenance-flagged decline that asserts
    // nothing (observed on `ood-australia-capital` over 10 distractors).
    for decline in [
        "I don't have reliable information in my knowledge base about the capital of Australia.",
        "I do not have reliable information on that in your sources.",
        grounded_abstention("x", 12).as_str(),
    ] {
        assert!(
            released_pure_decline(decline),
            "pure decline must reclassify: {decline:?}"
        );
    }
    // Caveated PARAMETRIC ANSWERS — the decline-then-answer shapes that
    // must keep releasing (chaos hybrid honesty: OOD questions are meant
    // to be answered from general knowledge with the caveat).
    for answer in [
            "Not in your sources — from general knowledge: The capital of Australia is Canberra.",
            "I don't have reliable information in my knowledge base, but from general knowledge: Canberra is the capital of Australia.",
            "The capital of Australia is Canberra.",
        ] {
            assert!(
                !released_pure_decline(answer),
                "caveated/substantive answer must release: {answer:?}"
            );
        }
}

/// EPISTEMIC_STATE P0: a NO_CLAIM release whose text is a pure decline is
/// reclassified `abstained_decline` — the ledger then derives
/// `CannotKnowFromHere` and the coverage probe runs. The model's own
/// decline prose ships unchanged.
#[tokio::test]
async fn no_claim_pure_decline_release_becomes_abstention() {
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(NoClaimGateMock);
    let profile = GateSurface::Refinement.profile();
    let draft =
        "I don't have reliable information in my knowledge base about the capital of Australia."
            .to_string();
    let outcome = gate_answer(
        &inference,
        "What is the capital of Australia?",
        draft.clone(),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("abstained_decline")
    );
    assert_eq!(
        outcome.answer.text(),
        draft,
        "the model's own decline prose ships"
    );
    assert!(outcome.claims.is_empty(), "a decline asserts nothing");
}

/// **The census must install on the REAL gate path, not just in its own
/// unit tests.** `call_census`'s tests prove the funnel records what it
/// is given; this proves `gate_answer_with_progress` actually opens the
/// scope around the ladder — a task-local that silently failed to
/// install would leave every one of those tests green while production
/// journaled an empty `calls` vec on every turn, which reads exactly
/// like a turn that made no model calls (ARCH §18.4: validate the
/// instrument on the path you will read it from).
#[tokio::test]
async fn a_real_gate_turn_names_every_call_it_made() {
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(GateMock { support: true });
    let outcome = gate_answer(
        &inference,
        "Where is the shop?",
        "The shop is on Crescent Lane.".to_string(),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &GateSurface::KnowledgeQuery.profile(),
    )
    .await;
    let by_ms = outcome
        .meta
        .get("gate_call_ms")
        .and_then(|v| v.as_object())
        .expect("a gate turn that called the model must publish its census");
    // The short path's mechanisms, each named — the exact question the
    // reconstructed census could not answer. Note the shape this pins:
    // the short path judges support with `claim_chunk_support`
    // (`chunk_judge`, one passage per call), NOT the long-form path's
    // `claim_violation_joint` (`per_claim_judge`, the shared window).
    // Those two have very different prefill costs and the pre-census
    // instrument could not tell them apart at all.
    for expected in ["claim_extraction", "chunk_judge", "citation"] {
        assert!(
            by_ms.contains_key(expected),
            "{expected} must be named on the short path: {by_ms:?}"
        );
    }
    let n = outcome
        .meta
        .get("gate_call_n")
        .and_then(|v| v.as_object())
        .expect("counts ride alongside the milliseconds");
    assert_eq!(
        n.keys().collect::<Vec<_>>(),
        by_ms.keys().collect::<Vec<_>>(),
        "the two summaries must name the same mechanisms"
    );
}

/// A retry that produces a pure decline (re-extracted as NO_CLAIM,
/// vp=0) must NOT release as `retry_released` with the original claim
/// marked supported — that forges a Verified holding for a claim the
/// final text no longer asserts (observed: ood-table-salt shipped
/// ledger verdict `grounded` on "I don't have reliable information on
/// this.", 2026-07-20). It is an abstention.
#[tokio::test]
async fn retry_decline_is_abstention_not_supported_release() {
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(RetryDeclineMock);
    let profile = GateSurface::KnowledgeQuery.profile();
    assert!(profile.retry, "this path needs a retry-capable surface");
    let outcome = gate_answer(
        &inference,
        "Where is the shop?",
        "The shop is on Crescent Lane.".to_string(),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("abstained_decline")
    );
    assert_eq!(
        outcome.answer.text(),
        "I don't have reliable information on this."
    );
    assert!(
        !outcome.claims.iter().any(|c| c.supported),
        "no claim may be marked supported by a NO_CLAIM decline retry: {:?}",
        outcome.claims
    );
}

/// The exclusion half: a caveated general-knowledge ANSWER that extracts
/// NO_CLAIM must keep releasing — reclassifying it would score an
/// answered turn as an abstention (the unfaithful-proxy failure the
/// I2-C parity gate exists to catch).
#[tokio::test]
async fn no_claim_caveated_gk_answer_still_releases() {
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(NoClaimGateMock);
    let profile = GateSurface::Refinement.profile();
    let draft =
        "Not in your sources — from general knowledge: The capital of Australia is Canberra."
            .to_string();
    let outcome = gate_answer(
        &inference,
        "What is the capital of Australia?",
        draft.clone(),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("released")
    );
    assert_eq!(outcome.answer.text(), draft);
}

/// The Phase-6 invariant's gate half: verify-only (retry: false)
/// on an unsupported claim must return `abstained_no_retry` — the
/// caller (collaboration refinement) keeps the verified original.
#[tokio::test]
async fn verify_only_failure_is_abstained_no_retry() {
    let inference: Arc<dyn crate::traits::InferenceProvider> =
        Arc::new(GateMock { support: false });
    let profile = GateSurface::Refinement.profile();
    assert!(!profile.retry);
    let outcome = gate_answer(
        &inference,
        "Where is the shop?",
        "The shop is on Crescent Lane.".to_string(),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("abstained_no_retry")
    );
    // grounded_abstention was rewritten (2026-06-17) to stop restating the
    // rejected claim verbatim (it leaked the fabrication + read as "answered"
    // to the primary judge), then re-toned (2026-06-30) to drop the abrupt
    // "so I'm not going to state one" lecture for a warm, helpful refusal,
    // then re-scoped (2026-07-08) from a universal negative about the sources
    // ("none of them cover it") to a self-scoped hedge ("I couldn't confirm")
    // so a mis-abstain isn't a FALSE claim about the sources. The action is
    // the invariant; the wording is graceful and source-honest.
    assert!(outcome.answer.text().starts_with("I couldn't confirm"));
    assert!(!outcome.answer.text().contains("not going to state"));
    // Must NOT assert a universal negative about the sources' content.
    assert!(!outcome.answer.text().contains("none of them"));
    assert!(!outcome.answer.text().contains("not recorded there"));
}

/// Supported claims release unchanged under verify-only.
#[tokio::test]
async fn verify_only_supported_claim_releases() {
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(GateMock { support: true });
    let profile = GateSurface::Refinement.profile();
    let draft = "The shop is on Harbour Row.".to_string();
    let outcome = gate_answer(
        &inference,
        "Where is the shop?",
        draft.clone(),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("released")
    );
    assert_eq!(outcome.answer.text(), draft);
}

fn native_verdict(
    decision: sovereign_contracts::types::GroundingDecision,
    answerability: f32,
) -> crate::types::GroundingVerdict {
    crate::types::GroundingVerdict {
        decision,
        answerability,
        semantic_entropy: None,
        agreement: None,
        decided_by: sovereign_contracts::types::DeciderId::Router,
        segments: Vec::new(),
    }
}

/// `refinement_evidence` with an H1 verdict attached — the arm where
/// the instrument RAN. Telemetry only: the tests below assert the
/// action is the same one the verdict-free turn produces.
fn evidence_with_native(verdict: crate::types::GroundingVerdict) -> EvidenceContext {
    EvidenceContext {
        native_verdict: Some(verdict),
        ..refinement_evidence()
    }
}

/// H1's verdict must be readable on a NON-decline action. Until
/// 2026-08-12 only the decline-guard exit attached the pair, so every
/// released turn — the bulk of any soak — carried nothing, and the
/// existing coverage (which only ever drove the decline branch) could
/// not see it. Absence reported as a value is ARCH §18.3, and the
/// native-grounding flip soak of 2026-08-11 misread it exactly so:
/// 69 of 73 turns took non-decline actions.
#[tokio::test]
async fn native_telemetry_rides_a_released_turn() {
    use sovereign_contracts::types::GroundingDecision;
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(GateMock { support: true });
    let profile = GateSurface::Refinement.profile();
    // 0.25 is exactly representable in binary32, so the f32 → f64
    // widening into JSON is lossless and this assert can be exact.
    let outcome = gate_answer(
        &inference,
        "Where is the shop?",
        "The shop is on Harbour Row.".to_string(),
        &evidence_with_native(native_verdict(GroundingDecision::Answer, 0.25)),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("released"),
        "instrument only: a verdict on the turn must not move the action \
             (same action as `verify_only_supported_claim_releases`)"
    );
    assert_eq!(
        outcome
            .meta
            .get("native_answerability")
            .and_then(|v| v.as_f64()),
        Some(0.25),
        "H1's answerability must be readable on a non-decline action: {}",
        outcome.meta
    );
    assert_eq!(
        outcome.meta.get("native_decision").and_then(|v| v.as_str()),
        Some("released"),
        "H1's decision must be readable on a non-decline action: {}",
        outcome.meta
    );
}

/// The long-form ladder's exits carry it too — seven of the fifteen
/// `GateOutcome` sites live there, and a soak's essays all come out
/// of them.
#[tokio::test]
async fn native_telemetry_rides_the_longform_ladder() {
    use sovereign_contracts::types::GroundingDecision;
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(GateMock { support: true });
    let profile = GateSurface::Refinement.profile();
    let pivot = std::env::var("SOVEREIGN_LONGFORM_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(profile.longform_chars)
        .max(profile.longform_chars);
    let draft = "The shop sits on Harbour Row, by the quay. ".repeat(pivot / 40 + 2);
    let outcome = gate_answer(
        &inference,
        "Tell me about the shop.",
        draft,
        &evidence_with_native(native_verdict(GroundingDecision::Abstain, 0.125)),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("mode").and_then(|m| m.as_str()),
        Some("per_claim"),
        "this test must drive the long-form ladder"
    );
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("released"),
        "instrument only: an Abstain verdict must not move the ladder's action"
    );
    assert_eq!(
        outcome
            .meta
            .get("native_answerability")
            .and_then(|v| v.as_f64()),
        Some(0.125)
    );
    assert_eq!(
        outcome.meta.get("native_decision").and_then(|v| v.as_str()),
        Some("abstained"),
        "H1 said abstain while the ladder released — reported beside the \
             decision, never in place of it"
    );
}

/// Build a draft long enough to drive the per-claim ladder on `profile`.
fn longform_draft(profile: &GroundingProfile) -> String {
    let pivot = std::env::var("SOVEREIGN_LONGFORM_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(profile.longform_chars)
        .max(profile.longform_chars);
    "The shop sits on Harbour Row, by the quay. ".repeat(pivot / 40 + 2)
}

/// TOMBSTONE (ECONOMY §9 Phase 4). On the DEFAULT configuration a
/// retry-capable surface whose audit found failures must release the
/// AUDITED DRAFT with those claims marked — never a re-synthesis.
///
/// The load-bearing assertion is the NEGATIVE one at the end: this mock
/// answers any non-judge completion with a fixed sentinel, so the
/// sentinel's absence from the released text proves no synthesis call was
/// made. That is what separates this from the reverted 2026-07-17
/// experiment (§7.4) — there, unaudited regenerated prose shipped with
/// its check removed; here nothing is regenerated at all, so there is no
/// unaudited text to check.
/// covers: GR-6
#[tokio::test]
async fn tombstoned_repair_releases_the_audited_draft_with_its_claims_marked() {
    if std::env::var("SOVEREIGN_GATE_LONGFORM_REPAIR").is_ok() {
        return; // the knob is set in this environment; default not under test
    }
    let inference: Arc<dyn crate::traits::InferenceProvider> =
        Arc::new(GateMock { support: false });
    let profile = GateSurface::KnowledgeQuery.profile();
    assert!(
        profile.retry,
        "this test must drive a surface where repair IS allowed — \
             otherwise it proves nothing about the tombstone"
    );
    let outcome = gate_answer(
        &inference,
        "Tell me about the shop.",
        longform_draft(&profile),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("mode").and_then(|m| m.as_str()),
        Some("per_claim"),
        "this test must drive the long-form ladder"
    );
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("annotated_marked"),
        "a retry-capable surface with the repair ladder tombstoned marks; \
             `annotated_no_retry` here would tell a Refinement consumer this \
             was a rejected refinement"
    );
    assert_eq!(
        outcome.meta.get("retried").and_then(|r| r.as_bool()),
        Some(false),
        "nothing was re-synthesised, so nothing was retried"
    );
    assert!(
        !outcome
            .meta
            .get("failed_claims")
            .and_then(|f| f.as_array())
            .expect("failed_claims present")
            .is_empty(),
        "the caught claims must be REPORTED, not dropped (ARCH §18.3)"
    );
    assert!(
        outcome.claims.iter().any(|c| c.failed_once && !c.supported),
        "the mark itself: a failed claim rides out as an unsupported \
             holding, which is what the epistemic ledger renders as \
             `failed_once` and what flips the turn's verdict to `mixed`"
    );
    // The two halves of "nothing was regenerated", which is the whole
    // safety argument in §7.4. The negative is the stronger one: this
    // mock answers any non-judge completion with a fixed sentinel, so
    // that string appearing in the released text is a synthesis call
    // that should not have happened.
    assert!(
        !outcome.answer.text().contains("unexpected synthesis call"),
        "the tombstoned path must make NO synthesis call — the released \
             text carries the mock's sentinel, so a rewrite ran"
    );
    assert!(
        outcome.answer.text().contains("Harbour Row"),
        "the released text must still be the audited draft"
    );
}

/// Mock for the INCREMENTAL re-audit path (order audit-economy D4):
/// extraction yields three claims, the forced-choice judge fails exactly
/// the "Crescent Lane" one, and the counters record how many times each
/// register ran — the two load-bearing counts being extraction (must be
/// 1: the incremental re-audit skips it) and the scan (must be 2: the
/// holistic floor runs on BOTH passes; the 2026-07-17 leak was a scoped
/// re-audit that skipped it).
struct IncrementalMock {
    extractions: std::sync::atomic::AtomicUsize,
    scans: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl crate::traits::InferenceProvider for IncrementalMock {
    async fn complete(
        &self,
        request: &crate::types::CompletionRequest,
    ) -> Result<CompletionResponse> {
        use std::sync::atomic::Ordering;
        let p = &request.prompt;
        let text = if request
            .structured_output
            .as_ref()
            .map(|s| s.to_string().contains("x_forced_choice"))
            .unwrap_or(false)
        {
            if p.contains("CLAIM: The shop is located on Crescent Lane") {
                r#"{"A": 0.02, "B": 0.98}"#.to_string()
            } else {
                r#"{"A": 0.98, "B": 0.02}"#.to_string()
            }
        } else if p.contains("List the SPECIFIC factual claims") {
            self.extractions.fetch_add(1, Ordering::SeqCst);
            "The shop is located on Crescent Lane.\n\
                 The shop sits on Harbour Row, by the quay.\n\
                 The shop is by the quay."
                .to_string()
        } else if p.contains("Compare the ANSWER against the") {
            // Matches both scan registers: the pre-D3 shape ("…against
            // the EVIDENCE") and the family-joined A' shape ("…against
            // the passages above", order audit-economy D3).
            self.scans.fetch_add(1, Ordering::SeqCst);
            "NONE".to_string()
        } else if p.contains("CLAIMS (numbered):") {
            // Batched pre-pass (if a config enables it): all supported.
            "1: A\n2: A\n3: A".to_string()
        } else {
            "unexpected synthesis call".to_string()
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "incremental-mock".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &crate::types::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented(
            "IncrementalMock: no streaming".into(),
        ))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: crate::types::Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

/// D4 (order audit-economy): with the repair ladder armed, a surgical
/// repair is followed by the INCREMENTAL re-audit — extraction runs once
/// (audit#1 only), the repaired text is released, the untouched verified
/// claims are carried into the ledger, and the holistic scan still runs
/// on the corrected text. Uses the repair env knob; safe under nextest's
/// process-per-test model, and the tombstone test independently guards
/// itself against a set knob.
#[tokio::test]
async fn surgical_repair_takes_the_incremental_reaudit_and_keeps_the_holistic_floor() {
    std::env::set_var("SOVEREIGN_GATE_LONGFORM_REPAIR", "1");
    let mock = Arc::new(IncrementalMock {
        extractions: std::sync::atomic::AtomicUsize::new(0),
        scans: std::sync::atomic::AtomicUsize::new(0),
    });
    let inference: Arc<dyn crate::traits::InferenceProvider> = mock.clone();
    let profile = GateSurface::KnowledgeQuery.profile();
    assert!(profile.retry, "must drive a repair-capable surface");
    // The audited draft: verified filler plus one fabricated sentence the
    // judge fails. No corrective evidence exists (no searcher), so
    // surgery resolves it as a DELETE — the repaired text carries no new
    // prose and the incremental re-audit owes zero per-claim calls.
    let draft = format!(
        "{} The shop is located on Crescent Lane.",
        longform_draft(&profile)
    );
    let outcome = gate_answer(
        &inference,
        "Tell me about the shop.",
        draft,
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    std::env::remove_var("SOVEREIGN_GATE_LONGFORM_REPAIR");
    use std::sync::atomic::Ordering;
    assert_eq!(
        outcome.meta.get("mode").and_then(|m| m.as_str()),
        Some("per_claim"),
        "must drive the long-form ladder"
    );
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("rewrite_released"),
        "surgery deleted the fabrication and the incremental re-audit \
             found the repaired text clean"
    );
    assert_eq!(
        mock.extractions.load(Ordering::SeqCst),
        1,
        "THE incremental claim: extraction ran for audit#1 only — the \
             re-audit judged the repaired spans, not a re-extracted claim list"
    );
    assert_eq!(
        mock.scans.load(Ordering::SeqCst),
        2,
        "the holistic scan ran on BOTH passes — the 2026-07-17 leak came \
             from a scoped re-audit that skipped it, and this floor is \
             structural, not remembered"
    );
    assert!(
        !outcome.answer.text().contains("Crescent Lane"),
        "the fabricated sentence is gone"
    );
    assert!(
        outcome.answer.text().contains("Harbour Row"),
        "the verified prose survives"
    );
    assert!(
        !outcome.answer.text().contains("unexpected synthesis call"),
        "no full re-synthesis ran — surgery handled it"
    );
    assert!(
        outcome
            .claims
            .iter()
            .any(|c| c.supported && c.text.contains("Harbour Row")),
        "the untouched verified claims are CARRIED into the released \
             ledger — an empty holdings list would read as 'less was \
             verified' (ARCH §18.3)"
    );
}

/// Mock for the ASYMMETRIC-TRUST batched verdict path (order
/// audit-economy D2). Extraction yields six claims; the batched pass
/// declares 1-4 supported, 5 unsupported, and omits 6 (parse gap); the
/// calibrated forced-choice supports everything it is asked. The
/// counters record which claims reached the calibrated judge.
struct AsymmetricBatchMock {
    batch_calls: std::sync::atomic::AtomicUsize,
    judged_claims: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl crate::traits::InferenceProvider for AsymmetricBatchMock {
    async fn complete(
        &self,
        request: &crate::types::CompletionRequest,
    ) -> Result<CompletionResponse> {
        use std::sync::atomic::Ordering;
        let p = &request.prompt;
        let text = if request
            .structured_output
            .as_ref()
            .map(|s| s.to_string().contains("x_forced_choice"))
            .unwrap_or(false)
        {
            if let Some(claim) = p.split("CLAIM: ").nth(1).and_then(|s| s.lines().next()) {
                self.judged_claims
                    .lock()
                    .unwrap()
                    .push(claim.trim().to_string());
            }
            // The calibrated judge SUPPORTS everything it is asked.
            r#"{"A": 0.98, "B": 0.02}"#.to_string()
        } else if p.contains("List the SPECIFIC factual claims") {
            "The shop sits on Harbour Row, by the quay.\n\
                 The shop is by the quay.\n\
                 The shop is on Harbour Row.\n\
                 The shop sells rope.\n\
                 The shop is painted blue.\n\
                 The shop opens at dawn."
                .to_string()
        } else if p.contains("CLAIMS (numbered):") {
            self.batch_calls.fetch_add(1, Ordering::SeqCst);
            // 1-4 supported, 5 unsupported, 6 omitted (parse gap).
            "1: A\n2: A\n3: A\n4: A\n5: B".to_string()
        } else if p.contains("Compare the ANSWER against the") {
            "NONE".to_string()
        } else {
            "unexpected synthesis call".to_string()
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "asymmetric-batch-mock".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &crate::types::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented(
            "AsymmetricBatchMock: no streaming".into(),
        ))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: crate::types::Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

/// D2 (order audit-economy): the batched verdict is trusted
/// ASYMMETRICALLY. Batch "supported" releases the claim with no
/// per-claim call; batch "unsupported" and parse gaps are DECIDED by the
/// calibrated forced-choice, never by the uncalibrated text A/B. Under
/// the retired both-directions wiring this test fails two ways at once:
/// the calibrated judge would see only the gap row (one call, not two)
/// and the batch-unsupported claim would ship flagged at vp 1.0 against
/// a judge that clears it. Env knob is safe under nextest's
/// process-per-test model.
#[tokio::test]
async fn batch_unsupported_falls_through_to_the_calibrated_judge() {
    std::env::set_var("SOVEREIGN_GATE_BATCH_VERIFY", "1");
    let mock = Arc::new(AsymmetricBatchMock {
        batch_calls: std::sync::atomic::AtomicUsize::new(0),
        judged_claims: std::sync::Mutex::new(Vec::new()),
    });
    let inference: Arc<dyn crate::traits::InferenceProvider> = mock.clone();
    let profile = GateSurface::KnowledgeQuery.profile();
    // Long enough that claim_budget (600 chars/claim, cap 10) admits all
    // six extracted claims and the batched pre-pass fires at the default
    // SOVEREIGN_GATE_BATCH_MIN_CLAIMS=6.
    let mut draft = longform_draft(&profile);
    while draft.len() < 3_700 {
        draft.push_str("The shop sits on Harbour Row, by the quay. ");
    }
    let outcome = gate_answer(
        &inference,
        "Tell me about the shop.",
        draft,
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    std::env::remove_var("SOVEREIGN_GATE_BATCH_VERIFY");
    use std::sync::atomic::Ordering;
    assert_eq!(
        outcome.meta.get("mode").and_then(|m| m.as_str()),
        Some("per_claim"),
        "must drive the long-form ladder"
    );
    assert_eq!(
        mock.batch_calls.load(Ordering::SeqCst),
        1,
        "one prefill, N verdicts — the batched pass ran exactly once"
    );
    let judged = mock.judged_claims.lock().unwrap().clone();
    assert_eq!(
        judged.len(),
        2,
        "EXACTLY the batch-unsupported claim and the parse-gap claim \
             reached the calibrated judge — batch-supported rows were \
             released without a per-claim call; got {judged:?}"
    );
    assert!(
        judged.iter().any(|c| c.contains("painted blue"))
            && judged.iter().any(|c| c.contains("opens at dawn")),
        "the fall-through rows are the unsupported and gap claims, \
             not arbitrary ones; got {judged:?}"
    );
    assert!(
        outcome.claims.iter().all(|c| c.supported),
        "a batch 'unsupported' is triage, never a released flag: the \
             calibrated judge cleared it, so nothing ships flagged"
    );
}

/// The two dispositions that share the marking branch keep separate
/// names. `runtime/collaboration.rs` reads `annotated_no_retry` as
/// "the refined text failed, keep the prior verified answer" — if the
/// tombstone reused that string, every marked knowledge answer would
/// read as a rejected refinement (ARCH §10.6).
// ───── Issue #57: the fan-out is concurrent and bounded, and an unjudged claim never ships as verified ─────

/// A sealed-evidence searcher with a scripted cost per call, counting
/// calls and peak overlap.
struct ScriptedSearcher {
    search_ms: u64,
    calls: std::sync::atomic::AtomicUsize,
    in_flight: std::sync::atomic::AtomicUsize,
    max_in_flight: std::sync::atomic::AtomicUsize,
    claims_seen: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl crate::runtime::grounding::search::SealedEvidenceSearch for ScriptedSearcher {
    async fn search(&self, claim: &str) -> Vec<String> {
        use std::sync::atomic::Ordering::SeqCst;
        self.calls.fetch_add(1, SeqCst);
        // Peak overlap, sampled on entry. This is the only thing that can
        // tell a concurrent fan-out from a serial one from the outside,
        // and the only thing that can catch the bound being removed.
        let cur = self.in_flight.fetch_add(1, SeqCst) + 1;
        self.max_in_flight.fetch_max(cur, SeqCst);
        self.claims_seen.lock().unwrap().push(claim.to_string());
        if self.search_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.search_ms)).await;
        }
        self.in_flight.fetch_sub(1, SeqCst);
        vec!["The shop on Harbour Row sells rope, is painted blue and opens at dawn.".to_string()]
    }
}

/// Six extracted claims (exactly the old batch threshold); every
/// forced-choice judge either clears the claim or — when `judges_fail` —
/// refuses the way a shed admission queue refuses ("host busy"). Counts
/// the batched pass and the per-claim judge calls.
struct LadderMock {
    batch_calls: std::sync::atomic::AtomicUsize,
    judge_calls: std::sync::atomic::AtomicUsize,
    judges_fail: bool,
}

#[async_trait::async_trait]
impl crate::traits::InferenceProvider for LadderMock {
    async fn complete(
        &self,
        request: &crate::types::CompletionRequest,
    ) -> Result<CompletionResponse> {
        use std::sync::atomic::Ordering;
        let p = &request.prompt;
        let forced_choice = request
            .structured_output
            .as_ref()
            .map(|s| s.to_string().contains("x_forced_choice"))
            .unwrap_or(false);
        let text = if forced_choice {
            self.judge_calls.fetch_add(1, Ordering::SeqCst);
            if self.judges_fail {
                return Err(Error::NotImplemented(
                    "host busy: ~75850 ms predicted wait at queue position 1".into(),
                ));
            }
            r#"{"A": 0.98, "B": 0.02}"#.to_string()
        } else if p.contains("List the SPECIFIC factual claims") {
            "The shop sits on Harbour Row, by the quay.\n\
                 The shop is by the quay.\n\
                 The shop is on Harbour Row.\n\
                 The shop sells rope.\n\
                 The shop is painted blue.\n\
                 The shop opens at dawn."
                .to_string()
        } else if p.contains("CLAIMS (numbered):") {
            self.batch_calls.fetch_add(1, Ordering::SeqCst);
            "1: A\n2: A\n3: A\n4: A\n5: A\n6: B".to_string()
        } else if p.contains("Compare the ANSWER against the") {
            "NONE".to_string()
        } else {
            "unexpected synthesis call".to_string()
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "ladder-mock".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &crate::types::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("LadderMock: no streaming".into()))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: crate::types::Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

/// One small-corpus grounded turn: a draft long enough for a claim
/// budget of six, sealed evidence with a scripted searcher attached.
fn fanout_fixture(
    search_ms: u64,
    judges_fail: bool,
) -> (
    Arc<LadderMock>,
    Arc<ScriptedSearcher>,
    EvidenceContext,
    String,
    GroundingProfile,
) {
    let mock = Arc::new(LadderMock {
        batch_calls: Default::default(),
        judge_calls: Default::default(),
        judges_fail,
    });
    let searcher = Arc::new(ScriptedSearcher {
        search_ms,
        calls: Default::default(),
        in_flight: Default::default(),
        max_in_flight: Default::default(),
        claims_seen: Default::default(),
    });
    let mut evidence = refinement_evidence();
    evidence.searcher =
        Some(searcher.clone() as Arc<dyn crate::runtime::grounding::search::SealedEvidenceSearch>);
    let profile = GateSurface::KnowledgeQuery.profile();
    // >= 3,600 chars -> claim_budget 6, so all six extracted claims are audited.
    let mut draft = longform_draft(&profile);
    while draft.len() < 3_700 {
        draft.push_str("The shop sits on Harbour Row, by the quay. ");
    }
    (mock, searcher, evidence, draft, profile)
}

/// Issue #57, the correctness defect. A per-claim judge that returned no
/// verdict shipped as `supported: true`, the audit exited `released`, and
/// the epistemic ledger rendered every holding Verified — observed as
/// eight shed judges on a `grounded` turn. Now every unjudged claim is
/// recorded, the exit is `judge_failed_open`, and no claim is supported.
#[tokio::test]
async fn unjudged_claims_exit_judge_failed_open_never_released() {
    use std::sync::atomic::Ordering;
    let (mock, _searcher, evidence, draft, profile) = fanout_fixture(0, true);
    let inference: Arc<dyn crate::traits::InferenceProvider> = mock.clone();
    let outcome = gate_answer(
        &inference,
        "Tell me about the shop.",
        draft,
        &evidence,
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("mode").and_then(|m| m.as_str()),
        Some("per_claim"),
        "must drive the long-form ladder"
    );
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("judge_failed_open"),
        "six judges refused: the gate fell open and must say so, never `released`; meta={}",
        outcome.meta
    );
    assert_eq!(
        outcome
            .meta
            .get("claim_check_outcome")
            .and_then(|a| a.as_str()),
        Some("could-not-judge")
    );
    assert_eq!(
        outcome.meta.get("unjudged_claims").and_then(|v| v.as_u64()),
        Some(6)
    );
    assert_eq!(outcome.claims.len(), 6);
    assert!(
        outcome
            .claims
            .iter()
            .all(|c| c.unjudged && !c.supported && !c.failed_once),
        "every record carries `unjudged` and nothing else; got {:?}",
        outcome.claims
    );
    assert_eq!(
        mock.judge_calls.load(Ordering::SeqCst),
        6,
        "each claim was offered to the judge exactly once"
    );
}

/// CHANGE 3'S REGRESSION GUARD (issue #57, 2026-09-02). `claims x corpora`
/// is the turn's only multiplicative term and it ran serially on BOTH
/// axes. The searches are hoisted out of the sequential judging loop and
/// run `buffered`, which is a SCHEDULING change and nothing more: it must
/// overlap them, and it must never overlap more than the derived bound.
/// The nested fan-out multiplies into that bound, and the product — up to
/// sixteen concurrent `open_index` + hybrid searches against 88 GB indexes
/// on a host holding a 17.7 GB model — caused a memory event on this host.
///
/// FAILS IF: the loop returns to `for … .await` (peak overlap falls to
/// one), or the bound is dropped (peak overlap exceeds it).
#[tokio::test]
async fn the_claim_search_fanout_overlaps_and_never_exceeds_its_bound() {
    use std::sync::atomic::Ordering;
    let (mock, searcher, evidence, draft, profile) = fanout_fixture(40, false);
    let inference: Arc<dyn crate::traits::InferenceProvider> = mock.clone();
    let _ = gate_answer(
        &inference,
        "Tell me about the shop.",
        draft,
        &evidence,
        &CompletionRequest::default(),
        &profile,
    )
    .await;

    assert_eq!(
        mock.batch_calls.load(Ordering::SeqCst),
        0,
        "no study flag is set, so no batched model call runs"
    );
    // Six audited claims, every one of them in the fan-out.
    const FANNED: usize = 6;
    assert_eq!(searcher.calls.load(Ordering::SeqCst), FANNED);

    let bound = config::claim_search_concurrency();
    let peak = searcher.max_in_flight.load(Ordering::SeqCst);
    assert!(
        peak <= bound,
        "peak overlap {peak} exceeded the one bound {bound} — the product is unbounded again"
    );
    assert_eq!(
        peak,
        bound.min(FANNED),
        "the fan-out must SATURATE its bound, not merely stay under it"
    );
    if bound > 1 {
        assert!(peak > 1, "a serial fan-out shows one search in flight");
    }
}

/// The hoist is a scheduling change, so it must lose nothing and repeat
/// nothing: every claim the serial loop would have searched is searched,
/// exactly once. The fan-out and the judging loop read ONE disposition
/// vector; this is what catches a second predicate creeping back in — one
/// that drops a claim costs a verdict, one that leaves the loop's own
/// search in place pays for it twice.
#[tokio::test]
async fn the_fanout_searches_every_audited_claim_exactly_once() {
    use std::sync::atomic::Ordering;
    let (mock, searcher, evidence, draft, profile) = fanout_fixture(10, false);
    let inference: Arc<dyn crate::traits::InferenceProvider> = mock.clone();
    let _ = gate_answer(
        &inference,
        "Tell me about the shop.",
        draft,
        &evidence,
        &CompletionRequest::default(),
        &profile,
    )
    .await;

    assert_eq!(mock.batch_calls.load(Ordering::SeqCst), 0);
    let seen = searcher.claims_seen.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        6,
        "every audited claim is searched, exactly once"
    );
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "a claim was searched twice — the hoist must REPLACE the loop's search, not add to it"
    );
}

/// The bound is ONE permit for the WHOLE nested fan-out, not one per
/// level. Bounding each level separately bounds NEITHER: four claims by
/// four corpora is sixteen concurrent `open_index` + hybrid searches, and
/// that product caused a memory event on 2026-09-02. Process-global rather
/// than per-turn because the resource it protects — page cache, file
/// handles, the box — is global.
///
/// FAILS IF: the semaphore becomes per-call or per-turn, or is sized to
/// anything but the derived concurrency, or the concurrency stops being
/// derived from the host and becomes a constant again.
#[test]
fn the_claim_search_bound_is_one_process_wide_permit() {
    let a = config::claim_search_permits();
    let b = config::claim_search_permits();
    assert!(
        std::ptr::eq(a, b),
        "two callers got two semaphores — the nested product would be unbounded again"
    );
    assert_eq!(
        config::claim_search_permits_bound(),
        config::claim_search_concurrency(),
        "the one bound must be sized to the derived concurrency"
    );
    // The DERIVATION, against named core counts. Re-deriving
    // `(cores / 4).clamp(1, 4)` here to check `(cores / 4).clamp(1, 4)`
    // would move both sides together and could never fail (§18.1).
    assert_eq!(
        config::concurrency_for_cores(4),
        1,
        "the reporter's 4-core laptop keeps its present serial behaviour"
    );
    assert_eq!(config::concurrency_for_cores(1), 1, "never below one");
    assert_eq!(config::concurrency_for_cores(12), 3, "this 12-core host");
    assert_eq!(
        config::concurrency_for_cores(256),
        4,
        "and never above four"
    );
    assert_eq!(
        config::claim_search_concurrency(),
        config::concurrency_for_cores(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        ),
        "the live value must come from that derivation, not a constant"
    );
}

#[tokio::test]
async fn a_verify_only_surface_keeps_its_own_action_name() {
    let inference: Arc<dyn crate::traits::InferenceProvider> =
        Arc::new(GateMock { support: false });
    let profile = GateSurface::Refinement.profile();
    assert!(!profile.retry, "refinement is the verify-only surface");
    let outcome = gate_answer(
        &inference,
        "Tell me about the shop.",
        longform_draft(&profile),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("annotated_no_retry"),
        "the verify-only surface's action is unchanged by the tombstone"
    );
}

/// The other half of §18.3: when H1 did not run, the keys are PRESENT
/// and say so. A missing key and a "no verdict" key must not read the
/// same — that ambiguity is what made the flip soak unreadable.
#[tokio::test]
async fn native_telemetry_states_absence_rather_than_omitting_it() {
    use sovereign_contracts::types::GroundingDecision;
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(GateMock { support: true });
    let profile = GateSurface::Refinement.profile();
    let outcome = gate_answer(
        &inference,
        "Where is the shop?",
        "The shop is on Harbour Row.".to_string(),
        &refinement_evidence(), // no H1 verdict — the state of this host
        &CompletionRequest::default(),
        &profile,
    )
    .await;
    let meta = outcome.meta.as_object().expect("gate meta is an object");
    assert!(
        meta.contains_key("native_answerability") && meta.contains_key("native_decision"),
        "both keys must be PRESENT when H1 did not run: {:?}",
        meta.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        meta.get("native_answerability").and_then(|v| v.as_str()),
        Some(NATIVE_VERDICT_NOT_COMPUTED)
    );
    assert_eq!(
        meta.get("native_decision").and_then(|v| v.as_str()),
        Some(NATIVE_VERDICT_NOT_COMPUTED)
    );
    // …and the sentinel can never be confused with something H1 said.
    for decision in [
        GroundingDecision::Answer,
        GroundingDecision::Hedge,
        GroundingDecision::Abstain,
    ] {
        assert_ne!(
            native_verdict(decision, 0.5).to_gate_action(),
            NATIVE_VERDICT_NOT_COMPUTED,
            "the absence sentinel must not collide with a real decision"
        );
    }
}

/// Drain every frame the gate pushed onto a progress channel.
async fn drain_frames(
    mut rx: tokio::sync::mpsc::Receiver<crate::types::NarrationPhase>,
) -> Vec<crate::types::NarrationPhase> {
    let mut frames = Vec::new();
    while let Some(f) = rx.recv().await {
        frames.push(f);
    }
    frames
}

/// Short path, supported claim: the progress channel carries the
/// desktop verification panel's contract — claim list opens, the
/// verdict stamps, the completion frame closes the pass.
#[tokio::test]
async fn short_path_progress_frames_on_release() {
    use crate::types::NarrationPhase;
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(GateMock { support: true });
    let profile = GateSurface::Refinement.profile();
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let outcome = gate_answer_with_progress(
        &inference,
        "Where is the shop?",
        "The shop is on Harbour Row.".to_string(),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
        Some(&tx),
    )
    .await;
    drop(tx);
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("released")
    );
    let frames = drain_frames(rx).await;
    assert!(
        matches!(
            &frames[0],
            NarrationPhase::ClaimCheckStart { claims, recheck: false } if claims.len() == 1
        ),
        "first frame should open the one-claim check: {frames:?}"
    );
    assert!(matches!(
        frames[1],
        NarrationPhase::ClaimVerdict {
            index: 0,
            supported: true
        }
    ));
    assert!(matches!(
        frames[2],
        NarrationPhase::ClaimCheckComplete {
            confirmed: 1,
            flagged: 0
        }
    ));
    assert_eq!(frames.len(), 3);
}

/// Short path, verify-only failure: verdict stamps unsupported and
/// the completion frame reports the flagged claim.
#[tokio::test]
async fn short_path_progress_frames_on_abstention() {
    use crate::types::NarrationPhase;
    let inference: Arc<dyn crate::traits::InferenceProvider> =
        Arc::new(GateMock { support: false });
    let profile = GateSurface::Refinement.profile();
    assert!(!profile.retry);
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let outcome = gate_answer_with_progress(
        &inference,
        "Where is the shop?",
        "The shop is on Crescent Lane.".to_string(),
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
        Some(&tx),
    )
    .await;
    drop(tx);
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("abstained_no_retry")
    );
    let frames = drain_frames(rx).await;
    assert!(matches!(
        &frames[0],
        NarrationPhase::ClaimCheckStart { recheck: false, .. }
    ));
    assert!(matches!(
        frames[1],
        NarrationPhase::ClaimVerdict {
            index: 0,
            supported: false
        }
    ));
    assert!(matches!(
        frames[2],
        NarrationPhase::ClaimCheckComplete {
            confirmed: 0,
            flagged: 1
        }
    ));
}

/// Longform path: the audit opens with the extracted claim LIST,
/// stamps every claim in order, and closes with the totals — the
/// full counter-card Check-station sequence.
#[tokio::test]
async fn longform_progress_frames_stamp_each_claim() {
    use crate::types::NarrationPhase;
    let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(GateMock { support: true });
    let profile = GateSurface::Refinement.profile();
    // Force the per-claim ladder regardless of the profile's pivot
    // (and of any SOVEREIGN_LONGFORM_CHARS ambient override — the
    // draft is longer than both).
    let pivot = std::env::var("SOVEREIGN_LONGFORM_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(profile.longform_chars)
        .max(profile.longform_chars);
    let draft = "The shop sits on Harbour Row, by the quay. ".repeat(pivot / 40 + 2);
    assert!(draft.chars().count() > pivot);
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let outcome = gate_answer_with_progress(
        &inference,
        "Tell me about the shop.",
        draft,
        &refinement_evidence(),
        &CompletionRequest::default(),
        &profile,
        Some(&tx),
    )
    .await;
    drop(tx);
    assert_eq!(
        outcome.meta.get("action").and_then(|a| a.as_str()),
        Some("released")
    );
    let frames = drain_frames(rx).await;
    // Claim list (two mock claims), then one verdict per claim in
    // index order, then the completion totals.
    assert!(
        matches!(
            &frames[0],
            NarrationPhase::ClaimCheckStart { claims, recheck: false } if claims.len() == 2
        ),
        "expected two-claim list first: {frames:?}"
    );
    assert!(matches!(
        frames[1],
        NarrationPhase::ClaimVerdict {
            index: 0,
            supported: true
        }
    ));
    assert!(matches!(
        frames[2],
        NarrationPhase::ClaimVerdict {
            index: 1,
            supported: true
        }
    ));
    assert!(matches!(
        frames[3],
        NarrationPhase::ClaimCheckComplete {
            confirmed: 2,
            flagged: 0
        }
    ));
}

fn fc(claim: &str, evidence: &[&str]) -> FailedClaim {
    FailedClaim {
        claim: claim.to_string(),
        evidence: evidence.iter().map(|s| s.to_string()).collect(),
    }
}

/// covers: GR-2
#[test]
fn rewrite_note_includes_corrective_passages() {
    let note = rewrite_system_note(&[fc(
        "Verloc instructs The Professor to bomb the Observatory.",
        &["Mr Vladimir tells Verloc the attack must be against the first meridian."],
    )]);
    assert!(note.contains("Verloc instructs The Professor"));
    assert!(
        note.contains("first meridian"),
        "corrective passage must reach the rewrite prompt"
    );
    assert!(note.contains("REPLACE"));
}

#[test]
fn rewrite_note_marks_claims_with_no_corpus_support() {
    let note = rewrite_system_note(&[fc("Mrs Veronica Verloc shoots her husband.", &[])]);
    assert!(note.contains("no corpus passage states this"));
}

#[test]
fn rewrite_note_caps_evidence_per_claim_and_length() {
    let long = "x".repeat(2_000);
    let note = rewrite_system_note(&[fc("c", &[&long, &long, &long])]);
    // 2 passages max, 700 chars each — the note stays prompt-sized.
    assert!(note.matches("  | ").count() <= 2);
    assert!(note.len() < 2_200);
}

#[test]
fn rewrite_note_commits_answer_shape_rules() {
    let note = rewrite_system_note(&[fc("c", &[])]);
    assert!(note.contains("Do not open with what the sources lack"));
    // The rewrite must not mint new claims about the sources (observed
    // 2026-07-01: a rewrite replaced one misattribution with "the text
    // cites Woolf's work" — a fresh unsupported claim ABOUT the text).
    assert!(note.contains("Never add a NEW statement about what the sources say"));
}

#[test]
fn verification_note_dedupes_caps_and_stays_unquoted() {
    let long = "x".repeat(200);
    let claims = vec![
        "Paul Samuelson admitted defeat around 1963".to_string(),
        "\"Paul Samuelson admitted defeat around 1963\"".to_string(), // dup modulo quotes
        "[unverified excerpt: ships cannot pay tolls at sea]".to_string(),
        long.clone(),
        String::new(),
    ];
    let note = verification_note(&claims);
    // Deduped: the claim appears once, as a plain list item.
    assert_eq!(note.matches("Samuelson").count(), 1);
    assert!(note.contains("- Paul Samuelson admitted defeat around 1963"));
    // The app's own wrapper is unwrapped to its content.
    assert!(note.contains("- ships cannot pay tolls at sea"));
    assert!(!note.contains("unverified excerpt:"));
    // UNQUOTED by design: a curly-quoted item reads as a quotation claim to
    // the post-synthesis quote guardrail, which demotes non-verbatim spans
    // to "[unverified excerpt: …]" — mangling the note (probed 2026-07-01).
    assert!(!note.contains('“') && !note.contains('”'));
    // Long item capped with an ellipsis; empty item dropped.
    assert!(note.contains(&format!("{}…", "x".repeat(160))));
    // Plain language — never judge vocabulary.
    assert!(!note.to_lowercase().contains("fabricated"));
}
