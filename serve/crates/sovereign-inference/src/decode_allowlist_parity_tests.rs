// SPDX-License-Identifier: AGPL-3.0-or-later
//! The grammar a remote llguidance host is sent for the allow-lists
//! (`sovereign_contracts::decode_allowlist::llguidance_lark`) must accept
//! exactly the outputs the engine's tries accept. Each string is judged
//! whole: every byte allowed, and EOS allowed after the last. The post-hoc
//! reader `AllowlistLanguage::entries_outside` is held to the same verdicts.

use sovereign_contracts::decode_allowlist::{llguidance_lark, EVIDENCE_ID, URL};

use crate::evidence_id_constraint::EvidenceIdAllowlistConstraint;
use crate::llg_whole_string::{Lcg, WholeString};
use crate::url_constraint::UrlAllowlistConstraint;

fn owned(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The URL list the cases below are written against.
pub(crate) fn test_urls() -> Vec<String> {
    owned(&["https://a.test/x", "https://a.test/xy", "http://b.test"])
}

/// The evidence-id list the cases below are written against.
pub(crate) fn test_ids() -> Vec<String> {
    owned(&["ev-T1-0001", "ev-T2-0010"])
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
pub(crate) fn generated(n: usize) -> Vec<String> {
    let mut rng = Lcg::new(0x9e37_79b9_7f4a_7c15);
    (0..n)
        .map(|_| {
            let parts = 1 + rng.below(6);
            (0..parts)
                .map(|_| FRAGMENTS[rng.below(FRAGMENTS.len())])
                .collect()
        })
        .collect()
}

pub(crate) fn hand_picked() -> Vec<String> {
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
    let g = WholeString::lark(llguidance_lark(&langs).expect("non-empty lists"));
    let mut accepted = 0;
    let cases: Vec<String> = hand_picked().into_iter().chain(generated(3000)).collect();
    for text in &cases {
        let want = truth(text.as_bytes());
        accepted += usize::from(want);
        assert_eq!(
            g.accepts(text.as_bytes()).expect("matcher"),
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
    let urls = test_urls();
    assert_parity(&[("url", &urls)], |t| {
        UrlAllowlistConstraint::accepts_whole(&urls, t)
    });
}

#[test]
fn the_evidence_grammar_accepts_what_the_evidence_trie_accepts() {
    let ids = test_ids();
    assert_parity(&[("ev", &ids)], |t| {
        EvidenceIdAllowlistConstraint::accepts_whole(&ids, t)
    });
}

/// THE FAILING INPUT for a combined loop: `[` ends a URL and opens an
/// evidence handle, so `https://a.test/x[ev-T9-0001]` must be refused.
#[test]
fn both_lists_accept_what_both_tries_accept() {
    let urls = test_urls();
    let ids = test_ids();
    assert_parity(&[("url", &urls), ("ev", &ids)], |t| {
        UrlAllowlistConstraint::accepts_whole(&urls, t)
            && EvidenceIdAllowlistConstraint::accepts_whole(&ids, t)
    });
}

#[test]
fn empty_lists_constrain_nothing() {
    assert!(llguidance_lark(&[(&URL, &[]), (&EVIDENCE_ID, &[])]).is_none());
}
