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
