# es-f1-rpc-peer-child: the director needs the operator: needs [linux] (engine-swap-3) is on the lane only; dispatch reads the queue branch, which still says none, so the Mac re-claims and bounces. Pushing the attached two-line patch to eutropia/engine-swap is the operator's; then unpark and the Halo resumes at 503ecb92e.

# es-f1-rpc-peer-child — push needs [linux] to the queue branch, then unpark

The row is parked because the Mac keeps claiming it. engine-swap-3 set
`needs [linux]` on the lane (ff522a675), but eutropia reads `needs` from
STATE.md at the queue branch tip (`eutropia-git/src/poll.rs:184`,
`eutropia/src/run.rs:1285`), and `origin/eutropia/engine-swap` (cfb46cf86)
still has no `needs` on the row. So the Mac re-claimed it (f9c249068) and its
worker bounced. The code and Mac gates are done at 01792faf8; the lane ref
`refs/eutropia/engine-swap/lane/es-f1-rpc-peer-child` is ff522a675. What's left
is Linux-only: HEADROOM, `rpc_peer_child_e2e` with its `booted:` line, and
NO-ORPHANS.

## Options

1. **Push the patch, then unpark (recommended).** `queue-needs-linux.patch`
   (beside this file) changes two lines on the queue branch: the es-f1 row in
   STATE.md and its MACHINES.md line. Both match the lane byte for byte, so
   the row's landing merge stays clean (checked with `git merge-tree`). Cost:
   one push and one unpark. After that, the Mac skips the row ("ready, not for
   this machine: needs linux") and the Halo claims it, resuming from the lane.

   ```sh
   git worktree add --detach /tmp/eq origin/eutropia/engine-swap
   git -C /tmp/eq am ~/.local/share/eutropia/lanes/engine-swap/es-f1-rpc-peer-child/control/queue-needs-linux.patch
   git -C /tmp/eq push origin HEAD:refs/heads/eutropia/engine-swap
   git worktree remove /tmp/eq
   eutropia unpark engine-swap es-f1-rpc-peer-child
   ```

2. **Unpark it while the Mac's loop is stopped, or only on the Halo.** This
   needs no queue edit, but the queue keeps saying `none` and the next Mac
   claim of this row repeats the loop. Cost: nothing now. Risk: every later
   resume of this row on the Mac wastes a worker session and a director
   session.

3. **Leave it parked until es-portable lands.** That doesn't help: es-portable
   removes the model-file reason, not PR_SET_PDEATHSIG or `/proc`, so the row
   stays Linux-only.

Recommendation: option 1. It is the decision engine-swap-3 already made,
placed where dispatch can read it.
