# `svrn bench situated` — the situatedness process lane

The chaos banks grade situated **outcomes**: answered, abstained, leaked, with
a causal partition that says whether a miss was retrieval's, the model's or
the gate's. This lane adds the **process** layer — *which situated behaviour
failed*, per probe, per model. That is the layer a harness change can act on,
and it is the tuner half of `SITUATED_FLYWHEEL.md`.

## It does not generate

The lane scores transcripts the chaos bench already produced by driving the
production turn (routing → retrieval → synthesis → grounding gate). There is
no bench-local chat loop here, by design: no place for a bench-only scaffold
to live, and no second generation path to drift from the shipped one. A change
that moves these numbers moved the real turn, because the real turn wrote the
transcript.

```bash
# 1. produce transcripts on the production path
svrn bench chaos-monkey --bank secret_agent --transcripts run-a.jsonl

# 2. certify the judge — once per judge, PER CRITERION VOCABULARY
svrn bench situated --calibrate --judge-model <J>

# 3. score, and diff a second arm against the first
svrn bench situated --transcripts run-a.jsonl --judge-model <J> --report a.json
svrn bench situated --transcripts run-b.jsonl --judge-model <J> --diff a.json
```

## Criteria are chosen by question TYPE, never by probe content

`criteria.toml` is a closed vocabulary of situated behaviours; each declares
the `QuestionType`s it applies to. The converter materialises per-probe
criteria with `{key}@{content-hash}` ids. Nothing about a probe's *content*
reaches a criterion, which is what structurally prevents the bank's own
vocabulary from being taught to the test — the audit surface is the 15
strings in `criteria.toml`, not 664 generated ones. Rationale, sizing and the
audit: `CRITERIA_DRAFT.md`.

Adding a behaviour is an edit to `criteria.toml` plus a re-calibration. It is
never an inline prompt change.

## Reading a report

- **By dimension** — fulfillment with Wilson 95% CIs. A delta earns a `*` only
  when the two intervals are **disjoint**. Overlap is not "no difference"; it
  is "this bank cannot tell them apart".
- **The paired block, printed directly under it** — exact two-sided McNemar
  over per-criterion verdict FLIPS, plus an exact sign-flip permutation test
  on per-probe scores. Both arms run the same probes against the same criteria,
  so treating them as independent samples (which the Wilson table does) throws
  the pairing away. Read this block *before* the rate delta: on the arm-C
  comparison the table renders `boundary +9.5 +` while the paired block renders
  `4 better / 2 worse, p = 0.6875` — the same numbers, and only the second one
  says the effect is heterogeneous. `unpairable` counts criteria that could not
  be matched across the arms (present in one only, could-not-judge in either,
  or a changed weight/dimension) and is printed whenever non-zero; a paired
  verdict over a shrunken set says so on its face.

  Why it matters for planning a run: at the arm-C effect size the independent
  test needs ~138 probes per arm to settle the question and the paired test
  needs ~49.
- **By question type** — situatedness is not one skill. A model can ground an
  answerable probe well and name a gap terribly; a single mean hides that.
- **By criterion** — fulfilled/judged per behaviour, worst first. These are
  the work items. A criterion flagged `never varied` is a lead about the
  *instrument* (or about probe selection), not a finding about the model.
- **Could-not-judge** is counted and disclosed, never defaulted. Over 10% and
  the run is degraded: a score over a shrunken denominator is not comparable.

Two refusals are deliberate. `--diff` refuses across criterion-vocabulary
versions, because a ruler change must not read as a harness win. And a run
under a vocabulary whose `status` is not `stable` says so in its header, so an
uncalibrated number cannot be quoted later as a settled one.

## Calibration does not transfer

`calibration.toml` is this family's own hand-labeled bank. A judge certified
on the moral lane's set is **not** certified here — that set says nothing
about whether a judge can tell a vague "I couldn't find it" from a specific
gap statement. The floors are sens/spec ≥ 0.85, and calibration items quote
criterion text verbatim (a test enforces it, so rewording a criterion cannot
silently leave the judge certified on a string it will never see).

Every criterion has at least one calibration item except
`grounds_in_superseded`, whose question type has no probes banked yet.

## Where the code is

| Concern | File |
|---|---|
| CLI, flags, calibration mode | `crates/sovereign-cli-llm/src/bench_cmd/situated/mod.rs` |
| Vocabulary load + type binding + ids | `…/situated/criteria.rs` |
| Chaos transcript input | `…/situated/transcripts.rs` |
| Judging loop | `…/situated/runner.rs` |
| Report shapes + the by-criterion table | `…/situated/report.rs` |
| **Judge, scoring formulas, Wilson CI, diff** | `…/bench_cmd/rubric/` (shared with `moral`) |

Nothing in `situated/` re-implements a formula, a threshold or the
significance rule; those have one implementation, in `rubric/`.
