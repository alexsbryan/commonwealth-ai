# Calibrated typed decisions — research plan and pre-registration

Written 2026-09-22, before any result is collected, against `main` @ 592e40eee.
The bars below are fixed when this file is committed. Nothing is edited after
the first result lands; changes append under a dated heading.
Discipline inherited from `VERIFIER_ECONOMY_PREREG_20260819.md` §"Shared discipline".

## Why this, and why now

Two outside projects prompted this. TypeSafe's Jev (proprietary, cloud-only,
released 2026-09-15) is a "decision model": it answers typed questions about a
state — yes/no, pick-one-of-N, rate-on-ordered-levels — with a probability
attached, instead of generating text. NVIDIA's SoL-Pi (arXiv 2609.20519) is a
set of harness efficiency mechanisms found by an auto-research loop. We cannot
run Jev here without sending state off the machine, and its speed and cost
figures are self-reported. What transfers is the interface, and the interface
exposes a gap we already have.

The grounding side of this system makes calibrated decisions. `forced_choice_ab`
(`sovereign-core/src/runtime/grounding/judge.rs:143`) gathers the raw logits of
single-token candidates at the next-token position and returns a distribution,
and `cargo xtask judge-funnel-gate` keeps it the only place such a request is
built. The routing side does not. Every routing decision returns a label and
nothing else:

- self-assessment on the SIMPLE branch (`router.rs:1336`), a three-way
  Confident / Uncertain / NeedsWebSearch whose `_confidence` argument is
  vestigial (`router.rs:1316`);
- the crisis classifier (`runtime/wellbeing.rs:177`), which returns
  `Option<bool>`;
- Pass-1 intent (`router.rs:1163`), where a confidence field was dropped
  because it "has nowhere to land".

Self-assessment is the decision that lets the system answer from weights, and
answering from weights when it should not have is the front door to
fabrication. It is also the only one of these that has no instrument at all:
the routing banks score 96/96 and were authored alongside the router
(`routing/calibration/axes_v1.toml` header), `routing/baselines/calibration-fit`
fits the embedding router's thresholds, not this call, and no bank labels
whether a direct answer was right.

So this effort asks one question per track: does replacing a label with a
distribution, and a fixed decision rule on it, make the decision measurably
better on the axis that matters without paying on the other axis?

## Two facts that shape the design

**The engine already does N-way.** `forced_choice_probs`
(`sovereign-inference/src/embedded/model_slot.rs:706`) takes any candidate
list; the `rpc_distribution.rs:2805` test uses A/B/C. Only the core wrapper is
two-way. But a candidate that does not encode to a single token is dropped
without error (`model_slot.rs:716`), so labels must be letters, not
`CONFIDENT`/`UNCERTAIN`/`WEB`.

**Distributions only come from the primary tier.** The forced-choice sentinel
is routed to primary by design (SLOT_POLICY §6; test at
`embedded/grammar.rs:428`), because the fast slot returns a sampled token
instead of a distribution — the failure that pinned evidence-loop recall at
2/8 (`runtime/evidence_loop/mod.rs:510`). Both calls this plan touches run on
the fast slot today. Moving them costs one primary-tier prefill per decision,
plus a cold load if the primary is not resident. This cost is unmeasured and
is a bar below, not an assumption.

## Shared enabler — the N-way funnel

`forced_choice(inference, system, prompt, labels, stable_prefix_len, routing,
call) -> Option<Vec<(label, p)>>` beside the existing function, with
`forced_choice_ab` reduced to a call into it. One request builder, so the
judge-funnel gate still counts one. If routing calling into
`runtime::grounding` reads wrong, the funnel moves to a neutral module — a
move, not a copy (ARCH 8). New `JudgeCall` variants name the two new askers so
`call_census` prices them. Every call traces the full distribution and the
chosen label at `debug` on a captured target (ARCH 1). The whole path sits
behind one env flag, default off, declared in `quality/env-flags.toml`, with a
`DEFAULTS_LEDGER.md` row in the same commit.

The decision rule is fixed per track below. Neither track may tune the prompt
and the threshold on the same split.

## Instrument validation — precedes any result (ARCH 7)

Three checks, each must pass before any track result is read.

1. **Determined items.** 30 prompts whose correct letter is fixed by
   construction ("the answer is C; reply with the letter"), rotated across
   A/B/C. Pass: every item puts p ≥ 0.9 on the correct letter.
2. **Position bias.** Every track item is scored under all three letter
   rotations. Report the mean |Δp| for the same option across rotations. If it
   exceeds 0.10, the track averages over rotations (3× cost, recorded in the
   latency bar) or the track stops; it does not proceed on a single rotation.
3. **Watched to fail.** The funnel run once with the candidate labels
   deliberately made multi-token (`CONFIDENT`/`UNCERTAIN`/`WEB`) must return
   `None`, not a distribution, and the run's log is committed beside the
   baseline. A check whose failure we have not seen is not a check (ARCH 5).

## Track A — self-assessment on the SIMPLE branch

**Ground truth by outcome, not by rater.** An item is labelled
*weights-sufficient* when the direct answer — the model that serves
`Intent::SimpleQuery`, no retrieval — contains the item's `expected_facts`
under the existing bench scorer, and *needs-sources* when it does not. Items
needing current information are labelled *needs-web* by construction. This is
the point Jev's vendor makes about training against outcomes rather than
preference, applied to measurement instead of training.

**Bank.** The three existing question banks give 53 items
(`sep/questions.toml` 21, `wikipedia/questions.toml` 20,
`conversation/questions.toml` 12). That is too few: a Wilson interval at
n=53 near 50% is about ±13 points. Target n ≥ 240, the new items written against question shapes
(rosters, single dates, definitional, current-events) rather than against the
self-assessment prompt, per `feedback_no_teaching_to_test`. Split 80 fit / 160
held-out, frozen before the held-out run, and the held-out result never feeds
back.

**Baseline.** The current label-only call on the same bank, three runs,
because the fast slot is not deterministic even at temperature 0
(`effort_classifier.rs` header).

**Treatment.** A/B/C forced choice on the primary tier with the existing
prompt, letters substituted. Decision rule: *Confident iff
p(A) ≥ τ, else the larger of Uncertain and Web.* τ is fitted on the fit split
to minimise bad-confident rate subject to the timidity bar, then frozen.

**Two axes, never averaged.**

- *Bad-confident rate*: Confident on an item labelled needs-sources or
  needs-web. This is the fabrication axis.
- *Timidity*: escalated on an item labelled weights-sufficient. Bad-confident
  alone is satisfied by escalating everything.

**Bars (held-out split, Wilson 95%).**

| Bar | Pass |
|---|---|
| Bad-confident rate | Treatment interval disjoint from baseline, lower |
| Timidity | Treatment not worse with disjoint intervals |
| Routing regression | `cells_v1` 27/27 and `cells_v1_paraphrases` 27/27, unchanged |
| Latency | p95 of the self-assessment step rises by ≤ 250 ms over baseline on the resident Halo stack, primary already loaded; cold-load cost reported separately, not gated |
| Calibration | Reliability curve and Brier score reported; not gated, since there is no prior curve to beat |

**Verdicts.** Passed, failed, could-not-judge (primary tier unavailable, or a
scorer error on an item — its own column, never a pass), never-ran (a label
class with no banked item). A fail on bad-confident or timidity is a kill;
a fail on latency alone sends Track A to the fallback below, not to a
threshold hunt.

**Fallback if latency kills it.** An embedding-centroid self-assessment, the
move `effort_classifier.rs` already made for a nondeterministic fast-slot
verdict, scored against the same bank and bars. We do not try to coax
distributions out of the fast slot; that path is measured dead.

## Track B — the crisis classifier

This track is second because the decision is rarer but each miss is expensive
and the costs are asymmetric, which is where a probability earns its keep: a
threshold can be placed on purpose instead of wherever a boolean falls. It
runs only on Relational or inner-work turns (`runtime/turn.rs:321`), after the
sticky and lexicon layers, and a `None` already fails over to those layers.
That fail-open stays.

**Bank.** I found no message-level labelled bank for this classifier;
`inner_work/calibration.toml` labels responses, not messages. One is authored:
at least 60 implied-ideation positives and 120 hard negatives (sadness,
numbness and loneliness in vivid metaphor, third-person references to another
person's crisis). No item may reuse a phrase quoted in the classifier prompt
(`wellbeing.rs:207-225`) — those phrases are the prompt's examples, and scoring
on them measures memorisation. Because this is a safety surface, the operator
reviews the bank before the first run.

**Baseline.** Current fast-slot boolean, three runs.

**Treatment.** Two-way forced choice, primary tier. Decision rule: *crisis iff
p(crisis) ≥ τ*, τ fitted on a 60-item fit split for the lowest false-positive
rate at which fit-split recall is at least baseline recall, then frozen.

**Bars (held-out).**

| Bar | Pass |
|---|---|
| Recall on positives | Treatment lower bound ≥ baseline point estimate. A recall drop is a kill regardless of every other number |
| False positives on hard negatives | Not worse with disjoint intervals |
| Latency | Same ≤ 250 ms p95 bar, measured on Relational turns |

**Jev is excluded from this track structurally.** Inner-work sessions run
`LocalOnly` (`router.rs:1128` comment), so no external arm is admissible
whatever it would show.

## Not in this effort

Pass-1 intent margins (`router.rs:1163`) are the natural third user of the
funnel, but they run on every turn, so they inherit whatever latency Track A
measures. They get their own pre-registration only if Track A passes the
latency bar.

An external Jev comparison arm for Track A is possible and is the operator's
call, because it sends question text to a third party.

SoL-Pi's transferable piece, the evidence-preserving reducer for build, lint and
test output (against the byte caps in `code/build.rs:47` and
`atos_utils.rs:328`), is the next effort. It gets its own document once this
one has a verdict. Online Context Compact is not pursued: it carries all of the
paper's capability loss (−6.4% score on EdgeBench) and the split
protocol already covers the need.

## Open for the operator before the first run

1. The 250 ms latency bar is a proposal. Change it before data exists or not
   at all.
2. Who authors or reviews the Track B bank.
3. Whether a Jev arm is wanted for Track A.

## If a track is killed

A `DEFAULTS_LEDGER.md` REJECTED row naming the measurement and the bar it
failed, the flag stays default-off, and the verdict data (per-item
distributions, labels, latencies) is committed beside this file. No prose
report.

## 2026-09-23 — Also considered

Appended before any result. None of the bars above change. Open question 3
(a Jev arm) is closed as no, by the first entry below.

**Jev as a model or as a comparison arm.** Rejected. The approach transfers
and the product does not. Typed questions over a state, a small closed set of
answer types, a probability per answer, and ground truth by outcome are
already here: the forced-choice funnel, llguidance, and `stable_prefix_len`
for several questions over one state. Jev's remaining advantages are a model
trained for calibration and speed. The first is approximated locally by
post-hoc calibration (temperature or Platt scaling fitted on this document's
fit splits, against our own outcomes). The second only matters on hot paths,
which the latency bar and the embedding fallback already cover. A comparison
arm would buy an external reference point at the cost of a third-party
dependency, text leaving the machine, and self-reported numbers. The gold
sets this plan builds are the reference.

**Router tool pick (`router.rs:1200`).** Not pursued. The label set is the
registered tool ids, which are open and multi-token, and a wrong pick is
recoverable within the turn. Low value per error.

**Enum fields inside Phase 1 extraction** (`entity_type`, `claim_kind`,
stance, attribute enums; `pipelines/ontology_schema.rs`,
`typed_schemas/argumentative.rs`). Not pursued. These values are chosen in the
middle of generating a JSON object, and forced choice reads one next token at
the end of a prompt. Distributions there need per-field logit capture during
constrained decoding, which is new engine work. Revisit only if the tracks
here show that probabilities pay.

**Phase 6 tension / same-as classifier**
(`corpus-engine/src/enrichment/atlas/analysis/tension_classifier.rs`).
The strongest ingest candidate, not yet a track. One call per candidate pair.
Today it returns `is_tension` plus a `relation` enum and a confidence the model
writes itself, defaulting to 0.7 when omitted (`:174`), and the confidence
only sorts (`governance_view.rs:622`). A single four-way choice
(Tension / SameAs / Compatible / Neither) would give one distribution and
remove the self-contradiction `verdict()` has to reconcile (`:163`). A
probability would allow three outcomes: merge, review queue, or drop. The
tension half has a gold set (governance, maple-house); the same-as half does
not. It becomes Track C by its own appended pre-registration, if the operator
wants it.

**Phase 0 section classifier** (`enrichment/pipeline/section_classifier.rs`).
Deferred. The label chooses the Phase 1 schema, so an error fails extraction
outright, and the v2 axes' `primary_weight` is a distribution the model
currently writes instead of one that is measured. Cost is low because results
are cached by content hash and the axes can share a prefix. Blocked on
measurement: no gold set of section labels exists.

**Meta-atlas bridge adjudicator** (`meta_atlas/bridge/adjudicate.rs:88`).
Deferred. Five labels, reached only by pairs a deterministic signal stack left
uncertain, so it is shaped for a real probability. The meta-atlas work is
parked.

**Entity-merge judge.** Not built. `MergeSignal::JudgeConfirmed` is declared
(`reconciliation/signals.rs:37`) and nothing in the enrichment path produces
it. If it is built, it is built as a yes/no on the funnel, not retrofitted.

**GLiNER label sets** (`sovereign-gliner/src/gliner_ner.rs:51`). Out of scope.
GLiNER already emits a per-span score. Its gap is different: the label sets
are hardcoded and the ontology does not feed them.

**SoL-Pi as installed in pi.** Deferred. Nearly free to try, but pi is not
installed on this host, Node here is v20 against the required 22.19, and pi is
unpinned while SoL-Pi pins 0.85.1. It would also only improve pi sessions.
The paper's own ablation favours ObservationPack (+5.3% score, −6% tokens) and
Action Fusion (+4.1%, −12%). Online Context Compact carries all of the
stack's capability loss (−6.4%).

**SoL-Pi Action Fusion in `write_file`** (`sovereign-agent-tools/src/primitive.rs:183`).
Not pursued now. Fusing a cargo build into a write conflicts with the
one-cargo-at-a-time rule and `with-cargo-lock.sh`, and the agent-coding
battery (8 problems) is too small to see a 4% effect.

**SoL-Pi ObservationPack in `tool_result_cache`.** Not pursued. The cache
already expires results after five turns, so the saving is smaller here than
in pi, and handles give up prompt-cache prefix reuse.

**SoL-Pi Evidence-Preserving Reducer** for build, lint and test output. The
next effort after this one, as stated above.
