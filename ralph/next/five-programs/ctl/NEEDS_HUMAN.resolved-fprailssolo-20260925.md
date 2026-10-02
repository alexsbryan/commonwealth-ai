# NEEDS_HUMAN — REVIEW-mint-fp-rails-solo (2026-09-25, at dc654cd31)

## (a) The unit

`ralph/next/five-programs/STATE.md:259`, marked `[~]`:
REVIEW-mint-fp-rails-solo (cap 5, five-programs-63). Clause (2) of the row:
"a `cw-rails ensure` entry starts it detached **under sovereign-contracts'
run_lock** when it is not running".

No rows were minted. The row and the tree disagree on one clause, and the
row's own words make that clause the operator's (five-programs-63's falsifier
names a lifecycle finding as a NEEDS_HUMAN line).

## (b) What was measured (read-only; no build, nothing edited but the [~])

The conflict:

- `quality/ARCH_LAYERS.toml:676-679` — `[[forbid]] from = "commonwealth-rails"
  to = "sovereign-*"`, reason: the binary is built and run OUTSIDE the monorepo
  by `scripts/cw-rails-lift.sh`. So cw-rails cannot link
  `sovereign_contracts::run_lock::RunLock`.
- `shared/crates/sovereign-contracts/src/run_lock.rs` — `RunLock::acquire(data_root)`
  is `flock(LOCK_EX|LOCK_NB)` on `<root>/daemon.lock` via `libc`, ~60 lines,
  no sovereign-specific dependency besides its error text (`svrn daemon stop`).
- A client-side lock does not serve: the lock must be held by the SERVING
  process for its lifetime, and `cw-rails run` launched by hand would bypass a
  lock only clients take (the two-doors hazard, memory: manage daemons via CLI).
- Precedent for crossing the lift boundary by mirroring rather than importing:
  `sovereign-daemon/src/rails_client.rs:33-38` — `DEFAULT_RAILS_BASE` "is
  mirrored and documented on both sides rather than imported across the lift
  boundary".

Site census for the rest of the row (all clauses other than the lock are
mintable within cap):

- `Refusal::NoMesh` has ONE producer (`commonwealth-rails/src/lib.rs:297-299`,
  `start_from_disk`) and ONE reader (`cli.rs:194`, the `run` verb; USAGE text
  `cli.rs:37` "Refuses to start with no mesh"). `MediaReachRefusal::NoMesh`
  (sovereign-daemon media_reach.rs:64,85, origin_fanout.rs:63) is a different
  type and not in scope.
- Roster/identity: `rail::derive_roster` (rail.rs:80) builds from
  `mesh.members` with self's pubkey substituted by node_id; `KvHost`
  (kv.rs:93) and `MembershipRosterSource::install` (rail.rs:125) take
  `Arc<RwLock<Mesh>>`. A self-only in-memory `Mesh` (never written to
  `mesh.json`) satisfies all three; `kv/tests.rs:57 host_at` already builds a
  host exactly that way. Local-only rehydrate (kv.rs:142-183, fp-108) keys on
  this node's actor = node_key pubkey, which persists across `cw-rails join`,
  so solo rows survive a later join + restart.
- Rails-client constructors to wire ensure into: daemon bootstrap.rs:2496,
  daemon.rs:3111 (RailsRingRail), daemon.rs:3259, daemon_services.rs:832;
  cli-llm `legacy_store.rs:20 rails_kv()` (the one site portfolio and
  newsworthy both use).
- Sibling resolution: the "existing" locators are SIX private copies with a
  hardcoded `BIN_NAME` in binary crates — sovereign-cli/src/{daemon,mesh,llm,
  agent_bench,dev}_bin.rs:15/25 and sovereign-cli-daemon/src/daemon_bin.rs:17.
  Neither sovereign-daemon (a library) nor sovereign-cli-llm can call any of
  them. Reuse means lifting one parametrized `locate(bin, env_var)` into
  sovereign-contracts and collapsing the copies onto it — one move row.

Plan as it would be minted (5 rows, at cap) once the lock is decided:
1. cw-rails solo mode: `start_from_disk` with no `mesh.json` starts over a
   self-only in-memory mesh, gossip/presence not spawned; unit test that the
   kv + ledger doors serve and a local-only row survives a restart and a
   later join. PLANT: restore the NoMesh return.
2. Lift the sibling locator into sovereign-contracts; the six copies call it.
3. `cw-rails ensure`: probe `listen`, else spawn `run` detached and wait for
   `/v1/mesh/status`; the lock per decision below; solo idle-exit.
4. Clients call ensure through the lifted locator at the five constructor
   sites above (boot, or a refused dial).
5. Process-level e2e against the built cw-rails on a meshless dir: daemon
   `/v1/models` 200, `svrn portfolio` round-trip across a cw-rails restart;
   PLANT (NoMesh); + full suite and `lint --full` (five-programs-54).

## (c) Decide

1. The single-instance guard for cw-rails' data root, given the forbid at
   `quality/ARCH_LAYERS.toml:676`:
   - (A) cw-rails takes its own flock on its data root in `run` (a ~20-line
     mirror of `run_lock.rs`, documented on both sides like
     `DEFAULT_RAILS_BASE`). Recommended: zero lift cost, matches the existing
     mirror precedent; cost is a second implementation of "one writer per
     root" across the lift boundary (principle 8, named).
   - (B) Move `RunLock` down into a crate both programs may name and widen
     cw-rails' closure by it (a forbid `except` edit + lift sandbox re-run).
   - (C) No lock: the loopback API bind on `listen` is the singleton decider,
     moved before endpoint bind. Leaves two cw-rails on one data dir with
     different ports unguarded.
2. Confirm that lifting the six sibling locators into sovereign-contracts
   (row 2) is the reuse the row means by "daemon_bin.rs / mesh_bin::locate".

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md (name the lock
option in its text), then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.

## (e) Seat note — the seat, 2026-09-25 (not the operator's word)

The seat reads (A) as principle 12, not a twin. The daemon's run_lock guards the DAEMON's data root, and cw-rails'
flock would guard CW-RAILS' root: two programs, each owning the singleton of its own data directory (§4 rule 1). No one
thing gets two deciders, so it needs no operator answer. On 2, lifting the six locators into one `locate` is the reuse
the row meant, and it deletes five copies, which fits five-programs-54.
