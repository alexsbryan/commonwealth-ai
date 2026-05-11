//! Per-article Wikipedia extractor — MediaWiki Action API responses.
//!
//! Consumes the JSON returned by:
//!
//! ```text
//! GET https://en.wikipedia.org/w/api.php
//!     ?action=parse
//!     &page=<title>
//!     &prop=wikitext|sections|links|properties
//!     &format=json&formatversion=2
//! ```
//!
//! and produces one [`ExtractedDoc`] per article section, mirroring
//! the shape that [`crate::extractors::wikipedia_jsonl::WikipediaJsonlExtractor`]
//! produces from the bulk dump. That parity is the whole point: an
//! article fetched on-demand via the Action API yields the same
//! `WikipediaChunkMetadata` (section_type, outgoing_links,
//! wikidata_qid, page_id, revision_id) that bulk-extracted articles
//! carry, so retrieval, atlas grounding, and the wikilink-graph
//! expansion treat fetched articles identically to dump-ingested
//! ones.
//!
//! Pair with `[corpus] kind = "knowledge"`, `on_demand = true`,
//! `parent_corpus_id = "wikipedia"` so per-article ingests land
//! under the user's existing Wikipedia corpus rather than as
//! one-off per-work corpora.
//!
//! ## Section parsing
//!
//! The Action API returns:
//!   - `wikitext.*`: full wikitext as one string
//!   - `sections[]`: anchor + level + line + offset metadata
//!
//! We slice the wikitext at section offsets to get per-section
//! bodies, then strip wikitext markup down to plain text. The
//! resulting docs feed into the chunker exactly like bulk JSONL
//! sections do. We deliberately don't attempt full wikitext-to-
//! HTML rendering — the embed model only needs prose, not perfect
//! formatting.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

use super::wikipedia_structured::{
    classify_section, DEFAULT_CONTROVERSY_PATTERNS, DEFAULT_FACTUAL_PATTERNS,
    MAX_SECTION_DEPTH, MIN_SECTION_TEXT,
};
use super::wikipedia_types::{WikiLink, WikipediaChunkMetadata};
use super::{slug, ExtractedDoc, Extractor};
use crate::error::{Error, Result};

pub struct WikipediaApiArticleExtractor {
    pub controversy_patterns: Vec<String>,
    pub factual_patterns: Vec<String>,
}

impl Default for WikipediaApiArticleExtractor {
    fn default() -> Self {
        Self {
            controversy_patterns: DEFAULT_CONTROVERSY_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            factual_patterns: DEFAULT_FACTUAL_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

impl Extractor for WikipediaApiArticleExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let mut file = File::open(source_path).map_err(|e| {
            Error::Extraction(format!(
                "wikipedia_api_article: open {}: {e}",
                source_path.display()
            ))
        })?;
        let mut raw = String::new();
        file.read_to_string(&mut raw).map_err(|e| {
            Error::Extraction(format!("wikipedia_api_article: read body: {e}"))
        })?;
        let v: Value = serde_json::from_str(&raw).map_err(|e| {
            Error::Extraction(format!("wikipedia_api_article: parse JSON: {e}"))
        })?;

        // The Action API surfaces errors at top-level; surface them
        // cleanly so the catalog ingest path can tell missing-page
        // from network-bad from API-quota apart.
        if let Some(err) = v.get("error") {
            return Err(Error::Extraction(format!(
                "wikipedia_api_article: API error: {}",
                serde_json::to_string(err).unwrap_or_default()
            )));
        }
        let parse = v
            .get("parse")
            .ok_or_else(|| Error::Extraction("wikipedia_api_article: missing `parse` field".into()))?;

        let docs = build_docs(parse, &self.controversy_patterns, &self.factual_patterns);
        Ok(Box::new(docs.into_iter().map(Ok)))
    }
}

fn build_docs(
    parse: &Value,
    controversy_patterns: &[String],
    factual_patterns: &[String],
) -> Vec<ExtractedDoc> {
    let title = parse
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if title.is_empty() {
        return Vec::new();
    }
    let url = format!(
        "https://en.wikipedia.org/wiki/{}",
        title.replace(' ', "_")
    );

    let page_id = parse.get("pageid").and_then(|v| v.as_i64());
    let revision_id = parse.get("revid").and_then(|v| v.as_i64());
    // wikidata QID lives under properties[].name == "wikibase_item"
    let wikidata_qid = parse
        .get("properties")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|p| {
                let name = p.get("name").and_then(|n| n.as_str())?;
                if name == "wikibase_item" {
                    p.get("value").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
        });

    let wikitext = parse
        .get("wikitext")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Article-wide outgoing links — the Action API gives us one flat
    // list rather than per-section. We attach them to the lead doc
    // and an empty list to body sections; downstream consumers
    // (atlas link-graph builder) aggregate at the article level
    // anyway, so this preserves the link signal without trying to
    // reconstruct per-section attribution we don't have.
    let article_links: Vec<WikiLink> = parse
        .get("links")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| {
                    let ns = l.get("ns").and_then(|n| n.as_i64()).unwrap_or(-1);
                    if ns != 0 {
                        return None; // Mainspace only.
                    }
                    let _exists = l.get("exists").and_then(|e| e.as_bool()).unwrap_or(true);
                    let title = l.get("title").and_then(|t| t.as_str())?.to_string();
                    Some(WikiLink {
                        link_text: title.clone(),
                        target_title: title,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let mut docs = Vec::new();

    // ── Lead chunk ────────────────────────────────────────────
    // Slice wikitext from the start to the first section heading
    // (or end-of-article when there are no sections).
    let sections = parse
        .get("sections")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let lead_end = sections
        .iter()
        .filter_map(|s| s.get("byteoffset").and_then(|n| n.as_u64()))
        .next()
        .map(|n| n as usize)
        .unwrap_or(wikitext.len());
    let lead_raw = wikitext.get(..lead_end.min(wikitext.len())).unwrap_or("");
    let lead = strip_wikitext(lead_raw);
    if lead.len() >= MIN_SECTION_TEXT {
        let meta = WikipediaChunkMetadata {
            section_name: "Lead".into(),
            section_path: vec![],
            section_depth: 0,
            section_type: "lead".into(),
            citation_needed_count: None,
            pov_count: None,
            clarification_needed_count: None,
            update_count: None,
            is_flagged_stable: None,
            outgoing_links: article_links.clone(),
            revision_id,
            wikidata_qid: wikidata_qid.clone(),
            page_id,
        };
        docs.push(ExtractedDoc {
            title: Some(title.clone()),
            content: lead,
            url: Some(url.clone()),
            source_id: format!("{}-lead", slug(&title)),
            metadata: serde_json::to_value(&meta).ok(),
            source_file: None,
            embed_text: None,
        });
    }

    // ── Body sections ─────────────────────────────────────────
    // Walk the section list; each section's body runs from its
    // own byteoffset to the next section's byteoffset (or EOF).
    for (i, sec) in sections.iter().enumerate() {
        let level = sec
            .get("level")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(2);
        // Wikipedia level 2 = top-level section; "depth" in
        // WikipediaChunkMetadata terms = level - 1 so a lead-
        // adjacent section is depth 1.
        let depth = level.saturating_sub(1);
        if depth > MAX_SECTION_DEPTH {
            continue;
        }
        let name = sec
            .get("line")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        if super::wikipedia_structured::should_skip_section(&name) {
            continue;
        }
        let start = sec
            .get("byteoffset")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize;
        let end = sections
            .get(i + 1)
            .and_then(|s| s.get("byteoffset").and_then(|n| n.as_u64()))
            .map(|n| n as usize)
            .unwrap_or(wikitext.len());
        if end <= start || end > wikitext.len() {
            continue;
        }
        // MediaWiki returns `byteoffset` as a UTF-8 byte index into the
        // wikitext, but the index occasionally lands inside a multi-byte
        // codepoint — typically when the section boundary abuts an
        // em-dash, en-dash, or non-ASCII character. A raw `wikitext[..]`
        // slice in that case panics the tokio worker (`byte index N is
        // not a char boundary; it is inside '–'`), which on
        // newsworthy-watcher follower steps takes down the daemon's
        // HTTP listener with no easy recovery. Use the fallible
        // `.get()` slice and skip the section if the bounds aren't
        // char-aligned — preferable to a global panic, and the next
        // refresh tick gets another chance with updated wikitext.
        // Surfaced 2026-05-10 by `2026_Israel–Lebanon_ceasefire` whose
        // 177703..177706 em-dash sat exactly at a section boundary.
        let Some(body_raw) = wikitext.get(start..end) else {
            tracing::warn!(
                start,
                end,
                section = name.as_str(),
                "wikipedia_api_article: byteoffset lands mid-codepoint; skipping section"
            );
            continue;
        };
        let body = strip_wikitext(body_raw);
        if body.len() < MIN_SECTION_TEXT {
            continue;
        }
        let section_type = classify_section(&name, controversy_patterns, factual_patterns);
        let meta = WikipediaChunkMetadata {
            section_name: name.clone(),
            section_path: vec![],
            section_depth: depth,
            section_type,
            citation_needed_count: None,
            pov_count: None,
            clarification_needed_count: None,
            update_count: None,
            is_flagged_stable: None,
            // We only have article-wide links from the Action API.
            // Leave per-section list empty so the atlas link-graph
            // builder doesn't double-count by aggregating per-
            // section + per-article.
            outgoing_links: vec![],
            revision_id,
            wikidata_qid: wikidata_qid.clone(),
            page_id,
        };
        docs.push(ExtractedDoc {
            title: Some(title.clone()),
            content: body,
            url: Some(url.clone()),
            source_id: format!("{}-{}", slug(&title), slug(&name)),
            metadata: serde_json::to_value(&meta).ok(),
            source_file: None,
            embed_text: None,
        });
    }

    docs
}

/// Strip wikitext markup down to readable prose. This is
/// intentionally lossy — the embed model wants meaning, not
/// faithful rendering. We:
///   - drop `<ref>...</ref>` blocks (citations)
///   - drop `{{...}}` templates (infoboxes, navboxes, citations)
///   - flatten `[[Target|Display]]` to `Display` (or `Target` if
///     no display text)
///   - drop bare `[url ...]` external links, keep the link text
///   - drop section headings (`==Heading==`) — those are tracked
///     separately as section metadata
///   - drop HTML tags, file embeds, and category links
///
/// Not perfect — wikitext is irregular — but good enough that
/// vector retrieval finds the right sections.
fn strip_wikitext(input: &str) -> String {
    let mut out = String::with_capacity(input.len());

    // Pass 1: drop {{templates}} via paren-matching. Naive nesting
    // tracker handles {{a|{{b}}|c}} correctly.
    let mut depth = 0;
    let mut buf1 = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            depth += 1;
            i += 2;
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'}' && bytes[i + 1] == b'}' && depth > 0 {
            depth -= 1;
            i += 2;
            continue;
        }
        if depth == 0 {
            buf1.push(bytes[i] as char);
        }
        i += 1;
    }

    // Pass 2: drop <ref>…</ref> + <ref ... /> via simple string ops.
    let buf2 = drop_tag_blocks(&buf1, "ref");
    let buf2 = drop_self_closing(&buf2, "ref");
    // Other narrative-irrelevant tags.
    let buf2 = drop_tag_blocks(&buf2, "noinclude");
    let buf2 = drop_tag_blocks(&buf2, "gallery");

    // Pass 3: flatten links + drop file embeds + drop headings
    // line-by-line so heading detection is anchored.
    for line in buf2.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
            continue;
        }
        // Drop section headings (==X==, ===X===, …) — section
        // metadata carries them separately.
        if line.starts_with('=') && line.ends_with('=') {
            continue;
        }
        let mut transformed = String::with_capacity(line.len());
        let cs: Vec<char> = line.chars().collect();
        let mut j = 0;
        while j < cs.len() {
            if j + 1 < cs.len() && cs[j] == '[' && cs[j + 1] == '[' {
                if let Some(close) = find_double_close(&cs, j + 2) {
                    let inner: String = cs[j + 2..close].iter().collect();
                    // Drop file/image/category/etc.
                    let lower = inner.to_lowercase();
                    if lower.starts_with("file:")
                        || lower.starts_with("image:")
                        || lower.starts_with("category:")
                    {
                        j = close + 2;
                        continue;
                    }
                    // Take display text after `|`, else the full target.
                    let display = inner.rsplit('|').next().unwrap_or(&inner);
                    transformed.push_str(display);
                    j = close + 2;
                    continue;
                }
            }
            if cs[j] == '[' {
                if let Some(close) = cs.iter().enumerate().skip(j + 1).find(|(_, c)| **c == ']') {
                    let inner: String = cs[j + 1..close.0].iter().collect();
                    // External link: `[url label]` → `label`. Bare URL: drop.
                    if let Some(idx) = inner.find(' ') {
                        transformed.push_str(&inner[idx + 1..]);
                    }
                    j = close.0 + 1;
                    continue;
                }
            }
            transformed.push(cs[j]);
            j += 1;
        }
        // Drop residual HTML tags on the line.
        let cleaned = strip_html_tags(&transformed);
        // Drop leading wikitext list / table / quote markers.
        let stripped = cleaned
            .trim_start_matches(|c: char| matches!(c, '*' | '#' | ':' | ';' | '|'))
            .trim();
        if !stripped.is_empty() {
            out.push_str(stripped);
            out.push('\n');
        }
    }

    out.trim().to_string()
}

fn find_double_close(cs: &[char], start: usize) -> Option<usize> {
    let mut k = start;
    while k + 1 < cs.len() {
        if cs[k] == ']' && cs[k + 1] == ']' {
            return Some(k);
        }
        k += 1;
    }
    None
}

fn drop_tag_blocks(input: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find(&open) {
        out.push_str(&rest[..idx]);
        // Find end of this opening tag (>) so we don't grab attrs.
        let after_open = &rest[idx..];
        let opening_end = after_open.find('>').map(|p| idx + p + 1).unwrap_or(rest.len());
        // Self-closing `<ref ... />` — handled by drop_self_closing later.
        if rest.get(opening_end.saturating_sub(2)..opening_end) == Some("/>") {
            rest = &rest[opening_end..];
            continue;
        }
        // Find matching close.
        let after = &rest[opening_end..];
        if let Some(close_idx) = after.find(&close) {
            rest = &after[close_idx + close.len()..];
        } else {
            rest = ""; // Malformed — drop the rest.
            break;
        }
    }
    out.push_str(rest);
    out
}

fn drop_self_closing(input: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find(&open) {
        // Look for the matching `/>` before any other `>`.
        let after = &rest[idx..];
        let close_self = after.find("/>");
        let close_normal = after.find('>');
        match (close_self, close_normal) {
            (Some(s), Some(n)) if s < n => {
                out.push_str(&rest[..idx]);
                rest = &after[s + 2..];
            }
            _ => {
                out.push_str(&rest[..idx + open.len()]);
                rest = &rest[idx + open.len()..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_response(json: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_response.json");
        let mut f = File::create(&path).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        dir
    }

    #[test]
    fn parses_a_canonical_response() {
        let body = r##"{
          "parse": {
            "title": "Albert Einstein",
            "pageid": 736,
            "revid": 12345,
            "wikitext": "Albert Einstein was a [[German people|German]]-born theoretical physicist who developed the [[theory of relativity]]. {{Infobox scientist|name=Albert Einstein}}\n\n==Early life==\nEinstein was born in [[Ulm]], in the Kingdom of [[Württemberg]].\n\n==Career==\nIn 1905 he published four [[Annus Mirabilis papers]].\n",
            "sections": [
              {"line":"Early life","level":"2","number":"1","index":"1","byteoffset":192},
              {"line":"Career","level":"2","number":"2","index":"2","byteoffset":290}
            ],
            "links": [
              {"ns":0,"exists":true,"title":"Theory of relativity"},
              {"ns":0,"exists":true,"title":"Ulm"},
              {"ns":0,"exists":true,"title":"Annus Mirabilis papers"},
              {"ns":14,"exists":true,"title":"Category:Physicists"}
            ],
            "properties": [
              {"name":"wikibase_item","value":"Q937"}
            ]
          }
        }"##;
        let dir = write_response(body);
        let path = dir.path().join("api_response.json");
        let docs: Vec<_> = WikipediaApiArticleExtractor::default()
            .extract(&path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        // Lead + 2 sections.
        assert!(docs.len() >= 1);
        // First doc: lead.
        let lead_meta: WikipediaChunkMetadata =
            serde_json::from_value(docs[0].metadata.clone().unwrap()).unwrap();
        assert_eq!(lead_meta.section_type, "lead");
        assert_eq!(lead_meta.wikidata_qid.as_deref(), Some("Q937"));
        assert_eq!(lead_meta.page_id, Some(736));
        assert_eq!(lead_meta.revision_id, Some(12345));
        // Article-level links land on the lead and exclude
        // category-namespace entries.
        assert!(lead_meta.outgoing_links.iter().any(|l| l.target_title == "Ulm"));
        assert!(!lead_meta
            .outgoing_links
            .iter()
            .any(|l| l.target_title.starts_with("Category")));
        // Lead body should be stripped of templates.
        assert!(!docs[0].content.contains("Infobox"));
        assert!(docs[0].content.contains("theory of relativity"));
    }

    #[test]
    fn surfaces_api_error_cleanly() {
        let body = r#"{"error":{"code":"missingtitle","info":"The page you specified doesn't exist."}}"#;
        let dir = write_response(body);
        let path = dir.path().join("api_response.json");
        let err = WikipediaApiArticleExtractor::default().extract(&path).err();
        assert!(err.is_some());
        assert!(format!("{}", err.unwrap()).contains("missingtitle"));
    }

    #[test]
    fn strip_wikitext_drops_templates_and_refs() {
        let wt = "Hello {{cite web|url=x}} world.<ref name=\"a\">cite</ref> more";
        let s = strip_wikitext(wt);
        assert!(!s.contains("cite web"));
        assert!(!s.contains("<ref"));
        assert!(s.contains("Hello"));
        assert!(s.contains("world"));
        assert!(s.contains("more"));
    }

    #[test]
    fn strip_wikitext_flattens_links() {
        let wt = "See [[theory of relativity|relativity]] and [[Ulm]] in Germany.";
        let s = strip_wikitext(wt);
        assert!(s.contains("relativity"));
        assert!(s.contains("Ulm"));
        // Display-text version, not the link target.
        assert!(!s.contains("theory of relativity|"));
    }

    #[test]
    fn strip_wikitext_drops_file_links() {
        let wt = "Photo: [[File:Einstein.jpg|thumb|caption]] then text.";
        let s = strip_wikitext(wt);
        assert!(!s.contains("File:"));
        assert!(!s.contains("thumb"));
        assert!(s.contains("Photo"));
        assert!(s.contains("then text"));
    }
}
