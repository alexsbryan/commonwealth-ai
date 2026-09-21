// SPDX-License-Identifier: AGPL-3.0-or-later
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
//!   3. **in-conversation predicate** — checks whether an entity name
//!      appears in any message of the current `ConversationContext`.
//!
//! All three are pure functions plus small struct wrappers — the
//! manager owns the I/O paths and calls into this module at splice
//! time. Formatters and the timeline assembler stay free of database
//! handles.

#![cfg(feature = "treesitter")]

use std::collections::HashMap;
use std::path::Path;

use corpus_engine::enrichment::atlas::atoms::AtomEnvelope;
use corpus_engine::enrichment::atlas::writer::{read_atlas_atoms, ATLAS_DIRNAME};
use corpus_engine_notes::notes::NoteStore;
use rusqlite::{Connection, OpenFlags};
use sovereign_core::memory::EntityInventory;

use crate::knowledge_view::relational::{RelationalNote, RelationalNoteKind};
use crate::knowledge_view::strategic::StrategicGoal;

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

    if let Ok(mut stmt) =
        conn.prepare("SELECT id, last_used FROM memories WHERE deleted_at IS NULL")
    {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        {
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
        }
    }

    if let Ok(mut stmt) =
        conn.prepare("SELECT id, updated_at FROM conversations WHERE deleted_at IS NULL")
    {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        {
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
    let rows = match notes.read_notes_by_related_entity(entity_name, kinds).await {
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
        for atom in atoms_file.atoms() {
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
            let before_ok =
                i == 0 || !bytes_h[i - 1].is_ascii_alphanumeric() && bytes_h[i - 1] != b'_';
            let after_idx = i + bytes_n.len();
            let after_ok = after_idx >= bytes_h.len()
                || !bytes_h[after_idx].is_ascii_alphanumeric() && bytes_h[after_idx] != b'_';
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
        assert_eq!(shorten_summary("first\nsecond\nthird"), "first");
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
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
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
        assert!(!map.contains_key("m-deleted"), "deleted memories excluded");
    }
}
