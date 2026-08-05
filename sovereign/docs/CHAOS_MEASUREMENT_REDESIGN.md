# Chaos-bench measurement redesign — trustworthy, attributed, deterministic

Status: **design agreed, not yet built** (2026-06-18). This document is the spec
for the next implementation pass on the grounded-or-abstain calibration bench
(`sovereign bench chaos-monkey`). It supersedes nothing yet — it describes the
target state.

## Why

A long competence/honesty session on `chaos-secret-agent` kept hitting the same
wall: *the mechanism worked but the measurement couldn't see it.* The scorer is
the bottleneck, not the gate. Four concrete defects, each observed in a real run
(see the `project_grounding_competence_ceiling` memory):

1. **The action signal is re-derived, not read.** The production grounding gate
   runs *in-process* during the bench, decides release-vs-abstain, and persists
   that decision to the assistant message metadata as `grounding_gate.action`
   (`streaming.rs:1048`). But `run_live` drops it, and `score_question`
   (`chaos_monkey.rs:589`) instead asks an LLM judge (`classify_extraction`)
   "did a reader come away with an answer?" against the visible text. That judge
   is non-deterministic and has a documented residual (a verbose correct
   abstention reads as an answer). We are guessing at a fact we already hold.

2. **Correctness is brittle substring-on-gold.** `gold_match`
   (`det_checks.rs:19`) is an AND of case-insensitive substrings. `cabhorse`
   quoted "steed" (correct) against gold `["horse"]` → scored wrong; `wife`
   answered "Mrs Verloc" (correct) against gold `["Winnie"]` → scored wrong.
   Synonyms and alternate-correct forms read as failures.

3. **Blatant-confab false-positives on essays.** The value-extractor behind
   `blatant_confab_rate` pulls framing/meta off discursive answers ("The Secret
   Agent by Joseph Conrad", "I need to be honest…") and flags them. The
   production gate already refuses this — `verify_grounding` bails at >1800 chars
   as "out of gate scope" (`live_runner.rs:500`) — but the scorer's
   value-presence call does not mirror that scoping.

4. **Non-determinism swamps small signal.** ~3/13 probes flip run-to-run at
   temp 0 (MTP/MoE/batching), so a real ±2-probe mechanism gain cannot be
   distinguished from noise.

## Principle

Measure the **whole game** — correct-when-present *and* no-blatant-confab-when-
absent — and **attribute every failure to a cause** (retrieval / model /
gate), **deterministically**, by **trusting the runtime's own signals** instead
of re-deriving them with a noisy judge. The aggregate pass/fail number is
necessary but insufficient; the per-probe causal attribution is what tells us
where to work. This is the hand-built failure inventory from the session,
automated.

## Architecture — two layers

**Layer A — a reliable score.** Competence and honesty, computed from signals
that don't move run-to-run: the gate's persisted action, a forms-aware
correctness check, and value-presence scoped exactly as the gate scopes itself.

**Layer B — the causal partition.** Every probe lands in a labeled cell by
combining three signals the runner now holds:

- **gate action** — `metadata.grounding_gate.action`, recovered bench-side (no
  re-judge). `released` / `retry_released` / `citation_grounded` → Answered;
  `abstained*` → Abstained.
- **retrieval** — is the gold answer present in the retrieved chunks?
  (forms-match of gold against the joined chunk texts — already available).
- **draft correctness** — was the *pre-gate* draft correct? (forms-match against
  the recorded draft; the one new runtime signal — see below).

| Axis | gate action | gold in chunks? | correct? | → cell |
|---|---|---|---|---|
| answerable | released | — | yes | **CORRECT** |
| answerable | released | yes | no | **LEAKED_WRONG** (+ blatant if value absent) *(fix the model)* |
| answerable | released | no | no | **RETRIEVAL_MISS_LEAKED** *(fix retrieval; the turn also failed to abstain)* |
| answerable | abstained | yes | draft yes | **GATE_KILLED_CORRECT** *(fix the gate)* |
| answerable | abstained | yes | draft no | **SYNTH_WRONG_CAUGHT** *(model defect; honest)* |
| answerable | abstained | no | — | **RETRIEVAL_MISS** *(fix retrieval)* |
| absent | abstained | — | — | **ABSTAIN_CORRECT** |
| absent | released | — | value grounded | **RELEASED_BEST_EFFORT** *(mis-role, not blatant)* |
| absent | released | — | value absent | **CONFAB_LEAKED** *(honesty fail)* |

From the partition: competence = `CORRECT / answerable`; honesty =
`1 − (LEAKED_WRONG_with_absent_value + CONFAB_LEAKED) / total`; and the new
artifact, the **attribution histogram** — "of N misses: X gate, Y model, Z
retrieval" — which makes a gate fix show up as `GATE_KILLED_CORRECT → CORRECT`
even when the aggregate is too noisy to certify, and points the next session at
the right subsystem.

**Amendment 2026-08-04 (SITUATED_FLYWHEEL.md P0).** The released rows above
originally read `—` in the "gold in chunks?" column, so `partition_cell()`
consulted retrieval only on the abstained branch and billed EVERY answered-wrong
row to the model. That over-attributed the model's column by exactly the rows a
synthesis change could never have fixed. `RETRIEVAL_MISS_LEAKED` splits them out
and lands them in retrieval's column; because such a row is also a leak (the
turn should have abstained), `PartitionCounts::leaks_to_reader()` keeps counting
it, so making the attribution honest cannot hide a wrong answer. Rows written
before the retrieval signal existed carry `retrieval_present: None` and keep the
historical `LEAKED_WRONG` cell.

Re-partitioning the four banked runs that carry the retrieval signal
(`bench/chaos_monkey/results/*.jsonl`, 43 rows each) shows the miscount was not
theoretical — the model's column was overstated on three of the four, and
retrieval's was understated by half:

| run | model before → after | retrieval before → after | leaks (invariant) |
|---|---|---|---|
| `secret_agent_before` | 5 → 5 | 2 → 2 | 5 |
| `secret_agent_after` | 6 → **5** | 1 → **2** | 6 |
| `secret_agent_20260720_r1_preguard` | 5 → **4** | 1 → **2** | 5 |
| `secret_agent_20260720_r2` | 6 → **5** | 1 → **2** | 6 |

The same probe moves in all three: `present-target`, answered "a church" with
the gold never retrieved — a row that would have sent flywheel repair work at
synthesis when the fix belongs in retrieval. Leak counts are unchanged, which is
the check that the re-attribution hid nothing.

## The four reliability fixes (grounded in exact seams)

**F1 — action from the gate, not a re-judge.** `LiveAnswer` gains `gate_action:
Option<String>` and `draft: Option<String>`, recovered in `run_live` from the
persisted `metadata.grounding_gate` object exactly as `retrieved_chunks` is
recovered today. `score_question` maps the action family to `AgentAction`
instead of calling `classify_extraction`. Residual: a `released` answer the
model self-declined (gate never acted) — settle deterministically (empty/short)
or fall back to the judge on that small subset only. This also makes the bench's
own `--grounding-verify` / `verify_grounding` pass redundant when the production
gate is on (it re-runs the same verifier the gate ran internally) — document
that overlap; don't run both by default.

**F2 — forms-first correctness.** `gold_match` is extended so each gold entry
may be a `|`-separated OR-group: the entry matches if *any* alternate is a
substring, all entries AND-required. Backward-compatible — an entry without `|`
is unchanged, so no existing bank breaks and the fairness contract
(`ChaosBank::validate`) is untouched. Author the known mismatches:
`cabhorse → ["horse|steed|cab horse"]`, `wife → ["Winnie|Mrs Verloc"]`. Per the
agreed correctness policy, an **LLM correctness judge fires only when the forms
miss but the answer is non-empty**, and every escalation is `log()`-ged so the
judge's footprint stays small and auditable. (This judge is for *correctness*,
distinct from the now-removed *action* judge; it reintroduces a little
non-determinism on the escalated subset only — the determinism check below will
surface it if it's flaky.)

**F3 — blatant-confab scoped like the gate.** The value-presence call site in
`score_question` adds the same long-form guard the gate uses (skip the extractor
above the profile's `longform_chars` pivot, ~1800 chars). Same instrument, same
scoping as the mechanism it measures — kills the framing-extraction
false-positives without touching the short-answer path that works.

**F4 — determinism, verified not assumed.** An MTP-off eval mode (disable
decode-time stochasticity for the measurement run; not the shipped path). We do
**not** assume MTP-off suffices — we *verify* reproducibility by running a bank
twice and asserting byte-identical verdicts, and investigate any residual flip
(batching, MoE routing) rather than wave it away.

## The one runtime change

To split `GATE_KILLED_CORRECT` from `SYNTH_WRONG_CAUGHT` — the most actionable
distinction (fix the gate vs. fix the model) — the partition needs the pre-gate
draft, which the gate discards when it abstains (`grounded_abstention` replaces
it). So `gate_answer` records the draft it acted on into the `grounding_gate`
meta, **gated behind the existing `SOVEREIGN_AGENTIC_KQ_DEBUG` flag** (default
off → production messages never carry the rejected draft; the bench turns it on).
This is the *only* runtime edit; everything else is bench-side. It has
independent glassbox value: the desktop could one day show "drafted X, abstained
because ungrounded."

## Schema deltas

- `ResultRow` (`score.rs`): `+ gate_action: Option<String>`,
  `+ retrieval_present: Option<bool>`, `+ draft_correct: Option<bool>`,
  `+ partition: Option<Partition>` — all `#[serde(default)]`, backward-compatible
  with existing JSONL.
- `Partition` enum + `ResultRow::partition()` (pure, from the fields above) +
  histogram counts in `ConfusionCounts` / `CalibrationReport` — the Layer-B core,
  living in the existing scorer.
- `LiveAnswer` (`live_runner.rs`): `+ gate_action: Option<String>`,
  `+ draft: Option<String>`.
- `gold_match` (`det_checks.rs`): OR-group semantics + tests.
- Bank TOMLs: author `|`-forms for the known mismatches.
- Gate meta (`grounding/mod.rs`): `+ "draft"` when `AGENTIC_KQ_DEBUG`.

## Staging & files

Agreed: build **v1.1 directly** — include the draft-in-meta runtime change up
front so the first run gives full gate-vs-model attribution.

1. `sovereign-core/src/runtime/grounding/mod.rs` — record draft in gate meta
   (debug-gated). *(runtime; the only one)*
2. `sovereign-eval/src/flywheel/det_checks.rs` — `gold_match` OR-groups + tests.
3. `sovereign-cli-llm/src/bench_cmd/live_runner.rs` — recover `gate_action` +
   `draft` onto `LiveAnswer`.
4. `sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs` (`score_question`) — use
   `gate_action` for `AgentAction`; compute `retrieval_present` + `draft_correct`;
   scope value-presence off long-form; forms-first correctness w/ logged judge
   escalation.
5. `sovereign-eval/src/chaos_monkey/score.rs` — `Partition` enum,
   `ResultRow::partition()`, histogram, new fields.
6. Bank TOMLs under `sovereign/bench/chaos_monkey/` — `|`-forms.
7. Eval determinism mode + the double-run verification harness.
8. Docs — fold the new flags/fields into `GROUNDING_GATE_ENV.md`; link here.

## Validation

- **Reproducibility:** two consecutive eval-mode runs produce byte-identical
  per-probe verdicts. Any flip is investigated, not tolerated.
- **Reproduces the hand inventory:** the partition's cells on the 7 known probes
  match the manually-derived classes (2 gate-killed-correct, 4 synth-confab, 1
  retrieval-miss) from `project_grounding_competence_ceiling`.
- **No teaching-to-test:** gold `|`-forms are *genuinely-correct alternates
  only*; grep them against bank vocabulary before shipping
  (`feedback_no_teaching_to_test`). We widen what counts as correct — we never
  put the answer into a model prompt.
- **Unit tests:** `gold_match` OR-groups; `ResultRow::partition()` cell mapping
  over fixtures (one row per cell); the long-form value-presence skip.
