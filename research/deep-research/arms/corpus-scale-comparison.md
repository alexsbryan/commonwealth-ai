# T6a phase-1c — corpus-scale comparison: does acquisition volume move the score?

Order deep-research-t6a, phase 1c (the estate-as-brain probe). Declaration,
amendment, and execution record: `research/deep-research/adversarial/
pre-registration.md` §"T6a phase 1c". Scorer: `arms/score-arms.py` (invoked
only, unmodified — deterministic C-class, rules journaled in its header).
Same judge identity and calibration-gate caveats as the t5a declaration
(items 1-6 inherited verbatim; zero untraced figures in [passed] position is
the constitution, never the thing that gives).

Pre-registered flags, both legs: `--backend auto --search-source corpus
--search 40 --fetch 60 --max-rounds 6`. Thresholds untouched, named:
code-set K=3, eps-quota 0.1, evidence window 20 chunks (CLI defaults).
27B draft throughout (Qwen3.8-27B-UD-Q6_K_XL). The frozen bank/subset/vendor
are never edited; the one-shot comparator is the t6b root
(`arms/runs-t6b/oneshot` — 13 drafts + windows, arm-independent by
construction).

## The legs and comparators

| leg | corpus set | tasks | fetched chunks/task |
|---|---|---|---|
| thin (corpus-scale) | wikipedia only | 13 (seed-01..12 + v1) | 4-18 |
| warm (corpus-scale) | wikipedia + dr-estate-demo13-warm | 3 (seed-01..03) | DEFERRED (operator review) |
| batteries t6b / t6c | (their execution records) | 13 | 12/12, max-rounds 3 |
| deep arm (demo13/runs/deep) | web, 8-10 search / 4-12 fetch spent (manifests) | 10 | 0-7 (38 total) |

## The thin leg — estate at its floor

Flight facts: 13/13 landed, terminal done-partial, exit 0 (bank driver log
`demo/demo13/runs/corpus-scale/bank-driver.log` — "ALL FLIGHTS OK"; state
files `thin/*.state.json`). Wall times 366-2088s (total ~3h50m; co-tenancy
with the battery worker — upper bounds, journaled at pre-registration).

Per task (from `arms/score-report-corp-scale-thin.json`):

| id | wall s | searches | fetched | loop_covered | loop density | oneshot covered | oneshot density | P3 | R-12 |
|---|---|---|---|---|---|---|---|---|---|
| seed-01 | 1087 | 40 | 12 | 2/6 | 0.867 | 5/6 | 1.0 | passed | failed |
| seed-02 | 1985 | 28 | 8 | 3/6 | 1.0 | 6/6 | 0.857 | failed | failed |
| seed-03 | 960 | 40 | 11 | 3/7 | 0.143 | 6/7 | 1.0 | passed | failed |
| seed-04 | 2064 | 40 | 18 | 3/6 | 0.625 | 6/6 | 1.0 | failed | passed |
| seed-05 | 660 | 40 | 14 | 2/6 | 0.857 | 6/6 | 1.0 | failed | failed |
| seed-06 | 496 | 40 | 11 | 1/6 | 0.214 | 4/6 | 0.833 | passed | failed |
| seed-07 | 475 | 40 | 9 | 2/6 | 0.833 | 6/6 | 1.0 | failed | failed |
| seed-08 | 888 | 40 | 14 | 2/5 | 0.400 | 5/5 | 1.0 | failed | failed |
| seed-09 | 734 | 32 | 4 | 2/6 | 1.0 | 4/6 | 1.0 | passed | failed |
| seed-10 | 904 | 40 | 12 | 4/6 | 0.571 | 6/6 | 1.0 | failed | passed |
| seed-11 | 1083 | 40 | 14 | 1/6 | 0.741 | 6/6 | 1.0 | passed | failed |
| seed-12 | 366 | 33 | 4 | 5/6 | 0.333 | 6/6 | 1.0 | passed | failed |
| v1 | 2088 | 40 | 14 | 1/16 | 0.100 | 13/16 | 1.0 | failed | failed |

Pooled: loop density **0.57** vs oneshot **0.979**, lift −0.409; loop
ungrounded fraction **0.43** vs one-shot **0.021** (honesty bar); loop
verdict set across 263 claims: 9 passed, 17 failed, 237 could-not-judge.

Bars (the one decider's report, `score-report-corp-scale-thin.json` §bars):

| bar | thin measured | bar | verdict | battery t6b | battery t6c |
|---|---|---|---|---|---|
| P4-v0 | 30/72 | >=58/72 | **failed** | 70/72 passed | 68/72 passed |
| P4-v1 (loop) | 1/16 | >=12/16 | **failed** | 13/16 passed | 11/16 failed |
| P3 | 6/13 | >=10/13 | **failed** | 12/13 passed | 12/13 passed |
| R-12-nongrow | 2/12 | >=10/12 | **failed** | 0/12 failed | 0/12 failed |
| T1.7 plan presence | 12/12 | all scoped flights | passed | 12/12 passed | 12/12 passed |
| two-arm lift (pooled) | 0.57 vs 0.979 | loop >= one-shot + 0.10 | **failed** | 1.0 vs 0.979 failed | 0.797 vs 0.807 failed |
| two-arm lift (v1) | 0.1 vs 1.0 | loop >= one-shot + 0.15 | **failed** | 1.0 vs 1.0 failed | 1.0 vs 0.968 failed |
| honesty not worse | 0.43 vs 0.021 | loop <= one-shot | **failed** | 0.0 vs 0.021 passed | 0.203 vs 0.193 failed |

## Does volume move the score? — the thin answer

No — on the wikipedia-only estate, MORE acquisition budget made the score
WORSE, not better. At 40 corpus searches / 60 fetch allowance the loop
fetched only 4-18 pages per task (vs the batteries' 12-budget arms), landed
every flight done-partial, and covered 1-5/6 keys (batteries: 6/6). The
search cap was hit (28-40/task) but the fetch decisions stalled — seed-01's
rounds 2-4 fetched 0 (gap trace 8→8→8→44→17, the t6c growth-engine shape),
and the drafts' grounding collapsed: 0.43 ungrounded fraction vs the
one-shot's 0.021, and 237/263 loop claims could-not-judge, overwhelmingly
the ref-required no-citation-handle class the order's phase-2(a) names.

The volume lever, where it is real (the t5a hybrid web leg at 4+4: 8.0848 →
8.6538, +7.0%, same judge — pre-registration.md §T5a), is acquired WEB
content, not more searches over a thin estate. Searches are not content.

## The warm leg — floor + one acquired cache — DEFERRED (operator review)

The warm bracket (seeds 01-03, `--corpora wikipedia,dr-estate-demo13-warm`,
same budgets/rounds/thresholds, only the corpus set varying) was
pre-registered, assembled, and verified — the warm corpus
(`dr-estate-demo13-warm`, 37 md files from the deep flights' 38
evidence-window chunks, ingested via the shipped `svrn corpus ingest`,
searchable with live hits) is flight-ready — but the flight itself is
HELD under operator review of the queue (seat directive 2026-08-20).
It would have bracketed floor vs floor + one acquired cache directly;
in its place, the deep arm supplies the ceiling reading.

**The ceiling reading (deep arm, acquisition positive).** The deep arm is
the web-acquisition comparator: 10 flights, 8-10 searches / 4-12 fetches
spent per task (manifests), 38 fetched chunks in hand, and — per the
T6a-t6b pilot record (pre-registration.md §"T6a-t6b pilot", the ceiling
arm's 5 landed reports with PERFECT acquisition in hand) — the universal
profile is ZERO [passed] findings. Its verdict sets (300 claims across
10 runs, `verdict-set.json` per run): 1 passed (drb-90), 33 failed,
264 could-not-judge, 2 never-ran; the could-not-judge flags are
predominantly the ref-required no-citation-handle class and
extracted-specifics-absent (honest refusals, as designed). So even where
the acquisition volume is real web content sitting in the evidence
window, the bars do not move: the binding residual is draft yield against
the verifier (the order's phase-2(a) seam) and report shape (phase-2(b)),
not acquisition volume. That is also what the thin leg shows from the
other side — at the estate floor, more searches produced no more
fetchable evidence.

## The explicit answer — does volume move the score, and by how much?

From the flown data: **no — acquisition volume does not move the bars,
from either direction.** Up: the thin leg raised the search budget to
40/task (the batteries' 12-budget arms) and P4-v0 FELL from 70/72 and
68/72 to 30/72, loop density from 1.0 to 0.57, honesty from 0.0/0.203 to
0.43 ungrounded — because searches are not content: the loop fetched only
4-18 pages per task and its fetch decisions stalled (seed-01 rounds 2-4
fetched 0; gap trace 8→8→8→44→17). In: the deep arm put real acquired
web pages in the window (38 chunks) and still scored 1/300 passed,
0 [passed] in the perfect-acquisition reports. The measured residual is
the draft/verifier/report seam — the order's phase-2 items, now pinned
from both sides of the volume lever. The warm bracket (estate as an
input corpus) would have quantified floor vs floor+cache on identical
questions; it is deferred, and its absence does not change the thin-leg
bars or the deep-arm ceiling reading above.

## The floor reading — stated explicitly

Per the pre-registered steer: "Estate is sort of impotent until it is
acquired — right now we just have wikipedia and the few searches we've done.
The estate ends up being a cache of those heavy web search runs." The thin
leg measures the wikipedia-only estate at its **floor** — it is NOT the
volume ceiling. The warm leg would have bracketed floor vs floor + one
acquired cache (the deep flights' fetched pages); it is deferred under
operator review, and the deferral does not weaken the floor reading:
corpus volume without acquisition is not evidence.

## Flywheel disposition

Verified end-to-end on corpus-mode flights: every fetched source's manifest
entry carries `ingested_into` (seed-01, `thin/seed-01/dr-1787160713/
manifest.json` `/sources/fetched[0..2]/ingested_into =
dr-estate-dr-1787160713`), the estate corpus is created at flight time
(index dir mtime within minutes of the run id), state ready, live search
hits. The thin bank compounded 13 new estates.

The web arm persists estates too — this CORRECTS the pre-registration's
recorded measurement ("the deep flights created NO dr-estate-dr-* corpora…
none for the demo13 deep ts range", measured 2026-08-19 before any flight):
direct evidence — every deep run with fetched>0 carries `ingested_into` in
its manifest (drb-58/59/62/65/69/78/83/90/95; drb-56 fetched 0, none
expected), and the estate index dirs exist with creation times at flight
time, Aug 18 (e.g. `dr-estate-dr-1787068173` created 2026-08-18 09:43 PDT;
drb-90's run id 1787068173 ≈ 08:49 PDT start). All state=ready and
searchable today (verified live: `svrn corpus search
dr-estate-dr-1787068173 "liability ADAS driver assistance accident"` returns
the run's fetched pages). The recorded "no estates" measurement was a
listing miss, journaled as superseded; the flight log carries this
correction.

Enrichment surface: VERIFIED usable end-to-end. The deep flights' 38
evidence-window chunks were extracted to `demo/demo13/warm-sources/` (37 md
files) and ingested via the shipped `svrn corpus ingest` →
`dr-estate-demo13-warm` (state ready; live search probe returns hits). The
estate flywheel — heavy web runs → estate corpus → searchable by later
flights — therefore exists in both directions: web flights persist their own
estates, and the ingest surface can fold any run's evidence windows into a
named corpus. The warm bracket would have been the first measured use of an
estate as an input corpus set; it is deferred (operator review), not
retracted.

## Citations

- pre-registration.md §"T6a phase 1c" (declaration, amendment, execution
  record, flight log); §T5a items 1-6 caveats; 8.6538 reference at
  pre-registration.md:3357-3364.
- `arms/score-report-corp-scale-thin.json` (scored 2026-08-19; scorer
  score-arms.py, C-class).
- `arms/score-report-t6b.json`, `arms/score-report-t6c.json` (battery
  comparators, §bars).
- `demo/demo13/runs/corpus-scale/bank-driver.log` (flight records, wall
  times); `demo/demo13/runs/corpus-scale/thin/*.state.json`.
- `demo/demo13/runs/deep/drb-*/dr-*/verdict-set.json` + `manifest.json`
  (deep arm: 300 claims — 1 passed [drb-90], 33 failed, 264 could-not-judge,
  2 never-ran; 38 fetched chunks; `ingested_into` on every fetched source).
- `svrn corpus status` / `svrn corpus search` live probes (estate corpora,
  warm corpus) — this session.
