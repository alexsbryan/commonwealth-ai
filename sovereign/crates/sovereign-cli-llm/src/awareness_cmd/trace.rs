// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn awareness trace <entity-name>` — per-entity decision walk.
//!
//! Shows every decision point the awareness pipeline made about a
//! single entity: extraction (canonical name, aliases, source
//! chunks), merge resolution (folded names that collapsed), ATOS
//! link composition (Initiative only), interaction timeline, cross-
//! references, and the digest line as it would render on the next
//! turn. The deepest diagnostic per the spec.
//!
//! Phase 3 ships everything except the ranking-score breakdown,
//! which depends on the `format_*_with_scores` split (deferred).

use std::collections::HashSet;

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, Entity};
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeType};
use corpus_engine::enrichment::atlas::writer::{read_atlas_atoms, read_atlas_edges};
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use sovereign_tools::knowledge_view::splice_extension::{
    load_chunk_timestamps, relational_notes_for_entity, AtosSnapshot,
};
use sovereign_tools::knowledge_view::timeline::{
    assemble_timelines_from_atlas, AtosLink, AtosLinkKind, CharterStatus, Interaction,
    InteractionTimeline, TimelineEntityKind,
};

use super::args::parse_args;
use super::render::{display_path, format_datetime};
use super::store_open::{
    atlas_dir_for, project_toml_path, sovereign_root, state_db_path, try_open_features,
    try_open_notes,
};

const RELATIONAL_VIEWS: &[&str] = &["personal-knowledge", "conversation-history"];

pub(super) async fn cmd_trace(args: &[String]) -> i32 {
    let flags = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("awareness: {e}");
            return 2;
        }
    };
    let positional = flags.positionals();
    let Some(name) = positional.into_iter().next() else {
        eprintln!("awareness trace: <entity-name> is required");
        return 2;
    };

    let root = sovereign_root(&flags);

    // Read both atlases up front; we'll filter in memory.
    let mut atom_records: Vec<(String, Entity)> = Vec::new();
    let mut edges_by_view: Vec<(String, Vec<Edge>)> = Vec::new();
    for view_id in RELATIONAL_VIEWS {
        let atlas_dir = atlas_dir_for(&root, view_id);
        if !atlas_dir.exists() {
            continue;
        }
        let atoms_file = match read_atlas_atoms(&atlas_dir) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "awareness trace: failed to read {}/atoms.json: {e}",
                    display_path(&atlas_dir)
                );
                return 1;
            }
        };
        let edges_file = match read_atlas_edges(&atlas_dir) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "awareness trace: failed to read {}/edges.json: {e}",
                    display_path(&atlas_dir)
                );
                return 1;
            }
        };
        for atom in atoms_file.atoms {
            if let AtomEnvelope::Entity(e) = atom {
                atom_records.push((view_id.to_string(), e));
            }
        }
        edges_by_view.push((view_id.to_string(), edges_file.edges));
    }

    // Match. We do case-insensitive whole-word against canonical
    // name + aliases and surface near misses on no-match.
    let needle = name.trim().to_lowercase();
    let matches: Vec<&(String, Entity)> = atom_records
        .iter()
        .filter(|(_, e)| {
            let cn = e.canonical_name.trim().to_lowercase();
            cn == needle || e.aliases.iter().any(|a| a.trim().to_lowercase() == needle)
        })
        .collect();

    let entity = match matches.len() {
        0 => {
            eprintln!("awareness trace: no entity matches \"{name}\"");
            let near: Vec<&str> = atom_records
                .iter()
                .filter(|(_, e)| e.canonical_name.to_lowercase().contains(&needle))
                .map(|(_, e)| e.canonical_name.as_str())
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
            eprintln!("awareness trace: \"{name}\" matched {n} entities; tracing the first.");
            matches[0]
        }
    };
    let (source_view, target) = entity;

    // ATOS lookup (built once for downstream use).
    let toml_path = project_toml_path();
    let toml_path_opt = if toml_path.exists() {
        Some(toml_path.as_path())
    } else {
        None
    };
    let features = try_open_features();
    let atos = AtosSnapshot::build(features.as_ref(), toml_path_opt).await;

    // Find the timeline produced for this entity by the assembler so
    // we can show the same data the digest would see.
    let db_path = state_db_path(&root);
    let chunk_ts = load_chunk_timestamps(&db_path);
    let resolver = move |id: &str| -> Option<i64> { chunk_ts.get(id).copied() };
    let mut all_timelines: Vec<InteractionTimeline> = Vec::new();
    for view_id in RELATIONAL_VIEWS {
        let corpus_dir = root.join("indexes").join(view_id);
        if !atlas_dir_for(&root, view_id).exists() {
            continue;
        }
        match assemble_timelines_from_atlas(&corpus_dir, &resolver, &atos) {
            Ok(mut t) => all_timelines.append(&mut t),
            Err(e) => {
                eprintln!(
                    "awareness trace: failed to assemble {}: {e}",
                    display_path(&corpus_dir)
                );
                return 1;
            }
        }
    }
    let target_timeline = all_timelines
        .iter()
        .find(|t| t.entity_id == target.id.as_str())
        .cloned();

    // Linked notes.
    let linked_notes = if let Some(notes) = try_open_notes() {
        relational_notes_for_entity(&notes, &target.canonical_name).await
    } else {
        Vec::new()
    };

    // ── Render ─────────────────────────────────────────────────
    print_extraction(target, source_view, &edges_by_view);
    print_merging(target, &edges_by_view);
    if matches!(target.entity_type, EntityType::Initiative) {
        print_atos_section(&target.canonical_name, &atos);
    }
    if let Some(tl) = target_timeline.as_ref() {
        print_timeline_section(tl);
        print_cross_refs(&all_timelines, tl);
        print_digest_line(tl, &linked_notes);
    } else {
        println!();
        println!(
            "(timeline assembly produced no entry for this entity — atlas may be inconsistent)"
        );
    }
    println!();
    0
}

fn print_extraction(target: &Entity, source_view: &str, edges_by_view: &[(String, Vec<Edge>)]) {
    let kind = match target.entity_type {
        EntityType::Person => "Person",
        EntityType::Institution => "Organization",
        EntityType::Initiative => "Initiative",
        _ => "Other",
    };
    println!("Trace: {} ({})", target.canonical_name, kind);
    println!();
    println!("═══ Extraction ═══");
    println!();
    println!("Atom id: {}", target.id.as_str());
    println!("Canonical name: {}", target.canonical_name);
    if !target.aliases.is_empty() {
        println!("Aliases observed: {}", target.aliases.join(", "));
    }
    if let Some(a) = &target.affiliation {
        println!("Affiliation: {a}");
    }
    if let Some(r) = &target.role {
        println!("Role: {r}");
    }
    if !target.description.is_empty() {
        println!("Description: {}", target.description);
    }
    println!("Source view: {source_view}");
    println!(
        "First appearance: chunk {}",
        target.first_appearance.chunk_id
    );

    // Count edges across all views.
    let mut total_edges = 0usize;
    for (_view, edges) in edges_by_view {
        total_edges += edges
            .iter()
            .filter(|e| {
                e.edge_type == EdgeType::Involves && e.target.as_str() == target.id.as_str()
            })
            .count();
    }
    println!("Involves edges (across views): {total_edges}");
}

fn print_merging(target: &Entity, edges_by_view: &[(String, Vec<Edge>)]) {
    println!();
    println!("═══ Merging ═══");
    println!();
    let folded = target.canonical_name.trim().to_lowercase();
    println!("Canonical (folded): {folded}");
    if target.aliases.is_empty() {
        println!("Aliases collapsed into canonical: 0 (entity emerged with a single name)");
    } else {
        println!(
            "Aliases collapsed into canonical: {} ({})",
            target.aliases.len(),
            target.aliases.join(", ")
        );
    }
    let mut chunk_ids: HashSet<String> = HashSet::new();
    for (_view, edges) in edges_by_view {
        for edge in edges {
            if edge.edge_type != EdgeType::Involves {
                continue;
            }
            if edge.target.as_str() != target.id.as_str() {
                continue;
            }
            let cid = if let Some(ev) = edge.evidence.first() {
                ev.chunk_id.clone()
            } else {
                edge.source
                    .as_str()
                    .strip_prefix("chunk-")
                    .unwrap_or(edge.source.as_str())
                    .to_string()
            };
            chunk_ids.insert(cid);
        }
    }
    println!(
        "Distinct source chunks: {} ({})",
        chunk_ids.len(),
        if chunk_ids.is_empty() {
            "no Involves edges".to_string()
        } else {
            let mut v: Vec<&String> = chunk_ids.iter().collect();
            v.sort();
            let preview: Vec<&str> = v.iter().take(8).map(|s| s.as_str()).collect();
            let suffix = if chunk_ids.len() > 8 {
                format!(", … ({} more)", chunk_ids.len() - 8)
            } else {
                String::new()
            };
            format!("{}{}", preview.join(", "), suffix)
        }
    );
}

fn print_atos_section(canonical_name: &str, atos: &AtosSnapshot) {
    println!();
    println!("═══ ATOS Link ═══");
    println!();
    let folded = canonical_name.trim().to_lowercase();
    use sovereign_tools::knowledge_view::timeline::AtosLookup;
    match atos.lookup(&folded) {
        Some(link) => print_atos_link(&link),
        None => println!("No ATOS project / feature matches \"{canonical_name}\" — initiative renders without phase data."),
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
    println!("ATOS: {kind} \"{}\", {phase}, charter: {charter}", link.id);
}

fn print_timeline_section(tl: &InteractionTimeline) {
    println!();
    println!("═══ Timeline ═══");
    println!();
    if tl.interactions.is_empty() {
        println!("(no Involves edges)");
        return;
    }
    println!("Interactions ({}):", tl.interactions.len());
    for i in &tl.interactions {
        print_interaction(i);
    }
    if !tl.participants.is_empty() {
        println!();
        println!("Participants: {}", tl.participants.join(", "));
    }
}

fn print_interaction(i: &Interaction) {
    println!("  {}  {}", format_datetime(i.timestamp), i.source_chunk_id);
}

fn print_cross_refs(all: &[InteractionTimeline], target: &InteractionTimeline) {
    let target_chunks: HashSet<&str> = target
        .interactions
        .iter()
        .map(|i| i.source_chunk_id.as_str())
        .collect();

    let mut refs: Vec<String> = Vec::new();
    for t in all {
        if t.entity_id == target.entity_id {
            continue;
        }
        if t.participants.contains(&target.entity_name) {
            refs.push(format!(
                "  {} (declared participant on {})",
                kind_label(t.entity_kind),
                t.entity_name
            ));
            continue;
        }
        if target.participants.contains(&t.entity_name) {
            refs.push(format!(
                "  {} (this entity declares {} as participant)",
                kind_label(t.entity_kind),
                t.entity_name
            ));
            continue;
        }
        let shared = t
            .interactions
            .iter()
            .filter(|i| target_chunks.contains(i.source_chunk_id.as_str()))
            .count();
        if shared > 0 {
            refs.push(format!(
                "  {} {} (co-occurs in {} chunk{})",
                kind_label(t.entity_kind),
                t.entity_name,
                shared,
                if shared == 1 { "" } else { "s" }
            ));
        }
    }
    if !refs.is_empty() {
        println!();
        println!("═══ Cross-references ═══");
        println!();
        for r in refs {
            println!("{r}");
        }
    }
}

fn kind_label(k: TimelineEntityKind) -> &'static str {
    match k {
        TimelineEntityKind::Person => "person",
        TimelineEntityKind::Organization => "organization",
        TimelineEntityKind::Initiative => "initiative",
    }
}

fn print_digest_line(
    tl: &InteractionTimeline,
    linked_notes: &[sovereign_tools::knowledge_view::relational::RelationalNote],
) {
    println!();
    println!("═══ Digest preview ═══");
    println!();
    let mut head = String::new();
    head.push_str(&tl.entity_name);
    if let Some(a) = &tl.affiliation {
        head.push_str(&format!(" ({a})"));
    }
    let n = tl.interactions.len();
    head.push_str(&format!(
        " — {n} interaction{}",
        if n == 1 { "" } else { "s" }
    ));
    if let Some(last) = sovereign_tools::knowledge_view::timeline::last_seen_at(tl) {
        let date = chrono::DateTime::<chrono::Utc>::from_timestamp(last, 0)
            .map(|d| d.format("%b %d").to_string())
            .unwrap_or_else(|| "—".to_string());
        head.push_str(&format!(", last {date}"));
    }
    if let Some(link) = &tl.atos_project {
        let phase = match (link.current_phase, link.total_phases) {
            (Some(c), Some(t)) => format!(", ATOS phase {c}/{t}"),
            (Some(c), None) => format!(", ATOS phase {c}"),
            _ => String::new(),
        };
        head.push_str(&phase);
    }
    if !linked_notes.is_empty() {
        let summary: Vec<String> = linked_notes
            .iter()
            .map(|n| match n.kind {
                sovereign_tools::knowledge_view::relational::RelationalNoteKind::Commitment => {
                    "commitment".to_string()
                }
                sovereign_tools::knowledge_view::relational::RelationalNoteKind::FollowUp => {
                    "follow-up".to_string()
                }
                sovereign_tools::knowledge_view::relational::RelationalNoteKind::Goal => {
                    "goal".to_string()
                }
            })
            .collect();
        head.push_str(&format!("; {}", summary.join(", ")));
    }
    println!("- {head}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_label_returns_lowercase() {
        assert_eq!(kind_label(TimelineEntityKind::Person), "person");
        assert_eq!(kind_label(TimelineEntityKind::Organization), "organization");
        assert_eq!(kind_label(TimelineEntityKind::Initiative), "initiative");
    }
}
