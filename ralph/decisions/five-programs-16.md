<!-- ledger -->

**five-programs-16 · 2026-09-24 · fp-54 (rail flip held open by the guest-stamp fork) · director** — this commit
- Needed: fp-54 halted (ctl/NEEDS_HUMAN.md). The counted flip landed at 24070aeb8, but the row stayed `[ ]` for one operator fork: re-mount guest rail writes, or keep refusing them.
- Chose: close fp-54 `[x] 24070aeb8` and move the fork to a new last-placed row, `HUMAN-fp54-guest-write-remount`. That row carries three options and a recommendation. The director does not pick one. No code behaviour changed; one stale comment in routes_rail.rs now points at the new row. Boundary gate 63, unchanged.
- Because: the row's gate edge is closed. The re-mount closes no edge and is either new auth machinery or a permanent end-user regression, and both are the operator's under the charter. Holding a done counted row open behind that fork only stalls the loop. This is the same shape as five-programs-15.

<!-- appendix -->

## five-programs-16 · 2026-09-24 — close fp-54 at its flip; the guest-write re-mount becomes an operator row

<details><summary>reasoning, evidence, package</summary>

Reproduced by the director at 70659b774:

- `cargo xtask boundary-gate` (toolbox, corpus-engine/) → `FAILED (63 violation(s))`. No `sovereign-daemon → commonwealth-rail` line appears in the output. sovereign-daemon/Cargo.toml:26 names only `commonwealth-rail-core`. The package said "stays at 62". That was stale by one: fp-55's `[bench] sovereign-eval → understanding-vocab` is the 63rd, and it is parked on HUMAN-fp58.
- commonwealth-rails/src/rail.rs:212-229: the append door warns and drops `on_behalf_of` before signing.
- sovereign-daemon/src/routes_rail.rs:414-431: a stamped append gets a named 503. Reads are untouched.
- The only prior ruling on a guest-stamp wire is archived ring-guest D1 (`_archive-ledger.md:253`, "signed `on_behalf_of` … in rail-core"). No live decision picks a re-mount.
- rails and the daemon both derive the node key from `commonwealth_transport::identity::load_or_generate_node_key` (rails lib.rs:140, daemon.rs:1293). That is why option (b), a daemon-signed attestation that rails verifies, needs no session state in cw-rails.

Why not decide (a) here: keeping the refusal as the design would permanently remove a working pre-flip capability (`guests = "write"`). That is end-user-observable behaviour the charter reserves. Why not (b) here: it is new auth machinery, meaning a new wire shape in rail-core, which is outside "strictly necessary".

Falsified if a live decision or FIVE_PROGRAMS §12 line already chooses the guest-stamp wire (then that decision governs and the HUMAN row is struck), or if the boundary gate still lists a daemon→commonwealth-rail edge (then fp-54 is not done).

</details>
