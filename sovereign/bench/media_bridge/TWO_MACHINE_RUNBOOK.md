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
diversity (different public IPs, CGNAT on the phone side). It does NOT, on its
own, defeat hole-punching: measured 2026-09-11, a Mac on an iPhone hotspot
punched straight to RuggedFox's public UDP endpoint on the first try
(`reversed-leg-2026-09-11.stdout`). CGNAT stops INBOUND punches to the
phone side, so the relayed path arises naturally only when the phone side is
the one being dialled; when it dials a home NAT that admits punches, the path
reads `mixed`. Plan on FORCING the relay (below) and recording it as forced.

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

**Gate on the BENCH's own `path=` line, not on `sovereign mesh transport`.**

The harness opens its OWN iroh endpoint and prints, from its own `remote_info`
(`media_bridge_bench.rs:542`):

    path={class} direct=[…] relay=[…]

`sovereign mesh transport` reports the DAEMON's paths to mesh peers — a
different endpoint on a different connection. Reading the daemon's row and
attributing it to the bench is the same contamination this bar exists to
avoid, one layer along. (An earlier revision of this file said to use
`mesh transport`. That was wrong.)

`class` must read `relayed`. `mesh transport` is still worth a glance for
context — it showed `Alexs-MacBoo` at `mixed`/`direct=1` while the Mac sat on
the LAN — but it is not the gate.

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

---

# LIVE RUN, 2026-09-10 — the reversed leg

## Where this stands

The first honest relayed reading was taken and it is **8.7 Mbit/s against a
25 Mbit/s bar** (`relay-leg-TWO-MACHINE-probe.stdout`). The gate passed —
`path=relayed direct=[]`, different public IPs — so it is a real relayed
number, unlike `relay-leg-CONTAMINATED.stdout`.

**It does not close the bar, because the SENDER was a laptop tethered to a
phone.** Cellular uplink is typically 5-20 Mbit/s, so 8.7 may be the phone's
upstream rather than the n0 relay.

This section reverses the roles so the sender is NOT the cellular leg:
RuggedFox serves from home broadband, the Mac pulls over cellular (whose
DOWNlink is far faster than its uplink). If the reversed number also lands
near 8.7, the relay is the ceiling and the bar fires cleanly. If it comes back
materially higher, the first reading was the phone and the relay question is
still open.

## RuggedFox — already running

    ./target/release/examples/media_bridge_bench gen    --file /tmp/media.bin --size-mb 1024
    ./target/release/examples/media_bridge_bench origin --port 9810 --file /tmp/media.bin
    ./target/release/examples/media_bridge_bench serve  --origin 127.0.0.1:9810

## Mac — run these two, stay on the hotspot

Copy the dial string whole, quotes included.

    ./target/release/examples/media_bridge_bench bridge --iroh '7b8a0d81ca94eb77df3f044ace6c11d8bf4d37bd75bf027d4923545cb162e33a@https://usw1-1.relay.n0.iroh.link./,69.181.167.209:47800,100.115.12.21:47800,192.168.1.13:47800'

It prints `bridge=127.0.0.1:NNNNN`. Put that port in the next line:

    ./target/release/examples/media_bridge_bench pull --addr 127.0.0.1:NNNNN --path /media.bin --range 0-33554431 --label reversed-probe

## WATCH FOR THIS — it would silently invalidate the run

`100.115.12.21` is RuggedFox's Tailscale address and `192.168.1.13` is its LAN
one. **If Tailscale is up on the Mac it may reach RuggedFox DIRECTLY over that
address**, and the bench will report `path=direct` or `path=mixed` with a
non-empty `direct=[…]`.

That is a DISCARD, not a result. Drop Tailscale on the Mac and re-run. The
bench's own `path=` line is the gate — the empty `direct=[]` is what makes a
reading count.

## If the reversed probe reads relayed and looks sane

Then take the actual bar: three or more runs, 600 s each, full file rather than
a 32 MiB range, reported as a distribution.

    ./target/release/examples/media_bridge_bench pull --addr 127.0.0.1:NNNNN --path /media.bin --verify --duration-secs 600 --label soak1

`--duration-secs` is not optional: the BAR verdict line prints only inside
that branch (`media_bridge_bench.rs`, the `if let Some(d) = duration_secs`).
And `fetch_until` checks the deadline at the top of each WHOLE-FILE pull, so
on a 1 GiB origin at ~22 Mbit/s a "600 s" run lasts ~760 s and `secs` reads
that; the verdict still judges `>= 600`. Budget ~13 min per soak.

---

# LIVE RUN, 2026-09-11 — the reversed leg, taken

`reversed-leg-2026-09-11.stdout`. **Could-not-judge, by construction. Kept 0,
discarded 1.** Two findings, both about the pair rather than the transport:

1. The probe read `path=mixed direct=[69.181.167.209:43327]` — the Mac on the
   hotspot punched to RuggedFox's PUBLIC endpoint, with Tailscale verifiably
   stopped. The premise sentence above was rewritten on this evidence.
2. RuggedFox's uplink to a neutral endpoint is 21.1 / 25.7 Mbit/s — AT the
   bar. The 22.6 Mbit/s probe was the home ISP, not the path; a soak on any
   path, forced relay included, would have filed an ISP cap as a verdict.

Both directions on this pair are sender-capped (phone uplink one way, home
uplink the other). **The next leg needs a sender with real uplink:** a cloud
VM on a symmetric >=100 Mbit link serving to the Mac on the hotspot, relay
FORCED on the Mac (`pfctl` anchor blocking outbound UDP to the VM's address —
a public VM punches too), recorded as forced; the Mac's hotspot downlink is
169 Mbit/s, not a cap. Check the VM's IPv6 before writing a v4-only block.
