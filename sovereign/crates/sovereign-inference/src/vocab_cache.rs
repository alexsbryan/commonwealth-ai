//! Per-model byte-rendering of a `LlamaModel`'s vocab + the
//! non-Latin token bitmap derived from it.
//!
//! Both surfaces existed in `json_constraint.rs` until 2026-05-22.
//! The migration to llguidance retired that 5623-line module; these
//! two utilities are still needed by `url_constraint`,
//! `evidence_id_constraint`, `llguidance_constraint`, and
//! `embedded::build_sampler`, so they live here as the smallest
//! sibling module that keeps the shared-cache invariant intact.
//!
//! Cache key: raw `LlamaModel` pointer. Same model handle across
//! requests → same `Arc<Vec<Vec<u8>>>`. Persists for the daemon's
//! lifetime.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::llama::cpp::model::LlamaModel;
use crate::llama::cpp::token::LlamaToken;
// Shim-restored 0.1.x method names: `token_to_piece_bytes` lives on
// the trait. We need the lossless `Vec<u8>` form so URL and citation
// constraints can walk the same bytes the streaming generation loop
// emits.
use crate::llama::LlamaModelExt;

fn vocab_cache() -> &'static Mutex<HashMap<usize, Arc<Vec<Vec<u8>>>>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, Arc<Vec<Vec<u8>>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Per-model byte mapping for every vocab token. Shared by all
/// constraint engines that need to simulate token byte runs
/// (`url_constraint`, `evidence_id_constraint`, `llguidance_constraint`).
///
/// CRITICAL: rendered with `special=true` so user-defined / control
/// tokens render as their text. Diverging from `special=true` here
/// means the constraint tracks a different `emitted` buffer than what
/// the response decoder produces. Observed 2026-04-30 with gemma-4-E4B
/// Phase 1: response had `entities_introduced` followed by a literal
/// backtick where the closing quote should be, because the `special=false`
/// cache view made the mask believe the candidate was a no-op while the
/// response decoder rendered it as text.
pub fn vocab_bytes_for(model: &LlamaModel) -> Arc<Vec<Vec<u8>>> {
    let key = model as *const LlamaModel as usize;
    {
        let guard = vocab_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = guard.get(&key) {
            return v.clone();
        }
    }
    let n_vocab = model.n_vocab();
    let mut vocab_bytes = Vec::with_capacity(n_vocab as usize);
    for id in 0..n_vocab {
        // Buffer size: the streaming loop uses `token_to_piece` which
        // retries on InsufficientBufferSpace; we replicate that
        // explicitly. 32 is a generous starting size for the typical
        // BPE token (≤16 bytes); the retry handles the long tail.
        let bytes = match model.token_to_piece_bytes(LlamaToken(id), 32, true, None) {
            Ok(b) => b,
            Err(crate::llama::cpp::TokenToStringError::InsufficientBufferSpace(neg)) => {
                let needed = (-neg).try_into().unwrap_or(1024_usize).max(32);
                model
                    .token_to_piece_bytes(LlamaToken(id), needed, true, None)
                    .unwrap_or_default()
            }
            Err(_) => Vec::new(),
        };
        vocab_bytes.push(bytes);
    }
    let arc = Arc::new(vocab_bytes);
    let mut guard = vocab_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.get(&key) {
        return existing.clone();
    }
    guard.insert(key, arc.clone());
    arc
}

// ─── non-Latin token denylist ─────────────────────────────────────────

fn non_latin_denylist_cache() -> &'static Mutex<HashMap<usize, Arc<Vec<bool>>>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, Arc<Vec<bool>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Build a vocab-sized boolean bitmap where `true` means "the token's
/// rendered bytes contain a UTF-8 leading byte for a 3+ byte
/// sequence" (`0xE0..=0xF7`). Blocking these tokens makes CJK,
/// Devanagari, Hangul, Hiragana/Katakana, and other 3-byte+ scripts
/// unsampleable.
///
/// 2-byte UTF-8 leads (`0xC2..=0xDF` → Latin Extended, Greek,
/// Cyrillic, Arabic, Hebrew base) and ASCII pass through.
///
/// Used by `ConstrainedSampler::sample` on every inference path
/// when the operator enables `SOVEREIGN_BLOCK_NON_LATIN`. Default OFF.
pub fn non_latin_denylist_for(model: &LlamaModel) -> Arc<Vec<bool>> {
    let key = model as *const LlamaModel as usize;
    {
        let guard = non_latin_denylist_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(v) = guard.get(&key) {
            return v.clone();
        }
    }
    let vocab = vocab_bytes_for(model);
    let denylist = build_non_latin_denylist(&vocab);
    let arc = Arc::new(denylist);
    let mut guard = non_latin_denylist_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.get(&key) {
        return existing.clone();
    }
    guard.insert(key, arc.clone());
    arc
}

/// Pure function variant of the bitmap construction. Separate so unit
/// tests can exercise it against a synthetic vocab without loading a
/// real `LlamaModel`.
fn build_non_latin_denylist(vocab_bytes: &[Vec<u8>]) -> Vec<bool> {
    vocab_bytes
        .iter()
        .map(|bytes| bytes.iter().any(|b| (0xE0..=0xF7).contains(b)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::build_non_latin_denylist;

    #[test]
    fn ascii_passes_through() {
        let vocab = vec![b"hello".to_vec(), b"world".to_vec()];
        let deny = build_non_latin_denylist(&vocab);
        assert_eq!(deny, vec![false, false]);
    }

    #[test]
    fn cjk_lead_bytes_flagged() {
        // U+4E2D '中' encodes as 0xE4 0xB8 0xAD — leading byte 0xE4 is
        // in the 0xE0..=0xF7 deny range.
        let vocab = vec![vec![0xE4, 0xB8, 0xAD]];
        let deny = build_non_latin_denylist(&vocab);
        assert_eq!(deny, vec![true]);
    }

    #[test]
    fn two_byte_latin_extended_passes() {
        // U+00E9 'é' encodes as 0xC3 0xA9 — leading byte 0xC3 is below
        // the deny range (0xE0+), so passes through.
        let vocab = vec![vec![0xC3, 0xA9]];
        let deny = build_non_latin_denylist(&vocab);
        assert_eq!(deny, vec![false]);
    }
}
