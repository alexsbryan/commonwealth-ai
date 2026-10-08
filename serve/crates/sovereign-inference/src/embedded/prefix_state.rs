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
//! Decision logic is pure and unit-tested weight-free below; context IO
//! lives in `model_slot.rs` where the decode paths are, and the request-side
//! half (which plan a request takes, the conversation re-pin) in
//! `prefix_pin.rs`.

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

    /// Plan for one turn of an external client's conversation
    /// (`PromptShape::Conversation`): the restore half, plus the family key
    /// to RE-PIN the whole prompt under once it has been prefilled.
    ///
    /// [`plan`] freezes a family at the boundary its first two sightings
    /// share, which for a conversation is turn 1: measured 2026-10-08 on the
    /// e2eswe-slice battery, the 27B restored the same 9,657 tokens for a
    /// whole task while prompts grew to 39k, and turns ran 280 s for ~100
    /// output tokens. A conversation is append-only — each prompt is the
    /// last one plus the reply and the new turn (the 9,657-token pin was ALL
    /// of turn 1's prompt) — so the pin should follow it. This is
    /// llama-server's semantics on hybrid models: it checkpoints the memory
    /// a few tokens before each prompt's end and restores the newest one
    /// the next prompt still extends (`server-context.cpp`, the
    /// `checkpoint_offsets` block at 035e227).
    ///
    /// Re-pinning costs one state save per turn, so it fires only once the
    /// prompt has grown `min_pin` tokens past the entry;
    /// below that the entry is restored and the short suffix prefilled. A
    /// prompt the entry does not prefix (a new task under the same system
    /// head, an edited history) is a full prefill and then re-pinned whole —
    /// never [`plan`]'s re-learn at the shared head, which would pin the
    /// head and prefill everything after it on every later turn.
    pub(crate) fn plan_conversation(&mut self, tokens: &[LlamaToken]) -> (PrefixPlan, Option<u64>) {
        if !self.enabled || tokens.len() < self.min_pin.max(PROBE_TOKENS) + 8 {
            return (PrefixPlan::Pass, None);
        }
        let key = Self::key(tokens);
        let entry_len = self.entries.get(&key).map(|e| e.tokens.len());
        // The same strict-prefix test as `plan`: the tail carries the
        // logits the sampler needs, and a state file carries none.
        let extended = self
            .entries
            .get(&key)
            .filter(|e| tokens.len() > e.tokens.len() && tokens[..e.tokens.len()] == e.tokens[..])
            .map(|e| e.tokens.len());
        if let Some(prefix_len) = extended {
            self.touch(key);
            let grown = tokens.len() - prefix_len;
            let repin = (grown >= self.min_pin).then_some(key);
            tracing::debug!(
                target: "prefix_state",
                key = format_args!("{key:016x}"),
                prompt_tokens = tokens.len(),
                prefix_len,
                grown,
                min_pin = self.min_pin,
                repin = repin.is_some(),
                "prefix_state: conversation turn extends its pin"
            );
            return (PrefixPlan::Restore { key, prefix_len }, repin);
        }
        tracing::info!(
            target: "prefix_state",
            key = format_args!("{key:016x}"),
            prompt_tokens = tokens.len(),
            entry_tokens = entry_len,
            "prefix_state: conversation turn has no pin it extends — full prefill, then pin it whole"
        );
        (PrefixPlan::Pass, Some(key))
    }

    /// Path a `Learn` plan should save the state file to.
    pub(crate) fn state_path(&self, key: u64) -> PathBuf {
        self.dir.join(format!("{key:016x}.state"))
    }

    /// Path a conversation re-pin saves to, named by the pinned CONTENT so
    /// it is distinct from the live entry's file (a re-pin never pins what
    /// the entry already holds): a failed save or a refused commit leaves
    /// the entry it would have replaced intact and serving.
    pub(crate) fn repin_path(&self, key: u64, pinned: &[LlamaToken]) -> PathBuf {
        let mut h = DefaultHasher::new();
        for t in pinned {
            t.0.hash(&mut h);
        }
        self.dir
            .join(format!("{key:016x}-{:016x}.state", h.finish()))
    }

    /// Ensure the state directory exists (call before saving).
    pub(crate) fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    /// Record a successfully saved pin. Evicts LRU overflow — by entry
    /// count AND by the per-slot byte budget (`SOVEREIGN_PREFIX_STATE_MAX_MB`)
    /// — deleting evicted state files best-effort. A pin whose file alone
    /// exceeds the whole budget is REFUSED (file deleted, nothing evicted):
    /// admitting it would flush every other family for one pin. Returns
    /// whether the pin was admitted.
    pub(crate) fn commit(
        &mut self,
        key: u64,
        prefix_tokens: Vec<LlamaToken>,
        path: PathBuf,
    ) -> bool {
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.commit_sized(key, prefix_tokens, path, bytes)
    }

    fn commit_sized(
        &mut self,
        key: u64,
        prefix_tokens: Vec<LlamaToken>,
        path: PathBuf,
        bytes: u64,
    ) -> bool {
        if bytes > self.max_bytes {
            tracing::warn!(
                target: "prefix_state",
                key = format_args!("{key:016x}"),
                bytes,
                budget = self.max_bytes,
                "prefix_state: pin larger than the whole byte budget — refused"
            );
            let _ = std::fs::remove_file(&path);
            return false;
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
        // The new key is the LRU's back, and the loop stops at one entry.
        true
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

// The tests live in a sibling file so this one stays under its arch-gate
// ceiling (ARCH §3.1). `#[path]`, so the test names are unchanged.
#[cfg(test)]
#[path = "prefix_state/tests.rs"]
mod tests;
