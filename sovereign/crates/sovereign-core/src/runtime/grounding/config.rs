// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate configuration: the closed set of gated surfaces, per-surface
//! verification budgets, and the env-flag registry (SSOT for every
//! knob the gate reads — mirrors `retrieval_pipeline_flags()`).

use crate::runtime::retrieval_pipeline::EnvFlag;

/// Whether `SOVEREIGN_AGENTIC_KQ_DEBUG` is set — the opt-in switch for the
/// gate's glassbox extras: the `dbg()` stderr/tracing mirror AND recording the
/// pre-gate draft into message metadata (see `gate_answer`). Default off, so a
/// production message never carries the rejected draft, which can be the very
/// confabulation the gate just suppressed. Cached once.
pub(crate) fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("SOVEREIGN_AGENTIC_KQ_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Stderr mirror for bench/CLI surfaces that install no tracing
/// subscriber — same pattern (and same env var) as the agentic
/// loop's dbg().
pub(crate) fn dbg(msg: &str) {
    if debug_enabled() {
        eprintln!("    [gate] {msg}");
        // Also emit via tracing: a DETACHED daemon discards stderr, so eprintln
        // never reaches daemon.err — the gate was invisible in the deployed path.
        // Use the DEFAULT target (this module = `sovereign_core::…`), which
        // matches the daemon's crate-scoped filter (`sovereign_core=info`); a
        // custom `target:` would be filtered out. (2026-06-18 glassbox fix.)
        tracing::info!("[gate] {msg}");
    }
}

/// The grounding verification contract is ON by default — it is the
/// "Grounded Everywhere" promise (desktop chat and every other
/// answer-producing surface ship with it live), not an opt-in env flag.
/// Only an explicit `SOVEREIGN_GROUNDING_GATE=0` / `false` turns it off
/// (naked benches, latency debugging); unset — or any other value —
/// leaves it on. Per-surface overrides (`SOVEREIGN_GROUNDING_GATE_<SURFACE>`)
/// still win over this global default, see `GateSurface::enabled`.
pub(crate) fn grounding_gate_enabled() -> bool {
    std::env::var("SOVEREIGN_GROUNDING_GATE")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true)
}

/// Violation-probability threshold τ — THE shared default for every consumer
/// of the external grounding-verifier: the production gate (via
/// `VerifyProfile::tau`) and the chaos bench's `--grounding-verify` lane.
/// `SOVEREIGN_GV_THRESHOLD` overrides; unset = the bench-calibrated 0.9.
/// Public so bench code cannot re-derive its own divergent default (the
/// chaos lane carried a silent 0.5 until 2026-07-30).
pub fn grounding_gate_threshold() -> f64 {
    std::env::var("SOVEREIGN_GV_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.9)
}

/// Citation-grounded answering on entity-anchored fact queries. OFF by default
/// (clean A/B until the bank justifies a flip): when on, the gate replaces
/// generate-then-substring-verify with active quoting — the model must copy the
/// supporting sentence before it answers. See `citation::citation_grounded_answer`.
pub(crate) fn citation_grounding_enabled() -> bool {
    std::env::var("SOVEREIGN_CITATION_GROUNDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        // Default ON: the attach-mode QA bank justified the flip (2026-06-24).
        // Pooled iter8+9 over the resident corpora: answers the verifier grounded
        // with a copied quote broke at 3.6% vs 25.9% for ungrounded ones — a 7x
        // reduction, because the model COPIES the supporting span instead of
        // confabulating the specific. Set SOVEREIGN_CITATION_GROUNDING=0 to A/B off.
        .unwrap_or(true)
}

/// Run quote-first citation grounding on ALL gated factual answers, not just
/// entity-anchored ones. The default `entity_anchored` gate is too strict — the
/// chaos stream tripped it 0 times — so quote-first never got to cure the
/// confabulated-specific class ("Ernest Rhys Jones" for "Ernest Rhys"). Quote-
/// first is ADDITIVE and SAFE where the per-claim rewrite is not: it makes the
/// model COPY a supporting sentence (it can't add a token the quote lacks) or
/// falls through to the legacy ladder — it never re-searches near-miss noise nor
/// rewrites a correct answer. A/B via `SOVEREIGN_CITATION_BROAD`.
pub(crate) fn citation_broad_enabled() -> bool {
    std::env::var("SOVEREIGN_CITATION_BROAD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        // Default ON (2026-06-24): the chaos stream tripped `entity_anchored` 0
        // times, so without broad the verifier never fires. The fall-through is
        // additive and safe (no clean quote → legacy ladder), so broad-by-default
        // only adds coverage. Set SOVEREIGN_CITATION_BROAD=0 to A/B off.
        .unwrap_or(true)
}

/// Multi-quote citation contract: let the quote-first path answer a COMPOUND
/// question part by part — one verified quote per sub-question — instead of
/// demanding a single sentence that answers the whole thing.
///
/// Why (measured 2026-08-04, chaos-monkey `saltgrass_compound`, n=7 x 2
/// independent runs): the single-sentence contract grounds 0/14 compound
/// probes. Every probe ends `ANSWER: NONE`. The decisive case is
/// `compound-inn-and-innkeeper`, where the model COPIED the correct sentence
/// ("They took Severin Quenholt at The Cold Lantern...") — which answers part
/// one verbatim — and still answered NONE, because no one sentence also gives
/// the innkeeper's first name. So `cites_a_source` is 0/7 structurally, not
/// because the model is weak: the only path that emits a locatable citation can
/// never release on a two-part question, and all 14 fall through to the legacy
/// ladder, where the gate then kills 3-4 correct drafts per run.
///
/// ON by default since 2026-08-05 — the ledger's flip condition was MET and the
/// trade it worried about did not materialise. It is NOT purely additive the way
/// `citation_broad` is: it converts a legacy-ladder turn into a partial citation
/// release, so it could in principle have replaced a full correct legacy answer
/// with a grounded-half-plus-named-gap. Measured instead (matched control, same
/// HEAD/day/topology, `saltgrass_compound` n=7, 0 extraction failures in both
/// arms, `=1` vs `=0`):
///   citation releases        0 -> 3
///   competence-when-present  0.14 (1/7 correct) -> 0.43 (3/7)
///   misses attributed to gate  4 -> 2
///   blatant-confab-rate      0.00 -> 0.00   (no regression — the flip condition)
/// It did not trade a correct answer for a partial one; it RECOVERED two turns
/// the legacy ladder was abstaining away (`compound-sentence-then-inn` and
/// `compound-constable-then-finder` both go Abstained -> citation_grounded).
/// Set `SOVEREIGN_CITATION_MULTIQUOTE=0` for the control arm.
/// Name the SECTION a released quote came from ("CHAPTER VII — \"…\"").
///
/// ON by default, and purely additive by construction: a locator appears only
/// where the corpus's chunk→section join can attribute the quote to one
/// passage, so a corpus without section structure — or without a populated
/// join — releases exactly the text it always did. It cannot make a claim
/// pass a check it would otherwise fail; the label is display-only and sits
/// OUTSIDE the quote marks the verbatim re-check reads.
///
/// The flag exists so the effect can be MEASURED, not because the default is
/// in doubt: `SOVEREIGN_CITATION_LOCATOR=0` is the control arm for the
/// situated lane's `cites_a_source` criterion, which was 0/7 in both arms of
/// the arm-C comparison because the answer path had nothing per-passage to
/// name (2026-08-05). Without a control there is no way to tell a locator
/// win from bank noise.
pub(crate) fn citation_locator_enabled() -> bool {
    std::env::var("SOVEREIGN_CITATION_LOCATOR")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true)
}

pub(crate) fn citation_multiquote_enabled() -> bool {
    std::env::var("SOVEREIGN_CITATION_MULTIQUOTE")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true)
}

/// Exact-value + GK-fabrication fidelity fixes (2026-07-01). ON by default;
/// `SOVEREIGN_EXACTVAL_FIX=0` restores the prior behaviour for a clean replay
/// A/B. Gates two changes together (both target the same exact-value residual):
/// (1) citation `answer_supported_by_quote` requires a numeric answer token to
/// match a COMPLETE digit-run in the quote, not a substring (kills truncated-
/// number grounding, "289494" vs "28949423"); (2) `gate_answer` strips the GK
/// caveat UNCONDITIONALLY before verifying (the gated path always has retrieved
/// docs, so a "from general knowledge" escape hatch must be held to the evidence,
/// not exempted as NO_CLAIM — kills confident GK fabrication like "Eddie
/// Henderson").
pub(crate) fn exactval_fix_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_EXACTVAL_FIX").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// EXPERIMENT (`SOVEREIGN_GATE_BATCH_VERIFY=1`, default OFF): in
/// `gate_longform`, run ONE batched support pass over all extracted claims
/// and trust it ASYMMETRICALLY — a batch "supported" clears the claim
/// without a per-claim call; "unsupported" and parse gaps fall through to
/// the calibrated per-claim forced-choice, so flags stay calibrated by
/// construction (order audit-economy D2). The register is the family-joined
/// batched shape (`EvidenceFamily` prefix + `CHUNK_JUDGE_SYSTEM` + numbered
/// claims), replay-recalibrated at catch 0.950 / clear 1.000 with zero
/// (c)-class loss (fc58319d). HISTORY, so the next reader does not re-buy
/// the premise: the original 2026-07-20 rationale ("N re-prefills collapse
/// to one, ~11x/~9x") went STALE when `SOVEREIGN_PREFIX_STATE` whole-state
/// restore amortized per-claim prefill (D0, a85cede1) — the surviving win
/// is skipping the per-claim CALL on batch-supported rows (~53.7% measured
/// support rate). Default OFF pending the D2 live smoke (amended bar,
/// directive 6686251c: batch+judges call-sum <=6.5s) and the full live
/// discipline; promotion is operator-held. Ledger:
/// sovereign/DEFAULTS_LEDGER.md.
pub(crate) fn gate_batch_verify_enabled() -> bool {
    std::env::var("SOVEREIGN_GATE_BATCH_VERIFY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// SHADOW measurement (`SOVEREIGN_GATE_BATCH_SHADOW=1`, default OFF): run the
/// batched pass AND the calibrated per-claim forced-choice for every claim, log
/// the pair, but KEEP baseline behavior (use the calibrated verdict). This scores
/// batch-vs-calibrated agreement without changing any answer — the decisive input
/// for the default-on call (the false-support rate = batch says supported where
/// calibrated says failed = fabrication-leak risk). Overrides the normal batch
/// path when set.
pub(crate) fn gate_batch_shadow_enabled() -> bool {
    std::env::var("SOVEREIGN_GATE_BATCH_SHADOW")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Minimum extracted-claim count for the batched pre-pass to fire (else the
/// per-claim path runs). The single batched prefill only amortises when it
/// replaces enough per-claim re-prefills; the 2026-07-20 A/B measured a net
/// regression on small answers (7→9 primary calls at ~3 claims) and clear wins
/// at ~10+ (23→10). Default 6; tune with `SOVEREIGN_GATE_BATCH_MIN_CLAIMS`.
pub(crate) fn gate_batch_min_claims() -> usize {
    std::env::var("SOVEREIGN_GATE_BATCH_MIN_CLAIMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6)
}

/// How many per-claim corpus searches the gate runs at once — DERIVED from the
/// host, not fixed.
///
/// A hard number cannot survive the hardware range this ships to, and that is
/// the same lesson as the retired 4 s triage floor and the retired cost ratio:
/// `cores / 4`, clamped to 1..=4. A 12-core workstation gets 3; the 4-core
/// laptop in the issue report gets 1, which is EXACTLY the serial behaviour it
/// had before — so weak hardware sees no new concurrency, and no new memory
/// risk, from this change.
///
/// The audit's fan-out was `claims x corpora` SEQUENTIAL round trips, each one
/// an embed call plus a hybrid index search — the only multiplicative term in
/// the whole turn: measured 2026-09-02 on wikipedia+sep, ten claims at ~9.6 s
/// each, 77 s spent one search after another. Concurrency changes wall time,
/// never work, and it is what retired the model-call triage that used to guard
/// this cost (issue #57): a bound you can derive beats a decision you have to
/// price.
///
/// The turn's OWN retrieval solved this in 2026-06 and the gate never got the
/// same treatment (`corpus_search.rs`, `SOVEREIGN_KQ_FANOUT_CONCURRENCY`,
/// default 4): "concurrency changes only WALL-TIME, never results". The same
/// argument holds here and is in fact stronger — each claim's hits depend on
/// that claim alone, they are merged per claim, and the judge that consumes
/// them still runs in claim order. So this collapses the worst case toward the
/// SLOWEST SINGLE SEARCH instead of their sum, without any claim losing a
/// search or a verdict.
///
/// Bounded rather than unbounded for the reason the retrieval path gives: a
/// wide fan-out must not thundering-herd the big indexes (sep/wikipedia) on
/// open + search, and every search also wants the embed slot. A plain constant
/// rather than an env read — it is a resource bound, not a tuning surface, and
/// a new env var would need a `quality/env-flags.toml` row to earn its keep.
pub(crate) fn claim_search_concurrency() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| concurrency_for_cores(n.get()))
            .unwrap_or(1)
    })
}

/// The derivation itself, separated from the host it reads, because a test
/// that re-derives `(cores / 4).clamp(1, 4)` to check `(cores / 4).clamp(1, 4)`
/// cannot fail — change the formula and both sides move together (§18.1). As
/// a function of a NAMED core count it is falsifiable on any host.
pub(crate) fn concurrency_for_cores(cores: usize) -> usize {
    (cores / 4).clamp(1, 4)
}

/// The ONE bound on in-flight claim searches, shared by both fan-out levels.
///
/// The fan-out is nested — claims on the outside, corpora within each claim —
/// and bounding each level separately bounds neither: 4 x 4 is sixteen
/// concurrent `open_index` + hybrid searches against indexes that reach 88 GB,
/// on a host that is also holding a 17.7 GB model resident. That product is a
/// memory event waiting to happen, and it caused one on 2026-09-02.
///
/// So the permit is taken around the index search itself, at the innermost
/// point, from a semaphore shared process-wide. Whatever the nesting, total
/// in-flight searches are `CLAIM_SEARCH_CONCURRENCY`. Process-global rather
/// than per-turn because the resource it protects — page cache, file handles,
/// the box — is global: two concurrent turns must share the bound, not get one
/// each.
pub(crate) fn claim_search_permits() -> &'static tokio::sync::Semaphore {
    static SEM: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SEM.get_or_init(|| tokio::sync::Semaphore::new(claim_search_concurrency()))
}

/// Kill-switch for the gate's per-claim corpus re-search
/// (`SOVEREIGN_GATE_CLAIM_SEARCH=0`, default ON). `ClaimSearcher::search_corpus`
/// runs one hybrid search PER ALLOWED CORPUS PER CLAIM and keeps
/// `CLAIM_SEARCH_K` chunks total; on a bank whose evidence pool includes a
/// large corpus that fan-out dominates the turn. Measured 2026-08-05 on
/// `bench sep/summarize --synth` (14 questions, HEAD d3c5261d): 753 searches
/// inside the gate window costing 608.9s, of which `wikipedia` was 247 calls
/// at 2218ms = 547.9s — 25% of total run wall-clock, against a 33.8s median
/// draft phase.
///
/// Turning this OFF makes the audit judge against the prompt chunks alone,
/// which is exactly the documented pre-feature behavior of `search_corpus`
/// ("Empty on any failure: the audit then judges against the prompt chunks
/// alone"). It exists so the re-search's VALUE can be measured against its
/// cost — if answer-equiv and the gate's action distribution hold with it
/// off, the fan-out is complexity to delete rather than to optimise.
pub(crate) fn claim_search_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_GATE_CLAIM_SEARCH").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// SHADOW measurement for the per-claim corpus re-search
/// (`SOVEREIGN_GATE_CLAIM_SEARCH_SHADOW=1`, default OFF). Same pattern as
/// [`gate_batch_shadow_enabled`]: measure the alternative WITHOUT changing any
/// answer.
///
/// `claim_violation_joint` already scores the re-searched hits and the prompt
/// chunks in ONE pass and takes their max, so the counterfactual "what would
/// this claim's verdict have been on the prompt chunks alone?" is derivable
/// from the same judge calls — no duplicate inference. The one thing in the
/// way is the `max_support >= 0.95` early break: extras are checked FIRST, so
/// a genuine rescue stops the loop before the chunk-only answer is known.
/// Shadow mode keeps iterating past that break PURELY to fill in the
/// counterfactual, and still returns the support value the production loop
/// would have stopped at — so the released verdict is bit-identical and only
/// wall-time changes.
///
/// What it buys: a per-claim (vp_production, vp_chunks_only) pair. That is the
/// tuning curve for spending the fan-out selectively — "if re-search only fired
/// for claims whose chunk-only vp sits in band [x,y], what fraction of the real
/// rescues would we still catch, at what fraction of the 2218ms-per-wikipedia
/// -call cost?" A bank-level on/off A/B cannot answer that; it averages the
/// rescues away.
pub(crate) fn claim_search_shadow_enabled() -> bool {
    std::env::var("SOVEREIGN_GATE_CLAIM_SEARCH_SHADOW")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// AUDIT FORENSICS (`SOVEREIGN_GATE_AUDIT_FORENSICS=<path>`, default unset).
///
/// The per-claim audit decides, on every long-form turn, which claims are
/// unsupported — and that decision has never been inspectable. `dbg()` prints
/// the claim and its violation probability; nothing prints THE EVIDENCE THE
/// CLAIM WAS JUDGED AGAINST, so "is this failure real?" could not be answered
/// from outside the process. That is the gap this knob closes: it appends one
/// JSONL record per audit (the evidence window) and one per claim decision (the
/// claim, the mechanism that decided it, its score, and any claim-conditioned
/// passages that widened the window), which is exactly the material a human
/// needs to validate the instrument before trusting its result (ARCH §18.4).
///
/// Default OFF and it must stay off: the records contain verbatim corpus text
/// and the model's draft claims, which is user content that has no business on
/// disk outside a deliberate diagnostic run.
pub(crate) fn audit_forensics_path() -> Option<std::path::PathBuf> {
    std::env::var("SOVEREIGN_GATE_AUDIT_FORENSICS")
        .ok()
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// Opt-in (SOVEREIGN_GATE_PIPELINE=1): PHASE A SCAFFOLD — verify sentences on the
/// fast slot AS the draft streams on the 35B, so audit #1 overlaps synthesis
/// instead of running after it (see docs/specs/STREAMING_GATE_PIPELINE.md).
/// Default OFF. In the scaffold the streamed verdicts are glassbox-logged but NOT
/// yet consumed by the gate — wiring them into gate_longform (to skip
/// re-verification) is the next increment, gated on a fast-slot-verify calibration.
pub(crate) fn gate_pipeline_enabled() -> bool {
    matches!(
        std::env::var("SOVEREIGN_GATE_PIPELINE").ok().as_deref(),
        Some("1") | Some("true") | Some("on")
    )
}

/// Default-ON (SOVEREIGN_SURGICAL_REWRITE=0 opts out): correct only the failed
/// sentences of a longform answer on the fast slot instead of re-synthesising
/// the whole answer on the 35B, then run the SAME full re-audit the full-rewrite
/// path runs. Proven fabrication-safe by the 2026-07-17 re-calibration: surgical
/// + full re-audit matched the OFF baseline exactly (hallucination 0.00,
/// grounding 1.00, CONFAB-LEAKED 0). The full re-audit is the safety floor; an
/// earlier scoped re-audit that skipped it leaked and was reverted.
pub(crate) fn surgical_rewrite_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_SURGICAL_REWRITE").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// TOMBSTONE (`NATIVE_GROUNDING_ECONOMY.md` §9 Phase 4, order
/// `gate-tombstone-ladder`). **Default OFF: the longform repair ladder does
/// not execute.** `SOVEREIGN_GATE_LONGFORM_REPAIR=1` re-arms it.
///
/// What stops executing when this is off: the longform **rewrite** pass
/// (surgical span-edits or full re-synthesis) and **audit #2**, the re-audit
/// that exists only because the rewrite produced new, unaudited prose. A
/// longform draft whose audit found failures is now RELEASED with those
/// claims marked, instead of being re-synthesised and re-audited.
///
/// **Why the grounding function is undiminished** (spec §3.3 G2): marking
/// discharges G2 completely. Nothing is regenerated, so the released text is
/// the *audited* draft — every failed claim reaches the reader as a
/// `failed_once` holding in a `mixed`-verdict epistemic ledger
/// (`grounding/mod.rs::longform_claims` → `runtime/epistemic.rs`), which is
/// the marking that ships and renders on the desktop today. This is NOT the
/// reverted 2026-07-17 experiment (spec §7.4): there, a rewrite's unaudited
/// new prose shipped with its check removed and leaked a fabrication
/// (CONFAB-LEAKED 0→1). Here there is no new prose to check.
///
/// **Why ONE knob covers two tombstoned paths.** Audit #2's only input is the
/// rewrite's output (`StageCause::RewriteProducedNewProse`). A separate
/// re-audit flag would make "rewrite ON, re-audit OFF" reachable — which is
/// exactly the configuration that leaked and was reverted. One switch keeps
/// that combination unreachable by construction rather than by memory
/// (ARCH §7, §10.6). Both paths carry their own `DEFAULTS_LEDGER.md` row.
///
/// Re-arming is verifiable from the product, not from this flag: the strip
/// records `Rewrite` / `ReAudit` rows from the branch actually taken, so a
/// tombstone that fires shows as an OLD STACK row (spec §9.0 guard 2).
pub(crate) fn longform_repair_enabled() -> bool {
    matches!(
        std::env::var("SOVEREIGN_GATE_LONGFORM_REPAIR")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("on")
    )
}

/// The closed set of answer-producing surfaces the gate covers.
/// Adding a surface = adding a variant + a profile + a bank — there
/// is no open registration, by design: every gated surface must have
/// shipped with its own measured calibration bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
// Variants land with their phases (attached_doc P2, complex_task P4,
// simple_query + refinement P6) — declared up front so the closed set
// and its override grammar are reviewable as one unit.
#[allow(dead_code)]
pub(crate) enum GateSurface {
    /// Streaming + non-streaming KnowledgeQuery (share one profile).
    KnowledgeQuery,
    /// Streaming DeepQuery spawn.
    DeepQuery,
    /// Attached-document Q&A (Phase 2).
    AttachedDoc,
    /// Tool-using task synthesis over step transcripts (Phase 4).
    ComplexTask,
    /// Non-streaming simple-query path when retrieval matched (Phase 6).
    SimpleQuery,
    /// Gap-check refinement re-verification (Phase 6; retry off —
    /// the refinement itself was the rewrite).
    Refinement,
    /// Governance Q&A over current law (FR-9). Cite an active rule or
    /// abstain — its own surface so the governance bank (RL-1: no
    /// confabulated rule; RL-2: honest abstention) calibrates the gate
    /// independently of the general KnowledgeQuery banks.
    Governance,
    /// Proxy-voting Q&A over a company's ballot (SEC DEF 14A). State the
    /// sides of a proposal from the filing's verbatim text or abstain —
    /// its own surface so the proxy bank (RL-1: no confabulated
    /// opposition for a management item; RL-2: both sides cited for a
    /// shareholder proposal) calibrates the gate independently. Mirrors
    /// the Governance discipline; the bank and override var are what make
    /// it separately measured.
    ProxyArgument,
}

impl GateSurface {
    pub(crate) const fn id(self) -> &'static str {
        match self {
            GateSurface::KnowledgeQuery => "knowledge_query",
            GateSurface::DeepQuery => "deep_query",
            GateSurface::AttachedDoc => "attached_doc",
            GateSurface::ComplexTask => "complex_task",
            GateSurface::SimpleQuery => "simple_query",
            GateSurface::Refinement => "refinement",
            GateSurface::Governance => "governance",
            GateSurface::ProxyArgument => "proxy_argument",
        }
    }

    /// Per-surface env override name (e.g.
    /// `SOVEREIGN_GROUNDING_GATE_ATTACHED_DOC`).
    const fn override_var(self) -> &'static str {
        match self {
            GateSurface::KnowledgeQuery => "SOVEREIGN_GROUNDING_GATE_KNOWLEDGE_QUERY",
            GateSurface::DeepQuery => "SOVEREIGN_GROUNDING_GATE_DEEP_QUERY",
            GateSurface::AttachedDoc => "SOVEREIGN_GROUNDING_GATE_ATTACHED_DOC",
            GateSurface::ComplexTask => "SOVEREIGN_GROUNDING_GATE_COMPLEX_TASK",
            GateSurface::SimpleQuery => "SOVEREIGN_GROUNDING_GATE_SIMPLE_QUERY",
            GateSurface::Refinement => "SOVEREIGN_GROUNDING_GATE_REFINEMENT",
            GateSurface::Governance => "SOVEREIGN_GROUNDING_GATE_GOVERNANCE",
            GateSurface::ProxyArgument => "SOVEREIGN_GROUNDING_GATE_PROXY_ARGUMENT",
        }
    }

    /// Is the gate on for THIS surface? Global `SOVEREIGN_GROUNDING_GATE`
    /// sets the default; the per-surface var overrides in either
    /// direction (=1 forces on, =0 forces off). Per-surface rollout is
    /// the whole point: each surface flips only on its own bank's
    /// evidence.
    pub(crate) fn enabled(self) -> bool {
        match std::env::var(self.override_var()) {
            Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => true,
            Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") => false,
            _ => grounding_gate_enabled(),
        }
    }

    /// This surface's verification budget. Defaults are pinned by the
    /// `profile_defaults_are_pinned` golden test — change a value
    /// there only together with the bank run that justifies it.
    pub(crate) fn profile(self) -> GroundingProfile {
        let tau = grounding_gate_threshold();
        match self {
            GateSurface::KnowledgeQuery | GateSurface::DeepQuery => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                retry: true,
                longform_chars: 1_800,
            },
            GateSurface::AttachedDoc | GateSurface::SimpleQuery => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                retry: true,
                longform_chars: 1_800,
            },
            // Synthesis claims assemble across step outputs — the
            // per-chunk max-support check is structurally biased
            // against exactly that, so ComplexTask always takes the
            // per-claim joint-judge ladder.
            GateSurface::ComplexTask => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                retry: true,
                longform_chars: 0,
            },
            // The refinement itself was the rewrite: verify only,
            // never re-synthesize. On failure the caller keeps the
            // already-verified original.
            GateSurface::Refinement => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                retry: false,
                longform_chars: 1_800,
            },
            // Governance answers are short statements of current law —
            // cite the active rule or abstain. `retry` on so a failed
            // verify becomes RL-2 honest abstention, not a confident
            // guess. Same budget as KnowledgeQuery; the override var and
            // bank are what make it a separately-calibrated surface.
            GateSurface::Governance => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                retry: true,
                longform_chars: 1_800,
            },
            // Proxy answers are short statements of a ballot item's sides
            // grounded in the filing's verbatim text — cite both sides
            // (or the single side present) or abstain. Same budget +
            // discipline as Governance; `retry` on so a failed verify
            // becomes an honest abstention ("the filing carries only the
            // board's recommendation"), never a fabricated against-case.
            GateSurface::ProxyArgument => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                retry: true,
                longform_chars: 1_800,
            },
        }
    }
}

/// HOW MUCH verification one surface budgets. Plain copyable data,
/// not behavior — the ladder in `gate_answer` is the behavior.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GroundingProfile {
    pub surface: GateSurface,
    /// Violation-probability threshold (bench-calibrated 0.9; the
    /// judge prompts are byte-pinned to the bench critic so it
    /// transfers).
    pub tau: f64,
    /// Long-form audit: claims checked per draft.
    pub max_claims: usize,
    // `max_chunks` (passages per claim-verdict judge call, 8 on every
    // surface) was REMOVED 2026-08-13. It had exactly one consumer —
    // `gate_longform`'s `per_claim_chunks` — and that site now derives the
    // window from the retrieved leaf set instead, because the auditor must be
    // shown what the drafter was shown (see the rationale at the derivation).
    // Leaving the field would have left a knob that reads as though it governs
    // the audit's evidence and no longer does: the next reader would change 8
    // to 20 and watch nothing happen (ARCH §10.6, one decider one name).
    /// Corrective retry/rewrite allowed (false = verify-only).
    pub retry: bool,
    /// Char pivot between the single-claim and per-claim ladders;
    /// 0 = always per-claim.
    pub longform_chars: usize,
}

/// Every env knob the grounding gate reads — registry-test consumed,
/// doc-table renderable; same pattern as `retrieval_pipeline_flags()`.
/// Human reference (gate + agentic-loop + observability flags, with the
/// canonical chaos-bench invocation): `sovereign/docs/GROUNDING_GATE_ENV.md`.
pub fn grounding_gate_flags() -> Vec<(&'static str, EnvFlag)> {
    vec![
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_GROUNDING_GATE",
                default: "on",
                purpose: "Global on/off for the hold→verify→retry→abstain gate on answer-producing surfaces. ON by default (the Grounded-Everywhere contract); set =0 to opt out (naked benches, latency debugging).",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_GROUNDING_GATE_<SURFACE>",
                default: "unset",
                purpose: "Per-surface override (=1 forces on, =0 forces off); SURFACE ∈ {KNOWLEDGE_QUERY, DEEP_QUERY, ATTACHED_DOC, COMPLEX_TASK, SIMPLE_QUERY, REFINEMENT, GOVERNANCE, PROXY_ARGUMENT}.",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_GATE_LONGFORM_REPAIR",
                default: "off",
                purpose: "TOMBSTONE (ECONOMY §9 Phase 4). Default OFF: the longform repair ladder — the rewrite pass AND audit #2, the re-audit that exists only because the rewrite produced new prose — does not execute. A draft whose audit found failures is released with those claims MARKED (failed_once holdings in a mixed-verdict epistemic ledger), not re-synthesised. =1 re-arms both. One knob covers both paths deliberately: audit #2's only input is the rewrite's output, so a separate re-audit flag would make the reverted 2026-07-17 leak configuration (rewrite on, re-audit off) reachable.",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_GV_THRESHOLD",
                default: "0.9",
                purpose: "Violation-probability threshold τ (bench-calibrated; transfers via judge-prompt byte-identity with the bench critic).",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_GATE_CLAIM_SEARCH",
                default: "on",
                purpose: "Per-claim corpus re-search that widens the audit's evidence beyond the prompt chunks. Set =0 to audit against the prompt chunks alone (the documented no-searcher fallback). Exists to price the fan-out: measured 2026-08-05 on bench sep/summarize --synth at 608.9s across 14 questions — 25% of run wall-clock — because it runs one hybrid search per allowed corpus per claim and keeps only CLAIM_SEARCH_K chunks total.",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_GATE_CLAIM_SEARCH_SHADOW",
                default: "off",
                purpose: "Measure the per-claim re-search without changing any answer: log each claim's verdict WITH the re-searched hits alongside the counterfactual on the prompt chunks alone (event `claim_search_shadow`). Derived from the same judge pass, so no duplicate inference except past the 0.95 early break. Produces the (vp_production, vp_chunks_only) pairs needed to fire the fan-out selectively instead of on every claim.",
            },
        ),
        (
            "-",
            EnvFlag {
                name: "SOVEREIGN_AGENTIC_KQ_DEBUG",
                default: "off",
                purpose: "Mirror gate (and agentic-loop) trace lines to stderr for bench/CLI surfaces with no tracing subscriber.",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_CITATION_GROUNDING",
                // Default flipped ON 2026-06-24 (attach-mode QA bank: 7x
                // confab reduction); this entry lagged at "off" until the
                // 2026-07-30 registry sync.
                default: "on",
                purpose: "Active citation-grounding on entity-anchored fact queries: the model must copy a verbatim supporting sentence before answering, grounded by quote-existence (curing A3B context-under-utilisation + the substring verifier's title/paraphrase false-negatives). No findable quote → honest abstention.",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_SPECIFICS_SCAN",
                default: "on",
                purpose: "Long-form holistic specifics scan inside gate_longform: one judge pass (whole answer vs full evidence) catching fabricated supporting specifics / misattributions the per-claim audit misses. =0 disables (clean A/B lever).",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_SHORT_SPECIFICS_SCAN",
                default: "off",
                purpose: "SHELVED (default off; =1 enables). Short-path second-opinion specifics scan on RELEASED single-claim/citation answers: catches fabricated cited specifics (a named entity/flag/number absent from evidence) the value-only verify waves through, then correct-or-abstains via one grounded rewrite. Skips abstention-shaped answers. Dormant pending clean-evidence validation — its target category proved ~90% measurement artifact.",
            },
        ),
        (
            "-",
            EnvFlag {
                name: "SOVEREIGN_GATE_AUDIT_FORENSICS",
                default: "unset",
                purpose: "Diagnostic JSONL dump of the per-claim audit's own decisions: one `audit` record per pass carrying the evidence window, then one `claim` record per decision naming the mechanism that made it (per_claim_judge / batched / deterministic_veto / specifics_scan / identifier_sweep), its violation probability, and any claim-conditioned passages. The material required to validate the audit against its own evidence (ARCH §18.4). Default unset because the records carry verbatim corpus text.",
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden pin on every surface's verification budget. A change
    /// here must ship together with the bank run that justifies it.
    #[test]
    fn profile_defaults_are_pinned() {
        for s in [
            GateSurface::KnowledgeQuery,
            GateSurface::DeepQuery,
            GateSurface::AttachedDoc,
            GateSurface::SimpleQuery,
            GateSurface::Governance,
            GateSurface::ProxyArgument,
        ] {
            let p = s.profile();
            assert_eq!(p.max_claims, 4, "{}", s.id());
            assert!(p.retry, "{}", s.id());
            assert_eq!(p.longform_chars, 1_800, "{}", s.id());
        }
        let ct = GateSurface::ComplexTask.profile();
        assert_eq!(ct.longform_chars, 0, "complex_task is always per-claim");
        assert!(ct.retry);
        let rf = GateSurface::Refinement.profile();
        assert!(!rf.retry, "refinement is verify-only");
        assert_eq!(rf.longform_chars, 1_800);
    }

    /// τ default and env override flow through every profile.
    #[test]
    fn tau_defaults_to_calibrated_value() {
        if std::env::var("SOVEREIGN_GV_THRESHOLD").is_err() {
            assert!((GateSurface::KnowledgeQuery.profile().tau - 0.9).abs() < f64::EPSILON);
        }
    }

    /// Every flag this table declares must ALSO be declared in the
    /// workspace env-knob registry (`quality/env-flags.toml`) — the
    /// env-gate's map and this runtime-facing table must not drift.
    /// This table stays the runtime SSOT for the gate's knobs; the TOML
    /// is the workspace-wide census surface.
    #[test]
    fn flags_table_is_declared_in_env_registry() {
        let toml_text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../quality/env-flags.toml"
        ))
        .expect("quality/env-flags.toml readable from sovereign-core");
        for (_, f) in grounding_gate_flags() {
            assert!(
                toml_text.contains(&format!("name = \"{}\"", f.name)),
                "`{}` is in grounding_gate_flags() but not declared in \
                 quality/env-flags.toml — add a [[flag]] entry",
                f.name
            );
        }
    }

    /// The registry names every surface the override grammar accepts.
    #[test]
    fn flags_registry_covers_surface_overrides() {
        let flags = grounding_gate_flags();
        let overrides = flags
            .iter()
            .find(|(_, f)| f.name.contains("<SURFACE>"))
            .expect("per-surface override flag registered");
        for s in [
            GateSurface::KnowledgeQuery,
            GateSurface::DeepQuery,
            GateSurface::AttachedDoc,
            GateSurface::ComplexTask,
            GateSurface::SimpleQuery,
            GateSurface::Refinement,
            GateSurface::Governance,
            GateSurface::ProxyArgument,
        ] {
            let suffix = s
                .override_var()
                .strip_prefix("SOVEREIGN_GROUNDING_GATE_")
                .unwrap()
                .to_string();
            assert!(
                overrides.1.purpose.contains(&suffix),
                "override grammar must document {suffix}"
            );
        }
    }
}
