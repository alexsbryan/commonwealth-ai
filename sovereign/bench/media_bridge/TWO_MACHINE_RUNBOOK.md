# The two-machine relay bar — runbook, written 2026-09-09

Closes the pre-registered NO-GO in `sovereign/deploy/mesh/WORK_PLANE.md`
§"Distance to the capstone". That bar has been COULD-NOT-JUDGE since it was
written, and it is the single measurement gating BOTH end-user claims on that
page — CI-on-your-metal across two nodes, and remote media playback.

## The bar, verbatim

> A single iroh stream between two members on different networks sustains
> **>= 25 Mbit/s for 10 minutes with no stall exceeding 2 s**, measured on the
> relayed path (the floor, not the hole-punched best case) and reported as a
> distribution over >= 3 runs rather than a peak.

## Read this before choosing where to run it

**Both machines are on `192.168.1.x`, so a LAN run does NOT meet the bar** —
"different networks" is unmet and the result is a relay floor, not a verdict.

**Put the MacBook on a phone hotspot instead.** Cellular gives genuine network
diversity and CGNAT, which defeats hole-punching on its own — so it produces a
real relayed path AND the bar's actual condition. That is the difference
between a reading and a verdict, for about five minutes of setup.

## Two preconditions are already cleared — do NOT redo them

- **`forget-member` is no longer needed.** It was gated on the twin Mac rows
  sharing endpoint key `86627fd5` (note `1ca75415`). `one_row_per_endpoint_key`
  landed 2026-09-09 and collapses them to a deterministic winner at the dial
  site; the live roster logs `collapsed=1`. The retarget storm that would have
  poisoned any reading is fixed.
- **Do NOT pass `--relay-only`.** It is a PREFERENCE, not a constraint:
  `RelayOnlySelector` returns an empty selection when no relay path is open,
  and an empty selection keeps the current path. Two of three prior runs
  measured the direct path while reporting `path=mixed`. See
  `relay-leg-CONTAMINATED.stdout`, named that way so nobody quotes it.

## Build — both machines, same rev

One of the genuine `--release` exceptions.

    cargo build --release -p commonwealth-transport --features iroh \
      --example media_bridge_bench

**Then invoke the built binary DIRECTLY.** Do not write the commands below as
`cargo run …` with the subcommand bare: cargo consumes it and you get
`unexpected argument 'gen'`. Going through cargo needs `--` between cargo's
arguments and the example's, and the placeholder that used to stand here hid
exactly that. The binary lands at:

    ./target/release/examples/media_bridge_bench

## Mac — the holder

Three terminals: `origin` and `serve` both stay running.

    ./target/release/examples/media_bridge_bench gen    --file /tmp/media.bin --size-mb 1024
    ./target/release/examples/media_bridge_bench origin --port 9810 --file /tmp/media.bin
    ./target/release/examples/media_bridge_bench serve  --origin 127.0.0.1:9810

`serve` prints `dial=…`. That string goes to RuggedFox.

## RuggedFox — the viewer

`bridge` stays running; `pull` is the one that measures.

    ./target/release/examples/media_bridge_bench bridge --iroh '<dial string from the Mac>'
    # prints  bridge=127.0.0.1:NNNNN
    ./target/release/examples/media_bridge_bench pull --addr 127.0.0.1:NNNNN \
      --path /media.bin --verify --label run1

Through cargo instead, if you prefer, the separator is not optional:

    cargo run --release -p commonwealth-transport --features iroh \
      --example media_bridge_bench -- gen --file /tmp/media.bin --size-mb 1024

## Forcing the relay — ONLY for the LAN fallback

Block the direct paths so iroh has nowhere but
`usw1-1.relay.n0.iroh.link`: UDP to `192.168.1.3`, to the Tailscale address
`100.104.36.28`, and to `fd7a:115c:a1e0::a3a:241c`. On a hotspot this should
be unnecessary — verify rather than assume.

## THE GATE — this is the whole discipline

Before AND after every single run, on RuggedFox:

    sovereign mesh transport

The `Alexs-MacBoo` row must read `path=relayed`. It reads `mixed` with
`direct=1` today.

**DISCARD any run whose row reads `direct` or `mixed`. Do not average it in.
Report the discard count beside the kept runs** — a bench that silently drops
half its samples is its own §18.3 problem.

That rule exists because the last attempt produced 7.8 Mbit/s, which sits
exactly inside the prior `WORK_PLANE.md` already writes down for relayed
throughput. A contaminated number that agrees with your prior is the one
nobody checks.

## What closes it

Three or more KEPT runs, 600 s each, >= 25 Mbit/s sustained, no stall > 2 s,
reported as a distribution rather than a peak. `--verify` carries the
byte-exactness claim continuously.

## Reading the result

- Gaps near **84 ms** are the origin's own 256 KiB pacing period, not the
  transport. Do not chase them.
- **Under 25 Mbit/s is a real verdict, not a failure to measure.**
  `WORK_PLANE.md` already says what it means: direct playback of a remote
  1080p title is not honest and the demo is LAN-only — a different product
  claim, and it should be made as one.

## Afterwards, and only afterwards

The blocker recorded in note `04b80710` was "there is no way to START a process
on a peer — no ssh, no mesh remote-exec surface." The work plane IS that
surface: `process:v1` runs argv on a consenting peer. Launching the Mac side as
a work unit would also take the cross-node CI reading that has been
could-not-judge since 5e (`2a3437399`).

Do the manual run FIRST and keep the measurement clean. Dogfood it as a second
pass, where a harness bug cannot be mistaken for a transport result.
