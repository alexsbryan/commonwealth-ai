<!-- ledger -->

**phase-b-73 · 2026-09-30 · pb-serve-ranks-tests-serve · director** — this commit
- Needed: the worker stopped at PLANT with the outcome landed (ee1a79259, 96346da12) and every other check green. The row's plant, inverting `count > 0` in `ThroughputObservedStream`'s Drop (throughput_tracking.rs:211), stays green at 256 tests because that line no longer decides the ledger emission.
- Chose: the row's PLANT moves to the decider that owns emission today, the `n > 0` filter in `ledger::emit_from_outcome` (ledger.rs:43). The row is marked done at 96346da12.
- Because:
  - Principle 5: a plant has to hit the line that decides the behaviour. Emission is decided at ledger.rs:43, and its own negative-control test (ledger.rs:137) calls it "the same `count > 0` gate the stream wrapper had".
  - Principle 8: one decider. `count > 0` at :211 is redundant with `first_chunk.is_some()`, because `chunk_count` and `first_chunk_at` are set in the same `is_data_frame()` branch (throughput_tracking.rs:172-176). So no test can guard it, and leaving it unguarded is not a gap.
  - Reproduced at 96346da12, in the toolbox via `scripts/ralph-check.sh test sovereign-serving-host`:
    - the successor plant gives pass 253, fail 3, and `throughput_ledger_emission::peer_routed_stream_emits_inference_received_on_drop` panics at tests/main/throughput_ledger_emission.rs:272;
    - after the revert it gives pass 256, fail 0.
  - BOUNDARY 17, delta 0. This commit changes no Rust.
- Correction: ee1a79259's body gives the moved test count as 25 + 2 + 3 + 2 = 34. The junit count is 26 + 2 + 3 + 2 = 33, and it agrees with the daemon's 1158 → 1125. The body is not amended because the tree is shared.
- Falsified if: a path emits `InferenceReceived` without going through `emit_from_outcome`, or a data frame can raise `chunk_count` without setting `first_chunk_at`.

<!-- appendix -->

## phase-b-73 · 2026-09-30 — pb-serve-ranks-tests-serve's PLANT moves from the stream Drop to emit_from_outcome's `n > 0` filter

<details><summary>reasoning, evidence, package</summary>

The worker's package is archived at target/ralph/phase-b/pb-serve-ranks-tests-serve-needs-human.phase-b-73.md.

- `emit_from_outcome` has one production caller, throughput_tracking.rs:247 (`grep -rn emit_from_outcome sovereign/crates`), and it runs inside Drop's spawned task. Every stream emission therefore passes through the ledger.rs:43 filter.
- The raw log for the successor plant is `target/sovereign-test/latest/cargo.raw.log` at the time of the run. Its three FAILED tests are `ledger::tests::a_peer_outcome_with_tokens_mints_one_fact`, `ledger::tests::a_zero_token_peer_outcome_mints_nothing` and the moved `throughput_ledger_emission::peer_routed_stream_emits_inference_received_on_drop`.
- Rejected: naming a plant at :211. Its `count > 0` term cannot change the outcome while it is implied by `first_chunk.is_some()`. Deleting that redundant term is a cleanup that advances no finish item, so it stays off this queue (scope guard, phase-b-29).

</details>
