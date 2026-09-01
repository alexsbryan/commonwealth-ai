// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parses `research/clean-room/REQUIREMENTS.md` into a [`RequirementRegistry`].
//!
//! # Why a parser and not a hand-copied list
//!
//! An earlier count of this specification said **513 requirements, 479 MUST**.
//! The truth is **625 / 591** — wrong by 112 — because the regex that produced
//! it matched only `**ID-N (MUST).**`, and §8 (`D6 — Grounding`, the domain the
//! spec calls "the product's central claim") declares its level once in a
//! preamble instead of per requirement. Fifty-three requirements vanished
//! silently, and the count still looked plausible.
//!
//! Everything defensive here descends from that (ARCH §18.4 — validate the
//! instrument before the result):
//!
//! - Six declaration forms are handled, and **an unrecognised one panics**.
//! - A **bare** declaration takes its level from [`SECTION_DEFAULT_LEVEL`], and a
//!   bare declaration in a section absent from that table **panics** rather than
//!   defaulting to `Must`. That default is the exact bug above.
//! - `§17`'s out-of-scope entries and the `ST-16 … ST-20` aliases are *parsed and
//!   labelled*, never dropped: a denominator that can be shrunk by omission is
//!   not a denominator (ARCH §18.3).
//!
//! Included with `#[path]` by `tests/requirements_registry.rs`; it is not a test
//! target of its own.

use kernel_types::conformance::{
    AcceptanceScenario, ReqLevel, Requirement, RequirementRegistry,
};
use kernel_types::ContentHash;

/// The specification, relative to the repo root.
pub const SPEC: &str = "research/clean-room/REQUIREMENTS.md";

// ─── Section defaults ───────────────────────────────────────────────────────

/// Sections that declare their level in a preamble instead of per requirement.
///
/// Looked up by `###` number first, then `##` number. **A bare declaration in a
/// section absent from this table is a panic**, not a silent `Must` — that
/// default is what lost the 53-requirement `GR` family from an earlier count.
/// Each entry carries the evidence for its level so the judgement is auditable
/// rather than remembered.
const SECTION_DEFAULT_LEVEL: &[(&str, ReqLevel, &str)] = &[
    (
        "8",
        ReqLevel::Must,
        "REQUIREMENTS.md:1507 — \"This domain is the product's central claim. Every requirement here is a MUST.\"",
    ),
    (
        "15.2",
        ReqLevel::Must,
        "§15.2 Scale targets declares no level. ADJUDICATED must-class 2026-08-31 so NF-10..NF-13 \
         cannot leave the denominator by omission (ARCH §18.3); their enforceability is Review.",
    ),
    (
        "16",
        ReqLevel::Must,
        "§16 declares ACCEPTANCE SCENARIOS (A-1..A-19), parsed as Scenario, never as requirements. \
         The level is never read.",
    ),
    (
        "17",
        ReqLevel::OutOfScope,
        "REQUIREMENTS.md:3456 — \"A rebuild MAY address them; it MUST NOT be judged for not addressing them.\"",
    ),
];

// ─── Parsing ────────────────────────────────────────────────────────────────

/// A declaration head: `GR-19` or `GR-2 (INVARIANT)`, i.e. the text between the
/// opening `**` and the first `.**`. Returns `None` for every other bold line
/// (`**Status:**`, `**BAR:**`, `**Tier 0 — unretrofittable.**`), which is what
/// keeps prose out of the registry.
fn declaration_head(line: &str) -> Option<(&str, Option<&str>)> {
    let rest = line.strip_prefix("**")?;
    let end = rest.find(".**")?;
    let head = &rest[..end];
    let (id, qual) = match head.strip_suffix(')') {
        Some(open) => match open.rfind(" (") {
            Some(at) => (&open[..at], Some(&open[at + 2..])),
            None => return None,
        },
        None => (head, None),
    };
    is_id(id).then_some((id, qual))
}

/// `FE-141`, `X-EH-2`. Uppercase family segments joined by `-`, then `-` and
/// digits. Deliberately strict: it is the only thing separating a declaration
/// from a bold sentence.
fn is_id(s: &str) -> bool {
    let Some((family, n)) = s.rsplit_once('-') else {
        return false;
    };
    !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
        && !family.is_empty()
        && family
            .split('-')
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_uppercase()))
}

/// Resolve a declaration's level. Panics on an unrecognised qualifier and on a
/// bare declaration in a section with no declared default — both of which are
/// the parser silently losing requirements if allowed to pass.
fn resolve_level(
    qual: Option<&str>,
    h2_no: &str,
    h3_no: &str,
    id: &str,
    line_no: usize,
) -> (ReqLevel, Option<String>) {
    let Some(q) = qual else {
        for (key, level, _why) in SECTION_DEFAULT_LEVEL {
            if *key == h3_no || *key == h2_no {
                return (*level, None);
            }
        }
        panic!(
            "{SPEC}:{line_no}: `{id}` is declared bare and section §{h3_no} (§{h2_no}) has no \
             entry in SECTION_DEFAULT_LEVEL. Refusing to default to MUST — that exact default \
             lost the 53-requirement GR family from an earlier count. Add the section with the \
             spec line that declares its level."
        );
    };
    // `MUST, where EN-18 is implemented` / `INVARIANT, where RT-61 is implemented`
    if let Some((base, tail)) = q.split_once(", where ") {
        let antecedent = tail.strip_suffix(" is implemented").unwrap_or_else(|| {
            panic!("{SPEC}:{line_no}: `{id}` has conditional qualifier `{q}` in an unrecognised shape")
        });
        assert!(
            is_id(antecedent),
            "{SPEC}:{line_no}: `{id}` is conditional on `{antecedent}`, which is not a requirement id"
        );
        return (base_level(base, id, line_no), Some(antecedent.to_string()));
    }
    (base_level(q, id, line_no), None)
}

fn base_level(q: &str, id: &str, line_no: usize) -> ReqLevel {
    match q {
        "MUST" => ReqLevel::Must,
        "SHOULD" => ReqLevel::Should,
        "INVARIANT" => ReqLevel::Invariant,
        "BAR" => ReqLevel::Bar,
        other => panic!(
            "{SPEC}:{line_no}: `{id}` carries unrecognised qualifier `({other})`. The parser \
             handles MUST / SHOULD / INVARIANT / BAR / `<LEVEL>, where <ID> is implemented` / bare. \
             A new form must be added here deliberately — defaulting is how 112 requirements went \
             missing once already."
        ),
    }
}

/// The requirement's own words: the declaration line's remainder plus following
/// lines, stopping at a blank line, the NEXT DECLARATION, a heading, or the
/// `⟨why⟩` block (which is rationale, not obligation).
///
/// The stop condition is `declaration_head`, not "starts with `**`": many
/// requirements open their second line with a bold phrase (`**failing case
/// before the guard and a passing case after**`), and breaking on bare `**`
/// silently truncated them to their first line — the same class of quiet loss
/// this whole parser exists to prevent.
fn collect_text(lines: &[&str], start: usize, first: &str) -> String {
    let mut parts = vec![first.trim().to_string()];
    for line in &lines[start + 1..] {
        let t = line.trim();
        if t.is_empty()
            || t.starts_with("⟨why⟩")
            || t.starts_with('#')
            || t.contains("are stated in §")
            || declaration_head(t).is_some()
        {
            break;
        }
        parts.push(t.to_string());
    }
    parts.join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `§4.4` alias sentence, e.g.
/// `**ST-16 through ST-20** are stated in §2.2 (X-PR-1 … X-PR-5) and apply here`.
/// Any line carrying `are stated in §` that does not parse is a panic — a new
/// alias form must be handled, not skipped.
fn parse_alias_line(line: &str, line_no: usize) -> Vec<(String, String)> {
    let bold_end = line.find("** are stated in §").unwrap_or_else(|| {
        panic!("{SPEC}:{line_no}: `are stated in §` in an unrecognised shape: {line}")
    });
    let range = line[2..bold_end].trim();
    let (from, to) = range.split_once(" through ").unwrap_or_else(|| {
        panic!("{SPEC}:{line_no}: alias range `{range}` is not `<ID> through <ID>`")
    });
    let open = line[bold_end..].find('(').unwrap_or_else(|| {
        panic!("{SPEC}:{line_no}: alias line names no target list in parentheses")
    }) + bold_end;
    let close = line[open..].find(')').unwrap_or_else(|| {
        panic!("{SPEC}:{line_no}: alias target list is unterminated")
    }) + open;
    let targets: Vec<&str> = line[open + 1..close]
        .split('…')
        .map(str::trim)
        .collect();
    assert_eq!(
        targets.len(),
        2,
        "{SPEC}:{line_no}: alias target list is not `<ID> … <ID>`"
    );
    let sources = expand(from, to, line_no);
    let dests = expand(targets[0], targets[1], line_no);
    assert_eq!(
        sources.len(),
        dests.len(),
        "{SPEC}:{line_no}: alias ranges have different lengths ({} vs {})",
        sources.len(),
        dests.len()
    );
    sources.into_iter().zip(dests).collect()
}

/// `ST-16` .. `ST-20` → the five ids between them, inclusive.
fn expand(from: &str, to: &str, line_no: usize) -> Vec<String> {
    let split = |s: &str| -> (String, u32) {
        let (fam, n) = s
            .rsplit_once('-')
            .unwrap_or_else(|| panic!("{SPEC}:{line_no}: `{s}` is not an id"));
        (fam.to_string(), n.parse().expect("numeric ordinal"))
    };
    let (f1, n1) = split(from);
    let (f2, n2) = split(to);
    assert_eq!(f1, f2, "{SPEC}:{line_no}: alias range crosses families");
    assert!(n1 <= n2, "{SPEC}:{line_no}: alias range runs backwards");
    (n1..=n2).map(|n| format!("{f1}-{n}")).collect()
}

/// Requirement ids named inside a scenario's prose, first appearance first.
///
/// Two shapes, both literal readings of the text rather than inference: a bare
/// id (`GR-19`), and a **parenthesised bare family** (`§2.1 (X-EH)`), which is
/// how A-1 cites all nine X-EH requirements at once. A family is expanded only
/// in that exact parenthesised shape — a loose `CI` in prose means continuous
/// integration, not the code-intelligence domain.
fn cited_ids(text: &str, known: &dyn Fn(&str) -> bool, families: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |id: String, out: &mut Vec<String>| {
        if !out.iter().any(|s| *s == id) {
            out.push(id);
        }
    };
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        // `§2.1 (X-EH)` — a whole family, cited at once.
        if let Some(fam) = word
            .strip_prefix('(')
            .and_then(|w| w.strip_suffix(')').or_else(|| w.strip_suffix("),")))
        {
            if let Some(f) = families.iter().find(|f| *f == fam) {
                for id in expand_family(f, known) {
                    push(id, &mut out);
                }
                i += 1;
                continue;
            }
        }
        let tok = bare(word);
        // `ST-36 … ST-40` — an inclusive range. Capturing only the endpoints
        // would drop ST-37..ST-39 while still reporting a citation, which is
        // the quiet-loss shape this file exists to refuse.
        if is_id(tok) && i + 2 < words.len() && matches!(words[i + 1], "…" | "...") {
            let end = bare(words[i + 2]);
            if is_id(end) {
                if let (Some((f1, n1)), Some((f2, n2))) =
                    (tok.rsplit_once('-'), end.rsplit_once('-'))
                {
                    if f1 == f2 {
                        if let (Ok(a), Ok(b)) = (n1.parse::<u32>(), n2.parse::<u32>()) {
                            if a <= b {
                                for n in a..=b {
                                    let id = format!("{f1}-{n}");
                                    if known(&id) {
                                        push(id, &mut out);
                                    }
                                }
                                i += 3;
                                continue;
                            }
                        }
                    }
                }
            }
        }
        if is_id(tok) && known(tok) {
            push(tok.to_string(), &mut out);
        }
        i += 1;
    }
    out
}

/// A word with its surrounding punctuation stripped, so `(GR-19),` reads as
/// `GR-19`.
fn bare(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
}

/// Every id in a family, in ordinal order. Used only for a parenthesised
/// family citation.
fn expand_family(family: &str, known: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    for n in 1.. {
        let id = format!("{family}-{n}");
        if !known(&id) {
            break;
        }
        out.push(id);
    }
    out
}

/// Parse the whole specification into a registry.
pub fn parse(spec: &str) -> RequirementRegistry {
    let lines: Vec<&str> = spec.split('\n').collect();
    let mut requirements: Vec<Requirement> = Vec::new();
    let mut scenarios: Vec<(AcceptanceScenario, String)> = Vec::new();
    let mut aliases: Vec<(String, String, u32)> = Vec::new();
    let (mut h2, mut h2_no, mut h3, mut h3_no) =
        (String::new(), String::new(), String::new(), String::new());

    for (i, line) in lines.iter().enumerate() {
        let line_no = i + 1;
        if let Some(rest) = line.strip_prefix("## ") {
            h2 = rest.trim().to_string();
            h2_no = leading_number(&h2);
            h3.clear();
            h3_no.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            h3 = rest.trim().to_string();
            h3_no = leading_number(&h3);
            continue;
        }
        if line.contains("are stated in §") {
            for (src, dst) in parse_alias_line(line, line_no) {
                aliases.push((src, dst, line_no as u32));
            }
            continue;
        }
        let Some((id, qual)) = declaration_head(line) else {
            continue;
        };
        let head_len = line.find(".**").expect("declaration_head found one") + 3;
        let text = collect_text(&lines, i, &line[head_len..]);
        let (family, n) = id.rsplit_once('-').expect("is_id checked");
        let n: u32 = n.parse().expect("is_id checked digits");

        if family == "A" {
            assert_eq!(
                h2_no, "16",
                "{SPEC}:{line_no}: acceptance scenario {id} outside §16"
            );
            scenarios.push((
                AcceptanceScenario {
                    id: id.to_string(),
                    suite: if h3.is_empty() { h2.clone() } else { h3.clone() },
                    line: line_no as u32,
                    text: text.clone(),
                    cites: Vec::new(),
                },
                text,
            ));
            continue;
        }

        let (level, conditional_on) = resolve_level(qual, &h2_no, &h3_no, id, line_no);
        requirements.push(Requirement {
            id: id.to_string(),
            family: family.to_string(),
            n,
            level,
            spec_line: line_no as u32,
            text,
            conditional_on,
            alias_of: None,
        });
    }

    // Aliases resolve against the requirements just parsed, so they are appended
    // after the sweep — and a dangling target is a panic, not a dropped entry.
    for (src, dst, spec_line) in aliases {
        let target = requirements
            .iter()
            .find(|r| r.id == dst)
            .unwrap_or_else(|| panic!("{SPEC}:{spec_line}: alias {src} names unknown {dst}"));
        let (family, n) = src.rsplit_once('-').expect("expand builds ids");
        requirements.push(Requirement {
            id: src.clone(),
            family: family.to_string(),
            n: n.parse().expect("expand builds numeric ordinals"),
            level: target.level,
            spec_line,
            text: format!("Stated as {dst}; applies here in full."),
            conditional_on: None,
            alias_of: Some(dst),
        });
    }

    requirements.sort_by(|a, b| a.family.cmp(&b.family).then(a.n.cmp(&b.n)));
    let known = |id: &str| requirements.iter().any(|r| r.id == id);
    let mut families: Vec<String> = requirements.iter().map(|r| r.family.clone()).collect();
    families.sort();
    families.dedup();
    let scenarios: Vec<AcceptanceScenario> = scenarios
        .into_iter()
        .map(|(mut s, text)| {
            s.cites = cited_ids(&text, &known, &families);
            s
        })
        .collect();

    // Every conditional antecedent must resolve, or the conditional cannot be
    // evaluated and would silently read as unconditional.
    for r in &requirements {
        if let Some(dep) = &r.conditional_on {
            assert!(
                requirements.iter().any(|o| &o.id == dep),
                "{SPEC}:{}: {} is conditional on unknown requirement {dep}",
                r.spec_line,
                r.id
            );
        }
    }

    RequirementRegistry {
        spec_hash: ContentHash::of_str(spec).to_hex(),
        spec_lines: lines.len() as u32,
        requirements,
        scenarios,
    }
}

/// `8. D6 — Grounding…` → `8`; `15.2 Scale targets` → `15.2`.
fn leading_number(heading: &str) -> String {
    heading
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string()
}

// ─── Parser unit tests: each form, and each refusal ─────────────────────────

/// The six declaration forms the specification actually uses. A seventh must
/// fail loudly, which the next test asserts.
#[test]
fn every_declaration_form_parses() {
    assert_eq!(declaration_head("**GR-19.** A thing."), Some(("GR-19", None)));
    assert_eq!(
        declaration_head("**X-EH-2 (MUST).** A check."),
        Some(("X-EH-2", Some("MUST")))
    );
    assert_eq!(
        declaration_head("**GR-2 (INVARIANT).** **Bold lead.** More."),
        Some(("GR-2", Some("INVARIANT")))
    );
    assert_eq!(
        declaration_head("**NF-9 (SHOULD).** Fast."),
        Some(("NF-9", Some("SHOULD")))
    );
    assert_eq!(
        declaration_head("**NF-1 (BAR).** Flat."),
        Some(("NF-1", Some("BAR")))
    );
    assert_eq!(
        declaration_head("**EN-19 (MUST, where EN-18 is implemented).** X."),
        Some(("EN-19", Some("MUST, where EN-18 is implemented")))
    );
}

/// Bold prose is not a requirement. Each of these appears verbatim in the
/// specification and each would have manufactured a phantom id.
#[test]
fn bold_prose_is_not_a_declaration() {
    for line in [
        "**Status:** clean-room specification, derived by reverse-engineering",
        "**In scope:** knowledge acquisition and indexing, retrieval, grounded",
        "**BAR:** total carried context must not grow with thread length.",
        "**Tier 0 — unretrofittable.** Choosing wrongly here means a rewrite.",
        "**ST-16 through ST-20** are stated in §2.2 (X-PR-1 … X-PR-5) and apply",
        "**Workshop** (making things), **Reflect** (personal/wellbeing lane), and",
    ] {
        assert_eq!(declaration_head(line), None, "parsed prose as a declaration: {line}");
    }
}

/// The refusal that matters most: a bare declaration in a section with no
/// declared default. Defaulting it to MUST is precisely how 53 requirements
/// went missing, so the parser must stop rather than guess.
#[test]
#[should_panic(expected = "no entry in SECTION_DEFAULT_LEVEL")]
fn a_bare_declaration_in_an_undeclared_section_panics() {
    resolve_level(None, "99", "99.1", "ZZ-1", 1);
}

/// A qualifier the parser has never seen is a new form, not a MUST.
#[test]
#[should_panic(expected = "unrecognised qualifier")]
fn an_unknown_qualifier_panics() {
    resolve_level(Some("RECOMMENDED"), "2", "2.1", "ZZ-1", 1);
}

/// The conditional form carries its antecedent instead of losing it, so an
/// unimplemented antecedent can resolve to could-not-judge rather than covered.
#[test]
fn a_conditional_must_keeps_its_antecedent() {
    let (level, dep) = resolve_level(
        Some("MUST, where EN-18 is implemented"),
        "5",
        "5.1",
        "EN-19",
        1,
    );
    assert_eq!(level, ReqLevel::Must);
    assert_eq!(dep.as_deref(), Some("EN-18"));
}

/// An alias expands to one entry per source id, paired in order.
#[test]
fn the_alias_sentence_expands_to_five_pairs() {
    let pairs = parse_alias_line(
        "**ST-16 through ST-20** are stated in §2.2 (X-PR-1 … X-PR-5) and apply here in",
        718,
    );
    assert_eq!(
        pairs,
        vec![
            ("ST-16".to_string(), "X-PR-1".to_string()),
            ("ST-17".to_string(), "X-PR-2".to_string()),
            ("ST-18".to_string(), "X-PR-3".to_string()),
            ("ST-19".to_string(), "X-PR-4".to_string()),
            ("ST-20".to_string(), "X-PR-5".to_string()),
        ]
    );
}

/// The defect this parser shipped with for one generation: many requirements
/// open their SECOND line with a bold phrase, and breaking on bare `**`
/// truncated them to their first line — quietly, with a plausible-looking
/// result. A-1 lost everything after "demonstrate a".
#[test]
fn text_survives_a_bold_continuation_line() {
    let lines = vec![
        "**A-1.** For every requirement in §2.1 (X-EH), the rebuild MUST demonstrate a",
        "**failing case before the guard and a passing case after** — the failure must",
        "have been watched to fail.",
        "",
        "**A-2.** Next one.",
    ];
    let text = collect_text(&lines, 0, " For every requirement in §2.1 (X-EH), the rebuild MUST demonstrate a");
    assert!(
        text.ends_with("the failure must have been watched to fail."),
        "truncated at the bold continuation: {text}"
    );
}

/// A scenario that cites a whole family cites every id in it — A-1's
/// "§2.1 (X-EH)" is nine requirements, not zero. A loose family token in prose
/// is NOT a citation.
#[test]
fn a_parenthesised_family_expands_and_a_loose_one_does_not() {
    let known = |id: &str| matches!(id, "X-EH-1" | "X-EH-2" | "X-EH-3" | "CI-1");
    let families = vec!["CI".to_string(), "X-EH".to_string()];
    assert_eq!(
        cited_ids("requirement in §2.1 (X-EH), the rebuild", &known, &families),
        vec!["X-EH-1", "X-EH-2", "X-EH-3"]
    );
    assert_eq!(
        cited_ids("the CI job must run it, see CI-1", &known, &families),
        vec!["CI-1"],
        "a loose family token is prose, not a citation"
    );
}

/// `ST-36 … ST-40` cites five requirements. Capturing only the endpoints
/// reports a citation while silently dropping three of them.
#[test]
fn an_ellipsis_range_expands_to_every_id_it_covers() {
    let known = |id: &str| {
        matches!(
            id,
            "ST-36" | "ST-37" | "ST-38" | "ST-39" | "ST-40" | "EV-16" | "EV-19"
        )
    };
    let fams = vec!["EV".to_string(), "ST".to_string()];
    assert_eq!(
        cited_ids("its reason (ST-36 … ST-40), and a turn", &known, &fams),
        vec!["ST-36", "ST-37", "ST-38", "ST-39", "ST-40"]
    );
    // A range whose interior ids do not exist contributes only what does.
    assert_eq!(
        cited_ids("scored on separate red lines (EV-16 … EV-19).", &known, &fams),
        vec!["EV-16", "EV-19"]
    );
}

/// The `⟨why⟩` block is rationale, not obligation — it must not land in the
/// requirement's text, or every conformance case would quote the incident
/// rather than the rule.
#[test]
fn text_stops_at_the_why_block() {
    let lines = vec![
        "**X-EH-1 (MUST).** Absence MUST be reported, never",
        "defaulted.",
        "⟨why⟩ A scoring term whose input was missing was silently treated as",
        "neutral.",
    ];
    let text = collect_text(&lines, 0, " Absence MUST be reported, never");
    assert_eq!(text, "Absence MUST be reported, never defaulted.");
}
