<!-- ledger -->

**engine-swap-4 · 2026-10-23 · es-f1-rpc-peer-child · director** — this commit
- Needed: engine-swap-3 set `needs [linux]` on the lane branch (ff522a675), and the Mac claimed the row again three minutes later (claim f9c249068, 15:52) and its worker bounced straight back to a director. eutropia's machine fit reads a row's `needs` from `STATE.md` at the queue branch tip (`eutropia-git/src/poll.rs:184`, filtered at `eutropia/src/run.rs:1285`), and `origin/eutropia/engine-swap` (cfb46cf86) still has the row with no `needs`. A lane edit reaches dispatch only when the row lands, which this row cannot do on the Mac.
- Chose: the row and acceptance stay as engine-swap-3 left them; the one missing act, putting the two lines on the queue branch, is a push, which the charter leaves to the operator. Parked with a package: `control/queue-needs-linux.patch` makes the queue tip's es-f1 row and MACHINES.md line byte-identical to the lane's, so the landing merge stays clean (checked with `git merge-tree` against ff522a675). Parking also stops the Mac re-claiming the row and burning a worker and a director each round.
- Because: `continue` would release the row to the same Mac under the same queue tip and loop. The decision itself is the charter's and already made; only the push is the operator's.

<!-- appendix -->

## engine-swap-4 · 2026-10-23 — a director's row edit on a lane does not reach dispatch; es-f1 parks for a queue-branch push

<details><summary>reasoning, evidence, package</summary>

Reproduced here (Mac, lane at ff522a675):

- `git show origin/eutropia/engine-swap:ralph/next/engine-swap/STATE.md`
  line 69: `es-f1-rpc-peer-child — depends [es-p1-build-modes] — A peer …`,
  no `needs`. MACHINES.md there still says `none`.
- `eutropia status`: `es-f1-rpc-peer-child leased by de76a8a4…` (this Mac),
  tip f9c249068 `claim`, after 5ece37aed `release continue`.
- `refs/eutropia/engine-swap/lane/es-f1-rpc-peer-child` is ff522a675, so a
  Halo claim resumes the lane with the code and engine-swap-3 in it.

Finding, not a row (scope is frozen): a director can change a row's text on
its lane, but every field dispatch reads (`needs`, `depends`, heavy) takes
effect only at landing. A `needs` correction for a row that cannot land on
the machine holding it has no path except the operator's push. That is
eutropia's to settle, not this queue's.

</details>
