# Distributed inference — pilot readiness + production roadmap

**Status:** living doc, opened 2026-07-19 during the 122B validation run; grown into
the production roadmap after the pilot held. Read order: **P0/P1** = the two-node
122B pilot baseline (largely landed); **"Production roadmap"** (5 root primitives P1–P5) =
the path to GLM-5.2 on a semi-technical mesh; **"Forward design"** = the MoE-splitting deep-dive
M4 references; **Validation log** = what's actually been run.
**Goal:** a semitechnical user can run a can't-fit-one-box model across two+ of their own
machines with **one config section, no environment variables, no supervision by a human,
and legible failure states**. The 122B pilot is the acceptance test; **GLM-5.2 on a
mesh of semi-technical users is the destination.**

**Context docs:** `RPC_DISTRIBUTED_INFERENCE.md` (architecture + supervision contract),
`QWEN122B_DISTRIBUTED_HANDOFF.md` (the investigation that got us here).
**Verdict as of opening:** the distribution *engine* (discovery → eligibility →
auto-warm → plan-agreement placement → never-wedge fallback) is validated; the
*operational shell* is not pilot-grade. Every P0 below traces to a live failure we hit.

---

> **Sweep status (2026-07-19 P0 sweep):** P0.1 **BUILT** (split-aware manifest +
> per-tensor `file_idx`/`file_urls` on the wire + split-aware whole-GGUF fetch +
> tests; live split acceptance pending). P0.2 **BUILT** (boot guard + tests).
> P0.3 **PRE-EXISTED** — `[shared_model] role/model_id` with election, quorum,
> and full env derivation was already in-tree (bootstrap.rs
> `apply_shared_model_role_to_env`); the sweep's contribution is knowing it and
> the config-only acceptance run. P0.4 **PRE-EXISTED** — shrink-fast-prune is
> live in the discovery loop, and the systemd/launchd units already carry
> `Restart=on-failure` + crash-loop budget; remaining: docs stance. P0.5
> **BUILT** (cached-bridge dial-info refresh — the frozen-target bug — plus the
> `transport=info` filter token; dual-restart acceptance needs both machines).
> P0.6 **BUILT** (`mesh_get_placement` command + placement chips in
> MeshDiagnosticsPanel; svelte-check clean).

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
- **Probe robustness vs the anchor eligibility profile — FIXED 2026-07-19**
  (root-caused from the acceptance3 e2e log, then fixed the same day). The
  observed failure was sharper than "quarantine on flap": `discover_rpc_workers`
  chose an endpoint per tick with **no memory**, and `reachable_rpc_endpoint` was
  a single 600ms TCP connect per IP. One congested-Wi-Fi miss returned `None`, so
  discovery fell back to the **iroh-bridge loopback** endpoint — a *different
  string* from the direct-ip. Because worker identity was the endpoint STRING
  end-to-end (`WorkerEligibility` map key + the reload loop's `Vec<String>`
  diff), the same peer under a new address read as *old worker gone (flap) + new
  worker appeared (re-settle from zero)*; the eligible set emptied and the host
  reloaded to local-only mid-inference. Relaxed knobs (`SETTLE=20 FLAP=3`) did
  NOT help — proving this was an identity-flip bug, not a threshold bug. **Fix
  (both landed):** (A) endpoint **stickiness/hysteresis** in `discover_rpc_workers`
  — a proven direct-ip is held through up to `SOVEREIGN_RPC_ENDPOINT_FLIP_THRESHOLD`
  (default 3) consecutive misses before flipping transport (pure `sticky_endpoint`
  fn, unit-tested); (B) **node-id identity** — `WorkerEligibility` now keys by the
  mesh `NodeId` with the endpoint as a mutable attribute, so an address change is
  never a flap (`DiscoveredWorker`; regression test
  `address_change_for_same_node_is_not_a_flap`). Stickiness is the load-bearing
  fix for *this* failure; node-id keying is the correctness hardening that
  prevents the whole class. Both are in `sovereign-mesh` (`daemon.rs`,
  `worker_eligibility.rs`); the bootstrap discovery loop was unchanged.
- **Stale iroh-bridge dial target (OPEN, P0.5-family, isolated 2026-07-19).**
  Distinct from the flap above and exposed by it: once acceptance3 flipped
  BeefyMac onto the bridge, the bridge dialed node id `161a8706…` and timed out
  on *every* attempt (~13s apart, sustained) while gossip to the peer's *live*
  identity `86627fd5` (`node-b88252e4…`) stayed healthy the whole time. So the
  bridge held a frozen/wrong node identity for the peer's RPC endpoint even
  though a good identity was reachable. The stickiness fix keeps us on direct-ip,
  so this stops being pilot-blocking — but the cached-bridge dial-info can still
  freeze onto a dead identity, which is exactly the frozen-target class P0.5's
  "refresh gossiped WAN direct addrs on change" targets. Fix direction: on a
  bridge dial timeout, re-resolve the peer's dial-info from live gossip (drop the
  cached NodeAddr) rather than retrying the frozen one. Needs both machines to
  reproduce; the acceptance3 log is the witness.
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

## Production roadmap — toward GLM-5.2 on a semi-technical mesh

**Opened 2026-07-19**, after the worker-flap fix made the 2-node 122B pilot HOLD
through sustained inference (see the flap entry in the validation log). The P0/P1
sections above are the *pilot* baseline; this section is the path from "the pilot
holds on two of my boxes" to "a mesh of semi-technical users runs GLM-5.2-class
models unattended." Grounded in a 2026-07-19 code audit (three agents mapping the
split engine, the UX surface, and the transport/recovery layer) — every gap below
carries its file:line so the work is scoped, not aspirational.

**Reframed 2026-07-19** from a symptom list into root primitives after an
architecture pass. The pilot P0/P1 above is the baseline; this is the path to
GLM-5.2-class models run unattended by semi-technical users.

### The thesis: one missing abstraction, not thirteen polish items
The distribution *mechanism* is more done than it looks — the splitter is genuinely
N-node (`plan_shards` over any device count, rpc_warm_cache.rs:531), per-tensor
multi-file attribution is on the wire (rpc_warm_http.rs:175), and the flap is fixed.
What's missing is a **first-class, supervised, observable "distributed inference
session."** Today distribution is an *emergent side-effect* of several decoupled
loops (discovery, eligibility, reload, warm POST, placement classifier) writing to
tracing logs and a `/status` snapshot, with the fragile ggml compute **fused into
the critical daemon process** (inference is an in-process `complete(CompletionRequest)`
call wired to `/v1/chat/completions`, engine.rs:303, daemon.rs:2298). Model the
session and whole categories of "polish" dissolve.

### Four recurring shapes (why the symptom list under-serves)
1. **Failure is a crash, not a value.** ggml `SIGABRT` on worker-loss / version
   mismatch kills the whole daemon (uncatchable; in-process rescue evaluated &
   rejected, P0.4). → supervision-by-default, connection-driven recovery, version guard.
2. **Liveness/identity from the wrong, inconsistent signal.** Endpoint-string
   identity (the flap), frozen dial target (stale-bridge), 15s-tick "dead worker"
   inference, ABI discovered at abort time — several noisy signals, each failing
   under the exact condition that matters (load, congestion, churn).
3. **Rich state never reaches the human.** Actionable messages → logs; forming(k/N)
   / quarantine / holds_output → `/status` but unrendered; warm progress → not
   modeled; path-grade → classified but never surfaced.
4. **Config is imperative env-var wiring, not declared intent.** `[shared_model]`
   exists but is translated to env vars at bootstrap; no generator; the desktop has
   no model of "join a shared model."
   (A fifth, scale-only shape: **placement from an incomplete cost model** — VRAM-only.)

### The five root primitives
**P1 — Compute-slot process boundary.** Run the distributed primary's compute as a
supervised **child process** (the existing `sovereign-server` binary + the `/v1` API
is the seam); the daemon keeps gossip, `/status`, the client API, the desktop bridge.
A ggml abort kills the *child*; the daemon observes the exit as an **event** and
re-plans (local-only / re-distribute). *Dissolves shape 1 wholesale.* **High leverage,
high cost** — reshapes the daemon↔engine split; needs a feasibility spike (KV/streaming
across the boundary; the in-process fast/embed slots). The load-bearing fork.

**P2 — Worker-session liveness/identity authority.** One component owns "is peer X a
usable worker, at what address + ABI," fusing signals by precedence: **connection-health
(from P1) > gossip > probe (discovery only)**, event-driven not poll-driven.
*Generalizes tonight's flap fix from a special case into a principle.* Cost: medium —
mostly consolidating signals that already exist.

**P3 — Event-sourced Distribution Session state.** A typed lifecycle
(`discovering → forming(k/N) → warming(%) → serving(N machines) → degraded(reason) →
fallback(reason, action)`) **streamed** to every surface, each state carrying its
human-facing reason + action. *The glassbox principle made structural.* Cost: medium —
the model + a stream endpoint; surfaces render it.

**P4 — Declarative shared-model intent.** `[shared_model]` (or a desktop toggle) is
the *single* source of truth; env vars demote to debug overrides; the daemon derives
role/serve/discover/fetch/ABI at runtime. Cost: low-medium.

**P5 — Complete placement cost model** (the scale root). Placement consumes a cost
**vector** (vram, byte-mass, path-grade/bandwidth) through one pluggable objective,
not a single VRAM scalar. Mass-aware and bandwidth-aware become one change; expert-split
becomes a *policy inside* the optimizer. Cost: medium. Design in "Forward design" below.

### How they compose (one subsystem, not five patches)
```
P1 child-exit / RPC error ─► P2 liveness authority ─► P3 observable session ─► surfaces (desktop/CLI/logs)
       (the event)                 (the truth)              (the render source)
P4 declares the session   ·   P5 plans it
```
The process boundary (P1) produces the authoritative liveness event, which feeds the
one liveness truth (P2), which feeds the one observable state (P3) the human sees.

### What each polish item maps to (nothing lost from the audit)
| Polish item (was M1–M4) | Primitive | Key file:line | State |
|---|---|---|---|
| Flap fix (commit) | P2 (already an instance) | daemon.rs `discover_rpc_workers`, worker_eligibility.rs | done, UNCOMMITTED |
| Version/ABI handshake | P2 (ABI∈identity) + P1 (caught crash) | rpc_warm_http.rs:220; advert daemon.rs:2014 | missing |
| Stale iroh-bridge self-heal | P2 (re-resolve on conn event) | iroh.rs:406 (dial-fail logs only), :635 | partial/open |
| Supervision by default | P1 (self-supervised child) | svrnmesh.service:23; setup_cmd/finish.rs:60 | partial (opt-in) |
| Connection-driven recovery | P1 + P2 (child-exit = the event) | rpc_distribution.rs:293 (15s tick) | missing |
| Dual-restart heal (mDNS) | P2 (reconcile discovery signals) | gossip.rs:291; daemon.rs:2554 (browse dropped) | partial |
| Config flow / kill env vars | P4 + desktop | bootstrap.rs:283; config_setup.rs:186 | missing |
| Warm progress | P3 (warming is a state w/ %) | rpc_warm_http.rs:236 (final totals only) | missing |
| Placement/recovery legibility | P3 (render the state) | MeshDiagnosticsPanel.svelte:78; worker_eligibility.rs | partial |
| Actionable errors to user | P3 (reason is a field, not a log) | rpc_distribution.rs:823/837/885 | log-only |
| Relay-quality warning | P3 (annotation) + P2 (path-grade signal) | iroh_access.rs:192 | missing |
| Mass-aware apportionment | P5 | rpc_warm_cache.rs:543; rpc_distribution.rs:571 | missing |
| Bandwidth-aware placement | P5 | rpc_distribution.rs:571 (VRAM-only) | missing |
| Within-layer expert split | P5 (policy, deferred) | rpc_warm_cache.rs:622 | deferred — measure first |
| 3+-box validation | exercise, not code | engine N-node rpc_warm_cache.rs:531 | untested |
| 1-device-per-worker cap | P5-adjacent | rpc_distribution.rs:505 | minor |

### Sequencing (decided 2026-07-19: robustness-first, mass-aware-only, desktop-is-surface)
1. **P2 + P3 first** — medium-cost, low-risk, high-leverage: consolidate scattered
   liveness + state logic into two clean models and dissolve most robustness +
   legibility items *without* the P1 process-boundary risk. P2 also carries the
   ABI-in-identity that closes the version-mismatch class.
2. **Spike P1 in parallel** — the load-bearing fork (self-supervised compute child).
   Decide feasibility before committing; it turns the whole crash class into caught
   events and makes external supervision optional. If P1 lands, "supervision by
   default" and "connection-driven recovery" fall out of it.
3. **P4 with the desktop work** — the desktop is the semi-technical surface, so P4's
   intent model + P3's session render together become the desktop shared-model shell
   (setup flow + live placement/warm/recovery).
4. **P5 last** (GLM-5.2 scale) — scoped to the cost-vector (vram + byte-mass +
   bandwidth); within-layer expert split stays deferred behind the layer-fit
   measurement. Validate a real 3-box split, confirming the memory-share property per
   node (proven for 2 boxes 2026-07-19: host 64.7GB = 36/48, worker ≈ 22GB = 12/48).

### Cross-cutting — throughput is the network; set expectations honestly
Decode is Wi-Fi-bound: 17.5 tok/s on a good direct-ip link, ~10 on congested Wi-Fi,
single-digit on relay (bench floor 5.8–7.1). Keep the output head home (done — the
~600KB logits return leg caps decode), guide wired-first, and let P5's bandwidth-aware
placement + P3's relay annotation make the ceiling legible rather than mysterious. For a
model that can't fit one box, distributed throughput is the *only* throughput — the
memory-share property (each node holds only its shard) is what makes GLM-5.2 runnable at
all.

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
- **2026-07-19 — P0 SWEEP ACCEPTANCE (split GGUF + config-derived, PASSED).**
  The 3-file split 122B distributed end-to-end on the sweep binary: split
  manifest walked all three shards (3m43s — identical to the merged-file hash,
  as it must be), warm `written=0 already=69` — **every tensor cache-HIT
  against blobs warmed from the MERGED file** (content addressing across file
  layouts, zero network bytes), placement 36/12, reload 5m47s total, decode
  16.8–17.6 tok/s ≈ the merged-file run. All RPC/discovery/shard config came
  from `[shared_model] role="host"` (SERVE/DISCOVER/ranges/model-id derived;
  host ELECTED, no pin). Two caveats recorded: (1) eligibility used env-relaxed
  settle/flap (20s/3) — the anchor profile (settle 300s, flap threshold 1)
  livelocked admission on congested Wi-Fi via probe timeouts (the P1
  probe-robustness item is therefore REQUIRED for unattended pilot admission);
  (2) an earlier attempt ran a stale binary and the empty-warm guard correctly
  refused → local-only — the guard's first live save, and a reminder that
  deploy hygiene (rebuild the bin, not just check) belongs in the pilot
  runbook.
  - **CORRECTION 2026-07-19 (log re-audit): the split-path `decode 16.8–17.6
    tok/s` above is NOT corroborated by any acceptance daemon log and almost
    certainly did not happen.** `e2e-acceptance3.log` (the run this entry
    describes — warm `already=69`, placement 36/12, reload `latency_ms=347649`
    = the "5m47s") shows the distributed slot became ready at 19:55:07 and the
    endpoint-flip collapsed it to local at 19:55:24 — a **17-second** functional
    window, and priming alone is ~56s, so no token could be generated. There is
    **no priming/token/decode line anywhere** in the log. Worse, the worker's
    RPC-tensor bridge (`161a8706…`) was **failing every dial** from 19:53:13
    through the collapse, so the "distributed" slot never had a live worker
    connection — the warm's `already=69` succeeded over a DIFFERENT, working
    bridge identity (`86627fd5`) at 19:53:03, the one good bridge use before the
    stale-target bug set in. `e2e-acceptance.log` and `e2e-acceptance2.log` never
    reached a distributed placement at all (acceptance2 decided `mode=local`). So
    the warm + placement + reload facts stand, but **the split-GGUF distributed
    DECODE is unverified** — it still needs a clean measurement after the flap
    fix. (The 16.8–17.6 figure most likely carried over from the merged-file run
    or the "≈ merged" assumption in the note.)
- **2026-07-19 — FLAP ROOT-CAUSED + FIXED (worker-identity dedup + endpoint
  stickiness).** The endpoint-flip that collapsed acceptance3 is a real
  pilot-blocker, not a knob-tuning issue (the run already used relaxed
  `SETTLE=20 FLAP=3` and still collapsed). Root cause: `discover_rpc_workers`
  chose a transport per tick with no memory + a single-probe `reachable_rpc_endpoint`,
  and worker identity was the endpoint STRING end-to-end — so one 600ms TCP miss
  flipped direct-ip→bridge, which read as flap + full re-settle and emptied the
  eligible set. Fixed both layers in `sovereign-mesh`: (A) `sticky_endpoint`
  hysteresis holds a proven direct-ip through `SOVEREIGN_RPC_ENDPOINT_FLIP_THRESHOLD`
  (default 3) misses; (B) `WorkerEligibility` now keys by `NodeId` with the
  endpoint a mutable attribute (`DiscoveredWorker`). Unit tests:
  `sticky_*` (daemon.rs) + `address_change_for_same_node_is_not_a_flap`
  (worker_eligibility.rs). Full-workspace lint clean; tests green. The stale
  bridge-target (`161a8706…`) is tracked separately as the P0.5-family item
  above. **Next: re-run the split e2e on the fixed binary to capture the still-
  missing split-path decode (P0.1 acceptance) and confirm the distribution
  HOLDS.**
