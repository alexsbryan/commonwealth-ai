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

---

## Item 7 — the maintenance sweep must not pin the query cache  ✅ LANDED

**Sites, re-verified.** `corpus_maintenance.rs:139` (`sweep_once`'s `engine.open_index`) —
the doc cited `:124-139`, still correct. `engine/mod.rs:186` — `index_cache`, an
insert-only `HashMap<PathBuf, (SystemTime, CorpusIndex)>` with **no eviction path
anywhere** (`grep -rn index_cache` returns exactly the declaration, the two accesses inside
`open_index`, and the constructor). So a handle admitted to it is resident for the life of
the process, and the hourly sweep visits *every* installed corpus.

**Fix.** `CorpusEngine::open_index_transient` — read-through, never write. `open_index` and
it now share one body (`open_index_inner`) parameterised by a `CacheOnOpen` enum, so
there is exactly one open/validate/cache implementation (§10.6) and the call site says what
`No` means (§2.1 — no bare bool). A cache HIT is still served from the cache: the sweep may
benefit from a handle the query path already paid for, it may not create one. That
asymmetry is the whole fix, and it is why the §5 LRU proposal was the wrong shape — an
hourly all-corpora scan is a textbook LRU-flusher.

`index_cache_len()` is the new observability surface for "how many handles are resident",
and is what the test asserts on.

**Red-first evidence.** `corpus-engine/tests/index_cache_residency.rs`. Two tiny real
LanceDB corpora are ingested with a mock 8-dim embedder, then a sweep tick is simulated
over all of them. Run with `open_index_transient` wired to `CacheOnOpen::Yes` — i.e.
byte-for-byte what the pre-fix sweep did:

```
thread 'a_full_sweep_admits_no_handles_to_the_query_cache' panicked at
  corpus-engine/tests/index_cache_residency.rs:140:5:
  assertion `left == right` failed: a background sweep may read through the query
    cache but must never populate it
    left: 3   right: 0
pass: 0  fail: 1   cargo exit: 100
```

Three corpora, three handles pinned, after ONE tick. With the fix: `pass: 6  fail: 0
cargo exit: 0` (the `--filter sweep` scope, which also picks up the pre-existing sweep
tests — they stay green).

The second test, `the_query_path_still_caches_and_the_sweep_reuses_its_handles`, exists
because "the sweep no longer caches" is also satisfiable by breaking the cache outright,
which would cost retrieval a LanceDB re-open per corpus per query. It pins that the query
path still admits, and that a sweep neither evicts the hot handle nor adds the cold one.

**Glassbox.** A transient open emits at DEBUG with `index_path` + `resident_handles`, on
the default target (the `corpus_maintenance` custom-target allowlist gotcha does not apply
— this event is in `corpus-engine`, untargeted).

**The per-handle memory number does not exist in this test** — it proves the mechanism, not
the magnitude. That is Probe B's job.

---

## Item 1 — surface the gossip push failures  ✅ LANDED

**Sites, re-verified.** The doc's `gossip.rs:658-667` / `:669-679` had drifted; both
branches are in the `Step 4: mesh_store replication` fan-out and both were `tracing::debug!`.
`max_online_peers_before_false_offline` at `:248-262` was correct — and `grep` showed its
only callers were **its own unit tests**. Nothing in production ever evaluated it, which is
why the doc calls it "checkable" but nobody was checking.

**Three changes, all instrumentation. No gossip behaviour changed** — no retry, no split,
no fanout change (that is Tier-2 business, and §7.2 says raising fanout is the wrong fix
anyway).

**(a) Push-failure surfacing.** A `PushOutcome` enum (`Ok` / `Rejected(status)` /
`Transport`) — a closed set, and the distinction is the point: 413 means "the snapshot
outgrew the receiver's body limit", a transport error means "we never reached them", and
those are different operator actions. The outcome is decided *per peer, after the address
list is exhausted*, not per address: a single failed address on a multi-homed peer is
expected (stale LAN IP behind a working Tailscale address) and stays at debug. Only "this
peer is not taking our snapshot" reaches WARN.

Rate limiting is per peer per **status transition**, via `PushStatusLedger`. Per-round
would be 8,640 lines/day for one broken peer on a 10s cadence, which operators filter, which
is functionally the same silence it replaces. Recovery is logged too (INFO), so a WARN
always has a matching all-clear.

**(b) Payload gauge.** The snapshot is now serialised ONCE and the bytes posted, so the
gauge measures the exact body on the wire (and the fan-out stops re-serialising per peer per
address — an incidental win). Gauge at DEBUG every round; WARN at
`MESH_STORE_PAYLOAD_WARN_BYTES`, which is `commonwealth_api::server::MAX_REQUEST_BODY_BYTES
/ 2` — **derived, not retyped**. `MAX_REQUEST_BODY_BYTES` was made `pub` for exactly this
(§10.6: the number a sender warns against and the number a receiver enforces must be one
number). A test asserts the derivation, so retyping it fails the build.

**(c) Online-population warn-rail.** Evaluated in `spawn_gossip_loop`, not `run_one_round` —
the loop is what holds the `interval` the formula needs, and putting it there avoids a
signature change across `run_one_round`'s 6 callers (dst.rs + 3 test files). Latched, so it
warns on crossing, not every 10s. The warn text says explicitly that raising fanout is NOT
the indicated fix, because §7.2 corrects §3 on precisely that point: the formula is a
worst-case no-relay sufficient condition, not an operating ceiling.

**Red-first evidence.** `tests/gossip_push_surfacing.rs` drives a REAL gossip round against
a REAL axum peer that answers `/internal/app/state` with 413, capturing at **WARN** — the
level a daemon with no `RUST_LOG` actually emits. Run with the surfacing block removed (the
pre-fix state: both branches debug-only):

```
thread 'a_rejected_mesh_store_push_reaches_an_operator_at_warn' panicked at
  sovereign/crates/sovereign-mesh/tests/gossip_push_surfacing.rs:192:5:
  a peer refusing our anti-entropy snapshot must be visible at WARN — a daemon with
  no RUST_LOG never emits debug, which is how replication could stop dead with every
  surface green.
captured at WARN:
                          ← empty. That is the finding.
pass: 0  fail: 1   cargo exit: 100
```

With the fix: green, and the same test asserts the SECOND round with the peer still broken
is silent (the transition contract). Five unit tests in `push_surfacing_tests` pin the
ledger contract (first-sighting failure surfaces; 8,639 repeats stay silent; a change of
failure SHAPE re-surfaces; per-peer independence) and the derived gauge constant.

**Glassbox verified, not assumed.** `the_payload_gauge_renders_at_debug` asserts the gauge
event actually reaches a `tracing=debug` subscriber and carries `payload_bytes` +
`warn_at_bytes` + `limit_bytes`. None of the new events use a custom `target:`, so the
allowlist gotcha does not apply — checked rather than recalled.

Full `sovereign-mesh` suite: `664 passed, 0 failed, cargo exit: 0`.
