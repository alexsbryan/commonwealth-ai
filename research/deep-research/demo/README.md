# T1a product demo — the loop, measured

The deep-research product demo (order `deep-research-t1a`, scene DEMO-1 in
`research/deep-research/DEMO_PLAN.md`): one bank seed question completes
end-to-end through the shipped CLI path — not a bench fork — and the estate
compounds. This directory is the demo's flight record.

**The scene.** Run 1 asks the estate first (honestly: it owns nothing yet),
names its gaps, acquires evidence, and returns a report where every claim
carries a verdict. Then the same question is re-asked with the compounded
corpus: the estate already has the answer, the run costs seconds and fetches
nothing. Acceptance is the operator holdout — the operator runs a question of
their own choosing in the product; their read of the answer is terminal
authority (DEMO_PLAN).

**The instrument discipline.** The bank is an instrument: run it, never edit
it. All runs use the shipped binary
(`target/debug/sovereign-cli deep-research ... --run-dir ... --max-rounds 3`),
the daemon on :9741 with Qwen3.6-35B-A3B-MTP-UD-Q6_K (draft) and
Qwen3-Embedding-0.6B-Q8_0 (embed), and the budgeted egress (DDG search +
page fetches only).

---

## The measurement history (real runs, honest outcomes)

The demo was the loop's first real measurement. Every defect caught was
fixed structurally with a watched test, never whack-a-mole.

| Run | dir | outcome | wall | what it measured |
|---|---|---|---|---|
| 1 | `runs/dr-1786720152` | DonePartial, 0 searches, 0 fetched, honest empty survey + refusal | 2s | **F16**: the empty estate was refused by a stale precondition — the loop refused to search an empty estate, so the web leg never ran. Fixed: `precondition_empty_estate_is_searchable` |
| 1b | `runs/dr-1786720584` | failed (empty-results kill) | 7s | DDG served two real result pages (200, has_result_a) then began 202-blocking mid-run. **F-empty-results-kill**: an empty (blocked) search result was an `Err`, which aborted the run. Fixed: empty results are a record, not a failure (`empty_results_are_a_record`) |
| 1c | `runs/dr-1786720828` | failed (transition gap) | 4s | DDG 202 on all three rounds (block sustained). **F-transition**: max_rounds exhausted at `(Rounding, BudgetExhausted)` had no transition row — the loop died instead of finishing honestly. Fixed: `(Rounding, BudgetExhausted) => Synthesizing` + watched test |
| 2 | `runs/dr-1786726986` | DonePartial, 1 Tavily search, 3 public-web fetches, partial answer | 43s | **The web leg, Tavily-keyed.** Backend honestly named in the console: `tavily keyed, duckduckgo (fallback)` — key from the operator's env, value never logged; `budget.tavily daily_calls=100` now applied on this path from the house defaults. Landing date/time claim passed with chunk citation; crew gap honestly could-not-judge. DDG was 202-blocked all day and the block lifted 17:00 PDT (monitor-confirmed) — the recorded fallback is live |
| RA-1 | `runs/dr-1786727099` | DonePartial, 2 Tavily searches, 4 off-topic fetches (museum-grant pages) | 30s | **F-snippet**: the estate snippet was the first 240 chars of each chunk — for long pages that is nav/donate boilerplate; the round-0 draft anchored on the donate blurb, and the gap-derived web query ("support helps fund exhibitions…") returned museum-grant pages. Fixed: `estate_snippet` centers on the deepest first-occurrence query term, function words filtered, 200-char lead (`estate_snippet_centers_on_query_terms_not_nav_chrome`, `estate_snippet_falls_back_to_prefix_without_query_terms`) |
| RA-2 | `runs/dr-1786727417` | DonePartial, 2 Tavily searches, 4 fetches (Agnew pages), **estate answered July 20 1969** | 37s | The compounding act works at the answer level: the landing claim passed **citation_grounded to the personal corpus** (`estate:apollo11-evidence`). The web leg did NOT collapse — see "The collapse bar" below |

Plus the same discipline's non-run defects:

| Defect | measured | fix |
|---|---|---|
| Empty-window gap list queried nothing | run 1's gap shape | gaps carry the prior gap list; an empty window queries the question itself (`empty_window_gap_queries_the_question`) |
| Budget ledger leaked to the process CWD | stray `budget-ledger.json` at repo root, written at run 1c's timestamp with an empty run_id | the placeholder decider was removed — the decider is born with real identity and its journal is always `<run_dir>/budget-ledger.json` (`start_journals_the_budget_ledger_only_inside_the_run_dir`) |
| Estate listing said `chunks_count: 0` for a 28-chunk corpus | RA-1's survey | `corpus_chunk_count` read the schema-v2 `chunk_count` key; the engine's meta schema v3 carries it as `next_chunk_id` (with `chunks_expected` null). Reads `chunks_expected` then `next_chunk_id` |
| Terminal poll 404'd the real daemon | run 2's preflight against the restarted daemon | the poll probed `/models`; the daemon's canonical surface is `/v1/models` (every other CLI consumer probes it). Fixed to `/v1/models` |

The stage strip — `artifacts (flight recorder)` — prints after Rendering
with its artifact list (charter, plan, survey, draft, gap list, verdict
set, report, manifest), and every run refuses to pass a charter it has not
validated.

---

## Run 2 (Tavily-keyed, completed 08-14)

```bash
cd /home/alexbryan/dev/commonwealth-ai
target/debug/sovereign-cli deep-research \
  "When did the Apollo 11 mission land on the Moon and who were its crew members?" \
  --run-dir research/deep-research/demo/runs --max-rounds 3
```

The web leg ran on the operator's Tavily key (`SOVEREIGN_TAVILY_API_KEY`,
bridged to the canonical `SVRNMESH_*` at CLI startup; read once via
`rebrand::svrnmesh_env` in the CLI process — the daemon never sees the key,
the fetch path is CLI-side — and declared in `quality/env-flags.toml`).
DuckDuckGo stays registered as the zero-config fallback (it was 202-blocked
during runs 1b–1c, unblocked 17:00 PDT, monitor-confirmed). Result:
`WALL_SECONDS=43`, 1 Tavily search, 3 public-web fetches (Wikipedia, NASA,
Smithsonian), landing claim passed with chunk citation, crew gap honestly
could-not-judge (report `runs/dr-1786726986/report.md`, manifest with
custody stamps, budget ledger `web-search:tavily: 1`).

---

## The compounding act (executed)

1. `extract-evidence.sh` pulled the run's WindowChunks into
   `apollo11-evidence/` (provenance headers preserved).
2. `sovereign corpus ingest apollo11-evidence --corpus apollo11-evidence` —
   28 chunks, local-only, personal custody.
3. Re-ask with `--corpora apollo11-evidence` (RA-1, then RA-2 after the
   snippet fix).

Re-ask numbers, recorded: RA-2's report cites the personal corpus
(`estate:apollo11-evidence`) for the landing claim — the answer the empty
estate could not give in run 2 — but the fetch count was **4**, not 0, and
wall time was 37s vs run 2's 43s: the collapse did not happen.

---

## The collapse bar (not met — escalated, not papered over)

The re-ask promise was ~0 fetches + wall-time collapse. RA-2's residual web
spend is fully accounted for, and every cause is **judge-side** (sovereign-core
`deep_research::audit`), not the CLI port — so per ARCH §18.6 this is
escalated to the seat rather than silently tuned:

1. The round-1 gaps were only the Agnew/Johnson claims (`gap-list-1.json`):
   the witness's specific-extractor produced a phantom specific — "Date:
   1973 (inauguration)" — present in neither claim nor evidence, which
   alone flips the witness to all-absent.
2. The other gap failed containment on paraphrase strictness: the estate
   evidence says "launched from Cape Kennedy", the claim says "pad 39A" —
   same fact, token-different, judged absent.
3. The round-2 draft claimed "none of the provided sources list the crew
   names" — a false negative (the names were in the round-0 window), which
   the judge PASSED as an un-witnessable negative claim.

The compounding act's mechanism is proven (estate-answer + personal-custody
citation); the fetch-collapse bar waits on the witness fix, which is the
seat's call. The operator holdout remains scheduled as the demo's terminal
acceptance.

---

## Acceptance

The demo's acceptance is the operator holdout (DEMO_PLAN): the operator runs
a question of their own choosing through the shipped path and reads the
answer. This record is the run-up; the holdout is scheduled as the terminal
verdict.
