# Distributed inference — pilot readiness (P0/P1 requirements)

**Status:** living requirements doc, opened 2026-07-19 during the 122B validation run.
**Goal:** a semitechnical user can run a can't-fit-one-box model across two of their own
machines with **one config section, no environment variables, no supervision by a human,
and legible failure states**. The 122B pilot is the acceptance test.

**Context docs:** `RPC_DISTRIBUTED_INFERENCE.md` (architecture + supervision contract),
`QWEN122B_DISTRIBUTED_HANDOFF.md` (the investigation that got us here).
**Verdict as of opening:** the distribution *engine* (discovery → eligibility →
auto-warm → plan-agreement placement → never-wedge fallback) is validated; the
*operational shell* is not pilot-grade. Every P0 below traces to a live failure we hit.

---

## P0 — blockers for an unsupervised pilot

### P0.1 Split-GGUF support in the warm plane
**Found live 2026-07-19:** the byte-range warm is split-naive end to end. `build_manifest`
reads only the file the config names — for a `-00001-of-00003.gguf` that is an ~11MB
header shard — so a placed worker got an **empty tensor list and the warm reported
success** (`written=0 already=0`). The load then proceeded as if warm and bulk-streamed
the worker's ~22GB share over ggml RPC: the >800MB upload deadlock, resurrected at 92GB
scale by a different door. Split GGUFs are the *norm* for the models this feature
targets — a pilot user's first big download is a split.

Requirements:
- **R1 (landed 2026-07-19):** an assigned worker whose manifest slice is empty MUST fail
  the warm (→ local-only fallback), never silently succeed. Guard lives in
  `orchestrate_warm`; regression test required.
- **R2:** either (a) wire extension — `TensorRange` gains per-tensor source-file
  attribution (serde-default to `model_id` for rolling back-compat), `build_manifest`
  merges all `-NNNNN-of-NNNNN` siblings with per-file offsets, host sends per-shard
  fetch URLs, worker groups ranges by file — **or** (b) an automatic merge step at
  model-install time (`llama-gguf-split --merge`, ~2min for 92GB) so the daemon only
  ever sees single files. (b) is less code and pushes complexity out of the wire;
  it costs 2× disk transiently. Decide when the pilot config-generator lands (P0.3).
- **R3:** `resolve_whole_gguf` / `fetch_named_model_from_peer` and
  `warm_cache_for_device`'s GGUF reader have the same gap for the whole-GGUF path —
  same fix decision applies.
- **Acceptance:** a 3-file split model distributes with zero bulk weight bytes streamed
  (worker RSS ≈ its shard, `already=N` on re-warm), or refuses cleanly to local-only.

### P0.2 Eager-fast-alias OOM (can't-fit-one-box boot)
With no distinct `fast` configured, `fast_path == primary_path` and the **fast slot
eagerly loads the full model 100% local at boot**; the later distributed reload then
overlaps with the alias-held weights (fast holds the `Arc<LlamaModel>`, so dropping
primary frees nothing) → OOM'd a 125GB box at 123GB in the July run. Note
`reload_primary` already frees primary before reloading — the alias is the leak.

Requirements:
- **R1:** boot-time guard: if `fast_path == primary_path` and
  `total_model_bytes(primary)` exceeds a sane alias ceiling (default ~40% of system
  RAM, env-overridable), **refuse to start with an actionable error** ("set
  `models.fast` to a small model — this model is too large to double-load"). Turning a
  silent OOM into a sentence a semitechnical user can act on is the P0; making `fast`
  optional in the engine is larger surgery the config generator makes unnecessary.
- **R2:** the pilot config generator (P0.3) always writes a distinct small `fast` (or a
  future no-fast mode) so users never construct the aliased-huge case by hand.
- **Acceptance:** booting a 122B-as-primary config with no distinct fast produces the
  error message, not an OOM.

### P0.3 Config collapse — `[shared_model]`
Today's flow needs ~6 env vars on two machines, a hex node id extracted from
`mesh.json`, and hand TOML surgery. Requirements:
- One config section, e.g. `[shared_model] model = "<path-or-id>"` (+ optional
  `role = "auto"`). Everything else derives: host election already exists
  (`can_anchor`/`should_host`) so `SOVEREIGN_SHARED_MODEL_HOST_NODE_ID` becomes
  "self if elected"; `SOVEREIGN_RPC_SERVE` defaults on for anchors;
  `SOVEREIGN_RPC_DISCOVER` defaults on when the section is present;
  `SHARD_FETCH=ranges` becomes the default once P0.1 lands.
- The generator writes the distinct-fast workaround (P0.2 R2) until fast-less mode exists.
- **Acceptance:** two fresh machines reach a distributed load with one `[shared_model]`
  section each and zero env vars.

### P0.4 Supervision by default
Three **uncatchable** ggml-rpc abort faces are now documented (`:491` mid-compute,
`:379` teardown, `:337` session-death mid-exchange, all SIGABRT the host): any worker
blip while a model is sharded across it can kill the daemon. In-process rescue was
evaluated and rejected (all asserts in void paths). Requirements:
- The supervised unit (systemd user unit / launchd; `Restart=on-failure`) is the ONE
  documented and default-installed way to run a daemon participating in shared models.
- Post-abort recovery must be automatic AND fast: warm cache makes the reload cheap;
  measure 122B post-abort recovery time in the validation run.
- The abort window on worker disappearance (≤1 discovery tick) gets the
  **shrink-fast-prune** mitigation: prune-on-disappear skips the 20s STABLE debounce
  (grows keep it). Reduces, does not eliminate — supervision is the backstop.
- **Acceptance:** `kill -9` of the worker daemon mid-inference on the host = host
  serves again (local-only or re-distributed) within N minutes with no human action.

### P0.5 Mesh heal after dual restart
**Found live 2026-07-19:** when both peers' daemons restart while apart, they never
re-converge — boot-time gossip push fires once against a down peer, the periodic loop
skips offline members, and mDNS doesn't cross AP-isolated Wi-Fi. Both endpoints were
provably relay-dialable by key the whole time. A power blip that reboots both machines
strands the pilot permanently. Requirements:
- Anti-entropy tick periodically re-attempts **offline** members via stored contact
  (pubkey + relay_url makes every member permanently dialable; stale ephemeral direct
  ports don't matter) with backoff.
- Refresh gossiped WAN direct addrs on change (observed overnight WAN churn).
- Surface gossip-dial outcomes in the default tracing filter (they were dark under
  `target: "transport"` — diagnosis was blind).
- **Acceptance:** kill both daemons, restart both in any order while mDNS is blocked →
  mesh re-forms within one anti-entropy period.

### P0.6 Degraded-state legibility (UI)
`/status` already carries the placement object (`mode`, blocks, workers,
`holds_output`). The desktop must render it: "distributed across 2 machines /
running locally (worker offline, recovering) / worker quarantined (flapping)".
A pilot user watching a silent local-only fallback currently sees only a slower model.
- **Acceptance:** every placement transition visible in the UI within one status poll.

## P1 — pilot quality
- **Wire-version handshake** on the warm POST: exchange the vendored llama-cpp tree
  hash; refuse distribution on mismatch with a clear message (ggml RPC is
  version-sensitive; today a mismatch fails undefined).
- **Placement observability++**: per-worker path grade (direct/relay) at bridge-mint
  time surfaced in `mesh status` — with the caveat (measured 2026-07-19) that iroh
  `remote_info` path classification is NOT trustworthy as a data-path witness; the
  RTT signature is. Consider an active per-boundary RTT probe instead.
- **Relay-quality placement policy**: warn (not refuse, until a metric justifies more)
  when the only path to a would-be worker is relay-grade — measured floor is
  single-digit tok/s (5.8–7.1 network ceiling), a mysterious experience if silent.
- **122B-scale warm UX**: the ~55min first warm needs progress surfaced (bytes/percent
  in `/status` + UI), else it reads as a hang.

## Forward design — intelligent tensor splitting for large MoEs (GLM-5.2-class)

Current splitter (`plan_shards`, rpc_warm_cache.rs:460): contiguous block ranges,
apportioned by free-VRAM weights via largest-remainder, `-ot` regex pins
`^blk\.N\.` per device, `token_embd` → host, output head → host (measured: the
600KB-logits return leg caps decode at ~12 tok/s even on LAN, so the head stays home).
This is correct for dense models and adequate for tonight's 122B. For
GLM-5.2 / DeepSeek-class MoEs it leaves real capability on the table:

1. **Mass-aware, not layer-count-aware apportionment.** Contiguity by *layer count*
   assumes uniform layer mass. MoE layers are dominated by expert tensors
   (`blk.N.ffn_*_exps.*`), and some models interleave dense/MoE layers — the manifest
   already knows per-tensor bytes, so `plan_shards` should apportion by **cumulative
   byte mass** over the block sequence, not block count. Cheap, uses data we already
   compute, keeps contiguity (pipeline latency profile unchanged).
2. **Pipeline split beats expert split for decode latency.** Splitting *experts within
   a layer* across machines would send activations over the network **every MoE layer,
   both directions** — per-token network cost multiplies by layer count vs one
   boundary-crossing per contiguous cut. On our measured envelope (10.9–13.3ms/16KB
   round-trip LAN), expert-split decode would be single-digit tok/s even locally
   networked. Expert-granular placement only wins when a *single layer's* experts
   exceed a device — not the case for any current open model on 64–128GB nodes.
   Keep contiguous cuts as the default policy; document why.
3. **Sparse activation changes what workers contribute.** An A10B-active MoE reads
   ~2 experts/token — workers are **RAM contributors more than FLOP contributors**,
   and the compute skew means the host (with the output head + tokenizer + router)
   should keep proportionally more layers than its VRAM share suggests. The
   apportionment weight should eventually be `min(vram, bandwidth_score)` — a
   Wi-Fi-attached worker's marginal value saturates well below its VRAM.
4. **Boundary placement matters more for MoE.** The activation crossing the cut is the
   dense hidden state (~16KB at 4B, ~30-50KB at 122B-class hidden dims) regardless of
   expert mass — so cuts are cheap *anywhere*, and the optimizer is free to place them
   purely by mass balance. Verify hidden-state size per model from the graph rather
   than assuming (bench `act-16KB` was calibrated on small models; re-measure at 122B).
5. **Warm-plane consequence:** expert tensors are individually large (well over the
   10.49MB cache threshold), so the content-addressed cache and byte-range warm work
   *better* for MoEs — but per-tensor file attribution (P0.1 R2) becomes mandatory
   because big MoEs ship as many-file splits universally.

None of (1)–(4) blocks the pilot; (1) is the first worth building (small, measurable
via load-balance skew on the next big-model validation).

## Validation log
- **2026-07-19:** 122B run 1: split-GGUF empty-warm → killed pre-wedge (P0.1 found).
  4B E2E same day: tunnel 39.6–41.0 tok/s ≈ direct 40.0–41.0 (tunnel tax invisible);
  auto-mode picks direct-ip; relay floor daemon-grade NOT measurable on a LAN
  (hole-punch migration defeats pinning — see transport memory; floor stands at
  5.8–7.1 tok/s from bench).
- **2026-07-19 — 122B VALIDATION COMPLETE (run 2, merged GGUF).** Full chain
  end-to-end with zero manual intervention after launch: manifest 3m43s → Mac
  range-fetched its ~22GB shard in ~96min over Wi-Fi (`written=31 already=38` —
  38 tensors reused from run 1's aborted bulk-stream, cached by the worker's
  rpc-server: the content-addressed cache turned a doomed load into progress) →
  distributed `-ot` load in **4m14s** post-warm → placement 36 local / 12 worker
  blocks, output head home. **Decode: 17.3 / 17.9 / 17.8 tok/s — BEATS the 14.8
  tok/s solo baseline by ~20%.** Interpretation: the solo 122B is
  memory-bandwidth-bound; offloading 25% of layers cuts the host's per-token
  weight reads by more than the ~11ms/token network tax costs. For sparse MoEs on
  bandwidth-bound hosts, workers are throughput contributors, not just RAM — a
  distribution-positive regime the MoE-splitting design should exploit
  (mass/bandwidth-aware apportionment has real headroom here). Host RSS stayed
  ~3.6GB resident (mmap page-cache holds weights); no OOM pressure with the
  tiny-fast workaround. Priming call 56s (one-time page-in of ~66GB local share).
  NOT yet validated (deliberately deferred to the pilot phase, needs supervision
  in place first): post-abort automatic recovery timing (P0.4 acceptance).
