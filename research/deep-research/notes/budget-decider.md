# Budget-decider unification — design note (order `deep-research-t0b`, red R-6)

**Status:** design only. Production is T2 (`dr-budget-one-decider`, PLAN
§6 bar 7: "one run-scoped fail-closed spend decider, persisted as an
ICD"). This note is the inventory + the design T2 lands. **No code
changes at this commit.**

## §1 Inventory — three facts, all fail-open

**Decider 1 — `BudgetView` (web-search backend selection).**
`studio/crates/sovereign-tools-base/src/web/search/orchestrator.rs:50-80`
defines a read-only view over `HashMap<String, u32>` of remaining units.
It has **no writer anywhere in the codebase**: its own doc comment
(lines 44-48) defers the store — "the budget store (a separate concern —
SQLite-backed counter that resets daily) owns the writes. Phase 2 ships
the read interface; the budget store lives in its own follow-up" — and
the follow-up never shipped. Every construction site is an empty view:
`web/mod.rs:176` `BudgetView::new()` ("Phase 6.5: thread real budget
through" — a TODO), `knowledge_lookup/mod.rs:322`
`BudgetView::default()`, and the orchestrator's own tests. The gate is
`budget_allows` (orchestrator.rs:222-230): local backends always pass;
external backends pass if `Some(_) | None => true` — **untracked =
unlimited**. A budget that cannot be written and defaults to spending is
a fail-open in both directions.

**Decider 2 — the monthly "web" budget (agent web_search tool).**
`studio/crates/sovereign-tools-base/src/search.rs:234-246`
(`check_budget`) and 248-259 (`decrement_budget`): a single SQLite row
keyed by the string `"web"`, monthly window, reset +30 days. `check_budget`
returns `_ => true` when no record exists — **no record = allowed** —
and `decrement_budget` swallows its own errors (`let _ =`). The
decrement happens after the search runs, so an error there leaves the
spend unrecorded but spent.

**No other spend metering exists.** The deep-research loop itself (the
future consumer of this decider) has no ledger; the dr arms' frontier-key
spend (o3/o4-mini-class calls — the seed-4 family) is unmetered. Two
deciders, two keys, two fail-open shapes, one missing store — and
nothing keying the spend that the dr bars actually measure.

## §2 The design — ONE run-scoped, fail-closed decider, persisted as an ICD

**One decider** (ARCH §10.6 — one implementation per threshold): a
`SpendDecider` owning the whole spend surface of a research run — both
meter families:

- **web-search** — external backend selection (supersedes `budget_allows`
  and `check_budget`; per-call decrement, not per-month);
- **frontier-key** — LLM API spend in the dr loop (per-completion
  decrement against the run's key allowance).

**Run-scoped** — the ledger is bound to one run (one seed through the
3-round recipe, one arm invocation — the unit the dr bars measure), not
to a global monthly counter. The operator's per-run allowance seeds the
ledger at run start; the run journal writes every decrement; the ICD
record closes with the run's final spend. Run-scoping is what makes the
dr bars honest: the cost side of P1 is measured per run, against the
allowance that was actually granted.

**Fail-closed** — every default refuses:

| situation | verdict |
|---|---|
| no allowance record for the backend/key | refuse |
| ledger read fails | refuse (never spend blind) |
| unknown backend / unknown key | refuse |
| allowance exhausted (== 0) | refuse |
| allowance remaining (> 0) | allow, then decrement |

This inverts both existing deciders' shapes (untracked = unlimited;
no record = allowed). The rationale is the instrument's charter: the dr
loop is a measurement instrument, and a spend gate that fails open
falsifies the cost side of every bar it feeds. A budget that defaults to
spending is not a budget; a gate that has not failed is not a gate
(ARCH §18.1).

**Persisted as an ICD artifact** (FR-2: "ICDs are the checkpoints" — the
plan extends it to name spend meters as ICDs, PLAN §1): the decider's
contract is a versioned ICD — inputs (per-run allowance, ledger),
outputs (allow/refuse with the reason), the two meter interfaces
(`WebSearchMeter`, `FrontierKeyMeter`), and the journal schema (one row
per decrement: family, key, units, run id, timestamp). The dr gates key
on the ICD's recorded spend; the R5 skip-ledger is the sibling ICD the
same discipline covers.

## §3 Transition

T2 lands the new decider and re-points the two existing call sites at
it — `budget_allows` and `check_budget`/`decrement_budget` are
**superseded, not paralleled** (one decider; the old read surfaces keep
their shape during the transition, then die). The `"web"` stringly key
dies with decider 2; keys become enum-typed meter ids. The two existing
fail-open sites are the R-6 red's targets: the red asserts the
fail-closed properties (missing record → refuse; read error → refuse)
that no current site provides.

## §4 Scope guard

Design only, per the order: no production code, no new decider, no
ledger schema file at this commit. T2 owns the ICD artifact, the
supersession, and the red's green run.
