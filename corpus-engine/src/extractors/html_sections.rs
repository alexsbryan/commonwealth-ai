// SPDX-License-Identifier: AGPL-3.0-or-later
//! Section-aware HTML extractor.
//!
//! Reads a directory of HTML files, strips tags via the same helper
//! the plain `Html` extractor uses, then for each declared
//! [`SectionRule`] tries to match the section's `start_pattern` and
//! `end_pattern` regexes against the stripped text. Each successful
//! match becomes one [`ExtractedDoc`] with `metadata.section_name =
//! "<rule.name>"` so downstream code can group / filter.
//!
//! When *no* section matches a file, the optional [`FallbackRule`]
//! decides what to do (emit the full document, emit the first N
//! characters, or drop).
//!
//! Misses are recorded in a sidecar
//! `<source-dir>/_section_misses.json` so `sovereign recipe test`
//! can show the recipe author "section X missed in file Y; nearby
//! text: …; suggestion: …" without re-running the regex on every
//! file.

use std::fs;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::recipe::{FallbackRule, SectionRule};

use super::html::collect_html_files;
use super::{slug, strip_html, ExtractedDoc, Extractor};

/// Section-aware HTML extractor. See module docs.
#[derive(Debug)]
pub struct HtmlSectionsExtractor {
    rules: Vec<CompiledRule>,
    fallback: Option<FallbackRule>,
}

#[derive(Debug)]
struct CompiledRule {
    name: String,
    description: String,
    start: Regex,
    end: Regex,
    repeating: bool,
}

impl HtmlSectionsExtractor {
    /// Construct from a list of section rules. Compiles every regex
    /// up front; a bad regex surfaces as a recipe-level error
    /// before any file is read.
    pub fn new(rules: &[SectionRule], fallback: Option<FallbackRule>) -> Result<Self> {
        if rules.is_empty() {
            return Err(Error::Recipe(
                "html_sections requires at least one [[extract.sections]] entry".into(),
            ));
        }
        let mut compiled = Vec::with_capacity(rules.len());
        for r in rules {
            let start = Regex::new(&r.start_pattern).map_err(|e| {
                Error::Recipe(format!(
                    "section `{}`: invalid start_pattern `{}`: {e}",
                    r.name, r.start_pattern
                ))
            })?;
            let end = Regex::new(&r.end_pattern).map_err(|e| {
                Error::Recipe(format!(
                    "section `{}`: invalid end_pattern `{}`: {e}",
                    r.name, r.end_pattern
                ))
            })?;
            compiled.push(CompiledRule {
                name: r.name.clone(),
                description: r.description.clone(),
                start,
                end,
                repeating: r.repeating,
            });
        }
        Ok(Self {
            rules: compiled,
            fallback,
        })
    }
}

impl Extractor for HtmlSectionsExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let files = collect_html_files(source_path)?;
        let mut docs: Vec<ExtractedDoc> = Vec::new();
        let mut misses: Vec<MissReport> = Vec::new();

        for file in &files {
            process_one_file(
                file,
                &self.rules,
                self.fallback.as_ref(),
                &mut docs,
                &mut misses,
            )?;
        }

        // Persist misses sidecar even when empty — its absence vs.
        // an empty array carries information for `recipe test`.
        if !misses.is_empty() {
            // Write under the source dir; if source_path is a single
            // file, write next to it.
            let misses_path = if source_path.is_dir() {
                source_path.join("_section_misses.json")
            } else {
                source_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("_section_misses.json")
            };
            // Best-effort write: failure to write the misses file
            // shouldn't kill the ingest.
            if let Ok(json) = serde_json::to_vec_pretty(&misses) {
                let _ = fs::write(&misses_path, json);
            }
        }

        Ok(Box::new(docs.into_iter().map(Ok)))
    }
}

fn process_one_file(
    file: &Path,
    rules: &[CompiledRule],
    fallback: Option<&FallbackRule>,
    docs: &mut Vec<ExtractedDoc>,
    misses: &mut Vec<MissReport>,
) -> Result<()> {
    let raw = fs::read_to_string(file)
        .map_err(|e| Error::Extraction(format!("Failed to read {}: {e}", file.display())))?;
    let title = extract_title_from_html(&raw).unwrap_or_else(|| {
        file.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string()
    });
    let stripped = strip_html(&raw);
    let stripped_trim = stripped.trim();
    if stripped_trim.is_empty() {
        return Ok(());
    }

    let file_label = file.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    let mut any_matched = false;

    for rule in rules {
        let sections = find_sections(&stripped, rule);
        if sections.is_empty() {
            misses.push(MissReport {
                file: file_label.to_string(),
                section: rule.name.clone(),
                description: rule.description.clone(),
                nearby_text: nearby_text_hint(&stripped, &rule.description),
            });
            continue;
        }
        any_matched = true;
        let multiple = sections.len() > 1;
        for (idx, section_text) in sections.into_iter().enumerate() {
            // Stable, unique source_id. Single-match rules keep the
            // historical 2-part slug (delta-update + chunk-id stability);
            // repeating rules append the occurrence index.
            let source_id = if multiple {
                slug(&format!("{}-{}-{}", file_label, rule.name, idx + 1))
            } else {
                slug(&format!("{}-{}", file_label, rule.name))
            };
            let combined_title = if multiple {
                format!("{} — {} #{}", title, rule.name, idx + 1)
            } else {
                format!("{} — {}", title, rule.name)
            };
            docs.push(ExtractedDoc {
                title: Some(combined_title),
                content: section_text,
                url: None,
                source_id,
                metadata: Some(serde_json::json!({
                    "section_name": rule.name,
                    "section_description": rule.description,
                    "section_index": idx + 1,
                    "source_file": file_label,
                })),
                source_file: Some(file_label.to_string()),
                embed_text: None,
            });
        }
    }

    if !any_matched {
        if let Some(fb) = fallback {
            let body = match fb {
                FallbackRule::FullDocument { max_chars } => truncate_chars(&stripped, *max_chars),
                FallbackRule::FirstNChars { n } => truncate_chars(&stripped, Some(*n)),
            };
            if !body.trim().is_empty() {
                let source_id = slug(&format!("{}-fallback", file_label));
                docs.push(ExtractedDoc {
                    title: Some(format!("{} — fallback", title)),
                    content: body,
                    url: None,
                    source_id,
                    metadata: Some(serde_json::json!({
                        "section_name": "_fallback",
                        "source_file": file_label,
                    })),
                    source_file: Some(file_label.to_string()),
                    embed_text: None,
                });
            }
        }
    }
    Ok(())
}

/// Find all sections a rule matches in `stripped`. A non-repeating
/// rule yields at most one (the first start→end span). A repeating
/// rule yields one per start match, each running to the next start
/// match and bounded earlier by `end_pattern` within that window — so
/// consecutive items never bleed and the final item can terminate on a
/// trailing anchor. Empty/whitespace-only spans are dropped.
fn find_sections(stripped: &str, rule: &CompiledRule) -> Vec<String> {
    if !rule.repeating {
        return find_section(stripped, rule)
            .filter(|s| !s.trim().is_empty())
            .into_iter()
            .collect();
    }
    let bounds: Vec<(usize, usize)> = rule
        .start
        .find_iter(stripped)
        .map(|m| (m.start(), m.end()))
        .collect();
    let mut out = Vec::with_capacity(bounds.len());
    for (i, &(from, start_end)) in bounds.iter().enumerate() {
        let next_start = bounds.get(i + 1).map(|&(s, _)| s).unwrap_or(stripped.len());
        // Look for an end anchor strictly after this start, but never
        // past the next start match (which always terminates the item).
        let window = &stripped[start_end..next_start];
        let end_rel = rule.end.find(window).map(|m| m.start()).unwrap_or(window.len());
        let to = start_end + end_rel;
        let segment = stripped[from..to].trim();
        if !segment.is_empty() {
            out.push(segment.to_string());
        }
    }
    out
}

fn find_section(stripped: &str, rule: &CompiledRule) -> Option<String> {
    let start_match = rule.start.find(stripped)?;
    let from = start_match.start();
    // Search for the end pattern strictly *after* the start match so
    // the start anchor's own match doesn't terminate the section.
    let after = &stripped[start_match.end()..];
    let end_match = rule.end.find(after);
    let to_relative = end_match.map(|m| m.start()).unwrap_or(after.len());
    let to = start_match.end() + to_relative;
    let segment = &stripped[from..to];
    Some(segment.trim().to_string())
}

/// Capture a 200-char window from the stripped text near where the
/// section was likely supposed to live. Heuristic: use any keyword
/// from the rule's description as a needle. If no match, return the
/// first 200 chars (better than nothing).
fn nearby_text_hint(stripped: &str, description: &str) -> Option<String> {
    let keywords: Vec<&str> = description
        .split_ascii_whitespace()
        .filter(|w| w.len() >= 4) // skip stopwords-ish tiny tokens
        .collect();
    for kw in keywords {
        let needle = kw.to_lowercase();
        let lower = stripped.to_lowercase();
        if let Some(pos) = lower.find(&needle) {
            let window = window_around(stripped, pos, 200);
            return Some(window);
        }
    }
    if stripped.len() > 200 {
        Some(stripped.chars().take(200).collect::<String>() + "…")
    } else {
        Some(stripped.to_string())
    }
}

fn window_around(text: &str, byte_pos: usize, span: usize) -> String {
    // Aim for ~span chars centered on byte_pos. Walk char boundaries
    // so we never split a UTF-8 codepoint.
    let half = span / 2;
    let start = byte_pos.saturating_sub(half);
    let end = (byte_pos + half).min(text.len());

    let safe_start = floor_char_boundary(text, start);
    let safe_end = ceil_char_boundary(text, end);
    let snippet = &text[safe_start..safe_end];
    let mut out = String::with_capacity(snippet.len() + 4);
    if safe_start > 0 {
        out.push('…');
    }
    out.push_str(snippet);
    if safe_end < text.len() {
        out.push('…');
    }
    out
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn truncate_chars(s: &str, max_chars: Option<usize>) -> String {
    match max_chars {
        Some(n) if s.chars().count() > n => s.chars().take(n).collect(),
        _ => s.to_string(),
    }
}

/// Same `<title>` extraction as plain HTML — duplicated locally so
/// we don't widen the html.rs module's API surface.
fn extract_title_from_html(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title>")? + 7;
    let end = lower[start..].find("</title>")? + start;
    let raw = &html[start..end];
    let title = raw.trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

/// Per-section miss for the `_section_misses.json` sidecar that
/// `sovereign recipe test` reads to report "section X missed in
/// file Y". Both Serialize (extractor writes) and Deserialize
/// (test harness reads) implementations are derived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissReport {
    pub file: String,
    pub section: String,
    pub description: String,
    /// 200-char snippet from the stripped HTML near where the
    /// section was expected — empty on totally empty inputs. Used
    /// by `recipe test` to produce the "Suggestion: try pattern X"
    /// nudge.
    pub nearby_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::SectionRule;

    fn rule(name: &str, start: &str, end: &str) -> SectionRule {
        SectionRule {
            name: name.into(),
            description: format!("description of {name}"),
            start_pattern: start.into(),
            end_pattern: end.into(),
            repeating: false,
        }
    }

    fn repeating_rule(name: &str, start: &str, end: &str) -> SectionRule {
        SectionRule {
            repeating: true,
            ..rule(name, start, end)
        }
    }

    #[test]
    fn repeating_rule_emits_one_doc_per_match() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("filing.html");
        // Three numbered proposals followed by a trailing anchor. A
        // non-repeating rule would capture only the first; repeating
        // must yield three, each bounded by the next item (and the
        // last by the end anchor).
        std::fs::write(
            &f,
            "<html><body>\
             <p>Item 4 - Alpha proposal. RESOLVED: do alpha.</p>\
             <p>Item 5 - Beta proposal. RESOLVED: do beta.</p>\
             <p>Item 6 - Gamma proposal. RESOLVED: do gamma.</p>\
             <p>ADDITIONAL INFORMATION other business</p>\
             </body></html>",
        )
        .unwrap();
        let r = repeating_rule(
            "proposal",
            r"(?i)item\s+\d+\s*[-–]",
            r"(?i)additional\s+information",
        );
        let docs: Vec<_> = HtmlSectionsExtractor::new(&[r], None)
            .unwrap()
            .extract(dir.path())
            .unwrap()
            .map(|d| d.unwrap())
            .collect();
        assert_eq!(docs.len(), 3, "one doc per Item match");
        assert!(docs[0].content.contains("Alpha"));
        assert!(!docs[0].content.contains("Beta"), "items must not bleed");
        assert!(docs[1].content.contains("Beta"));
        assert!(docs[2].content.contains("Gamma"));
        // The last item terminates on the end anchor, not EOF.
        assert!(!docs[2].content.contains("ADDITIONAL INFORMATION"));
        // Distinct, index-suffixed source ids.
        let ids: std::collections::HashSet<_> =
            docs.iter().map(|d| d.source_id.clone()).collect();
        assert_eq!(ids.len(), 3, "source ids must be unique per occurrence");
    }

    #[test]
    fn rejects_empty_rules() {
        let err = HtmlSectionsExtractor::new(&[], None).unwrap_err();
        assert!(format!("{err}").contains("at least one"));
    }

    #[test]
    fn rejects_invalid_regex() {
        let r = rule("x", "(((bad", "y");
        let err = HtmlSectionsExtractor::new(&[r], None).unwrap_err();
        assert!(format!("{err}").contains("invalid start_pattern"));
    }

    #[test]
    fn extracts_one_section_per_match() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("filing.html");
        std::fs::write(
            &f,
            "<html><head><title>NVDA 10-K</title></head><body>\
             <p>Item 6 Selected Financial Data</p>\
             <p>Boring numbers go here.</p>\
             <p>Item 7 Management Discussion and Analysis</p>\
             <p>Forward-looking statements about cloud GPUs.</p>\
             <p>Item 8 Financial Statements</p>\
             <p>Tables.</p></body></html>",
        )
        .unwrap();

        let extractor =
            HtmlSectionsExtractor::new(&[rule("md_and_a", r"(?i)Item\s+7", r"(?i)Item\s+8")], None)
                .unwrap();

        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert!(docs[0].title.as_deref().unwrap().contains("md_and_a"));
        assert!(docs[0].content.contains("Forward-looking"));
        assert!(!docs[0].content.contains("Tables"));

        let meta = docs[0].metadata.as_ref().unwrap();
        assert_eq!(meta["section_name"], "md_and_a");
    }

    #[test]
    fn miss_writes_sidecar_with_nearby_text() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("filing.html");
        std::fs::write(
            &f,
            "<html><head><title>X</title></head><body>\
             <p>Note 12 Concentration of Credit Risk</p>\
             <p>Some financial details.</p></body></html>",
        )
        .unwrap();

        // Try to find Revenue Disaggregation — this filing uses the
        // older "Concentration of Credit Risk" heading instead, so
        // we expect a miss.
        let extractor = HtmlSectionsExtractor::new(
            &[rule(
                "revenue_disaggregation",
                r"(?i)revenue.disaggregat",
                r"(?i)note\s+\d+",
            )],
            None,
        )
        .unwrap();
        let _docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        let sidecar = dir.path().join("_section_misses.json");
        assert!(sidecar.exists(), "miss sidecar must be written");
        let raw = std::fs::read_to_string(&sidecar).unwrap();
        let parsed: Vec<MissReport> = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].section, "revenue_disaggregation");
        // Nearby text should reference some keyword from the
        // description "description of revenue_disaggregation".
        assert!(parsed[0].nearby_text.is_some());
    }

    #[test]
    fn fallback_full_document_emits_when_no_section_matches() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("filing.html");
        std::fs::write(
            &f,
            "<html><head><title>X</title></head><body>\
             <p>This is a press release with no item structure.</p>\
             </body></html>",
        )
        .unwrap();

        let extractor = HtmlSectionsExtractor::new(
            &[rule("md_and_a", r"(?i)Item\s+7", r"(?i)Item\s+8")],
            Some(FallbackRule::FullDocument {
                max_chars: Some(80),
            }),
        )
        .unwrap();

        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        // strip_html keeps the <title> tag's text inline with body
        // text, so the fallback content starts with the title `X`
        // followed by the body. We only assert the body is in there
        // and the cap is honored.
        assert!(docs[0].content.contains("This is a press"));
        assert!(docs[0].content.chars().count() <= 80);
        assert_eq!(
            docs[0].metadata.as_ref().unwrap()["section_name"],
            "_fallback"
        );
    }

    #[test]
    fn no_fallback_drops_files_with_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("filing.html");
        std::fs::write(&f, "<html><body><p>nothing relevant</p></body></html>").unwrap();
        let extractor =
            HtmlSectionsExtractor::new(&[rule("md_and_a", r"(?i)Item\s+7", r"(?i)Item\s+8")], None)
                .unwrap();
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(docs.is_empty());
    }

    #[test]
    fn multiple_sections_emit_multiple_docs() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("filing.html");
        std::fs::write(
            &f,
            "<html><body>\
             <p>Item 7 MD&amp;A. Cloud GPU revenue grew 200%.</p>\
             <p>Item 8 Financial Statements.</p>\
             <p>Note 12 Related Party Transactions. Sold to MSFT.</p>\
             <p>Note 13 Subsequent Events.</p></body></html>",
        )
        .unwrap();

        let extractor = HtmlSectionsExtractor::new(
            &[
                rule("md_and_a", r"(?i)Item\s+7", r"(?i)Item\s+8"),
                rule(
                    "related_party",
                    r"(?i)Note\s+\d+\s+Related",
                    r"(?i)Note\s+\d+\s+Subsequent",
                ),
            ],
            None,
        )
        .unwrap();
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 2);
        let names: Vec<&str> = docs
            .iter()
            .map(|d| {
                d.metadata
                    .as_ref()
                    .unwrap()
                    .get("section_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            })
            .collect();
        assert!(names.contains(&"md_and_a"));
        assert!(names.contains(&"related_party"));
    }
}
