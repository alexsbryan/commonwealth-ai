// SPDX-License-Identifier: AGPL-3.0-or-later
//! Syntax-aware site filtering for the next-edit rule lane — the
//! [`next_edit::SiteOracle`] the route supplies.
//!
//! WHAT PROBLEM THIS SOLVES. The rule lane proposes every textual match
//! of its rule, and most of those are wrong: measured over the golden
//! set, only 34% of proposed hunks are edits the author actually made
//! (`hunk-precision`, `gym/next-edit/golden/README.md`). A large share
//! of the rest are matches whose SYNTACTIC KIND differs from the thing
//! the user edited — the same word sitting in a comment, a string
//! body, a JSX attribute, a schema key.
//!
//! THE EXEMPLARS COME FOR FREE. Sites the user has ALREADY edited are
//! exactly the occurrences of the rule's `replace` in the buffer. Their
//! node kind IS the user's intent, with no configuration and no
//! guessing: keep a candidate only when its kind agrees with one of
//! them.
//!
//! WHY TREE-SITTER AND NOT THE SCIP GRAPH. Next-edit fires mid-typing,
//! on a buffer that is usually unsaved and often not indexed, so a SCIP
//! index is stale exactly where it is needed. Tree-sitter parses the
//! text the request already carries. The cost is that a parse tree
//! carries no name binding — see "what this cannot do" below.
//!
//! MEASURED at depth 2 over the golden set (rule lane isolated):
//! hunk-precision 34.0% → 50.8%, junk hunks 2064 → 907 (−56%) against
//! good hunks 1065 → 937 (−12%). Per first-user language the trade is
//! 11.5:1 on Go, 9.8:1 on Rust, 6.75:1 on TypeScript. Case verdicts
//! improve slightly rather than regressing (note `e8ecaef7`).
//!
//! WHAT THIS CANNOT DO, on the record so nobody re-derives it: the
//! surviving wrong fires are SAME-KIND, DIFFERENT-REFERENT — a
//! `const settings` that is simply a different variable from the
//! `settings` the user renamed. Telling those apart is name binding,
//! which a parse tree does not carry (note `e0d16d45`). Depth 3+ was
//! measured and rejected: it keeps buying precision but the marginal
//! trade collapses from 4.3:1 to 1.5:1, and it is worst on TypeScript.

use crate::next_edit::GuardedRule;

/// Ancestors compared, including the innermost named node. 1 is too
/// coarse (it cannot tell a call argument from a declaration); 3+ makes
/// the whole enclosing chain load-bearing and is brittle in ways one
/// bank cannot reveal. Measured marginal trade: 1→2 removes 4.3 junk
/// hunks per good one lost, 2→3 only 1.5.
const KIND_DEPTH: usize = 2;

/// Exemplar sites examined. The user's intent is established by the
/// first handful; scanning a buffer that repeats `replace` thousands of
/// times would pay for nothing.
const MAX_EXEMPLARS: usize = 32;

/// Buffers above this are not parsed. Bounds the hot path: a parse is
/// ~2.4 ms at the golden set's p90 file (114 KiB), and next-edit runs
/// on every coalesced edit unit. Above the cap the oracle declines,
/// which costs precision and never correctness.
const MAX_PARSE_BYTES: usize = 1024 * 1024;

/// Languages this filter is MEASURED to help, by `LanguageConfig::id`.
///
/// It is a whitelist rather than "every grammar we have" because the
/// filter is not uniformly good, and the difference is large enough to
/// invert the decision. Measured 2026-08-06 at depth 2 over both banks
/// (rule lane isolated):
///
/// | bank | hunk-precision | useful-fire | wrong-fire |
/// |---|---|---|---|
/// | main (rust/go-heavy) | 33.9% → 48.6% | 37.4% → 33.2% | 12.8% → 12.9% |
/// | react-ts (ts/tsx-only) | 38.9% → 45.5% | 52.0% → **41.2%** | 6.2% → **9.7%** |
///
/// On TypeScript the filter costs 10.8 points of useful-fire and RAISES
/// wrong-fire — `.ts` wrong fires went 2 → 4. Removing sites can empty
/// the literal lane's set, which hands the case to the pair fallback
/// (`next_edit::predict_filtered`), and that rule can be wrong. The
/// per-hunk trade is also worst there: 6.75 junk removed per good hunk
/// lost, against 11.5:1 on Go and 9.8:1 on Rust.
///
/// So Go and Rust get it; TypeScript, JavaScript and Python wait for a
/// measurement that earns them a place. Adding an id here without one
/// is exactly the move this list exists to prevent (ARCH §18.1: a gate
/// nobody has watched work is not a gate).
const PROVEN_LANGUAGES: &[&str] = &["rust", "go"];

/// A parsed buffer, ready to judge candidate sites.
pub struct SyntaxOracle {
    tree: tree_sitter::Tree,
}

impl SyntaxOracle {
    /// Parse `text` using the grammar registered for `path`'s
    /// extension. `None` when the language has no grammar, the buffer
    /// is too large, or the parse fails — every one of which means
    /// "cannot judge", and the caller must then leave sites alone.
    ///
    /// The grammar comes from `corpus-engine`'s registry rather than a
    /// second table here, so `.tsx` routing (which is a DIFFERENT
    /// grammar from `.ts`, not a suffix of it) stays fixed in one place.
    pub fn parse(path: &str, text: &str) -> Option<Self> {
        if text.len() > MAX_PARSE_BYTES {
            return None;
        }
        let ext = path.rsplit('.').next()?;
        let cfg = corpus_engine::extractors::code::language_for_extension(ext)?;
        if !PROVEN_LANGUAGES.contains(&cfg.id) {
            return None;
        }
        let language: tree_sitter::Language = cfg.lang.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).ok()?;
        Some(Self {
            tree: parser.parse(text, None)?,
        })
    }

    /// The named-node kind chain at a byte offset, innermost first.
    fn kinds_at(&self, byte: usize) -> Vec<&str> {
        let mut out = Vec::with_capacity(KIND_DEPTH);
        let mut node = self
            .tree
            .root_node()
            .descendant_for_byte_range(byte, byte + 1);
        while let Some(n) = node {
            if out.len() == KIND_DEPTH {
                break;
            }
            if n.is_named() {
                out.push(n.kind());
            }
            node = n.parent();
        }
        out
    }

    /// Keep only sites whose syntactic kind matches a site the user has
    /// already edited. Returns `sites` unchanged when there is nothing
    /// to compare against — declining to judge is not the same as
    /// judging everything acceptable, and only the former is safe.
    pub fn keep(&self, text: &str, rule: &GuardedRule, sites: Vec<usize>) -> Vec<usize> {
        let locus = edit_locus(&rule.find, &rule.replace);
        let exemplars = self.exemplar_kinds(text, rule, locus);
        if exemplars.is_empty() {
            return sites;
        }
        sites
            .into_iter()
            .filter(|&o| exemplars.iter().any(|e| *e == self.kinds_at(o + locus)))
            .collect()
    }

    /// Kinds at the occurrences of `replace` — the edits the user has
    /// already made, which is what makes this need no configuration.
    fn exemplar_kinds(&self, text: &str, rule: &GuardedRule, locus: usize) -> Vec<Vec<&str>> {
        let mut out: Vec<Vec<&str>> = Vec::new();
        if rule.replace.is_empty() {
            return out;
        }
        let mut at = 0;
        while let Some(rel) = text[at..].find(&rule.replace) {
            if out.len() == MAX_EXEMPLARS {
                break;
            }
            let p = at + rel;
            let kinds = self.kinds_at(p + locus);
            if !kinds.is_empty() && !out.contains(&kinds) {
                out.push(kinds);
            }
            at = p + rule.replace.len().max(1);
        }
        out
    }
}

/// Byte offset within `find` of the first character that differs from
/// `replace` — where the edit actually lands inside the
/// context-expanded rule.
///
/// Clamped inside `find` so `site + locus` always addresses a byte the
/// rule covers; for a pure insertion the two share their whole prefix
/// and the locus is the last byte of `find`, i.e. the anchor rather
/// than the inserted text.
fn edit_locus(find: &str, replace: &str) -> usize {
    let (f, r) = (find.as_bytes(), replace.as_bytes());
    let mut i = 0;
    while i < f.len().min(r.len()) && f[i] == r[i] {
        i += 1;
    }
    i.min(f.len().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(find: &str, replace: &str) -> GuardedRule {
        GuardedRule {
            find: find.into(),
            replace: replace.into(),
            guard_left: false,
            guard_right: false,
        }
    }

    /// The case the filter exists for: the user renamed an identifier
    /// in code, and the only remaining textual match is inside a
    /// comment. Proposing that one is the classic text-engine defect.
    #[test]
    fn a_comment_match_is_not_a_site() {
        let text = "fn a() { let alpha = 1; }\n\
                    fn b() { let beta = 2; }\n\
                    // alpha is described here\n";
        let o = SyntaxOracle::parse("x.rs", text).expect("rust grammar");
        let r = rule("alpha", "beta");
        let sites: Vec<usize> = text.match_indices("alpha").map(|(i, _)| i).collect();
        assert_eq!(sites.len(), 2, "one in code, one in the comment");
        let kept = o.keep(text, &r, sites);
        assert_eq!(kept.len(), 1, "the comment match must be dropped");
        assert!(kept[0] < text.find("//").unwrap());
    }

    /// Declining to judge must return the input. A language with no
    /// registered grammar has to leave the lane exactly as it was.
    #[test]
    fn no_grammar_means_no_opinion() {
        assert!(SyntaxOracle::parse("notes.txt", "alpha alpha").is_none());
        assert!(SyntaxOracle::parse("noextension", "alpha").is_none());
    }

    /// A rule the user has not applied anywhere yet gives the oracle no
    /// exemplar, and it must then keep every site rather than invent a
    /// preference.
    #[test]
    fn no_exemplar_means_every_site_survives() {
        let text = "fn a() { let alpha = 1; let alpha2 = alpha; }\n";
        let o = SyntaxOracle::parse("x.rs", text).unwrap();
        let r = rule("alpha", "zzz_never_present");
        let sites: Vec<usize> = text.match_indices("alpha").map(|(i, _)| i).collect();
        assert_eq!(o.keep(text, &r, sites.clone()), sites);
    }

    /// `.tsx` is a different grammar from `.ts` (corpus-engine carries
    /// the split). A JSX buffer must parse well enough that a match
    /// after the JSX is still judged in code.
    /// TypeScript has a grammar and is still DECLINED, because the
    /// filter measured worse there — useful-fire 52.0% → 41.2% and
    /// wrong-fire 6.2% → 9.7% on the React/TS bank. Having a parser is
    /// not evidence that using it helps.
    ///
    /// This is the test that stops someone widening [`PROVEN_LANGUAGES`]
    /// because "we already parse tsx". We do; it did not help.
    #[test]
    fn a_grammar_we_have_is_not_a_language_we_filter() {
        let tsx = "const Row = () => <div className=\"x\">y</div>;\n\
                   const beta = 1;\n\
                   const alpha = 2;\n";
        assert!(
            SyntaxOracle::parse("Row.tsx", tsx).is_none(),
            "typescript is parseable but unproven — see PROVEN_LANGUAGES"
        );
        assert!(SyntaxOracle::parse("x.ts", tsx).is_none());
        assert!(SyntaxOracle::parse("x.py", "alpha = 1\n").is_none());
        // ...while the proven two are admitted.
        assert!(SyntaxOracle::parse("x.rs", "fn a() { let alpha = 1; }\n").is_some());
        assert!(SyntaxOracle::parse("x.go", "func a() { alpha := 1 }\n").is_some());
    }

    #[test]
    fn locus_is_inside_find_even_for_a_pure_insertion() {
        assert_eq!(edit_locus("ab", "aXb"), 1);
        // Insertion: `find` is a prefix of `replace`, so the first
        // difference is past the end of `find` — clamp inside it.
        assert_eq!(edit_locus("abc", "abcdef"), 2);
        assert_eq!(edit_locus("", "x"), 0);
    }
}
