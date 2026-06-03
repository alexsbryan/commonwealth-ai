//! Working notes store — persistent, searchable scratchpad for agents.
//!
//! Notes survive across sessions and are retrieved by full-text search,
//! symbol name, file path, or kind filter. Unlike test/lint stores (which
//! are overwritten on every run), notes are only deleted explicitly via
//! [`NoteStore::delete_note`].
//!
//! ## Kinds
//!
//! - `"decision"` — architectural or implementation choices made
//! - `"attempt"` — approaches tried and abandoned (so future sessions don't repeat)
//! - `"invariant"` — constraints that must never be violated
//! - `"todo"` — follow-up work for a future session
//! - `"reflection"` — post-task structured feedback on tool quality
//!
//! ## Schema
//!
//! - **`notes`** — one row per note with JSON arrays for `symbols` and `files`.
//!   Three nullable columns (`tool_name`, `retired_at`, `retired_by`) support
//!   the reflection lifecycle: write → surface → fix → retire.
//! - **`notes_fts`** — FTS5 virtual table backed by `notes`, kept in sync by
//!   three triggers (after insert, before delete, after update).
//! - **`tool_call_log`** — ring buffer (10,000 rows) of MCP tool invocations.
//!   Records tool names and outcomes only — no parameters, no content.
//!
//! ## Threading
//!
//! `NoteStore` wraps a synchronous `rusqlite::Connection` in a
//! `tokio::sync::Mutex`. All operations are microsecond-fast; no
//! `spawn_blocking` is needed.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::notes_schema::{
    MIGRATION_V1, MIGRATION_V10, MIGRATION_V2, MIGRATION_V3, MIGRATION_V4, MIGRATION_V5,
    MIGRATION_V6, MIGRATION_V7, MIGRATION_V8, MIGRATION_V9, SCHEMA_NEW,
};

/// Propagation event shipped on the mesh wire for a single note.
///
/// The full unit of propagation: note row + T1 embedding (if
/// present) + T2 entities (empty until T2 lands) + the propagation
/// metadata (`tombstone`, `updated_at`, `private`). Identified
/// uniquely by `content_hash` — stable across `origin_node_id`
/// rotation, idempotent on re-delivery, the same on every peer.
///
/// Wire format chosen so:
/// - Two peers writing semantically identical notes produce
///   byte-identical events keyed by the same `content_hash`.
///   Dedup on receive is a `SELECT WHERE content_hash = ?`.
/// - The reader can apply embeddings + entities in the same SQL
///   transaction as the note row.
/// - A `tombstone=true` event with the same `content_hash` as a
///   prior note marks it deleted; LWW on `updated_at` is the
///   tiebreak, but a tombstone always wins (per Step 6b).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotePropagationEvent {
    pub content_hash: String,
    pub note: ExportedNoteRow,
    pub embedding: Option<ExportedNoteEmbedding>,
    /// Empty until T2 lands; the wire field is provisioned now so
    /// T2 ships as a data change, not a schema change.
    #[serde(default)]
    pub entities: Vec<ExportedNoteEntity>,
    pub tombstone: bool,
    pub updated_at: i64,
}

/// Note row carried on the propagation wire. Mirrors the
/// `NoteRow` shape (minus rowid + retirement metadata, which is
/// node-local lifecycle state) plus the v9 propagation fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportedNoteRow {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub symbols: Vec<String>,
    pub files: Vec<String>,
    pub session_id: String,
    pub created_at: i64,
    pub scope: String,
    pub feature_id: Option<String>,
    pub related_entity: Option<String>,
    pub source: String,
    pub supersedes: Option<String>,
    pub payload_json: Option<String>,
    pub origin_node_id: Option<String>,
}

/// T1 embedding wire payload. Carries the LE-encoded BLOB along
/// with the model id + dim so a peer that's running a different
/// embed model can fall back to recomputing rather than blending
/// incompatible vectors.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportedNoteEmbedding {
    pub model_id: String,
    pub dim: i64,
    pub embedding: Vec<u8>,
}

/// T2 entity wire payload (one row per (entity, kind) tuple
/// extracted from the note's content). Empty until GLiNER
/// extraction lands.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportedNoteEntity {
    pub entity: String,
    pub kind: String,
}

/// Fire-and-forget callback the daemon installs to publish
/// propagation events. The closure adapts the event into whatever
/// transport the caller owns (most commonly
/// `MeshStore::put(app_id="notes", key=content_hash, value=event)`,
/// occasionally a channel for tests).
///
/// Sync because `MeshStore` writes are SQLite + LWW — microsecond
/// fast. NoteStore stays dep-free per `ARCH §5.4` — the closure
/// hides the transport.
pub type PropagationSinkFn = Arc<dyn Fn(&NotePropagationEvent) + Send + Sync>;

/// Counts surfaced by [`NoteStore::backfill_tier_artifacts`].
#[derive(Debug, Default, Clone)]
pub struct BackfillReport {
    pub embeddings_backfilled: usize,
    pub embed_skipped: usize,
    pub entities_backfilled: usize,
    pub entity_skipped: usize,
}

/// Result of [`NoteStore::ingest_remote_notes`] — counts so the
/// gossip layer can log convergence progress.
#[derive(Debug, Default, Clone)]
pub struct IngestRemoteReport {
    /// Events that inserted a new note locally.
    pub inserted: usize,
    /// Events whose `content_hash` already existed; idempotent skip.
    pub deduplicated: usize,
    /// Events that marked an existing note as a tombstone.
    pub tombstoned: usize,
    /// Events that flagged a concurrent-supersedes fork (set
    /// `fork_of` on insert; preserves both siblings).
    pub forked: usize,
    /// Events that were rejected for structural reasons (private
    /// flag on the wire, scope != global, etc).
    pub rejected: usize,
}

/// GLiNER entity-extraction function injected by the caller.
///
/// Returns `Vec<(entity, kind)>` per text — `entity` is the raw
/// surface form found in the content, `kind` is the GLiNER label
/// (e.g. `"Person"`, `"Organization"`, `"Symbol"`, `"File"`).
/// The set of admissible kinds is the caller's concern; NoteStore
/// stores whatever it's handed.
///
/// Sovereign passes its loaded GLiNER session (the same one
/// `chunk_entity_extractor` uses for the corpus pipeline).
/// Commonwealth passes an HTTP shim. Tests pass a deterministic
/// mock that emits known labels per substring.
///
/// Async because GLiNER inference is non-trivial; tens of ms
/// even on a small model. The closure copies `&str` internally.
pub type GlinerFn = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<(String, String)>>> + Send>>
        + Send
        + Sync,
>;

/// Embedding function injected by the caller.
///
/// Sovereign passes its local Embed slot (a closure over a daemon
/// inference client). Commonwealth passes an HTTP client wrapping
/// `/v1/embeddings`. Tests pass a deterministic mock that returns
/// a fixed vector keyed on the input text. NoteStore stays
/// dependency-free of any concrete embed transport per
/// `ARCH §5.4` (parameterise on data, not source identity) — the
/// caller wires the closure once and the store handles persistence,
/// blending, and soft-fail.
///
/// Shape mirrors `corpus_engine::types::EmbedFn` exactly so call
/// sites that already produce one can share the same `Arc` between
/// the document corpus and the note store.
///
/// The future is `'static`; the closure copies `&str` internally
/// rather than borrowing across the await point.
pub type EmbedFn =
    Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>> + Send + Sync>;

/// Unit separator (0x1F) used between fields in [`content_hash`]
/// preimage. Outside the printable-ASCII range, so user content
/// can't collide with a delimiter the way `'|'` or `':'` would.
pub(crate) const HASH_FIELD_SEP: char = '\u{1F}';

/// Compute the content hash that identifies a note across peers.
///
/// Stable across `origin_node_id` rotation (toolbx container
/// rebuilds): hash inputs are content + scoping fields only, not
/// the node id. Two peers writing the same `(kind, content, scope,
/// feature_id, session_id)` produce the same hash, so the dedup
/// on receive is idempotent.
///
/// We salt with `HASH_FIELD_SEP` between fields so adversarial
/// content can't tunnel a forged delimiter (e.g. `"a\u{1F}b"` in
/// content would still hash distinctly from a real `kind="a"
/// content="b"` because of the field-position prefix bytes).
///
/// SHA-256 over UTF-8 bytes, hex-encoded lowercase. Output is 64
/// hex chars. Deterministic, no salt, no clock dependency.
pub(crate) fn compute_content_hash(
    kind: &str,
    content: &str,
    scope: &str,
    feature_id: Option<&str>,
    session_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([HASH_FIELD_SEP as u8]);
    hasher.update(content.as_bytes());
    hasher.update([HASH_FIELD_SEP as u8]);
    hasher.update(scope.as_bytes());
    hasher.update([HASH_FIELD_SEP as u8]);
    hasher.update(feature_id.unwrap_or("").as_bytes());
    hasher.update([HASH_FIELD_SEP as u8]);
    hasher.update(session_id.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// FNV-1a 64-bit hash over a slice of `&str`s. Used by the
/// reconciliation digest to summarise each 8-bit bucket of
/// content hashes into one cheap u64. Not cryptographic — the
/// guarantee we need is "two peers with identical hash lists
/// produce identical digests", and FNV gives that with much less
/// code than SHA. Iteration order is the caller's responsibility
/// (`content_hash_digest` sorts before calling).
pub(crate) fn fnv1a_64_strings(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for p in parts {
        for byte in p.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        // Use 0x1F separator so a hash list `["abc"]` and `["a","bc"]`
        // produce distinct digests.
        hash ^= 0x1F;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Parse the bucket id (first hex byte) out of a `content_hash`.
/// Returns `None` for malformed hashes — the caller drops these
/// from the digest rather than panicking.
pub(crate) fn parse_bucket_id(content_hash: &str) -> Option<u8> {
    if content_hash.len() < 2 {
        return None;
    }
    u8::from_str_radix(&content_hash[..2], 16).ok()
}

/// Read the blend weight from `SOVEREIGN_NOTES_EMBED_WEIGHT`,
/// clamped to `[0.0, 1.0]`. Unset → default `0.5`.
///
/// A weight of `0.0` is the operator-facing kill-switch for the
/// semantic blend; baseline FTS5-only behaviour resumes
/// byte-identical.
pub(crate) fn read_embed_weight_env() -> f32 {
    std::env::var("SOVEREIGN_NOTES_EMBED_WEIGHT")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|w| w.clamp(0.0, 1.0))
        .unwrap_or(0.5)
}

/// Min/max bounds over a candidate pool, used to normalise blend
/// inputs into `[0.0, 1.0]`. Degenerate pools (single element, or
/// all-equal) normalise everyone to `0.5` so the dimension
/// contributes equally — preferable to NaN or division-by-zero.
pub(crate) struct MinMax {
    min: f64,
    max: f64,
}

impl MinMax {
    pub(crate) fn from_slice(values: &[f64]) -> Self {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for v in values {
            if *v < min {
                min = *v;
            }
            if *v > max {
                max = *v;
            }
        }
        Self { min, max }
    }

    pub(crate) fn normalise(&self, value: f64) -> f64 {
        let span = self.max - self.min;
        if span.abs() < 1e-9 {
            0.5
        } else {
            ((value - self.min) / span).clamp(0.0, 1.0)
        }
    }
}

/// Cosine similarity between two embedding vectors. Returns `0.0`
/// on dimension mismatch (we treat dim-mismatch as "incompatible
/// model, no signal" rather than panicking — see content_hash
/// invariant about LE-encoded BLOB reproducibility).
pub(crate) fn cosine_sim(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let xf = *x as f64;
        let yf = *y as f64;
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom < 1e-9 {
        0.0
    } else {
        dot / denom
    }
}

/// FTS5 candidate pool: top-`pool_size` notes ranked by BM25.
/// Returns `(NoteRow, bm25_rank)` — bm25 returns "lower is
/// better"; the caller flips the sign before min-max
/// normalisation.
fn fetch_bm25_pool(
    conn: &Connection,
    query: &str,
    symbols: &[String],
    files: &[String],
    kinds: &[String],
    pool_size: usize,
    include_retired: bool,
) -> Result<Vec<(NoteRow, f64)>> {
    let (where_extra, bound) = build_filter_clause(symbols, files, kinds);
    let retired_clause = if include_retired {
        ""
    } else {
        "AND n.retired_at IS NULL"
    };
    let sql = format!(
        "WITH ranked AS (
            SELECT rowid, bm25(notes_fts) AS rank
            FROM notes_fts
            WHERE notes_fts MATCH ?
        )
        SELECT n.id, n.kind, n.content, n.symbols, n.files, n.session_id,
               n.created_at, n.tool_name, n.retired_at, n.retired_by,
               n.scope, n.feature_id, n.promoted_from, n.related_entity,
               n.source, n.supersedes, n.payload_json,
               r.rank AS bm25_rank
        FROM notes n
        JOIN ranked r ON r.rowid = n.rowid
        WHERE 1=1 {retired_clause} {where_extra}
        ORDER BY r.rank
        LIMIT ?"
    );
    let mut params_owned: Vec<rusqlite::types::Value> = Vec::new();
    params_owned.push(rusqlite::types::Value::Text(query.to_string()));
    params_owned.extend(bound);
    params_owned.push(rusqlite::types::Value::Integer(pool_size as i64));
    let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
    let mapped = stmt
        .query_map(rusqlite::params_from_iter(params_owned), |row| {
            let note = map_note_row(row)?;
            let rank: f64 = row.get(17)?;
            Ok((note, rank))
        })
        .map_err(sqlite_err)?;
    let mut out = Vec::new();
    for r in mapped {
        out.push(r.map_err(sqlite_err)?);
    }
    Ok(out)
}

/// Cosine candidate pool: every non-retired note with an
/// embedding row, scored by cosine against `query_vec`, top-N
/// retained. The full embedding scan is in-process — at ≤10k
/// notes with 768-dim fp32 this is microseconds and the SQLite
/// alternative (loading all blobs then scanning) is no faster.
fn fetch_cosine_pool(
    conn: &Connection,
    symbols: &[String],
    files: &[String],
    kinds: &[String],
    pool_size: usize,
    include_retired: bool,
    query_vec: &[f32],
) -> Result<Vec<(NoteRow, f64)>> {
    let (where_extra, bound) = build_filter_clause(symbols, files, kinds);
    let retired_clause = if include_retired {
        ""
    } else {
        "AND n.retired_at IS NULL"
    };
    let sql = format!(
        "SELECT n.id, n.kind, n.content, n.symbols, n.files, n.session_id,
                n.created_at, n.tool_name, n.retired_at, n.retired_by,
                n.scope, n.feature_id, n.promoted_from, n.related_entity,
                n.source, n.supersedes, n.payload_json,
                e.embedding
         FROM notes n
         JOIN note_embeddings e ON e.note_id = n.id
         WHERE 1=1 {retired_clause} {where_extra}"
    );
    let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
    let mapped = stmt
        .query_map(rusqlite::params_from_iter(bound), |row| {
            let note = map_note_row(row)?;
            let bytes: Vec<u8> = row.get(17)?;
            Ok((note, bytes))
        })
        .map_err(sqlite_err)?;
    let mut scored: Vec<(NoteRow, f64)> = Vec::new();
    for r in mapped {
        let (note, bytes) = r.map_err(sqlite_err)?;
        let vec = match embedding_from_le_bytes(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    target = "notes",
                    note_id = %note.id,
                    error = %e,
                    "notes: corrupt embedding blob; skipping"
                );
                continue;
            }
        };
        let cos = cosine_sim(query_vec, &vec);
        scored.push((note, cos));
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(pool_size);
    Ok(scored)
}

/// Shared WHERE-clause builder for the candidate-pool fetches.
/// Mirrors the inline clause in `read_notes_scoped` (lines
/// ~1139-1175) so the SQL filter pushdown semantics are identical
/// between the baseline and semantic paths.
fn build_filter_clause(
    symbols: &[String],
    files: &[String],
    kinds: &[String],
) -> (String, Vec<rusqlite::types::Value>) {
    let mut where_extra = String::new();
    let mut bound: Vec<rusqlite::types::Value> = Vec::new();
    if !kinds.is_empty() {
        where_extra.push_str(" AND n.kind IN (");
        for (i, k) in kinds.iter().enumerate() {
            if i > 0 {
                where_extra.push(',');
            }
            where_extra.push('?');
            bound.push(rusqlite::types::Value::Text(k.clone()));
        }
        where_extra.push(')');
    }
    if !symbols.is_empty() {
        where_extra.push_str(" AND EXISTS (SELECT 1 FROM json_each(n.symbols) WHERE value IN (");
        for (i, s) in symbols.iter().enumerate() {
            if i > 0 {
                where_extra.push(',');
            }
            where_extra.push('?');
            bound.push(rusqlite::types::Value::Text(s.clone()));
        }
        where_extra.push_str("))");
    }
    if !files.is_empty() {
        where_extra.push_str(" AND EXISTS (SELECT 1 FROM json_each(n.files) WHERE value IN (");
        for (i, f) in files.iter().enumerate() {
            if i > 0 {
                where_extra.push(',');
            }
            where_extra.push('?');
            bound.push(rusqlite::types::Value::Text(f.clone()));
        }
        where_extra.push_str("))");
    }
    (where_extra, bound)
}

/// Pack a `Vec<f32>` embedding into a little-endian byte string
/// for SQLite BLOB storage. SQLite is endian-agnostic; we pick LE
/// explicitly so the on-disk format is reproducible across hosts
/// (an `ARM64 Mac` + a `x86_64 Linux toolbx peer` must produce
/// byte-identical BLOBs for the same vector, otherwise the
/// content_hash dedup on the gossip wire breaks).
///
/// Round-trips via [`embedding_from_le_bytes`].
pub(crate) fn embedding_to_le_bytes(vec: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for f in vec {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decode a LE-byte blob back into a `Vec<f32>`. Returns
/// [`Error::Io`] on length mismatch (blob length not a multiple
/// of 4).
pub(crate) fn embedding_from_le_bytes(bytes: &[u8]) -> Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(Error::Io(std::io::Error::other(format!(
            "embedding_from_le_bytes: blob length {} is not a multiple of 4",
            bytes.len()
        ))));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let arr: [u8; 4] = chunk
            .try_into()
            .expect("chunks_exact(4) yields 4-byte chunks");
        out.push(f32::from_le_bytes(arr));
    }
    Ok(out)
}

/// Post-migration backfill: compute `content_hash` for every row
/// where it's NULL. Runs once on first open after v9 lands; on
/// subsequent opens the predicate matches nothing and the call is
/// a no-op.
///
/// SQLite has no built-in cryptographic hash, so the backfill
/// happens in Rust. Wrapped in one transaction so a crash midway
/// leaves either every row hashed or none — never half (so a
/// follow-up open re-runs the whole pass).
fn backfill_content_hashes(conn: &Connection) -> Result<()> {
    let needs_backfill: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notes WHERE content_hash IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(sqlite_err)?;
    if needs_backfill == 0 {
        return Ok(());
    }

    let mut rows: Vec<(String, String, String, String, Option<String>, String)> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, content, scope, feature_id, session_id
                   FROM notes
                  WHERE content_hash IS NULL",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(sqlite_err)?;
        for r in mapped {
            rows.push(r.map_err(sqlite_err)?);
        }
    }

    conn.execute_batch("BEGIN").map_err(sqlite_err)?;
    let result: Result<()> = (|| {
        let mut update = conn
            .prepare("UPDATE notes SET content_hash = ?1 WHERE id = ?2")
            .map_err(sqlite_err)?;
        for (id, kind, content, scope, feature_id, session_id) in &rows {
            let hash =
                compute_content_hash(kind, content, scope, feature_id.as_deref(), session_id);
            update.execute(params![hash, id]).map_err(sqlite_err)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = conn.execute_batch("ROLLBACK");
        result?;
    }
    conn.execute_batch("COMMIT").map_err(sqlite_err)?;

    tracing::info!(
        target = "notes",
        backfilled = rows.len(),
        "notes: content_hash backfill complete"
    );
    Ok(())
}

// ─── Types ────────────────────────────────────────────────────────────────────

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

/// A single note row returned from [`NoteStore::read_notes`].
#[derive(Debug, Clone)]
pub struct NoteRow {
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
    /// application layer (in [`NoteStore::write_note_with_source`])
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

/// A single row from the tool call ring buffer.
#[derive(Debug, Clone)]
pub struct ToolCallLogRow {
    pub id: String,
    pub session_id: String,
    pub tool_name: String,
    /// `"success"` | `"error"` | `"empty_result"`
    pub outcome: String,
    pub called_at: i64,
}

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite + FTS5 store for agent working notes.
pub struct NoteStore {
    conn: Arc<Mutex<Connection>>,
    /// Optional T1 embedding hook. When set, [`write_note_full`]
    /// computes an embedding inside the write transaction and
    /// persists it to `note_embeddings`. When `None`, T1 is
    /// disabled for this store — writes still succeed,
    /// embedding-less, and the semantic-blend read path silently
    /// falls back to FTS5-only ranking.
    ///
    /// `OnceLock` so the daemon can open the store early (before
    /// the inference slot is built) and inject the embed closure
    /// later without breaking the existing `Arc<NoteStore>` graph.
    embed_fn: OnceLock<EmbedFn>,
    /// Optional mesh-propagation sink. When set, every `scope=global
    /// && !private` write fires the sink with the corresponding
    /// [`NotePropagationEvent`]. `None` keeps notes node-local
    /// (the pre-v9 default). The caller (most commonly the
    /// sovereign daemon) wires this to `MeshStore::put` under
    /// `app_id="notes"`. Wired late — after `set_mesh_store` on
    /// the daemon.
    propagation_sink: OnceLock<PropagationSinkFn>,
    /// Node id stamped on outbound propagation events as the
    /// `origin_node_id`. Optional because the store can run
    /// dependency-free in tests / CLI tools that don't have a
    /// mesh identity. Set via [`NoteStore::set_origin_node_id`].
    origin_node_id: OnceLock<String>,
    /// Optional T2 entity extractor. When set, every
    /// `write_note_full_v9` call runs the closure against the
    /// note's content and writes `(entity, kind)` tuples into
    /// `note_entities` in the same transaction. Author-supplied
    /// `symbols` are merged in first (INSERT then INSERT OR
    /// IGNORE on the extracted set), so manual tags always win
    /// on overlap.
    gliner_fn: OnceLock<GlinerFn>,
}

impl NoteStore {
    /// Open or create the database at `db_path`. Schema migrations are
    /// idempotent — safe to call on an existing database.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "NoteStore::open {}: {e}",
                db_path.display()
            )))
        })?;

        // WAL mode is always desirable — idempotent, safe to repeat.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| Error::Io(std::io::Error::other(format!("NoteStore WAL: {e}"))))?;

        // Determine whether this is a fresh DB or an existing one, then
        // run the appropriate setup (full schema vs. incremental migration).
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);

        if version == 0 {
            let table_exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notes'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);

            if table_exists > 0 {
                // Existing database on old schema — apply migration.
                conn.execute_batch(MIGRATION_V1).map_err(|e| {
                    Error::Io(std::io::Error::other(format!("NoteStore migrate v1: {e}")))
                })?;
            } else {
                // Brand-new database — create full schema and mark it current.
                conn.execute_batch(SCHEMA_NEW).map_err(|e| {
                    Error::Io(std::io::Error::other(format!("NoteStore schema: {e}")))
                })?;
            }
        }

        // Re-read after any v0→v1 work above, then apply v1→v2 if needed.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 2 {
            conn.execute_batch(MIGRATION_V2).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v2: {e}")))
            })?;
        }

        // v2 → v3: expand the `kind` CHECK constraint to admit three
        // new ATOS note kinds. SQLite can't alter a CHECK in-place, so
        // we follow MIGRATION_V1's rename-recreate-copy pattern. The
        // FTS5 virtual table and triggers must also be rebuilt because
        // they reference the `notes` table by name.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 3 {
            conn.execute_batch(MIGRATION_V3).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v3: {e}")))
            })?;
        }

        // v3 → v4: ATOS M4 adds the `deviation` kind for automatic
        // spec-drift notes written by the approval-gate middleware.
        // Same rename-recreate pattern as V3 — SQLite cannot ALTER a
        // CHECK constraint in place.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 4 {
            conn.execute_batch(MIGRATION_V4).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v4: {e}")))
            })?;
        }

        // v4 → v5: Relational + Strategic Awareness changeset
        // (requirements §6). Three new kinds — `commitment`,
        // `follow_up`, `goal` — plus a `related_entity` text column
        // on every row. Rename-recreate again because the CHECK
        // constraint changes and SQLite can't ALTER one in place.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 5 {
            conn.execute_batch(MIGRATION_V5).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v5: {e}")))
            })?;
        }

        // v5 → v6: Audit-hardening provenance fields. Two new columns
        // (`source`, `supersedes`) supporting the four extraction
        // streams (agent / committed / extracted / inferred / observed)
        // and decision-reversal display. Plain ADD COLUMN — no CHECK
        // constraint changes, so no rename-recreate.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 6 {
            conn.execute_batch(MIGRATION_V6).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v6: {e}")))
            })?;
        }

        // v6 → v7: Recipe-author note kinds + structured payload column.
        // Six new kinds — `research_finding`, `capability_request`,
        // `recipe_issue`, `checkpoint`, `checkpoint_restored`,
        // `deferred_question` — plus a nullable `payload_json` TEXT
        // column for per-kind structured data (decision_kind on
        // `decision` rows, authority on `research_finding`, category
        // on `recipe_issue`, etc.). Rename-recreate because the CHECK
        // constraint changes; SQLite can't ALTER one in place.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 7 {
            conn.execute_batch(MIGRATION_V7).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v7: {e}")))
            })?;
        }

        // v7 → v8: Tool-Mastery framework adds the `tool_decision`
        // kind (no new columns; piggybacks on v7's `payload_json`).
        // Same rename-recreate dance because the CHECK constraint
        // changes.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 8 {
            conn.execute_batch(MIGRATION_V8).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v8: {e}")))
            })?;
        }

        // v8 → v9: Tiered-retrieval + mesh-propagation surface.
        // Additive only — five new columns on `notes`, two new
        // tables (`note_embeddings`, `note_propagation_watermark`),
        // three new indexes. No CHECK constraint change, so no
        // rename-recreate, no FTS5 rebuild. After the SQL fires,
        // backfill `content_hash` for pre-v9 rows in Rust (SQLite
        // has no built-in cryptographic hash).
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 9 {
            conn.execute_batch(MIGRATION_V9).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v9: {e}")))
            })?;
            backfill_content_hashes(&conn)?;
        }

        // v9 → v10: T2 entity-graph surface. One additive table
        // (`note_entities`) + two indexes; same `chunk_entities`
        // row shape so PPR utilities work unchanged. No
        // back-compat cliff — pre-v10 notes simply have no
        // entity rows yet; `sovereign notes reindex --t2` is the
        // operator-driven backfill once GLiNER is wired.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 10 {
            conn.execute_batch(MIGRATION_V10).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v10: {e}")))
            })?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            embed_fn: OnceLock::new(),
            propagation_sink: OnceLock::new(),
            origin_node_id: OnceLock::new(),
            gliner_fn: OnceLock::new(),
        })
    }

    /// Builder: attach an [`EmbedFn`] so writes compute and persist
    /// T1 embeddings, and reads can blend semantic similarity into
    /// the FTS5 BM25 ranking.
    ///
    /// Per `ARCH §5.4` the store stays primitive: it takes a
    /// closure, not a daemon handle. The caller owns the
    /// transport (local Embed slot, HTTP client, mock) and
    /// adapts it into the [`EmbedFn`] shape.
    ///
    /// Use [`NoteStore::set_embed_fn`] for the deferred /
    /// post-Arc-wrapping wiring shape needed by the daemon.
    pub fn with_embed_fn(self, embed_fn: EmbedFn) -> Self {
        let _ = self.embed_fn.set(embed_fn);
        self
    }

    /// Wire the [`EmbedFn`] AFTER the store is already shared via
    /// `Arc<NoteStore>`. Idempotent (only the first call sticks);
    /// returns `Err` on the second call so the daemon can detect
    /// double-wiring during construction-order bugs.
    pub fn set_embed_fn(&self, embed_fn: EmbedFn) -> std::result::Result<(), &'static str> {
        self.embed_fn
            .set(embed_fn)
            .map_err(|_| "embed_fn already set")
    }

    /// Whether T1 embeddings are enabled on this store. Useful
    /// for diagnostics and for skipping embed-dependent tests
    /// against a NoteStore opened without `with_embed_fn`.
    pub fn has_embed_fn(&self) -> bool {
        self.embed_fn.get().is_some()
    }

    /// Builder: attach a [`PropagationSinkFn`] so global,
    /// non-private writes fan out to the mesh. The sink fires
    /// AFTER the local write commits — peers only see notes that
    /// landed locally, never half-committed state.
    ///
    /// Use [`NoteStore::set_propagation_sink`] for the deferred
    /// wiring shape.
    pub fn with_propagation_sink(self, sink: PropagationSinkFn) -> Self {
        let _ = self.propagation_sink.set(sink);
        self
    }

    /// Wire the propagation sink AFTER `Arc::new` — the
    /// counterpart to `with_propagation_sink` for the daemon's
    /// construction order (mesh state lands later than the
    /// NoteStore needs to exist).
    pub fn set_propagation_sink(
        &self,
        sink: PropagationSinkFn,
    ) -> std::result::Result<(), &'static str> {
        self.propagation_sink
            .set(sink)
            .map_err(|_| "propagation_sink already set")
    }

    /// Builder: stamp outbound propagation events with this node's
    /// id. When unset, events carry `origin_node_id = None` and
    /// peers reconcile by `content_hash` alone (the toolbx
    /// node-rotation-safe path).
    pub fn with_origin_node_id(self, node_id: impl Into<String>) -> Self {
        let _ = self.origin_node_id.set(node_id.into());
        self
    }

    /// Deferred-wiring counterpart to `with_origin_node_id`.
    pub fn set_origin_node_id(
        &self,
        node_id: impl Into<String>,
    ) -> std::result::Result<(), &'static str> {
        self.origin_node_id
            .set(node_id.into())
            .map_err(|_| "origin_node_id already set")
    }

    /// Whether propagation is wired on this store.
    pub fn has_propagation_sink(&self) -> bool {
        self.propagation_sink.get().is_some()
    }

    /// Builder: attach a [`GlinerFn`] so writes extract entities
    /// into `note_entities`. Persisted alongside the note row in
    /// the same SQL transaction; failure soft-fails per `ARCH §9`.
    pub fn with_gliner_fn(self, gliner_fn: GlinerFn) -> Self {
        let _ = self.gliner_fn.set(gliner_fn);
        self
    }

    /// Deferred-wiring counterpart to `with_gliner_fn` — for the
    /// daemon construction order where GLiNER is loaded after
    /// the NoteStore is already Arc-wrapped.
    pub fn set_gliner_fn(&self, gliner_fn: GlinerFn) -> std::result::Result<(), &'static str> {
        self.gliner_fn
            .set(gliner_fn)
            .map_err(|_| "gliner_fn already set")
    }

    /// Whether T2 entity extraction is wired on this store.
    pub fn has_gliner_fn(&self) -> bool {
        self.gliner_fn.get().is_some()
    }

    // ── Note writes ────────────────────────────────────────────────────────

    /// Persist a new note at global scope. Back-compat wrapper over
    /// [`write_note_scoped`]; new call sites should prefer the scoped API.
    pub async fn write_note(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
    ) -> Result<String> {
        self.write_note_scoped(
            kind,
            content,
            symbols,
            files,
            session_id,
            NoteScope::Global,
            None,
        )
        .await
    }

    /// Persist a new note with an explicit scope. Returns the generated id.
    ///
    /// `kind` must be one of `"decision"`, `"attempt"`, `"invariant"`, `"todo"`.
    /// Use [`write_reflection_scoped`] for `kind = "reflection"`.
    ///
    /// Invariant: `scope == Feature` requires `feature_id.is_some()`; violators
    /// return [`Error::InvalidInput`].
    pub async fn write_note_scoped(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
    ) -> Result<String> {
        self.write_note_with_relation(
            kind, content, symbols, files, session_id, scope, feature_id, None,
        )
        .await
    }

    /// Persist a note with all of: explicit scope, optional feature
    /// id, and an optional `related_entity` anchor. The note is
    /// tagged `source = 'agent'` (the highest-confidence source).
    ///
    /// This is a back-compat wrapper over [`write_note_with_source`];
    /// new call sites that have a non-agent provenance (commit
    /// harvester, diff extractor, response miner, pattern matcher)
    /// should call `write_note_with_source` directly.
    ///
    /// The `related_entity` field is a free-text entity name —
    /// typically a Person / Organization name for `commitment` /
    /// `follow_up` kinds, an Initiative name for `goal`. It's not
    /// validated against the entity graph here (the graph is rebuilt
    /// each enrichment cycle, so a hard FK would be a write-time
    /// race); the Relational / Strategic digests match it at query
    /// time. `related_entity = None` matches the pre-v5 behaviour.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_note_with_relation(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
    ) -> Result<String> {
        self.write_note_with_source(
            kind,
            content,
            symbols,
            files,
            session_id,
            scope,
            feature_id,
            related_entity,
            NoteSource::Agent,
            None,
        )
        .await
    }

    /// Full-fat write path with explicit provenance.
    ///
    /// `source` records where the note came from — agent (explicit
    /// `note` tool call), committed (commit-message harvest),
    /// extracted (LLM pass over session diff), inferred (regex over
    /// agent response text), or observed (tool-call pattern match).
    /// The audit assembly orders by source priority
    /// (agent > committed > extracted > inferred > observed) and
    /// renders attribution.
    ///
    /// `supersedes` carries the note id this note reverses, when
    /// applicable. NULL for first-time decisions. The audit display
    /// renders a `↳ REVERSED` line under the original on a match.
    ///
    /// CHECK enforcement on `source` is at the API layer (the
    /// [`NoteSource`] enum is the source of truth) rather than via
    /// SQL constraint — adding a new source becomes a one-line code
    /// change rather than a schema migration.
    ///
    /// Invariant: `scope == Feature` requires `feature_id.is_some()`;
    /// violators return [`Error::InvalidInput`].
    #[allow(clippy::too_many_arguments)]
    pub async fn write_note_with_source(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: NoteSource,
        supersedes: Option<&str>,
    ) -> Result<String> {
        self.write_note_full(
            kind,
            content,
            symbols,
            files,
            session_id,
            scope,
            feature_id,
            related_entity,
            source,
            supersedes,
            None,
        )
        .await
    }

    /// Full-fat v7 write path that also accepts a structured
    /// `payload_json` blob for per-kind data (e.g. `decision_kind`
    /// on `decision` rows, `authority` on `research_finding`,
    /// `category`/`status` on `recipe_issue`). The string is stored
    /// verbatim — callers serialise their own JSON and own its
    /// schema. NULL is the valid "no payload" value and matches
    /// pre-v7 semantics.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_note_full(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: NoteSource,
        supersedes: Option<&str>,
        payload_json: Option<&str>,
    ) -> Result<String> {
        self.write_note_full_v9(
            kind,
            content,
            symbols,
            files,
            session_id,
            scope,
            feature_id,
            related_entity,
            source,
            supersedes,
            payload_json,
            false,
        )
        .await
    }

    /// v9 write path that exposes the per-note `private` flag.
    /// Private notes are persisted locally but never enter the
    /// mesh wire (the `propagation_sink` is skipped). `false` is
    /// equivalent to [`write_note_full`] and is the safe default.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_note_full_v9(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: NoteSource,
        supersedes: Option<&str>,
        payload_json: Option<&str>,
        private: bool,
    ) -> Result<String> {
        if scope == NoteScope::Feature && feature_id.is_none() {
            return Err(Error::InvalidInput(
                "write_note_full: scope='feature' requires feature_id".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let symbols_json = serde_json::to_string(&symbols).unwrap_or_else(|_| "[]".to_string());
        let files_json = serde_json::to_string(&files).unwrap_or_else(|_| "[]".to_string());

        // Content hash identifies the note across peers; stable
        // across `origin_node_id` rotation. Computed before
        // locking so the cheap hash doesn't block other writers.
        let content_hash =
            compute_content_hash(kind, content, scope.as_str(), feature_id, session_id);

        // T1: compute the embedding outside the connection mutex.
        // The Embed slot can be local (microseconds) or HTTP
        // (tens of milliseconds); holding the SQLite lock across
        // a network round-trip starves other writers and gives
        // nothing back — we just need both rows to land in one
        // SQL transaction. Soft-fail per `ARCH §9`: a note with
        // no embedding is strictly better than no note.
        let embedding: Option<(Vec<f32>, String)> = match self.embed_fn.get() {
            Some(embed) => match embed(content).await {
                Ok(vec) => {
                    let model_id = std::env::var("SOVEREIGN_EMBED_MODEL_ID")
                        .unwrap_or_else(|_| "qwen-embedding-0.6b".to_string());
                    Some((vec, model_id))
                }
                Err(e) => {
                    tracing::warn!(
                        target = "notes",
                        error = %e,
                        note_id = %id,
                        "notes: embed_fn failed; persisting note without T1 embedding"
                    );
                    None
                }
            },
            None => None,
        };

        // T2: extract entities outside the connection mutex too.
        // Soft-fail with empty Vec on extractor error — author's
        // explicit `symbols` + `files` still seed the entity table
        // below, so the related-notes path keeps a signal even
        // when GLiNER is offline.
        let extracted_entities: Vec<(String, String)> = match self.gliner_fn.get() {
            Some(extract) => match extract(content).await {
                Ok(pairs) => pairs,
                Err(e) => {
                    tracing::warn!(
                        target = "notes",
                        error = %e,
                        note_id = %id,
                        "notes: gliner_fn failed; persisting note without T2 entities"
                    );
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

        // Author-supplied symbols + files always become entity
        // rows of kind="Symbol" / kind="File", even when GLiNER
        // isn't wired. This keeps `read_notes_related` useful for
        // the canonical "find related decisions by symbol" case
        // from day 0; GLiNER widens the recall later.
        let mut all_entities: Vec<(String, String)> = Vec::new();
        for s in &symbols {
            all_entities.push((s.clone(), "Symbol".to_string()));
        }
        for f in &files {
            all_entities.push((f.clone(), "File".to_string()));
        }
        for (e, k) in &extracted_entities {
            all_entities.push((e.clone(), k.clone()));
        }

        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN").map_err(sqlite_err)?;
        let txn_result: Result<()> = (|| {
            conn.execute(
                "INSERT INTO notes (
                    id, kind, content, symbols, files, session_id,
                    created_at, updated_at, scope, feature_id,
                    related_entity, source, supersedes, payload_json,
                    content_hash, private, origin_node_id
                )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    id, kind, content, symbols_json, files_json, session_id, now,
                    scope.as_str(), feature_id, related_entity,
                    source.as_str(), supersedes, payload_json,
                    content_hash,
                    private as i64,
                    self.origin_node_id.get().map(String::as_str)
                ],
            )
            .map_err(sqlite_err)?;
            if let Some((vec, model_id)) = &embedding {
                let dim = vec.len() as i64;
                let bytes = embedding_to_le_bytes(vec);
                conn.execute(
                    "INSERT INTO note_embeddings (
                        note_id, embedding, model_id, dim, created_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, &bytes[..], model_id, dim, now],
                )
                .map_err(sqlite_err)?;
                tracing::debug!(
                    target = "notes",
                    note_id = %id,
                    dim,
                    model = %model_id,
                    "notes: embedding persisted"
                );
            }
            // T2 entities — `INSERT OR IGNORE` so duplicates within
            // the merged list (author + extracted) collapse on the
            // composite primary key.
            for (entity, kind) in &all_entities {
                conn.execute(
                    "INSERT OR IGNORE INTO note_entities (
                        note_id, entity, kind, salience, created_at
                    ) VALUES (?1, ?2, ?3, 1.0, ?4)",
                    params![id, entity, kind, now],
                )
                .map_err(sqlite_err)?;
            }
            bump_notes_version(&conn)?;
            Ok(())
        })();
        match txn_result {
            Ok(()) => {
                conn.execute_batch("COMMIT").map_err(sqlite_err)?;
                drop(conn);

                // Sink only fires for global, non-private notes —
                // session/feature scope stays node-local; private
                // notes are structurally excluded from gossip even
                // if a future caller wires the sink to the
                // non-private app_id. Fire AFTER commit so peers
                // only see notes that landed locally.
                if !private && scope == NoteScope::Global {
                    if let Some(sink) = self.propagation_sink.get() {
                        let event = NotePropagationEvent {
                            content_hash: content_hash.clone(),
                            note: ExportedNoteRow {
                                id: id.clone(),
                                kind: kind.to_string(),
                                content: content.to_string(),
                                symbols,
                                files,
                                session_id: session_id.to_string(),
                                created_at: now,
                                scope: scope.as_str().to_string(),
                                feature_id: feature_id.map(str::to_string),
                                related_entity: related_entity.map(str::to_string),
                                source: source.as_str().to_string(),
                                supersedes: supersedes.map(str::to_string),
                                payload_json: payload_json.map(str::to_string),
                                origin_node_id: self.origin_node_id.get().cloned(),
                            },
                            embedding: embedding.map(|(vec, model_id)| ExportedNoteEmbedding {
                                model_id,
                                dim: vec.len() as i64,
                                embedding: embedding_to_le_bytes(&vec),
                            }),
                            entities: all_entities
                                .iter()
                                .map(|(e, k)| ExportedNoteEntity {
                                    entity: e.clone(),
                                    kind: k.clone(),
                                })
                                .collect(),
                            tombstone: false,
                            updated_at: now,
                        };
                        sink(&event);
                    }
                }
                Ok(id)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Mark a note tombstoned (soft-delete for propagation).
    /// Tombstoned notes survive in the local table — the
    /// `supersedes` chain may still land later edits — but are
    /// filtered out of the audit display and the delta scan, and
    /// the tombstone propagates so peers converge.
    ///
    /// Tombstone wins over any concurrent edit per the Step 6b
    /// conflict-resolution policy: a later `updated_at` on an
    /// edit does NOT resurrect a tombstoned note. To recover a
    /// tombstoned note, call again with `tombstone = false`.
    pub async fn set_note_tombstone(&self, note_id: &str, tombstone: bool) -> Result<()> {
        let now = unix_now();
        let (content_hash, scope, private_flag) = {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE notes
                    SET tombstone = ?1,
                        updated_at = ?2
                  WHERE id = ?3",
                params![tombstone as i64, now, note_id],
            )
            .map_err(sqlite_err)?;
            // Fetch the propagation fields for the sink event.
            let row: Result<(String, String, i64)> = conn
                .query_row(
                    "SELECT content_hash, scope, private FROM notes WHERE id = ?",
                    params![note_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(sqlite_err);
            row?
        };

        // Propagate the tombstone via the same sink. Skip if scope
        // != global or private, mirroring write_note_full_v9.
        if !tombstone || private_flag != 0 || scope != "global" {
            return Ok(());
        }
        if let Some(sink) = self.propagation_sink.get() {
            // Tombstone wire format: same content_hash + a stub
            // ExportedNoteRow + tombstone=true. Peers receiving
            // this hit the tombstone branch in ingest_remote_notes
            // and update the row in place; they don't need the
            // full content to apply a tombstone.
            let event = NotePropagationEvent {
                content_hash,
                note: ExportedNoteRow {
                    id: note_id.to_string(),
                    kind: String::new(),
                    content: String::new(),
                    symbols: Vec::new(),
                    files: Vec::new(),
                    session_id: String::new(),
                    created_at: now,
                    scope: scope.clone(),
                    feature_id: None,
                    related_entity: None,
                    source: String::new(),
                    supersedes: None,
                    payload_json: None,
                    origin_node_id: self.origin_node_id.get().cloned(),
                },
                embedding: None,
                entities: Vec::new(),
                tombstone: true,
                updated_at: now,
            };
            sink(&event);
        }
        Ok(())
    }

    /// Whether a note carries an active tombstone. `false` for
    /// unknown ids. Lookup helper for callers that need to render
    /// tombstone state separately from the row body.
    pub async fn is_note_tombstoned(&self, note_id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let row: rusqlite::Result<i64> = conn.query_row(
            "SELECT tombstone FROM notes WHERE id = ?",
            params![note_id],
            |r| r.get(0),
        );
        match row {
            Ok(v) => Ok(v != 0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(other) => Err(sqlite_err(other)),
        }
    }

    /// Apply a batch of [`NotePropagationEvent`]s received from
    /// peers. Idempotent (dedup by `content_hash`), tombstone-wins,
    /// fork-preserving (per Step 6b). Returns counts so the
    /// caller can log convergence progress.
    ///
    /// Filters at the boundary:
    /// - `scope != "global"` events are rejected (peers shouldn't
    ///   send anything but global notes anyway; this is belt-and-
    ///   braces). The transport-level `app_id="notes-private"`
    ///   gossip exclusion is the structural privacy guarantee.
    /// - Identical `content_hash` → idempotent INSERT OR IGNORE.
    /// - Tombstone → apply regardless of timestamp ordering.
    /// - Concurrent supersedes (same `supersedes` target as a
    ///   locally-known sibling) → set `fork_of` on the inserted
    ///   row, preserving both branches.
    ///
    /// All applied changes land in a single SQL transaction so
    /// partial application can't leave torn state.
    pub async fn ingest_remote_notes(
        &self,
        events: Vec<NotePropagationEvent>,
    ) -> Result<IngestRemoteReport> {
        let mut report = IngestRemoteReport::default();
        if events.is_empty() {
            return Ok(report);
        }
        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN").map_err(sqlite_err)?;
        let txn: Result<()> = (|| {
            for ev in &events {
                if ev.note.scope != "global" {
                    report.rejected += 1;
                    continue;
                }
                // Look up any existing row with this content_hash.
                let existing: Option<(String, i64)> = conn
                    .query_row(
                        "SELECT id, tombstone FROM notes WHERE content_hash = ? LIMIT 1",
                        params![ev.content_hash],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .ok();

                if let Some((existing_id, existing_tomb)) = existing {
                    if ev.tombstone && existing_tomb == 0 {
                        // Tombstone-wins: apply the soft-delete
                        // regardless of `updated_at`.
                        conn.execute(
                            "UPDATE notes
                                SET tombstone = 1,
                                    updated_at = ?1
                              WHERE id = ?2",
                            params![ev.updated_at, existing_id],
                        )
                        .map_err(sqlite_err)?;
                        report.tombstoned += 1;
                        tracing::debug!(
                            target = "notes",
                            content_hash = %ev.content_hash,
                            "notes: ingest applied tombstone"
                        );
                    } else {
                        // Idempotent — same hash already present.
                        report.deduplicated += 1;
                    }
                    continue;
                }

                if ev.tombstone {
                    // Tombstone for an unknown note — insert a
                    // stub row carrying the tombstone so a later
                    // arrival of the actual write can be shadowed
                    // by it. Without this, the tombstone would
                    // disappear and a stale write could resurrect.
                    conn.execute(
                        "INSERT INTO notes (
                            id, kind, content, symbols, files, session_id,
                            created_at, updated_at, scope, source,
                            content_hash, tombstone, origin_node_id
                        ) VALUES (?1, 'todo', '', '[]', '[]', '', ?2, ?2, 'global', 'agent', ?3, 1, ?4)",
                        params![
                            ev.note.id,
                            ev.updated_at,
                            ev.content_hash,
                            ev.note.origin_node_id
                        ],
                    )
                    .map_err(sqlite_err)?;
                    report.tombstoned += 1;
                    continue;
                }

                // Check for concurrent-supersedes fork: if this
                // event supersedes a base note that a locally-
                // known note also supersedes, the new note becomes
                // a sibling fork rather than a silent collapse.
                let fork_of: Option<String> = if let Some(superseded) = &ev.note.supersedes {
                    conn.query_row(
                        "SELECT id FROM notes
                          WHERE supersedes = ?1
                            AND content_hash <> ?2
                          LIMIT 1",
                        params![superseded, ev.content_hash],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                } else {
                    None
                };

                let symbols_json =
                    serde_json::to_string(&ev.note.symbols).unwrap_or_else(|_| "[]".to_string());
                let files_json =
                    serde_json::to_string(&ev.note.files).unwrap_or_else(|_| "[]".to_string());
                conn.execute(
                    "INSERT INTO notes (
                        id, kind, content, symbols, files, session_id,
                        created_at, updated_at, scope, feature_id,
                        related_entity, source, supersedes, payload_json,
                        content_hash, origin_node_id, fork_of
                    )
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                    params![
                        ev.note.id,
                        ev.note.kind,
                        ev.note.content,
                        symbols_json,
                        files_json,
                        ev.note.session_id,
                        ev.note.created_at,
                        ev.note.scope,
                        ev.note.feature_id,
                        ev.note.related_entity,
                        ev.note.source,
                        ev.note.supersedes,
                        ev.note.payload_json,
                        ev.content_hash,
                        ev.note.origin_node_id,
                        fork_of.clone(),
                    ],
                )
                .map_err(sqlite_err)?;
                report.inserted += 1;
                if fork_of.is_some() {
                    report.forked += 1;
                    tracing::info!(
                        target = "notes",
                        content_hash = %ev.content_hash,
                        sibling = ?fork_of,
                        "notes: ingest preserved concurrent-supersedes fork"
                    );
                }

                if let Some(emb) = &ev.embedding {
                    conn.execute(
                        "INSERT INTO note_embeddings (
                            note_id, embedding, model_id, dim, created_at
                        ) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            ev.note.id,
                            &emb.embedding[..],
                            emb.model_id,
                            emb.dim,
                            ev.note.created_at
                        ],
                    )
                    .map_err(sqlite_err)?;
                }
                for ent in &ev.entities {
                    conn.execute(
                        "INSERT OR IGNORE INTO note_entities (
                            note_id, entity, kind, salience, created_at
                        ) VALUES (?1, ?2, ?3, 1.0, ?4)",
                        params![ev.note.id, ent.entity, ent.kind, ev.note.created_at],
                    )
                    .map_err(sqlite_err)?;
                }
            }
            bump_notes_version(&conn)?;
            Ok(())
        })();
        match txn {
            Ok(()) => {
                conn.execute_batch("COMMIT").map_err(sqlite_err)?;
                tracing::debug!(
                    target = "notes",
                    inserted = report.inserted,
                    deduplicated = report.deduplicated,
                    tombstoned = report.tombstoned,
                    forked = report.forked,
                    rejected = report.rejected,
                    "notes: ingest_remote_notes complete"
                );
                Ok(report)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Per-round propagation delta: notes for `peer_node_id` that
    /// landed locally since the last successful ACK. Bounded by
    /// `limit` (cap 200 in production; large catch-ups span
    /// multiple rounds).
    ///
    /// Filters mirror Step 5's restraint: `scope='global' AND
    /// private=0 AND tombstone=0`. Tombstones are propagated via
    /// the separate sink-on-tombstone path so they don't show up
    /// here — peers learn deletion from the same wire they
    /// learned the note from.
    ///
    /// Returns events with full embeddings + entities attached
    /// (one query for the note rows + one for the embeddings,
    /// joined in-process).
    pub async fn notes_delta_since(
        &self,
        peer_node_id: &str,
        limit: usize,
    ) -> Result<Vec<NotePropagationEvent>> {
        let conn = self.conn.lock().await;
        let watermark: i64 = conn
            .query_row(
                "SELECT last_sent_created_at FROM note_propagation_watermark
                   WHERE peer_node_id = ?",
                params![peer_node_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let cap = limit.min(500);
        let mut stmt = conn
            .prepare(
                "SELECT n.id, n.kind, n.content, n.symbols, n.files,
                        n.session_id, n.created_at, n.scope, n.feature_id,
                        n.related_entity, n.source, n.supersedes,
                        n.payload_json, n.origin_node_id, n.content_hash,
                        n.updated_at,
                        e.embedding, e.model_id, e.dim
                   FROM notes n
              LEFT JOIN note_embeddings e ON e.note_id = n.id
                  WHERE n.scope = 'global'
                    AND n.private = 0
                    AND n.tombstone = 0
                    AND n.created_at > ?1
                    AND n.content_hash IS NOT NULL
               ORDER BY n.created_at ASC, n.id ASC
                  LIMIT ?2",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![watermark, cap as i64], |row| {
                let symbols_json: String = row.get(3)?;
                let files_json: String = row.get(4)?;
                let symbols: Vec<String> = serde_json::from_str(&symbols_json).unwrap_or_default();
                let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();
                let embedding_bytes: Option<Vec<u8>> = row.get(16)?;
                let embedding_model: Option<String> = row.get(17)?;
                let embedding_dim: Option<i64> = row.get(18)?;
                let embedding = match (embedding_bytes, embedding_model, embedding_dim) {
                    (Some(b), Some(m), Some(d)) => Some(ExportedNoteEmbedding {
                        model_id: m,
                        dim: d,
                        embedding: b,
                    }),
                    _ => None,
                };
                Ok(NotePropagationEvent {
                    content_hash: row.get(14)?,
                    note: ExportedNoteRow {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        content: row.get(2)?,
                        symbols,
                        files,
                        session_id: row.get(5)?,
                        created_at: row.get(6)?,
                        scope: row.get(7)?,
                        feature_id: row.get(8)?,
                        related_entity: row.get(9)?,
                        source: row.get(10)?,
                        supersedes: row.get(11)?,
                        payload_json: row.get(12)?,
                        origin_node_id: row.get(13)?,
                    },
                    embedding,
                    entities: Vec::new(),
                    tombstone: false,
                    updated_at: row.get(15)?,
                })
            })
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    /// Persist the propagation watermark for `peer_node_id`. Call
    /// after a successful gossip round so the next round only
    /// ships the delta beyond this point. The store records both
    /// the `created_at` bound and the last note id — the id
    /// disambiguates within-second ties.
    pub async fn set_propagation_watermark(
        &self,
        peer_node_id: &str,
        last_sent_created_at: i64,
        last_sent_note_id: &str,
    ) -> Result<()> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO note_propagation_watermark (
                peer_node_id, last_sent_created_at, last_sent_note_id, last_acked_at
            ) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(peer_node_id) DO UPDATE SET
                last_sent_created_at = excluded.last_sent_created_at,
                last_sent_note_id    = excluded.last_sent_note_id,
                last_acked_at        = excluded.last_acked_at",
            params![peer_node_id, last_sent_created_at, last_sent_note_id, now],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Bucketed content-hash digest for reconciliation. Bucket id
    /// is the first two hex chars of `content_hash` (256 buckets).
    /// Each bucket's value is FNV-1a-64 over the
    /// lexicographically-sorted hashes that fall inside it.
    ///
    /// Cheap on the wire (~2KB for 256 u64 buckets) and answers
    /// "do peer A and peer B agree on every note?" with one
    /// round-trip. Per-bucket disagreement triggers the wider
    /// pull via [`content_hashes_in_bucket`].
    ///
    /// The Phase-A reconciliation path lives at this granularity
    /// to keep the digest stable across small note arrivals — a
    /// single note write only mutates one bucket, so a peer's
    /// digest diff isolates the change cheaply.
    pub async fn content_hash_digest(&self) -> Result<std::collections::BTreeMap<u8, u64>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT content_hash FROM notes
                  WHERE scope = 'global'
                    AND private = 0
                    AND content_hash IS NOT NULL
               ORDER BY content_hash ASC",
            )
            .map_err(sqlite_err)?;
        let hashes: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        let mut by_bucket: std::collections::BTreeMap<u8, Vec<&str>> = Default::default();
        for h in &hashes {
            let Some(bucket) = parse_bucket_id(h) else {
                continue;
            };
            by_bucket.entry(bucket).or_default().push(h.as_str());
        }
        let mut digest = std::collections::BTreeMap::new();
        for (bucket, list) in by_bucket {
            digest.insert(bucket, fnv1a_64_strings(&list));
        }
        Ok(digest)
    }

    /// Return every `content_hash` in `bucket`. Called when a
    /// peer's digest disagrees with ours on this bucket — both
    /// sides exchange lists, diff, pull missing.
    pub async fn content_hashes_in_bucket(&self, bucket: u8) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        // Hex prefix: bucket 0x3A → "3a". Two lowercase hex chars
        // pinned by content_hash output format (compute_content_hash).
        let prefix = format!("{:02x}", bucket);
        let pattern = format!("{}%", prefix);
        let mut stmt = conn
            .prepare(
                "SELECT content_hash FROM notes
                  WHERE scope = 'global'
                    AND private = 0
                    AND content_hash LIKE ?1
               ORDER BY content_hash ASC",
            )
            .map_err(sqlite_err)?;
        let out = stmt
            .query_map(params![pattern], |r| r.get::<_, String>(0))
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        Ok(out)
    }

    /// Pull full propagation events for a specific set of
    /// `content_hash`es — the second leg of the reconciliation
    /// dance after bucket diff. Returns events in the same order
    /// as `hashes`.
    pub async fn events_for_content_hashes(
        &self,
        hashes: &[String],
    ) -> Result<Vec<NotePropagationEvent>> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().await;
        let mut placeholders = String::new();
        for i in 0..hashes.len() {
            if i > 0 {
                placeholders.push(',');
            }
            placeholders.push('?');
        }
        let sql = format!(
            "SELECT n.id, n.kind, n.content, n.symbols, n.files,
                    n.session_id, n.created_at, n.scope, n.feature_id,
                    n.related_entity, n.source, n.supersedes,
                    n.payload_json, n.origin_node_id, n.content_hash,
                    n.updated_at, n.tombstone,
                    e.embedding, e.model_id, e.dim
               FROM notes n
          LEFT JOIN note_embeddings e ON e.note_id = n.id
              WHERE n.content_hash IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
        let bound: Vec<rusqlite::types::Value> = hashes
            .iter()
            .map(|s| rusqlite::types::Value::Text(s.clone()))
            .collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bound), |row| {
                let symbols_json: String = row.get(3)?;
                let files_json: String = row.get(4)?;
                let symbols: Vec<String> = serde_json::from_str(&symbols_json).unwrap_or_default();
                let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();
                let tombstone: i64 = row.get(16)?;
                let embedding_bytes: Option<Vec<u8>> = row.get(17)?;
                let embedding_model: Option<String> = row.get(18)?;
                let embedding_dim: Option<i64> = row.get(19)?;
                let embedding = match (embedding_bytes, embedding_model, embedding_dim) {
                    (Some(b), Some(m), Some(d)) => Some(ExportedNoteEmbedding {
                        model_id: m,
                        dim: d,
                        embedding: b,
                    }),
                    _ => None,
                };
                Ok(NotePropagationEvent {
                    content_hash: row.get(14)?,
                    note: ExportedNoteRow {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        content: row.get(2)?,
                        symbols,
                        files,
                        session_id: row.get(5)?,
                        created_at: row.get(6)?,
                        scope: row.get(7)?,
                        feature_id: row.get(8)?,
                        related_entity: row.get(9)?,
                        source: row.get(10)?,
                        supersedes: row.get(11)?,
                        payload_json: row.get(12)?,
                        origin_node_id: row.get(13)?,
                    },
                    embedding,
                    entities: Vec::new(),
                    tombstone: tombstone != 0,
                    updated_at: row.get(15)?,
                })
            })
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    /// One-shot backfill: compute T1 embeddings for every note
    /// that doesn't have one yet, and T2 entities (author symbols
    /// + GLiNER, if wired) for every note that doesn't have any.
    /// Called once at daemon startup to retrofit pre-v9 notes
    /// (and pre-T1/T2 wiring) so the existing corpus benefits
    /// from semantic recall + related-notes lookup immediately
    /// instead of only when notes get re-written.
    ///
    /// Best-effort: rows that error out (embed slot busy,
    /// extractor failure) are skipped with a warn; the next
    /// invocation picks them up. Returns counts so the caller
    /// can log convergence progress.
    ///
    /// Bounded by `max_per_run` (0 → unlimited). The daemon
    /// passes a generous cap so a fresh DB with 10k notes finishes
    /// in one pass; tests pass a small cap.
    pub async fn backfill_tier_artifacts(&self, max_per_run: usize) -> BackfillReport {
        let mut report = BackfillReport::default();
        if self.embed_fn.get().is_none() && self.gliner_fn.get().is_none() {
            return report;
        }

        // Pull candidate ids in two passes (T1 + T2) so a missing
        // embedding doesn't block an entity backfill on the same row.
        let embed_targets: Vec<(String, String)> = {
            let conn = self.conn.lock().await;
            let cap = if max_per_run == 0 {
                i64::MAX
            } else {
                max_per_run as i64
            };
            let mut stmt = conn
                .prepare(
                    "SELECT n.id, n.content FROM notes n
                       LEFT JOIN note_embeddings e ON e.note_id = n.id
                      WHERE e.note_id IS NULL
                        AND n.retired_at IS NULL
                        AND n.tombstone = 0
                   ORDER BY n.created_at DESC
                      LIMIT ?",
                )
                .ok();
            match stmt.as_mut() {
                Some(s) => match s.query_map(params![cap], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                }) {
                    Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
                    Err(_) => Vec::new(),
                },
                None => Vec::new(),
            }
        };

        if let Some(embed_fn) = self.embed_fn.get().cloned() {
            let model_id = std::env::var("SOVEREIGN_EMBED_MODEL_ID")
                .unwrap_or_else(|_| "qwen-embedding-0.6b".to_string());
            for (note_id, content) in &embed_targets {
                let vec = match embed_fn(content).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!(
                            target = "notes",
                            note_id = %note_id,
                            error = %e,
                            "notes: backfill embed failed; will retry next run"
                        );
                        report.embed_skipped += 1;
                        continue;
                    }
                };
                let bytes = embedding_to_le_bytes(&vec);
                let dim = vec.len() as i64;
                let now = unix_now();
                let conn = self.conn.lock().await;
                match conn.execute(
                    "INSERT OR IGNORE INTO note_embeddings (
                        note_id, embedding, model_id, dim, created_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![note_id, &bytes[..], model_id, dim, now],
                ) {
                    Ok(_) => report.embeddings_backfilled += 1,
                    Err(e) => {
                        tracing::warn!(
                            target = "notes",
                            note_id = %note_id,
                            error = %e,
                            "notes: backfill embed INSERT failed"
                        );
                        report.embed_skipped += 1;
                    }
                }
            }
        }

        // T2 backfill — symbols + files always; GLiNER content
        // pass only when wired. Pulls every note that has zero
        // entity rows; this lets read_notes_related find related
        // by author-supplied symbols even when GLiNER is offline.
        let entity_targets: Vec<(String, String, Vec<String>, Vec<String>)> = {
            let conn = self.conn.lock().await;
            let cap = if max_per_run == 0 {
                i64::MAX
            } else {
                max_per_run as i64
            };
            let mut stmt = conn
                .prepare(
                    "SELECT n.id, n.content, n.symbols, n.files FROM notes n
                       LEFT JOIN note_entities ne ON ne.note_id = n.id
                      WHERE ne.note_id IS NULL
                        AND n.retired_at IS NULL
                        AND n.tombstone = 0
                   ORDER BY n.created_at DESC
                      LIMIT ?",
                )
                .ok();
            match stmt.as_mut() {
                Some(s) => match s.query_map(params![cap], |r| {
                    let symbols_json: String = r.get(2)?;
                    let files_json: String = r.get(3)?;
                    let symbols: Vec<String> =
                        serde_json::from_str(&symbols_json).unwrap_or_default();
                    let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        symbols,
                        files,
                    ))
                }) {
                    Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
                    Err(_) => Vec::new(),
                },
                None => Vec::new(),
            }
        };

        for (note_id, content, symbols, files) in &entity_targets {
            let mut entities: Vec<(String, String)> = Vec::new();
            for s in symbols {
                entities.push((s.clone(), "Symbol".to_string()));
            }
            for f in files {
                entities.push((f.clone(), "File".to_string()));
            }
            if let Some(extract) = self.gliner_fn.get().cloned() {
                match extract(content).await {
                    Ok(pairs) => entities.extend(pairs),
                    Err(e) => {
                        tracing::debug!(
                            target = "notes",
                            note_id = %note_id,
                            error = %e,
                            "notes: backfill gliner failed; using author-supplied symbols only"
                        );
                    }
                }
            }
            if entities.is_empty() {
                report.entity_skipped += 1;
                continue;
            }
            let now = unix_now();
            let conn = self.conn.lock().await;
            for (entity, kind) in &entities {
                if conn
                    .execute(
                        "INSERT OR IGNORE INTO note_entities (
                            note_id, entity, kind, salience, created_at
                        ) VALUES (?1, ?2, ?3, 1.0, ?4)",
                        params![note_id, entity, kind, now],
                    )
                    .is_ok()
                {
                    report.entities_backfilled += 1;
                }
            }
        }

        tracing::info!(
            target = "notes",
            embeddings = report.embeddings_backfilled,
            embed_skipped = report.embed_skipped,
            entities = report.entities_backfilled,
            entity_skipped = report.entity_skipped,
            "notes: backfill_tier_artifacts complete"
        );
        report
    }

    /// T2 surface: find notes related to a symbol / file / entity
    /// via the `note_entities` co-occurrence graph.
    ///
    /// Algorithm (v1, no PPR):
    /// 1. Find every entity row whose `entity` equals `seed`
    ///    (case-sensitive — symbols + files are exact-token,
    ///    GLiNER-extracted entities preserve their surface form).
    /// 2. The notes those entity rows belong to are the
    ///    **seed notes**.
    /// 3. Pull every entity from every seed note → the
    ///    **seed entity bag**.
    /// 4. Score every note by how many entities it shares with
    ///    the seed bag (excluding the seed notes themselves).
    /// 5. Return top-`k` by overlap count, tombstoned + retired
    ///    notes filtered out.
    ///
    /// Empty result when `note_entities` is empty (no T2 writes
    /// have landed yet) or no seed match — the caller falls back
    /// to FTS5 / semantic blend.
    ///
    /// PPR-seeded ranking (per `PROGRESSIVE_ENRICHMENT.md` step 2)
    /// is a v2 optimisation that swaps the overlap-count score
    /// for a diffusion score over the bipartite entity↔note
    /// graph. Same input + output shape, deeper signal.
    pub async fn read_notes_related(&self, seed: &str, k: usize) -> Result<Vec<NoteRow>> {
        let cap = k.min(100);
        let conn = self.conn.lock().await;

        // Step 1+2: seed note ids. Match the seed against the
        // `entity` column directly. Symbols + files land as
        // entities with kind="Symbol"/"File" on write so the
        // index covers both axes from one column.
        let seed_note_ids: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT note_id FROM note_entities WHERE entity = ?")
                .map_err(sqlite_err)?;
            let mapped = stmt
                .query_map(params![seed], |r| r.get::<_, String>(0))
                .map_err(sqlite_err)?;
            let collected: rusqlite::Result<Vec<String>> = mapped.collect();
            collected.map_err(sqlite_err)?
        };
        if seed_note_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Step 3: collect every entity from those seed notes.
        let seed_entities: Vec<(String, String)> = {
            let mut placeholders = String::new();
            for i in 0..seed_note_ids.len() {
                if i > 0 {
                    placeholders.push(',');
                }
                placeholders.push('?');
            }
            let sql = format!(
                "SELECT DISTINCT entity, kind FROM note_entities WHERE note_id IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
            let bound: Vec<rusqlite::types::Value> = seed_note_ids
                .iter()
                .map(|s| rusqlite::types::Value::Text(s.clone()))
                .collect();
            let mapped = stmt
                .query_map(rusqlite::params_from_iter(bound), |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(sqlite_err)?;
            let collected: rusqlite::Result<Vec<(String, String)>> = mapped.collect();
            collected.map_err(sqlite_err)?
        };
        if seed_entities.is_empty() {
            return Ok(Vec::new());
        }

        // Step 4+5: score every other note by entity overlap.
        // SQL does the join + count + order in one statement.
        // Build the `(entity, kind) IN (...)` tuple bag.
        let mut entity_clause = String::from("(");
        for i in 0..seed_entities.len() {
            if i > 0 {
                entity_clause.push(',');
            }
            entity_clause.push_str("(?, ?)");
        }
        entity_clause.push(')');
        let mut seed_id_clause = String::new();
        for i in 0..seed_note_ids.len() {
            if i > 0 {
                seed_id_clause.push(',');
            }
            seed_id_clause.push('?');
        }
        let sql = format!(
            "WITH scored AS (
               SELECT ne.note_id, COUNT(*) AS overlap
                 FROM note_entities ne
                WHERE (ne.entity, ne.kind) IN {entity_clause}
                  AND ne.note_id NOT IN ({seed_id_clause})
             GROUP BY ne.note_id
             )
             SELECT n.id, n.kind, n.content, n.symbols, n.files, n.session_id,
                    n.created_at, n.tool_name, n.retired_at, n.retired_by,
                    n.scope, n.feature_id, n.promoted_from, n.related_entity,
                    n.source, n.supersedes, n.payload_json,
                    s.overlap
               FROM notes n
               JOIN scored s ON s.note_id = n.id
              WHERE n.retired_at IS NULL AND n.tombstone = 0
           ORDER BY s.overlap DESC, n.created_at DESC
              LIMIT ?"
        );
        let mut bound: Vec<rusqlite::types::Value> = Vec::new();
        for (e, k) in &seed_entities {
            bound.push(rusqlite::types::Value::Text(e.clone()));
            bound.push(rusqlite::types::Value::Text(k.clone()));
        }
        for nid in &seed_note_ids {
            bound.push(rusqlite::types::Value::Text(nid.clone()));
        }
        bound.push(rusqlite::types::Value::Integer(cap as i64));
        let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bound), |row| map_note_row(row))
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(sqlite_err)?);
        }
        tracing::debug!(
            target = "notes",
            seed,
            seed_notes = seed_note_ids.len(),
            seed_entities = seed_entities.len(),
            related = out.len(),
            "notes: read_notes_related"
        );
        Ok(out)
    }

    /// Persist a reflection note at global scope. Back-compat wrapper.
    pub async fn write_reflection(
        &self,
        content: &str,
        tool_name: Option<&str>,
        session_id: &str,
    ) -> Result<String> {
        self.write_reflection_scoped(content, tool_name, session_id, NoteScope::Global, None)
            .await
    }

    /// Persist a reflection note with an explicit scope.
    pub async fn write_reflection_scoped(
        &self,
        content: &str,
        tool_name: Option<&str>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
    ) -> Result<String> {
        if scope == NoteScope::Feature && feature_id.is_none() {
            return Err(Error::InvalidInput(
                "write_reflection_scoped: scope='feature' requires feature_id".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at, tool_name, scope, feature_id)
             VALUES (?1, 'reflection', ?2, '[]', '[]', ?3, ?4, ?4, ?5, ?6, ?7)",
            params![id, content, session_id, now, tool_name, scope.as_str(), feature_id],
        )
        .map_err(sqlite_err)?;
        bump_notes_version(&conn)?;

        Ok(id)
    }

    /// Rewrite `id`'s scope (and optional `feature_id`) to match a promotion.
    ///
    /// Returns the newly inserted promoted note id (a fresh row is created;
    /// the source row is left intact for audit). The new row carries
    /// `promoted_from = <source id>`.
    pub async fn promote_note(
        &self,
        source_id: &str,
        new_scope: NoteScope,
        new_feature_id: Option<&str>,
        new_content: Option<&str>,
    ) -> Result<String> {
        if new_scope == NoteScope::Feature && new_feature_id.is_none() {
            return Err(Error::InvalidInput(
                "promote_note: scope='feature' requires feature_id".into(),
            ));
        }

        let new_id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();

        let conn = self.conn.lock().await;
        let (kind, content, symbols, files, session_id, tool_name): (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT kind, content, symbols, files, session_id, tool_name
                 FROM notes WHERE id = ?",
                params![source_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::InvalidInput(format!("promote_note: source id not found: {source_id}"))
                }
                other => sqlite_err(other),
            })?;

        let final_content = new_content.unwrap_or(&content);
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at, tool_name, scope, feature_id, promoted_from)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11)",
            params![
                new_id,
                kind,
                final_content,
                symbols,
                files,
                session_id,
                now,
                tool_name,
                new_scope.as_str(),
                new_feature_id,
                source_id,
            ],
        )
        .map_err(sqlite_err)?;
        bump_notes_version(&conn)?;

        Ok(new_id)
    }

    /// Look up a single note by id, or return `None` when not found.
    ///
    /// Used by compaction-recovery paths: a digest references notes by id
    /// (`[note:abc-123]`), and the agent calls this to fetch the full row
    /// only for those it needs.
    pub async fn read_note_by_id(&self, id: &str) -> Result<Option<NoteRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT id, kind, content, symbols, files, session_id,
                        created_at, tool_name, retired_at, retired_by,
                        scope, feature_id, promoted_from, related_entity,
                        source, supersedes, payload_json
                 FROM notes WHERE id = ?",
                params![id],
                map_note_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(sqlite_err(other)),
            })?;
        Ok(row)
    }

    /// Returns the current monotonic counter that increments on every note
    /// write / delete / retire. Used by the digest cache in M1.4 to key
    /// cached digests without invalidating on every call.
    pub async fn notes_version(&self) -> Result<i64> {
        let conn = self.conn.lock().await;
        let v: i64 = conn
            .query_row(
                "SELECT val FROM meta_counters WHERE key = 'notes_version'",
                [],
                |r| r.get(0),
            )
            .map_err(sqlite_err)?;
        Ok(v)
    }

    /// Look up a cached digest by `(scope_hash, notes_version)`.
    ///
    /// Returns `None` if no matching row exists — the caller
    /// (`ReadNoteDigestTool`) should regenerate via the Fast slot and
    /// write back with [`digest_cache_put`](Self::digest_cache_put).
    pub async fn digest_cache_get(
        &self,
        scope_hash: &str,
        notes_version: i64,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let row: rusqlite::Result<String> = conn.query_row(
            "SELECT digest_md FROM note_digest_cache
             WHERE scope_hash = ?1 AND notes_version = ?2",
            params![scope_hash, notes_version],
            |r| r.get(0),
        );
        match row {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(sqlite_err(e)),
        }
    }

    /// Store a digest for `(scope_hash, notes_version)`. `INSERT OR
    /// REPLACE` semantics — racing regens (two callers computed the
    /// same digest at the same version) converge on the later write
    /// without erroring.
    pub async fn digest_cache_put(
        &self,
        scope_hash: &str,
        notes_version: i64,
        digest_md: &str,
        token_count: i64,
    ) -> Result<()> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO note_digest_cache
                (scope_hash, notes_version, digest_md, token_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![scope_hash, notes_version, digest_md, token_count, now],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    // ── Note reads ─────────────────────────────────────────────────────────

    /// Query notes. Filters compose with AND.
    ///
    /// - `query` — FTS5 full-text search, ordered by BM25 relevance.
    ///   When `None`, results are ordered by recency (newest first).
    /// - `symbols` — retain notes that mention any of these symbol names.
    /// - `files` — retain notes that mention any of these file paths.
    /// - `kinds` — retain notes whose `kind` is in this list.
    /// - `limit` — maximum number of results (capped at 100 internally).
    /// - `include_retired` — when `false` (default for agents), retired reflections
    ///   are filtered out. Pass `true` for developer history views.
    pub async fn read_notes(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
    ) -> Result<Vec<NoteRow>> {
        self.read_notes_scoped(
            query,
            symbols,
            files,
            kinds,
            limit,
            include_retired,
            &ScopeFilter::default(),
        )
        .await
    }

    /// Query notes with an additional scope predicate.
    ///
    /// All filters from [`read_notes`] apply. Scope filtering is a post-match
    /// step (like `kinds`) because the FTS5 index is not aware of the `scope`
    /// column — see `idx_notes_scope_feature` for the recency path.
    pub async fn read_notes_scoped(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
        scope_filter: &ScopeFilter,
    ) -> Result<Vec<NoteRow>> {
        let cap = limit.min(100);
        // `scope` is the only filter still applied post-fetch; SQL covers
        // kinds, symbols, and files via WHERE so the LIMIT window no longer
        // hides notes that match an exact symbol/file/kind written outside
        // the most-recent N rows. Over-fetch only when the residual
        // scope_filter is active.
        let post_fetch_active = !scope_filter_is_no_op(scope_filter);
        let fetch_limit = if post_fetch_active { cap * 10 } else { cap };

        let retired_clause = if include_retired {
            ""
        } else {
            "AND n.retired_at IS NULL"
        };

        // Build WHERE fragments + bound params in lock-step. Each
        // `EXISTS (SELECT 1 FROM json_each(n.<col>) WHERE value IN (?,?,…))`
        // matches if any candidate value appears anywhere in the JSON array
        // stored at `n.<col>`. SQLite's `json_each` table-valued function
        // walks the array element-by-element so element-equality is exact
        // (no substring false-positives against the surrounding `[...]`).
        let mut where_extra = String::new();
        let mut bound: Vec<rusqlite::types::Value> = Vec::new();

        if !kinds.is_empty() {
            where_extra.push_str(" AND n.kind IN (");
            for (i, k) in kinds.iter().enumerate() {
                if i > 0 {
                    where_extra.push(',');
                }
                where_extra.push('?');
                bound.push(rusqlite::types::Value::Text(k.clone()));
            }
            where_extra.push(')');
        }

        if !symbols.is_empty() {
            where_extra
                .push_str(" AND EXISTS (SELECT 1 FROM json_each(n.symbols) WHERE value IN (");
            for (i, s) in symbols.iter().enumerate() {
                if i > 0 {
                    where_extra.push(',');
                }
                where_extra.push('?');
                bound.push(rusqlite::types::Value::Text(s.clone()));
            }
            where_extra.push_str("))");
        }

        if !files.is_empty() {
            where_extra.push_str(" AND EXISTS (SELECT 1 FROM json_each(n.files) WHERE value IN (");
            for (i, f) in files.iter().enumerate() {
                if i > 0 {
                    where_extra.push(',');
                }
                where_extra.push('?');
                bound.push(rusqlite::types::Value::Text(f.clone()));
            }
            where_extra.push_str("))");
        }

        let rows: Vec<NoteRow> = {
            let conn = self.conn.lock().await;
            if let Some(q) = query.filter(|s| !s.is_empty()) {
                let sql = format!(
                    "WITH ranked AS (
                        SELECT rowid, bm25(notes_fts) AS rank
                        FROM notes_fts
                        WHERE notes_fts MATCH ?
                    )
                    SELECT n.id, n.kind, n.content, n.symbols, n.files, n.session_id,
                           n.created_at, n.tool_name, n.retired_at, n.retired_by,
                           n.scope, n.feature_id, n.promoted_from, n.related_entity,
                           n.source, n.supersedes, n.payload_json
                    FROM notes n
                    JOIN ranked r ON r.rowid = n.rowid
                    WHERE 1=1 {retired_clause} {where_extra}
                    ORDER BY r.rank
                    LIMIT ?"
                );
                let mut params_owned: Vec<rusqlite::types::Value> = Vec::new();
                params_owned.push(rusqlite::types::Value::Text(q.to_string()));
                params_owned.extend(bound.into_iter());
                params_owned.push(rusqlite::types::Value::Integer(fetch_limit as i64));
                let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
                let mapped = stmt
                    .query_map(rusqlite::params_from_iter(params_owned), map_note_row)
                    .map_err(sqlite_err)?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row.map_err(sqlite_err)?);
                }
                out
            } else {
                let sql = format!(
                    "SELECT n.id, n.kind, n.content, n.symbols, n.files, n.session_id,
                            n.created_at, n.tool_name, n.retired_at, n.retired_by,
                            n.scope, n.feature_id, n.promoted_from, n.related_entity,
                            n.source, n.supersedes, n.payload_json
                     FROM notes n
                     WHERE 1=1 {retired_clause} {where_extra}
                     ORDER BY n.created_at DESC
                     LIMIT ?"
                );
                let mut params_owned: Vec<rusqlite::types::Value> = bound;
                params_owned.push(rusqlite::types::Value::Integer(fetch_limit as i64));
                let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
                let mapped = stmt
                    .query_map(rusqlite::params_from_iter(params_owned), map_note_row)
                    .map_err(sqlite_err)?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row.map_err(sqlite_err)?);
                }
                out
            }
        };

        let mut out: Vec<NoteRow> = rows
            .into_iter()
            .filter(|n| scope_matches(n, scope_filter))
            .collect();

        out.truncate(cap);
        Ok(out)
    }

    /// Semantic-blend variant of [`read_notes_scoped`].
    ///
    /// Returns the same NoteRow shape but ranks the candidate pool
    /// using a min-max normalised blend of:
    /// - BM25 rank (from FTS5, when `query` is set), and
    /// - cosine similarity between the query embedding and each
    ///   note's `note_embeddings` row.
    ///
    /// The blend weight is `embed_weight ∈ [0.0, 1.0]`, read from
    /// the `SOVEREIGN_NOTES_EMBED_WEIGHT` env var (default 0.5):
    /// - `0.0` → byte-identical to [`read_notes_scoped`] (cosine
    ///   disabled, this method short-circuits to the existing
    ///   path). The cluster-blend invariant from
    ///   `CLUSTER_SCORE_BLEND.md`.
    /// - `1.0` → cosine-only ranking, FTS5 rank ignored.
    /// - `0.5` (default) → equal mix.
    ///
    /// Soft-fail per `ARCH §9`: when `embed_fn` is unset OR
    /// `semantic_query` is `None` OR the embed call errors, we
    /// fall back to the existing FTS5-only path silently. The
    /// caller never has to know whether T1 is wired.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_notes_scoped_semantic(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
        scope_filter: &ScopeFilter,
        semantic_query: Option<&str>,
    ) -> Result<Vec<NoteRow>> {
        let weight = read_embed_weight_env();

        // Fast path: any of these conditions short-circuits to the
        // FTS5-only baseline. Weight=0 is the canonical
        // byte-identical-to-baseline invariant — even a microsecond
        // of cosine compute is forbidden here, because tests
        // assert byte equivalence.
        if weight == 0.0 || semantic_query.is_none() || self.embed_fn.get().is_none() {
            return self
                .read_notes_scoped(
                    query,
                    symbols,
                    files,
                    kinds,
                    limit,
                    include_retired,
                    scope_filter,
                )
                .await;
        }
        let sem_q = semantic_query.unwrap();
        let embed_fn = self.embed_fn.get().cloned().expect("checked above");

        // Compute query embedding outside the lock; soft-fail on
        // error (drop to baseline).
        let query_vec = match embed_fn(sem_q).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    target = "notes",
                    error = %e,
                    "notes: semantic query embed failed; falling back to FTS5-only"
                );
                return self
                    .read_notes_scoped(
                        query,
                        symbols,
                        files,
                        kinds,
                        limit,
                        include_retired,
                        scope_filter,
                    )
                    .await;
            }
        };

        // Over-fetch candidate pool: 10x the requested limit, capped
        // at 200, capped further by the SQLite per-statement bound.
        // The candidate union must be wide enough that cosine-only
        // winners aren't pre-truncated by BM25 (and vice versa).
        let cap = limit.min(100);
        let pool_size = (limit.saturating_mul(10)).clamp(100, 500);

        // Pull the BM25 candidate pool (when query set) and the
        // cosine candidate pool from `note_embeddings` separately,
        // then union by note_id. Both pools respect the
        // scope/kind/symbol/file/retired filters.
        let (bm25_pool, cosine_pool) = {
            let conn = self.conn.lock().await;
            let bm25 = if let Some(q) = query.filter(|s| !s.is_empty()) {
                fetch_bm25_pool(&conn, q, symbols, files, kinds, pool_size, include_retired)?
            } else {
                Vec::new()
            };
            let cosine = fetch_cosine_pool(
                &conn,
                symbols,
                files,
                kinds,
                pool_size,
                include_retired,
                &query_vec,
            )?;
            (bm25, cosine)
        };

        // Union by note id; remember which scores were observed in
        // each pool. Missing-in-pool → score stays None and that
        // dimension contributes 0 to the blend after normalisation.
        use std::collections::HashMap;
        let mut blended: HashMap<String, (NoteRow, Option<f64>, Option<f64>)> = HashMap::new();
        for (row, rank) in bm25_pool {
            blended
                .entry(row.id.clone())
                .or_insert((row, Some(rank), None))
                .1 = Some(rank);
        }
        for (row, cos) in cosine_pool {
            blended
                .entry(row.id.clone())
                .and_modify(|slot| slot.2 = Some(cos))
                .or_insert((row, None, Some(cos)));
        }

        // Min-max normalise per-dimension. BM25 is "lower is better"
        // — invert by negating before normalising. Cosine is
        // "higher is better" — use directly. Both end up on [0.0,
        // 1.0] where 1.0 is "best in this pool".
        let bm25_vals: Vec<f64> = blended
            .values()
            .filter_map(|(_, r, _)| r.map(|x| -x))
            .collect();
        let cosine_vals: Vec<f64> = blended.values().filter_map(|(_, _, c)| *c).collect();
        let bm25_minmax = MinMax::from_slice(&bm25_vals);
        let cosine_minmax = MinMax::from_slice(&cosine_vals);

        let mut scored: Vec<(NoteRow, f64)> = blended
            .into_iter()
            .map(|(_, (row, bm, cos))| {
                let bm_norm = bm.map(|x| bm25_minmax.normalise(-x)).unwrap_or(0.0);
                let cos_norm = cos.map(|x| cosine_minmax.normalise(x)).unwrap_or(0.0);
                let blended = (1.0 - weight as f64) * bm_norm + (weight as f64) * cos_norm;
                (row, blended)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut out: Vec<NoteRow> = scored
            .into_iter()
            .map(|(row, _)| row)
            .filter(|n| scope_matches(n, scope_filter))
            .collect();
        out.truncate(cap);

        tracing::debug!(
            target = "notes",
            blend_weight = weight,
            pool_total = out.len(),
            "notes: semantic blend applied"
        );
        Ok(out)
    }

    /// Return reflection notes for the developer-facing `sovereign reflect` command.
    ///
    /// - `since` — unix timestamp lower bound (0 = all time)
    /// - `tool_filter` — restrict to notes with this `tool_name`
    /// - `include_retired` — include retired reflections (for `--history`)
    pub async fn read_reflections(
        &self,
        since: i64,
        tool_filter: Option<&str>,
        include_retired: bool,
    ) -> Result<Vec<NoteRow>> {
        let retired_clause = if include_retired {
            ""
        } else {
            "AND retired_at IS NULL"
        };
        let tool_clause = if tool_filter.is_some() {
            "AND tool_name = ?"
        } else {
            ""
        };

        let sql = format!(
            "SELECT id, kind, content, symbols, files, session_id,
                    created_at, tool_name, retired_at, retired_by,
                    scope, feature_id, promoted_from, related_entity,
                    source, supersedes, payload_json
             FROM notes
             WHERE kind = 'reflection'
               AND created_at >= ?
               {retired_clause}
               {tool_clause}
             ORDER BY created_at DESC
             LIMIT 1000"
        );

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;

        let mapped = if let Some(tool) = tool_filter {
            stmt.query_map(params![since, tool], map_note_row)
                .map_err(sqlite_err)?
        } else {
            stmt.query_map(params![since], map_note_row)
                .map_err(sqlite_err)?
        };

        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    /// Return all active notes whose `related_entity` matches the given
    /// canonical name (case-sensitive match against the column value).
    /// Filters out retired notes and orders by `created_at DESC`. Used by
    /// the relational + strategic digests at splice time to find
    /// commitment / follow_up / goal notes anchored to an entity.
    ///
    /// `kinds`, when non-empty, restricts to a subset of note kinds —
    /// the digest passes `["commitment", "follow_up"]` for the
    /// relational block and `["goal"]` for the strategic block.
    pub async fn read_notes_by_related_entity(
        &self,
        related_entity: &str,
        kinds: &[&str],
    ) -> Result<Vec<NoteRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, content, symbols, files, session_id,
                        created_at, tool_name, retired_at, retired_by,
                        scope, feature_id, promoted_from, related_entity,
                        source, supersedes, payload_json
                 FROM notes
                 WHERE related_entity = ?1
                   AND retired_at IS NULL
                 ORDER BY created_at DESC",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![related_entity], map_note_row)
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            let row = row.map_err(sqlite_err)?;
            if !kinds.is_empty() && !kinds.iter().any(|k| *k == row.kind) {
                continue;
            }
            out.push(row);
        }
        Ok(out)
    }

    // ── Note deletion / retirement ─────────────────────────────────────────

    /// Delete a note by ID. Returns `true` if a row was removed, `false` if
    /// the ID was not found.
    pub async fn delete_note(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute("DELETE FROM notes WHERE id = ?", params![id])
            .map_err(sqlite_err)?;
        if affected > 0 {
            bump_notes_version(&conn)?;
        }
        Ok(affected > 0)
    }

    /// Mark all active reflections for `tool_name` as retired.
    ///
    /// Returns the IDs of the notes that were retired. Returns an empty vec
    /// if no matching active reflections exist.
    pub async fn retire_by_tool(&self, tool_name: &str, reason: &str) -> Result<Vec<String>> {
        let now = unix_now();
        let conn = self.conn.lock().await;

        // Collect IDs first so we can return them.
        let mut stmt = conn
            .prepare(
                "SELECT id FROM notes WHERE tool_name = ? AND kind = 'reflection' AND retired_at IS NULL",
            )
            .map_err(sqlite_err)?;
        let ids: Vec<String> = stmt
            .query_map(params![tool_name], |r| r.get(0))
            .map_err(sqlite_err)?
            .filter_map(|r| r.ok())
            .collect();

        if ids.is_empty() {
            return Ok(ids);
        }

        conn.execute(
            "UPDATE notes SET retired_at = ?1, retired_by = ?2
             WHERE tool_name = ?3 AND kind = 'reflection' AND retired_at IS NULL",
            params![now, reason, tool_name],
        )
        .map_err(sqlite_err)?;
        bump_notes_version(&conn)?;

        Ok(ids)
    }

    /// Mark a single reflection as retired by its ID.
    ///
    /// Returns `true` if the note existed and was not already retired,
    /// `false` otherwise.
    pub async fn retire_by_id(&self, id: &str, reason: &str) -> Result<bool> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE notes SET retired_at = ?1, retired_by = ?2
                 WHERE id = ?3 AND retired_at IS NULL",
                params![now, reason, id],
            )
            .map_err(sqlite_err)?;
        if affected > 0 {
            bump_notes_version(&conn)?;
        }
        Ok(affected > 0)
    }

    // ── Todo summary ───────────────────────────────────────────────────────

    /// Return the most recent open `todo` notes (for the startup summary).
    pub async fn open_todos(&self, limit: usize) -> Result<Vec<NoteRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, content, symbols, files, session_id,
                        created_at, tool_name, retired_at, retired_by,
                        scope, feature_id, promoted_from, related_entity,
                        source, supersedes, payload_json
                 FROM notes
                 WHERE kind = 'todo' AND retired_at IS NULL
                 ORDER BY created_at DESC
                 LIMIT ?",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![limit as i64], map_note_row)
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    // ── Tool call ring buffer ──────────────────────────────────────────────

    /// Record a single MCP tool invocation. Fire-and-forget: errors are
    /// silently ignored by callers so a logging failure never kills a tool call.
    ///
    /// Automatically purges rows beyond the 10,000-row ring buffer limit.
    pub async fn log_tool_call(
        &self,
        session_id: &str,
        tool_name: &str,
        outcome: &str,
    ) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let conn = self.conn.lock().await;

        conn.execute(
            "INSERT INTO tool_call_log (id, session_id, tool_name, outcome, called_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, session_id, tool_name, outcome, now],
        )
        .map_err(sqlite_err)?;

        // Trim to ring buffer limit.
        conn.execute(
            "DELETE FROM tool_call_log WHERE id IN (
                SELECT id FROM tool_call_log ORDER BY called_at DESC LIMIT -1 OFFSET 10000
             )",
            [],
        )
        .map_err(sqlite_err)?;

        Ok(())
    }

    /// Return recent tool call log entries for the developer-facing `sovereign reflect --log`.
    pub async fn tool_call_log_rows(
        &self,
        since: i64,
        limit: usize,
    ) -> Result<Vec<ToolCallLogRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, tool_name, outcome, called_at
                 FROM tool_call_log
                 WHERE called_at >= ?
                 ORDER BY called_at DESC, rowid DESC
                 LIMIT ?",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![since, limit as i64], |r| {
                Ok(ToolCallLogRow {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    tool_name: r.get(2)?,
                    outcome: r.get(3)?,
                    called_at: r.get(4)?,
                })
            })
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }
}

// ─── Schema (new databases) ───────────────────────────────────────────────────

/// Full schema for brand-new databases. Sets `user_version = 1`; the
/// open path then steps the DB through migrations v1→v2→…→v5 so a
/// fresh install lands in the same final shape as an upgraded
/// install, with no schema drift between paths. Adding a new kind
/// or column means writing one new migration constant — never
/// editing this schema twice.
// ─── Helpers ──────────────────────────────────────────────────────────────────

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Io(std::io::Error::other(format!("NoteStore sqlite: {e}")))
}

/// Returns `true` when the note matches the caller's scope predicate.
///
/// Default `ScopeFilter` (no scopes, no feature_id) always matches — the
/// True when the scope filter has no predicate to apply (empty scopes AND
/// no feature_id). Used by `read_notes_scoped` to decide whether the SQL
/// LIMIT can be tight (`cap`) or whether the post-fetch scope filter still
/// needs over-fetch headroom (`cap * 10`).
fn scope_filter_is_no_op(filter: &ScopeFilter) -> bool {
    filter.scopes.is_empty() && filter.feature_id.is_none()
}

/// legacy [`NoteStore::read_notes`] wrapper uses this to preserve behavior.
fn scope_matches(note: &NoteRow, filter: &ScopeFilter) -> bool {
    if filter.scopes.is_empty() && filter.feature_id.is_none() {
        return true;
    }

    if !filter.scopes.is_empty() {
        let ok = filter.scopes.iter().any(|s| s.as_str() == note.scope);
        if !ok {
            return false;
        }
    }

    if let Some(fid) = &filter.feature_id {
        // Feature_id predicate only applies to feature-scoped rows. Global /
        // session rows pass through regardless so a `scopes = [global,
        // feature]` + `feature_id = X` query returns globals + one feature.
        if note.scope == "feature" && note.feature_id.as_deref() != Some(fid.as_str()) {
            return false;
        }
    }

    true
}

/// Monotonic counter bumped on every note mutation. Callers must hold the
/// NoteStore lock so the bump is effectively atomic with the mutation.
fn bump_notes_version(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE meta_counters SET val = val + 1 WHERE key = 'notes_version'",
        [],
    )
    .map_err(sqlite_err)?;
    Ok(())
}

fn map_note_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRow> {
    let symbols_json: String = row.get(3)?;
    let files_json: String = row.get(4)?;
    let created_at_secs: i64 = row.get(6)?;

    let symbols: Vec<String> = serde_json::from_str(&symbols_json).unwrap_or_default();
    let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();

    // Convert unix timestamp to RFC 3339.
    let created_at = chrono::DateTime::<chrono::Utc>::from_timestamp(created_at_secs, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| created_at_secs.to_string());

    Ok(NoteRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        content: row.get(2)?,
        symbols,
        files,
        session_id: row.get(5)?,
        created_at,
        tool_name: row.get(7)?,
        retired_at: row.get(8)?,
        retired_by: row.get(9)?,
        scope: row.get(10)?,
        feature_id: row.get(11)?,
        promoted_from: row.get(12)?,
        related_entity: row.get(13)?,
        source: row.get(14)?,
        supersedes: row.get(15)?,
        payload_json: row.get(16)?,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> NoteStore {
        let dir = tempfile::tempdir().unwrap();
        NoteStore::open(&dir.path().join("notes.db")).unwrap()
    }

    // ── Existing note tests (must continue to pass) ──────────────────────

    #[tokio::test]
    async fn write_note_roundtrip() {
        let store = make_store().await;
        let id = store
            .write_note(
                "decision",
                "Use BFS for blast radius",
                vec!["blast_radius".into()],
                vec!["src/lib.rs".into()],
                "test-session",
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].kind, "decision");
        assert_eq!(notes[0].content, "Use BFS for blast radius");
        assert_eq!(notes[0].symbols, vec!["blast_radius"]);
        assert!(notes[0].tool_name.is_none());
        assert!(notes[0].retired_at.is_none());
        // Pre-v5 path: writes that don't go through
        // `write_note_with_relation` leave related_entity NULL.
        assert!(notes[0].related_entity.is_none());
    }

    #[tokio::test]
    async fn write_note_v5_kinds_round_trip() {
        // Each of the three new kinds must be admitted by the
        // CHECK constraint and round-trip through write → read.
        let store = make_store().await;
        for kind in ["commitment", "follow_up", "goal"] {
            let id = store
                .write_note(kind, &format!("test {kind}"), vec![], vec![], "s1")
                .await
                .unwrap_or_else(|e| panic!("kind {kind} should be accepted: {e}"));
            assert!(!id.is_empty());
        }
        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        let kinds: std::collections::HashSet<_> = notes.iter().map(|n| n.kind.as_str()).collect();
        assert!(kinds.contains("commitment"));
        assert!(kinds.contains("follow_up"));
        assert!(kinds.contains("goal"));
    }

    #[tokio::test]
    async fn write_note_with_relation_persists_related_entity() {
        let store = make_store().await;
        let id = store
            .write_note_with_relation(
                "commitment",
                "send revised pricing to Sarah by Friday",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
                Some("Sarah Chen"),
            )
            .await
            .unwrap();
        let row = store.read_note_by_id(&id).await.unwrap().unwrap();
        assert_eq!(row.kind, "commitment");
        assert_eq!(row.related_entity.as_deref(), Some("Sarah Chen"));
    }

    #[tokio::test]
    async fn related_entity_filters_correctly_via_read_notes() {
        // The sql index `idx_notes_related_entity` is a partial
        // index — we don't query it here directly but we exercise
        // the post-filter path that production digests use.
        let store = make_store().await;
        store
            .write_note_with_relation(
                "commitment",
                "alpha",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
                Some("Sarah Chen"),
            )
            .await
            .unwrap();
        store
            .write_note_with_relation(
                "follow_up",
                "beta",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
                Some("Mike Torres"),
            )
            .await
            .unwrap();
        store
            .write_note_with_relation(
                "goal",
                "gamma",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
                None,
            )
            .await
            .unwrap();

        let all = store
            .read_notes(None, &[], &[], &[], 100, false)
            .await
            .unwrap();
        let with_entity: Vec<_> = all
            .iter()
            .filter(|n| n.related_entity.as_deref() == Some("Sarah Chen"))
            .collect();
        assert_eq!(with_entity.len(), 1);
        assert_eq!(with_entity[0].content, "alpha");
    }

    #[tokio::test]
    async fn read_notes_by_related_entity_returns_only_matching_active_notes() {
        // Three notes: two for Sarah (one commitment, one retired
        // commitment), one for Mike. The query must surface only the
        // active Sarah note — retired and unrelated rows are excluded.
        let store = make_store().await;
        let sarah_active = store
            .write_note_with_relation(
                "commitment",
                "send pricing",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
                Some("Sarah Chen"),
            )
            .await
            .unwrap();
        let sarah_retired = store
            .write_note_with_relation(
                "commitment",
                "old commitment",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
                Some("Sarah Chen"),
            )
            .await
            .unwrap();
        store
            .write_note_with_relation(
                "follow_up",
                "ping mike",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
                Some("Mike Torres"),
            )
            .await
            .unwrap();
        store.retire_by_id(&sarah_retired, "test").await.unwrap();

        let all_for_sarah = store
            .read_notes_by_related_entity("Sarah Chen", &[])
            .await
            .unwrap();
        assert_eq!(all_for_sarah.len(), 1);
        assert_eq!(all_for_sarah[0].id, sarah_active);

        // Kind filter narrows the result.
        let goals_for_sarah = store
            .read_notes_by_related_entity("Sarah Chen", &["goal"])
            .await
            .unwrap();
        assert!(goals_for_sarah.is_empty());

        let commitments_for_sarah = store
            .read_notes_by_related_entity("Sarah Chen", &["commitment"])
            .await
            .unwrap();
        assert_eq!(commitments_for_sarah.len(), 1);

        // Unknown entity → empty.
        let none = store
            .read_notes_by_related_entity("Nobody", &[])
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn write_note_rejects_unknown_kind_at_check_constraint() {
        let store = make_store().await;
        // The kind 'totally_invalid' is not in the CHECK list — the
        // SQLite layer must reject it. We don't validate kind in
        // the Rust API on purpose so new kinds can land in one PR
        // (schema migration) without source-side ceremony; the
        // CHECK constraint is the structural backstop.
        let r = store
            .write_note("totally_invalid", "x", vec![], vec![], "s1")
            .await;
        assert!(r.is_err(), "CHECK must reject unknown kind");
    }

    #[tokio::test]
    async fn read_notes_fts_search() {
        let store = make_store().await;
        store
            .write_note(
                "decision",
                "Use BFS for blast radius traversal",
                vec![],
                vec![],
                "s1",
            )
            .await
            .unwrap();
        store
            .write_note("todo", "Unrelated note about caching", vec![], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(Some("blast radius"), &[], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("blast radius"));
    }

    #[tokio::test]
    async fn read_notes_symbol_filter() {
        let store = make_store().await;
        store
            .write_note("attempt", "tried foo", vec!["foo_fn".into()], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("attempt", "tried bar", vec!["bar_fn".into()], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(None, &["foo_fn".to_string()], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("foo"));
    }

    /// Regression for the post-fetch LIMIT bug — pre-fix, `symbols` /
    /// `files` / `kinds` were applied AFTER `ORDER BY created_at DESC
    /// LIMIT cap`, so a matching note pushed out of the recency window
    /// by newer writes silently disappeared. Push the matching note to
    /// the bottom by writing 20 newer unrelated notes; then ask for it
    /// by exact symbol with the default limit of 10. Pre-fix returned
    /// 0; post-fix returns 1.
    #[tokio::test]
    async fn symbol_filter_survives_recency_window_displacement() {
        let store = make_store().await;
        let needle_id = store
            .write_note(
                "invariant",
                "needle — must survive recency window",
                vec!["NeedleSymbol".into()],
                vec![],
                "s1",
            )
            .await
            .unwrap();
        for i in 0..20 {
            store
                .write_note(
                    "todo",
                    &format!("haystack note {i}"),
                    vec![format!("unrelated_{i}")],
                    vec![],
                    "s1",
                )
                .await
                .unwrap();
        }

        let results = store
            .read_notes(None, &["NeedleSymbol".to_string()], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(
            results.len(),
            1,
            "exact-symbol filter must find the note even when recency-displaced"
        );
        assert_eq!(results[0].id, needle_id);
    }

    /// Same shape for files filter — pinned because we also hit this
    /// in the 2026-05-25 audit (`--files=[…]` returned 0 against the
    /// live DB even though the path was stored verbatim).
    #[tokio::test]
    async fn file_filter_survives_recency_window_displacement() {
        let store = make_store().await;
        let needle_id = store
            .write_note(
                "decision",
                "shipped X — see file",
                vec![],
                vec!["crates/foo/src/needle.rs".into()],
                "s1",
            )
            .await
            .unwrap();
        for i in 0..20 {
            store
                .write_note(
                    "todo",
                    &format!("displacer {i}"),
                    vec![],
                    vec![format!("crates/other/src/file_{i}.rs")],
                    "s1",
                )
                .await
                .unwrap();
        }

        let results = store
            .read_notes(
                None,
                &[],
                &["crates/foo/src/needle.rs".to_string()],
                &[],
                10,
                false,
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, needle_id);
    }

    /// Same shape for kinds filter.
    #[tokio::test]
    async fn kind_filter_survives_recency_window_displacement() {
        let store = make_store().await;
        let needle_id = store
            .write_note("invariant", "needle invariant", vec![], vec![], "s1")
            .await
            .unwrap();
        for i in 0..20 {
            store
                .write_note("todo", &format!("displacer {i}"), vec![], vec![], "s1")
                .await
                .unwrap();
        }

        let results = store
            .read_notes(None, &[], &[], &["invariant".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, needle_id);
    }

    /// Filter combinators: symbols AND kinds, both required. SQL `IN`
    /// lists give OR semantics within a slot; multiple slots compose
    /// with AND.
    #[tokio::test]
    async fn combined_filters_intersect() {
        let store = make_store().await;
        let target = store
            .write_note(
                "decision",
                "right kind + right symbol",
                vec!["TargetSymbol".into()],
                vec![],
                "s1",
            )
            .await
            .unwrap();
        // Same symbol, wrong kind:
        store
            .write_note(
                "todo",
                "right symbol, wrong kind",
                vec!["TargetSymbol".into()],
                vec![],
                "s1",
            )
            .await
            .unwrap();
        // Right kind, wrong symbol:
        store
            .write_note(
                "decision",
                "right kind, wrong symbol",
                vec!["OtherSymbol".into()],
                vec![],
                "s1",
            )
            .await
            .unwrap();

        let results = store
            .read_notes(
                None,
                &["TargetSymbol".to_string()],
                &[],
                &["decision".to_string()],
                10,
                false,
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, target);
    }

    #[tokio::test]
    async fn read_notes_kind_filter() {
        let store = make_store().await;
        store
            .write_note("decision", "keep this", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("todo", "do this later", vec![], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(None, &[], &[], &["todo".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "todo");
    }

    #[tokio::test]
    async fn delete_note_removes() {
        let store = make_store().await;
        let id = store
            .write_note("invariant", "never call this twice", vec![], vec![], "s1")
            .await
            .unwrap();

        let deleted = store.delete_note(&id).await.unwrap();
        assert!(deleted);

        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        assert!(notes.is_empty());

        // Deleting again returns false.
        let deleted_again = store.delete_note(&id).await.unwrap();
        assert!(!deleted_again);
    }

    #[tokio::test]
    async fn open_todos_returns_only_todos() {
        let store = make_store().await;
        store
            .write_note("todo", "fix the thing", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("decision", "keep it", vec![], vec![], "s1")
            .await
            .unwrap();

        let todos = store.open_todos(10).await.unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "fix the thing");
    }

    // ── Reflection tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn reflection_stored_with_kind() {
        let store = make_store().await;
        let id = store
            .write_reflection(
                r#"{"task_summary":"Refactored EmbedFn"}"#,
                Some("blast_radius"),
                "s1",
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        let results = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "reflection");
        assert_eq!(results[0].tool_name.as_deref(), Some("blast_radius"));
    }

    #[tokio::test]
    async fn reflection_active_by_default() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"test"}"#, None, "s1")
            .await
            .unwrap();

        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        let note = notes.iter().find(|n| n.id == id).unwrap();
        assert!(note.retired_at.is_none());
        assert!(note.retired_by.is_none());
    }

    #[tokio::test]
    async fn retired_reflection_hidden_by_default() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"test"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();

        let retired = store.retire_by_id(&id, "fixed in PR #1").await.unwrap();
        assert!(retired);

        // Default read (include_retired=false) should not return it.
        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        assert!(notes.is_empty());

        // But read_notes with include_retired=true should.
        let all = store
            .read_notes(None, &[], &[], &[], 10, true)
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].retired_by.as_deref(), Some("fixed in PR #1"));
        assert!(all[0].retired_at.is_some());
    }

    #[tokio::test]
    async fn retired_reflection_visible_in_history() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"test"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        store.retire_by_id(&id, "v0.4.2").await.unwrap();

        let reflections = store.read_reflections(0, None, true).await.unwrap();
        assert_eq!(reflections.len(), 1);
        assert_eq!(reflections[0].retired_by.as_deref(), Some("v0.4.2"));
    }

    #[tokio::test]
    async fn retire_by_tool_matches_all() {
        let store = make_store().await;
        for _ in 0..3 {
            store
                .write_reflection(
                    r#"{"task_summary":"blast radius miss"}"#,
                    Some("blast_radius"),
                    "s1",
                )
                .await
                .unwrap();
        }
        // Unrelated reflection — should not be retired.
        store
            .write_reflection(
                r#"{"task_summary":"project context miss"}"#,
                Some("project_context"),
                "s1",
            )
            .await
            .unwrap();

        let retired_ids = store
            .retire_by_tool("blast_radius", "macro support added")
            .await
            .unwrap();
        assert_eq!(retired_ids.len(), 3);

        // blast_radius reflections gone from default read.
        let active = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].tool_name.as_deref(), Some("project_context"));
    }

    #[tokio::test]
    async fn retire_by_id_leaves_others_active() {
        let store = make_store().await;
        let id1 = store
            .write_reflection(r#"{"task_summary":"a"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        let _id2 = store
            .write_reflection(r#"{"task_summary":"b"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();

        let retired = store.retire_by_id(&id1, "fixed").await.unwrap();
        assert!(retired);

        let active = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, _id2);
    }

    #[tokio::test]
    async fn retire_by_id_already_retired_returns_false() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"a"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        store.retire_by_id(&id, "first").await.unwrap();
        let second = store.retire_by_id(&id, "second").await.unwrap();
        assert!(!second);
    }

    // ── tool_call_log tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn tool_call_log_records_outcome() {
        let store = make_store().await;
        store
            .log_tool_call("sess-1", "lint_status", "success")
            .await
            .unwrap();
        store
            .log_tool_call("sess-1", "blast_radius", "error")
            .await
            .unwrap();

        let rows = store.tool_call_log_rows(0, 100).await.unwrap();
        assert_eq!(rows.len(), 2);
        // Most recent first.
        assert_eq!(rows[0].tool_name, "blast_radius");
        assert_eq!(rows[0].outcome, "error");
        assert_eq!(rows[1].tool_name, "lint_status");
        assert_eq!(rows[1].outcome, "success");
    }

    #[tokio::test]
    async fn tool_call_log_ring_buffer() {
        let store = make_store().await;
        for i in 0..10_001usize {
            store
                .log_tool_call("sess", "lint_status", "success")
                .await
                .unwrap();
            let _ = i; // suppress unused warning
        }

        let rows = store.tool_call_log_rows(0, 20_000).await.unwrap();
        assert_eq!(rows.len(), 10_000);
    }

    // ── Migration test ────────────────────────────────────────────────────

    #[tokio::test]
    async fn migration_v0_to_v1_preserves_data_and_enables_reflections() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Simulate an old-schema database (no tool_name/retired_at/retired_by,
        // restricted CHECK constraint, no tool_call_log).
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE notes (
                     id TEXT PRIMARY KEY,
                     kind TEXT NOT NULL CHECK(kind IN ('decision','attempt','invariant','todo')),
                     content TEXT NOT NULL,
                     symbols TEXT NOT NULL DEFAULT '[]',
                     files TEXT NOT NULL DEFAULT '[]',
                     session_id TEXT NOT NULL,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE VIRTUAL TABLE notes_fts USING fts5(content, kind, content='notes', content_rowid='rowid');
                 INSERT INTO notes VALUES ('id-1','todo','old note','[]','[]','s0',1000,1000);",
            )
            .unwrap();
            // user_version stays 0 (default).
        }

        // Open with new NoteStore — migration should run.
        let store = NoteStore::open(&db_path).unwrap();

        // Old note is preserved.
        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].content, "old note");

        // Reflection kind is now accepted.
        let id = store
            .write_reflection(
                r#"{"task_summary":"post-migration"}"#,
                Some("blast_radius"),
                "s1",
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        // tool_call_log is available.
        store
            .log_tool_call("sess", "lint_status", "success")
            .await
            .unwrap();
        let rows = store.tool_call_log_rows(0, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    // ── ATOS scope tests (M1.1) ──────────────────────────────────────────

    #[tokio::test]
    async fn scoped_note_persists_scope_and_feature_id() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "decision",
                "prefer UNION over sequential queries",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("atos-version-flag"),
            )
            .await
            .unwrap();

        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        let note = notes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(note.scope, "feature");
        assert_eq!(note.feature_id.as_deref(), Some("atos-version-flag"));
        assert!(note.promoted_from.is_none());
    }

    #[tokio::test]
    async fn feature_scope_requires_feature_id() {
        let store = make_store().await;
        let result = store
            .write_note_scoped(
                "decision",
                "bad",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                None,
            )
            .await;
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[tokio::test]
    async fn legacy_write_note_defaults_to_global_scope() {
        let store = make_store().await;
        let id = store
            .write_note("invariant", "never panic", vec![], vec![], "s1")
            .await
            .unwrap();
        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        let note = notes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(note.scope, "global");
        assert!(note.feature_id.is_none());
    }

    #[tokio::test]
    async fn scope_filter_selects_feature_and_global() {
        let store = make_store().await;

        // A global invariant visible to every feature.
        store
            .write_note_scoped(
                "invariant",
                "global rule",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
            )
            .await
            .unwrap();
        // Two features' worth of feature-scoped notes.
        store
            .write_note_scoped(
                "decision",
                "feat-A decision",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("feat-a"),
            )
            .await
            .unwrap();
        store
            .write_note_scoped(
                "decision",
                "feat-B decision",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("feat-b"),
            )
            .await
            .unwrap();

        let filter = ScopeFilter {
            scopes: vec![NoteScope::Global, NoteScope::Feature],
            feature_id: Some("feat-a".into()),
        };
        let notes = store
            .read_notes_scoped(None, &[], &[], &[], 10, false, &filter)
            .await
            .unwrap();

        let contents: Vec<_> = notes.iter().map(|n| n.content.as_str()).collect();
        assert!(contents.contains(&"global rule"));
        assert!(contents.contains(&"feat-A decision"));
        assert!(!contents.contains(&"feat-B decision"));
        assert_eq!(notes.len(), 2);
    }

    #[tokio::test]
    async fn notes_version_counter_increments_on_writes() {
        let store = make_store().await;
        let v0 = store.notes_version().await.unwrap();

        store
            .write_note("decision", "a", vec![], vec![], "s1")
            .await
            .unwrap();
        let v1 = store.notes_version().await.unwrap();
        assert_eq!(v1, v0 + 1);

        let id = store
            .write_note_scoped(
                "decision",
                "b",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
            )
            .await
            .unwrap();
        let v2 = store.notes_version().await.unwrap();
        assert_eq!(v2, v1 + 1);

        store.delete_note(&id).await.unwrap();
        let v3 = store.notes_version().await.unwrap();
        assert_eq!(v3, v2 + 1);

        // No-op delete should not bump.
        store.delete_note("nonexistent").await.unwrap();
        let v4 = store.notes_version().await.unwrap();
        assert_eq!(v4, v3);
    }

    #[tokio::test]
    async fn promote_note_creates_new_row_and_tags_origin() {
        let store = make_store().await;
        let src = store
            .write_note_scoped(
                "decision",
                "feature-local decision",
                vec!["Foo".into()],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("feat-a"),
            )
            .await
            .unwrap();

        let promoted_id = store
            .promote_note(
                &src,
                NoteScope::Global,
                None,
                Some("rewritten as global rule"),
            )
            .await
            .unwrap();

        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        let promoted = notes.iter().find(|n| n.id == promoted_id).unwrap();
        assert_eq!(promoted.scope, "global");
        assert_eq!(promoted.content, "rewritten as global rule");
        assert_eq!(promoted.promoted_from.as_deref(), Some(src.as_str()));
        // Source row still exists at feature scope.
        let source = notes.iter().find(|n| n.id == src).unwrap();
        assert_eq!(source.scope, "feature");
    }

    // ── v3 kinds round-trip ──────────────────────────────────────────────

    #[tokio::test]
    async fn write_uncertainty_kind_round_trip() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "uncertainty",
                "Deep collection nesting in Zotero exports — flatten to nearest ancestor.",
                vec![],
                vec!["acquirers/zotero.rs".into()],
                "s1",
                NoteScope::Feature,
                Some("zotero-acquirer"),
            )
            .await
            .unwrap();
        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        let n = notes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(n.kind, "uncertainty");
        assert_eq!(n.feature_id.as_deref(), Some("zotero-acquirer"));
    }

    #[tokio::test]
    async fn write_postmortem_pointer_kind_round_trip() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "postmortem_pointer",
                "zotero_rdf.rs::parse_item — RDF boundary detection is the most complex path.",
                vec!["parse_item".into()],
                vec!["extractors/zotero_rdf.rs".into()],
                "s1",
                NoteScope::Feature,
                Some("zotero-acquirer"),
            )
            .await
            .unwrap();
        let notes = store
            .read_notes(
                None,
                &[],
                &[],
                &["postmortem_pointer".to_string()],
                10,
                false,
            )
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, id);
    }

    #[tokio::test]
    async fn write_redteam_finding_kind_round_trip() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "redteam_finding",
                "ZoteroLibrary factory does not explicitly set scope=Local.",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("zotero-acquirer"),
            )
            .await
            .unwrap();
        let notes = store
            .read_notes_scoped(
                None,
                &[],
                &[],
                &["redteam_finding".to_string()],
                10,
                false,
                &ScopeFilter {
                    scopes: vec![NoteScope::Feature],
                    feature_id: Some("zotero-acquirer".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, id);
    }

    #[tokio::test]
    async fn write_deviation_kind_round_trip() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "deviation",
                "Spec content hash changed since approval (a3f1 → 8b7c).",
                vec![],
                vec![".sovereign/features/fx/spec.md".into()],
                "atos-middleware",
                NoteScope::Feature,
                Some("fx"),
            )
            .await
            .unwrap();
        let notes = store
            .read_notes(None, &[], &[], &["deviation".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, id);
        assert!(notes[0].content.contains("Spec content hash"));
    }

    #[tokio::test]
    async fn migration_v3_to_v4_preserves_data_and_enables_deviation() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v3 database by running SCHEMA_NEW + V2 + V3 and
        // then seeding a row. We can't simulate a "v3 without V4's
        // kind" from a clean DB because SCHEMA_NEW already has the
        // expanded kind list after M4.2's edit — so we seed the
        // note BEFORE V4 runs by opening and closing the store once
        // at v3, then running V4 and checking behavior.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(SCHEMA_NEW).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.execute_batch(MIGRATION_V3).unwrap();
            conn.execute(
                "INSERT INTO notes (id, kind, content, symbols, files, session_id,
                    created_at, updated_at, scope)
                 VALUES ('pre-v4','decision','survived','[]','[]','s0',1000,1000,'global')",
                [],
            )
            .unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 3, "baseline should be v3");
        }

        // Reopen — MIGRATION_V4 runs.
        let store = NoteStore::open(&db_path).unwrap();

        let old = store.read_note_by_id("pre-v4").await.unwrap().unwrap();
        assert_eq!(old.content, "survived");

        let new_id = store
            .write_note_scoped(
                "deviation",
                "post-migration",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
            )
            .await
            .unwrap();
        assert!(!new_id.is_empty());

        // FTS5 still works after the rebuild.
        let hits = store
            .read_notes(Some("survived"), &[], &[], &[], 5, false)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn unknown_kind_still_rejected() {
        let store = make_store().await;
        let err = store
            .write_note("not_a_kind", "hi", vec![], vec![], "s1")
            .await
            .unwrap_err();
        // CHECK constraint violation surfaces via rusqlite → Error::Io.
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("check") || msg.to_lowercase().contains("constraint"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn migration_v2_to_v3_preserves_data_and_enables_new_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v2 database manually — run SCHEMA_NEW (v1) + MIGRATION_V2 (v2),
        // then stop short of MIGRATION_V3. Insert a pre-v3 row so we can
        // prove it survives the rebuild.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(SCHEMA_NEW).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            // Drop the post-migration CHECK back down to the v2 set so
            // we're actually simulating a v2 DB (SCHEMA_NEW already has
            // the expanded list after this file's M3.2 edit, but a
            // real-world v2 DB on disk won't).
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 2, "baseline should be v2");
            conn.execute(
                "INSERT INTO notes (id, kind, content, symbols, files, session_id,
                    created_at, updated_at, scope)
                 VALUES ('pre-v3','decision','survives migration','[]','[]','s0',1000,1000,'global')",
                [],
            )
            .unwrap();
        }

        // Reopen — MIGRATION_V3 runs.
        let store = NoteStore::open(&db_path).unwrap();

        // Old row preserved.
        let old = store.read_note_by_id("pre-v3").await.unwrap().unwrap();
        assert_eq!(old.content, "survives migration");
        assert_eq!(old.scope, "global");

        // New kinds accepted.
        let id = store
            .write_note_scoped(
                "uncertainty",
                "post-migration",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        // FTS5 still works (was rebuilt during the migration).
        let hits = store
            .read_notes(Some("survives"), &[], &[], &[], 5, false)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "pre-v3");
    }

    #[tokio::test]
    async fn digest_cache_round_trip() {
        let store = make_store().await;
        let v = store.notes_version().await.unwrap();

        // Miss on an empty cache.
        assert!(store.digest_cache_get("abc", v).await.unwrap().is_none());

        // Put and hit.
        store
            .digest_cache_put("abc", v, "## Digest\n\n[note:xyz] invariant", 8)
            .await
            .unwrap();
        let hit = store.digest_cache_get("abc", v).await.unwrap();
        assert_eq!(hit.as_deref(), Some("## Digest\n\n[note:xyz] invariant"));

        // Same scope_hash, different version → miss. The cache is
        // versioned precisely so a post-write read doesn't serve
        // stale content.
        assert!(store
            .digest_cache_get("abc", v + 1)
            .await
            .unwrap()
            .is_none());

        // Put with replace at same key.
        store
            .digest_cache_put("abc", v, "## Digest v2", 3)
            .await
            .unwrap();
        let replaced = store.digest_cache_get("abc", v).await.unwrap();
        assert_eq!(replaced.as_deref(), Some("## Digest v2"));
    }

    #[tokio::test]
    async fn migration_v1_to_v2_adds_scope_columns_and_counter() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v1 database manually (stops short of MIGRATION_V2).
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(SCHEMA_NEW).unwrap();
            conn.execute(
                "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at)
                 VALUES ('old-1','decision','pre-ATOS note','[]','[]','s0',1000,1000)",
                [],
            )
            .unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 1, "baseline should be at v1");
        }

        // Reopen — MIGRATION_V2 runs.
        let store = NoteStore::open(&db_path).unwrap();

        // Old row preserved; gains scope='global', feature_id=NULL via column default.
        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        let old = notes.iter().find(|n| n.id == "old-1").unwrap();
        assert_eq!(old.scope, "global");
        assert!(old.feature_id.is_none());

        // notes_version counter is available.
        let v = store.notes_version().await.unwrap();
        assert!(v >= 0);

        // note_digest_cache table exists (query returns 0 rows, no error).
        let conn = Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM note_digest_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    // ── v5 → v6 migration (audit-hardening: source + supersedes) ──────────

    /// Build a v5 database by hand and confirm the v6 migration adds the
    /// two new columns + indexes without losing any rows. Pre-existing
    /// rows must default to `source = 'agent'` so the audit assembly
    /// continues to render them as the highest-priority source.
    #[tokio::test]
    async fn migrates_v5_to_v6_preserving_existing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v5 database manually: copy the schema we know v5 ends
        // with (no source/supersedes columns, kind CHECK admits the v5
        // set), insert one row, set user_version = 5.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE notes (
                     id            TEXT    PRIMARY KEY,
                     kind          TEXT    NOT NULL CHECK(kind IN (
                         'decision','attempt','invariant','todo','reflection',
                         'uncertainty','postmortem_pointer','redteam_finding',
                         'deviation','commitment','follow_up','goal'
                     )),
                     content       TEXT    NOT NULL,
                     symbols       TEXT    NOT NULL DEFAULT '[]',
                     files         TEXT    NOT NULL DEFAULT '[]',
                     session_id    TEXT    NOT NULL,
                     created_at    INTEGER NOT NULL,
                     updated_at    INTEGER NOT NULL,
                     tool_name     TEXT,
                     retired_at    INTEGER,
                     retired_by    TEXT,
                     scope         TEXT    NOT NULL DEFAULT 'global'
                                   CHECK(scope IN ('global','feature','session')),
                     feature_id    TEXT,
                     promoted_from TEXT,
                     related_entity TEXT
                 );
                 CREATE VIRTUAL TABLE notes_fts USING fts5(
                     content, kind, content='notes', content_rowid='rowid'
                 );
                 CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                 END;
                 CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TABLE meta_counters (key TEXT PRIMARY KEY, val INTEGER NOT NULL);
                 INSERT INTO meta_counters(key, val) VALUES ('notes_version', 0);
                 CREATE TABLE note_digest_cache (
                     scope_hash    TEXT    PRIMARY KEY,
                     digest_text   TEXT    NOT NULL,
                     notes_version INTEGER NOT NULL,
                     created_at    INTEGER NOT NULL
                 );
                 CREATE TABLE tool_call_log (
                     id         TEXT    PRIMARY KEY,
                     session_id TEXT    NOT NULL,
                     tool_name  TEXT    NOT NULL,
                     outcome    TEXT    NOT NULL,
                     called_at  INTEGER NOT NULL
                 );
                 INSERT INTO notes (
                     id, kind, content, session_id, created_at, updated_at,
                     scope
                 ) VALUES (
                     'pre-v6-row', 'decision', 'before migration',
                     'sess-1', 1000, 1000, 'global'
                 );
                 PRAGMA user_version = 5;
                 COMMIT;",
            )
            .unwrap();
        }

        // Now open through NoteStore — should run V6 and add columns.
        let store = NoteStore::open(&db_path).unwrap();

        // Pre-existing row gets source='agent', supersedes=NULL.
        let row = store
            .read_note_by_id("pre-v6-row")
            .await
            .unwrap()
            .expect("row preserved across v5→v6 migration");
        assert_eq!(row.kind, "decision");
        assert_eq!(row.content, "before migration");
        assert_eq!(row.source, "agent", "default source for pre-v6 rows");
        assert_eq!(row.supersedes, None, "no supersedes default");

        // user_version is at v6 or higher. Pinning the head version
        // here would force this test to be edited every schema bump
        // even though the v5→v6 invariants under test (source/
        // supersedes columns + indexes) don't change. Lower-bound
        // assertion captures the actual contract: opening a v5 db
        // must run v6's migration successfully.
        let conn = Connection::open(&db_path).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(
            v >= 6,
            "expected user_version >= 6 after migration, got {v}"
        );

        // The two new indexes exist.
        let has_source_idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='index' AND name='idx_notes_source_created'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_source_idx, 1);

        let has_supersedes_idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='index' AND name='idx_notes_supersedes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_supersedes_idx, 1);
    }

    // ── v6 → v7 migration (recipe-author kinds + payload_json) ────────────

    /// Build a v6 database by hand and confirm the v7 migration:
    ///
    /// 1. Adds the six new kinds to the CHECK constraint (verified
    ///    by writing a `research_finding` after the migration runs).
    /// 2. Adds the nullable `payload_json` column (verified by
    ///    reading back a written value).
    /// 3. Preserves every pre-v7 row, with `payload_json = NULL`.
    #[tokio::test]
    async fn migrates_v6_to_v7_preserving_existing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v6 database manually. v6 = v5 + (source, supersedes,
        // two indexes); we replicate enough to look like a real on-disk
        // v6 then bump user_version.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE notes (
                     id            TEXT    PRIMARY KEY,
                     kind          TEXT    NOT NULL CHECK(kind IN (
                         'decision','attempt','invariant','todo','reflection',
                         'uncertainty','postmortem_pointer','redteam_finding',
                         'deviation','commitment','follow_up','goal'
                     )),
                     content       TEXT    NOT NULL,
                     symbols       TEXT    NOT NULL DEFAULT '[]',
                     files         TEXT    NOT NULL DEFAULT '[]',
                     session_id    TEXT    NOT NULL,
                     created_at    INTEGER NOT NULL,
                     updated_at    INTEGER NOT NULL,
                     tool_name     TEXT,
                     retired_at    INTEGER,
                     retired_by    TEXT,
                     scope         TEXT    NOT NULL DEFAULT 'global'
                                   CHECK(scope IN ('global','feature','session')),
                     feature_id    TEXT,
                     promoted_from TEXT,
                     related_entity TEXT,
                     source        TEXT    NOT NULL DEFAULT 'agent',
                     supersedes    TEXT
                 );
                 CREATE VIRTUAL TABLE notes_fts USING fts5(
                     content, kind, content='notes', content_rowid='rowid'
                 );
                 CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                 END;
                 CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TABLE meta_counters (key TEXT PRIMARY KEY, val INTEGER NOT NULL);
                 INSERT INTO meta_counters(key, val) VALUES ('notes_version', 0);
                 CREATE TABLE note_digest_cache (
                     scope_hash    TEXT    PRIMARY KEY,
                     digest_text   TEXT    NOT NULL,
                     notes_version INTEGER NOT NULL,
                     created_at    INTEGER NOT NULL
                 );
                 CREATE TABLE tool_call_log (
                     id         TEXT    PRIMARY KEY,
                     session_id TEXT    NOT NULL,
                     tool_name  TEXT    NOT NULL,
                     outcome    TEXT    NOT NULL,
                     called_at  INTEGER NOT NULL
                 );
                 INSERT INTO notes (
                     id, kind, content, session_id, created_at, updated_at,
                     scope, source
                 ) VALUES (
                     'pre-v7-row', 'decision', 'before v7 migration',
                     'sess-1', 1000, 1000, 'global', 'agent'
                 );
                 PRAGMA user_version = 6;
                 COMMIT;",
            )
            .unwrap();
        }

        // Open through NoteStore — should run V7 and rebuild the table.
        let store = NoteStore::open(&db_path).unwrap();

        // Pre-existing row preserved with payload_json = NULL.
        let row = store
            .read_note_by_id("pre-v7-row")
            .await
            .unwrap()
            .expect("row preserved across v6→v7 migration");
        assert_eq!(row.kind, "decision");
        assert_eq!(
            row.payload_json, None,
            "pre-v7 rows default payload to NULL"
        );

        // user_version advances to the latest schema. The migration
        // ladder always runs every pending step, so once V8 lands
        // this test sees v8 (not v7) after opening a v6 DB. The
        // *behaviour* it actually pins — that v6→v7 preserves rows
        // and admits the recipe-author kinds — still holds; the
        // version number just reflects the full ladder.
        let conn = Connection::open(&db_path).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(
            v >= 7,
            "user_version must be at least 7 after v6 DB upgrade"
        );

        // The new kinds are admitted by the rebuilt CHECK. Round-trip
        // a `research_finding` write through `write_note_full` to
        // confirm both the CHECK and the payload column work.
        let payload = r#"{"authority":"authoritative","host":"courtlistener.com"}"#;
        let id = store
            .write_note_full(
                "research_finding",
                "CourtListener documents API supports cursor pagination",
                vec![],
                vec![],
                "sess-1",
                NoteScope::Feature,
                Some("p1"),
                None,
                NoteSource::Agent,
                None,
                Some(payload),
            )
            .await
            .unwrap();
        let written = store
            .read_note_by_id(&id)
            .await
            .unwrap()
            .expect("research_finding row should round-trip");
        assert_eq!(written.kind, "research_finding");
        assert_eq!(written.payload_json.as_deref(), Some(payload));
    }

    // ── v7 → v8 migration (tool_decision kind) ────────────────────────────

    /// Round-trip a `tool_decision` write through a fresh DB. Fresh
    /// installs run `SCHEMA_NEW` (not the migration ladder), so this
    /// test guards both the migration AND the new-DB path admitting
    /// the new kind. Payload is stored verbatim via `payload_json`.
    #[tokio::test]
    async fn fresh_db_admits_tool_decision_kind() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();

        let payload =
            r#"{"tool_id":"knowledge_lookup","outcome":"no-results","reasoning":"corpus thin"}"#;
        let id = store
            .write_note_full(
                "tool_decision",
                "knowledge_lookup → no-results (corpus thin)",
                vec!["knowledge_lookup".into()],
                vec![],
                "sess-td-1",
                NoteScope::Global,
                None,
                None,
                NoteSource::Agent,
                None,
                Some(payload),
            )
            .await
            .unwrap();

        let row = store
            .read_note_by_id(&id)
            .await
            .unwrap()
            .expect("tool_decision row should round-trip");
        assert_eq!(row.kind, "tool_decision");
        assert_eq!(row.payload_json.as_deref(), Some(payload));

        // Discoverable via the public `kinds` filter.
        let recent = store
            .read_notes(None, &[], &[], &["tool_decision".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, id);
    }

    /// Build a v7 database by hand and confirm the v8 migration:
    /// 1. Admits the new `tool_decision` kind through the rebuilt
    ///    CHECK constraint.
    /// 2. Preserves every pre-v8 row (including its v7 `payload_json`).
    /// 3. Bumps `PRAGMA user_version` to 8.
    #[tokio::test]
    async fn migrates_v7_to_v8_preserving_existing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v7 database manually with one pre-existing
        // research_finding row that carries a payload.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE notes (
                     id            TEXT    PRIMARY KEY,
                     kind          TEXT    NOT NULL CHECK(kind IN (
                         'decision','attempt','invariant','todo','reflection',
                         'uncertainty','postmortem_pointer','redteam_finding',
                         'deviation','commitment','follow_up','goal',
                         'research_finding','capability_request','recipe_issue',
                         'checkpoint','checkpoint_restored','deferred_question'
                     )),
                     content       TEXT    NOT NULL,
                     symbols       TEXT    NOT NULL DEFAULT '[]',
                     files         TEXT    NOT NULL DEFAULT '[]',
                     session_id    TEXT    NOT NULL,
                     created_at    INTEGER NOT NULL,
                     updated_at    INTEGER NOT NULL,
                     tool_name     TEXT,
                     retired_at    INTEGER,
                     retired_by    TEXT,
                     scope         TEXT    NOT NULL DEFAULT 'global'
                                   CHECK(scope IN ('global','feature','session')),
                     feature_id    TEXT,
                     promoted_from TEXT,
                     related_entity TEXT,
                     source        TEXT    NOT NULL DEFAULT 'agent',
                     supersedes    TEXT,
                     payload_json  TEXT
                 );
                 CREATE VIRTUAL TABLE notes_fts USING fts5(
                     content, kind, content='notes', content_rowid='rowid'
                 );
                 CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                 END;
                 CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TABLE meta_counters (key TEXT PRIMARY KEY, val INTEGER NOT NULL);
                 INSERT INTO meta_counters(key, val) VALUES ('notes_version', 0);
                 CREATE TABLE note_digest_cache (
                     scope_hash    TEXT    PRIMARY KEY,
                     digest_text   TEXT    NOT NULL,
                     notes_version INTEGER NOT NULL,
                     created_at    INTEGER NOT NULL
                 );
                 CREATE TABLE tool_call_log (
                     id         TEXT    PRIMARY KEY,
                     session_id TEXT    NOT NULL,
                     tool_name  TEXT    NOT NULL,
                     outcome    TEXT    NOT NULL,
                     called_at  INTEGER NOT NULL
                 );
                 INSERT INTO notes (
                     id, kind, content, session_id, created_at, updated_at,
                     scope, source, payload_json
                 ) VALUES (
                     'pre-v8-row', 'research_finding',
                     'recipe-author finding from v7',
                     'sess-old', 1000, 1000, 'global', 'agent',
                     '{\"authority\":\"authoritative\"}'
                 );
                 PRAGMA user_version = 7;
                 COMMIT;",
            )
            .unwrap();
        }

        // Open through NoteStore — should run V8 and rebuild the table.
        let store = NoteStore::open(&db_path).unwrap();

        // Pre-existing v7 row preserved with payload intact.
        let row = store
            .read_note_by_id("pre-v8-row")
            .await
            .unwrap()
            .expect("row preserved across v7→v8 migration");
        assert_eq!(row.kind, "research_finding");
        assert_eq!(
            row.payload_json.as_deref(),
            Some("{\"authority\":\"authoritative\"}"),
            "payload_json must survive the v7→v8 rename-recreate"
        );

        // user_version is at the current head after open. NoteStore
        // advances through every available migration; v9 lands the
        // tiered-retrieval + propagation surface additively, so
        // opening a v7 fixture leaves the DB at v9.
        let conn = Connection::open(&db_path).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 8, "v7→v8 migration must have fired, got {v}");

        // The new kind is admitted by the rebuilt CHECK.
        let id = store
            .write_note_full(
                "tool_decision",
                "first tool_decision post-migration",
                vec![],
                vec![],
                "sess-td-2",
                NoteScope::Global,
                None,
                None,
                NoteSource::Agent,
                None,
                Some(r#"{"tool_id":"search","outcome":"useful"}"#),
            )
            .await
            .unwrap();
        let written = store.read_note_by_id(&id).await.unwrap().unwrap();
        assert_eq!(written.kind, "tool_decision");
    }

    /// New writes via `write_note_with_source` carry their explicit
    /// source through to the read path. Each of the five sources
    /// round-trips intact.
    #[tokio::test]
    async fn write_note_with_source_round_trips_each_source() {
        let store = make_store().await;

        for src in [
            NoteSource::Agent,
            NoteSource::Committed,
            NoteSource::Extracted,
            NoteSource::Inferred,
            NoteSource::Observed,
        ] {
            let id = store
                .write_note_with_source(
                    "decision",
                    &format!("from {}", src.as_str()),
                    vec![],
                    vec![],
                    "sess-source",
                    NoteScope::Global,
                    None,
                    None,
                    src,
                    None,
                )
                .await
                .unwrap();
            let row = store.read_note_by_id(&id).await.unwrap().unwrap();
            assert_eq!(row.source, src.as_str());
            assert_eq!(row.supersedes, None);
        }
    }

    /// `write_note_with_source` carries `supersedes` through, and the
    /// referenced original row is left untouched (the reversal is a
    /// display-time concept).
    #[tokio::test]
    async fn supersedes_threads_through_writes_without_mutating_original() {
        let store = make_store().await;
        let original = store
            .write_note(
                "decision",
                "BTreeMap for ordered iteration",
                vec![],
                vec![],
                "sess-rev",
            )
            .await
            .unwrap();

        let reversal = store
            .write_note_with_source(
                "decision",
                "HashMap — ordered iteration not actually needed",
                vec![],
                vec![],
                "sess-rev",
                NoteScope::Global,
                None,
                None,
                NoteSource::Extracted,
                Some(&original),
            )
            .await
            .unwrap();

        let original_row = store.read_note_by_id(&original).await.unwrap().unwrap();
        let reversal_row = store.read_note_by_id(&reversal).await.unwrap().unwrap();

        // Original is preserved verbatim — only the reversal carries
        // the link.
        assert_eq!(original_row.content, "BTreeMap for ordered iteration");
        assert_eq!(original_row.supersedes, None);
        assert_eq!(original_row.source, "agent");
        assert_eq!(reversal_row.supersedes.as_deref(), Some(original.as_str()));
        assert_eq!(reversal_row.source, "extracted");
    }

    /// `NoteSource::priority` orders the five sources from highest
    /// to lowest. The audit assembly relies on this order.
    #[test]
    fn note_source_priority_order_is_stable() {
        assert!(NoteSource::Agent.priority() > NoteSource::Committed.priority());
        assert!(NoteSource::Committed.priority() > NoteSource::Extracted.priority());
        assert!(NoteSource::Extracted.priority() > NoteSource::Inferred.priority());
        assert!(NoteSource::Inferred.priority() > NoteSource::Observed.priority());
    }

    // ── v9 migration + T1 (tiered retrieval) tests ─────────────────────

    /// `content_hash` is deterministic over the canonical input
    /// tuple and stable across node_id rotation. Two peers writing
    /// the same `(kind, content, scope, feature_id, session_id)`
    /// must produce byte-identical hashes — that's the propagation
    /// dedup primary key.
    #[test]
    fn content_hash_is_deterministic_over_canonical_inputs() {
        let h1 = compute_content_hash(
            "decision",
            "use BTreeMap for ordered iteration",
            "global",
            None,
            "sess-1",
        );
        let h2 = compute_content_hash(
            "decision",
            "use BTreeMap for ordered iteration",
            "global",
            None,
            "sess-1",
        );
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64, "SHA-256 hex digest is 64 chars");

        // Changing any field changes the hash.
        let diff_kind = compute_content_hash(
            "attempt",
            "use BTreeMap for ordered iteration",
            "global",
            None,
            "sess-1",
        );
        let diff_session = compute_content_hash(
            "decision",
            "use BTreeMap for ordered iteration",
            "global",
            None,
            "sess-2",
        );
        let diff_feature = compute_content_hash(
            "decision",
            "use BTreeMap for ordered iteration",
            "feature",
            Some("feat-x"),
            "sess-1",
        );
        assert_ne!(h1, diff_kind);
        assert_ne!(h1, diff_session);
        assert_ne!(h1, diff_feature);

        // Field separator prevents content-tunneling: 'foobar' as
        // (kind="foo", content="bar") must not collide with
        // (kind="f", content="oobar"). Both share the prefix bytes
        // until the separator differs, so SHA-256 diverges.
        let tunnel_a = compute_content_hash("foo", "bar", "global", None, "s");
        let tunnel_b = compute_content_hash("f", "oobar", "global", None, "s");
        assert_ne!(tunnel_a, tunnel_b);
    }

    /// Migration v9 lands additively: pre-v9 rows survive, new
    /// columns appear with their defaults, and `content_hash` is
    /// backfilled in Rust post-migration.
    #[tokio::test]
    async fn migrates_v8_to_v9_preserving_existing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Hand-construct a v8 DB with one pre-v9 row by replaying
        // the same inline schema the v7→v8 test uses, plus the v8
        // CHECK update + `user_version = 8`. Replaying migrations
        // by chaining the consts doesn't work — `MIGRATION_V1`
        // assumes an existing `notes` table to rename.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE notes (
                     id            TEXT    PRIMARY KEY,
                     kind          TEXT    NOT NULL CHECK(kind IN (
                         'decision','attempt','invariant','todo','reflection',
                         'uncertainty','postmortem_pointer','redteam_finding',
                         'deviation','commitment','follow_up','goal',
                         'research_finding','capability_request','recipe_issue',
                         'checkpoint','checkpoint_restored','deferred_question',
                         'tool_decision'
                     )),
                     content       TEXT    NOT NULL,
                     symbols       TEXT    NOT NULL DEFAULT '[]',
                     files         TEXT    NOT NULL DEFAULT '[]',
                     session_id    TEXT    NOT NULL,
                     created_at    INTEGER NOT NULL,
                     updated_at    INTEGER NOT NULL,
                     tool_name     TEXT,
                     retired_at    INTEGER,
                     retired_by    TEXT,
                     scope         TEXT    NOT NULL DEFAULT 'global'
                                   CHECK(scope IN ('global','feature','session')),
                     feature_id    TEXT,
                     promoted_from TEXT,
                     related_entity TEXT,
                     source        TEXT    NOT NULL DEFAULT 'agent',
                     supersedes    TEXT,
                     payload_json  TEXT
                 );
                 CREATE VIRTUAL TABLE notes_fts USING fts5(
                     content, kind, content='notes', content_rowid='rowid'
                 );
                 CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                 END;
                 CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TABLE meta_counters (key TEXT PRIMARY KEY, val INTEGER NOT NULL);
                 INSERT INTO meta_counters(key, val) VALUES ('notes_version', 0);
                 CREATE TABLE note_digest_cache (
                     scope_hash    TEXT    PRIMARY KEY,
                     digest_text   TEXT    NOT NULL,
                     notes_version INTEGER NOT NULL,
                     created_at    INTEGER NOT NULL
                 );
                 CREATE TABLE tool_call_log (
                     id         TEXT    PRIMARY KEY,
                     session_id TEXT    NOT NULL,
                     tool_name  TEXT    NOT NULL,
                     outcome    TEXT    NOT NULL,
                     called_at  INTEGER NOT NULL
                 );
                 INSERT INTO notes (
                     id, kind, content, session_id, created_at, updated_at,
                     scope, source
                 ) VALUES (
                     'pre-v9', 'decision', 'v8 row',
                     'sess-old', 1000, 1000, 'global', 'agent'
                 );
                 PRAGMA user_version = 8;
                 COMMIT;",
            )
            .unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 8, "fixture should be at v8");
        }

        // Open through NoteStore — runs MIGRATION_V9 + backfill.
        let store = NoteStore::open(&db_path).unwrap();
        let conn = store.conn.lock().await;
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 9, "post-migration user_version should be ≥ 9, got {v}");

        // Pre-v9 row still there.
        let row: (
            String,
            String,
            i64,
            i64,
            Option<String>,
            Option<i64>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT id, content, private, tombstone, origin_node_id, NULL, content_hash
                   FROM notes WHERE id = 'pre-v9'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get::<_, Option<i64>>(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.1, "v8 row");
        assert_eq!(row.2, 0, "private defaults to 0");
        assert_eq!(row.3, 0, "tombstone defaults to 0");
        assert_eq!(row.4, None, "origin_node_id starts NULL");
        let backfilled_hash = row.6.expect("backfill populates content_hash");
        let expected_hash = compute_content_hash("decision", "v8 row", "global", None, "sess-old");
        assert_eq!(backfilled_hash, expected_hash);

        // New tables exist.
        let embed_table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                   WHERE type='table' AND name='note_embeddings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(embed_table, 1, "note_embeddings table created");
        let watermark_table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                   WHERE type='table' AND name='note_propagation_watermark'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            watermark_table, 1,
            "note_propagation_watermark table created"
        );

        // Indexes exist.
        let prop_idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                   WHERE type='index' AND name='idx_notes_propagation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prop_idx, 1);
    }

    /// Re-opening a v9+ DB is a no-op: no errors, no double-fire,
    /// backfill query matches zero rows on the second open.
    #[tokio::test]
    async fn v9_migration_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");
        let s1 = NoteStore::open(&db_path).unwrap();
        drop(s1);
        // Second open should silently no-op.
        let s2 = NoteStore::open(&db_path).unwrap();
        let conn = s2.conn.lock().await;
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 9);
    }

    /// New notes written through `write_note_full` carry a
    /// `content_hash`, and the hash matches the
    /// `compute_content_hash` invariant.
    #[tokio::test]
    async fn write_note_full_sets_content_hash() {
        let store = make_store().await;
        let id = store
            .write_note(
                "invariant",
                "EOS bypass needs force_continue",
                vec!["embedded.rs".into()],
                vec![],
                "sess-1",
            )
            .await
            .unwrap();
        let conn = store.conn.lock().await;
        let hash: String = conn
            .query_row(
                "SELECT content_hash FROM notes WHERE id = ?",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        let expected = compute_content_hash(
            "invariant",
            "EOS bypass needs force_continue",
            "global",
            None,
            "sess-1",
        );
        assert_eq!(hash, expected);
    }

    /// `with_embed_fn` causes `write_note_full` to persist a
    /// `note_embeddings` row in the same transaction.
    #[tokio::test]
    async fn write_note_with_embed_fn_persists_embedding() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");
        // Mock embed_fn that returns a fixed 4-dim vector keyed on
        // input length — deterministic, dependency-free.
        let embed: EmbedFn = Arc::new(|text: &str| {
            let len = text.len() as f32;
            let v = vec![len, len + 1.0, len + 2.0, len + 3.0];
            Box::pin(async move { Ok(v) })
        });
        let store = NoteStore::open(&db_path).unwrap().with_embed_fn(embed);
        assert!(store.has_embed_fn());

        let id = store
            .write_note("decision", "hello world", vec![], vec![], "sess-1")
            .await
            .unwrap();
        let conn = store.conn.lock().await;
        let (bytes, dim, model): (Vec<u8>, i64, String) = conn
            .query_row(
                "SELECT embedding, dim, model_id FROM note_embeddings WHERE note_id = ?",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(dim, 4);
        assert!(!model.is_empty());
        let decoded = embedding_from_le_bytes(&bytes).unwrap();
        assert_eq!(decoded, vec![11.0, 12.0, 13.0, 14.0]); // "hello world".len() == 11
    }

    /// `read_notes_scoped_semantic` with `weight=0.0` returns the
    /// exact same rows in the exact same order as
    /// `read_notes_scoped`. This is the canonical
    /// "no-cost-when-off" invariant from `CLUSTER_SCORE_BLEND.md`.
    #[tokio::test]
    async fn semantic_blend_weight_zero_is_byte_identical_to_baseline() {
        // SAFETY: env vars are process-global; this test sets and
        // restores SOVEREIGN_NOTES_EMBED_WEIGHT serially. The test
        // suite does not run write_note_full concurrently with this
        // weight reset; if added later, wrap with a serial guard.
        unsafe {
            std::env::set_var("SOVEREIGN_NOTES_EMBED_WEIGHT", "0.0");
        }
        let store = make_store().await;
        for (kind, content) in [
            ("decision", "use BTreeMap for ordered iteration"),
            ("invariant", "EOS bypass needs force_continue"),
            ("attempt", "tried mocking the database but it broke prod"),
        ] {
            store
                .write_note(kind, content, vec![], vec![], "s1")
                .await
                .unwrap();
        }
        let baseline = store
            .read_notes_scoped(
                Some("BTreeMap"),
                &[],
                &[],
                &[],
                10,
                false,
                &ScopeFilter::default(),
            )
            .await
            .unwrap();
        let blended = store
            .read_notes_scoped_semantic(
                Some("BTreeMap"),
                &[],
                &[],
                &[],
                10,
                false,
                &ScopeFilter::default(),
                Some("any semantic query — should be ignored at weight=0"),
            )
            .await
            .unwrap();
        let baseline_ids: Vec<_> = baseline.iter().map(|n| &n.id).collect();
        let blended_ids: Vec<_> = blended.iter().map(|n| &n.id).collect();
        assert_eq!(baseline_ids, blended_ids);
        unsafe {
            std::env::remove_var("SOVEREIGN_NOTES_EMBED_WEIGHT");
        }
    }

    /// MinMax normalisation collapses a degenerate (all-equal)
    /// pool to `0.5`, not NaN. The blend invariant requires every
    /// candidate score lands on `[0.0, 1.0]`.
    #[test]
    fn min_max_normalise_handles_degenerate_pool() {
        let mm = MinMax::from_slice(&[3.0, 3.0, 3.0]);
        assert_eq!(mm.normalise(3.0), 0.5);

        let mm = MinMax::from_slice(&[1.0, 5.0, 9.0]);
        assert_eq!(mm.normalise(1.0), 0.0);
        assert_eq!(mm.normalise(9.0), 1.0);
        assert_eq!(mm.normalise(5.0), 0.5);
        // Out-of-pool values clamp to [0, 1].
        assert_eq!(mm.normalise(100.0), 1.0);
        assert_eq!(mm.normalise(-100.0), 0.0);
    }

    /// `cosine_sim` returns 0.0 on dim mismatch (no signal) and
    /// 1.0 on identical vectors.
    #[test]
    fn cosine_sim_handles_edges() {
        let v = vec![1.0_f32, 0.0, 0.0, 0.0];
        assert!((cosine_sim(&v, &v) - 1.0).abs() < 1e-6);

        let orthogonal = vec![0.0_f32, 1.0, 0.0, 0.0];
        assert!(cosine_sim(&v, &orthogonal).abs() < 1e-6);

        let mismatch_dim = vec![1.0_f32, 0.0];
        assert_eq!(cosine_sim(&v, &mismatch_dim), 0.0);

        let zero = vec![0.0_f32, 0.0, 0.0, 0.0];
        assert_eq!(cosine_sim(&zero, &v), 0.0);
    }

    // ── v10 migration + T2 (entity-graph) tests ────────────────────────

    /// Author-supplied `symbols` and `files` land as
    /// `note_entities` rows with kind="Symbol" / "File", even
    /// without a GLiNER extractor wired. This keeps
    /// `read_notes_related` useful from day 0.
    #[tokio::test]
    async fn write_note_persists_author_symbols_as_entities() {
        let store = make_store().await;
        let id = store
            .write_note(
                "decision",
                "use BTreeMap",
                vec!["BTreeMap".into(), "Ordering".into()],
                vec!["src/lib.rs".into()],
                "s1",
            )
            .await
            .unwrap();
        let conn = store.conn.lock().await;
        let rows: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare("SELECT entity, kind FROM note_entities WHERE note_id = ? ORDER BY kind, entity")
                .unwrap();
            let mapped = stmt
                .query_map(params![id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .unwrap();
            mapped.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(
            rows,
            vec![
                ("src/lib.rs".to_string(), "File".to_string()),
                ("BTreeMap".to_string(), "Symbol".to_string()),
                ("Ordering".to_string(), "Symbol".to_string()),
            ]
        );
    }

    /// `read_notes_related` finds notes that co-mention a symbol.
    /// Seed entity exists in two notes; the third note shares an
    /// adjacent entity ("BTreeMap" appears in n1 + n3, n2 shares
    /// "Ordering" with n1) → result is ranked by entity-overlap.
    #[tokio::test]
    async fn read_notes_related_finds_co_mentioned_notes() {
        let store = make_store().await;
        // n1 mentions BTreeMap + Ordering
        let _n1 = store
            .write_note(
                "decision",
                "use BTreeMap; depends on Ordering",
                vec!["BTreeMap".into(), "Ordering".into()],
                vec![],
                "s",
            )
            .await
            .unwrap();
        // n2 shares Ordering with n1
        let n2 = store
            .write_note(
                "invariant",
                "Ordering must be total",
                vec!["Ordering".into()],
                vec![],
                "s",
            )
            .await
            .unwrap();
        // n3 shares BTreeMap with n1
        let n3 = store
            .write_note(
                "todo",
                "switch BTreeMap to HashMap",
                vec!["BTreeMap".into()],
                vec![],
                "s",
            )
            .await
            .unwrap();
        // n4 unrelated
        let _n4 = store
            .write_note(
                "decision",
                "use channels for IPC",
                vec!["mpsc".into()],
                vec![],
                "s",
            )
            .await
            .unwrap();

        let related = store.read_notes_related("BTreeMap", 10).await.unwrap();
        let ids: Vec<&str> = related.iter().map(|n| n.id.as_str()).collect();
        // n2 shares Ordering with the seed bag (n1+n3's entities)
        // and IS the related note. n1 + n3 are seed notes
        // themselves (directly mention BTreeMap) so the algorithm
        // excludes them — "find OTHER notes connected to the seed,
        // not the seed itself".
        assert!(
            ids.contains(&n2.as_str()),
            "n2 (shares Ordering with seed-bag) must appear, got {ids:?}"
        );
        // n3 is a seed note; excluded.
        assert!(
            !ids.contains(&n3.as_str()),
            "n3 is a seed note (directly mentions BTreeMap); must NOT appear, got {ids:?}"
        );
        // n4 must NOT appear (no overlap with seed bag).
        let n4_in_results = related.iter().any(|n| n.content.contains("mpsc"));
        assert!(!n4_in_results, "n4 must not appear in BTreeMap-related set");
    }

    /// Empty result when the seed isn't in any entity row — no
    /// crash, no fallback to FTS5 (caller does that).
    #[tokio::test]
    async fn read_notes_related_returns_empty_for_unknown_seed() {
        let store = make_store().await;
        store
            .write_note("decision", "alpha", vec![], vec![], "s")
            .await
            .unwrap();
        let related = store.read_notes_related("DoesNotExist", 10).await.unwrap();
        assert!(related.is_empty());
    }

    /// GLiNER extractor closure receives the note content and its
    /// emitted `(entity, kind)` pairs land in `note_entities`
    /// alongside the author-supplied symbols.
    #[tokio::test]
    async fn gliner_fn_extracts_and_persists_entities() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");
        // Mock GLiNER: any text containing "Anthropic" emits one
        // Organization entity. Deterministic, no model load.
        let gliner: GlinerFn = Arc::new(|text: &str| {
            let t = text.to_string();
            Box::pin(async move {
                if t.contains("Anthropic") {
                    Ok(vec![("Anthropic".to_string(), "Organization".to_string())])
                } else {
                    Ok(Vec::new())
                }
            })
        });
        let store = NoteStore::open(&db_path).unwrap().with_gliner_fn(gliner);
        assert!(store.has_gliner_fn());

        let id = store
            .write_note(
                "decision",
                "switch primary model to Anthropic claude-opus-4-7",
                vec!["primary_model".into()],
                vec![],
                "s",
            )
            .await
            .unwrap();
        let conn = store.conn.lock().await;
        let rows: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT entity, kind FROM note_entities WHERE note_id = ? ORDER BY kind, entity",
                )
                .unwrap();
            let mapped = stmt
                .query_map(params![id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .unwrap();
            mapped.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert!(
            rows.contains(&("Anthropic".to_string(), "Organization".to_string())),
            "GLiNER emission must land, got {rows:?}"
        );
        assert!(
            rows.contains(&("primary_model".to_string(), "Symbol".to_string())),
            "author-supplied symbol must land alongside, got {rows:?}"
        );
    }

    /// `embedding_to_le_bytes` round-trips through
    /// `embedding_from_le_bytes` byte-identically. The on-disk
    /// format is endian-pinned so a Mac and a Linux toolbx peer
    /// agree on the BLOB bytes for the same vector.
    #[test]
    fn embedding_le_bytes_roundtrip() {
        let v = vec![1.5_f32, -2.25, 0.0, 99999.0];
        let bytes = embedding_to_le_bytes(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        let decoded = embedding_from_le_bytes(&bytes).unwrap();
        assert_eq!(decoded, v);
    }
}
