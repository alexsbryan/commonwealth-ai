# svrn mesh bench KEEPS ONLY 8 RUNS PER KEY — SO ESTABLISHING A BAND DESTROYS THE HISTORY YOU WANTED TO COMPARE AGAINST. MAX_RUNS_PER_KEY = 8…

`svrn mesh bench` KEEPS ONLY 8 RUNS PER KEY — SO ESTABLISHING A BAND DESTROYS THE HISTORY YOU WANTED TO COMPARE AGAINST. `MAX_RUNS_PER_KEY = 8` (sovereign-core/src/mesh_measurements.rs:177, eviction at :1344).

HIT LIVE 2026-08-06. Eight solo benches of Qwen3.6-35B-A3B-MTP-UD-Q6_K evicted every prior record under that key. The three historical rows (decode 44.111 / 44.27 valid, 29.999 invalid — all itl_p50 ~22.6ms) are GONE from the store; they survive only in a session transcript.

WHY THIS BITES THE MESH_N4 PROGRAM SPECIFICALLY: M4 Experiment A restated calls for >=5 interleaved runs per configuration. The 122B 2-node key already holds 7 rows (the 72.9-100.7ms spread that motivates the whole experiment). Five new runs would evict all but two of them — i.e. running the experiment deletes the baseline the experiment exists to re-examine. BACK UP ~/.sovereign/mesh-measurements.json BEFORE ANY BANDING RUN. A backup was taken at ~/.sovereign/mesh-measurements.json.bak-pre-c5-20260806.

SECOND HAZARD, SAME MEASUREMENT: RECORDS UNDER ONE KEY ARE NOT COMPARABLE ACROSS TIME. The 35B at IDENTICAL model + placement ("41 local") read 44.11 tok/s / itl_p50 22.7ms historically and 69.85 tok/s / itl_p50 0.1ms today — +58% decode, same key, nothing in the record saying why. Explanation: MTP draft acceptance. When speculation is OFF, itl_p50 == 1000/decode exactly (1000/44.11 = 22.67, matching 22.7). When it is ON, accepted drafts arrive in bursts so itl_p50 collapses to ~0.1ms and p95 (57ms) carries the real step cost.

CONSEQUENCE FOR §2/§3.1 OF MESH_N4_TOPOLOGY.md: itl_p50 IS NOT A SOUND CROSS-MODEL METRIC. It means "per-token latency" only for non-speculative models. The spec's 21.2ms-per-boundary constant and its whole N=3/N=4 extrapolation table are built on itl_p50. Use decode_tok_s for comparisons — it is (frames-1)/(last-first) with TTFT excluded by construction (mesh_bench.rs:255) and is sound for both shapes.

NEGATIVE CONTROL ESTABLISHED (the useful half of C5, run today): 5 solo benches with ping sampled concurrently.
  decode_tok_s 69.64-70.18, spread 0.77%
  concurrent ping avg 21.5-32.9 ms, spread 39%
  Pearson r(decode, ping) = +0.21 at n=5 — noise.
SOLO THROUGHPUT IS INSENSITIVE TO LINK CONDITIONS: ~50x difference in sensitivity. This validates the instrument (§18.4) — a solo bench is repeatable to under 1% and does not track the link, so ANY link-dependence observed in a 2-node run is attributable to the tensor boundary rather than to measurement noise. That is the control the 2-node half of M4-A needs in order to mean anything.
