# Pre-registration — demand-directed retrieval

Written 2026-09-21, before any arm has run. Bars below are fixed at this
revision. Changes after first data land in §9 Deviations with a reason, or the
result is not honest.

**Claim under test.** Computing the turn's demands BEFORE retrieval and using
them to steer it produces evidence windows that serve more of the question's
information need, at equal or better density, than the current undirected
pipeline — and that gain reaches the answer.

**Why staged.** E0-E2 are cheap and gate an expensive build (E3). Each can
kill or redirect before that cost is paid. Run in order; a failed stage stops
the sequence.

---

## 1. Operational definitions

Fixed here so no measure is chosen after seeing data.

| Symbol | Definition |
|---|---|
| `t` | one bank question |
| `G(t)` | gold facts: the bank's `expected_facts`, NFKD-casefolded, whitespace-collapsed |
| `W(t)` | evidence window: ordered chunks from `svrn eval run --prod-pipeline`, full text (`snippet`, uncapped) |
| `P(t)` | wide pool: identical run at `SOVEREIGN_KQ_POOL_SCALE=4` |
| `D(t)` | demands declared by `build_demands` (`runtime/epistemic.rs:363`), zero model calls |
| `C(t)` | query class: the bank's `category` field |

`present(g, X)` ≡ normalized `g` occurs as a substring in the concatenated
full text of `X`.

- **recall(X)** = `|{g ∈ G : present(g, X)}| / |G|`
- **density(X)** = `Σ chars of chunks in X containing ≥1 g  /  Σ chars of X`
- **pool_recall** = recall(P)
- **selection_efficiency** = recall(W) / recall(P), undefined when recall(P)=0
- **demand_recall(t)** = fraction of `g ∈ G` retrievable by at least one
  `d ∈ D(t)` issued as a standalone retrieval query. Deterministic; no judge.

`C` is taken from the bank, never from the router. Whether the router assigns
the class correctly is a different question and is not measured here.

---

## 2. Arms

| id | arm | configuration | exists |
|---|---|---|---|
| A0 | base | pipeline defaults | yes |
| A1 | wide | `SOVEREIGN_KQ_POOL_SCALE=4` | yes |
| A2 | demand-directed | demands threaded through `PipelineState`, steering prefilter + selection | **must be built — E3 only** |

A1 is the recall ceiling, not a candidate configuration. No other arm is in
this pre-registration; anything discovered mid-run goes to `NEXT.md`.

---

## 3. E0 — instrument validation. Nothing else is read until this passes.

**E0a — controls.** Two rows in every bank.
- `ctl_pos`: a gold string verbatim in exactly one chunk of the corpus.
  **Must** score recall 1.0 under A1. Failure ⇒ retriever or scorer broken.
- `ctl_neg`: a gold string present in no installed corpus.
  **Must** score recall 0.0 under every arm. Failure ⇒ matcher too loose.

**E0b — noise floor.** `--prod-pipeline` is declared deterministic (intent
pinned, no synthesis). Run A0 three times on the full bank.

- **Required:** per-question recall spread = **exactly 0** across the three runs.
- **A non-zero spread is a DEFECT, not a noise band.** It halts the sequence
  and becomes its own investigation. Prior evidence this may fire: the
  `full` arm of `cdfb971d5` moved 0.537 / 0.500 / 0.426 at temperature 0 while
  `bare` and `deep` held still.

**E0c — binary freshness.** `svrn eval` must be running a `sovereign-cli-llm`
built after the `snippet` uncapping. Verified by asserting
`max(len(snippet)) > 600` somewhere in the run, or by `readlink -f` + mtime.
A capped run measures a truncated haystack and is void.

---

## 4. E1 — where we are: retrieval-bound or selection-bound

**Runs only if E0 passes.** No build required; A0 and A1 both exist.

**H0.** recall(W) under A0 does not differ from recall(P) under A1 — i.e. the
selector is not discarding reachable facts.

**Primary measurement.** Per class: recall(A0), recall(A1),
selection_efficiency, density(A0), density(A1).

**Decision table — this is the whole point of E1:**

| observed | reading | what gets funded |
|---|---|---|
| recall(A1) high, selection_efficiency low | selection-bound | demand-directed selection (E3), class objectives |
| recall(A1) low | retrieval-bound | collection selection, contextual chunking; E3 deferred |
| both high | neither is the constraint | **stop.** Re-aim at synthesis; this program is misdirected |

"High" and "low" are read against the per-class distribution and reported as
numbers, not adjudicated against a threshold — E1 is a direction-finder, not
a gate, and is labelled as such.

**Cost.** Two retrieval-only sweeps. Minutes.

---

## 5. E2 — the precondition. Demands must span the question.

**Runs only if E1 says selection-bound.**

**Why it exists.** `build_demands` was written to EXPLAIN a finished turn. A
demand it misses costs nothing today — the ledger merely under-reports. Once
demands steer retrieval, a missed demand is discarded by construction, so the
system optimizes confidently for the wrong target. That is strictly worse than
today's undirected breadth, and it is a new failure mode E3 would introduce.

**H0.** demand_recall < 0.90 — the declared demands do not span the question's
information need.

**Bar.** demand_recall ≥ **0.90**, pooled and in every class with n ≥ 20.

**Justification for 0.90, not a guess:** at 1 − demand_recall = 0.10, one in
ten required facts corresponds to no demand and is unreachable under steering.
E3's pre-registered win is +0.10 recall. A precondition that admits a loss
equal to the target gain cannot distinguish them.

**KILL BAR.** demand_recall < 0.90 ⇒ E3 does not run. The surviving branch is
to fix the demand builder first, as its own work with its own bar.

**Cost.** One retrieval sweep per demand. No model.

---

## 6. E3 — the treatment

**Runs only if E2 passes.** Requires building A2.

**H0.** A2 does not improve recall over A0 in any class.

**Primary.** Per class, paired by question: `recall(A2) − recall(A0)`.

**Bar (all three, conjunctive):**
1. `Δrecall ≥ +0.10` in at least one class with n ≥ 20;
2. no class with n ≥ 20 shows `Δrecall < −0.02`;
3. in every class where recall rose, `Δdensity ≥ −0.02`.

Condition 3 is not optional. Widening a window raises recall and drops density
for free; a recall gain reported without its density term is **not judged**,
per the two-direction rule.

**Statistics.** Same questions across arms ⇒ paired. Two-sided sign test on
per-question recall deltas, per class. Report n, wins, losses, ties, p.
Precedent: the PPR and reranker rows used the same test.

**No post-hoc subgrouping.** Classes are fixed in §1. A cut not named here is
not reported as a result.

**KILL BAR.** Bars 1-3 not met ⇒ REJECTED row in `DEFAULTS_LEDGER.md` with the
curve that said no. Demand-directed retrieval is not re-proposed without a new
signal, not a new threshold.

---

## 7. E4 — transfer. The experiment that can kill the program.

**Runs only if E3 passes.** This is the only stage with a model in the
scoring path and the only one whose result can stop the whole thesis.

**H0.** Window recall does not predict answer quality — the binding constraint
is synthesis, not evidence.

**Design.** ≥3 arms spanning the achieved recall range, same bank, `--synth`,
existing judge, `--judge-trials 3` (their own record: judge verdicts flipped
on 37% of facts, 104/284, across trials on one transcript — a single-trial
judge cannot carry this).

**Measured once, not per arm.** The transfer function is a property of the
system, not of an arm.

**KILL BAR for the thesis.** `Δrecall ≥ +0.15` producing `Δanswer-quality
< +0.03` with intervals excluding the effect ⇒ evidence quality is not the
constraint. The evidence program stops and the finding is the result.

---

## 8. Verdicts, exclusions, power

**Four verdicts per class, each its own exit code:** `passed`, `failed`,
`could-not-judge`, `never-ran`. Never two.

**could-not-judge** is returned, not a mean, when: n < 20 in that class; or
E0a controls failed; or E0b spread ≠ 0; or E0c found a capped run.
Precedent for n ≥ 20: `cdfb971d5` reported could-not-judge at n = 16.

**Exclusions, declared here and nowhere else:**
- `recall(A1) = 0` ⇒ the fact is unreachable at any pool size. Excluded from
  selection_efficiency only; counted and reported as `unreachable`.
- Empty `expected_facts` ⇒ excluded, counted.
- No other exclusion is permitted after data.

**Not measured by this design, stated so it is not claimed:** router class
accuracy; latency; multi-corpus behaviour beyond the banks used; any effect on
turns the banks do not represent.

---

## 9. Deviations

Every departure from §1-§8 after the first run, with date and reason. Empty at
registration.

## 10. Named substitutions

Anything run with a different model, quant, corpus or bank than specified,
named here rather than inferred from a log. Empty at registration.
