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

// ── Carried-item detection ───────────────────────────────────────────
//
// A frame can recopy its own backlog forever at zero cost. Measured on
// RuggedFox 2026-07-29 across the lineage 311ec4b7 → 8815fdb9 →
// c96d55a6: four `## Next` items rode all three frames — the 43% spread,
// the WorkerOverflow capacity basis, retiring the block-split pin, the
// gossip stall. None were done, none dropped, none re-ranked. Inheriting
// an item should be a decision; without a signal it is the default.
//
// These are pure combinators over rendered bodies. The chain walk that
// feeds them lives in the writer, which owns the filesystem.

/// Words too common to carry identity. Deliberately tiny — this is
/// noise reduction for short technical bullets, not English NLP.
const STOPWORDS: [&str; 24] = [
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "in", "is", "it", "of",
    "on", "or", "that", "the", "then", "this", "to", "with", "still",
];

/// Split a section body into items. Recognises `- `, `* ` and `1. `
/// markers; lines that continue an item are folded into it, and prose
/// before the first marker is ignored (frames often open `## Next` with
/// a framing paragraph — see `311ec4b7`).
pub fn bullet_items(body: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let is_marker = t.starts_with("- ")
            || t.starts_with("* ")
            || t.split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if is_marker {
            items.push(t.to_string());
        } else if !t.is_empty() {
            if let Some(last) = items.last_mut() {
                last.push(' ');
                last.push_str(t);
            }
        }
    }
    items
}

/// An item's identity: lowercased content words, markdown and
/// punctuation stripped, stopwords dropped.
fn content_words(item: &str) -> Vec<String> {
    item.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .map(|w| w.trim_matches('.').to_string())
        .filter(|w| w.len() > 1 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// True when two `## Next` items name the same piece of work.
///
/// Uses the OVERLAP coefficient (`|A∩B| / min(|A|,|B|)`), not Jaccard,
/// because the real failure mode is an item that gets *elaborated* as it
/// is carried rather than reworded. Jaccard punishes that: the block-split
/// item went from "Retire `SOVEREIGN_RPC_BLOCK_SPLIT=12,36` — needs
/// BeefyMac" to a 22-word version, scoring 0.22 by Jaccard and 0.83 by
/// overlap. Overlap says "one of these is contained in the other", which
/// is exactly the question.
///
/// Biased toward under-reporting: a missed carry costs nothing, a false
/// one trains agents to ignore the signal.
pub fn items_match(a: &str, b: &str) -> bool {
    // Sets on BOTH sides: a repeated word must not inflate either the
    // overlap or the denominator.
    let sa: std::collections::BTreeSet<String> = content_words(a).into_iter().collect();
    let sb: std::collections::BTreeSet<String> = content_words(b).into_iter().collect();
    // Too short to have an identity — two three-word bullets can collide
    // by accident, and a false positive is the expensive error here.
    if sa.len() < 3 || sb.len() < 3 {
        return false;
    }
    let hits = sa.intersection(&sb).count();
    hits * 10 >= sa.len().min(sb.len()) * 6
}

/// Items in `next_body` that CONSECUTIVE ancestors were also carrying,
/// as `(item, depth)`, worst first. `ancestor_next_bodies` is nearest
/// ancestor first.
///
/// CONSECUTIVE is the load-bearing word. An item that appeared, was
/// dropped, and came back is a re-prioritisation — legitimate, and
/// precisely the behaviour this wants to encourage. Flagging it would
/// punish the cure.
///
/// Lives here, not in either caller, because the frame WRITER and the
/// boot-time READER must agree exactly: an advisory that changes its mind
/// between "what you were told at boot" and "what you are told on write"
/// is worse than no advisory.
pub fn carried_across(next_body: &str, ancestor_next_bodies: &[String]) -> Vec<(String, usize)> {
    let ancestors: Vec<Vec<String>> = ancestor_next_bodies
        .iter()
        .map(|b| bullet_items(b))
        .collect();
    let mut out: Vec<(String, usize)> = Vec::new();
    for item in bullet_items(next_body) {
        let depth = ancestors
            .iter()
            .take_while(|theirs| theirs.iter().any(|t| items_match(&item, t)))
            .count();
        if depth > 0 {
            out.push((item, depth));
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// How many consecutive frames (this one included) say the same thing in
/// a section. `0` when `body` is blank; `1` means fresh or just changed.
pub fn same_across(body: &str, ancestor_bodies: &[String]) -> usize {
    if body.trim().is_empty() {
        return 0;
    }
    1 + ancestor_bodies
        .iter()
        .take_while(|theirs| !theirs.trim().is_empty() && items_match(body, theirs))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: FrameSchema = FrameSchema {
        schema_id: "test-frame/v1",
        sections: &["Topics", "Entities", "Open threads"],
        token_budget: 100,
    };

    // ── Carried-item detection ───────────────────────────────────────
    // The fixtures below are VERBATIM from the frames that motivated
    // this: `8815fdb9` and `c96d55a6`, banked on RuggedFox 2026-07-29.
    // Synthetic fixtures would have let a too-strict matcher pass.

    /// `## Next` bodies open with framing prose often enough (`311ec4b7`)
    /// that swallowing it as an item would poison every comparison.
    #[test]
    fn bullet_items_folds_continuations_and_ignores_leading_prose() {
        let body = "**THE STRUCTURAL GAP (verified this session):** measurements never\n\
                    leave the machine that took them.\n\
                    \n\
                    1. **Make a measurement travel.** (a) gossip records peer-to-peer\n\
                    (b) a published corpus keyed by hw fingerprints.\n\
                    2. Retire the block-split pin.\n\
                    - a dash item too\n";
        let items = bullet_items(body);
        assert_eq!(items.len(), 3, "leading prose is not an item: {items:?}");
        assert!(
            items[0].contains("gossip records peer-to-peer")
                && items[0].contains("published corpus"),
            "continuation lines fold into their item: {:?}",
            items[0]
        );
        assert!(items[2].starts_with("- a dash item"));
    }

    /// The real failure mode is an item that gets ELABORATED as it is
    /// carried, not reworded. This exact pair rode `8815fdb9` and
    /// `c96d55a6`; Jaccard scores it 0.22 and would miss it entirely.
    #[test]
    fn an_elaborated_item_still_matches_its_earlier_self() {
        let earlier = "4. Retire `SOVEREIGN_RPC_BLOCK_SPLIT=12,36` — needs BeefyMac.";
        let later = "5. Retire `SOVEREIGN_RPC_BLOCK_SPLIT=12,36` — `mesh plan` on the 35B \
                     reports the pin does NOT apply (needs 41 blocks) and the loader \
                     rejects it too.";
        assert!(items_match(earlier, later), "elaboration must still match");
        assert!(items_match(later, earlier), "and the relation is symmetric");
    }

    #[test]
    fn a_verbatim_recopy_matches() {
        let a = "3. WorkerOverflow capacity basis (note cc8d033f) — park on `total`, \
                 retry on `free`.";
        assert!(items_match(a, a));
    }

    /// A false positive is the expensive error: it trains agents to
    /// ignore the signal. Two unrelated items that share filler must not
    /// collide, and neither must two items too short to have identity.
    /// CONSECUTIVE is load-bearing: a gap in the chain means the item was
    /// dropped and revived, which is re-prioritisation — the behaviour
    /// this feature exists to ENCOURAGE, not flag.
    #[test]
    fn carried_across_counts_only_consecutive_ancestors() {
        let item = "- WorkerOverflow capacity basis (note cc8d033f) — park on `total`, retry \
                    on `free`.";
        let other = "- something else entirely, concerning TLS certificate rotation";

        let unbroken = carried_across(item, &[item.to_string(), item.to_string()]);
        assert_eq!(unbroken.len(), 1);
        assert_eq!(unbroken[0].1, 2, "two consecutive ancestors");

        let broken = carried_across(item, &[other.to_string(), item.to_string()]);
        assert!(
            broken.is_empty(),
            "the nearest ancestor dropped it, so this is a revival: {broken:?}"
        );
    }

    /// Worst first, so a renderer can name the oldest without sorting.
    #[test]
    fn carried_across_reports_worst_first() {
        let shallow = "- Cross-machine hop is UNOBSERVED, BeefyMac offline, needs the new build";
        let deep = "- WorkerOverflow capacity basis (note cc8d033f) — park on `total`, retry \
                    on `free`.";
        let out = carried_across(
            &format!("{shallow}\n{deep}"),
            &[format!("{shallow}\n{deep}"), deep.to_string()],
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].1, 2, "the deeper item leads: {out:?}");
        assert!(out[0].0.contains("WorkerOverflow"));
    }

    #[test]
    fn same_across_counts_the_frame_itself_and_resets_on_change() {
        let obj = "- Mesh users get a trustworthy speed number before committing hardware.";
        let other = "- Something entirely different about the desktop installer surface.";
        assert_eq!(
            same_across(obj, &[]),
            1,
            "no ancestors, but this frame counts"
        );
        assert_eq!(same_across(obj, &[obj.to_string(), obj.to_string()]), 3);
        assert_eq!(same_across(obj, &[other.to_string()]), 1, "a change resets");
        assert_eq!(
            same_across("   ", &[obj.to_string()]),
            0,
            "blank is not a streak"
        );
    }

    #[test]
    fn unrelated_items_do_not_collide() {
        assert!(!items_match(
            "- Fix the flaky test in `scheduler_core`",
            "- Fix the daemon restart race"
        ));
        assert!(
            !items_match("- commit it", "- commit it"),
            "too short to carry identity, even when identical"
        );
    }

    #[test]
    fn round_trips_sections_and_unknown_frontmatter() {
        let mut f = SCHEMA.empty();
        f.set("schema", SCHEMA.schema_id.to_string());
        f.set("some_future_key", "kept".to_string());
        assert!(f.set_body("Topics", "polonium; radium".into()));

        let reparsed = SCHEMA.parse(&f.render());
        // `body` returns the raw span between headings — blank lines
        // included; `render` re-trims, so a round-trip is stable.
        assert_eq!(
            reparsed.body("Topics").map(str::trim),
            Some("polonium; radium")
        );
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
        assert_eq!(
            SCHEMA.canonical_section("open_threads"),
            Some("Open threads")
        );
        assert_eq!(
            SCHEMA.canonical_section("OPEN THREADS"),
            Some("Open threads")
        );
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
        assert!(
            !p.contains("Entities"),
            "empty sections cost tokens for nothing"
        );
        assert!(!p.contains("schema"), "frontmatter is storage, not prompt");
        assert!(SCHEMA.empty().render_for_prompt().is_empty());
    }
}
