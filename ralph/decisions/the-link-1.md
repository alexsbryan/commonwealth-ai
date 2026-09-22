<!-- ledger -->

**the-link-1 · 2026-09-22 · tl-2-offline-leg-exports · director** — this commit
- Needed: the row's DEMO-BG/DEMO-WAIT checks are unrunnable on this macOS host (Mach-O `target/debug`, no `room`/`uplink` podman nets, no `sovereign-vulkan` toolbox; `demo-bg`'s `setsid` is util-linux) and the worker packaged this as NEEDS_HUMAN instead of stalling.
- Chose: the demo checks run on the Halo — where the run-of-show already pins the room demo — as soon as the operator pushes this tree there; the row stays `[~]` until that run passes and `scripts/ralph-mark.sh` lands. The macOS `demo-bg` silent no-op is recorded as a hazard, not fixed here.
- Because: O Demo §1 already names the Halo ("After the heal, the Halo verifies that file cold") and RS §"Running it" pins builds + demo to the `sovereign-vulkan` toolbox; the tree contradicts only the premise that the checks run on the worktree's host (charter: fix a row whose premise the tree contradicts, citing the order step). Reaching the Halo needs a push — the operator's. A `demo-bg` macOS fix is not strictly necessary for flow (the demo runs where `setsid` exists) and changes peer-observable tool behaviour.

<!-- appendix -->

## the-link-1 · 2026-09-22 — tl-2's demo checks run on the Halo; the row waits there, not here

<details><summary>reasoning, evidence, package</summary>

**The fork.** `tl-2-offline-leg-exports`'s code is committed (`4f08eafdc`); its
two remaining checks (`DEMO-BG`, `DEMO-WAIT`) could not run on this macOS
host. The worker's package (`ralph/NEEDS_HUMAN.md`, removed by this commit)
asked three questions: where the room demo runs, whether to fix `demo-bg` on
macOS, and what the expected paste is.

**Evidence — every package claim reproduced here (2026-09-22):**

- `git grep -n "export RING_DOC_BACKEND=podman" scripts/ring-room-demo.sh` →
  line 77; under `RING_ROOM_TOPOLOGY=room` the room nodes are
  `beefy halo little phone`, all podman.
- `file target/debug/sovereign-cli` → Mach-O 64-bit arm64. The room's Linux
  containers cannot exec it; the demo "measures the binary, never the tree"
  (RS §Running it), so running it here needs a cross build or a forked target
  dir — invented machinery no row names (charter: strictly necessary).
- `podman network ls` → today the podman machine is DOWN on this host
  (connection refused on 127.0.0.1:62636); during the session it was up with
  only the default `podman` bridge — no `room`, no `uplink`. The room demo
  has never run here either way.
- `podman inspect sovereign-vulkan` → no such object: the `sovereign-vulkan`
  toolbox image (RS §Running it's venue) does not exist on this host.
- `scripts/ralph-check.sh` `demo-bg` verb: `setsid nohup bash -c … &` then
  `echo started …; exit 0` — on macOS `setsid` does not exist, the
  backgrounded job dies instantly as command-not-found, and the verb exits 0
  having started nothing. Reproduced by reading the code; the worker hit it
  live (started pid, no demo.log, no demo.rc).
- `scripts/ralph-check.sh clean` → exit 0 (the du gate; the one check that
  could run here and did pass).

**The decision.** The order already names the venue: O §Demo step 1 —
"After the heal, the Halo verifies that file cold" — and RS §"Running it"
builds and runs the demo inside the `sovereign-vulkan` toolbox on the amd64
Fedora host. The row's premise that was false is only "the checks run on the
host holding the worktree". Corrected in the row, citing the order step —
the charter's "fixing a row whose premise the tree contradicts, when the
order already implies the fix".

**What remains, and whose it is:**

1. Operator: push this tree (34+ commits ahead of origin; pushing is the
   operator's per the charter and AGENTS.md).
2. On the Halo (toolbox), the row's checks verbatim:
   `RING_ROOM_TOPOLOGY=room scripts/ralph-check.sh demo-bg scripts/ring-room-demo.sh`,
   then `demo-wait` to exhaustion (it returns exit 3 while running; repeat),
   then `scripts/ralph-mark.sh tl-2-offline-leg-exports <sha>
   ralph/next/the-link/STATE.md`.
3. Expected paste (the worker's §c3, confirmed against the row and bar C):
   six rr-2 and five rg rows PASSED beside three tl rows —
   `tl-checkpoint-verifies` PASSED; `tl-link-carries-its-couriers` and
   `tl-dial-measured` read COULD-NOT-JUDGE naming their proof sites until
   tl-3 lands and REVIEW-DEMO judges them; demo exit 4 until then is the
   four-verdict rule working, not a failure.

**Campaign flow after this commit.** The supervisor resumes; the first `[ ]`
row with all deps `[x]` is `tl-3-dial-measured`, which runs on THIS host
(wasm re-measure + the iroh wasm32 outcome; clause (c) is dead — the
inventory measured iroh 1.0.2 NO for wasm32 at the locked pin). After tl-3
the queue is honestly exhausted pending the Halo: REVIEW-DEMO, REVIEW-audit
and HUMAN-sneakernet all need the room topology or the operator.

**REVIEW-AFTER: the macOS `demo-bg` hazard.** Not fixed here — not strictly
necessary for flow, and it changes peer-observable tool behaviour, which the
charter leaves to the operator. Recommendation on record: an explicit host
refusal (a `setsid`-availability check that exits non-zero naming the host)
rather than a macOS fallback — an exit-0 that started nothing is a gate that
lies (ARCH 5/6). Same class of latent hazard: every demo-gated row
(REVIEW-DEMO included) is Halo-bound, which the next NEEDS_HUMAN package
should not have to rediscover.

**What would falsify this decision:** the room demo running green on this
macOS host without new machinery (e.g. podman gains a Linux builder the rows
already name), or the Halo losing the toolbox so that RS §"Running it" stops
naming a real venue — either reopens the fork.

</details>
