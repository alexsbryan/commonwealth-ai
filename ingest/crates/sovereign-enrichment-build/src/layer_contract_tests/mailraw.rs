// SPDX-License-Identifier: AGPL-3.0-or-later
//! The customer journey's fixture in process (`tests/fixtures/layer/mailraw/`,
//! whose recipe tells the story): six RFC 5322 messages through the engine's
//! own ingest (`Fixture::ingest`), then the default path. What a correct read
//! of a mailbox must never do is asserted on every window read, and the
//! records a right model makes are asserted with the oracle answering RESOLVE
//! right for the parties the fixture names.

use super::*;

const SHAPE: &str = "mailraw";

/// The forwarded message's own line, in a message that is not in the corpus.
const FORWARDED: &str = "Oakline Joinery Ltd. will deliver the quarter-sawn oak";

/// What a right model knows of the fixture's particulars (`Parties`): each
/// company by the words a statement names it with and the identity value its
/// record shows as written; the order by its words.
const PARTIES: &[(&str, &str, &str)] = &[
    ("company", "Birch Hall Trust", "birchhall.example"),
    ("company", "Alder and Pine Workshop", "alderpine.example"),
    ("order", "oak chairs", "oak chairs"),
];

/// A line a header block states: the fixture writes every header line, of a
/// message and of a message embedded in one, as `Field: value`, and no body
/// line opens that way.
fn is_header_line(text: &str) -> bool {
    ["From:", "To:", "Cc:", "Date:", "Sent:", "Subject:"]
        .iter()
        .any(|field| text.trim_start().starts_with(field))
}

/// C1 on a raw mailbox: a message's header facts are shown beside it as
/// declared facts, so no header line is shown as a message's own text,
/// neither the message's own (the extractor writes them into the stored text)
/// nor an embedded message's.
#[tokio::test]
async fn no_window_shows_a_header_line_as_its_own() {
    let run = run(&Fixture::ingest(SHAPE).await).await;
    let shown = own_lines(&run, None);
    assert!(!shown.is_empty(), "no line was shown");
    let headers: Vec<&&str> = shown.iter().filter(|l| is_header_line(l)).collect();
    assert!(headers.is_empty(), "header lines were shown: {headers:?}");
}

/// A forwarded message that is not in the corpus is a message of its own,
/// never a statement of the message carrying it: none of its lines is shown
/// as the forward's own, and every claim citing it is the embedded
/// message's, dated and authored by its own header (Sam Reed, 3 March), not
/// by the forward (Mia Holt, 4 March). Until 2026-10-17 such a message was
/// read nowhere (ward-story tep/10., note f9142d79).
#[tokio::test]
async fn an_embedded_message_is_read_as_its_own_and_never_as_the_forwards() {
    let run = run(&Fixture::ingest(SHAPE).await).await;
    let shown = own_lines(&run, Some("message_id: <n2@alderpine.example>"));
    assert!(
        shown
            .iter()
            .any(|l| l.starts_with("The timber merchant answered")),
        "the forward's own line is shown: {shown:?}"
    );
    assert!(
        !shown.iter().any(|l| l.contains(FORWARDED)),
        "a line of the embedded message was shown as the forward's: {shown:?}"
    );
    assert!(
        own_lines(&run, None).iter().any(|l| l.contains(FORWARDED)),
        "the embedded message is shown as a message of its own"
    );
    let atoms = run.atoms();
    let citing: Vec<&serde_json::Value> = atoms
        .iter()
        .filter(|a| a["atom_type"] == "Claim")
        .filter(|a| a.to_string().contains(FORWARDED))
        .map(|a| &a["data"]["attributes"])
        .collect();
    assert!(!citing.is_empty(), "no claim cites the embedded message");
    for attributes in citing {
        let id = attributes["document_id"].as_str().unwrap_or_default();
        let date = attributes["document_date"].as_str().unwrap_or_default();
        assert!(
            id.starts_with("embedded-"),
            "cited as the forward's: {attributes}"
        );
        assert!(
            date.starts_with("2026-03-04T00:20"),
            "not its own header's date: {attributes}"
        );
    }
}

/// What RESOLVE shows of a record is what a reader would recognise it by: an
/// identity value as written, never its folded comparison key, and never a
/// document stamp dressed as one of the record's attributes.
#[tokio::test]
async fn no_resolve_prompt_shows_a_folded_key_or_a_stamp_as_an_attribute() {
    let run = run_knowing(&Fixture::ingest(SHAPE).await, PARTIES).await;
    let shown: Vec<String> = run
        .prompts
        .iter()
        .filter(|p| p.phase_id.as_deref() == Some("resolve_select"))
        .flat_map(|p| {
            let labels: Vec<String> = p
                .response_schema
                .as_ref()
                .and_then(|s| s["enum"].as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            candidate_blocks(&p.user, &labels)
        })
        .collect();
    assert!(
        shown.iter().any(|b| b.contains("birchhall.example")),
        "no candidate shows birchhall.example as written: {shown:?}"
    );
    for block in &shown {
        for folded in ["birchhall example", "alderpine example"] {
            assert!(!block.contains(folded), "a folded key is shown: {block}");
        }
        for stamp in ["document_date:", "document_thread:", "document_id:"] {
            assert!(
                !block.contains(stamp),
                "a stamp is shown as an attribute: {block}"
            );
        }
    }
}
