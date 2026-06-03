//! Pure composer for `sovereign project plan`.
//!
//! Takes a `DesignSignals` snapshot and the parsed state of
//! `OPEN_QUESTIONS.md`; returns a list of plan items (one per
//! phase-scoped checklist entry) and the rendered
//! `IMPLEMENTATION_PLAN.md` content. No IO — the caller in
//! `project_cmd.rs::cmd_plan` wires this to `plan_items` writes,
//! disk writes, and `ProjectDocsStore::index_file`.
//!
//! ## Composition rules
//!
//! 1. **Phase 0 is always present.** Skeleton — "cargo test / npm
//!    build / go test / pytest" (picked from the language in the
//!    observation). Realizes `§Anchors` conceptually because
//!    skeleton work puts the anchors into running code.
//! 2. **Phases 1..N come from non-Anchors, non-Open-questions
//!    sections in DESIGN.md, in document order.** Each phase's
//!    `realizes` anchor points back at its section.
//! 3. **Unanswered OPEN_QUESTIONS.md entries attach as open risks**
//!    on the phase whose section anchor matches the OQ's anchor.
//!    Answered OQs are noted on the phase as resolved risks with
//!    the answer quoted, so future readers can see how the
//!    question cleared.
//! 4. **Plan item ids are stable slugs:** `plan.phase-<n>.<slug>`.
//!    Regenerating the same DESIGN.md + OPEN_QUESTIONS.md produces
//!    byte-identical output and byte-identical ids — callers can
//!    diff confidently.
//!
//! ## What this module does NOT own
//!
//! - Reading DESIGN.md / OPEN_QUESTIONS.md (caller does the IO).
//! - Writing IMPLEMENTATION_PLAN.md (caller writes).
//! - Persisting to plan.db (caller wires `PlanStore::upsert`).
//! - Phase-pass stop-condition verification (that's `project phase
//!   pass N` in a separate command).

use corpus_engine_atos::design_signals::DesignSignals;

// ─── Types ─────────────────────────────────────────────────────────

/// A single parsed entry from `OPEN_QUESTIONS.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenQuestionEntry {
    /// `oq.<slug>.<n>`.
    pub id: String,
    pub question: String,
    /// `"DESIGN.md §<section>"` or similar anchor string.
    pub anchor: String,
    /// Non-empty when the user has written an answer.
    pub answer: String,
}

impl OpenQuestionEntry {
    pub fn is_answered(&self) -> bool {
        !self.answer.trim().is_empty()
    }
    /// Extract the bare section label from `anchor`. Best-effort —
    /// handles the common `"DESIGN.md §Data & interfaces"` shape.
    pub fn section_from_anchor(&self) -> &str {
        self.anchor
            .split('§')
            .nth(1)
            .map(|s| s.trim())
            .unwrap_or("")
    }
}

/// Composed plan-item data ready to upsert into `plan.db`. Shape
/// intentionally mirrors `corpus_engine_atos::plan_items::PlanItem` so
/// the caller's conversion is a thin shim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedPlanItem {
    pub id: String,
    pub phase: u32,
    pub title: String,
    pub body: String,
    pub realizes: Option<String>,
    pub stop_hint: Option<String>,
    /// Open risks attached to this phase: unanswered OQs whose
    /// anchor points at this phase's section.
    pub open_risks: Vec<OpenQuestionEntry>,
    /// Resolved risks: answered OQs for the same section.
    pub resolved_risks: Vec<OpenQuestionEntry>,
}

/// Output of [`compose_plan`].
pub struct ComposedPlan {
    pub items: Vec<ComposedPlanItem>,
    /// Markdown rendering, suitable for writing to
    /// `<repo>/IMPLEMENTATION_PLAN.md`.
    pub markdown: String,
    /// SHA-256 of the DESIGN.md input (truncated to 12 hex chars)
    /// that produced this plan. Recorded in plan_items rows so stale
    /// ones can be `defer_stale()`'d on regeneration.
    pub design_hash: String,
}

/// Inputs to [`compose_plan`].
pub struct ComposeInputs<'a> {
    pub project_id: &'a str,
    pub design_md: &'a str,
    pub signals: &'a DesignSignals,
    pub open_questions: &'a [OpenQuestionEntry],
    /// Language id (from observation) — used to pick Phase 0's stop
    /// condition. `None` yields a generic "run your tests" hint.
    pub primary_language: Option<&'a str>,
    /// Human-readable date (e.g. `"2026-04-22"`). Kept as a param so
    /// tests can assert on a stable date without freezing system time.
    pub today: &'a str,
}

// ─── Entry point ───────────────────────────────────────────────────

pub fn compose_plan(input: &ComposeInputs<'_>) -> ComposedPlan {
    let design_hash = short_hash(input.design_md);
    let items = build_items(input);
    let markdown = render_markdown(input, &items, &design_hash);
    ComposedPlan {
        items,
        markdown,
        design_hash,
    }
}

// ─── Item construction ─────────────────────────────────────────────

const ANCHORS_HEADING_LC: &str = "anchors";
const OPEN_QUESTIONS_HEADING_LC: &str = "open questions";

fn build_items(input: &ComposeInputs<'_>) -> Vec<ComposedPlanItem> {
    let mut items = Vec::new();

    // Phase 0 — Skeleton. Always fires.
    items.push(compose_phase_zero(input));

    // Phases 1..N from H2 sections in document order, skipping
    // Anchors and Open questions (those are metadata sections, not
    // implementation phases). H1 is the project title; H3+ roll up
    // into their parent H2's body via DesignSignals' section list.
    let mut phase_counter: u32 = 1;
    for section in &input.signals.sections {
        if section.level != 2 {
            continue;
        }
        let lower = section.heading.to_lowercase();
        if lower == ANCHORS_HEADING_LC || lower == OPEN_QUESTIONS_HEADING_LC {
            continue;
        }

        let slug = slugify(&section.heading);
        let id = format!("plan.phase-{phase_counter}.{slug}");
        let realizes = format!("DESIGN.md §{}", section.heading);

        let (open_risks, resolved_risks) =
            split_risks_for_section(input.open_questions, &section.heading);

        let body = derive_section_body_summary(&section.body);

        items.push(ComposedPlanItem {
            id,
            phase: phase_counter,
            title: section.heading.clone(),
            body,
            realizes: Some(realizes),
            stop_hint: None, // user fills this in or `project phase pass` prompts for it
            open_risks,
            resolved_risks,
        });
        phase_counter += 1;
    }

    items
}

fn compose_phase_zero(input: &ComposeInputs<'_>) -> ComposedPlanItem {
    let stop = phase_zero_stop_condition(input.primary_language);
    // Even Phase 0 surfaces risks if OQs are anchored to §Anchors —
    // uncommon but useful when the anchors themselves are under-
    // specified.
    let (open_risks, resolved_risks) = split_risks_for_section(input.open_questions, "Anchors");
    ComposedPlanItem {
        id: "plan.phase-0.skeleton".to_string(),
        phase: 0,
        title: "Skeleton".to_string(),
        body: "Scaffolding only — the project builds, tests pass, linter is clean, dependencies resolve. No real features yet; just the shape of the thing."
            .to_string(),
        realizes: Some("DESIGN.md §Anchors".to_string()),
        stop_hint: Some(stop),
        open_risks,
        resolved_risks,
    }
}

/// Language-specific stop condition. Mirrors the logic used in
/// `found.rs:1337-1352` so `project plan` and `project found` agree.
fn phase_zero_stop_condition(lang: Option<&str>) -> String {
    match lang {
        Some("rust") => "cargo build && cargo test".into(),
        Some("go") => "go build ./... && go test ./...".into(),
        Some("typescript") | Some("javascript") => "npm run build && npm test".into(),
        Some("python") => "python -m pytest".into(),
        Some("java") => "mvn test".into(),
        _ => "run your language's build + test command; it must pass clean".into(),
    }
}

/// Split OQs into (open-for-section, resolved-for-section). Matching
/// is by section label (§heading). Conservative: unmatched OQs stay
/// with the "Open questions" top-level section and don't spuriously
/// attach to arbitrary phases.
fn split_risks_for_section(
    oqs: &[OpenQuestionEntry],
    section_label: &str,
) -> (Vec<OpenQuestionEntry>, Vec<OpenQuestionEntry>) {
    let target = section_label.to_lowercase();
    let mut open = Vec::new();
    let mut resolved = Vec::new();
    for oq in oqs {
        let oq_section = oq.section_from_anchor().to_lowercase();
        if oq_section == target {
            if oq.is_answered() {
                resolved.push(oq.clone());
            } else {
                open.push(oq.clone());
            }
        }
    }
    (open, resolved)
}

fn derive_section_body_summary(body: &str) -> String {
    // Plan-item body is a short description rendered into the
    // markdown + persisted as `body` in plan.db. Take the first
    // non-empty, non-HTML-comment line of the DESIGN.md section
    // body; fall back to "(no description)" when the section is
    // empty — a valid state that triggers an open-risk elsewhere.
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("<!--") {
            continue;
        }
        // Truncate hard — plan-item bodies shouldn't be paragraphs.
        return truncate(t, 200);
    }
    "(no description)".to_string()
}

// ─── Markdown rendering ────────────────────────────────────────────

/// Render the plan markdown given a (possibly post-composition-mutated)
/// item list. `cmd_plan` calls this directly after the inference
/// enrichment pass mutates `body` / `stop_hint` on each item.
pub fn render(input: &ComposeInputs<'_>, items: &[ComposedPlanItem], design_hash: &str) -> String {
    render_markdown(input, items, design_hash)
}

fn render_markdown(
    input: &ComposeInputs<'_>,
    items: &[ComposedPlanItem],
    design_hash: &str,
) -> String {
    let open_count: usize = input
        .open_questions
        .iter()
        .filter(|o| !o.is_answered())
        .count();
    let answered_count: usize = input
        .open_questions
        .iter()
        .filter(|o| o.is_answered())
        .count();

    let mut out = String::new();
    out.push_str(&format!("# {} — Implementation plan\n", input.project_id));
    out.push_str(&format!(
        "_Generated: {} · DESIGN.md sha=`{}` · OPEN_QUESTIONS: {answered_count} answered / {open_count} open_\n",
        input.today, design_hash
    ));
    out.push_str(
        "\n<!-- Regenerate with `sovereign project plan`. Blocks you want\n\
             to keep across regenerations can be wrapped in `<!-- keep -->`\n\
             markers (not yet wired; safe to author ahead of the feature). -->\n",
    );

    for item in items {
        out.push('\n');
        out.push_str(&format!("## Phase {} · {}\n", item.phase, item.title));
        if let Some(r) = &item.realizes {
            out.push_str(&format!("_Realizes: {r}_\n"));
        }
        out.push('\n');
        if !item.body.is_empty() {
            out.push_str(&item.body);
            out.push_str("\n\n");
        }

        if !item.open_risks.is_empty() {
            out.push_str("**Open risks:**\n");
            for risk in &item.open_risks {
                out.push_str(&format!(
                    "- `{}` — {}\n",
                    risk.id,
                    truncate(&risk.question, 160)
                ));
            }
            out.push('\n');
        }
        if !item.resolved_risks.is_empty() {
            out.push_str("**Resolved (for the record):**\n");
            for risk in &item.resolved_risks {
                out.push_str(&format!(
                    "- `{}` — {} → {}\n",
                    risk.id,
                    truncate(&risk.question, 100),
                    truncate(&risk.answer, 120)
                ));
            }
            out.push('\n');
        }

        if let Some(stop) = &item.stop_hint {
            out.push_str(&format!("**Stop condition:** `{stop}`\n"));
        } else {
            out.push_str(
                "**Stop condition:** _(fill this in — what proves this phase is done?)_\n",
            );
        }
    }

    // Orphaned OQs: entries whose section label didn't match any
    // phase. Surfacing them at the bottom keeps the plan honest —
    // "here are questions that don't map to a phase; consider
    // re-anchoring or clarifying which section owns them."
    let orphaned = find_orphaned_oqs(input.open_questions, items);
    if !orphaned.is_empty() {
        out.push_str("\n## Unanchored open questions\n\n");
        out.push_str(
            "These `OPEN_QUESTIONS.md` entries didn't match any phase by section label.\n\
             Either re-anchor them in DESIGN.md or clarify which phase owns them.\n\n",
        );
        for oq in orphaned {
            out.push_str(&format!(
                "- `{}` · {} · {}\n",
                oq.id,
                oq.anchor,
                truncate(&oq.question, 140)
            ));
        }
    }

    out
}

fn find_orphaned_oqs<'a>(
    oqs: &'a [OpenQuestionEntry],
    items: &[ComposedPlanItem],
) -> Vec<&'a OpenQuestionEntry> {
    let covered: std::collections::BTreeSet<String> = items
        .iter()
        .map(|i| i.title.to_lowercase())
        // Phase 0 owns the §Anchors label.
        .chain(std::iter::once("anchors".to_string()))
        .collect();
    oqs.iter()
        .filter(|o| {
            !o.is_answered() && !covered.contains(o.section_from_anchor().to_lowercase().as_str())
        })
        .collect()
}

// ─── OPEN_QUESTIONS.md parser ──────────────────────────────────────

/// Parse `OPEN_QUESTIONS.md` into a list of entries. Lenient — the
/// user can freely edit answers, add narrative between entries, etc.
/// The parser only recognizes `### <id>` headings and the three
/// known field labels.
pub fn parse_open_questions(md: &str) -> Vec<OpenQuestionEntry> {
    let mut out = Vec::new();
    let mut current: Option<OpenQuestionEntry> = None;
    let mut reading_answer = false;

    for raw in md.lines() {
        let line = raw;
        if let Some(h3) = line.strip_prefix("### ") {
            // Flush the previous entry.
            if let Some(prev) = current.take() {
                out.push(prev);
            }
            let id = h3.trim().to_string();
            current = Some(OpenQuestionEntry {
                id,
                question: String::new(),
                anchor: String::new(),
                answer: String::new(),
            });
            reading_answer = false;
            continue;
        }
        if line.starts_with("---") || line.starts_with("## ") || line.starts_with("# ") {
            // Section boundary; close the current entry.
            if let Some(prev) = current.take() {
                out.push(prev);
            }
            reading_answer = false;
            continue;
        }

        let Some(entry) = current.as_mut() else {
            continue;
        };

        // Field detection: `**Q:**`, `**Anchor:**`, `**Answer:**`.
        if let Some(rest) = strip_field(line, "Q:") {
            entry.question = rest.trim().to_string();
            reading_answer = false;
        } else if let Some(rest) = strip_field(line, "Anchor:") {
            entry.anchor = rest.trim().to_string();
            reading_answer = false;
        } else if strip_field(line, "Answer:").is_some() {
            // Answer is the content AFTER this line (multi-line is
            // valid and common), so flip into "collecting answer"
            // mode until another field or section boundary.
            reading_answer = true;
        } else if reading_answer {
            // Skip italic provenance footers the solo writer
            // appends (e.g. `_Captured by ... session `<id>` · date_`).
            let t = line.trim();
            if t.starts_with('_') && t.ends_with('_') && t.contains("Captured by") {
                continue;
            }
            if !entry.answer.is_empty() {
                entry.answer.push('\n');
            }
            entry.answer.push_str(line);
        }
    }
    if let Some(prev) = current.take() {
        out.push(prev);
    }
    // Trim answers so trailing blank lines don't count as "answered".
    for oq in &mut out {
        oq.answer = oq.answer.trim().to_string();
    }
    out
}

fn strip_field<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    // Match `**<label>** rest` with the bold markers and the
    // colon already part of `<label>`. Cheap, precise for the solo
    // writer's output shape.
    let bold = format!("**{}**", label);
    let trimmed = line.trim_start();
    trimmed.strip_prefix(&bold)
}

// ─── Small helpers ─────────────────────────────────────────────────

pub fn short_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    let bytes = h.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(12);
    for b in bytes.iter().take(6) {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("section");
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let mut out: String = chars.into_iter().take(max - 1).collect();
        out.push('…');
        out
    }
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_atos::design_signals;

    fn inputs_from<'a>(
        design_md: &'a str,
        oqs: &'a [OpenQuestionEntry],
        lang: Option<&'a str>,
        today: &'a str,
    ) -> (DesignSignals, ComposeInputs<'a>) {
        let signals = design_signals::extract(design_md);
        let sig_ref: &'a DesignSignals = Box::leak(Box::new(signals));
        let inputs = ComposeInputs {
            project_id: "probe",
            design_md,
            signals: sig_ref,
            open_questions: oqs,
            primary_language: lang,
            today,
        };
        // Return a clone of the signals too so tests can inspect it
        // if they want. (Cheap Clone on the snapshot.)
        (sig_ref.clone(), inputs)
    }

    #[test]
    fn parse_open_questions_handles_solo_format() {
        let md = r#"# Open questions

Stuff.

---

### oq.data-interfaces.1
**Q:** What's the wire format?
**Anchor:** DESIGN.md §Data & interfaces
**Answer:**
JSON with a versioned envelope.

_Captured by `sovereign project design --solo` · session `design-123` · 2026-04-22_

---

### oq.anchors.1
**Q:** Persistence?
**Anchor:** DESIGN.md §Anchors
**Answer:**
"#;
        let parsed = parse_open_questions(md);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "oq.data-interfaces.1");
        assert_eq!(parsed[0].question, "What's the wire format?");
        assert_eq!(parsed[0].anchor, "DESIGN.md §Data & interfaces");
        assert!(parsed[0]
            .answer
            .starts_with("JSON with a versioned envelope."));
        assert!(parsed[0].is_answered());

        assert_eq!(parsed[1].id, "oq.anchors.1");
        assert!(!parsed[1].is_answered(), "blank answer => unanswered");
    }

    #[test]
    fn section_from_anchor_extracts_label() {
        let oq = OpenQuestionEntry {
            id: "a".into(),
            question: "?".into(),
            anchor: "DESIGN.md §Data & interfaces".into(),
            answer: String::new(),
        };
        assert_eq!(oq.section_from_anchor(), "Data & interfaces");
    }

    #[test]
    fn phase_zero_is_always_present_with_language_specific_stop() {
        let md = "# p — Design\n\n## Anchors\n\n- Language: Rust\n";
        let (_sig, inputs) = inputs_from(md, &[], Some("rust"), "2026-04-22");
        let plan = compose_plan(&inputs);
        assert_eq!(plan.items[0].phase, 0);
        assert_eq!(plan.items[0].title, "Skeleton");
        assert_eq!(
            plan.items[0].stop_hint.as_deref(),
            Some("cargo build && cargo test")
        );
    }

    #[test]
    fn phase_zero_stop_falls_back_when_language_unknown() {
        let md = "# p\n\n## Anchors\n\n- TBD\n";
        let (_s, i) = inputs_from(md, &[], None, "d");
        let plan = compose_plan(&i);
        let stop = plan.items[0].stop_hint.as_deref().unwrap();
        assert!(stop.contains("run your language's"));
    }

    #[test]
    fn phases_come_from_h2_sections_in_order_skipping_meta_headings() {
        let md = "# p — Design\n\n## Anchors\n\n- L: Rust\n\n## Ingest\n\ndo ingest.\n\n## Storage\n\ndo storage.\n\n## Open questions\n\n- TBD\n";
        let (_s, i) = inputs_from(md, &[], Some("rust"), "d");
        let plan = compose_plan(&i);
        // Expect phases 0, 1, 2 — with Ingest as 1, Storage as 2.
        let phases: Vec<(u32, &str)> = plan
            .items
            .iter()
            .map(|it| (it.phase, it.title.as_str()))
            .collect();
        assert_eq!(phases, vec![(0, "Skeleton"), (1, "Ingest"), (2, "Storage")]);
    }

    #[test]
    fn unanswered_oq_attaches_as_open_risk() {
        let md = "# p\n\n## Ingest\n\nbody.\n";
        let oq = OpenQuestionEntry {
            id: "oq.ingest.1".into(),
            question: "Wire format?".into(),
            anchor: "DESIGN.md §Ingest".into(),
            answer: String::new(),
        };
        let (_s, i) = inputs_from(md, std::slice::from_ref(&oq), Some("rust"), "d");
        let plan = compose_plan(&i);
        let ingest_phase = plan
            .items
            .iter()
            .find(|p| p.title == "Ingest")
            .expect("ingest");
        assert_eq!(ingest_phase.open_risks.len(), 1);
        assert_eq!(ingest_phase.open_risks[0].id, "oq.ingest.1");
    }

    #[test]
    fn answered_oq_moves_to_resolved_bucket() {
        let md = "# p\n\n## Ingest\n\nbody.\n";
        let oq = OpenQuestionEntry {
            id: "oq.ingest.1".into(),
            question: "Wire format?".into(),
            anchor: "DESIGN.md §Ingest".into(),
            answer: "gRPC".into(),
        };
        let (_s, i) = inputs_from(md, std::slice::from_ref(&oq), Some("rust"), "d");
        let plan = compose_plan(&i);
        let ingest_phase = plan.items.iter().find(|p| p.title == "Ingest").unwrap();
        assert!(ingest_phase.open_risks.is_empty());
        assert_eq!(ingest_phase.resolved_risks.len(), 1);
    }

    #[test]
    fn orphaned_oq_surfaces_in_dedicated_section() {
        let md = "# p\n\n## Ingest\n\nbody.\n";
        let oq = OpenQuestionEntry {
            id: "oq.misc.1".into(),
            question: "What about X?".into(),
            anchor: "DESIGN.md §Misc".into(),
            answer: String::new(),
        };
        let (_s, i) = inputs_from(md, std::slice::from_ref(&oq), Some("rust"), "d");
        let plan = compose_plan(&i);
        assert!(
            plan.markdown.contains("Unanchored open questions"),
            "orphan section missing from: {}",
            plan.markdown
        );
        assert!(plan.markdown.contains("oq.misc.1"));
    }

    #[test]
    fn plan_item_ids_are_stable_slugs() {
        let md = "# p\n\n## Data & Interfaces\n\nbody.\n";
        let (_s, i) = inputs_from(md, &[], Some("rust"), "d");
        let plan = compose_plan(&i);
        let ids: Vec<&str> = plan.items.iter().map(|it| it.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["plan.phase-0.skeleton", "plan.phase-1.data-interfaces"]
        );
    }

    #[test]
    fn rendered_markdown_has_header_and_all_phases() {
        let md = "# p — Design\n\n## Anchors\n\n- L: Rust\n\n## Build\n\nstuff.\n";
        let (_s, i) = inputs_from(md, &[], Some("rust"), "2026-04-22");
        let plan = compose_plan(&i);
        assert!(plan.markdown.starts_with("# probe — Implementation plan"));
        assert!(plan.markdown.contains("Generated: 2026-04-22"));
        assert!(plan.markdown.contains("Phase 0 · Skeleton"));
        assert!(plan.markdown.contains("Phase 1 · Build"));
    }

    #[test]
    fn open_count_in_header_reflects_answered_state() {
        let oqs = vec![
            OpenQuestionEntry {
                id: "oq.x.1".into(),
                question: "?".into(),
                anchor: "DESIGN.md §Ingest".into(),
                answer: String::new(),
            },
            OpenQuestionEntry {
                id: "oq.x.2".into(),
                question: "?".into(),
                anchor: "DESIGN.md §Ingest".into(),
                answer: "yes".into(),
            },
        ];
        let md = "# p\n\n## Ingest\n\nbody.\n";
        let (_s, i) = inputs_from(md, &oqs, Some("rust"), "d");
        let plan = compose_plan(&i);
        assert!(plan.markdown.contains("1 answered / 1 open"));
    }
}
