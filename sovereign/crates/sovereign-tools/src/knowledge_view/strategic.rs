//! Strategic digest — the second of the two new digest blocks
//! (requirements §4.3).
//!
//! Formats `Initiative` interaction timelines into a compact markdown
//! block bounded by a token budget (default 100). Each entry shows
//! the initiative name, recent activity, ATOS phase + drift status
//! when an [`AtosLink`](crate::knowledge_view::timeline::AtosLink) is
//! attached, and any `goal`-kind notes the user has confirmed.
//!
//! Like [`relational`](crate::knowledge_view::relational), this
//! formatter is pure — caller assembles timelines + supplies a
//! goal-note resolver + a "currently in conversation" predicate.

use crate::knowledge_view::timeline::{
    AtosLinkKind, CharterStatus, InteractionTimeline, TimelineEntityKind,
};
use crate::knowledge_view::tokens::estimate_tokens;
use crate::knowledge_view::view_kind::ViewKind;

/// Recency half-life for the strategic digest, in seconds. 21 days
/// per requirements §4.3 — initiatives have a longer shelf life
/// than interpersonal context.
const STRATEGIC_HALF_LIFE_SECS: f64 = 21.0 * 86_400.0;

/// Window for the staleness annotation ("no progress discussions in
/// 3 weeks") per requirements §6.4. Same 21-day boundary.
const STALENESS_WINDOW_SECS: i64 = 21 * 86_400;

/// A goal note linked to an initiative entity by name. The caller
/// queries the NoteStore for active `goal`-kind notes whose
/// `related_entity` matches the initiative name (or its
/// case-insensitive normalisation).
#[derive(Debug, Clone)]
pub struct StrategicGoal {
    pub created_at: i64,
    pub summary: String,
}

/// Format the strategic digest block. Returns `(rendered, n_entries)`.
pub fn format_strategic(
    timelines: &[InteractionTimeline],
    goals: &dyn Fn(&str) -> Vec<StrategicGoal>,
    in_conversation: &dyn Fn(&str) -> bool,
    now_unix: i64,
    budget_tokens: usize,
) -> (String, usize) {
    // Filter to Initiative + score.
    let mut scored: Vec<(f64, &InteractionTimeline)> = timelines
        .iter()
        .filter(|t| matches!(t.entity_kind, TimelineEntityKind::Initiative))
        .map(|t| (strategic_score(t, &in_conversation, now_unix), t))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = String::new();
    let mut included: usize = 0;
    out.push_str(ViewKind::Strategic.title());
    out.push_str(":\n");
    let mut consumed = estimate_tokens(&out);

    for (_score, t) in &scored {
        let entry = render_entry(t, &goals(&t.entity_name), now_unix);
        let line_tokens = estimate_tokens(&entry);
        if consumed + line_tokens > budget_tokens && included > 0 {
            break;
        }
        out.push_str(&entry);
        consumed += line_tokens;
        included += 1;
    }

    if included == 0 {
        return (String::new(), 0);
    }
    (out, included)
}

fn strategic_score(
    t: &InteractionTimeline,
    in_conversation: &dyn Fn(&str) -> bool,
    now_unix: i64,
) -> f64 {
    let last = crate::knowledge_view::timeline::last_seen_at(t).unwrap_or(0);
    let age = (now_unix - last).max(0) as f64;
    let recency = (-age / STRATEGIC_HALF_LIFE_SECS * std::f64::consts::LN_2).exp();
    // 120-day frequency window per requirements §4.3.
    let window_start = now_unix - 120 * 86_400;
    let freq = crate::knowledge_view::timeline::interactions_within(t, window_start, now_unix)
        as f64;
    let conv_boost = if in_conversation(&t.entity_name) {
        2.0
    } else {
        0.0
    };
    let drift_boost = matches!(
        t.atos_project.as_ref().map(|l| l.charter_status),
        Some(CharterStatus::Drifted)
    );
    let drift_score = if drift_boost { 1.0 } else { 0.0 };
    recency + 0.05 * freq + conv_boost + drift_score
}

fn render_entry(
    t: &InteractionTimeline,
    goals: &[StrategicGoal],
    now_unix: i64,
) -> String {
    let mut s = String::with_capacity(120);
    s.push_str("- ");
    s.push_str(&t.entity_name);

    let n = t.interactions.iter().filter(|i| i.timestamp.is_some()).count();
    if n > 0 {
        s.push_str(" — ");
        s.push_str(&format!(
            "{} discussion{}",
            n,
            if n == 1 { "" } else { "s" }
        ));
        if let Some(last) = crate::knowledge_view::timeline::last_seen_at(t) {
            s.push_str(", last ");
            s.push_str(&format_relative_strategic(last, now_unix));
        }
    }

    // ATOS link rendering: "ATOS phase 2/4" + "(drift)" annotation.
    if let Some(link) = &t.atos_project {
        s.push_str("; ATOS ");
        match link.kind {
            AtosLinkKind::Project => s.push_str("project "),
            AtosLinkKind::Feature => s.push_str("feature "),
        }
        match (link.current_phase, link.total_phases) {
            (Some(p), Some(total)) => s.push_str(&format!("phase {}/{}", p, total)),
            (Some(p), None) => s.push_str(&format!("phase {}", p)),
            _ => s.push_str(&link.id),
        }
        if matches!(link.charter_status, CharterStatus::Drifted) {
            s.push_str(" (drift)");
        }
    }

    // Goal annotations.
    if !goals.is_empty() {
        for g in goals {
            s.push_str("; goal: ");
            s.push_str(&g.summary);
        }
        // Staleness — when the most recent interaction is older
        // than the window, append "(no recent discussion)".
        if let Some(last) = crate::knowledge_view::timeline::last_seen_at(t) {
            if now_unix - last > STALENESS_WINDOW_SECS {
                s.push_str(" (no recent discussion)");
            }
        } else {
            s.push_str(" (no recent discussion)");
        }
    }

    s.push('\n');
    s
}

fn format_relative_strategic(ts: i64, now_unix: i64) -> String {
    let delta = (now_unix - ts).max(0);
    if delta < 86_400 {
        return "today".into();
    }
    let days = delta / 86_400;
    if days < 21 {
        return format!("{}d ago", days);
    }
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0);
    match dt {
        Some(d) => d.format("%b %-d").to_string(),
        None => format!("{}d ago", days),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge_view::timeline::{
        AtosLink, AtosLinkKind, CharterStatus, Interaction, TimelineEntityKind,
    };

    fn initiative_timeline(name: &str, ts: &[i64]) -> InteractionTimeline {
        InteractionTimeline {
            entity_id: format!("entity-{}", name.len()),
            entity_name: name.into(),
            entity_kind: TimelineEntityKind::Initiative,
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
        let (out, n) = format_strategic(
            &[],
            &|_: &str| Vec::new(),
            &|_: &str| false,
            0,
            100,
        );
        assert!(out.is_empty());
        assert_eq!(n, 0);
    }

    #[test]
    fn only_initiatives_appear() {
        let now = 1_700_000_000;
        let mut person = initiative_timeline("Sarah", &[now]);
        person.entity_kind = TimelineEntityKind::Person;
        let init = initiative_timeline("API migration", &[now]);
        let (out, n) = format_strategic(
            &[person, init],
            &|_: &str| Vec::new(),
            &|_: &str| false,
            now,
            100,
        );
        assert_eq!(n, 1);
        assert!(out.contains("API migration"));
        assert!(!out.contains("Sarah"));
    }

    #[test]
    fn atos_link_renders_phase_and_drift() {
        let now = 1_700_000_000;
        let mut t = initiative_timeline("API migration", &[now - 3 * 86_400]);
        t.atos_project = Some(AtosLink {
            kind: AtosLinkKind::Project,
            id: "api-migration".into(),
            current_phase: Some(2),
            total_phases: Some(4),
            charter_status: CharterStatus::Drifted,
        });
        let (out, _) = format_strategic(
            &[t],
            &|_: &str| Vec::new(),
            &|_: &str| false,
            now,
            100,
        );
        assert!(out.contains("ATOS project phase 2/4"), "got: {}", out);
        assert!(out.contains("(drift)"));
    }

    #[test]
    fn no_atos_link_renders_without_phase_filler() {
        let now = 1_700_000_000;
        let t = initiative_timeline("Q3 enterprise push", &[now]);
        let (out, _) = format_strategic(
            &[t],
            &|_: &str| Vec::new(),
            &|_: &str| false,
            now,
            100,
        );
        assert!(out.contains("Q3 enterprise push"));
        assert!(!out.contains("phase"));
        assert!(!out.contains("n/a"));
    }

    #[test]
    fn stale_goal_gets_staleness_annotation() {
        let now = 1_700_000_000;
        let t = initiative_timeline("Churn reduction", &[now - 30 * 86_400]);
        let goals = move |name: &str| -> Vec<StrategicGoal> {
            if name == "Churn reduction" {
                vec![StrategicGoal {
                    created_at: now - 60 * 86_400,
                    summary: "under 5% by Q3".into(),
                }]
            } else {
                Vec::new()
            }
        };
        let (out, _) =
            format_strategic(&[t], &goals, &|_: &str| false, now, 200);
        assert!(out.contains("goal: under 5% by Q3"));
        assert!(out.contains("(no recent discussion)"));
    }

    #[test]
    fn drift_boost_lifts_drifted_initiative_to_top() {
        let now = 1_700_000_000;
        // One initiative discussed yesterday with no ATOS; one
        // discussed a month ago with ATOS drift. Drift should still
        // win the ranking.
        let recent_clean = initiative_timeline("Recent", &[now - 86_400]);
        let mut stale_drift = initiative_timeline("Drifted", &[now - 30 * 86_400]);
        stale_drift.atos_project = Some(AtosLink {
            kind: AtosLinkKind::Project,
            id: "drifted".into(),
            current_phase: Some(1),
            total_phases: Some(3),
            charter_status: CharterStatus::Drifted,
        });
        let (out, _) = format_strategic(
            &[recent_clean, stale_drift],
            &|_: &str| Vec::new(),
            &|_: &str| false,
            now,
            300,
        );
        let drifted_pos = out.find("Drifted").unwrap();
        let recent_pos = out.find("Recent").unwrap();
        assert!(
            drifted_pos < recent_pos,
            "ATOS drift must outrank pure recency in strategic digest:\n{out}"
        );
    }
}
