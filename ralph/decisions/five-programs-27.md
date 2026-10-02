<!-- ledger -->

**five-programs-27 · 2026-09-24 · fp-42 + fp-47 (parked rows served as ready) · director** — this commit
- Needed: the loop served fp-42 as the first ready row. Its text says PARKED on the operator (five-programs-8), but its deps were all `[x]` and no HUMAN row held it. So a worker halted with no new fact, which is the stall five-programs-21 predicted for fp-42 and fp-47.
- Chose: gate both rows on new operator rows, HUMAN-fp42-state-store-cost and HUMAN-fp47-app-registry, using the fp-7/fp-9/fp-10 shape. Each row carries its options and a recommendation. No code changed. Boundary gate FAILED at 60 violations (`RALPH_QUEUE=five-programs scripts/ralph-check.sh boundary`, at 9f7ca8630).
- Because: both forks belong to the operator under the charter. fp-42's is the state-wire durable store owner, which the charter names outright, and it also means widening a forbid row. fp-47's arms are either an end-user reachability change or reversing a surface rails disclaims (five-programs-20/-21 precedent). Five-programs-9 argued that a HUMAN row would be redundant with the scout finding. The loop proved it is not: `pick_wave` gates on deps, not on prose.

<!-- appendix -->

## five-programs-27 · 2026-09-24 — fp-42 and fp-47 gated on operator rows; the queue is now operator-bound

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp42-20260924.md. Reproduced at 9f7ca8630:

- quality/ARCH_LAYERS.toml:684 `except` list for the `commonwealth-rails → commonwealth-*` forbid row does not name commonwealth-state; commonwealth-state/Cargo.toml:26 `rusqlite = { version = "0.35", features = ["bundled"] }`. The five-programs-8 cost is unchanged.
- boundary-gate: 60 violations. Both fp-42 edges are still red.
- fp-47's knot still holds. daemon.rs:288 holds `published_apps: commonwealth_media::PublishedApps`, and :4328-4351 hands the same handle to the iroh acceptor's `AppRoutes`. commonwealth-rails lib.rs:142-147 records that `cwth/app/0` is not bound there. Neither decision -9, -20 nor -21 resolved the fork. Decision -9 deliberately left the row without a HUMAN gate.
- scripts/ralph.py `pick_wave` skips `HUMAN-` rows and rows whose deps are not met. It does not read row prose. That is why "PARKED" in the text did not stop the serve.

After the edit, `python3 scripts/ralph.py plan --queue five-programs` serves HUMAN-fp58-bench-atlas-read. So no non-HUMAN row is ready. Every open non-HUMAN row now depends, directly or through other rows, on one of the seven HUMAN rows (fp-58, fp-54, fp-7, fp-9, fp-10, fp-42, fp-47). From here the campaign advances only on operator answers. That is the honest state, and minting work around the parked questions would add scope.

Recommendations are written on the rows. fp-42: (b), keep D4's interim. fp-47: (c), flip only the presence half and keep PublishedApps daemon-side.

Falsified if a later census shows either fork is settled by an existing §12 decision (then the HUMAN row was unnecessary and the row should have been rescoped), or if cw-rails already binds `cwth/app/0` (then fp-47's option (b) was never a reversal).

</details>
