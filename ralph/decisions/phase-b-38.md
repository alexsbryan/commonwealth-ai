<!-- ledger -->

**phase-b-38 · 2026-09-28 · pb-work-donor · director** — this commit
- Needed: the worker stopped at census with five forks: where the projection's types live (1), `RingRailPort::work_projection`'s signature (2), the donor's foreground yield (3), where the ingest:v1 proof runs (4), and a split for a measured ~5,500-line lift against 2,600 (5).
- Chose: forks 1 and 2 were false premises, because pb-work-doors already put every projection type, `LEASE_MS`/`MAX_UNIT_ATTEMPTS`, ActorKey and `HandoffPhase` in the leaves. The only non-wire read, `phase_at`, moves its body onto `WorkHandoff` in oicp-types, and the row opens with that repoint (BOUNDARY 0). Fork 3 keeps the behaviour: a loopback `POST /v1/work/yield` that the daemon posts, and the donor's take reads. Fork 4 is proved by a daemon e2e over a spawned `CW_RAILS_BIN`, not a dev-dependency. Fork 5 is re-priced to ~3,800 and not split.
- Because: `HandoffPhase` is oicp-types' (work_queue.rs:288, re-exported by commonwealth-core knowledge.rs:5). The trial compiles and leaves only the moving files and the two sites the row removes. The census's recommended yield option (b) is an end-user delta the row does not state. The census's dev-dependency plan fails the boundary gate, which counts dev edges (boundary_gate.rs:710-713).

<!-- appendix -->

## phase-b-38 · 2026-09-28 — pb-work-donor: the wire half is already in the leaves; keep the foreground yield through a cw-rails door; prove ingest over a spawned binary

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md (removed in this commit), census at a2a02af8d, no code written.

Evidence, reproduced by the director at a2a02af8d:
- `WorkUnitStatus` oicp-types work/projection.rs:50, `ProjectedUnit` :119, `WorkHandoff` :172, `WorkProjection` :293, `WorkRefusal` work/refusal.rs:116 (with `Yielding` at :167), `MAX_UNIT_ATTEMPTS`/`LEASE_MS` work/mod.rs:32-36, `ActorKey` kernel-types actor.rs:36. `HandoffPhase` is oicp-types work_queue.rs:288, and commonwealth-core knowledge.rs:5 is `pub use oicp_types::work_queue::*`. commonwealth-work projection.rs:95-100 re-exports the types. So the census's fork-1 recommendation (move them to oicp-types) is already done, and fork 2 reduces to naming the same type by its leaf path.
- pb-work-doors kept `phase_at` in commonwealth-work because it "names commonwealth-core's HandoffPhase". That is false for the reason above. `phase_at` reads only `revoked`, `units`, `expires_at_ms` and `ProjectedUnit::status_at`, all in oicp-types.
- Trial (applied, reverted; raw log at target/ralph/phase-b/trials/t-work-donor-wire.log): `WorkHandoff::phase_at` added in oicp-types, the commonwealth-work free fn made a delegate, and ingest_executor.rs:83,85,86,116,682 plus rails_client.rs:653 repointed. `cargo check -p commonwealth-work --features process -p sovereign-daemon --lib` finished clean. With `commonwealth-work` then removed from sovereign-daemon's Cargo.toml it gives 15 errors: work_donor.rs 12, work_donor_checkout.rs 1, ingest_executor.rs:84 (`JobExecutor` import, replaced by the origin) and daemon.rs:3008 (`Sandbox::probe`, donor wiring). LAYER and BOUNDARY were not re-run on the trial. The edge removal is the row's own, already trialed at f7238e6d3 (BOUNDARY 49 → 48).
- Daemon tests naming commonwealth_work: 2,963 lines across six files. The non-import uses are `projection::fold` ×3, `seal::seal` ×3, `to_payload` ×2, `phase_at` ×3, `Sandbox::Direct` ×1 and `JobExecutorRegistry` ×1. corpus-engine/xtask/src/boundary_gate.rs:710-713: "a third party who lifts a package carries its tests, so this gate does not get to ignore them". The census's claim that "the boundary map ignores dev edges" read the layer map's comment as the boundary gate's.
- scripts/program-lift.toml:105-110: `[lift.cmnwlth]` builds and tests commonwealth-rails only. The census is right that the ingest:v1 proof cannot run there.
- Foreground yield: work_donor.rs gates on `offer.yield_to_foreground && app_state.should_yield_to_foreground()` (state.rs:1466). `[compute.work_offer] yield_to_foreground` defaults on (sovereign-contracts setup_config.rs:889-919).

Fork 3 options, as the census gave them:
- (a) the daemon publishes its foreground to cw-rails through a door. This keeps the behaviour and adds about 150 lines.
- (b) the ingest origin refuses while process:v1 stops yielding. This changes what a svrn user sees: a donated process unit competes with their chat turn.
- (c) drop the flag.

(b) and (c) are the charter's "changes end-user-observable behaviour beyond what a row states", so they belong to the operator. (a) is the in-charter choice. It passes principle 12 because the daemon owns its foreground and cw-rails owns only the take and a deadline. It also passes the cmnwlth-alone test: with no daemon, nobody posts and nothing yields, which is what happens today on a daemon-less node.

Fork 5: the census priced the six test files whole (2,963) and counted the edge-free repoint as work. Most of those files change by imports. The re-sourced part is the four fold/seal files, about 700 lines. The re-price is ~3,800, under 2× the stated 2,600. The wire repoint retires no edge alone, so under the charter it is the row's first commit, not a row of its own.

FIVE_PROGRAMS is unchanged. §2c already names job execution as one drive, and the row keeps it at one. The yield door is local (not mesh-facing), so §4 rule 8 does not speak to it, and no program boundary moves.

Falsified if:
- the `phase_at` move fails to compile once the free fn is a delegate, for example because a caller relies on the `HandoffPhase` path through commonwealth-core in a way the re-export does not preserve;
- a daemon test cannot express its fixture without the fold, which would make "build the value from pub fields or append through the spawned binary" insufficient;
- the per-turn post to `/v1/work/yield` measurably slows a chat turn. Then the yield signal needs a different carrier, and the operator should see the numbers.

REVIEW-AFTER: pb-work-donor lands. The yield door is new surface that the charter's "decide a false premise" covers only by keeping behaviour. Check that it earned its lines.

</details>
