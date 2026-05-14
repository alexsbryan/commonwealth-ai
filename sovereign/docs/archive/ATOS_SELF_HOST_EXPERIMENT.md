# ATOS self-host experiment — overnight test-and-iterate loop

A plan for using ATOS to drive an autonomous coding agent (opencode
+ K2.6 across the mesh) at a known target — `oicp-types` — and
score its output against the existing reference implementation. The
goal isn't a new `oicp-types`; it's a **calibrated loop** we can
later point at unknown targets with confidence about what the score
means.

← related: [`ATOS.md`](./ATOS.md), [`PHASE_7_GAP_CLOSURE_PLAN.md`](./PHASE_7_GAP_CLOSURE_PLAN.md)

---

## Load-bearing framing

We're using ATOS to implement a subset of the system that *contains*
ATOS. That self-reference is the upside (the score tells us how good
our prompts/specs/charter actually are) and the failure mode worth
naming up front:

> When the agent gets a bad score, four hypotheses are alive at
> once: the **agent** was bad, the **spec** was bad, the **charter**
> was bad, or the **rubric** was bad. One signal, four causes.

The mitigation is not to abandon the loop. The mitigation is to
instrument it enough that a regression's cause is *attributable* —
that's what the rest of this plan is for. If a run scores poorly and
we can't say *why*, we have observation, not learning.

---

## What to build (target selection)

Two candidates considered:

| | A — `oicp-types` | B — `corpus-engine` acquire→extract→chunk→embed |
|---|---|---|
| Size | 1500–3000 LOC | 5000–8000 LOC |
| Overnight runs | 1 | 2–3 |
| Surface | Wire types + scorer + benchmark helpers | Trait-shaped pipeline w/ contracts |
| Ambiguity | Low (mostly mechanical pass/fail) | High (many opinions per choice) |
| Spec coverage today | Covered by §4.8 | Partially specified |
| Risk if score is bad | "Loop is broken" (clean attribution) | "Model and I disagree about good" (muddy) |

**Decision: start with Option A.** Low ambiguity is *good* for the
first iteration — it lets us debug the loop itself before debugging
the loop's output. Once the loop is calibrated, B becomes the
graduation target.

The discipline that makes the score meaningful: **the agent never
sees the reference implementation.** Not in the charter, not in the
spec, not in chat history. Only the scorer sees it. Otherwise we're
measuring how well the agent copies, not how well the loop produces
correct code from a contract.

---

## The four artifacts

ATOS already mandates a charter and per-feature specs. Concretely
for `oicp-types`:

### 1. `CHARTER.md` (project root)
The constitutional doc. Names:
- **Top-level invariants** — `no_std` compatible, all wire types
  serde round-trippable, scorer is a pure function with no I/O.
- **Success criteria** — all reference test vectors pass; qualitative
  axes ≥ thresholds (see scoring section).
- **Red-team gate** — `Red team: auto`.
- **Phases** — `bootstrap → wire-types → scoring → benchmark-helpers
  → done`.

### 2. `spec.md` per feature, under `.sovereign/features/<feature-id>/spec.md`
Local contract for one milestone. ~100–300 lines each. For
`oicp-types`, ~4–5 specs:

| feature-id | what the spec contracts |
|---|---|
| `wire-types` | Public types, serde derive matrix, no_std boundaries |
| `capability-hint-validation` | Hint shape, validation rules, error type |
| `score-claim-for-request` | Signature, scoring formula, fixed test vectors |
| `effective-affinity-blending` | Blending formula, edge cases (NaN, empty) |
| `benchmark-extrapolation` | Helpers + trait surface, what's pure-Rust vs needs_runtime |

Each spec includes: signature of public surface, semantic invariants,
edge cases, test vectors that constitute pass/fail.

### 3. `AGENTS.md` (project root)
opencode's local rules file — what opencode reads as ambient context.
Bench commands, how to run tests, lint command. Separate from ATOS's
chat-completion-middleware injection, which handles the spec
content. AGENTS.md is the native channel.

### 4. `scorer/rubric.md` (lives in experiment repo, *outside* `.sovereign/`)
The scoring agent's contract. The fourth doc and the one needing
the most thought — see next section.

---

## The scoring agent

The load-bearing part of the loop. Tiered design with a mechanical
gate:

### Layer 1 — mechanical (always runs first)
Run the agent's code against a frozen test suite extracted from the
reference's `oicp-types/tests/`. Score = % tests passed.

- Cheap, deterministic, catches behavioral divergence cleanly.
- If this fails, score caps at the percentage and the loop iterates.
  No need to spend tokens on qualitative judgment of code that
  doesn't work.

### Layer 2 — LLM-as-judge over diff (gated on layer 1 pass)
Feed both implementations to a strong external model (Claude Opus or
similar — **not** our mesh K2.6, to avoid self-judgment). Score on
five axes, 0–3 each:

| axis | what it measures |
|---|---|
| API congruence | Are public types and signatures equivalent enough that a downstream caller could swap implementations? The hard contract. |
| Internal coherence | Does the implementation hold together — no orphan helpers, no half-stubbed paths, modules cohere? |
| Idiomatic Rust | `?` over `match`, `From`/`Into` for conversions, `#[derive]` over hand impls, etc. |
| Specification fidelity | Did it implement what the spec said, or add scope? |
| Testing discipline | Meaningful unit tests, or copy/paste assertion patterns? |

Total: 0–15. Bias-aware caveats baked into `rubric.md`: judge the
diff, not the surface length; same judge model + temperature across
runs or scores aren't comparable.

The scorer emits a single JSON record:
```json
{
  "mechanical": {"tests_passed": 47, "tests_total": 53},
  "qualitative": {"api_congruence": 2, "internal_coherence": 3, ...},
  "notes": "..."
}
```

This is what we wake up to.

### Why not pure mechanical, why not pure LLM-judge?
- **Pure mechanical** misses everything about quality, idiom,
  structure. We'd ship a tarball of `panic!`s that happens to pass.
- **Pure LLM-judge** is prone to known judge biases (length, surface
  similarity) and runs blind to whether the code actually works.

The synthesis: mechanical gates the qualitative, qualitative
disambiguates "passed but ugly" from "passed cleanly."

---

## Glass-box tracing — what to capture

The whole point of iterating the loop is being able to attribute
regressions. Categories of signal:

### Per-turn (already native to opencode + ATOS)
- Tool calls (which, args, return) — opencode logs these.
- `finish_reason` per LLM call — catches silent truncation.
- Drift events — ATOS records every turn that detects spec drift.
- Notes scoped to feature — the agent's own scratchpad. Read these
  to understand what it was thinking.

### Per-milestone (already native to ATOS)
- `start-milestone` / `end-milestone` event pairs, durations.
- Red-team findings.
- Postmortem pointers.

### Per-run (NEW — small thing to build)
A `run_manifest.json` written at session-end:
- Charter SHA, spec SHAs, model identifiers (the
  `claude-opus-4-7` / `kimi-k2.6` strings), opencode version, git
  SHA of experiment repo at run start, env fingerprints.
- Scorer output, attached.
- Aggregate metrics — total tokens, total tool calls, wall time,
  peak deviation rate, mean turn duration.
- Reasoning trail — every spec amendment requested, every red-team
  finding, every approval gate hit. ATOS already writes these into
  `NoteStore`; this just rolls them up.

### Cross-run (NEW — small thing to build)
- `runs/` directory, one subdir per overnight: manifest, scorer
  output, final source tree.
- A diff tool: `score-diff <run-a> <run-b>` → "what changed in
  score, what changed in charter/spec, what changed in opencode
  prompt config." **Without this we have observations; with it we
  have learning.**

### Critical: don't build a parallel telemetry stack
The tracing rides what ATOS already records (`NoteStore`,
`FeatureStore`, drift detection) plus a thin `run_manifest.json`
collector triggered by the daemon's session-end hook. A new
parallel stack is the wrong direction.

---

## The iteration mechanism

After each overnight, we have a score. Suppose it's 60% mechanical
pass, 11/15 qualitative. Inspection workflow:

### Mechanical failures
Each failure points at a specific test vector. Those tests come
from the reference's behavior, which the agent didn't see. Ask:
**did the spec specify this clearly?**
- **Yes** → agent failure. Note it; don't immediately change anything.
- **No** → spec failure. Amend the spec, rerun.

### Qualitative failures
Each axis points at a class. If `idiomatic_rust` scored 1, look at
the agent's code: hand-rolled `match` where `?` would work? That
suggests charter/spec didn't establish the idiom expectation.
Amend the charter: *"Use `?` for error propagation. Use `#[derive]`
over hand impls when possible. Prefer `Into`/`From` over manual
conversions."*

### One change per iteration
**The discipline that makes the loop produce signal rather than
noise.** Change the charter, the spec, *and* the model on iteration
2 and we can't attribute the score delta. One change, one rerun,
one attribution.

### Change-type hierarchy (cheap → expensive)
1. **Prompt-level**: tweak `AGENTS.md`, tweak charter wording.
   Re-run cheap, signal local.
2. **Spec-level**: rewrite a spec for clarity. Agent gets a different
   contract; behavior changes.
3. **Charter-level**: change invariants or phases. Essentially
   restarting the experiment.
4. **Loop-level**: change the model, change the scorer, change the
   rubric. **Only when 1–3 are exhausted.**

Most learning happens in tiers 1–2.

---

## Day-1 concrete steps

End-to-end, what an actual first day looks like:

1. **Create experiment repo** `atos-experiment-oicp-types/` —
   separate from the main monorepo so there's no path for the agent
   to see the reference.
2. **Author the four artifacts** — `CHARTER.md` (~150 lines), four
   `spec.md` files (~150 lines each), `AGENTS.md` (~50 lines),
   `scorer/rubric.md` (~100 lines). Estimated time: half a day,
   mostly thinking, not writing.
3. **Extract reference test vectors** into `scorer/golden_tests/` —
   the actual `oicp-types/tests/` from the monorepo, shaped into a
   freestanding crate the scorer can run against the agent's output.
4. **Stand up the cluster** — K2.6 running across the 8 mesh nodes
   (already running from the Tier-2 build's hardware footprint).
5. **Kick off opencode**:
   ```bash
   cd atos-experiment-oicp-types
   opencode --model commonwealth/sovereign-coder
   ```
   ATOS plugin injects charter+spec, autonomous loop begins.
6. **Run-manifest collector** — ~50 lines of bash + a small Rust
   binary, triggered by the daemon's session-end hook.
7. **Sleep. Wake up.** Run the scorer:
   ```bash
   scorer score runs/2026-05-04/
   ```
   Read the output.
8. **Iterate** — single change, rerun, compare via `score-diff`.

---

## Realistic expectations

**Iteration 1 will score poorly.** First run on any new ATOS scope
surfaces gaps in charter and specs that we didn't see while writing
them. *That's not a failure of the loop — that's the loop doing its
job*, surfacing contract holes that exist in our heads but aren't
yet on paper. The discipline is to **amend, not to manually fix the
agent's output.** Manual fixes contaminate the next iteration's
attribution.

### Calibration goal across 4–6 iterations
- **>85% mechanical pass**
- **>12/15 qualitative**

At that point the loop is calibrated: we know what charter/spec
quality produces what score, we trust the rubric, and we can move
to a higher-ambiguity target (`corpus-engine` pipeline subset, or
the next roadmap feature) with calibrated confidence about what the
score means.

**The real prize isn't `oicp-types` rebuilt — we already have it.
The prize is a working autonomous-coding loop we can point at the
next feature with calibrated confidence.**

---

## Open questions to resolve before kickoff

These are pending decisions that should be locked before iteration 1
starts; otherwise they become confounders:

1. **Judge model identity** — Claude Opus 4.7 vs GPT-5 vs an
   Anthropic-internal fixed checkpoint? Decision should be
   *recorded in the manifest* and *unchanged across iterations*
   without a deliberate loop-level change (tier 4).
2. **Test-vector freezing** — checksum the extracted golden tests
   at experiment start. If we update them mid-experiment, that's a
   tier-4 loop change.
3. **Token budget per overnight** — wall-clock vs token cap. Either
   is fine; both must be recorded and stable.
4. **Failure-mode for partial completion** — if the agent finishes
   3 of 4 specs in the overnight, does the scorer run partial or
   wait? Recommend: scorer always runs, manifest records phase
   reached.
5. **Where the `runs/` directory lives** — local-only, or pushed to
   a separate `atos-experiment-oicp-types-runs` repo for
   git-bisect-friendly history? Recommend: separate runs repo,
   one commit per overnight, manifests are the audit trail.

---

## What this plan is *not*

- **Not a rebuild of `oicp-types`** — that crate is shipped and
  load-bearing. The experiment repo is a sandbox.
- **Not a benchmark of K2.6 vs other coder models** — that's a
  separate experiment. Here we hold the model fixed and vary the
  contract artifacts.
- **Not a replacement for human review** — the loop catches contract
  drift; it doesn't catch novel design insight. After calibration,
  human review of the agent's *non-obvious choices* still matters.
- **Not infinite** — calibration goal is 4–6 iterations. If we're
  past 8 with the score not improving, that's a signal the target
  was wrong (move to B) or the rubric is broken (tier-4 change).
