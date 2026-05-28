//! `sovereign awareness entities` — list extracted entities.
//!
//! Aggregates Entity atoms across both relational atlas dirs
//! (personal-knowledge + conversation-history), joins them with
//! NoteStore counts and FeatureStore-backed ATOS links, and renders
//! plain text or JSON. The "borderline initiative" flag surfaces
//! Initiative entities that look topic-shaped (single-conversation
//! provenance + hedge-word evidence) so the developer knows which
//! entries to scrutinise when tuning the extraction prompt.

use std::collections::{HashMap, HashSet};

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, Entity};
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeType};
use corpus_engine::enrichment::atlas::writer::{read_atlas_atoms, read_atlas_edges};
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use serde_json::json;

use sovereign_tools::knowledge_view::splice_extension::{
    relational_notes_for_entity, AtosSnapshot,
};
use sovereign_tools::knowledge_view::timeline::{AtosLink, AtosLinkKind, AtosLookup};

use super::args::{get_flag, has_flag, split_args};
use super::render::{display_path, format_date};
use super::store_open::{
    atlas_dir_for, project_toml_path, sovereign_root, try_open_features, try_open_notes,
};

/// Atlas view ids that the relational pipeline writes to. A multi-
/// or future-corpus addition would extend this list.
const RELATIONAL_VIEWS: &[&str] = &["personal-knowledge", "conversation-history"];

/// Hedge words that signal an "initiative" entity might actually be a
/// topic. Combined with single-conversation provenance, drives the
/// borderline flag.
const HEDGE_WORDS: &[&str] = &[
    "talked about",
    "discussed",
    "thinking about",
    "thought about",
    "considered",
    "wondering",
];

pub(super) async fn cmd_entities(args: &[String]) -> i32 {
    let (_pos, flags) = split_args(args);

    let kind_filter = get_flag(&flags, "kind").unwrap_or_else(|| "all".to_string());
    if !matches!(
        kind_filter.as_str(),
        "all" | "person" | "organization" | "initiative"
    ) {
        eprintln!(
            "awareness entities: --kind must be one of: person, organization, initiative, all (got '{kind_filter}')"
        );
        return 2;
    }

    let sort_mode = get_flag(&flags, "sort").unwrap_or_else(|| "name".to_string());
    if !matches!(sort_mode.as_str(), "name" | "recency" | "frequency") {
        eprintln!(
            "awareness entities: --sort must be one of: recency, frequency, name (got '{sort_mode}')"
        );
        return 2;
    }

    let json_out = has_flag(&flags, "json");

    let root = sovereign_root(&flags);

    // Read both atlas dirs. Either may be absent on a fresh install
    // — that's not an error, just an empty contribution.
    let mut all_entities: Vec<EntityRow> = Vec::new();
    let mut atlas_dirs_seen = Vec::new();
    let mut atlas_dirs_missing = Vec::new();

    for view_id in RELATIONAL_VIEWS {
        let atlas_dir = atlas_dir_for(&root, view_id);
        if !atlas_dir.exists() {
            atlas_dirs_missing.push(atlas_dir.clone());
            continue;
        }
        atlas_dirs_seen.push(atlas_dir.clone());

        let atoms_file = match read_atlas_atoms(&atlas_dir) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "awareness entities: failed to read {}/atoms.json: {e}",
                    display_path(&atlas_dir)
                );
                return 1;
            }
        };
        let edges_file = match read_atlas_edges(&atlas_dir) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "awareness entities: failed to read {}/edges.json: {e}",
                    display_path(&atlas_dir)
                );
                return 1;
            }
        };

        // Build a chunk-id → conversation/source bucket so we can
        // count "conversations across" without hitting the DB. Each
        // Involves edge contributes one (entity_id, chunk_id) pair.
        let entity_chunks = build_entity_chunk_map(&edges_file.edges);

        for atom in &atoms_file.atoms {
            let AtomEnvelope::Entity(e) = atom else {
                continue;
            };
            let kind = match classify(&e.entity_type) {
                Some(k) => k,
                None => continue,
            };
            let chunks: Vec<String> = entity_chunks
                .get(e.id.as_str())
                .cloned()
                .unwrap_or_default();
            all_entities.push(EntityRow::from_entity(
                e,
                kind,
                view_id.to_string(),
                chunks,
            ));
        }
    }

    // Filter by --kind.
    if kind_filter != "all" {
        let want = match kind_filter.as_str() {
            "person" => EntityKind::Person,
            "organization" => EntityKind::Organization,
            "initiative" => EntityKind::Initiative,
            _ => unreachable!(),
        };
        all_entities.retain(|r| r.kind == want);
    }

    // Resolve participant atom_ids → names across the union (an
    // Initiative's participants may be Person atoms in either atlas
    // file; the typed-id lookup handles that without merging atom
    // sources).
    let by_id: HashMap<String, String> = all_entities
        .iter()
        .map(|e| (e.atom_id.clone(), e.canonical_name.clone()))
        .collect();
    for e in all_entities.iter_mut() {
        e.participants = e
            .participant_ids
            .iter()
            .filter_map(|id| by_id.get(id).cloned())
            .collect();
    }

    // Note counts (commitment / follow_up / goal joined with
    // related_entity = canonical_name).
    if let Some(notes) = try_open_notes() {
        for e in all_entities.iter_mut() {
            let rows = relational_notes_for_entity(&notes, &e.canonical_name).await;
            // The splice helper already filters retired and groups
            // by kind via the enum — count by kind.
            for row in rows {
                use sovereign_tools::knowledge_view::relational::RelationalNoteKind;
                match row.kind {
                    RelationalNoteKind::Commitment => e.commitments += 1,
                    RelationalNoteKind::FollowUp => e.follow_ups += 1,
                    RelationalNoteKind::Goal => e.goals += 1,
                }
            }
        }
    }

    // ATOS link for Initiative entities.
    let toml_path = project_toml_path();
    let toml_path_opt = if toml_path.exists() {
        Some(toml_path.as_path())
    } else {
        None
    };
    let features = try_open_features();
    let atos = AtosSnapshot::build(features.as_ref(), toml_path_opt).await;
    for e in all_entities.iter_mut() {
        if e.kind != EntityKind::Initiative {
            continue;
        }
        let folded = e.canonical_name.trim().to_lowercase();
        if let Some(link) = atos.lookup(&folded) {
            e.atos = Some(link);
        }
    }

    // Borderline-initiative heuristic: Initiative entity, single
    // distinct chunk, hedge word in description.
    for e in all_entities.iter_mut() {
        if e.kind == EntityKind::Initiative {
            let unique: HashSet<&str> = e.chunks.iter().map(|s| s.as_str()).collect();
            if unique.len() <= 1 && contains_hedge(&e.description) {
                e.borderline = true;
            }
        }
    }

    // Sort.
    match sort_mode.as_str() {
        "name" => all_entities.sort_by(|a, b| a.canonical_name.cmp(&b.canonical_name)),
        "frequency" => all_entities.sort_by(|a, b| b.chunks.len().cmp(&a.chunks.len())),
        "recency" => {
            // Recency uses the lexicographic max of source chunk ids
            // as a proxy. Without timestamps in the atom file (those
            // come from the joined StateStore), this is a "later
            // chunk_id is later" heuristic — accurate when chunks
            // are issued in order, which they are for both
            // memory-write and conversation-message paths.
            all_entities.sort_by(|a, b| b.last_chunk().cmp(&a.last_chunk()));
        }
        _ => unreachable!(),
    }

    if json_out {
        emit_json(&all_entities, &atlas_dirs_seen, &atlas_dirs_missing);
    } else {
        emit_text(&all_entities, &atlas_dirs_seen, &atlas_dirs_missing);
    }
    0
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntityKind {
    Person,
    Organization,
    Initiative,
}

impl EntityKind {
    fn label(&self) -> &'static str {
        match self {
            EntityKind::Person => "person",
            EntityKind::Organization => "organization",
            EntityKind::Initiative => "initiative",
        }
    }
}

fn classify(t: &EntityType) -> Option<EntityKind> {
    match t {
        EntityType::Person => Some(EntityKind::Person),
        EntityType::Institution => Some(EntityKind::Organization),
        EntityType::Initiative => Some(EntityKind::Initiative),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct EntityRow {
    atom_id: String,
    canonical_name: String,
    kind: EntityKind,
    affiliation: Option<String>,
    role: Option<String>,
    description: String,
    /// View id this entity was observed in (`personal-knowledge` or
    /// `conversation-history`). When the same canonical name
    /// appears in both atlases, we currently emit two rows — atlas
    /// merging across views is out of scope for the inspector.
    source_view: String,
    /// Distinct source chunk ids touching this entity (the count is
    /// the "interactions" tally; the unique count is "conversations
    /// across").
    chunks: Vec<String>,
    /// Atom ids this Initiative declares as participants. Resolved
    /// to names in a second pass once the full atom set is known.
    participant_ids: Vec<String>,
    participants: Vec<String>,
    /// Note counts.
    commitments: usize,
    follow_ups: usize,
    goals: usize,
    /// ATOS link (Initiative only).
    atos: Option<AtosLink>,
    /// Borderline-initiative flag — set when the Initiative is
    /// suspected of being a topic (single-chunk + hedge wording).
    borderline: bool,
}

impl EntityRow {
    fn from_entity(e: &Entity, kind: EntityKind, source_view: String, chunks: Vec<String>) -> Self {
        Self {
            atom_id: e.id.as_str().to_string(),
            canonical_name: e.canonical_name.clone(),
            kind,
            affiliation: e.affiliation.clone(),
            role: e.role.clone(),
            description: e.description.clone(),
            source_view,
            chunks,
            participant_ids: e.participants.iter().map(|a| a.as_str().to_string()).collect(),
            participants: Vec::new(),
            commitments: 0,
            follow_ups: 0,
            goals: 0,
            atos: None,
            borderline: false,
        }
    }

    fn last_chunk(&self) -> &str {
        self.chunks.iter().map(|s| s.as_str()).max().unwrap_or("")
    }

    fn unique_conversation_count(&self) -> usize {
        self.chunks.iter().collect::<HashSet<_>>().len()
    }
}

fn build_entity_chunk_map(edges: &[Edge]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for edge in edges {
        if edge.edge_type != EdgeType::Involves {
            continue;
        }
        let chunk_id = if let Some(ev) = edge.evidence.first() {
            ev.chunk_id.clone()
        } else {
            edge.source
                .as_str()
                .strip_prefix("chunk-")
                .unwrap_or(edge.source.as_str())
                .to_string()
        };
        map.entry(edge.target.as_str().to_string())
            .or_default()
            .push(chunk_id);
    }
    map
}

fn contains_hedge(text: &str) -> bool {
    let lower = text.to_lowercase();
    HEDGE_WORDS.iter().any(|h| lower.contains(h))
}

fn emit_text(rows: &[EntityRow], seen: &[std::path::PathBuf], missing: &[std::path::PathBuf]) {
    if rows.is_empty() {
        eprintln!("(no entities extracted yet)");
        if !missing.is_empty() {
            eprintln!();
            eprintln!("Missing atlas dirs:");
            for d in missing {
                eprintln!("  · {}", display_path(d));
            }
            eprintln!();
            eprintln!("Run `sovereign awareness extract` (Phase 2) to populate them.");
        }
        return;
    }

    // Group by kind; preserve current sort order within each group.
    for kind in [
        EntityKind::Person,
        EntityKind::Organization,
        EntityKind::Initiative,
    ] {
        let group: Vec<&EntityRow> = rows.iter().filter(|r| r.kind == kind).collect();
        if group.is_empty() {
            continue;
        }
        let kind_label = kind.label();
        let plural = if group.len() == 1 { "y" } else { "ies" };
        println!();
        println!(
            "{}{} entit{} ({}):",
            kind_label.chars().next().unwrap().to_uppercase().collect::<String>(),
            &kind_label[1..],
            plural,
            group.len()
        );
        for r in group {
            print_row(r);
        }
    }

    let total = rows.len();
    let person_n = rows.iter().filter(|r| r.kind == EntityKind::Person).count();
    let org_n = rows.iter().filter(|r| r.kind == EntityKind::Organization).count();
    let init_n = rows
        .iter()
        .filter(|r| r.kind == EntityKind::Initiative)
        .count();
    let borderline_n = rows.iter().filter(|r| r.borderline).count();
    println!();
    println!(
        "Total: {total} entities ({person_n} person, {org_n} organization, {init_n} initiative)"
    );
    if borderline_n > 0 {
        println!("       {borderline_n} flagged as borderline initiative/topic");
    }
    if !seen.is_empty() {
        println!();
        println!("Atlas sources:");
        for d in seen {
            println!("  · {}", display_path(d));
        }
    }
    if !missing.is_empty() {
        println!();
        println!("Missing atlas dirs (run `awareness extract` to populate):");
        for d in missing {
            println!("  · {}", display_path(d));
        }
    }
}

fn print_row(r: &EntityRow) {
    let warn = if r.borderline {
        "  ⚠ borderline (topic?)"
    } else {
        ""
    };
    println!("  {}{}", r.canonical_name, warn);
    if let (Some(a), Some(role)) = (r.affiliation.as_deref(), r.role.as_deref()) {
        println!("    Affiliation: {a}, {role}");
    } else if let Some(a) = r.affiliation.as_deref() {
        println!("    Affiliation: {a}");
    } else if let Some(role) = r.role.as_deref() {
        println!("    Role: {role}");
    }
    let interactions = r.chunks.len();
    let convs = r.unique_conversation_count();
    println!(
        "    Interactions: {interactions} across {convs} conversation{}",
        if convs == 1 { "" } else { "s" }
    );
    if !r.participants.is_empty() {
        println!("    Participants: {}", r.participants.join(", "));
    }
    let n_total = r.commitments + r.follow_ups + r.goals;
    if n_total > 0 {
        println!(
            "    Linked notes: {} commitment{}, {} follow-up{}, {} goal{}",
            r.commitments,
            if r.commitments == 1 { "" } else { "s" },
            r.follow_ups,
            if r.follow_ups == 1 { "" } else { "s" },
            r.goals,
            if r.goals == 1 { "" } else { "s" },
        );
    }
    if let Some(link) = &r.atos {
        let kind = match link.kind {
            AtosLinkKind::Project => "project",
            AtosLinkKind::Feature => "feature",
        };
        let phase = match (link.current_phase, link.total_phases) {
            (Some(c), Some(t)) => format!("phase {c}/{t}"),
            (Some(c), None) => format!("phase {c}"),
            _ => "no phase".to_string(),
        };
        let charter = match link.charter_status {
            sovereign_tools::knowledge_view::timeline::CharterStatus::Clean => "Clean",
            sovereign_tools::knowledge_view::timeline::CharterStatus::Drifted => "Drifted",
            sovereign_tools::knowledge_view::timeline::CharterStatus::Unapproved => "Unapproved",
        };
        println!(
            "    ATOS link: {} \"{}\", {}, charter: {}",
            kind, link.id, phase, charter
        );
    }
    let preview: Vec<&str> = r.chunks.iter().take(5).map(|s| s.as_str()).collect();
    if !preview.is_empty() {
        let suffix = if r.chunks.len() > 5 {
            format!(", … ({} more)", r.chunks.len() - 5)
        } else {
            String::new()
        };
        println!(
            "    Source chunks: {}{}",
            preview.join(", "),
            suffix
        );
    }
    println!("    Source view: {}", r.source_view);
    println!();
}

fn emit_json(
    rows: &[EntityRow],
    seen: &[std::path::PathBuf],
    missing: &[std::path::PathBuf],
) {
    let entities: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let atos = r.atos.as_ref().map(|link| {
                json!({
                    "kind": match link.kind {
                        AtosLinkKind::Project => "project",
                        AtosLinkKind::Feature => "feature",
                    },
                    "id": link.id,
                    "current_phase": link.current_phase,
                    "total_phases": link.total_phases,
                    "charter_status": match link.charter_status {
                        sovereign_tools::knowledge_view::timeline::CharterStatus::Clean => "clean",
                        sovereign_tools::knowledge_view::timeline::CharterStatus::Drifted => "drifted",
                        sovereign_tools::knowledge_view::timeline::CharterStatus::Unapproved => "unapproved",
                    },
                })
            });
            json!({
                "atom_id": r.atom_id,
                "canonical_name": r.canonical_name,
                "kind": r.kind.label(),
                "affiliation": r.affiliation,
                "role": r.role,
                "description": r.description,
                "source_view": r.source_view,
                "interactions": r.chunks.len(),
                "conversations": r.unique_conversation_count(),
                "participants": r.participants,
                "linked_notes": {
                    "commitment": r.commitments,
                    "follow_up": r.follow_ups,
                    "goal": r.goals,
                },
                "atos_link": atos,
                "borderline": r.borderline,
                "source_chunks": r.chunks,
                "first_seen_chunk": r.chunks.iter().min(),
                "last_seen_chunk": r.chunks.iter().max(),
                "first_seen_date": format_date(None),
                "last_seen_date": format_date(None),
            })
        })
        .collect();

    let out = json!({
        "entities": entities,
        "atlas_dirs_present": seen.iter().map(|d| display_path(d)).collect::<Vec<_>>(),
        "atlas_dirs_missing": missing.iter().map(|d| display_path(d)).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::atoms::{AtomId, ChunkRef};
    use corpus_engine::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
    use corpus_engine::enrichment::pipeline::atlas::EnrichmentDepth;

    fn entity(idx: usize, name: &str, t: EntityType, description: &str) -> Entity {
        Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: t,
            first_appearance: ChunkRef::new("chunk-0".to_string(), None),
            description: description.into(),
            salience: 0.7,
            enrichment_depth: EnrichmentDepth::extracted_default(),
            affiliation: None,
            role: None,
            participants: Vec::new(),
                    provenance: Default::default(),
                    concept_kind: None,
}
}

    fn involves(idx: usize, target: &AtomId, chunk_id: &str) -> Edge {
        Edge {
            id: EdgeId::new(idx),
            edge_type: EdgeType::Involves,
            source: AtomId::from_raw(format!("chunk-{}", chunk_id)),
            target: target.clone(),
            evidence: vec![ChunkRef::new(chunk_id.to_string(), None)],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    #[test]
    fn classify_only_returns_relational_kinds() {
        assert_eq!(classify(&EntityType::Person), Some(EntityKind::Person));
        assert_eq!(
            classify(&EntityType::Institution),
            Some(EntityKind::Organization)
        );
        assert_eq!(
            classify(&EntityType::Initiative),
            Some(EntityKind::Initiative)
        );
        assert_eq!(classify(&EntityType::Concept), None);
    }

    #[test]
    fn build_entity_chunk_map_aggregates_per_target() {
        let id1 = AtomId::entity(1);
        let id2 = AtomId::entity(2);
        let edges = vec![
            involves(1, &id1, "100"),
            involves(2, &id1, "200"),
            involves(3, &id2, "100"),
        ];
        let map = build_entity_chunk_map(&edges);
        assert_eq!(map.get(id1.as_str()).map(|v| v.len()), Some(2));
        assert_eq!(map.get(id2.as_str()).map(|v| v.len()), Some(1));
    }

    #[test]
    fn build_entity_chunk_map_skips_non_involves_edges() {
        let id = AtomId::entity(1);
        let mut edges = vec![involves(1, &id, "100")];
        edges.push(Edge {
            id: EdgeId::new(99),
            edge_type: EdgeType::Grounds,
            source: AtomId::entity(7),
            target: id.clone(),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        });
        let map = build_entity_chunk_map(&edges);
        assert_eq!(map.get(id.as_str()).map(|v| v.len()), Some(1));
    }

    #[test]
    fn contains_hedge_detects_topic_signals() {
        assert!(contains_hedge("Talked about onboarding improvements"));
        assert!(contains_hedge("we DISCUSSED architecture"));
        assert!(!contains_hedge("Shipping the API migration by Friday"));
    }

    #[test]
    fn entity_row_unique_conversation_count() {
        let e = entity(1, "Sarah", EntityType::Person, "");
        let r = EntityRow::from_entity(
            &e,
            EntityKind::Person,
            "personal-knowledge".into(),
            vec!["c1".into(), "c1".into(), "c2".into()],
        );
        assert_eq!(r.chunks.len(), 3);
        assert_eq!(r.unique_conversation_count(), 2);
    }
}
