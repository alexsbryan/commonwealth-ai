# DEMO-9 — the DRB between-arm measurement (order `deep-research-t2b`)

The measured half of PLAN.md §4 T2: the P2 between-arm fabrication
measurement on the frozen DRB subset, the P1 cost arm against a named
proxy, and the restated kill bar — **ship iff P4 AND P2 AND P1**.

Everything here is pre-registered and frozen before the first flight
(`research/deep-research/adversarial/pre-registration.md`); the score
files (`score-local.json`, `score-hybrid.json`, `score-hybrid-delta.json`)
are scorer output, not this page. The bar text in `bars.md` mirrors
`quality/initiative-bars.toml` (id `dr-verdict`) verbatim.

## What ran

The frozen subset: tasks 56, 58, 59, 62, 65, 69, 78, 83, 90, 95
(n = 10, hashes verified, never edited). Two arms, paired on the same
tasks:

- **local** — corpus leg only (`--search-source corpus --corpora
  wikipedia`), the t1h boundary.
- **hybrid** — the rung-3 web leg through the t2a boundary
  (`--search-source web --consent personal`), run-scoped consent grant.

Judge: pinned local, `Qwen3.6-35B-A3B-MTP-UD-Q6_K` at daemon :9741
(pre-registered default; the external frontier judge was the named
fallback and was never needed). All 20 flights terminal
(`done-partial`), driver log `../drb/runs/driver.log` — ALL FLIGHTS OK.

## The measured numbers

| arm    | pairs | judged (supported+unsupported) | supported | unsupported | unknown | K/N (supported / (supported+unsupported)) | pooled fabrication | paper-mean fabrication (Eq 4-5) | cluster-bootstrap 95% CI |
|--------|-------|------|-----------|-------------|---------|-------------------------------------------|--------------------|--------------------------------|--------------------------|
| local  | 219   | 85   | 11        | 74          | 134     | 11/85 = 0.1294                             | 0.8706             | 0.9244                         | [0.7241, 1.0000] (dropped 2/10000) |
| hybrid | 117   | 112  | 72        | 40          | 5       | 72/112 = 0.6429                            | 0.3571             | 0.4864                         | [0.2564, 0.4554] (dropped 0/10000) |

Pooled fabrication = unsupported/(supported+unsupported); unknown
verdicts are neither. The bootstrap resamples 10 tasks, 10,000 reps,
seed 4234932947 = sha256("deep-research-t2b-bootstrap-2026-08-17"),
CI = 2.5/97.5 percentiles — all as pre-registered. Reference:
perplexity-Research fabrication 0.1737 (primary), openai-deepresearch
0.2499 (secondary), both from the frozen `../drb/leaderboard.csv`.

## Verdicts

- **P2 (gate arm: hybrid)** — cluster-bootstrap 95% CI [0.2564, 0.4554]
  vs reference 0.1737. Four-verdict: **failed** (CI lower ≥ reference;
  the interval does not straddle). The local arm (CI [0.7241, 1.0000])
  also fails. The between-arm delta (hybrid − local) is −0.5134
  [−0.6232, −0.3941] — **descriptive only**, as pre-registered; it is
  not a pass and the P2 gate stands on the hybrid arm.
- **P1** — mean cost/task: local $0.000573, hybrid $0.000315;
  max(arm means) $0.000573 < $1.45 proxy (o3-deep-research, frozen in
  `../drb/p1-cost-reference.md`). Four-verdict: **met**.
- **P4** — carried from t1h (63/72 coverage floor, not re-measured):
  **passed**.
- **Kill bar** — ship iff P4 AND P2 AND P1: **failed** (P2 failed).
  As the bar says, cheapness is never a pass: P1 met on cost alone does
  not ship.

## Per-task rates

Local arm (`task = <id>`, fab = unsupported/(supported+unsupported);
pairs = claims the extractor anchored to a reference):

| task | pairs | supported | unsupported | unknown | fab |
|------|-------|-----------|-------------|---------|-----|
| task = 56 | 12 | 0 | 2 | 10 | 1.000 |
| task = 58 | 28 | 2 | 4 | 22 | 0.667 |
| task = 59 | 25 | 0 | 0 | 25 | 1.000 |
| task = 62 | 0  | 0 | 0 | 0  | 1.000* |
| task = 65 | 42 | 0 | 22 | 20 | 1.000 |
| task = 69 | 13 | 0 | 8 | 5  | 1.000 |
| task = 78 | 43 | 6 | 13 | 24 | 0.684 |
| task = 83 | 56 | 3 | 25 | 28 | 0.893 |
| task = 90 | 0  | 0 | 0 | 0  | 1.000* |
| task = 95 | 0  | 0 | 0 | 0  | 1.000* |

Hybrid arm:

| task | pairs | supported | unsupported | unknown | fab |
|------|-------|-----------|-------------|---------|-----|
| task = 56 | 0  | 0 | 0 | 0  | 1.000* |
| task = 58 | 12 | 7 | 4 | 1  | 0.364 |
| task = 59 | 15 | 11 | 4 | 0  | 0.267 |
| task = 62 | 0  | 0 | 0 | 0  | 1.000* |
| task = 65 | 10 | 6 | 3 | 1  | 0.333 |
| task = 69 | 25 | 12 | 12 | 1  | 0.500 |
| task = 78 | 18 | 15 | 3 | 0  | 0.167 |
| task = 83 | 7  | 4 | 3 | 0  | 0.429 |
| task = 90 | 15 | 6 | 7 | 2  | 0.538 |
| task = 95 | 15 | 11 | 4 | 0  | 0.267 |

\* Zero-pair tasks: the flight's claims carried no citation apparatus
(no `citations[]`, no `[Source: …]` tails), so nothing anchored to a
reference. Per the pre-registered drop rule they contribute fab = 1.0
to the paper mean and nothing to the pooled rate. This is real flight
behavior (local 62/90/95, hybrid 56/62), not a scoring artifact; the
hybrid arm anchored far more of its claims than the local arm, which
is the asymmetry the delta is read against.

## Attribution — what failed, named

The failure is the system's honesty claim, not a scoring defect, and
the failed claims are attributable per arm:

- **Local arm (74 unsupported of 85 judged)**: the corpus-leg flights
  overwhelmingly produced claims the pinned judge marked unsupported —
  statements the reference evidence did not support. 134 of 219 judged
  statements were unknown (the judge could not tell), and the paper
  mean (0.9244) shows the drop-rule contribution of the zero-pair
  tasks. The local-only leg does not approach the reference.
- **Hybrid arm (40 unsupported of 112 judged)**: materially better —
  the web leg's evidence anchored claims and the judge supported 72 —
  but the fabrication rate still sits above both references, and the
  CI lower bound (0.2564) clears the primary reference (0.1737), so
  the improvement is measured and real, yet not a pass.
- **Zero-pair flights (local 62/90/95, hybrid 56/62)** are attributed
  as flights whose drafts never anchored claims to windows — they
  cannot be defended, and the drop rule reports them as fab = 1.0
  rather than hiding them.

Two instrument defects were found and fixed during the battery, both
journaled as NAMED AMENDMENTS (4, 5) before their re-runs, both fixed
red-first at the production site: the estate-snippet char-boundary
panic (local 95) and binary-payload refusal + control-char drop
(hybrid 56, three PDFs refused and journaled in the manifest). A
daemon restart during the hybrid arm was journaled as a BATTERY EVENT
and the affected flights re-run per §4. None of these changed the bar,
the judge pin, or the subset.

## Files

- `score-local.json`, `score-hybrid.json`, `score-hybrid-delta.json` —
  scorer output (judge pinned local, bootstrap seed 4234932947).
- `bars.md` — the dr-verdict bar and both transitions verbatim.
- `../drb/` — the frozen subset, SHA256SUMS, scorer, the run evidence.
- `verify-demo9.sh` — re-checks every claim on this page at landing.
