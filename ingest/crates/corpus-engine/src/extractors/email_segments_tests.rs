// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn one(body: &str) -> EmbeddedSegment {
    let found = embedded_segments(body);
    assert_eq!(found.len(), 1, "{found:?}");
    found.into_iter().next().unwrap()
}

#[test]
fn a_quoted_original_under_a_separator_runs_to_the_end_with_its_header() {
    let body = "Dear Rosa,\n\nOur price is 2,400 dollars.\n\n-----Original Message-----\n\
                From: Rosa Lind <rosa@birch.example>\nSent: Monday, March 02, 2026 9:15 AM\n\
                To: Tom Avery\nSubject: Chairs\n\nCould you send a price?\n";
    let s = one(body);
    assert_eq!(&body[s.start..s.start + 5], "-----");
    assert_eq!(s.end, body.len());
    assert!(body[..s.header_end].ends_with("Subject: Chairs"));
    assert_eq!(s.kind, SegmentKind::Reply);
}

#[test]
fn a_forward_banner_is_a_forward() {
    let body = "Tom,\n\nTheir note is below.\n\n------- Forwarded by Mia Holt on 03/04/2026 08:28 AM -------\n\
                From: Sam Reed <sam@oak.example>\nDate: Tue, 03 Mar 2026\nTo: Mia Holt\n\nWe will deliver by 20 March.\n";
    let s = one(body);
    assert_eq!(s.kind, SegmentKind::Forward);
    assert!(body[s.header_end..].contains("We will deliver"));
}

#[test]
fn a_header_run_takes_the_sender_and_date_lines_written_above_it() {
    let body = "Thanks, see below.\n\nSam Reed@OAK\n03/04/2026 08:28 AM\nTo: Mia Holt/ALDER@ALDER\ncc:  \n\
                Subject: Re: oak\n\nWe will deliver by 20 March.\n";
    let s = one(body);
    assert_eq!(&body[s.start..s.start + 12], "Sam Reed@OAK");
    assert_eq!(s.kind, SegmentKind::Reply);
}

#[test]
fn marked_lines_are_quotes_and_the_answers_between_them_are_the_authors() {
    let body = "On Mon, Mar 2, 2026, Rosa wrote:\n> Could you send a price?\n> And a date?\n\
                \nThe price is 2,400 dollars.\n> Is oak possible?\nYes, oak.\n";
    let found = embedded_segments(body);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|s| s.kind == SegmentKind::Quote));
    for s in &found {
        assert!(
            !body[s.start..s.end].contains("2,400"),
            "an answer was taken as a quote"
        );
        assert!(!body[s.start..s.end].contains("Yes, oak."));
    }
}

#[test]
fn one_field_shaped_line_in_prose_embeds_nothing() {
    let body = "Hello,\n\nSubject: the chairs are late again.\nPlease call me.\n";
    assert!(embedded_segments(body).is_empty());
}
