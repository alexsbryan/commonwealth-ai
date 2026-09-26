<!-- ledger -->

**phase-b-10 · 2026-09-26 · pb-hostkit · director** — this commit
- Needed: pb-hostkit's second census (NEEDS_HUMAN, before any edit) found that the row's cli-base vocabulary moves cannot hold with "NO consumer changes": guest_link.rs and rail.rs each have a [svrn] consumer besides cli-mesh, and the re-exports would widen three leaf `allow` lists.
- Chose:
  - The row's outcome is the lock, so all four cli-base vocabulary moves (guest_link, rail, urls, dispatcher) are struck from it. guest_link and rail stay in cli-base. urls moves with `client_daemon_base` in phase-c's pc-config-split. dispatcher's collapse onto turn-client's `locate_sibling` goes to pb-distribution.
  - run_lock's two out-of-crate callers repoint to the kit directly, and `sovereign_contracts::run_lock` is deleted instead of re-exported. sovereign-contracts' allow stays as it is.
  - Only corpus-engine-scip's allow grows, by the kit, for the fs4 collapse. The row now names that.
  - The kit is `host-kit`, at the repo root, in the `contract` layer.
- Because:
  - §12 3a is first-match. Two programs use guest_link and rail, so rung 1 does not fire, and cli-base is already the shared leaf that admits commonwealth-rail-core.
  - rail.rs:80 reads `crate::urls`. Moving urls would force a cli-base → sovereign-turn-client edge that the lock does not need.
  - boot.rs:224 and ablate.rs:335 are the only callers outside contracts, and the row edits both anyway, so repointing them costs no extra file.
  - Boundary gate: 51, unchanged. There is no code in this commit.

<!-- appendix -->

## phase-b-10 · 2026-09-26 — pb-hostkit narrows to the lock; no cli-base vocabulary moves, no contracts re-export

<details><summary>reasoning, evidence, package</summary>

Reproduced at 9a9377899:

- `git grep -n "sovereign_cli_shared::guest_link"` finds sovereign-cli-llm chat_cmd/config.rs:13.
- `git grep -n "sovereign_cli_shared::rail"` finds sovereign-cli quality_check_cmd/distribute.rs:61.
- cli-mesh reaches both through `sovereign_cli_base::` in guest_route.rs, mesh_guest.rs, job_cmd.rs, mesh_offers.rs and ring_cmd/mod.rs.
- rail.rs:80 is `crate::urls::daemon_base_url()`.
- Nothing names `sovereign_cli_base::urls` directly. Every urls consumer goes through sovereign-cli-shared.
- `sovereign_cli_base::dispatcher` has one direct user, cli-mesh mesh_media/offer.rs:348.
- turn-client already owns `locate_sibling` (reach.rs:415), whose doc records seven retired copies.
- `run_lock` outside contracts: boot.rs:224 and bench_cmd/ablate.rs:335 are code. cli-daemon lib.rs:182, turn-client reach.rs:319 and contracts launch.rs:246 are doc text only.
- `svrn code converge noun HostKit --corpus-id commonwealth-ai`: 0 definitions.
- boundary-gate: EXIT=1, 51 violations.

The package's three questions, answered:

1. Guest_link and rail: strike their move (the recommendation).
2. The three widenings: only one is needed. scip gains the kit. Contracts does not, because its callers repoint. cli-base does not, because urls and dispatcher stay put.
3. The name: `host-kit`, as proposed.

The package read PROMPT.base §7 as forbidding `allow` widenings. §7 actually covers `[[exception]]` rows and `except` lists. The row names the one scip widening anyway, so it is explicit.

The charter's "admitting any leaf other than the host kit" is not touched. The kit is the one leaf this admits.

Falsified if:
- a program other than svrn and cmnwlth, or neither of them, turns out to use guest_link or rail. Then rung 1 or 2 places it again;
- the daemon's `Held` refusal text changes when its hint moves to boot.rs. That is observable behaviour, and it goes to the operator;
- scip's lock is found not to be exclusive non-blocking with held → `Ok(None)`. Then the collapse is wrong and scip keeps fs4.

</details>
