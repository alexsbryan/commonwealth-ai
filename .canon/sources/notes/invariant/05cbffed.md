# HOST MEMORY CEILING ROOT-CAUSED AND PARTLY FIXED (2026-08-25). Extends f2afc2cf ("the daemon is the designated OOM victim") with the actual…

HOST MEMORY CEILING ROOT-CAUSED AND PARTLY FIXED (2026-08-25). Extends f2afc2cf ("the daemon is the designated OOM victim") with the actual accounting. Six OOM kills in 24h (4 overnight + 09:08:14 and 09:17:07) each landed MID-JUDGE and cost a 7-minute judge run.

WHERE THE 125 GB WENT — all measured, nothing inferred.
- NOTHING LEAKED AT THE GPU LEVEL. Live DRM clients summed to 59.73 GB vs the kernel's mem_info_gtt_used 59.59 GB. The "orphaned GTT from OOM-killed daemons" hypothesis is DEAD; every byte was owned by a live process.
- Daemon before restart: 46.4 GB GTT + 33.4 GB anonymous host RAM (one rw-p 00:00 mapping, no inode) = ~80 GB.
- gnome-shell: 12.55 GB GTT. Not ours, 4-day uptime, a sixth of the box. Do NOT kill it — Wayland, it ends the operator's session.
- Swap (8 GB) fully exhausted before the kills.

THE 33.4 GB ANON IS AN ACCUMULATION, NOT A SECOND WEIGHT COPY. I first called it double residency; that was WRONG and the restart disproved it. A freshly restarted daemon serving the SAME three models sits at 3.7 GB RSS idle / 4.9 GB with the 27B loaded. The 38 GB was grown over a day of large-prefill judging and never released. Leak-shaped. Prime suspect: `ggml_vulkan: Failed to allocate pinned memory (Requested buffer size exceeds device buffer size limit: ErrorOutOfDeviceMemory)` — logged at 27B load AND again during each big judge prefill; the host-memory fallback appears not to be freed. NOT yet root-caused to a line. Next reader: this is the open bug.

MEASURED RESTART DELTA (the fix's magnitude):
  daemon RSS   38.0 GB -> 4.9 GB
  MemAvailable 19.1 GB -> 48.7 GB (83.6 GB with the 27B unloaded)

CONFIG FIX APPLIED (operator-approved): ~/.sovereign/config.toml `[daemon] extras_idle_secs = 0 -> 1800`. 0 means the idle monitor is NEVER SPAWNED (engine.rs:2447), so Fast+FastShort+Embed stayed resident forever through 27B-only research runs. Verified live in the journal: "extras idle monitor started idle_secs=1800". Backup: config.toml.bak-20260825-094647.

THE RSS HARD LIMIT CANNOT FIRE ON THIS HOST — do not "fix" this by setting it. memory_watch samples PROCESS RSS; `SOVEREIGN_RSS_HARD_LIMIT_MB=auto` resolves to 85% of RAM = ~108 GB. The kernel killed the daemon at anon-rss 36.4 GiB and 38.4 GiB. The exhausted resource is GTT, which RSS does not count. An explicit value is no better: steady state under judging was 36.0 GiB and the first kill was 36.4 GiB — no separation. A guard that cannot fire is worse than none (ARCH_PRINCIPLES §18.1). The real fix is a code change: watch MemAvailable / GTT, not process RSS.

KV COSTS, MEASURED. `[models] context_size = 65536` in ~/.sovereign/config.toml is ONE GLOBAL KNOB applied to primary AND fast. Extras (4B Fast + FastShort + Embed) = ~15.3 GB GTT of which only ~4.6 GB is weights — so ~10.7 GB is KV for COMPANION models. The 4B's 64k window costs more than the 4B does. UNEXPLAINED ASYMMETRY, do not design against it yet: the 27B showed only ~6.6 GB KV despite 2x the layers (likely lazy KV allocation after a single short request — confirm before relying on it).

WHAT IS FUNDAMENTAL vs OURS (operator asked). Fundamental: a KV cache belongs to a llama_context; llama.cpp has no cross-context arena, and cross-MODEL KV sharing is not expressible (shape is a function of layers/kv_heads/head_dim). Already right: Fast and FastShort share one Arc<LlamaModel> (model_slot.rs:101) so weights are paid once; and FastShort does NOT allocate per instance — it is ONE context, ONE KV, partitioned n_ctx 16384 / n_seq_max 8 = 2048 tokens per sequence (engine.rs:192-199). Ours to fix: per-slot context_size instead of one global; collapsing Fast+FastShort into one context via n_seq_max + with_kv_unified(true) (already used at rerank_slot.rs:237); SOVEREIGN_FAST_SHORT_DISABLE=1 is an existing escape hatch. KV-Q8 was tried 2026-05-17 and REVERTED on THROUGHPUT grounds (-9% dominant phase) when memory was not the binding constraint — worth revisiting as a PER-SLOT choice now that it is.

ALSO: hardware.rs:129 `detect_gpu()` returns `false, // discrete GPU — not unified memory` for EVERY non-Apple GPU. Strix Halo is unified; this drives effective_vram_gb() -> profile selection. And there are THREE divergent default_context_size() implementations — 2048 (sovereign-server/config.rs:285), 16384 (setup_config.rs:678), 4096 (engine.rs:1653) — none of which is what actually runs (§10.6 one-decider smell).

OPERATIONAL RULE FOR JUDGING RUNS: a judge call sends article + the ~9,000-word reference in ONE prompt (~30k tokens for an 11.8k-word article). That prefill is what tips the box. Restart the daemon before a scoring batch; do not let it run a full day of judging without one.
