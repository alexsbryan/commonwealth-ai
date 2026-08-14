# ICD schemas — field-level shapes (D1) + R11-thin state machine

Order `deep-research-t1a`, build item 1. The order's D1: the ICD schemas'
field-level shapes designed, one golden fixture per boundary. The types
below are the contract the code implements verbatim
(`sovereign-core/src/deep_research/icd.rs` + `golden/`); this note
is the authoritative record, and the fixture set in `golden/` is the
qualification surface every boundary is tested against.

## §0 Frame

- **FR-2**: every inter-component payload is a serialized, versioned
  artifact in the run directory. Each artifact carries `icd` (the
  boundary name) + `version` at the top level; a parser that meets an
  unknown `icd` or `version` refuses (never silently skips, §18.3).
- **FR-3**: thresholds are frozen into `charter.json` at launch; every
  other artifact's `charter_hash` binds it to the charter that governed
  the run. The state machine reads the charter once.
- The run directory is the flight recorder: one artifact per boundary,
  append-only; the manifest at run close is the index.
- Naming: `<icd>-<round>.json` where round-scoped (survey, gap-list,
  fetch-list, skip-ledger, evidence-window, draft); run-scoped otherwise
  (charter, plan, budget-ledger, verdict-set, report, manifest).
- Wire spellings (stable, from the custody reds): custody classes
  `public-web | personal | peer` + the third variant `unknown`;
  verdicts `passed | failed | could-not-judge | never-ran` (§18.1).

## §1 charter.json — R0, frozen at launch (FR-3)

```json
{
  "icd": "charter", "version": 1,
  "run_id": "dr-8f3a2c1e",
  "question": "…",
  "seed_id": "seed-1",
  "created_at_unix": 1786710000,
  "charter": {
    "max_rounds": 3,
    "evidence_window_max_chunks": 12,
    "containment": { "trigger": "judge-supported", "extraction_max_tokens": 32, "specifics_max": 4 },
    "triage": { "code_set_k": 6, "eps_quota": 0.1 },
    "budget": { "web_search_queries": 12, "web_fetch_pages": 6 },
    "custody": { "stamp_required": true, "unknown_refuses": true },
    "url_constraint": { "enabled": true, "layer": "prompt+renderer-verify" }
  },
  "frozen": true
}
```

| field | meaning |
|---|---|
| `seed_id` | bank provenance (`seed-N` from `research/deep-research/bank/seeds.md`); null for operator-authored questions |
| `max_rounds` | slot deadline in rounds (F28): the audit's gap set must shrink strictly each round, else the run terminates at the slot deadline with gaps declared |
| `containment.trigger` | the C-class witness fires on judge-`supported` claims only (gate-redesign.md) |
| `triage.eps_quota` | fraction of below-cut hits admitted for fetch (R5 ranker: code-set K + ε-quota) |
| `budget` | the run-scoped allowance seeded into the SpendDecider (web-search half) |
| `custody.unknown_refuses` | a claim resting on unknown-provenance evidence must refuse (R-3) |

Golden: `golden/charter.json`. Every later fixture's `charter_hash` is
sha256 of this file's bytes.

## §2 plan.json — R1

```json
{
  "icd": "plan", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…",
  "rounds_planned": 3,
  "estate_first": true, "network_after_estate": true,
  "acquisition": { "queries_preplanned": [], "source": "gap-driven" }
}
```

R1 is thin at T1: no preplanned queries — acquisition is driven by the
R3 gaps (the compass mechanism). `estate_first` is the charter-level
declaration that no network call precedes the estate survey (F16).

## §3 survey-<round>.json — R2 (estate survey, existing-first)

```json
{
  "icd": "survey", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…", "round": 1,
  "estate_precondition": { "asserted": true, "estate_searchable": true, "detail": "corpus-engine index reachable; 0 corpora" },
  "estate_corpora": [
    { "corpus_id": "dr-8f3a2c1e-fetch", "kind": "knowledge", "chunks_count": 0, "searchable": true, "custody": "public-web" }
  ],
  "searched": [
    { "query": "Meridian Bridge Selune river", "hits": [ { "chunk_id": "…", "corpus_id": "…", "score": 0.42, "url": null, "custody": "personal" } ] }
  ],
  "estate_answer": "…"
}
```

| field | meaning |
|---|---|
| `estate_precondition` | **F16 assert**: the estate was asked and is searchable before any network call. The loop refuses R4 while this record's `asserted` is false — the precondition is an artifact, not a memory |
| `estate_corpora[].custody` | per-corpus custody class (the estate's own corpora are `personal`; the loop's fetched corpus is `public-web`) |
| `searched[].hits[].url` | null for estate chunks (no source URL — they are personal material, not fetches); non-null is a web-fetch signature (R-2) |
| `estate_answer` | the round's estate-only draft (the "what we already own" answer) |

## §4 gap-list-<round>.json — R3 (the compass output)

```json
{
  "icd": "gap_list", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…", "round": 1,
  "claims": [
    { "id": "c1", "text": "The Meridian Bridge was completed in 1873.",
      "verdict": "passed", "evidence_ids": ["ev-1"],
      "witness": { "ran": false }, "action": "citation_grounded" },
    { "id": "c2", "text": "Its engineer was Helena Voss, who also designed the Larkhall viaduct.",
      "verdict": "could-not-judge", "evidence_ids": [],
      "witness": { "ran": true, "specifics": ["Helena Voss", "Larkhall viaduct"], "all_absent": true },
      "action": "abstained_decline", "empty_evidence_window": false }
  ],
  "gaps": [
    { "id": "g1", "text": "the engineer's identity is not corroborated by the estate",
      "actionable_query": "Helena Voss Larkhall viaduct engineer", "from_claim_id": "c2" }
  ],
  "empty_evidence_windows": [],
  "strict_subset_of_prior": true
}
```

| field | meaning |
|---|---|
| `verdict` | one of the four; `never-ran` appears when the audit could not execute for a claim (empty evidence window with no draft, provider failure) — never defaulted (§18.3) |
| `witness` | the containment witness record: `ran`, `specifics` (extracted), `all_absent` — the downgrade evidence |
| `action` | the gate action family from the custody reds: `citation_grounded \| abstained_decline \| rewrite_annotated` (+ `refused_*` on unknown provenance) |
| `gaps[].actionable_query` | search-actionable phrasing — the compass's output that drives R4 (the hand-run recipe's shape) |
| `strict_subset_of_prior` | the dr-compass terminal test: round-N gap set ⊆ round-(N-1)'s; false with no new gaps at the slot deadline → terminal |

## §5 fetch-list-<round>.json — R4 (query forming + search) + R5 (triage)

```json
{
  "icd": "fetch_list", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…", "round": 1,
  "queries": [ { "id": "q1", "text": "Helena Voss Larkhall viaduct engineer", "from_gap_id": "g1", "formed_by": "generator", "provider": "local" } ],
  "search_hits": [
    { "id": "h1", "query_id": "q1", "url": "https://…", "title": "…", "snippet": "…", "engine": "duckduckgo" }
  ],
  "triage": { "code_set_k": ["h1"], "eps_admits": ["h3"], "below_cut": ["h2", "h4"], "threshold": 0.55, "eps_quota": 0.1 }
}
```

The ranker is a **ranker, never an excluder**: `eps_admits` carries the
ε-quota of below-cut hits; everything rejected lands in the skip ledger
(F25) with a reason. Every search call in this round appears in the
budget ledger (R-6 — one decider, fail-closed).

## §6 skip-ledger-<round>.json — F25

```json
{
  "icd": "skip_ledger", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…", "round": 1,
  "entries": [
    { "url": "https://…/h2", "title": "…", "score": 0.31, "rank": 2, "reason": "below-cut", "decision": "skipped" },
    { "url": "https://…/h4", "title": "…", "score": 0.44, "rank": 4, "reason": "below-cut", "decision": "skipped" }
  ]
}
```

A skip is a recorded fact, never a silent drop: every hit that does not
enter the fetch list appears here with `reason ∈ below-cut | duplicate
| unfetchable`. The demo's "what was NOT covered" section traces through
this ledger.

## §7 budget-ledger.json — the one decider's journal (R-6, budget-decider.md)

```json
{
  "icd": "budget_ledger", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…",
  "allowance": { "web_search": 12, "web_fetch": 6 },
  "entries": [
    { "family": "web-search", "key": "duckduckgo", "units": 1, "at_unix": 1786710100, "decision": "allow", "reason": null },
    { "family": "web-search", "key": "duckduckgo", "units": 1, "at_unix": 1786710500, "decision": "refuse", "reason": "allowance-exhausted" }
  ],
  "spent": { "web_search": 12, "web_fetch": 5 },
  "remaining": { "web_search": 0, "web_fetch": 1 }
}
```

Fail-closed table (from budget-decider.md §2): no allowance record /
ledger read failure / unknown family-or-key / exhausted → `refuse` with
a recorded `reason`; remaining → `allow` then decrement. The loop's
search + fetch paths consult **this decider only** — the two legacy
fail-open deciders are not in the loop's path (their supersession is
T2). `allowance-exhausted` refusals are what drive the `done-partial`
terminal at the budget check.

## §8 evidence-window-<round>.json — R6 (fetch + ingest, custody-stamped)

```json
{
  "icd": "evidence_window", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…", "round": 1,
  "chunks": [
    { "id": "ev-1", "locator": "web-1",
      "source_url": "https://…", "custody": "public-web", "provenance_class": "known",
      "content": "…", "ingested_into": "dr-8f3a2c1e-fetch",
      "tags": ["meridian-bridge", "1873"] }
  ],
  "fetch_failures": [ { "url": "https://…/blocked", "error": "http-403", "absent": true } ],
  "derived_custody": "public-web"
}
```

| field | meaning |
|---|---|
| `custody` | stamped by the fetcher — code, never a model (custody.md §2); the exact wire spelling |
| `provenance_class` | `known \| unknown`; `unknown` (unstamped/sealed/pinned) is a third variant that **refuses** (R-3) |
| `fetch_failures[]` | failures recorded absent per source — a failed fetch is a named artifact, not a hole |
| `derived_custody` | max-restrictiveness join over the window's chunks, computed **at creation** (custody.md §3) — `personal > peer > public-web`; `unknown` poisons the join |
| `ingested_into` | the estate corpus the fetched chunk was written into — the estate compounding |

Terminal-state poll (F17) runs between fetch batches: an abort request
or the slot deadline lands the run in Rendering with truncation declared
before any further fetch.

## §9 draft-<round>.json — R8 (local synthesis)

```json
{
  "icd": "draft", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…", "round": 2,
  "provider": "Qwen3.6-35B-A3B-MTP-UD-Q6_K",
  "url_constraint": { "enabled": true, "layer": "prompt+renderer-verify" },
  "text": "…", "citations": [ { "evidence_id": "ev-1", "url": "https://…", "custody": "public-web" } ]
}
```

Synthesis runs local-only with the URL constraint on: citations must
resolve against the window's URLs; `layer` records the enforcement —
prompt-side allowlist + the renderer's C-class verification (a citation
whose URL is not in the window is a fabrication → flagged, never
silently removed). The token-level `UrlAllowlistConstraint` masking is
wired where the provider seam exposes it; the renderer verification is
the guarantee that always exists.

## §10 verdict-set.json — R9 (claim splitter)

```json
{
  "icd": "verdict_set", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…",
  "claims": [
    { "id": "c1", "text": "The Meridian Bridge was completed in 1873.",
      "verdict": "passed", "status": "supported",
      "evidence_ids": ["ev-1"],
      "citations": [ { "evidence_id": "ev-1", "url": "https://…", "chunk_id": "ev-1" } ],
      "flag": null },
    { "id": "c2", "text": "Its engineer was Helena Voss.",
      "verdict": "could-not-judge", "status": "open", "evidence_ids": [],
      "citations": [], "flag": "specifics absent from evidence" }
  ]
}
```

| field | meaning |
|---|---|
| `status` | `supported \| flagged \| open \| not-covered` — the report's four display states, derived from the verdict: passed→supported; failed→flagged; could-not-judge→open; never-ran→not-covered |
| `citations[].chunk_id` | chunk-level citations: each claim's supporting evidence resolves to window chunks with URLs (R-2 surfaces) |
| `flag` | never-remove: a failed/never-ran claim is carried with its reason, never stripped from the report |

## §11 report.md — the rendered artifact (R9)

The report is the product artifact. Sections, in order:
1. Header: question, run id, terminal state, date.
2. **TRUNCATED** banner when `truncation_declared` (abort/slot-deadline).
3. Verdict-stamped claims — `[passed] [flagged] [open] [not-covered]`
   tags, each claim carrying its chunk-level citations as
   `(#ev-N — url)` handles.
4. **Open questions** — every `could-not-judge` and `never-ran` claim,
   with the actionable gap query.
5. **Manifest** — sources fetched (url + custody), sources failed (url +
   error), budget spent/remaining, **what was NOT covered** (traced
   through the skip ledger), round timeline.
6. Evidence appendix — the window's chunks by `EvidenceId`.

## §12 manifest.json — run close

```json
{
  "icd": "manifest", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…",
  "terminal_state": "done", "aborted_at_round": null, "truncation_declared": false,
  "rounds": [
    { "round": 1, "gaps_before": 2, "gaps_after": 1, "fetched": 3, "search_calls": 4 }
  ],
  "sources": {
    "fetched": [ { "url": "https://…", "custody": "public-web", "ingested_into": "dr-8f3a2c1e-fetch" } ],
    "failed": [ { "url": "https://…/blocked", "error": "http-403" } ]
  },
  "budget": { "spent": { "web_search": 4, "web_fetch": 3 }, "remaining": { "web_search": 8, "web_fetch": 3 } },
  "not_covered": [ "g2: the viaduct's completion year" ],
  "reframe": {
    "icd": "reframe", "version": 1, "run_id": "dr-8f3a2c1e", "charter_hash": "…",
    "round": 2, "original_question": "…", "reframed_question": "…",
    "reason": "the loop spun", "trigger": "structural surprise: …"
  },
  "lock": { "id": "dr-8f3a2c1e", "acquired_at_unix": 1786710000, "released_at_unix": 1786710600 }
}
```

Terminal states are **distinct and distinctly reported**: `done` (no new
gaps), `done-partial` (budget exhausted / slot deadline with gaps
remaining), `aborted` (operator abort — report still rendered, gated,
truncation declared). The lock record (F19) shows the run-scoped lock's
lifecycle; a second run against the same run dir refuses at
acquisition.

`reframe` is `null` (absent) unless the loop re-framed (GAP-4, §13
below): then it carries the stewardship record — original question,
reframed question, the round it fired at, and the operator's stated
reason. The report NAMES the substitution on its header line; the
question swap is never silent.

## §13 R11-thin state machine — enumerated states, one transition table

```
Initializing ──charter+plan──▶ Planning ──────────────▶ Rounding
                                                            │
   Rounding (per round): Surveying → Auditing
      │ no new gaps (strict_subset terminal) ──────▶ Synthesizing
      │ budget exhausted at check ─────────────────▶ Synthesizing (done-partial)
      │ structural surprise (GAP-4): round ≥ 2, gap list unchanged,
      │   last acquire round fetched nothing, input staged
      │   ──ReframeRequested──▶ Reframing ──ReframeWritten──▶ Planning
      │      (writes reframe-1.json, re-plans as plan-2.json through
      │       the SAME PlanWritten row — ONE enumerated re-plan)
      │ else → Querying → Triage → Fetching → Enriching → budget check → Surveying
Synthesizing → Rendering → Done | DonePartial
EVERY state ──abort──▶ Aborted ──▶ Rendering (truncation_declared=true)
```

| rule | enforcement |
|---|---|
| Enumerated states + transitions | a typed enum; a transition not in the table is a compile error (FR-1) |
| Abort from every state | the abort signal is a state-machine input in every state's transition set; landing is Rendering with `truncation_declared` |
| Slot deadline (F28) | `max_rounds` from the charter; non-strictly-shrinking gap set at the deadline is terminal with gaps declared |
| Run-scoped lock (F19) | flock on `<run_dir>/lock`; second opener refuses; lifecycle recorded in the manifest |
| Re-frame is input-gated (GAP-4) | the trigger cannot fire without a staged `reframe-input.json` (`{"question": …, "reason": …}` — a typed input, malformed JSON refuses loudly); fires at most ONCE per run; the reframed question drives every later draft, gap query, and the report |
| Never silently substitute (§18.3) | the reframe record is written (`reframe-1.json`), carried in the manifest, and the report names the swap on its header line |

## §14 Golden fixtures — one per boundary

`sovereign-core/tests/golden/` holds one fixture dir per consistent
synthetic run (the "Meridian Bridge" seed, the same flavor the custody
reds use): `run-meridian-1/` at the top level of `tests/golden/` —
charter, plan, survey-1, gap-list-1, fetch-list-1, skip-ledger-1,
budget-ledger, evidence-window-1, survey-2, gap-list-2, draft-2,
verdict-set, report, manifest — the run's gap set strictly shrinks
(2 → 1) and terminates `done`. `run-reframe-1/` pins the GAP-4
boundary: a deterministic gym-deck drill (garbage judge + scripted
draft + clean deck) with a staged `reframe-input.json` — charter,
plan, plan-2, reframe-1, gap-list-1/2, draft-1/2, verdict-set, report,
manifest — the loop spins (gaps unchanged, nothing fetched) and the
re-frame fires at round 2. Each boundary's test parses its fixture,
validates it (required fields, enum spellings, charter_hash), and
qualifies the boundary against it. The demo's live run produces the same
shapes; the fixtures are the qualification surface, the demo is the
product proof.
