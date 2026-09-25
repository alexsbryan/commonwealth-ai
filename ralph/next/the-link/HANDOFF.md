# the-link — Halo handoff (git is synced)

*Written 2026-09-22 on the macOS peer after the operator pushed. If origin
has this file, everything the campaign needs is in git; nothing else travels.
The one machine-local file that matters is `ralph/models.env` (see step 2).*

## What has already happened

The campaign ran on the macOS peer to the edge of what that host can judge:
five rows `[x]` (checkpoint export + verify, link courier fields, the dial
measurement — rail-core wasm 415 KB, iroh 1.0.2 does NOT build for wasm32 at
the pin), plus a same-day repair pass (harness perimeter posture, F26
reclassification + registry split, conformance/golden regens) leaving
13,701 tests green and `commonwealth-rail-core` zero-diff from BASE.

What is left needs Linux and the room topology: `tl-2-offline-leg-exports`
(build landed; the DEMO checks are the Halo's), `REVIEW-DEMO-the-link-run`,
`REVIEW-audit-the-link`, and the operator's own `HUMAN-the-link-sneakernet`
(any second machine that was never a member).

## The plan, concretely

**1 — catch up** (inside the `sovereign-vulkan` toolbox for everything
below; from the Fedora host prefix each line with
`toolbox run -c sovereign-vulkan`):

```bash
git checkout main && git pull --ff-only origin
```

**2 — set the worker model** if `ralph/models.env` does not already say what
you want (it is gitignored, per-host):

```bash
printf 'MODEL=zai-coding-plan/glm-5.3-flash\nREVIEW_MODEL=zai-coding-plan/glm-5.3-flash\nRESOLVE_MODEL=zai-coding-plan/glm-5.3-flash\n' > ralph/models.env
```

(any model the host can reach is fine; that one was smoke-tested on the Mac.)

**3 — build first.** The demo measures binaries, never the tree, and refuses
stale ones:

```bash
./scripts/with-cargo-lock.sh ./scripts/dev-build.sh
```

**4 — launch the loop.** The block at the top of `ralph/next/the-link/STATE.md`,
verbatim. The queue's first ready row is `tl-2-offline-leg-exports`; the
worker runs its checks natively here (setsid, ELF, the A48 seal are all
Linux facts this host has) and `ralph-mark.sh` closes the row. The loop then
walks `REVIEW-DEMO-the-link-run` and `REVIEW-audit-the-link` on its own.

If you would rather drive the first row by hand, its three commands are:

```bash
RING_ROOM_TOPOLOGY=room scripts/ralph-check.sh demo-bg
scripts/ralph-check.sh demo-wait    # repeat until it returns; 0/1/4 are its codes
scripts/ralph-mark.sh tl-2-offline-leg-exports <sha> ralph/next/the-link/STATE.md
```

## What the runs should read

- The room verdict table is FOURTEEN bars: six `ra-room-*`, five `rg-*`,
  three `tl-*` — twelve PASSED and TWO DESIGNED CNJs, so `demo-wait` exits 4:
  `tl-link-carries-its-couriers` and `tl-dial-measured` name their proof
  sites (the cargo tests; `ralph/DECISIONS.md`) rather than reading 1.0 on
  kindness (order Demo §2-§3; the-link-6). `tl-checkpoint-verifies` is the new
  claim: the wall exports mid-cut (`created_unix` strictly inside
  `cut_at..heal_at`), the halo container verifies the file cold after
  `converged_s`, and three planted forgeries (flipped signature byte,
  truncated tail, grown same-seq fork) are refused by name in the same run.
- A `cut-not-a-cut` COULD-NOT-JUDGE means the run was not cold — rerun it.
- The rr-1 regression (`RING_ROOM_TOPOLOGY=three`, run once by REVIEW-DEMO)
  has TWO EXPECTED REDS that are the baseline, not failures: answer 0.8,
  plug-in 0.0 on `c_answer_names` — `docs/RING_ROOM_RUN_OF_SHOW.md` has the
  account. What the gate checks is that they have not MOVED.
- The audit row should fold in the macOS-side preliminary findings: the rail
  predictions held exactly (rail +0, rail-core +0 — the invariant), and the
  code crates overran — cli-llm +586 vs +130 predicted, daemon +358 vs +90,
  discovery +115 vs +55, demo script +193 vs +110, cli-shared +25 unnamed.
  Those are findings to record in `ralph/REVIEW_FINDINGS.md`, not to fix in
  the audit.

## Notes

- The parked `ralph/NEEDS_HUMAN.md` on the Mac is machine-local (excluded);
  it does not travel and the Halo owes it nothing. The halt it described is
  this handoff.
- Origin's pre-push hook was still red on `eval_cmd/runner.rs` (+59 over
  slack, raptor's `a313c9c18`/`d1a8596b2`) when the push went through —
  their split is still owed wherever that lane lives.
- Context if anything surprises: `docs/RING_ROOM_RUN_OF_SHOW.md` (the demo
  contract), `ralph/next/the-link/` (queue, order, charter, prompt),
  `ralph/DECISIONS.md` the-link-1..5 (why it parked), and the daemon logs
  under `target/ring-room-rr2-demo/` are ground truth.
