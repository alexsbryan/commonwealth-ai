// SPDX-License-Identifier: AGPL-3.0-or-later
//! The golden bank: the aligner's output over `bank/bank.txt`, frozen in
//! `bank/golden.txt` under the `ALIGNER_ID` it was blessed for. The output
//! may change only together with `ALIGNER_ID` (ADDRESSED_TEXT §5.3,
//! Identity): a client caches alignments keyed on that id.
//!
//! Re-bless after a deliberate change: bump `ALIGNER_ID`, run this crate's
//! tests with `UPDATE_QUOTE_ALIGN_GOLDEN=1`, and review every changed case
//! in the diff.

use std::path::PathBuf;

use crate::{align, code_point_slice, AlignConfig, Aligned, QuoteEditKind, ALIGNER_ID};

const BANK: &str = include_str!("../bank/bank.txt");

struct Case {
    name: String,
    texts: Vec<String>,
    quote: String,
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let hex: String = chars
                    .by_ref()
                    .skip_while(|c| *c == '{')
                    .take_while(|c| *c != '}')
                    .collect();
                let code = u32::from_str_radix(&hex, 16).expect("bank: bad \\u{..}");
                out.push(char::from_u32(code).expect("bank: not a scalar value"));
            }
            other => panic!("bank: unknown escape \\{other:?}"),
        }
    }
    out
}

fn bank() -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    for line in BANK.lines() {
        if let Some(name) = line.strip_prefix("=== ") {
            cases.push(Case {
                name: name.trim().to_string(),
                texts: Vec::new(),
                quote: String::new(),
            });
        } else if let Some(t) = line.strip_prefix("text: ") {
            cases
                .last_mut()
                .expect("text before a case")
                .texts
                .push(unescape(t));
        } else if let Some(q) = line.strip_prefix("quote: ") {
            cases.last_mut().expect("quote before a case").quote = unescape(q);
        } else {
            assert!(
                line.is_empty() || line.starts_with('#'),
                "bank: unreadable line {line:?}"
            );
        }
    }
    assert!(cases
        .iter()
        .all(|c| !c.texts.is_empty() && !c.quote.is_empty()));
    cases
}

fn kind(k: QuoteEditKind) -> &'static str {
    match k {
        QuoteEditKind::Substituted => "substituted",
        QuoteEditKind::Added => "added",
        QuoteEditKind::Omitted => "omitted",
        QuoteEditKind::Elided => "elided",
        QuoteEditKind::Bracketed => "bracketed",
    }
}

/// One case's verdict, every range with the exact strings it names.
fn render_case(case: &Case, found: &Aligned) -> String {
    let mut out = format!(
        "=== {}\nanchors={} below_floor={}\n",
        case.name, found.anchors, found.below_floor
    );
    for a in &found.alignments {
        let text = &case.texts[a.text];
        let exact = code_point_slice(text, a.source.clone()).expect("span inside its text");
        out += &format!(
            "alignment text={} source=[{}, {}) coverage={:.4} {:?}\n",
            a.text, a.source.start, a.source.end, a.coverage, exact
        );
        for e in &a.edits {
            out += &format!(
                "  {} quote=[{}, {}) {:?} source=[{}, {}) {:?}\n",
                kind(e.kind),
                e.quote.start,
                e.quote.end,
                code_point_slice(&case.quote, e.quote.clone()).expect("edit inside the quote"),
                e.source.start,
                e.source.end,
                code_point_slice(text, e.source.clone()).expect("edit inside its text"),
            );
        }
    }
    out
}

fn render_bank(aligner: &dyn Fn(&str, &[&str]) -> Aligned) -> String {
    let mut out = format!("aligner: {ALIGNER_ID}\n");
    for case in bank() {
        let texts: Vec<&str> = case.texts.iter().map(String::as_str).collect();
        out += "\n";
        out += &render_case(&case, &aligner(&case.quote, &texts));
    }
    out
}

/// Names of the cases whose rendering differs from the golden file's.
fn changed_cases(rendered: &str, golden: &str) -> Vec<String> {
    let blocks = |s: &str| -> Vec<(String, String)> {
        s.split("\n=== ")
            .skip(1)
            .map(|b| {
                let name = b.lines().next().unwrap_or("").to_string();
                (name, b.trim_end().to_string())
            })
            .collect()
    };
    let (now, then) = (blocks(rendered), blocks(golden));
    let mut changed: Vec<String> = now
        .iter()
        .filter(|(name, body)| !then.iter().any(|(n, b)| n == name && b == body))
        .map(|(name, _)| name.clone())
        .collect();
    changed.extend(
        then.iter()
            .filter(|(n, _)| !now.iter().any(|(m, _)| m == n))
            .map(|(n, _)| format!("{n} (gone)")),
    );
    changed
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bank/golden.txt")
}

fn shipped(quote: &str, texts: &[&str]) -> Aligned {
    align(
        quote,
        texts,
        &AlignConfig::shipped().expect("the shipped align.toml loads"),
    )
}

#[test]
fn the_golden_bank_changes_only_with_the_aligner_id() {
    let rendered = render_bank(&shipped);
    if std::env::var("UPDATE_QUOTE_ALIGN_GOLDEN").is_ok() {
        std::fs::write(golden_path(), &rendered).expect("write bank/golden.txt");
        return;
    }
    let golden = std::fs::read_to_string(golden_path()).expect("bank/golden.txt is committed");
    let blessed_for = golden.lines().next().unwrap_or("");
    assert_eq!(
        blessed_for,
        format!("aligner: {ALIGNER_ID}"),
        "golden.txt was blessed for another aligner: re-bless with \
         UPDATE_QUOTE_ALIGN_GOLDEN=1 and review every changed case"
    );
    let changed = changed_cases(&rendered, &golden);
    assert!(
        changed.is_empty(),
        "the aligner's output changed on {changed:?} while ALIGNER_ID is still \
         {ALIGNER_ID:?}. A cached alignment keyed on that id is now wrong: bump \
         ALIGNER_ID (the algorithm, norm_v0 or align.toml moved), then re-bless."
    );
}

/// The planted aligner of ADDRESSED_TEXT §5.3: the shipped one, except that
/// it reports every substituted word as an omission plus an addition.
fn planted(quote: &str, texts: &[&str]) -> Aligned {
    let mut found = shipped(quote, texts);
    for a in &mut found.alignments {
        a.edits = std::mem::take(&mut a.edits)
            .into_iter()
            .flat_map(|e| match e.kind {
                QuoteEditKind::Substituted => vec![
                    crate::QuoteEdit {
                        kind: QuoteEditKind::Omitted,
                        quote: e.quote.start..e.quote.start,
                        source: e.source.clone(),
                    },
                    crate::QuoteEdit {
                        kind: QuoteEditKind::Added,
                        quote: e.quote,
                        source: e.source.end..e.source.end,
                    },
                ],
                _ => vec![e],
            })
            .collect();
    }
    found
}

#[test]
fn the_golden_bank_is_red_against_the_planted_aligner() {
    let golden = std::fs::read_to_string(golden_path()).expect("bank/golden.txt is committed");
    let changed = changed_cases(&render_bank(&planted), &golden);
    for case in ["substituted_word", "substituted_at_the_end", "case_only"] {
        assert!(
            changed.iter().any(|c| c == case),
            "the bank did not see {case} go red: {changed:?}"
        );
    }
    assert!(
        !changed
            .iter()
            .any(|c| c == "verbatim" || c == "elided_interior"),
        "the plant moves only substitutions: {changed:?}"
    );
}
