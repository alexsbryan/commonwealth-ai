// SPDX-License-Identifier: AGPL-3.0-or-later
//! The grammar a remote llguidance host is sent for the allow-lists
//! (`sovereign_contracts::decode_allowlist::llguidance_lark`) must accept
//! exactly the outputs the engine's tries accept. Each string is judged
//! whole: every byte allowed, and EOS allowed after the last. The post-hoc
//! reader `AllowlistLanguage::entries_outside` is held to the same verdicts.

use llguidance::{api::TopLevelGrammar, toktrie::ApproximateTokEnv, Matcher, ParserFactory};
use sovereign_contracts::decode_allowlist::{llguidance_lark, EVIDENCE_ID, URL};

use crate::evidence_id_constraint::EvidenceIdAllowlistConstraint;
use crate::url_constraint::UrlAllowlistConstraint;

fn owned(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

struct Grammar {
    factory: ParserFactory,
    grammar: TopLevelGrammar,
}

impl Grammar {
    fn new(lark: String) -> Self {
        let env = ApproximateTokEnv::single_byte_env();
        let factory = ParserFactory::new_simple(&env).expect("factory");
        let grammar = TopLevelGrammar::from_lark(lark.clone());
        let m = Matcher::new(factory.create_parser(grammar.clone()));
        assert!(
            !m.is_error(),
            "grammar must compile: {:?}\n{lark}",
            m.get_error()
        );
        Self { factory, grammar }
    }

    fn accepts(&self, text: &[u8]) -> bool {
        let mut m = Matcher::new(self.factory.create_parser(self.grammar.clone()));
        for &b in text {
            if !m.compute_mask().expect("mask").is_allowed(b as u32) {
                return false;
            }
            m.consume_token(b as u32).expect("consume");
        }
        m.is_accepting().expect("accepting")
    }
}

/// Fragments that hit every edge: whole and partial entries of both lists,
/// both URL markers, terminators of one list that open the other, and prose.
const FRAGMENTS: &[&str] = &[
    "https://a.test/x",
    "https://a.test/",
    "https://a.test/xy",
    "https://evil.test",
    "http://b.test",
    "http://",
    "https:/",
    "http",
    "[ev-T1-0001",
    "[ev-T2-0010",
    "[ev-T9-0001",
    "[ev-T",
    "ev-T1-0001",
    "]",
    "[",
    " ",
    ".",
    ":",
    "/",
    "x",
    "h",
    "see ",
];

/// A fixed-seed walk over [`FRAGMENTS`], so a failure reproduces.
fn generated(n: usize) -> Vec<String> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as usize
    };
    (0..n)
        .map(|_| {
            let parts = 1 + next() % 6;
            (0..parts)
                .map(|_| FRAGMENTS[next() % FRAGMENTS.len()])
                .collect()
        })
        .collect()
}

fn hand_picked() -> Vec<String> {
    owned(&[
        "",
        "plain prose with no links",
        "see https://a.test/x.",
        "https://a.test/x",
        "https://a.test/xy",
        "https://a.test/xz",
        "https://a.test/x/more",
        "https://a.test/",
        "https://evil.test",
        "http://b.test, then prose",
        "http://c.test",
        "https:/ not a url",
        "HTTPS://upper.test",
        "a (https://a.test/x) b",
        "https://a.test/xhttps://a.test/x",
        "https://a.test/x[ev-T1-0001]",
        "https://a.test/x[ev-T9-0001]",
        "[ev-T1-0001]",
        "[ev-T1-0002]",
        "[ev-T1-000",
        "a bare ev-T9-9999 mention",
        "[ev-T1-0001] and [ev-T2-0010].",
        "[ev-T1-00010]",
        "[ev-T1-0001[",
        "[ev-T1-0001]https://evil.test",
    ])
}

fn assert_parity(lists: &[(&str, &[String])], truth: impl Fn(&[u8]) -> bool) {
    let langs: Vec<_> = lists
        .iter()
        .map(|(kind, ids)| match *kind {
            "url" => (&URL, *ids),
            _ => (&EVIDENCE_ID, *ids),
        })
        .collect();
    let g = Grammar::new(llguidance_lark(&langs).expect("non-empty lists"));
    let mut accepted = 0;
    let cases: Vec<String> = hand_picked().into_iter().chain(generated(3000)).collect();
    for text in &cases {
        let want = truth(text.as_bytes());
        accepted += usize::from(want);
        assert_eq!(
            g.accepts(text.as_bytes()),
            want,
            "grammar and trie disagree on {text:?} (trie says {want})"
        );
        // The post-hoc reader the conformance judge uses holds to the same
        // language.
        let post_hoc = langs
            .iter()
            .all(|(lang, ids)| lang.entries_outside(text, ids).is_empty());
        assert_eq!(
            post_hoc, want,
            "entries_outside and trie disagree on {text:?} (trie says {want})"
        );
    }
    // Both verdicts occur, or agreement says nothing.
    assert!(
        accepted > 0 && accepted < cases.len(),
        "{accepted}/{}",
        cases.len()
    );
}

#[test]
fn the_url_grammar_accepts_what_the_url_trie_accepts() {
    let urls = owned(&["https://a.test/x", "https://a.test/xy", "http://b.test"]);
    assert_parity(&[("url", &urls)], |t| {
        UrlAllowlistConstraint::accepts_whole(&urls, t)
    });
}

#[test]
fn the_evidence_grammar_accepts_what_the_evidence_trie_accepts() {
    let ids = owned(&["ev-T1-0001", "ev-T2-0010"]);
    assert_parity(&[("ev", &ids)], |t| {
        EvidenceIdAllowlistConstraint::accepts_whole(&ids, t)
    });
}

/// THE FAILING INPUT for a combined loop: `[` ends a URL and opens an
/// evidence handle, so `https://a.test/x[ev-T9-0001]` must be refused.
#[test]
fn both_lists_accept_what_both_tries_accept() {
    let urls = owned(&["https://a.test/x", "https://a.test/xy", "http://b.test"]);
    let ids = owned(&["ev-T1-0001", "ev-T2-0010"]);
    assert_parity(&[("url", &urls), ("ev", &ids)], |t| {
        UrlAllowlistConstraint::accepts_whole(&urls, t)
            && EvidenceIdAllowlistConstraint::accepts_whole(&ids, t)
    });
}

#[test]
fn empty_lists_constrain_nothing() {
    assert!(llguidance_lark(&[(&URL, &[]), (&EVIDENCE_ID, &[])]).is_none());
}
