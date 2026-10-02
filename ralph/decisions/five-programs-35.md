<!-- ledger -->

**five-programs-35 · 2026-09-24 · REVIEW-mint-fp-state-pure-rust · director** — this commit
- Needed: the mint proved that a pure-Rust serving shape exists. It halted without minting because the census needs about 11 rows against a cap of 8, and because it could not choose where the typed ledger writers live once the daemon may not name commonwealth-state.
- Chose: the cap is raised to 11. The writers stay in commonwealth-state, cw-rails serves them as typed doors, and the daemon dials (option b). The legacy-file migration reads through commonwealth-state's own backend from a [cmnwlth]-closure binary. The row resumes as a mint. Boundary gate 54, unchanged.
- Because: §12 D4 already names the shape: "cmnwlth owns the disk; the daemon keeps a read-through cache and dials." Keeping the writers where they are keeps one decider per key scheme (ARCH 8). A new shared leaf is reserved to the operator by the charter, and it would still name commonwealth-core, which is a red daemon edge. The per-request write already crosses a port (venue_host.rs:29), so the flip swaps one impl behind an existing trait (ARCH 11).

<!-- appendix -->

## five-programs-35 · 2026-09-24 — state mint resumes: cap 11, writers stay with their store and are served by cw-rails

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fpstate-20260924.md. Reproduced by the director at b6d3d56c1:

- `git grep -c rusqlite commonwealth/crates/commonwealth-state/src` returns backend.rs:3 and store.rs:1. The C dependency is confined to the backend, so the pure-Rust claim holds.
- The daemon src tree has 16 files naming `commonwealth_state` and the daemon tests tree has 35. Those counts are what drive the row count.
- Every production MeshStore in the daemon is `in_memory()` (daemon.rs:3035, state.rs:753/766/782). The only `MeshStore::open` calls are the cli-llm file stores (newsworthy_cmd.rs:73, portfolio_cmd/mod.rs:51). `MeshReplicatedKv::open` appears only in a doc comment (routes_mesh_kv.rs:6).
- The `ReplicatedKv` port already lives in sovereign-contracts (peer.rs:173), and so do the kv wire bodies.
- Every consumer of the typed writers (ContributionEmitter, ActivityEmitter, PeerPreferenceStore, InferenceStateStore, processed_shards, current_*, RetentionGc) is on the sovereign side, per `git grep -l` outside commonwealth-state. Each writer imports `commonwealth_core` vocabulary and `crate::store::MeshStore`.
- sovereign-contracts does not depend on commonwealth-core, and commonwealth-core is not a `[[package_leaf]]`. The boundary log lists `sovereign-daemon → commonwealth-core` as red. So any sovereign-side home for the writers either widens a leaf or admits a new one. The charter gives both of those to the operator.
- The daemon's ledger emission already goes through serving-host's ledger port, implemented over ContributionEmitter at sovereign-daemon/src/venue_host.rs:29. The daemon already holds a rails dial (rails_client.rs, fp-54).
- In the toolbox, from corpus-engine/, `cargo xtask boundary-gate` gives FAILED (54 violation(s)), EXIT=1.

Options weighed. (a) A new shared leaf is operator-reserved, and it would carry commonwealth-core into the leaf set. (b) Typed cw-rails doors are the D4 shape. They add wire, but they swap behind an existing port, and the store keeps its own key schemes. (c) Re-deriving the key schemes in the daemon is refused by ARCH 8. On the cap, compressing the test tree into one 35-file row would trade atomicity for a number chosen before the census existed. Eleven rows is what the census demands.

The migration constraint was added because the package's row 11 would have read legacy SQLite "from the svrn side". Done with a fresh rusqlite in cli-llm, that is a second schema implementation. Done through commonwealth-state, it re-acquires the edge being closed. Reading through commonwealth-state's feature-gated backend from a [cmnwlth] binary avoids both problems, provided cw-rails' closure is proved unchanged under feature unification.

Falsified if: the per-request contribution write cannot tolerate a local HTTP hop, so that serving latency moves measurably or the ledger loses writes under load (then (a) returns to the operator); if feature unification pulls libsqlite3-sys into cw-rails' lifted closure whatever the migration binary does (then the migration needs another home); or if the operator admits a shared leaf for mesh-ledger vocabulary, which would make (a) cheaper than the doors.

</details>
