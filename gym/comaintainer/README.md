# The comaintainer gym — judgment episodes mined from four months of case law

The second bank in the `gym/` genre (`gym/next-edit/golden/` is the
first). It scores a **(model, charter)** pair on typed-verdict
agreement against the house's own recorded verdicts: 301 episodes
mined from the ledger, verdict commits, notes, invariants, operator
transcripts, and fix-chain diffs. Vision: `docs/COMAINTAINER.md`.
The charter under test: `CHARTER.md`. The ruler: `contract.txt`.

## Predeclared margin — written BEFORE the first charter run (§18.5)

**Charter v1 is promoted only if, on the tier-A holdout (n=72), its
exact-6 agreement beats the charter-less baseline by ≥10 points with
non-overlapping 95% Wilson intervals, AND its citation `basis-exists`
is ≥80%.** A miss is reported honestly and iterated on the dev split
only; the holdout stays frozen (BENCH_LOOP discipline).

## Taxonomy

Episode: `{request: {situation, proposal, evidence}, expect: {verdict,
<arg>, basis, rationale}}` — `request` is the ONLY block a candidate
sees. Six verdicts (closed set, `markers.VERDICTS`): approve(citations),
revise(ask), measure-first(instrument), split(scopes),
escalate(question), could-not-judge(missing).

Tiers: **A** = settled by a later instrument (236) — gates score tier A
only; **B** = operator-settled, no instrument (31) — training breadth,
never gating; **C** = inferred (fix-chains, 30-day-quiet receipt
commits; 34) — weakest labels, never gate.

Split: stratum `(source, tier, verdict)`, every 3rd by id → holdout.
Stamped at harvest, recomputed by the validator from the same
`markers.split_of` — a mismatch cannot ship.

## Distribution (bank of 2026-08-06, 301 episodes)

| source | n | what it carries |
|---|---|---|
| ledger | 20 | DEFAULTS_LEDGER rows: DARK→measure-first, REJECTED→revise, GRADUATED→approve, OWED→measure-first(B), OPT-IN→revise(B) |
| commit | 31 | verdict-bearing commit messages (reject/approve) + ≤20 receipt-bearing benign commits ≥30d quiet (approve, C) |
| attempt | 14 | failed approaches; result sentences move expect-side |
| decision | 44 | agent-authored decision notes with ≥2 measurement hits and one verdict family |
| tripwire | 50 | invariants negated in prose (planted revise) + compliant twins (approve) — §18.1 pairs |
| constructed | 59 | smell-table rows, escalate/CNJ/measure-first scenarios from house case law, bundled-order splits |
| transcript | 25 | operator interventions (revise) and go-aheads (approve), mined locally; tier B |
| fixchain | 14 | introducing-commit diff reviewed with the later fix as answer key; tier C, never gates |
| twin | 44 | -t1 evidence-stripped→measure-first (31), -t2 artifact-elided→could-not-judge (13), from tier-A separable parents |

Classes: approve 101 / revise 105 / measure-first 50 / split 13 /
escalate 15 / could-not-judge 17. Max class share 35% (the ceiling).
Tier-A holdout (the HARD set): **72** — approve 22, revise 21,
measure-first 15, escalate 5, could-not-judge 5, split 4.

**Class-cap deviation from the plan, recorded:** the plan's caps
(revise 130 / approve 120) breach the 35% ceiling at the realized bank
size; with the four small classes supplying ~95 episodes the algebra
pins both large caps at ≤113 (`markers.py`). Bank target 380–420 was
supply-limited for the same reason; 301 ≥ the 300 floor.

## Constant-verdict floors (tier-A holdout, analytic — `--constant`)

approve 30.6% · revise 29.2% · measure-first 20.8% ·
could-not-judge 6.9% · escalate 6.9% · split 5.6%.
**Any headline below ~31% is indistinguishable from a coin with a
favorite side.**

## Audit (label precision, 2026-08-06)

Two-pass audit of an 80-episode sample (every transcript episode +
every-k-th of the rest), auditor: Claude (this session); operator
spot-check of flagged + every 5th + ALL transcript episodes pending —
**transcript episodes' inclusion in the committed bank is contingent
on that operator pass.**

- **Pass 1 (pre-fix bank): 69/80 labels plausible (86%).** The 11 bad
  labels fell in four classes, ALL made structural and re-verified
  gone: harness noise as transcript proposals (API errors / login
  prompts), interrupt-then-"continue" mislabeled revise,
  AskUserQuestion boilerplate as corrections, decision notes carrying
  both a delivery and a sub-rejection (now dropped as
  `decision_two_verdicts_inseparable`, 30 dropped), ship-plus-reject
  commits (now dropped), junk twin instruments ("bench that" — the
  extractor now requires a lane-shaped name or falls back to a
  by-reference instrument).
- **Pass 2 (final bank): transcript class re-read clean (25/25);
  twin instruments clean; decision verdict mix sane (30 approve /
  14 revise).**
- **Known residual weaknesses, documented not hidden:** attempt notes
  often state the failure in their own first sentences, so ~4/5
  sampled attempt proposals partially reveal the conclusion — they
  measure whether a judge respects recorded failure, not blind
  discovery. Fixchain tier-C asks inherit the fix commit's subject,
  which sometimes names a feature rather than a defect (1/1 sampled)
  — tier C never gates. Constructed split proposals built from
  decision-note prose are wordy.
- **Leakage:** 0 hits from the shared linter (`markers.lint_leaks`,
  same function at harvest and validation; 44 leaky episodes dropped
  and counted at harvest). One SEMANTIC leak class the regexes cannot
  see was audit-found and fixed at the source (the ledger's "What
  shipped instead" bullet revealed the house's call; it no longer
  enters requests). `leak_secret`: **0 hits** (required for commit).

## Run commands

```bash
python3 gym/comaintainer/harvest_episodes.py            # rebuild bank (deterministic)
python3 gym/comaintainer/validate_episodes.py           # exit 0 = clean; 4 = empty
python3 gym/comaintainer/validate_episodes.py --audit-sample 80
python3 gym/comaintainer/score.py --constant approve    # analytic floor, no model
python3 gym/comaintainer/score.py --charter none        # baseline (daemon engine)
python3 gym/comaintainer/score.py                       # charter run
python3 gym/comaintainer/score.py --rescore gym/comaintainer/runs/<stamp>  # zero calls
python3 gym/comaintainer/score.py --engine claude --limit 60  # paired slice (budgeted ≤190)
```

Runs land under `gym/comaintainer/runs/` (gitignored) with FULL raw
completions + bank/charter/contract sha256s; headline numbers are
committed here. Engine of record: the daemon
(`FINAL-Bench_Darwin-36B-Opus-Q6_K` as configured primary, temp 0).

## Results

<!-- filled by the measurement pass; noise floor FIRST (§18.4) -->
- Noise floor (two identical `--charter none` holdout runs): _pending_
- Charter-less baseline (tier-A holdout exact-6): _pending_
- Charter v1 (tier-A holdout exact-6 + basis-exists): _pending_
- Paired engine slice (daemon vs claude, 60 holdout episodes): _pending_

## Honest limitations

- Expected verdicts are the HOUSE's verdicts, not ground truth from
  the universe: an episode's "right answer" is what this repo's later
  instruments or operator settled, tiered accordingly.
- Transcript and note anchors resolve only on this host; committed
  snapshots (`provenance.note_snapshot.head200`) carry a stub for
  everyone else, and the validator prints `NOTES_DB_ABSENT` rather
  than silently passing (§18.3).
- `HOLDOUT-EMPTY` strata exist for singleton (source, tier, verdict)
  cells; the validator names them on every run.
- The bank is deterministic (no RNG) but NOT frozen: rebuilding after
  ledger/notes/git changes yields a different bank. The committed
  `cases.jsonl.gz` + sha256 in each run's meta is what a number refers
  to.
