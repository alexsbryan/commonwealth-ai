// SPDX-License-Identifier: AGPL-3.0-or-later
//! Charter parser — turns a markdown spec into a structured set of
//! milestones.
//!
//! Expected shape:
//!
//! ```markdown
//! # zotero-acquirer — Specification
//!
//! Some preamble about the feature. Anything above `## Milestones`
//! is kept verbatim as `CharterParse.preamble_md` and gets stored
//! on `features.charter_md`.
//!
//! ## Milestones
//!
//! ### 1. ZoteroAcquirer skeleton
//!
//! Add ZoteroLibrary variant to LocalCorpusSourceType.
//!
//! **Stop condition:** `cargo test -p corpus-engine acquirers::zotero`
//!
//! ### 2. RDF parser integration
//!
//! Wire the parser in.
//!
//! **Stop condition:** `cargo test -p corpus-engine extractors::zotero_rdf`
//! ```
//!
//! What the parser is strict about:
//! - A `## Milestones` heading (case-insensitive, level 2) must exist.
//! - Each milestone uses a level-3 heading (`###`). The ordinal is
//!   extracted from a leading `N.` prefix when present, otherwise it's
//!   auto-assigned by document order (1, 2, 3, ...).
//! - Every milestone MUST contain a paragraph starting with
//!   `**Stop condition:**` (or `**stop condition:**`, case-insensitive)
//!   followed by the command. Empty-body stop conditions are allowed
//!   (the feature has a manual review step) but the marker must still
//!   be present — explicit beats implicit.
//!
//! What the parser tolerates:
//! - Any markdown inside the body (lists, code blocks, nested
//!   blockquotes). We capture bytes, not a rendered DOM.
//! - Out-of-order ordinals (`### 3.`, then `### 1.`, then `### 2.`) —
//!   we preserve document order; the ordinals from the headings are
//!   reported in a warning but not re-sequenced. Authors pick their
//!   own numbering.
//! - Extra headings beyond level 3 inside a milestone. They stay in
//!   `brief_md`.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::{Error, Result};

/// Result of parsing a charter document.
#[derive(Debug, Clone)]
pub struct CharterParse {
    /// Everything before the `## Milestones` heading. Stored as
    /// `features.charter_md`; read back verbatim by the Marcus-at-PR
    /// view.
    pub preamble_md: String,
    /// Milestones in document order.
    pub milestones: Vec<MilestoneSpec>,
    /// Author opted into automatic red-team after the last milestone.
    /// Triggered by a line in the preamble matching
    /// `**Red team:** auto` (case-insensitive; also `true`, `yes`, `on`;
    /// also accepts the phrasing `**Auto red-team:** true`). Absent
    /// or any unrecognized value → `false`.
    pub auto_redteam: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MilestoneSpec {
    pub ordinal: i64,
    pub title: String,
    pub brief_md: String,
    /// Shell command the `end-milestone` runner executes. May be empty
    /// for manual-review milestones — the marker must still be
    /// present in the source for the parser to accept the milestone.
    pub stop_condition: String,
}

pub fn parse(md: &str) -> Result<CharterParse> {
    let milestones_heading = find_milestones_heading(md)?;
    let (preamble, body) = md.split_at(milestones_heading);
    // Body still starts with the `## Milestones` heading line; skip
    // past that line to find the first sub-heading.
    let body_after_heading = skip_first_line(body);

    let milestones = parse_milestones(body_after_heading)?;
    let preamble_md = preamble.trim_end_matches('\n').to_string();
    let auto_redteam = detect_auto_redteam(&preamble_md);
    Ok(CharterParse {
        preamble_md,
        milestones,
        auto_redteam,
    })
}

/// Scan the preamble for an `**Red team:** auto` (or equivalent)
/// opt-in. Case-insensitive. Accepted phrasings:
///
/// - `**Red team:** auto`
/// - `**Red-team:** auto`
/// - `**Auto red-team:** true`
/// - (values: `auto`, `true`, `yes`, `on`; anything else → off)
///
/// Returns `false` when no line matches so legacy charters opt out
/// by default.
fn detect_auto_redteam(preamble: &str) -> bool {
    for line in preamble.lines() {
        let lower = line.to_lowercase();
        let stripped = lower.trim();
        let Some(rest) = split_on_bold_label(stripped) else {
            continue;
        };
        let (label, value) = rest;
        let label_ok = matches!(
            label.trim(),
            "red team"
                | "red-team"
                | "redteam"
                | "auto red-team"
                | "auto red team"
                | "auto-red-team"
        );
        if !label_ok {
            continue;
        }
        let v = value.trim();
        if matches!(v, "auto" | "true" | "yes" | "on") {
            return true;
        }
    }
    false
}

/// Extract `(label, value)` from a line shaped like
/// `**label:** value` (bold + colon). Returns `None` when the
/// markdown bolding is absent.
fn split_on_bold_label(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("**")?;
    let close = rest.find("**")?;
    let inner = &rest[..close];
    let label = inner.strip_suffix(':').unwrap_or(inner);
    let after = &rest[close + 2..];
    Some((label.to_string(), after.to_string()))
}

/// Locate the byte offset of the `## Milestones` heading. The parser
/// scans the markdown event stream because raw-string search would
/// misfire on `##Milestones` inside a code block.
fn find_milestones_heading(md: &str) -> Result<usize> {
    let parser = Parser::new(md).into_offset_iter();
    let mut current_heading_start: Option<(HeadingLevel, usize)> = None;
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current_heading_start = Some((level, range.start));
            }
            Event::End(TagEnd::Heading(_)) => {
                current_heading_start = None;
            }
            Event::Text(text) => {
                if let Some((level, start)) = current_heading_start {
                    if level == HeadingLevel::H2 && text.trim().eq_ignore_ascii_case("Milestones") {
                        return Ok(start);
                    }
                }
            }
            _ => {}
        }
    }
    Err(Error::CharterParse(
        "charter must contain a `## Milestones` section".into(),
    ))
}

fn skip_first_line(s: &str) -> &str {
    match s.find('\n') {
        Some(i) => &s[i + 1..],
        None => "",
    }
}

/// Walk the body after `## Milestones` and carve it into
/// `MilestoneSpec` entries keyed off `###` headings. We slice the
/// source by byte offsets rather than reconstructing markdown from
/// events — authors' formatting survives round-trip unchanged.
fn parse_milestones(body: &str) -> Result<Vec<MilestoneSpec>> {
    // Collect `###` heading positions + title text.
    let parser = Parser::new(body).into_offset_iter();
    let mut headings: Vec<(usize, usize, String)> = Vec::new(); // (start, end_of_heading, title)
    let mut cur_start: Option<(HeadingLevel, usize)> = None;
    let mut cur_title_buf = String::new();
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                cur_start = Some((level, range.start));
                cur_title_buf.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start)) = cur_start.take() {
                    if level == HeadingLevel::H3 {
                        headings.push((start, range.end, cur_title_buf.trim().to_string()));
                    }
                }
            }
            Event::Text(text) => {
                if cur_start.is_some() {
                    cur_title_buf.push_str(&text);
                }
            }
            Event::Code(code) => {
                if cur_start.is_some() {
                    cur_title_buf.push_str(&code);
                }
            }
            _ => {}
        }
    }

    if headings.is_empty() {
        return Err(Error::CharterParse(
            "charter `## Milestones` section has no `###` milestones".into(),
        ));
    }

    // Slice the body into per-milestone spans. The brief for milestone
    // N runs from the end of its heading to the start of milestone N+1
    // (or the end of body for the last one).
    let mut milestones = Vec::with_capacity(headings.len());
    for (idx, (_start, end_of_heading, title)) in headings.iter().enumerate() {
        let next_start = headings
            .get(idx + 1)
            .map(|(s, _, _)| *s)
            .unwrap_or(body.len());
        let brief_raw = &body[*end_of_heading..next_start];
        let brief_md = brief_raw.trim().to_string();

        let (ordinal, display_title) = split_ordinal_from_title(title, idx as i64 + 1);
        let stop_condition = find_stop_condition(&brief_md).ok_or_else(|| {
            Error::CharterParse(format!(
                "milestone {ordinal} ({display_title}) has no `**Stop condition:**` marker"
            ))
        })?;

        milestones.push(MilestoneSpec {
            ordinal,
            title: display_title,
            brief_md,
            stop_condition,
        });
    }

    Ok(milestones)
}

/// Extract a leading ordinal from a heading like `1. ZoteroAcquirer
/// skeleton`. Falls back to the document-order ordinal when the
/// heading has no numeric prefix.
fn split_ordinal_from_title(raw: &str, fallback: i64) -> (i64, String) {
    let trimmed = raw.trim();
    // Accept `1.`, `01.`, `1)`, `1 — `, etc. The heuristic is
    // deliberately forgiving — this is markdown authors write.
    let mut chars = trimmed.char_indices();
    let mut end_of_digits = 0;
    for (i, c) in chars.by_ref() {
        if c.is_ascii_digit() {
            end_of_digits = i + c.len_utf8();
        } else {
            break;
        }
    }
    if end_of_digits == 0 {
        return (fallback, trimmed.to_string());
    }
    let digits = &trimmed[..end_of_digits];
    let Ok(n) = digits.parse::<i64>() else {
        return (fallback, trimmed.to_string());
    };
    let rest = trimmed[end_of_digits..]
        .trim_start_matches(['.', ')', ':', ' ', '\t', '—', '-'])
        .trim();
    if rest.is_empty() {
        // `### 1.` with nothing else — keep the number as the title so
        // the operator sees something meaningful in status output.
        return (n, digits.to_string());
    }
    (n, rest.to_string())
}

/// Find the `**Stop condition:**` line inside a milestone body. Both
/// `**Stop condition:**` and `**stop condition:**` match — Yara's
/// story uses the title-cased form, but authors should not be
/// punished for lowercase.
///
/// Public because the ATOS Runner ([`sovereign-cli atos run`]) gates
/// the reviewer pass on this command's exit-zero per
/// `sovereign/docs/ATOS_RUNNER.md` § Stop conditions.
pub fn find_stop_condition(brief: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "**Stop condition:**",
        "**stop condition:**",
        "**STOP CONDITION:**",
    ];
    for line in brief.lines() {
        let trimmed = line.trim_start();
        for marker in MARKERS {
            if trimmed.to_lowercase().starts_with(&marker.to_lowercase()) {
                let after = &trimmed[marker.len()..];
                // Strip one backtick pair if the author wrapped the
                // command — it's almost always in a monospace code span.
                let after = after.trim();
                let cmd = if let Some(stripped) = after
                    .strip_prefix('`')
                    .and_then(|rest| rest.strip_suffix('`'))
                {
                    stripped.to_string()
                } else {
                    after.to_string()
                };
                return Some(cmd.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const HAPPY_CHARTER: &str = "# zotero-acquirer — Specification

Preamble describing motivation. Kept verbatim in the preamble field.

## Milestones

### 1. ZoteroAcquirer skeleton

Add ZoteroLibrary variant to LocalCorpusSourceType. Keep it minimal —
just the struct + factory.

**Stop condition:** `cargo test -p corpus-engine acquirers::zotero`

### 2. RDF parser integration

Wire the parser in.

**Stop condition:** `cargo test -p corpus-engine extractors::zotero_rdf`
";

    #[test]
    fn happy_path_two_milestones() {
        let parsed = parse(HAPPY_CHARTER).unwrap();
        assert!(parsed
            .preamble_md
            .contains("Preamble describing motivation"));
        assert!(!parsed.preamble_md.contains("## Milestones"));
        assert_eq!(parsed.milestones.len(), 2);

        let m1 = &parsed.milestones[0];
        assert_eq!(m1.ordinal, 1);
        assert_eq!(m1.title, "ZoteroAcquirer skeleton");
        assert!(m1.brief_md.contains("Add ZoteroLibrary variant"));
        assert_eq!(
            m1.stop_condition,
            "cargo test -p corpus-engine acquirers::zotero"
        );

        let m2 = &parsed.milestones[1];
        assert_eq!(m2.ordinal, 2);
        assert_eq!(m2.title, "RDF parser integration");
        assert_eq!(
            m2.stop_condition,
            "cargo test -p corpus-engine extractors::zotero_rdf"
        );
    }

    #[test]
    fn case_insensitive_stop_marker() {
        let md = "## Milestones\n\n### 1. t\n\n**stop condition:** `echo ok`\n";
        let parsed = parse(md).unwrap();
        assert_eq!(parsed.milestones[0].stop_condition, "echo ok");
    }

    #[test]
    fn missing_stop_condition_errors() {
        let md = "## Milestones\n\n### 1. t\n\nBody but no stop marker.\n";
        let err = parse(md).unwrap_err();
        assert!(matches!(err, Error::CharterParse(_)));
        let msg = format!("{err}");
        assert!(msg.contains("Stop condition"), "got: {msg}");
    }

    #[test]
    fn missing_milestones_section_errors() {
        let md = "# title\n\nNo milestones section here.\n";
        let err = parse(md).unwrap_err();
        assert!(matches!(err, Error::CharterParse(_)));
    }

    #[test]
    fn empty_milestones_section_errors() {
        let md = "## Milestones\n\nBody but no third-level headings.\n";
        let err = parse(md).unwrap_err();
        assert!(matches!(err, Error::CharterParse(_)));
    }

    #[test]
    fn headings_without_ordinal_get_document_order() {
        let md = "## Milestones\n\n### Skeleton\n\n**Stop condition:** `a`\n\n### Plumbing\n\n**Stop condition:** `b`\n";
        let parsed = parse(md).unwrap();
        assert_eq!(parsed.milestones[0].ordinal, 1);
        assert_eq!(parsed.milestones[0].title, "Skeleton");
        assert_eq!(parsed.milestones[1].ordinal, 2);
        assert_eq!(parsed.milestones[1].title, "Plumbing");
    }

    #[test]
    fn out_of_order_ordinals_preserved_not_resequenced() {
        // Authors occasionally write 3, 1, 2 during drafting. We don't
        // rewrite it; the numbers they typed win.
        let md = "## Milestones\n\n### 3. Late\n\n**Stop condition:** `c`\n\n### 1. First\n\n**Stop condition:** `a`\n\n### 2. Middle\n\n**Stop condition:** `b`\n";
        let parsed = parse(md).unwrap();
        assert_eq!(parsed.milestones[0].ordinal, 3);
        assert_eq!(parsed.milestones[1].ordinal, 1);
        assert_eq!(parsed.milestones[2].ordinal, 2);
    }

    #[test]
    fn empty_stop_condition_body_accepted() {
        // Manual-review milestone — marker present, body empty.
        let md =
            "## Milestones\n\n### 1. Manual check\n\nPlease eyeball it.\n\n**Stop condition:**\n";
        let parsed = parse(md).unwrap();
        assert_eq!(parsed.milestones[0].stop_condition, "");
    }

    #[test]
    fn stop_condition_may_span_no_backticks() {
        let md = "## Milestones\n\n### 1. t\n\n**Stop condition:** cargo test\n";
        let parsed = parse(md).unwrap();
        assert_eq!(parsed.milestones[0].stop_condition, "cargo test");
    }

    // ── auto-redteam opt-in (M5.7) ───────────────────────────────

    #[test]
    fn auto_redteam_absent_defaults_to_false() {
        let parsed = parse(HAPPY_CHARTER).unwrap();
        assert!(!parsed.auto_redteam);
    }

    #[test]
    fn auto_redteam_opt_in_auto_value_recognized() {
        let md = "# T\n\n**Red team:** auto\n\n## Milestones\n\n### 1. m\n\n**Stop condition:** `true`\n";
        let parsed = parse(md).unwrap();
        assert!(parsed.auto_redteam);
    }

    #[test]
    fn auto_redteam_accepts_true_yes_on() {
        for value in ["true", "yes", "on", "AUTO", "True"] {
            let md = format!(
                "# T\n\n**Red team:** {value}\n\n## Milestones\n\n### 1. m\n\n**Stop condition:** `true`\n"
            );
            let parsed = parse(&md).unwrap();
            assert!(parsed.auto_redteam, "value `{value}` should opt in");
        }
    }

    #[test]
    fn auto_redteam_accepts_alt_phrasings() {
        let variants = [
            "**Red-team:** auto",
            "**Auto red-team:** true",
            "**Auto Red Team:** yes",
        ];
        for line in variants {
            let md = format!(
                "# T\n\n{line}\n\n## Milestones\n\n### 1. m\n\n**Stop condition:** `true`\n"
            );
            let parsed = parse(&md).unwrap();
            assert!(parsed.auto_redteam, "phrasing `{line}` should opt in");
        }
    }

    #[test]
    fn auto_redteam_off_and_unknown_values_stay_false() {
        for value in ["off", "false", "no", "maybe", ""] {
            let md = format!(
                "# T\n\n**Red team:** {value}\n\n## Milestones\n\n### 1. m\n\n**Stop condition:** `true`\n"
            );
            let parsed = parse(&md).unwrap();
            assert!(!parsed.auto_redteam, "value `{value}` should NOT opt in");
        }
    }

    #[test]
    fn auto_redteam_unrelated_bold_lines_ignored() {
        let md = "# T\n\n**Note:** nothing to do with red-team\n\n## Milestones\n\n### 1. m\n\n**Stop condition:** `true`\n";
        let parsed = parse(md).unwrap();
        assert!(!parsed.auto_redteam);
    }
}
