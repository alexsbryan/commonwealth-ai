// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wrapped — the precomputed story-card artifact over a conversation
//! corpus.
//!
//! The show reads this artifact, never live inference. Three commitments
//! are enforced here, not in the UI:
//!
//! 1. **No model originates a number.** Every figure is a deterministic
//!    Rust fold over chunk rows (and the GLiNER `chunk_entities` table,
//!    whose surface forms are verbatim chunk spans). [`ScaleCard`] carries
//!    a `derivation` trace, parcel_analytics-style.
//! 2. **Every quote is verbatim and cited.** Excerpts/citations carry the
//!    `(chunk_id, text)` pair; folds only pick spans verified against the
//!    in-memory chunk content, and [`verify_wrapped_artifact`] re-checks
//!    the finished artifact against the index — a failing artifact is
//!    never served.
//! 3. **Absent data ⇒ absent card.** Each card section is optional; the
//!    deck is whatever earned its way in. The JS skips unknown card
//!    types, so future enriched cards slot in without a schema break.
//!
//! Build trigger is desktop-native: [`wrapped_artifact`] is the bridge
//! op — it returns the cached `wrapped/all-time.json` under the corpus
//! index dir when fresh (vs `_corpus_meta.json`'s `last_updated` +
//! `canonical_fingerprint`), and rebuilds on demand otherwise.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use chrono::{Datelike, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};

use corpus_engine::chunkers::threaded_turns::{parse_turns, Attribution};
use corpus_engine::index::CorpusIndex;

pub const WRAPPED_SCHEMA_VERSION: u32 = 2;
pub const WRAPPED_DIRNAME: &str = "wrapped";
const EDITION_ALL_TIME: &str = "all-time";

/// Two turns more than this many minutes apart belong to different
/// sessions (the "longest rabbit hole" boundary).
const SESSION_GAP_MIN: i64 = 30;

/// GLiNER score floor — matches the production extraction threshold.
const ENTITY_SCORE_FLOOR: f64 = 0.6;

/// The pronoun/furniture FLOOR — forms the case-profile signal cannot
/// judge ("I" is always capitalized; "user"/"assistant" appear in every
/// turn header). Everything else generic is dropped by corpus evidence
/// (see [`generic_keys_by_case_profile`]), not by growing this list.
const WRAPPED_ENTITY_STOPLIST: &[&str] = &[
    "i",
    "me",
    "my",
    "you",
    "your",
    "we",
    "us",
    "our",
    "he",
    "she",
    "it",
    "they",
    "them",
    "user",
    "users",
    "the user",
    "assistant",
    "the assistant",
    "claude",
];

/// A surface form needs at least this many lowercase sightings in
/// assistant prose before the case-profile signal may call it generic.
const CASE_PROFILE_MIN_EVIDENCE: u32 = 3;

/// …and lowercase sightings must be at least this share of the decisive
/// evidence (lowercase + mid-sentence-capitalized). Proper nouns that
/// collide with common words ("Fed"/"fed", "Apple"/"apple") survive
/// because their capitalized-mid-sentence count dominates.
const CASE_PROFILE_GENERIC_SHARE: f64 = 0.5;

/// A topic must recur across at least this many distinct conversations
/// to appear on the Obsessions card — "X came up once" is no hook.
const MIN_TOPIC_CONVS: usize = 3;

// ─── Artifact schema (the JSON the JS reads) ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedArtifact {
    pub schema_version: u32,
    pub edition: String,
    pub built_at_unix: i64,
    pub corpus_id: String,
    /// Staleness key — `_corpus_meta.json::last_updated` at build time.
    pub corpus_last_updated: i64,
    pub corpus_fingerprint: Option<String>,
    /// Ordered deck. A card with no data is simply absent.
    pub cards: Vec<WrappedCard>,
}

/// One story card. Tagged so the JS can switch on `type` and **skip
/// unknown types** — the forward-compat seam for future enriched cards.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WrappedCard {
    Scale(ScaleCard),
    Rhythm(RhythmCard),
    Obsessions(ObsessionsCard),
    Cast(CastCard),
    Door(DoorCard),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaleCard {
    pub conversations: usize,
    pub months_active: usize,
    pub words_total: u64,
    pub words_user: u64,
    pub words_assistant: u64,
    /// `YYYY-MM-DD` of the earliest / latest timestamped turn.
    pub first_date: String,
    pub last_date: String,
    /// Human-readable trace of how each figure was computed — the
    /// parcel_analytics no-confabulated-numbers discipline.
    pub derivation: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RhythmCard {
    /// `heatmap[weekday][hour]` turn counts; weekday 0 = Monday, hour is
    /// UTC (the archive's clock — turn headers slice the export's UTC
    /// ISO timestamps).
    pub heatmap: Vec<Vec<u32>>,
    pub total_turns: u64,
    pub longest_session: Option<LongestSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongestSession {
    pub conv_uuid: String,
    pub title: Option<String>,
    /// `YYYY-MM-DD` of the session's first turn.
    pub date: String,
    pub duration_minutes: u32,
    pub turns: usize,
    /// Tap-through targets: the chunks this session's turns live in.
    pub chunk_ids: Vec<u64>,
    /// First user turn's opening line — verbatim, audited.
    pub excerpt: Option<Excerpt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Excerpt {
    pub chunk_id: u64,
    pub text: String,
}

/// A cited verbatim span. `char_start`/`char_end` come from the GLiNER
/// row and are best-effort (highlighting); the audited invariant is
/// that `text` appears verbatim in chunk `chunk_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Citation {
    pub chunk_id: u64,
    pub char_start: usize,
    pub char_end: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObsessionsCard {
    pub quarters: Vec<QuarterTopics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarterTopics {
    /// `YYYY-Qn`.
    pub quarter: String,
    pub topics: Vec<TopicStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicStat {
    pub text: String,
    pub label: String,
    /// Distinct conversations mentioning this entity in the quarter.
    pub conversations: usize,
    pub sample: Citation,
}

/// Shaped so the SDK `forceGraph` view renders nodes/edges directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CastCard {
    pub nodes: Vec<CastNode>,
    pub edges: Vec<CastEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CastNode {
    pub id: String,
    pub canonical_name: String,
    pub entity_type: String,
    /// Incident co-occurrence edges among the cast.
    pub degree: usize,
    /// Distinct conversations this entity appears in.
    pub conversations: usize,
    pub sample: Citation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CastEdge {
    pub source: String,
    pub target: String,
    pub relationship_type: String,
    /// Distinct conversations where both endpoints appear.
    pub co_conversations: usize,
}

/// Static closing card; copy lives in the bundle.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DoorCard {}

// ─── Intermediate model (pure-fold input) ────────────────────────────

/// One conversation reconstructed from its chunks — the unit the folds
/// consume. Pure data so the folds unit-test without a Lance index.
#[derive(Debug, Clone)]
pub struct ConvDoc {
    pub conv_uuid: String,
    pub title: Option<String>,
    /// Turns in archive order (chunk id ascending, in-chunk order).
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub ts: Option<NaiveDateTime>,
    pub is_user: bool,
    /// Whitespace-token count of the body (header line excluded).
    pub words: u64,
    pub chunk_id: u64,
    /// First non-empty body line — the only quotable span a turn offers.
    pub first_line: String,
}

/// One GLiNER `chunk_entities` row.
#[derive(Debug, Clone)]
pub struct EntityRow {
    pub chunk_id: u64,
    pub text: String,
    pub label: String,
    pub char_start: usize,
    pub char_end: usize,
    pub score: f64,
    pub conv_uuid: Option<String>,
}

// ─── The op ──────────────────────────────────────────────────────────

/// Load-or-build — THE op the bridge serves. Returns the cached
/// artifact when fresh against `_corpus_meta.json`, rebuilds otherwise.
pub async fn wrapped_artifact(
    index_path: &Path,
    state_db: Option<&Path>,
) -> Result<WrappedArtifact, String> {
    let meta = read_corpus_meta(index_path)?;
    if let Some(cached) = read_cached_artifact(index_path, &meta) {
        return Ok(cached);
    }
    build_wrapped_artifact(index_path, state_db).await
}

/// Full scan → folds → audit → best-effort cache write. Never returns
/// an artifact the audit rejected.
pub async fn build_wrapped_artifact(
    index_path: &Path,
    state_db: Option<&Path>,
) -> Result<WrappedArtifact, String> {
    let meta = read_corpus_meta(index_path)?;
    let index = CorpusIndex::open(index_path)
        .await
        .map_err(|e| format!("open index: {e}"))?;
    let rows = index
        .all_chunks_full()
        .await
        .map_err(|e| format!("scan chunks: {e}"))?;

    let chunk_content: HashMap<u64, String> =
        rows.iter().map(|r| (r.id, r.content.clone())).collect();
    let chunk_quarter = chunk_quarter_map(&rows);
    let docs = build_conv_docs(&rows);

    let entity_rows = match state_db {
        Some(db) => read_chunk_entities(db, &meta.corpus_id)?,
        None => Vec::new(),
    };

    // Corpus-evidence generics pass: which candidate surface forms does
    // the assistant's own prose write in lowercase? Those are common
    // nouns, not names — drop them from the entity cards.
    let assistant_texts = collect_assistant_text(&rows);
    let candidates: HashSet<String> = entity_rows
        .iter()
        .filter(|r| r.score >= ENTITY_SCORE_FLOOR && r.text.trim().len() >= 3)
        .map(|r| r.text.trim().to_ascii_lowercase())
        .collect();
    let generic = generic_keys_by_case_profile(&candidates, &assistant_texts);

    let mut cards = Vec::new();
    if let Some(c) = fold_scale(&docs) {
        cards.push(WrappedCard::Scale(c));
    }
    if let Some(c) = fold_rhythm(&docs) {
        cards.push(WrappedCard::Rhythm(c));
    }
    if let Some(c) = fold_obsessions(&entity_rows, &chunk_quarter, &chunk_content, &generic) {
        cards.push(WrappedCard::Obsessions(c));
    }
    if let Some(c) = fold_cast(&entity_rows, &chunk_content, &generic) {
        cards.push(WrappedCard::Cast(c));
    }
    cards.push(WrappedCard::Door(DoorCard::default()));

    let artifact = WrappedArtifact {
        schema_version: WRAPPED_SCHEMA_VERSION,
        edition: EDITION_ALL_TIME.to_string(),
        built_at_unix: now_unix(),
        corpus_id: meta.corpus_id.clone(),
        corpus_last_updated: meta.last_updated,
        corpus_fingerprint: meta.canonical_fingerprint.clone(),
        cards,
    };

    verify_artifact_against_content(&artifact, &chunk_content)?;

    // Cache write is best-effort: a read-only index dir must not fail
    // the op — the artifact is already audited and servable.
    if let Err(e) = write_cached_artifact(index_path, &artifact) {
        eprintln!("wrapped: cache write skipped: {e}");
    }
    Ok(artifact)
}

/// The audit, against the live index: every cited chunk id resolves and
/// every embedded span is a verbatim substring of its chunk's content.
/// `Err` = do not serve.
pub async fn verify_wrapped_artifact(
    artifact: &WrappedArtifact,
    index_path: &Path,
) -> Result<(), String> {
    let cited: Vec<u64> = cited_chunk_ids(artifact).into_iter().collect();
    if cited.is_empty() {
        return Ok(());
    }
    let index = CorpusIndex::open(index_path)
        .await
        .map_err(|e| format!("open index: {e}"))?;
    let chunks = index
        .get_chunks(&cited)
        .await
        .map_err(|e| format!("read cited chunks: {e}"))?;
    let content: HashMap<u64, String> = chunks.into_iter().map(|c| (c.id, c.content)).collect();
    verify_artifact_against_content(artifact, &content)
}

// ─── Audit internals ─────────────────────────────────────────────────

fn cited_chunk_ids(a: &WrappedArtifact) -> HashSet<u64> {
    let mut ids = HashSet::new();
    for card in &a.cards {
        match card {
            WrappedCard::Rhythm(r) => {
                if let Some(s) = &r.longest_session {
                    ids.extend(s.chunk_ids.iter().copied());
                    if let Some(e) = &s.excerpt {
                        ids.insert(e.chunk_id);
                    }
                }
            }
            WrappedCard::Obsessions(o) => {
                for q in &o.quarters {
                    for t in &q.topics {
                        ids.insert(t.sample.chunk_id);
                    }
                }
            }
            WrappedCard::Cast(c) => {
                for n in &c.nodes {
                    ids.insert(n.sample.chunk_id);
                }
            }
            WrappedCard::Scale(_) | WrappedCard::Door(_) => {}
        }
    }
    ids
}

/// One span check: the cited chunk must resolve and `text` must appear
/// verbatim in its content.
fn check_span(
    content: &HashMap<u64, String>,
    failures: &mut Vec<String>,
    chunk_id: u64,
    text: &str,
    what: &str,
) {
    match content.get(&chunk_id) {
        None => failures.push(format!("{what}: cited chunk {chunk_id} does not resolve")),
        Some(c) if !c.contains(text) => failures.push(format!(
            "{what}: span not verbatim in chunk {chunk_id}: {:?}",
            &text[..text.len().min(80)]
        )),
        Some(_) => {}
    }
}

/// Shared audit core: check citations against an id → content map.
fn verify_artifact_against_content(
    a: &WrappedArtifact,
    content: &HashMap<u64, String>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    for card in &a.cards {
        match card {
            WrappedCard::Rhythm(r) => {
                if let Some(s) = &r.longest_session {
                    for id in &s.chunk_ids {
                        if !content.contains_key(id) {
                            failures.push(format!(
                                "rhythm.longest_session: cited chunk {id} does not resolve"
                            ));
                        }
                    }
                    if let Some(e) = &s.excerpt {
                        check_span(
                            content,
                            &mut failures,
                            e.chunk_id,
                            &e.text,
                            "rhythm.excerpt",
                        );
                    }
                }
            }
            WrappedCard::Obsessions(o) => {
                for q in &o.quarters {
                    for t in &q.topics {
                        check_span(
                            content,
                            &mut failures,
                            t.sample.chunk_id,
                            &t.sample.text,
                            "obsessions.sample",
                        );
                    }
                }
            }
            WrappedCard::Cast(c) => {
                for n in &c.nodes {
                    check_span(
                        content,
                        &mut failures,
                        n.sample.chunk_id,
                        &n.sample.text,
                        "cast.sample",
                    );
                }
            }
            WrappedCard::Scale(_) | WrappedCard::Door(_) => {}
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "wrapped audit failed ({} violations): {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

// ─── Corpus meta + cache ─────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct CorpusMetaSlice {
    corpus_id: String,
    #[serde(default)]
    last_updated: i64,
    #[serde(default)]
    canonical_fingerprint: Option<String>,
}

fn read_corpus_meta(index_path: &Path) -> Result<CorpusMetaSlice, String> {
    let path = index_path.join("_corpus_meta.json");
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn cache_path(index_path: &Path) -> std::path::PathBuf {
    index_path
        .join(WRAPPED_DIRNAME)
        .join(format!("{EDITION_ALL_TIME}.json"))
}

/// A cache that fails to deserialize (schema drift) or is stale simply
/// triggers a rebuild — self-healing, never an error. Staleness is
/// keyed on the CORPUS (last_updated + fingerprint), not the builder:
/// a fold/stoplist change serves the old cache until the corpus next
/// updates. Bump [`WRAPPED_SCHEMA_VERSION`] when a builder change must
/// reach existing installs immediately.
fn read_cached_artifact(index_path: &Path, meta: &CorpusMetaSlice) -> Option<WrappedArtifact> {
    let bytes = std::fs::read(cache_path(index_path)).ok()?;
    let artifact: WrappedArtifact = serde_json::from_slice(&bytes).ok()?;
    let fresh = artifact.schema_version == WRAPPED_SCHEMA_VERSION
        && artifact.corpus_last_updated == meta.last_updated
        && artifact.corpus_fingerprint == meta.canonical_fingerprint;
    fresh.then_some(artifact)
}

fn write_cached_artifact(index_path: &Path, artifact: &WrappedArtifact) -> Result<(), String> {
    let path = cache_path(index_path);
    let dir = path.parent().expect("cache path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let json = serde_json::to_vec_pretty(artifact).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

use sovereign_time::unix_now as now_unix;

// ─── Chunk rows → ConvDocs ───────────────────────────────────────────

fn parse_turn_ts(raw: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M").ok()
}

/// Body of a turn block: everything after the `### [...]` header line.
fn turn_body(block: &str) -> &str {
    match block.find('\n') {
        Some(i) => block[i + 1..].trim_start_matches('\n'),
        None => "",
    }
}

fn first_nonempty_line(body: &str) -> String {
    body.lines()
        .map(str::trim_end)
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string()
}

/// Group chunk rows into per-conversation docs, parsing turn blocks via
/// the chunker's own header grammar. Rows without a `source_doc_id`
/// fold under their stringified chunk id (degenerate but total).
pub fn build_conv_docs(rows: &[corpus_engine::index::EnrichmentChunkRow]) -> Vec<ConvDoc> {
    #[derive(Deserialize, Default)]
    struct DocMeta {
        #[serde(default)]
        summary: Option<String>,
    }

    let mut sorted: Vec<&corpus_engine::index::EnrichmentChunkRow> = rows.iter().collect();
    sorted.sort_by_key(|r| r.id);

    let mut by_doc: BTreeMap<String, ConvDoc> = BTreeMap::new();
    for row in sorted {
        let key = row
            .source_doc_id
            .clone()
            .unwrap_or_else(|| row.id.to_string());
        let doc = by_doc.entry(key.clone()).or_insert_with(|| ConvDoc {
            conv_uuid: key,
            title: None,
            turns: Vec::new(),
        });
        if doc.title.is_none() {
            let from_meta = row
                .metadata_raw
                .as_deref()
                .and_then(|m| serde_json::from_str::<DocMeta>(m).ok())
                .and_then(|m| m.summary)
                .filter(|s| !s.trim().is_empty());
            doc.title = from_meta.or_else(|| row.title.clone());
        }
        for t in parse_turns(&row.content) {
            let body = turn_body(&t.block);
            doc.turns.push(Turn {
                ts: t.timestamp.as_deref().and_then(parse_turn_ts),
                is_user: t.attribution == Attribution::User,
                words: body.split_whitespace().count() as u64,
                chunk_id: row.id,
                first_line: first_nonempty_line(body),
            });
        }
    }
    by_doc.into_values().collect()
}

/// chunk id → `YYYY-Qn` of its first timestamped turn.
fn chunk_quarter_map(rows: &[corpus_engine::index::EnrichmentChunkRow]) -> HashMap<u64, String> {
    let mut out = HashMap::new();
    for row in rows {
        if let Some(ts) = parse_turns(&row.content)
            .iter()
            .find_map(|t| t.timestamp.as_deref().and_then(parse_turn_ts))
        {
            out.insert(row.id, format!("{}-Q{}", ts.year(), (ts.month0() / 3) + 1));
        }
    }
    out
}

// ─── Pure folds ──────────────────────────────────────────────────────

pub fn fold_scale(docs: &[ConvDoc]) -> Option<ScaleCard> {
    if docs.is_empty() {
        return None;
    }
    let mut words_user = 0u64;
    let mut words_assistant = 0u64;
    let mut months: HashSet<(i32, u32)> = HashSet::new();
    let mut first: Option<NaiveDateTime> = None;
    let mut last: Option<NaiveDateTime> = None;
    let mut turn_count = 0u64;
    for d in docs {
        for t in &d.turns {
            turn_count += 1;
            if t.is_user {
                words_user += t.words;
            } else {
                words_assistant += t.words;
            }
            if let Some(ts) = t.ts {
                months.insert((ts.year(), ts.month()));
                first = Some(first.map_or(ts, |f| f.min(ts)));
                last = Some(last.map_or(ts, |l| l.max(ts)));
            }
        }
    }
    let fmt_date = |ts: Option<NaiveDateTime>| {
        ts.map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    };
    let words_total = words_user + words_assistant;
    Some(ScaleCard {
        conversations: docs.len(),
        months_active: months.len(),
        words_total,
        words_user,
        words_assistant,
        first_date: fmt_date(first),
        last_date: fmt_date(last),
        derivation: vec![
            format!("conversations = distinct source documents in the index ({})", docs.len()),
            format!(
                "words_total = Σ whitespace tokens over {turn_count} turn bodies (headers excluded) = {words_total}"
            ),
            format!(
                "months_active = distinct (year, month) over timestamped turns = {}",
                months.len()
            ),
        ],
    })
}

pub fn fold_rhythm(docs: &[ConvDoc]) -> Option<RhythmCard> {
    let mut heatmap = vec![vec![0u32; 24]; 7];
    let mut total_turns = 0u64;
    let mut any_ts = false;
    for d in docs {
        for t in &d.turns {
            total_turns += 1;
            if let Some(ts) = t.ts {
                any_ts = true;
                let wd = ts.weekday().num_days_from_monday() as usize;
                heatmap[wd][ts.hour() as usize] += 1;
            }
        }
    }
    if !any_ts {
        return None;
    }
    Some(RhythmCard {
        heatmap,
        total_turns,
        longest_session: longest_session(docs),
    })
}

/// Best maximal run of timestamped turns within one conversation with
/// inter-turn gap ≤ [`SESSION_GAP_MIN`]; ranked by duration, then turn
/// count. Excerpt = first user turn's opening line within the run.
fn longest_session(docs: &[ConvDoc]) -> Option<LongestSession> {
    let mut best: Option<LongestSession> = None;
    for d in docs {
        let stamped: Vec<&Turn> = d.turns.iter().filter(|t| t.ts.is_some()).collect();
        let mut i = 0usize;
        while i < stamped.len() {
            let mut j = i;
            while j + 1 < stamped.len() {
                let gap = stamped[j + 1].ts.unwrap() - stamped[j].ts.unwrap();
                if gap.num_minutes() > SESSION_GAP_MIN || gap.num_minutes() < 0 {
                    break;
                }
                j += 1;
            }
            let run = &stamped[i..=j];
            let start = run.first().unwrap().ts.unwrap();
            let end = run.last().unwrap().ts.unwrap();
            let duration = (end - start).num_minutes().max(0) as u32;
            let turns = run.len();
            let better = match &best {
                None => true,
                Some(b) => {
                    duration > b.duration_minutes
                        || (duration == b.duration_minutes && turns > b.turns)
                }
            };
            if better {
                let mut chunk_ids: Vec<u64> = run.iter().map(|t| t.chunk_id).collect();
                chunk_ids.dedup();
                let excerpt = run
                    .iter()
                    .find(|t| t.is_user && !t.first_line.is_empty())
                    .map(|t| Excerpt {
                        chunk_id: t.chunk_id,
                        text: excerpt_prefix(&t.first_line),
                    });
                best = Some(LongestSession {
                    conv_uuid: d.conv_uuid.clone(),
                    title: d.title.clone(),
                    date: start.format("%Y-%m-%d").to_string(),
                    duration_minutes: duration,
                    turns,
                    chunk_ids,
                    excerpt,
                });
            }
            i = j + 1;
        }
    }
    best
}

/// A prefix is still a verbatim substring; cap display length at a char
/// boundary without inventing an ellipsis inside the quoted span.
fn excerpt_prefix(line: &str) -> String {
    const MAX: usize = 240;
    if line.len() <= MAX {
        return line.to_string();
    }
    let mut end = MAX;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_string()
}

/// Entity rows that clear the deterministic quality bar AND whose
/// surface form is verbatim-present in their chunk (checked here, at
/// fold time, so the final audit is a safety net, not the filter).
/// `generic` is the corpus-evidence verdict set from
/// [`generic_keys_by_case_profile`].
fn quality_rows<'a>(
    rows: &'a [EntityRow],
    content: &HashMap<u64, String>,
    generic: &HashSet<String>,
) -> Vec<&'a EntityRow> {
    rows.iter()
        .filter(|r| r.score >= ENTITY_SCORE_FLOOR)
        .filter(|r| r.text.trim().len() >= 3)
        .filter(|r| {
            let key = r.text.trim().to_ascii_lowercase();
            !WRAPPED_ENTITY_STOPLIST.contains(&key.as_str()) && !generic.contains(&key)
        })
        .filter(|r| {
            content
                .get(&r.chunk_id)
                .is_some_and(|c| c.contains(&r.text))
        })
        .collect()
}

// ─── Case-profile generic filter ─────────────────────────────────────
// The principled replacement for an enumerated-generics stoplist: let
// the archive itself decide. A surface form the ASSISTANT's prose
// frequently writes in lowercase ("workers", "research", "the user")
// is a common noun, not a name — the assistant register capitalizes
// real names reliably, where the user's informal typing ("i love
// rust") does not, so only assistant-authored turn bodies count as
// evidence. Deterministic, data-driven, no model.

/// Case evidence for one surface form over assistant-authored prose.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CaseProfile {
    /// Word-boundary occurrences written fully lowercase.
    pub lowercase: u32,
    /// Occurrences carrying capitalization at a NON-sentence-initial
    /// position — strong proper-noun evidence. (Sentence-initial
    /// capitals are ambiguous and count for neither side.)
    pub capitalized_mid: u32,
}

impl CaseProfile {
    /// Generic = enough lowercase sightings AND lowercase dominates
    /// the decisive evidence.
    pub fn is_generic(&self) -> bool {
        let decisive = self.lowercase + self.capitalized_mid;
        self.lowercase >= CASE_PROFILE_MIN_EVIDENCE
            && decisive > 0
            && (self.lowercase as f64 / decisive as f64) >= CASE_PROFILE_GENERIC_SHARE
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// True when the match at `pos` sits at a sentence-ish start: beginning
/// of text, or preceded (across whitespace) by a terminator, a list
/// bullet, a markdown header, or an opening quote/paren.
fn at_sentence_start(orig: &[u8], pos: usize) -> bool {
    let mut i = pos;
    while i > 0 && (orig[i - 1] == b' ' || orig[i - 1] == b'\t') {
        i -= 1;
    }
    if i == 0 {
        return true;
    }
    matches!(
        orig[i - 1],
        b'.' | b'!' | b'?' | b':' | b'\n' | b'#' | b'-' | b'*' | b'"' | b'\'' | b'('
    )
}

/// Count case evidence for `key` (an ASCII-lowercased surface form)
/// across `texts`. Matching is byte-offset-aligned: the haystack is
/// ASCII-lowercased (byte-preserving) for finding, the original is read
/// at the same offsets for the case verdict.
pub fn case_profile(key: &str, texts: &[String]) -> CaseProfile {
    let mut p = CaseProfile::default();
    if key.is_empty() {
        return p;
    }
    for text in texts {
        let orig = text.as_bytes();
        let lower = text.to_ascii_lowercase();
        for (pos, _) in lower.match_indices(key) {
            let end = pos + key.len();
            // Word boundaries on both sides ("user" ≠ "userspace").
            if pos > 0 && is_word_byte(orig[pos - 1]) {
                continue;
            }
            if end < orig.len() && is_word_byte(orig[end]) {
                continue;
            }
            if &text[pos..end] == key {
                p.lowercase += 1;
            } else if !at_sentence_start(orig, pos) {
                p.capitalized_mid += 1;
            }
        }
    }
    p
}

/// The verdict pass: which of `candidates` (ASCII-lowercased keys) does
/// the assistant's own prose treat as common nouns? Returns the generic
/// set; one glassbox line summarizes what was dropped and why.
pub fn generic_keys_by_case_profile(
    candidates: &HashSet<String>,
    assistant_texts: &[String],
) -> HashSet<String> {
    let mut generic = HashSet::new();
    let mut dropped: Vec<(String, CaseProfile)> = Vec::new();
    for key in candidates {
        let p = case_profile(key, assistant_texts);
        if p.is_generic() {
            dropped.push((key.clone(), p));
            generic.insert(key.clone());
        }
    }
    if !dropped.is_empty() {
        dropped.sort_by(|a, b| b.1.lowercase.cmp(&a.1.lowercase));
        let preview: Vec<String> = dropped
            .iter()
            .take(30)
            .map(|(k, p)| format!("{k} (lc {} / cap-mid {})", p.lowercase, p.capitalized_mid))
            .collect();
        eprintln!(
            "wrapped: case-profile dropped {} generic surface forms: {}",
            dropped.len(),
            preview.join(", ")
        );
    }
    generic
}

/// Assistant-authored turn bodies — the formal-register evidence pool
/// for the case profile.
pub fn collect_assistant_text(rows: &[corpus_engine::index::EnrichmentChunkRow]) -> Vec<String> {
    let mut out = Vec::new();
    for row in rows {
        for t in parse_turns(&row.content) {
            if t.attribution == Attribution::Assistant {
                out.push(turn_body(&t.block).to_string());
            }
        }
    }
    out
}

pub fn fold_obsessions(
    rows: &[EntityRow],
    chunk_quarter: &HashMap<u64, String>,
    content: &HashMap<u64, String>,
    generic: &HashSet<String>,
) -> Option<ObsessionsCard> {
    const TOP_PER_QUARTER: usize = 5;
    let rows = quality_rows(rows, content, generic);
    if rows.is_empty() {
        return None;
    }
    // (quarter, entity key) → (display text, label, conv set, best row)
    struct Acc<'a> {
        text: String,
        label: String,
        convs: HashSet<&'a str>,
        best: &'a EntityRow,
    }
    let mut by_q: BTreeMap<String, HashMap<String, Acc>> = BTreeMap::new();
    for r in rows {
        let Some(quarter) = chunk_quarter.get(&r.chunk_id) else {
            continue;
        };
        let Some(conv) = r.conv_uuid.as_deref() else {
            continue;
        };
        let key = r.text.trim().to_ascii_lowercase();
        let acc = by_q
            .entry(quarter.clone())
            .or_default()
            .entry(key)
            .or_insert_with(|| Acc {
                text: r.text.trim().to_string(),
                label: r.label.clone(),
                convs: HashSet::new(),
                best: r,
            });
        acc.convs.insert(conv);
        if r.score > acc.best.score {
            acc.best = r;
        }
    }
    let quarters: Vec<QuarterTopics> = by_q
        .into_iter()
        .map(|(quarter, entities)| {
            let mut topics: Vec<TopicStat> = entities
                .into_values()
                .filter(|a| a.convs.len() >= MIN_TOPIC_CONVS)
                .map(|a| TopicStat {
                    conversations: a.convs.len(),
                    sample: Citation {
                        chunk_id: a.best.chunk_id,
                        char_start: a.best.char_start,
                        char_end: a.best.char_end,
                        text: a.best.text.clone(),
                    },
                    text: a.text,
                    label: a.label,
                })
                .collect();
            topics.sort_by(|a, b| {
                b.conversations
                    .cmp(&a.conversations)
                    .then_with(|| a.text.cmp(&b.text))
            });
            topics.truncate(TOP_PER_QUARTER);
            QuarterTopics { quarter, topics }
        })
        .filter(|q| !q.topics.is_empty())
        .collect();
    (!quarters.is_empty()).then_some(ObsessionsCard { quarters })
}

pub fn fold_cast(
    rows: &[EntityRow],
    content: &HashMap<u64, String>,
    generic: &HashSet<String>,
) -> Option<CastCard> {
    const CAST_LABELS: &[&str] = &["Person", "Organization", "Work"];
    const MAX_NODES: usize = 20;
    const MAX_EDGES: usize = 60;
    const MIN_CO_CONVS: usize = 2;

    struct Acc<'a> {
        text: String,
        label: String,
        convs: HashSet<&'a str>,
        best: &'a EntityRow,
    }
    let mut by_key: HashMap<String, Acc> = HashMap::new();
    for r in quality_rows(rows, content, generic) {
        if !CAST_LABELS.contains(&r.label.as_str()) {
            continue;
        }
        let Some(conv) = r.conv_uuid.as_deref() else {
            continue;
        };
        let key = r.text.trim().to_ascii_lowercase();
        let acc = by_key.entry(key).or_insert_with(|| Acc {
            text: r.text.trim().to_string(),
            label: r.label.clone(),
            convs: HashSet::new(),
            best: r,
        });
        acc.convs.insert(conv);
        if r.score > acc.best.score {
            acc.best = r;
        }
    }
    if by_key.is_empty() {
        return None;
    }
    let mut ranked: Vec<(String, Acc)> = by_key.into_iter().collect();
    ranked.sort_by(|(ka, a), (kb, b)| b.convs.len().cmp(&a.convs.len()).then_with(|| ka.cmp(kb)));
    ranked.truncate(MAX_NODES);

    let mut edges: Vec<CastEdge> = Vec::new();
    for i in 0..ranked.len() {
        for j in (i + 1)..ranked.len() {
            let shared = ranked[i].1.convs.intersection(&ranked[j].1.convs).count();
            if shared >= MIN_CO_CONVS {
                edges.push(CastEdge {
                    source: ranked[i].0.clone(),
                    target: ranked[j].0.clone(),
                    relationship_type: "appears_with".to_string(),
                    co_conversations: shared,
                });
            }
        }
    }
    edges.sort_by(|a, b| b.co_conversations.cmp(&a.co_conversations));
    edges.truncate(MAX_EDGES);

    let mut degree: HashMap<&str, usize> = HashMap::new();
    for e in &edges {
        *degree.entry(e.source.as_str()).or_default() += 1;
        *degree.entry(e.target.as_str()).or_default() += 1;
    }
    let nodes: Vec<CastNode> = ranked
        .iter()
        .map(|(key, a)| CastNode {
            id: key.clone(),
            canonical_name: a.text.clone(),
            entity_type: a.label.clone(),
            degree: degree.get(key.as_str()).copied().unwrap_or(0),
            conversations: a.convs.len(),
            sample: Citation {
                chunk_id: a.best.chunk_id,
                char_start: a.best.char_start,
                char_end: a.best.char_end,
                text: a.best.text.clone(),
            },
        })
        .collect();
    Some(CastCard { nodes, edges })
}

// ─── GLiNER entity reads ─────────────────────────────────────────────

/// Read this corpus's GLiNER rows from the sovereign state db. A
/// missing file or table degrades to no rows — the entity-backed cards
/// are simply absent; the deck still plays.
pub fn read_chunk_entities(db: &Path, corpus_id: &str) -> Result<Vec<EntityRow>, String> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("open {}: {e}", db.display()))?;
    let mut stmt = match conn.prepare(
        "SELECT chunk_id, text, label, char_start, char_end, score, conv_uuid \
         FROM chunk_entities WHERE corpus_id = ?1",
    ) {
        Ok(s) => s,
        // No chunk_entities table in this db — not an error, no cards.
        Err(rusqlite::Error::SqliteFailure(_, Some(msg))) if msg.contains("no such table") => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(format!("prepare chunk_entities query: {e}")),
    };
    let rows = stmt
        .query_map([corpus_id], |row| {
            Ok(EntityRow {
                chunk_id: row.get::<_, i64>(0)? as u64,
                text: row.get(1)?,
                label: row.get(2)?,
                char_start: row.get::<_, i64>(3)?.max(0) as usize,
                char_end: row.get::<_, i64>(4)?.max(0) as usize,
                score: row.get(5)?,
                conv_uuid: row.get(6)?,
            })
        })
        .map_err(|e| format!("query chunk_entities: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("read chunk_entities rows: {e}"))?;
    Ok(rows)
}

#[cfg(test)]
mod tests;
