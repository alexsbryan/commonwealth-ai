//! Evidence-id allowlist constraint for citation-faithful generation.
//!
//! Companion to [`crate::url_constraint::UrlAllowlistConstraint`].
//! Same architecture, applied to the `ev-Tn-NNNN` handles the
//! `knowledge_lookup` tool returns. The handles are addressable
//! cross-turn (Tier 1 result memory): a turn-N response can cite an
//! `[ev-T2-0001]` handle from turn 2, and the runtime dereferences
//! it to the original evidence row.
//!
//! The model sees these ids during prefill (in the dossier's
//! "Outcome history this conversation" section AND in the current
//! turn's tool result envelope) and is liable to extrapolate sibling
//! ids it didn't actually receive. Prompt rules nudge against this
//! but cannot eliminate it; token-level masking at sampling time
//! CAN. That's what this constraint does.
//!
//! Mechanism: build a byte-keyed trie of allowed ids prefixed by
//! their canonical bracket form (`[ev-T2-0001`). Track the emitted
//! byte stream. When the model is mid-citation, mask any vocab
//! token whose bytes would extend the cursor into a non-existent
//! trie path. Allow tokens that extend a valid prefix OR that
//! terminate the citation at a trie-terminal node (typically with
//! `]` but any of the URL-style terminators are accepted for
//! resilience to whitespace/punctuation variation).
//!
//! The state machine has two states:
//! - `InProse`: not inside a citation. Watch the emitted suffix for
//!   the `[ev-T` start marker; on match, transition to InId and
//!   walk the cursor through the marker bytes.
//! - `InId(node)`: emitting citation bytes. The cursor is at trie
//!   node `node`. Valid next bytes are the trie's children of
//!   `node`, plus citation-terminator bytes IF `node` is a
//!   terminal (meaning the id ends here cleanly).
//!
//! Per-token mask logic simulates feeding each candidate token's
//! bytes through this state machine on a clone of the current
//! state. Tokens that would leave the cursor in an INVALID state
//! get clamped to `-INFINITY`. Includes the URL constraint's
//! Phase-3c regression fix: EOS / empty-bytes tokens at non-terminal
//! cursor positions get clamped too, so the model cannot truncate
//! a citation mid-id.

use std::sync::Arc;

use crate::llama::cpp::token::data_array::LlamaTokenDataArray;
use crate::llama::cpp::token::LlamaToken;

/// Byte-keyed trie node. `children[b] = Some(idx)` means there's an
/// edge from this node to `nodes[idx]` on byte `b`. `is_terminal` =
/// true when the path from root to this node spells out a complete
/// allowed citation.
#[derive(Clone)]
struct TrieNode {
    children: Box<[Option<u32>; 256]>,
    is_terminal: bool,
}

impl Default for TrieNode {
    fn default() -> Self {
        Self {
            children: Box::new([None; 256]),
            is_terminal: false,
        }
    }
}

/// Bytes that legitimately terminate an evidence citation when seen
/// after a terminal trie node. `]` is canonical (the standard
/// closing-bracket form); the URL-style terminators are accepted
/// too so the model can emit `[ev-T2-0001 ` (trailing space) or
/// `[ev-T2-0001,` (in a list) without the constraint mistakenly
/// blocking the next byte.
fn is_id_terminator(b: u8) -> bool {
    b == b']'
        || b == b' '
        || b == b'\t'
        || b == b'\n'
        || b == b'\r'
        || b == b','
        || b == b'.'
        || b == b'('
        || b == b')'
        || b == b'<'
        || b == b'>'
        || b == b'"'
        || b == b'\''
        || b == b'?'
        || b == b'!'
        || b == b';'
        || b == b':'
}

/// Marker bytes that trigger the prose → in-id transition. The
/// opening bracket is part of the marker so we don't engage on a
/// bare `ev-T` mention in conversational prose ("an ev-T-like
/// scheme would..."). Real citations always come inside brackets.
const EV_START: &[u8] = b"[ev-T";
/// Sliding window length for the InProse cursor — long enough to
/// hold any prefix of `EV_START` that straddles a token boundary,
/// with headroom for token chunking quirks.
const EV_START_WATCH_BYTES: usize = 12;

/// Position in the constraint state machine.
#[derive(Clone)]
enum CursorMode {
    /// Outside a citation. Sliding window of the most recent
    /// ≤ `EV_START_WATCH_BYTES` emitted bytes.
    InProse(Vec<u8>),
    /// Inside a citation. The u32 is an index into `nodes`.
    InId(u32),
}

/// Per-request evidence-id allowlist constraint.
pub struct EvidenceIdAllowlistConstraint {
    nodes: Vec<TrieNode>,
    vocab_bytes: Arc<Vec<Vec<u8>>>,
    cursor: CursorMode,
}

impl EvidenceIdAllowlistConstraint {
    /// Construct a constraint over `allowed_ids`. Each id is the
    /// CANONICAL handle form (`ev-T0-0001`, no brackets). The
    /// constraint internally prefixes each entry with `[` so the
    /// trie walks from the opening bracket through the closing
    /// digit.
    ///
    /// Returns `None` if the allowlist is empty — no ids to
    /// constrain → no-op constraint, caller should treat None as
    /// "do not apply".
    pub fn new(
        allowed_ids: &[String],
        vocab_bytes: Arc<Vec<Vec<u8>>>,
    ) -> Option<Self> {
        if allowed_ids.is_empty() {
            return None;
        }
        let mut nodes: Vec<TrieNode> = vec![TrieNode::default()];
        for id in allowed_ids {
            let mut cur = 0usize;
            // Build the trie entry as `[<id>` so the cursor walks
            // from the opening bracket onward. The trailing `]` is
            // NOT in the trie — it's recognised as a terminator
            // when the cursor sits at a terminal node.
            let mut bracketed: Vec<u8> = Vec::with_capacity(id.len() + 1);
            bracketed.push(b'[');
            bracketed.extend_from_slice(id.as_bytes());
            for &b in &bracketed {
                let next = match nodes[cur].children[b as usize] {
                    Some(idx) => idx as usize,
                    None => {
                        let new_idx = nodes.len();
                        nodes.push(TrieNode::default());
                        nodes[cur].children[b as usize] = Some(new_idx as u32);
                        new_idx
                    }
                };
                cur = next;
            }
            nodes[cur].is_terminal = true;
        }
        Some(Self {
            nodes,
            vocab_bytes,
            cursor: CursorMode::InProse(Vec::with_capacity(EV_START_WATCH_BYTES)),
        })
    }

    /// Apply the mask to a token-data array. Tokens whose bytes
    /// would drive the cursor into an invalid state get logits
    /// clamped to `-INFINITY`.
    pub fn mask(&self, data: &mut LlamaTokenDataArray) {
        let vocab = &self.vocab_bytes;
        // Cursor mid-citation at a non-terminal trie node? Then
        // EOS/EOG tokens (empty bytes) MUST be masked too — the
        // model can otherwise terminate generation mid-id and
        // emit a truncated-prefix fabrication. Mirrors the URL
        // constraint's Phase-3c fix.
        let force_continue = matches!(
            &self.cursor,
            CursorMode::InId(node_idx) if !self.nodes[*node_idx as usize].is_terminal,
        );
        for c in data.data.iter_mut() {
            let id = c.id().0 as usize;
            if id >= vocab.len() {
                continue;
            }
            let bytes = &vocab[id];
            if bytes.is_empty() {
                if force_continue {
                    c.set_logit(f32::NEG_INFINITY);
                }
                continue;
            }
            let mut sim = self.cursor.clone();
            if !simulate_bytes(&self.nodes, &mut sim, bytes) {
                c.set_logit(f32::NEG_INFINITY);
            }
        }
    }

    /// Advance the state machine on the chosen token.
    pub fn accept(&mut self, token: LlamaToken) {
        let id = token.0 as usize;
        if id >= self.vocab_bytes.len() {
            return;
        }
        let bytes = self.vocab_bytes[id].clone();
        let _ = simulate_bytes(&self.nodes, &mut self.cursor, &bytes);
    }

    /// Test-only accessor: is the cursor currently emitting a
    /// citation?
    #[cfg(test)]
    fn in_id_mode(&self) -> bool {
        matches!(self.cursor, CursorMode::InId(_))
    }
}

fn simulate_bytes(nodes: &[TrieNode], cursor: &mut CursorMode, bytes: &[u8]) -> bool {
    for &b in bytes {
        if !feed_byte(nodes, cursor, b) {
            return false;
        }
    }
    true
}

fn feed_byte(nodes: &[TrieNode], cursor: &mut CursorMode, b: u8) -> bool {
    match cursor {
        CursorMode::InProse(window) => {
            window.push(b);
            if window.len() > EV_START_WATCH_BYTES {
                let drain_n = window.len() - EV_START_WATCH_BYTES;
                window.drain(..drain_n);
            }
            if window.ends_with(EV_START) {
                let walked = walk_marker(nodes, EV_START);
                match walked {
                    Some(node_idx) => {
                        *cursor = CursorMode::InId(node_idx);
                    }
                    None => {
                        // No allowed citation starts with the
                        // marker (allowlist is non-empty but the
                        // trie doesn't actually contain `[ev-T` —
                        // shouldn't happen with valid ids but
                        // guards against malformed input).
                        return false;
                    }
                }
            }
            true
        }
        CursorMode::InId(node_idx) => {
            let cur = *node_idx as usize;
            match nodes[cur].children[b as usize] {
                Some(next) => {
                    *cursor = CursorMode::InId(next);
                    true
                }
                None => {
                    if nodes[cur].is_terminal && is_id_terminator(b) {
                        *cursor = CursorMode::InProse(Vec::with_capacity(EV_START_WATCH_BYTES));
                        true
                    } else {
                        false
                    }
                }
            }
        }
    }
}

fn walk_marker(nodes: &[TrieNode], marker: &[u8]) -> Option<u32> {
    let mut cur = 0u32;
    for &b in marker {
        match nodes[cur as usize].children[b as usize] {
            Some(next) => cur = next,
            None => return None,
        }
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(ids: &[&str]) -> EvidenceIdAllowlistConstraint {
        let vocab_bytes = Arc::new(vec![Vec::new(); 1]);
        let owned: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        EvidenceIdAllowlistConstraint::new(&owned, vocab_bytes).expect("non-empty allowlist")
    }

    fn feed(c: &mut EvidenceIdAllowlistConstraint, s: &str) -> bool {
        simulate_bytes(&c.nodes, &mut c.cursor, s.as_bytes())
    }

    #[test]
    fn empty_allowlist_returns_none() {
        assert!(EvidenceIdAllowlistConstraint::new(&[], Arc::new(Vec::new())).is_none());
    }

    #[test]
    fn prose_bytes_are_always_accepted() {
        let mut c = build(&["ev-T0-0001"]);
        assert!(feed(&mut c, "Hello world, this is some prose. "));
        assert!(!c.in_id_mode());
    }

    #[test]
    fn allowed_id_accepted_then_terminator_returns_to_prose() {
        let mut c = build(&["ev-T0-0001"]);
        assert!(feed(&mut c, "see [ev-T0-0001] for details"));
        assert!(!c.in_id_mode(), "id should have terminated on ']'");
    }

    #[test]
    fn fabricated_id_rejected_wrong_index() {
        // Allowlist has 0001. Model tries 0099.
        let mut c = build(&["ev-T0-0001"]);
        assert!(!feed(&mut c, "[ev-T0-0099]"));
    }

    #[test]
    fn fabricated_id_rejected_wrong_turn() {
        // Allowlist has T0. Model tries T5.
        let mut c = build(&["ev-T0-0001"]);
        assert!(!feed(&mut c, "[ev-T5-0001]"));
    }

    #[test]
    fn id_extension_past_terminal_rejected() {
        // Allowlist has 0001 (4 digits). Model tries 0001 then
        // an extra digit before the bracket — fabrication.
        let mut c = build(&["ev-T0-0001"]);
        assert!(feed(&mut c, "[ev-T0-000"));
        assert!(c.in_id_mode());
        // Valid: "1]" (terminator after the terminal). We're
        // testing rejection of "12" (extra digit, no terminator,
        // not in the trie).
        assert!(!feed(&mut c, "12"));
    }

    #[test]
    fn multi_turn_ids_share_trie_correctly() {
        let mut c = build(&["ev-T2-0001", "ev-T3-0001", "ev-T22-0000"]);
        // T2 path.
        assert!(feed(&mut c, "[ev-T2-0001] "));
        assert!(!c.in_id_mode());
        // T3 path.
        assert!(feed(&mut c, "and [ev-T3-0001] "));
        assert!(!c.in_id_mode());
        // T22 path (two-digit turn).
        let mut c2 = build(&["ev-T2-0001", "ev-T3-0001", "ev-T22-0000"]);
        assert!(feed(&mut c2, "[ev-T22-0000]"));
        assert!(!c2.in_id_mode());
    }

    #[test]
    fn two_digit_turn_does_not_collide_with_single_digit() {
        // T2 and T22 must coexist: the trie has both `[ev-T2-` and
        // `[ev-T22-` paths from the `[ev-T` marker. Model emitting
        // T22 should walk through `T2` → `2` (extending instead of
        // terminating).
        let mut c = build(&["ev-T2-0001", "ev-T22-0000"]);
        // T22 first — extending T2's path.
        assert!(feed(&mut c, "[ev-T22-0000]"));
        assert!(!c.in_id_mode());
    }

    #[test]
    fn empty_trie_marker_walk_succeeds_if_some_id_matches() {
        // Sanity: any non-empty allowlist must let the marker walk.
        let mut c = build(&["ev-T0-0001"]);
        // Walk just the marker as prose → should transition to InId.
        for &b in b"[ev-T" {
            assert!(feed_byte(&c.nodes, &mut c.cursor, b));
        }
        assert!(c.in_id_mode());
    }

    #[test]
    fn marker_straddling_byte_boundary() {
        // Tokens commonly chunk `[ev-T` into multiple pieces.
        let mut c = build(&["ev-T2-0001"]);
        for byte in "[ev-T2-0001]".as_bytes() {
            assert!(
                feed_byte(&c.nodes, &mut c.cursor, *byte),
                "byte {:?} should be accepted",
                *byte as char
            );
        }
    }

    #[test]
    fn second_id_after_first_completes() {
        let mut c = build(&["ev-T0-0001", "ev-T0-0002"]);
        assert!(feed(
            &mut c,
            "first [ev-T0-0001], then [ev-T0-0002]."
        ));
        assert!(!c.in_id_mode());
    }

    /// Regression: mirrors URL constraint's
    /// `empty_bytes_token_masked_mid_url_non_terminal`. EOS at a
    /// non-terminal cursor position must clamp to -INFINITY so the
    /// model can't truncate a citation mid-id; EOS at a terminal
    /// position must remain allowed so generation can stop cleanly
    /// once the id is complete.
    #[test]
    fn empty_bytes_token_masked_mid_id_non_terminal() {
        let vocab: Vec<Vec<u8>> = vec![Vec::new(), vec![b'1']];
        let ids = vec!["ev-T0-0001".to_string()];
        let mut c =
            EvidenceIdAllowlistConstraint::new(&ids, Arc::new(vocab.clone())).expect("non-empty");
        // Walk cursor up to but NOT including the terminal `1`.
        for byte in "[ev-T0-000".as_bytes() {
            assert!(
                feed_byte(&c.nodes, &mut c.cursor, *byte),
                "prefix byte {:?} should be accepted",
                *byte as char
            );
        }
        assert!(c.in_id_mode(), "cursor should still be inside citation");

        // Mid-id at non-terminal: EOS must be clamped, byte `1`
        // must remain valid.
        let mut data = LlamaTokenDataArray::from_iter(
            vec![
                crate::llama::cpp::token::data::LlamaTokenData::new(
                    LlamaToken(0),
                    0.0,
                    0.0,
                ),
                crate::llama::cpp::token::data::LlamaTokenData::new(
                    LlamaToken(1),
                    0.0,
                    0.0,
                ),
            ],
            false,
        );
        c.mask(&mut data);
        assert_eq!(
            data.data[0].logit(),
            f32::NEG_INFINITY,
            "EOS at non-terminal node must be masked"
        );
        assert_ne!(
            data.data[1].logit(),
            f32::NEG_INFINITY,
            "the byte that completes the citation must remain valid"
        );

        // Walk to terminal. EOS at terminal must STAY allowed.
        assert!(feed_byte(&c.nodes, &mut c.cursor, b'1'));
        let mut data = LlamaTokenDataArray::from_iter(
            vec![crate::llama::cpp::token::data::LlamaTokenData::new(
                LlamaToken(0),
                0.0,
                0.0,
            )],
            false,
        );
        c.mask(&mut data);
        assert_ne!(
            data.data[0].logit(),
            f32::NEG_INFINITY,
            "EOS at terminal node must remain allowed"
        );
    }
}
