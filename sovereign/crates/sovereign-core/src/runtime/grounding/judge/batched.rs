//! Extracted from judge.rs (2026-09-03, ARCH §3.1) — see the judge façade.
use super::*;
use crate::oicp::ShardingPrivacy;
use crate::runtime::grounding::call_census::gate_call;
use crate::runtime::grounding::config::dbg;
use crate::runtime::grounding::search::SealedEvidenceSearch;
use crate::slot_policy::Workload;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};
use sovereign_contracts::types::GateCallMechanism;
use std::sync::Arc;

/// Leading literal of every claim-check prompt. Split out so the stable-prefix
/// byte math below and the prompt construction cannot drift apart.
pub(crate) const PASSAGES_SCAFFOLD: &str = "PASSAGES (multiple, separated by ---):\n\"\"\"\n";

/// Separator between passages, everywhere. One literal, so the renderer's
/// bytes and its boundary arithmetic cannot disagree about it.
const PASSAGE_SEP: &str = "\n---\n";

/// The system turn of the forced-choice judge register.
///
/// # This is a calibration surface, not a string
///
/// τ = 0.9 is calibrated against the bench critic
/// (`sovereign-cli-llm/src/bench_cmd/live_runner.rs`), and the transfer
/// argument in this module's header — "prompts are byte-identical to the bench
/// critic, so the bench-calibrated threshold transfers" — is only true while
/// the two registers really are identical. It used to be true by *coincidence
/// maintained by hand*: the same literal typed into two crates. This constant
/// and [`chunk_judge_prompt`] make it true STRUCTURALLY; the critic imports
/// both, so the identity cannot be broken by editing one side (ARCH §10.6).
///
/// **Land C changes this**, deliberately and with the adversarial set as its
/// evidence — and because the critic now shares the constant, it moves with
/// production instead of being left behind holding the calibration.
pub const CHUNK_JUDGE_SYSTEM: &str = "You are a careful classifier. Answer with a single letter.";

/// The forced-choice per-passage judge prompt — **the register τ is calibrated
/// on**, rendered once for both the runtime gate and the bench critic.
///
/// `passage` is capped at [`CHUNK_JUDGE_PASSAGE_CHARS`] here rather than by the
/// caller, so the cap cannot drift between the two either.
pub fn chunk_judge_prompt(passage: &str, claim: &str) -> String {
    let passage: String = passage.chars().take(CHUNK_JUDGE_PASSAGE_CHARS).collect();
    format!(
        "PASSAGE:\n\"\"\"\n{passage}\n\"\"\"\n\n\
         CLAIM: {claim}\n\n\
         Does the passage state or clearly imply this claim? Paraphrase counts; \
         the passage merely mentioning the people or things involved, without \
         establishing the claimed connection between them, does NOT count.\n\n\
         Answer with exactly one letter — A = the passage supports the claim, \
         B = it does not."
    )
}

/// Per-passage cap of the calibrated chunk-judge register. Untouched by land B:
/// the truncation land B removed is the *joint* window's 1,500-char cap inside
/// [`EvidenceFamily`], a register the critic has no counterpart for.
pub const CHUNK_JUDGE_PASSAGE_CHARS: usize = 2_400;

/// **The one renderer of the gate's shared evidence block, and the one decider
/// of where it ends.**
///
/// # Why this type exists
///
/// The boundary had two implementations: the prompt's bytes came from a
/// `format!` join, and the declared `stable_prefix_len` came from a *separate*
/// arithmetic re-derivation of the same byte count — two implementations of one
/// layout, kept aligned only by a test (ARCH §10.6, the smell-table row "two
/// implementations of one threshold, formula, or key"). Here the boundary is
/// `self.prefix.len()`: not a formula that agrees with the join, but the length
/// of the very `String` the join starts from. There is no arithmetic left to
/// drift.
///
/// # Why it matters beyond tidiness
///
/// The engine's pinned-prefix cache keys a DECLARED family — every call that
/// passes `stable_prefix_len`, which is every call in this module — on the
/// CONTENT of the declared prefix, and restores only when the declaration
/// matches that entry exactly (`prefix_state::directed_key`, 2026-09-01; it
/// keyed on the first 48 rendered tokens until then, which made two turns over
/// one corpus collide and pin at their common prefix — issue #57). Byte
/// identity across sibling calls is therefore not a nicety — it is the
/// difference between restoring a ~5,500-token prefix in ~26 ms and
/// re-prefilling it for ~7.7 s (measured 2026-08-13,
/// `bench/chaos_monkey/results/gate_call_census_20260813.txt`). A mismatch
/// does not error and does not change a verdict; it silently full-prefills.
/// Byte identity is therefore asserted at the request boundary by
/// `the_gate_shares_one_prefix_family`, not argued in prose.
///
/// # Land A scope
///
/// This introduction is **byte-identical to the inline `format!` it replaces**,
/// which is what makes it exempt from the adversarial gate — and that identity
/// is proven by `evidence_family_reproduces_the_legacy_judge_prompt`, a golden
/// test carrying the legacy construction, not by this sentence.
pub(super) struct EvidenceFamily {
    /// `PASSAGES_SCAFFOLD` + the shared window, joined. The family prefix.
    prefix: String,
    /// Whether the window carried any passage. A window of zero passages still
    /// renders the scaffold, but declares nothing and takes no separator before
    /// the first appended passage — the case an arithmetic boundary got to
    /// ignore and a real `String` does not.
    non_empty: bool,
}

impl EvidenceFamily {
    /// Render the shared window once per audit pass.
    ///
    /// `window` is the evidence every sibling call in the pass sees, in
    /// retrieval order. Callers append their own passages after it; nothing
    /// they append can move the boundary.
    pub(super) fn new(window: &[String]) -> Self {
        let mut prefix = String::from(PASSAGES_SCAFFOLD);
        for (i, chunk) in window.iter().enumerate() {
            if i > 0 {
                prefix.push_str(PASSAGE_SEP);
            }
            // FULL TEXT. The per-chunk 1,500-char cap that stood here is gone
            // (land B). Two reasons, and the second is the one that was
            // measured: a cut chunk MANUFACTURES ABSENCES — a judge asked
            // "do the passages support this claim" against a copy of the
            // evidence with the support snipped off will say no, and the
            // sibling specifics scan was observed doing exactly that,
            // flagging a phrase sitting verbatim at offset 1,497 of a chunk
            // it had been handed (note 95b82f97, which lifted the cap THERE
            // and left it here, unmeasured). And the pinned prefix contains
            // these bytes, so while they were truncated the scan's full-text
            // opening could not strict-prefix-match the judges' entry — the
            // cap was the thing standing between the two mechanisms and one
            // shared family.
            prefix.push_str(chunk);
        }
        Self {
            prefix,
            non_empty: !window.is_empty(),
        }
    }

    /// The family boundary, in bytes. `None` when the window carried no
    /// passage: every caller then declares nothing and degrades to an
    /// undeclared prompt. Absence is reported, never defaulted to 0 — a
    /// zero-length declaration is a different claim from "there is no stable
    /// window" (ARCH §18.3).
    pub(super) fn prefix_len(&self) -> Option<usize> {
        self.non_empty.then(|| self.prefix.len())
    }

    /// One claim-check prompt: the family prefix, then this call's own
    /// passages (summaries for a thematic claim, claim-conditioned hits), then
    /// the claim and the question. Returns the prompt and the boundary to
    /// declare.
    pub(super) fn claim_prompt(&self, appended: &[String], claim: &str) -> (String, Option<usize>) {
        let mut prompt = self.prefix.clone();
        for (i, chunk) in appended.iter().enumerate() {
            if self.non_empty || i > 0 {
                prompt.push_str(PASSAGE_SEP);
            }
            prompt.push_str(chunk);
        }
        prompt.push_str(&format!(
            "\n\"\"\"\n\n\
             CLAIM: {claim}\n\n\
             Do the passages, taken together, state or clearly imply this claim? \
             Support assembled across several passages counts; paraphrase counts; \
             the passages merely mentioning the people or things involved, without \
             establishing the claimed connection, does NOT count.\n\n\
             Answer with exactly one letter — A = the passages support the claim, \
             B = they do not."
        ));
        let boundary = self.prefix_len();
        debug_assert!(
            boundary.is_none_or(|n| prompt.is_char_boundary(n) && n <= prompt.len()),
            "the family boundary must be a char boundary inside the prompt"
        );
        debug_assert!(
            prompt.starts_with(&self.prefix),
            "a claim prompt must open with the family prefix"
        );
        (prompt, boundary)
    }

    /// The BATCHED register's prompt: the family prefix, then every extracted
    /// claim numbered, then one instruction — one prefill, N verdicts.
    ///
    /// Rendered HERE, by the family's own renderer, because family membership
    /// is the entire point of this register's 2026-08-14 reshape: D0 of order
    /// `audit-economy` measured the per-claim judges restoring the pinned
    /// evidence window in 34-53ms (129/129 calls), which made the original
    /// batched prompt — its own scaffold, its own 1,500-char chunk cuts, no
    /// declared boundary — a register that FULL-PREFILLS ~9K tokens to save
    /// calls that no longer pay for prefill. Opening with the byte-identical
    /// family prefix (and carrying [`CHUNK_JUDGE_SYSTEM`], asserted by
    /// `batched_register_joins_the_judges_prefix_family`) puts the one
    /// batched call in the same pinned-prefix family as its sibling judges:
    /// it restores the window the first per-claim call pinned, or pins it
    /// for them.
    ///
    /// The instruction language deliberately tracks [`Self::claim_prompt`]'s
    /// (assembly across passages counts, mere mention does not) — the batched
    /// verdict is judged against the same support standard, differing only in
    /// answer shape (N text lines vs one forced-choice logit). That shape
    /// difference is exactly what the judge-replay recalibration prices; see
    /// [`claims_support_batched`].
    pub(super) fn batched_claims_prompt(&self, claims: &[String]) -> (String, Option<usize>) {
        let mut prompt = self.prefix.clone();
        prompt.push_str("\n\"\"\"\n\nCLAIMS (numbered):\n");
        for (i, claim) in claims.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, claim));
        }
        prompt.push_str(&format!(
            "\nFor EACH numbered claim, do the passages, taken together, state or \
             clearly imply it? Support assembled across several passages counts; \
             paraphrase counts; the passages merely mentioning the people or things \
             involved, without establishing the claimed connection, does NOT count.\n\n\
             Output EXACTLY one line per claim, in order, formatted \"<n>: A\" (the \
             passages support it) or \"<n>: B\" (they do not). Output the {n} lines \
             and nothing else.",
            n = claims.len(),
        ));
        let boundary = self.prefix_len();
        debug_assert!(
            boundary.is_none_or(|n| prompt.is_char_boundary(n) && n <= prompt.len()),
            "the family boundary must be a char boundary inside the prompt"
        );
        debug_assert!(
            prompt.starts_with(&self.prefix),
            "a batched prompt must open with the family prefix"
        );
        (prompt, boundary)
    }

    /// The LOCATED-SPAN TRIAGE register's prompt: the family prefix — this
    /// call's candidate spans, one per chunk, in chunk order — then ONE claim
    /// and one instruction. One prefill, N verdicts.
    ///
    /// # The transpose of [`Self::batched_claims_prompt`]
    ///
    /// That register asks N claims against one shared window, and it is a
    /// family MEMBER because its window is shared with every sibling judge of
    /// the pass. This one's window is CLAIM-CONDITIONED — the spans are the
    /// ones cosine picked as being about this particular claim — so it has no
    /// sibling to share a prefix with and pins nothing for anyone. It renders
    /// here anyway because `impl EvidenceFamily` is the one place the scaffold
    /// and the separator may be written (`one_renderer_owns_the_family`);
    /// putting it anywhere else is how the boundary got two deciders before.
    ///
    /// Passages are addressed by ORDINAL POSITION rather than by an injected
    /// number, because the prefix render belongs to the family and this
    /// register does not get to change it. `n` is passed in rather than
    /// recomputed here so the instruction's count and the caller's expectation
    /// are one number and cannot drift apart.
    ///
    /// # A TRIAGE IS A RECALL INSTRUMENT — measured 2026-08-26
    ///
    /// The first cut of this prompt asked whether each passage supported the
    /// claim **on its own**, reasoning that the location loop wants origins
    /// and the whole-window judge upstream has already settled assembly. That
    /// is a STRICTER bar than [`Self::claim_prompt`]'s, and putting a stricter
    /// bar in front of a calibrated judge inverts what a triage is for. On the
    /// binder bed it voted B on 49 of 52 candidates and threw away BOTH chunks
    /// the calibrated register went on to bind — turning a `Passed` claim with
    /// two origins into a corroboration-floor `CouldNotJudge`.
    ///
    /// So the standard now tracks `claim_prompt`'s exactly, and the tie-break
    /// is stated explicitly and in the recall direction: when unsure, admit.
    /// The cost of a false admit is one ~2.5s calibrated call that says no;
    /// the cost of a false reject is a citation the deliverable never gets and
    /// a verdict that silently changes. Those are not symmetric and the prompt
    /// says which way to err.
    pub(super) fn span_triage_prompt(&self, claim: &str, n: usize) -> (String, Option<usize>) {
        let mut prompt = self.prefix.clone();
        prompt.push_str(&format!(
            "\n\"\"\"\n\nThe {n} passages above are numbered 1 to {n} in the order shown.\n\n\
             CLAIM: {claim}\n\n\
             For EACH numbered passage, could that passage support the CLAIM — does it \
             state, clearly imply, or supply part of it? Paraphrase counts; partial \
             support counts; a passage merely mentioning the people or things involved, \
             without bearing on the claimed connection at all, does NOT count.\n\n\
             This is a SHORTLIST, not a verdict: each passage you mark A is then checked \
             by a stricter judge, so a wrong A costs almost nothing and a wrong B loses \
             the evidence for good. WHEN IN DOUBT, ANSWER A.\n\n\
             Output EXACTLY one line per passage, in order, formatted \"<n>: A\" (could \
             support the claim) or \"<n>: B\" (clearly irrelevant to it). Output the {n} \
             lines and nothing else.",
            claim = claim.chars().take(2_000).collect::<String>(),
        ));
        let boundary = self.prefix_len();
        debug_assert!(
            boundary.is_none_or(|b| prompt.is_char_boundary(b) && b <= prompt.len()),
            "the family boundary must be a char boundary inside the prompt"
        );
        debug_assert!(
            prompt.starts_with(&self.prefix),
            "a span-triage prompt must open with the family prefix"
        );
        (prompt, boundary)
    }

    /// The specifics scan's prompt as a MEMBER of the family (order
    /// audit-economy D3 candidate A): the family prefix, then the summary
    /// tier appended after the boundary (same placement as a thematic claim
    /// check's summaries), then the question, the answer, and the scan
    /// instruction. The instruction is the pre-candidate scan's, with the
    /// item budget folded into the user prompt because the system turn is
    /// now the family's shared constant and cannot carry `max_items`.
    pub(super) fn scan_prompt(
        &self,
        summaries: &[String],
        question: &str,
        answer: &str,
        max_items: usize,
    ) -> (String, Option<usize>) {
        let mut prompt = self.prefix.clone();
        for (i, chunk) in summaries.iter().enumerate() {
            if self.non_empty || i > 0 {
                prompt.push_str(PASSAGE_SEP);
            }
            prompt.push_str(chunk);
        }
        prompt.push_str(&format!(
            "\n\"\"\"\n\nA user asked: {q}\n\n\
             The assistant's ANSWER:\n\"\"\"\n{ans}\n\"\"\"\n\n\
             Compare the ANSWER against the passages above and list every statement \
             in the ANSWER that is UNSUPPORTED or WRONG given those passages. Three \
             kinds to catch:\n\
             (1) A fabricated specific — a named person/place/thing, number, date, \
             direct quotation, section/version/chapter reference, code identifier or \
             value, or claimed programming language that is NOT in the passages.\n\
             (2) A misattribution — a statement, position, or quote the answer credits \
             to the wrong author/source/speaker relative to what the passages show.\n\
             (3) A false claim ABOUT the passages — e.g. the answer says the sources do \
             NOT contain something that they DO contain, or vice-versa.\n\
             (4) A stitched relation — the answer presents a person or position as \
             bridging, combining, or agreeing with another when the passages never \
             state that relation, even if both sides are real.\n\
             A detail the passages state, even paraphrased, is SUPPORTED — do not list \
             it. Ignore [Source: …] citation markers entirely — they are validated by a \
             separate pass; never list one as unsupported. \
             When genuinely unsure, leave it out, but DO flag a clear contradiction. \
             Quote the answer's exact wording. One item per line, at most {max_items} \
             lines. Reply with exactly NONE only if every statement in the answer is \
             supported by the passages.",
            q = question.chars().take(400).collect::<String>(),
            ans = answer.chars().take(12_000).collect::<String>(),
        ));
        let boundary = self.prefix_len();
        debug_assert!(
            boundary.is_none_or(|n| prompt.is_char_boundary(n) && n <= prompt.len()),
            "the family boundary must be a char boundary inside the prompt"
        );
        (prompt, boundary)
    }
}

/// Render one joint-register claim prompt without a model call — the
/// replay harness's window into [`EvidenceFamily`] (which stays
/// `pub(super)`: the harness gets bytes to fingerprint, not a second
/// renderer to drift). Byte-identical to what
/// [`claim_violation_joint`] sends for `chunks = shared ++ appended`,
/// `n_stable = shared.len()` — asserted by
/// `replay_render_matches_the_joint_register` below, not argued here.
pub(crate) fn replay_render_claim_prompt(
    shared: &[String],
    appended: &[String],
    claim: &str,
) -> (String, Option<usize>) {
    EvidenceFamily::new(shared).claim_prompt(appended, claim)
}

/// The batched register's prompt without a model call — the replay harness's
/// window into the batched shape, same contract as
/// [`replay_render_claim_prompt`]: byte-identical to what
/// [`claims_support_batched`] sends for the same `(shared, claims)`, asserted
/// by `replay_render_matches_the_batched_register` below.
pub(crate) fn replay_render_batched_claims_prompt(
    shared: &[String],
    claims: &[String],
) -> (String, Option<usize>) {
    EvidenceFamily::new(shared).batched_claims_prompt(claims)
}

/// Score EVERY candidate span of ONE claim in a single generation — the
/// deep-research audit's location loop, batched.
///
/// # Why this exists
///
/// `deep_research::audit::assess_claim` locates a claim's origins by judging
/// the claim against each chunk's best span separately. Measured on the
/// pin-validate flight of 2026-08-25 (`runs-pin-validate/pinned-1.log`, 328
/// claim audits over 102.5 minutes): 35 claims — 11% — reached that loop and
/// consumed 90.6 minutes, 88% of the whole audit, at ~130s each against a
/// 57-chunk window. The other 285 claims short-circuited before the loop and
/// averaged 1.85s. The loop is one model call per chunk and it returned 0-2
/// bound chunks out of 57.
///
/// # The window is the PINNED one, and that is the whole latency argument
///
/// `passages` MUST be the same slice the pass's whole-window judge was given,
/// so `EvidenceFamily::new` renders a byte-identical prefix and the daemon
/// restores it instead of prefilling it. The first cut built the window from
/// claim-conditioned best-spans, which by construction shares a prefix with
/// nothing: measured 2026-08-26, that cost **71,947ms of pure prefill per
/// claim** (43,816 prompt chars at this host's ~160 tok/s) against the 1,613ms
/// the same claim's whole-window judge paid on a warm prefix. A triage that
/// costs more than the 52 calls it saves is not a triage.
///
/// # TRIAGE ONLY — this is never the released verdict
///
/// This is a text A/B over N lines, not the calibrated single-token
/// forced-choice logit, so `SUPPORT_FLOOR`'s semantics do not transfer to it —
/// the same gap [`claims_support_batched`] carries. It is therefore used
/// strictly to decide WHICH spans are worth the calibrated call: a span this
/// register admits is re-judged by [`claim_violation_joint`] against
/// `SUPPORT_FLOOR` before it may bind, and a span it cannot settle (`None`)
/// falls through to that same call. The only verdict it can change is a span's
/// REJECTION, whose consequence is a claim losing support it might have had —
/// could-not-judge rather than passed. That direction is the honesty floor's,
/// which is why this may default on where a pass-direction substitution could
/// not (ARCH §18.3).
///
/// Alignment is hardened exactly as the sibling register's is: explicit
/// numbering, and a mis-count leaves the affected rows `None` (fallback to the
/// calibrated call), never a shifted verdict.
pub async fn spans_supporting_claim_batched(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
    passages: &[String],
    posture: ShardingPrivacy,
) -> Vec<Option<bool>> {
    let spans = passages;
    if spans.is_empty() {
        return Vec::new();
    }
    let family = EvidenceFamily::new(spans);
    let (prompt, stable_prefix_len) = family.span_triage_prompt(claim, spans.len());
    let req = CompletionRequest {
        prompt,
        stable_prefix_len,
        system_message: Some(CHUNK_JUDGE_SYSTEM.into()),
        preferred_speed: Speed::Slow,
        oicp: Some(Workload::Judge.requirements(posture)),
        // ~5 tokens per "<n>: A\n" verdict line + headroom for two-digit indices.
        max_tokens: Some(spans.len() * 8 + 16),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match gate_call(&**inference, &req, GateCallMechanism::LocatedSpanTriage).await {
        Ok(resp) => {
            let verdicts = parse_batched_verdicts(&resp.text, spans.len());
            let n_sup = verdicts.iter().filter(|v| **v == Some(true)).count();
            let n_none = verdicts.iter().filter(|v| v.is_none()).count();
            dbg(&format!(
                "span triage: {} spans -> {} admitted, {} unparsed | raw head: {:?}",
                spans.len(),
                n_sup,
                n_none,
                resp.text.chars().take(220).collect::<String>()
            ));
            verdicts
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "span triage pass failed");
            dbg(&format!("span triage failed: {e}"));
            // Total failure -> every span falls through to the calibrated call,
            // which is exactly today's behaviour. A failed triage costs time,
            // never a verdict.
            vec![None; spans.len()]
        }
    }
}

/// `n_stable`: how many leading entries of `chunks` are the shared prompt
/// window (byte-identical across every claim of this gate pass); entries after
/// that are claim-conditioned and vary per call. 0 = declare nothing.
pub async fn claim_violation_joint(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
    chunks: &[String],
    n_chunks: usize,
    n_stable: usize,
    posture: ShardingPrivacy,
) -> Option<f64> {
    // The window every sibling of this pass shares, then this call's own
    // passages. The split is the caller's `n_stable` contract, unchanged; what
    // changed is that the boundary now comes from the rendered window's length
    // rather than from a second formula computing the same number.
    let seen = chunks.len().min(n_chunks);
    let split = n_stable.min(seen);
    let family = EvidenceFamily::new(&chunks[..split]);
    let (prompt, stable_prefix_len) = family.claim_prompt(&chunks[split..seen], claim);
    let (a, b) = forced_choice_ab(
        inference,
        &prompt,
        stable_prefix_len,
        posture,
        GateCallMechanism::PerClaimJudge,
    )
    .await?;
    let denom = a + b;
    let support = if denom > 0.0 { a / denom } else { 0.0 };
    Some(1.0 - support)
}

/// Batched support pre-pass: every claim judged in a SINGLE generation off one
/// evidence window, returning per-claim support aligned to the input order
/// (`Some(true)` supported, `Some(false)` unsupported, `None` = no clean
/// aligned verdict → the caller re-verifies that row with the calibrated
/// per-claim `claim_violation_joint`).
///
/// # History — the premise this register was built on is measured stale
///
/// The original rationale ("the N per-claim calls re-prefill the SAME evidence
/// N times — ~11x prefill / ~9x slower", 2026-07-20) predates
/// `SOVEREIGN_PREFIX_STATE`: whole-context state restore now amortizes the
/// evidence prefill across sibling judges (D0 of order `audit-economy`,
/// 2026-08-14: 129/129 per-claim calls restored the 8.25K-token window in
/// 34-53ms; per-claim calls median 1.78s, not prefill-bound). The original
/// batched shape — own scaffold, 1,500-char chunk cuts, own system turn, no
/// declared boundary — therefore paid a FULL ~9K-token prefill to replace
/// calls that no longer pay one: measured net-zero to net-negative on the
/// composed-arm instrument (`audit_economy_d0_decomposition_20260814.md`).
///
/// # The reshape: the batched call JOINS the judges' prefix family
///
/// The prompt now opens with the byte-identical [`EvidenceFamily`] prefix,
/// carries [`CHUNK_JUDGE_SYSTEM`], and declares the family boundary — so the
/// one batched call restores the pin its sibling judges use (or pins it for
/// them), and "one prefill" becomes a ~40ms restore on warm evidence. The
/// 1,500-char cut is gone for the same reason land B removed it from the
/// family: a cut chunk manufactures absences, and cut bytes can never
/// strict-prefix-match the pinned window.
///
/// STUDY ONLY (behind `SOVEREIGN_GATE_BATCH_VERIFY`): the verdict here is a
/// TEXT A/B over N lines, NOT the calibrated single-token forced-choice logit,
/// so `tau` semantics do not transfer — the `svrn bench judge-replay`
/// recalibration (order `audit-economy` D1) prices exactly that gap before any
/// flip. The deterministic in-world name/identifier veto still runs first,
/// catching blatant fabrication regardless of this register's verdict.
/// Alignment is hardened by explicit numbering; a mis-count leaves the
/// affected rows `None` (fallback), never a shifted verdict.
pub(crate) async fn claims_support_batched(
    inference: &Arc<dyn InferenceProvider>,
    claims: &[String],
    chunks: &[String],
    n_chunks: usize,
    posture: ShardingPrivacy,
) -> Vec<Option<bool>> {
    if claims.is_empty() {
        return Vec::new();
    }
    let seen = chunks.len().min(n_chunks);
    let family = EvidenceFamily::new(&chunks[..seen]);
    let (prompt, stable_prefix_len) = family.batched_claims_prompt(claims);
    let req = CompletionRequest {
        prompt,
        stable_prefix_len,
        system_message: Some(CHUNK_JUDGE_SYSTEM.into()),
        preferred_speed: Speed::Slow,
        oicp: Some(Workload::Judge.requirements(posture)),
        // ~5 tokens per "<n>: A\n" verdict line + headroom for two-digit indices.
        max_tokens: Some(claims.len() * 8 + 16),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match gate_call(&**inference, &req, GateCallMechanism::BatchedSupport).await {
        Ok(resp) => {
            let verdicts = parse_batched_verdicts(&resp.text, claims.len());
            let n_sup = verdicts.iter().filter(|v| **v == Some(true)).count();
            let n_none = verdicts.iter().filter(|v| v.is_none()).count();
            dbg(&format!(
                "batched verify: {} claims -> {} supported, {} unparsed | raw head: {:?}",
                claims.len(),
                n_sup,
                n_none,
                resp.text.chars().take(220).collect::<String>()
            ));
            verdicts
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "batched verify pass failed");
            dbg(&format!("batched verify failed: {e}"));
            vec![None; claims.len()] // total failure → per-claim fallback for all
        }
    }
}

/// Parse `"<n>: A|B"` verdict lines into a per-claim support vec (1-based `n` →
/// 0-based index). Tolerant of `:`/`.`/`)` separators and list bullets; last
/// write wins; out-of-range or malformed rows stay `None` so the caller
/// re-verifies them with the calibrated pass. Pure/synchronous so the alignment
/// contract is pinned by `cargo test` without a model.
pub(crate) fn parse_batched_verdicts(text: &str, n: usize) -> Vec<Option<bool>> {
    let mut out = vec![None; n];
    for line in text.lines() {
        let t = line.trim().trim_start_matches(['-', '*', '•', ' ']).trim();
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let idx = match digits.parse::<usize>() {
            Ok(v) if v >= 1 && v <= n => v - 1,
            _ => continue,
        };
        let rest = t[digits.len()..]
            .trim_start_matches([':', '.', ')', ' ', '-', '=', '>'])
            .trim();
        match rest.chars().next().map(|c| c.to_ascii_uppercase()) {
            Some('A') => out[idx] = Some(true),
            Some('B') => out[idx] = Some(false),
            _ => {} // ambiguous → leave None (fallback re-verifies)
        }
    }
    out
}
