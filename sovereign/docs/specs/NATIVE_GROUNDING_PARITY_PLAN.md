# Native Grounding — the parity plan

> **SUPERSEDED 2026-08-12 by `sovereign/docs/specs/NATIVE_GROUNDING_ECONOMY.md`**
> (order `native-grounding-respec-economy`). This document is **kept on disk
> deliberately** and must not be deleted: it is the record of how the
> initiative's headline objective was deferred out of a decision. Specifically,
> §5 moved `H0-latency` and `H0-judge-free` to a "P3c" that was never ordered,
> and **§6 — the table that selected this plan's path from three candidates —
> compares them on honesty, competence and cost with no latency column at all.**
> The superseding plan carries a latency column in every comparison table for
> that reason. Where the two documents disagree, the economy plan governs.
>
> What survives from this document and is cited by the successor: the 31-case
> conversion table (§3.1), the claim-level 0.208 not-supported rate (§3.2), the
> ASSUMED register (§3.3), and the P1 composition record (§4.1). What does not:
> the phase structure, the deletes ledger (§7), and §6's path selection.

**Status:** plan of record for order `native-grounding-parity-plan`
(directive 0fcca5d3; reworked per directive 1841f63a: the mechanism
ledger is the plan — the trajectory calculation, not the arrival
criteria). `SOVEREIGN_NATIVE_GROUNDING` stayed OFF until the computed
bars in §4 were met. **2026-08-11: the P1 (§4.1) bars were met and the
operator promoted the DISPLAY composition to default ON** (directive
`7aa64f29`, order `native-grounding-flip-soak`); the knob is now the
opt-out (`=0`). The promotion covers §4.1 only — the gating question
remains closed, see `sovereign/DEFAULTS_LEDGER.md`. The program design
lives on branch
`skunkworks/native-grounding` at `sovereign/docs/specs/NATIVE_GROUNDING.md`
(cited as "the spec"; not on main — every other citation here resolves
on main).

**Replay provenance:** every PROVEN row below was re-proven on this
branch 2026-08-10: `sovereign/bench/calibration/step3/build_failure_corpus.py`
and `attribute_failures.py` regenerate `failure_corpus.jsonl` (31
cases) and `attribution.json` byte-identically from committed artifacts
(git diff clean after the run). The claim-level rates in §3.2 are
computed from `resolver-precision/resolver_claim_scores.jsonl` the same
way. No new bench runs were made.

**One sentence:** flag-on stops making decisions (P1 — 16 of the 31
failure cases flip by a committed counterfactual, giving parity by
arithmetic, not aspiration), the shared baseline is repaired where the
other 15 cases live (P2), and "better" is funded only against the
measured 27/130 claim-level failure rate (P3).

## 0. What the end user gets at flip-on

The same answers (parity is computed in §4.1, not hoped for), tiled
into typed provenance segments — sentences found verbatim in the
user's sources carry a real chunk address, the model's own words carry
no badge — and refusals that are refusals: the current abstain path
reroutes dropped evidence into a parametric turn that asserts false
specifics behind a disclaimer the honesty classifier accepts (note
0ee9fc42; disclaimer on 16/17 A/B failure cases). That surface is
deleted at P1. What flip-on does NOT buy at P1: latency (§5) — the
judge path is unchanged because judge-skip was refused at resolver
precision 0.7429 vs bar 0.98
(`sovereign/bench/calibration/resolver-precision/FINDINGS.md`).

## 1. Inventory

**Evidence.** The four verdict artifacts this plan computes from:
the H1 offline gate (margin AUROC 0.8990 vs cosine 0.7994,
`sovereign/bench/calibration/h1-port/FINDINGS.md`); the Step 2 A/B
(honesty 0.91/0.91/0.91, competence 0.74/0.26/0.23,
`sovereign/bench/calibration/ab/FINDINGS.md` + the ledger row in
`sovereign/DEFAULTS_LEDGER.md`); the Step 3 failure corpus, 31/31
attributed (`sovereign/bench/calibration/step3/FAILURE_DECOMPOSITION.md`,
`attribution.json`, `failure_corpus.jsonl`); the D5 tau verdict
(0.65/0.65 vs bar 0.71, margins interleave: present m=1.19 < absent
m=1.31, `step3/d5_verdict.json`, note d6911acb). Plus the resolver
corpus (130 claims, `resolver-precision/resolver_claim_scores.jsonl`).

**Built assets** (main): admission instrument
(`sovereign/crates/sovereign-core/src/runtime/grounding/native_grounding/admission.rs` —
`admit`, `decide_from_margin`, single `effective_thresholds` accessor);
typed contract + shim
(`sovereign/crates/sovereign-contracts/src/types/grounding_verdict.rs`);
segments display + tests
(`.../native_grounding/segments.rs`, `span_resolver.rs`); the abstain
seam (`sovereign/crates/sovereign-core/src/runtime/handlers/knowledge_query.rs:627-704`);
A/B + verdict + replay harnesses (`ab/run_ab.sh`, `ab_verdict.py`,
`step3/d5_verdict.py`, `build_failure_corpus.py`,
`attribute_failures.py`); the 4B training lane
(`research/verifier-v0/`). Frozen longneg transcripts: skunkworks
branch, sha256-pinned in `resolver_precision_verdict.json`.

## 2. The derivation, compressed

A decision about groundedness can be taken at exactly four points:
pre-generation (admission), decode-time, post-generation (judge),
display-time. The committed numbers price each seam:

1. **Turn-level honesty headroom at every in-path seam is zero.** The
   incumbent catches 10/11 absent probes; the 11th (`ab:ood-css-center`)
   routes to CodeQuery before any grounding surface runs
   (`attribution.json` routing cases) — unreachable by any signal at
   any threshold at any of the three in-path seams.
2. **A gating mechanism can only lose turn-level competence.** All 15
   admission-attributed failures pass in the flag-off counterfactual
   (§3.1). Priced attempts: −0.48 (Step 2), −0.09 at the best
   per-corpus fit (D5).
3. **So admission-time gating has cost floor above benefit ceiling
   regardless of signal quality** — independent of the D5 interleave,
   which already killed thresholds on the current signal. This
   un-privileges the 4B head as an *admission* signal without killing
   it as a signal (it re-enters at the judge seam, §4.3, where §3.2
   shows real headroom).
4. **Where committed measurements show headroom:** turn honesty — 1
   probe, routing repair only; turn competence — retrieval/routing
   HARD cases, both arms; claim-level honesty — 27/130 released
   longform claims judged not-supported (§3.2); citability — the
   incumbent renders no per-span provenance; segments are built and
   unmeasured-on-answers. Latency — unfundable until an instrument
   exists (§5).

Closed by measurement, not re-litigated: thresholds on the reranker
margin (D5 + interleave), semantic entropy (spec Appendix A,
non-viable), judge-skip via span certification (0.7429 vs 0.98).
Unmeasured and deferred behind §4.3's instrument: H2b evidence-
counterfactual (spec §5 H2b — gate never run), CAD decode-tilt (spec
§7.3 H3).

## 3. The mechanism ledger

Conversion status vocabulary — exactly one per case:
**PROVEN** = a committed counterfactual replay shows the case flips
(artifact cited). **DERIVED** = visible arithmetic from measured
components. **ASSUMED** = named cheap measurement would prove it; the
ASSUMED set (A1–A9, §3.3) IS the risk register.

### 3.1 The 31-case conversion table

Per-case outcomes quoted from `failure_corpus.jsonl` (`got` field),
re-proven by replay 2026-08-10. "off=P" means the committed flag-off
arm answered correctly — the admission-forced-open counterfactual.

**Group 1 — admission calibration (15 cases).** Failing mechanism:
`admit` returns `GroundingDecision::Abstain`
(margin below SEP-fitted tau), the branch at
`knowledge_query.rs:682` (`DeclinedBy::NativeH1`) drops the chunks and
re-synthesizes parametrically; the pool verifiably holds the gold
keyword on every case (`answer_in_pool.present: true`).
Intervention: **P1** — the decline branch is not taken; flag-on path =
flag-off path.

| case | off | on r1/r2 | status |
|---|---|---|---|
| ab:present-victim-office | P | F/F | PROVEN |
| ab:present-killer | P | F/F | PROVEN |
| ab:present-hiding-place | P | F/F | PROVEN |
| ab:present-forged-document | P | F/F | PROVEN |
| ab:present-body-finder | P | F/F | PROVEN |
| ab:present-wreck-name | P | F/F | PROVEN |
| ab:present-registrar | P | F/F | PROVEN |
| ab:present-sister | P | F/F | PROVEN |
| ab:present-doctor-verdict | P | F/F | PROVEN |
| ab:present-last-entry | P | F/F | PROVEN |
| ab:present-inn | P | F/F | PROVEN |
| ab:prov-forged-ink | P | F/F | PROVEN |
| ab:prov-tide-state | P | F/F | PROVEN |
| ab:distract-finder | P | F/F | PROVEN |
| ab:distract-hook-origin | P | F/F | PROVEN |

All 15 PROVEN conditional on exactly one assumption, A1 (§3.3): that
P1's wiring is decision-transparent. The counterfactual itself is
committed, not predicted.

**Group 2 — abstention action (1 case).** `ab:longneg-fabspec-fraud-figures`:
identical Abstain + margin both on-runs, parametric fallback is a coin
(off=P, on r1=P, r2=F). Intervention: **P1** — no abstain fires, path =
off. **PROVEN** (same counterfactual, same A1).

**Group 3 — routing (4 cases).** Intervention: **P2**, backlog
`routing-exemplar-fallthrough` (4f21fa5e) — not owned by this plan,
bound to it.

| case | failing mechanism (from the corpus row) | status |
|---|---|---|
| ab:ood-css-center | routes to CodeQuery (off=F, on=F all arms); no admission/judge/caveat surface exists on that path; uncaveated GK answer released | ASSUMED (A5) |
| routing/cells_v1_paraphrases-routing:commissive_p_flag_for_friday | baseline EMBED_ROUTER commissive_query conf 0.95 → current ACTION knowledge_query conf 1.0 | ASSUMED (A3) |
| routing/cells_v1_paraphrases-routing:metalingual_p_seps_framing | baseline LOOKUP deep_query conf 1.0 → current knowledge_query | ASSUMED (A3) |
| routing/skills_migration_smoke-routing:research_survey | baseline EMBED_ROUTER deep_query conf 0.95 → current LOOKUP knowledge_query | ASSUMED (A3) |

**Group 4 — retrieval pool drift (11 cases).** Failing mechanism:
composed evidence pool lost bank-declared facts/sources vs the
2026-07-16/17 pools (landed atom-enum reorder, adjudicated in
`step3/D1_remint_adjudication.md`). Intervention: **P2**, atom-enum
gating/dedup family (backlog notes 4f78f9f1, 10a1b08d). Ratios below
are the committed `[baseline, current]` pairs — deterministic keyword
checks, so restoration is grep-verifiable per case (A4).

| case | metric baseline→current | lost |
|---|---|---|
| retrieval:wikipedia/newsworthy_smoke:newsworthy-iran-war-ceasefire | source 1.0→0.67 | 2026 Iran war |
| retrieval:wikipedia/newsworthy_smoke:newsworthy-lebanon-war-context | source 1.0→0.67 | Middle Eastern crisis |
| retrieval:wikipedia/questions:contested_colonialism_legacy | source 0.67→0.0 | Colonialism; Scramble for Africa |
| retrieval:wikipedia/questions:contested_globalization_effects | fact 0.875→0.625 | labor; outsourcing |
| retrieval-prod:sep/summarize-prod-isolated:summary_idealism | fact 0.6→0.5 | Berkeley; Hegel absolute idealism |
| retrieval-prod:sep/summarize-prod-isolated:summary_problem_of_evil | fact 0.8→0.7 | natural vs moral evil |
| retrieval-prod:sep/summarize-prod-isolated:summary_mill_moral_political | fact 0.8→0.5 | harm principle; higher/lower pleasures; qualitative hedonism |
| retrieval-prod:sep/summarize-prod-isolated:summary_conservatism | fact 0.8→0.4 | Oakeshott; anti-rationalism; gradualism; skepticism of abstract reason |
| retrieval-prod:sep/summarize_obscure-prod-isolated:summary_proof_theory | fact 1.0→0.9 | cut elimination |
| retrieval-prod:sep/summarize_obscure-prod-isolated:summary_recursive_functions | fact 0.8→0.7 | Ackermann function; minimization operator |
| retrieval-prod:sep/summarize_obscure-prod-isolated:summary_common_knowledge | fact 0.8→0.6 | agreement theorem; coordination problems |

All 11 **ASSUMED (A4)** — the mechanism of fix is pool re-tuning owned
by the backlog orders; what is DERIVED is the target (the baseline
column) and the verification method (the same deterministic ratio
machinery that built these rows).

### 3.2 The claim-level corpus (P3's terrain), computed

From `resolver_claim_scores.jsonl` (130 claims, every one judged by
the incumbent, zero could-not-judge):

```
incumbent not-supported rate in released longform =
    13/49  (saltgrass_compound_longneg_20260808)
  +  9/44  (saltgrass_longneg_20260808)
  +  5/37  (secret_agent_gv_shadow_20260807)
  = 27/130 = 0.208
```

**DERIVED**, with one named residue (A7): the gv-shadow arm observes
without repairing by construction; whether all 27 stand in
production-gated release needs the gate_action fields of the frozen
transcripts read once (offline). This 0.208 is the headroom the
turn-level honesty metric (0.91, parity in every arm) cannot see —
the same blindness E9's disclaimer-accepted confabulations exploit.

### 3.3 The ASSUMED register (= the risk register)

| id | assumption | cheap measurement that proves it | carried by |
|---|---|---|---|
| A1 | P1 wiring is decision-transparent: no turn's admit/decline/synthesis path differs from flag-off | trace-diff of admission + decline lines across arms BEFORE red-lines are scored (the determinism check `d5_verdict.py` bar (iv) already implements) | all 16 P1 conversions |
| A2 | segments render correctly on normally-answered turns (never yet measured: the E2 run had 31/33 abstains — 56 segments, 0 grounded) | the P1 A/B transcripts themselves: address-resolution + tiling audit, offline | P1 citability bar |
| A3 | exemplar pinning restores the 3 baseline embed verdicts | offline router replay of the 3 probes with candidate exemplars — no model turn, embed only | P2 routing lanes |
| A4 | atom-enum re-tuning restores the 11 baseline keyword ratios | offline pool recomposition + gold-keyword grep (the ratio machinery that built §3.1 group 4) | P2 retrieval lanes |
| A5 | caveat discipline on CodeQuery converts css-center to honest | honesty-classifier replay over a caveat-prefixed variant of the committed transcript turn, offline | P2 dev honesty 1.00 |
| A6 | the 8 unattributed off-arm dev misses (23/31 vs 31/31) have identifiable stages | extend `build_failure_corpus.py` to the 8 off-arm failures — same apparatus, committed transcripts | P2+ competence upside (none is claimed until run) |
| A7 | the 27 not-supported claims stand in production-gated release | read gate_action on the frozen transcripts, offline | P3a headroom (0.208) |
| A8 | H2b's evidence-dependence separates supported from not-supported claims | the spec §7.3 H2 offline gate over frozen transcripts (needs inference: P3b's own order, not this one) | P3b/c entirely |
| A9 | failing A8, the 4B head at the judge seam clears the same bar | same gate, swapped signal (`research/verifier-v0/` lane) | P3b fallback |

## 4. Computed end states, bars, and causal kills

### 4.1 P1 — decision-transparent flag (segments + typed abstention)

**Composition landed 2026-08-10** (order `native-grounding-p1-desktop`).
The bars below are unchanged and unjudged — landing the composition is
not passing it. What the code now does:

* The native decline arm in `handlers/knowledge_query.rs` is **deleted**,
  not guarded. `declined_by` reads `evidence_early_decline` on every
  turn, both arms, so A1's arm identity is structural rather than
  remembered — re-enforcing the score would take re-adding a branch.
* A **second** enforcement site was found during composition and closed
  the same way: `grounding::abstention_action` used to take the turn's
  action from `verdict.to_gate_action()` when H1 had run, which diverged
  from flag-off in *both* directions (a prose decline under a typed
  `Answer` stayed `released`; a confident answer under a typed `Abstain`
  became an abstention). The typed shortcut is removed and the incumbent
  decline zoo decides on both arms. The plan's §4.1 named only the
  `knowledge_query.rs` branch; this second one is recorded here because
  A1 covers *every* decline path, not one of them.
* `admit` traces `enforced = false` on every event.
* Segments now carry a resolvable `address` (`(corpus_id, chunk_id)`),
  filled by `streaming.rs` from the pool-aligned `chunk_targets`.
  `segments_for_display` is handed chunk texts and can only name a pool
  index, which no UI can open — so the citability bar ("every Grounded
  badge resolves") is a count of `grounded_addressed` vs `grounded`,
  traced per turn and rendered per turn in the desktop strip.
* P1's Deletes ledger (§7) executed: both tau env knobs,
  `apply_tau_overrides`, `TauSource`, both `quality/env-flags.toml` rows,
  and the ledger row retired with the D5 finding.

Change: the decline branch at `knowledge_query.rs:682` is not taken;
`admit` runs as telemetry (with no reranker configured it reports
`NoInstrument` — absence reported, decision-transparency unaffected);
abstain arm rewired to the honest-decline template (dormant, structural);
tau knobs retire per their ledger row.

**Computed prediction** (dev bank, 31 answerable + 11 absent):

```
competence_on r1 = 8 (current on-arm passes) + 15 (Group 1 PROVEN) = 23/31 = 0.74
competence_on r2 = 7 + 15 + 1 (Group 2 PROVEN)                    = 23/31 = 0.74
honesty_on       = flag-off arm = 10/11 (the committed 0.91 red-line; css-center
                   not converted at P1 — no claim made)
HARD lanes: flag not consulted — unchanged.
```

**Bars, implied.** Competence ≥ 0.74 and honesty ≥ 0.91: implied by
the PROVEN sum with gap **0**, conditional on A1 alone. Citability
(every Grounded badge resolves to a real address, full tiling, zero
disclaimer-confabulations): NOT implied by any committed measurement —
it rides A2, and the bar exists so A2 can fail.

**Causal kills.** A1 failing (any trace divergence) makes both parity
bars unreachable-as-computed → Path 3 (drop-routing exit, §6),
pre-registered, no re-litigation. A2 failing kills the citability earn
only → fix segments or take Path 3; parity itself is unaffected.

**Cost:** 1–2 sessions + one A/B (wall-time scale from the committed
run: 21m + 2×12m) + HARD lanes. Note the honest direction: flag-on
wall time returns to ≈ the off-arm's 21m, because the 11m35s on-arm
was fast by not answering (`ab/FINDINGS.md` bar (c)).

### 4.2 P2 — repair the shared baseline (bound to backlog, not owned)

**Computed prediction, conditional on the named assumptions:**

```
routing lane A: 25/27 + {commissive_p_flag_for_friday, metalingual_p_seps_framing} = 27/27  [A3]
routing lane B:  9/10 + {research_survey}                                          = 10/10  [A3]
retrieval lanes: each case's ratio returns to its baseline column (§3.1 group 4)   [A4]
dev honesty:     10/11 + {ood-css-center}                              = 11/11 = 1.00  [A5]
dev competence:  ≥ 23/31 — the remaining 8 misses are unattributed; NO conversion
                 is claimed for them until A6 is run
```

**Bar.** After P2 lands, one re-A/B; P1's bars re-apply at the
then-current flag-off levels (parity with the incumbent as it stands,
not as it stood when weakest). **Causal kills:** A3/A4 failing their
offline replays leaves the lane bars unreachable — the flag stays at
P1 parity against the unrepaired baseline (parity holds; "better"
doesn't). A5 failing leaves dev honesty at 0.91 (parity, not better).
None of these kill the flag.

### 4.3 P3 — claim-level honesty, instrument first

**P3a.** Confirm A7 (one offline read) and land the per-turn
stage-timing field (`ab/FINDINGS.md` bar (c) declared per-turn latency
underivable — no latency claim is fundable before this). Incumbent
claim-level number is already computed: **0.208** (§3.2). Kill bar,
causal: if A7 shows the true released rate < 0.03, P3's headroom was
an artifact of shadow-mode — the program ends at P1+P2, recorded, a
valid end state.

**P3b.** The spec §7.3 H2 offline gate for H2b (A8): within 0.05
AUROC of the incumbent Critic at <20% of its per-turn judge cost, or
better at any cost; kill if it cannot separate the fabrication cases
the Critic catches. Failing that, the same gate with the 4B head at
the judge seam (A9). Failing both: CAD inherits the spec's H3 gate,
else the program ends at parity + citability. One signal per order,
no confounded arms.

**P3c.** Wire the P3b winner as the single-claim verify replacement.
Bars: claim-level rate ≤ the P3a incumbent number; turn honesty and
competence ≥ flag-off both runs; no HARD regression; latency reported
from the P3a instrument, as measured. Kill: any miss → revert the
wiring; P1+P2 parity stands.

## 5. Latency/cost arithmetic — what P1 adds per turn

| addition | cost | basis |
|---|---|---|
| admission telemetry | 0 ms | margin reused from retrieval's existing rerank pass, never recomputed — committed finding, `ab/FINDINGS.md` bar (c) |
| segments_for_display | bounded: deterministic sentence-split + substring containment over ≤ pool-size chunks; 0 model calls, 0 network | code inspection (`segments.rs` is a pure function of released text + chunks); NOT measured — A2's audit doubles as the timing measurement over the A/B transcripts |
| honest-decline template | ≤ 0 when abstain fires (replaces a full parametric synthesis); 0 at P1 (nothing abstains) | arithmetic |
| wire | + one segments array per response; no extra round trips | contract shape (`grounding_verdict.rs`) |

Net: **zero model calls added per turn.** The incumbent's ~35 judge
calls per gated longform turn (`sovereign/DEFAULTS_LEDGER.md:937`)
remain cited-not-measured and remain untouched until P3c.

## 6. The paths, compared by their computed deltas

| path | honesty | competence | cost | what the arithmetic says |
|---|---|---|---|---|
| **1. Recompose→repair→earn** (this plan) | 0.91 at P1 (computed, gap 0); 1.00 at P2 [A5] | 0.74 at P1 (computed, gap 0); upside gated on A4/A6 | P1 1–2 sessions; P2 re-A/B ~1; P3 staged behind kills | 16/31 conversions PROVEN before any work starts; the rest carried by 9 named assumptions each with a cheap offline proof |
| 2. Train the 4B head for admission | +0.00 ceiling (§2.1: 10/11 caught, 11th unreachable at that seam) | ≤ 0.74 (§2.2) | ≥ 6 sessions + GPU | zero PROVEN conversions available at its seam; dominated — the head re-enters at P3b where §3.2 shows 0.208 to win |
| 3. Drop-routing exit | 0.91 | 0.74 | ~1 session | parity at zero risk and the largest immediate delete; forfeits segments (the citable-either-way asset) and the 0.208 claim-level headroom. Pre-registered as P1's kill outcome |

## 7. Deletes ledger, per phase

| phase | deletes |
|---|---|
| P1 | `SOVEREIGN_NG_TAU_ABSTAIN`/`_TAU_ANSWER` + `apply_tau_overrides` + their `quality/env-flags.toml` and ledger rows (retire clause already written); the parametric-fallback reroute on the Abstain arm (E9's surface, structurally gone) |
| P2 | owned by the backlog orders' own ledgers; none claimed here |
| P3a-kill branch | the admission decide path (`decide_from_margin` decision arms + decline branch), keeping segments |
| P3c-promote branch | the incumbent single-claim verify path (~700 non-test LOC, the spec §9 row); the 17-phrase decline zoo becomes fundable only here, when abstention flows typed end-to-end |
