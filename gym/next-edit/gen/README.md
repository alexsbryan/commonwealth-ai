# next-edit generalization bank (NEXT_EDIT.md §6, model lane)

The hand-curated half of §6: cases the **rule lane structurally
cannot fire on**, which the model lane (P2) must clear before it may
default-on. Where `../` (the rule bank) is a contract check with
100%/0% bars, this bank scores a *model* — the bars are pre-registered
quality gates in the FIM-bank mold, and the **wrong-edit rate is the
default-on decider**: a wrong edit proposal is the expensive failure
(§1 of the spec), so the lane ships opt-in and stays opt-in until GM3
is green.

## Case categories

Positives — the generalizations named in the spec, ~10 each:

- **casing_variant** — the literal-rule sites are exhausted, but the
  same rename remains at other casings (`getUserData` edited out;
  `get_user_data` sites remain). The rule lane is silent (`no_sites`);
  the induced rules are *identical*, so the consult gate detects this
  shape by probing the document for casing-variant renderings of the
  rule's token sequence. **DEFERRED from the v1 model lane
  (2026-07-30, after runs 1–2)** — see the deferral record below. The
  ten cv cases stay in the bank as kind `deferred_casing`: they assert
  the gate declines by name (`skipped: "casing_deferred"`, counted by
  GM2) and keep their content expectations for the day the category
  re-activates.
- **signature_fanout** — the same insertion at differently-shaped call
  sites (`connect(h1, 80` / `connect(host2, p` both gain
  `, DEFAULT_TIMEOUT`). Unit cores agree (`""` → `", DEFAULT_TIMEOUT"`)
  while the expanded rules differ per site, so induction never reaches
  support 2.
- **param_insert** — the replacement varies per site
  (`.unwrap()` → `.expect("load config")` / `.expect("bind socket")`).
  Befores agree; afters share a prefix but differ, so no single
  literal rule exists. Expected `new_text` is checked by pattern
  (`new_regex`), not byte equality — the model authors the message.
- **field_init** — a struct/object literal gains a member across
  initializer sites; the captured units are **multi-line**, which the
  rule lane declines by design (`expand_rule` → None). The consult
  gate therefore compares raw unit cores, not only expanded rules.

Negatives, two sub-classes with different bars:

- **gate_negative (~10)** — the deterministic consult gate itself must
  refuse: dissimilar edits, single-edit history, empty history, and
  rule-lane-owns cases (the rule lane fires; the model must never be
  consulted when it does).
- **model_negative (~10)** — the gate legitimately consults, but the
  correct output is *no edit*: the pattern is exhausted in the region
  (every applicable site already edited). A fire here is a wrong edit
  by definition and feeds GM3.

## What the checker asserts (per case, pre-registered at authoring)

- `expect.consult` / `expect.consult_reason` — against
  `sovereign_debug.model.consulted` / `.reason` (deterministic).
- `expect.fire` — non-empty `edits` with `engine: "model"`.
- Content, one or both of:
  - `expect.first_edit` — the first edit's old span must equal
    `old_text` and its `new_text` must equal one of
    `new_alternatives` or match `new_regex` (compared after stripping
    trailing whitespace per line).
  - `expect.after_counts` — occurrence counts of pinned substrings in
    the document after applying ALL returned edits (the
    insertion-friendly check: `{"s": "retries: 3,", "n": 3}`).
- Structural (every fired case): offsets in-bounds and non-overlapping,
  every old span matches the live document, and every edit falls
  inside the region the daemon reported in
  `sovereign_debug.model.region`.

## Pre-registered gates (set before the first run, 2026-07-30)

| Gate | Metric | Bar |
|---|---|---|
| GM1 structural | malformed edits on the wire (bad offsets, overlap, old-span mismatch, outside reported region) | **0** |
| GM2 gate determinism | consult decision + reason match expectation, all cases | **100%** |
| GM3 wrong-edit rate | fired cases failing their content checks, plus ANY fire on a model_negative, over all fires | **≤ 5%** |
| GM4 usefulness floor | positives fired AND content-correct | **≥ 60%** |
| GM5 latency | wall p95 (local daemon, model resident) | **≤ 6000 ms** |

Verdict semantics: **GM1/GM2 red = named bug** (they gate code, not
the model — triage, never move). **GM3 green AND GM4 green →
the lane may default-on** (flip the extension setting default).
GM3 green with GM4 red → the lane stays opt-in (safe but not yet
useful enough). GM3 red → the lane must not ship default-on,
full stop. Reported but not gated: drop-reason histogram
(`model_invalid` / `model_noop` / …), per-category breakdown,
region-selection needle hit rate, and p50 latency.

## Deferral record — casing_variant (2026-07-30)

The gate BARS above never moved; the system under test changed, and
this is the evidence. Runs 1–2 (Mellum2-12B-A2.5B-Instruct-Q6_K,
dedicated slot): every wrong edit the lane produced was in
casing_variant — 4 wrong of 9 casing fires across both runs (run 1:
cv01 deleted an unrelated code block and fabricated `await`
insertions, cv07 applied the rename *backwards*; run 2 with a
casing-specific instruction: cv07 reversed again, cv08 newly wrong)
— against **0 wrong of 60 fires** in signature_fanout + param_insert
+ field_init, and 20/20 correct silence on both negative kinds.
Prompting made casing worse, not better: this is a model-capability
verdict, not a tuning gap. Per the precision posture the consult gate
now *detects* the shape and declines by name (`casing_deferred`).
Re-activation path: the detection already computes the variant
find/replace deterministically, so casing belongs to a rule-engine
sub-lane (byte-precise, no model) — when that lands, flip the cv
cases back to firing expectations and score it under GM1/G-style
exact bars, not GM3/GM4.

## Run

```
python3 gym/next-edit/gen/author.py        # (re)build cases.jsonl — deterministic
python3 scripts/next_edit_gen_eval.py      # run vs live daemon :9741 (needs [models.fim] resident)
```

Cases are authored in `author.py` as reviewable code (the file IS the
provenance — hand asserts at build time verify each document actually
contains the sites a case claims). Unlike the rule bank there is no
git harvesting: generalization episodes need intent the harvester
cannot infer, so every case is written by hand, in the spirit of
FIM's 60-case bank. Exit code is the gate verdict; `--json out.json`
dumps per-case results for triage.
