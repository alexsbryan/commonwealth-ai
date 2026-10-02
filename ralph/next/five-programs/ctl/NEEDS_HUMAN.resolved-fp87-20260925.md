# NEEDS_HUMAN — fp-87 (premise check failed before any edit)

## (a) The unit

`fp-87 — depends [fp-88, fp-83, fp-84, fp-85, fp-86,
REVIEW-mint-fp-local-only-durable, fp-106, fp-107, fp-108]` (all nine `[x]`).
CLOSE both commonwealth-state edges in one commit: (1) drop
`commonwealth-state` from sovereign-daemon/Cargo.toml (deps and dev-deps);
(2) portfolio/newsworthy dial cw-rails' `/v1/mesh/kv/*` "through the existing
`ReplicatedKv` client (sovereign-cli-dev/src/mesh_kv_client.rs — reuse it, or
name why it cannot serve)"; (3) migration in sovereign-cli-mesh;
(4) NO-LAUNDERING BAR crate-wide over sovereign-daemon = 0.

The tree is untouched. The only change is the row marked `[~]` in STATE.md.

## (b) What I ran and what came back (HEAD 154df6d8e)

```
$ git grep -nw 'commonwealth_state\|MeshStore\|MeshReplicatedKv' -- sovereign/crates/sovereign-daemon
src/rails_client/ledger/tests.rs:193:  let store = commonwealth_state::MeshStore::in_memory().unwrap();
src/rails_client/ledger/tests.rs:194:  let emitter = commonwealth_state::ActivityEmitter::new(store, NodeId::from_u128(ME));
src/state/store.rs:24:   use commonwealth_state::MeshStore;
src/state/store.rs:67:   /// Every port over the node's own `MeshStore` through `LocalLedger`.
src/state/store.rs:68:   pub fn local(mesh_store: Arc<MeshStore>, self_node_id: NodeId) -> Self {
src/state/store.rs:70:   sovereign_mesh::peer_adapter::MeshReplicatedKv::over(Arc::clone(&mesh_store)),
tests/main/store_seed_double.rs:5:  //! to the double, not to any `MeshStore`.

$ git grep -n "StoreSeed::local" -- sovereign
sovereign-daemon/src/state.rs:956:  store::StoreSeed::local(Arc::clone(&fabric.mesh_store), self_node_id);
```

Conflict 1 — step (1) and bar (4) vs decision five-programs-59. -59 (fp-83's
resolution) struck "StoreSeed::local goes with its last caller" and made
`StoreSeed::local` NAMED RESIDUE "because it is what the `AppState::new`
family of test constructors seeds over (state.rs:956) and no row removes that
family". fp-87 was written before -59 and still requires the crate-wide bar
= 0 and `commonwealth-state` gone from the daemon's deps. Both cannot hold:
store.rs:24 names `commonwealth_state::MeshStore` in a non-test module, so the
dep cannot be dropped while `StoreSeed::local` keeps that signature. Second,
`rails_client/ledger/tests.rs:193-194` (a src unit test, added by fp-110/88's
ledger dial) records through `commonwealth_state::ActivityEmitter` to produce
the events its stand-in door serves; no row names it.

Conflict 2 — step (2)'s named client. `mesh_kv_client.rs` is `mod
mesh_kv_client;` (private, sovereign-cli-dev/src/lib.rs:86), dials the
DAEMON's `/v1/mesh/kv/*` (`urls::daemon_v1_base()`, routes at
sovereign-daemon/src/routes_mesh_kv.rs:44-47), not cw-rails'
(commonwealth-rails/src/kv.rs:571-574), and lives in the [code] package;
sovereign-cli-llm has no dep on sovereign-cli-dev (cli-llm Cargo.toml). The
row allows "name why it cannot serve"; I name it here rather than choose the
replacement host, since the choice is a new edge or a move.

## (c) What the operator must decide

1. **`StoreSeed::local` (sovereign-daemon/src/state/store.rs:24,67-93).** Pick one:
   (A) move the body behind a sovereign-mesh constructor over Fabric's own
       private store (fp-111 gave Fabric its `MeshStore::in_memory()`), e.g.
       `StoreSeed::local(&fabric, self_node_id)` calling a sovereign-mesh fn
       that returns the six ports — the daemon then names no store and the dep
       drops. Is that laundering under the bar's meaning? The concrete store
       stays owned by Fabric, not re-exported to the daemon.
   (B) re-seed the `AppState::new` family (state.rs:956) over fp-80's
       recording double; that reaches the 17 daemon test files -59 listed and
       is its own row.
   (C) amend fp-87: the daemon edge stays until a named later row; this row
       closes only cli-llm → commonwealth-state (BOUNDARY −1, not −2).
2. **`rails_client/ledger/tests.rs:193-194`.** Either it builds the served
   `ActivityEvent`s literally (no emitter), or it moves beside
   commonwealth-state's `ActivityEmitter` tests, or it stays under a
   dev-dependency (which bar (4) as written forbids).
3. **The client for step (2).** `mesh_kv_client` cannot serve as-is (private,
   dials the daemon, different package). Options: point cli-llm at the daemon's
   door through a shared copy moved to a crate both may name (which? cli-shared?),
   or have portfolio/newsworthy dial cw-rails' door via a client in that shared
   crate with the base URL from cw-rails' accessor. Name the host.

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
