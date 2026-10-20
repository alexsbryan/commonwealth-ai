// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn carrier(d: &str) -> Option<DateTime<FixedOffset>> {
    rfc2822(Some(d))
}

fn messages(body: &str, date: &str) -> (Vec<EmbeddedMessage>, Vec<Option<String>>) {
    embedded_messages(body, &embedded_segments(body), carrier(date))
}

/// A Notes history: each message under its own header block is read with
/// its own author, date and lines, and the carrier's segments name them.
/// Failing input, named: ward-story tep/10., Tracy Ngo's 12/19 message
/// ("The cp is looking for a power swap this summer.") read in no window.
#[test]
fn each_message_of_a_notes_history_is_read_with_its_own_header() {
    let body = "Ed, see Tracy's note.\n\nKim Ward\n01/02/2001 08:13 AM\nTo: Tracy Ngo/PDX/ECT@ECT\n\
                cc:\nSubject: Re: TEP\n\nPushing on Tucson.\n\nTracy Ngo\n12/19/2000 02:13 PM\n\
                To: Ed Sacks/HOU/ECT@ECT\ncc:\nSubject: TEP\n\nThe cp is looking for a power swap this summer.\n";
    let (found, opens) = messages(body, "Tue, 2 Jan 2001 17:01:00 -0800 (PST)");
    assert_eq!(found.len(), 2, "{found:#?}");
    assert_eq!(found[0].from.as_deref(), Some("Kim Ward"));
    assert_eq!(found[0].text, "Pushing on Tucson.");
    assert_eq!(found[1].from.as_deref(), Some("Tracy Ngo"));
    assert_eq!(found[1].subject.as_deref(), Some("TEP"));
    assert_eq!(
        found[1].text,
        "The cp is looking for a power swap this summer."
    );
    let Dated::Header(d) = found[1].date else {
        panic!("{:?}", found[1].date)
    };
    assert_eq!(d.to_rfc2822(), "Tue, 19 Dec 2000 14:13:00 -0800");
    assert_eq!(
        opens,
        [Some(found[0].key.clone()), Some(found[1].key.clone())]
    );
}

/// A forward quoted line by line: the message under the quote marks is read
/// unmarked, with the header under its separator. Failing input, named:
/// ward-story tep/3., whose every line is `>`-marked.
#[test]
fn a_message_inside_a_quoted_run_is_read_unmarked() {
    let body = "> -----Original Message-----\n> From: Janette McClintock\n\
                > Sent: Friday, March 09, 2001 10:33 AM\n> To: Dave Hutchens\n> Subject: Bid\n>\n\
                > Attached is the Invitation to Bid for EPNG pipeline capacity.\n\
                \n - INVITATION FOR BID.doc\n";
    let (found, opens) = messages(body, "Fri, 9 Mar 2001 12:33:00 -0800 (PST)");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].from.as_deref(), Some("Janette McClintock"));
    assert_eq!(
        found[0].text,
        "Attached is the Invitation to Bid for EPNG pipeline capacity."
    );
    assert!(
        matches!(found[0].date, Dated::Header(_)),
        "{:?}",
        found[0].date
    );
    assert_eq!(opens, [Some(found[0].key.clone())]);
}

/// A date is the header's when it reads as one date before the carrier's;
/// otherwise the message is known only to precede its carrier, and says why.
#[test]
fn a_header_date_is_read_only_as_one_date_before_the_carrier() {
    let at = |w: &str| dated(Some(w), carrier("Tue, 2 Jan 2001 17:01:00 -0800"));
    // 01/02 is 2 January: 1 February would follow the carrier.
    assert!(
        matches!(at("01/02/2001 08:13 AM"), Dated::Header(d) if d.to_rfc2822() == "Tue, 2 Jan 2001 08:13:00 -0800")
    );
    assert!(matches!(at("29/12/2000 16:39"), Dated::Header(_)));
    assert!(matches!(
        at("Thursday, December 28, 2000 1:16 PM"),
        Dated::Header(_)
    ));
    assert!(matches!(
        at("12/11/2000 09:00 AM"),
        Dated::BeforeCarrier { why, .. } if why.contains("either way")
    ));
    assert!(matches!(
        at("01/03/2001 08:13 AM"),
        Dated::BeforeCarrier { why, .. } if why.contains("not before")
    ));
    assert!(matches!(
        dated(None, carrier("Tue, 2 Jan 2001 17:01:00 -0800")),
        Dated::BeforeCarrier { why, .. } if why.contains("no date")
    ));
    assert!(matches!(
        at("sometime last week"),
        Dated::BeforeCarrier { why, .. } if why.contains("no form read here")
    ));
    assert_eq!(dated(Some("12/11/2000"), None), Dated::Unknown);
    let (stamp, basis) = at("12/11/2000 09:00 AM").stamp();
    assert_eq!(stamp.as_deref(), Some("Tue, 2 Jan 2001 17:00:59 -0800"));
    assert!(basis.starts_with("before its carrier"), "{basis}");
}

/// The same message carried by two replies, wrapped differently, is one
/// message; the same words from someone else are another.
#[test]
fn a_message_is_identified_by_who_said_what() {
    let one = key(
        Some("Thomas Boettcher@ENRON"),
        "Please send the\nconfirm today.",
    );
    let again = key(
        Some("Thomas Boettcher <tb@enron.example>"),
        "Please send the confirm\ntoday.",
    );
    let other = key(Some("Kim Ward"), "Please send the confirm today.");
    assert_eq!(one, again);
    assert_ne!(one, other);
    assert!(one.starts_with("embedded-"));
}
