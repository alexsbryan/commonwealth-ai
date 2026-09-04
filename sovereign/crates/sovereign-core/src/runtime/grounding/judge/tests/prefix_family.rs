//! Prefix-family and wire-boundary tests: what bytes the gate actually
//! puts on the wire, and where it declares the shared prefix.
//!
//! Paired with `claim_scan`, which asserts what the judge's replies are
//! allowed to become. Split out of a single 1,162-line `tests.rs` so the
//! module stays inside ARCH §3.1's 800-line line.

use super::super::*;
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
    const SRC: &str = include_str!("../../judge.rs");
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
