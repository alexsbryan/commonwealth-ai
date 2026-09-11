# media_bridge — raw readings, 2026-09-09

Data behind the reading in `sovereign/deploy/mesh/WORK_PLANE.md`
§"Distance to the capstone". Verdict: **could-not-judge on the pre-registered
bar; mechanism cleared.** The bar is two machines on different networks over
the relayed path; no peer was reachable, so no cross-machine number exists.

Harness: `commonwealth-transport/examples/media_bridge_bench.rs` (release,
`--features iroh[,iroh-relay-only]`). Origin, acceptor and bridge on one host,
n0 severed (`presets::Minimal`) except where noted.

- `ceiling-and-range.stdout` — Range/206 checksums vs `dd`+`sha256sum`
  (incl. a negative control that MUST fail), the 1 GiB ceiling runs with and
  without the splice, 4-viewer concurrency, and the two instrument-validation
  runs (rate knob, stall knob).
- `soak-25mbit-10min-n3.stdout` — 3 x 600 s at the bar's 25 Mbit/s. Gaps near
  84 ms are the origin's own 256 KiB pacing period, not the transport.
- `relay-leg-CONTAMINATED.stdout` — DISCARD the rates. `--relay-only` did not
  hold: `path=mixed`, and 2 of 3 runs measured the direct path. Kept because
  the failure is the finding — gate any relayed reading on the `path=` line.
- `reversed-leg-2026-09-11.stdout` — the reversed leg (RuggedFox serving, the
  Mac pulling over cellular). COULD-NOT-JUDGE by construction: the probe
  punched to RuggedFox's public endpoint (discarded, `mixed`), and RuggedFox's
  own uplink measures 21-26 Mbit/s — at the bar. Both directions on this pair
  are sender-capped; the floor needs a cloud sender.
