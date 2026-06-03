//! `sovereign awareness timeline <entity-name>` — interaction history.
//!
//! Calls `assemble_timelines_from_atlas` per relational atlas dir,
//! picks the entity matching the given name (case-insensitive whole
//! match against canonical_name + aliases), joins the source chunks
//! to the StateStore for date + body lookup, and renders the timeline
//! plus linked notes plus cross-references.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use sovereign_tools::knowledge_view::splice_extension::{
    load_chunk_timestamps, relational_notes_for_entity, AtosSnapshot,
};
use sovereign_tools::knowledge_view::timeline::{
    assemble_timelines_from_atlas, AtosLink, AtosLinkKind, CharterStatus, Interaction,
    InteractionTimeline, TimelineEntityKind,
};

use super::args::{get_flag, has_flag, split_args};
use super::render::{display_path, format_date, format_datetime};
use super::store_open::{
    atlas_dir_for, project_toml_path, sovereign_root, state_db_path, try_open_features,
    try_open_notes,
};

const RELATIONAL_VIEWS: &[&str] = &["personal-knowledge", "conversation-history"];

pub(super) async fn cmd_timeline(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);

    let Some(name) = positional.into_iter().next() else {
        eprintln!("awareness timeline: <entity-name> is required");
        eprintln!(
            "usage: sovereign awareness timeline \"<name>\" [--window 90] [--include-chunks]"
        );
        return 2;
    };

    let window_days: i64 = match get_flag(&flags, "window") {
        Some(s) => match s.parse::<i64>() {
            Ok(n) if n > 0 => n,
            _ => {
                eprintln!("awareness timeline: --window must be a positive integer (got '{s}')");
                return 2;
            }
        },
        None => 90,
    };
    let include_chunks = has_flag(&flags, "include-chunks");

    let root = sovereign_root(&flags);

    // Build chunk-timestamp resolver from the user's state DB. This
    // is the same resolver the splice path uses; it short-circuits to
    // an empty map when the DB is missing.
    let db_path = state_db_path(&root);
    let chunk_ts = load_chunk_timestamps(&db_path);
    let resolver = move |id: &str| -> Option<i64> { chunk_ts.get(id).copied() };

    // ATOS lookup for Initiative entities.
    let toml_path = project_toml_path();
    let toml_path_opt = if toml_path.exists() {
        Some(toml_path.as_path())
    } else {
        None
    };
    let features = try_open_features();
    let atos = AtosSnapshot::build(features.as_ref(), toml_path_opt).await;

    // Walk both atlas dirs; collect every timeline.
    let mut all_timelines: Vec<InteractionTimeline> = Vec::new();
    for view_id in RELATIONAL_VIEWS {
        let corpus_dir = root.join("indexes").join(view_id);
        if !atlas_dir_for(&root, view_id).exists() {
            continue;
        }
        match assemble_timelines_from_atlas(&corpus_dir, &resolver, &atos) {
            Ok(mut tls) => all_timelines.append(&mut tls),
            Err(e) => {
                eprintln!(
                    "awareness timeline: failed to assemble {}: {e}",
                    display_path(&corpus_dir)
                );
                return 1;
            }
        }
    }

    if all_timelines.is_empty() {
        eprintln!("(no entities extracted yet — run `awareness extract` first)");
        return 0;
    }

    // Match by canonical name (case-insensitive whole match). Picks
    // the first hit; warns when more than one matches.
    let needle = name.trim().to_lowercase();
    let matches: Vec<&InteractionTimeline> = all_timelines
        .iter()
        .filter(|t| t.entity_name.trim().to_lowercase() == needle)
        .collect();

    let timeline = match matches.len() {
        0 => {
            eprintln!("awareness timeline: no entity matches \"{name}\"");
            // Suggest near misses — names that contain the needle as
            // a substring.
            let near: Vec<&str> = all_timelines
                .iter()
                .filter(|t| t.entity_name.to_lowercase().contains(&needle))
                .map(|t| t.entity_name.as_str())
                .collect();
            if !near.is_empty() {
                eprintln!("did you mean:");
                for n in near.iter().take(5) {
                    eprintln!("  · {n}");
                }
            }
            return 1;
        }
        1 => matches[0],
        n => {
            eprintln!(
                "awareness timeline: \"{name}\" matched {n} entities; using the first. \
                 Run `awareness entities` to inspect all candidates."
            );
            matches[0]
        }
    };

    // Window filter — keep only interactions within the last N days
    // of the most recent grounded timestamp.
    let now = unix_now();
    let window_floor = now.saturating_sub(window_days * 86_400);
    let mut filtered_inter: Vec<&Interaction> = timeline
        .interactions
        .iter()
        .filter(|i| match i.timestamp {
            Some(ts) => ts >= window_floor,
            None => true, // keep ungrounded — they sort to the end
        })
        .collect();
    filtered_inter.sort_by_key(|i| i.timestamp.unwrap_or(i64::MAX));

    // Pre-fetch chunk text if --include-chunks. Hits the same SQLite
    // file the StateStore writes to; one batched query.
    let chunk_text = if include_chunks {
        load_chunk_text(
            &db_path,
            filtered_inter
                .iter()
                .map(|i| i.source_chunk_id.as_str())
                .collect(),
        )
    } else {
        HashMap::new()
    };

    // Linked notes via NoteStore.
    let linked_notes = if let Some(notes) = try_open_notes() {
        relational_notes_for_entity(&notes, &timeline.entity_name).await
    } else {
        Vec::new()
    };

    // Cross-references: every other timeline that shares a chunk_id
    // with this entity, or lists this entity as a participant.
    let cross_refs = collect_cross_refs(&all_timelines, timeline);

    // Render.
    print_header(timeline, window_days);
    print_interactions(&filtered_inter, &chunk_text, include_chunks);
    print_linked_notes(&linked_notes, now);
    print_cross_refs(&cross_refs);
    println!();
    0
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn load_chunk_text(db_path: &Path, ids: Vec<&str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return out;
    }
    let Ok(conn) = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return out;
    };

    // Memories: chunk_id == memories.id, body in memories.content.
    if let Ok(mut stmt) = conn.prepare("SELECT id, content FROM memories WHERE id = ?1") {
        for id in &ids {
            if let Ok(row) = stmt.query_row([*id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            }) {
                out.insert(row.0, row.1);
            }
        }
    }

    // Conversation messages: the chunk_id may be a conversation id;
    // pull the most recent message body as the representative text.
    // Awareness is a glassbox, not a brief — exact-message lookup
    // would require message ids in the atlas, which they aren't.
    if let Ok(mut stmt) = conn.prepare(
        "SELECT body FROM messages WHERE conversation_id = ?1 ORDER BY created_at DESC LIMIT 1",
    ) {
        for id in &ids {
            if out.contains_key(*id) {
                continue;
            }
            if let Ok(body) = stmt.query_row([*id], |r| r.get::<_, String>(0)) {
                out.insert((*id).to_string(), body);
            }
        }
    }

    out
}

fn collect_cross_refs(
    all: &[InteractionTimeline],
    self_timeline: &InteractionTimeline,
) -> Vec<CrossRef> {
    let self_chunks: HashSet<&str> = self_timeline
        .interactions
        .iter()
        .map(|i| i.source_chunk_id.as_str())
        .collect();

    let mut out = Vec::new();
    for t in all {
        if t.entity_id == self_timeline.entity_id {
            continue;
        }
        // Participant-driven: this entity declares the other, or
        // vice versa.
        if t.participants.contains(&self_timeline.entity_name) {
            out.push(CrossRef::Participant(t.entity_name.clone(), t.entity_kind));
            continue;
        }
        if self_timeline.participants.contains(&t.entity_name) {
            out.push(CrossRef::Participant(t.entity_name.clone(), t.entity_kind));
            continue;
        }
        // Co-occurrence: shared chunk_id.
        let shared = t
            .interactions
            .iter()
            .filter(|i| self_chunks.contains(i.source_chunk_id.as_str()))
            .count();
        if shared > 0 {
            out.push(CrossRef::CoOccurrence(
                t.entity_name.clone(),
                t.entity_kind,
                shared,
            ));
        }
    }
    out
}

#[derive(Debug)]
enum CrossRef {
    Participant(String, TimelineEntityKind),
    CoOccurrence(String, TimelineEntityKind, usize),
}

fn kind_label(k: TimelineEntityKind) -> &'static str {
    match k {
        TimelineEntityKind::Person => "person",
        TimelineEntityKind::Organization => "organization",
        TimelineEntityKind::Initiative => "initiative",
    }
}

fn print_header(t: &InteractionTimeline, window_days: i64) {
    let kind = match t.entity_kind {
        TimelineEntityKind::Person => "Person",
        TimelineEntityKind::Organization => "Organization",
        TimelineEntityKind::Initiative => "Initiative",
    };
    let mut header = format!("Timeline: {} ({}", t.entity_name, kind);
    if let Some(a) = &t.affiliation {
        header.push_str(&format!(", {a}"));
    }
    if let Some(r) = &t.role {
        header.push_str(&format!(", {r}"));
    }
    header.push(')');
    println!("{header}");

    let now = unix_now();
    let from = now.saturating_sub(window_days * 86_400);
    println!(
        "Window: {} days ({} → {})",
        window_days,
        format_date(Some(from)),
        format_date(Some(now))
    );

    if !t.participants.is_empty() {
        println!();
        println!("Participants: {}", t.participants.join(", "));
    }
    if let Some(link) = &t.atos_project {
        println!();
        print_atos_link(link);
    }
}

fn print_atos_link(link: &AtosLink) {
    let kind = match link.kind {
        AtosLinkKind::Project => "project",
        AtosLinkKind::Feature => "feature",
    };
    let phase = match (link.current_phase, link.total_phases) {
        (Some(c), Some(t)) => format!("phase {c}/{t}"),
        (Some(c), None) => format!("phase {c}"),
        _ => "no phase data".to_string(),
    };
    let charter = match link.charter_status {
        CharterStatus::Clean => "Clean",
        CharterStatus::Drifted => "Drifted",
        CharterStatus::Unapproved => "Unapproved",
    };
    println!(
        "ATOS: {} \"{}\", {}, charter: {}",
        kind, link.id, phase, charter
    );
}

fn print_interactions(
    interactions: &[&Interaction],
    chunk_text: &HashMap<String, String>,
    include_chunks: bool,
) {
    println!();
    if interactions.is_empty() {
        println!("(no interactions in window)");
        return;
    }
    println!("Interactions ({}):", interactions.len());
    for i in interactions {
        let date = format_datetime(i.timestamp);
        println!("  {}  {}", date, i.source_chunk_id);
        if include_chunks {
            if let Some(body) = chunk_text.get(&i.source_chunk_id) {
                let preview = preview(body, 200);
                println!("                    {preview}");
            }
        }
    }
}

fn print_linked_notes(
    notes: &[sovereign_tools::knowledge_view::relational::RelationalNote],
    now: i64,
) {
    if notes.is_empty() {
        return;
    }
    println!();
    println!("Linked notes:");
    for n in notes {
        let kind = match n.kind {
            sovereign_tools::knowledge_view::relational::RelationalNoteKind::Commitment => {
                "commitment"
            }
            sovereign_tools::knowledge_view::relational::RelationalNoteKind::FollowUp => {
                "follow_up"
            }
            sovereign_tools::knowledge_view::relational::RelationalNoteKind::Goal => "goal",
        };
        let date = format_date(Some(n.anchor_timestamp));
        let age_days = (now.saturating_sub(n.anchor_timestamp)) / 86_400;
        println!("  {} ({}): {}", kind, date, n.summary);
        println!(
            "    Status: outstanding ({} day{})",
            age_days,
            if age_days == 1 { "" } else { "s" }
        );
    }
}

fn print_cross_refs(refs: &[CrossRef]) {
    if refs.is_empty() {
        return;
    }
    println!();
    println!("Cross-references:");
    for r in refs {
        match r {
            CrossRef::Participant(name, kind) => {
                println!("  {}: {} (declared participant)", kind_label(*kind), name);
            }
            CrossRef::CoOccurrence(name, kind, count) => {
                println!(
                    "  {}: {} (co-occurs in {} chunk{})",
                    kind_label(*kind),
                    name,
                    count,
                    if *count == 1 { "" } else { "s" }
                );
            }
        }
    }
}

fn preview(s: &str, max_chars: usize) -> String {
    let single_line: String = s.replace('\n', " ").chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        format!("{}…", single_line.trim_end())
    } else {
        single_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_truncates_with_ellipsis() {
        assert_eq!(preview("short", 100), "short");
        let long = "x".repeat(120);
        let p = preview(&long, 50);
        assert!(p.ends_with('…'));
        assert!(p.chars().count() <= 51);
    }

    #[test]
    fn preview_collapses_newlines() {
        assert_eq!(preview("a\nb", 10), "a b");
    }

    #[test]
    fn collect_cross_refs_finds_participant_and_co_occurrence() {
        use sovereign_tools::knowledge_view::timeline::Interaction;

        let sarah = InteractionTimeline {
            entity_id: "entity-0001".into(),
            entity_name: "Sarah".into(),
            entity_kind: TimelineEntityKind::Person,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            interactions: vec![
                Interaction {
                    timestamp: Some(100),
                    source_chunk_id: "c1".into(),
                },
                Interaction {
                    timestamp: Some(200),
                    source_chunk_id: "c2".into(),
                },
            ],
            atos_project: None,
        };
        let api = InteractionTimeline {
            entity_id: "entity-0002".into(),
            entity_name: "API migration".into(),
            entity_kind: TimelineEntityKind::Initiative,
            affiliation: None,
            role: None,
            participants: vec!["Sarah".into()],
            interactions: vec![Interaction {
                timestamp: Some(300),
                source_chunk_id: "c3".into(),
            }],
            atos_project: None,
        };
        let mike = InteractionTimeline {
            entity_id: "entity-0003".into(),
            entity_name: "Mike".into(),
            entity_kind: TimelineEntityKind::Person,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            interactions: vec![Interaction {
                timestamp: Some(150),
                source_chunk_id: "c1".into(),
            }],
            atos_project: None,
        };
        let all = vec![sarah.clone(), api, mike];
        let refs = collect_cross_refs(&all, &sarah);
        // API migration should be Participant; Mike should be CoOccurrence.
        assert!(refs.iter().any(|r| matches!(
            r,
            CrossRef::Participant(name, _) if name == "API migration"
        )));
        assert!(refs.iter().any(|r| matches!(
            r,
            CrossRef::CoOccurrence(name, _, count) if name == "Mike" && *count == 1
        )));
    }
}
