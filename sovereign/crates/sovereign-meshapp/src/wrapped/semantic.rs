// SPDX-License-Identifier: AGPL-3.0-or-later
//! The enrichment- and geometry-backed folds.
//!
//! `wrapped.rs` owns the artifact, the audit and the folds that need
//! nothing but chunk text. This module owns the folds that read the two
//! richer signals already sitting on the machine:
//!
//! - **`conv_raptor_nodes`** — per-cluster LLM summaries with
//!   `primary_entities`. Measured on the reference archive these are a
//!   different class of signal from the GLiNER `chunk_entities` surface
//!   forms the v2 deck ranked: GLiNER's top-of-archive is `People (77) ·
//!   WORK (53) · Companies (46) · Research (44)`, RAPTOR's is
//!   `San Francisco (37) · Federal Reserve (33) · Taoism (13) · VIX (12) ·
//!   Wu Wei (10)`. Nouns versus a life.
//! - **chunk embeddings** — the corpus geometry, which is what makes
//!   "where did this conversation turn" and "what did you keep coming
//!   back to" answerable at all.
//!
//! The three commitments in [`super`] hold unchanged here:
//!
//! 1. **No model originates a number.** Every figure below is a fold in
//!    this file. The LLM's contribution is *nomination* — it named a
//!    cluster's entities during enrichment — never arithmetic. Each card
//!    carries a `derivation` string saying so.
//! 2. **Every quote is verbatim and cited.** RAPTOR entity names are
//!    LLM-normalised (`Dodd Frank Act` for the archive's
//!    `Dodd-Frank Act`), so they are NOT usable as citations directly.
//!    [`resolve_entity_citation`] matches punctuation-insensitively and
//!    then cites *the archive's own surface form* at the byte offsets it
//!    was found at. An entity with no verbatim occurrence in its own
//!    cluster's chunks is dropped, not paraphrased.
//! 3. **Absent data ⇒ absent card.** No RAPTOR rows, no embeddings, or
//!    too little evidence for a threshold, and the fold returns `None`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use chrono::{Datelike, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};

use super::{excerpt_prefix, Citation, ConvDoc, Excerpt, TopicStat};

// ─── Thresholds ──────────────────────────────────────────────────────

/// A theme must recur across at least this many distinct conversations
/// in a quarter before it can be called out. RAPTOR entities are
/// cluster-scope (~2 per conversation), so the bar sits lower than the
/// per-chunk GLiNER bar it replaces.
const MIN_QUARTER_CONVS: usize = 2;

/// Themes shown per quarter / per hour band.
const TOP_PER_GROUP: usize = 6;

/// A conversation needs this many chunks before a "turn" in it means
/// anything — the seam statistic is a comparison against the
/// conversation's OWN median coherence, which needs samples.
const TURN_MIN_CHUNKS: usize = 8;

/// The seam must fall at least this far below its conversation's median
/// adjacent-chunk cosine. Measured on the reference archive the seam
/// distribution is p50 = 0.736, p10 = 0.558, p01 = 0.361; a 0.15 drop
/// below a conversation's own norm is a topic change, not drift.
const TURN_MIN_DROP: f64 = 0.15;

const TURN_TOP: usize = 5;

/// Cosine between two conversation openings for them to count as the
/// same recurring question.
const RECUR_SIM: f64 = 0.64;

/// A recurring thread needs this many separate askings.
const RECUR_MIN_MEMBERS: usize = 3;

const RECUR_TOP: usize = 6;

/// An hour band needs this many theme-mentions to be characterised.
const BAND_MIN_MENTIONS: usize = 20;

const CAST_MAX_NODES: usize = 20;
const CAST_MAX_EDGES: usize = 60;
const CAST_MIN_CO_CONVS: usize = 2;

/// Hour bands, in LOCAL time. See [`infer_utc_offset`] — the archive's
/// clock is UTC and the claim these bands make ("after midnight you are
/// a different person") is false unless it is stated in the reader's
/// own time.
const BANDS: &[(&str, u32, u32)] = &[
    ("late night", 0, 5),
    ("morning", 6, 11),
    ("afternoon", 12, 17),
    ("evening", 18, 23),
];

// ─── Rows ────────────────────────────────────────────────────────────

/// One `conv_raptor_nodes` row at level 0 — a cluster of chunks within
/// one conversation, summarised during enrichment.
#[derive(Debug, Clone)]
pub struct RaptorNode {
    pub conv_uuid: String,
    pub summary: String,
    /// LLM-nominated cluster entities. Display names — see
    /// [`resolve_entity_citation`] before quoting one.
    pub entities: Vec<String>,
    pub chunk_ids: Vec<u64>,
    pub coherence: f64,
}

/// Read this corpus's level-0 RAPTOR nodes. A missing file or table
/// degrades to no rows — the enrichment-backed cards are simply absent
/// and the deck still plays, exactly as with `chunk_entities`.
pub fn read_raptor_nodes(db: &Path, corpus_id: &str) -> Result<Vec<RaptorNode>, String> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("open {}: {e}", db.display()))?;
    let mut stmt = match conn.prepare(
        "SELECT conv_uuid, summary, primary_entities, direct_member_chunk_ids, cluster_coherence \
         FROM conv_raptor_nodes WHERE corpus_id = ?1 AND level = 0",
    ) {
        Ok(s) => s,
        Err(rusqlite::Error::SqliteFailure(_, Some(msg))) if msg.contains("no such table") => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(format!("prepare conv_raptor_nodes query: {e}")),
    };
    let rows = stmt
        .query_map([corpus_id], |row| {
            let entities_json: String = row.get(2)?;
            let chunks_json: Option<String> = row.get(3)?;
            Ok(RaptorNode {
                conv_uuid: row.get(0)?,
                summary: row.get(1)?,
                entities: parse_json_strings(&entities_json),
                chunk_ids: parse_json_u64(chunks_json.as_deref().unwrap_or("[]")),
                coherence: row.get(4)?,
            })
        })
        .map_err(|e| format!("query conv_raptor_nodes: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("read conv_raptor_nodes rows: {e}"))?;
    Ok(rows)
}

/// A malformed JSON column degrades to empty rather than failing the
/// build — one bad enrichment row must not cost the whole deck.
fn parse_json_strings(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw)
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_json_u64(raw: &str) -> Vec<u64> {
    serde_json::from_str::<Vec<u64>>(raw).unwrap_or_default()
}

// ─── Cards ───────────────────────────────────────────────────────────

/// What you talk about, split by the hour of day you talk about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NightShiftCard {
    /// Offset applied to the archive's UTC turn stamps to reach the
    /// reader's clock.
    pub utc_offset_hours: i32,
    /// How that offset was arrived at — shown behind "why this?".
    pub derivation: Vec<String>,
    pub bands: Vec<HourBand>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HourBand {
    pub name: String,
    pub start_hour: u32,
    pub end_hour: u32,
    /// Theme-mentions attributed to this band — the denominator behind
    /// every `distinctiveness` in `topics`.
    pub mentions: usize,
    pub topics: Vec<TopicStat>,
}

/// The places conversations changed direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnCard {
    pub pivots: Vec<Pivot>,
    pub derivation: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pivot {
    pub conv_uuid: String,
    pub title: Option<String>,
    /// `YYYY-MM-DD` of the first turn after the seam.
    pub date: String,
    /// 1-based seam position, and how many chunks the conversation has.
    pub seam_index: usize,
    pub chunk_count: usize,
    /// Cosine at the seam, this conversation's median, and the drop.
    pub cosine: f64,
    pub conv_median: f64,
    pub drop: f64,
    /// Last thing you said before the turn, first thing after. Verbatim,
    /// audited; either may be absent when the side has no user turn.
    pub before: Option<Excerpt>,
    pub after: Option<Excerpt>,
}

/// The question asked again and again, in different words, over months.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurringCard {
    pub threads: Vec<RecurringThread>,
    pub derivation: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurringThread {
    pub conversations: usize,
    pub span_days: i64,
    /// Each asking, oldest first. Verbatim openings, audited.
    pub askings: Vec<Asking>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asking {
    pub conv_uuid: String,
    pub date: String,
    pub excerpt: Excerpt,
}

// ─── Citation resolution ─────────────────────────────────────────────

/// Byte-offset-aligned normalisation: lowercase, alphanumerics only,
/// with a parallel table mapping each normalised BYTE back to the byte
/// offset of the original char it came from. Lets an LLM-normalised
/// entity name find the archive's own punctuation and casing.
///
/// The table is indexed by byte, not by char, because `str::find`
/// returns a byte offset — indexing a per-char table with it silently
/// skews the span the moment any multibyte char precedes the match.
fn normalize_with_offsets(s: &str) -> (String, Vec<usize>) {
    let mut norm = String::with_capacity(s.len());
    let mut offsets = Vec::with_capacity(s.len());
    let mut buf = [0u8; 4];
    for (byte_idx, ch) in s.char_indices() {
        if !ch.is_alphanumeric() {
            continue;
        }
        for lower in ch.to_lowercase() {
            let encoded = lower.encode_utf8(&mut buf);
            norm.push_str(encoded);
            offsets.extend(std::iter::repeat_n(byte_idx, encoded.len()));
        }
    }
    debug_assert_eq!(norm.len(), offsets.len());
    (norm, offsets)
}

fn normalize(s: &str) -> String {
    normalize_with_offsets(s).0
}

/// Find `entity` in one of `chunk_ids` and cite THE ARCHIVE'S surface
/// form — never the LLM's normalisation. Returns `None` when the entity
/// has no verbatim occurrence in its own cluster, which is the honest
/// outcome: a theme we cannot show the reader in their own words does
/// not earn a card slot.
///
/// Measured on the reference archive: 90.2% of RAPTOR entity mentions
/// resolve against their own cluster's chunks.
pub fn resolve_entity_citation(
    entity: &str,
    chunk_ids: &[u64],
    content: &HashMap<u64, String>,
) -> Option<Citation> {
    let needle = normalize(entity);
    if needle.is_empty() {
        return None;
    }
    for &cid in chunk_ids {
        let Some(text) = content.get(&cid) else {
            continue;
        };
        // Fast path: the entity is already verbatim.
        if let Some(pos) = text.find(entity) {
            return Some(Citation {
                chunk_id: cid,
                char_start: pos,
                char_end: pos + entity.len(),
                text: entity.to_string(),
            });
        }
        let (hay, offsets) = normalize_with_offsets(text);
        let Some(hit) = hay.find(&needle) else {
            continue;
        };
        let start = offsets[hit];
        // `hit` and `needle.len()` are both byte quantities, and so is
        // the table — end is one past the last byte of the original char
        // the match's final byte came from.
        let last = offsets[hit + needle.len() - 1];
        let end = last + text[last..].chars().next().map(char::len_utf8).unwrap_or(1);
        let span = text.get(start..end)?;
        return Some(Citation {
            chunk_id: cid,
            char_start: start,
            char_end: end,
            text: span.to_string(),
        });
    }
    None
}

// ─── Distinctiveness ─────────────────────────────────────────────────

/// z-scored log-odds-ratio with an informative Dirichlet prior (Monroe,
/// Colaresi & Quinn 2008, "Fightin' Words").
///
/// This is the fold that replaces raw frequency, and the replacement is
/// the point: frequency ranks a quarter by what it is MOSTLY about,
/// which is the archive's baseline showing through and is therefore the
/// same list every quarter. Log-odds against that baseline ranks it by
/// what it is UNUSUALLY about. On the reference archive 2025-Q4 by
/// frequency is `Federal Reserve · San Francisco · China`; by
/// distinctiveness it is `VIX · Fed · Goldman Sachs · Elinor Ostrom`.
///
/// `group` and `rest` are counts over the same key space; `prior` is the
/// whole-corpus count that shapes the prior. Returns z per key.
pub fn log_odds(
    group: &HashMap<String, usize>,
    rest: &HashMap<String, usize>,
    prior: &HashMap<String, usize>,
) -> HashMap<String, f64> {
    let n_group: f64 = group.values().sum::<usize>() as f64;
    let n_rest: f64 = rest.values().sum::<usize>() as f64;
    let prior_total: f64 = prior.values().sum::<usize>().max(1) as f64;
    // Prior strength: 1% of corpus mass, floored so tiny archives still
    // get smoothing rather than divide-by-noise.
    let alpha0 = (prior_total * 0.01).max(25.0);

    let mut out = HashMap::with_capacity(prior.len());
    for (key, &prior_count) in prior {
        let alpha = alpha0 * prior_count as f64 / prior_total;
        let num_group = *group.get(key).unwrap_or(&0) as f64 + alpha;
        let num_rest = *rest.get(key).unwrap_or(&0) as f64 + alpha;
        let den_group = n_group + alpha0 - num_group;
        let den_rest = n_rest + alpha0 - num_rest;
        // Degenerate only when a theme IS the whole vocabulary: then
        // y + α = n + α₀ exactly and there is no baseline to be unusual
        // against. Drop it rather than report a fabricated score.
        if den_group <= 0.0 || den_rest <= 0.0 {
            continue;
        }
        let delta = (num_group / den_group).ln() - (num_rest / den_rest).ln();
        let var = 1.0 / num_group + 1.0 / num_rest;
        out.insert(key.clone(), delta / var.sqrt());
    }
    out
}

/// Rank `group` by distinctiveness and take the top `n`, keeping only
/// keys that clear `min_count` (a z-score over one sighting is noise).
/// Ties break on the key so the deck is byte-identical across rebuilds.
fn top_distinctive(
    group: &HashMap<String, usize>,
    rest: &HashMap<String, usize>,
    prior: &HashMap<String, usize>,
    min_count: usize,
    n: usize,
) -> Vec<(String, usize, f64)> {
    let z = log_odds(group, rest, prior);
    let mut ranked: Vec<(String, usize, f64)> = group
        .iter()
        .filter(|(_, &c)| c >= min_count)
        .filter_map(|(k, &c)| z.get(k).map(|&zv| (k.clone(), c, zv)))
        .collect();
    ranked.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked.truncate(n);
    ranked
}

// ─── Shared index over the RAPTOR layer ──────────────────────────────

/// One theme sighting — the single primitive every theme fold consumes.
///
/// Both sources reduce to this, which is the whole reason the folds
/// below are written once rather than twice: `fold_obsessions` does not
/// know or care whether a theme was nominated by the enrichment LLM or
/// tagged by GLiNER. Swapping the source swaps the quality of the deck
/// without touching a fold.
#[derive(Debug, Clone)]
pub struct ThemeMention {
    /// Normalised (lowercase, alphanumerics-only) theme key.
    pub key: String,
    pub conv_uuid: String,
    pub chunk_id: u64,
}

/// Where the themes came from. Reported in the card derivations, because
/// "who chose these words" is exactly the thing a reader should be able
/// to interrogate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeSource {
    /// `conv_raptor_nodes.primary_entities` — cluster themes nominated
    /// by the enrichment pass. Preferred.
    Enrichment,
    /// `chunk_entities` — GLiNER per-chunk NER. The fallback for a
    /// corpus that has not been through conversation enrichment.
    Ner,
}

impl ThemeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeSource::Enrichment => "conversation enrichment (cluster themes)",
            ThemeSource::Ner => "per-chunk named-entity recognition",
        }
    }
}

/// The joins every theme fold needs, computed once, over either source.
pub struct ThemeIndex {
    pub source: ThemeSource,
    pub mentions: Vec<ThemeMention>,
    /// conv_uuid → earliest timestamped turn.
    pub conv_time: HashMap<String, NaiveDateTime>,
    /// Display name keyed by its normalised form — collapses casing and
    /// punctuation drift (`Wu Wei` / `wu wei`) into one theme.
    pub display: HashMap<String, String>,
    /// Normalised theme key → distinct conversations, archive-wide.
    /// This is the Dirichlet prior every distinctiveness score is
    /// measured against.
    pub prior: HashMap<String, usize>,
    /// Normalised theme key → a resolved verbatim citation.
    pub citation: HashMap<String, Citation>,
    /// Normalised theme key → an entity-type label, where one is known.
    pub label: HashMap<String, String>,
}

/// A theme accumulating evidence during index construction.
struct ThemeAcc {
    display: String,
    label: Option<String>,
    citation: Option<Citation>,
    convs: HashSet<String>,
    mentions: Vec<ThemeMention>,
}

impl ThemeIndex {
    /// Preferred source: the enrichment layer's cluster themes.
    pub fn from_enrichment(
        nodes: &[RaptorNode],
        docs: &[ConvDoc],
        content: &HashMap<u64, String>,
        labels: &HashMap<String, String>,
    ) -> Self {
        let mut acc: HashMap<String, ThemeAcc> = HashMap::new();
        for node in nodes {
            for entity in &node.entities {
                let key = normalize(entity);
                if key.len() < 3 {
                    continue;
                }
                let entry = acc.entry(key.clone()).or_insert_with(|| ThemeAcc {
                    display: entity.clone(),
                    label: labels.get(&key).cloned(),
                    citation: None,
                    convs: HashSet::new(),
                    mentions: Vec::new(),
                });
                if entry.citation.is_none() {
                    entry.citation = resolve_entity_citation(entity, &node.chunk_ids, content);
                }
                entry.convs.insert(node.conv_uuid.clone());
                for &chunk_id in &node.chunk_ids {
                    entry.mentions.push(ThemeMention {
                        key: key.clone(),
                        conv_uuid: node.conv_uuid.clone(),
                        chunk_id,
                    });
                }
            }
        }
        Self::finish(ThemeSource::Enrichment, acc, docs)
    }

    /// Fallback source: GLiNER rows that already cleared the
    /// deterministic quality bar (score floor, stoplist, and the
    /// corpus-evidence case-profile verdict — see
    /// [`super::generic_keys_by_case_profile`]). A corpus that has not
    /// been through conversation enrichment still gets a deck; it just
    /// gets a shallower one, and the card says so.
    pub fn from_ner(
        rows: &[&super::EntityRow],
        docs: &[ConvDoc],
        content: &HashMap<u64, String>,
    ) -> Self {
        let mut acc: HashMap<String, ThemeAcc> = HashMap::new();
        for r in rows {
            let Some(conv_uuid) = r.conv_uuid.clone() else {
                continue;
            };
            let surface = r.text.trim();
            let key = normalize(surface);
            if key.len() < 3 {
                continue;
            }
            let entry = acc.entry(key.clone()).or_insert_with(|| ThemeAcc {
                display: surface.to_string(),
                label: Some(r.label.clone()),
                citation: None,
                convs: HashSet::new(),
                mentions: Vec::new(),
            });
            // quality_rows already proved this span verbatim in its own
            // chunk, so the citation is the row itself.
            if entry.citation.is_none() && content.contains_key(&r.chunk_id) {
                entry.citation = Some(Citation {
                    chunk_id: r.chunk_id,
                    char_start: r.char_start,
                    char_end: r.char_end,
                    text: surface.to_string(),
                });
            }
            entry.convs.insert(conv_uuid.clone());
            entry.mentions.push(ThemeMention {
                key,
                conv_uuid,
                chunk_id: r.chunk_id,
            });
        }
        Self::finish(ThemeSource::Ner, acc, docs)
    }

    /// Drop every theme we cannot quote, then flatten. A theme we cannot
    /// show the reader in their own words does not earn a card slot —
    /// this is where commitment 2 is actually enforced, upstream of the
    /// folds, so no fold can accidentally emit an unciteable claim.
    fn finish(source: ThemeSource, acc: HashMap<String, ThemeAcc>, docs: &[ConvDoc]) -> Self {
        let mut conv_time = HashMap::new();
        for d in docs {
            if let Some(ts) = d.turns.iter().filter_map(|t| t.ts).min() {
                conv_time.insert(d.conv_uuid.clone(), ts);
            }
        }

        let mut mentions = Vec::new();
        let mut display = HashMap::new();
        let mut prior = HashMap::new();
        let mut citation = HashMap::new();
        let mut label = HashMap::new();
        for (key, theme) in acc {
            let Some(cite) = theme.citation else {
                continue;
            };
            prior.insert(key.clone(), theme.convs.len());
            display.insert(key.clone(), theme.display);
            citation.insert(key.clone(), cite);
            if let Some(l) = theme.label {
                label.insert(key.clone(), l);
            }
            mentions.extend(theme.mentions);
        }
        // Stable order in ⇒ byte-identical deck out across rebuilds.
        mentions.sort_by(|a, b| {
            a.key
                .cmp(&b.key)
                .then_with(|| a.conv_uuid.cmp(&b.conv_uuid))
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });

        ThemeIndex {
            source,
            mentions,
            conv_time,
            display,
            prior,
            citation,
            label,
        }
    }

    fn topic_stat(
        &self,
        key: &str,
        conversations: usize,
        distinctiveness: f64,
    ) -> Option<TopicStat> {
        Some(TopicStat {
            text: self.display.get(key)?.clone(),
            label: self
                .label
                .get(key)
                .cloned()
                .unwrap_or_else(|| "Theme".to_string()),
            conversations,
            distinctiveness,
            sample: self.citation.get(key)?.clone(),
        })
    }

    /// Distinct conversations per theme, archive-wide.
    fn theme_convs(&self) -> HashMap<&str, HashSet<&str>> {
        let mut out: HashMap<&str, HashSet<&str>> = HashMap::new();
        for m in &self.mentions {
            out.entry(m.key.as_str())
                .or_default()
                .insert(m.conv_uuid.as_str());
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.prior.is_empty()
    }
}

/// Lowercase-normalised surface form → GLiNER's most frequent label for
/// it. Types cast nodes without letting GLiNER pick who is on the card.
pub fn gliner_label_map(rows: &[super::EntityRow]) -> HashMap<String, String> {
    let mut counts: HashMap<String, HashMap<&str, usize>> = HashMap::new();
    for r in rows {
        if r.score < super::ENTITY_SCORE_FLOOR {
            continue;
        }
        *counts
            .entry(normalize(&r.text))
            .or_default()
            .entry(r.label.as_str())
            .or_default() += 1;
    }
    counts
        .into_iter()
        .filter_map(|(k, labels)| {
            labels
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(l, _)| (k, l.to_string()))
        })
        .collect()
}

// ─── Fold: obsessions, by distinctiveness ────────────────────────────

/// What each quarter was UNUSUALLY about.
///
/// The v2 fold ranked each quarter's themes by how many conversations
/// mentioned them, which ranks the archive's own baseline and therefore
/// produces roughly the same list every quarter — the reason the card
/// read as topical. Ranking by log-odds against that baseline instead
/// turns "you talked about the Federal Reserve a lot" into "this was the
/// quarter of VIX, Goldman Sachs and Elinor Ostrom".
pub fn fold_obsessions(idx: &ThemeIndex) -> Option<super::ObsessionsCard> {
    if idx.is_empty() {
        return None;
    }
    let mut by_quarter: BTreeMap<String, HashMap<String, HashSet<&str>>> = BTreeMap::new();
    for m in &idx.mentions {
        let Some(ts) = idx.conv_time.get(&m.conv_uuid) else {
            continue;
        };
        let quarter = format!("{}-Q{}", ts.year(), (ts.month0() / 3) + 1);
        by_quarter
            .entry(quarter)
            .or_default()
            .entry(m.key.clone())
            .or_default()
            .insert(m.conv_uuid.as_str());
    }

    let quarters: Vec<super::QuarterTopics> = by_quarter
        .into_iter()
        .filter_map(|(quarter, entities)| {
            let group: HashMap<String, usize> =
                entities.iter().map(|(k, v)| (k.clone(), v.len())).collect();
            let rest: HashMap<String, usize> = idx
                .prior
                .iter()
                .map(|(k, &total)| (k.clone(), total.saturating_sub(*group.get(k).unwrap_or(&0))))
                .collect();
            let topics: Vec<TopicStat> =
                top_distinctive(&group, &rest, &idx.prior, MIN_QUARTER_CONVS, TOP_PER_GROUP)
                    .into_iter()
                    .filter_map(|(key, count, z)| idx.topic_stat(&key, count, z))
                    .collect();
            (!topics.is_empty()).then_some(super::QuarterTopics { quarter, topics })
        })
        .collect();

    (!quarters.is_empty()).then_some(super::ObsessionsCard {
        quarters,
        derivation: vec![
            format!(
                "themes from {} — {} of them appear in ≥ 1 conversation and are quotable verbatim",
                idx.source.as_str(),
                idx.prior.len()
            ),
            format!(
                "ranked by z-scored log-odds against the whole-archive baseline, NOT by count; a theme needs ≥ {MIN_QUARTER_CONVS} conversations in the quarter"
            ),
            "count shown beside each theme is distinct conversations in that quarter".to_string(),
        ],
    })
}

// ─── Fold: the night shift ───────────────────────────────────────────

/// The archive stamps turns in UTC. This card's claim is about the
/// reader's own night, so the offset has to come from somewhere.
///
/// It comes from sleep: find the 4-hour window with the fewest user
/// turns and call its centre 03:00 local. On the reference archive the
/// user-turn histogram has a hard floor at UTC 08–11 (0, 1, 5, 18 turns
/// against a 200+ peak) and the inference returns UTC−7 — which is
/// correct, and which the naive UTC reading got backwards, labelling
/// 17:00 local as "late night".
///
/// `os_offset` wins when the host supplies one; this is the fallback.
pub fn infer_utc_offset(docs: &[ConvDoc], os_offset: Option<i32>) -> (i32, Vec<String>) {
    if let Some(off) = os_offset {
        return (
            off,
            vec![format!(
                "local clock = UTC{off:+} (supplied by the host OS)"
            )],
        );
    }
    let mut hours = [0u64; 24];
    let mut total = 0u64;
    for d in docs {
        for t in &d.turns {
            if t.is_user {
                if let Some(ts) = t.ts {
                    hours[ts.hour() as usize] += 1;
                    total += 1;
                }
            }
        }
    }
    if total == 0 {
        return (
            0,
            vec!["no timestamped user turns — assuming UTC".to_string()],
        );
    }
    let (start, count) = (0..24u32)
        .map(|s| {
            let c: u64 = (0..4).map(|k| hours[((s + k) % 24) as usize]).sum();
            (s, c)
        })
        .min_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        .expect("24 candidate windows");
    let centre = (start + 2) % 24;
    // Put the trough's centre at 03:00 local, expressed in (-12, 12].
    let mut offset = 3i32 - centre as i32;
    while offset <= -12 {
        offset += 24;
    }
    while offset > 12 {
        offset -= 24;
    }
    (
        offset,
        vec![
            format!(
                "quietest 4h of user turns = UTC {start:02}:00–{:02}:59 ({count} of {total} turns)",
                (start + 3) % 24
            ),
            format!("its centre placed at 03:00 local ⇒ local clock = UTC{offset:+}"),
            "every hour this deck shows you is local; the archive itself stamps UTC".to_string(),
        ],
    )
}

/// The archive's local clock — inferred once and handed to every card
/// that puts an hour in front of the reader.
///
/// It exists because two cards deriving the clock independently is two
/// chances to disagree on the same slide deck. Before it, the Rhythm
/// heatmap bucketed in UTC while the Night Shift bands were labelled
/// UTC−7, so the reference archive's peak read as 20:00 on one card and
/// 13:00 on the other — the same person, seven hours apart.
#[derive(Debug, Clone)]
pub struct LocalClock {
    pub offset_hours: i32,
    /// How the offset was arrived at — verbatim into the cards' own
    /// `derivation`, so the reader can audit the clock like any figure.
    pub derivation: Vec<String>,
}

impl LocalClock {
    pub fn infer(docs: &[ConvDoc], os_offset: Option<i32>) -> Self {
        let (offset_hours, derivation) = infer_utc_offset(docs, os_offset);
        Self {
            offset_hours,
            derivation,
        }
    }

    /// A UTC stamp read on the archive-owner's own wall clock. Shifts the
    /// whole datetime, not just the hour: a 23:00 UTC turn at UTC−7 is
    /// the PREVIOUS day at 16:00, and a heatmap keyed by weekday has to
    /// move it there too.
    pub fn local(&self, ts: NaiveDateTime) -> NaiveDateTime {
        ts + chrono::Duration::hours(self.offset_hours as i64)
    }
}

pub fn fold_night_shift(
    idx: &ThemeIndex,
    docs: &[ConvDoc],
    clock: &LocalClock,
) -> Option<NightShiftCard> {
    if idx.is_empty() {
        return None;
    }
    let (offset, derivation) = (clock.offset_hours, clock.derivation.clone());

    // chunk id → local hour of its first user turn.
    let mut chunk_hour: HashMap<u64, u32> = HashMap::new();
    for d in docs {
        for t in &d.turns {
            if !t.is_user {
                continue;
            }
            if let Some(ts) = t.ts {
                let local = (ts.hour() as i32 + offset).rem_euclid(24) as u32;
                chunk_hour.entry(t.chunk_id).or_insert(local);
            }
        }
    }

    let band_of = |hour: u32| -> Option<usize> {
        BANDS
            .iter()
            .position(|&(_, lo, hi)| hour >= lo && hour <= hi)
    };

    let mut per_band: Vec<HashMap<String, usize>> = vec![HashMap::new(); BANDS.len()];
    let mut overall: HashMap<String, usize> = HashMap::new();
    for m in &idx.mentions {
        let Some(band) = chunk_hour.get(&m.chunk_id).copied().and_then(band_of) else {
            continue;
        };
        *per_band[band].entry(m.key.clone()).or_default() += 1;
        *overall.entry(m.key.clone()).or_default() += 1;
    }

    let bands: Vec<HourBand> = BANDS
        .iter()
        .enumerate()
        .filter_map(|(i, &(name, lo, hi))| {
            let group = &per_band[i];
            let mentions: usize = group.values().sum();
            if mentions < BAND_MIN_MENTIONS {
                return None;
            }
            let rest: HashMap<String, usize> = overall
                .iter()
                .map(|(k, &t)| (k.clone(), t.saturating_sub(*group.get(k).unwrap_or(&0))))
                .collect();
            let topics: Vec<TopicStat> = top_distinctive(group, &rest, &overall, 2, TOP_PER_GROUP)
                .into_iter()
                .filter_map(|(key, count, z)| idx.topic_stat(&key, count, z))
                .collect();
            (!topics.is_empty()).then_some(HourBand {
                name: name.to_string(),
                start_hour: lo,
                end_hour: hi,
                mentions,
                topics,
            })
        })
        .collect();

    (bands.len() >= 2).then_some(NightShiftCard {
        utc_offset_hours: offset,
        derivation,
        bands,
    })
}

// ─── Fold: the turn ──────────────────────────────────────────────────

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let mut dot = 0f64;
    let mut na = 0f64;
    let mut nb = 0f64;
    for (x, y) in a.iter().zip(b) {
        dot += *x as f64 * *y as f64;
        na += *x as f64 * *x as f64;
        nb += *y as f64 * *y as f64;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

/// A verbatim, audited excerpt from one turn's opening line.
fn excerpt_of(turn: &super::Turn, content: &HashMap<u64, String>) -> Option<Excerpt> {
    let text = excerpt_prefix(turn.first_line.trim());
    if text.is_empty() {
        return None;
    }
    content
        .get(&turn.chunk_id)
        .filter(|c| c.contains(&text))
        .map(|_| Excerpt {
            chunk_id: turn.chunk_id,
            text,
        })
}

/// Where conversations changed direction.
///
/// Scored as the drop below the conversation's OWN median adjacent-chunk
/// cosine — not an archive-wide threshold, because conversations differ
/// enormously in how tightly they hold a subject, and not a MAD-scaled
/// z-score, which collapses on short conversations and ranks
/// continuations rather than pivots.
pub fn fold_turn(
    docs: &[ConvDoc],
    embeddings: &HashMap<u64, Vec<f32>>,
    content: &HashMap<u64, String>,
) -> Option<TurnCard> {
    if embeddings.is_empty() {
        return None;
    }
    let mut pivots: Vec<Pivot> = Vec::new();
    let mut seams_examined = 0usize;
    let mut eligible = 0usize;
    let mut examined = 0usize;

    for d in docs {
        // The conversation's SHAPE comes from its chunks, not its parsed
        // turns: on this archive most chunks carry no `### [ts] role`
        // header, so a turn-derived sequence sees a tenth of the seams
        // that exist. Turns are consulted below, for quotes only.
        //
        // A chunk with no embedding drops out rather than disqualifying
        // its whole conversation — the seam it would have formed simply
        // spans to the next embedded chunk. Ids and vectors are unzipped
        // from ONE pass so `seq[i]` and `vectors[i]` cannot drift apart;
        // every index below leans on that.
        let (seq, vectors): (Vec<u64>, Vec<&Vec<f32>>) = d
            .chunk_ids
            .iter()
            .filter_map(|c| embeddings.get(c).map(|v| (*c, v)))
            .unzip();
        if d.chunk_ids.len() >= TURN_MIN_CHUNKS {
            eligible += 1;
        }
        if seq.len() < TURN_MIN_CHUNKS {
            continue;
        }
        examined += 1;
        let adjacent: Vec<f64> = vectors.windows(2).map(|w| cosine(w[0], w[1])).collect();
        seams_examined += adjacent.len();
        let med = median(adjacent.clone());
        let Some((k, &low)) = adjacent
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        else {
            continue;
        };
        let drop = med - low;
        if drop < TURN_MIN_DROP {
            continue;
        }

        // Chunk ids ascend, so the seam is the span [seq[k], seq[k+1]]:
        // quote the last user turn at or before its left edge and the
        // first at or after its right edge.
        //
        // Neither search is bounded by chunk distance, and it is worth
        // being precise about why, because the obvious worry — that an
        // unparsed stretch lets "the last thing you said" come from half
        // a conversation away — turns out not to exist. A chunk fails to
        // parse EXACTLY when it holds no `### [ts] role` header, i.e.
        // when it is a mid-turn continuation fragment; sampled 2026-07-26
        // they begin mid-sentence, inside a long assistant answer. So an
        // unparsed chunk cannot hide a user turn, and the nearest user
        // turn walking outward IS the last/first one — no matter how many
        // chunks of answer sit in between. Measured over the reference
        // archive's 388 qualified seams, the back-side turn sits 3 chunks
        // away at the mode and 0 chunks away only 0.5% of the time; a
        // 3-chunk window would have discarded a correct quote on 70% of
        // them.
        let (seam_lhs, seam_rhs) = (seq[k], seq[k + 1]);

        let before = d
            .turns
            .iter()
            .filter(|t| t.is_user && t.chunk_id <= seam_lhs)
            .next_back()
            .and_then(|t| excerpt_of(t, content));
        let after = d
            .turns
            .iter()
            .find(|t| t.is_user && t.chunk_id >= seam_rhs)
            .and_then(|t| excerpt_of(t, content));
        if before.is_none() && after.is_none() {
            continue;
        }
        // Date the seam from the nearest timestamped turn, preferring the
        // side the reader lands on. No timestamped turn ⇒ no date claim.
        let date = d
            .turns
            .iter()
            .find(|t| t.chunk_id >= seam_rhs && t.ts.is_some())
            .or_else(|| {
                d.turns
                    .iter()
                    .filter(|t| t.chunk_id <= seam_lhs && t.ts.is_some())
                    .next_back()
            })
            .and_then(|t| t.ts)
            .map(|ts| ts.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        pivots.push(Pivot {
            conv_uuid: d.conv_uuid.clone(),
            title: d.title.clone(),
            date,
            seam_index: k + 1,
            chunk_count: seq.len(),
            cosine: low,
            conv_median: med,
            drop,
            before,
            after,
        });
    }

    pivots.sort_by(|a, b| {
        b.drop
            .partial_cmp(&a.drop)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.conv_uuid.cmp(&b.conv_uuid))
    });
    let considered = pivots.len();
    pivots.truncate(TURN_TOP);

    (!pivots.is_empty()).then_some(TurnCard {
        pivots,
        derivation: vec![
            format!(
                "{seams_examined} chunk-to-chunk seams examined across {examined} of {eligible} conversations of ≥ {TURN_MIN_CHUNKS} chunks"
            ),
            format!(
                "a seam counts when its cosine falls ≥ {TURN_MIN_DROP} below its own conversation's median ({considered} qualified)"
            ),
            "quotes are the last thing you said before the seam and the first after".to_string(),
        ],
    })
}

// ─── Fold: the question you keep asking ──────────────────────────────

/// Recurrence × time-spread over conversation openings.
///
/// Ranked by `members × ln(1 + span_days)` rather than by member count:
/// the reveal is not "you asked this a lot", which is a word count, but
/// "you asked this again fourteen months later, in different words",
/// which is a fact about a person.
pub fn fold_recurring(
    docs: &[ConvDoc],
    embeddings: &HashMap<u64, Vec<f32>>,
    content: &HashMap<u64, String>,
) -> Option<RecurringCard> {
    if embeddings.is_empty() {
        return None;
    }
    struct Opening<'a> {
        doc: &'a ConvDoc,
        ts: NaiveDateTime,
        excerpt: Excerpt,
        vector: &'a Vec<f32>,
    }

    let mut openings: Vec<Opening> = Vec::new();
    for d in docs {
        let Some(turn) = d.turns.iter().find(|t| t.is_user) else {
            continue;
        };
        let (Some(ts), Some(excerpt)) = (turn.ts, excerpt_of(turn, content)) else {
            continue;
        };
        let Some(vector) = embeddings.get(&turn.chunk_id) else {
            continue;
        };
        openings.push(Opening {
            doc: d,
            ts,
            excerpt,
            vector,
        });
    }
    if openings.len() < RECUR_MIN_MEMBERS {
        return None;
    }
    // Deterministic order in, deterministic clusters out.
    openings.sort_by(|a, b| {
        a.ts.cmp(&b.ts)
            .then_with(|| a.doc.conv_uuid.cmp(&b.doc.conv_uuid))
    });

    let n = openings.len();
    let mut similar: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        for j in (i + 1)..n {
            if cosine(openings[i].vector, openings[j].vector) >= RECUR_SIM {
                similar[i].push(j);
                similar[j].push(i);
            }
        }
    }

    // Greedy: repeatedly take the densest unassigned seed and claim its
    // neighbourhood. Ties break on index, so the deck is reproducible.
    let mut assigned = vec![false; n];
    let mut threads: Vec<RecurringThread> = Vec::new();
    loop {
        let seed = (0..n).filter(|&i| !assigned[i]).max_by_key(|&i| {
            (
                similar[i].iter().filter(|&&j| !assigned[j]).count(),
                std::cmp::Reverse(i),
            )
        });
        let Some(seed) = seed else { break };
        let mut members: Vec<usize> = similar[seed]
            .iter()
            .copied()
            .filter(|&j| !assigned[j])
            .collect();
        members.push(seed);
        members.sort_unstable();
        if members.len() < RECUR_MIN_MEMBERS {
            assigned[seed] = true;
            continue;
        }
        for &m in &members {
            assigned[m] = true;
        }
        let first = openings[*members.first().expect("non-empty")].ts;
        let last = openings[*members.last().expect("non-empty")].ts;
        threads.push(RecurringThread {
            conversations: members.len(),
            span_days: (last - first).num_days().max(0),
            askings: members
                .iter()
                .map(|&m| Asking {
                    conv_uuid: openings[m].doc.conv_uuid.clone(),
                    date: openings[m].ts.format("%Y-%m-%d").to_string(),
                    excerpt: openings[m].excerpt.clone(),
                })
                .collect(),
        });
    }

    let score = |t: &RecurringThread| t.conversations as f64 * (1.0 + t.span_days as f64).ln();
    threads.sort_by(|a, b| {
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.conversations.cmp(&a.conversations))
    });
    let found = threads.len();
    threads.truncate(RECUR_TOP);

    (!threads.is_empty()).then_some(RecurringCard {
        threads,
        derivation: vec![
            format!("{n} conversation openings compared pairwise by embedding cosine"),
            format!(
                "openings within {RECUR_SIM} cosine group into one thread; {RECUR_MIN_MEMBERS}+ askings qualify ({found} threads)"
            ),
            "ranked by askings × ln(1 + days spanned) — returning matters more than repeating"
                .to_string(),
        ],
    })
}

// ─── Fold: the cast, rebuilt ─────────────────────────────────────────

/// The recurring cast, and what actually links them.
///
/// The v2 graph linked any two entities sharing ≥ 2 conversations and
/// labelled every edge `appears_with`. Over a few hundred conversations
/// that graph is near-complete, and a near-complete graph carries no
/// information — which is exactly why the card read as topical.
///
/// This one keeps an edge only where the pair co-occurs MORE than chance
/// would predict (positive pointwise mutual information), and sizes
/// nodes by betweenness — in an archive the interesting names are the
/// ones bridging separate concerns, not the most frequent ones.
pub fn fold_cast(idx: &ThemeIndex) -> Option<super::CastCard> {
    if idx.is_empty() {
        return None;
    }
    let total_convs = idx.conv_time.len().max(1) as f64;

    let mut ranked: Vec<(String, HashSet<&str>)> = idx
        .theme_convs()
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    ranked.sort_by(|(ka, a), (kb, b)| b.len().cmp(&a.len()).then_with(|| ka.cmp(kb)));
    ranked.truncate(CAST_MAX_NODES);
    if ranked.len() < 2 {
        return None;
    }

    let mut edges: Vec<super::CastEdge> = Vec::new();
    for i in 0..ranked.len() {
        for j in (i + 1)..ranked.len() {
            let shared = ranked[i].1.intersection(&ranked[j].1).count();
            if shared < CAST_MIN_CO_CONVS {
                continue;
            }
            // PMI: log( P(a,b) / (P(a)·P(b)) ). Positive means the pair
            // shows up together more than two independent themes would.
            let p_ab = shared as f64 / total_convs;
            let p_a = ranked[i].1.len() as f64 / total_convs;
            let p_b = ranked[j].1.len() as f64 / total_convs;
            let pmi = (p_ab / (p_a * p_b)).ln();
            if pmi <= 0.0 {
                continue;
            }
            let mut dates: Vec<NaiveDateTime> = ranked[i]
                .1
                .intersection(&ranked[j].1)
                .filter_map(|c| idx.conv_time.get(*c).copied())
                .collect();
            dates.sort_unstable();
            edges.push(super::CastEdge {
                source: ranked[i].0.clone(),
                target: ranked[j].0.clone(),
                co_conversations: shared,
                pmi,
                first_date: dates
                    .first()
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                last_date: dates
                    .last()
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
            });
        }
    }
    edges.sort_by(|a, b| {
        b.pmi
            .partial_cmp(&a.pmi)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.co_conversations.cmp(&a.co_conversations))
            .then_with(|| a.source.cmp(&b.source))
    });
    edges.truncate(CAST_MAX_EDGES);

    let ids: Vec<&str> = ranked.iter().map(|(k, _)| k.as_str()).collect();
    let betweenness = betweenness(&ids, &edges);
    let mut degree: HashMap<&str, usize> = HashMap::new();
    for e in &edges {
        *degree.entry(e.source.as_str()).or_default() += 1;
        *degree.entry(e.target.as_str()).or_default() += 1;
    }

    let nodes: Vec<super::CastNode> = ranked
        .iter()
        .filter_map(|(key, cs)| {
            let mut dates: Vec<NaiveDateTime> = cs
                .iter()
                .filter_map(|c| idx.conv_time.get(*c).copied())
                .collect();
            dates.sort_unstable();
            Some(super::CastNode {
                id: key.clone(),
                canonical_name: idx.display.get(key)?.clone(),
                entity_type: idx
                    .label
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| "Theme".to_string()),
                degree: degree.get(key.as_str()).copied().unwrap_or(0),
                bridging: betweenness.get(key.as_str()).copied().unwrap_or(0.0),
                conversations: cs.len(),
                first_date: dates
                    .first()
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                last_date: dates
                    .last()
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                sample: idx.citation.get(key)?.clone(),
            })
        })
        .collect();

    let kept = edges.len();
    (nodes.len() >= 2).then_some(super::CastCard {
        derivation: vec![
            format!(
                "the {} themes appearing in the most conversations, from {}",
                nodes.len(),
                idx.source.as_str()
            ),
            format!(
                "a link needs ≥ {CAST_MIN_CO_CONVS} shared conversations AND positive pointwise mutual information — co-occurring more than two independent themes would ({kept} links kept)"
            ),
            "node size is betweenness, not frequency — who connects otherwise-separate concerns"
                .to_string(),
        ],
        nodes,
        edges,
    })
}

/// Brandes betweenness on the unweighted PMI graph, normalised to
/// [0, 1]. The graph is capped at [`CAST_MAX_NODES`], so the O(VE) cost
/// is trivial and the exact answer beats a heuristic.
fn betweenness(ids: &[&str], edges: &[super::CastEdge]) -> HashMap<String, f64> {
    let index: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let n = ids.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        if let (Some(&a), Some(&b)) = (index.get(e.source.as_str()), index.get(e.target.as_str())) {
            adj[a].push(b);
            adj[b].push(a);
        }
    }
    let mut score = vec![0f64; n];
    for s in 0..n {
        let mut stack: Vec<usize> = Vec::new();
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut sigma = vec![0f64; n];
        let mut dist = vec![-1i64; n];
        sigma[s] = 1.0;
        dist[s] = 0;
        let mut queue = std::collections::VecDeque::from([s]);
        while let Some(v) = queue.pop_front() {
            stack.push(v);
            for &w in &adj[v] {
                if dist[w] < 0 {
                    dist[w] = dist[v] + 1;
                    queue.push_back(w);
                }
                if dist[w] == dist[v] + 1 {
                    sigma[w] += sigma[v];
                    preds[w].push(v);
                }
            }
        }
        let mut delta = vec![0f64; n];
        while let Some(w) = stack.pop() {
            for &v in &preds[w] {
                if sigma[w] > 0.0 {
                    delta[v] += sigma[v] / sigma[w] * (1.0 + delta[w]);
                }
            }
            if w != s {
                score[w] += delta[w];
            }
        }
    }
    // Undirected: each pair counted twice. Normalise by the max possible.
    let scale = if n > 2 {
        2.0 / ((n - 1) * (n - 2)) as f64
    } else {
        1.0
    };
    ids.iter()
        .enumerate()
        .map(|(i, id)| (id.to_string(), (score[i] * scale).clamp(0.0, 1.0)))
        .collect()
}

#[cfg(test)]
mod tests;
