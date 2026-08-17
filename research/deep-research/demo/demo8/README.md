# DEMO-8 — the egress boundary and the rung-3 web flight (order deep-research-t2a)

Order `deep-research-t2a` — T2's trust-boundary half (PLAN §4) + rung 3 of
the acquisition ladder. The demo closes the order's done-when (h): the v1
report-class question rendered by a loop whose acquisition searches the
REAL WEB, every web-fetched chunk stamped public-web custody, every
egress decision — released or refused — traced at `tracing=debug`
through the ONE boundary, and the default-deny refusal case demonstrated
side by side:

> "How did American cities change across four decades (1980-2024):
> gentrification, inequality, affordability, and displacement — every claim
> cited?"

Two flights of the SAME question and the SAME search source, differing in
one flag:

| flight | invocation | outcome |
|---|---|---|
| refusal | `--search-source web` (no `--consent`) | the first query egress REFUSES, typed, naming what was withheld — exit 1, zero web egress |
| consent | `--search-source web --consent personal` | the run-scoped grant releases the machine-formed query egress; every fetched chunk lands with public-web custody; done-partial, exit 0 |

The boundary (pre-registered `adversarial/pre-registration.md`,
Instruments 1-3 + the same-day amendments, BEFORE any flight — §18.6):
ONE choke point in `sovereign-core/src/egress.rs` for remote-model calls
AND query egress. The release rule: public-web custody releases
unconditionally; a run-scoped `ConsentGrant { run_id, granted_at_unix,
release_floor }` releases what its floor covers (personal covers all,
peer covers peer + public-web, public-web covers public-web only);
a query formed verbatim by the user releases; everything else refuses,
typed. `Unknown` custody always refuses. Every event — released or
refused — is traced at `tracing=debug` under `sovereign_core::egress`
(the deep-research verb now installs the tracing subscriber with that
target at debug by default — the pre-flight observability amendment).

The two reds, both measured at HEAD before the fix:

- **R-5** — a personal-corpus chunk could reach a remote payload via
  `enrich --provider` with zero privacy tokens (nothing refused). The
  red-first test (`sovereign/crates/sovereign-cli-llm/src/enrich_cmd/
  egress_reds.rs:96`) failed at HEAD and passes with ZERO assertion
  changes after the landing: the enrich dispatch verifies before any
  request is built, the gate keying on the derived local-daemon base,
  not on the provider's name.
- **R-6** — the budget deciders were all fail-open and none run-scoped
  (BudgetView had no writer anywhere; `budget_allows(None) => true`;
  a second monthly fail-open decider in studio search.rs). The landing
  removes them; the F26 census's r6 gate scans every production src
  tree for the retired identifiers and reads zero.

The F26 census (`sovereign/crates/sovereign-core/tests/f26_egress_census.rs`)
is the build gate: at HEAD it counted FIVE egress-class client
construction sites outside any boundary (inference_client,
deep_research_cmd, knowledge_lookup, web/mod, conversation.rs — the
landing review corrected the red's count and the record is amended in
pre-registration); the landing registers ZERO outside the boundary, and
the census runs in the standard test suite (9907 tests green).

## What is in this directory

| File | What it is |
|---|---|
| `report-web.md` | The consent flight's report (verbatim from the run dir) — verdict-stamped claims, chunk-level web citations |
| `manifest.json` | The consent flight's terminal manifest (verbatim) — the consent grant record, the public-web custody stamps on every fetched source, the budget totals |
| `charter.json` | The consent flight's frozen charter (verbatim) — the consent grant frozen at launch (FR-3), the custody policy (`stamp_required`, `unknown_refuses`), the budget allowance |
| `egress-trace.log` | The consent flight's egress trace (stderr, ANSI-stripped) — 4 query releases under the run grant + 4 public-web url releases |
| `refusal-transcript.log` | The refusal flight's stderr (ANSI-stripped) — the typed default-deny refusal, the egress DEBUG event, the loud failure |
| `refusal-exit.txt` | The refusal flight's exit code (1) |
| `consent-budget-ledger.json` | The consent flight's budget ledger (verbatim) — 8 allow decisions journaled before each spend, both families |
| `refusal-budget-ledger.json` | The refusal flight's budget ledger (verbatim) — the single attempted spend journaled, the allowance consumed by the attempt |
| `bars.md` | The two bar transitions (dr-egress, dr-budget-one-decider) — the measured evidence, carried verbatim from `quality/initiative-bars.toml`, never hand-typed |
| `verify-demo8.sh` | The strips — the demo is only as strong as its verification |
| `README.md` | This file |

The raw run dirs live under `demo/demo8/runs/dr-*/` (gitignored —
ephemeral flights; the committed copies above are the record) — the
per-round fetch lists (every search hit stamped `engine: web` with
custody `public-web`), the evidence windows (chunks with custody
`public-web`), the skip ledgers, the gap lists, the drafts.

## What the boundary did on these flights

1. **The refusal case (default-deny).** Same question, same source, no
   `--consent`: the run charters, plans, aligns — and the FIRST query
   egress refuses before any request is built. The refusal names what
   was withheld: `egress refused: query with personal custody to tavily
   — no run consent grant — the boundary is default-deny for
   non-public-web payloads (grant absent — default-deny)`. Exit 1,
   loud — never a silent empty round, never a done-partial that looks
   like success (the F28 shape, measured). The refusal run's ledger
   journals the single attempted spend — an allowance unit is consumed
   by the attempt, recorded first. The run dir contains NO fetch list:
   zero acquisition spend, zero egress.
2. **The consent flight (run-scoped typed grant).** `--consent personal`
   mints the grant once, at launch: `ConsentGrant { run-id:
   dr-1786940569, granted-at-unix, release-floor: personal }` — frozen
   into the charter (FR-3, the pre-registered Instrument 2 contract),
   carried by the port to the boundary at every dispatch, recorded in
   the manifest. The egress trace shows the release rule deciding: 4
   query releases under the run grant (provider, custody, exact-payload
   size, run id, release floor) and 4 url releases on public-web
   custody (fetching the evidence pages). Every search hit is stamped
   `engine: web`, and every fetched source — and every evidence-window
   chunk — carries custody `public-web` (code, never a model).
3. **The ONE decider.** Both flights spent through the ONE SpendDecider
   (`deep_research::budget`, run-scoped, fail-closed): the consent
   flight's ledger journals 8 allows (4 web-search + 4 web-fetch) before
   each spend and lands at exactly 0 remaining; the frontier-key family
   is declared and INERT until t2b (no allowance is ever seeded; an
   attempted spend refuses `no-allowance-or-exhausted` — pinned by the
   `fail_closed_table` test). The old fail-open deciders are gone; the
   census's r6 gate reads zero identifiers across every production src
   tree.
4. **The honest report.** The consent flight is `done-partial` with
   truncation declared: 21 verdict-stamped claims, one passed, twenty
   could-not-judge (corroboration floor + extracted-specifics-absent).
   The loop did NOT render the web's figures as facts it could not
   attribute — the single-origin floor caps each one. The web leg
   retrieves real pages; the honesty machinery is untouched (the t2a
   frame's split: the dr-local-loop battery is NOT re-measured here —
   that is t2b's DRB arms).

## How to verify

```bash
./verify-demo8.sh
```

The strips check the committed artifacts — refusal exit code + typed
refusal + journaled attempt; manifest/charter consent records; the
egress trace's release lines (grant run id matching the manifest's, the
public-web url releases); the custody stamps; the ledger's 8 allows to
0 remaining; bars.md carrying the two met transitions verbatim from
`quality/initiative-bars.toml`. `DR8_RUN_DIR` and `DR8_REFUSAL_LIVE=1`
re-verify against a fresh live run when the daemon + rebuilt binary are
available.

The bars transitions (dr-egress, dr-budget-one-decider → met) carry the
measured evidence in `quality/initiative-bars.toml` — this demo's
artifacts are the flight half of that evidence; the red-first tests
and the census are the structural half, both green in the standard
suite.
