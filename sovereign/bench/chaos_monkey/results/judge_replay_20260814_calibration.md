# Per-register operating curves — and land C re-armed through replay (D1+D2 of order judge-calibration-replay)

2026-08-14. All model calls on the local daemon (model `primary`,
http://localhost:9741) — zero external model tokens. Every number below has
its n and the naive baseline beside it (E-naive-baseline). Verdict files:
`judge_replay_20260814_{main,landc}.verdicts.jsonl`, joined report
`judge_replay_20260814_two_arm_report.json`, case set
`../judge_replay_cases_v1.jsonl` (regeneration commands in the README).

## Instrument validation, before any result (ARCH §18.4)

- **Byte-faithfulness**: 217/217 per_claim_judge rows across the three prior
  forensics ledgers rebuild the recorded `n_shared` exactly (147 landed14 +
  39 arm2 + 31 d0pre; the extractor refuses to emit on any mismatch). The
  portfolio-baseline ledger (pbase14, commit 4cb8ee5c) joins the pool the
  same way.
- **Replay reproduces the production run**: on same-build (landed14) cases,
  replayed vp equals recorded vp to 4 decimals (deltas 0.0000); across the
  23 labeled forensics cases with recorded vps, zero verdict flips, mean
  |vp delta| 0.0039, max 0.0695 (the max is a cross-BUILD case — d0pre was
  recorded pre-land-B, honest drift, not noise).
- **Bit-stability**: mechanical facet — two `--render-only` runs render 41/41
  identical (prompt FNV + declared boundary). Model facet — `--repeat 2` on
  both arms: max vp spread 0.000000 over 32 (main) / 30 (landC) repeated
  cases. Verdicts are bit-stable on this host.
- **Cost**: one full arm = 41 cases ≈ 6 min on the daemon, vs the 30-40 min
  live adversarial arm it replaces for judge-input questions.

## Register 1 — `claim_violation_joint` (the production long-form register)

Label supply, stated honestly: **20 negatives / 8 positives**, every one
hand-read (etiology specimen table 2eef5117, notes 95b82f97 / 139ab0be /
d474ac24, and this order's cross-arm derivation of the land-C clearances).
Labels are SUPPORT-IN-VIEW. Two contested rows (the Hume+Hobbes-conditional
twins) were deliberately left unlabeled rather than anchor the curve — the
register's own prompt licenses cross-passage assembly, and the two hand-reads
disagree.

Main (register fingerprint `cab517e9d980c22d`), operating curve:

| tau | catch-rate (20 neg) | clear-rate (8 pos) |
|---|---|---|
| 0.70 | 1.000 | 0.750 |
| 0.80 | 1.000 | 0.750 |
| 0.85 | **1.000** | **0.750** |
| 0.90 (current) | 0.900 | 0.750 |
| 0.95 | 0.900 | 0.750 |
| 0.98 | 0.700 | 0.875 |

Naive baselines on the same label set: always-flag = catch 1.000 / clear
0.000; always-clear = catch 0.000 / clear 1.000. The register beats both at
every tau in [0.70, 0.95]; at 0.98 its catch (0.700) is the number to watch,
not celebrate.

**Finding 1 — tau=0.85 dominates tau=0.90 on the labeled set** (catch 1.000
vs 0.900, clear identical). The two catches 0.90 loses are the pinned
officials-selfinflicted (vp .853) and motive-coverup (vp .881) specimens.
Cost side, measured on the full recorded population (93 audit passes, 623
per-claim rows): 9 rows sit in [0.85, 0.90) and **4 of 93 passes (4.3%)
would newly pay a rewrite** at 0.85. Hand-grep of those 9: mixed — some are
summary-carriage negatives, but at least two ("Incompatibilists hold that
agents act freely only if determinism is false", "John Locke is historically
debated…") look supported-in-view, i.e. new false failures. **Because the
initiative objective is latency with no quality hit, this order does NOT
move the production tau**: 0.9 stands, and the 0.85 trade (2 real catches
gained vs ~4.3% more rewriting turns of mixed merit) is recorded here as an
operator decision with its numbers attached.

**Finding 2 — the residual false-positive mechanism is dilution, not window
membership.** The seat's handoff specimen (pbase14 79179042-c1, "Hobbes
distinguished general necessity imposed by nature…") is leaf[18]-supported,
fully in a 36-chunk view, and still fails: replayed main vp .9679 (recorded
.9669). A dozen near-identical wordings in the same ledger clear at
.0068-.70. Same fact, same window, verdict rides the phrasing — the
register is unstable under a 36-chunk load. Window-size-conditioned tau or
claim-conditioned window ordering are the candidate levers; both are
register changes and now cost ~6 min each to price through this harness.

**Finding 3 — two positives no tau can rescue**: James-coined-hard-determinism
(vp .9845, verbatim at leaf[13]) and the dilution specimen (.9679). They cap
clear-rate at 0.750 until ~0.98. These are register errors, not threshold
errors.

## Register 2 — `chunk_judge` (singular, the bench-critic register)

Calibration artifact remains the bench critic lane (`live_runner.rs`,
tau=0.9 provenance). This order adds 3 replay-parity smoke cases, not a new
calibration (label supply 2 pos / 1 neg — far too thin for a curve, stated
as such). Observed supports: Luck-Objection .97 (correctly high),
James-coinage-of-both-terms .52 (uncomfortably low for a verbatim-supported
claim — same dilution-adjacent softness, single specimen, no conclusion),
bondage-coinage .002 (correctly low).

## Register 3 — `specifics_scan` (generative — no scalar, no curve)

Label supply: 9 item-level labels (6 should_not_flag, 3 should_flag) — a
finding in itself: the should-flag side is thin because the scan's genuine
catches were never hand-inventoried. Main replay: 3/10 correct — it flags
all six should-not-flag items (reproducing the recorded false positives,
including items verbatim in its own view). The scan is currently a
false-positive engine on this bank; every FP feeds the repair chain.

## D2 — land C, re-armed replay-first: **REJECTED, second and final**

Candidate = build `measure/land-c-replay` (main 4cb8ee5c + cherry-pick
87017ca9 + harness; register fingerprint `76e41703c60e1f61`). The
pre-registration (139ab0be) kill list, checked case by case:

| pinned specimen | main vp | landC vp | at tau 0.9 |
|---|---|---|---|
| c-clear-lessa-watch | .970 | **.639** | **C loses a catch main makes — (c)-class** |
| c-clear-evidence-chain-indiaman | 1.000 | 1.000 | both flag |
| c-clear-officials-selfinflicted | .853 | .305 | both clear (main too — see Finding 1) |
| c-clear-motive-coverup | .881 | .015 | both clear; C's margin collapses |
| b-clear-salvage-pattern-hook | .990 | .997 | both flag |

- **(c)-class loss: YES** — lessa-watch, the cleanest live-arm specimen,
  reproduced offline: flagged by main, cleared by C. Kill condition hit.
- **Material vp compression: YES** — mean vp delta C−main = **−0.085 on
  negatives** (3 of 20 drop >0.1) and −0.085 on positives. The d474ac24
  hypothesis (C2's less-decisive system turn compressing the A/B margin) is
  confirmed offline: C is globally softer — it cures two false positives
  (clear-rate 1.000 at tau .95; the dilution specimen drops to .451) by the
  same mechanism that surrenders real catches.
- Labeled curve at 0.9: C catch 0.850 vs main 0.900; C would need tau ≈ 0.6
  to recover main's catch-rate, at which point nothing is calibrated.
- Scan under C: 5/10 vs main's 3/10 — better on false positives, but it
  LOSES a should-flag (the Keynes parametric garnish). Reported both
  directions (§18.6).

Two of the order's kill conditions are met; **C does not ship**. This is the
second honest rejection, now with the mechanism quantified and a ~6-minute
regression gate (`--filter c-clear`) that any future register change must
pass before a live arm. The latency win C wanted (scan joins the judges'
prefix family) needs a system turn that does NOT soften the forced choice —
candidates can now be priced offline against this same case set.

## Standing duties (E-judge-adversarial)

The frozen-3 trend series and the dropped-catch read remain live-arm duties
for any judge change that SHIPS; nothing shipped here (C rejected, tau
unchanged), so the series is unchanged. The replay deltas-vs-recorded table
(zero flips on main) is the offline early warning, not a substitute.
