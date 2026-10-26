<!-- ledger -->

**engine-swap-8 · 2026-10-26 · es-g4-delete-mesh-fabric · director** — this commit
- Needed: the row deletes `distributed_role` whole and stops on a caller outside the fabric (§6). What is left of it, `apply_shared_model_role_to_env`, is the only writer of `SOVEREIGN_SHARED_MODEL_ID`, which serve's ranking reads once at construction (`rank.rs:217`, `SharedModelFleet::from_env` → `set_shared_model_id`) to route this node's primary turns to the mesh peer serving the shared model. That is peer inference, not the fabric; deleting it whole leaves a configured `[shared_model] model_id` unrouted with no error (ARCH 6).
- Chose: keep that one line and delete the rest. `[shared_model] model_id` → `SOVEREIGN_SHARED_MODEL_ID` (explicit env still wins, the `info!` kept) moves to `serve/crates/sovereign-serve/src/shared_model.rs` as `sovereign_serve::apply_shared_model_to_env`; `sovereign_compute::distributed_role` and the `sovereign-serve/src/lib.rs:105` re-export go, and the three callers (`standalone.rs:22`, `sovereign-stock/src/main.rs:100`, `sovereign-onprem/src/main.rs:64`) and the doc comments naming the old function are repointed, nothing left at the old path. Everything else it wrote goes: the role → discover translation, the anchor eligibility knobs, the host pin, quorum, pooled, headroom and shard-fetch. The worker's package recommended keeping `quorum_anchors` too, and that half is not taken because it has no production reader.
- Because: serve's ranking is what decides where primary turns go, so the value it reads lives beside the reader and not in compute, which owns process supervision and model kinds (ARCH 12, the charter's placement rule). It is the smallest change that keeps routing intact, and the row's acceptance and checks are unchanged. No other write had a reader in the process that sets it, and that was verified at 7a610b614 (appendix).

<!-- appendix -->

## engine-swap-8 · 2026-10-26 — the shared-model routing line outlives `distributed_role`

<details><summary>reasoning, evidence, findings</summary>

Reproduced at 7a610b614.

`git grep -n 'SOVEREIGN_SHARED_MODEL_ID' -- '*.rs'`: one writer
(`sovereign-compute/src/distributed_role.rs:28`); one reader,
`sovereign-contracts/src/launch.rs:423` (`SharedModelFleet::from_env`), whose one
production caller is `sovereign-serve/src/rank.rs:217`, followed by
`mesh_provider.set_shared_model_id(..)`. `rank/tests.rs:109,121` already cover the
env → provider half.

`git grep -n '\.quorum_anchors()' -- '*.rs'` finds only `launch.rs:503,514`, which are
tests. `SOVEREIGN_RPC_QUORUM_ANCHORS` is still parsed into `SharedModelFleet::Member`,
but no production code reads that field, and the write sat behind `discover`, i.e.
serving roles only.

The other writes, and why dropping them changes no behaviour:
- `SOVEREIGN_RPC_WORKER_SETTLE_SECS`/`_FLAP_THRESHOLD` are read by
  `worker_eligibility.rs:232-233`, which has had no writer since `rpc_discovery` was
  deleted (b585dbf4d).
- `SOVEREIGN_RPC_DISCOVER` is read only by `contracts/containment.rs:20`, whose caller
  is doctor (`checks_sovereign.rs:979`). Doctor runs in the CLI process, which never
  inherited the daemon's `set_var`, and `classify_containment` discounts the variable
  whenever the role implies discovery anyway (`containment.rs:81-93`).
- `SOVEREIGN_SHARED_MODEL_HOST_NODE_ID`, `SOVEREIGN_RPC_MIN_POOLED_GB` and
  `SOVEREIGN_RPC_SHARD_FETCH` have no `.rs` reader.
- `SOVEREIGN_RPC_HEADROOM` is read by `mesh_plan.rs:230`, a CLI process that falls
  back to `[shared_model] headroom` from config itself (`mesh_plan.rs:225-227`), and
  es-g4-delete-mesh-fabric-verbs deletes it.

Findings filed and not acted on, because scope is frozen:
1. The `[shared_model]` keys `role`, `host_node_id`, `quorum_anchors`,
   `min_pooled_gb`, `shard_fetch` and `allow_plaintext_lan` are now inert, and
   `headroom` becomes inert with the verbs row. Refusing them by name, as
   `[compute] distributed_primary` is refused in `assembly.rs`, would be a new row
   and is the operator's call. Until then a config that sets them loads without a
   word (ARCH 6).
2. `worker_eligibility` (serving-host) and its `/v1/mesh/status` rows
   (`sovereign-serve/src/lib.rs:116`) have had no writer since b585dbf4d. No row
   names them.
3. `SharedModelFleet::Member.quorum_anchors` and the env read behind it are an
   accessor with no production reader, and could fold into a `from_config` with no
   env var at all. That is a behaviour change, so it does not belong in this row.

Package: the lane's `control/package.md`. Its option 1 is taken, minus
`quorum_anchors`, with the home in sovereign-serve rather than sovereign-compute.
Options 2 and 3 are filed as findings 1 and 2.

</details>
