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

/// A Notes history holds one message per header block, each running to the
/// next; the sender and date lines a Notes client writes above "To:" open the
/// block across the line of tabs between them. Failing input, named:
/// ward-story tep/2., where Jason Williams' name and date read as Dan Hyvl's.
#[test]
fn a_notes_history_is_one_segment_per_message() {
    let body = "Debra,\n Can you forward it.\n\n\n\tJason R Williams/ENRON@enronXgate\n\
                \t03/26/2001 03:51 PM\n\t\t \n\t\t To: Debra Perlingiere/HOU/ECT@ECT\n\t\t cc: \n\
                \t\t Subject: Tucson Electric Power\n\nDebra -\nAttached is a worksheet.\n\n\
                Tracy Ngo\n12/19/2000 02:13 PM\nTo: Ed Sacks/HOU/ECT@ECT\ncc:\nSubject: TEP\n\n\
                The cp is looking for a power swap.\n";
    let found = embedded_segments(body);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(body[found[0].start..].starts_with("\tJason R Williams"));
    assert_eq!(found[0].end, found[1].start);
    assert!(body[found[0].header_end..found[0].end].contains("Attached is a worksheet."));
    assert!(!body[found[0].start..found[0].end].contains("power swap"));
    assert!(body[found[1].start..].starts_with("Tracy Ngo"));
    assert_eq!(found[1].end, body.len());
}

/// A separator a client wrapped onto a second line its dashes close, a Notes
/// "Sent by:" line, and a blank line between "cc:" and "Subject:" are all
/// one message's header, so none of them reads as a message's text. Failing
/// input, named: ward-story tep/1. and tep/10.
#[test]
fn a_wrapped_separator_and_a_spaced_notes_header_are_one_header() {
    let body = "See below.\n\n---------------------- Forwarded by Chris H Foster/HOU/ECT on 03/30/2001 \n\
                09:24 AM ---------------------------\n\n\nChristopher McKey\n03/29/2001 08:48 AM\n\
                Sent by: Christopher McKey\nTo: Chris H Foster/HOU/ECT@ECT\ncc:  \n\nSubject: PD\n\n\
                In case you hadn't seen this.\n";
    let s = one(body);
    assert_eq!(s.kind, SegmentKind::Forward);
    assert!(
        body[s.start..s.header_end].ends_with("Subject: PD"),
        "{:?}",
        &body[s.start..s.header_end]
    );
    assert_eq!(
        body[s.header_end..s.end].trim(),
        "In case you hadn't seen this."
    );
}

#[test]
fn one_field_shaped_line_in_prose_embeds_nothing() {
    let body = "Hello,\n\nSubject: the chairs are late again.\nPlease call me.\n";
    assert!(embedded_segments(body).is_empty());
}
