<!-- ledger -->

**phase-b-24 · 2026-09-26 · pb-svrn-dials-serve → mesh-distributed inference split to pb-serve-distributes; the switch keeps it in-process behind one opt-in decider · director** — this commit
- Needed: the second worker's census at cd9fab8f5 stopped before any code. Mesh-distributed inference (RPC-worker discovery, distributed-primary respawn, discovery policy, the auto-warm orchestrator, the worker role; about 2,700 lines, all wired at boot) needs the mesh roster AND the loading process. No row placed it, so the switch as written would silently drop distributed loads. Hot reload and the daemon's engine-memory rows had no stated meaning once the daemon holds no weights.
- Chose:
  - Placement: serve owns distributed inference. pb-mesh-exit-mesh already has serve register "rpc (plus model transfer and rpc-warm)", and FIVE_PROGRAMS §4 rule 8 puts a capability's mesh face with its §2 owner. Discovery reads peers through cw-rails, so the move needs pb-rails-origins.
  - Sequencing: split by proof. pb-svrn-dials-serve switches every config that does not opt into distribution. ONE boot-time decider keeps the in-process path when `SOVEREIGN_RPC_SERVE`/`_DISCOVER`/`_WORKERS` is set or `[compute] distributed_primary` is true (all default unset), traced and named in `svrn daemon status`. A new row, pb-serve-distributes (depends pb-svrn-dials-serve, pb-rails-origins), moves the subsystem into serve, deletes the decider and the in-process bootstrap, and retires fp-10 ×2. pb-serve-package and pb-mesh-exit-mesh depend on it.
  - Hot reload: serve mounts a reload route over the one assembly pb-serving-assembly made; the daemon's `/v1/admin/reload` forwards and refuses by name when serve is unreachable.
  - Engine-memory rows: serve exposes its cached loader view; the daemon's mesh status reads it, with an unreachable serve as a named absence.
- Because:
  - Option (c), a stated loss of distributed inference, changes what users see; the charter leaves that to the operator, and it is avoidable. Option (b), the daemon pushing workers and respawns to serve, makes the daemon a second owner of serve's child lifecycle (principle 12) and mints a control surface pb-serve-distributes would delete.
  - The switch is proved by a loopback chat turn, the move by a model loaded across a mesh-of-two. Different proofs, so the charter's split rule applies. Folding into pb-serve-ranks was considered: same dependency, different proof (routing a turn vs loading a split model).
  - Reload and engine memory are serve's by pb-serving-assembly and by where the loader runs; forwarding keeps both user-visible behaviours (extend, never re-own).
  - Boundary gate: 49 at cd9fab8f5 (EXIT=1). No code is in this commit.

<!-- appendix -->

## phase-b-24 · 2026-09-26 — distributed inference is serve's and moves behind pb-rails-origins; the switch keeps it in place for opt-in configs only

<details><summary>reasoning, evidence, package</summary>

Reproduced at cd9fab8f5 from the host:
- `wc -l`: bootstrap.rs 2,703, discovery_policy.rs 689, rpc_warm_http.rs 692, rpc_warm_http/orchestrator.rs 323, admin_http.rs 390, provider.rs 362, daemon_cmd/boot.rs 1,196.
- Wired at boot: boot.rs:258 `apply_rpc_worker_flag`, :259 `apply_shared_model_role_to_env`, :337 the serving bootstrap returning `distributed_primary_slot` and `reload_factory`, :1094 `install_rpc_warm_orchestrator`, :1105 `spawn_rpc_worker_discovery`.
- Process-global hooks in the daemon: bootstrap.rs:579 `embedded::set_rpc_worker_provider`, orchestrator.rs:39 `embedded::set_rpc_warm_orchestrator`.
- mesh_http.rs:433 `embedded::last_device_memory()`, :440 `embedded::pinned_block_split_raw()`; the comment at :426-430 records why the view is cached, never sampled.
- admin_http.rs:70 mounts `/v1/admin/reload`. sovereign-serve/src/lib.rs:306-308 mounts only chat, embeddings and `/v1/models`: no reload, no engine-state route.
- bin/sovereign-daemon.rs:60/65 re-exec `Launch::ComputeChild` / `Launch::RpcWorker`.
- tests/main/ holds compute_child_e2e.rs, distributed_primary_respawn_e2e.rs, named_model_routes_after_child_serves_e2e.rs.
- quality/env-flags.toml: `SOVEREIGN_RPC_SERVE` and `SOVEREIGN_RPC_DISCOVER` default unset (shadowing `shared_model.role`); bootstrap.rs:411 translates `[shared_model] role` into that env contract, so a decider read after it sees one input set.
- STATE.md pb-mesh-exit-mesh: "serve registers the member client (peer inference and its OICP manifest) and rpc (plus model transfer and rpc-warm)". pb-rails-origins lists rpc among the new `OriginKind` variants.
- `cd corpus-engine && cargo xtask boundary-gate` in the toolbox: 49 violations, EXIT=1.
- `work_in_flight(ralph/, file)` could not judge: cw-rails on :9747 unreachable. The only files edited are the campaign's own queue and ledger.

The interim keeps two serving paths for one row's span. That is a transition with one decider and a named owner of its removal, not two implementations of one threshold: the decider is the single site that chooses, and pb-serve-distributes deletes it.

What would falsify this:
- The worker finds a default config (none of the four inputs set) that still starts discovery or the worker role, e.g. a node that serves as another node's RPC worker without its own opt-in. Then the decider's input set is wrong and the default switch would drop a live role: re-census, and if the role cannot be kept without the in-process path, it goes to the operator as an end-user-observable loss.
- serve's reload route cannot rebuild what the daemon's reload rebuilt without the daemon's router state, i.e. the forward is not behaviour-preserving. Then reload's meaning is the operator's.
- pb-serve-distributes' mesh-of-two load misses the pre-registered latency bar. Report the numbers and stop.

</details>
