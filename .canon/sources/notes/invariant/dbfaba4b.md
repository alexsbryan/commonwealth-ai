# CROSS-BACKEND DISTRIBUTED INFERENCE IS A LOTTERY, NOT A CAPABILITY: ggml-RPC NEVER ASKS THE WORKER WHAT IT SUPPORTS, SO A SPLIT WORKS ONLY…

CROSS-BACKEND DISTRIBUTED INFERENCE IS A LOTTERY, NOT A CAPABILITY: ggml-RPC NEVER ASKS THE WORKER WHAT IT SUPPORTS, SO A SPLIT WORKS ONLY IF THE MODEL'S GRAPH HAPPENS TO CONTAIN NO OP THE WORKER REJECTS. WHEN IT DOES, THE WORKER abort()s.

Established 2026-08-03. Root cause of the DeepSeek-V4-Flash run that froze RuggedFox. Diagnosed on BeefyMac from its own stderr + backtrace, then VERIFIED against this repo's vendored llama.cpp (target/debug/build/llama-cpp-sys-4-*/out/llama.cpp) — not taken on trust.

WHAT THE WORKER DID. Both times, BeefyMac's RPC worker self-SIGABRTed 4.969s and 4.976s into prefill (7 ms apart — a fixed graph position, not a resource race):
  ERROR ggml_metal_op_encode_impl: error: unsupported op 'ADD'
  ggml-metal-ops.cpp:203: unsupported op
  frames: ggml_abort <- ggml_metal_op_encode <- ggml_metal_graph_compute
          <- rpc_server::graph_compute <- ggml_backend_rpc_start_server
NOT a memory failure. ~20-26 GB free, 60 GB Metal working set, 36.4 GB resident, zero allocation errors in a 6.7 MB stderr, no jetsam, no external kill. The memory hypothesis was refuted on four independent counts.

THE THREE-PART MECHANISM, each half verified in our tree:
  1. ggml-rpc.cpp:1822 `ggml_backend_rpc_device_supports_op` is a STUB:
       GGML_UNUSED(dev); GGML_UNUSED(op); //TODO: call the remote backend and cache the results
       return true;
     The RPC device tells the host's scheduler it supports EVERYTHING.
  2. There is no wire command to ask. `enum rpc_cmd` has 17 entries (ALLOC_BUFFER..GRAPH_RECOMPUTE) and NONE is a supports-op query. The negotiation is missing end to end, not just on the device side.
  3. The two backends genuinely disagree on ADD:
       Metal  (ggml-metal-device.m:1145): ggml_is_contiguous_rows(src0) && ggml_is_contiguous_rows(src1) && src0->type == GGML_TYPE_F32
       Vulkan (ggml-vulkan.cpp:17495):    (src0 F32|F16) && (src1 F32|F16) && (dst F32|F16)   -- NO contiguity requirement
     So an ADD with an F16 src0, or with non-row-contiguous sources, is legal on the Vulkan host and fatal on the Metal worker. The host plans against ITS OWN capability set and ships the node anyway.

THE POSITIVE CONTROL — QWEN 122B WORKS DISTRIBUTED ON THIS EXACT PAIR, AND THAT IS EVIDENCE *FOR* THIS DIAGNOSIS, NOT AGAINST IT. Operator raised it as a possible confound 2026-08-03; checked, and it closes rather than opens the question. `~/.sovereign/mesh-measurements.json` holds SIX valid `Qwen3.5-122B-A10B-UD-Q5_K_XL` records dated 07-29/07-30, placement "36 local + 12 @BeefyMac", 7.75-11.08 tok/s, plus two valid distributed 4B records. Same machines, same backends, same RPC path, comparable worker share (~25% of blocks). The llama.cpp version is NOT a confound either: `vendor/llama-cpp-4` was last touched 2026-07-22 (0.4.2 bump 07-16) and the vendored tree materialized 07-28, so the successful runs and today's abort ran the SAME llama.cpp; all four build dirs carry a byte-identical ggml-metal-device.m. THE ONLY VARIABLE IS THE MODEL ARCHITECTURE — exactly what the mechanism predicts. Qwen's residual ADDs are F32 + contiguous; `arch=deepseek4` emits one that is not. Stated precisely: the control proves Qwen never HIT a rejected op across eight valid runs, not that its graph contains none.
CONSEQUENCE FOR THE INSTRUMENTATION STEP: this is a controlled A/B, so step 1 is a DIFF, not a fishing trip — log op signatures under both models and the delta names the culprit.

SCOPE — DO NOT UNDER-READ THIS AS A DEEPSEEK BUG. ADD is one op verified to differ; every backend defines its own supports_op independently, so a mismatch is structural. Moving the block boundary CANNOT help: ADD appears in every block, so no split point keeps the offending op class off the Metal worker. Any Vulkan-host + Metal-worker split is exposed; the same applies to any heterogeneous pair (CUDA pods included) until (1)+(2) are fixed. It works today only where the draw is lucky.

THE FAILURE MODE IS abort(), NOT A FALLBACK. ggml's contract at ggml-metal-ops.cpp:201 is `if (!supports_op) GGML_ABORT`. There is no graceful path, so a scheduling mistake is always fatal to the worker process.

WHY DEEPSEEK-V4 IS STILL THE RIGHT DEMO MODEL. 122B measured FASTER local (19.34 tok/s, "48 local") than distributed (7.75-11.08) — at ~85 GB Q5 it fits RuggedFox's 124 GB alone, so splitting it costs roughly half the throughput. DeepSeek-V4-Flash at 155 GB is the first model that genuinely needs both boxes, which is the whole "fits neither box" premise. "Just use Qwen" is not a substitute demonstration.

AGREED PROGRESSION (operator, 2026-08-03): 1 -> 2, learn, then implement 3 with data in hand.
  (1) Name which ADD. The error prints only the op name. Log src[0]->type/src[1]->type/ne[]/contiguity at the rejection site, under BOTH models (see the control above). If it is only an F16 src0, a narrow fix may beat the full negotiation.
  (2) Refuse the plan instead of aborting. `resolve_placement` (rpc_distribution.rs) already returns a named `LoadPlacement` enum including `InsufficientCluster` — a backend-compatibility arm belongs there. Turns a machine-freezing abort into a legible refusal. Absence reported, never defaulted.
  (3) Implement the negotiation: new rpc_cmd + server handler calling the local backend's supports_op + host-side cache. Viable HERE because both ends are our own sovereign-cli-daemon build (never a stock rpc-server), but it forks the wire protocol — gate on the HELLO version. This is what actually unlocks heterogeneous splits.

CONTAINMENT, a second and independent defect. BeefyMac's RPC worker runs as a THREAD inside sovereign-cli-daemon (backtrace frame 14: serve_rpc_worker_if_configured), so one unsupported op took down mesh gossip, notes and code intel on that machine — twice on 2026-08-03. Our own code predicts this in containment.rs:162 ("shared-model ANCHOR with no distributed-primary containment... a worker leaving aborts the whole daemon"), but the warning is written for the HOST direction; the gap is symmetric and bit us in the WORKER direction. Running the RPC worker under the compute supervisor as a child process converts a daemon outage into a worker restart.

TIMING CORRECTION for anyone reading the host-side story: the worker died 4.97s into prefill, not the 18.3s/12.6s the host logs suggest. The host merely took a further ~13s and ~7.5s to notice the dead socket.
