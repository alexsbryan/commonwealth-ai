// SPDX-License-Identifier: AGPL-3.0-or-later
//! The export's batched production checks decide exactly what the direct
//! check decides: a batch is only a way of asking svrn fewer times.

use super::*;

/// A stand-in for the presence check: any chunk contains the value.
fn contains(value: &str, chunks: &[String]) -> bool {
    chunks.iter().any(|c| c.contains(value))
}

fn case(id: &str, label: adv::CaseLabel, witness: adv::SiteWitness) -> adv::StreamBCase {
    adv::StreamBCase {
        id: id.into(),
        corpus_id: "c".into(),
        source_item_id: "i".into(),
        kind: adv::CorruptionKind::EntitySwap,
        label,
        claim: "a claim".into(),
        question: "q".into(),
        evidence_chunks: vec!["Paris is in France.".into(), "Lyon is too.".into()],
        evidence_chunk_ids: vec!["0".into(), "1".into()],
        spans: Vec::new(),
        witness,
    }
}

/// Failing input: a driver that returned its first pass (where every unknown
/// question read `false`) would fail the grounded case and pass the injected
/// one whose displaced original is present.
#[tokio::test]
async fn batched_checks_decide_what_the_direct_check_decides() {
    let cases = vec![
        case(
            "swap-ok",
            adv::CaseLabel::Ungrounded,
            adv::SiteWitness::InjectedAbsent {
                injected: "Berlin".into(),
                original: Some("Paris".into()),
            },
        ),
        case(
            "swap-bad",
            adv::CaseLabel::Ungrounded,
            adv::SiteWitness::InjectedAbsent {
                injected: "Lyon".into(),
                original: None,
            },
        ),
        case(
            "grounded-ok",
            adv::CaseLabel::Grounded,
            adv::SiteWitness::Supported {
                terms: vec!["Paris".into(), "France".into()],
            },
        ),
        case(
            "grounded-bad",
            adv::CaseLabel::Grounded,
            adv::SiteWitness::Supported {
                terms: vec!["Paris".into(), "Spain".into()],
            },
        ),
    ];
    let direct: Vec<Result<(), String>> = cases
        .iter()
        .map(|c| production_site_check(c, &mut |v, ch| contains(v, ch)))
        .collect();
    let mut batches = 0usize;
    let batched = answered_checks(&cases, |asked| {
        batches += 1;
        async move { Ok(asked.iter().map(|(v, ch)| contains(v, ch)).collect()) }
    })
    .await
    .unwrap();
    assert_eq!(batched, direct);
    assert_eq!(
        direct.iter().map(Result::is_ok).collect::<Vec<_>>(),
        [true, false, true, false],
        "the fixture has a pass and a failure of each witness"
    );
    assert!(batches <= 3, "{batches} batches for four cases");
}
