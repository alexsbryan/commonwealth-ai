# Iteration journal — order `mesh-scale-t0`

Branch `mesh-scale-t0`. Tier 0 of `MESH_SCALE_100_USERS_1000_CORPORA.md` §7.4, seven
independent items, each landing with a red-first test. This file is the evidence trail:
for every item, the run that FAILED on the pre-fix code, and the run that passed after.

Gate discipline: iterate with `--package`/`--filter`; the full lint+test pair runs once at
the end (and once mid-branch if the diff grows risky).

---

## Item 2 — jitter the constant shed `retry_after_secs: 2`  ✅ LANDED

**Site.** `commonwealth/crates/commonwealth-api/src/state.rs:2086` (`admit_peer_request`,
the `CeilingExceeded` arm). Verified by grep before editing — the doc's cited `:2082` had
drifted by 4 lines.

**Survey (§19, inventory before building).** `grep -rn "retry_after_secs: [0-9]"` over the
workspace found exactly three constant hints:
- `state.rs` `CeilingExceeded` — the live shed path. **Fixed.**
- `routes_inference.rs:1424` — a test fixture (`AlwaysSheds`), not a live path. Untouched.
- `mobile_host.rs:311` — a *generated config file* value (`GenServer.retry_after_secs`),
  not a per-shed hint. Untouched.

The *local* queue shed (`model_slot.rs:992`) does NOT share the constant — it derives
`retry_after_secs` from its own `predicted_wait_ms`, so it is already spread by the wait
distribution. Nothing to jitter there. (Order text: "also the local shed's retry hint *if
it shares the constant*" — it does not.)

**Fix.** `admission::jittered_retry_after_secs(base)` — one decider, one name (§10.6), used
by the shed path. Split in two so the policy is testable apart from its entropy source:
`jitter_retry_after(base, entropy)` is pure; the public wrapper supplies entropy as a
process-local counter mixed through a splitmix64 finalizer with the wall clock's
nanosecond field. The counter alone would phase-align across processes; the nanos alone
would collide under a coarse clock. Spread constant: `RETRY_AFTER_JITTER_SPREAD_SECS = 4`,
so a shed hint lands in `[2, 6)`.

**Red-first evidence.** Test `admission::tests::ceiling_shed_retry_after_is_jittered`
(32 sheds at ceiling 0, assert ≥3 distinct hints, all inside the window).

Run with the pre-fix constant restored (`retry_after_secs: 2`):

```
thread 'admission::tests::ceiling_shed_retry_after_is_jittered' panicked at
  commonwealth/crates/commonwealth-api/src/admission.rs:449:9:
  a shed hint with no spread is a synchronized-retry generator; got {2}
pass: 1  fail: 1   cargo exit: 100
```

Run with the fix in place: `pass: 2  fail: 0  cargo exit: 0`.

**No env knob added** — the spread is a policy constant, not an operator dial, so there is
nothing to declare in `quality/env-flags.toml`.

---

## Item 6 — spawn `RetentionGc` in the sovereign daemon  ✅ LANDED (with one named change of shape)

**Sites.** `grep -rn RetentionGc` confirmed the order's claim: the only construction was
`commonwealth/crates/commonwealth-daemon/src/main.rs:789`. The sovereign daemon's
`MeshStore` is built in `sovereign/crates/sovereign-mesh/src/daemon.rs:2300` and handed to
`AppState`, so the spawn belongs there — next to the `StorageSnapshot` loop, which already
carries the shutdown-channel pattern this reuses. (The order's Scope named
`sovereign-cli-daemon/src/`; the store and every other daemon background task live in
`sovereign-mesh/src/daemon.rs`. Named substitution, not a silent one.)

**What I did NOT do, and why.** A verbatim copy of `commonwealth-daemon`'s spawn is
`RetentionGc::new(store, 7 days, hourly)`, which sweeps the WHOLE store by age. On the
sovereign daemon that is not safe, and the hazard is not hypothetical:

- `PROCESSED_SHARDS_APP_ID` markers (`auto_ingest.rs:575`) are a write-once dedup ledger.
- `corpus-engine/handoff:*` records (`shard_manager.rs:158`) are written once per handoff.

Neither is ever rewritten, so an age-based whole-store delete removes them and re-opens
ingest work the mesh already completed. `RetentionGc` therefore gained an optional
`app_scope` (`scoped_to_app`), backed by `MeshStore::gc_app` /
`SqliteBackend::delete_older_than_in_app`. The unscoped path is untouched, so
`commonwealth-daemon` behaves exactly as before.

**TTL is not a new number.** Every reader of the ledger (`current_contributions`,
`commonwealth balance`) aggregates over `DEFAULT_WINDOW_DAYS` (30, `commonwealth-core`),
so a row older than the window is provably invisible to every reader. The GC TTL is
derived from that constant rather than being independently chosen (§10.6 — one decider).
Note in passing: `commonwealth-daemon`'s 7-day TTL is *narrower* than its own 30-day read
window, i.e. it silently truncates balances. Left alone — out of scope, banked as a
finding below.

**Red-first evidence.** `gc::tests::scoped_gc_bounds_the_ledger_without_touching_other_apps`
seeds one out-of-window ledger event, one in-window event, and one 400-day-old
processed-shards marker. Run with the pre-fix, unscoped `RetentionGc` (the `.scoped_to_app`
call removed — i.e. exactly what a verbatim copy of the prior art would have spawned):

```
thread 'gc::tests::scoped_gc_bounds_the_ledger_without_touching_other_apps' panicked at
  commonwealth/crates/commonwealth-state/src/gc.rs:137:9:
  assertion `left == right` failed: only the out-of-window ledger event is dead
pass: 1  fail: 1   cargo exit: 100
```

(It deleted 2 — the ledger event AND the shard marker.) With the scope in place:
`pass: 2  fail: 0  cargo exit: 0`. `unscoped_gc_sweeps_every_app` pins the old behaviour
so the two paths cannot drift.

Scoped lint (13 crates, `--all-targets`): `errors: 0  cargo exit: 0`.

**Glassbox.** Spawn emits `RetentionGc started (contributions ledger)` at INFO with
`app_scope`/`ttl_days`/`interval_secs`; each non-empty sweep emits at DEBUG with
`deleted`/`ttl_secs`/`app_scope`. Both on the default (no custom `target:`) so the
allowlist gotcha does not apply. Verified live in Probe A (see §Probes).

**Finding for the backlog (not this branch).** `commonwealth-daemon` GCs its ledger at 7
days while aggregating over 30 — balances there are truncated by the GC, silently.
