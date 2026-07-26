// SPDX-License-Identifier: AGPL-3.0-or-later
//! Frames — bounded, section-structured, incrementally-updated summaries
//! of a long-running interaction.
//!
//! ## Why this is a shared primitive
//!
//! Two callers arrived at the same problem from opposite ends:
//!
//! * **Session frames** (`sovereign_tools::code::session_state`): a
//!   coding agent records what it is doing so a successor session can
//!   resume without re-reading the repo.
//! * **Conversation frames** (`sovereign_core::conv_frame`): the chat
//!   runtime carries what a conversation established past the point
//!   where the verbatim turns roll out of the prompt window.
//!
//! Both need the same three properties, and getting any of them wrong
//! is what makes long-context memory quietly lossy:
//!
//! 1. **Named sections, not prose.** A prose blob has to be re-narrated
//!    on every update, and re-narration is where entities go to die. A
//!    section-keyed document is updated by replacing only the sections
//!    that changed.
//! 2. **A hard token budget, enforced at write time.** The document
//!    rides a prompt, so it cannot be allowed to grow. An over-budget
//!    write is REJECTED with per-section counts, so the writer trims
//!    deliberately — rather than the document being silently truncated
//!    somewhere downstream, which is invisible until recall fails.
//! 3. **Upsert, not replace.** Sections the writer did not mention keep
//!    their previous bodies. This is what makes the update incremental:
//!    the cost of an update is set by what changed, not by how long the
//!    interaction has run.
//!
//! This module is the mechanics only — parse, upsert, render, budget. It
//! holds no storage and no schema opinions: session frames live in
//! `~/.sovereign/sessions/<id>/frame.md` with git frontmatter,
//! conversation frames live on a conversation row. Each caller owns its
//! [`FrameSchema`], its persistence, and its writer.
//!
//! ## What it deliberately does NOT equalise
//!
//! A session frame is written by an agent that knows what it did; a
//! conversation frame is written by a model summarising turns it did not
//! author. Self-reported frames measure ~100% recall against the golden
//! where post-hoc distillation measures ~17% (SESSION_CONTINUITY.md §3).
//! Sharing the container improves robustness and makes both renderable;
//! it does not make a distilled frame as good as a self-reported one.

/// A frame's contract: which sections exist, in what order, and how
/// large the whole rendered document may get.
#[derive(Debug, Clone, Copy)]
pub struct FrameSchema {
    /// Stamped into new frames as the `schema:` frontmatter key.
    pub schema_id: &'static str,
    /// Canonical section headings, in render order. Every section is
    /// always materialised — "drop detail, never sections", so a reader
    /// can tell "nothing recorded here" from "this schema has no such
    /// slot".
    pub sections: &'static [&'static str],
    /// Cap on `approx_tokens(frame.render())`.
    pub token_budget: usize,
}

impl FrameSchema {
    /// Map a caller-supplied section name (canonical, lowercase, or
    /// `snake_case` param id) to its canonical heading.
    pub fn canonical_section(&self, name: &str) -> Option<&'static str> {
        let norm = name.trim().to_lowercase().replace('_', " ");
        self.sections
            .iter()
            .find(|s| s.to_lowercase() == norm)
            .copied()
    }

    /// A frame with every section materialised and empty, and no
    /// frontmatter. Callers add their own frontmatter keys.
    pub fn empty(&self) -> Frame {
        Frame {
            front: Vec::new(),
            bodies: self
                .sections
                .iter()
                .map(|s| (s.to_string(), String::new()))
                .collect(),
        }
    }

    /// Parse a rendered frame. Lenient by design: sections outside the
    /// schema are dropped (the upsert re-normalises to the contract),
    /// missing sections materialise empty, and a document with no
    /// frontmatter block parses fine. Unknown FRONTMATTER keys are
    /// preserved verbatim so a round-trip never strips fields a newer
    /// writer added.
    pub fn parse(&self, text: &str) -> Frame {
        let mut front = Vec::new();
        let mut rest = text;
        if let Some(after) = text.strip_prefix("---") {
            if let Some(end) = after.find("\n---") {
                for line in after[..end].lines() {
                    if let Some((k, v)) = line.split_once(':') {
                        front.push((k.trim().to_string(), v.trim().to_string()));
                    }
                }
                rest = &after[end + 4..];
            }
        }
        let mut frame = self.empty();
        frame.front = front;
        let mut current: Option<usize> = None;
        for line in rest.lines() {
            if let Some(heading) = line.strip_prefix("## ") {
                current = self
                    .canonical_section(heading)
                    .and_then(|c| frame.bodies.iter().position(|(n, _)| n == c));
                continue;
            }
            if let Some(idx) = current {
                frame.bodies[idx].1.push_str(line);
                frame.bodies[idx].1.push('\n');
            }
        }
        frame
    }
}

/// A parsed frame: ordered frontmatter plus one body per schema section.
#[derive(Debug, Clone, Default)]
pub struct Frame {
    /// Frontmatter key/value pairs in file order.
    pub front: Vec<(String, String)>,
    /// `(canonical section, body)` in schema order. Bodies may be empty.
    pub bodies: Vec<(String, String)>,
}

impl Frame {
    /// Frontmatter lookup.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.front
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Set a frontmatter key, appending if absent (order preserved).
    pub fn set(&mut self, key: &str, value: String) {
        match self.front.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => self.front.push((key.to_string(), value)),
        }
    }

    /// A section's body, or `None` when the section isn't in this frame.
    pub fn body(&self, section: &str) -> Option<&str> {
        self.bodies
            .iter()
            .find(|(n, _)| n == section)
            .map(|(_, b)| b.as_str())
    }

    /// Replace a section's body. Returns `false` (and changes nothing)
    /// when the section isn't in this frame — callers should validate
    /// against the schema first and treat `false` as a bug.
    pub fn set_body(&mut self, section: &str, body: String) -> bool {
        match self.bodies.iter_mut().find(|(n, _)| n == section) {
            Some(entry) => {
                entry.1 = body;
                true
            }
            None => false,
        }
    }

    /// True when no section carries any content — a frame not worth
    /// rendering into a prompt.
    pub fn is_empty(&self) -> bool {
        self.bodies.iter().all(|(_, b)| b.trim().is_empty())
    }

    /// The canonical on-disk / on-row form: frontmatter, then every
    /// section in schema order.
    pub fn render(&self) -> String {
        let mut out = String::from("---\n");
        for (k, v) in &self.front {
            out.push_str(&format!("{k}: {v}\n"));
        }
        out.push_str("---\n");
        for (name, body) in &self.bodies {
            out.push_str(&format!("\n## {name}\n\n"));
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                out.push_str(trimmed);
                out.push('\n');
            }
        }
        out
    }

    /// Prompt form: no frontmatter, and empty sections omitted.
    ///
    /// The full [`Self::render`] is the storage contract (all sections
    /// always present, so absence is legible). A prompt has the opposite
    /// need — an empty `## Commitments` heading spends tokens to say
    /// nothing, and invites the model to invent a commitment to put
    /// under it. Returns an empty string for an empty frame.
    pub fn render_for_prompt(&self) -> String {
        let mut out = String::new();
        for (name, body) in &self.bodies {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("{name}: {trimmed}\n"));
        }
        out
    }

    /// Per-section token estimates, for a budget-rejection message that
    /// tells the writer WHERE to trim.
    pub fn per_section_tokens(&self) -> Vec<(String, usize)> {
        self.bodies
            .iter()
            .map(|(n, b)| (n.clone(), approx_tokens(b)))
            .collect()
    }

    /// Check the rendered document against a schema's budget. `Ok` is
    /// the token total; `Err` carries everything needed to tell the
    /// writer what to drop.
    pub fn check_budget(&self, schema: &FrameSchema) -> Result<usize, BudgetError> {
        let total = approx_tokens(&self.render());
        if total > schema.token_budget {
            return Err(BudgetError {
                total,
                budget: schema.token_budget,
                per_section: self.per_section_tokens(),
            });
        }
        Ok(total)
    }
}

/// An over-budget frame. Carries per-section counts because "too big" is
/// not actionable and "State is 1.4k of your 2k" is.
#[derive(Debug, Clone)]
pub struct BudgetError {
    /// Estimated tokens the rendered frame would occupy.
    pub total: usize,
    /// The schema's cap.
    pub budget: usize,
    /// `(section, approx tokens)` for every section.
    pub per_section: Vec<(String, usize)>,
}

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let per: Vec<String> = self
            .per_section
            .iter()
            .map(|(n, t)| format!("{n} {t}t"))
            .collect();
        write!(
            f,
            "frame would be ~{} tokens (budget {}) — trim before writing. \
             Per section: {}. The spec: drop detail, never sections.",
            self.total,
            self.budget,
            per.join(", ")
        )
    }
}

/// ~4 chars per token — the same heuristic `cache-audit` uses. Frames
/// are budgeted, not billed, so an estimate that never needs a tokenizer
/// (and so works in `sovereign-contracts`, below every model binding) is
/// the right trade.
pub fn approx_tokens(s: &str) -> usize {
    s.chars().count() / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: FrameSchema = FrameSchema {
        schema_id: "test-frame/v1",
        sections: &["Topics", "Entities", "Open threads"],
        token_budget: 100,
    };

    #[test]
    fn round_trips_sections_and_unknown_frontmatter() {
        let mut f = SCHEMA.empty();
        f.set("schema", SCHEMA.schema_id.to_string());
        f.set("some_future_key", "kept".to_string());
        assert!(f.set_body("Topics", "polonium; radium".into()));

        let reparsed = SCHEMA.parse(&f.render());
        // `body` returns the raw span between headings — blank lines
        // included; `render` re-trims, so a round-trip is stable.
        assert_eq!(reparsed.body("Topics").map(str::trim), Some("polonium; radium"));
        assert_eq!(
            reparsed.get("some_future_key"),
            Some("kept"),
            "a round-trip must not strip keys a newer writer added"
        );
    }

    #[test]
    fn upsert_preserves_sections_the_writer_did_not_mention() {
        let mut f = SCHEMA.empty();
        f.set_body("Topics", "polonium".into());
        f.set_body("Entities", "Marie Curie".into());

        // Second write touches only Topics.
        let mut f = SCHEMA.parse(&f.render());
        f.set_body("Topics", "polonium; radium".into());

        assert_eq!(f.body("Entities").map(str::trim), Some("Marie Curie"));
    }

    #[test]
    fn unknown_sections_are_dropped_not_smuggled_through() {
        let doc = "---\n---\n\n## Topics\n\nkept\n\n## Nonsense\n\ndropped\n";
        let f = SCHEMA.parse(doc);
        assert_eq!(f.bodies.len(), SCHEMA.sections.len());
        assert!(!f.render().contains("Nonsense"));
        assert!(f.render().contains("kept"));
    }

    #[test]
    fn section_names_normalise_across_spellings() {
        assert_eq!(SCHEMA.canonical_section("open_threads"), Some("Open threads"));
        assert_eq!(SCHEMA.canonical_section("OPEN THREADS"), Some("Open threads"));
        assert_eq!(SCHEMA.canonical_section("nope"), None);
    }

    #[test]
    fn over_budget_reports_where_to_trim() {
        let mut f = SCHEMA.empty();
        f.set_body("Topics", "x".repeat(1000));
        let err = f.check_budget(&SCHEMA).expect_err("must reject");
        assert!(err.total > SCHEMA.token_budget);
        let topics = err
            .per_section
            .iter()
            .find(|(n, _)| n == "Topics")
            .expect("Topics counted");
        assert!(
            topics.1 > 200,
            "the message must point at the section that busted the budget"
        );
    }

    #[test]
    fn prompt_form_omits_empty_sections_and_frontmatter() {
        let mut f = SCHEMA.empty();
        f.set("schema", "test-frame/v1".into());
        f.set_body("Topics", "polonium".into());

        let p = f.render_for_prompt();
        assert!(p.contains("Topics: polonium"));
        assert!(!p.contains("Entities"), "empty sections cost tokens for nothing");
        assert!(!p.contains("schema"), "frontmatter is storage, not prompt");
        assert!(SCHEMA.empty().render_for_prompt().is_empty());
    }
}
