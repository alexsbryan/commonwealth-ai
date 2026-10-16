// SPDX-License-Identifier: AGPL-3.0-or-later
//! Transcripts read by turn (v0.6 §2.14): a recipe document that declares
//! `format = "transcript"` is read as a paragraph transcript or Whisper's
//! JSON, its stored text the words alone and each turn's start and speaker
//! the locator of every span that starts in it.
//!
//! The rule is here, beside [`crate::locator`], so the reference host's
//! extractor and a test host read transcripts by one implementation; the
//! conformance fixture's expected locators are data.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::locator::{PlacedLocator, PlacedLocators, SpanLocator};

/// A recipe `[[document]]`'s `format`: how its source is read. Absent, the
/// recipe's extractor reads it as it reads any document. A closed set: a
/// value outside it is a recipe that does not load, never a document read
/// as plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    /// A transcript, read by turn ([`TurnOutline::read`]).
    Transcript,
}

/// The transcript shapes v0.6 reads, detected from the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptShape {
    /// Header lines of a speaker and a timestamp, each followed by its turn.
    Paragraph,
    /// Whisper's JSON: an object whose `segments` carry `start` (seconds)
    /// and `text`, and no speakers.
    WhisperJson,
}

/// v0.6 §2.14's named reason.
pub mod reasons {
    /// A document declared `format = "transcript"` that is in neither
    /// shape: listed `failed` (v0.6 §2.9), never read as plain text.
    pub const NOT_A_TRANSCRIPT: &str = "not a transcript";
}

/// The longest speaker name, in words, a header line carries.
pub const SPEAKER_MAX_WORDS: usize = 6;

/// Where a turn starts in the stored text, and its locator's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStart {
    /// The turn's first code point in the stored text.
    pub at: u64,
    /// When the turn starts in the recording.
    pub milliseconds: u64,
    /// Who speaks it, when the transcript names them.
    pub speaker: Option<String>,
}

/// A transcript read by turn: the text stored for it and where each turn
/// starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutline {
    /// The shape it was read as.
    pub shape: TranscriptShape,
    /// The words: what is stored, so a quotation is the speaker's bytes.
    pub text: String,
    /// The turns, in order.
    pub turns: Vec<TurnStart>,
}

impl TurnOutline {
    /// Read `source` as Whisper's JSON when it is a JSON object with a
    /// `segments` array, else as a paragraph transcript. `None` when it is
    /// neither: [`reasons::NOT_A_TRANSCRIPT`].
    pub fn read(source: &str) -> Option<Self> {
        match serde_json::from_str::<Value>(source) {
            Ok(v) if v.get("segments").is_some_and(Value::is_array) => whisper(&v),
            _ => paragraph(source),
        }
    }

    /// The turns as the places a host keeps beside the stored text.
    pub fn placed(&self) -> PlacedLocators {
        PlacedLocators(
            self.turns
                .iter()
                .map(|t| PlacedLocator {
                    at: t.at,
                    locator: SpanLocator::Timestamp {
                        milliseconds: t.milliseconds,
                        speaker: t.speaker.clone(),
                    },
                })
                .collect(),
        )
    }

    /// The locator at code point `at`: the turn it falls in. `None` before
    /// the first turn (a paragraph transcript's opening lines).
    pub fn at(&self, at: u64) -> Option<SpanLocator> {
        self.turns
            .iter()
            .rev()
            .find(|t| t.at <= at)
            .map(|t| SpanLocator::Timestamp {
                milliseconds: t.milliseconds,
                speaker: t.speaker.clone(),
            })
    }
}

/// Whisper's segments, each a turn with no speaker, their texts trimmed and
/// joined by one space, so a sentence may run across segments and takes the
/// first one's time.
fn whisper(v: &Value) -> Option<TurnOutline> {
    let mut text = String::new();
    let mut turns = Vec::new();
    for seg in v.get("segments")?.as_array()? {
        let start = seg.get("start")?.as_f64()?;
        let words = seg.get("text")?.as_str()?.trim();
        if !start.is_finite() || start < 0.0 {
            return None;
        }
        if words.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        turns.push(TurnStart {
            at: text.chars().count() as u64,
            milliseconds: (start * 1000.0).round() as u64,
            speaker: None,
        });
        text.push_str(words);
    }
    (!turns.is_empty()).then_some(TurnOutline {
        shape: TranscriptShape::WhisperJson,
        text,
        turns,
    })
}

/// A paragraph transcript: each header line opens a turn of the lines after
/// it; lines before the first header are kept as the text's opening, under
/// no turn. Paragraphs (the opening, then each turn's lines, trimmed) are
/// joined by a blank line; a header with no words after it opens nothing.
fn paragraph(source: &str) -> Option<TurnOutline> {
    let mut parts: Vec<(Option<(u64, Option<String>)>, String)> = vec![(None, String::new())];
    for line in source.split_inclusive('\n') {
        match header(line) {
            Some(h) => parts.push((Some(h), String::new())),
            None => parts.last_mut()?.1.push_str(line),
        }
    }
    if parts.len() == 1 {
        return None;
    }
    let mut text = String::new();
    let mut turns = Vec::new();
    for (head, words) in parts {
        let words = words.trim();
        if words.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        if let Some((milliseconds, speaker)) = head {
            turns.push(TurnStart {
                at: text.chars().count() as u64,
                milliseconds,
                speaker,
            });
        }
        text.push_str(words);
    }
    (!turns.is_empty()).then_some(TurnOutline {
        shape: TranscriptShape::Paragraph,
        text,
        turns,
    })
}

/// A header line: one timestamp, first or last, and the rest a speaker of
/// at most [`SPEAKER_MAX_WORDS`] words, each opening with a capital, a
/// digit or a bracket, with `:` `-` `–` `—` `|` trimmed from its ends. A
/// timestamp alone is a turn with no speaker.
fn header(line: &str) -> Option<(u64, Option<String>)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let last = words.len().checked_sub(1)?;
    let (ms, rest) = match (timestamp(words[0]), timestamp(words[last])) {
        (Some(ms), _) => (ms, &words[1..]),
        (None, Some(ms)) => (ms, &words[..last]),
        (None, None) => return None,
    };
    if rest.iter().any(|w| timestamp(w).is_some()) {
        return None;
    }
    let joined = rest.join(" ");
    let name = joined.trim_matches(|c: char| c.is_whitespace() || ":-–—|".contains(c));
    if name.is_empty() {
        return Some((ms, None));
    }
    let named: Vec<&str> = name.split(' ').collect();
    let opens = |w: &&str| {
        w.chars()
            .next()
            .is_some_and(|c| c.is_uppercase() || c.is_ascii_digit() || c == '(' || c == '[')
    };
    if named.len() > SPEAKER_MAX_WORDS
        || name.ends_with(['.', '!', '?'])
        || !named.iter().all(opens)
    {
        return None;
    }
    Some((ms, Some(name.to_string())))
}

/// A timestamp token, in milliseconds: `H:MM:SS` (any number of hours) or
/// `M:SS`/`MM:SS`, with an optional `.` or `,` fraction of up to three
/// digits, optionally in `()` or `[]` and followed by `:` or `,`.
fn timestamp(token: &str) -> Option<u64> {
    let t = token.trim_end_matches([':', ',']);
    let t = t
        .strip_prefix('(')
        .and_then(|t| t.strip_suffix(')'))
        .or_else(|| t.strip_prefix('[').and_then(|t| t.strip_suffix(']')))
        .unwrap_or(t);
    let (clock, frac) = match t.split_once(['.', ',']) {
        Some((c, f)) if (1..=3).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_digit()) => {
            (c, format!("{f:0<3}").parse::<u64>().ok()?)
        }
        Some(_) => return None,
        None => (t, 0),
    };
    let fields: Vec<&str> = clock.split(':').collect();
    let num = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    let sixty = |s: &str| num(s).filter(|n| s.len() == 2 && *n < 60);
    let seconds = match fields.as_slice() {
        [h, m, s] => num(h)? * 3600 + sixty(m)? * 60 + sixty(s)?,
        [m, s] if m.len() <= 2 => num(m)? * 60 + sixty(s)?,
        _ => return None,
    };
    Some(seconds * 1000 + frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn_of(o: &TurnOutline, quote: &str) -> Option<(u64, Option<String>)> {
        let at = o.text[..o.text.find(quote).unwrap()].chars().count() as u64;
        match o.at(at)? {
            SpanLocator::Timestamp {
                milliseconds,
                speaker,
            } => Some((milliseconds, speaker)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_paragraph_transcript_stores_the_words_and_each_turn_its_header() {
        let src = "Recorded at the quay.\n\nHARBOUR MASTER  00:00:04\nThe ferry is late.\n\n\
                   Interviewer (00:00:11):\nWhy?\n[01:02:03.5] Dr. Ada Lake -\nTables.\nAt 10:30 we left.\n\
                   We left at 10:30\n\n00:59\nThe end.\n";
        let o = TurnOutline::read(src).unwrap();
        assert_eq!(o.shape, TranscriptShape::Paragraph);
        assert_eq!(
            o.text,
            "Recorded at the quay.\n\nThe ferry is late.\n\nWhy?\n\nTables.\nAt 10:30 we left.\n\
             We left at 10:30\n\nThe end."
        );
        assert_eq!(turn_of(&o, "Recorded"), None);
        assert_eq!(
            turn_of(&o, "The ferry"),
            Some((4000, Some("HARBOUR MASTER".into())))
        );
        assert_eq!(
            turn_of(&o, "Why?"),
            Some((11000, Some("Interviewer".into())))
        );
        assert_eq!(
            turn_of(&o, "At 10:30"),
            Some((3_723_500, Some("Dr. Ada Lake".into())))
        );
        assert_eq!(turn_of(&o, "The end."), Some((59_000, None)));
    }

    #[test]
    fn whisper_segments_are_turns_with_no_speaker_and_a_sentence_takes_its_first_time() {
        let src = r#"{"text": "x", "segments": [
            {"start": 0.0, "end": 3.5, "text": " The tide came in early."},
            {"start": 3.5, "end": 7.25, "text": " Nobody had read"},
            {"start": 7.254, "end": 9.0, "text": " the tables yet."}]}"#;
        let o = TurnOutline::read(src).unwrap();
        assert_eq!(o.shape, TranscriptShape::WhisperJson);
        assert_eq!(
            o.text,
            "The tide came in early. Nobody had read the tables yet."
        );
        assert_eq!(
            turn_of(&o, "Nobody had read the tables"),
            Some((3500, None))
        );
        assert_eq!(turn_of(&o, "tables yet"), Some((7254, None)));
    }

    #[test]
    fn a_document_in_neither_shape_is_not_a_transcript() {
        assert_eq!(TurnOutline::read("The meeting opened at noon.\n"), None);
        assert_eq!(
            TurnOutline::read(r#"{"segments": [{"text": "no start"}]}"#),
            None
        );
        assert_eq!(timestamp("12:60"), None);
        assert_eq!(timestamp("1:2"), None);
        assert_eq!(timestamp("102:45:40,25"), Some(369_940_250));
    }

    /// The places a host stores beside the text read back, through JSON, to
    /// the locator `TurnOutline::at` gives: the daemon serves from these.
    #[test]
    fn placed_turns_survive_json_and_answer_as_the_outline_does() {
        let o = TurnOutline::read(
            "HARBOUR MASTER  00:00:04\nWe left early.\n\nInterviewer (00:00:11)\nWhy?\n",
        )
        .unwrap();
        let back: PlacedLocators =
            serde_json::from_str(&serde_json::to_string(&o.placed()).unwrap()).unwrap();
        for at in [0, 5, 20, o.text.chars().count() as u64 - 1] {
            assert_eq!(back.at(at), o.at(at), "at {at}");
        }
        assert_eq!(PlacedLocators::default().at(3), None);
    }

    #[test]
    fn the_format_set_is_closed() {
        assert_eq!(
            serde_json::from_str::<DocumentFormat>("\"transcript\"").unwrap(),
            DocumentFormat::Transcript
        );
        assert!(serde_json::from_str::<DocumentFormat>("\"subtitles\"").is_err());
    }
}
