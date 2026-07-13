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

pub(crate) struct PinnedPrefix {
    pub(crate) tokens: Vec<LlamaToken>,
    pub(crate) path: PathBuf,
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

fn lcp_len(a: &[LlamaToken], b: &[LlamaToken]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

impl PrefixStateCache {
    pub(crate) fn new(slot_label: &str) -> Self {
        let sanitized: String = slot_label
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        let dir = std::env::temp_dir()
            .join("sovereign-prefix-state")
            .join(format!("{}-{}", std::process::id(), sanitized));
        Self {
            enabled: env_enabled(),
            min_pin: env_min_pin(),
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

    /// Path a `Learn` plan should save the state file to.
    pub(crate) fn state_path(&self, key: u64) -> PathBuf {
        self.dir.join(format!("{key:016x}.state"))
    }

    /// Ensure the state directory exists (call before saving).
    pub(crate) fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    /// Record a successfully saved pin. Evicts LRU overflow (and its
    /// file, best-effort).
    pub(crate) fn commit(&mut self, key: u64, prefix_tokens: Vec<LlamaToken>, path: PathBuf) {
        self.entries.insert(
            key,
            PinnedPrefix {
                tokens: prefix_tokens,
                path,
            },
        );
        self.lru.retain(|k| *k != key);
        self.lru.push_back(key);
        while self.lru.len() > MAX_ENTRIES {
            if let Some(old) = self.lru.pop_front() {
                if let Some(e) = self.entries.remove(&old) {
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
    fn disabled_via_env_shape_passes_everything() {
        let mut cache = PrefixStateCache::new_for_test(64);
        cache.enabled = false;
        let a = family_member(200, 1, 40);
        let b = family_member(200, 2, 55);
        assert_eq!(cache.plan(&a), PrefixPlan::Pass);
        assert_eq!(cache.plan(&b), PrefixPlan::Pass);
    }
}
