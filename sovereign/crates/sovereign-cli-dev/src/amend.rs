//! `sovereign project amend` — the post-founding charter edit flow.
//!
//! ## Shape
//!
//! Founding is once; amendment is repeatable. The discipline
//! (from the requirements) is:
//!
//! 1. User opens `CHARTER.md` in `$EDITOR` (we facilitate).
//! 2. We diff the old charter against the edited version,
//!    section-by-section.
//! 3. For each section that changed, we play adversarial reviewer:
//!    we ask "what about X?" — surfacing downstream assumptions,
//!    invariants that might be violated, context that led to the
//!    prior choice. The user answers.
//! 4. We show the user the composed amendment-log entry
//!    (their edits + the adversarial Q&A) and ask for explicit
//!    approval.
//! 5. On approval: append the entry to `## Amendment log`,
//!    write CHARTER.md back, bump `charter_version`, record the
//!    new hash, drop a `decision`-kind note with the full Q&A so
//!    future sessions can read why the amendment went through
//!    despite the named risks.
//!
//! ## Adversarial review without an LLM
//!
//! The requirements describe the system "arguing against" the
//! amendment. For M6.7 v1 we do this with a curated template
//! catalog keyed on which section changed. The questions are
//! genuinely useful — "what downstream code assumes the old
//! invariant?" is a real question worth answering — without
//! inventing domain knowledge we don't have. When the Fast-slot
//! path is available (future work), the catalog becomes seed
//! material the LLM elaborates on; for now the catalog is the
//! whole review.
//!
//! ## Drift detection
//!
//! If the on-disk `CHARTER.md` doesn't hash to
//! `lifecycle.charter_hash`, someone edited outside the amend
//! flow. The amend command surfaces this explicitly and asks the
//! user whether the existing drift is part of the pending
//! amendment or should be reverted first. Mirrors the ATOS spec
//! drift philosophy: warn, name, let the human decide.

#![allow(dead_code)]

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::found::{hash_charter, stdin_read_line};

// ─── Section parsing ─────────────────────────────────────────────────────────

/// A parsed CHARTER.md, bucketed by known top-level sections.
/// Unknown headings go into `extras` — we don't drop content we
/// don't recognize.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharterSections {
    /// Everything before the first `##` heading (title + metadata).
    pub preamble: String,
    pub system_design: String,
    pub invariants: String,
    pub resolved_decisions: String,
    pub open_questions: String,
    pub amendment_log: String,
    /// Sections the parser didn't recognize, keyed by their exact
    /// heading text (without the `## ` prefix).
    pub extras: Vec<(String, String)>,
}

/// Split a charter into sections. Recognizes the five headings
/// produced by [`crate::found::compose_charter`]; unknown `##`
/// headings are captured in `extras` so round-tripping is lossless.
pub fn parse_charter_sections(md: &str) -> CharterSections {
    let mut s = CharterSections::default();
    let mut current_heading: Option<String> = None;
    let mut current_body = String::new();

    let flush = |s: &mut CharterSections, heading: Option<&str>, body: String| {
        // Trim trailing blank lines so identical sources parse
        // to identical sections regardless of whether they had a
        // final `\n\n` or just `\n`. Without this the round-trip
        // is sensitive to end-of-file whitespace.
        let body = body.trim_end().to_string() + "\n";
        let body = if body.trim().is_empty() {
            String::new()
        } else {
            body
        };
        match heading {
            None => s.preamble = body,
            Some(h) => {
                let normalized = h.trim().to_lowercase();
                match normalized.as_str() {
                    "system design" => s.system_design = body,
                    "invariants" => s.invariants = body,
                    "resolved decisions" => s.resolved_decisions = body,
                    "open questions" => s.open_questions = body,
                    "amendment log" => s.amendment_log = body,
                    _ => s.extras.push((h.to_string(), body)),
                }
            }
        }
    };

    for raw in md.lines() {
        if let Some(rest) = raw.strip_prefix("## ") {
            let body = std::mem::take(&mut current_body);
            flush(&mut s, current_heading.as_deref(), body);
            current_heading = Some(rest.to_string());
        } else {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(raw);
        }
    }
    // Final flush for whatever was pending at EOF.
    let body = std::mem::take(&mut current_body);
    flush(&mut s, current_heading.as_deref(), body);
    s
}

/// Which top-level sections changed between two parses. Returns
/// the canonical ids (matching the catalog's `section_id` keys)
/// in a stable order.
pub fn changed_sections(old: &CharterSections, new: &CharterSections) -> Vec<&'static str> {
    let mut out = Vec::new();
    // Deliberately ignore `preamble` (title/metadata drift is
    // cosmetic) and `amendment_log` (it's the target of this
    // amendment, not its subject).
    if normalize(&old.system_design) != normalize(&new.system_design) {
        out.push("system-design");
    }
    if normalize(&old.invariants) != normalize(&new.invariants) {
        out.push("invariants");
    }
    if normalize(&old.resolved_decisions) != normalize(&new.resolved_decisions) {
        out.push("resolved-decisions");
    }
    if normalize(&old.open_questions) != normalize(&new.open_questions) {
        out.push("open-questions");
    }
    out
}

fn normalize(section: &str) -> String {
    // Trim trailing whitespace per line + collapse trailing blank
    // lines so whitespace-only edits don't register as changes.
    section
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

// ─── Adversarial catalog ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdversarialQuestion {
    pub id: String,
    pub section: String,
    pub prompt: String,
    pub why: String,
}

struct CatalogEntry {
    section_id: &'static str,
    question_id: &'static str,
    prompt: &'static str,
    why: &'static str,
}

fn catalog() -> &'static [CatalogEntry] {
    &[
        CatalogEntry {
            section_id: "system-design",
            question_id: "amend.adv.system-design.downstream",
            prompt: "Which code, tests, or documents reference the old design shape, \
                 and what is your plan to update them?",
            why: "System-design changes ripple further than the charter — \
                 if you don't enumerate the references now, drift emerges \
                 later as confused docs and stale tests.",
        },
        CatalogEntry {
            section_id: "invariants",
            question_id: "amend.adv.invariants.assumers",
            prompt: "Which callers / components assume the OLD invariant? \
                 Are there tests that LOCK it?",
            why: "Invariants aren't just documentation — things are built \
                 ON TOP of them. Changing one without naming the dependents \
                 is how silent regressions happen.",
        },
        CatalogEntry {
            section_id: "invariants",
            question_id: "amend.adv.invariants.replacement",
            prompt: "What's the replacement invariant, stated as strictly as the old one was?",
            why: "A removed invariant with nothing in its place means the \
                 constraint is still there, just undocumented. Name it.",
        },
        CatalogEntry {
            section_id: "resolved-decisions",
            question_id: "amend.adv.decisions.context-change",
            prompt: "What CONTEXT has changed since the original decision was made? \
                 (Not just new preferences — what new evidence, constraint, or requirement?)",
            why: "Reversing a decision without naming what changed is how teams \
                 oscillate. Future-you should be able to read this amendment and \
                 know whether your NEXT context-shift warrants another reversal.",
        },
        CatalogEntry {
            section_id: "resolved-decisions",
            question_id: "amend.adv.decisions.deprecation",
            prompt: "Is there partially-written code or data that still assumes the old decision? \
                 What's the migration plan?",
            why: "A flipped decision with no migration plan produces two systems \
                 running the old and the new rule. Usually one wins silently.",
        },
        CatalogEntry {
            section_id: "open-questions",
            question_id: "amend.adv.open.resolution",
            prompt: "What concrete thing resolved this open question? \
                 (Evidence, spike result, new constraint — not just \"we decided.\")",
            why: "An open question closing without a concrete reason \
                 invites re-opening when the next person hits the same decision point.",
        },
        // ─── DESIGN.md section catalog (step 9) ─────────────────────
        //
        // Charter amendments bump `charter_version`; design
        // amendments don't — DESIGN.md is expected to iterate.
        // These catalog entries fire when the user edits specific
        // DESIGN.md sections (anchors / data-interfaces / open
        // questions). The adversarial engine is the same; only the
        // section ids differ.
        CatalogEntry {
            section_id: "design.anchors",
            question_id: "amend.adv.design.anchors",
            prompt: "Which downstream assumption in DESIGN.md or IMPLEMENTATION_PLAN.md \
                 changes if this anchor is reworded?",
            why: "Anchors are the promises the rest of the doc (and the plan) rests \
                 on — a quiet rewording silently invalidates them.",
        },
        CatalogEntry {
            section_id: "design.data-interfaces",
            question_id: "amend.adv.design.data-interfaces",
            prompt: "What callers already code against this data shape, \
                 and what's your migration plan?",
            why: "Data-shape drift is the #1 source of post-founding reversals. \
                 A new column, a renamed field, a different null semantics — each \
                 is a commitment, not just a doc edit.",
        },
        CatalogEntry {
            section_id: "design.open-questions",
            question_id: "amend.adv.design.open-questions",
            prompt: "Why add this open question now instead of resolving it? \
                 What evidence is missing?",
            why: "An open question deferred without rationale will be deferred \
                 forever. Name the missing evidence so you know when you can close it.",
        },
    ]
}

/// Build the adversarial question set from a list of changed
/// section ids. Questions are returned in catalog order so the
/// user sees a consistent flow. Duplicate section ids collapse —
/// we only ask once per section even if the diff is large.
pub fn questions_for(changed: &[&str]) -> Vec<AdversarialQuestion> {
    let mut out = Vec::new();
    for entry in catalog() {
        if changed.contains(&entry.section_id) {
            out.push(AdversarialQuestion {
                id: entry.question_id.into(),
                section: entry.section_id.into(),
                prompt: entry.prompt.into(),
                why: entry.why.into(),
            });
        }
    }
    out
}

// ─── Interlocutor ────────────────────────────────────────────────────────────

pub trait AmendmentInterlocutor {
    fn ask_adversarial(&mut self, question: &AdversarialQuestion) -> String;
    fn confirm_amendment(&mut self, preview: &str) -> bool;
    /// The user acknowledges that on-disk charter drift (edits
    /// outside `amend`) will be folded into this amendment.
    fn confirm_drift(&mut self, diff_hint: &str) -> bool;
}

pub struct StdinAmendmentInterlocutor;

impl StdinAmendmentInterlocutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdinAmendmentInterlocutor {
    fn default() -> Self {
        Self::new()
    }
}

impl AmendmentInterlocutor for StdinAmendmentInterlocutor {
    fn ask_adversarial(&mut self, q: &AdversarialQuestion) -> String {
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = writeln!(
            stderr,
            "  [{section}] {prompt}",
            section = q.section,
            prompt = q.prompt
        );
        let _ = writeln!(stderr, "      Why: {}", q.why);
        let _ = write!(stderr, "  > ");
        let _ = stderr.flush();
        stdin_read_line()
    }

    fn confirm_amendment(&mut self, preview: &str) -> bool {
        println!();
        println!("  ══════════════════════════════════════════════════════");
        println!("  Amendment-log entry (preview)");
        println!("  ══════════════════════════════════════════════════════");
        println!();
        println!("{preview}");
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = write!(stderr, "  [A]pprove amendment, or [C]ancel? ");
        let _ = stderr.flush();
        matches!(stdin_read_line().to_lowercase().chars().next(), Some('a'))
    }

    fn confirm_drift(&mut self, diff_hint: &str) -> bool {
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = writeln!(
            stderr,
            "  \u{26a0} CHARTER.md on disk doesn't match the recorded charter_hash."
        );
        let _ = writeln!(
            stderr,
            "    That means someone edited it outside `sovereign project amend`."
        );
        let _ = writeln!(stderr, "    {diff_hint}");
        let _ = writeln!(stderr);
        let _ = write!(
            stderr,
            "  Fold these existing edits into this amendment? [y/N] "
        );
        let _ = stderr.flush();
        matches!(stdin_read_line().to_lowercase().chars().next(), Some('y'))
    }
}

// ─── Composition ─────────────────────────────────────────────────────────────

/// A single completed amendment, ready to be rendered into the
/// charter's ## Amendment log and persisted as a decision note.
#[derive(Debug, Clone)]
pub struct AmendmentEntry {
    pub version: u32,
    pub date: String,
    pub changed_sections: Vec<String>,
    pub qa: Vec<(AdversarialQuestion, String)>,
    pub committer: String,
    pub new_charter_hash: String,
}

/// Render the markdown entry that gets appended to
/// `## Amendment log`. Deterministic — same input, same output —
/// so tests can pin the exact shape.
pub fn render_amendment_entry(entry: &AmendmentEntry) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "### Amendment {} — {}\n\n",
        entry.version, entry.date
    ));
    out.push_str(&format!(
        "Changed sections: {}\n\n",
        if entry.changed_sections.is_empty() {
            "_(none — charter edit was cosmetic or preamble-only)_".into()
        } else {
            entry.changed_sections.join(", ")
        },
    ));
    if entry.qa.is_empty() {
        out.push_str("_No adversarial questions fired for this amendment._\n\n");
    } else {
        for (q, a) in &entry.qa {
            out.push_str(&format!("**{}**\n", q.prompt));
            let trimmed = a.trim();
            out.push_str(&format!(
                "_Answer:_ {}\n\n",
                if trimmed.is_empty() {
                    "_(no response given — amendment proceeded without answering this adversarial question)_"
                } else {
                    trimmed
                }
            ));
        }
    }
    out.push_str(&format!(
        "_Committed by: {}. New charter_hash: {}._\n",
        entry.committer, entry.new_charter_hash
    ));
    out
}

/// Compose the final CHARTER.md content: start from the user's
/// edited charter, then append the new amendment entry into
/// `## Amendment log`, preserving any prior entries.
///
/// The parsing-then-rewriting cycle is lossless for recognized
/// sections and round-trips `extras` in their original slot.
pub fn apply_amendment(edited_charter: &str, entry: &AmendmentEntry) -> String {
    let mut sections = parse_charter_sections(edited_charter);

    // Strip the founding placeholder ("_(Empty at founding...") if
    // present. It's supposed to be absent once any real amendment
    // lands.
    let old_log = sections.amendment_log.trim();
    let preserved = if old_log.contains("Empty at founding") {
        String::new()
    } else {
        old_log.to_string()
    };

    let new_entry = render_amendment_entry(entry);
    let mut new_log = String::new();
    if !preserved.is_empty() {
        new_log.push_str(&preserved);
        if !preserved.ends_with('\n') {
            new_log.push('\n');
        }
        new_log.push('\n');
    }
    new_log.push_str(&new_entry);
    sections.amendment_log = new_log;

    serialize_charter_sections(&sections)
}

/// Reserialize a parsed charter back to markdown. Order is fixed
/// to match what `found::compose_charter` produces so amendments
/// don't accidentally reorder sections.
pub fn serialize_charter_sections(s: &CharterSections) -> String {
    let mut out = String::new();
    if !s.preamble.trim().is_empty() {
        out.push_str(s.preamble.trim_end());
        out.push_str("\n\n");
    }
    push_section(&mut out, "System design", &s.system_design);
    push_section(&mut out, "Invariants", &s.invariants);
    push_section(&mut out, "Resolved decisions", &s.resolved_decisions);
    push_section(&mut out, "Open questions", &s.open_questions);
    // Extras go AFTER the known sections and BEFORE the log — a
    // user who added `## Context` mid-charter gets to keep it in
    // a predictable place.
    for (heading, body) in &s.extras {
        push_section(&mut out, heading, body);
    }
    push_section(&mut out, "Amendment log", &s.amendment_log);
    out
}

fn push_section(out: &mut String, heading: &str, body: &str) {
    out.push_str("## ");
    out.push_str(heading);
    out.push_str("\n\n");
    let trimmed = body.trim();
    if trimmed.is_empty() {
        out.push('\n');
    } else {
        out.push_str(trimmed);
        out.push_str("\n\n");
    }
}

/// Minimal hint the drift-confirmation prompt can show the user
/// without dumping a full diff — summarizes what sections differ
/// between the recorded-hash state and the on-disk state.
pub fn drift_summary(recorded_hash: &str, current_content: &str) -> String {
    let current_hash = hash_charter(current_content);
    if recorded_hash == current_hash {
        // Caller shouldn't have invoked this, but be honest about it.
        return "No drift detected.".into();
    }
    let short_old = recorded_hash.get(..8).unwrap_or(recorded_hash);
    let short_new = current_hash.get(..8).unwrap_or(current_hash.as_str());
    format!("recorded_hash={short_old}…, on-disk hash={short_new}…")
}

// ─── Runner ──────────────────────────────────────────────────────────────────

/// The full outcome of `run_amend`. The caller writes files +
/// persists state based on `Approved`; on `Cancelled` nothing
/// changes on disk (beyond whatever the editor already wrote,
/// which the user saw and walked away from).
#[derive(Debug, Clone)]
pub enum AmendOutcome {
    Approved {
        new_charter: String,
        entry: AmendmentEntry,
    },
    Cancelled,
    NoChange,
}

/// Drive the amendment flow given already-loaded old + new
/// charter text. Pure logic — I/O (editor invocation, file
/// writes, project.toml updates) happens in the caller.
pub fn run_amend<I: AmendmentInterlocutor>(
    old_charter: &str,
    edited_charter: &str,
    next_version: u32,
    date: &str,
    committer: &str,
    interlocutor: &mut I,
) -> AmendOutcome {
    let old_sections = parse_charter_sections(old_charter);
    let new_sections = parse_charter_sections(edited_charter);
    let changed = changed_sections(&old_sections, &new_sections);

    if changed.is_empty() {
        return AmendOutcome::NoChange;
    }

    let questions = questions_for(&changed);
    let mut qa = Vec::with_capacity(questions.len());
    for q in questions {
        let answer = interlocutor.ask_adversarial(&q);
        qa.push((q, answer));
    }

    // Build the entry BEFORE the preview so the user sees the
    // same text that'll land in CHARTER.md. new_charter_hash is
    // computed against the post-apply content, which requires a
    // two-pass: first render a placeholder hash, apply, recompute.
    let placeholder_entry = AmendmentEntry {
        version: next_version,
        date: date.into(),
        changed_sections: changed.iter().map(|s| (*s).to_string()).collect(),
        qa: qa.clone(),
        committer: committer.into(),
        new_charter_hash: "<pending>".into(),
    };
    let provisional = apply_amendment(edited_charter, &placeholder_entry);
    let final_hash = hash_charter(&provisional);

    let entry = AmendmentEntry {
        new_charter_hash: final_hash,
        ..placeholder_entry
    };
    let final_charter = apply_amendment(edited_charter, &entry);
    let preview = render_amendment_entry(&entry);

    if !interlocutor.confirm_amendment(&preview) {
        return AmendOutcome::Cancelled;
    }
    AmendOutcome::Approved {
        new_charter: final_charter,
        entry,
    }
}

// ─── Decision note body ──────────────────────────────────────────────────────

/// Render the decision-kind note body that mirrors the amendment
/// log entry — same content, written to the project's note scope
/// so a session six weeks later can find it without reading
/// CHARTER.md first.
pub fn render_amendment_note_body(entry: &AmendmentEntry) -> String {
    let mut out = String::from("Amendment · ");
    out.push_str(&format!("v{}", entry.version));
    out.push_str("\n\n");
    out.push_str(&render_amendment_entry(entry));
    out
}

// ─── Editor integration ──────────────────────────────────────────────────────

/// Open `$EDITOR` on the given path. Returns the edited content,
/// or `None` if `$EDITOR` was unset / the editor errored. The
/// caller is expected to refuse to proceed with `None`.
pub fn invoke_editor(path: &Path) -> Option<String> {
    let editor = std::env::var("EDITOR").unwrap_or_default();
    if editor.is_empty() {
        eprintln!(
            "  $EDITOR is unset. Open {} in your editor and re-run `sovereign project amend`,\n\
             OR set $EDITOR and retry — we'll auto-reopen the same file.",
            path.display()
        );
        return None;
    }
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} {}", shell_escape(path)))
        .status();
    match status {
        Ok(s) if s.success() => std::fs::read_to_string(path).ok(),
        Ok(s) => {
            eprintln!(
                "  $EDITOR exited non-zero ({}) — amendment not applied.",
                s.code().unwrap_or(-1)
            );
            None
        }
        Err(e) => {
            eprintln!("  Could not launch $EDITOR: {e}");
            None
        }
    }
}

fn shell_escape(path: &Path) -> String {
    let s = path.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Convenience for cmd_amend — the stable path to `CHARTER.md`.
pub fn charter_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("CHARTER.md")
}

// ─── DESIGN.md amend flow (step 9) ───────────────────────────────────────────
//
// Simpler than charter amend because:
//   - DESIGN.md is iterative by design; no version bump on each save.
//   - No lifecycle flag means no drift-vs-hash check (although the
//     plan.db `design_hash` column will let future work surface
//     drift between the plan's snapshot and current DESIGN.md).
//   - Appending an amendment-log entry to DESIGN.md itself
//     preserves provenance inline. The agent / next reader sees
//     both the edits AND the argued-against risks.
//
// Reuses the shared `catalog()` via `questions_for` with DESIGN-
// specific section ids. Section-change detection walks the
// `DesignSignals::sections` snapshot before/after the edit and
// checks three specific H2 headings: `Anchors`, `Data & interfaces`,
// `Open questions`. Extra sections added by the user round-trip
// faithfully but don't trigger catalog entries today (the plan
// suggests deferring a catalog expansion until real patterns emerge).

/// Map a DESIGN.md H2 heading to the catalog section id. Returns
/// `None` for headings outside the curated set — we don't invent
/// adversarial questions for arbitrary sections.
pub fn design_section_id(heading: &str) -> Option<&'static str> {
    let h = heading.trim().to_lowercase();
    match h.as_str() {
        "anchors" => Some("design.anchors"),
        "data & interfaces" | "data and interfaces" | "data/interfaces" => {
            Some("design.data-interfaces")
        }
        "open questions" => Some("design.open-questions"),
        _ => None,
    }
}

/// Compute the list of catalog section ids whose DESIGN.md content
/// changed between two snapshots. Walks H2 sections only; nested
/// H3 differences bubble up via their parent section's body diff.
pub fn changed_design_sections(
    old: &corpus_engine_atos::design_signals::DesignSignals,
    new: &corpus_engine_atos::design_signals::DesignSignals,
) -> Vec<&'static str> {
    use std::collections::BTreeMap;
    fn h2_map(
        signals: &corpus_engine_atos::design_signals::DesignSignals,
    ) -> BTreeMap<String, String> {
        signals
            .sections
            .iter()
            .filter(|s| s.level == 2)
            .map(|s| (s.heading.clone(), normalize(&s.body)))
            .collect()
    }
    let old_m = h2_map(old);
    let new_m = h2_map(new);
    let mut out: Vec<&'static str> = Vec::new();
    for (heading, new_body) in &new_m {
        let Some(id) = design_section_id(heading) else {
            continue;
        };
        let old_body = old_m.get(heading).map(String::as_str).unwrap_or("");
        if old_body != new_body && !out.contains(&id) {
            out.push(id);
        }
    }
    // Section deletions: heading present in old, gone in new.
    for (heading, _) in &old_m {
        let Some(id) = design_section_id(heading) else {
            continue;
        };
        if !new_m.contains_key(heading) && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// Convenience: where DESIGN.md lives. Single source of truth —
/// matches `design_onboarding::design_path`.
pub fn design_md_path(repo_root: &Path) -> PathBuf {
    repo_root.join("DESIGN.md")
}

/// Render an amendment-log entry for DESIGN.md. Appended verbatim
/// to the `## Amendment log` section (creating it if absent).
/// Intentionally terse — the Q&A captures the why; the diff lives
/// in git.
pub fn render_design_amendment_entry(
    timestamp_iso: &str,
    qa: &[(AdversarialQuestion, String)],
    old_hash: &str,
    new_hash: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "### {timestamp_iso} · design amend\n\
         _Old DESIGN.md sha: `{old_hash}` → new: `{new_hash}`_\n\n"
    ));
    if qa.is_empty() {
        out.push_str(
            "_(No curated catalog questions fired — the changed sections are \
             outside the adversarial set. Content edit only.)_\n\n",
        );
    } else {
        for (q, a) in qa {
            out.push_str(&format!("- **{}** — {}\n", q.section, q.prompt));
            out.push_str(&format!("  _Why asked:_ {}\n", q.why));
            if a.trim().is_empty() {
                out.push_str("  _Answer:_ _(skipped)_\n\n");
            } else {
                out.push_str(&format!("  _Answer:_ {}\n\n", a.trim()));
            }
        }
    }
    out
}

/// Append `entry` to DESIGN.md's `## Amendment log` section,
/// creating the section at the end of the file if absent.
pub fn append_design_amendment_log(md: &str, entry: &str) -> String {
    const HEADING: &str = "## Amendment log";
    if let Some(idx) = md.find(HEADING) {
        // Insert entry immediately after the heading line (and the
        // blank line that typically follows). Find the end of the
        // heading line and splice in.
        let after_heading = idx + HEADING.len();
        // Skip to end of line
        let rest = &md[after_heading..];
        let eol = rest
            .find('\n')
            .map(|n| after_heading + n + 1)
            .unwrap_or(md.len());
        let mut out = String::with_capacity(md.len() + entry.len() + 2);
        out.push_str(&md[..eol]);
        // Ensure a blank line between the heading and the new entry.
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(entry);
        out.push_str(&md[eol..]);
        out
    } else {
        // No amendment-log section yet — append both the heading
        // and the first entry at the end.
        let mut out = md.trim_end().to_string();
        out.push_str("\n\n");
        out.push_str(HEADING);
        out.push_str("\n\n");
        out.push_str(entry);
        out
    }
}

#[cfg(test)]
mod design_amend_tests {
    use super::*;

    fn signals(md: &str) -> corpus_engine_atos::design_signals::DesignSignals {
        corpus_engine_atos::design_signals::extract(md)
    }

    #[test]
    fn design_section_id_maps_curated_headings() {
        assert_eq!(design_section_id("Anchors"), Some("design.anchors"));
        assert_eq!(
            design_section_id("Data & interfaces"),
            Some("design.data-interfaces")
        );
        assert_eq!(
            design_section_id("data and interfaces"),
            Some("design.data-interfaces")
        );
        assert_eq!(
            design_section_id("Open questions"),
            Some("design.open-questions")
        );
        assert_eq!(design_section_id("What we're building"), None);
    }

    #[test]
    fn changed_design_sections_detects_body_edit() {
        let old = signals("# P\n\n## Anchors\n\n- a\n\n## Data & interfaces\n\nv1\n");
        let new = signals("# P\n\n## Anchors\n\n- a\n\n## Data & interfaces\n\nv2\n");
        assert_eq!(
            changed_design_sections(&old, &new),
            vec!["design.data-interfaces"]
        );
    }

    #[test]
    fn changed_design_sections_detects_section_removal() {
        let old = signals("# P\n\n## Anchors\n\n- a\n\n## Open questions\n\n- tbd\n");
        let new = signals("# P\n\n## Anchors\n\n- a\n");
        assert_eq!(
            changed_design_sections(&old, &new),
            vec!["design.open-questions"]
        );
    }

    #[test]
    fn changed_design_sections_ignores_non_curated() {
        let old = signals("# P\n\n## What we're building\n\nfoo\n");
        let new = signals("# P\n\n## What we're building\n\nbar\n");
        assert!(
            changed_design_sections(&old, &new).is_empty(),
            "non-curated heading edits shouldn't drive adversarial Q"
        );
    }

    #[test]
    fn render_design_amendment_entry_shapes_well() {
        let q = AdversarialQuestion {
            id: "amend.adv.design.anchors".into(),
            section: "design.anchors".into(),
            prompt: "Which assumption shifts?".into(),
            why: "Anchors ripple.".into(),
        };
        let entry = render_design_amendment_entry(
            "2026-04-22",
            &[(q.clone(), "I migrated the callers.".to_string())],
            "aaaaaa",
            "bbbbbb",
        );
        assert!(entry.contains("2026-04-22"));
        assert!(entry.contains("aaaaaa"));
        assert!(entry.contains("bbbbbb"));
        assert!(entry.contains("I migrated the callers."));
        assert!(entry.contains("Anchors ripple."));
    }

    #[test]
    fn render_design_amendment_entry_handles_empty_qa() {
        let entry = render_design_amendment_entry("2026-04-22", &[], "old", "new");
        assert!(entry.contains("No curated catalog questions fired"));
    }

    #[test]
    fn append_to_existing_amendment_log_keeps_order() {
        let doc = "# P\n\n## Anchors\n\n- a\n\n## Amendment log\n\n### previous\nold entry.\n";
        let out = append_design_amendment_log(doc, "### new\nnew entry.\n");
        let log_idx = out.find("## Amendment log").unwrap();
        let new_idx = out.find("### new").unwrap();
        let prev_idx = out.find("### previous").unwrap();
        assert!(log_idx < new_idx, "log heading precedes new entry");
        assert!(
            new_idx < prev_idx,
            "new entry appears BEFORE prior entries (newest-on-top convention)"
        );
    }

    #[test]
    fn append_creates_amendment_log_when_missing() {
        let doc = "# P\n\n## Anchors\n\n- a\n";
        let out = append_design_amendment_log(doc, "### new\nnew entry.\n");
        assert!(out.contains("## Amendment log"));
        assert!(out.contains("### new"));
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const FOUNDED_CHARTER: &str = r#"# p — Charter

_Founded: 2026-04-20, charter version 1._

## System design

The original design.

## Invariants

- No writes before approval.

## Resolved decisions

- **found.stage1.project-purpose** — Ingest ticks.

## Open questions

- **fault.id-scheme** — Identifier scheme. _still deciding_

## Amendment log

_(Empty at founding. Amendments land here via `sovereign project amend`, each carrying the adversarial review + the reasoning that overrode it.)_
"#;

    // ── Parsing ────────────────────────────────────────────────

    #[test]
    fn parse_extracts_each_known_section() {
        let s = parse_charter_sections(FOUNDED_CHARTER);
        assert!(s.preamble.contains("# p — Charter"));
        assert!(s.system_design.contains("The original design."));
        assert!(s.invariants.contains("No writes before approval."));
        assert!(s.resolved_decisions.contains("Ingest ticks."));
        assert!(s.open_questions.contains("still deciding"));
        assert!(s.amendment_log.contains("Empty at founding"));
        assert!(s.extras.is_empty());
    }

    #[test]
    fn parse_preserves_unknown_sections_in_extras() {
        let md = "# t\n\n## System design\n\nbody\n\n## Context\n\nextra body\n";
        let s = parse_charter_sections(md);
        assert_eq!(s.system_design.trim(), "body");
        assert_eq!(s.extras.len(), 1);
        assert_eq!(s.extras[0].0, "Context");
        assert_eq!(s.extras[0].1.trim(), "extra body");
    }

    #[test]
    fn serialize_roundtrips_with_no_data_loss() {
        let s = parse_charter_sections(FOUNDED_CHARTER);
        let re = serialize_charter_sections(&s);
        let s2 = parse_charter_sections(&re);
        assert_eq!(s, s2, "parse-serialize should round-trip losslessly");
    }

    // ── Diff ───────────────────────────────────────────────────

    #[test]
    fn changed_sections_ignores_preamble_and_amendment_log() {
        let old = parse_charter_sections(FOUNDED_CHARTER);
        let mut new = old.clone();
        new.preamble = "different title\n".into();
        new.amendment_log = "whatever\n".into();
        assert!(changed_sections(&old, &new).is_empty());
    }

    #[test]
    fn changed_sections_detects_invariants_edit() {
        let old = parse_charter_sections(FOUNDED_CHARTER);
        let mut new = old.clone();
        new.invariants = "- New stricter invariant.\n".into();
        let changed = changed_sections(&old, &new);
        assert_eq!(changed, vec!["invariants"]);
    }

    #[test]
    fn changed_sections_ignores_trailing_whitespace_only_edits() {
        let old = parse_charter_sections(FOUNDED_CHARTER);
        let mut new = old.clone();
        // Add trailing whitespace on each line.
        new.system_design = old
            .system_design
            .lines()
            .map(|l| format!("{l}   "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            changed_sections(&old, &new).is_empty(),
            "whitespace-only diffs must not trigger adversarial review"
        );
    }

    // ── Catalog ────────────────────────────────────────────────

    #[test]
    fn every_catalog_entry_has_non_empty_prompt_and_why() {
        for e in catalog() {
            assert!(
                !e.prompt.trim().is_empty(),
                "empty prompt: {}",
                e.question_id
            );
            assert!(!e.why.trim().is_empty(), "empty why: {}", e.question_id);
            assert!(e.question_id.starts_with("amend.adv."));
        }
    }

    #[test]
    fn questions_for_only_fires_for_changed_sections() {
        let qs = questions_for(&["invariants"]);
        assert!(qs.iter().all(|q| q.section == "invariants"));
        assert!(!qs.is_empty());
    }

    #[test]
    fn questions_for_combines_multiple_sections_without_duplicates_per_entry() {
        let qs = questions_for(&["invariants", "resolved-decisions"]);
        let ids: Vec<&str> = qs.iter().map(|q| q.id.as_str()).collect();
        // Each catalog id appears at most once.
        let mut seen = std::collections::HashSet::new();
        for id in &ids {
            assert!(seen.insert(*id), "duplicate question id: {id}");
        }
        assert!(qs.iter().any(|q| q.section == "invariants"));
        assert!(qs.iter().any(|q| q.section == "resolved-decisions"));
    }

    // ── Composition ────────────────────────────────────────────

    fn sample_entry() -> AmendmentEntry {
        AmendmentEntry {
            version: 2,
            date: "2026-05-01".into(),
            changed_sections: vec!["invariants".into()],
            qa: vec![(
                AdversarialQuestion {
                    id: "amend.adv.invariants.assumers".into(),
                    section: "invariants".into(),
                    prompt: "Who assumes the old invariant?".into(),
                    why: "tests lock it".into(),
                },
                "The schema validator. Updated in this same PR.".into(),
            )],
            committer: "Yara <yara@example.test>".into(),
            new_charter_hash: "abc123def".into(),
        }
    }

    #[test]
    fn amendment_entry_renders_qa_and_metadata() {
        let e = sample_entry();
        let body = render_amendment_entry(&e);
        assert!(body.contains("### Amendment 2 — 2026-05-01"));
        assert!(body.contains("Changed sections: invariants"));
        assert!(body.contains("Who assumes the old invariant?"));
        assert!(body.contains("The schema validator"));
        assert!(body.contains("Committed by: Yara <yara@example.test>"));
        assert!(body.contains("abc123def"));
    }

    #[test]
    fn empty_qa_entry_marks_it_explicitly() {
        let mut e = sample_entry();
        e.qa.clear();
        let body = render_amendment_entry(&e);
        assert!(body.contains("No adversarial questions fired"));
    }

    #[test]
    fn blank_answer_gets_sentinel_not_empty_string() {
        let mut e = sample_entry();
        e.qa[0].1 = "".into();
        let body = render_amendment_entry(&e);
        assert!(body.contains("no response given"));
    }

    #[test]
    fn apply_amendment_strips_founding_placeholder() {
        let edited =
            FOUNDED_CHARTER.replace("No writes before approval.", "Writes OK when approved.");
        let entry = sample_entry();
        let after = apply_amendment(&edited, &entry);
        assert!(
            !after.contains("Empty at founding"),
            "founding placeholder must be stripped once a real amendment lands"
        );
        assert!(after.contains("### Amendment 2 — 2026-05-01"));
    }

    #[test]
    fn apply_amendment_preserves_prior_entries() {
        let with_one_entry = FOUNDED_CHARTER.replace(
            "_(Empty at founding. Amendments land here via `sovereign project amend`, each carrying the adversarial review + the reasoning that overrode it.)_",
            "### Amendment 1 — 2026-04-25\n\nChanged sections: system-design\n\n_Committed by: First. New charter_hash: zzz._",
        );
        let entry = sample_entry();
        let after = apply_amendment(&with_one_entry, &entry);
        assert!(after.contains("### Amendment 1 — 2026-04-25"));
        assert!(after.contains("### Amendment 2 — 2026-05-01"));
    }

    #[test]
    fn apply_amendment_preserves_extras_sections() {
        let with_extra = FOUNDED_CHARTER.replace(
            "## Amendment log",
            "## Context\n\nextra body\n\n## Amendment log",
        );
        let entry = sample_entry();
        let after = apply_amendment(&with_extra, &entry);
        assert!(after.contains("## Context"));
        assert!(after.contains("extra body"));
        assert!(after.contains("### Amendment 2"));
    }

    // ── Runner ─────────────────────────────────────────────────

    struct ScriptedAmendment {
        answers: Vec<String>,
        asked: RefCell<Vec<String>>,
        approve: bool,
        drift_yes: bool,
    }

    impl ScriptedAmendment {
        fn new(answers: Vec<&str>, approve: bool) -> Self {
            Self {
                answers: answers.into_iter().map(String::from).collect(),
                asked: RefCell::new(Vec::new()),
                approve,
                drift_yes: false,
            }
        }
    }

    impl AmendmentInterlocutor for ScriptedAmendment {
        fn ask_adversarial(&mut self, q: &AdversarialQuestion) -> String {
            self.asked.borrow_mut().push(q.id.clone());
            self.answers.remove(0)
        }
        fn confirm_amendment(&mut self, _preview: &str) -> bool {
            self.approve
        }
        fn confirm_drift(&mut self, _hint: &str) -> bool {
            self.drift_yes
        }
    }

    #[test]
    fn run_amend_reports_no_change_when_charter_untouched() {
        let mut interloc = ScriptedAmendment::new(vec![], true);
        let outcome = run_amend(
            FOUNDED_CHARTER,
            FOUNDED_CHARTER,
            2,
            "2026-05-01",
            "Y",
            &mut interloc,
        );
        matches!(outcome, AmendOutcome::NoChange);
        assert!(interloc.asked.borrow().is_empty());
    }

    #[test]
    fn run_amend_fires_section_specific_questions_and_approves() {
        let edited =
            FOUNDED_CHARTER.replace("No writes before approval.", "Writes OK when approved.");
        let mut interloc =
            ScriptedAmendment::new(vec!["schema validator updated", "v2 invariant: ..."], true);
        let outcome = run_amend(
            FOUNDED_CHARTER,
            &edited,
            2,
            "2026-05-01",
            "Y",
            &mut interloc,
        );
        match outcome {
            AmendOutcome::Approved { new_charter, entry } => {
                assert!(new_charter.contains("### Amendment 2 — 2026-05-01"));
                assert_eq!(entry.version, 2);
                assert_eq!(entry.changed_sections, vec!["invariants"]);
                // New charter hash should not be the "<pending>"
                // placeholder — the runner recomputes after apply.
                assert_ne!(entry.new_charter_hash, "<pending>");
                assert_eq!(entry.new_charter_hash.len(), 64);
            }
            other => panic!("expected Approved, got {other:?}"),
        }
        // Both invariants questions fired.
        let asked = interloc.asked.borrow();
        assert!(asked.iter().any(|id| id == "amend.adv.invariants.assumers"));
        assert!(asked
            .iter()
            .any(|id| id == "amend.adv.invariants.replacement"));
    }

    #[test]
    fn run_amend_cancel_returns_without_approved() {
        let edited =
            FOUNDED_CHARTER.replace("No writes before approval.", "Writes OK when approved.");
        let mut interloc = ScriptedAmendment::new(vec!["a", "b"], /* approve */ false);
        let outcome = run_amend(
            FOUNDED_CHARTER,
            &edited,
            2,
            "2026-05-01",
            "Y",
            &mut interloc,
        );
        matches!(outcome, AmendOutcome::Cancelled);
    }

    #[test]
    fn drift_summary_names_both_hashes() {
        let recorded = hash_charter("original");
        let current = "mutated charter";
        let hint = drift_summary(&recorded, current);
        assert!(hint.contains("recorded_hash="));
        assert!(hint.contains("on-disk hash="));
    }

    #[test]
    fn drift_summary_is_honest_when_called_without_drift() {
        let c = "same";
        let hint = drift_summary(&hash_charter(c), c);
        assert!(hint.contains("No drift"));
    }
}
