# Archaeology Eval — measure, iterate, improve

> Re-verify the claims archaeology makes against git itself. No
> ground truth required, no LLM judgment in the loop. Witness
> checks + baseline diff + curated regression cases (inquiries).

## Why this exists

[Git archaeology](./GIT_ARCHAEOLOGY.md) makes lots of claims:
"this commit hash exists, touches this file, was authored by X."
v1 claims are mechanically derived from `git log` — they can't
fabricate. v2 atoms (Lineage, Person-Knowledge Locus) will carry
**LLM-generated** reasoning citations, and those *can* fabricate.

Eval is the surrogate-truth signal that detects fabrication and
catches regressions between iterations of the archaeology
prompt/threshold/model. Three signals, all cheap:

1. **Witness checks** — every cited commit, file, and date is
   re-verified against git. Pass / Fail / Stale per check.
2. **Baseline diff** — current run is diffed against the last
   saved baseline. Added / removed / score-changed atoms surface.
3. **Inquiries** — curated cases ("the X invariant should yield
   atoms anchored to these files with these keywords") that
   become a permanent regression suite as you accumulate them.

The CSV trend at `~/.svrnmesh/eval/history.csv` is the artifact
you treat as "is the system improving?"

## Quick start

```bash
# Pre-req: you've already run `sovereign git-archaeology <atlas>`
sovereign archaeology-eval sovereign-self-atlas \
    --inquiry inquiries/callers_callees_twin_tools.toml \
    --save-baseline
```

That run:
- Reads `~/.svrnmesh/indexes/<atlas>/atlas/git_archaeology.json`
- Runs four always-on witness checks per atom + three
  inquiry-driven checks per match
- Saves the result as the baseline
- Writes a markdown report to `~/.svrnmesh/eval/<atlas>.eval.md`
- Appends a CSV row to `~/.svrnmesh/eval/history.csv`

Subsequent runs — without `--save-baseline` — diff against the
saved baseline. Promote a run to baseline only when you've
confirmed it's an improvement.

## Witness checks

### Always on (run on every atom)

| Kind | What it asks |
|---|---|
| `first_seen_commit_exists` | `git cat-file -e <first_seen.hash>` succeeds |
| `last_modified_commit_exists` | Same for `last_modified.hash` |
| `first_seen_touches_file` | `git show --name-only <first_seen.hash>` includes the atom's `file_path` |
| `file_exists_at_head` | `file_path` is in the working tree at HEAD |

A `Fail` on `first_seen_commit_exists` is the **fabrication
signal** — the cited hash is not in the repo. The eval report
counts these separately as `fabricated_atoms` because they're
load-bearing for trust.

When `first_seen_commit_exists` fails, `first_seen_touches_file`
flips to `Stale` (we can't ask git about a commit it doesn't
have) rather than `Fail`. This keeps the staleness signal from
silently masking a real fabrication.

### Inquiry-driven (run only when an inquiry targets the atom)

| Kind | What it asks |
|---|---|
| `keyword_present` | At least one inquiry keyword appears in some commit subject in the file's history |
| `author_present` | `primary_authors` overlaps inquiry's `authors` |
| `date_in_range` | `last_modified.date_iso` falls within `inquiry.date_range` |

Each atom's **score** is `passed / (passed + failed)`. `Stale`
counts are tracked but excluded from the score so a partial-clone
repo doesn't deflate the metric unfairly.

## Inquiry schema

A single inquiry is one TOML file under `inquiries/`. Minimum
shape:

```toml
[inquiry]
id = "callers_callees_twin_tools"
title = "callers/callees code-intel tools share a lineage"
file_globs = [
    "**/sovereign-tools/src/code/callers.rs",
    "**/sovereign-tools/src/code/callees.rs",
]
```

Anything else is optional and adds a witness check on top:

```toml
keywords = ["callers", "callees", "scip", "graph"]
authors  = ["maintainer@example.com", "maintainer@example.com"]
min_score = 0.75   # per-atom witness score required to pass

[inquiry.date_range]
start = "2026-04-20"
end   = "2026-05-08"
```

Globs support `*` (within a path segment) and `**` (across path
segments). Match is case-sensitive, target is the
`AtomProvenance.file_path` rendered as a string.

The inquiry **passes** when:

- At least one atom matched the globs, AND
- Every matched atom's witness score ≥ `min_score`

If no atom matched the globs at all, the inquiry **fails** with
`no atoms matched file_globs …` — that's the regression signal
when archaeology output structure changes.

## Reading the report

The markdown report at `~/.svrnmesh/eval/<atlas>.eval.md` has
four sections:

### Witness rollup
Pass / Fail / Stale counts grouped by witness kind. The headline
counter — `100% witness rate · 0 fabricated · 1/1 inquiries
passing` — lives at the top.

### Inquiries
Per-inquiry verdict (`✓` / `✗`), matched-atom count, aggregate
score, and the first three failure notes when it didn't pass.

### Lowest-witness atoms
Up to 10 atoms with the lowest score (and at least one Fail).
Where you start when investigating a regression.

### Baseline diff
`Δrate ±N.NN%`, then four lists: added / removed / score-changed
(↑ or ↓) / path-changed atoms. Score-changes are sorted with the
biggest improvements first.

## CSV history

Every run appends one row to `~/.svrnmesh/eval/history.csv`:

```
timestamp,atlas,atoms,witness_rate,fabricated,baseline_score_changes,inquiries_passing,witness_rate_delta
2026-05-08T05:41:00Z,sovereign-self-atlas,1872,1.0000,0,0,1/1,+0.0000
```

The columns to watch over iterations:

- `witness_rate` — what fraction of all witness checks passed
- `fabricated` — how many atoms cite non-existent commits
- `inquiries_passing` — N/M ratio across your regression suite
- `witness_rate_delta` — change vs the saved baseline

`witness_rate` at 1.0 is the expected ceiling for v1
(mechanically derived). The metric earns its keep with v2 atoms
where the LLM is in the loop.

## Pre-push ratchet

The gated eval can run as a pre-push hook so commits that *don't
introduce a regression* sail through, but commits that drop the
inquiry-passing count or introduce fabricated atoms are blocked
before they leave the laptop.

```bash
# One-time setup (per checkout):
git config core.hooksPath .githooks
```

The hook at `.githooks/pre-push` runs:

```
sovereign archaeology-eval <repo>-self-atlas \
    --inquiries-dir inquiries \
    --gate-on-baseline
```

The `--gate-on-baseline` flag changes the exit-code semantics:

| Comparison vs baseline | Exit | Effect |
|---|---|---|
| Same passing-inquiry count, same fabricated count | 0 | Push allowed |
| More passing OR fewer fabricated | 0 | Push allowed (improvement) |
| Fewer passing OR more fabricated | 1 | Push blocked, regression report on stderr |
| No baseline saved yet | 0 | Push allowed (fresh repo) |
| Daemon / CLI / inquiries dir missing | 0 | Push allowed (offline-friendly) |

**Override path:** `git push --no-verify`. Use sparingly; pair
with a memory note (`sovereign code reflect --content "bypassed
pre-push because …"`) so the next session's brief sees the
context.

**When to update the baseline:** after an intentional improvement
(`--save-baseline` once the eval shows the new state is better),
or after a deliberate regression with documented rationale (e.g.
a doc was retired without a replacement principle yet).

## Workflow

The intended **run-measure-iterate** loop:

1. Build / rebuild the structural atlas. (Or accept that yesterday's
   is fine — staleness will tell you if it isn't.)
2. `sovereign git-archaeology <atlas>` — emit sidecar.
3. `sovereign archaeology-eval <atlas> --inquiry inquiries/*.toml`
   — score it. Read the lowest-witness atoms section. If
   `witness_rate < baseline`, investigate before promoting.
4. When confident the run is an improvement (rate up, no new
   fabrication, no inquiry regressions), `--save-baseline`.
5. Iterate: change a threshold, swap a model (when v2 lands),
   tighten a prompt — re-run and let the diff tell you whether
   it was actually better.

The CSV is your changelog. Eyeball it; the trend should climb
monotonically over weeks if you're doing it right.

## Exit codes

- **0** — every always-on check passed AND every inquiry passed.
- **1** — at least one fabricated atom OR at least one inquiry
  failed.
- **2** — argument parsing failed.

This shape is CI-friendly: drop `archaeology-eval` into a
GitHub Action or local pre-commit and your ratchet stays bound.

## Out of scope (for now)

- **Adversarial probes** — `probes.yaml` with planted decoys
  (e.g., "decisions that look sequential but were actually
  parallel"). Inquiries cover the positive case; probes cover
  the negative. v2.
- **Human spot-check sample pack** — `archaeology-sample N`
  emits a markdown sheet of N randomly-sampled atoms with their
  citations + a verdict slot for human review. Gold-standard
  signal, expensive in your time. v2.
- **Sub-witness checks** — verifying that a cited commit's body
  contains a specific quoted phrase, not just a keyword. Useful
  for v2 Lineage atoms whose claims include verbatim reasoning.
- **Cross-atlas eval** — running the same inquiry suite against
  multiple atlases simultaneously and surfacing aggregate rates.
  Today the eval is per-atlas; cross-atlas dashboards are v2.
- **Eval inside drift orchestrator** — `sovereign drift detect`
  runs archaeology automatically (Step 3.5) but does not run
  eval. v2 will fold the eval verdict into the drift report's
  header.

## See also

- [`docs/PLAN_ALIGNMENT.md`](./PLAN_ALIGNMENT.md) — the
  plan-time analogue of the pre-push ratchet. Forces the four
  alignment questions before any code is generated; this doc
  catches the regressions that slip through.
- [`docs/GIT_ARCHAEOLOGY.md`](./GIT_ARCHAEOLOGY.md) — what eval
  evaluates.
- [`docs/DRIFT_DETECTION.md`](./DRIFT_DETECTION.md) — eval is the
  upstream check before the drift report is trusted.
- `corpus-engine/src/archaeology_eval.rs` — types, witness
  checks, baseline diff, glob matcher, tests.
- `crates/sovereign-cli/src/archaeology_eval_cmd.rs` — CLI
  surface, markdown rendering, CSV append.
- `inquiries/` — curated regression cases. Each one is a
  permanent unit test of "what good looks like."
