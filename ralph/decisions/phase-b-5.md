<!-- ledger -->

**phase-b-5 · 2026-09-26 · cw-rails serves only after projecting its store · seat (operator: go with recs)** — this commit
- Needed: pb-handover-first (075beac8a) found that cw-rails answers `/v1/mesh/status` and its KV doors while its pump is still projecting the store from the journals on disk. `ensure_rails` treats that status path as ready. So a client's first read after ANY cw-rails start can answer absent while the rows exist; the row's proof flaked 1 in 3 until it waited for the rebuild line.
- Chose: a new row, pb-rails-ready, placed ahead of the census's dependants: cw-rails projects its store before it serves.
- Because:
  - Principle 6: an absence that is really "not loaded yet" is a silent substitution.
  - Principle 12: readiness is cw-rails' own fact. No client should have to learn a second, later signal.
  - Principle 10: a wait on a log line in a test is a convention, and the order in `run` is structure.
  - Boundary gate: 51, unchanged. There is no code in this commit.

<!-- appendix -->

## phase-b-5 · 2026-09-26 — pb-rails-ready: no false absence in the projection window

<details><summary>reasoning, evidence, package</summary>

`RailsDaemon::run` spawns `kv::run_forever` (commonwealth-rails lib.rs:437) alongside gossip, presence and the API. `run_forever` projects every namespace on disk first (kv.rs:527 `project_all_on_disk`), then loops. Nothing orders the listener after that projection. `ServingHost::ensure_reachable` in `ensure_rails` probes `ready_at("/v1/mesh/status")`, so "reachable" can precede "projected". On the operator's node the store is 14 MB across 10 namespaces. The window's length at that size is unmeasured; the row measures it.

</details>
