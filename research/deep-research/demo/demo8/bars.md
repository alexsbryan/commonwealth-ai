# DEMO-8 bars — the egress boundary and the ONE spend decider (order deep-research-t2a)

Order `deep-research-t2a` (PLAN §4 T2's trust-boundary half + the
acquisition ladder's rung 3 web leg). The two bars below are FROZEN —
this order re-cuts nothing; it measures. The transitions carry the
measured evidence (the red-first tests, the F26 census, the DEMO-8
flights) verbatim from `quality/initiative-bars.toml` — never
hand-typed; this file is generated from that toml, and `verify-demo8.sh`
strip 5 checks the correspondence both ways.

The measurements in one line:

- **R-5 red -> green**: a personal-corpus chunk CAN reach a remote
  payload via `enrich --provider` at HEAD (zero privacy tokens,
  nothing refused — the red-first test failed); after the landing the
  boundary refuses, typed, naming what was withheld — the SAME test
  passes with zero assertion changes.
- **R-6 red -> green**: the budget deciders at HEAD were all fail-open
  and none run-scoped (`budget_allows(None) => true`); the landing
  removes them and the census's r6 gate reads zero of the retired
  identifiers in every production src tree.
- **The flights** (DEMO-8): the no-consent run refuses at the first
  query egress, typed, exit 1, zero web egress; the `--consent
  personal` run releases 4 machine-formed queries under the run's
  grant + 4 public-web url fetches, all traced at debug, every fetched
  chunk stamped public-web custody, and spends exactly its allowance
  through the ONE decider (8 allows journaled, 0 remaining).

## The bar transitions (verbatim from quality/initiative-bars.toml)

### dr-egress

```toml
  [[initiative.bar.transition]]
  on = "2026-08-16"
  to = "met"
  by = """order deep-research-t2a — the boundary landed boundary-first, the
two reds measured before the fix. R-5 (red-first, zero assertion changes
between red and green): a personal-corpus chunk must not reach a remote
payload via `enrich --provider` — RED at HEAD (the remote path existed
with zero privacy tokens, nothing refused), GREEN after the landing
(sovereign/crates/sovereign-cli-llm/src/enrich_cmd/egress_reds.rs:96).
ONE boundary choke point for remote-model calls AND query egress:
sovereign/crates/sovereign-core/src/egress.rs — `verify` decides on the
caller-declared payload (privacy, custody, what, target, exact detail),
`search_client`/`model_client` are the only client factories; every
remote host (cli, cli-llm, tools, server, desktop) injects the
boundary-built client. Enrich `--provider` absorbed through the same
gate (inference_client.rs verify-before-dispatch). Refusal is typed and
names what was withheld (EgressRefusal Display). Every egress event —
released or refused — traced at tracing=debug (provider, custody, exact
payload size, grant run id + release floor when one released it): the
DEMO-8 flight transcripts are the evidence (query releases under the
run grant + public-web url releases + the typed default-deny refusal).
Consent gate: run-scoped typed grant (`--consent public-web|peer|personal`),
default-deny, frozen into the charter and recorded in manifest.json.
F26 census enforced as a build gate (sovereign/crates/sovereign-core/
tests/f26_egress_census.rs, in the standard suite): the red counted FIVE
egress-class construction sites at HEAD; the landing registers zero
outside the boundary. DEMO-8: research/deep-research/demo/demo8/;
pre-registration + execution journal: research/deep-research/adversarial/
pre-registration.md."""
```

### dr-budget-one-decider

```toml
  [[initiative.bar.transition]]
  on = "2026-08-16"
  to = "met"
  by = """order deep-research-t2a — ONE run-scoped, fail-closed spend
decider, the R-6 red measured first: at HEAD every budget decider was
fail-open and none run-scoped (BudgetView had no writer anywhere; every
production construction empty; budget_allows(None) => true;
orchestrator.rs:222-230; a second monthly fail-open decider at
search.rs:234-238). The landing: sovereign/crates/sovereign-core/src/
deep_research/budget.rs SpendDecider — the ONE decider — covers
web-search spend AND frontier-key spend (FAMILY_WEB_SEARCH /
FAMILY_WEB_FETCH / FAMILY_FRONTIER_KEY; the frontier-key family is
declared and INERT until t2b: no allowance is ever seeded in t2a, an
attempted spend refuses `no-allowance-or-exhausted`, pinned by the
fail_closed_table test). Run-scoped to the charter: hash -> allowance ->
decider at run start (deep_research/mod.rs), thresholds frozen at
launch (FR-3). Fail-closed: no allowance, unknown family, unknown key,
exhausted, and insufficient allowance all refuse — the fail_closed_table
test pins every row. Persisted as the budget-ledger.json ICD (FR-2,
`budget_ledger` v1, journal appended synchronously before the spend
executes — allow AND refuse decisions journaled; the DEMO-8 flights'
ledgers are the artifacts). No second budget decider remains: the F26
census's r6 gate scans every production src tree for the retired
identifiers (budget_allows, decrement_budget, BudgetView — zero, doc
comments included) plus the path-scoped check_budget absence in studio
search.rs; the old BudgetView machinery was removed with its call sites
(orchestrator SelectInputs, conversation.rs, knowledge_lookup, the two
real-network e2e tests). DEMO-8: research/deep-research/demo/demo8/;
pre-registration + execution journal: research/deep-research/adversarial/
pre-registration.md."""
```
