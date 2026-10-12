// SPDX-License-Identifier: AGPL-3.0-or-later
//! One grammar judging whole strings over a single-byte token env, shared by
//! the allow-list parity tests and the llg-parity corpus. A string is accepted
//! when the mask allows every byte in turn and the grammar accepts at the end.
//!
//! `bench/lanes/engine-swap/llg-parity/src/main.rs` applies the same rule
//! under llguidance 1.0.1; a change to [`WholeString::accepts`] is a change to
//! both, or the two sides stop measuring the same thing.

use llguidance::{api::TopLevelGrammar, toktrie::ApproximateTokEnv, Matcher, ParserFactory};

/// A grammar compiled once, judged per string.
pub(crate) struct WholeString {
    factory: ParserFactory,
    grammar: TopLevelGrammar,
}

impl WholeString {
    /// Compile `grammar`, or say why it does not compile.
    pub(crate) fn new(grammar: TopLevelGrammar) -> Result<Self, String> {
        let env = ApproximateTokEnv::single_byte_env();
        let factory = ParserFactory::new_simple(&env).map_err(|e| e.to_string())?;
        let m = Matcher::new(factory.create_parser(grammar.clone()));
        if m.is_error() {
            return Err(m.get_error().unwrap_or_else(|| "unknown".into()));
        }
        Ok(Self { factory, grammar })
    }

    /// A Lark grammar that must compile.
    pub(crate) fn lark(lark: String) -> Self {
        Self::new(TopLevelGrammar::from_lark(lark.clone()))
            .unwrap_or_else(|e| panic!("grammar must compile: {e}\n{lark}"))
    }

    fn matcher(&self) -> Matcher {
        Matcher::new(self.factory.create_parser(self.grammar.clone()))
    }

    /// Whether the grammar accepts `text` whole. A byte past a stopped
    /// grammar is refused; any other matcher error is returned, not read as
    /// a refusal.
    pub(crate) fn accepts(&self, text: &[u8]) -> Result<bool, String> {
        let mut m = self.matcher();
        for &b in text {
            if m.is_stopped() {
                return Ok(false);
            }
            let mask = m.compute_mask().map_err(|e| e.to_string())?;
            if !mask.is_allowed(b as u32) {
                return Ok(false);
            }
            m.consume_token(b as u32).map_err(|e| e.to_string())?;
        }
        m.is_accepting().map_err(|e| e.to_string())
    }

    /// A random walk over the mask, restricted to printable ASCII and `\n`.
    /// Past `soft_len` bytes it prefers bytes that close a string, object or
    /// array, so JSON walks end; it gives up at eight times `soft_len`.
    /// Returns the bytes and whether the grammar accepts them.
    pub(crate) fn walk(&self, rng: &mut Lcg, soft_len: usize) -> (String, bool) {
        const CLOSERS: &[u8] = b"\"}]";
        let mut m = self.matcher();
        let mut out = Vec::new();
        while !m.is_stopped() && out.len() < soft_len * 8 {
            let accepting = m.is_accepting().unwrap_or(false);
            if accepting && (out.len() >= soft_len || rng.below(5) == 0) {
                break;
            }
            let Ok(mask) = m.compute_mask() else { break };
            let allowed: Vec<u8> = (0x20u8..0x7f)
                .chain([b'\n'])
                .filter(|b| mask.is_allowed(*b as u32))
                .collect();
            let closing: Vec<u8> = allowed
                .iter()
                .copied()
                .filter(|b| CLOSERS.contains(b))
                .collect();
            let pool = if out.len() >= soft_len && !closing.is_empty() && rng.below(4) != 0 {
                &closing
            } else {
                &allowed
            };
            if pool.is_empty() {
                break;
            }
            let b = pool[rng.below(pool.len())];
            if m.consume_token(b as u32).is_err() {
                break;
            }
            out.push(b);
        }
        let accepted = m.is_accepting().unwrap_or(false);
        (String::from_utf8(out).expect("ASCII walk"), accepted)
    }
}

/// A fixed-seed generator, so a failure reproduces.
pub(crate) struct Lcg(u64);

impl Lcg {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// A value in `0..n`.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n
    }
}
