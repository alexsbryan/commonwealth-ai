// SPDX-License-Identifier: AGPL-3.0-or-later
//! The messages a mail body embeds, each read as a message of its own: its
//! own lines, dated and authored by its own header (`email_segments` finds
//! where each one is). Mail knowledge, so it lives with the mail extractor.
//! For every mail corpus (research/ontology-apps/ward-rootcause-prereg.md):
//!
//! - every line has one owner: a line inside an embedded message is that
//!   message's, never its carrier's (the carrier keeps the bytes, and each
//!   of its segments names the message it holds);
//! - a message's provenance is its own: its date and author are its header's,
//!   and a date the header does not give is said so (`Dated`), never read off
//!   the carrier as if the header gave it;
//! - a message's identity is its content, who said what (`key`), so the same
//!   message carried by several replies is one message.
//!
//! Before this, an embedded message no stored document held was read
//! nowhere: ward-story tep/10.'s "The cp is looking for a power swap this
//! summer." was in no window of any read (note f9142d79).

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use sha2::{Digest, Sha256};

use super::email_segments::{
    embedded_segments, fenced_end, is_separator, names_date, EmbeddedSegment, Field, SegmentKind,
};

/// When an embedded message was written. Closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dated {
    /// Its own header says, before its carrier's date.
    Header(DateTime<FixedOffset>),
    /// Its header says nothing that reads as one date before its carrier's:
    /// it was written before its carrier, and that is all that is known.
    BeforeCarrier {
        carrier: DateTime<FixedOffset>,
        why: &'static str,
    },
    /// Neither it nor its carrier is dated.
    Unknown,
}

impl Dated {
    /// The date its document is stamped with (RFC 2822), and how it is
    /// known: a message known only to precede its carrier is stamped a
    /// second before it, so the clock orders it there and no later.
    pub fn stamp(&self) -> (Option<String>, String) {
        match self {
            Self::Header(d) => (Some(d.to_rfc2822()), "header".into()),
            Self::BeforeCarrier { carrier, why } => (
                Some((*carrier - Duration::seconds(1)).to_rfc2822()),
                format!("before its carrier: {why}"),
            ),
            Self::Unknown => (None, "unknown".into()),
        }
    }
}

/// One message a mail body embeds.
#[derive(Debug, Clone)]
pub struct EmbeddedMessage {
    /// Its identity (`key`).
    pub key: String,
    pub kind: SegmentKind,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub subject: Option<String>,
    /// The date its header writes, as written.
    pub date_written: Option<String>,
    pub date: Dated,
    /// Its own lines, quote marks removed, without the messages it embeds in
    /// turn (each of those is one of its own).
    pub text: String,
}

/// Every message `body` embeds at `segments` (`embedded_segments`), depth
/// first; and for each segment, the message it opens: its own, else the
/// first under it with a line of its own. `carrier` is the carrier's date.
pub fn embedded_messages(
    body: &str,
    segments: &[EmbeddedSegment],
    carrier: Option<DateTime<FixedOffset>>,
) -> (Vec<EmbeddedMessage>, Vec<Option<String>>) {
    let mut out = Vec::new();
    let opens = segments
        .iter()
        .map(|s| {
            read(
                &body[s.start..s.header_end],
                &body[s.header_end..s.end],
                s.kind,
                carrier,
                &mut out,
            )
        })
        .collect();
    (out, opens)
}

/// The messages `body` embeds: its segments as the carrier's metadata writes
/// them (offsets into its stored text, whose body starts at `body_start`),
/// each naming the message it opens, and each message as a document of its
/// own. `written_date` is the carrier's Date header.
pub fn layout(
    body: &str,
    body_start: usize,
    written_date: Option<&str>,
    carrier: &Carrier<'_>,
) -> (Vec<serde_json::Value>, Vec<super::ExtractedDoc>) {
    let found = embedded_segments(body);
    let (messages, opens) = embedded_messages(body, &found, rfc2822(written_date));
    let segments = found
        .iter()
        .zip(&opens)
        .map(|(s, message)| {
            serde_json::json!({
                "start": s.start + body_start,
                "end": s.end + body_start,
                "header_end": s.header_end + body_start,
                "kind": s.kind,
                "message": message,
            })
        })
        .collect();
    let documents = messages.into_iter().map(|m| document(m, carrier)).collect();
    (segments, documents)
}

/// What an embedded message's document takes from the message carrying it:
/// where it was found, and the thread it is in.
pub struct Carrier<'a> {
    pub key: &'a str,
    pub thread_id: &'a str,
    pub base_subject: Option<&'a str>,
    pub path: &'a std::path::Path,
}

/// `m` as a document of its own: its header written as a preamble (its
/// `body_start`), its lines after it, stamped with its own date and how that
/// is known (`date_basis`), and `embedded_in` naming its carrier, which marks
/// it as a message no store holds on its own (`line_classes`: it never takes
/// a line a stored message holds). Its subject is its header's, else its
/// carrier's thread's, and says which (`subject_basis`).
pub fn document(m: EmbeddedMessage, carrier: &Carrier<'_>) -> super::ExtractedDoc {
    let (date, date_basis) = m.date.stamp();
    let (base_subject, subject_basis) = match m
        .subject
        .as_deref()
        .and_then(super::email_base_subject::base_subject)
    {
        Some(s) => (Some(s), "header"),
        None => (carrier.base_subject.map(str::to_string), "carrier"),
    };
    let preamble = super::email_rfc5322::build_header_preamble(
        m.from.as_deref(),
        m.to.as_deref(),
        m.cc.as_deref(),
        m.date_written.as_deref(),
        m.subject.as_deref(),
    );
    tracing::debug!(carrier = carrier.key, message = %m.key, kind = ?m.kind, %date_basis, subject_basis, chars = m.text.len(), "email_embedded: an embedded message read as a document of its own");
    let metadata = serde_json::json!({
        "doc_type": "email",
        "message_id": m.key,
        "thread_id": carrier.thread_id,
        "embedded_in": carrier.key,
        "from": m.from,
        "to": m.to,
        "cc": m.cc,
        "date": date,
        "date_basis": date_basis,
        "subject": m.subject,
        "base_subject": base_subject,
        "subject_basis": subject_basis,
        "source_path": carrier.path.to_string_lossy(),
        "body_start": preamble.len(),
        "segments": [],
    });
    super::ExtractedDoc {
        title: m.subject,
        content: preamble + &m.text,
        url: None,
        source_id: m.key,
        metadata: Some(metadata),
        source_file: carrier
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string),
        embed_text: None,
        source: super::DocSource::Record,
    }
}

/// A carrier's own Date header, RFC 2822, a trailing zone name aside.
pub fn rfc2822(written: Option<&str>) -> Option<DateTime<FixedOffset>> {
    let w = written?.trim();
    DateTime::parse_from_rfc2822(w.split(" (").next().unwrap_or(w)).ok()
}

fn read(
    header: &str,
    content: &str,
    kind: SegmentKind,
    before: Option<DateTime<FixedOffset>>,
    out: &mut Vec<EmbeddedMessage>,
) -> Option<String> {
    let text = if kind == SegmentKind::Quote {
        unmarked(content)
    } else {
        content.to_string()
    };
    let inner = embedded_segments(&text);
    let (mut own, mut at) = (String::new(), 0);
    for s in &inner {
        own.push_str(&text[at..s.start]);
        at = s.end;
    }
    own.push_str(&text[at..]);
    let head = Head::of(header);
    let date = dated(head.date.as_deref(), before);
    // What it embeds was written before it: bounded by its date when its
    // header gives one, else by what bounds it.
    let under = match &date {
        Dated::Header(d) => Some(*d),
        _ => before,
    };
    let mine = own.chars().any(char::is_alphanumeric).then(|| {
        let key = key(head.from.as_deref(), &own);
        out.push(EmbeddedMessage {
            key: key.clone(),
            kind,
            from: head.from,
            to: head.to,
            cc: head.cc,
            subject: head.subject,
            date_written: head.date,
            date,
            text: own
                .trim_matches(|c: char| c == '\n' || c == '\r')
                .to_string(),
        });
        key
    });
    let mut first = None;
    for s in &inner {
        let key = read(
            &text[s.start..s.header_end],
            &text[s.header_end..s.end],
            s.kind,
            under,
            out,
        );
        first = first.or(key);
    }
    mine.or(first)
}

/// A quoted run's lines with one level of quote mark removed.
fn unmarked(content: &str) -> String {
    content
        .split('\n')
        .map(|line| match line.trim_start().strip_prefix('>') {
            Some(rest) => rest.strip_prefix(' ').unwrap_or(rest),
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What an embedded message's header block says of it.
#[derive(Debug, Default)]
struct Head {
    from: Option<String>,
    to: Option<String>,
    cc: Option<String>,
    subject: Option<String>,
    date: Option<String>,
}

impl Head {
    /// Fields (`From: Ann on 12/12/2000 11:47 AM` gives both), the sender and
    /// date lines a Notes client writes above them, or an attribution ("On
    /// <date>, <name> wrote:"). A separator names who forwarded the message,
    /// not who wrote it, so it says neither.
    fn of(block: &str) -> Self {
        let some = |v: &str| Some(v.trim().to_string()).filter(|v| !v.is_empty());
        let mut h = Self::default();
        for line in block.lines() {
            let t = line.trim();
            if t.is_empty() || is_separator(t).is_some() || fenced_end(t) {
                continue;
            }
            if let Some((field, value)) = Field::of(t) {
                match field {
                    Field::From => match value.split_once(" on ").filter(|(_, w)| names_date(w)) {
                        Some((who, when)) => {
                            h.from = some(who);
                            h.date = h.date.or_else(|| some(when));
                        }
                        None => h.from = some(value),
                    },
                    Field::Sent | Field::Date => h.date = some(value),
                    Field::To => h.to = some(value),
                    Field::Cc => h.cc = some(value),
                    Field::Subject => h.subject = some(value),
                    Field::Bcc | Field::SentBy => {}
                }
            } else if let Some(rest) = t.strip_suffix("wrote:") {
                let rest = rest.trim().trim_end_matches(',');
                let rest = rest.strip_prefix("On ").unwrap_or(rest);
                match rest.rsplit_once(", ") {
                    Some((when, who)) if when.chars().any(|c| c.is_ascii_digit()) => {
                        h.date = some(when);
                        h.from = some(who);
                    }
                    _ => h.from = some(rest),
                }
            } else if names_date(t) {
                h.date = h.date.or_else(|| some(t));
            } else {
                // Any other line of a header block is its sender's, as Notes
                // writes "Kim Ward" or "Name/ORG@ORG" over the date.
                h.from = h.from.or_else(|| some(t));
            }
        }
        h
    }
}

/// The one date a header's written date can be, before `carrier`.
fn dated(written: Option<&str>, carrier: Option<DateTime<FixedOffset>>) -> Dated {
    let zone = carrier.map_or(FixedOffset::east_opt(0).expect("zero offset"), |c| {
        *c.offset()
    });
    let all = written.map(|w| readings(w, zone)).unwrap_or_default();
    if let (Some(w), true) = (written, all.is_empty()) {
        tracing::debug!(
            written = w,
            "email_embedded: a header date in no form read here"
        );
    }
    let Some(carrier) = carrier else {
        return match all.as_slice() {
            [one] => Dated::Header(*one),
            _ => Dated::Unknown,
        };
    };
    let before: Vec<&DateTime<FixedOffset>> = all.iter().filter(|d| **d < carrier).collect();
    match before.as_slice() {
        [one] => Dated::Header(**one),
        [] if written.is_none() => Dated::BeforeCarrier {
            carrier,
            why: "its header gives no date",
        },
        [] if all.is_empty() => Dated::BeforeCarrier {
            carrier,
            why: "its header's date is in no form read here",
        },
        [] => Dated::BeforeCarrier {
            carrier,
            why: "its header's date is not before its carrier's",
        },
        _ => Dated::BeforeCarrier {
            carrier,
            why: "its header's day and month read either way",
        },
    }
}

/// Every date `written` can be read as: RFC 2822; Outlook's "Tuesday, March
/// 27, 2001 1:16 PM"; a client's "03/26/2001 03:51 PM", month and day either
/// way when both are possible. A time written without a zone is the
/// carrier's (`zone`).
fn readings(written: &str, zone: FixedOffset) -> Vec<DateTime<FixedOffset>> {
    let w = written.trim();
    if let Some(d) = rfc2822(Some(w)) {
        return vec![d];
    }
    let local = |n: NaiveDateTime| zone.from_local_datetime(&n).single();
    let long = w.split_once(", ").map_or(w, |(day, rest)| {
        if day.chars().all(char::is_alphabetic) {
            rest
        } else {
            w
        }
    });
    for f in ["%B %d, %Y %I:%M %p", "%B %d, %Y %H:%M"] {
        if let Some(d) = NaiveDateTime::parse_from_str(long, f).ok().and_then(local) {
            return vec![d];
        }
    }
    let tokens: Vec<&str> = w.split_whitespace().collect();
    let Some((a, b, y)) = tokens.iter().find_map(|t| {
        let p: Vec<u32> = t
            .trim_matches(',')
            .split('/')
            .map(|n| n.parse().ok())
            .collect::<Option<Vec<u32>>>()?;
        (p.len() == 3).then(|| (p[0], p[1], p[2]))
    }) else {
        return Vec::new();
    };
    let y = match y {
        0..=49 => 2000 + y,
        50..=99 => 1900 + y,
        _ => y,
    } as i32;
    let pm = tokens.iter().any(|t| t.eq_ignore_ascii_case("pm"));
    let am = tokens.iter().any(|t| t.eq_ignore_ascii_case("am"));
    let time = tokens
        .iter()
        .find_map(|t| {
            let (h, m) = t.split_once(':')?;
            let (h, m): (u32, u32) = (h.parse().ok()?, m.get(..2)?.parse().ok()?);
            let h = match (am, pm, h) {
                (_, true, 1..=11) => h + 12,
                (true, _, 12) => 0,
                _ => h,
            };
            NaiveTime::from_hms_opt(h, m, 0)
        })
        .unwrap_or(NaiveTime::MIN);
    let mut out: Vec<DateTime<FixedOffset>> = [(a, b), (b, a)]
        .iter()
        .filter_map(|&(m, d)| NaiveDate::from_ymd_opt(y, m, d))
        .filter_map(|d| local(d.and_time(time)))
        .collect();
    out.dedup();
    out
}

/// A message's identity: its author's name and its own words, case,
/// punctuation and whitespace folded, so the same message written into
/// several replies, wrapped alike or not, is one, and two people's identical
/// words are two. The name is what the header writes before an address.
fn key(from: Option<&str>, text: &str) -> String {
    let who = from.map_or("", |f| f.split(['<', '/', '@', '[']).next().unwrap_or(f));
    let fold = |s: &str| {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let digest = Sha256::digest(format!("{}\n{}", fold(who), fold(text)));
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("embedded-{hex}")
}

#[cfg(test)]
#[path = "email_embedded_tests.rs"]
mod tests;
