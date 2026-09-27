<!-- ledger -->

**phase-b-25 · 2026-09-26 · pb-svrn-dials-serve → the latency bars bind the release profile; the ~5 ms is attributed before the switch; the embedding bar is re-measured · operator** — this commit
- Needed: the row measured the header's pre-registered bars before the switch (e50c62bc4). First-token p50 over loopback was ×1.157, ×1.162 and ×1.228 on the 0.8B model against ≤1.10, a fixed ~5 ms per request (×1.067 on the 4B). Embedding throughput at batch 32 was ×0.900, ×0.915 and ×0.895 against ≥0.90. The header named no build profile. The charter leaves a bar a row cannot meet to the operator, and four director sessions declined it.
- Chose (operator, from the seat's escalation):
  - Profile: the bars bind the `release` profile, as a distribution builds it. Debug readings (what this host deploys) are reported beside them and gate nothing. The thresholds are unchanged.
  - Sequencing: attribute the ~5 ms before the switch. Debug-level spans at serve's request boundary and in the loopback client, in both profiles, then both bars re-measured under release.
  - Embedding bar: could-not-judge at debug. Re-measure at n ≥ 15 per run. If the JSON float payload dominates, the terminal arm asks for OpenAI's `encoding_format: "base64"` rather than a new wire.
  - Supervisor: a package marked `operator-only:` gets no resolution session (2880e5de0).
- Because:
  - Principle 7: the instrument measures the claim. The bar is about the cost of the process boundary, and the debug reading gates on the dev profile's unoptimized JSON/HTTP layer, which no user of a shipped build pays. Every run so far is debug, so choosing release now is still pre-registration, not a re-tune.
  - Principles 1 and 2: the 5 ms is invisible today. Understand it, then fix or accept it.
  - Principle 5: three runs straddle the embedding bar within their own spread, so neither verdict is earned.
  - Principle 11: base64 is the standard's own option. Keeping embeddings in-process was refused (principle 12: the daemon would keep the engine while its uses shrink).
  - Boundary gate: 50 at 02d70a0f2. The 50th is the harness's runtime root escape (serve_latency_bars.rs:34, routed in 78824348d). No code is in this commit.

<!-- appendix -->

## phase-b-25 · 2026-09-26 — the latency bars bind release; attribute the ~5 ms, then re-measure

<details><summary>reasoning, evidence, package</summary>

The package was ralph/next/phase-b/ctl/NEEDS_HUMAN.md at 22:53Z, sections (a) to (h). Worker logs: target/ralph/phase-b/latency-bars-{1,2,3,4b}.log. The director's own rerun (latency-bars-director.log) ran on a contended host (cw-rails ~92% CPU, load 3.2) and is not a measurement.

The deployed daemon was verified as debug at the time: `readlink -f /proc/<pid on :9741>/exe` gave target/debug/sovereign-daemon.

What would falsify this:
- Release loopback still misses first-token by more than 10% on the 0.8B. Then the cost is architectural. It goes back to the operator with the span breakdown, marked `operator-only: a pre-registered bar a row cannot meet`.
- The spans cannot place the 5 ms (for example, it sits in the kernel's loopback path). Then attribution is incomplete: report what the spans cover and stop.
- base64 does not move the embedding ratio. Then the payload was not the cost: report the breakdown and let the release numbers decide.

</details>
