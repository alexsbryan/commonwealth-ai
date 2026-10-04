// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `[work_offer]` section's tests, moved from sovereign-contracts with it.
use super::*;

/// The zero value donates nothing, stated three ways because any ONE of
/// them is enough and a reader should not have to guess which.
#[test]
fn a_node_that_says_nothing_offers_nothing() {
    let section = WorkOfferSection::default();
    assert_eq!(section.accept, WorkAcceptFrom::Nobody);
    assert_eq!(section.max_concurrent, 0);
    assert_eq!(
        section
            .to_offer("linux", "x86_64", oicp_types::Isolation::Subprocess)
            .expect("an empty section is not an error"),
        None,
        "no kinds means no offer at all — not an offer of nothing"
    );
}

/// The tri-state survives the flat spelling. `Nobody` is `Some(∅)` and
/// `Anyone` is `None`; collapsing either into the other is the defect the
/// enum exists to prevent, and `accepts_from` reads them oppositely.
#[test]
fn the_accept_policy_carries_the_wire_tri_state() {
    let key = "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29";
    let mut section = WorkOfferSection {
        kinds: vec!["process:v1".to_string()],
        max_concurrent: 1,
        accept_from: vec![key.to_string()],
        ..Default::default()
    };

    let offer = |s: &WorkOfferSection| {
        s.to_offer("linux", "x86_64", oicp_types::Isolation::Subprocess)
            .expect("valid kind")
            .expect("kinds are set")
    };

    section.accept = WorkAcceptFrom::Nobody;
    let o = offer(&section);
    assert_eq!(o.accept_from, Some(Vec::new()));
    assert!(!o.accepts_from(key), "`nobody` accepts nobody");

    section.accept = WorkAcceptFrom::Listed;
    assert!(offer(&section).accepts_from(key));
    assert!(!offer(&section).accepts_from("someone-else"));

    section.accept = WorkAcceptFrom::Anyone;
    assert_eq!(offer(&section).accept_from, None);
    assert!(offer(&section).accepts_from("someone-else"));
}

/// The failing input: `process@1`, the spelling four parse-and-discard
/// helpers in this tree accept. A donor booting on it would offer a kind
/// no submitter can name.
#[test]
fn a_kind_that_is_not_id_vn_is_refused_naming_it() {
    let section = WorkOfferSection {
        kinds: vec!["process@1".to_string()],
        ..Default::default()
    };
    let err = section
        .to_offer("linux", "x86_64", oicp_types::Isolation::Subprocess)
        .expect_err("`process@1` is not a job kind");
    assert!(
        err.to_string().contains("process@1"),
        "the refusal must name the entry, got: {err}"
    );
}
