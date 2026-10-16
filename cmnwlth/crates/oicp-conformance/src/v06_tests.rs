// SPDX-License-Identifier: AGPL-3.0-or-later
//! The v0.6 checks that install a shelf of their own, green against the
//! lawful fake and red against the fake that breaks exactly their field
//! (oicp-v0.6.md §6).

use crate::claim_checks::check_evidence_verify_sentences;
use crate::evidence_checks::v05_feature_failures;
use crate::fake_host::Breaks;
use crate::located_checks::{check_ingest_transcript, check_knowledge_locator};
use crate::queue_checks::check_knowledge_sentence_queue;
use crate::record_checks::check_ingest_lookups;
use crate::v05_tests::{bench, green, red};

/// v0.6 §2.13's two failing inputs: a span given the heading path of the
/// part before the one it stands in, and a locator invented for a span
/// that stands under no heading (a preface, a text with none).
#[tokio::test]
async fn knowledge_locator_is_red_against_the_part_before_and_an_invented_path() {
    let b = bench(Breaks::default()).await;
    green(&check_knowledge_locator(&b.host, &b.manifest).await);
    assert!(v05_feature_failures(&b.manifest).is_empty());
    for (breaks, says) in [
        (
            Breaks {
                locator_previous_part: true,
                ..Breaks::default()
            },
            "carries under [\"BOOK I. OF THE HARBOUR.\"]; it stands under [\"BOOK I. OF THE \
             HARBOUR.\", \"CHAPTER I. OF THE TIDES.\"]",
        ),
        (
            Breaks {
                locator_invented: true,
                ..Breaks::default()
            },
            "carries under [\"Annals of a Small Port\"]; it stands no locator",
        ),
    ] {
        let b = bench(breaks).await;
        red(&check_knowledge_locator(&b.host, &b.manifest).await, says);
    }
    let mut m = b.manifest.clone();
    m.features.retain(|f| f != "knowledge:range");
    let f = v05_feature_failures(&m);
    assert!(
        f.iter()
            .any(|x| x == "knowledge:locator requires knowledge:range"),
        "{f:?}"
    );
}

/// v0.6 §2.14's two failing inputs: a header line read into the turn before
/// it (the turn attributed to the previous speaker), and a speaker invented
/// for a Whisper segment; beside them, a declared transcript in neither
/// shape stored as plain text rather than listed failed.
#[tokio::test]
async fn ingest_transcript_is_red_against_a_turn_read_into_the_one_before_and_an_invented_speaker()
{
    let b = bench(Breaks::default()).await;
    green(&check_ingest_transcript(&b.host, &b.manifest).await);
    assert!(v05_feature_failures(&b.manifest).is_empty());
    for (breaks, says) in [
        (
            Breaks {
                turn_read_into_previous: true,
                ..Breaks::default()
            },
            "carries at 4000 ms, speaker `HARBOUR MASTER`; it stands at 11000 ms, speaker \
             `Interviewer`",
        ),
        (
            Breaks {
                whisper_speaker_invented: true,
                ..Breaks::default()
            },
            "carries at 0 ms, speaker `SPEAKER_00`; it stands at 0 ms, speaker absent",
        ),
        (
            Breaks {
                non_transcript_stored: true,
                ..Breaks::default()
            },
            "`minutes` is listed Some(Ready",
        ),
    ] {
        let b = bench(breaks).await;
        red(&check_ingest_transcript(&b.host, &b.manifest).await, says);
    }
    let mut m = b.manifest.clone();
    m.features.retain(|f| f != "knowledge:locator");
    let f = v05_feature_failures(&m);
    assert!(
        f.iter()
            .any(|x| x == "ingest:transcripts requires knowledge:locator"),
        "{f:?}"
    );
}

/// v0.6 §2.15's failing inputs: a claim settled from the fast hint alone; a
/// citation's year made a specific; a figure inside parentheses left out of
/// the claim's specifics; beside them, a contradiction that names no
/// differing specific.
#[tokio::test]
async fn evidence_verify_sentences_is_red_against_a_fast_verdict_a_cited_year_and_a_dropped_parenthesis(
) {
    let b = bench(Breaks::default()).await;
    green(&check_evidence_verify_sentences(&b.host, &b.manifest).await);
    for (breaks, says) in [
        (
            Breaks {
                fast_hint_decides: true,
                ..Breaks::default()
            },
            "at fast, `Late sailings rose to 7.5% of the total in 2020.` is unsupported; it \
             should be abstained",
        ),
        (
            Breaks {
                citation_year_specific: true,
                ..Breaks::default()
            },
            "`2019` is named a specific inside `(Okafor 2019)`, a citation or a locator",
        ),
        (
            Breaks {
                parenthesis_dropped: true,
                ..Breaks::default()
            },
            "no figure, date or quantity specific covers `6.1`",
        ),
        (
            Breaks {
                contradicted_unnamed: true,
                ..Breaks::default()
            },
            "is contradicted and no passage names a differing specific",
        ),
    ] {
        let b = bench(breaks).await;
        red(
            &check_evidence_verify_sentences(&b.host, &b.manifest).await,
            says,
        );
    }
    let mut m = b.manifest.clone();
    m.features.retain(|f| f != "knowledge:sentences");
    let f = v05_feature_failures(&m);
    assert!(
        f.iter()
            .any(|x| x == "evidence:verify_sentences requires knowledge:sentences"),
        "{f:?}"
    );
}

/// v0.6 §2.16's three failing inputs: a field invented for an identifier
/// that did not resolve; a declared title overwritten by the looked-up one;
/// a lookup sent for an install that did not allow it.
#[tokio::test]
async fn ingest_lookups_is_red_against_an_invented_field_an_overwritten_title_and_an_unallowed_lookup(
) {
    let b = bench(Breaks::default()).await;
    green(&check_ingest_lookups(&b.host, &b.manifest).await);
    for (breaks, says) in [
        (
            Breaks {
                lookup_invents_unresolved: true,
                ..Breaks::default()
            },
            "a field (`title`) from `crossref` for `no-such-doi`'s DOI, which did not resolve",
        ),
        (
            Breaks {
                lookup_overwrites_declared: true,
                ..Breaks::default()
            },
            "declared `title` of `fair-principles` is not kept",
        ),
        (
            Breaks {
                lookup_without_allowance: true,
                ..Breaks::default()
            },
            "`fair-principles`'s DOI was looked up (Resolved) for an install that did not allow it",
        ),
    ] {
        let b = bench(breaks).await;
        red(&check_ingest_lookups(&b.host, &b.manifest).await, says);
    }
    let mut m = b.manifest.clone();
    m.features.retain(|f| f != "ingest:documents");
    let f = v05_feature_failures(&m);
    assert!(
        f.iter()
            .any(|x| x == "ingest:lookups requires ingest:documents"),
        "{f:?}"
    );
}

/// v0.6 §2.11's two failing inputs for `sentence_queue`: a corpus queued
/// behind another answered as embedding now, and a corpus that has finished
/// still named ahead of the one behind it.
#[tokio::test]
async fn knowledge_sentence_queue_is_red_against_a_queued_corpus_answered_embedding_and_a_finished_one_named_ahead(
) {
    let b = bench(Breaks::default()).await;
    green(&check_knowledge_sentence_queue(&b.host, &b.manifest).await);
    assert!(v05_feature_failures(&b.manifest).is_empty());
    for (breaks, says) in [
        (
            Breaks {
                queued_as_embedding: true,
                ..Breaks::default()
            },
            "2 corpora answered embedding now (oicp-conformance-queue-ahead, \
             oicp-conformance-queue-behind); the host embeds one at a time",
        ),
        (
            Breaks {
                finished_named_ahead: true,
                ..Breaks::default()
            },
            "which has no sentence pending in this reply (it has finished)",
        ),
    ] {
        let b = bench(breaks).await;
        red(
            &check_knowledge_sentence_queue(&b.host, &b.manifest).await,
            says,
        );
    }
    let mut m = b.manifest.clone();
    m.features.retain(|f| f != "knowledge:sentences");
    let f = v05_feature_failures(&m);
    assert!(
        f.iter()
            .any(|x| x == "knowledge:sentence_queue requires knowledge:sentences"),
        "{f:?}"
    );
    m.features.retain(|f| f != "knowledge:sentence_queue");
    let skipped = check_knowledge_sentence_queue(&b.host, &m).await;
    assert_eq!(skipped.status, crate::report::CheckStatus::Skip);
}
