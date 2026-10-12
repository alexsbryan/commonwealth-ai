// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compiled reader's questions (`waves.rs`). Every question about a
//! document is one system prompt and one user turn that opens with the
//! document's declared facts and its numbered lines and ends with the
//! question, so all of a document's questions share that prefix: any set of
//! them composes into one forced-choice wave (`resolve_records/wave.rs`
//! `compose`: identical but for the user turn and its labels), and the engine
//! decodes the document once per wave (`decide_wave.rs`).

use oicp_types::forced_choice;

use super::act::Unit;
use super::passes::{excerpt, Line};
use super::pick::Candidate;
use crate::enrichment::atlas::resolve_records::{ClosedAttr, NONE};
use crate::enrichment::ontology::{AttrDecl, OntologyTypeDecl};
use crate::enrichment::pipeline::types::ChatPrompt;

const WAVES_SYSTEM: &str = include_str!("passes_waves_prompt.md");
/// One phase for every question of the compiled reader: a wave is one
/// request, so its questions share one phase (`compose`).
pub(super) const WAVES_PHASE: &str = "document_waves";
/// The count question's answers: one, more than one, none (`about::count_of`).
pub(super) const COUNT_LABELS: [&str; 3] = ["A", "B", NONE];

/// A question: the shared prefix (`ask::opening`), then `suffix`.
pub(super) fn question(prefix: &str, suffix: &str, labels: &[&str]) -> ChatPrompt {
    ChatPrompt::new(WAVES_SYSTEM, format!("{prefix}{suffix}"))
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(WAVES_PHASE)
        .with_temperature(0.0)
}

fn described(name: &str, description: &str) -> String {
    if description.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({description})")
    }
}

/// A declared type of thing, with the criterion that says when two mentions
/// of it are one particular.
fn thing(of: &OntologyTypeDecl) -> String {
    let mut s = format!("Kind of thing: {}\n", described(&of.name, &of.description));
    if let Some(criterion) = &of.identity_criterion {
        s.push_str(&format!("Same particular when: {criterion}\n"));
    }
    s
}

fn claim(kind: &OntologyTypeDecl) -> String {
    format!(
        "Kind of claim: {}\n",
        described(&kind.name, &kind.description)
    )
}

/// What the compiled reader enumerates in a document.
#[derive(Debug, Clone, Copy)]
pub(super) enum Target<'a> {
    /// The particulars of a type RESOLVE decides the document is about.
    Decided(&'a OntologyTypeDecl),
    /// The particulars of a type a read reference targets it names by name.
    Mentioned(&'a OntologyTypeDecl),
    /// The claims of a kind it states.
    Kind(&'a OntologyTypeDecl),
}

impl<'a> Target<'a> {
    pub(super) fn decl(&self) -> &'a OntologyTypeDecl {
        match self {
            Target::Decided(d) | Target::Mentioned(d) | Target::Kind(d) => d,
        }
    }

    fn head(&self) -> String {
        match self {
            Target::Decided(of) | Target::Mentioned(of) => thing(of),
            Target::Kind(kind) => claim(kind),
        }
    }
}

/// What a reading is about, as its questions name it.
#[derive(Debug, Clone)]
pub(super) enum Who<'a> {
    /// A particular, by the line that first names it and its words there.
    Particular {
        of: &'a OntologyTypeDecl,
        line: usize,
        words: String,
    },
    /// The particular of `of` the document is about, none pointed at.
    It { of: &'a OntologyTypeDecl },
    /// A claim, by its lines and their text.
    Claim {
        kind: &'a OntologyTypeDecl,
        lines: (usize, usize),
        text: String,
    },
}

impl Who<'_> {
    fn head(&self) -> String {
        match self {
            Who::Particular { of, line, words } => format!(
                "{}The {}: first named or described on line {line} (\"{}\")\n",
                thing(of),
                of.name,
                excerpt(words)
            ),
            Who::It { of } => format!(
                "{}The {}: the one the document is about\n",
                thing(of),
                of.name
            ),
            Who::Claim { kind, lines, text } => format!(
                "{}The {}: stated on {} (\"{}\")\n",
                claim(kind),
                kind.name,
                at(*lines),
                excerpt(text)
            ),
        }
    }

    fn this(&self) -> String {
        match self {
            Who::Particular { of, .. } | Who::It { of } => format!("this {}", of.name),
            Who::Claim { kind, .. } => format!("this {}", kind.name),
        }
    }
}

fn at((a, b): (usize, usize)) -> String {
    if a == b {
        format!("line {a}")
    } else {
        format!("lines {a}-{b}")
    }
}

/// Whether the document holds one, several or none of `target`.
pub(super) fn count(target: Target<'_>) -> String {
    let (n, ask) = match target {
        Target::Decided(of) => (
            &of.name,
            format!("Is the document about a particular {}?", of.name),
        ),
        Target::Mentioned(of) => (
            &of.name,
            format!("Does the document name a particular {} by name?", of.name),
        ),
        Target::Kind(kind) => (
            &kind.name,
            format!("Does the document state a {}?", kind.name),
        ),
    };
    format!(
        "{}\n{ask}\n{} yes, one {n}\n{} yes, more than one {n}\n{NONE} no\nAnswer with its letter.",
        target.head(),
        COUNT_LABELS[0],
        COUNT_LABELS[1]
    )
}

/// On which of `window`'s lines the next of `target` is first named,
/// described or stated, `found` being those already pointed at.
pub(super) fn line(
    target: Target<'_>,
    within: Option<&Who<'_>>,
    found: &[(usize, String)],
    window: &[(usize, &str)],
    labels: &[&str],
) -> String {
    let mut s = target.head();
    if let Some(w) = within {
        s.push_str(&format!("About:\n{}", w.head()));
    }
    let another = !found.is_empty();
    if another {
        s.push_str("Already found:\n");
        for (n, words) in found {
            s.push_str(&format!("- line {n}: \"{}\"\n", excerpt(words)));
        }
    }
    let ask = match target {
        Target::Decided(of) if another => format!(
            "On which line is another {}, not the same particular as any already found, that the document is about first named or described?",
            of.name
        ),
        Target::Decided(of) => format!(
            "On which line is the {} the document is about first named or described?",
            of.name
        ),
        Target::Mentioned(of) if another => format!(
            "On which line is another {}, not the same particular as any already found, first named?",
            of.name
        ),
        Target::Mentioned(of) => format!("On which line is a {} first named?", of.name),
        Target::Kind(kind) => {
            let about = within.map_or(String::new(), |w| format!(" about {}", w.this()));
            if another {
                format!("On which line is another {}{about} stated, not one already found?", kind.name)
            } else {
                format!("On which line is a {}{about} stated?", kind.name)
            }
        }
    };
    s.push_str(&format!("\n{ask}\n"));
    for ((n, text), label) in window.iter().zip(labels) {
        s.push_str(&format!("{label} line {n}: {}\n", excerpt(text)));
    }
    s.push_str(&format!(
        "{NONE} none of these lines\nAnswer with its letter."
    ));
    s
}

/// The words of `text` (a line, or a claim's lines) under labels, and which
/// starts `what`; `None` labels the window's own "none of these words".
pub(super) fn word_start(
    head: &str,
    shown: &str,
    words: &[&str],
    what: &str,
    labels: &[&str],
) -> String {
    let mut s = format!("{head}{shown}\n\nThe words, each under a letter:\n");
    for (w, label) in words.iter().zip(labels) {
        s.push_str(&format!("{label} {w}\n"));
    }
    s.push_str(&format!(
        "{NONE} none of these words\nAt which word does {what} start?\nAnswer with its letter."
    ));
    s
}

/// The words from the start word on, and which ends `what`.
pub(super) fn word_end(
    head: &str,
    shown: &str,
    words: &[&str],
    what: &str,
    labels: &[&str],
) -> String {
    let mut s = format!(
        "{head}{shown}\n\nIt starts at the word \"{}\". The words from there, each under a letter:\n",
        words[0]
    );
    for (w, label) in words.iter().zip(labels) {
        s.push_str(&format!("{label} {w}\n"));
    }
    s.push_str(&format!(
        "At which word does {what} end?\nAnswer with its letter."
    ));
    s
}

/// A line as the word questions show it.
pub(super) fn shown_line(n: usize, text: &str) -> String {
    format!("Line {n}: \"{text}\"")
}

/// A claim's lines as the word questions show them.
pub(super) fn shown_claim(lines: (usize, usize), text: &str) -> String {
    format!("Statement, {}: \"{text}\"", at(lines))
}

/// What the word questions about a found particular show above its line.
pub(super) fn naming_head(target: Target<'_>) -> String {
    target.head()
}

/// What a found particular's words are asked as.
pub(super) fn naming_what(target: Target<'_>) -> String {
    match target {
        Target::Decided(of) => format!("the words that name or describe the {}", of.name),
        Target::Mentioned(of) => format!("the words that name the {}", of.name),
        Target::Kind(kind) => format!("the words that state the {}", kind.name),
    }
}

/// A field's head: who it is of, and the attribute with its family.
pub(super) fn field_head(who: &Who<'_>, attr: &AttrDecl, family: &str) -> String {
    format!(
        "{}Attribute: {}, {family}\n",
        who.head(),
        described(&attr.name, &attr.description)
    )
}

/// Which of a closed attribute's values the document gives `who`.
pub(super) fn choose(who: &Who<'_>, attr: &ClosedAttr, labels: &[&str]) -> String {
    let mut s = format!(
        "{}Attribute: {}\n\nWhich {} does the document give {}?\n",
        who.head(),
        described(&attr.name, &attr.description),
        attr.name,
        who.this()
    );
    for (v, label) in attr.values.iter().zip(labels) {
        s.push_str(&format!("{label} {v}\n"));
    }
    s.push_str(&format!(
        "{NONE} none of them, or the document does not say\nAnswer with its letter."
    ));
    s
}

/// Which candidate `who`'s reference `attr` (to `of`) names; `attr` `None`
/// asks for the claim's subject.
pub(super) fn pick(
    who: &Who<'_>,
    attr: Option<&AttrDecl>,
    of: &str,
    shown: &[Candidate],
    labels: &[&str],
) -> String {
    let mut s = who.head();
    match attr {
        Some(a) => s.push_str(&format!(
            "Attribute: {}, one {of}\n\nWhich {of} is {}'s {}?\n",
            described(&a.name, &a.description),
            who.this(),
            a.name
        )),
        None => s.push_str(&format!("\nWhich {of} is {} about?\n", who.this())),
    }
    for (c, label) in shown.iter().zip(labels) {
        s.push_str(&format!("{label} {}\n", c.shown(of)));
    }
    s.push_str(&format!(
        "{NONE} none of them, or the document does not say\nAnswer with its letter."
    ));
    s
}

/// On which of `window`'s lines the document states `who`'s `attr`.
pub(super) fn field_line(
    who: &Who<'_>,
    attr: &AttrDecl,
    family: &str,
    window: &[(usize, &str)],
    labels: &[&str],
) -> String {
    let mut s = format!(
        "{}\nOn which line does the document state {}'s {}?\n",
        field_head(who, attr, family),
        who.this(),
        attr.name
    );
    for ((n, text), label) in window.iter().zip(labels) {
        s.push_str(&format!("{label} line {n}: {}\n", excerpt(text)));
    }
    s.push_str(&format!(
        "{NONE} none of these lines\nAnswer with its letter."
    ));
    s
}

/// The act question's two wordings: the question, how it names the
/// particular when none was pointed at, and the answer that is none of the
/// values (`act.rs`'s, the system prompt's instruction now in the question).
const WORDINGS: [(&str, &str, &str); 2] = [
    (
        "Where does this document show {which} standing?",
        "the {type} it is about",
        "it shows no {type}'s {field}",
    ),
    (
        "According to this document, where does {which} stand?",
        "the {type} it concerns",
        "the document shows no {type}'s {field}",
    ),
];

pub(super) const ACT_WORDINGS: usize = WORDINGS.len();

/// Where `who` stands on the protocol field `key`, in wording `wording`.
pub(super) fn act(who: &Who<'_>, key: &ClosedAttr, wording: usize, labels: &[&str]) -> String {
    let (ask, it, none) = WORDINGS[wording];
    let of = match who {
        Who::Particular { of, .. } | Who::It { of } => *of,
        Who::Claim { kind, .. } => *kind,
    };
    let which = match who {
        Who::Particular { line, words, .. } => format!(
            "the {} first named or described on line {line} (\"{}\")",
            of.name,
            excerpt(words)
        ),
        _ => it.replace("{type}", &of.name),
    };
    let fill = |s: &str| s.replace("{type}", &of.name).replace("{field}", &key.name);
    let mut s = format!(
        "Kind of thing: {}\nAttribute: {}\n\n{}\n",
        described(&of.name, &of.description),
        described(&key.name, &key.description),
        ask.replace("{which}", &which)
    );
    for (v, label) in key.values.iter().zip(labels) {
        s.push_str(&format!("{label} {v}\n"));
    }
    s.push_str(&format!("{NONE} {}\nAnswer with its letter.", fill(none)));
    s
}

/// Which of the shown sentences shows `fact` (no "none": presence is decided).
pub(super) fn cite(fact: &str, shown: &[&Unit], labels: &[&str]) -> String {
    let mut s = format!("{fact}\n\nWhich of these sentences of the document shows this fact?\n");
    for (u, label) in shown.iter().zip(labels) {
        s.push_str(&format!("{label} {}\n", u.text));
    }
    s.push_str("Answer with its letter.");
    s
}

/// The asked lines of a window as `(number, text)`.
pub(super) fn window<'b>(body: &'b str, lines: &[&Line]) -> Vec<(usize, &'b str)> {
    lines.iter().map(|l| (l.n, &body[l.start..l.end])).collect()
}
