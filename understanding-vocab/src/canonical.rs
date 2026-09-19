// SPDX-License-Identifier: AGPL-3.0-or-later
//! Canonical-name normalisation primitives shared by the meta-atlas
//! substrate.
//!
//! Move 4 originally shipped a full cross-corpus
//! `CanonicalRegistry` here with a per-corpus integer priority dial.
//! Move 5 replaced that with the derived per-atom articulation +
//! per-corpus stability taxonomy under corpus-engine's `meta_atlas` and
//! removed the priority dial wholesale. What survives in this
//! module is the [`lookup_key`] normaliser — the canonical-form
//! function the meta-atlas builder uses to cluster Entity atoms
//! across corpora, and the same function retrieval-time lookups
//! call to resolve a surface form against the index.

/// Normalise a surface form into the registry's lookup key. The
/// transformation is intentionally aggressive: lowercase + drop
/// every non-alphanumeric run (collapsing them to a single space),
/// then trim. "Albert Einstein", "albert einstein", and "ALBERT
/// EINSTEIN!" all collapse to `albert einstein`.
pub fn lookup_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_sep = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            for low in c.to_lowercase() {
                out.push(low);
            }
            last_was_sep = false;
        } else if !last_was_sep {
            out.push(' ');
            last_was_sep = true;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_key_collapses_punct_and_case() {
        assert_eq!(lookup_key("Albert Einstein"), "albert einstein");
        assert_eq!(lookup_key("ALBERT-EINSTEIN!"), "albert einstein");
        assert_eq!(lookup_key("  einstein  "), "einstein");
        assert_eq!(lookup_key(""), "");
        assert_eq!(lookup_key("...,,,"), "");
    }

    #[test]
    fn lookup_key_handles_unicode_lowercasing() {
        // German Eszett — lowercases to ss in Rust's char-iter rules.
        assert_eq!(lookup_key("STRASSE"), "strasse");
    }

    #[test]
    fn lookup_key_collapses_inner_whitespace_runs() {
        assert_eq!(lookup_key("foo   bar"), "foo bar");
        assert_eq!(lookup_key("foo\t\nbar"), "foo bar");
    }
}
