<!-- ledger -->

**five-programs-42 · 2026-09-24 · fp-79 · director** — this commit
- Needed: fp-79 halted on five-programs-36's own falsifier. FabricPart's two public fields are read by 31 daemon src sites in 14 files, plus 24 daemon test files and sovereign-mesh's dst.rs, through surfaces no port carries. The row cannot land inside sovereign-mesh.
- Chose: split the type flip from the backing flip. fp-79 becomes an unwired in-process impl of fp-78's five ports (`LocalLedger`, following the `LocalRingRail` precedent). fp-80 to fp-82 flip the daemon's readers to AppState port fields over that in-process backing on Fabric's one store, which preserves behaviour. The new fp-88 flips the backing to `rails_client`, stops the RetentionGc and KV pump arms, and sheds Fabric's `mesh_store` param and `contribution_emitter` field. fp-83 and fp-87 depend on fp-88. The no-laundering bar moves to fp-88. Boundary 54, unchanged, since no code moved.
- Because: -36's prescribed merge gives about 25 src files plus 24 test files. That trips fp-80's own "past about ten files, §6" clause immediately. The other resequence (fp-79 after fp-82) splits the brain between fp-80 and fp-82: writers would dial cw-rails while readers still read Fabric's in-process store. Flipping types and backing separately makes each commit behaviour-preserving (ARCH 2), and it reuses an existing precedent rather than minting an abstraction (ARCH 11).

<!-- appendix -->

## five-programs-42 · 2026-09-24 — fp-79's falsifier fired; the type flip and the backing flip are split

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp79-20260924.md. The director reproduced it at 99a133b47.

- `git grep -n "fabric\.mesh_store\|\.contribution_emitter" -- 'sovereign/crates/sovereign-daemon/src/*.rs'`, comments excluded, returns 31 hits in 14 files. That matches the package file for file.
- `FabricPart::new` has three callers: daemon.rs:3256, state.rs:971, and sovereign-mesh tests/main/dst.rs:74. The row's pointers daemon.rs:3035 and bootstrap.rs:2526 are construction sites, not calls.
- AppState holds no store or contribution field of its own. `mesh_store` and `contribution_emitter` live only on FabricPart (fabric.rs:350, :392). StorePart holds inference_store and peer_preferences, and NodePart holds activity_emitter (state.rs:1025-1027). So "field NAMES survive" in fp-80 already implied minting those two port fields on AppState. The rewrite says so.
- fp-78's ports (sovereign-mesh/src/ledger_port.rs:48-91) are implemented only by `RailsLedger` (the daemon's rails_client). There is no in-process impl, so the readers could not have flipped to ports before the backing changed. `LocalRingRail` (rail_port.rs:174) is the existing in-process twin of a rail port. `MeshReplicatedKv` (peer_adapter.rs:99) already serves as the in-process `ReplicatedKv`.

Options weighed:
- (a) -36's merge. One commit spans two crates and about 49 files. That crosses fp-80's own §6 bound, so it only moves the halt one row later.
- (b) The package's resequence: fp-79 after fp-82, with fp-80 minting port fields over the dial while Fabric keeps its store. Between fp-80 and fp-82, contribution writes would reach cw-rails while contribution.rs and knowledge.rs still read Fabric's local store. That is a behaviour change inside the campaign that no row states.
- (c) Chosen. Types first over an in-process backing, then the backing in one small commit. It adds one row, fp-88, taking the state mint to 14 rows against -36's cap of 13. The cap was sized before this measurement, the same situation -35 and -36 corrected. The added row is the backing flip that fp-80 used to carry, not new scope.

REVIEW-AFTER: this departs from the prescription five-programs-36 wrote for its own falsifier (merge). The departure rests on that prescription's cost, measured here, and on the atomicity rule -36 itself kept.

Falsified if LocalLedger cannot implement a port without re-deriving a key scheme (then fp-79 is §6), or if a reader in fp-80 to fp-82 cannot move to a port while the backing is in-process, which would mean the type flip and the backing flip are not independent. It is also falsified if fp-88 exceeds about ten files once the ring-sync premise is checked.

</details>
