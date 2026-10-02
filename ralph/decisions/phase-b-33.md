<!-- ledger -->

**phase-b-33 · 2026-09-27 · Phase B → every open row's blockers cleared before dispatch: ten forks answered, ~200 seat fixes, four operator steps queued · operator**
- Needed: the operator: "Can we just get all blockers out of the way? … take a broad view and do real due diligence with arch principles." Of this campaign's 19 halts, 9 were a worker finding a false premise at census, 6 a bar, 2 HUMAN rows, 1 a permission, 1 an unstated delta. phase-b-30's census was taken at f7238e6d3, and four rows landed after it.
- The sweep (REVIEW-pb-preflight-4, run by the seat): three read-only agents, one per cluster (mesh/serve/work, code/notes, ingest/cli-llm/bench), re-checked every open row at HEAD against ARCH_PRINCIPLES and FIVE_PROGRAMS. They found no dependency cycles. They found ~10 forks, ~200 row-text fixes (false premises, cite drift, unowned pieces, missing depends, masked test sites) and the steps a worker cannot take on the operator's host.
- Chose (operator, every recommendation):
  1. `:9741/mcp` stays the one MCP address. The stock binary serves svrn's and code's bundles there, a standalone code serves code's, and the second to bind :9741 refuses by name. No client or hook is repointed.
  2. Code's one notes store is code's data root (default the svrnmesh root); a per-repo store only via `--data-dir`. `svrn reflect` on this host switches stores.
  3. bench's leaf_budget admits oicp-client, sovereign-turn-client, sovereign-cli-base, host-kit and corpus-index (not corpus-engine-atlas-reader).
  4. A pure file move carries its baseline rows at the same count, and a split lands in its own commit.
  5. svrn keeps `/v1/rail/{append,log,live}` as a grant-checked forward to cw-rails after the flip.
  6. mesh-reach's falsifier is its workspace allow list (kernel-types, workspace-hack); third-party deps come as its contents need them.
  7. pb-ingest-dial-tools gets one narrow port per ingest family, pre-authorised.
  8. pb-rails-idle's bar is fixed before data: first token on serve's `/v1/chat/completions`, against the toolbox's Vulkan llama-server b9307, on one named GGUF and context, with attribution by gdb/eu-stack sampling. The row splits into -cwrails and -stock.
  9. `svrn solve` dials serve and stops reaching mesh-routed models.
  10. Four HUMAN rows, which block nothing: HUMAN-pb-code-cutover, HUMAN-pb-notes-migrate (pre-migration copy), HUMAN-pb-flip-roster, and HUMAN-pb-serve-distributes-bar (the seat's fallback).
- Seat decisions in the same edit:
  - workflow_cmd stays in svrn.
  - pb-distribution-setup splits ahead of pb-distribution, and the flip runs as a series.
  - Release BINS becomes every sibling the dispatcher can exec, pinned by a test. It already misses sovereign-stock and sovereign-cli-mesh.
  - `sovereign-daemon → commonwealth-media` closes in pb-mesh-exit-transport, so pb-distribution closes no edge of its own.
  - Dependency and ownership fixes follow the agents' cross-row sections.
  - Two header rules: a re-found cite is not a false premise, and trials grep `tests/`.
- Because: principle 11 (every premise re-checked against HEAD, never recalled), principle 5 (the trials' blind spot on test targets), principle 12 (each item moved to the row that owns it), principle 6 (a second binder refuses by name), and principle 10 (the freeze and the BINS test). Boundary gate: 46 red + 4 excepted at 15:48 local (pb-cli-llm and pb-pods-verb closed three since phase-b-32).

<!-- appendix -->

## phase-b-33 · 2026-09-27 — the blocker sweep

<details><summary>reasoning, evidence, package</summary>

Instruments: three general-purpose agents, read-only (no cargo; the loop held the lock), at HEAD df90dc575..8aa8d2c5d. Each wrote its fixes as (old, new) pairs: code 48, mesh 96, ingest 56. The seat applied them in memory in order: 196 applied and 4 conflicted, all on pb-distribution, where two clusters had each struck `:66`. The seat merged that row by hand, taking ingest's finish, trial and LIFT and mesh's Layering line. The scripts are kept under target/ralph/phase-b/preflight-2/. The row edits and the owners-appendix removals landed in 56d2bd4f3, because the loop's `ralph: REVIEW-audit-pb-auto-5 done` mark commits the whole state file and swept them in. This commit carries the header rules, scope.txt, and the record. The result parses (82 rows, 48 open), validate_queue.py is OK (red 46 + excepted 4, owners 50), and `ralph.py plan` reports 0 open rows lacking finish or trial.

The owners appendix lost three lines for edges now closed: `sovereign-cli-llm → sovereign-inference` and `→ sovereign-gliner` (pb-cli-llm), and `→ sovereign-pods` (pb-pods-verb).

The live finding: pb-cli-llm left the f26 egress census red at HEAD, and REVIEW-audit-pb-auto-5 fixed it (352893860) before the seat's planned patch.

</details>
