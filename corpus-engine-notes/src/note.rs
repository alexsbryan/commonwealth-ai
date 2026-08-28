// SPDX-License-Identifier: AGPL-3.0-or-later
//! `Note` — what the knowledge layer publishes about agent working memory.
//!
//! The noun and its three value types, held apart from the store that
//! persists them. Until noun-convergence rung 7 they shared
//! `notes.rs` with `NoteStore`, all 8,003 lines of it, and a consumer
//! that wanted only the vocabulary had to read past the SQL to find it.
//!
//! Everything here is plain data: no rusqlite, no tokio, no I/O. That is
//! the property that makes it a published surface rather than an
//! implementation detail — the store can change its schema, its
//! transactions, or its whole storage engine without any of these four
//! types moving.
//!
//! | Type | Answers |
//! |---|---|
//! | [`Note`] | what was recorded |
//! | [`NoteScope`] | how far it reaches — repo, feature, or session |
//! | [`NoteSource`] | who or what recorded it |
//! | [`ScopeFilter`] | which of them a read should see |
//!
//! Re-exported from [`crate::notes`] and from the crate root, so every
//! historical import path still resolves; this is a move, not a rename of
//! anyone's `use` line.

/// Scope dimension for ATOS notes.
///
/// - `Global`: architectural invariants that outlive any one feature.
/// - `Feature`: decisions/attempts/invariants tied to a single feature id.
/// - `Session`: ephemeral scratch within one agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteScope {
    Global,
    Feature,
    Session,
}

impl NoteScope {
    /// Every variant, once. Exists so a consumer that mirrors this closed set
    /// across a package boundary — `sovereign_contracts::recipe::notes` does —
    /// can WALK it in a test instead of restating it from memory. Adding a
    /// variant without adding it here fails `all_lists_every_scope` below.
    pub const ALL: &'static [NoteScope] = &[Self::Global, Self::Feature, Self::Session];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Feature => "feature",
            Self::Session => "session",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "global" => Some(Self::Global),
            "feature" => Some(Self::Feature),
            "session" => Some(Self::Session),
            _ => None,
        }
    }
}

/// Note kinds that are OPERATIONAL EXHAUST, not durable knowledge: high-volume,
/// machine-emitted, read back only within a conversation (never a cross-session
/// or cross-node reference). They are the lifecycle opposite of durable kinds
/// (decision / invariant / attempt / reflection / …).
///
/// This is the single source of truth for "is this note ephemeral?", consulted
/// by both the write path ([`crate::notes::NoteStore::write_note_full_v9`] auto-scopes these
/// to Session so they never gossip) and the TTL sweep
/// ([`crate::notes::NoteStore::purge_expired_ephemeral`]). The CLI `notes rationalize` imports
/// it too, so there is exactly ONE list to keep current.
pub const EPHEMERAL_KINDS: &[&str] = &["tool_decision", "checkpoint", "checkpoint_restored"];

/// True when `kind` is operational exhaust (see [`EPHEMERAL_KINDS`]).
pub fn is_ephemeral_kind(kind: &str) -> bool {
    EPHEMERAL_KINDS.contains(&kind)
}

/// Provenance dimension for notes (audit-hardening v6 schema).
///
/// `Agent` is the highest-confidence source — the agent explicitly
/// called the `note` tool. The other four record automated sources
/// the audit assembly ranks lower:
///
/// - `Committed` — harvested from a git commit message by the daemon
///   reindexer's git HEAD poll.
/// - `Extracted` — produced by an LLM pass over the session diff at
///   audit-assembly time.
/// - `Inferred` — regex-mined from agent response text in the
///   conversation transcript.
/// - `Observed` — derived from a tool-call pattern match (e.g.
///   `blast` → file write counts as "investigated impact before
///   modifying").
///
/// The audit floor is non-empty when at least one of these fires,
/// even if the agent never wrote an explicit note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteSource {
    Agent,
    Committed,
    Extracted,
    Inferred,
    Observed,
}

impl NoteSource {
    /// Every variant, once. See [`NoteScope::ALL`] for why this exists.
    pub const ALL: &'static [NoteSource] = &[
        Self::Agent,
        Self::Committed,
        Self::Extracted,
        Self::Inferred,
        Self::Observed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Committed => "committed",
            Self::Extracted => "extracted",
            Self::Inferred => "inferred",
            Self::Observed => "observed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "committed" => Some(Self::Committed),
            "extracted" => Some(Self::Extracted),
            "inferred" => Some(Self::Inferred),
            "observed" => Some(Self::Observed),
            _ => None,
        }
    }

    /// Audit-display priority. Higher number = higher priority.
    /// Used to sort decisions so agent-written notes appear above
    /// extracted/inferred/observed ones at the same date.
    pub fn priority(self) -> u8 {
        match self {
            Self::Agent => 4,
            Self::Committed => 3,
            Self::Extracted => 2,
            Self::Inferred => 1,
            Self::Observed => 0,
        }
    }
}

/// One note. The unit the knowledge layer publishes; what a caller
/// holds after a read, and what it hands back to write.
///
/// Was `Note` until noun-convergence rung 7 — a name that said
/// "a row of the store's table" to every consumer that only ever
/// wanted the note.
#[derive(Debug, Clone)]
pub struct Note {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub symbols: Vec<String>,
    pub files: Vec<String>,
    pub session_id: String,
    /// RFC 3339 timestamp string.
    pub created_at: String,
    /// Primary tool this note concerns (reflections only; `None` for other kinds).
    pub tool_name: Option<String>,
    /// Unix timestamp when this note was retired; `None` means active.
    pub retired_at: Option<i64>,
    /// Human-readable reason for retirement (e.g. "fixed in PR #88").
    pub retired_by: Option<String>,
    /// Scope dimension: `"global"` | `"feature"` | `"session"`.
    pub scope: String,
    /// ATOS feature id when `scope == "feature"`. `None` otherwise.
    pub feature_id: Option<String>,
    /// Origin note id when this row was created by `promote_note`. `None` for
    /// native writes.
    pub promoted_from: Option<String>,
    /// Free-text entity name this note relates to — typically a
    /// `Person` / `Organization` name for `commitment` and
    /// `follow_up` kinds, an `Initiative` name for `goal` kind. Not
    /// a foreign key into the entity graph (the graph is rebuilt
    /// each enrichment cycle); the digest matches at query time.
    /// `None` when the note has no relational anchor (e.g. classic
    /// `decision` / `invariant` kinds).
    pub related_entity: Option<String>,
    /// Provenance of the note. One of:
    /// - `"agent"`     — explicit `note` tool call by an agent (highest signal).
    /// - `"committed"` — harvested from a git commit message.
    /// - `"extracted"` — extracted by an LLM pass over the session diff.
    /// - `"inferred"`  — regex-mined from agent response text.
    /// - `"observed"`  — derived from a tool-call pattern match.
    ///
    /// Pre-v6 rows default to `"agent"`. CHECK enforcement is at the
    /// application layer (in [`crate::notes::NoteStore::write_note_with_source`])
    /// rather than via a SQL constraint, so adding a new source is a
    /// one-line code change rather than a schema migration.
    pub source: String,
    /// Note id this note reverses. `None` for first-time decisions.
    /// Audit assembly uses this to render `↳ REVERSED` lines under the
    /// original decision. The referenced row is left intact — only the
    /// audit display treats this as a reversal.
    pub supersedes: Option<String>,
    /// Structured per-kind payload (v7+). Used by the recipe-author
    /// kinds (`decision` with a `decision_kind`, `research_finding`
    /// with `authority`, `recipe_issue` with category/count, etc.) so
    /// the dashboard / CLI can read fields without reparsing
    /// `content`. NULL for pre-v7 rows and for kinds that don't carry
    /// structured data.
    pub payload_json: Option<String>,
    /// Node that authored this note, in [`NodeId`]'s lossy `Display`
    /// form (`node-` + first 8 bytes hex). `None` for rows written
    /// before the column existed, and for stores opened without a mesh
    /// identity (a bare CLI `NoteStore::open` never calls
    /// [`crate::notes::NoteStore::set_origin_node_id`]).
    ///
    /// Kept as the raw string rather than a parsed id because the
    /// `Display` form is truncated and cannot round-trip — resolution
    /// to a human name is prefix-matching against the roster, which is
    /// [`crate::notes::NoteStore::attribution`]'s job. Readers that want a label MUST
    /// go through that method rather than matching on this field, so
    /// there is one decider for "whose note is this?".
    ///
    /// `None` here is reported as [`NodeAttribution::Unattributed`],
    /// never silently rendered as the local node (ARCH_PRINCIPLES §18.3).
    pub origin_node_id: Option<String>,
    /// Receipt stamp, origin side (order `commons-fluency` fix 3):
    /// unix seconds when THIS node's daemon last successfully
    /// published the note through the mesh sink. `None` = never
    /// published — the write stayed node-local (or the row predates
    /// the v12 columns). The same value is carried on the wire
    /// inside [`crate::notes::NotePropagationEvent::sent_at`].
    pub sent_at: Option<i64>,
    /// Receipt stamp, receiver side: unix seconds when THIS node's
    /// daemon first applied the note from the wire
    /// ([`crate::notes::NoteStore::ingest_remote_notes`]). `None` = authored
    /// locally, never received. Together with `sent_at` this forms
    /// the two-sided receipt: on a peer's row `sent_at` is the
    /// ORIGIN's publish clock, `received_at` is the receiver's own
    /// apply clock, and a drill can bracket `sent_at <= received_at
    /// <= now` without trusting either machine's clock for both
    /// ends.
    pub received_at: Option<i64>,
}

/// Retrieval filter for scope/feature combinations.
///
/// Use `ScopeFilter::default()` to preserve the legacy behavior of reading
/// all notes regardless of scope.
#[derive(Debug, Clone, Default)]
pub struct ScopeFilter {
    /// When non-empty, results are restricted to rows with `scope` in this list.
    pub scopes: Vec<NoteScope>,
    /// When `Some`, applies `feature_id = ?` as an additional predicate. Only
    /// meaningful when `scopes` includes `NoteScope::Feature`.
    pub feature_id: Option<String>,
}

impl ScopeFilter {
    /// Everything a feature-scoped reader wants: repo-wide notes plus the
    /// ones belonging to `feature_id`, and nothing session-local.
    ///
    /// This pairing was spelled as a struct literal at every call site that
    /// composes a briefing — `brief`, `read_note_digest`, and commonwealth's
    /// context injector each rebuilt it by hand. It is ONE decision about
    /// what a feature-aware read sees (ARCH §10.6), so it gets one name.
    pub fn for_feature(feature_id: Option<&str>) -> Self {
        match feature_id {
            Some(f) => Self {
                scopes: vec![NoteScope::Global, NoteScope::Feature],
                feature_id: Some(f.to_string()),
            },
            None => Self {
                scopes: vec![NoteScope::Global],
                feature_id: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_scope() {
        // The exhaustive match is the structural half: adding a variant to
        // `NoteScope` stops compiling HERE, which puts the author in front of
        // the length assertion on the next line.
        for s in NoteScope::ALL {
            match s {
                NoteScope::Global | NoteScope::Feature | NoteScope::Session => {}
            }
        }
        assert_eq!(NoteScope::ALL.len(), 3, "ALL must list every variant once");
    }

    #[test]
    fn all_lists_every_source() {
        for s in NoteSource::ALL {
            match s {
                NoteSource::Agent
                | NoteSource::Committed
                | NoteSource::Extracted
                | NoteSource::Inferred
                | NoteSource::Observed => {}
            }
        }
        assert_eq!(NoteSource::ALL.len(), 5, "ALL must list every variant once");
    }

    #[test]
    fn every_scope_round_trips_through_its_wire_string() {
        for s in NoteScope::ALL {
            assert_eq!(NoteScope::parse(s.as_str()), Some(*s));
        }
    }

    #[test]
    fn every_source_round_trips_through_its_wire_string() {
        for s in NoteSource::ALL {
            assert_eq!(NoteSource::parse(s.as_str()), Some(*s));
        }
    }

    #[test]
    fn source_priority_is_a_total_order_with_agent_on_top() {
        let mut seen: Vec<u8> = NoteSource::ALL.iter().map(|s| s.priority()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            NoteSource::ALL.len(),
            "priorities must be distinct"
        );
        assert_eq!(
            NoteSource::Agent.priority(),
            *seen.last().unwrap(),
            "an agent-written note outranks every automated source"
        );
    }

    #[test]
    fn for_feature_reads_global_plus_the_named_feature() {
        let f = ScopeFilter::for_feature(Some("nc-7-note"));
        assert_eq!(f.scopes, vec![NoteScope::Global, NoteScope::Feature]);
        assert_eq!(f.feature_id.as_deref(), Some("nc-7-note"));
    }

    #[test]
    fn for_feature_without_a_feature_reads_global_only() {
        let f = ScopeFilter::for_feature(None);
        assert_eq!(f.scopes, vec![NoteScope::Global]);
        assert!(f.feature_id.is_none());
        // Never Session: session notes are one agent's scratch and must not
        // surface in another session's briefing.
        assert!(!f.scopes.contains(&NoteScope::Session));
    }

    #[test]
    fn ephemeral_kinds_are_exactly_the_operational_exhaust() {
        for k in EPHEMERAL_KINDS {
            assert!(is_ephemeral_kind(k));
        }
        for k in ["decision", "invariant", "attempt", "reflection", "todo"] {
            assert!(
                !is_ephemeral_kind(k),
                "{k} is durable knowledge, not exhaust"
            );
        }
    }
}
