//! Structural extractor over a DESIGN.md document.
//!
//! Returns a [`DesignSignals`] snapshot — sections, an optional Anchors
//! block, gap markers (TBDs, empty stubs, open choices, literal
//! questions), and keyword presence flags for fault-line gating.
//!
//! **Strictly structural.** Nothing in this module interprets semantics.
//! "The section body is empty" is a structural claim; "this design needs
//! a persistence decision" is not. Semantic judgement is the agent's
//! job during the `sovereign project design` session; this extractor
//! only surfaces the evidence.
//!
//! ## Why pulldown-cmark
//!
//! Raw regex over markdown misfires on code fences, HTML comments, and
//! nested lists. `sovereign-atos`'s `charter.rs` already uses
//! pulldown-cmark for the same reason — see ARCH_PRINCIPLES.md §8.1 on
//! keeping one markdown parser across the workspace.
//!
//! ## Gap triggers (ref: plan step 1, signals 1–6)
//!
//! 1. Heading with empty / whitespace-only body           → `EmptySection`
//! 2. Heading with body == single `-` / `*`                → `EmptySection`
//! 3. Body contains `tbd`, `todo`, `???` (word-boundary)   → `TbdMarker`
//!    Body contains `unclear`, `open question`             → `UnclearMarker`
//! 4. Body contains `X vs Y` without `chose/picked/decided` → `OpenChoice`
//! 5. `Anchors` section with fewer than 3 non-empty bullets → `EmptySection`
//! 6. Body line ending in `?` (outside headings/code)       → `LiteralQuestion`
//!
//! Keyword buckets (presence flags): `time`, `persistence`, `api`,
//! `queue`, `concurrency`, `secrets`, `consumers`. Used by the solo
//! fallback in `found.rs` to gate fault-line firing — a `time-representation`
//! question fires only if the design text mentions time at all.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

// ─── Public types ──────────────────────────────────────────────────

/// Snapshot returned by [`extract`]. See module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignSignals {
    /// Ordered list of bullets under the `Anchors` section. Empty if
    /// the section is missing or contains only placeholder bullets.
    pub anchors: Vec<AnchorLine>,
    /// Load-bearing gaps — each one is a candidate OPEN_QUESTIONS.md
    /// entry. Ordered by document position.
    pub gaps: Vec<GapMarker>,
    /// Presence flags for fault-line gating (solo fallback only; the
    /// agent path uses its own judgement).
    pub keywords: KeywordBuckets,
    /// Every H1/H2/H3 section in document order. H4+ stay inside
    /// their parent section's body.
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorLine {
    /// Bullet text with the leading `- ` / `* ` stripped.
    pub text: String,
    /// 1-based line number in the source markdown.
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapMarker {
    /// Heading text of the section the gap lives in, empty if the gap
    /// precedes any heading.
    pub section: String,
    /// Short quote of the line (or a synthesized label for empty
    /// sections). Bounded to ~200 chars for display.
    pub snippet: String,
    pub reason: GapReason,
    /// 1-based line number of the triggering content (heading line for
    /// `EmptySection`, body line otherwise).
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GapReason {
    TbdMarker,
    EmptySection,
    UnclearMarker,
    OpenChoice,
    LiteralQuestion,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeywordBuckets {
    pub time: bool,
    pub persistence: bool,
    pub api: bool,
    pub queue: bool,
    pub concurrency: bool,
    pub secrets: bool,
    pub consumers: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Heading text, trimmed.
    pub heading: String,
    /// 1 for `#`, 2 for `##`, 3 for `###`.
    pub level: u8,
    /// Body between this heading and the next heading (any level),
    /// trimmed of leading/trailing whitespace.
    pub body: String,
    /// 1-based line number of the heading line itself.
    pub heading_line: usize,
}

// ─── Entry point ───────────────────────────────────────────────────

pub fn extract(markdown: &str) -> DesignSignals {
    let line_starts = build_line_starts(markdown);
    let sections = parse_sections(markdown, &line_starts);
    let anchors = extract_anchors(&sections, &line_starts);
    let gaps = detect_gaps(&sections, &line_starts);
    let keywords = detect_keywords(markdown);
    DesignSignals {
        anchors,
        gaps,
        keywords,
        sections,
    }
}

// ─── Section parsing ───────────────────────────────────────────────

fn parse_sections(md: &str, line_starts: &[usize]) -> Vec<Section> {
    // Pass 1: collect heading ranges in document order.
    let mut headings: Vec<HeadingInfo> = Vec::new();
    let mut pending: Option<PendingHeading> = None;

    for (event, range) in Parser::new(md).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let lv = match level {
                    HeadingLevel::H1 => 1u8,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    // H4+ collapse into their parent section's body.
                    _ => continue,
                };
                pending = Some(PendingHeading {
                    level: lv,
                    text: String::new(),
                    start: range.start,
                });
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(p) = pending.as_mut() {
                    p.text.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(p) = pending.take() {
                    headings.push(HeadingInfo {
                        level: p.level,
                        text: p.text.trim().to_string(),
                        start: p.start,
                        end: range.end,
                    });
                }
            }
            _ => {}
        }
    }

    // Pass 2: slice bodies between consecutive headings.
    let mut sections = Vec::with_capacity(headings.len());
    for (i, h) in headings.iter().enumerate() {
        let body_end = headings
            .get(i + 1)
            .map(|next| next.start)
            .unwrap_or(md.len());
        // Guard against empty or inverted ranges (shouldn't happen for
        // well-formed markdown but pulldown-cmark can report unusual
        // offsets for malformed input).
        let body = if h.end <= body_end && body_end <= md.len() {
            md[h.end..body_end].trim().to_string()
        } else {
            String::new()
        };
        sections.push(Section {
            heading: h.text.clone(),
            level: h.level,
            body,
            heading_line: line_for_offset(line_starts, h.start),
        });
    }
    sections
}

struct HeadingInfo {
    level: u8,
    text: String,
    start: usize,
    end: usize,
}

struct PendingHeading {
    level: u8,
    text: String,
    start: usize,
}

// ─── Anchors extraction ────────────────────────────────────────────

fn extract_anchors(sections: &[Section], _line_starts: &[usize]) -> Vec<AnchorLine> {
    let Some(section) = sections
        .iter()
        .find(|s| s.heading.eq_ignore_ascii_case("anchors"))
    else {
        return Vec::new();
    };
    let mut anchors = Vec::new();
    // Each non-empty `-` or `*` bullet is an anchor line. Bullets are
    // counted relative to the Anchors section heading — line = heading_line
    // + 1 (the conventional blank line) + line-offset-in-body.
    for (offset, raw) in section.body.lines().enumerate() {
        let trimmed = raw.trim_start();
        let stripped = if let Some(rest) = trimmed.strip_prefix("- ") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix("* ") {
            rest
        } else if trimmed == "-" || trimmed == "*" {
            // Empty placeholder bullet — skip, but don't break (more may follow).
            continue;
        } else {
            // Not a bullet at this indent level.
            continue;
        };
        let text = stripped.trim();
        if text.is_empty() {
            continue;
        }
        anchors.push(AnchorLine {
            text: text.to_string(),
            // Approximate line number: body begins roughly two lines
            // below the heading (one blank line). Good enough for a
            // display anchor; exact mapping would need line-precise
            // offsets from the parser.
            line: section.heading_line + 1 + offset,
        });
    }
    anchors
}

// ─── Gap detection ─────────────────────────────────────────────────

const SNIPPET_MAX: usize = 200;
const ANCHORS_MIN_BULLETS: usize = 3;

fn detect_gaps(sections: &[Section], _line_starts: &[usize]) -> Vec<GapMarker> {
    let mut gaps: Vec<GapMarker> = Vec::new();

    for section in sections {
        // H1 is conventionally the document title (`# <project> — Design`)
        // and its "body" is everything between the title and the first
        // H2 — typically empty. Flagging that as a gap generates spurious
        // EmptySection markers on every well-formed DESIGN.md. Content
        // sections start at H2, so gap detection does too.
        //
        // Note: we still *include* H1 in `sections` (callers may want to
        // read the title), we just don't produce gaps for it.
        if section.level == 1 {
            continue;
        }

        // Signal 1: empty body.
        let body_trim = section.body.trim();
        if body_trim.is_empty() {
            gaps.push(GapMarker {
                section: section.heading.clone(),
                snippet: String::new(),
                reason: GapReason::EmptySection,
                line: section.heading_line,
            });
            continue;
        }

        // Signal 2: body is a single empty-bullet placeholder.
        let non_empty: Vec<&str> = body_trim.lines().filter(|l| !l.trim().is_empty()).collect();
        if non_empty.len() == 1 {
            let only = non_empty[0].trim();
            if only == "-" || only == "*" {
                gaps.push(GapMarker {
                    section: section.heading.clone(),
                    snippet: only.to_string(),
                    reason: GapReason::EmptySection,
                    line: section.heading_line,
                });
                continue;
            }
        }

        // Signals 3, 4, 6 — scan body lines. Skip fenced code blocks
        // and HTML comments because they're commentary, not
        // load-bearing claims.
        let mut in_code = false;
        let mut in_html_comment = false;
        for (offset, raw) in section.body.lines().enumerate() {
            let line = raw.trim();
            if line.starts_with("```") {
                in_code = !in_code;
                continue;
            }
            if in_code {
                continue;
            }
            if line.starts_with("<!--") {
                in_html_comment = true;
            }
            if in_html_comment {
                if line.contains("-->") {
                    in_html_comment = false;
                }
                continue;
            }
            if line.is_empty() {
                continue;
            }

            let line_num = section.heading_line + 1 + offset;
            let lower = line.to_lowercase();

            // Signal 3a: TBD markers.
            if contains_word(&lower, "tbd") || contains_word(&lower, "todo") || line.contains("???")
            {
                gaps.push(GapMarker {
                    section: section.heading.clone(),
                    snippet: truncate(line),
                    reason: GapReason::TbdMarker,
                    line: line_num,
                });
                continue;
            }

            // Signal 3b: Unclear markers.
            if contains_word(&lower, "unclear") || lower.contains("open question") {
                gaps.push(GapMarker {
                    section: section.heading.clone(),
                    snippet: truncate(line),
                    reason: GapReason::UnclearMarker,
                    line: line_num,
                });
                continue;
            }

            // Signal 4: "X vs Y" without resolution on the same line.
            if has_unresolved_vs(&lower) {
                gaps.push(GapMarker {
                    section: section.heading.clone(),
                    snippet: truncate(line),
                    reason: GapReason::OpenChoice,
                    line: line_num,
                });
                continue;
            }

            // Signal 6: literal question. Skip lines that are under the
            // "Open questions" heading — those are legitimately the place
            // for open questions to live.
            if line.ends_with('?') && !section.heading.eq_ignore_ascii_case("open questions") {
                gaps.push(GapMarker {
                    section: section.heading.clone(),
                    snippet: truncate(line),
                    reason: GapReason::LiteralQuestion,
                    line: line_num,
                });
            }
        }
    }

    // Signal 5: Anchors under-specified (< 3 real bullets). Only fire
    // if we haven't already logged an EmptySection for Anchors.
    if let Some(anchors_section) = sections
        .iter()
        .find(|s| s.heading.eq_ignore_ascii_case("anchors"))
    {
        let already_flagged = gaps.iter().any(|g| {
            g.section.eq_ignore_ascii_case("anchors") && g.reason == GapReason::EmptySection
        });
        if !already_flagged {
            let bullet_count = count_real_bullets(&anchors_section.body);
            if bullet_count < ANCHORS_MIN_BULLETS {
                gaps.push(GapMarker {
                    section: anchors_section.heading.clone(),
                    snippet: format!("{bullet_count} anchor(s)"),
                    reason: GapReason::EmptySection,
                    line: anchors_section.heading_line,
                });
            }
        }
    }

    gaps
}

fn count_real_bullets(body: &str) -> usize {
    body.lines()
        .filter_map(|l| {
            let t = l.trim_start();
            let stripped = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))?;
            let content = stripped.trim();
            if content.is_empty() {
                None
            } else {
                Some(())
            }
        })
        .count()
}

fn has_unresolved_vs(lower_line: &str) -> bool {
    // Require `vs` or `vs.` surrounded by word-boundary characters
    // (spaces are the common case). "verse" must not match.
    let hit = find_word(lower_line, "vs").is_some() || find_word(lower_line, "vs.").is_some();
    if !hit {
        return false;
    }
    // Resolution signals on the same line.
    let resolution = [
        "chose",
        "picked",
        "decided",
        "we use",
        "we picked",
        "we chose",
    ];
    !resolution.iter().any(|kw| lower_line.contains(kw))
}

fn truncate(s: &str) -> String {
    if s.len() <= SNIPPET_MAX {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(SNIPPET_MAX - 1).collect();
        out.push('…');
        out
    }
}

// ─── Keyword detection ─────────────────────────────────────────────

fn detect_keywords(md: &str) -> KeywordBuckets {
    let stripped = strip_code_and_comments(md).to_lowercase();
    KeywordBuckets {
        time: any_word(
            &stripped,
            &[
                "time",
                "timestamp",
                "timestamps",
                "utc",
                "timezone",
                "schedule",
            ],
        ),
        persistence: any_word(
            &stripped,
            &[
                "persist",
                "persistence",
                "database",
                "sqlite",
                "postgres",
                "postgresql",
                "sql",
                "storage",
                "disk",
            ],
        ),
        api: any_word(
            &stripped,
            &["api", "http", "endpoint", "rest", "graphql", "grpc"],
        ),
        queue: any_word(
            &stripped,
            &["queue", "kafka", "redis", "pubsub", "mq", "nats"],
        ),
        concurrency: any_word(
            &stripped,
            &[
                "async",
                "thread",
                "actor",
                "goroutine",
                "tokio",
                "mutex",
                "channel",
            ],
        ),
        secrets: any_word(
            &stripped,
            &["secret", "vault", "kms", "credential", "credentials"],
        ),
        consumers: any_word(&stripped, &["consumer", "downstream", "caller", "client"]),
    }
}

/// Strip fenced code blocks (```...```) and HTML comments (<!-- ... -->)
/// so keyword scanning doesn't trip on commentary. Cheap state machine;
/// doesn't try to handle nested fences (they're illegal in CommonMark).
fn strip_code_and_comments(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut in_code = false;
    let mut i = 0;
    let bytes = md.as_bytes();
    while i < bytes.len() {
        // HTML comment.
        if !in_code && bytes[i..].starts_with(b"<!--") {
            if let Some(end) = md[i..].find("-->") {
                i += end + 3;
                continue;
            } else {
                break; // unterminated comment — drop the rest
            }
        }
        // Start / end of fence, aligned to newline or start-of-doc.
        let at_line_start = i == 0 || bytes[i - 1] == b'\n';
        if at_line_start && bytes[i..].starts_with(b"```") {
            in_code = !in_code;
            // Skip to end of the fence line.
            if let Some(nl) = md[i..].find('\n') {
                i += nl + 1;
            } else {
                break;
            }
            continue;
        }
        if in_code {
            // Skip whole line.
            if let Some(nl) = md[i..].find('\n') {
                i += nl + 1;
            } else {
                break;
            }
            continue;
        }
        // Copy byte.
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn any_word(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| contains_word(haystack, n))
}

/// Case-sensitive word-boundary containment. ASCII word chars are
/// alphanumeric + underscore. `needle` must be ASCII; every keyword
/// checked in this module is.
fn contains_word(haystack: &str, needle: &str) -> bool {
    find_word(haystack, needle).is_some()
}

fn find_word(haystack: &str, needle: &str) -> Option<usize> {
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.is_empty() || nb.len() > hb.len() {
        return None;
    }
    let mut i = 0;
    while i + nb.len() <= hb.len() {
        if hb[i..i + nb.len()] == *nb {
            let before_ok = i == 0 || !is_word_char(hb[i - 1]);
            let after_ok = i + nb.len() == hb.len() || !is_word_char(hb[i + nb.len()]);
            if before_ok && after_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// ─── Line-offset helpers ───────────────────────────────────────────

fn build_line_starts(s: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in s.as_bytes().iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// 1-based line number for a byte offset. Consistent with text editors.
fn line_for_offset(starts: &[usize], offset: usize) -> usize {
    match starts.binary_search(&offset) {
        Ok(idx) => idx + 1,
        // `idx` is where `offset` would be inserted — that's the
        // (0-based) index of the line that contains the offset.
        Err(idx) => idx.max(1),
    }
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn first_gap(signals: &DesignSignals, reason: GapReason) -> Option<&GapMarker> {
        signals.gaps.iter().find(|g| g.reason == reason)
    }

    #[test]
    fn empty_doc_yields_nothing() {
        let s = extract("");
        assert!(s.sections.is_empty());
        assert!(s.anchors.is_empty());
        assert!(s.gaps.is_empty());
        assert_eq!(s.keywords, KeywordBuckets::default());
    }

    #[test]
    fn parses_sections_in_document_order() {
        let md = "# Title\n\n## A\n\nalpha\n\n## B\n\nbeta\n";
        let s = extract(md);
        let headings: Vec<&str> = s.sections.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(headings, vec!["Title", "A", "B"]);
        assert_eq!(s.sections[1].body, "alpha");
        assert_eq!(s.sections[2].body, "beta");
    }

    #[test]
    fn empty_section_gap_fires() {
        let md = "# P\n\n## Data & interfaces\n\n## Next\n\nhere\n";
        let s = extract(md);
        let g = first_gap(&s, GapReason::EmptySection).expect("empty section gap");
        assert_eq!(g.section, "Data & interfaces");
    }

    #[test]
    fn h1_title_is_not_flagged_empty() {
        // The project-name H1 (`# X — Design`) conventionally has no body.
        // Flagging it as a gap would emit noise on every DESIGN.md.
        let md = "# MyProj — Design\n\n## Content\n\nreal body\n";
        let s = extract(md);
        assert!(
            s.gaps.is_empty(),
            "H1 title with following H2 body shouldn't trip any gap: {:?}",
            s.gaps
        );
    }

    #[test]
    fn placeholder_bullet_empty_section() {
        let md = "# P\n\n## Stuff\n\n-\n";
        let s = extract(md);
        let g = first_gap(&s, GapReason::EmptySection).expect("placeholder bullet");
        assert_eq!(g.section, "Stuff");
    }

    #[test]
    fn tbd_marker_fires() {
        let md = "# P\n\n## Plan\n\nTBD: wire format\n";
        let s = extract(md);
        let g = first_gap(&s, GapReason::TbdMarker).expect("tbd gap");
        assert_eq!(g.section, "Plan");
        assert!(g.snippet.to_lowercase().contains("wire format"));
    }

    #[test]
    fn todo_word_boundary() {
        // "todolist" should NOT fire; "TODO:" should.
        let no_match = extract("# P\n\n## Plan\n\na todolist here\n");
        assert!(first_gap(&no_match, GapReason::TbdMarker).is_none());
        let hit = extract("# P\n\n## Plan\n\nTODO: finish this\n");
        assert!(first_gap(&hit, GapReason::TbdMarker).is_some());
    }

    #[test]
    fn triple_question_marker() {
        let s = extract("# P\n\n## Plan\n\nrate limit ???\n");
        assert!(first_gap(&s, GapReason::TbdMarker).is_some());
    }

    #[test]
    fn unclear_marker_fires() {
        let s = extract("# P\n\n## Plan\n\nthe boundary is unclear here.\n");
        assert!(first_gap(&s, GapReason::UnclearMarker).is_some());
    }

    #[test]
    fn open_choice_fires_without_resolution() {
        let s = extract("# P\n\n## Schema\n\nUUID vs ULID for the id?\n");
        // Note: line ends in `?` so LiteralQuestion ALSO fires. We only
        // require OpenChoice is present (snippet comes from same line).
        assert!(first_gap(&s, GapReason::OpenChoice).is_some());
    }

    #[test]
    fn open_choice_suppressed_when_resolved() {
        let s = extract("# P\n\n## Schema\n\nUUID vs ULID — we chose ULID.\n");
        assert!(first_gap(&s, GapReason::OpenChoice).is_none());
    }

    #[test]
    fn verse_does_not_match_vs_keyword() {
        let s = extract("# P\n\n## Prose\n\nThe verse is long.\n");
        assert!(first_gap(&s, GapReason::OpenChoice).is_none());
    }

    #[test]
    fn literal_question_fires_outside_open_questions() {
        let s = extract("# P\n\n## Plan\n\nWhat is the retry cadence?\n");
        assert!(first_gap(&s, GapReason::LiteralQuestion).is_some());
    }

    #[test]
    fn literal_question_suppressed_inside_open_questions_section() {
        let s = extract("# P\n\n## Open questions\n\n- What is the retry cadence?\n");
        // The Open questions section legitimately holds questions; don't
        // re-flag each bullet as a separate literal-question gap.
        assert!(first_gap(&s, GapReason::LiteralQuestion).is_none());
    }

    #[test]
    fn code_fence_content_is_ignored_for_markers() {
        let md = "# P\n\n## Plan\n\n```\nTBD: this is a code sample\n```\n\nprose here.\n";
        let s = extract(md);
        // Body's TBD is inside a fence — must not fire.
        assert!(first_gap(&s, GapReason::TbdMarker).is_none());
    }

    #[test]
    fn anchors_with_three_bullets_is_fine() {
        let md = "# P\n\n## Anchors\n\n- Primary persistence: sqlite\n- Primary interface: HTTP\n- Language: Rust\n\n## Next\n\nfoo\n";
        let s = extract(md);
        assert_eq!(s.anchors.len(), 3);
        let empty_for_anchors = s.gaps.iter().find(|g| {
            g.section.eq_ignore_ascii_case("anchors") && g.reason == GapReason::EmptySection
        });
        assert!(empty_for_anchors.is_none(), "anchors should not be flagged");
    }

    #[test]
    fn anchors_with_one_bullet_is_under_specified() {
        let md = "# P\n\n## Anchors\n\n- Primary persistence: sqlite\n\n## Next\n\nfoo\n";
        let s = extract(md);
        assert_eq!(s.anchors.len(), 1);
        let anchors_gap = s
            .gaps
            .iter()
            .find(|g| g.section.eq_ignore_ascii_case("anchors"));
        assert!(anchors_gap.is_some(), "anchors under-specified gap missing");
    }

    #[test]
    fn anchors_empty_bullets_are_skipped() {
        let md = "# P\n\n## Anchors\n\n-\n-\n-\n";
        let s = extract(md);
        assert!(s.anchors.is_empty());
        // Treated as EmptySection (single-bullet placeholder on a single
        // non-empty line is the specific case; three empty bullets is
        // caught by the `< 3 real bullets` rule).
        let flagged = s
            .gaps
            .iter()
            .any(|g| g.section.eq_ignore_ascii_case("anchors"));
        assert!(flagged);
    }

    #[test]
    fn keyword_time_fires_on_timestamp() {
        let s = extract("# P\n\n## Plan\n\nTick timestamps are stored in UTC.\n");
        assert!(s.keywords.time);
    }

    #[test]
    fn keyword_time_does_not_fire_for_unrelated_doc() {
        let s = extract("# CLI tool\n\n## What we're building\n\nA simple command-line utility for parsing CSV.\n");
        assert!(!s.keywords.time, "CLI doc should not flag time keyword");
    }

    #[test]
    fn keyword_scan_skips_code_fences() {
        // Put "timestamp" only inside a code fence — should NOT fire.
        let md =
            "# P\n\n## Plan\n\n```rust\nlet timestamp = 0;\n```\n\nNo temporal concerns here.\n";
        let s = extract(md);
        assert!(!s.keywords.time);
    }

    #[test]
    fn keyword_scan_skips_html_comments() {
        let md = "# P\n\n## Plan\n\n<!-- timestamp, persist, secret — all commentary -->\n\nBasic math library.\n";
        let s = extract(md);
        assert!(!s.keywords.time);
        assert!(!s.keywords.persistence);
        assert!(!s.keywords.secrets);
    }

    #[test]
    fn keyword_persistence_detects_sqlite_and_postgres() {
        let sqlite = extract("# P\n\n## Data\n\nWe use SQLite for storage.\n");
        assert!(sqlite.keywords.persistence);
        let pg = extract("# P\n\n## Data\n\nPostgres handles the persistence layer.\n");
        assert!(pg.keywords.persistence);
    }

    #[test]
    fn extracts_anchors_with_dash_and_star_bullets() {
        let md = "## Anchors\n\n- first\n* second\n- third\n";
        let s = extract(md);
        let texts: Vec<&str> = s.anchors.iter().map(|a| a.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn heading_line_numbers_are_one_based() {
        let md = "# First\n\n## Second\n\nbody\n";
        let s = extract(md);
        assert_eq!(s.sections[0].heading_line, 1);
        assert_eq!(s.sections[1].heading_line, 3);
    }

    #[test]
    fn sections_preserve_free_form_body() {
        let md = "# P\n\n## What we're building\n\nA distributed cache with the following properties.\n\n- LRU eviction\n- 10GB max\n";
        let s = extract(md);
        let body = &s.sections[1].body;
        assert!(body.contains("distributed cache"));
        assert!(body.contains("LRU"));
        assert!(body.contains("10GB"));
    }

    /// Integration-ish: a complete DESIGN.md template (the "Anchors
    /// + free-form body" pattern from the plan) should yield:
    ///   - 0 anchors (template bullet is empty `-`)
    ///   - multiple EmptySection gaps (body sections are empty)
    ///   - the Anchors under-specified gap.
    ///   - no spurious keyword hits (all real text is inside
    ///     HTML comments in the template).
    #[test]
    fn skeleton_template_is_mostly_gaps() {
        let md = r#"# project — Design

<!-- Replace or delete every line of this file. -->

## Anchors

<!-- 3-7 lines, each a stable fact. Examples... -->

-

## What we're building


## Data & interfaces


## Open questions

-
"#;
        let s = extract(md);
        assert!(s.anchors.is_empty(), "empty template anchors");
        // EmptySection for Anchors (< 3), What we're building, Data & interfaces.
        let empty_count = s
            .gaps
            .iter()
            .filter(|g| g.reason == GapReason::EmptySection)
            .count();
        assert!(
            empty_count >= 3,
            "expected >=3 empty-section gaps, got {empty_count}: {:?}",
            s.gaps
        );
        // HTML comments mentioning "timestamps" etc. must NOT inflate
        // keyword buckets (none of the comments mention those anyway).
        assert!(!s.keywords.time);
        assert!(!s.keywords.persistence);
    }
}
