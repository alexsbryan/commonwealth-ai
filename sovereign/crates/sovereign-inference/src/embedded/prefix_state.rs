// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pinned-prefix full-state cache — prefix reuse for the architectures
//! partial KV-keep cannot serve.
//!
//! The partial-keep prefix cache (`compute_lcp` + `clear_kv_cache_seq`)
//! is architecturally vetoed on recurrent/hybrid models (Gated
//! DeltaNet: `clear_kv_cache_seq` cannot rewind recurrent state — see
//! the gate rationale in `generate_sync`). FULL-state save/restore is
//! sound where partial keep is not: `llama_save_session_file`
//! serializes the whole memory module (attention KV + recurrent
//! buffers) and restoring it at position 0 of a fresh request is
//! bit-faithful (proven by `state_cartridge_spike.rs`, 2026-07-12:
//! 32/32 greedy-identical continuations on both `qwen35` and
//! `qwen35moe` hybrids; restore ≈10ms vs ≈1.3-1.4s live prefill for a
//! 1.5k-token prefix).
//!
//! This module is the bookkeeping half: a small per-slot LRU of
//! `(family key → pinned token prefix + state file)`. Two key
//! derivations, one per planning path:
//!
//!   * **Undirected** ([`PrefixStateCache::plan`]): keyed by the first
//!     [`PROBE_TOKENS`] of the tokenized prompt, boundary auto-learned as
//!     the longest common prefix of two sightings. The boundary lands
//!     exactly where requests start to diverge (in practice: the
//!     byte-stable synthesis system core, ~2.5k tokens, ends right where
//!     the varying budget directive splices in — measured 2026-07-12,
//!     prefill audit). No caller cooperation needed: the probe separates
//!     the per-handoff prompt families (synthesis / gate / gap-check /
//!     router) without any API change.
//!   * **Directed** ([`PrefixStateCache::plan_directed`], the caller
//!     declared `stable_prefix_len`): keyed by a hash of the declared
//!     prefix CONTENT, `tokens[..directed_pin]`. Siblings declaring the
//!     identical window share one entry; a different window — the next
//!     turn's evidence, a grown audit window — is a different family with
//!     its own entry, and the byte-budget LRU owns its lifetime. The probe
//!     cannot key these (2026-09-01): the grounding gate's judges all open
//!     with the same scaffold plus the head of the first evidence chunk,
//!     so two TURNS on one corpus collided on one probe key, the pin was
//!     shortened to the ~500-1300 tokens the turns shared, and every judge
//!     of every later turn re-prefilled ~12K tokens — 2-3 s judges became
//!     15-20 s.
//!
//! File placement is per-process (`temp_dir/sovereign-prefix-state/
//! <pid>-<slot>/`) — boot-scoped by design: session files embed model
//! identity and `llama_load_session_file` rejects mismatches, so
//! cross-boot reuse buys little and risks nothing but a graceful miss;
//! we simply don't attempt it.
//!
//! **Disk discipline (2026-07-21 hardening):** state files run ~64KB/token
//! (a 10K-token evidence pin ≈ 650MB), so the LRU is byte-capped —
//! `SOVEREIGN_PREFIX_STATE_MAX_MB` (default 2048) per slot, oversize pins
//! refused — and the first slot constructed per process sweeps sibling
//! `<pid>-*` dirs whose pid is dead (restart-heavy days leaked ~4GB/day
//! before the sweep).
//!
//! **Default ON since 2026-08-03** (opt out with
//! `SOVEREIGN_PREFIX_STATE=0`). Measured on the production answer path
//! via `svrn bench enrichment-ablate --prefix-state`, which is the
//! committed instrument for this knob:
//!
//!   Qwen3.6-35B-A3B, obsidian bank (12 q), 2 reps/arm
//!     OFF  901.7s, 835.2s   mean 868.4s   fact 0.4736
//!     ON   671.1s, 667.0s   mean 669.0s   fact 0.4597
//!     → 1.30x, -199s/rep, against an OFF spread of 66.5s
//!     → pin activity OFF: LEARNED=0 HIT=0 · ON: LEARNED=28 HIT=86
//!
//! The quality delta (-0.0139 mean fact ratio, ~1 fact in 60) is below
//! the ablation's 0.02 separation floor and is reported as NOT
//! SEPARABLE — but it was identical in both reps, so treat it as a
//! small reproducible difference rather than as noise. If restore is
//! bit-exact it should be zero; that is the open check.
//!
//! Three experiments measured this, on DIFFERENT workloads — and the
//! paragraph that used to live here cited only the first, which is not
//! the workload that consumes the pin:
//!
//!   * 2026-07-12, one synthesis prefill: worth ≈0 wall-clock.
//!     Synthesis prefill runs ~800 tok/s, so a ~2.7k-token stable
//!     prefix is ~3.4s inside 40-180s turns owned by retrieval fan-out
//!     and housekeeping. Not worth 172MB state saves.
//!   * 2026-07-21, the grounding gate: **1.35x end-to-end** (786.3s →
//!     584.5s, prefill 140,155 → 47,165 tokens) in a controlled A/B
//!     whose only delta was this variable, and TTFT p50 173s → 66s on
//!     a fixed persona mix over a 180-min soak (restore p90 29ms).
//!     `SOVEREIGN_GATE_BATCH_VERIFY` is off on merit, so the gate
//!     still issues one judge call per claim (~35/turn), each
//!     re-prefilling the same ~10k-token evidence prefix.
//!
//! The gate is the pin's **only** consumer (`judge.rs` passes
//! `stable_prefix_len`; ~20 other construction sites pass `None`), so
//! the 07-21 number is the one that governs. These are not in
//! conflict — the pin is worth ≈0 on a single prefill and ~1.35x when
//! the same prefix is re-prefilled 35 times.
//!
//! **Why it is still OFF:** the flip was recommended
//! (`docs/specs/BATCHED_GATE_VERIFY.md`) contingent on two hardenings,
//! both of which shipped (stale-pid sweep, byte-capped LRU) — and then
//! nobody executed it. `DEFAULTS_LEDGER.md` recorded the
//! recommendation as though it had been. Both measurements above ran
//! on `qwen35moe`; the configured primary is now dense Qwen3.5, so the
//! flip is gated on reproducing the soak there rather than on the
//! mechanism, which is unchanged. See the ledger row for the
//! falsifiable flip condition.
//!
//! Note the pin matters MOST on models where the ordinary prefix cache
//! is vetoed: `prefix_cache_gate` (`gates.rs`) refuses partial-KV
//! reuse on recurrent/hybrid architectures — including both
//! `qwen35moe` and dense `qwen35` — so on those models every gate call
//! re-prefills from zero and whole-context restore is the only thing
//! that can amortise it.
//!
//! The mechanism is kept regardless (spike-verified 116-145x
//! restore-vs-prefill on both DeltaNet hybrids) as the foundation for
//! cartridges, where pinned prefixes are 10k+ tokens.
//! Pin floor override: `SOVEREIGN_PREFIX_STATE_MIN=<tokens>`.
//!
//! Decision logic is pure and unit-tested weight-free below; all file
//! and context IO stays in `model_slot.rs` where the decode paths live.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crate::llama::cpp::token::LlamaToken;

/// Tokens hashed to identify a request family. Big enough that
/// different handoff prompts (different system openings) never
/// collide; small enough that every family member shares it.
const PROBE_TOKENS: usize = 48;

/// Default minimum pin length. Below this the state file + restore
/// bookkeeping isn't worth it (a few hundred tokens prefill in well
/// under a second); above it we're in synthesis-system-core territory
/// (~2.5k tokens) where the win is seconds per request.
const DEFAULT_MIN_PIN: usize = 384;

/// Tokens deliberately left OUT of any pin so the restored state has a
/// non-empty tail to decode. llama state files carry no logits
/// (`n_outputs=0` on load), so the sampler needs at least one fresh
/// position after a restore; `PrefixPlan::Restore` enforces the same
/// thing with its strict-prefix test.
///
/// This exists as a shared constant because the two planning paths used
/// to disagree about it, and the disagreement was a silent
/// full-prefill: `directed_pin_tokens` has always backed off
/// (`lcp.saturating_sub(2)`), while the undirected path REFUSED to pin
/// whenever `lcp == tokens.len()` and fell through to `Pass` — so two
/// BYTE-IDENTICAL prompts never formed a family, no matter how often
/// they recurred. Measured live 2026-09-02 on issue #57: the DeepQuery
/// synthesis call, 9,891 tokens, `lcp=9891 len=9891 min_pin=384`,
/// re-prefilled in full on every single turn while the gate's judges
/// beside it restored 4,881 tokens in 45 ms. The old log line called
/// that "shares too little to pin"; it shared everything.
pub(crate) const PIN_TAIL_MARGIN: usize = 2;

/// The largest pin that still leaves a decodable tail. `lcp` is what the
/// two sightings share; the result is what may be pinned.
fn pin_with_tail(lcp: usize, len: usize) -> usize {
    lcp.min(len.saturating_sub(PIN_TAIL_MARGIN))
}

/// Per-slot entry cap. Distinct request families per slot in practice:
/// synthesis primary/fast variants, gate verifier, gap check, router
/// coarse, title — six covers the live set with headroom. Directed
/// windows (one per gate turn) rotate through the same cap; at ~64KB/token
/// the byte budget below usually retires them first.
const MAX_ENTRIES: usize = 6;

/// Domain tag hashed ahead of a directed key so a 48-token declaration
/// can never alias the undirected probe key over the same tokens.
const DIRECTED_KEY_DOMAIN: &str = "directed-prefix-content";

/// Default per-slot byte budget for state files (MB). State files run
/// ~64KB/token, so a 10K-token evidence pin is ~650MB — the 2026-07-21
/// soak measured ~3.9GB steady state with the entry cap alone. 2GB
/// keeps roughly three big-corpus pins (gate + synthesis + one more)
/// while small-corpus pins fit by the dozen. Override:
/// `SOVEREIGN_PREFIX_STATE_MAX_MB`.
const DEFAULT_MAX_MB: u64 = 2_048;

pub(crate) struct PinnedPrefix {
    pub(crate) tokens: Vec<LlamaToken>,
    pub(crate) path: PathBuf,
    /// On-disk size of the state file, for the byte-budget eviction.
    pub(crate) bytes: u64,
}

/// What `generate_sync`/`generate_sync_mtp` should do for this request.
#[derive(Debug, PartialEq)]
pub(crate) enum PrefixPlan {
    /// A pinned prefix matches: restore its state file and prefill
    /// only `tokens[prefix_len..]`.
    Restore { key: u64, prefix_len: usize },
    /// Second sighting of a family: prefill `tokens[..pin_len]` first,
    /// save state, then prefill the rest. Call `commit` on success.
    Learn { key: u64, pin_len: usize },
    /// No cache interaction — existing behavior byte-for-byte.
    Pass,
}

pub(crate) struct PrefixStateCache {
    enabled: bool,
    min_pin: usize,
    max_bytes: u64,
    dir: PathBuf,
    entries: HashMap<u64, PinnedPrefix>,
    lru: VecDeque<u64>,
    /// First sighting per family, awaiting a second to learn the
    /// boundary from. Bounded alongside `entries`. Undirected families
    /// only — a directed window learns on first sight.
    last_seen: HashMap<u64, Vec<LlamaToken>>,
}

/// Default **ON** since 2026-08-03; opt OUT with
/// `SOVEREIGN_PREFIX_STATE=0` (also `false` / `off`).
///
/// Earned by a controlled A/B on `Qwen3.6-35B-A3B` through the
/// production answer path: 868.4s → 669.0s (**1.30x**) over the
/// 12-question obsidian bank, 2 reps per arm, against an OFF-arm spread
/// of 66.5s — the delta is 3x the noise. Reproduces the 2026-07-21
/// result (1.35x) on HEAD. See the ledger row for the quality caveat.
fn env_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_PREFIX_STATE").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    )
}

fn env_min_pin() -> usize {
    std::env::var("SOVEREIGN_PREFIX_STATE_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MIN_PIN)
}

fn env_max_bytes() -> u64 {
    std::env::var("SOVEREIGN_PREFIX_STATE_MAX_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_MB)
        .saturating_mul(1_048_576)
}

/// Is a `<pid>-<slot>` state dir stale — i.e. left behind by a dead
/// process? Pure decision (liveness injected) so the sweep policy is
/// unit-testable without spawning processes. Unparseable names are NOT
/// stale: we only delete what we can positively attribute to a dead pid.
fn dir_is_stale(name: &str, current_pid: u32, alive: impl Fn(u32) -> bool) -> bool {
    let Some(pid) = name.split('-').next().and_then(|p| p.parse::<u32>().ok()) else {
        return false;
    };
    pid != current_pid && !alive(pid)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0): 0 = alive; -1 with EPERM = alive but not ours;
    // -1 with ESRCH = gone.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true // no cheap probe — never sweep, correctness over tidiness
}

/// One-shot per process: remove sibling `<pid>-*` state dirs whose pid
/// is dead. Restart-heavy days measurably leak — 2026-07-21: ~4GB of
/// stale dirs across one day of daemon restarts, on top of the live
/// slot's budget. Runs at first slot construction; failures are logged
/// and ignored (a leftover dir costs disk, never correctness).
fn sweep_stale_dirs_once(base: &std::path::Path) {
    static SWEEP: std::sync::Once = std::sync::Once::new();
    SWEEP.call_once(|| {
        let current = std::process::id();
        let Ok(entries) = std::fs::read_dir(base) else {
            return; // nothing persisted yet
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if dir_is_stale(&name, current, pid_alive) {
                match std::fs::remove_dir_all(e.path()) {
                    Ok(()) => tracing::info!(
                        target: "prefix_state",
                        dir = %name,
                        "prefix_state: swept stale state dir (dead pid)"
                    ),
                    Err(err) => tracing::warn!(
                        target: "prefix_state",
                        dir = %name,
                        error = %err,
                        "prefix_state: stale-dir sweep failed — continuing"
                    ),
                }
            }
        }
    });
}

fn lcp_len(a: &[LlamaToken], b: &[LlamaToken]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

impl PrefixStateCache {
    pub(crate) fn new(slot_label: &str) -> Self {
        let sanitized: String = slot_label
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        let base = std::env::temp_dir().join("sovereign-prefix-state");
        let enabled = env_enabled();
        if enabled {
            sweep_stale_dirs_once(&base);
        }
        let dir = base.join(format!("{}-{}", std::process::id(), sanitized));
        Self {
            enabled,
            min_pin: env_min_pin(),
            max_bytes: env_max_bytes(),
            dir,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            last_seen: HashMap::new(),
        }
    }

    #[cfg(test)]
    fn new_for_test(min_pin: usize) -> Self {
        Self {
            enabled: true,
            min_pin,
            max_bytes: u64::MAX,
            dir: std::env::temp_dir().join("prefix-state-test"),
            entries: HashMap::new(),
            lru: VecDeque::new(),
            last_seen: HashMap::new(),
        }
    }

    /// Family key for the undirected path: the first [`PROBE_TOKENS`].
    fn key(tokens: &[LlamaToken]) -> u64 {
        let mut h = DefaultHasher::new();
        for t in &tokens[..PROBE_TOKENS] {
            t.0.hash(&mut h);
        }
        h.finish()
    }

    /// Family key for a DIRECTED pin: a hash of the declared prefix
    /// content, `tokens[..directed_pin]`, domain-separated from [`key`].
    /// By construction an entry under this key holds exactly these
    /// tokens, so a hit restores `directed_pin` tokens and a miss learns
    /// `directed_pin` tokens — there is no third shape.
    fn directed_key(tokens: &[LlamaToken], directed_pin: usize) -> u64 {
        let mut h = DefaultHasher::new();
        DIRECTED_KEY_DOMAIN.hash(&mut h);
        directed_pin.hash(&mut h);
        for t in &tokens[..directed_pin] {
            t.0.hash(&mut h);
        }
        h.finish()
    }

    /// Decide the cache interaction for this request's token stream.
    /// Mutates only the in-memory learning state (`last_seen`, LRU
    /// touch); file IO is the caller's.
    pub(crate) fn plan(&mut self, tokens: &[LlamaToken]) -> PrefixPlan {
        if !self.enabled || tokens.len() < self.min_pin.max(PROBE_TOKENS) + 8 {
            tracing::debug!(
                target: "prefix_state",
                enabled = self.enabled,
                prompt_tokens = tokens.len(),
                floor = self.min_pin.max(PROBE_TOKENS) + 8,
                "prefix_state: PASS — not eligible"
            );
            return PrefixPlan::Pass;
        }
        let key = Self::key(tokens);

        if let Some(entry) = self.entries.get(&key) {
            let entry_len = entry.tokens.len();
            // Strict-prefix match with a non-empty tail: the tail
            // carries the fresh logits the sampler needs, and llama
            // state files carry none (n_outputs=0 on load).
            let is_strict_prefix =
                tokens.len() > entry_len && tokens[..entry_len] == entry.tokens[..];
            let lcp = lcp_len(&entry.tokens, tokens);
            if is_strict_prefix {
                self.touch(key);
                return PrefixPlan::Restore {
                    key,
                    prefix_len: entry_len,
                };
            }
            // The family drifted (e.g. daily anchor rotated, config
            // changed). Re-learn at the surviving common prefix when
            // it's still worth pinning; otherwise drop and start over.
            let pin = pin_with_tail(lcp, tokens.len());
            if pin >= self.min_pin {
                return PrefixPlan::Learn { key, pin_len: pin };
            }
            tracing::debug!(
                target: "prefix_state",
                key = format!("{key:016x}"),
                prompt_tokens = tokens.len(),
                entry_tokens = entry_len,
                lcp,
                pin,
                min_pin = self.min_pin,
                "prefix_state: PASS — family drifted below the pin floor; dropped and re-sighting"
            );
            self.invalidate(key);
            self.last_seen.insert(key, tokens.to_vec());
            return PrefixPlan::Pass;
        }

        if let Some(prev) = self.last_seen.get(&key) {
            let lcp = lcp_len(prev, tokens);
            // `pin_with_tail`, not `lcp < tokens.len()`: two identical
            // sightings share EVERYTHING, which is the strongest possible
            // evidence for a pin and used to be the one case that refused
            // one. See `PIN_TAIL_MARGIN`.
            let pin = pin_with_tail(lcp, tokens.len());
            if pin >= self.min_pin {
                self.last_seen.remove(&key);
                return PrefixPlan::Learn { key, pin_len: pin };
            }
            // Same family fingerprint, but what they share is below the
            // floor — keep the newest sighting.
            tracing::debug!(
                target: "prefix_state",
                key = format!("{key:016x}"),
                prompt_tokens = tokens.len(),
                prev_tokens = prev.len(),
                lcp,
                pin,
                min_pin = self.min_pin,
                "prefix_state: PASS — second sighting shares too little to pin"
            );
            self.last_seen.insert(key, tokens.to_vec());
            return PrefixPlan::Pass;
        }

        // First sighting of this family.
        tracing::debug!(
            target: "prefix_state",
            key = format!("{key:016x}"),
            prompt_tokens = tokens.len(),
            sightings = self.last_seen.len(),
            "prefix_state: PASS — first sighting of this family; a second is needed to learn a boundary"
        );
        if self.last_seen.len() >= MAX_ENTRIES * 2 {
            // Bounded: drop an arbitrary stale sighting.
            if let Some(&stale) = self.last_seen.keys().next() {
                self.last_seen.remove(&stale);
            }
        }
        self.last_seen.insert(key, tokens.to_vec());
        PrefixPlan::Pass
    }

    /// Caller-directed variant of [`plan`]: the request declared its
    /// stable-prefix token boundary (`CompletionRequest.stable_prefix_len`
    /// mapped to tokens by the caller), so no two-sighting learning is
    /// needed — a window not yet pinned learns IMMEDIATELY at the
    /// directed boundary. This removes the auto-learn path's two costs
    /// for declared families: the extra full prefill of the first
    /// sighting, and relearn churn when the auto boundary lands inside
    /// shared claim-opening text (observed 2026-07-21).
    ///
    /// The family key is the declared prefix CONTENT ([`directed_key`]),
    /// not the 48-token probe, so an entry under the key IS the declared
    /// window: a hit restores exactly `directed_pin` tokens, a miss learns
    /// exactly `directed_pin` tokens, and the LRU / byte budget in
    /// [`commit_sized`] is the only thing that ever removes an entry. Two
    /// branches used to live between hit and miss; both were symptoms of
    /// keying a declared window on a probe it shared with other windows:
    ///
    ///   * "pin is short of the declared prefix — re-learning"
    ///     (2026-08-24): a grown audit window shared the probe with its
    ///     smaller predecessor and kept restoring the small pin (124
    ///     restores at 1064 tokens, mean 2289 re-prefilled, ~35 min of a
    ///     39.5-min leg). Under content keys the grown window is its own
    ///     key and learns its own pin once.
    ///   * "two shapes share this family — pinning at their common prefix"
    ///     (2026-08-27): two windows alternating within one flight evicted
    ///     each other under one probe key (Flash-Next: [3998, 4612, 3998,
    ///     4612], ~240 s of 566 s cold prefill), so the pin was shortened
    ///     to what both shared and frozen there. That compromise then bit
    ///     the grounding gate (2026-09-01): every later TURN on the same
    ///     corpus shares the probe with the previous one, so the gate
    ///     pinned the ~500-1300 tokens two turns share and re-prefilled
    ///     ~12K per judge. Under content keys alternating windows hold two
    ///     entries and cannot evict each other.
    ///
    /// Out-of-range/short directives fall back to the sighting-based
    /// [`plan`] so a bad caller can never make behavior worse than
    /// undeclared.
    pub(crate) fn plan_directed(
        &mut self,
        tokens: &[LlamaToken],
        directed_pin: usize,
    ) -> PrefixPlan {
        if !self.enabled {
            return PrefixPlan::Pass;
        }
        if tokens.len() < PROBE_TOKENS
            || directed_pin < self.min_pin.max(PROBE_TOKENS)
            || directed_pin >= tokens.len()
        {
            return self.plan(tokens);
        }
        let key = Self::directed_key(tokens, directed_pin);
        if let Some(entry) = self.entries.get(&key) {
            // Content-keyed: the entry holds exactly `tokens[..directed_pin]`.
            // The compare is the one guard against a 64-bit hash collision,
            // and it is not optional — restoring foreign state would be wrong
            // output, not a slow path. `directed_pin < tokens.len()` above
            // guarantees the non-empty tail the sampler's logits come from.
            if entry.tokens[..] == tokens[..directed_pin] {
                let prefix_len = entry.tokens.len();
                self.touch(key);
                return PrefixPlan::Restore { key, prefix_len };
            }
            tracing::warn!(
                target: "prefix_state",
                key = format_args!("{key:016x}"),
                pinned_tokens = entry.tokens.len(),
                directed_tokens = directed_pin,
                "prefix_state: directed key collision — entry content differs, replacing"
            );
        }
        // First sighting of this window: learn NOW at the directed
        // boundary. `commit` files it under the content key.
        tracing::info!(
            target: "prefix_state",
            key = format_args!("{key:016x}"),
            family_key = "hash(tokens[..directed_pin])",
            directed_tokens = directed_pin,
            prompt_tokens = tokens.len(),
            resident_pins = self.entries.len(),
            "prefix_state: unpinned directed window — learning at the declared prefix"
        );
        PrefixPlan::Learn {
            key,
            pin_len: directed_pin,
        }
    }

    /// Path a `Learn` plan should save the state file to.
    pub(crate) fn state_path(&self, key: u64) -> PathBuf {
        self.dir.join(format!("{key:016x}.state"))
    }

    /// Ensure the state directory exists (call before saving).
    pub(crate) fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    /// Record a successfully saved pin. Evicts LRU overflow — by entry
    /// count AND by the per-slot byte budget (`SOVEREIGN_PREFIX_STATE_MAX_MB`)
    /// — deleting evicted state files best-effort. A pin whose file alone
    /// exceeds the whole budget is REFUSED (file deleted, nothing evicted):
    /// admitting it would flush every other family for one pin.
    pub(crate) fn commit(&mut self, key: u64, prefix_tokens: Vec<LlamaToken>, path: PathBuf) {
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.commit_sized(key, prefix_tokens, path, bytes);
    }

    fn commit_sized(
        &mut self,
        key: u64,
        prefix_tokens: Vec<LlamaToken>,
        path: PathBuf,
        bytes: u64,
    ) {
        if bytes > self.max_bytes {
            tracing::warn!(
                target: "prefix_state",
                key = format_args!("{key:016x}"),
                bytes,
                budget = self.max_bytes,
                "prefix_state: pin larger than the whole byte budget — refused"
            );
            let _ = std::fs::remove_file(&path);
            return;
        }
        // Replacing an entry under the same key: drop the old file first
        // so the byte accounting below sees only live entries.
        if let Some(old) = self.entries.remove(&key) {
            if old.path != path {
                let _ = std::fs::remove_file(&old.path);
            }
        }
        self.entries.insert(
            key,
            PinnedPrefix {
                tokens: prefix_tokens,
                path,
                bytes,
            },
        );
        self.lru.retain(|k| *k != key);
        self.lru.push_back(key);
        let total = |entries: &HashMap<u64, PinnedPrefix>| -> u64 {
            entries.values().map(|e| e.bytes).sum()
        };
        while self.lru.len() > MAX_ENTRIES
            || (total(&self.entries) > self.max_bytes && self.lru.len() > 1)
        {
            if let Some(old) = self.lru.pop_front() {
                if let Some(e) = self.entries.remove(&old) {
                    tracing::info!(
                        target: "prefix_state",
                        key = format_args!("{old:016x}"),
                        freed_bytes = e.bytes,
                        "prefix_state: evicted pin (LRU / byte budget)"
                    );
                    let _ = std::fs::remove_file(&e.path);
                }
            }
        }
    }

    pub(crate) fn entry_path(&self, key: u64) -> Option<PathBuf> {
        self.entries.get(&key).map(|e| e.path.clone())
    }

    /// Drop a pin whose restore failed (self-healing: next sightings
    /// re-learn).
    pub(crate) fn invalidate(&mut self, key: u64) {
        if let Some(e) = self.entries.remove(&key) {
            let _ = std::fs::remove_file(&e.path);
        }
        self.lru.retain(|k| *k != key);
    }

    fn touch(&mut self, key: u64) {
        self.lru.retain(|k| *k != key);
        self.lru.push_back(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(head: i32, body: &[i32]) -> Vec<LlamaToken> {
        // First PROBE_TOKENS identical per `head` (the family
        // fingerprint), then the body.
        let mut v: Vec<LlamaToken> = (0..PROBE_TOKENS as i32)
            .map(|i| LlamaToken(head * 10_000 + i))
            .collect();
        v.extend(body.iter().map(|&t| LlamaToken(t)));
        v
    }

    /// Two request shapes that share a family fingerprint AND a common
    /// core, then diverge — the shape that thrashed on Flash-Next
    /// (2026-08-26). `own_len` is stable text only THIS shape declares.
    fn shape(shared_core: usize, own: i32, own_len: usize, tail_len: usize) -> Vec<LlamaToken> {
        let core: Vec<i32> = (0..shared_core as i32).collect();
        let mut v = toks(1, &core);
        v.extend((0..own_len as i32).map(|i| LlamaToken(500_000 + own * 10_000 + i)));
        v.extend((0..tail_len as i32).map(|i| LlamaToken(900_000 + own * 1_000 + i)));
        v
    }

    /// A sibling call: the identical declared `window`, then a tail only
    /// this call carries (a different claim under the same evidence).
    fn sibling(window: &[LlamaToken], tail_seed: i32, tail_len: usize) -> Vec<LlamaToken> {
        let mut v = window.to_vec();
        v.extend((0..tail_len as i32).map(|i| LlamaToken(950_000 + tail_seed * 1_000 + i)));
        v
    }

    /// Two directed windows that share the 48-token probe but diverge
    /// inside the declared prefix are two families: each learns its own
    /// FULL declared prefix, and a sibling of either restores all of it —
    /// never a common-prefix compromise. This is the 2026-09-01 gate defect
    /// in miniature: turn N+1's judges open like turn N's (same scaffold,
    /// same first chunk) and diverge at the second chunk.
    ///
    /// Watched red on the probe-keyed code: turn 2 planned
    /// `Learn { pin_len: 248 }` — the 200-token core it shares with turn 1,
    /// not its own 300.
    /// ISSUE #57, found live on 2026-09-02 by instrumenting the undirected
    /// path's silent `Pass` returns. The DeepQuery synthesis call sends the
    /// SAME 9,891-token prompt every turn, and the cache refused it a pin
    /// every turn: the learn guard was `lcp < tokens.len()`, so the one case
    /// where two sightings share everything fell through to `Pass` and the
    /// family never formed. The gate's judges, four inches away in the same
    /// turn, were restoring 4,881 tokens in 45 ms off the directed path,
    /// which had always backed off by `PIN_TAIL_MARGIN` instead of refusing.
    ///
    /// Watched red on the old code: the second sighting returned `Pass`, and
    /// so did the third, and the tenth.
    #[test]
    fn two_identical_sightings_learn_a_pin_rather_than_refusing_one() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let prompt = toks(1, &(0..600).collect::<Vec<i32>>());
        assert_eq!(
            lcp_len(&prompt, &prompt),
            prompt.len(),
            "fixture: the two sightings are byte-identical"
        );

        assert!(
            matches!(cache.plan(&prompt), PrefixPlan::Pass),
            "first sighting has nothing to compare against"
        );

        let want = prompt.len() - PIN_TAIL_MARGIN;
        match cache.plan(&prompt) {
            PrefixPlan::Learn { pin_len, .. } => assert_eq!(
                pin_len, want,
                "an identical repeat pins all but the decodable tail"
            ),
            other => panic!("identical repeat must learn, got {other:?}"),
        }
    }

    /// The pin the case above learns has to be RESTORABLE, or the fix just
    /// moves the full prefill one turn later. `Restore` needs a strict
    /// prefix with a non-empty tail, which is what the margin buys.
    #[test]
    fn the_pin_learned_from_an_identical_repeat_is_then_restorable() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let prompt = toks(1, &(0..600).collect::<Vec<i32>>());
        cache.plan(&prompt);
        let PrefixPlan::Learn { key, pin_len } = cache.plan(&prompt) else {
            panic!("second sighting must learn");
        };
        cache.commit(key, prompt[..pin_len].to_vec(), std::path::PathBuf::from("/tmp/x"));

        match cache.plan(&prompt) {
            PrefixPlan::Restore { prefix_len, .. } => assert_eq!(
                prefix_len, pin_len,
                "the third sighting restores the whole pin"
            ),
            other => panic!("expected Restore, got {other:?}"),
        }
    }

    #[test]
    fn directed_windows_sharing_the_probe_get_their_own_entries() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let turn1 = shape(200, 1, 100, 40);
        let turn2 = shape(200, 2, 100, 40);
        let pin = PROBE_TOKENS + 200 + 100;
        assert_eq!(
            lcp_len(&turn1, &turn2),
            PROBE_TOKENS + 200,
            "fixture: the windows share the probe and diverge inside the declared prefix"
        );

        let PrefixPlan::Learn { key: key1, pin_len } = cache.plan_directed(&turn1, pin) else {
            panic!("turn 1 learns on first sight")
        };
        assert_eq!(pin_len, pin);
        cache.commit(key1, turn1[..pin].to_vec(), cache.state_path(key1));

        let plan = cache.plan_directed(&turn2, pin);
        let PrefixPlan::Learn { key: key2, pin_len } = plan else {
            panic!("turn 2 must learn its own window, got {plan:?}")
        };
        assert_eq!(
            pin_len, pin,
            "turn 2 learns its FULL declared prefix, not the prefix it shares with turn 1"
        );
        assert_ne!(key2, key1, "a different window is a different family key");
        cache.commit(key2, turn2[..pin].to_vec(), cache.state_path(key2));
        assert_eq!(cache.entries.len(), 2, "two windows, two entries");

        // A sibling of EITHER turn restores that turn's whole declared prefix.
        assert_eq!(
            cache.plan_directed(&sibling(&turn1[..pin], 1, 55), pin),
            PrefixPlan::Restore {
                key: key1,
                prefix_len: pin
            }
        );
        assert_eq!(
            cache.plan_directed(&sibling(&turn2[..pin], 2, 55), pin),
            PrefixPlan::Restore {
                key: key2,
                prefix_len: pin
            }
        );
    }

    /// Siblings declaring the identical window share ONE entry — the second
    /// call restores `prefix_len == directed_pin` — and that entry is not
    /// shrunk by a nested SHORTER window that shares its probe (the
    /// 2026-08-24 shape: an audit window that grew between passes, so the
    /// small window and the grown one are both declared for a while).
    ///
    /// Watched red on the probe-keyed code: the nested window came back
    /// under the long window's key, then replaced its entry with the
    /// 248-token compromise.
    #[test]
    fn directed_siblings_with_one_declared_window_share_one_entry() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let long = shape(200, 1, 100, 40);
        let pin_long = PROBE_TOKENS + 300;
        let PrefixPlan::Learn { key, .. } = cache.plan_directed(&long, pin_long) else {
            panic!("first sighting learns")
        };
        cache.commit(key, long[..pin_long].to_vec(), cache.state_path(key));

        for seed in 2..5 {
            assert_eq!(
                cache.plan_directed(
                    &sibling(&long[..pin_long], seed, 30 + seed as usize),
                    pin_long
                ),
                PrefixPlan::Restore {
                    key,
                    prefix_len: pin_long
                },
                "sibling {seed} restores the whole declared window"
            );
        }
        assert_eq!(
            cache.entries.len(),
            1,
            "identical declared windows share one entry"
        );

        // A nested shorter window: the same bytes up to the shared core,
        // declared as the whole prefix.
        let pin_short = PROBE_TOKENS + 200;
        let short = sibling(&long[..pin_short], 9, 40);
        let plan = cache.plan_directed(&short, pin_short);
        let PrefixPlan::Learn {
            key: key_short,
            pin_len,
        } = plan
        else {
            panic!("a shorter window is its own family, got {plan:?}")
        };
        assert_eq!(pin_len, pin_short);
        assert_ne!(key_short, key, "a nested window is a different family key");
        cache.commit(
            key_short,
            short[..pin_short].to_vec(),
            cache.state_path(key_short),
        );

        assert_eq!(
            cache.plan_directed(&sibling(&long[..pin_long], 7, 33), pin_long),
            PrefixPlan::Restore {
                key,
                prefix_len: pin_long
            },
            "the long window was not shrunk to the nested one"
        );
        assert_eq!(cache.entries.len(), 2);
    }

    /// Two windows sharing the probe must not evict each other, and neither
    /// may be shortened to what they share.
    ///
    /// Rewritten from `two_shapes_sharing_a_family_converge_instead_of_thrashing`
    /// (2026-08-27). That test asserted the COMPROMISE: once A and B had each
    /// learned under the one probe key, A re-pinned at their common prefix
    /// and both shapes restored `pin_a` forever — B paying its own 100-token
    /// tail on every call. The compromise stopped the eviction thrash it was
    /// built for (Flash-Next, [3998, 4612, 3998, 4612]) and then broke the
    /// grounding gate: every later TURN on one corpus shares the probe with
    /// the previous turn, so the gate pinned the ~500-1300 tokens two turns
    /// share and re-prefilled ~12K per judge (2026-09-01). Under content keys
    /// the two windows are two entries. The property that survives is "once
    /// both are pinned, nothing re-learns"; the one that changed is "each
    /// restores ITS OWN full declared prefix".
    ///
    /// Watched red on the probe-keyed code: round 0, A restored 248 tokens
    /// (the compromise) instead of its declared 348.
    #[test]
    fn alternating_directed_windows_never_relearn_once_both_are_pinned() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = shape(200, 1, 100, 40);
        let b = shape(200, 2, 150, 40);
        let pin_a = PROBE_TOKENS + 200 + 100;
        let pin_b = PROBE_TOKENS + 200 + 150;

        let PrefixPlan::Learn {
            key: key_a,
            pin_len,
        } = cache.plan_directed(&a, pin_a)
        else {
            panic!("A learns first")
        };
        cache.commit(key_a, a[..pin_len].to_vec(), cache.state_path(key_a));
        let PrefixPlan::Learn {
            key: key_b,
            pin_len,
        } = cache.plan_directed(&b, pin_b)
        else {
            panic!("B learns its own window")
        };
        cache.commit(key_b, b[..pin_len].to_vec(), cache.state_path(key_b));

        for round in 0..4 {
            let plan_a = cache.plan_directed(&a, pin_a);
            assert!(
                !matches!(plan_a, PrefixPlan::Learn { .. }),
                "round {round}: A re-learned — the eviction thrash is back"
            );
            assert_eq!(
                plan_a,
                PrefixPlan::Restore {
                    key: key_a,
                    prefix_len: pin_a
                },
                "round {round}: A must restore its whole declared prefix"
            );
            let plan_b = cache.plan_directed(&b, pin_b);
            assert!(
                !matches!(plan_b, PrefixPlan::Learn { .. }),
                "round {round}: B re-learned — the eviction thrash is back"
            );
            assert_eq!(
                plan_b,
                PrefixPlan::Restore {
                    key: key_b,
                    prefix_len: pin_b
                },
                "round {round}: B must restore its whole declared prefix"
            );
        }
        assert_eq!(cache.entries.len(), 2, "two windows, two entries, no churn");
    }

    /// Distinct directed windows accumulate only up to the LRU cap.
    ///
    /// Rewritten from `a_drifted_family_still_replaces_its_pin`
    /// (2026-08-27), which asserted the compromise's escape hatch: evidence
    /// sharing only the probe (`lcp < min_pin`) learned under the SAME key
    /// and replaced the entry in place. Under content keys nothing is ever
    /// replaced in place — each drifted window is its own key — so what has
    /// to be proven instead is the closure: the entry count is bounded by
    /// `MAX_ENTRIES` (and the byte budget, `byte_budget_evicts_lru_until_under_cap`)
    /// and the oldest window is the one retired.
    ///
    /// Watched red on the probe-keyed code: all eight windows came back
    /// under one key.
    #[test]
    fn distinct_directed_windows_are_bounded_by_the_lru() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let pin = PROBE_TOKENS + 200;
        let mut keys = Vec::new();
        for turn in 0..(MAX_ENTRIES as i32 + 2) {
            // Same 48-token opening, different evidence from token 48 on.
            let mut w = toks(1, &[]);
            w.extend((0..300i32).map(|i| LlamaToken(770_000 + turn * 1_000 + i)));
            let plan = cache.plan_directed(&w, pin);
            let PrefixPlan::Learn { key, pin_len } = plan else {
                panic!("turn {turn}: a new window learns immediately, got {plan:?}")
            };
            assert_eq!(
                pin_len, pin,
                "turn {turn}: learns the whole declared prefix"
            );
            cache.commit(key, w[..pin].to_vec(), cache.state_path(key));
            keys.push(key);
        }
        let distinct: std::collections::HashSet<u64> = keys.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            keys.len(),
            "every drifted window is its own family key"
        );
        assert_eq!(cache.entries.len(), MAX_ENTRIES, "bounded by the entry cap");
        assert!(
            !cache.entries.contains_key(&keys[0]),
            "the oldest window was retired"
        );
        assert!(cache.entries.contains_key(keys.last().unwrap()));
    }

    /// A family: shared stable core of `core_len` tokens, then a
    /// per-request variable tail.
    fn family_member(core_len: usize, tail_seed: i32, tail_len: usize) -> Vec<LlamaToken> {
        let core: Vec<i32> = (0..core_len as i32).collect();
        let mut v = toks(1, &core);
        v.extend((0..tail_len as i32).map(|i| LlamaToken(900_000 + tail_seed * 1_000 + i)));
        v
    }

    #[test]
    fn learns_boundary_from_two_sightings_then_restores() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = family_member(200, 1, 40);
        let b = family_member(200, 2, 55);

        // First sighting: pass (nothing to compare against).
        assert_eq!(cache.plan(&a), PrefixPlan::Pass);

        // Second sighting: learn at the exact divergence boundary.
        let plan = cache.plan(&b);
        let PrefixPlan::Learn { key, pin_len } = plan else {
            panic!("expected Learn, got {plan:?}");
        };
        assert_eq!(
            pin_len,
            PROBE_TOKENS + 200,
            "pin lands at the divergence point"
        );

        // Commit the pin; a third member restores.
        cache.commit(key, b[..pin_len].to_vec(), cache.state_path(key));
        let c = family_member(200, 3, 70);
        assert_eq!(
            cache.plan(&c),
            PrefixPlan::Restore {
                key,
                prefix_len: pin_len
            }
        );
    }

    #[test]
    fn short_prompts_and_short_overlap_pass() {
        let mut cache = PrefixStateCache::new_for_test(64);
        // Too short to consider at all.
        let tiny = toks(2, &[1, 2, 3]);
        assert_eq!(cache.plan(&tiny), PrefixPlan::Pass);

        // Same fingerprint but the shared prefix is under min_pin:
        // never pins.
        let a = family_member(10, 1, 400);
        let b = family_member(10, 2, 400);
        assert_eq!(cache.plan(&a), PrefixPlan::Pass);
        assert_eq!(cache.plan(&b), PrefixPlan::Pass);
    }

    #[test]
    fn identical_full_prompts_never_restore_with_empty_tail() {
        // The tail carries the fresh logits, so an exact-match prompt must
        // never plan a ZERO-TAIL restore. It used to get there by refusing
        // the pin outright (`lcp < tokens.len()`), which also refused the
        // ~9.9k-token DeepQuery synthesis prompt on every turn forever
        // (issue #57). It now pins all but `PIN_TAIL_MARGIN` instead — the
        // invariant this test is named for, without the full prefill.
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = family_member(200, 1, 40);
        cache.plan(&a);
        let PrefixPlan::Learn { key, pin_len } = cache.plan(&a.clone()) else {
            panic!("an identical repeat is the strongest evidence for a pin");
        };
        assert!(
            pin_len < a.len(),
            "the pin must leave a tail: pin_len={pin_len} len={}",
            a.len()
        );
        cache.commit(key, a[..pin_len].to_vec(), cache.state_path(key));
        // And the restore it enables still leaves that tail to decode.
        assert_eq!(
            cache.plan(&a),
            PrefixPlan::Restore {
                key,
                prefix_len: pin_len
            }
        );
        assert!(a.len() > pin_len, "restore keeps a non-empty tail");
    }

    #[test]
    fn drift_relearns_at_shorter_boundary() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = family_member(200, 1, 40);
        let b = family_member(200, 2, 55);
        cache.plan(&a);
        let PrefixPlan::Learn { key, pin_len } = cache.plan(&b) else {
            panic!("expected Learn");
        };
        cache.commit(key, b[..pin_len].to_vec(), cache.state_path(key));

        // The family drifts: only the first 120 core tokens survive
        // (e.g. daily anchor rotated mid-core). Next sighting re-learns
        // at the shorter boundary instead of restoring stale state.
        let drifted = family_member(120, 9, 60);
        let plan = cache.plan(&drifted);
        assert_eq!(
            plan,
            PrefixPlan::Learn {
                key,
                pin_len: PROBE_TOKENS + 120
            }
        );
    }

    #[test]
    fn lru_evicts_oldest_family() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let mut keys = Vec::new();
        for fam in 0..(MAX_ENTRIES as i32 + 2) {
            let core: Vec<i32> = (0..200).collect();
            let mut a = toks(fam + 1, &core);
            a.extend([LlamaToken(1)]);
            let mut b = toks(fam + 1, &core);
            b.extend([LlamaToken(2)]);
            cache.plan(&a);
            if let PrefixPlan::Learn { key, pin_len } = cache.plan(&b) {
                cache.commit(key, b[..pin_len].to_vec(), cache.state_path(key));
                keys.push(key);
            } else {
                panic!("expected Learn for family {fam}");
            }
        }
        assert!(cache.entries.len() <= MAX_ENTRIES);
        // The first-committed families were evicted.
        assert!(!cache.entries.contains_key(&keys[0]));
        assert!(cache.entries.contains_key(keys.last().unwrap()));
    }

    #[test]
    fn byte_budget_evicts_lru_until_under_cap() {
        let mut cache = PrefixStateCache::new_for_test(64);
        cache.max_bytes = 1_000;
        let mut keys = Vec::new();
        // Three families of 400 bytes each: the third commit must evict
        // the first (1200 > 1000 → evict LRU front → 800 ≤ 1000).
        for fam in 0..3i32 {
            let core: Vec<i32> = (0..200).collect();
            let mut a = toks(fam + 1, &core);
            a.extend([LlamaToken(1)]);
            let key = PrefixStateCache::key(&a);
            cache.commit_sized(
                key,
                a[..PROBE_TOKENS + 100].to_vec(),
                cache.state_path(key),
                400,
            );
            keys.push(key);
        }
        assert!(!cache.entries.contains_key(&keys[0]), "oldest evicted");
        assert!(cache.entries.contains_key(&keys[1]));
        assert!(cache.entries.contains_key(&keys[2]));
        assert!(cache.entries.values().map(|e| e.bytes).sum::<u64>() <= 1_000);
    }

    #[test]
    fn oversized_pin_is_refused_not_admitted() {
        let mut cache = PrefixStateCache::new_for_test(64);
        cache.max_bytes = 1_000;
        let core: Vec<i32> = (0..200).collect();
        let small = toks(1, &core);
        let k_small = PrefixStateCache::key(&small);
        cache.commit_sized(
            k_small,
            small[..PROBE_TOKENS + 100].to_vec(),
            cache.state_path(k_small),
            400,
        );

        // A pin bigger than the WHOLE budget: refused, and the resident
        // small pin survives (admitting would have flushed everything).
        let big = toks(2, &core);
        let k_big = PrefixStateCache::key(&big);
        cache.commit_sized(
            k_big,
            big[..PROBE_TOKENS + 100].to_vec(),
            cache.state_path(k_big),
            5_000,
        );
        assert!(!cache.entries.contains_key(&k_big));
        assert!(cache.entries.contains_key(&k_small));
    }

    #[test]
    fn stale_dir_decision_only_deletes_dead_foreign_pids() {
        let alive = |p: u32| p == 111 || p == 222;
        // Foreign + dead → stale.
        assert!(dir_is_stale("999-qwen35moe", 111, alive));
        // Own pid → never stale, even if the probe lies.
        assert!(!dir_is_stale("111-qwen35moe", 111, |_| false));
        // Foreign but alive → keep (another daemon / compute child).
        assert!(!dir_is_stale("222-qwen35moe", 111, alive));
        // Unparseable name → keep (only delete what we can attribute).
        assert!(!dir_is_stale("not-a-pid-dir", 111, alive));
    }

    #[test]
    fn directed_learns_on_first_sighting_then_restores() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = family_member(200, 1, 40);
        let pin = PROBE_TOKENS + 180; // directed boundary inside the shared core

        // First sighting: directed plan learns IMMEDIATELY (no second
        // sighting needed — the whole point).
        let plan = cache.plan_directed(&a, pin);
        let PrefixPlan::Learn { key, pin_len } = plan else {
            panic!("expected immediate Learn, got {plan:?}");
        };
        assert_eq!(pin_len, pin);
        cache.commit(key, a[..pin].to_vec(), cache.state_path(key));

        // Sibling with a different tail restores at the entry boundary.
        let b = family_member(200, 2, 55);
        assert_eq!(
            cache.plan_directed(&b, pin),
            PrefixPlan::Restore {
                key,
                prefix_len: pin
            }
        );
    }

    /// Drifted evidence learns immediately — no invalidate → Pass → Learn
    /// sighting dance — and under its OWN key.
    ///
    /// Until 2026-09-01 this asserted the Learn came back under the same
    /// key as the stale entry (probe keying: same 48-token opening, one
    /// family, entry replaced in place). A drifted window is now a different
    /// family; the stale entry is the LRU's to retire
    /// (`distinct_directed_windows_are_bounded_by_the_lru`), not this call's.
    ///
    /// Watched red on the probe-keyed code: `key_c == key`.
    #[test]
    fn directed_drifted_evidence_learns_immediately_under_its_own_key() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = family_member(200, 1, 40);
        let pin_a = PROBE_TOKENS + 180;
        let PrefixPlan::Learn { key, .. } = cache.plan_directed(&a, pin_a) else {
            panic!("expected Learn");
        };
        cache.commit(key, a[..pin_a].to_vec(), cache.state_path(key));

        // Next turn: same family fingerprint, new evidence (core drifts
        // right after the probe).
        let mut c = toks(1, &(500..700).collect::<Vec<i32>>());
        c.extend([LlamaToken(1), LlamaToken(2), LlamaToken(3)]);
        let pin_c = PROBE_TOKENS + 150;
        let plan = cache.plan_directed(&c, pin_c);
        let PrefixPlan::Learn {
            key: key_c,
            pin_len,
        } = plan
        else {
            panic!("drifted evidence learns on first sight, got {plan:?}")
        };
        assert_eq!(pin_len, pin_c, "at the NEW declared boundary");
        assert_ne!(key_c, key, "a drifted window is its own family key");
        assert!(
            cache.entries.contains_key(&key),
            "the previous window is not invalidated by a drift"
        );
    }

    /// **A grown declared window is its own entry, learned at full length.**
    ///
    /// The 2026-08-24 deep-research task-69 flight logged 124 restores,
    /// every one `restored_tokens=1064` against a declared window that had
    /// grown past 3,300 — mean `suffix_tokens=2289` re-prefilled, 283,874
    /// tokens total, roughly 35 minutes of a 39.5-minute audit leg: the
    /// short pin strict-prefix-matched, so it was restored and the growth
    /// re-prefilled on every call.
    ///
    /// Until 2026-09-01 this test (`directed_relearns_a_pin_shorter_than_the_declaration`)
    /// asserted the cure as a RE-LEARN under the same probe key — the short
    /// entry replaced by the grown one, with a `min_pin` margin so a small
    /// growth would not churn. Under content keys there is no margin and no
    /// replacement: the grown window is a different key, learned once at its
    /// full length, and the short window's entry stays until the LRU retires
    /// it. The cost that matters — every sibling of the grown window
    /// restoring the whole declared prefix — is asserted at the end.
    ///
    /// Watched red on the probe-keyed code: `key_grown == key`.
    #[test]
    fn a_grown_declared_window_is_its_own_entry_learned_at_full_length() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let short = family_member(200, 1, 40);
        let pin_short = PROBE_TOKENS + 100;
        let PrefixPlan::Learn { key, .. } = cache.plan_directed(&short, pin_short) else {
            panic!("expected Learn on first sighting");
        };
        cache.commit(key, short[..pin_short].to_vec(), cache.state_path(key));

        // The same opening, now declaring a MUCH longer stable prefix — the
        // shape of an evidence window that grew between passes.
        let mut grown = short[..pin_short].to_vec();
        grown.extend((0..900i32).map(|i| LlamaToken(700_000 + i)));
        let directed = pin_short + 800;
        assert!(grown.len() > directed);
        let plan = cache.plan_directed(&grown, directed);
        let PrefixPlan::Learn {
            key: key_grown,
            pin_len,
        } = plan
        else {
            panic!("the grown window learns, got {plan:?}")
        };
        assert_eq!(pin_len, directed, "learned at the full declared length");
        assert_ne!(key_grown, key, "a grown window is a different family key");
        assert!(
            cache.entries.contains_key(&key),
            "the short window's entry is the LRU's to retire, not this call's"
        );
        cache.commit(
            key_grown,
            grown[..directed].to_vec(),
            cache.state_path(key_grown),
        );

        assert_eq!(
            cache.plan_directed(&sibling(&grown[..directed], 3, 60), directed),
            PrefixPlan::Restore {
                key: key_grown,
                prefix_len: directed
            },
            "every sibling of the grown window restores the whole declared prefix"
        );
    }

    /// A declared window that differs from a pinned one by a few tokens is
    /// its own entry, and never disturbs the pinned one.
    ///
    /// Until 2026-09-01 this test (`directed_keeps_restoring_when_the_pin_is_close_or_longer`)
    /// asserted two riders on probe keying: a declaration 10 tokens longer
    /// than the pin RESTORED the pin (the re-learn margin, so a trivial
    /// shortfall would not churn), and a declaration 50 tokens shorter
    /// restored the pin too ("an entry longer than the directive restores at
    /// its own length"). Under content keys `pin + 10` and `pin - 50` name
    /// different windows: each learns once under its own key, and the churn
    /// the margin guarded against cannot occur because no entry is ever
    /// replaced by another window. The real consumer never jitters — the gate
    /// asserts one byte-identical boundary across siblings
    /// (`the_gate_shares_one_prefix_family`, judge.rs).
    ///
    /// Watched red on the probe-keyed code: `pin + 10` planned a Restore.
    #[test]
    fn a_declared_window_that_differs_by_a_few_tokens_is_its_own_entry() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let base = family_member(200, 1, 40);
        let pin = PROBE_TOKENS + 200;
        let PrefixPlan::Learn { key, .. } = cache.plan_directed(&base, pin) else {
            panic!("expected Learn");
        };
        cache.commit(key, base[..pin].to_vec(), cache.state_path(key));

        let mut longer = base[..pin].to_vec();
        longer.extend((0..600i32).map(|i| LlamaToken(800_000 + i)));

        for declared in [pin + 10, pin - 50] {
            let plan = cache.plan_directed(&longer, declared);
            let PrefixPlan::Learn { key: k, pin_len } = plan else {
                panic!("a declaration of {declared} is its own window, got {plan:?}")
            };
            assert_eq!(pin_len, declared, "learned at exactly the declared length");
            assert_ne!(k, key, "a different window is a different family key");
            cache.commit(k, longer[..declared].to_vec(), cache.state_path(k));
        }
        assert_eq!(cache.entries.len(), 3, "three windows, three entries");
        assert_eq!(
            cache.plan_directed(&sibling(&base[..pin], 4, 30), pin),
            PrefixPlan::Restore {
                key,
                prefix_len: pin
            },
            "the original window's pin was not disturbed"
        );
    }

    #[test]
    fn directed_out_of_range_falls_back_to_sighting_plan() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = family_member(200, 1, 40);
        // Pin below min_pin and pin past the end both degrade to the
        // sighting-based plan. Each uses its OWN family, so what is asserted
        // is the fallback itself — a first sighting Passes — rather than the
        // undirected path's second-sighting rule, which these two calls used
        // to exercise by accident (both passed `a`).
        let b = toks(7, &(0..300).collect::<Vec<i32>>());
        assert_ne!(
            PrefixStateCache::key(&a),
            PrefixStateCache::key(&b),
            "fixture: the two probes must be different families"
        );
        assert_eq!(cache.plan_directed(&a, 8), PrefixPlan::Pass);
        assert_eq!(cache.plan_directed(&b, b.len() + 5), PrefixPlan::Pass);
    }

    #[test]
    fn disabled_via_env_shape_passes_everything() {
        let mut cache = PrefixStateCache::new_for_test(64);
        cache.enabled = false;
        let a = family_member(200, 1, 40);
        let b = family_member(200, 2, 55);
        assert_eq!(cache.plan(&a), PrefixPlan::Pass);
        assert_eq!(cache.plan(&b), PrefixPlan::Pass);
    }
}
