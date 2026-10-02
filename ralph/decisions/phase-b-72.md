<!-- ledger -->

**phase-b-72 · 2026-09-30 · pb-serve-ranks-tests · director** — this commit
- Needed: the worker stopped pb-serve-ranks-tests at census with no code edited. Its file list held, but "shared helpers move with their first user" was false: every helper a moving file uses (`TestProvider` 28 users, `spawn_router` 51, `RecordingLedger` 20) is also used by files that stay. The director's reproduction found a second false premise the worker missed. sovereign-stock is a `[[distribution]]` whose dev edges count, so the stock-class moves as written add two BOUNDARY violations (17 → 19).
- Chose: split into four rows, in order.
  - -helpers: `TestProvider` goes to `sovereign_contracts::double` behind contracts' existing `test-fixtures` feature, with an optional `tokio`. The daemon's `common` re-exports it.
  - -daemon: five route-plumbing files stay and swap the adapter for one shared `LocalInferenceService` double.
  - -serve: scheduler_decision_records, manifest_fanout_concurrency, openai_finish_reason and throughput_ledger_emission go to serving-host's tests. openai_finish_reason is reclassified from daemon to serve class. throughput_ledger_emission's double is rewritten onto serve's own `LedgerEmitter`.
  - -stock: seven files go to stock/tests. Stock reaches serve's library through a declared face on `sovereign-serving-host` with empty `items`. The commonwealth-core records come from a `sovereign_daemon::double` behind a new daemon `test-doubles` feature, and ids come from kernel-types.
  - The PROOF grep is widened to the `sovereign_mesh::` re-exports of serving-host.
- Because:
  - Principle 8: one double per port, in the crate that owns the port, and one feature name per crate.
  - Principle 12: throughput_ledger_emission asserts serve's `Drop` → `LedgerEmitter`, and the mesh port is only its scaffolding.
  - Principle 5: the narrow grep could pass while the tests still drove serve through mesh.
  - The distribution gate's own remedy is "declare that program's face". phase-b-68 is the precedent for a director adding a face.
  - Trials: -helpers COMPILE 48s, 0 errors, BOUNDARY 17, LAYER ✓. The -stock face gave BOUNDARY 17 with the face and 19 without it. Both trials were reverted.
  - BOUNDARY is 17 at 05a2b40da and this commit changes no Rust.

<!-- appendix -->

## phase-b-72 · 2026-09-30 — pb-serve-ranks-tests splits into -helpers, -daemon, -serve, -stock; stock's tests reach serving-host through an empty-items face

<details><summary>reasoning, evidence, package</summary>

The worker's package is archived at target/ralph/phase-b/pb-serve-ranks-tests-needs-human.phase-b-72.md. The director reproduced it at 05a2b40da.

- `grep -rl 'sovereign_daemon::slot_manifest\|sovereign_daemon::inference_adapter\|sovereign_serving_host' sovereign-daemon/tests` finds 20 files with 5,578 lines. The widened grep, `sovereign_serving_host|slot_manifest|inference_adapter|sovereign_mesh::(peer_inference|guest_lender|pinned_pod_snapshot|pinned_worker_source|worker_eligibility|model_fetch)`, finds 21. The one extra is chat_completion_e2e/model_resolution.rs, a submodule that already moves.
- Helper users, by a per-file grep of `common::` names: the serving-class movers use only `TestProvider::new` / `.with_model_id` and, in throughput_ledger_emission, `RecordingLedger`. The stock-class movers use `spawn_router`, `member`, `member_with_last_seen`, `id_to_hex`, `TestProvider` and `RecordingLedger`.
- `TestProvider` (common/mod.rs:259-568) names only contracts types, `futures`, `async_trait` and `tokio::time::sleep`.
- Contracts already has a `test-fixtures` feature (Cargo.toml:75, middleware.rs:178). sovereign-core and sovereign-cli-dev enable it.
- `LocalInferenceService` is a contracts trait (local_inference.rs:34). openai_finish_reason reaches it through the daemon's re-export (state.rs:26) and has no AppState and no route. Its class is serve.
- serving-host's ledger.rs:70-80 already has a double on `LedgerEmitter` that records `(NodeId, String, u64)`, and serving-host's `NodeId` is kernel_types'.
- Stock trials, run from corpus-engine in the toolbox with `cargo xtask boundary-gate`, all reverted:
  - baseline: 17;
  - stock dev-deps `commonwealth-core` and `sovereign-serving-host`: 19, `[stock] sovereign-stock → commonwealth-core` and `→ sovereign-serving-host`, "a dev dependency outside the distribution's own crates, the shared leaves and its faces";
  - with `[[distribution.face]] package="cmnwlth" krate="sovereign-serving-host" items=[]` and only the serving-host dev-dep: 17, and layer-gate ✓.
- Tests do not count toward the distribution's 300-line cap: size_gate.rs `in_test_tree` puts `/tests/` lines in the test column.
- -helpers trial: common/mod.rs:259-568 moved to sovereign-contracts/src/double.rs, `#[cfg(feature = "test-fixtures")] pub mod double;`, `test-fixtures = ["dep:tokio"]`, and the daemon dev-dep with the feature. `cargo check -p sovereign-daemon --tests --features treesitter` finished in 48.36s with 0 errors, BOUNDARY stayed at 17, and LAYER passed. Reverted.

Rejected:
- Moving throughput_ledger_emission unchanged to stock, which was the worker's recommendation. It would keep a serve test on a mesh port and need the commonwealth-core edge for no assertion stock owns.
- A local `TestProvider` stub in serving-host, which is a second copy of the double.
- A cross-crate `#[path]` include of the daemon's common/. That is a fragile relative path, and it drags in corpus_index and DaemonServices.
- Reaching serve types through the `sovereign_mesh::` shims. That hides the edge from the gate, and the parent will delete or strand those shims.

What would falsify this:
- A stock-class test that must name a commonwealth-core type the daemon double cannot build. The -stock row stops at census and must not add a commonwealth-core face on its own authority.
- The `sovereign_daemon::double` move failing COMPILE. That half was not trialed.
- An empty-items face being read as an exception in disguise. It admits only test edges, because the face-item scan reads `src/`, but it is a new use of the face mechanism. REVIEW-AFTER: operator to confirm the empty-items face shape for stock's composition tests.

FIVE_PROGRAMS §2c now records the empty-items face rule.

</details>
