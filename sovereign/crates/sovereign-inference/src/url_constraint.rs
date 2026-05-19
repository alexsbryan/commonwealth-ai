//! URL-allowlist constraint enforcer for citation-faithful generation.
//!
//! When the model receives a tool result containing
//! `"https://realsite.io/path-A"`, it learns the URL shape during
//! prefill and is liable to emit a sibling like
//! `"https://realsite.io/path-B"` during decode — pure pattern
//! extrapolation. Observed empirically during search-gym Phase 3c
//! (2026-05-19): the model emitted `/after-years` and `/after-posts`
//! when only `/after-hours` was a real URL. Prompt rules nudge against
//! this but cannot eliminate it; token-level masking at sampling time
//! CAN.
//!
//! Mechanism: build a byte-keyed trie of allowed URLs. Track the
//! emitted byte stream. When the model is mid-URL emission, mask any
//! vocab token whose bytes would extend the cursor into a non-existent
//! trie path. Allow tokens that extend a valid prefix OR that
//! terminate the URL at a trie-terminal node (followed by a non-URL
//! byte like whitespace or punctuation).
//!
//! The state machine has two states:
//! - `InProse`: not inside a URL. Watch the emitted suffix for the
//!   `http://` or `https://` start marker; on match, transition to
//!   InUrl and walk the cursor through the marker bytes.
//! - `InUrl(node)`: emitting URL bytes. The cursor is at trie node
//!   `node`. Valid next bytes are the trie's children of `node`, plus
//!   URL-terminator bytes IF `node` is a terminal (meaning the URL
//!   ends here cleanly).
//!
//! Per-token mask logic simulates feeding each candidate token's
//! bytes through this state machine on a clone of the current state.
//! Tokens that would leave the cursor in an INVALID state get clamped
//! to `-INFINITY`.

use std::sync::Arc;

use crate::llama::cpp::token::data_array::LlamaTokenDataArray;
use crate::llama::cpp::token::LlamaToken;

/// Byte-keyed trie node. `children[b] = Some(idx)` means there's an
/// edge from this node to `nodes[idx]` on byte `b`. `is_terminal` =
/// true when the path from root to this node spells out a complete
/// allowed URL.
#[derive(Clone)]
struct TrieNode {
    /// `[None; 256]` initially. Populated as URLs are inserted.
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

/// Bytes that legitimately terminate a URL when seen after a
/// terminal trie node. Anything else, when the cursor is at a
/// non-terminal node, is invalid (the model is trying to extend a
/// URL with a byte that has no trie edge).
fn is_url_terminator(b: u8) -> bool {
    b == b' '
        || b == b'\t'
        || b == b'\n'
        || b == b'\r'
        || b == b','
        || b == b'.'
        || b == b'<'
        || b == b'>'
        || b == b'('
        || b == b')'
        || b == b'['
        || b == b']'
        || b == b'"'
        || b == b'\''
        || b == b'?'
        || b == b'!'
        || b == b';'
        || b == b':'
}

const HTTPS_START: &[u8] = b"https://";
const HTTP_START: &[u8] = b"http://";
const URL_START_WATCH_BYTES: usize = 16;

/// Position in the constraint state machine. Cheap to clone for the
/// per-candidate simulation inside `mask()`.
#[derive(Clone)]
enum CursorMode {
    /// Outside a URL. The contained Vec is a sliding window of the
    /// most recent ≤16 emitted bytes — long enough to recognise an
    /// `https://` start that may straddle a token boundary.
    InProse(Vec<u8>),
    /// Inside a URL. The u32 is an index into `nodes`.
    InUrl(u32),
}

/// Per-request URL-allowlist constraint.
pub struct UrlAllowlistConstraint {
    /// Flat-array trie. Node 0 is the root. Bytes index into
    /// `children[b]`; following the edge gives the next node index.
    nodes: Vec<TrieNode>,
    /// Vocab byte representations, indexed by token id. Built once at
    /// construction by walking 0..n_vocab and calling `token_to_piece`.
    vocab_bytes: Arc<Vec<Vec<u8>>>,
    /// Current state.
    cursor: CursorMode,
}

impl UrlAllowlistConstraint {
    /// Construct a constraint over `allowed_urls`. Returns `None` if
    /// the allowlist is empty (no URLs to constrain → no-op constraint
    /// is wasteful to keep around; caller should treat None as "do
    /// not apply").
    pub fn new(
        allowed_urls: &[String],
        vocab_bytes: Arc<Vec<Vec<u8>>>,
    ) -> Option<Self> {
        if allowed_urls.is_empty() {
            return None;
        }
        let mut nodes: Vec<TrieNode> = vec![TrieNode::default()];
        for url in allowed_urls {
            let mut cur = 0usize;
            for &b in url.as_bytes() {
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
            cursor: CursorMode::InProse(Vec::with_capacity(URL_START_WATCH_BYTES)),
        })
    }

    /// Apply the mask to a token-data array. Tokens whose bytes would
    /// drive the cursor into an invalid state get logits clamped to
    /// `-INFINITY`. Tokens whose bytes would keep the cursor valid
    /// (whether by extending a URL prefix, terminating a URL cleanly,
    /// or staying in prose) are left untouched.
    pub fn mask(&self, data: &mut LlamaTokenDataArray) {
        let vocab = &self.vocab_bytes;
        // Pre-compute "is the cursor mid-URL at a non-terminal trie
        // node?" — if so, the URL is incomplete and EOS/EOG tokens
        // (rendered as empty bytes) MUST be masked too, otherwise the
        // model can terminate generation mid-URL and produce a
        // truncated-prefix fabrication like `/after-hour` when the
        // allowlist only carries `/after-hours`. Observed empirically
        // in search-gym Phase 3c (2026-05-19, fixtures 07 + 08).
        let force_continue = matches!(
            &self.cursor,
            CursorMode::InUrl(node_idx) if !self.nodes[*node_idx as usize].is_terminal,
        );
        for c in data.data.iter_mut() {
            let id = c.id().0 as usize;
            if id >= vocab.len() {
                continue;
            }
            let bytes = &vocab[id];
            if bytes.is_empty() {
                // Special tokens (EOS / EOG / BOS / etc.) render as
                // empty bytes when fetched via `token_to_piece`.
                // Outside URL mode they're harmless; mid-URL at a
                // non-terminal they truncate the citation.
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
        // For commit, panicking on invalid would be too noisy — if the
        // upstream chain produced a token we'd masked, log and ignore
        // (the rest of the chain is fault-tolerant).
        let _ = simulate_bytes(&self.nodes, &mut self.cursor, &bytes);
    }

    /// Test-only accessor: are we currently in URL-emission mode?
    #[cfg(test)]
    fn in_url_mode(&self) -> bool {
        matches!(self.cursor, CursorMode::InUrl(_))
    }
}

/// Feed `bytes` through `cursor` in place. Returns `true` if every
/// byte was accepted; `false` if any byte broke the state machine
/// (i.e. extended a URL into an invalid trie path).
fn simulate_bytes(nodes: &[TrieNode], cursor: &mut CursorMode, bytes: &[u8]) -> bool {
    for &b in bytes {
        if !feed_byte(nodes, cursor, b) {
            return false;
        }
    }
    true
}

/// Single-byte step. Returns `true` if the byte is valid in the
/// current state, `false` otherwise.
fn feed_byte(nodes: &[TrieNode], cursor: &mut CursorMode, b: u8) -> bool {
    match cursor {
        CursorMode::InProse(window) => {
            // In prose, all bytes are allowed by default. We watch for
            // the URL-start marker and transition when matched.
            window.push(b);
            if window.len() > URL_START_WATCH_BYTES {
                let drain_n = window.len() - URL_START_WATCH_BYTES;
                window.drain(..drain_n);
            }
            if window.ends_with(HTTPS_START) {
                let walked = walk_marker(nodes, HTTPS_START);
                match walked {
                    Some(node_idx) => {
                        *cursor = CursorMode::InUrl(node_idx);
                    }
                    None => {
                        // No allowed URL starts with `https://`. The
                        // model has emitted these bytes anyway; we
                        // reject this transition. Caller will mask
                        // the offending token.
                        return false;
                    }
                }
            } else if window.ends_with(HTTP_START) {
                let walked = walk_marker(nodes, HTTP_START);
                match walked {
                    Some(node_idx) => {
                        *cursor = CursorMode::InUrl(node_idx);
                    }
                    None => {
                        return false;
                    }
                }
            }
            true
        }
        CursorMode::InUrl(node_idx) => {
            let cur = *node_idx as usize;
            match nodes[cur].children[b as usize] {
                Some(next) => {
                    *cursor = CursorMode::InUrl(next);
                    true
                }
                None => {
                    // No trie edge for this byte. Either the URL ends
                    // here (terminal + URL-terminator byte) or this is
                    // an invalid extension.
                    if nodes[cur].is_terminal && is_url_terminator(b) {
                        // URL completes cleanly. Drop back to prose
                        // and reprocess `b` there (it might itself
                        // start a new prose context).
                        *cursor = CursorMode::InProse(Vec::with_capacity(URL_START_WATCH_BYTES));
                        // Don't recursively feed `b` — it's a
                        // terminator and shouldn't trigger another
                        // URL start on its own.
                        true
                    } else {
                        false
                    }
                }
            }
        }
    }
}

/// Walk the trie from root through `marker`'s bytes. Returns the
/// final node index if every byte has an edge; `None` if any byte
/// has no edge (meaning the marker is not a valid URL prefix in this
/// allowlist).
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

    fn build(urls: &[&str]) -> UrlAllowlistConstraint {
        let vocab_bytes = Arc::new(vec![Vec::new(); 1]); // placeholder, tests don't mask
        let owned: Vec<String> = urls.iter().map(|s| s.to_string()).collect();
        UrlAllowlistConstraint::new(&owned, vocab_bytes).expect("non-empty allowlist")
    }

    fn feed(c: &mut UrlAllowlistConstraint, s: &str) -> bool {
        simulate_bytes(&c.nodes, &mut c.cursor, s.as_bytes())
    }

    #[test]
    fn empty_allowlist_returns_none() {
        assert!(UrlAllowlistConstraint::new(&[], Arc::new(Vec::new())).is_none());
    }

    #[test]
    fn prose_bytes_are_always_accepted() {
        let mut c = build(&["https://a.test/x"]);
        assert!(feed(&mut c, "Hello world, this is some prose. "));
        assert!(!c.in_url_mode());
    }

    #[test]
    fn allowed_url_accepted_then_terminator_returns_to_prose() {
        let mut c = build(&["https://a.test/x"]);
        assert!(feed(&mut c, "see https://a.test/x for details"));
        assert!(!c.in_url_mode(), "URL should have terminated on space");
    }

    #[test]
    fn fabricated_url_rejected() {
        // Allowlist has /x, model tries to emit /y instead.
        let mut c = build(&["https://a.test/x"]);
        assert!(!feed(&mut c, "https://a.test/y"));
    }

    #[test]
    fn url_extension_past_terminal_rejected() {
        // Allowlist has /x exactly. Model tries to extend it to /xz
        // (i.e. emit extra chars after the terminal node without a
        // URL terminator first). That's a fabrication.
        let mut c = build(&["https://a.test/x"]);
        // First eat the valid prefix
        assert!(feed(&mut c, "see https://a.test/"));
        assert!(c.in_url_mode());
        // Now valid: "x " (terminator) works. We're testing the
        // rejection of "xz" (no terminator, byte not in trie).
        assert!(!feed(&mut c, "xz"));
    }

    #[test]
    fn prefix_sharing_urls_both_accepted() {
        // /a is a prefix of /ab. Both should be reachable + terminal.
        let mut c = build(&["https://x.test/a", "https://x.test/ab"]);
        // Path /a + space → ok.
        assert!(feed(&mut c, "https://x.test/a "));
        assert!(!c.in_url_mode());
        // Reset (new constraint) and check /ab.
        let mut c2 = build(&["https://x.test/a", "https://x.test/ab"]);
        assert!(feed(&mut c2, "https://x.test/ab "));
        assert!(!c2.in_url_mode());
    }

    #[test]
    fn url_inside_markdown_link_form() {
        // [label](https://a.test/x) — the brackets and paren are URL
        // terminators per is_url_terminator.
        let mut c = build(&["https://a.test/x"]);
        assert!(feed(&mut c, "see [label](https://a.test/x) here"));
        assert!(!c.in_url_mode());
    }

    #[test]
    fn empty_trie_with_url_in_prose_rejects_url_start() {
        // No URLs allowed at all → model can't emit any URL start.
        // (We don't construct an empty-allowlist constraint per
        // `new()`'s contract, so this test uses a single-URL trie
        // and feeds a different scheme that's not in the trie.)
        let mut c = build(&["https://a.test/x"]);
        // The trie covers https:// (8 bytes). `http://` is not in
        // the trie (none of the URLs are http://), so when the model
        // tries to emit http:// the walk_marker returns None and
        // feed_byte rejects.
        assert!(!feed(&mut c, "see http://b.test/y here"));
    }

    #[test]
    fn second_url_after_first_completes() {
        let mut c = build(&["https://a.test/x", "https://a.test/y"]);
        assert!(feed(&mut c, "first https://a.test/x, then https://a.test/y."));
        assert!(!c.in_url_mode());
    }

    #[test]
    fn url_start_straddling_byte_boundary() {
        // Tokens commonly chunk `https://` into multiple pieces.
        // Feed it byte-by-byte (worst case).
        let mut c = build(&["https://a.test/x"]);
        for byte in "https://a.test/x".as_bytes() {
            assert!(feed_byte(&c.nodes, &mut c.cursor, *byte), "byte {:?} should be accepted", *byte as char);
        }
    }

    /// Regression: search-gym Phase 3c (2026-05-19) fixtures 07/08.
    /// The model emitted a strict byte-prefix of an allowed URL
    /// (`…/after-hour` when only `…/after-hours` was in the
    /// allowlist), then terminated generation via EOS. The empty-
    /// bytes EOS bypassed the mask, leaving a truncated URL in the
    /// final message that the predicate `must_not_cite_url_outside_mock`
    /// flagged as a fabrication. `mask()` must clamp empty-bytes
    /// tokens when the cursor is mid-URL at a non-terminal node;
    /// when the cursor IS at a terminal node, empty-bytes tokens
    /// stay allowed because the URL is already complete.
    #[test]
    fn empty_bytes_token_masked_mid_url_non_terminal() {
        // Construct a vocab where token 0 is an empty-bytes token
        // (stand-in for EOS) and token 1 has the byte `s` (the byte
        // that would extend `/after-hour` into `/after-hours`).
        let vocab: Vec<Vec<u8>> = vec![Vec::new(), vec![b's']];
        let urls = vec!["https://a.test/after-hours".to_string()];
        let mut c =
            UrlAllowlistConstraint::new(&urls, Arc::new(vocab.clone())).expect("non-empty");
        // Walk cursor up to but NOT including the terminal `s`.
        for byte in "https://a.test/after-hour".as_bytes() {
            assert!(
                feed_byte(&c.nodes, &mut c.cursor, *byte),
                "prefix byte {:?} should be accepted",
                *byte as char
            );
        }
        assert!(c.in_url_mode(), "cursor should still be inside URL");

        // Mid-URL at non-terminal: EOS (token 0, empty bytes) must
        // get clamped, AND the byte-`s` token (token 1) must stay
        // valid (it's the only legal continuation).
        let mut data = LlamaTokenDataArray::from_iter(
            vec![
                crate::llama::cpp::token::data::LlamaTokenData::new(LlamaToken(0), 0.0, 0.0),
                crate::llama::cpp::token::data::LlamaTokenData::new(LlamaToken(1), 0.0, 0.0),
            ],
            false,
        );
        c.mask(&mut data);
        let eos_logit = data.data[0].logit();
        let s_logit = data.data[1].logit();
        assert_eq!(
            eos_logit,
            f32::NEG_INFINITY,
            "EOS at non-terminal node must be masked"
        );
        assert_ne!(
            s_logit,
            f32::NEG_INFINITY,
            "the byte that completes the URL must remain valid"
        );

        // Now walk the cursor to the terminal node by feeding the
        // final `s`. EOS at terminal must STAY allowed (the URL is
        // complete; the model can stop generation cleanly).
        assert!(feed_byte(&c.nodes, &mut c.cursor, b's'));
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
