//! Phase 4.B — splice-path integration for the Relational + Strategic
//! digest blocks.
//!
//! Holds the cross-store glue that the rest of the knowledge_view
//! module deliberately keeps out of the formatters and the timeline
//! assembler. Concretely:
//!
//!   1. **chunk_timestamp resolver** — joins atlas `chunk-id` strings
//!      back to `memories.last_used` and `conversations.updated_at` so
//!      the timeline assembler can place each interaction in time.
//!   2. **NoteStore lookup** — answers "what `commitment`/`follow_up`
//!      notes are anchored to this entity name" for the relational
//!      block; the `goal`-kind variant feeds the strategic block.
//!   3. **AtosLookup** — composes the `Initiative` entity name against
//!      the local `project.toml` + `FeatureStore` so the strategic
//!      digest can render "ATOS project phase 2/4 (drift)".
//!   4. **in-conversation predicate** — checks whether an entity name
//!      appears in any message of the current `ConversationContext`.
//!
//! All four are pure functions plus small struct wrappers — the
//! manager owns the I/O paths and calls into this module at splice
//! time. Formatters and the timeline assembler stay free of database
//! handles.
//!
//! Gated behind the `treesitter` cargo feature alongside `corpus-engine`'s
//! `notes` and `features` modules — both are required for the lookups.

#![cfg(feature = "treesitter")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms::AtomEnvelope;
use corpus_engine::enrichment::atlas::writer::{read_atlas_atoms, ATLAS_DIRNAME};
use corpus_engine::features::FeatureStore;
use corpus_engine::notes::NoteStore;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use sovereign_core::memory::EntityInventory;

use crate::knowledge_view::relational::{RelationalNote, RelationalNoteKind};
use crate::knowledge_view::strategic::StrategicGoal;
use crate::knowledge_view::timeline::{
    AtosLink, AtosLinkKind, AtosLookup, CharterStatus,
};

// ── Chunk-timestamp resolver ────────────────────────────────────

/// Read every `(memory.id, last_used)` and `(conversation.id,
/// updated_at)` pair from the sovereign state DB into a flat map.
/// Returns an empty map on any I/O / SQL error — the assembler
/// handles missing timestamps by sinking those interactions to the
/// end of the timeline.
///
/// Cost is one SELECT per table; on a typical DB (a few thousand
/// memories, a hundred-or-so conversations) this is < 5 ms. Cheaper
/// than per-chunk lookups which would hit the page cache repeatedly.
///
/// Public so the `sovereign awareness` glassbox CLI can resolve
/// timestamps the same way the splice path does.
pub fn load_chunk_timestamps(db_path: &Path) -> HashMap<String, i64> {
    let mut map = HashMap::new();
    let Ok(conn) = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return map;
    };

    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, last_used FROM memories WHERE deleted_at IS NULL",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        }) {
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, updated_at FROM conversations WHERE deleted_at IS NULL",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        }) {
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
        }
    }

    map
}

// ── NoteStore relational + strategic adapters ───────────────────

/// Map an active commitment / follow_up note to the relational
/// digest's `RelationalNote` payload. `created_at` becomes the
/// anchor timestamp — the relational formatter uses it for the
/// "(noted Mar 14)" / "(overdue)" annotations.
pub async fn relational_notes_for_entity(
    notes: &NoteStore,
    entity_name: &str,
) -> Vec<RelationalNote> {
    let kinds: &[&str] = &["commitment", "follow_up", "goal"];
    let rows = match notes
        .read_notes_by_related_entity(entity_name, kinds)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(
                entity = entity_name,
                error = %e,
                "splice: notes-by-entity query failed; treating as empty"
            );
            return Vec::new();
        }
    };
    rows.into_iter()
        .filter_map(|row| {
            let kind = match row.kind.as_str() {
                "commitment" => RelationalNoteKind::Commitment,
                "follow_up" => RelationalNoteKind::FollowUp,
                "goal" => RelationalNoteKind::Goal,
                _ => return None,
            };
            Some(RelationalNote {
                kind,
                anchor_timestamp: parse_rfc3339_to_unix(&row.created_at),
                summary: shorten_summary(&row.content),
            })
        })
        .collect()
}

/// Same NoteStore query, narrowed to `goal` kinds and shaped for
/// the strategic digest.
pub async fn strategic_goals_for_entity(
    notes: &NoteStore,
    entity_name: &str,
) -> Vec<StrategicGoal> {
    let rows = match notes
        .read_notes_by_related_entity(entity_name, &["goal"])
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    rows.into_iter()
        .map(|row| StrategicGoal {
            created_at: parse_rfc3339_to_unix(&row.created_at),
            summary: shorten_summary(&row.content),
        })
        .collect()
}

/// Parse an RFC 3339 timestamp string (the shape NoteStore writes)
/// into a unix-seconds i64. Falls back to 0 on parse failure — the
/// digest formatters tolerate stale anchors and the splice path
/// already logs the underlying parse error.
fn parse_rfc3339_to_unix(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

/// Trim a note's body to a one-line summary suitable for the
/// digest. The full content lives in the NoteStore — the digest only
/// needs a fragment, otherwise a long commitment would blow the
/// budget on a single line.
fn shorten_summary(content: &str) -> String {
    const MAX_CHARS: usize = 80;
    let line = content.lines().next().unwrap_or("").trim();
    if line.chars().count() <= MAX_CHARS {
        return line.to_string();
    }
    let truncated: String = line.chars().take(MAX_CHARS).collect();
    format!("{}…", truncated.trim_end())
}

// ── ATOS lookup composition ─────────────────────────────────────

/// Concrete `AtosLookup` built from a snapshot of the local
/// `FeatureStore` + `project.toml` lifecycle state.
///
/// The snapshot is rebuilt per splice (cheap — a single SELECT on
/// `features` + per-feature SELECT on `feature_milestones`). We
/// materialise it eagerly because the underlying calls are async
/// while the `AtosLookup` trait is sync — pre-loading lets the
/// formatter stay synchronous.
pub struct AtosSnapshot {
    project: Option<ProjectMatch>,
    features: Vec<FeatureMatch>,
}

struct ProjectMatch {
    folded_name: String,
    current_phase: Option<u32>,
    charter_status: CharterStatus,
}

struct FeatureMatch {
    id: String,
    folded_keys: Vec<String>,
    current_phase: Option<u32>,
    total_phases: Option<u32>,
}

impl AtosSnapshot {
    /// Empty snapshot — yields no matches. Used when the caller has
    /// no `project.toml` or no `features.db` configured.
    pub fn empty() -> Self {
        Self {
            project: None,
            features: Vec::new(),
        }
    }

    /// Build a snapshot. `project_toml_path` may point at a missing
    /// file (e.g. the user hasn't run `sovereign project init`); the
    /// snapshot then carries no project entry but still surfaces any
    /// features. `features` is the live FeatureStore handle (already
    /// async-compatible — caller awaits the listing).
    pub async fn build(
        features: Option<&Arc<FeatureStore>>,
        project_toml_path: Option<&Path>,
    ) -> Self {
        let project = project_toml_path.and_then(load_project_match);

        let mut feature_matches = Vec::new();
        if let Some(store) = features {
            if let Ok(rows) = store.list(false).await {
                for row in rows {
                    let milestones = store.list_milestones(&row.id).await.unwrap_or_default();
                    let total = if milestones.is_empty() {
                        None
                    } else {
                        Some(milestones.len() as u32)
                    };
                    let current = milestones
                        .iter()
                        .filter(|m| m.started_at.is_some())
                        .map(|m| m.ordinal as u32)
                        .max();
                    let folded_keys: Vec<String> = [&row.id, &row.title]
                        .into_iter()
                        .map(|s| fold_name(s))
                        .filter(|s| !s.is_empty())
                        .collect();
                    feature_matches.push(FeatureMatch {
                        id: row.id,
                        folded_keys,
                        current_phase: current,
                        total_phases: total,
                    });
                }
            }
        }

        Self {
            project,
            features: feature_matches,
        }
    }
}

impl AtosLookup for AtosSnapshot {
    fn lookup(&self, name: &str) -> Option<AtosLink> {
        if let Some(p) = &self.project {
            if p.folded_name == name {
                return Some(AtosLink {
                    kind: AtosLinkKind::Project,
                    id: p.folded_name.clone(),
                    current_phase: p.current_phase,
                    total_phases: None,
                    charter_status: p.charter_status,
                });
            }
        }
        for f in &self.features {
            if f.folded_keys.iter().any(|k| k == name) {
                return Some(AtosLink {
                    kind: AtosLinkKind::Feature,
                    id: f.id.clone(),
                    current_phase: f.current_phase,
                    total_phases: f.total_phases,
                    charter_status: CharterStatus::Unapproved,
                });
            }
        }
        None
    }
}

/// Read project.toml; return a `ProjectMatch` when the file is
/// present and the lifecycle section carries enough to render an
/// AtosLink. Drift status compares the current `CHARTER.md` SHA-256
/// against `lifecycle.charter_hash` — same algorithm as
/// `sovereign-atos::approval::detect_drift` but inlined here so the
/// splice path doesn't pull in the `sovereign-atos` crate.
fn load_project_match(project_toml_path: &Path) -> Option<ProjectMatch> {
    let body = std::fs::read_to_string(project_toml_path).ok()?;
    let parsed: toml::Value = toml::from_str(&body).ok()?;

    let project_name = parsed
        .get("project")
        .and_then(|t| t.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            // Same fallback as `ProjectTomlFile::read_with_name_fallback`:
            // parent of `.sovereign/`. Keeps behaviour consistent for
            // pre-v2 files that omit the `[project]` section.
            project_toml_path
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty())?;

    let lifecycle = parsed.get("lifecycle");
    let current_phase = lifecycle
        .and_then(|t| t.get("current_phase"))
        .and_then(|v| v.as_integer())
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0);
    let charter_hash = lifecycle
        .and_then(|t| t.get("charter_hash"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let charter_status = match charter_hash {
        None => CharterStatus::Unapproved,
        Some(approved_hash) => {
            // Look for CHARTER.md adjacent to project.toml (.sovereign/).
            let charter_path = project_toml_path
                .parent()
                .map(|p| p.join("CHARTER.md"))
                .unwrap_or_else(|| PathBuf::from("CHARTER.md"));
            match std::fs::read(&charter_path) {
                Ok(bytes) => {
                    let mut hasher = Sha256::new();
                    hasher.update(&bytes);
                    let current_hash = format!("{:x}", hasher.finalize());
                    if current_hash == approved_hash {
                        CharterStatus::Clean
                    } else {
                        CharterStatus::Drifted
                    }
                }
                Err(_) => CharterStatus::Unapproved,
            }
        }
    };

    Some(ProjectMatch {
        folded_name: fold_name(&project_name),
        current_phase,
        charter_status,
    })
}

fn fold_name(s: &str) -> String {
    s.trim().to_lowercase()
}

// ── Entity inventory assembly ───────────────────────────────────

/// Read every Entity atom's `canonical_name` + aliases across the two
/// relational atlas dirs (`personal-knowledge` + `conversation-history`),
/// fold to lowercase, and return as an `EntityInventory` (HashSet).
///
/// Used by:
///   1. `KnowledgeViewManager::entity_inventory_from_atlases` to
///      produce the inventory the runtime hands to the memory-decay
///      path on each pruning cycle.
///   2. `sovereign awareness decay` (Phase 3) — the development CLI
///      surfaces "what survives entity-aware decay vs uniform" by
///      passing this inventory into `apply_confidence_decay_with_rate_and_inventory`.
///
/// Returns an empty set when both atlases are absent — the caller
/// treats "no inventory" as "uniform decay" (the
/// `Option<&EntityInventory>` argument signals this with `None`).
pub fn build_entity_inventory(index_dir: &Path) -> EntityInventory {
    let mut inv = EntityInventory::new();
    for view_id in ["personal-knowledge", "conversation-history"] {
        let atlas_dir = index_dir.join(view_id).join(ATLAS_DIRNAME);
        if !atlas_dir.exists() {
            continue;
        }
        let Ok(atoms_file) = read_atlas_atoms(&atlas_dir) else {
            continue;
        };
        for atom in &atoms_file.atoms {
            if let AtomEnvelope::Entity(e) = atom {
                let name = e.canonical_name.trim();
                if !name.is_empty() {
                    inv.insert(name.to_lowercase());
                }
                for alias in &e.aliases {
                    let a = alias.trim();
                    if !a.is_empty() {
                        inv.insert(a.to_lowercase());
                    }
                }
            }
        }
    }
    inv
}

// ── In-conversation predicate ───────────────────────────────────

/// Lowercased message bodies for the current conversation. The
/// `format_relational` and `format_strategic` formatters call this
/// once per entity name, so we precompute the lowercased text once
/// per splice.
pub struct ConversationCorpus {
    lowered_messages: Vec<String>,
}

impl ConversationCorpus {
    pub fn from_messages<I, S>(messages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            lowered_messages: messages
                .into_iter()
                .map(|m| m.as_ref().to_lowercase())
                .collect(),
        }
    }

    pub fn contains_entity(&self, entity_name: &str) -> bool {
        let needle = entity_name.trim().to_lowercase();
        if needle.is_empty() {
            return false;
        }
        self.lowered_messages
            .iter()
            .any(|m| contains_whole_word(m, &needle))
    }
}

/// Whole-word (case-insensitive, alphanumeric-bounded) substring
/// match. Same shape as the memory-decay entity matcher: prevents
/// "Sarah" from matching "Sarahkov".
fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    let bytes_h = haystack.as_bytes();
    let bytes_n = needle.as_bytes();
    if bytes_n.is_empty() || bytes_n.len() > bytes_h.len() {
        return false;
    }
    let mut i = 0usize;
    while i + bytes_n.len() <= bytes_h.len() {
        if &bytes_h[i..i + bytes_n.len()] == bytes_n {
            let before_ok = i == 0
                || !bytes_h[i - 1].is_ascii_alphanumeric()
                    && bytes_h[i - 1] != b'_';
            let after_idx = i + bytes_n.len();
            let after_ok = after_idx >= bytes_h.len()
                || !bytes_h[after_idx].is_ascii_alphanumeric()
                    && bytes_h[after_idx] != b'_';
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_summary_truncates_with_ellipsis() {
        let long = "x".repeat(120);
        let s = shorten_summary(&long);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= 81);
    }

    #[test]
    fn shorten_summary_keeps_first_line_only() {
        assert_eq!(
            shorten_summary("first\nsecond\nthird"),
            "first"
        );
    }

    #[test]
    fn fold_name_lowers_and_trims() {
        assert_eq!(fold_name("  API Migration  "), "api migration");
    }

    #[test]
    fn conversation_corpus_matches_entity_whole_word() {
        let corpus = ConversationCorpus::from_messages([
            "I'm meeting Sarah tomorrow.",
            "We discussed the API migration plan.",
        ]);
        assert!(corpus.contains_entity("Sarah"));
        assert!(corpus.contains_entity("API migration"));
        assert!(!corpus.contains_entity("Sarahkov"));
    }

    #[test]
    fn contains_whole_word_respects_word_boundaries() {
        assert!(contains_whole_word("hello sarah world", "sarah"));
        assert!(contains_whole_word("sarah is here", "sarah"));
        assert!(contains_whole_word("here is sarah", "sarah"));
        assert!(!contains_whole_word("sarahkov", "sarah"));
        assert!(!contains_whole_word("oversaraherror", "sarah"));
    }

    #[test]
    fn atos_snapshot_empty_returns_no_matches() {
        let snap = AtosSnapshot::empty();
        assert!(snap.lookup("anything").is_none());
    }

    #[test]
    fn atos_snapshot_matches_project_name_after_folding() {
        let snap = AtosSnapshot {
            project: Some(ProjectMatch {
                folded_name: "api migration".into(),
                current_phase: Some(2),
                charter_status: CharterStatus::Clean,
            }),
            features: Vec::new(),
        };
        let link = snap.lookup("api migration").unwrap();
        assert_eq!(link.kind, AtosLinkKind::Project);
        assert_eq!(link.current_phase, Some(2));
        assert_eq!(link.charter_status, CharterStatus::Clean);

        assert!(snap.lookup("unrelated").is_none());
    }

    #[test]
    fn atos_snapshot_matches_feature_id_or_title() {
        let snap = AtosSnapshot {
            project: None,
            features: vec![FeatureMatch {
                id: "knowledge-view-relational".into(),
                folded_keys: vec![
                    "knowledge-view-relational".into(),
                    "relational and strategic awareness".into(),
                ],
                current_phase: Some(3),
                total_phases: Some(8),
            }],
        };
        let by_id = snap.lookup("knowledge-view-relational").unwrap();
        assert_eq!(by_id.kind, AtosLinkKind::Feature);
        assert_eq!(by_id.current_phase, Some(3));
        assert_eq!(by_id.total_phases, Some(8));

        let by_title = snap
            .lookup("relational and strategic awareness")
            .unwrap();
        assert_eq!(by_title.kind, AtosLinkKind::Feature);
        assert_eq!(by_title.id, "knowledge-view-relational");
    }

    #[test]
    fn load_project_match_returns_unapproved_when_charter_hash_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let sov_dir = tmp.path().join(".sovereign");
        std::fs::create_dir_all(&sov_dir).unwrap();
        let path = sov_dir.join("project.toml");
        std::fs::write(
            &path,
            "schema_version = 2\n\n[project]\nname = \"my-app\"\n\n[lifecycle]\ncurrent_phase = 4\n",
        )
        .unwrap();
        let m = load_project_match(&path).unwrap();
        assert_eq!(m.folded_name, "my-app");
        assert_eq!(m.current_phase, Some(4));
        assert_eq!(m.charter_status, CharterStatus::Unapproved);
    }

    #[test]
    fn load_project_match_drift_when_charter_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let sov_dir = tmp.path().join(".sovereign");
        std::fs::create_dir_all(&sov_dir).unwrap();
        let charter_path = sov_dir.join("CHARTER.md");
        std::fs::write(&charter_path, "v1 charter").unwrap();
        // Hash of "v1 charter" placed in lifecycle.charter_hash; then
        // mutate the file to force drift.
        let approved = {
            let mut h = Sha256::new();
            h.update(b"v1 charter");
            format!("{:x}", h.finalize())
        };
        std::fs::write(&charter_path, "v2 charter, edited").unwrap();
        let path = sov_dir.join("project.toml");
        std::fs::write(
            &path,
            format!(
                "schema_version = 2\n\n[project]\nname = \"my-app\"\n\n[lifecycle]\ncurrent_phase = 1\ncharter_hash = \"{}\"\n",
                approved
            ),
        )
        .unwrap();
        let m = load_project_match(&path).unwrap();
        assert_eq!(m.charter_status, CharterStatus::Drifted);
    }

    #[test]
    fn load_project_match_clean_when_hash_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let sov_dir = tmp.path().join(".sovereign");
        std::fs::create_dir_all(&sov_dir).unwrap();
        let charter_path = sov_dir.join("CHARTER.md");
        let body = "stable charter content";
        std::fs::write(&charter_path, body).unwrap();
        let approved = {
            let mut h = Sha256::new();
            h.update(body.as_bytes());
            format!("{:x}", h.finalize())
        };
        let path = sov_dir.join("project.toml");
        std::fs::write(
            &path,
            format!(
                "schema_version = 2\n\n[project]\nname = \"my-app\"\n\n[lifecycle]\ncurrent_phase = 1\ncharter_hash = \"{}\"\n",
                approved
            ),
        )
        .unwrap();
        let m = load_project_match(&path).unwrap();
        assert_eq!(m.charter_status, CharterStatus::Clean);
    }

    #[test]
    fn build_entity_inventory_lowercases_canonical_names_and_aliases() {
        use corpus_engine::enrichment::atlas::atoms::{AtomId, AtomsFile, ChunkRef, Entity};
        use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("personal-knowledge").join("atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();
        let entity = Entity {
            id: AtomId::entity(1),
            canonical_name: "Sarah Chen".into(),
            aliases: vec!["Sarah".into(), "S. Chen".into()],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("c1".to_string(), None),
            description: String::new(),
            salience: 0.7,
            enrichment_depth: EnrichmentDepth::extracted_default(),
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
        };
        let file = AtomsFile::new(vec![AtomEnvelope::Entity(entity)]);
        let body = serde_json::to_string(&file).unwrap();
        std::fs::write(atlas_dir.join("atoms.json"), body).unwrap();

        let inv = build_entity_inventory(tmp.path());
        assert!(inv.contains("sarah chen"));
        assert!(inv.contains("sarah"));
        assert!(inv.contains("s. chen"));
        assert!(!inv.contains("Sarah Chen"), "names should be lowercased");
    }

    #[test]
    fn build_entity_inventory_returns_empty_for_missing_atlases() {
        let tmp = tempfile::tempdir().unwrap();
        let inv = build_entity_inventory(tmp.path());
        assert!(inv.is_empty());
    }

    #[test]
    fn load_chunk_timestamps_returns_empty_for_missing_db() {
        let map = load_chunk_timestamps(Path::new("/nonexistent/path.db"));
        assert!(map.is_empty());
    }

    #[test]
    fn load_chunk_timestamps_reads_memories_and_conversations() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("state.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                last_used INTEGER NOT NULL,
                deleted_at INTEGER
            );
            CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                updated_at INTEGER NOT NULL,
                deleted_at INTEGER
            );
            INSERT INTO memories VALUES ('m1', 'a', 100, NULL);
            INSERT INTO memories VALUES ('m-deleted', 'b', 200, 1);
            INSERT INTO conversations VALUES ('c1', 500, NULL);
            ",
        )
        .unwrap();
        drop(conn);
        let map = load_chunk_timestamps(&db_path);
        assert_eq!(map.get("m1"), Some(&100));
        assert_eq!(map.get("c1"), Some(&500));
        assert!(map.get("m-deleted").is_none(), "deleted memories excluded");
    }
}
