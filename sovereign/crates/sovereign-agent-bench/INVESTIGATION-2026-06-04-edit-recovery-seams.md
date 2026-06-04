# Investigation — role-split, multi-file ceiling, and the edit-recovery seam map (2026-06-04)

Continuation notes for the `native` role-aware runner. Pairs with `HANDOFF.md`.
Pick-up doc if we resume the "make the fast+122B split reliable on hard
multi-file refactors" thread.

## What we set out to do

1. Optimize *model role* in the agent-coding flow: a fast 4B model
   (`Qwopus3.5-4B-v3-MTP`) on the light roles, the 122B
   (`Qwen3.5-122B-A10B`) only where it's needed.
2. Match the benchmark "we established with the 35/36B" model.
3. Push to a harder *multi-file* problem and find the ceiling.
4. Make that ceiling *reliable* (the reason this doc exists).

## Headline results (evidence-backed)

- **Role split works.** `--agent native --planner-model commonwealth/fast
  --implementer-model commonwealth/primary --evaluator-model
  commonwealth/fast`. Routing held ~2:1 fast:big. The 4B is too weak to be
  the *judge* (can't hold anchor-JSON) — judge stays on the 122B.
- **Split beats the 36B baseline overall, complementary strengths.**
  Witness pass-fraction (dim_a), split (4B+122B) vs Darwin-36B single-model:
  - 3.2-lights-out (Rust): **11/12 vs 0/0**  (36B no_progress)
  - 3.2-lights-out-python: **11/12 vs 1/12**
  - 4.1-config-applier:    5/12 vs **12/12**
  - 4.2-mini-evaluator:    0/20 vs **10/20**
  - 9-pt total: **split 19/36 vs 36B 13/36.** The 122B *dominates
    algorithmic* (solves GF(2) the 36B can't start); the 36B *wins the
    refactors*.
- **Authored `5.1-minilang-multifile-python`** — a 3-file interpreter
  (tokenizer→parser→evaluator), 7 bugs cascading *across* files. Reference
  24/24, buggy scaffold 9-pass/15-fail. (`bench/agent-coding/problems/`.)
- **5.1 ceiling: up to 20/24 (83%), but HIGH variance.** Across all runs the
  split scored `{0,0,8,16,20}/24` (single split config), then with the
  recovery stack `{13,14}`, `{14,0}`, `{9,0,0}`. The good runs make real,
  steady cross-file progress; the bad runs bomb to 0.

## The core finding: it's the bench amplifier, not the 122B

A 122B that scores 0/24 one run and 20/24 the next on the *same* problem is
not a capability story. We opened every bomb. **The 122B is capable
(9–20/24 when it dodges a seam). The variance comes from a small set of
brittle SEAMS where the model's output meets a strict harness contract,
each amplified to a 0/24 catastrophe by the same machinery:
`reject (or timeout) → model re-emits the same → sticky-retry kill`.**

### The seam map (each opened with the actual artifact)

| # | Seam | Mechanism (verbatim from a bombed run) | Recovery built | Status |
|---|------|----------------------------------------|----------------|--------|
| 1 | `write_file` escaping | model double-escaped `\n`; file got literal `\n` → "unexpected character after line continuation" | `repair_escaped_whitespace` | built, **never re-fired in prod** |
| 2 | `patch_file` boundary-dup | model added a trailing CONTEXT line (`if op == "**":`) that the strict line-range splice DUPLICATED → "expected an indented block" | `dedup_patch_boundary` | built, **fired 0×** |
| 3 | `replace_function` format glitch | model emitted one line (`word = src[i:j]`) at 1-space indent in an otherwise-perfect body → "unindent does not match" | escalation→write_file (below) | built, **fired 0×** |
| 4 | **`smoke` timeout** | model introduced an infinite loop in the interpreter → `pytest` hangs → `smoke` times out → Evaluator re-runs → sticky | **NONE** | **gap** |
| 5 | `no_progress` | loop spins (build/smoke) without an edit | old detector | exists |

Critical nuance (we corrected a lazy claim mid-investigation): the seams are
a MIX of **system** (1, 2 — interface mismatches: a capable model adds
diff-style context / off-by-one ranges that strict splicing corrupts) and
**model** (3 — a rare one-off formatting glitch any model can emit; 4 — a
genuine logic bug). "A bigger model patches worse" is FALSE; the system
amplifies *any* glitch, from either source, into a 0.

## What was built (all unit-tested, UNCOMMITTED in-tree)

Crates: `commonwealth-agent-tools` (executor + roles) and
`sovereign-agent-bench` (runner). Rebuild: `cargo build --release -p
sovereign-cli` (agent-bench runs IN-PROCESS in sovereign-cli). Tests:
`cargo test -p commonwealth-agent-tools -p sovereign-agent-bench --lib`.

- **`executor.rs::syntax_gate_with_gutter_recovery`** — generalized
  post-failure recovery: tries caller-supplied candidates, then
  `repair_escaped_whitespace` (seam 1), then `strip_echoed_line_number_gutters`
  (line-number echo); adopts the first that PARSES (never corrupts valid
  content). Wired into write_file / patch_file / replace_function.
- **`executor.rs::dedup_patch_boundary`** (seam 2) — trims leading/trailing
  replacement lines that duplicate the unchanged prefix/suffix.
- **`shared_detectors.rs::HANDOFF_CYCLE_CAP` 6→14** — single-file problems
  converge in 1–3 cycles; a 7-bug multi-file problem needs ~bug-count, and
  even the 20/24 run was capped mid-climb.
- **`dossier.rs::verification_just_failed` + native.rs Evaluator restriction**
  (surgical-B) — when smoke FAILS on an unchanged workdir, force
  `[handoff_to_implementer]` so the fast 4B Evaluator can't dead-loop on
  re-running tests. **NOTE: does NOT cover seam 4 (timeout) — see below.**
- **native.rs recovery-escalation (seam 3 keystone)** — one rejection before
  the sticky-kill, if the Implementer is stuck re-emitting a rejected SPLICE
  edit (patch_file/replace_function) at the same site, force its next turn to
  `[write_file]` (full rewrite — no splice contract, fresh regeneration).
  `IMPLEMENTER_REWRITE_SUBSET`.

**Honest status of the recoveries: the gutter-strip and surgical-B fire and
are sound. The escape-repair, patch-dedup, and escalation are CORRECT BY
CONSTRUCTION + unit-tested but UNVERIFIED IN PRODUCTION — in every run since
they shipped, their triggering seam did not recur (the failure moved to an
uncovered seam instead). Do not report them as confirmed wins.**

## Where to investigate next (ordered)

1. **Cover seam 4 (smoke/build timeout) — the immediate gap.** A timeout is
   NOT currently recorded as a failing verify, so `verification_just_failed`
   is false and the Evaluator dead-loops on it. Fix: on a build/smoke
   timeout, call `on_verify()` + `record_verification(kind, ok=false)` (or a
   dedicated timeout flag) so surgical-B forces the handoff — and surface
   "your code likely infinite-loops" to the Implementer so it can fix the
   logic bug (a model-capability test in its own right).

2. **VERIFY the unverified recoveries.** They fired 0× live. Either (a) a
   replay/integration harness that feeds the captured bombed `requests.jsonl`
   back through the executor to assert the recovery now rescues it, or (b)
   enough multi-trial volume that each seam recurs. Without this we don't
   actually know they work end-to-end.

3. **Statistical rigor.** Variance dominates (see memory
   `project_bench_variance_dominance`): single/2-trial runs are unreliable.
   Use stable-seeding or N≥5 before claiming any delta. We repeatedly
   mistook a draw from `{0,…,20}` for a fix's effect.

4. **The architecture question.** Per-seam recovery is a bounded but LONG
   tail. Consider whether the edit INTERFACE should change instead: a single
   robust primitive (search/replace by anchor text, not line ranges), or
   normalize-on-ingest (strip gutters, fix escaping, dedup boundaries
   unconditionally with a parse guard) so the three edit tools share one
   hardened path. That would collapse seams 1–3 structurally.

5. **Re-test the 36B on 5.1.** It won the single-file refactors; it patches
   differently (more write_file full-rewrites observed). A 36B vs split
   head-to-head ON 5.1 would tell us whether full-rewrite habit ⇒ fewer
   seams, which would also validate the seam thesis.

## Reproduction

```
# Daemon must serve commonwealth/primary (122B) + commonwealth/fast (4B).
sovereign agent-bench run --agent native \
  --problems 5.1-minilang-multifile-python \
  --planner-model commonwealth/fast --implementer-model commonwealth/primary \
  --evaluator-model commonwealth/fast \
  --model commonwealth/primary --judge-model commonwealth/primary \
  --judge-trials 1 --trials 3 \
  --bench-root sovereign/bench/agent-coding \
  --report /tmp/r.json --artifacts-dir /tmp/r
```
Bombs are in `<artifacts-dir>/<problem>/trial-N/requests.jsonl` (the last
record holds the full conversation; grep the assistant tool_calls + the
`tool` rejection messages). The witness pass-fraction (dim_a) is the
trustworthy metric; dim_b/dim_c depend on the judge.

## Related memory

`project_agent_bench_role_split`, `project_agent_bench_harness_fixes`,
`project_qwen3.5_122b_throughput`, `project_bench_variance_dominance`,
`feedback_sovereign_binary_symlink_gotcha`.
