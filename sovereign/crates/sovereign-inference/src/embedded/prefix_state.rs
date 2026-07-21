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
//! `(prefix-fingerprint → pinned token prefix + state file)` with an
//! auto-learned pin boundary — the longest common prefix of two
//! sightings of the same request family. The boundary lands exactly
//! where requests start to diverge (in practice: the byte-stable
//! synthesis system core, ~2.5k tokens, ends right where the varying
//! budget directive splices in — measured 2026-07-12, prefill audit).
//! No caller cooperation needed: keying is by the first
//! [`PROBE_TOKENS`] of the tokenized prompt, which separates the
//! per-handoff prompt families (synthesis / gate / gap-check / router)
//! without any API change.
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
//! **Default OFF (opt-in via `SOVEREIGN_PREFIX_STATE=1`).** The
//! 2026-07-12 prefill A/B measured the mechanism correct and safe but
//! worth ≈0 wall-clock at current turn anatomy: synthesis prefill runs
//! ~800 tok/s, so the ~2.7k-token stable prefix is ~3.4s inside 40-180s
//! turns owned by retrieval fan-out and housekeeping. Not worth 172MB
//! state saves in production. The mechanism is kept (spike-verified
//! 116-145x restore-vs-prefill on both DeltaNet hybrids) as the
//! foundation for cartridges, where pinned prefixes are 10k+ tokens.
//! Pin floor override: `SOVEREIGN_PREFIX_STATE_MIN=<tokens>`.
//!
//! Decision logic is pure and unit-tested weight-free below; all file
//! and context IO stays in `model_slot.rs` where the decode paths live.

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

/// Per-slot entry cap. Distinct request families per slot in practice:
/// synthesis primary/fast variants, gate verifier, gap check, router
/// coarse, title — six covers the live set with headroom.
const MAX_ENTRIES: usize = 6;

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
    /// boundary from. Bounded alongside `entries`.
    last_seen: HashMap<u64, Vec<LlamaToken>>,
}

fn env_enabled() -> bool {
    matches!(
        std::env::var("SOVEREIGN_PREFIX_STATE").as_deref(),
        Ok("1") | Ok("true") | Ok("on")
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

    fn key(tokens: &[LlamaToken]) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for t in &tokens[..PROBE_TOKENS] {
            t.0.hash(&mut h);
        }
        h.finish()
    }

    /// Decide the cache interaction for this request's token stream.
    /// Mutates only the in-memory learning state (`last_seen`, LRU
    /// touch); file IO is the caller's.
    pub(crate) fn plan(&mut self, tokens: &[LlamaToken]) -> PrefixPlan {
        if !self.enabled || tokens.len() < self.min_pin.max(PROBE_TOKENS) + 8 {
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
            if lcp >= self.min_pin && lcp < tokens.len() {
                return PrefixPlan::Learn { key, pin_len: lcp };
            }
            self.invalidate(key);
            self.last_seen.insert(key, tokens.to_vec());
            return PrefixPlan::Pass;
        }

        if let Some(prev) = self.last_seen.get(&key) {
            let lcp = lcp_len(prev, tokens);
            if lcp >= self.min_pin && lcp < tokens.len() {
                self.last_seen.remove(&key);
                return PrefixPlan::Learn { key, pin_len: lcp };
            }
            // Same family fingerprint but the shared prefix is too
            // short to pin — keep the newest sighting.
            self.last_seen.insert(key, tokens.to_vec());
            return PrefixPlan::Pass;
        }

        // First sighting of this family.
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
    /// needed — an unusable/missing entry learns IMMEDIATELY at the
    /// directed boundary. This removes the auto-learn path's two costs
    /// for declared families: the extra full prefill of the first
    /// sighting, and relearn churn when the auto boundary lands inside
    /// shared claim-opening text (observed 2026-07-21).
    ///
    /// A matching entry restores at ITS length even if it differs from
    /// the directed boundary — restoring more matched tokens is strictly
    /// better, and a stale-but-strict-prefix entry is still bit-faithful.
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
        let key = Self::key(tokens);
        if let Some(entry) = self.entries.get(&key) {
            let entry_len = entry.tokens.len();
            if tokens.len() > entry_len && tokens[..entry_len] == entry.tokens[..] {
                self.touch(key);
                return PrefixPlan::Restore {
                    key,
                    prefix_len: entry_len,
                };
            }
        }
        // No usable entry (first sighting of this evidence, or the
        // family drifted to new evidence): learn NOW at the directed
        // boundary. `commit` replaces any stale entry under this key.
        self.last_seen.remove(&key);
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

    fn commit_sized(&mut self, key: u64, prefix_tokens: Vec<LlamaToken>, path: PathBuf, bytes: u64) {
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
        // The tail carries the fresh logits; an exact-match prompt must
        // not plan a zero-tail restore.
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = family_member(200, 1, 40);
        cache.plan(&a);
        let plan = cache.plan(&a.clone());
        // Identical twice → lcp == len → not pinnable (lcp < len fails).
        assert_eq!(plan, PrefixPlan::Pass);
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
            cache.commit_sized(key, a[..PROBE_TOKENS + 100].to_vec(), cache.state_path(key), 400);
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
        cache.commit_sized(k_small, small[..PROBE_TOKENS + 100].to_vec(), cache.state_path(k_small), 400);

        // A pin bigger than the WHOLE budget: refused, and the resident
        // small pin survives (admitting would have flushed everything).
        let big = toks(2, &core);
        let k_big = PrefixStateCache::key(&big);
        cache.commit_sized(k_big, big[..PROBE_TOKENS + 100].to_vec(), cache.state_path(k_big), 5_000);
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

    #[test]
    fn directed_replaces_drifted_entry_without_sighting_dance() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = family_member(200, 1, 40);
        let pin_a = PROBE_TOKENS + 180;
        let PrefixPlan::Learn { key, .. } = cache.plan_directed(&a, pin_a) else {
            panic!("expected Learn");
        };
        cache.commit(key, a[..pin_a].to_vec(), cache.state_path(key));

        // Next turn: same family fingerprint, new evidence (core drifts
        // right after the probe) — directed plan learns at the NEW
        // boundary immediately instead of invalidate → Pass → Learn.
        let mut c = toks(1, &(500..700).collect::<Vec<i32>>());
        c.extend([LlamaToken(1), LlamaToken(2), LlamaToken(3)]);
        let pin_c = PROBE_TOKENS + 150;
        assert_eq!(
            cache.plan_directed(&c, pin_c),
            PrefixPlan::Learn {
                key,
                pin_len: pin_c
            }
        );
    }

    #[test]
    fn directed_out_of_range_falls_back_to_sighting_plan() {
        let mut cache = PrefixStateCache::new_for_test(64);
        let a = family_member(200, 1, 40);
        // Pin below min_pin and pin past the end both degrade to the
        // sighting-based plan (first sighting → Pass, no learn).
        assert_eq!(cache.plan_directed(&a, 8), PrefixPlan::Pass);
        assert_eq!(cache.plan_directed(&a, a.len() + 5), PrefixPlan::Pass);
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
