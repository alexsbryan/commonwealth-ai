# Engram experiment — complete lab record

Recovered from harness transcript `/home/alexbryan/.claude/projects/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db.jsonl`
(session 6fe59b24, 2026-08-26 15:23:34Z -> 15:51:47Z, branch main).

The working directory was tmpfs and was lost to the 08:58 reboot; every
command below and its captured output are verbatim from the transcript.


## Call 01

```bash
echo "=== MEM ==="; free -g; echo; echo "=== CPU ==="; nproc; lscpu | grep -iE "model name|^Vendor" ; echo; echo "=== GPU / VRAM ==="; ls /sys/class/drm/ 2>/dev/null | head; cat /sys/class/drm/card*/device/mem_info_vram_total 2>/dev/null; echo; echo "=== DISK (models) ==="; df -h /home/alexbryan/dev/commonwealth-ai 2>/dev/null; echo; echo "=== models dir ==="; du -sh /home/alexbryan/dev/commonwealth-ai/sovereign/models 2>/dev/null; ls -la /home/alexbryan/dev/commonwealth-ai/sovereign/models 2>/dev/null | head -40
```

<details><summary>output</summary>

```
=== MEM ===
               total        used        free      shared  buff/cache   available
Mem:             125          38          79           0           8          86
Swap:              7           3           4

=== CPU ===
32
Vendor ID:                               AuthenticAMD
Model name:                              AMD RYZEN AI MAX+ 395 w/ Radeon 8060S

=== GPU / VRAM ===
card1
card1-DP-1
card1-DP-2
card1-DP-3
card1-DP-4
card1-DP-5
card1-DP-6
card1-DP-7
card1-DP-8
card1-HDMI-A-1
536870912

=== DISK (models) ===
Filesystem      Size  Used Avail Use% Mounted on
/dev/nvme0n1p3  1.9T  1.3T  580G  69% /home

=== models dir ===
782G	/home/alexbryan/dev/commonwealth-ai/sovereign/models
total 380886900
drwxr-xr-x. 1 alexbryan alexbryan        1998 Aug 14 09:05 .
drwxr-xr-x. 1 alexbryan alexbryan         842 Aug 24 15:20 ..
drwxr-xr-x. 1 alexbryan alexbryan          22 Apr 26 10:18 .cache
-rw-r--r--. 1 alexbryan alexbryan  9527502912 Apr 26 12:18 Darwin-9B-Opus.Q8_0.gguf
drwxr-xr-x. 1 alexbryan alexbryan          20 Aug  1 20:32 DeepSeek-V4-Flash-0731-GGUF
-rw-r--r--. 1 alexbryan alexbryan 36914691616 Apr 26 12:12 FINAL-Bench_Darwin-35B-A3B-Opus-Q8_0.gguf
-rw-r--r--. 1 alexbryan alexbryan 21768895712 May 11 07:00 FINAL-Bench_Darwin-36B-Opus-Q4_K_L.gguf
-rw-r--r--. 1 alexbryan alexbryan 30053416160 May  4 20:30 FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan  4977169088 Apr 27 11:31 gemma-4-E4B-it-Q4_K_M.gguf
-rw-r--r--. 1 alexbryan alexbryan 23041357248 Apr 27 11:15 google_gemma-4-26B-A4B-it-Q6_K_L.gguf
drwxr-xr-x. 1 alexbryan alexbryan           0 Apr 26 13:16 Jackrong
-rw-r--r--. 1 alexbryan alexbryan   495710816 May 11 14:30 jina-reranker-v3-Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan 10879832736 Jul 21 15:25 Mellum2-12B-A2.5B-Instruct-Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan 10879834176 Jul 21 14:25 Mellum2-12B-A2.5B-Thinking-Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan 33508168288 Apr 27 09:21 Nemotron-Cascade-2-30B-A3B.Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan   771092736 May 12 18:03 Qwen3.5-0.8B-UD-Q6_K_XL.gguf
drwxr-xr-x. 1 alexbryan alexbryan          30 Jul 15 21:52 Qwen3.5-122B-A10B-MTP-GGUF
drwxr-xr-x. 1 alexbryan alexbryan         352 Jul 19 09:53 Qwen3.5-122B-A10B-UD-Q5_K_XL
-rw-r--r--. 1 alexbryan alexbryan  1556386752 Apr 29 21:15 Qwen3.5-2B.Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan  3464051520 May  1 01:26 Qwen3.5-4B.Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan  4261908800 May 18 16:12 Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf
-rw-r--r--. 1 alexbryan alexbryan  8987439456 May 18 16:30 Qwen3.5-9B-UD-MTP-Q6_K_XL.gguf
-rw-r--r--. 1 alexbryan alexbryan   514682208 May 12 17:52 Qwen3.6-35B-A3B-DFlash-Q8_0.gguf
-rw-r--r--. 1 alexbryan alexbryan 30011242784 May 16 19:37 Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan 29207678368 Jun 15 12:49 Qwen3.6-35B-A3B-Opus-abl-ggml-model-Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan 22360456160 May 12 00:32 Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf
-rw-r--r--. 1 alexbryan alexbryan 30649317504 Aug  5 11:06 Qwen3.6-35B-A3B-Uncensored-HauhauCS-Aggressive-Q6_K_P.gguf
-rw-r--r--. 1 alexbryan alexbryan 25924152384 Aug 14 09:05 Qwen3.8-27B-UD-Q6_K_XL.gguf
-rw-r--r--. 1 alexbryan alexbryan   639150592 Apr 23 23:13 Qwen3-Embedding-0.6B-Q8_0.gguf
-rw-r--r--. 1 alexbryan alexbryan   639153184 Jul 16 15:32 qwen3-reranker-0.6b-q8_0.gguf
-rw-r--r--. 1 alexbryan alexbryan 22082525088 Apr 26 10:27 Qwopus3.5-27B-v3.5-Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan  3563024608 Jun  3 14:30 Qwopus3.5-4B-v3-MTP-Q6_K.gguf
-rw-r--r--. 1 alexbryan alexbryan 13038846240 Apr 26 13:21 Qwopus-GLM-18B-Healed-Q6_K.gguf
drwxr-xr-x. 1 alexbryan alexbryan         498 Aug  1 20:51 split-warm-test
-rw-r--r--. 1 alexbryan alexbryan  1537269856 Jul 31 08:41 sweep-next-edit-1.5b.q8_0.v2.gguf
-rw-r--r--. 1 alexbryan alexbryan  8773161888 Jul 31 09:12 zeta-2.1.Q8_0.gguf
```

</details>

## Call 02

```bash
cd /home/alexbryan/.claude/projects/-home-alexbryan-dev-commonwealth-ai/memory/ 2>/dev/null && for f in project_rpc_distributed_hang_root_cause.md project_qwen122b_iroh_transport_characterization.md project_moe_byte_aware_split.md project_heterogeneous_inference_ds4_parked.md project_qwen3.5_122b_throughput.md project_distributed_worker_flap_fix.md user_setup_ruggedfox.md; do echo "########## $f ##########"; cat "$f" 2>/dev/null || echo "(missing)"; echo; done
```

<details><summary>output</summary>

```
<persisted-output>
Output too large (59KB). Full output saved to: /home/alexbryan/.claude/projects/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/tool-results/bv41t14ro.txt

Preview (first 2KB):
########## project_rpc_distributed_hang_root_cause.md ##########
---
name: project_rpc_distributed_hang_root_cause
description: "Distributed-inference RPC \"Metal worker\" hang is actually a host-side weight-upload send() deadlock, not Metal"
metadata: 
  node_type: memory
  type: project
  originSessionId: ed6e8e1a-6a76-4373-9e88-755eb8a14f93
---

2026-06-05 (Strix+Mac mesh). The "Vulkan-host + Metal-worker hangs at model-load" symptom (docs/RPC_METAL_WORKER_INVESTIGATION.md) is **NOT Metal-specific**. Root-caused by elimination + a host gdb backtrace.

**Root cause:** the RPC *host* blocks in `send()` inside `ggml_backend_rpc_buffer_set_tensor` during `llama_model_loader::load_all_data` when streaming weights to a remote worker. Wedges after ~800 MB uploaded (35B). Backend-agnostic — a **CPU worker reproduces the identical wedge**, so Metal was never the cause. Matches upstream llama.cpp **#19745** (RPC large-model upload hang on UMA hosts).

**Proven working:** cross-machine RPC transport is fine — a 0.8B model loads split 50/50 across Strix-Vulkan-host + Mac-CPU-worker over a **direct LAN** Tailscale path and runs inference end-to-end. So "is RPC working between the machines?" = yes; only the large weight-upload deadlocks.

**Why localhost passed but cross-machine hangs:** loopback `send()` never blocks; a real network's TCP window fills and the host wedges. My llama-bench `-ts 0.2,0.8` used COMMAS — llama-bench wants SLASHES (`0.2/0.8`) — so it mis-parsed and dumped 100% on the worker (test artifact, not a product bug; the daemon's `with_tensor_split` float array is correct).

**Two secondary worker-resilience bugs (explain why Mac :50052 went dead and stayed dead):**
- Bug A (ggml): rpc-server accept loop `return`s on a single `accept()` failure — `ggml-rpc.cpp:1744` (vendored llama-cpp-sys-4 0.2.57, b9180). No retry/continue.
...
</persisted-output>
Shell cwd was reset to /home/alexbryan/dev/commonwealth-ai
```

</details>

## Call 03

```bash
sed -n '1,400p' /home/alexbryan/.claude/projects/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/tool-results/bv41t14ro.txt
```

<details><summary>output</summary>

```
<persisted-output>
Output too large (59KB). Full output saved to: /home/alexbryan/.claude/projects/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/tool-results/bk8u83a9d.txt

Preview (first 2KB):
########## project_rpc_distributed_hang_root_cause.md ##########
---
name: project_rpc_distributed_hang_root_cause
description: "Distributed-inference RPC \"Metal worker\" hang is actually a host-side weight-upload send() deadlock, not Metal"
metadata: 
  node_type: memory
  type: project
  originSessionId: ed6e8e1a-6a76-4373-9e88-755eb8a14f93
---

2026-06-05 (Strix+Mac mesh). The "Vulkan-host + Metal-worker hangs at model-load" symptom (docs/RPC_METAL_WORKER_INVESTIGATION.md) is **NOT Metal-specific**. Root-caused by elimination + a host gdb backtrace.

**Root cause:** the RPC *host* blocks in `send()` inside `ggml_backend_rpc_buffer_set_tensor` during `llama_model_loader::load_all_data` when streaming weights to a remote worker. Wedges after ~800 MB uploaded (35B). Backend-agnostic — a **CPU worker reproduces the identical wedge**, so Metal was never the cause. Matches upstream llama.cpp **#19745** (RPC large-model upload hang on UMA hosts).

**Proven working:** cross-machine RPC transport is fine — a 0.8B model loads split 50/50 across Strix-Vulkan-host + Mac-CPU-worker over a **direct LAN** Tailscale path and runs inference end-to-end. So "is RPC working between the machines?" = yes; only the large weight-upload deadlocks.

**Why localhost passed but cross-machine hangs:** loopback `send()` never blocks; a real network's TCP window fills and the host wedges. My llama-bench `-ts 0.2,0.8` used COMMAS — llama-bench wants SLASHES (`0.2/0.8`) — so it mis-parsed and dumped 100% on the worker (test artifact, not a product bug; the daemon's `with_tensor_split` float array is correct).

**Two secondary worker-resilience bugs (explain why Mac :50052 went dead and stayed dead):**
- Bug A (ggml): rpc-server accept loop `return`s on a single `accept()` failure — `ggml-rpc.cpp:1744` (vendored llama-cpp-sys-4 0.2.57, b9180). No retry/continue.
...
</persisted-output>
```

</details>

## Call 04

```bash
cd /home/alexbryan/dev/commonwealth-ai/sovereign/models; echo "=== DeepSeek-V4-Flash-0731-GGUF ==="; ls -la DeepSeek-V4-Flash-0731-GGUF/ 2>/dev/null; find DeepSeek-V4-Flash-0731-GGUF -maxdepth 3 | head -30; echo; echo "=== split-warm-test ==="; ls -la split-warm-test/ | head -20; echo; echo "=== 122B dirs ==="; ls -la Qwen3.5-122B-A10B-UD-Q5_K_XL/ | head; du -sh Qwen3.5-122B-A10B-UD-Q5_K_XL/ Qwen3.5-122B-A10B-MTP-GGUF/ 2>/dev/null
```

<details><summary>output</summary>

```
=== DeepSeek-V4-Flash-0731-GGUF ===
total 0
drwxr-xr-x. 1 alexbryan alexbryan   20 Aug  1 20:32 .
drwxr-xr-x. 1 alexbryan alexbryan 1998 Aug 14 09:05 ..
drwxr-xr-x. 1 alexbryan alexbryan  530 Aug  1 20:32 UD-Q4_K_XL
DeepSeek-V4-Flash-0731-GGUF
DeepSeek-V4-Flash-0731-GGUF/UD-Q4_K_XL
DeepSeek-V4-Flash-0731-GGUF/UD-Q4_K_XL/DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00001-of-00005.gguf
DeepSeek-V4-Flash-0731-GGUF/UD-Q4_K_XL/DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00005-of-00005.gguf
DeepSeek-V4-Flash-0731-GGUF/UD-Q4_K_XL/DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00003-of-00005.gguf
DeepSeek-V4-Flash-0731-GGUF/UD-Q4_K_XL/DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00002-of-00005.gguf
DeepSeek-V4-Flash-0731-GGUF/UD-Q4_K_XL/DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00004-of-00005.gguf

=== split-warm-test ===
total 4884112
drwxr-xr-x. 1 alexbryan alexbryan        498 Aug  1 20:51 .
drwxr-xr-x. 1 alexbryan alexbryan       1998 Aug 14 09:05 ..
-rw-r--r--. 1 alexbryan alexbryan   10938752 Jul 31 14:52 Qwen3.5-4B-SplitTest-Q6_K-00001-of-00003.gguf
-rw-r--r--. 1 alexbryan alexbryan 2071173856 Jul 31 14:52 Qwen3.5-4B-SplitTest-Q6_K-00002-of-00003.gguf
-rw-r--r--. 1 alexbryan alexbryan 1381939232 Jul 31 14:52 Qwen3.5-4B-SplitTest-Q6_K-00003-of-00003.gguf
-rw-r--r--. 1 alexbryan alexbryan    1497600 Aug  1 20:51 Sweep-SplitTest-q8-00001-of-00003.gguf
-rw-r--r--. 1 alexbryan alexbryan  839428512 Aug  1 20:51 Sweep-SplitTest-q8-00002-of-00003.gguf
-rw-r--r--. 1 alexbryan alexbryan  696344064 Aug  1 20:51 Sweep-SplitTest-q8-00003-of-00003.gguf

=== 122B dirs ===
total 179547200
drwxr-xr-x. 1 alexbryan alexbryan         352 Jul 19 09:53 .
drwxr-xr-x. 1 alexbryan alexbryan        1998 Aug 14 09:05 ..
-rw-r--r--. 1 alexbryan alexbryan    10943552 May 13 13:38 Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003.gguf
-rw-r--r--. 1 alexbryan alexbryan 49730924512 May 13 16:24 Qwen3.5-122B-A10B-UD-Q5_K_XL-00002-of-00003.gguf
-rw-r--r--. 1 alexbryan alexbryan 42186295776 May 13 15:18 Qwen3.5-122B-A10B-UD-Q5_K_XL-00003-of-00003.gguf
-rw-r--r--. 1 alexbryan alexbryan 91928163616 Jul 19 09:54 Qwen3.5-122B-merged.Q5_K_XL.gguf
172G	Qwen3.5-122B-A10B-UD-Q5_K_XL/
83G	Qwen3.5-122B-A10B-MTP-GGUF/
```

</details>

## Call 05

```bash
F=/home/alexbryan/.claude/projects/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/tool-results/bk8u83a9d.txt; awk '/^########## project_qwen122b_iroh/,/^########## project_heterogeneous/' "$F" | head -120
```

<details><summary>output</summary>

```
########## project_qwen122b_iroh_transport_characterization.md ##########
---
name: qwen122b-iroh-transport-characterization
description: Measured iroh tunnel envelope for distributed inference + the TCP_NODELAY tunnel bug fix (2026-07-18)
metadata: 
  node_type: memory
  type: project
  originSessionId: 188016f2-aebc-4331-8e7b-57a10c97a2b0
---

Path characterization for the Qwen-122B distributed arc (see [[rpc-distributed-hang-root-cause]], docs/QWEN122B_DISTRIBUTED_HANDOFF.md), measured 2026-07-18 with `commonwealth-transport/examples/tunnel_bench.rs` (build: `--features iroh,iroh-relay-only`).

**Real bug found+fixed:** iroh tunnel pump legs (`HttpBridge` accepted TCP + `IrohAcceptor` forward connect) lacked `TCP_NODELAY` — Nagle × delayed-ACK added ~40ms/direction (~82ms on a 16KB round-trip, measured on loopback). Fixed in `commonwealth-transport/src/iroh.rs`. Invariant: every TCP leg of a byte tunnel sets nodelay. Production gossip/control over iroh silently paid this before.

**Envelope (Strix↔BeefyMac; the "LAN" is Wi-Fi: ~6ms RTT, ~7MB/s):**
- raw TCP: 16KB rt p50 10.9ms → 91.8 tok/s network ceiling
- iroh direct: 13.3ms → 75.2 tok/s (tunnel tax ~2.4ms/rt; bulk unchanged)
- iroh relay floor (usw1, relay-only PathSelector BOTH sides): 182ms → **5.5 tok/s ceiling**, 3.7MB/s bulk (40GB warm ≈ 3h; n0 public relay may throttle — self-hosted relay is the product answer there)
- worker-holds-output-head (600KB logits/token) caps 12.7 tok/s even raw-TCP-LAN → **placement must keep the output head on the host**
- cross-network direct model: ceiling ≈ 1000/(13.3 + WAN_RTT_ms); viable for 122B decode (~15-19 t/s compute) when hole-punch lands and RTT ≲ 30ms; relay fallback = single-digit t/s

**Two-machine relay floor VALIDATED (2026-07-18, both sides `--relay-only`, `path=relayed direct=[]` confirmed):** Strix dial ↔ BeefyMac serve via usw1: 64B rt p50 63.2ms; 16KB rt p50 141.2ms → **7.1 tok/s** hidden-return ceiling (bench now computes it); logits-return 228.9ms → 4.4 tok/s; bulk 4.5 up / 3.4 down MB/s (40GB warm ≈ 2.5–3.3h). Solo-hairpin (5.5 tok/s) was conservative by ~25% — validated as a same-box lower bound. Composed with the 19.2 t/s A0 compute baseline: 52.1ms + 141.2ms → **~5.2 t/s end-to-end on relay**; p90 tail 203ms → jittery. CAVEAT: both boxes share one WAN IP (67.180.200.149), so this relay path rides the same home uplink twice — a true two-WAN pair will differ with each side's RTT to the relay. Log: session scratchpad `relay-dial-crossmachine.log`.

**RELAY-PIN NEGATIVE RESULT (2026-07-19, definitive — both pins statically+runtime verified on both boxes):** `RelayOnlySelector` CANNOT hold a live daemon pair's data path on a LAN. Sequence of findings: (1) the pin was a RACE — seeding direct addrs at dial lets a fast-validating direct path become current before the relay, and the selector's empty-selection-keeps-current fallback rides it silently (hairpin measured 0.9ms/16KB while claiming pinned); FIXED by relay-only addr-stripping at dial (`relay_pin_active()` in transport + bench `--relay-only` target filter, commit d3df43be) — hairpin then holds deterministically (5.8 tok/s, June's 5.5 reproduced). (2) Even fixed + both daemons verified pinned: the LOAD phase rides the relay (50-54s reload vs 29s direct, twice) but DECODE migrates to direct minutes later (~40 tok/s ≈ direct; physically impossible via usw1) — hole-punching validates a direct path mid-connection and transmission migrates despite both selectors; "relay-only" degrades to "relay-first" over a connection's lifetime. Short fresh-endpoint bench runs hold ONLY because they end before migration. (3) `remote_info`/`peer_path_on` reported "relayed, 0 active directs" while data flew direct — path introspection is NOT a pin verifier. CONCLUSION: relay-floor numbers stand at transport level (5.8-7.1 ceiling → ~5-6.5 composed); daemon-grade relay validation requires a genuinely cross-network peer (no hole-punch possible = the production scenario). Do NOT burn more time hardening the pin on a LAN. ALSO FOUND: third uncatchable ggml-rpc abort face `:337` ("Remote RPC server crashed or returned malformed response" on session death mid-exchange, triggered by a transient ~60s ALPN-refusal window on the Mac's endpoint ~5min post-restart — suspected iroh-watchdog endpoint rebuild briefly holding the key with a non-serving endpoint; error 120 "peer doesn't support any known protocol"; healed itself). Daemon crash handler captured the backtrace (P0 bulletproofing worked).

**Relay-forcing recipe:** nft is impossible from the toolbox (container root lacks host-netns CAP_NET_ADMIN even under sudo). Use the `iroh-relay-only` feature (custom `PathSelector` via iroh's `unstable-custom-transports`) — deterministic, no root, but BOTH peers must run it or the far side answers over the direct path.

Architecture amendment for the fix (tasks 5/6 of the session plan): the RPC layer's currency is bare `host:port` strings — identity is dropped at the discovery boundary, and `orchestrate_warm` reverse-parses worker IPs from endpoint strings (breaks under bridge-local endpoints → parses to 127.0.0.1 = self). Warm plane must resolve peers via `transport.endpoints(contact, ModelTransfer)` with a daemon-owned endpoint→NodeId map; ggml RPC rides a new `cwth/rpc/0` ALPN with per-peer `HttpBridge`s (the pump is already byte-generic despite the name).

########## project_moe_byte_aware_split.md ##########
---
name: project_moe_byte_aware_split
description: "Byte-mass-aware tensor split for non-uniform MoE models; verified, UNCOMMITTED; node/hop advisor caveat."
metadata: 
  node_type: memory
  type: project
  originSessionId: e2c2e2a3-5371-4c00-a298-03c37f564ea7
  modified: 2026-07-20T19:39:57.400Z
---

2026-07-20. The "intelligently split GGUF MoEs" feature for the Run-a-Bigger-Model push. `plan_shards_weighted(n_layer, weights, block_bytes, head_bytes)` in `rpc_warm_cache.rs` apportions each device a CONTIGUOUS block range whose *bytes* (not block count) are ∝ its VRAM, folding the output head onto the last (host) device. `plan_shards` now delegates (empty `block_bytes` = count split). Helpers: `closest_boundary`, `build_shards_from_counts`, `is_routed_expert_tensor` (`_exps.weight` = cold routed experts). Wired into BOTH `rpc_distribution::plan_distribution` (live load overlays real per-block mass via `tensor_sizes` on cache-miss; glassbox per-shard byte-balance log) and `mesh plan` (preview) — ONE function so preview == load.

Why: MoE mass is deeply non-uniform. On merged 122B-A10B: routed experts = 93% of bytes but COLD (top-k only); per-block spread 1.23×; a count split hands a small node a heavy run and OOMs it.

**VERIFIED 2026-07-20 (UNCOMMITTED, 4 files):** full-workspace lint 0-fail (7 crates incl. daemon); 7/7 unit tests (6 `weighted_*` + `routed_expert_classifier`); `mesh plan` dry-run on merged 122B (`--devices 48,124 --host 1`) shows worker 24.4GB/host 60.4GB (mass-∝-VRAM), MoE hot/cold report, node/hop advisor. Files: rpc_warm_cache.rs, rpc_distribution.rs, mesh_cmd.rs, SYSTEM_OVERVIEW.md.

**Node/hop advisor throughput claim — SOFTENED to be accurate (2026-07-20).** The first cut said "fewer nodes = fewer hops = higher tok/s" and told the operator to "drop a node, speed decode." That's unsound for the flagship case: the MEASURED 122B ([[project_distributed_worker_flap_fix]]) was distribution-POSITIVE — 36/12 split hit 17.3-17.9 tok/s vs 14.8 solo (~+20%) because the Strix Halo host is memory-bandwidth-bound and offloading 25% of layers saves more host weight-read time than the ~11ms/token hop costs. The advisor models only hop count, not host bandwidth. Fixed in both `mesh_cmd.rs` and SYSTEM_OVERVIEW: now framed as a tradeoff — fewer nodes cut per-token hop LATENCY, but on a bandwidth-bound host keeping layers offloaded can raise THROUGHPUT, so "Net tok/s depends on the host … Measure both." It no longer claims fewer nodes is always faster. (A future bandwidth-aware cost model = the real fix; P5 placement-cost-vector.) Related: [[project_rpc_distributed_hang_root_cause]], [[project_bigmodel_local_load_freeze]].

########## project_heterogeneous_inference_ds4_parked.md ##########
```

</details>

## Call 06

```bash
F=/home/alexbryan/.claude/projects/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/tool-results/bk8u83a9d.txt; awk '/^########## project_heterogeneous/,/^########## project_distributed_worker_flap/' "$F" | head -150
```

<details><summary>output</summary>

```
########## project_heterogeneous_inference_ds4_parked.md ##########
---
name: project_heterogeneous_inference_ds4_parked
description: DeepSeek-V4-Flash/ds4 heterogeneous-inference exploration — PARKED; real goal + external-provider arch
metadata: 
  node_type: memory
  type: project
  originSessionId: eaee3057-f024-465e-992b-ff9796890799
---

Explored running DeepSeek-V4-Flash across Strix Halo (128GB, Vulkan/ROCm) + BeefyMac (64GB, Metal). **PARKED 2026-07-09** — off-axis for the true goal.

**Why:** The real goal is *demonstrating distributed inference across heterogeneous hardware*, not running that specific model. DeepSeek-V4 support is a WIP llama.cpp branch (not in our vendored binding — we ship `llama-cpp-4` fork at `vendor/llama-cpp-4` but pull `llama-cpp-sys-4 0.3.1` = llama.cpp `94a220cd6` unmodified from crates.io). Its best runner, `antirez/ds4` (DwarfStar), is a non-ggml C engine whose own distribution is homogeneous Mac↔Mac (cross-backend ROCm↔Metal unproven), and at q2 (~81GB) the model fits the Strix solo → never even exercises a split.

**Real path (the actual goal):** our existing ggml-RPC path is already validated heterogeneous (Strix-Vulkan host ↔ Mac-Metal worker at 4B, Mac holds only its shard). Scale it with a **Qwen-122B split** (already on disk, runs on our bindings on both backends) via the RUN_A_BIGGER_MODEL flow, measure tok/s. That's SHARED_MODEL ladder rung 1, zero bleeding edge, and *our mesh* does the splitting.

**Durable design learning — external-provider architecture:** don't add a new engine as a llama-cpp slot; add it as a *provider*. Generalize `PinnedWorkerEndpointSource` → a **managed external provider** (rented pod OR local sidecar): config-declared, child-supervised, health-gated before advertisement, scored in the OICP pool like any peer. Crash isolation becomes the *reason* (HTTP boundary → engine SEGV can't kill the daemon, unlike in-process GGML_ABORT). Route B (sidecar) is the permanent home for volatile engines; Route A (bump vendored binding) only once a model merges to llama.cpp master.

**Two distribution layers never compose:** request-level (OICP, engine-agnostic, where sidecars join) vs tensor-level (engine-specific — ours is ggml-RPC, ds4's is its own). Can't reuse our RPC sharding for a non-ggml engine.

**Strix ROCm gotcha:** default exposes only ~62GB of 128GB; unlock full GTT via GRUB (`amdgpu.gttsize` / `ttm.*`) + reboot. kyuz0 toolboxes make ROCm-on-gfx1151 viable (kernel ≥6.18.4, `-fa 1 --no-mmap`) — worth revisiting our Vulkan-only default someday. ds4 (fast, SSD-streaming, prebuilt kyuz0 toolbox) stays a good *fast local provider* option if we want DeepSeek-V4 for its own sake.

Full record: `docs/RUN_DEEPSEEK_V4_FLASH.md` (concluded honestly, not deleted). Relates to [[project_shared_fleet_initiative]], [[project_rpc_distributed_hang_root_cause]], [[project_pinned_pod_inference]], [[project_qwen3.5_122b_throughput]].

########## project_qwen3.5_122b_throughput.md ##########
---
name: project-qwen3-5-122b-throughput
description: Measured throughput of Qwen3.5-122B-A10B-Q5_K_XL on Strix Halo Vulkan — solo 19.34 tok/s (2026-07-30 mesh bench, valid record) vs two-node split 9.73; June atlas figure 14.8 superseded
metadata: 
  node_type: memory
  type: project
  originSessionId: 11482d6e-d24f-4594-816d-68049cc775de
  modified: 2026-07-31T00:22:52.480Z
---

**2026-07-30 UPDATE — valid solo `mesh bench` record supersedes the June decode figure.** Fully resident on RuggedFox (48 blocks local, 0 hops, canary-asserted; ctx 32768, build 0.4.0, embed+fast co-resident): **19.34 tok/s decode** (median of 3 steady trials, 19.33–19.38), TTFT 532ms, ITL p50 51.7ms, prefill 58 tok/s, cold load 67s. Store key `pd2:57ee26b477ce0f2c`. Network-latency tax vs the same-build two-node split (9.73 tok/s, ITL 77.4ms): **+25.7ms per token for the 1 hop — split runs at ~50% of solo; prefill 5.3× slower, TTFT 5.2×.** The old nodes=1 111 tok/s store entries were fast-slot-hijack artifacts, invalid. Load path gotchas that session: `SOVEREIGN_LOCAL_FIT_RESERVE_GB` (default total/8≈15.6GiB) gates the local load — need-estimate pads ~11GB over measured-resident; and `[compute] distributed_primary=true` + no discovered workers = primary waits forever ("unspawned"). Comparison published in docs/A_NUMBER_YOU_CAN_CHECK.md ("What the split costs").

2026-06-03 `sovereign bench atlas` on Qwen3.5-122B-A10B-UD-Q5_K_XL (3-shard GGUF at sovereign/models/), loaded as daemon primary inside the sovereign-vulkan toolbox.

- **~14.8 tok/s effective** (end-to-end; bench's `decode_tokens_per_sec` = completion_tokens ÷ total_latency, so ~4k-token prefill is folded in — pure decode ~17-18). Phase 1 mean 14.91, cluster_name_synth 14.43.
- **No MTP heads** — load log `MTP upgrade probe failed … gguf likely lacks MTP heads` → single-token decode, no speculative speedup. Contrast [[project_mtp_invariants]]: the 35B-A3B-**MTP** GGUF hit ~32 tok/s on the same box *with* MTP. So 122B ≈ half the speed for ~3.5× total params (~10B active either way).
- **100% Phase 1 success**, grammar-constrained (llguidance) structured extraction, no daemon crash — the [[project_grammar_alpha_blocker]] did NOT bite this model.
- **122 s/chapter** → full SEP (1800 art × 5 ch) projected **305 h ≈ 12.7 days single-slot**. Parallelize across mesh peers / pinned pods to cut this.
- **Memory:** 87.6 GB resident (48 layers, params=122111526912), fit in 116 GB Vulkan budget; peaked ~19 GiB free, swap untouched. context_size 32768.
- Results JSON: `~/.sovereign/bench-runs/atlas-qwen3.5-122b-a10b-q5kxl-2026-06-03.json`. Bench tool: [[project_atlas_bench]].

Config swap left a revert chain in `~/.sovereign/config.toml` comments (prior primary = Darwin-36B-Opus-Q4_K_L).

########## project_distributed_worker_flap_fix.md ##########
```

</details>

## Call 07

```bash
cd /home/alexbryan/dev/commonwealth-ai; echo "=== llama-cpp-sys version in lockfiles ==="; grep -rn -A2 'name = "llama-cpp-sys-4"' */Cargo.lock 2>/dev/null | head -20; echo; echo "=== vendored llama-cpp-4? ==="; ls vendor/ 2>/dev/null; ls sovereign/vendor/ 2>/dev/null; echo; echo "=== where is llama-cpp-sys sourced ==="; grep -rn "llama-cpp" sovereign/crates/*/Cargo.toml 2>/dev/null | head -20
```

<details><summary>output</summary>

```
=== llama-cpp-sys version in lockfiles ===

=== vendored llama-cpp-4? ===
lance-linalg
llama-cpp-4
llama-cpp-sys-4

=== where is llama-cpp-sys sourced ===
sovereign/crates/sovereign-cli-llm/Cargo.toml:20:# linked llama-cpp-2 in the first place.
sovereign/crates/sovereign-inference/Cargo.toml:38:# Jinja2 chat-template renderer. The llama-cpp-4 0.2.x binding
sovereign/crates/sovereign-inference/Cargo.toml:51:# llama-cpp-4 binding's `LlamaSampler::llguidance` shim doesn't
sovereign/crates/sovereign-inference/Cargo.toml:64:# lands a fix in a llama-cpp-2 release we can pin to.
sovereign/crates/sovereign-inference/Cargo.toml:66:# llama-cpp-4 0.5.1 (bumped from 0.4.2 on 2026-08-03). The crate bundles
sovereign/crates/sovereign-inference/Cargo.toml:69:# vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT.
sovereign/crates/sovereign-inference/Cargo.toml:80:# (PR #22673, our original reason for leaving llama-cpp-2 0.1.146), the GLM-5
sovereign/crates/sovereign-inference/Cargo.toml:95:# llama-cpp-sys-4 hunt for a Homebrew libomp and build ggml with
sovereign/crates/sovereign-inference/Cargo.toml:133:llama-cpp-4 = { version = "0.5.1", default-features = false, features = ["metal", "mtmd"] }
sovereign/crates/sovereign-inference/Cargo.toml:136:# than llama-cpp-4's `rpc` feature, whose binding module targets an older
sovereign/crates/sovereign-inference/Cargo.toml:140:llama-cpp-sys-4 = { version = "0.5.1", features = ["rpc"] }
sovereign/crates/sovereign-inference/Cargo.toml:146:llama-cpp-4 = { version = "0.5.1", default-features = false, features = ["mtmd"] }
sovereign/crates/sovereign-inference/Cargo.toml:147:llama-cpp-sys-4 = { version = "0.5.1", features = ["rpc"] }
sovereign/crates/sovereign-inference/Cargo.toml:150:llama-cpp-4 = { version = "0.5.1", features = ["vulkan"] }
sovereign/crates/sovereign-inference/Cargo.toml:151:llama-cpp-sys-4 = { version = "0.5.1", features = ["rpc"] }
sovereign/crates/sovereign-inference/Cargo.toml:162:llama-cpp-4 = { version = "0.5.1", default-features = false, features = ["mtmd"] }
sovereign/crates/sovereign-inference/Cargo.toml:163:llama-cpp-sys-4 = { version = "0.5.1", features = ["rpc"] }
sovereign/crates/sovereign-inference/Cargo.toml:173:windows-vulkan = ["llama-cpp-4/vulkan"]
sovereign/crates/sovereign-inference/Cargo.toml:174:windows-cuda = ["llama-cpp-4/cuda"]
sovereign/crates/sovereign-cli/Cargo.toml:16:#   removed: sovereign-inference (llama-cpp-2), sovereign-tools,
```

</details>

## Call 08

```bash
cd /home/alexbryan/dev/commonwealth-ai; echo "=== RPC / split code ==="; find . -name "rpc_warm_cache.rs" -o -name "rpc_distribution.rs" 2>/dev/null | grep -v target; echo; echo "=== plan_shards_weighted ==="; grep -rn "plan_shards_weighted\|is_routed_expert_tensor\|closest_boundary" --include=*.rs . 2>/dev/null | grep -v target | head -20; echo; echo "=== git status of those ==="; git status --porcelain | grep -iE "rpc_|mesh_cmd" | head
```

<details><summary>output</summary>

```
=== RPC / split code ===
./sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs
./sovereign/crates/sovereign-inference/src/embedded/rpc_distribution.rs
./.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_distribution.rs
./.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs

=== plan_shards_weighted ===
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-cli-llm/src/mesh_bench.rs:305:/// workers first, host last, which is the order `plan_shards_weighted` is
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-cli-llm/src/mesh_cmd.rs:1286:/// Pure. Uses the same `plan_shards_weighted` the live load uses, over the same
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-cli-llm/src/mesh_cmd.rs:1359:                    inf::plan_shards_weighted(n_layer, &weights, &mass.block_bytes, mass.head_bytes)
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-cli-llm/src/mesh_cmd.rs:3481:        let plan = inf::plan_shards_weighted(48, &weights, &mass.block_bytes, mass.head_bytes);
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-cli-llm/src/mesh_bench/tests.rs:667:         the order `plan_shards_weighted` is called with"
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:581:/// [`plan_shards_weighted`] apportions by real byte mass instead; this is its
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:584:    plan_shards_weighted(n_layer, weights, &[], 0)
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:604:pub fn plan_shards_weighted(
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:669:                closest_boundary(&prefix, cum_tgt, prev_cut, n_layer).max(prev_cut)
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:787:// `plan_shards_weighted` decides where each block GOES. Nothing above decides
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:861:        if is_routed_expert_tensor(name) {
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:968:/// `capacities` is in **plan order** — the same order `plan_shards_weighted`'s
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:1051:pub fn is_routed_expert_tensor(name: &str) -> bool {
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:1457:    // ── byte-mass-aware split (plan_shards_weighted) ──
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:1479:        assert_eq!(plan_shards_weighted(4, &w, &[], 0), plan_shards(4, &w));
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:1481:            plan_shards_weighted(4, &w, &[1, 2, 3], 0),
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:1493:        let byte: Vec<_> = plan_shards_weighted(12, &w, &bb, 0)
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:1514:        let byte = plan_shards_weighted(n, &[1.0, 1.0], &bb, 0);
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:1541:        let plan = plan_shards_weighted(10, &[1.0, 1.0], &bb, head);
.claude/worktrees/agent-aaaeff1fb0082cc25/sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:1556:        let plan = plan_shards_weighted(6, &[1.0, 0.0, 1.0], &bb, 0);

=== git status of those ===
```

</details>

## Call 09

```bash
cd /home/alexbryan/dev/commonwealth-ai; echo "=== LLAMA_CPP_COMMIT ==="; cat vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT 2>/dev/null; find vendor/llama-cpp-sys-4 -maxdepth 1 | head -20; echo; echo "=== does main have plan_shards_weighted? ==="; grep -c "plan_shards_weighted" sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs 2>/dev/null; echo "--- git log for that file ---"; git log --oneline -3 -- sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs
```

<details><summary>output</summary>

```
=== LLAMA_CPP_COMMIT ===
1464c62d88f699ec9700c8010bbfdbc603a9efd6

# What this file is
#
# `llama.cpp/` in this directory is a plain vendored copy with NO sovereign
# divergence. This file names the upstream commit it equals, because the
# bundled tree carries no marker of its own: llama.cpp fills BUILD_COMMIT at
# build time from a `.git` that does not exist here, so without this file the
# only way to identify the revision is to diff whole trees.
#
# The claim is checkable, not asserted:
#
#     ./scripts/verify-vendored-llama-cpp.sh
#
# It fetches the upstream tarball at the SHA on line 1 and compares every
# vendored file byte-for-byte. Verified 2026-08-03: 1552/1552 identical,
# 0 differing, 0 files absent upstream.
#
# Why this commit and not the one the crate ships
#
# llama-cpp-sys-4 0.5.1 bundles llama.cpp 221f0f6356efe2260023208365705ec5d5a7c8f5.
# 1464c62d88 is its DIRECT CHILD — exactly one commit ahead:
#
#     1464c62d88  metal: implement DSv4 Lightning Indexer (#25893)  2026-08-03
#
# That commit is load-bearing for distributed DeepSeek-V4. deepseek4.cpp:671
# calls ggml_lightning_indexer unconditionally on the DSv4 path, and at
# 221f0f63 GGML_OP_LIGHTNING_INDEXER is absent from Metal's supports_op. The
# RPC backend's supports_op is an unconditional `return true` (ggml-rpc.cpp,
# still a //TODO upstream), so the scheduler hands the Metal worker an op it
# cannot run and the worker aborts mid-graph. Shipping 221f0f63 would trade
# yesterday's csa_top_k_mask abort for a lightning-indexer abort.
#
# The commit touches 8 files; 7 are under ggml/src/ggml-metal/ and are shipped
# by the crate, the 8th is tests/test-backend-ops.cpp which the published crate
# strips. None overlap the three patches build.rs requires (0003/0004/0005 touch
# common/speculative.*, include/llama.h, src/llama-context.cpp,
# src/llama-cparams.h), so patch application is unaffected.
#
# How to move this pin
#
# Do NOT hand-edit files under llama.cpp/. Two reasons. First, build.rs's
# `llama_src_version` keys its re-copy cache on CMakeLists.txt's MTIME for a
# non-git tree, so an in-place edit elsewhere may not trigger a rebuild. Second,
# an edited tree matches no upstream commit and the verify script can no longer
# tell you what you are building.
#
# Instead: re-extract the whole tree at the new SHA, update line 1, and re-run
# the verify script. Adding a `patches/NNNN-*.patch` file is NOT a route either
# — build.rs:stage_active_patches copies only a HARDCODED allowlist
# (0003/0004/0005, plus 0001 under the `q1` feature) into the staged dir, and
# only staged patches are applied. A new patch file is silently ignored.
#
# When a llama-cpp-rs release ships a submodule at or past this SHA, this whole
# fast-forward becomes redundant: re-vendor the stock crate and set line 1 to
# whatever that release bundles.
vendor/llama-cpp-sys-4
vendor/llama-cpp-sys-4/llama.cpp
vendor/llama-cpp-sys-4/.cargo_vcs_info.json
vendor/llama-cpp-sys-4/Cargo.lock
vendor/llama-cpp-sys-4/Cargo.toml
vendor/llama-cpp-sys-4/Cargo.toml.orig
vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT
vendor/llama-cpp-sys-4/README.md
vendor/llama-cpp-sys-4/ext_shim
vendor/llama-cpp-sys-4/mtp_shim
vendor/llama-cpp-sys-4/patches
vendor/llama-cpp-sys-4/prebuilt_download.rs
vendor/llama-cpp-sys-4/src
vendor/llama-cpp-sys-4/wrapper.h
vendor/llama-cpp-sys-4/build.rs

=== does main have plan_shards_weighted? ===
14
--- git log for that file ---
1c9e7af4a Merge remote-tracking branch 'origin/main'
e8c00bf5b refactor(paths): ~/.svrnmesh is the only per-user root, and the compiler enforces it
c7b82215d fmt
```

</details>

## Call 10

```bash
cd /home/alexbryan/dev/commonwealth-ai; L=$(find / -maxdepth 8 -type d -name "llama.cpp*" -path "*llama-cpp-sys*" 2>/dev/null | head -3); echo "candidates: $L"; for d in vendor/llama-cpp-sys-4 ~/.cargo/registry/src/*/llama-cpp-sys-4-0.5.1; do echo "=== $d ==="; ls "$d" 2>/dev/null | head -20; done
```

<details><summary>output</summary>

```
candidates: /var/tmp/buildah-cache-1000/0e3aaf2d40883620/src/index.crates.io-1949cf8c6b5b557f/llama-cpp-sys-4-0.4.2/llama.cpp
/home/alexbryan/dev/commonwealth-ai/vendor/llama-cpp-sys-4/llama.cpp
/home/alexbryan/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/llama-cpp-sys-4-0.4.2/llama.cpp
=== vendor/llama-cpp-sys-4 ===
build.rs
Cargo.lock
Cargo.toml
Cargo.toml.orig
ext_shim
llama.cpp
LLAMA_CPP_COMMIT
mtp_shim
patches
prebuilt_download.rs
README.md
src
wrapper.h
=== /home/alexbryan/.cargo/registry/src/*/llama-cpp-sys-4-0.5.1 ===
```

</details>

## Call 11

```bash
cd /home/alexbryan/dev/commonwealth-ai/vendor/llama-cpp-sys-4/llama.cpp; echo "=== per-model sources ==="; ls src/models/ 2>/dev/null | head -60; echo "(count: $(ls src/models/ 2>/dev/null | wc -l))"; echo; echo "=== qwen3next / deltanet / GDN ==="; ls src/models/ 2>/dev/null | grep -iE "qwen|next|delta"; echo "--- grep ---"; grep -rilE "gated.?delta|deltanet|qwen3next|qwen3_next" src/ ggml/src/ 2>/dev/null | head -20
```

<details><summary>output</summary>

```
=== per-model sources ===
afmoe.cpp
apertus.cpp
arcee.cpp
arctic.cpp
arwkv7.cpp
baichuan.cpp
bailingmoe2.cpp
bailingmoe.cpp
bert.cpp
bitnet.cpp
bloom.cpp
chameleon.cpp
chatglm.cpp
codeshell.cpp
cogvlm.cpp
cohere2.cpp
cohere2moe.cpp
command-r.cpp
dbrx.cpp
deci.cpp
deepseek2.cpp
deepseek2ocr.cpp
deepseek32.cpp
deepseek4.cpp
deepseek.cpp
delta-net-base.cpp
dflash.cpp
dots1.cpp
dream.cpp
eagle3.cpp
ernie4-5.cpp
ernie4-5-moe.cpp
eurobert.cpp
exaone4.cpp
exaone.cpp
exaone-moe.cpp
falcon.cpp
falcon-h1.cpp
gemma2.cpp
gemma3.cpp
gemma3n.cpp
gemma4-assistant.cpp
gemma4.cpp
gemma.cpp
gemma-embedding.cpp
glm4.cpp
glm4-moe.cpp
glm-dsa.cpp
gpt2.cpp
gptneox.cpp
granite.cpp
granite-hybrid.cpp
granite-moe.cpp
grok.cpp
grovemoe.cpp
hunyuan-dense.cpp
hunyuan-moe.cpp
hunyuan-vl.cpp
hy-v3.cpp
internlm2.cpp
(count: 141)

=== qwen3next / deltanet / GDN ===
delta-net-base.cpp
qwen2.cpp
qwen2moe.cpp
qwen2vl.cpp
qwen35.cpp
qwen35moe.cpp
qwen3.cpp
qwen3moe.cpp
qwen3next.cpp
qwen3vl.cpp
qwen3vlmoe.cpp
qwen.cpp
rwkv6qwen2.cpp
--- grep ---
src/llama-arch.h
src/llama-cparams.h
src/llama-arch.cpp
src/llama-context.h
src/llama-model.cpp
src/llama-context.cpp
src/llama-model.h
src/models/delta-net-base.cpp
src/models/qwen35.cpp
src/models/qwen3next.cpp
src/models/qwen35moe.cpp
ggml/src/ggml-cpu/ggml-cpu.c
ggml/src/ggml-backend-meta.cpp
src/models/models.h
ggml/src/ggml-cpu/ops.h
ggml/src/ggml.c
ggml/src/ggml-cann/aclnn_ops.cpp
ggml/src/ggml-cuda/gated_delta_net.cuh
ggml/src/ggml-cpu/ops.cpp
ggml/src/ggml-cpu/spacemit/ime.cpp
```

</details>

## Call 12

```bash
cd /home/alexbryan/dev/commonwealth-ai/vendor/llama-cpp-sys-4/llama.cpp; echo "=== LLM_ARCH list ==="; grep -oE "LLM_ARCH_[A-Z0-9_]+" src/llama-arch.h | sort -u | tr '\n' ' '; echo; echo; echo "=== ggml ops (sparse/indexer/ssm) ==="; grep -oE "GGML_OP_[A-Z0-9_]+" ggml/include/ggml.h | sort -u | tr '\n' ' '
```

<details><summary>output</summary>

```
=== LLM_ARCH list ===
LLM_ARCH_AFMOE LLM_ARCH_APERTUS LLM_ARCH_ARCEE LLM_ARCH_ARCTIC LLM_ARCH_ARWKV7 LLM_ARCH_BAICHUAN LLM_ARCH_BAILINGMOE LLM_ARCH_BAILINGMOE2 LLM_ARCH_BERT LLM_ARCH_BITNET LLM_ARCH_BLOOM LLM_ARCH_CHAMELEON LLM_ARCH_CHATGLM LLM_ARCH_CLIP LLM_ARCH_CODESHELL LLM_ARCH_COGVLM LLM_ARCH_COHERE2 LLM_ARCH_COHERE2MOE LLM_ARCH_COMMAND_R LLM_ARCH_DBRX LLM_ARCH_DECI LLM_ARCH_DEEPSEEK LLM_ARCH_DEEPSEEK2 LLM_ARCH_DEEPSEEK2OCR LLM_ARCH_DEEPSEEK32 LLM_ARCH_DEEPSEEK4 LLM_ARCH_DFLASH LLM_ARCH_DOTS1 LLM_ARCH_DREAM LLM_ARCH_EAGLE3 LLM_ARCH_ERNIE4_5 LLM_ARCH_ERNIE4_5_MOE LLM_ARCH_EUROBERT LLM_ARCH_EXAONE LLM_ARCH_EXAONE4 LLM_ARCH_EXAONE_MOE LLM_ARCH_FALCON LLM_ARCH_FALCON_H1 LLM_ARCH_GEMMA LLM_ARCH_GEMMA2 LLM_ARCH_GEMMA3 LLM_ARCH_GEMMA3N LLM_ARCH_GEMMA4 LLM_ARCH_GEMMA4_ASSISTANT LLM_ARCH_GEMMA_EMBEDDING LLM_ARCH_GLM4 LLM_ARCH_GLM4_MOE LLM_ARCH_GLM_DSA LLM_ARCH_GPT2 LLM_ARCH_GPTJ LLM_ARCH_GPTNEOX LLM_ARCH_GRANITE LLM_ARCH_GRANITE_HYBRID LLM_ARCH_GRANITE_MOE LLM_ARCH_GROK LLM_ARCH_GROVEMOE LLM_ARCH_HUNYUAN_DENSE LLM_ARCH_HUNYUAN_MOE LLM_ARCH_HUNYUAN_VL LLM_ARCH_HY_V3 LLM_ARCH_INTERNLM2 LLM_ARCH_JAIS LLM_ARCH_JAIS2 LLM_ARCH_JAMBA LLM_ARCH_JINA_BERT_V2 LLM_ARCH_JINA_BERT_V3 LLM_ARCH_KIMI_LINEAR LLM_ARCH_LAGUNA LLM_ARCH_LFM2 LLM_ARCH_LFM2MOE LLM_ARCH_LLADA LLM_ARCH_LLADA_MOE LLM_ARCH_LLAMA LLM_ARCH_LLAMA4 LLM_ARCH_LLAMA_EMBED LLM_ARCH_MAINCODER LLM_ARCH_MAMBA LLM_ARCH_MAMBA2 LLM_ARCH_MELLUM LLM_ARCH_MIMO2 LLM_ARCH_MINICPM LLM_ARCH_MINICPM3 LLM_ARCH_MINIMAX_M2 LLM_ARCH_MINIMAX_M3 LLM_ARCH_MISTRAL3 LLM_ARCH_MISTRAL4 LLM_ARCH_MODERN_BERT LLM_ARCH_MPT LLM_ARCH_NANBEIGE LLM_ARCH_NEMOTRON LLM_ARCH_NEMOTRON_H LLM_ARCH_NEMOTRON_H_MOE LLM_ARCH_NEO_BERT LLM_ARCH_NOMIC_BERT LLM_ARCH_NOMIC_BERT_MOE LLM_ARCH_OLMO LLM_ARCH_OLMO2 LLM_ARCH_OLMOE LLM_ARCH_OPENAI_MOE LLM_ARCH_OPENELM LLM_ARCH_ORION LLM_ARCH_PADDLEOCR LLM_ARCH_PANGU_EMBED LLM_ARCH_PHI2 LLM_ARCH_PHI3 LLM_ARCH_PHIMOE LLM_ARCH_PLAMO LLM_ARCH_PLAMO2 LLM_ARCH_PLAMO3 LLM_ARCH_PLM LLM_ARCH_QWEN LLM_ARCH_QWEN2 LLM_ARCH_QWEN2MOE LLM_ARCH_QWEN2VL LLM_ARCH_QWEN3 LLM_ARCH_QWEN35 LLM_ARCH_QWEN35MOE LLM_ARCH_QWEN3MOE LLM_ARCH_QWEN3NEXT LLM_ARCH_QWEN3VL LLM_ARCH_QWEN3VLMOE LLM_ARCH_REFACT LLM_ARCH_RND1 LLM_ARCH_RWKV6 LLM_ARCH_RWKV6QWEN2 LLM_ARCH_RWKV7 LLM_ARCH_SEED_OSS LLM_ARCH_SMALLTHINKER LLM_ARCH_SMOLLM3 LLM_ARCH_STABLELM LLM_ARCH_STARCODER LLM_ARCH_STARCODER2 LLM_ARCH_STEP35 LLM_ARCH_T5 LLM_ARCH_T5ENCODER LLM_ARCH_TALKIE LLM_ARCH_UNKNOWN LLM_ARCH_WAVTOKENIZER_DEC LLM_ARCH_XVERSE 

=== ggml ops (sparse/indexer/ssm) ===
GGML_OP_ACC GGML_OP_ADD GGML_OP_ADD1 GGML_OP_ADD_ID GGML_OP_ADD_REL_POS GGML_OP_ARANGE GGML_OP_ARGMAX GGML_OP_ARGSORT GGML_OP_CLAMP GGML_OP_COL2IM_1D GGML_OP_CONCAT GGML_OP_CONT GGML_OP_CONV_2D GGML_OP_CONV_2D_DW GGML_OP_CONV_3D GGML_OP_CONV_TRANSPOSE_1D GGML_OP_CONV_TRANSPOSE_2D GGML_OP_COS GGML_OP_COUNT GGML_OP_COUNT_EQUAL GGML_OP_CPY GGML_OP_CROSS_ENTROPY_LOSS GGML_OP_CROSS_ENTROPY_LOSS_BACK GGML_OP_CUMSUM GGML_OP_CUSTOM GGML_OP_DIAG GGML_OP_DIAG_MASK_INF GGML_OP_DIAG_MASK_ZERO GGML_OP_DIV GGML_OP_DSV4_HC_COMB GGML_OP_DSV4_HC_POST GGML_OP_DSV4_HC_PRE GGML_OP_DUP GGML_OP_FILL GGML_OP_FLASH_ATTN_BACK GGML_OP_FLASH_ATTN_EXT GGML_OP_GATED_DELTA_NET GGML_OP_GATED_LINEAR_ATTN GGML_OP_GET_REL_POS GGML_OP_GET_ROWS GGML_OP_GET_ROWS_BACK GGML_OP_GLU GGML_OP_GROUP_NORM GGML_OP_IM2COL GGML_OP_IM2COL_3D GGML_OP_IM2COL_BACK GGML_OP_L2_NORM GGML_OP_LEAKY_RELU GGML_OP_LIGHTNING_INDEXER GGML_OP_LOG GGML_OP_MAP_CUSTOM1 GGML_OP_MAP_CUSTOM2 GGML_OP_MAP_CUSTOM3 GGML_OP_MEAN GGML_OP_MUL GGML_OP_MUL_MAT GGML_OP_MUL_MAT_ID GGML_OP_NONE GGML_OP_NORM GGML_OP_OPT_STEP_ADAMW GGML_OP_OPT_STEP_SGD GGML_OP_OUT_PROD GGML_OP_PAD GGML_OP_PAD_REFLECT_1D GGML_OP_PERMUTE GGML_OP_POOL_1D GGML_OP_POOL_2D GGML_OP_POOL_2D_BACK GGML_OP_POOL_AVG GGML_OP_POOL_COUNT GGML_OP_POOL_MAX GGML_OP_REPEAT GGML_OP_REPEAT_BACK GGML_OP_RESHAPE GGML_OP_RMS_NORM GGML_OP_RMS_NORM_BACK GGML_OP_ROLL GGML_OP_ROPE GGML_OP_ROPE_BACK GGML_OP_RWKV_WKV6 GGML_OP_RWKV_WKV7 GGML_OP_SCALE GGML_OP_SET GGML_OP_SET_ROWS GGML_OP_SILU_BACK GGML_OP_SIN GGML_OP_SOFT_MAX GGML_OP_SOFT_MAX_BACK GGML_OP_SOLVE_TRI GGML_OP_SQR GGML_OP_SQRT GGML_OP_SSM_CONV GGML_OP_SSM_SCAN GGML_OP_SUB GGML_OP_SUM GGML_OP_SUM_ROWS GGML_OP_TIMESTEP_EMBEDDING GGML_OP_TOP_K GGML_OP_TRANSPOSE GGML_OP_TRI GGML_OP_UNARY GGML_OP_UPSCALE GGML_OP_VIEW GGML_OP_WIN_PART GGML_OP_WIN_UNPART
```

</details>

## Call 13

```bash
cd /home/alexbryan/dev/commonwealth-ai/vendor/llama-cpp-sys-4/llama.cpp; echo "=== gemma3n PLE tensors ==="; grep -rn "per_layer\|PER_LAYER\|ple_" src/llama-arch.cpp | head -25; echo; echo "=== gemma3n.cpp shape ==="; wc -l src/models/gemma3n.cpp; grep -n "get_rows\|per_layer" src/models/gemma3n.cpp | head -20
```

<details><summary>output</summary>

```
=== gemma3n PLE tensors ===
src/llama-arch.cpp:181:    { LLM_KV_EMBEDDING_LENGTH_PER_LAYER,        "%s.embedding_length_per_layer_input"  },
src/llama-arch.cpp:481:    { LLM_TENSOR_PER_LAYER_TOKEN_EMBD,                   "per_layer_token_embd" },
src/llama-arch.cpp:482:    { LLM_TENSOR_PER_LAYER_MODEL_PROJ,                   "per_layer_model_proj" },
src/llama-arch.cpp:483:    { LLM_TENSOR_PER_LAYER_PROJ_NORM,                    "per_layer_proj_norm" },
src/llama-arch.cpp:486:    { LLM_TENSOR_PER_LAYER_INP_GATE,                     "blk.%d.inp_gate" },
src/llama-arch.cpp:487:    { LLM_TENSOR_PER_LAYER_PROJ,                         "blk.%d.proj" },
src/llama-arch.cpp:488:    { LLM_TENSOR_PER_LAYER_POST_NORM,                    "blk.%d.post_norm" },
src/llama-arch.cpp:802:    {LLM_TENSOR_PER_LAYER_TOKEN_EMBD,       {LLM_TENSOR_LAYER_INPUT,     GGML_OP_GET_ROWS}},
src/llama-arch.cpp:803:    {LLM_TENSOR_PER_LAYER_MODEL_PROJ,       {LLM_TENSOR_LAYER_REPEATING, GGML_OP_MUL_MAT}},
src/llama-arch.cpp:804:    {LLM_TENSOR_PER_LAYER_PROJ_NORM,        {LLM_TENSOR_LAYER_REPEATING, GGML_OP_MUL}},
src/llama-arch.cpp:807:    {LLM_TENSOR_PER_LAYER_INP_GATE,         {LLM_TENSOR_LAYER_REPEATING, GGML_OP_MUL_MAT}},
src/llama-arch.cpp:808:    {LLM_TENSOR_PER_LAYER_PROJ,             {LLM_TENSOR_LAYER_REPEATING, GGML_OP_MUL_MAT}},
src/llama-arch.cpp:809:    {LLM_TENSOR_PER_LAYER_POST_NORM,        {LLM_TENSOR_LAYER_REPEATING, GGML_OP_MUL}},

=== gemma3n.cpp shape ===
459 src/models/gemma3n.cpp
41:    per_layer_tok_embd   = create_tensor(tn(LLM_TENSOR_PER_LAYER_TOKEN_EMBD, "weight"), {n_embd_altup * n_layer, n_vocab}, 0);
42:    per_layer_model_proj = create_tensor(tn(LLM_TENSOR_PER_LAYER_MODEL_PROJ, "weight", 0), {n_embd, n_embd_altup * n_layer}, 0);
43:    per_layer_proj_norm  = create_tensor(tn(LLM_TENSOR_PER_LAYER_PROJ_NORM,  "weight", 0), {n_embd_altup}, 0);
66:        layer.per_layer_inp_gate   = create_tensor(tn(LLM_TENSOR_PER_LAYER_INP_GATE,  "weight", i), {n_embd, n_embd_altup}, 0);
67:        layer.per_layer_proj       = create_tensor(tn(LLM_TENSOR_PER_LAYER_PROJ,      "weight", i), {n_embd_altup, n_embd}, 0);
68:        layer.per_layer_post_norm  = create_tensor(tn(LLM_TENSOR_PER_LAYER_POST_NORM, "weight", i), {n_embd}, 0);
113:    ggml_tensor * inp_per_layer = build_inp_per_layer();
114:    ggml_build_forward_expand(gf, inp_per_layer);
116:    // inp_per_layer now has shape: [n_embd_altup, n_tokens, n_layer]
117:    inp_per_layer = project_per_layer_inputs(inpL, inp_per_layer);
234:            first_prediction = build_lora_mm(model.layers[il].per_layer_inp_gate, first_prediction);
238:            ggml_tensor * inp_this_layer = ggml_view_2d_slice(ctx0, inp_per_layer, il);   // [n_embd_altup, n_tokens]
242:            first_prediction = build_lora_mm(model.layers[il].per_layer_proj, first_prediction);  // [n_embd, n_tokens]
244:                build_norm(first_prediction, model.layers[il].per_layer_post_norm, NULL, LLM_NORM_RMS, il);
292:        cur                       = ggml_get_rows(ctx0, cur, inp_out_ids);
317:// equivalent to get_per_layer_inputs() in python code
319:ggml_tensor * llama_model_gemma3n::graph::build_inp_per_layer() {
321:    ggml_tensor * inp_per_layer;
327:        inp_per_layer = ggml_get_rows  (ctx0, model.per_layer_tok_embd, inp->tokens);
328:        inp_per_layer = ggml_reshape_3d(ctx0, inp_per_layer, n_embd_altup, n_layer, n_tokens);
```

</details>

## Call 14

```bash
cd /home/alexbryan/dev/commonwealth-ai/vendor/llama-cpp-sys-4/llama.cpp; echo "=== tensor buft override (-ot) ==="; grep -rn "tensor_buft_override\|override_tensor\|buft_override" include/llama.h src/llama-model-loader.h common/arg.cpp 2>/dev/null | head -15; echo; echo "=== qwen3next.cpp size + ops ==="; wc -l src/models/qwen3next.cpp src/models/delta-net-base.cpp src/models/deepseek4.cpp; echo; echo "=== hybrid memory (recurrent+attn) ==="; ls src/ | grep -iE "memory|kv"
```

<details><summary>output</summary>

```
=== tensor buft override (-ot) ===
src/llama-model-loader.h:92:    const llama_model_tensor_buft_override * tensor_buft_overrides;
src/llama-model-loader.h:135:        const llama_model_tensor_buft_override * param_tensor_buft_overrides_p);
include/llama.h:301:    struct llama_model_tensor_buft_override {
include/llama.h:311:        const struct llama_model_tensor_buft_override * tensor_buft_overrides;
include/llama.h:544:    LLAMA_API size_t llama_max_tensor_buft_overrides(void);
common/arg.cpp:250:static void parse_tensor_buffer_overrides(const std::string & value, std::vector<llama_model_tensor_buft_override> & overrides) {
common/arg.cpp:278:        static std::list<std::string> buft_overrides;
common/arg.cpp:279:        buft_overrides.push_back(tensor_name);
common/arg.cpp:280:        overrides.push_back({buft_overrides.back().c_str(), buft_list.at(buffer_type)});
common/arg.cpp:905:    // pad tensor_buft_overrides for llama_params_fit:
common/arg.cpp:906:    const size_t ntbo = llama_max_tensor_buft_overrides();
common/arg.cpp:907:    while (params.tensor_buft_overrides.size() < ntbo) {
common/arg.cpp:908:        params.tensor_buft_overrides.push_back({nullptr, nullptr});
common/arg.cpp:911:    if (!params.speculative.draft.tensor_buft_overrides.empty()) {
common/arg.cpp:912:        params.speculative.draft.tensor_buft_overrides.push_back({nullptr, nullptr});

=== qwen3next.cpp size + ops ===
   595 src/models/qwen3next.cpp
   606 src/models/delta-net-base.cpp
  1544 src/models/deepseek4.cpp
  2745 total

=== hybrid memory (recurrent+attn) ===
llama-kv-cache.cpp
llama-kv-cache-dsa.cpp
llama-kv-cache-dsa.h
llama-kv-cache-dsv4.cpp
llama-kv-cache-dsv4.h
llama-kv-cache.h
llama-kv-cache-iswa.cpp
llama-kv-cache-iswa.h
llama-kv-cells.h
llama-memory.cpp
llama-memory.h
llama-memory-hybrid.cpp
llama-memory-hybrid.h
llama-memory-hybrid-iswa.cpp
llama-memory-hybrid-iswa.h
llama-memory-recurrent.cpp
llama-memory-recurrent.h
```

</details>

## Call 15

```bash
cd /home/alexbryan/dev/commonwealth-ai; echo "=== python ==="; python3 -VV; python3 -c "import numpy; print('numpy', numpy.__version__)" 2>&1 | tail -1; for m in tokenizers transformers gguf; do python3 -c "import $m; print('$m', getattr($m,'__version__','?'))" 2>&1 | tail -1; done; echo; echo "=== gguf-py vendored? ==="; ls vendor/llama-cpp-sys-4/llama.cpp/gguf-py/gguf/ 2>/dev/null | head; echo; echo "=== llama-tokenize built? ==="; find . -name "llama-tokenize" -o -name "llama-cli" 2>/dev/null | grep -v "\.git" | head
```

<details><summary>output</summary>

```
=== python ===
Python 3.14.7 (main, Aug 10 2026, 00:00:00) [GCC 16.1.1 20260515 (Red Hat 16.1.1-2)]
numpy 2.4.4
ModuleNotFoundError: No module named 'tokenizers'
ModuleNotFoundError: No module named 'transformers'
ModuleNotFoundError: No module named 'gguf'

=== gguf-py vendored? ===

=== llama-tokenize built? ===
./target-container-linux/llama-cmake-cache/380bf11711a74cf6/bin/llama-tokenize
./.claude/worktrees/agent-aaaeff1fb0082cc25/target/llama-cmake-cache/3cf5f825d3a057ab/bin/llama-tokenize
./.claude/worktrees/agent-aaaeff1fb0082cc25/target/llama-cmake-cache/3cf5f825d3a057ab/build/bin/llama-tokenize
./target/llama-cmake-cache/380bf11711a74cf6/bin/llama-tokenize
./target/llama-cmake-cache/380bf11711a74cf6/build/bin/llama-tokenize
./target-container-linux/llama-cmake-cache/380bf11711a74cf6/build/bin/llama-tokenize
```

</details>

## Call 16

```bash
cd /home/alexbryan/dev/commonwealth-ai; echo "=== candidate corpora ==="; du -sh research/deep-research 2>/dev/null; find research/deep-research -name "*.article.md" 2>/dev/null | wc -l; echo "--- sep / wikipedia corpora ---"; ls ~/.sovereign/corpora 2>/dev/null | head -20; ls ~/.svrnmesh 2>/dev/null | head; echo "--- markdown mass in repo ---"; find . -name "*.md" -not -path "./.git/*" -not -path "./vendor/*" -not -path "./target/*" 2>/dev/null | wc -l
```

<details><summary>output</summary>

```
=== candidate corpora ===
510M	research/deep-research
1
--- sep / wikipedia corpora ---
sep
active_notes_db
arch
backups
bench-cache
bench-corpora
bench-runs
capabilities
catalog-embed-cache.json
client-exposed
codex-sessions
--- markdown mass in repo ---
9050
```

</details>

## Call 17

```bash
cd /home/alexbryan/dev/commonwealth-ai; grep -n "vocab_only" vendor/llama-cpp-sys-4/llama.cpp/tools/tokenize/tokenize.cpp 2>/dev/null | head; echo "---"; TK=./target/llama-cmake-cache/380bf11711a74cf6/bin/llama-tokenize; ls -la $TK; echo "test:"; printf 'The quick brown fox jumps over the lazy dog.' > /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/tk.txt; timeout 180 $TK -m sovereign/models/Qwen3.8-27B-UD-Q6_K_XL.gguf -f /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/tk.txt --ids 2>/dev/null | tail -3
```

<details><summary>output</summary>

```
146:    model_params.vocab_only = true;
---
-rwxr-xr-x. 1 alexbryan alexbryan 62268216 Aug 24 13:49 ./target/llama-cmake-cache/380bf11711a74cf6/bin/llama-tokenize
test:
[760, 3841, 13477, 37550, 33075, 888, 279, 15217, 5388, 13]
```

</details>

## Call 18

```bash
ls -la ~/.sovereign/corpora/sep/ 2>/dev/null | head -20; echo "--- size ---"; du -sh ~/.sovereign/corpora/sep 2>/dev/null; echo "--- any raw text? ---"; find ~/.sovereign/corpora/sep -maxdepth 2 -type d 2>/dev/null | head -20
```

<details><summary>output</summary>

```
total 0
drwxr-xr-x. 1 alexbryan alexbryan    16 Apr 25 17:15 .
drwxr-xr-x. 1 alexbryan alexbryan     6 Apr 25 17:15 ..
drwxr-xr-x. 1 alexbryan alexbryan 60828 May 11 23:46 articles
--- size ---
131M	/home/alexbryan/.sovereign/corpora/sep
--- any raw text? ---
/home/alexbryan/.sovereign/corpora/sep
/home/alexbryan/.sovereign/corpora/sep/articles
```

</details>

## Call 19

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad; mkdir -p $S/engram; ls ~/.sovereign/corpora/sep/articles | head -5; echo "count: $(ls ~/.sovereign/corpora/sep/articles | wc -l)"; F=$(ls ~/.sovereign/corpora/sep/articles/* | head -1); echo "=== sample: $F ==="; head -c 600 "$F"; echo; echo "..."; file "$F"
```

<details><summary>output</summary>

```
18thGerman-preKant.md
abduction.md
abelard.md
abhidharma.md
abilities.md
count: 1770
=== sample: /home/alexbryan/.sovereign/corpora/sep/articles/18thGerman-preKant.md ===
<!-- Stanford Encyclopedia of Philosophy — source: https://plato.stanford.edu/entries/18thGerman-preKant/ -->

## Section 001

Kant undoubtedly casts a long shadow in the history of eighteenth century German philosophy. Not only did he initiate a revolution in philosophy, but in the course of doing so he thoroughly exposed the metaphysical systems of his predecessors as rationalistic castles in the air. This latter, negative part of his project was in fact so successful that the pre-Kantian period of German philosophy is widely viewed, even today, as a period of benighted dogmatism. During t
...
/home/alexbryan/.sovereign/corpora/sep/articles/18thGerman-preKant.md: exported SGML document, Unicode text, UTF-8 text, with very long lines (2299)
```

</details>

## Call 20

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram; cat > $S/PRE-REGISTRATION.md <<'EOF'
# Engram hot-set experiment — bars fixed before any data

Question: can the 51.2B-param n-gram embedding table (16 heads x 20M rows x 160
dims) be served from a SMALL resident hot set, with the cold remainder on NVMe
or on a mesh peer?

Geometry taken from the vendor writeup (2026-08-26), treated as spec:
  8 bigram heads + 8 trigram heads, 20,000,000 rows/head, 160 dims/row.

## What is measured
Row-ID access distribution under a uniform hash, over real corpora tokenized
with a real Qwen tokenizer. Hit rate of a STATIC top-K-by-frequency hot set,
mined on one corpus and evaluated on a DISJOINT one.

## Why the hash identity does not need to be the vendor's
Row frequency = n-gram frequency pushed through a many-to-one uniform map.
Any well-distributed hash gives the same CDF SHAPE; only the specific
collision pairs differ. The decision below turns on order of magnitude.

## Pre-registered decision bars (held-out hit rate, all 16 heads)
- GREEN  : <= 2 GiB resident hot set reaches >= 95% ==> tiering is the design.
           Cold remainder may live on NVMe or on a peer; ITL tax = miss x RTT.
- AMBER  : needs 2-16 GiB for 95%  ==> tiering works locally, remote is marginal.
- RED    : needs > 16 GiB for 95%  ==> hot set is NOT a lever. Engram must be
           fully resident; the only remaining lever is quantization.

## Held-out discipline
Mine on sep_mine (even-index SEP articles). Evaluate on:
  - sep_holdout (odd-index SEP articles)  -- in-domain, unseen
  - repo_md                               -- near-domain shift
  - rust_src                              -- hard domain shift
A hot set that only scores on its own mining corpus is reported as FAILED.

## Null / instrument check
Shuffle the token stream (destroys n-gram structure, preserves unigram
frequency). If the shuffled stream shows the same hot-set curve, the curve is
a unigram artifact, not an n-gram result, and the experiment is void.
EOF
echo "pre-registration written"; wc -l $S/PRE-REGISTRATION.md
```

<details><summary>output</summary>

```
pre-registration written
37 /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/PRE-REGISTRATION.md
```

</details>

## Call 21

```bash
cd /home/alexbryan/dev/commonwealth-ai; S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram
# sep split: even index -> mine, odd -> holdout
i=0; : > $S/sep_mine.txt; : > $S/sep_holdout.txt
for f in ~/.sovereign/corpora/sep/articles/*.md; do
  if [ $((i % 2)) -eq 0 ]; then cat "$f" >> $S/sep_mine.txt; else cat "$f" >> $S/sep_holdout.txt; fi
  i=$((i+1))
done
# repo markdown (technical prose), excluding vendor/target/git
find . -name "*.md" -not -path "./.git/*" -not -path "./vendor/*" -not -path "./target*" -not -path "./.claude/worktrees/*" -print0 2>/dev/null | xargs -0 cat > $S/repo_md.txt 2>/dev/null
# rust source
find sovereign corpus-engine commonwealth -name "*.rs" -not -path "*/target/*" -print0 2>/dev/null | xargs -0 cat > $S/rust_src.txt 2>/dev/null
ls -la $S/*.txt | awk '{printf "%-20s %10.1f MB\n", $NF, $5/1048576}'
```

<details><summary>output</summary>

```
/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/repo_md.txt       53.8 MB
/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/rust_src.txt       35.3 MB
/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/sep_holdout.txt       63.1 MB
/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/sep_mine.txt       64.0 MB
```

</details>

## Call 22

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram; cd /home/alexbryan/dev/commonwealth-ai
TK=./target/llama-cmake-cache/380bf11711a74cf6/bin/llama-tokenize
M=sovereign/models/Qwen3.8-27B-UD-Q6_K_XL.gguf
head -c 4000000 $S/sep_mine.txt > $S/_timing.txt
echo "=== timing 4MB chunk ==="; /usr/bin/time -f "wall %e s  maxrss %M KB" $TK -m $M -f $S/_timing.txt --ids 2>&1 >$S/_timing.ids | tail -2
echo "output bytes: $(wc -c < $S/_timing.ids)"; head -c 200 $S/_timing.ids; echo; echo "..."; tail -c 100 $S/_timing.ids
```

<details><summary>output</summary>

```
=== timing 4MB chunk ===
0.00.364.725 W llama_context: n_ctx_seq (512) > n_ctx_train (0) -- possible training context overflow
wall 2.06 s  maxrss 254188 KB
output bytes: 4798040
[6164, 29687, 64831, 314, 44340, 1892, 2450, 25, 3577, 1074, 490, 4189, 1194, 90856, 12225, 14, 12569, 14, 16, 23, 337, 31022, 20678, 42, 505, 14, 3716, 271, 550, 10790, 220, 15, 15, 16, 271, 42, 505,
...
 3140, 1010, 4138, 314, 1880, 318, 72, 1673, 2487, 17264, 321, 30333, 4138, 8, 513, 264, 2993, 220]
```

</details>

## Call 23

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram; cd /home/alexbryan/dev/commonwealth-ai
cat > $S/tokenize.sh <<'EOF'
set -e
S="$1"
TK=/home/alexbryan/dev/commonwealth-ai/target/llama-cmake-cache/380bf11711a74cf6/bin/llama-tokenize
M=/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.8-27B-UD-Q6_K_XL.gguf
mkdir -p "$S/chunks"
rm -f "$S/chunks"/*
for c in sep_mine sep_holdout repo_md rust_src; do
  split -C 16m -d -a 3 "$S/$c.txt" "$S/chunks/${c}." 
done
ls "$S/chunks" | wc -l
ls "$S/chunks"/* | xargs -P 3 -I{} sh -c "$TK -m $M -f {} --ids 2>/dev/null > {}.ids"
echo "tokenized"
EOF
rm -f $S/_timing.txt $S/_timing.ids
time bash $S/tokenize.sh $S 2>&1 | tail -5
```

<details><summary>output</summary>

```
15
tokenized

real	0m37.395s
user	1m28.521s
sys	0m16.215s
```

</details>

## Call 24

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram
cat > $S/to_npy.py <<'PY'
import numpy as np, glob, os, sys, re
S = sys.argv[1]
for c in ["sep_mine","sep_holdout","repo_md","rust_src"]:
    parts = []
    for f in sorted(glob.glob(f"{S}/chunks/{c}.*.ids")):
        raw = open(f, "r").read().strip()
        if not raw: continue
        raw = raw.strip().lstrip("[").rstrip("]")
        a = np.fromstring(raw, dtype=np.int64, sep=",")
        parts.append(a.astype(np.uint32))
    toks = np.concatenate(parts)
    np.save(f"{S}/{c}.npy", toks)
    print(f"{c:14s} {len(toks):>12,} tokens   max_id={toks.max():,}")
PY
python3 -W ignore $S/to_npy.py $S
```

<details><summary>output</summary>

```
sep_mine         14,497,455 tokens   max_id=247,199
sep_holdout      14,301,769 tokens   max_id=247,408
repo_md          15,855,182 tokens   max_id=248,069
rust_src          9,431,903 tokens   max_id=248,069
```

</details>

## Call 25

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram
cat > $S/engram_sim.py <<'PY'
"""Engram hot-set simulator.

Models the Qwen3.8-Flash n-gram embedding table addressing:
  8 bigram heads + 8 trigram heads, 20M rows/head, 160 dims/row.
Row id = splitmix64(ngram_key ^ seed_head) % N_ROWS.

Measures, per head, the coverage of a STATIC top-K-by-frequency hot set
mined on one corpus and evaluated on a disjoint one.
"""
import numpy as np, sys, json

N_ROWS   = 20_000_000
V_BITS   = 18            # vocab 248,069 < 2^18
N_BI, N_TRI = 8, 8
ROW_DIM  = 160

M1 = np.uint64(0xBF58476D1CE4E5B9)
M2 = np.uint64(0x94D049BB133111EB)

def splitmix64(z):
    z = z.copy()
    z ^= z >> np.uint64(30); z *= M1
    z ^= z >> np.uint64(27); z *= M2
    z ^= z >> np.uint64(31)
    return z

def ngram_keys(toks, n):
    """Packed n-gram keys, one per position with full history."""
    t = toks.astype(np.uint64)
    k = np.zeros(len(t) - (n - 1), dtype=np.uint64)
    for j in range(n):
        k |= t[j : len(t) - (n - 1) + j] << np.uint64(V_BITS * (n - 1 - j))
    return k

def rows_for_head(keys, seed):
    return (splitmix64(keys ^ np.uint64(seed)) % np.uint64(N_ROWS)).astype(np.uint32)

def coverage_curve(rows_mine, rows_eval):
    """Exact coverage of eval accesses by a top-K hot set mined on rows_mine.

    Returns cumulative coverage indexed by K (hot-set size in rows)."""
    cm = np.bincount(rows_mine, minlength=N_ROWS).astype(np.int32)
    ce = np.bincount(rows_eval, minlength=N_ROWS).astype(np.int32)
    nz = np.flatnonzero(cm)                       # rows the miner ever saw
    order = nz[np.argsort(cm[nz])[::-1]]          # ranked by mine frequency
    cum = np.cumsum(ce[order].astype(np.int64))
    return cum / len(rows_eval), len(order)

def at_k(curve, k):
    if k <= 0: return 0.0
    return float(curve[min(k, len(curve)) - 1])

def run(mine_name, evals, seeds_bi, seeds_tri, tag=""):
    mine = np.load(f"{S}/{mine_name}.npy")
    out = {}
    for kind, n, seeds in (("bigram", 2, seeds_bi), ("trigram", 3, seeds_tri)):
        km = ngram_keys(mine, n)
        for hi, seed in enumerate(seeds):
            rm = rows_for_head(km, seed)
            for ev_name in evals:
                ev = np.load(f"{S}/{ev_name}.npy")
                ke = ngram_keys(ev, n)
                re_ = rows_for_head(ke, seed)
                curve, n_mined = coverage_curve(rm, re_)
                out.setdefault((kind, ev_name), []).append(
                    dict(seed=seed, n_mined=n_mined,
                         uniq_eval=int(len(np.unique(re_))),
                         k=[at_k(curve, K) for K in KS],
                         ceiling=float(curve[-1])))
                del ke, re_, ev, curve
            del rm
        del km
    return out

KS = [10_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 2_000_000,
      4_000_000, 8_000_000, 16_000_000]

if __name__ == "__main__":
    S = sys.argv[1]
    # 2 heads per kind is statistically sufficient: heads differ only in
    # which collisions occur, not in the frequency distribution's shape.
    seeds_bi  = [0x1000, 0x2000]
    seeds_tri = [0xA000, 0xB000]
    evals = ["sep_mine", "sep_holdout", "repo_md", "rust_src"]
    res = run("sep_mine", evals, seeds_bi, seeds_tri)

    print(f"{'kind':8s} {'eval corpus':13s} {'uniq rows':>11s} {'ceil':>6s} " +
          " ".join(f"{k//1000}k".rjust(7) for k in KS))
    print("-" * 118)
    rows = {}
    for (kind, ev), lst in sorted(res.items()):
        k = np.mean([x["k"] for x in lst], axis=0)
        rows[(kind, ev)] = k
        ceil = np.mean([x["ceiling"] for x in lst])
        uq = int(np.mean([x["uniq_eval"] for x in lst]))
        star = " <- MINE" if ev == "sep_mine" else ""
        print(f"{kind:8s} {ev:13s} {uq:>11,} {ceil:>6.3f} " +
              " ".join(f"{v:7.3f}" for v in k) + star)
    json.dump({f"{a}|{b}": list(v) for (a, b), v in rows.items()},
              open(f"{S}/curves.json", "w"), indent=1)
    print(f"\nKS = {KS}")
PY
echo "written"; time python3 -W ignore $S/engram_sim.py $S
```

<details><summary>output</summary>

```
written
kind     eval corpus     uniq rows   ceil     10k     50k    100k    250k    500k   1000k   2000k   4000k   8000k  16000k
----------------------------------------------------------------------------------------------------------------------
bigram   repo_md         1,457,896  0.467   0.193   0.252   0.285   0.329   0.375   0.417   0.467   0.467   0.467   0.467
bigram   rust_src        1,048,673  0.272   0.069   0.103   0.122   0.155   0.195   0.230   0.272   0.272   0.272   0.272
bigram   sep_holdout     1,775,347  0.907   0.458   0.630   0.702   0.788   0.841   0.877   0.907   0.907   0.907   0.907
bigram   sep_mine        1,765,412  1.000   0.466   0.648   0.728   0.826   0.892   0.947   1.000   1.000   1.000   1.000 <- MINE
trigram  repo_md         3,262,932  0.400   0.050   0.085   0.101   0.125   0.149   0.182   0.237   0.331   0.400   0.400
trigram  rust_src        2,624,712  0.317   0.015   0.023   0.029   0.042   0.058   0.087   0.143   0.245   0.317   0.317
trigram  sep_holdout     5,464,089  0.706   0.176   0.272   0.322   0.395   0.453   0.513   0.580   0.653   0.706   0.706
trigram  sep_mine        5,445,413  1.000   0.182   0.291   0.350   0.444   0.529   0.633   0.762   0.900   1.000   1.000 <- MINE

KS = [10000, 50000, 100000, 250000, 500000, 1000000, 2000000, 4000000, 8000000, 16000000]

real	1m53.259s
user	1m8.734s
sys	0m46.161s
```

</details>

## Call 26

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram
cat > $S/scaling.py <<'PY'
"""Is the coverage ceiling a property of the table, or of my mining budget?

Mines hot sets from increasing prefixes of sep_mine and measures the ceiling
(fraction of held-out accesses whose row was seen AT LEAST ONCE while mining).
If the ceiling grows like log(mining tokens), the tail is fat and no
realistically-mined hot set closes it.

Also runs the pre-registered NULL: shuffled tokens destroy n-gram structure
while preserving unigram frequency.
"""
import numpy as np, sys
sys.path.insert(0, sys.argv[1])
from engram_sim import ngram_keys, rows_for_head, N_ROWS

S = sys.argv[1]
mine = np.load(f"{S}/sep_mine.npy")
hold = np.load(f"{S}/sep_holdout.npy")
rng  = np.random.default_rng(20260826)
shuf = rng.permutation(mine)          # null: same unigrams, no n-gram structure

SIZES = [500_000, 1_000_000, 2_000_000, 4_000_000, 8_000_000, len(mine)]

print(f"{'kind':8s} {'mine toks':>11s} {'ceiling':>8s} {'uniq mined':>12s} "
      f"{'@1M rows':>9s} {'@4M rows':>9s}   (eval = sep_holdout)")
print("-" * 88)
for kind, n, seed in (("bigram", 2, 0x1000), ("trigram", 3, 0xA000)):
    ke = ngram_keys(hold, n); re_ = rows_for_head(ke, seed)
    ce = np.bincount(re_, minlength=N_ROWS).astype(np.int32)
    tot = len(re_)
    for sz in SIZES:
        km = ngram_keys(mine[:sz], n); rm = rows_for_head(km, seed)
        cm = np.bincount(rm, minlength=N_ROWS).astype(np.int32)
        nz = np.flatnonzero(cm)
        order = nz[np.argsort(cm[nz])[::-1]]
        cum = np.cumsum(ce[order].astype(np.int64)) / tot
        g = lambda K: float(cum[min(K, len(cum)) - 1]) if len(cum) else 0.0
        print(f"{kind:8s} {sz:>11,} {cum[-1]:>8.3f} {len(nz):>12,} "
              f"{g(1_000_000):>9.3f} {g(4_000_000):>9.3f}")
        del km, rm, cm, nz, order, cum
    # null
    km = ngram_keys(shuf, n); rm = rows_for_head(km, seed)
    cm = np.bincount(rm, minlength=N_ROWS).astype(np.int32)
    nz = np.flatnonzero(cm); order = nz[np.argsort(cm[nz])[::-1]]
    cum = np.cumsum(ce[order].astype(np.int64)) / tot
    print(f"{kind:8s} {'NULL(shuf)':>11s} {cum[-1]:>8.3f} {len(nz):>12,} "
          f"{float(cum[min(1_000_000,len(cum))-1]):>9.3f} "
          f"{float(cum[min(4_000_000,len(cum))-1]):>9.3f}")
    print()
    del ke, re_, ce, km, rm, cm, nz, order, cum
PY
time python3 -W ignore $S/scaling.py $S
```

<details><summary>output</summary>

```
kind       mine toks  ceiling   uniq mined  @1M rows  @4M rows   (eval = sep_holdout)
----------------------------------------------------------------------------------------
bigram       500,000    0.644      169,782     0.644     0.644
bigram     1,000,000    0.709      278,213     0.709     0.709
bigram     2,000,000    0.779      479,934     0.779     0.779
bigram     4,000,000    0.830      755,389     0.830     0.830
bigram     8,000,000    0.876    1,216,836     0.862     0.876
bigram    14,497,455    0.907    1,765,635     0.877     0.907
bigram    NULL(shuf)    0.808    4,336,904     0.692     0.798

trigram      500,000    0.288      339,276     0.288     0.288
trigram    1,000,000    0.354      619,095     0.354     0.354
trigram    2,000,000    0.436    1,158,679     0.418     0.436
trigram    4,000,000    0.520    1,995,067     0.444     0.520
trigram    8,000,000    0.619    3,517,373     0.491     0.619
trigram   14,497,455    0.706    5,444,616     0.513     0.653
trigram   NULL(shuf)    0.564    9,203,745     0.167     0.325


real	0m38.598s
user	0m9.986s
sys	0m30.943s
```

</details>

## Call 27

```bash
cd /home/alexbryan/dev/commonwealth-ai/vendor/llama-cpp-sys-4/llama.cpp; echo "=== block sizes ==="; grep -nE "#define QK4_0|#define QK4_1|#define QK5_0|#define QK8_0|#define QK_K|#define QK4_NL" ggml/src/ggml-common.h | head; echo; echo "=== struct sizes (bytes/block) ==="; grep -nE "static_assert\(sizeof\(block_(q4_0|q5_0|q8_0|q4_K|q6_K)\)" ggml/src/ggml-common.h | head; echo; echo "=== does GET_ROWS require row%blocksize==0? ==="; grep -n "ne00 % \|% ggml_blck_size\|ggml_blck_size(src0->type)" ggml/src/ggml.c | head -8
```

<details><summary>output</summary>

```
=== block sizes ===
89:#define QK_K 256
194:#define QK4_0 32
201:#define QK4_1 32
229:#define QK5_0 32
251:#define QK8_0 32
447:#define QK4_NL 32

=== struct sizes (bytes/block) ===
199:static_assert(sizeof(block_q4_0) == sizeof(ggml_half) + QK4_0 / 2, "wrong q4_0 block size/padding");
235:static_assert(sizeof(block_q5_0) == sizeof(ggml_half) + sizeof(uint32_t) + QK5_0 / 2, "wrong q5_0 block size/padding");
256:static_assert(sizeof(block_q8_0) == sizeof(ggml_half) + QK8_0, "wrong q8_0 block size/padding");
338:static_assert(sizeof(block_q4_K) == 2*sizeof(ggml_half) + K_SCALE_SIZE + QK_K/2, "wrong q4_K block size/padding");
368:static_assert(sizeof(block_q6_K) == sizeof(ggml_half) + QK_K / 16 + 3*QK_K/4, "wrong q6_K block size/padding");

=== does GET_ROWS require row%blocksize==0? ===
1341:    assert(ne % ggml_blck_size(type) == 0);
```

</details>

## Call 28

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram
cat > $S/nvme_gather.py <<'PY'
"""Price the engram access pattern on this host's NVMe.

Per decoded token the model gathers 16 rows (one per n-gram head). The row ids
are independent, so the 16 reads issue concurrently: cost is ONE device round
trip, not sixteen. Measures p50/p90/p99 of a 16-way concurrent random gather
over a 92 GB file, cold (page cache advised away) and warm (cache-resident).
"""
import os, sys, time, numpy as np
from concurrent.futures import ThreadPoolExecutor

PATH = sys.argv[1]
ROW  = 170          # Q8_0: 160 dims = 5 blocks x 34 B
HEADS= 16
N    = int(sys.argv[2]) if len(sys.argv) > 2 else 400

size = os.path.getsize(PATH)
fd   = os.open(PATH, os.O_RDONLY)
rng  = np.random.default_rng(7)
print(f"file {PATH.split('/')[-1]}  {size/2**30:.1f} GiB   rows/token={HEADS} row={ROW}B")

def gather(offs, pool):
    list(pool.map(lambda o: os.pread(fd, ROW, int(o)), offs))

def bench(label, cold):
    lat = []
    with ThreadPoolExecutor(max_workers=HEADS) as pool:
        base = rng.integers(0, size - ROW, size=HEADS) if not cold else None
        for _ in range(N):
            offs = rng.integers(0, size - ROW, size=HEADS) if cold else base
            if cold:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            t = time.perf_counter_ns()
            gather(offs, pool)
            lat.append((time.perf_counter_ns() - t) / 1000.0)   # us
    a = np.array(lat)
    print(f"  {label:22s} p50 {np.percentile(a,50):8.1f} us   "
          f"p90 {np.percentile(a,90):8.1f} us   p99 {np.percentile(a,99):8.1f} us")
    return float(np.percentile(a, 50))

print("\n16-way concurrent gather, one token's worth:")
cold = bench("COLD (fadvise away)", True)
warm = bench("WARM (page-cached)",  False)
os.close(fd)

print(f"\nAs a share of a decode step:")
for tps, name in ((39, "projected Flash 6B-active"), (19.34, "measured 122B-A10B")):
    budget = 1e6 / tps
    print(f"  {name:28s} {budget:7.0f} us/token -> "
          f"cold {100*cold/budget:5.2f}%   warm {100*warm/budget:5.2f}%")
PY
python3 -W ignore $S/nvme_gather.py /home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.5-122B-merged.Q5_K_XL.gguf 400
```

<details><summary>output</summary>

```
Exit code 1
Traceback (most recent call last):
  File "/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/nvme_gather.py", line 16, in <module>
    size = os.path.getsize(PATH)
  File "<frozen genericpath>", line 86, in getsize
FileNotFoundError: [Errno 2] No such file or directory: '/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.5-122B-merged.Q5_K_XL.gguf'
```

</details>

## Call 29

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram
python3 -W ignore $S/nvme_gather.py /home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.5-122B-A10B-UD-Q5_K_XL/Qwen3.5-122B-merged.Q5_K_XL.gguf 400
```

<details><summary>output</summary>

```
file Qwen3.5-122B-merged.Q5_K_XL.gguf  85.6 GiB   rows/token=16 row=170B

16-way concurrent gather, one token's worth:
  COLD (fadvise away)    p50    376.3 us   p90    533.5 us   p99    710.8 us
  WARM (page-cached)     p50    284.0 us   p90    445.6 us   p99    526.1 us

As a share of a decode step:
  projected Flash 6B-active      25641 us/token -> cold  1.47%   warm  1.11%
  measured 122B-A10B             51706 us/token -> cold  0.73%   warm  0.55%
```

</details>

## Call 30

```bash
cd /home/alexbryan/dev/commonwealth-ai/vendor/llama-cpp-sys-4/llama.cpp; echo "=== mmap: does a host buffer point INTO the mapping? ==="; grep -n "is_host\|mmap_used\|ggml_backend_buft_is_host\|data = (uint8_t \*) mapping->addr" src/llama-model-loader.cpp | head -20; echo; echo "=== gemma3n: is the big PLE table forced to CPU? ==="; grep -rn "per_layer_token_embd\|PER_LAYER_TOKEN_EMBD" src/llama-model.cpp | head -6
```

<details><summary>output</summary>

```
=== mmap: does a host buffer point INTO the mapping? ===
1540:            uint8_t * data = (uint8_t *) mapping->addr() + weight->offs;
1556:                auto & mmap_used = mmaps_used[weight->idx];
1557:                mmap_used.first  = std::min(mmap_used.first,  weight->offs);
1558:                mmap_used.second = std::max(mmap_used.second, weight->offs + n_size);
1565:            if (ggml_backend_buffer_is_host(cur->buffer)) {
1670:                const auto & mmap_used = mmaps_used.at(idx);
1672:                mapping->unmap_fragment(0, mmap_used.first);
1673:                if (mmap_used.second != 0) {
1674:                    mapping->unmap_fragment(mmap_used.second, mapping->size());

=== gemma3n: is the big PLE table forced to CPU? ===
```

</details>

## Call 31

```bash
cd /home/alexbryan/dev/commonwealth-ai/vendor/llama-cpp-sys-4/llama.cpp; echo "=== load_all_data host/mmap branch ==="; grep -n -B3 -A12 "if (buf_mmap && cur->data == nullptr)" src/llama-model-loader.cpp | head -30
```

<details><summary>output</summary>

```
=== load_all_data host/mmap branch ===
1546-            }
1547-
1548-            GGML_ASSERT(buf_mmap || cur->data); // either we have a buffer to allocate the tensor in, or it is already allocated
1549:            if (buf_mmap && cur->data == nullptr) {
1550-                ggml_backend_tensor_alloc(buf_mmap, cur, data);
1551-                if (lmlocks) {
1552-                    const auto & lmlock = lmlocks->at(weight->idx);
1553-                    lmlock->grow_to(weight->offs + n_size);
1554-                }
1555-
1556-                auto & mmap_used = mmaps_used[weight->idx];
1557-                mmap_used.first  = std::min(mmap_used.first,  weight->offs);
1558-                mmap_used.second = std::max(mmap_used.second, weight->offs + n_size);
1559-            } else {
1560-                ggml_backend_tensor_set(cur, data, 0, n_size);
1561-            }
```

</details>

## Call 32

```bash
cd /home/alexbryan/dev/commonwealth-ai; echo "=== binding exposes buft override? ==="; grep -rn "tensor_buft_override\|buft_override" vendor/llama-cpp-4/src/ 2>/dev/null | head -10; echo "(binding hits above)"; echo; echo "=== our daemon/inference side ==="; grep -rn "buft_override\|override_tensor\|\-\-override-tensor\|n_cpu_moe\|cpu_moe" --include=*.rs sovereign/crates/ 2>/dev/null | grep -v target | head -10; echo; echo "=== what model params DO we set? ==="; grep -rn "LlamaModelParams\|with_n_gpu_layers\|with_tensor_split\|with_use_mmap" --include=*.rs sovereign/crates/sovereign-inference/src/ 2>/dev/null | head -12
```

<details><summary>output</summary>

```
=== binding exposes buft override? ===
vendor/llama-cpp-4/src/fit.rs:68:use crate::{max_devices, max_tensor_buft_overrides};
vendor/llama-cpp-4/src/fit.rs:298:    pub(crate) tensor_buft_overrides: Vec<llama_cpp_sys_4::llama_model_tensor_buft_override>,
vendor/llama-cpp-4/src/fit.rs:354:    let ntbo = max_tensor_buft_overrides();
vendor/llama-cpp-4/src/fit.rs:355:    let mut tensor_buft_overrides = vec![
vendor/llama-cpp-4/src/fit.rs:356:        llama_cpp_sys_4::llama_model_tensor_buft_override {
vendor/llama-cpp-4/src/fit.rs:370:    model_params.params.tensor_buft_overrides = tensor_buft_overrides.as_mut_ptr();
vendor/llama-cpp-4/src/fit.rs:380:            tensor_buft_overrides.as_mut_ptr(),
vendor/llama-cpp-4/src/fit.rs:390:            model_params.params.tensor_buft_overrides = tensor_buft_overrides.as_mut_ptr();
vendor/llama-cpp-4/src/fit.rs:395:                tensor_buft_overrides,
vendor/llama-cpp-4/src/lib.rs:490:pub fn max_tensor_buft_overrides() -> usize {
(binding hits above)

=== our daemon/inference side ===
sovereign/crates/sovereign-inference/src/embedded/rpc_warm_cache.rs:573:/// llama.cpp's split — we ENFORCE this assignment via `tensor_buft_overrides` at
sovereign/crates/sovereign-inference/src/embedded/model_slot.rs:1437:                    .with_tensor_buft_overrides(&dist.overrides)

=== what model params DO we set? ===
sovereign/crates/sovereign-inference/src/smoketest.rs:48:use crate::llama::cpp::model::params::LlamaModelParams;
sovereign/crates/sovereign-inference/src/smoketest.rs:148:    let model_params = LlamaModelParams::default().with_n_gpu_layers(args.n_gpu_layers);
sovereign/crates/sovereign-inference/src/embedded/sampler.rs:21:use crate::llama::cpp::model::params::LlamaModelParams;
sovereign/crates/sovereign-inference/src/embedded/rerank_slot.rs:21:use crate::llama::cpp::model::params::LlamaModelParams;
sovereign/crates/sovereign-inference/src/embedded/rerank_slot.rs:190:        let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_gpu_layers);
sovereign/crates/sovereign-inference/src/embedded/grammar.rs:21:use crate::llama::cpp::model::params::LlamaModelParams;
sovereign/crates/sovereign-inference/src/embedded/prompt_helpers.rs:21:use crate::llama::cpp::model::params::LlamaModelParams;
sovereign/crates/sovereign-inference/src/embedded/embed_slot.rs:21:use crate::llama::cpp::model::params::LlamaModelParams;
sovereign/crates/sovereign-inference/src/embedded/embed_slot.rs:113:        let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_gpu_layers);
sovereign/crates/sovereign-inference/src/embedded/rpc_distribution.rs:29:use crate::llama::cpp::model::params::LlamaModelParams;
sovereign/crates/sovereign-inference/src/embedded/rpc_distribution.rs:886:    let mparams = LlamaModelParams::default().with_n_gpu_layers(999);
sovereign/crates/sovereign-inference/src/embedded/engine.rs:21:use crate::llama::cpp::model::params::LlamaModelParams;
```

</details>

## Call 33

```bash
S=/tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram
cat > $S/RESULTS.md <<'EOF'
# Engram hot-set experiment — results (2026-08-26, RuggedFox)

Corpora: 54.1M tokens, real Qwen3.8 vocab (max_id 248,069) via llama-tokenize
--vocab-only on Qwen3.8-27B-UD-Q6_K_XL.gguf.
  sep_mine 14,497,455 | sep_holdout 14,301,769 | repo_md 15,855,182 | rust_src 9,431,903

## 1. Static hot-set coverage (mined sep_mine, K = rows/head, mean of 2 seeds)
kind     eval          ceiling   @100k   @1M    @2M    @8M
bigram   sep_holdout   0.907     0.702   0.877  0.907  0.907
bigram   repo_md       0.467     0.285   0.417  0.467  0.467
bigram   rust_src      0.272     0.122   0.230  0.272  0.272
trigram  sep_holdout   0.706     0.322   0.513  0.580  0.706
trigram  repo_md       0.400     0.101   0.182  0.237  0.400
trigram  rust_src      0.317     0.029   0.087  0.143  0.317

Hot-set bytes = 16 heads x K x 170 B (Q8_0).  K=1M -> 2.72 GB.  K=2M -> 5.44 GB.
At 2.72 GB, held-out hit rate = (0.877+0.513)/2 = 0.695.

VERDICT vs pre-registered bars: RED. 95% held-out is unreachable at ANY K.

## 2. Ceiling is mining-budget-limited and grows logarithmically
mine toks   bigram ceil   trigram ceil   uniq trigram rows
   500,000        0.644          0.288             339,276
 1,000,000        0.709          0.354             619,095
 2,000,000        0.779          0.436           1,158,679
 4,000,000        0.830          0.520           1,995,067
 8,000,000        0.876          0.619           3,517,373
14,497,455        0.907          0.706           5,444,616

Heaps fit on trigram rows: beta ~= 0.824. Occupancy 1-exp(-distinct/20M).
5.44M occupied <=> ~6.35M distinct trigrams from 14.5M tokens.
90% occupancy (46M distinct) reached at ~1.6e8 tokens.
=> After ~160M tokens of ordinary text, 90% of the 20M trigram rows are live.
   The model saw trillions. The table is DENSE-in-use; there is no cold part.

## 3. NULL (pre-registered): shuffled tokens, unigrams preserved
kind      uniq rows mined   @1M rows   ceiling
bigram    4,336,904         0.692      0.808   (real: 1,765,635 / 0.877 / 0.907)
trigram   9,203,745         0.167      0.564   (real: 5,444,616 / 0.513 / 0.706)
=> Concentration is genuine n-gram structure, not a unigram artifact. PASS.

## 4. NVMe 16-way concurrent gather (the real access pattern), 85.6 GiB file
                    p50        p90        p99
COLD (fadvised)   376.3 us   533.5 us   710.8 us
WARM (cached)     284.0 us   445.6 us   526.1 us
Marginal I/O = 376-284 = ~92 us. The 284 us floor is Python pool dispatch,
not device time; a Rust/io_uring implementation pays less.

Against our own measured transport envelope (note qwen122b-iroh-transport-
characterization): iroh DIRECT 16KB rt p50 13.3 ms; RELAY 141-182 ms.
NVMe beats iroh-direct by ~35x on marginal cost, ~140x vs relay.

## 5. Quantization constraint (verified in ggml source)
Row width is 160. QK_K = 256 (ggml-common.h:89); legacy blocks are 32
(QK4_0/QK5_0/QK8_0). ggml.c:1341 asserts ne % ggml_blck_size(type) == 0.
160 = 5x32 but 160 % 256 != 0  =>  K-quants are STRUCTURALLY UNAVAILABLE for
the engram tensor. Only legacy 32-blocks (or a new row-aligned type) apply.
  row bytes: F16 320 | Q8_0 170 | Q5_0 110 | Q4_0 90
  table:     F16 102.4 GB | Q8_0 54.4 GB | Q5_0 35.2 GB | Q4_0 28.8 GB
  per-token gather (16 rows): F16 5,120 B | Q8_0 2,720 B | Q4_0 1,440 B
Disk free on this host: 580 GB. => the engram never needs quantizing at all.
EOF
echo "recorded"; ls -la $S/*.md $S/*.json $S/*.py 2>/dev/null | awk '{print $5, $NF}'
```

<details><summary>output</summary>

```
recorded
1909 /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/curves.json
3893 /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/engram_sim.py
2012 /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/nvme_gather.py
1931 /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/PRE-REGISTRATION.md
3369 /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/RESULTS.md
2265 /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/scaling.py
540 /tmp/claude-1000/-home-alexbryan-dev-commonwealth-ai/6fe59b24-9117-4b7e-9a35-d2a3badf92db/scratchpad/engram/to_npy.py
```

</details>
