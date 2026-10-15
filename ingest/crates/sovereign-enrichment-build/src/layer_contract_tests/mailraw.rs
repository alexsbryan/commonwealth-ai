// SPDX-License-Identifier: AGPL-3.0-or-later
//! The customer journey's fixture in process (`tests/fixtures/layer/mailraw/`,
//! whose recipe tells the story): six RFC 5322 messages through the engine's
//! own ingest (`Fixture::ingest`), then the default path. What a correct read
//! of a mailbox must never do is asserted on every question asked, and the
//! records a right model makes are asserted with the oracle answering RESOLVE
//! and Mention right for the parties the fixture names.

use super::reader::{asked_line, line_questions};
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

/// Whether `prompt` is about the message whose Message-ID is `id`: its
/// declared facts name the field the recipe stamps a document's id from.
fn in_message(prompt: &ChatPrompt, id: &str) -> bool {
    prompt.user.contains(&format!("\nmessage_id: {id}\n"))
}

/// A line a header block states: the fixture writes every header line, of a
/// message and of a message embedded in one, as `Field: value`, and no body
/// line opens that way.
fn is_header_line(text: &str) -> bool {
    ["From:", "To:", "Cc:", "Date:", "Sent:", "Subject:"]
        .iter()
        .any(|field| text.trim_start().starts_with(field))
}

/// C1 on a raw mailbox: the header facts open every question as declared
/// facts, so no header line is ever asked, neither the message's own (the
/// extractor writes them into the stored text) nor an embedded message's.
#[tokio::test]
async fn no_question_asks_a_header_line() {
    let run = run(&Fixture::ingest(SHAPE).await).await;
    let asked: Vec<&str> = line_questions(&run).into_iter().map(asked_line).collect();
    assert!(!asked.is_empty(), "no line was asked");
    let headers: Vec<&&str> = asked.iter().filter(|l| is_header_line(l)).collect();
    assert!(headers.is_empty(), "header lines were asked: {headers:?}");
    // Nor is one offered as where a particular is first named (About).
    let offered: Vec<&str> = run
        .prompts
        .iter()
        .filter(|p| p.user.contains("first named or described?"))
        .flat_map(|p| p.user.lines())
        .filter_map(|l| l.split_once(" line ").and_then(|(_, r)| r.split_once(": ")))
        .map(|(_, text)| text)
        .filter(|text| is_header_line(text))
        .collect();
    assert!(
        offered.is_empty(),
        "About offered header lines: {offered:?}"
    );
}

/// A forwarded message that is not in the corpus is no statement of the
/// message carrying it: none of its lines is asked, so nothing it says is
/// dated or authored by the forward, and no claim cites it.
#[tokio::test]
async fn no_line_of_an_embedded_message_is_asked_or_cited() {
    let run = run(&Fixture::ingest(SHAPE).await).await;
    let asked: Vec<&str> = line_questions(&run)
        .into_iter()
        .filter(|p| in_message(p, "<n2@alderpine.example>"))
        .map(asked_line)
        .collect();
    assert!(
        asked
            .iter()
            .any(|l| l.starts_with("The timber merchant answered")),
        "the forward's own line is asked: {asked:?}"
    );
    assert!(
        !asked.iter().any(|l| l.contains(FORWARDED)),
        "a line of the embedded message was asked: {asked:?}"
    );
    let citing: Vec<String> = run
        .atoms()
        .iter()
        .filter(|a| a["atom_type"] == "Claim")
        .filter(|a| a.to_string().contains(FORWARDED))
        .map(|a| a["data"]["id"].to_string())
        .collect();
    assert!(
        citing.is_empty(),
        "claims cite the embedded message: {citing:?}"
    );
}

/// A company named in prose before any message from it arrives is the record
/// a later header seeds, and no record is named with trailing punctuation or
/// an address: the note names "Birch Hall Trust," and the oracle points at
/// the words as a span model would, punctuation included.
#[tokio::test]
async fn a_company_named_before_its_mail_is_its_seeded_record_and_named_cleanly() {
    let run = run_knowing(&Fixture::ingest(SHAPE).await, PARTIES).await;
    let atoms = run.atoms();
    let companies: Vec<&Value> = atoms
        .iter()
        .filter(|a| a["data"]["entity_type"] == "company")
        .collect();
    let birch = companies
        .iter()
        .find(|a| a["data"]["attributes"]["web_domain"] == "birchhall.example")
        .and_then(|a| a["data"]["id"].as_str())
        .expect("a header seeds birchhall.example");
    for c in &companies {
        let name = c["data"]["canonical_name"].as_str().unwrap_or("");
        assert!(
            !name.contains('@') && name.ends_with(|ch: char| ch.is_alphanumeric()),
            "a company is named {name:?}"
        );
    }
    let in_note: Vec<&Value> = atoms
        .iter()
        .filter(|a| a["data"]["attributes"]["document_id"] == "<n1@alderpine.example>")
        .filter(|a| {
            a["data"]["attributes"]["__document_read_words"]
                .as_str()
                .is_some_and(|w| w.contains("Birch Hall"))
        })
        .collect();
    assert_eq!(
        in_note.len(),
        1,
        "the note names Birch Hall once: {in_note:?}"
    );
    assert_eq!(
        in_note[0]["data"]["attributes"]["__document_read_words"], "Birch Hall Trust",
        "the mention's words are trimmed of punctuation"
    );
    let decisions: Vec<Value> = run
        .lines(DECISIONS_FILE)
        .into_iter()
        .filter(|d| d["type"] == "company" && d["document"] == "<n1@alderpine.example>")
        .collect();
    assert_eq!(
        in_note[0]["data"]["subject"].as_str(),
        Some(birch),
        "the note's Birch Hall Trust is not birchhall.example's record; RESOLVE: {decisions:?}"
    );
    let opened: Vec<&&Value> = companies
        .iter()
        .filter(|a| a["data"]["attributes"]["web_domain"].is_null())
        .collect();
    assert!(opened.is_empty(), "a mention opened a company: {opened:?}");
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
