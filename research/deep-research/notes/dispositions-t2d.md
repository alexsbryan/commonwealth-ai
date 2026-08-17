# T2d — the open-bar dispositions (consolidation record)

Order deep-research-t2d, executed 2026-08-17. Lane: disposition-and-
consolidation — verification over authorship, no new mechanisms, no
instrument patches (that space closed at t2c, directive 03a0ab98; the
bank stays frozen). This note is the consolidation record the order
required; the dispositions are journaled in
`research/deep-research/adversarial/pre-registration.md` (execution
section) and the bar transitions in `quality/initiative-bars.toml`.

## 1. dr-compass — failed (disposition)

The bar's `failed` transition now sits on `quality/initiative-bars.toml`
(dr-compass, on 2026-08-17). It cites:

- the **seven measurements** — R-12 0/12 at t1c, t1d, t1e, t1f, t1g, t1h,
  t2c, each journaled inside a dr-local-loop `failed` transition in the
  same file (2026-08-14 .. 2026-08-17), with the gap-growth sequence
  (1->7->7, 1->15->27, v1 1->26) and the "structural, unchanged shape"
  verdicts verbatim;
- the **structural cause** — the v0 decks are single-origin and the
  corroboration floor is never weakened: dr-corroboration is MET (a
  claim whose support set has <2 distinct origins caps at
  could-not-judge — F22, `sovereign/crates/sovereign-core/src/
  deep_research/gym.rs:828`; the floor is downgrade-only, per
  dr-corroboration's met transition), so every round-N audit caps
  claims and the gap set only grows. Strict shrink on >=10/12 is
  structurally unreachable under the met corroboration mechanism on
  the single-origin v0 estate — the convergence bar cannot clear on
  the estate it is measured against;
- the **convergence hypothesis banked as a heap item** — the re-cut
  path runs THROUGH the corroboration mechanism on a multi-origin
  estate (see §5); not an instrument patch.

## 2. dr-estate-integrity — met (verified at HEAD)

The bar's `met` transition now sits on `quality/initiative-bars.toml`
(dr-estate-integrity, on 2026-08-17), written by the verified outcome.
Clause verification at HEAD, READ-ONLY, with the evidence surfaces the
order named:

| Clause | Verdict | Evidence at HEAD (file:line) |
|---|---|---|
| (a) R6 stance item 2 — fetch failures recorded absent per-source | verified | `deep_research/mod.rs:1276-1277` (the F17 comment at the fetch call site), `mod.rs:1295-1299` (window.fetch_failures -> run failed_sources), `mod.rs:944` (report card carries them); `deep_research/fetch.rs:9-11` (module contract), `fetch.rs:62-84` (terminal-poll failure records every planned fetch absent:true, spends nothing), `fetch.rs:91-131` (per-fetch failures absent:true); F2 deck pins the per-fetch path (`gym.rs:1066`, `gym.rs:1167`) |
| (b) F17 — ingest-laundering asserts loud | verified | the T1 F17 wire is the terminal-state poll (PLAN.md §4 T1: "R6 (+ terminal-state poll, F17)"); declared `estate.rs:106-108`, polled FIRST in fetch_round (`fetch.rs:62-64`), Err branch is the loud assert (`fetch.rs:63-84`); F17 row `gym.rs:823`; fetch-side stamp watched by `fetch.rs:397` custody_join_max_restrictiveness. Named residual: the terminal-poll Err branch has no dedicated failing-terminal unit test (deterministic branch; the F2 deck pins the adjacent per-fetch path) |
| (c) F18 — dead-inference enrichment asserts loud | verified as the named v1 disposition | `gym.rs:824` names it: "enrich_window is C-class tags in v1 (no inference to die); the faithful-mode asserts are the T2 R7 regime" — a named substitution, §18.3; consistent with `enrich.rs:7-10` (tags derived deterministically, no model) and `enrich.rs:112-143` (its tests); watched/named discipline pinned by `gym.rs:850` |

Red check (the bar's red: F17/F18 asserts fail at HEAD) — not red: the
deep_research battery passes **88/88** (nextest, sovereign-vulkan
toolbox, 2026-08-17; `sovereign-test.sh --filter deep_research`).

## 3. Attribution — t1b serves: amendment

`dr-corroboration`, `dr-residue`, `dr-reframe` read met by the t1b
landing (verdict directive 90a064c4) but no order's `serves:` named
them — the coverage headline ("UNCOVERED BARS = 3 of 13"). Fixed by
amending the closed order's frontmatter:
`.sovereign/features/deep-research-t1b/order.md` — `serves:` now reads
`deep-research dr-local-loop dr-corroboration dr-residue dr-reframe`
(plus an `amended:` line for the record). The evidence events were
already on record (each bar's met transition cites its t1b landing
commit: b939bcf6, d2119001 + 8b41d725, 5169e236). The t1b order's own
D0 contract said "at landing the serves line is amended to name them" —
this closes that missed step.

## 4. Run-evidence tidiness — runs-drill-1786761309.log

The untracked t2c battery evidence
`research/deep-research/demo/p5/runs-drill-1786761309.log` is **already
committed** — swept into commit `07750430` ("noun convergence") by the
noun-convergence session's commit while this order was being drafted
(the seat confirmed: the working tree is clean, HEAD moved 586c1839 ->
07750430). Named decision: **nothing further to do** — no .gitignore
addition and no second commit; the file is tracked at HEAD and remains
the t2c battery evidence. `git ls-files research/deep-research/demo/p5/`
confirms it is tracked.

## 5. The convergence heap item

The convergence hypothesis is banked as a backlog item — note
`83849ebf-2b3b-4506-90e0-53066410da0a` (kind=todo, related_entity=backlog,
filed via `svrn backlog add` 2026-08-17, title "Fix R-12 convergence by
enabling multi-origin estate", scored Value 5 axis A Grounded, Cost L,
unvetted — greyed until a person clears `Scored-by:`): the re-cut path
runs through the corroboration mechanism on a multi-origin estate — a
future re-cut order builds or designates an estate whose evidence spans
>=2 distinct origins per claim and re-runs R-12's 12-question battery
with the floor as the verdict dimension, as the mechanism already is.
Explicitly NOT an instrument patch (the filed Approach says it: "without
modifying prompts or judges"). It serves PLAN.md §3 R-12 green /
dr-compass (>=10 of 12 strict shrink).

## 6. Files touched and the git invocation

- `quality/initiative-bars.toml` — two transition rows only (dr-compass
  failed, dr-estate-integrity met); bar text FROZEN; no noun-convergence
  hunks touched.
- `.sovereign/features/deep-research-t1b/order.md` — serves: amendment.
- `research/deep-research/notes/dispositions-t2d.md` — this note.
- `research/deep-research/adversarial/pre-registration.md` — execution-
  section append (dispositions journaled, never silent).

One git invocation, local only, no push. No .rs file moved — the
lint/test gate's full run was not required (the order's Done-when (f)
permits stating that none did); the clause-verification battery ran
88/88 instead. The heap item lives in the notes store, not the repo.
