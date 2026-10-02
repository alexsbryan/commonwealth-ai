<!-- ledger -->

**five-programs-40 · 2026-09-24 · fp-77 · director** — this commit
- Needed: fp-77 said "local-only namespaces (fp-76) are now journaled" and tested that one survives a restart, but four places in the tree refuse it (the outbox guard, the inbound projection refusal that -37 kept, enumeration, and the two `:` ids that `valid_namespace` rejects). Separately, the row ran two pumps sealing the same journals under one actor during the twin.
- Chose: option (C). Local-only stays unjournaled and the clause leaves the row, which makes the package's forks 2 and 3 moot. On sealing, each journal gets exactly one sealer, the owner of its writers. cw-rails pumps and seals only the KV namespaces its own store feeds. fp-77 no longer forwards the daemon's `/v1/mesh/kv/*` routes, so no namespace has writers in two stores; routes_mesh_kv.rs moves to fp-82. The `mesh-measurements` and `work` seal arms stay in the daemon, and fp-80 and fp-83 now say so.
- Because: every daemon store is `in_memory()` today, so a local-only row does not survive a restart now either. Dropping the clause preserves behaviour, while keeping it would add a capability, a second door into the store, and a rename (charter size rule). A seal's snapshot mark retires every row of this actor it does not name, so two sealers over two stores would retire each other's rows (ARCH 8: one decider).

<!-- appendix -->

## five-programs-40 · 2026-09-24 — fp-77 drops local-only journaling; one sealer per journal, owned by its writers

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp77-20260924.md. The director reproduced it at d4c100aaf.

- Outbox guard: commonwealth-state/src/backend.rs:519-528 (`enqueue_on`) and backend/memory.rs:78-82 skip every `is_gossip_excluded` id, and store.rs:818 pins that. The inbound refusal is at store.rs:356-362. `valid_namespace` (commonwealth-rail/src/lib.rs:42-48) rejects `:`. All confirmed as the package states.
- No daemon store is durable. daemon.rs:3035, daemon_services.rs:832/860 and bootstrap.rs:2527 all construct `MeshStore::in_memory()` / `MeshReplicatedKv::in_memory()`, and `MeshStore::open` has no daemon caller since fp-75. So "a local-only row survives a restart" was never true, and fp-77 was being asked to invent it.
- Seal hazard. `snapshot` (sovereign-mesh/src/rail_kv_pump.rs:550) re-appends this actor's live rows from ITS store, then writes a mark that lets readers retire the actor's rows it does not name. The daemon projects own-actor journal rows into its store only at pump start (:198) and on ring sync (ring_sync.rs:408). Rows written through cw-rails' door would therefore be missing from a daemon snapshot, and the reverse holds too. The package's per-write twin safety holds for appends and fails for seals.
- `snapshot`'s measurements arm (:562-573) calls `sovereign_mesh::mesh_measurements::load` and `measurements_rail::republish`. cw-rails may not name sovereign-* (ARCH_LAYERS forbid row), so that arm stays with its writer in the daemon. The work arm's writer is the daemon's donor loop, so it stays too.
- fp-80 gains a premise, not a mechanism: the peer ops the daemon's ring sync admits have to reach cw-rails' store once the daemon store is gone. If they do not, fp-80 halts under §6.

The charter covers this as a false row premise: the smaller, behaviour-preserving step, plus principle 8. REVIEW-AFTER: fp-76's local-only journal class has no appender now (principle 12, "the uses go to zero and the ability stays"). Making local-only rows durable is new capability, and it is the operator's to order. The costs the package priced: narrow the outbox guard or add a store-level opt-in, add a self-actor-only rehydrate door beside `apply_projection`, and rename the two `wikipedia-newsworthy:*` ids.

Falsified if some daemon store turns out to be durable (a `MeshStore::open` or `MeshReplicatedKv::open` on a live boot path), which would make dropping the clause a regression. Also falsified if the daemon's pump reliably projects cw-rails' own-actor appends before it seals, which would make the twin-seal hazard unreal and forwarding the routes in fp-77 safe.

</details>
