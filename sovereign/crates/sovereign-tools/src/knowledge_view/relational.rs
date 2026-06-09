// SPDX-License-Identifier: AGPL-3.0-or-later
//! Relational digest — one of the two new digest blocks added by the
//! Relational + Strategic Awareness changeset (requirements §4.2).
//!
//! Formats a slice of [`InteractionTimeline`]s filtered to `Person`
//! and `Organization` entities into a compact markdown block bounded
//! by a token budget (default 150). The block names who the user has
//! discussed recently, with affiliation/role + outstanding-commitment
//! annotations.
//!
//! The formatter is **pure**: it doesn't read disk, doesn't call
//! inference, doesn't touch the state store. The caller (the
//! `KnowledgeViewManager` splice path) gathers timelines via
//! `timeline::assemble_timelines_from_atlas`, queries the NoteStore
//! for outstanding commitment / follow-up notes (Phase 6), then
//! hands both into [`format_relational`].

use crate::knowledge_view::timeline::{InteractionTimeline, TimelineEntityKind};
use crate::knowledge_view::tokens::estimate_tokens;
use crate::knowledge_view::view_kind::ViewKind;

/// Half-life used for the relational recency-decay score, in seconds.
/// Matches requirements §4.2 (14 days). 14d × 86400s/d = 1_209_600.
const RELATIONAL_HALF_LIFE_SECS: f64 = 14.0 * 86_400.0;

/// Note attached to a relational entity — surfaced in the digest as
/// "(noted Mar 14)" or "(overdue)" annotations. The caller (Phase 6
/// integration) populates these from the NoteStore by looking up
/// notes whose `related_entity` matches the entity name.
#[derive(Debug, Clone)]
pub struct RelationalNote {
    pub kind: RelationalNoteKind,
    /// `created_at` for commitments / goals; the "approximate
    /// timeframe" deadline for follow-ups.
    pub anchor_timestamp: i64,
    /// Free-text summary surfaced in the digest. Typically a short
    /// fragment of `note.content`.
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationalNoteKind {
    Commitment,
    FollowUp,
    Goal,
}

/// Format a relational digest block.
///
/// `now_unix` is the assembler's current-time anchor — used for
/// recency decay and "(overdue)" annotation. Tests pass a fixed
/// timestamp; production passes `chrono::Utc::now().timestamp()`.
///
/// `budget_tokens` is the soft cap; the formatter stops emitting
/// rows when adding the next row would push past the budget.
/// Returns the rendered markdown block (no trailing newline) plus
/// the number of entries actually included.
pub fn format_relational(
    timelines: &[InteractionTimeline],
    notes: &dyn Fn(&str) -> Vec<RelationalNote>,
    in_conversation: &dyn Fn(&str) -> bool,
    now_unix: i64,
    budget_tokens: usize,
) -> (String, usize) {
    // Filter to Person + Organization, score, sort.
    let mut scored: Vec<(f64, &InteractionTimeline)> = timelines
        .iter()
        .filter(|t| {
            matches!(
                t.entity_kind,
                TimelineEntityKind::Person | TimelineEntityKind::Organization
            )
        })
        .map(|t| (relational_score(t, &notes, &in_conversation, now_unix), t))
        .collect();
    // Highest score first.
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = String::new();
    let mut included: usize = 0;
    out.push_str(ViewKind::Relational.title());
    out.push_str(":\n");
    let header_tokens = estimate_tokens(&out);
    let mut consumed = header_tokens;

    for (_score, t) in &scored {
        let line = render_entry(t, &notes(&t.entity_name));
        let line_tokens = estimate_tokens(&line);
        if consumed + line_tokens > budget_tokens && included > 0 {
            // Already have at least one entry — stop rather than
            // overrun budget. Rendering even one entry is better
            // than an empty block per requirements §4.2 (digest
            // never reads as "no people" when entities exist).
            break;
        }
        out.push_str(&line);
        consumed += line_tokens;
        included += 1;
    }

    if included == 0 {
        // No entities to surface — return an empty string rather
        // than a heading-only block. The splice path drops empty
        // blocks entirely.
        return (String::new(), 0);
    }

    (out, included)
}

/// Per-entity score combining recency, frequency, outstanding-note
/// boost, and in-conversation boost. Higher is more relevant.
fn relational_score(
    t: &InteractionTimeline,
    notes: &dyn Fn(&str) -> Vec<RelationalNote>,
    in_conversation: &dyn Fn(&str) -> bool,
    now_unix: i64,
) -> f64 {
    let last = crate::knowledge_view::timeline::last_seen_at(t).unwrap_or(0);
    let age_secs = (now_unix - last).max(0) as f64;
    let recency = (-age_secs / RELATIONAL_HALF_LIFE_SECS * std::f64::consts::LN_2).exp();
    // Frequency over a 90-day window per requirements §4.2.
    let window_start = now_unix - 90 * 86_400;
    let freq =
        crate::knowledge_view::timeline::interactions_within(t, window_start, now_unix) as f64;
    // Boost if the entity has any outstanding note.
    let has_note = !notes(&t.entity_name).is_empty();
    let note_boost = if has_note { 0.5 } else { 0.0 };
    // Strongest boost: the entity is mentioned in the current turn.
    let conv_boost = if in_conversation(&t.entity_name) {
        2.0
    } else {
        0.0
    };
    recency + 0.1 * freq + note_boost + conv_boost
}

fn render_entry(t: &InteractionTimeline, notes: &[RelationalNote]) -> String {
    let mut s = String::with_capacity(96);
    s.push_str("- ");
    s.push_str(&t.entity_name);

    // Affiliation parenthetical for people: "Sarah Chen (Acme Corp)".
    if let Some(aff) = &t.affiliation {
        s.push_str(" (");
        s.push_str(aff);
        s.push(')');
    }

    // Frequency phrasing: "3 conversations" or "1 conversation".
    let n = t
        .interactions
        .iter()
        .filter(|i| i.timestamp.is_some())
        .count();
    if n > 0 {
        s.push_str(" — ");
        if n == 1 {
            s.push_str("1 mention");
        } else {
            s.push_str(&format!("{} mentions", n));
        }
        // Append last-seen relative date if we can compute one.
        if let Some(last) = crate::knowledge_view::timeline::last_seen_at(t) {
            s.push_str(", last ");
            s.push_str(&format_relative(last));
        }
    }

    if let Some(role) = &t.role {
        s.push_str("; role: ");
        s.push_str(role);
    }

    // Outstanding-note annotations: "you committed to send pricing
    // (noted Mar 14)" / "follow-up overdue".
    let mut commit_summaries: Vec<&str> = Vec::new();
    let mut follow_up_overdue: bool = false;
    let mut goal_count: usize = 0;
    for n in notes {
        match n.kind {
            RelationalNoteKind::Commitment => commit_summaries.push(&n.summary),
            RelationalNoteKind::FollowUp => {
                // "overdue" check: anchor in the past.
                let now = chrono::Utc::now().timestamp();
                if n.anchor_timestamp <= now {
                    follow_up_overdue = true;
                }
            }
            RelationalNoteKind::Goal => goal_count += 1,
        }
    }
    if !commit_summaries.is_empty() {
        s.push_str("; you committed to ");
        s.push_str(commit_summaries.join("; ").as_str());
    }
    if follow_up_overdue {
        s.push_str("; follow-up overdue");
    }
    if goal_count > 0 {
        s.push_str(&format!(
            "; {} goal{} attached",
            goal_count,
            if goal_count == 1 { "" } else { "s" }
        ));
    }

    s.push('\n');
    s
}

/// Render a relative date — "yesterday", "3d ago", "Mar 14", etc.
/// The exact strings are not contractual; tests pin behaviour
/// through approximate matchers (substring on "ago" or month name).
fn format_relative(ts: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let delta = (now - ts).max(0);
    if delta < 86_400 {
        return "today".into();
    }
    let days = delta / 86_400;
    if days < 14 {
        return format!("{}d ago", days);
    }
    // Older: ISO-ish "yyyy-mm-dd".
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0);
    match dt {
        Some(d) => d.format("%b %-d").to_string(),
        None => format!("{}d ago", days),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge_view::timeline::{Interaction, TimelineEntityKind};

    fn person_timeline(name: &str, ts: &[i64]) -> InteractionTimeline {
        InteractionTimeline {
            entity_id: format!("entity-{}", name.len()),
            entity_name: name.into(),
            entity_kind: TimelineEntityKind::Person,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            interactions: ts
                .iter()
                .map(|&t| Interaction {
                    timestamp: Some(t),
                    source_chunk_id: t.to_string(),
                })
                .collect(),
            atos_project: None,
        }
    }

    #[test]
    fn empty_input_returns_empty_block() {
        let (out, n) = format_relational(
            &[],
            &|_: &str| Vec::new(),
            &|_: &str| false,
            1_700_000_000,
            150,
        );
        assert!(out.is_empty());
        assert_eq!(n, 0);
    }

    #[test]
    fn person_entry_includes_affiliation_and_mention_count() {
        let now = chrono::Utc::now().timestamp();
        let mut t = person_timeline("Sarah Chen", &[now - 86_400, now - 3 * 86_400]);
        t.affiliation = Some("Acme Corp".into());
        let (out, n) = format_relational(&[t], &|_: &str| Vec::new(), &|_: &str| false, now, 150);
        assert_eq!(n, 1);
        assert!(out.contains("Sarah Chen"));
        assert!(out.contains("Acme Corp"));
        assert!(out.contains("2 mentions"));
        assert!(out.contains("People on your radar"));
    }

    #[test]
    fn initiative_entries_are_filtered_out() {
        let now = chrono::Utc::now().timestamp();
        let mut init = person_timeline("API Migration", &[now]);
        init.entity_kind = TimelineEntityKind::Initiative;
        let person = person_timeline("Mike Torres", &[now]);
        let (out, n) = format_relational(
            &[init, person],
            &|_: &str| Vec::new(),
            &|_: &str| false,
            now,
            150,
        );
        assert_eq!(n, 1);
        assert!(out.contains("Mike Torres"));
        assert!(!out.contains("API Migration"));
    }

    #[test]
    fn in_conversation_boost_lifts_entity_to_top() {
        let now = chrono::Utc::now().timestamp();
        // Two people, one with a much older mention but currently
        // in conversation. The boost should put them on top.
        let recent = person_timeline("RecentPerson", &[now - 60]);
        let stale_but_active = person_timeline("ActivePerson", &[now - 200 * 86_400]);
        let (out, _) = format_relational(
            &[recent, stale_but_active],
            &|_: &str| Vec::new(),
            &|name: &str| name == "ActivePerson",
            now,
            150,
        );
        let active_pos = out.find("ActivePerson").unwrap();
        let recent_pos = out.find("RecentPerson").unwrap_or(usize::MAX);
        assert!(
            active_pos < recent_pos,
            "in-conversation boost must rank ActivePerson above RecentPerson; got:\n{out}"
        );
    }

    #[test]
    fn budget_caps_block_size() {
        let now = chrono::Utc::now().timestamp();
        // Twenty entities; tight budget keeps only a handful.
        let timelines: Vec<_> = (0..20)
            .map(|i| person_timeline(&format!("Person{:02}", i), &[now - i * 86_400]))
            .collect();
        let (out, n) = format_relational(
            &timelines,
            &|_: &str| Vec::new(),
            &|_: &str| false,
            now,
            120, // small budget
        );
        assert!(n >= 1);
        assert!(n < 20, "budget should clip the list; got {n}");
        assert!(
            estimate_tokens(&out) <= 130,
            "block within (close to) budget"
        );
    }

    #[test]
    fn outstanding_commitment_surfaces_in_entry() {
        let now = chrono::Utc::now().timestamp();
        let t = person_timeline("Sarah Chen", &[now - 86_400]);
        let notes = move |name: &str| -> Vec<RelationalNote> {
            if name == "Sarah Chen" {
                vec![RelationalNote {
                    kind: RelationalNoteKind::Commitment,
                    anchor_timestamp: now - 7 * 86_400,
                    summary: "send revised pricing".into(),
                }]
            } else {
                Vec::new()
            }
        };
        let (out, _) = format_relational(&[t], &notes, &|_: &str| false, now, 150);
        assert!(out.contains("you committed to send revised pricing"));
    }
}
