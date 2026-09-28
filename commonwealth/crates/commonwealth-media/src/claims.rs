// SPDX-License-Identifier: AGPL-3.0-or-later
//! The claim lifecycle — claim, TTL, renew, release, no history — once, for
//! every registry that publishes something a live process owns.
//!
//! Split out of [`crate::apps`] when the origin registry ([`crate::origins`])
//! needed the same lifecycle for ALPNs and path prefixes (phase-b
//! pb-rails-origins). One implementation, so an app claim and an origin
//! claim cannot disagree about when a TTL has run out (ARCH principle 8).
//!
//! A claim holds one or more KEYS under one id: an app claim holds its name,
//! an origin claim holds its ALPN or every prefix it registered. Renew and
//! release act on the id, so a registrant never has to track its keys.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// TTL a claim gets when the caller names none. An hour: long enough that a
/// laptop asleep for a coffee break does not lose its publish, short enough
/// that a `kill -9`'d runner is gone before anyone notices it in a fan-out.
pub const DEFAULT_CLAIM_TTL: Duration = Duration::from_secs(3600);
/// The longest TTL a claim may ask for, matching the work atlas's own cap.
/// Beyond this, the honest shape is a durable assertion with an owner, not a
/// claim renewed by nobody.
pub const MAX_CLAIM_TTL: Duration = Duration::from_secs(24 * 3600);

/// One claimed key: which claim holds it, what it publishes, and until when.
pub(crate) struct Row<V> {
    pub id: String,
    pub value: V,
    pub deadline: Instant,
}

/// The live claims of one registry, by key.
pub(crate) struct Claims<V> {
    rows: BTreeMap<String, Row<V>>,
}

impl<V> Default for Claims<V> {
    fn default() -> Self {
        Self {
            rows: BTreeMap::new(),
        }
    }
}

impl<V> Claims<V> {
    /// Drop every expired row and hand them back, so the registry that owns
    /// them can say in its own words what stopped being published. Called on
    /// every read as well as every write, so expiry is observed by the next
    /// question anyone asks rather than by a timer that has to be running —
    /// a registry whose correctness depends on a background task is a
    /// registry that is wrong whenever that task dies (ARCH principle 10).
    pub fn sweep(&mut self, now: Instant) -> Vec<(String, Row<V>)> {
        let expired: Vec<String> = self
            .rows
            .iter()
            .filter(|(_, row)| row.deadline <= now)
            .map(|(key, _)| key.clone())
            .collect();
        expired
            .into_iter()
            .filter_map(|key| self.rows.remove(&key).map(|row| (key, row)))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.rows.contains_key(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Row<V>)> {
        self.rows.iter()
    }

    /// Take `key` under claim `id` for `ttl` (capped at [`MAX_CLAIM_TTL`]).
    /// The caller has already refused a key that is taken.
    pub fn insert(&mut self, key: String, id: &str, value: V, ttl: Duration) {
        self.rows.insert(
            key,
            Row {
                id: id.to_string(),
                value,
                deadline: Instant::now() + ttl.min(MAX_CLAIM_TTL),
            },
        );
    }

    /// Every key claim `id` holds, in key order. Empty when there is no such
    /// live claim.
    pub fn keys_of(&self, id: &str) -> Vec<String> {
        self.rows
            .iter()
            .filter(|(_, row)| row.id == id)
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Push every row of claim `id` out to `ttl` from now. `None` when there
    /// is no such live claim.
    pub fn renew(&mut self, id: &str, ttl: Duration) -> Option<Vec<String>> {
        let keys = self.keys_of(id);
        if keys.is_empty() {
            return None;
        }
        let deadline = Instant::now() + ttl.min(MAX_CLAIM_TTL);
        for key in &keys {
            if let Some(row) = self.rows.get_mut(key) {
                row.deadline = deadline;
            }
        }
        Some(keys)
    }

    /// Remove every row of claim `id`. `None` when there is no such live
    /// claim.
    pub fn release(&mut self, id: &str) -> Option<Vec<(String, V)>> {
        let keys = self.keys_of(id);
        if keys.is_empty() {
            return None;
        }
        Some(
            keys.into_iter()
                .filter_map(|key| self.rows.remove(&key).map(|row| (key, row.value)))
                .collect(),
        )
    }

    pub fn get(&self, key: &str) -> Option<&Row<V>> {
        self.rows.get(key)
    }
}

/// A claim id: the key it holds, plus enough entropy that two runners of the
/// same app on one box cannot collide.
///
/// Identity from essence and a random seed, never a counter (ARCH principle
/// 8). It is not a secret and is not treated as one — every surface that
/// takes it is loopback-only, and any process that can present a claim id
/// could have taken the claim itself.
pub(crate) fn mint_claim_id(name: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write(name.as_bytes());
    // The decider, not a hand-read clock (clock-gate). Milliseconds rather
    // than the nanoseconds this read before: the collision defence here is
    // `RandomState::new()`'s per-call seed plus the name — the doc above says
    // so — and the timestamp is a secondary source, so the six orders of
    // magnitude buy nothing a random seed is not already buying.
    h.write_u64(commonwealth_core::clock::unix_now_millis());
    format!("{name}-{:012x}", h.finish() & 0xffff_ffff_ffff)
}
