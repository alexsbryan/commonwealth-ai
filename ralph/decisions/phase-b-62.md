<!-- ledger -->

**phase-b-62 · 2026-09-29 · pb-bench-dials-docs · director** — this commit
- Needed: the worker's census at 29973b5b1 found five document/store sites in the bench group with no svrn route to dial (no attach-asset-to-conversation route; `/ask` runs a different pipeline; no asset-chunks, RAPTOR-rebuild, ingest-knob, resource-ledger or corpus-RAPTOR-nodes route). The row forbade minting routes, so it stopped.
- Chose: extend phase-b-58's `svrn __probe` instead of minting routes or dialing `/ask`.
  - pb-bench-dials-docs is rewritten: an `attached` probe mode runs the metered asset build (with `--enrich-model`, `--no-gliner` and the RAPTOR rebuild as request fields) and ONE minted-DocumentSession turn drive, collapsing book_report's `dispatch_question` and live_runner's `run_attached` (2 copies). resource_meter moves to probe_cmd; `ResourceReport` goes on the probe wire. book_report and chaos_monkey's attached transport exec it.
  - pb-bench-dials-vault is split out by proof: `vault-build` and `raptor-nodes` modes for vault_report and faithfulness. pb-cli-llm-bench-move depends on it.
- Because:
  - Dialing `/ask` changes the subject: it is the route→execute pipeline decision 7693f16b moved the lane off, so the row's same-verdict proof cannot hold and the swap would be silent (principle 6).
  - The probe is the existing owner of "svrn describes its own internals for bench to judge" (principle 11, extend never re-own); a new attach route would be svrn product surface minted for a bench migration (principle 12), and the ingest knobs and ledger would still have no route.
  - No end-user behaviour changes: verbs and flags keep their spelling, and the same in-process code runs, now in svrn's process. No leaf, no exception, no manifest line.
  - Boundary gate: EXIT=1, 20 violations (delta 0). This commit changes no Rust.
- REVIEW-AFTER: pb-bench-dials-docs' PROOF run. The charter covers a false premise, but widening the probe from one stage per question to a full attached turn was not ruled on before. Falsified if the probed answers differ from the in-process ones at temperature 0, or if the moved build needs a crate outside cli-llm's current manifest.

<!-- appendix -->

## phase-b-62 · 2026-09-29 — the document lanes exec `svrn __probe` (attached, vault-build, raptor-nodes); no routes are minted

<details><summary>reasoning, evidence, package</summary>

Reproduced in this session at 29973b5b1:

- book_report.rs:1635-1648 records the 2026-05-20 move off `manager.route()`/`manager.ask()` (decision `7693f16b`); :1696 `create_document_session`, :1701 `handle_turn`. live_runner.rs:248 and :260 the same pair.
- documents_http.rs:174-187 lists every documents route: list/upload, legacy promote, `{id}/skeleton`, `{id}/progress`, `{id}/ask`, `{id}/ask/{job_id}`. No attach, chunks or RAPTOR rebuild route. `attached_asset_id` (:609) has no reader in sovereign-core/src.
- `grep prompt_tokens|completion_tokens|llm_calls sovereign-daemon/src/lc_*.rs`: no hit.
- faithfulness.rs:238-248 opens `state_db_path` and calls `list_corpus_raptor_nodes(&corpus_id, 0)`.
- Call sites: `run_attached` only chaos_monkey.rs:752; `provider_for_model` (book_report.rs:816) at chaos_monkey.rs:530, vault_report.rs:819; `MeteredInference`/`ResourceLedger` only in book_report.rs and vault_report.rs; `cmd_vault_report` bench_cmd/mod.rs:197, `cmd_faithfulness` :213. chaos_monkey's `cmd_chaos_monkey` is also called by proxy_bench.rs:98 and governance.rs:125, which is why only its attached transport moves, not the file.
- probe_cmd/mod.rs:1-12 describes the probe as svrn's self-description for bench; sovereign-contracts/src/probe.rs holds its wire.

Options weighed from the package: (1a) an attach route, rejected (new svrn surface for a migration; leaves sites 2-4 unrouted); (1b) accept `/ask`, rejected (undoes 7693f16b's measurement silently); refusing `--reuse`, `--enrich-model`, `--no-gliner` by name, rejected (verbs that work today would error, operator-only). Sites 4 and 5 go through the probe rather than waiting for pb-cli-llm-bench-move's census, because the probe covers them with no leaf.

The worker's package is archived at target/ralph/phase-b/parked-pb-bench-dials-docs.phase-b-62.md.

</details>
