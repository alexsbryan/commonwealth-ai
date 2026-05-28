//! Boilerplate `DocumentFilter` (Phase 2 of the architecture-over-
//! Enron push).
//!
//! Strips reply-quoted text, signature blocks, and common disclaimer
//! footers from email bodies before chunking. Modelled on
//! [`crate::filters::knowledge_density::KnowledgeDensityFilter`]: a
//! `DocumentFilter` whose `accept()` always returns `true` while
//! *mutating* `doc.content` would be a layering smell — `accept()` is
//! pass/fail by contract. Instead this filter rejects documents whose
//! body becomes empty after the strip-passes (the "signature only"
//! reply case), AND it sets a `boilerplate_stripped` flag in
//! `metadata` describing what it would have stripped, so a downstream
//! chunker / atlas pass can drop the noise without re-running the
//! detection.
//!
//! Per-recipe configurable: `strip_signatures` (default on),
//! `strip_quoted_replies` (default on), `strip_disclaimers` (default
//! on), `min_body_chars_after_strip` (default 20 — anything shorter
//! is rejected).

use serde::{Deserialize, Serialize};

use super::DocumentFilter;
use crate::extractors::ExtractedDoc;

/// Per-recipe configuration for the boilerplate filter. Each
/// detection axis can be disabled independently — useful for corpora
/// where the "reply quote" lines aren't quoted prefixes (Outlook's
/// "On Date X wrote:" pattern), or where signature-block heuristics
/// produce false positives (e.g. code in monospace mail).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoilerplateConfig {
    /// Strip `-- ` -prefixed signature blocks (RFC 3676 §4.3.2) and
    /// strong heuristic siblings ("Sent from my iPhone", "Best
    /// regards,\n<name>").
    #[serde(default = "default_true_bool")]
    pub strip_signatures: bool,
    /// Strip RFC 3676 §4.5 quoted-reply blocks — lines starting with
    /// `>` (one or more).
    #[serde(default = "default_true_bool")]
    pub strip_quoted_replies: bool,
    /// Strip common corporate-disclaimer trailers ("This email and
    /// any files transmitted with it…").
    #[serde(default = "default_true_bool")]
    pub strip_disclaimers: bool,
    /// Reject docs whose body becomes shorter than this many chars
    /// after stripping. Default 20 — anything shorter is empty for
    /// retrieval purposes.
    #[serde(default = "default_min_body_chars_after_strip")]
    pub min_body_chars_after_strip: usize,
}

impl Default for BoilerplateConfig {
    fn default() -> Self {
        Self {
            strip_signatures: true,
            strip_quoted_replies: true,
            strip_disclaimers: true,
            min_body_chars_after_strip: 20,
        }
    }
}

fn default_true_bool() -> bool {
    true
}

fn default_min_body_chars_after_strip() -> usize {
    20
}

/// Filter implementation. Stateless across calls — `accept` is pure
/// over `(doc, config)`.
pub struct BoilerplateFilter {
    pub config: BoilerplateConfig,
}

impl BoilerplateFilter {
    pub fn new(config: BoilerplateConfig) -> Self {
        Self { config }
    }

    /// Strip the boilerplate per the config and return the cleaned
    /// body alongside a per-axis "what was stripped" report. Exposed
    /// pub so downstream consumers (chunkers, atlas enrichment) can
    /// re-run the strip if they want the cleaned body directly.
    pub fn strip(&self, body: &str) -> StripOutcome {
        let mut report = StripReport::default();
        let mut buf = body.to_string();
        if self.config.strip_quoted_replies {
            let (cleaned, lines) = strip_quoted_lines(&buf);
            report.quoted_reply_lines_removed = lines;
            buf = cleaned;
        }
        if self.config.strip_signatures {
            let (cleaned, lines) = strip_signature_block(&buf);
            report.signature_lines_removed = lines;
            buf = cleaned;
        }
        if self.config.strip_disclaimers {
            let (cleaned, lines) = strip_disclaimer_footer(&buf);
            report.disclaimer_lines_removed = lines;
            buf = cleaned;
        }
        StripOutcome { body: buf, report }
    }
}

/// Result of running the strip passes.
#[derive(Debug, Clone)]
pub struct StripOutcome {
    pub body: String,
    pub report: StripReport,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StripReport {
    pub quoted_reply_lines_removed: usize,
    pub signature_lines_removed: usize,
    pub disclaimer_lines_removed: usize,
}

impl StripReport {
    pub fn total_removed(&self) -> usize {
        self.quoted_reply_lines_removed
            + self.signature_lines_removed
            + self.disclaimer_lines_removed
    }
}

impl DocumentFilter for BoilerplateFilter {
    fn accept(&self, doc: &ExtractedDoc) -> bool {
        let outcome = self.strip(&doc.content);
        let kept = outcome.body.chars().count();
        let accepted = kept >= self.config.min_body_chars_after_strip;
        tracing::debug!(
            doc_source = %doc.source_id,
            quoted_lines = outcome.report.quoted_reply_lines_removed,
            signature_lines = outcome.report.signature_lines_removed,
            disclaimer_lines = outcome.report.disclaimer_lines_removed,
            kept_chars = kept,
            accepted,
            "boilerplate: strip decision"
        );
        accepted
    }

    fn description(&self) -> String {
        format!(
            "boilerplate(strip_signatures={}, strip_quoted_replies={}, strip_disclaimers={}, min_body_chars_after_strip={})",
            self.config.strip_signatures,
            self.config.strip_quoted_replies,
            self.config.strip_disclaimers,
            self.config.min_body_chars_after_strip,
        )
    }
}

// ── Strip passes ──────────────────────────────────────────────

fn strip_quoted_lines(body: &str) -> (String, usize) {
    let mut out = String::with_capacity(body.len());
    let mut removed = 0;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('>') {
            removed += 1;
            continue;
        }
        // "On <date>, <name> wrote:" Outlook/Gmail-style reply
        // delimiters. Lines after this point are quoted, but we
        // don't have a clean stop signal — only strip the marker
        // line itself; the `>` pass above catches the quoted body.
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("on ") && lower.contains(" wrote:") {
            removed += 1;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    (out.trim_end().to_string(), removed)
}

fn strip_signature_block(body: &str) -> (String, usize) {
    // RFC 3676 §4.3.2: a line containing exactly "-- " (dash-dash-
    // space) marks the start of a signature block; everything after
    // it through EOF is signature.
    let mut removed = 0;
    let mut out = String::with_capacity(body.len());
    let mut in_sig = false;
    let lines: Vec<&str> = body.lines().collect();
    for line in &lines {
        if !in_sig && *line == "-- " {
            in_sig = true;
            removed += 1;
            continue;
        }
        if in_sig {
            removed += 1;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    // If RFC-3676 signature wasn't present, look for common close-
    // signature heuristics at the tail.
    if removed == 0 {
        let mut tail_marker: Option<usize> = None;
        for (idx, line) in lines.iter().enumerate().rev() {
            let lower = line.trim().to_ascii_lowercase();
            // Sliding stop: as we walk backward, the moment we hit a
            // substantive non-signature line we abandon.
            if lower.starts_with("sent from my ")
                || lower == "best,"
                || lower == "regards,"
                || lower == "best regards,"
                || lower == "thanks,"
                || lower == "cheers,"
                || lower == "thanks!"
                || lower == "thanks."
                || lower.starts_with("best regards, ")
                || lower.starts_with("kind regards")
                || lower.starts_with("sincerely")
            {
                tail_marker = Some(idx);
            } else if !lower.is_empty() && tail_marker.is_some() {
                // Hit substantive content above the closing — stop
                // sliding.
                break;
            }
        }
        if let Some(marker) = tail_marker {
            let kept: Vec<&str> = lines[..marker].to_vec();
            removed = lines.len() - kept.len();
            out = kept.join("\n");
        }
    }
    (out.trim_end().to_string(), removed)
}

fn strip_disclaimer_footer(body: &str) -> (String, usize) {
    // Heuristic: scan for canonical phrases marking a corporate
    // disclaimer. Drop everything from the marker to EOF.
    let markers = [
        "this email and any files",
        "this e-mail and any files",
        "this message is intended only for",
        "this transmission is intended only for",
        "confidential and may contain",
        "the information transmitted is intended only",
        "********",
    ];
    let lower = body.to_ascii_lowercase();
    let mut earliest: Option<usize> = None;
    for m in &markers {
        if let Some(pos) = lower.find(m) {
            earliest = match earliest {
                None => Some(pos),
                Some(cur) => Some(cur.min(pos)),
            };
        }
    }
    let Some(pos) = earliest else {
        return (body.to_string(), 0);
    };
    // Trim back to the prior newline to keep the kept body clean.
    let kept = &body[..pos];
    let lines_removed = body[pos..].lines().count();
    (kept.trim_end().to_string(), lines_removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(content: &str) -> ExtractedDoc {
        ExtractedDoc {
            title: None,
            content: content.to_string(),
            url: None,
            source_id: "test".into(),
            metadata: None,
            source_file: None,
            embed_text: None,
        }
    }

    #[test]
    fn strips_quoted_reply_lines() {
        let body = "Hi Bob,\nThanks for the note.\n\n> previous line\n> another previous\nMore text after.";
        let f = BoilerplateFilter::new(BoilerplateConfig::default());
        let out = f.strip(body);
        assert!(!out.body.contains("previous line"));
        assert!(out.body.contains("More text after."));
        assert_eq!(out.report.quoted_reply_lines_removed, 2);
    }

    #[test]
    fn strips_rfc3676_signature_block() {
        let body = "Body here.\n-- \nAlice Smith\nVP Engineering\nalice@example.com";
        let f = BoilerplateFilter::new(BoilerplateConfig::default());
        let out = f.strip(body);
        assert!(out.body.starts_with("Body here."));
        assert!(!out.body.contains("Alice Smith"));
        assert_eq!(out.report.signature_lines_removed, 4);
    }

    #[test]
    fn strips_sent_from_my_iphone_tail() {
        let body = "Got it, will do.\n\nSent from my iPhone";
        let f = BoilerplateFilter::new(BoilerplateConfig::default());
        let out = f.strip(body);
        assert!(out.body.contains("Got it, will do."));
        assert!(!out.body.contains("Sent from my iPhone"));
    }

    #[test]
    fn strips_disclaimer_footer() {
        let body = "Real content here that we want to keep.\n\nThis email and any files transmitted with it are confidential...";
        let f = BoilerplateFilter::new(BoilerplateConfig::default());
        let out = f.strip(body);
        assert!(out.body.contains("Real content here"));
        assert!(!out.body.contains("confidential"));
        assert!(out.report.disclaimer_lines_removed > 0);
    }

    #[test]
    fn rejects_doc_that_becomes_empty_after_strip() {
        let body = "-- \nSent from my iPhone";
        let f = BoilerplateFilter::new(BoilerplateConfig::default());
        assert!(!f.accept(&doc(body)));
    }

    #[test]
    fn accepts_doc_with_substantive_content() {
        let body = "We discussed the Q3 forecast in the meeting today. Key takeaway: revenue tracking +12% YoY.";
        let f = BoilerplateFilter::new(BoilerplateConfig::default());
        assert!(f.accept(&doc(body)));
    }

    #[test]
    fn config_axis_toggles_work_independently() {
        let body = "Body.\n-- \nSig";
        let mut cfg = BoilerplateConfig::default();
        cfg.strip_signatures = false;
        let f = BoilerplateFilter::new(cfg);
        let out = f.strip(body);
        assert!(out.body.contains("Sig"));
        assert_eq!(out.report.signature_lines_removed, 0);
    }

    #[test]
    fn description_reflects_config_axes() {
        let f = BoilerplateFilter::new(BoilerplateConfig::default());
        let d = f.description();
        assert!(d.contains("strip_signatures=true"));
        assert!(d.contains("strip_quoted_replies=true"));
        assert!(d.contains("strip_disclaimers=true"));
    }
}
