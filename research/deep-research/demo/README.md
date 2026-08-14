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

The demo was the loop's first real measurement. Three runs, five defects
caught — every one fixed structurally with a watched test, never
whack-a-mole.

| Run | dir | outcome | wall | what it measured |
|---|---|---|---|---|
| 1 | `runs/dr-1786720152` | DonePartial, 0 searches, 0 fetched, honest empty survey + refusal | 2s | **F16**: the empty estate was refused by a stale precondition — the loop refused to search an empty estate, so the web leg never ran. Fixed: `precondition_empty_estate_is_searchable` |
| 1b | `runs/dr-1786720584` | failed (empty-results kill) | 7s | DDG served two real result pages (200, has_result_a) then began 202-blocking mid-run. **F-empty-results-kill**: an empty (blocked) search result was an `Err`, which aborted the run. Fixed: empty results are a record, not a failure (`empty_results_are_a_record`) |
| 1c | `runs/dr-1786720828` | failed (transition gap) | 4s | DDG 202 on all three rounds (block sustained). **F-transition**: max_rounds exhausted at `(Rounding, BudgetExhausted)` had no transition row — the loop died instead of finishing honestly. Fixed: `(Rounding, BudgetExhausted) => Synthesizing` + watched test |

Plus two more defects caught by the same discipline, outside the runs table:

| Defect | measured | fix |
|---|---|---|
| Empty-window gap list queried nothing | run 1's gap shape | gaps carry the prior gap list; an empty window queries the question itself (`empty_window_gap_queries_the_question`) |
| Budget ledger leaked to the process CWD | stray `budget-ledger.json` at repo root, written at run 1c's timestamp with an empty run_id | the placeholder decider was removed — the decider is born with real identity and its journal is always `<run_dir>/budget-ledger.json` (`start_journals_the_budget_ledger_only_inside_the_run_dir`) |

The stage strip — `artifacts (flight recorder)` — printed in run 1 with its
artifact list (charter, plan, survey, draft, gap list, verdict set, report,
manifest), and the run refused to pass a charter it had not validated
(hash `e9970c1e007111e4`).

---

## Run 2 (pending — blocked on DuckDuckGo)

The fixed binary is rebuilt and armed; the run cannot complete while DDG
202-blocks this host's IP. Evidence of the block:
`run1b-console.log` (two 200s then 202) and `run1c-console.log` (202 ×3).
Probe: `/tmp/ddg_probe.sh` (the orchestrator's exact request shape). The
block began 08-14 ~08:16 PDT (mid run 1b) and the response body is DDG's
anomaly/challenge page (bot detection, not a rate limit). Run 1b proves the
same request shape was served two 200s-with-results earlier that morning —
the block is rolling-uncertain, not demonstrated sustained. A 30-minute
monitor watches for the unblock; if the block exceeds ~4-6h total, the
order's escape clause ("cannot complete end-to-end after reasonable
effort") is invoked and the evidence re-escalated.

```bash
cd /home/alexbryan/dev/commonwealth-ai
target/debug/sovereign-cli deep-research \
  "When did the Apollo 11 mission land on the Moon and who were its crew members?" \
  --run-dir research/deep-research/demo/runs --max-rounds 3
```

Verify, when it runs: honest empty survey → gap carrying the question as its
query → DDG hits → custody-stamped fetches → grounded draft → report with
all four verdicts, citations, Open questions, and the manifest — and the
stage strip (it prints only after Rendering). Wall time recorded here with
`WALL_SECONDS=`.

---

## The compounding act (recipe)

The re-ask needs the estate to own the evidence. Per run 2's
`evidence-window-{round}.json` chunks:

1. Extract each chunk to `<folder>/<chunk-id>.md` with a header block
   (`source_url:`, `locator:`, `custody:`, `provenance_class:` — the
   `WindowChunk` ICD fields) and the chunk `content` as the body.
2. `svrn corpus ingest <folder> --corpus apollo11-evidence` (default corpus
   id = folder basename; daemon embed path).
3. Re-ask: the same question with `--corpora apollo11-evidence`.

Re-ask numbers to record here when it runs: `fetched sources: 0`, wall-time
collapse vs run 1, and the report's citations now pointing at the personal
corpus instead of the web.

---

## Acceptance

The demo's acceptance is the operator holdout (DEMO_PLAN): the operator runs
a question of their own choosing through the shipped path and reads the
answer. This record is the run-up; the holdout is scheduled as the terminal
verdict.
