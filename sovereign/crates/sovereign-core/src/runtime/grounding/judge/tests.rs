#[cfg(test)]
use super::*;
use crate::error::Result;
use crate::types::{CompletionResponse, Depth, ProviderCapabilities};
use futures::Stream;
use std::pin::Pin;
use std::sync::Mutex;

/// Records every `CompletionRequest` the gate issues and answers with a
/// constant. Prefix-family membership is a property of the REQUEST — its
/// system message and its prompt bytes — so these tests assert at the wire
/// boundary and need no model.
#[derive(Default)]
struct CaptureProvider(Mutex<Vec<CompletionRequest>>);

#[async_trait::async_trait]
impl InferenceProvider for CaptureProvider {
    async fn complete(&self, r: &CompletionRequest) -> Result<CompletionResponse> {
        self.0.lock().unwrap().push(r.clone());
        Ok(CompletionResponse {
            // Parses as NONE for the scan and as an unusable forced-choice
            // reply for the judges, which is fine: these tests read the
            // REQUESTS, never the verdicts.
            text: "NONE".into(),
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "capture".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }
    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        unimplemented!("no stream in prefix-family tests")
    }
    async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
        unimplemented!("no embed in prefix-family tests")
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 32_768,
            supports_structured_output: false,
            relative_speed: Speed::Slow,
            relative_reasoning: Depth::Deep,
        }
    }
}

/// **§5.2 — one renderer owns the family, enforced at compile time.**
///
/// The boundary got two deciders once already (a `format!` join and an
/// arithmetic re-derivation of the same byte count, kept in step by hand),
/// and a third lived in this very test module. `EvidenceFamily` collapsed
/// them; this is what stops a fourth. In production code the family's
/// literals may appear only in their `const` definitions and inside the
/// renderer — a second construction site fails here with a file:line
/// rather than as a silent cache miss weeks later (ARCH §10.6).
///
/// Same mechanism as `call_census`'s funnel guard: `include_str!` is
/// resolved by the compiler relative to THIS file, so it cannot go stale
/// against a moved module or pass vacuously from another directory.
#[test]
fn one_renderer_owns_the_family() {
    const SRC: &str = include_str!("../judge.rs");
    // Production code only: the test module legitimately names the
    // literals to assert against them.
    let prod = SRC.split("\n#[cfg(test)]").next().unwrap_or(SRC);
    let mut offenders: Vec<String> = Vec::new();
    let mut in_renderer = false;
    for (i, line) in prod.lines().enumerate() {
        if line.starts_with("impl EvidenceFamily {") {
            in_renderer = true;
        } else if in_renderer && line == "}" {
            in_renderer = false;
        }
        let l = line.trim_start();
        if l.starts_with("//") || l.starts_with("///") {
            continue;
        }
        // Only the FAMILY's literals are policed here. The exported
        // calibration surface (`CHUNK_JUDGE_SYSTEM`,
        // `CHUNK_JUDGE_PASSAGE_CHARS`) is deliberately referenced from two
        // crates — that sharing IS the fix — so it is guarded by the
        // single-render check below instead.
        let is_definition =
            l.starts_with("const PASSAGES_SCAFFOLD") || l.starts_with("const PASSAGE_SEP");
        if in_renderer || is_definition {
            continue;
        }
        if line.contains("PASSAGES_SCAFFOLD") || line.contains("PASSAGE_SEP") {
            offenders.push(format!("judge.rs:{}: {}", i + 1, line.trim()));
        }
    }
    assert!(
        offenders.is_empty(),
        "the evidence family is rendered outside `impl EvidenceFamily` — that is how \
             the boundary got two deciders the first time. Move it into the renderer:\n{}",
        offenders.join("\n")
    );

    // The calibrated chunk-judge register has the same one-renderer rule,
    // for a sharper reason: its second copy lived in ANOTHER CRATE (the
    // bench critic) and kept tau's transfer argument alive by hand. Its
    // opening literal may appear only inside `chunk_judge_prompt`.
    let renders: Vec<usize> = prod
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("\"PASSAGE:"))
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(
        renders.len(),
        1,
        "the calibrated per-passage judge prompt is rendered in {} places (lines {:?}); \
             it must be rendered only by `chunk_judge_prompt`, which the bench critic \
             imports — a second copy is how the byte-identity this module's header \
             claims becomes a comment describing a dead identity",
        renders.len(),
        renders
    );
}

/// How far past the old 1,500-char cap the fixture's long chunk reaches.
/// A fixture at 935 chars — which is what land A shipped — cannot tell a
/// re-introduced `.take(1_500)` from a correct renderer, so the guard
/// built on it was watched to fail on `take(400)` and would have sat
/// green through the real regression.
const LONG_CHUNK_TAIL: usize = 1_800;

/// Evidence whose first leaf chunk is deliberately LONGER than the cap
/// land B removed, with multi-byte characters throughout — the two ways a
/// renderer silently diverges (a re-introduced cut, a byte index landing
/// mid-char).
fn family_evidence() -> Vec<String> {
    vec![
        format!(
            "Ada Lovelace — première note «G» — {}",
            "é".repeat(LONG_CHUNK_TAIL)
        ),
        "The Analytical Engine was designed by Charles Babbage.".to_string(),
        "Menabrea's memoir was translated in 1843.".to_string(),
    ]
}

/// **§5.1 — the family contract, asserted at the wire boundary.**
///
/// Prefix-cache membership is a property of the RENDERED request, so this
/// checks the captured `CompletionRequest`s rather than the strings: byte
/// identity of the shared window across a factual judge (extras only) and
/// a thematic judge (summaries + extras), a declared boundary that is a
/// real char boundary, and suffixes that actually diverge past it.
///
/// The system-message assertion is the one that would have been missed:
/// the engine keys the family on the first 48 tokens of the rendered
/// prompt, **system message first**, so equal user-prompt prefixes are NOT
/// family membership on their own. Land B unifies the judges' system
/// message with the scan's; until then this pins that the judges at least
/// agree with each other.
///
/// Land C extends this to the scan.
#[tokio::test]
async fn the_gate_shares_one_prefix_family() {
    let cap = Arc::new(CaptureProvider::default());
    let inf: Arc<dyn InferenceProvider> = cap.clone();
    let posture = ShardingPrivacy::LocalOnly;
    let leaves = family_evidence();
    let summary = "SUMMARY: early computing pioneers and their attributions.".to_string();
    let extra = "A claim-conditioned hit fetched for this claim only.".to_string();

    // Factual: leaf window + a claim-conditioned extra appended after it.
    let mut factual = leaves.clone();
    factual.push(extra.clone());
    claim_violation_joint(
        &inf,
        "Lovelace wrote the first algorithm.",
        &factual,
        factual.len(),
        leaves.len(),
        posture,
    )
    .await;
    // Thematic: the same leaf window, then summaries, then an extra.
    let mut thematic = leaves.clone();
    thematic.push(summary);
    thematic.push(extra);
    claim_violation_joint(
        &inf,
        "The memoir shaped how the engine was understood.",
        &thematic,
        thematic.len(),
        leaves.len(),
        posture,
    )
    .await;

    // A DIFFERENT MECHANISM on the same family. Without this the
    // system-message assertion below cannot fail: both `claim_violation_joint`
    // calls route through `forced_choice_ab` as `PerClaimJudge`, so a fork
    // that varied the system turn BY MECHANISM left this test green
    // (checked twice — before and after land B). `claim_chunk_support`
    // reaches the same function as `ChunkJudge`, which is the input that
    // makes the assertion real. Its prompt is a single passage carrying no
    // family boundary, so it is excluded from the prefix loop and checked
    // only for the system turn.
    claim_chunk_support(&inf, &leaves[1], "Babbage designed it.", posture).await;

    let all = cap.0.lock().unwrap();
    assert_eq!(all.len(), 3, "two claim checks and one chunk check");
    assert_eq!(
        all[2].system_message,
        Some(CHUNK_JUDGE_SYSTEM.to_string()),
        "the single-passage judge left the calibrated forced-choice system turn. \
             That string is shared with the bench critic and is what tau=0.9 was \
             calibrated on — moving one side of it silently voids the transfer \
             argument in this module's header. Land C moves BOTH sides together."
    );
    let reqs = &all[..2];
    let m = reqs[0]
        .stable_prefix_len
        .expect("a non-empty leaf window must declare a boundary");
    for (i, r) in reqs.iter().enumerate() {
        assert_eq!(
            r.stable_prefix_len,
            Some(m),
            "request {i} declared a different boundary — siblings must agree"
        );
        assert!(
            r.prompt.is_char_boundary(m),
            "request {i}: boundary off a char boundary (multi-byte evidence)"
        );
        assert!(m < r.prompt.len(), "request {i}: boundary is not interior");
        assert_eq!(
            r.prompt.as_bytes()[..m],
            reqs[0].prompt.as_bytes()[..m],
            "request {i}: the shared window is not byte-identical"
        );
        assert_eq!(
            r.system_message, reqs[0].system_message,
            "request {i}: differing system messages are DIFFERENT prefix families, \
                 whatever the user prompt looks like"
        );
    }
    // The watched-to-fail arm: prompts that never diverge would make every
    // assertion above vacuous.
    assert_ne!(
        reqs[0].prompt, reqs[1].prompt,
        "the two claims must produce different suffixes or this test proves nothing"
    );
    // The long chunk survived intact inside the window — a re-introduced
    // truncation shows up here rather than as a silent cache miss.
    let head = &reqs[0].prompt[..m];
    assert_eq!(
        head.matches('é').count(),
        LONG_CHUNK_TAIL,
        "the leaf chunk was CUT inside the family window. Land B removed the \
             1,500-char cap precisely because a cut chunk manufactures absences — \
             a judge cannot honestly be asked 'do the passages support this' \
             against evidence with the support snipped off."
    );
    drop(all);

    // ── THE SCAN: not in the family yet, and that is the point ──
    //
    // The system-message assertion above is VACUOUS on judges alone —
    // both come from `forced_choice_ab`, which carries one constant, so
    // no perturbation of a single call site can make them differ (checked:
    // varying it by mechanism leaves this test green, because both judge
    // calls are the same mechanism). A check with no failing input you can
    // name is not a check (ARCH §18.1). The scan is that input: it carries
    // a DIFFERENT system message today — one that interpolates
    // `max_items`, so its family is not even stable against a budget
    // change — and by the engine's keying rule that alone puts it in a
    // different prefix family no matter how its prompt is laid out.
    //
    // So this records the pre-B state as an assertion rather than as a
    // comment. Land B unifies the system messages and land C moves the
    // scan onto `scan_prompt`; BOTH of these assertions then invert, and
    // this block becomes the positive check that the scan shares the
    // judges' family. A test that failed to notice the unification is a
    // test that would have let C ship without its own win.
    scan_unsupported_specifics(
        &inf,
        "Who wrote it?",
        "Lovelace wrote it.",
        &leaves,
        &[],
        4,
        posture,
    )
    .await;
    let all = cap.0.lock().unwrap();
    assert_eq!(all.len(), 4, "the scan added its own request");
    let scan = &all[3];
    // FLIPPED (order audit-economy D3 candidate A): the scan is now a
    // member of the judges' family — the assertions this block carried
    // before said exactly how to invert them when that landed.
    assert_eq!(
        scan.system_message, all[0].system_message,
        "the scan left the judges' system turn — by the engine's keying rule that \
             alone evicts it from the family, whatever its prompt looks like"
    );
    assert!(
        scan.prompt.starts_with(PASSAGES_SCAFFOLD),
        "the scan no longer opens with the judges' scaffold — family broken"
    );
    assert_eq!(
        scan.stable_prefix_len,
        Some(m),
        "the scan must declare the SAME family boundary as its sibling judges"
    );
    assert_eq!(
        scan.prompt.as_bytes()[..m],
        all[0].prompt.as_bytes()[..m],
        "the scan's window is not byte-identical to the judges' — it would silently \
             full-prefill instead of restoring the pin"
    );
}

/// The specifics scan's prefix-cache declaration (D1a). Two scans of the
/// **The replay seam is the register, byte for byte.** The judge-replay
/// harness scores recorded evidence through
/// **The family split is a CACHE boundary, never a prompt change.**
///
/// `claim_violation_joint`'s `n_stable` decides how much of the window
/// `EvidenceFamily` renders as the shared prefix and how much each call
/// appends. The whole safety argument for CHANGING a caller's `n_stable`
/// is that the two halves render to the same bytes — same bytes to the
/// judge means the same logits and therefore the same verdict, so moving
/// the split can only cost or save prefill.
///
/// This pins it directly: every split of one window, 0..=n, must issue a
/// byte-identical prompt, while the DECLARED boundary tracks the split.
/// The deep-research audit passed `n_stable = 0` until 2026-08-24 and so
/// never declared a prefix at all — every sibling claim re-prefilled the
/// entire evidence window. The fix that declares it is only safe because
/// of this property, which until now was argued and not asserted.
#[tokio::test]
async fn the_family_split_moves_the_boundary_not_the_prompt() {
    let window = family_evidence();
    let claim = "Lovelace wrote the first algorithm.";
    let mut rendered: Vec<String> = Vec::new();
    let mut boundaries: Vec<Option<usize>> = Vec::new();

    for split in 0..=window.len() {
        let cap = Arc::new(CaptureProvider::default());
        let inf: Arc<dyn InferenceProvider> = cap.clone();
        claim_violation_joint(
            &inf,
            claim,
            &window,
            window.len(),
            split,
            ShardingPrivacy::LocalOnly,
        )
        .await;
        let all = cap.0.lock().unwrap();
        assert_eq!(all.len(), 1, "one judge call per split");
        rendered.push(all[0].prompt.clone());
        boundaries.push(all[0].stable_prefix_len);
    }

    for (split, prompt) in rendered.iter().enumerate() {
        assert_eq!(
            prompt, &rendered[0],
            "split {split} rendered DIFFERENT prompt bytes than split 0 — \
                 moving the family boundary would change the verdict, and every \
                 caller's n_stable would be a judge change, not a cache hint"
        );
    }
    assert_eq!(
        boundaries[0], None,
        "split 0 declares NO stable window — absence reported, never a zero-length claim"
    );
    assert!(
        boundaries[window.len()].is_some_and(|b| b > 0),
        "declaring the whole window stable must yield a real byte boundary"
    );
    assert!(
        boundaries[window.len()] > boundaries[1],
        "a larger stable half must declare a larger prefix — the boundary \
             tracks the split even though the bytes do not"
    );
}

/// `replay_render_claim_prompt` + `replay_claim_violation_joint`
/// (grounding/mod.rs wrappers); an offline verdict transfers to the
/// production gate only if those wrappers send the same bytes the gate
/// sends. Asserted at the wire boundary: the rendered (prompt, boundary)
/// must equal what `claim_violation_joint` actually issues for the same
/// (shared ++ appended, n_stable) inputs. A drift here would make every
/// replay curve an artifact of a second renderer — the exact failure the
/// harness exists to rule out (ARCH §18.4: validate the instrument).
#[tokio::test]
async fn replay_render_matches_the_joint_register() {
    let cap = Arc::new(CaptureProvider::default());
    let inf: Arc<dyn InferenceProvider> = cap.clone();
    let shared = family_evidence();
    let appended = vec!["A claim-conditioned hit fetched for this claim only.".to_string()];
    let claim = "Lovelace wrote the first algorithm.";

    let (rendered, boundary) = replay_render_claim_prompt(&shared, &appended, claim);

    let mut chunks = shared.clone();
    chunks.extend(appended.clone());
    claim_violation_joint(
        &inf,
        claim,
        &chunks,
        chunks.len(),
        shared.len(),
        ShardingPrivacy::LocalOnly,
    )
    .await;

    let all = cap.0.lock().unwrap();
    assert_eq!(all.len(), 1, "one judge call");
    assert_eq!(
        all[0].prompt, rendered,
        "replay render diverged from the register's own bytes"
    );
    assert_eq!(
        all[0].stable_prefix_len, boundary,
        "replay boundary diverged from the register's declared prefix"
    );
    assert_eq!(
        all[0].system_message.as_deref(),
        Some(CHUNK_JUDGE_SYSTEM),
        "the replay fingerprint accessor must report the register's real system turn"
    );
}

/// **The batched register is a MEMBER of the judges' prefix family** —
/// the whole point of its 2026-08-14 reshape (order `audit-economy`,
/// D0 finding: the per-claim evidence prefill is restore-amortized, so a
/// batched call outside the family full-prefills ~9K tokens for nothing).
/// Asserted at the wire boundary like `the_gate_shares_one_prefix_family`:
/// same system turn, same declared boundary, byte-identical window,
/// diverging suffix. The engine keys families on the first 48 rendered
/// tokens (system first), so the system-message assertion is load-bearing,
/// not cosmetic — the pre-reshape register fails exactly there.
#[tokio::test]
async fn batched_register_joins_the_judges_prefix_family() {
    let cap = Arc::new(CaptureProvider::default());
    let inf: Arc<dyn InferenceProvider> = cap.clone();
    let posture = ShardingPrivacy::LocalOnly;
    let leaves = family_evidence();

    claim_violation_joint(
        &inf,
        "Lovelace wrote the first algorithm.",
        &leaves,
        leaves.len(),
        leaves.len(),
        posture,
    )
    .await;
    let claims = vec![
        "Lovelace wrote the first algorithm.".to_string(),
        "Babbage designed the Analytical Engine.".to_string(),
    ];
    claims_support_batched(&inf, &claims, &leaves, leaves.len(), posture).await;

    let all = cap.0.lock().unwrap();
    assert_eq!(all.len(), 2, "one per-claim judge and one batched call");
    let (judge, batched) = (&all[0], &all[1]);
    assert_eq!(
        batched.system_message, judge.system_message,
        "differing system messages are DIFFERENT prefix families, whatever \
             the user prompt looks like — this is the byte the old batched \
             register lost the family on"
    );
    let m = judge
        .stable_prefix_len
        .expect("a non-empty leaf window must declare a boundary");
    assert_eq!(
        batched.stable_prefix_len,
        Some(m),
        "the batched call must declare the SAME family boundary as its siblings"
    );
    assert_eq!(
        batched.prompt.as_bytes()[..m],
        judge.prompt.as_bytes()[..m],
        "the batched window is not byte-identical to the judges' — it would \
             silently full-prefill instead of restoring the pin"
    );
    // Watched-to-fail arm: identical prompts would make the byte-identity
    // assertions vacuous.
    assert_ne!(
        batched.prompt, judge.prompt,
        "the batched suffix must diverge from the per-claim suffix or this \
             test proves nothing"
    );
    // The batched pass answers in text lines, not a single forced token.
    assert!(
        batched.max_tokens.unwrap_or(0) > 1,
        "the batched register generates one verdict line per claim"
    );
}

/// The batched replay seam sends the register's own bytes — same contract
/// as `replay_render_matches_the_joint_register`, for the batched shape.
#[tokio::test]
async fn replay_render_matches_the_batched_register() {
    let cap = Arc::new(CaptureProvider::default());
    let inf: Arc<dyn InferenceProvider> = cap.clone();
    let shared = family_evidence();
    let claims = vec![
        "Lovelace wrote the first algorithm.".to_string(),
        "The memoir was translated in 1843.".to_string(),
        "Babbage designed the Analytical Engine.".to_string(),
    ];

    let (rendered, boundary) = replay_render_batched_claims_prompt(&shared, &claims);
    claims_support_batched(
        &inf,
        &claims,
        &shared,
        shared.len(),
        ShardingPrivacy::LocalOnly,
    )
    .await;

    let all = cap.0.lock().unwrap();
    assert_eq!(all.len(), 1, "one batched call");
    assert_eq!(
        all[0].prompt, rendered,
        "batched replay render diverged from the register's own bytes"
    );
    assert_eq!(
        all[0].stable_prefix_len, boundary,
        "batched replay boundary diverged from the register's declared prefix"
    );
}

/// SAME turn — the audit and the re-audit — differ only in the answer, so
/// the declared prefix must be byte-identical between them or the engine
/// has nothing to restore. This is the property the whole change exists
/// for, and it is a property of the PROMPT LAYOUT, so it is pinned here
/// rather than inferred from a latency number.
#[tokio::test]
async fn the_specifics_scan_declares_a_prefix_its_sibling_can_reuse() {
    let cap = Arc::new(CaptureProvider::default());
    let inf: Arc<dyn InferenceProvider> = cap.clone();
    let evidence = vec![
        "Ada Lovelace wrote the first algorithm intended for a machine.".to_string(),
        "The Analytical Engine was designed by Charles Babbage.".to_string(),
    ];
    let posture = ShardingPrivacy::LocalOnly;
    // The audit pass, then the re-audit pass over a repaired answer.
    for answer in [
        "Lovelace wrote the first algorithm. Babbage built it in 1837.",
        "Lovelace wrote the first algorithm.",
    ] {
        scan_unsupported_specifics(&inf, "Who wrote it?", answer, &evidence, &[], 4, posture)
            .await
            .expect("the capture stub always answers");
    }
    let reqs = cap.0.lock().unwrap();
    assert_eq!(reqs.len(), 2, "one call per scan");
    let n = reqs[0]
        .stable_prefix_len
        .expect("the scan must declare a prefix — this is the D1a change");
    assert_eq!(
        reqs[1].stable_prefix_len,
        Some(n),
        "both scans of a turn must declare the SAME boundary or the pin cannot be reused"
    );
    assert_eq!(
        reqs[0].prompt.as_bytes()[..n],
        reqs[1].prompt.as_bytes()[..n],
        "the declared prefix must be byte-identical across siblings"
    );
    assert!(
        reqs[0].prompt.is_char_boundary(n),
        "a declaration off a char boundary is rejected by the engine"
    );
    // It is a real prefix of a longer prompt, and the part after it is
    // what actually varies — i.e. the answer sits on the far side.
    assert!(n < reqs[0].prompt.len() && n < reqs[1].prompt.len());
    assert_ne!(
        reqs[0].prompt, reqs[1].prompt,
        "the two scans do differ — otherwise this test proves nothing"
    );
    // And the layout is still the one the judge is calibrated on: the
    // evidence is inside the declared prefix, the answer is outside it.
    let head = &reqs[0].prompt[..n];
    assert!(
        head.contains("Analytical Engine"),
        "evidence inside the pin"
    );
    assert!(!head.contains("Babbage built it in 1837"), "answer outside");
}

#[test]
fn structural_specificity_fires_on_numbers_and_quotes_only() {
    // Numbers and quotations are form-level specificity — factual
    // regardless of vocabulary. (Semantic class for everything else
    // is the embed classifier's job — see claim_class_classifier
    // tests; no vocabulary assertions here by design.)
    assert!(claim_has_structural_specificity(
        "The text discusses the 1894 Greenwich bombing."
    ));
    assert!(claim_has_structural_specificity(
        "The section argues that \"esse est percipi\" grounds idealism."
    ));
    assert!(!claim_has_structural_specificity(
        "The text explores the theme of betrayal within the family."
    ));
    assert!(!claim_has_structural_specificity("Verloc runs a shop."));
}

#[test]
fn batched_verdicts_align_by_number_and_fallback_on_gaps() {
    // Clean case: all rows present, mixed A/B, tolerant separators.
    let v = parse_batched_verdicts("1: A\n2. B\n3) A", 3);
    assert_eq!(v, vec![Some(true), Some(false), Some(true)]);
    // Out-of-order lines still land on the right claim (numbering, not position).
    let v = parse_batched_verdicts("2: B\n1: A", 2);
    assert_eq!(v, vec![Some(true), Some(false)]);
    // A missing row stays None (caller re-verifies with the calibrated pass);
    // a bullet-prefixed / prose-wrapped line is tolerated.
    let v = parse_batched_verdicts("- 1: A\n3: B", 3);
    assert_eq!(v, vec![Some(true), None, Some(false)]);
    // Out-of-range index is ignored (no panic, no shifted verdict).
    let v = parse_batched_verdicts("1: A\n9: B", 2);
    assert_eq!(v, vec![Some(true), None]);
    // Ambiguous verdict token → None, not a coin-flip.
    let v = parse_batched_verdicts("1: maybe\n2: B", 2);
    assert_eq!(v, vec![None, Some(false)]);
}

/// The artifact gate is a WORD gate, not a substring gate.
///
/// Watched failing on a live desktop turn 2026-08-13: "Harry Frankfurt
/// designed cases intended to prove moral responsibility does not require
/// alternate possibilities" was vetoed as a fabricated in-world
/// attribution. The gate opened because "de-SIGNED" contains "signed", and
/// the bigram check then flagged "Harry Frankfurt" — a philosopher named
/// in four of the turn's own chunks — because the corpus writes the
/// surname alone. That single veto was the only thing between that turn
/// and a zero-failure turn.
///
/// Every string below is ordinary essay prose. Before the fix each one
/// opened a veto meant for claims about emails, letters and source files.
#[test]
fn artifact_gate_matches_whole_words_not_substrings() {
    let hay = "frankfurt cases are the primary compatibilist response.";
    // "designed" ⊃ "signed" — the live case.
    assert_eq!(
        absent_name_attribution("Harry Frankfurt designed cases about responsibility.", hay),
        None,
        "\"designed\" must not open the artifact gate via \"signed\""
    );
    // "present" / "represent" / "consent" / "absent" / "sentence" ⊃ "sent"
    for prose in [
        "Peter Strawson present arguments about reactive attitudes.",
        "Galen Strawson represent the basic-argument position.",
        "Susan Wolf absent from this particular debate entirely.",
        "Robert Kane sentence structures favour event-causal accounts.",
    ] {
        assert_eq!(
            absent_name_attribution(prose, hay),
            None,
            "ordinary prose must not open the artifact gate: {prose:?}"
        );
    }
    // "classical" ⊃ "class", "denotes" ⊃ "notes" — identifier sibling.
    assert_eq!(
        absent_identifier_attribution(
            "Classical compatibilism denotes the Hobbes-Hume position.",
            hay
        ),
        None,
        "\"classical\"/\"denotes\" must not open the identifier gate"
    );
    // ...and the gate still OPENS on the real thing it was built for.
    assert_eq!(
        absent_name_attribution(
            "Betty Alexander sent an email about the schedule.",
            "unrelated evidence with no such person"
        ),
        Some("Betty Alexander".to_string()),
        "a genuine in-world artifact attribution must still be vetoed"
    );
}

#[test]
fn name_sweep_skips_citation_labels_and_boilerplate() {
    // The persona-QA self-indictment class (2026-07-10): label fragments
    // and header bigrams flagged as fabricated names.
    assert_eq!(
        absent_name_attribution(
            "The passage discusses effects as documented [Source: Psilocybin Mushrooms — Effects]",
            "some unrelated evidence text"
        ),
        None
    );
    assert_eq!(
        absent_name_attribution(
            "From Retrieved Sources: the document describes the mechanism in a later section.",
            "some unrelated evidence text"
        ),
        None
    );
    // Heading bigrams and comma-separated name lists are not names
    // (overnight soak receipts).
    assert_eq!(
        absent_name_attribution(
            "**Energy Costs**: The document describes rate changes for households.",
            "unrelated evidence"
        ),
        None
    );
    assert_eq!(
        absent_name_attribution(
            "The letter was signed by Hamilton, Madison and Jay together.",
            "hamilton wrote often. madison replied. jay concurred."
        ),
        None
    );
    // Surname + capitalized pronoun is not a name ("Webber He
    // averaged…" — observed live).
    assert_eq!(
        absent_name_attribution(
            "The document states Webber He averaged 19.1 points per game.",
            "webber averaged 19.1 points"
        ),
        None
    );
    // Positive control: a genuine in-world attribution absent from
    // evidence still trips the veto.
    assert_eq!(
        absent_name_attribution(
            "The email was sent by Betty Alexander to the finance team.",
            "totally different evidence"
        ),
        Some("Betty Alexander".to_string())
    );
    // Unclosed bracket strips to end-of-line, not end-of-answer.
    assert_eq!(
        absent_name_attribution(
            "cited in [Source: Broken Label\nThe letter was written by Elowen Marsh yesterday.",
            "nothing relevant"
        ),
        Some("Elowen Marsh".to_string())
    );
}

#[test]
fn self_referential_declines_are_exempt() {
    // The two live-observed rejection shapes (persona-QA 2026-07-10).
    assert!(is_self_referential_decline(
        "The system does not have access to real-time earthquake or tsunami data for Japan."
    ));
    assert!(is_self_referential_decline(
            "As of 2026-07-10, there is no evidence that the assistant's capabilities include live seismic feeds."
        ));
    assert!(is_self_referential_decline(
        "The provided passages do not contain real-time viewership data."
    ));
    // Markdown-decorated variant (scan findings arrive with emphasis).
    assert!(is_self_referential_decline(
        "**The system does **not** have access to real-time earthquake data"
    ));
}

#[test]
fn world_claims_are_not_exempt() {
    assert!(!is_self_referential_decline(
        "Azelaic acid inhibits tyrosinase and has anti-inflammatory properties."
    ));
    assert!(!is_self_referential_decline(
        "Family Guy remains a consistent driver of engagement on Hulu."
    ));
    // System-subject but AFFIRMATIVE (not a decline) stays in jurisdiction.
    assert!(!is_self_referential_decline(
        "The system retrieves twelve chunks per query."
    ));
}

const ANSWER: &str = "Robinson attacked aggregate production functions and \
        neoclassical production theory more broadly, a task she showed to be \
        circular reasoning [Source: Joan Robinson]. The lighthouse also appears \
        as a title of James Joyce's novel.";

#[test]
fn quoted_answer_span_is_extracted() {
    // The observed live shape: the model wraps the span in quotes and
    // appends judgment chatter after an em-dash.
    let item = "\"and neoclassical production theory more broadly\" — The \
                    evidence does not mention this";
    assert_eq!(
        anchor_scan_item(item, ANSWER).as_deref(),
        Some("and neoclassical production theory more broadly")
    );
}

#[test]
fn dash_appended_commentary_is_cut() {
    let item = "a task she showed to be circular reasoning — not stated in the sources";
    assert_eq!(
        anchor_scan_item(item, ANSWER).as_deref(),
        Some("a task she showed to be circular reasoning")
    );
}

#[test]
fn ascii_hyphen_appended_commentary_is_cut() {
    // The shape the live judge actually emitted on the measured turn: a
    // plain " - ", which the em/en-dash list did not cover.
    let item = "a task she showed to be circular reasoning - the evidence does not say this";
    assert_eq!(
        anchor_scan_item(item, ANSWER).as_deref(),
        Some("a task she showed to be circular reasoning")
    );
}

#[test]
fn abstractive_finding_is_not_a_claim() {
    // REVERSED 2026-08-08. This case used to pass through unchanged, on
    // the reasoning that an abstractive finding still guides the
    // corrective search. It does — but the same value is ALSO recorded
    // as a `failed_once` holding and listed in the user's verification
    // note, and there it is the judge talking about the answer rather
    // than a claim the answer made. The search hint is not worth a false
    // holding; see `judge_commentary_never_becomes_a_claim` for the
    // transcript this was measured on.
    let item = "The answer claims there is no single item explicitly labeled";
    assert_eq!(anchor_scan_item(item, ANSWER), None);
}

#[test]
fn curly_quotes_are_handled() {
    let item = "“The lighthouse also appears as a title of James Joyce's novel” — misattributed";
    assert_eq!(
        anchor_scan_item(item, ANSWER).as_deref(),
        Some("The lighthouse also appears as a title of James Joyce's novel")
    );
}

#[test]
fn emphasis_markers_do_not_hide_an_answer_span() {
    // The judge drops the answer's `**bold**` when it re-quotes. Anchoring
    // must see through that, or a real span falls off the ladder.
    let ans = "Corwin Pellow was murdered by **Severin Quenholt**, the broker.";
    let item = "\"Corwin Pellow was murdered by Severin Quenholt\" - not in the evidence";
    assert_eq!(
        anchor_scan_item(item, ans).as_deref(),
        Some("Corwin Pellow was murdered by Severin Quenholt")
    );
}

#[test]
fn an_elided_quote_anchors_on_its_prefix() {
    let ans = "The killing took place at the inn on a pleasant evening in summer, \
                   where he sat with his usual glass and agreed with neighbors.";
    let item = "\"The killing took place at the inn on a pleasant evening in summer, \
                    where he sat with his usual glass...\" - This is fabricated.";
    assert_eq!(
        anchor_scan_item(item, ans).as_deref(),
        Some(
            "The killing took place at the inn on a pleasant evening in summer, \
                 where he sat with his usual glass"
        )
    );
}

#[test]
fn a_stitched_quote_is_not_salvaged_into_a_fragment() {
    // An INTERIOR ellipsis means the judge spliced two spans and appended
    // a verdict. Anchoring must reject it rather than reduce it to the
    // bare name in front — that name is not the claim.
    let ans = "Severin Quenholt was the broker. Corwin Pellow was the harbormaster.";
    let item = "\"Severin Quenholt... As harbormaster, his signature validated salvage \
                    lots.\" (Misattribution: the text identifies Corwin Pellow as harbormaster.)";
    assert_eq!(anchor_scan_item(item, ans), None);
}

#[test]
fn legitimate_em_dash_inside_a_present_item_is_kept() {
    // The whole item occurs in the answer -> no cut at its interior dash.
    let ans = "The rule — quiet hours after ten — is strict.";
    let item = "The rule — quiet hours after ten — is strict.";
    assert_eq!(
        anchor_scan_item(item, ans).as_deref(),
        Some("The rule — quiet hours after ten — is strict.")
    );
}

#[test]
fn quoted_spans_extraction_walks_pairs() {
    let spans = extract_quoted_spans(r#"cites "[Source: x]" for "the atomic idea" here"#);
    assert_eq!(spans, vec!["[Source: x]", "the atomic idea"]);
}

// ---- The judge-prose defect, replayed from the transcript that shipped it.
//
// Provenance and the byte-identity check: `testdata/README.md`.
// `saltgrass_compound_gv_shadow_20260808.transcripts.jsonl`, turn
// `compound-killer-and-lugger`. Three of that turn's five `failed_once`
// holdings were the specifics scan's own commentary, and the user read
// them — in the ledger AND in the appended verification note — as their
// answer's failed claims.

/// The draft body the specifics scan audited (released answer, minus the
/// verification note the gate appended afterwards).
const POLLUTED_ANSWER: &str = include_str!("../testdata/polluted_answer.md");
/// The scan's raw reply, one judge line per line.
const POLLUTED_SCAN_REPLY: &str = include_str!("../testdata/polluted_scan_items.txt");
/// The three prose rows exactly as the ledger recorded them.
const POLLUTED_HOLDINGS: &str = include_str!("../testdata/polluted_holdings.txt");

#[test]
fn judge_commentary_never_becomes_a_claim() {
    let items = scan_items_from_reply(POLLUTED_SCAN_REPLY, POLLUTED_ANSWER, 12);
    for prose in POLLUTED_HOLDINGS.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            !items.iter().any(|i| i == prose),
            "the ledger's judge-prose holding came back as a claim: {:?}\n\
                 items: {items:#?}",
            prose.chars().take(90).collect::<String>()
        );
    }
}

#[test]
fn every_scan_item_is_a_span_of_the_answer() {
    // The positive half of the contract: whatever survives must be
    // wording the ANSWER used, not wording the judge used. Compared
    // modulo emphasis markers, because the judge re-quotes
    // `**Severin Quenholt**` as `Severin Quenholt`.
    let strip = |s: &str| -> String {
        s.to_lowercase()
            .chars()
            .filter(|c| !matches!(c, '*' | '_' | '`'))
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let ans = strip(POLLUTED_ANSWER);
    for item in scan_items_from_reply(POLLUTED_SCAN_REPLY, POLLUTED_ANSWER, 12) {
        assert!(
            ans.contains(&strip(&item)),
            "scan item is not a span of the answer: {:?}",
            item.chars().take(90).collect::<String>()
        );
    }
}

#[test]
fn the_turns_real_claims_survive_the_filter() {
    // Guard against over-correcting into silence: the two spans the
    // answer genuinely asserted are still flagged.
    let items = scan_items_from_reply(POLLUTED_SCAN_REPLY, POLLUTED_ANSWER, 12);
    assert_eq!(items.len(), 2, "expected 2 answer spans, got {items:#?}");
    assert!(items
        .iter()
        .any(|i| i == "Corwin Pellow was murdered by Severin Quenholt"));
    assert!(items
        .iter()
        .any(|i| i.starts_with("The killing took place at *The Cold Lantern* inn")));
}

#[test]
fn unverified_excerpt_wrappers_unwrap_to_content() {
    let s = "It holds [unverified excerpt: As Samuelson (1954) noted, free-riding \
                 justifies provision] and more.";
    assert_eq!(
        unwrap_unverified_excerpts(s),
        "It holds As Samuelson (1954) noted, free-riding justifies provision and more."
    );
    // Unclosed wrapper survives verbatim (never destroy text).
    let broken = "tail [unverified excerpt: cut off";
    assert_eq!(unwrap_unverified_excerpts(broken), broken);
    // No wrapper → untouched.
    assert_eq!(unwrap_unverified_excerpts("plain"), "plain");
}

#[test]
fn in_world_attribution_with_absent_name_is_vetoed() {
    let hay = "ok, jeff, you requested that we be candid about enron. rosalee \
                   fleming forwarded this to kenneth lay."
        .to_string();
    // The measured ghost: cleared at vp=0.010 by the joint judge.
    assert_eq!(
        absent_name_attribution(
            "Betty Alexander sent an email to Jeff Skilling on July 7, 2000.",
            &hay
        ),
        Some("Betty Alexander".to_string())
    );
    // A present name passes to the judge.
    assert_eq!(
        absent_name_attribution("Rosalee Fleming forwarded the email to Kenneth Lay.", &hay),
        None
    );
    // No artifact noun → general-knowledge territory → never vetoed
    // (do not shackle the model).
    assert_eq!(
        absent_name_attribution(
            "Noam Cohen called Wikipedia the last best place on the Internet.",
            &hay
        ),
        None
    );
    // Acronyms/date fragments are not name bigrams.
    assert_eq!(
        absent_name_attribution("The email was escalated to HR VP leadership in July.", &hay),
        None
    );
}

#[test]
fn absent_identifier_attribution_is_vetoed() {
    let hay = "the step kind enum defines reason, tool, user, plan, act, and                    awaituserinfo. see planner.rs and cmd_design."
            .to_string();
    // gen75c ghosts: invented snake_case fn + invented file + invented variant.
    assert_eq!(
        absent_identifier_attribution("The material centers on the cmd_init function.", &hay),
        Some("cmd_init".to_string())
    );
    assert_eq!(
        absent_identifier_attribution("The file design_signals.rs defines the gaps.", &hay),
        Some("design_signals.rs".to_string())
    );
    assert_eq!(
        absent_identifier_attribution("The StepKind enum values include ReasonWithTools.", &hay),
        Some("ReasonWithTools".to_string())
    );
    // Present identifiers pass (case-insensitive), including real variants.
    assert_eq!(
        absent_identifier_attribution("The enum defines AwaitUserInfo as a variant.", &hay),
        None
    );
    assert_eq!(
        absent_identifier_attribution("The file planner.rs holds the logic.", &hay),
        None
    );
    // No artifact context → GK territory → untouched.
    assert_eq!(
        absent_identifier_attribution("React's useStateHook pattern is popular.", &hay),
        None
    );
}

#[test]
fn wrapped_scan_item_is_judged_on_content() {
    // A scan item echoing the app's own wrapper must reduce to the span
    // content so the note never lists a double-wrapped self-indictment.
    let answer = "The gate held [unverified excerpt: ships cannot pay tolls at sea] today.";
    let item = "[unverified excerpt: ships cannot pay tolls at sea]";
    assert_eq!(
        anchor_scan_item(item, answer).as_deref(),
        Some("ships cannot pay tolls at sea")
    );
}

/// The scalpel's two arms and — load-bearing — what it must NOT exempt.
/// The step-91 shape (2026-07-21 soak): decline headline + a POSITIVE
/// meta-rider about the passages, which the negation-requiring longform
/// predicate deliberately lets through, burned 16 per-passage checks +
/// a doomed retry. The conjunction (decline headline AND meta subject)
/// exempts it; a world-claim rider keeps its audit.
#[test]
fn decline_rider_exemption_scalpel() {
    let decline_answer = "I don't have reliable information on this. The \
             provided passages are Rust source code snippets from a \
             corpus-engine project.";
    // Arm 2: positive evidence-meta rider under a decline headline → exempt.
    assert!(decline_rider_exempt(
        decline_answer,
        "The provided passages are Rust source code snippets from a corpus-engine project."
    ));
    // World-claim rider under the same decline headline → NOT exempt
    // (subject is the world, must stay audited).
    assert!(!decline_rider_exempt(
        "I don't have reliable information on this. However, John Smith sent the memo.",
        "John Smith sent the memo on May 5."
    ));
    // No decline headline → a positive meta-shaped claim is NOT exempt
    // via arm 2 (the decline supplies the safety).
    assert!(!decline_rider_exempt(
        "The passages are Rust source code snippets.",
        "The passages are Rust source code snippets."
    ));
    // Arm 1: a negated self-referential decline claim is exempt
    // regardless of the answer's headline (longform-established shape).
    assert!(decline_rider_exempt(
        "Summary of what I found.",
        "The sources do not contain information about the lamp mechanism."
    ));
    // Markdown emphasis must not defeat the subject/negation matching.
    assert!(decline_rider_exempt(
        "I don't have reliable information on this.",
        "The **provided** passages are configuration files."
    ));
    // Pronoun-subject world claim under an answer that merely CONTAINS
    // a decline phrase ("does not contain") — the loose "it " prefix is
    // negation-guarded and must NOT satisfy the negation-free rider arm.
    assert!(!decline_rider_exempt(
        "The report does not contain the exact date, but John sent it in May.",
        "It was sent in May."
    ));
}

/// The declared stable prefix must be byte-identical across sibling
/// claim-check prompts — one with claim-conditioned extras appended,
/// one without — and land on a char boundary. This is the contract
/// the engine's directed pin relies on; if the prompt construction
/// and `stable_passages_prefix_len` drift apart, restores silently
/// degrade to full prefills (latency, not correctness — but the
/// whole point of the feature evaporates).
#[test]
fn stable_prefix_is_shared_across_sibling_prompts() {
    // This test used to build its OWN copy of the prompt to assert
    // against — a third renderer, kept in step by hand, which is the
    // drift `EvidenceFamily` exists to end. It now drives the real
    // renderer, so a layout change cannot pass by being made twice.
    let shared = vec![
        "alpha passage with some grounding text — ünïcode too".to_string(),
        "beta passage carrying different content".to_string(),
    ];
    let extras = vec!["claim-conditioned hit only one sibling has".to_string()];
    let family = EvidenceFamily::new(&shared);

    let (p_extras, n_extras) = family.claim_prompt(&extras, "claim one");
    let (p_plain, n_plain) = family.claim_prompt(&[], "another claim");
    let n = n_extras.expect("a non-empty window declares a boundary");
    assert_eq!(n_plain, Some(n), "siblings must declare the same boundary");
    assert!(p_extras.is_char_boundary(n) && p_plain.is_char_boundary(n));
    assert_eq!(
        &p_extras.as_bytes()[..n],
        &p_plain.as_bytes()[..n],
        "siblings must share the declared prefix byte-for-byte"
    );
    // The prompts genuinely diverge just past the boundary (separator
    // + extra vs block close — both open with '\n', so compare a small
    // window, not the single next byte).
    assert_ne!(
        &p_extras.as_bytes()[n..n + 5],
        &p_plain.as_bytes()[n..n + 5]
    );

    // Degenerate window: nothing stable to declare. Reported as absence
    // rather than as a zero-length boundary, and the prompt still renders
    // — with no leading separator before the first appended passage,
    // which an arithmetic boundary never had to get right.
    let empty = EvidenceFamily::new(&[]);
    assert_eq!(empty.prefix_len(), None);
    let (p_empty, n_empty) = empty.claim_prompt(&shared, "claim");
    assert_eq!(n_empty, None, "no window means no declaration");
    assert!(
        p_empty.starts_with(&format!("{PASSAGES_SCAFFOLD}alpha passage")),
        "an empty window must not emit a dangling separator: {:?}",
        &p_empty[..80.min(p_empty.len())]
    );
}
