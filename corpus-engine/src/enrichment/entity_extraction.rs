//! Phase 1b — entity extraction for the personal + conversational
//! domains.
//!
//! Produces typed `Entity` atoms (Person / Organization / Initiative)
//! and `Involves` edges that link each entity to the chunks where it
//! appears. The chunk_id on each Involves edge is the join key the
//! KnowledgeView timeline assembler uses to recover the source row's
//! `created_at` timestamp.
//!
//! Opt-in per domain via `Domain::entity_extraction_prompt`. When the
//! domain returns `None`, this whole step is a no-op — no inference
//! calls, no atoms.json write. Domains that opt in produce JSON in
//! the shape described by [`EntityExtractionResponse`].
//!
//! Cross-batch entity merging is by normalized canonical name (lower-
//! case, trim). When two batches reference the same name, the entity
//! gets a single atom; the first batch's mention determines
//! `first_appearance`, subsequent mentions accumulate as Involves
//! edges. Ambiguous cases (a partial-name reference resolvable to
//! more than one prior entity) are recorded as
//! `FailureKind::EntityMergeAmbiguous` per the existing atlas
//! `PhaseFailureKind` vocabulary.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::index::StoredChunk;
use crate::types::InferenceFn;

use super::atlas::atoms::{AtomId, ChunkRef, Entity};
use super::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use super::atlas::writer::write_atlas;
use super::clustering::EnrichmentProgress;
use super::domain::Domain;
use super::pipeline::atlas::{EnrichmentDepth, EntityType};

/// Concurrency for the batch driver. Matches skeleton extraction
/// (`field_engine.rs::CONCURRENCY = 4`) so a slow inference back-end
/// caps both phases at the same in-flight count.
const CONCURRENCY: usize = 4;

/// Number of chunks per inference batch. Same as skeleton extraction.
const BATCH_SIZE: usize = 4;

/// Salience floor for synthesised entities. The personal /
/// conversational corpora are small enough that frequency-based
/// salience adjustment isn't load-bearing — a single mention is
/// enough to surface in the digest. Held high so the digest ranking
/// algorithm gives entity timelines fair weight against canonical
/// questions.
const DEFAULT_SALIENCE: f32 = 0.7;

// ── Response schema ─────────────────────────────────────────────

/// Top-level JSON the model returns per batch. Lenient parsing —
/// missing arrays default to empty so a batch that found nothing
/// returns a well-formed `{}` rather than an error.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct EntityExtractionResponse {
    #[serde(default)]
    pub persons: Vec<PersonEntity>,
    #[serde(default)]
    pub organizations: Vec<OrganizationEntity>,
    #[serde(default)]
    pub initiatives: Vec<InitiativeEntity>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PersonEntity {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affiliation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Chunk ids the person was mentioned in within this batch.
    /// Each becomes one Involves edge.
    #[serde(default, deserialize_with = "deserialize_lenient_string_array")]
    pub mentions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrganizationEntity {
    pub name: String,
    /// Free-text relationship to the user — "client", "employer",
    /// "vendor", "partner". Surfaced descriptively in the digest;
    /// the model is not constrained to a fixed taxonomy per
    /// requirements §3.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_string_array")]
    pub mentions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InitiativeEntity {
    pub name: String,
    /// Current framing ("in negotiation", "team aligned", "stalled").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Person / organization names involved. Resolved to AtomIds at
    /// merge time; orphans (names not seen as Person/Organization
    /// entities in this run) are dropped from the participants list
    /// and counted in `failures` as `OrphanParticipant`.
    ///
    /// The on-the-wire shape is permissive: the model often emits
    /// participants as `["Mike Torres"]` (matching the prompt's
    /// example) but sometimes promotes the array entries to objects
    /// like `[{"name": "Mike Torres"}]`. We accept either via a
    /// custom deserializer; the rest of the pipeline still sees a
    /// flat `Vec<String>` of names.
    #[serde(default, deserialize_with = "deserialize_participants")]
    pub participants: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_string_array")]
    pub mentions: Vec<String>,
}

// ── Result + failure shape ──────────────────────────────────────

/// What a single end-to-end run produces. The caller writes
/// `entities` + `edges` via `write_atlas` and surfaces `failures`
/// to the operator (the existing `sovereign enrich errors`
/// aggregator already handles unknown-kind failures gracefully).
#[derive(Debug, Default)]
pub struct EntityExtractionResult {
    pub entities: Vec<Entity>,
    pub edges: Vec<Edge>,
    pub failures: Vec<EntityExtractionFailure>,
    pub batches_run: usize,
}

#[derive(Debug, Clone)]
pub struct EntityExtractionFailure {
    pub batch_index: usize,
    pub kind: FailureKind,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    InferenceError,
    ParseError,
    EntityMergeAmbiguous,
    OrphanParticipant,
}

// ── Public entry point ──────────────────────────────────────────

/// Run the entity-extraction step over `chunks` for `domain`. When
/// the domain doesn't override `entity_extraction_prompt`, the call
/// short-circuits to an empty result — the caller can treat this as
/// "phase ran, found nothing" and continue.
pub async fn run_entity_extraction(
    chunks: &[StoredChunk],
    domain: &dyn Domain,
    inference: InferenceFn,
    progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
) -> Result<EntityExtractionResult> {
    if chunks.is_empty() {
        return Ok(EntityExtractionResult::default());
    }

    // Probe: does this domain opt in? Uses an empty-slice probe so
    // we never burn an inference call before knowing.
    if domain.entity_extraction_prompt(&[]).is_none() {
        tracing::debug!(
            domain = domain.id(),
            "entity_extraction: domain opted out, skipping"
        );
        return Ok(EntityExtractionResult::default());
    }

    let batches: Vec<&[StoredChunk]> = chunks.chunks(BATCH_SIZE).collect();
    let total_batches = batches.len();

    progress(EnrichmentProgress::Phase {
        phase: 2,
        name: "Entity extraction",
        note: "",
    });

    // Build prompts for every batch up front. A batch whose chunks
    // didn't yield a prompt (Some(empty)) still gets dispatched —
    // the model can correctly emit "no entities" for benign batches.
    let mut prompts: Vec<(usize, String)> = Vec::with_capacity(total_batches);
    for (i, batch) in batches.iter().enumerate() {
        let refs: Vec<&StoredChunk> = batch.iter().collect();
        if let Some(prompt) = domain.entity_extraction_prompt(&refs) {
            prompts.push((i, prompt));
        }
    }

    // Concurrent in-flight window with refill, mirroring
    // field_engine's skeleton-extraction shape.
    type InferenceFuture = std::pin::Pin<
        Box<
            dyn futures::Future<Output = (usize, crate::error::Result<String>)> + Send,
        >,
    >;

    let spawn = |inference: InferenceFn, idx: usize, prompt: String| -> InferenceFuture {
        Box::pin(async move {
            let r = (inference)(&prompt).await;
            (idx, r)
        })
    };

    let mut iter = prompts.into_iter();
    let mut in_flight: FuturesUnordered<InferenceFuture> = FuturesUnordered::new();
    for _ in 0..CONCURRENCY {
        if let Some((idx, p)) = iter.next() {
            in_flight.push(spawn(inference.clone(), idx, p));
        }
    }

    // Per-batch parsed responses, indexed by batch_idx. Resolution
    // is two-pass so participant names can find their target Person
    // or Organization atoms across batches.
    let mut parsed: Vec<(usize, EntityExtractionResponse)> = Vec::new();
    let mut failures: Vec<EntityExtractionFailure> = Vec::new();
    let mut batches_done: usize = 0;

    while let Some((batch_idx, result)) = in_flight.next().await {
        if let Some((next_idx, next_p)) = iter.next() {
            in_flight.push(spawn(inference.clone(), next_idx, next_p));
        }
        batches_done += 1;

        match result {
            Err(e) => {
                tracing::warn!(
                    batch = batch_idx,
                    error = %e,
                    "entity_extraction: inference failed"
                );
                failures.push(EntityExtractionFailure {
                    batch_index: batch_idx,
                    kind: FailureKind::InferenceError,
                    reason: e.to_string(),
                });
                continue;
            }
            Ok(response) => match parse_response(&response) {
                Ok(mut extracted) => {
                    // Rewrite mention labels (e.g. "1", "Memory 1",
                    // "Conversation 2") into the actual chunk_id
                    // strings so the merge step joins on a stable
                    // global key. Out-of-range labels are silently
                    // dropped — they're a model glitch rather than
                    // a parse error.
                    let batch = batches[batch_idx];
                    rewrite_mentions(&mut extracted, batch);
                    parsed.push((batch_idx, extracted));
                }
                Err(e) => {
                    tracing::warn!(
                        batch = batch_idx,
                        error = %e,
                        "entity_extraction: parse failed"
                    );
                    failures.push(EntityExtractionFailure {
                        batch_index: batch_idx,
                        kind: FailureKind::ParseError,
                        reason: e,
                    });
                }
            },
        }
    }

    let _ = total_batches; // logged below
    tracing::info!(
        total = batches_done,
        parsed = parsed.len(),
        failures = failures.len(),
        "entity_extraction: batches processed"
    );

    let merged = merge_responses(parsed, &mut failures);
    Ok(EntityExtractionResult {
        entities: merged.entities,
        edges: merged.edges,
        failures,
        batches_run: batches_done,
    })
}

/// Convenience helper that runs extraction and writes the result
/// into the corpus's `atlas/` directory. Empty result → no write
/// (so a domain that opted out doesn't materialise an empty
/// atoms.json that would mislead Phase 3 timeline lookups).
pub async fn run_and_write_entity_extraction(
    chunks: &[StoredChunk],
    domain: &dyn Domain,
    inference: InferenceFn,
    index_dir: &Path,
    progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
) -> Result<EntityExtractionResult> {
    let result = run_entity_extraction(chunks, domain, inference, progress).await?;
    if result.entities.is_empty() && result.edges.is_empty() {
        return Ok(result);
    }
    let atlas_dir = index_dir.join(super::atlas::writer::ATLAS_DIRNAME);
    write_atlas(&atlas_dir, &result.entities, &[], &result.edges)
        .map_err(|e| crate::error::Error::Io(e))?;
    tracing::info!(
        entities = result.entities.len(),
        edges = result.edges.len(),
        atlas_dir = %atlas_dir.display(),
        "entity_extraction: atlas written"
    );
    Ok(result)
}

// ── Parsing ─────────────────────────────────────────────────────

/// Pull the outermost JSON **object** from the model's raw response.
/// The shared `extract_json_from_response` in `skeleton_parse` is
/// array-biased (skeleton extraction returns top-level `[...]`); our
/// schema is object-shaped, so we run our own scan that strips the
/// same conventional wrappers (think-tags, ```json fences, ``` fences)
/// and then locks onto the first `{` / last `}` boundary.
fn extract_json_object<'a>(raw: &'a str) -> &'a str {
    let mut text = raw.trim();

    if let Some(end) = text.find("</think>") {
        text = text[end + "</think>".len()..].trim();
    }

    let lower = text.to_lowercase();
    if let Some(fence_start) = lower.find("```json") {
        let content_start = fence_start + "```json".len();
        let content_start = if text[content_start..].starts_with('\n') {
            content_start + 1
        } else {
            content_start
        };
        if let Some(fence_end) = text[content_start..].find("```") {
            let inner = text[content_start..content_start + fence_end].trim();
            if inner.starts_with('{') {
                return inner;
            }
        }
    }
    if let Some(fence_start) = text.find("```") {
        let content_start = fence_start + 3;
        let after = &text[content_start..];
        let content_start = if let Some(nl) = after.find('\n') {
            content_start + nl + 1
        } else {
            content_start
        };
        if let Some(fence_end) = text[content_start..].find("```") {
            let inner = text[content_start..content_start + fence_end].trim();
            if inner.starts_with('{') {
                return inner;
            }
        }
    }

    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return text[start..=end].trim();
            }
        }
    }
    text
}

fn parse_response(raw: &str) -> std::result::Result<EntityExtractionResponse, String> {
    let json = extract_json_object(raw);
    serde_json::from_str::<EntityExtractionResponse>(json).map_err(|e| e.to_string())
}

/// Lenient `Vec<String>` deserializer: accepts both bare strings
/// and `{"name": "..."}` objects, drops nulls and non-string entries
/// silently. Used wherever the model emits an array that we want
/// projected to a flat list of names — `participants` (the most
/// common offender) and `mentions` arrays. The behaviour matches
/// `rewrite_mentions`'s philosophy: tolerate model glitches at the
/// entry level instead of dropping the whole batch.
fn deserialize_participants<'de, D>(d: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_lenient_string_array(d)
}

fn deserialize_lenient_string_array<'de, D>(
    d: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let raw = serde_json::Value::deserialize(d)?;
    let arr = match raw {
        serde_json::Value::Null => return Ok(Vec::new()),
        serde_json::Value::Array(a) => a,
        serde_json::Value::String(s) => return Ok(vec![s]),
        other => {
            return Err(D::Error::custom(format!(
                "expected array, got {}",
                match other {
                    serde_json::Value::Object(_) => "object",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::Bool(_) => "bool",
                    _ => "unknown",
                }
            )))
        }
    };
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        match entry {
            serde_json::Value::String(s) => out.push(s),
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(s)) = map.get("name") {
                    out.push(s.clone());
                }
            }
            // Null + non-string scalars: skip silently.
            _ => {}
        }
    }
    Ok(out)
}

/// Translate batch-relative mention labels into stable global
/// chunk-id strings.
///
/// The prompt presents chunks as `[Memory 1]`, `[Memory 2]`, …, and
/// the model echoes those labels (or just bare integers like `"1"`)
/// in each entity's `mentions` array. We accept either form and
/// rewrite to the chunk's actual id (e.g. `"1234"`) so downstream
/// merge keys are global rather than batch-local.
///
/// Labels that don't resolve to a chunk in the batch are dropped
/// silently — a model glitch, not a parse error. If every label in
/// a single entity's `mentions` is dropped, that entity still
/// produces an atom (with an empty mentions list, no Involves edges)
/// so the user can see the extraction even when grounding failed.
fn rewrite_mentions(response: &mut EntityExtractionResponse, batch: &[StoredChunk]) {
    let resolve = |label: &str| -> Option<String> {
        let trimmed = label.trim();
        // Accept "1", "Memory 1", "Conversation 1", "[Memory 1]" — pull
        // out the trailing integer and use it as a 1-indexed batch index.
        let n: usize = trimmed
            .rsplit(|c: char| !c.is_ascii_digit())
            .find(|s| !s.is_empty())
            .and_then(|s| s.parse().ok())?;
        if n == 0 || n > batch.len() {
            return None;
        }
        Some(batch[n - 1].id.to_string())
    };

    for p in &mut response.persons {
        p.mentions = p.mentions.iter().filter_map(|m| resolve(m)).collect();
    }
    for o in &mut response.organizations {
        o.mentions = o.mentions.iter().filter_map(|m| resolve(m)).collect();
    }
    for it in &mut response.initiatives {
        it.mentions = it.mentions.iter().filter_map(|m| resolve(m)).collect();
    }
}

// ── Resolver ────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct MergedResult {
    entities: Vec<Entity>,
    edges: Vec<Edge>,
}

fn fold_name(s: &str) -> String {
    s.trim().to_lowercase()
}

fn merge_responses(
    parsed: Vec<(usize, EntityExtractionResponse)>,
    failures: &mut Vec<EntityExtractionFailure>,
) -> MergedResult {
    // Single shared index across all entity kinds — initiative
    // participant resolution can target a Person or Organization
    // atom indifferently, so they share a namespace.
    let mut by_folded_name: HashMap<String, usize> = HashMap::new();
    let mut entities: Vec<Entity> = Vec::new();
    // (entity_index, chunk_id) pairs — deduplicated before edge
    // emission so a person appearing twice in the same chunk
    // produces one Involves edge, not two.
    let mut mentions: Vec<(usize, String)> = Vec::new();
    // Initiative participant names that need second-pass resolution.
    let mut pending_participants: Vec<(usize, Vec<String>, usize)> = Vec::new();

    for (batch_idx, response) in &parsed {
        for p in &response.persons {
            let id = upsert_entity(
                &mut entities,
                &mut by_folded_name,
                EntityType::Person,
                &p.name,
                p.affiliation.clone(),
                p.role.clone(),
                p.description.clone(),
                first_chunk(&p.mentions),
            );
            for m in &p.mentions {
                mentions.push((id, m.clone()));
            }
            // Ambiguity check: did upsert merge into an existing
            // entity that disagrees on entity_type? (Same name,
            // different kind.) That's a real conflict — record but
            // keep the first-seen kind.
            if entities[id].entity_type != EntityType::Person {
                failures.push(EntityExtractionFailure {
                    batch_index: *batch_idx,
                    kind: FailureKind::EntityMergeAmbiguous,
                    reason: format!(
                        "name '{}' appeared as {:?} in earlier batch and Person in batch {}",
                        p.name, entities[id].entity_type, batch_idx
                    ),
                });
            }
        }

        for o in &response.organizations {
            let role_label = o.relationship.clone();
            let id = upsert_entity(
                &mut entities,
                &mut by_folded_name,
                EntityType::Institution,
                &o.name,
                None, // organisations don't carry affiliation
                role_label,
                o.description.clone(),
                first_chunk(&o.mentions),
            );
            for m in &o.mentions {
                mentions.push((id, m.clone()));
            }
        }

        for it in &response.initiatives {
            let id = upsert_entity(
                &mut entities,
                &mut by_folded_name,
                EntityType::Initiative,
                &it.name,
                None,
                it.status.clone(),
                it.description.clone(),
                first_chunk(&it.mentions),
            );
            for m in &it.mentions {
                mentions.push((id, m.clone()));
            }
            if !it.participants.is_empty() {
                pending_participants.push((id, it.participants.clone(), *batch_idx));
            }
        }
    }

    // Short-form alias consolidation.
    //
    // The model frequently emits a Person or Organization by short
    // form ("Mike", "Sarah", "Acme") in chunks where the surname or
    // suffix wasn't repeated, alongside a separate batch where the
    // full form ("Mike Torres", "Sarah Chen", "Acme Corp") *did*
    // appear. Without consolidation, these become two atoms — one
    // per form — and recall on the digest collapses (a timeline for
    // "Sarah Chen" misses every chunk where the model wrote just
    // "Sarah").
    //
    // Conservative rule: a single-token entity folds into a
    // multi-token entity of the same kind iff the multi-token's
    // *first whitespace-separated token* (case-insensitive) matches
    // the single-token's name AND it's the unique such target. If
    // two multi-token entities ("Mike Torres", "Mike Smith") share
    // the first token, we leave the short form alone — disambiguation
    // would require evidence we don't have here.
    //
    // Initiatives are excluded; their naming is too varied for a
    // first-token rule to be safe ("API" is not "API migration").
    consolidate_short_form_aliases(
        &mut entities,
        &mut mentions,
        &mut pending_participants,
        &mut by_folded_name,
    );

    // Second consolidation pass: conservative typo fold.
    //
    // Model occasionally misspells novel proper nouns in some chunks
    // ("Stonewell Industries" in c11 → "Stonewall Industries" in c20).
    // Without this pass the two land as separate atoms and timeline /
    // digest split between them. The rule is deliberately tight:
    // same kind, both names ≥ 7 characters (so 4-letter name pairs
    // like "Mike"/"Mile" can't accidentally merge), same first
    // letter, Levenshtein distance == 1, and the higher-chunk-count
    // entity wins. Ties don't fold — without a clear winner we'd be
    // guessing.
    consolidate_typo_aliases(
        &mut entities,
        &mut mentions,
        &mut pending_participants,
        &mut by_folded_name,
    );

    // Third consolidation pass: strip organization-name prefixes
    // from initiative canonicals.
    //
    // The model often emits "<Org>'s <Effort>" or "<Org> <Effort>"
    // when chunks frame an initiative through its organizational
    // owner ("Helios HIPAA compliance review", "Meridian
    // architecture refresh", "Acme API migration"). The prompt asks
    // for the bare canonical, but cross-batch chaos sometimes leaves
    // only the prefixed form on the atlas — the canonical never
    // surfaces and the digest treats them as distinct initiatives.
    //
    // Rule: if an Initiative's canonical_name starts with an
    // Organization name (full canonical or its first token,
    // optionally followed by `'s`), strip the prefix and use the
    // bare form. Bare must be ≥ 2 tokens to avoid producing
    // single-word initiative names like "procurement" or "board".
    // Renamed initiatives that collide with an existing canonical
    // fold via the shared compaction step.
    consolidate_org_prefixed_initiatives(
        &mut entities,
        &mut mentions,
        &mut pending_participants,
        &mut by_folded_name,
    );

    // Second pass: resolve initiative participant names to AtomIds.
    // Drop names we don't recognize (orphans) and surface them as
    // failures so the operator can see drift.
    for (init_idx, names, batch_idx) in pending_participants {
        let mut resolved: Vec<AtomId> = Vec::new();
        for name in names {
            match by_folded_name.get(&fold_name(&name)) {
                Some(&target_idx) if target_idx != init_idx => {
                    resolved.push(entities[target_idx].id.clone());
                }
                _ => {
                    failures.push(EntityExtractionFailure {
                        batch_index: batch_idx,
                        kind: FailureKind::OrphanParticipant,
                        reason: format!(
                            "initiative '{}' references unknown participant '{}'",
                            entities[init_idx].canonical_name, name
                        ),
                    });
                }
            }
        }
        // Dedup while preserving order.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        resolved.retain(|aid| seen.insert(aid.as_str().to_string()));
        entities[init_idx].participants = resolved;
    }

    // Edge emission — dedup (entity, chunk_id) pairs.
    let mut seen_edges: std::collections::HashSet<(usize, String)> =
        std::collections::HashSet::new();
    let mut edges: Vec<Edge> = Vec::new();
    for (idx, chunk_id) in mentions {
        if !seen_edges.insert((idx, chunk_id.clone())) {
            continue;
        }
        let entity = &entities[idx];
        // Synthesise a per-chunk pseudo-event-id of the form
        // "involves-<entity-idx>-<chunk-id>" so the edge has a
        // stable target distinct from the entity itself. This
        // matches Step 3a's convention (Involves edges connect an
        // event-anchor atom to its participating entity); for the
        // personal/conversational pipeline, the "event" is the
        // message/memory chunk itself, not a separately resolved
        // event atom — so we use the chunk_id as the source-side
        // atom id and the entity as the target.
        edges.push(Edge {
            id: EdgeId::new(edges.len() + 1),
            edge_type: EdgeType::Involves,
            // Source = the chunk; target = the entity. Chunk id is
            // a string in the atlas, so it round-trips through
            // `AtomId::from_raw` — readers that walk the graph
            // discover chunk-keyed atoms via the Involves edge's
            // source endpoint.
            source: AtomId::from_raw(format!("chunk-{}", chunk_id)),
            target: entity.id.clone(),
            evidence: vec![ChunkRef::new(chunk_id, None)],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        });
    }

    MergedResult { entities, edges }
}

/// Fold single-token Person and Organization entities into their
/// multi-token counterparts when the multi-token entity is unique
/// for that first token. Mutates `entities`, `mentions`,
/// `pending_participants`, and `by_folded_name` in place.
///
/// Implementation sketch:
///   1. Group multi-token entities by (kind, first_token).
///   2. For each single-token entity, look up the unique
///      multi-token target. Record `remap[single_idx] = target_idx`.
///   3. Apply `remap` to `mentions` and `pending_participants` so
///      every chunk reference and every participant that pointed at
///      the short form now points at the canonical entity.
///   4. Compact `entities`: keep entities not in the remap source
///      set, in original order. Renumber `AtomId`s densely so the
///      atlas writer doesn't see gaps. The single-token's display
///      name is appended to the canonical entity's `aliases`.
///   5. Rebuild `by_folded_name`: every retained entity contributes
///      its canonical name *and* every alias, all pointing at the
///      same compacted index. This lets the second-pass participant
///      resolution find the canonical atom even when the model
///      cited a participant by short form.
fn consolidate_short_form_aliases(
    entities: &mut Vec<Entity>,
    mentions: &mut [(usize, String)],
    pending_participants: &mut [(usize, Vec<String>, usize)],
    by_folded_name: &mut HashMap<String, usize>,
) {
    // ── Step 1: index multi-token entities by (kind, first_token) ──
    let mut multi_by_first_token: HashMap<(EntityType, String), Vec<usize>> = HashMap::new();
    for (idx, e) in entities.iter().enumerate() {
        if !matches!(e.entity_type, EntityType::Person | EntityType::Institution) {
            continue;
        }
        let tokens: Vec<&str> = e.canonical_name.split_whitespace().collect();
        if tokens.len() < 2 {
            continue;
        }
        let key = (e.entity_type.clone(), tokens[0].to_lowercase());
        multi_by_first_token.entry(key).or_default().push(idx);
    }

    // ── Step 2: for each single-token, find unique target ──────────
    let mut remap: HashMap<usize, usize> = HashMap::new();
    for (idx, e) in entities.iter().enumerate() {
        if !matches!(e.entity_type, EntityType::Person | EntityType::Institution) {
            continue;
        }
        let tokens: Vec<&str> = e.canonical_name.split_whitespace().collect();
        if tokens.len() != 1 {
            continue;
        }
        let key = (e.entity_type.clone(), tokens[0].to_lowercase());
        if let Some(targets) = multi_by_first_token.get(&key) {
            // Unique multi-token target → fold. Skip if the single
            // happens to also be in the target list (shouldn't
            // happen — single tokens never index here — but a guard
            // is cheap).
            if targets.len() == 1 {
                let target = targets[0];
                if target != idx {
                    remap.insert(idx, target);
                }
            }
        }
    }

    apply_alias_remap_and_compact(
        entities,
        mentions,
        pending_participants,
        by_folded_name,
        remap,
    );
}

/// Conservative typo fold: collapse Person/Organization atoms whose
/// canonical names differ by a single character ("Stonewell" vs
/// "Stonewall"). Both names must be ≥7 characters (so short-name
/// pairs like "Mike"/"Mile" can't accidentally merge), share a first
/// letter, and the same kind. The atom with more chunk references
/// wins; ties don't fold.
///
/// Runs after `consolidate_short_form_aliases` so the chunk-count
/// signal already reflects post-canonicalization weight.
fn consolidate_typo_aliases(
    entities: &mut Vec<Entity>,
    mentions: &mut [(usize, String)],
    pending_participants: &mut [(usize, Vec<String>, usize)],
    by_folded_name: &mut HashMap<String, usize>,
) {
    if entities.len() < 2 {
        return;
    }

    // Compute per-entity chunk count from the (idx, chunk_id) pairs
    // built during the merge loop. A unique chunk_id contributes one
    // mention; duplicates are deduped here so the count matches what
    // the atlas writer will emit (one Involves edge per chunk).
    let mut chunk_counts: HashMap<usize, std::collections::HashSet<String>> = HashMap::new();
    for (idx, chunk_id) in mentions.iter() {
        chunk_counts
            .entry(*idx)
            .or_default()
            .insert(chunk_id.clone());
    }
    let chunk_count = |idx: usize| -> usize {
        chunk_counts.get(&idx).map(|s| s.len()).unwrap_or(0)
    };

    let mut remap: HashMap<usize, usize> = HashMap::new();
    // Skip entities already remapped (transitive closure not needed
    // — one fold per source is enough; if A→B and B→C, the second
    // fold can run in a future pass if quality regressions ever
    // demand it).
    for i in 0..entities.len() {
        if remap.contains_key(&i) {
            continue;
        }
        let a = &entities[i];
        if !matches!(a.entity_type, EntityType::Person | EntityType::Institution) {
            continue;
        }
        let a_folded = fold_name(&a.canonical_name);
        if a_folded.chars().count() < 7 {
            continue;
        }
        let a_first = a_folded.chars().next();
        for j in (i + 1)..entities.len() {
            if remap.contains_key(&j) {
                continue;
            }
            let b = &entities[j];
            if a.entity_type != b.entity_type {
                continue;
            }
            let b_folded = fold_name(&b.canonical_name);
            if b_folded.chars().count() < 7 {
                continue;
            }
            if a_first != b_folded.chars().next() {
                continue;
            }
            if levenshtein_at_most_one(&a_folded, &b_folded) {
                let a_chunks = chunk_count(i);
                let b_chunks = chunk_count(j);
                use std::cmp::Ordering;
                match a_chunks.cmp(&b_chunks) {
                    Ordering::Greater => {
                        remap.insert(j, i);
                    }
                    Ordering::Less => {
                        remap.insert(i, j);
                        // i is now folded; can't act as target for
                        // any later pair; break out of the inner
                        // loop.
                        break;
                    }
                    Ordering::Equal => {
                        // Tie: don't fold ambiguously.
                    }
                }
            }
        }
    }

    apply_alias_remap_and_compact(
        entities,
        mentions,
        pending_participants,
        by_folded_name,
        remap,
    );
}

/// Levenshtein-distance ≤ 1 fast path. Returns true iff the two
/// strings are equal or differ by exactly one insertion, deletion,
/// or substitution. Cheap O(min(|a|, |b|)) — we don't need the full
/// distance, only the ≤ 1 predicate.
fn levenshtein_at_most_one(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() {
        (&a[..], &b[..])
    } else {
        (&b[..], &a[..])
    };
    if long.len() - short.len() > 1 {
        return false;
    }
    let mut i = 0usize;
    let mut j = 0usize;
    let mut diffs = 0u8;
    while i < short.len() && j < long.len() {
        if short[i] == long[j] {
            i += 1;
            j += 1;
            continue;
        }
        diffs += 1;
        if diffs > 1 {
            return false;
        }
        if short.len() == long.len() {
            // substitution
            i += 1;
            j += 1;
        } else {
            // insertion in long
            j += 1;
        }
    }
    if j < long.len() {
        diffs += long.len() as u8 - j as u8;
    }
    diffs <= 1
}

/// Strip organization-name prefixes from Initiative canonicals,
/// then fold the renamed initiatives into any matching existing
/// canonical. The org list is data we already have on the atlas;
/// no new model calls.
///
/// Rule:
///   - Build a candidate-prefix set from every Organization atom:
///     each org's canonical_name AND its first whitespace token
///     (when ≥ 4 chars — guards against stripping short tokens
///     that double as proper nouns).
///   - For each Initiative, try to strip the longest matching
///     candidate prefix (with optional `'s` suffix and trailing
///     whitespace). Try longer candidates before shorter so that
///     "Acme Corp" strips before "Acme".
///   - The bare suffix must be ≥ 2 tokens — otherwise we'd be
///     producing single-word initiative names like "procurement"
///     or "board" which read as nouns, not strategic frames.
///   - Rename in place. If two initiatives end up with the same
///     folded name, fold the lower-chunk-count one into the
///     higher-chunk-count one. Tie → keep the renamed one (the
///     bare canonical is preferred).
fn consolidate_org_prefixed_initiatives(
    entities: &mut Vec<Entity>,
    mentions: &mut [(usize, String)],
    pending_participants: &mut [(usize, Vec<String>, usize)],
    by_folded_name: &mut HashMap<String, usize>,
) {
    if entities.len() < 2 {
        return;
    }

    // Build candidate org prefixes. Sort longest first so that
    // multi-token org names match before their first-token
    // shortcuts. Lowercased for case-insensitive matching.
    let mut org_prefixes: Vec<String> = Vec::new();
    for e in entities.iter() {
        if !matches!(e.entity_type, EntityType::Institution) {
            continue;
        }
        let name = e.canonical_name.trim();
        if !name.is_empty() {
            org_prefixes.push(name.to_lowercase());
        }
        if let Some(first) = name.split_whitespace().next() {
            if first.chars().count() >= 4 {
                org_prefixes.push(first.to_lowercase());
            }
        }
    }
    if org_prefixes.is_empty() {
        return;
    }
    org_prefixes.sort_by_key(|s| std::cmp::Reverse(s.chars().count()));
    org_prefixes.dedup();

    // First pass: rename eligible initiatives. We rename in place;
    // dedupe via the chunk-count-aware fold below.
    let mut renamed: Vec<(usize, String, String)> = Vec::new(); // (idx, old, new)
    for (idx, e) in entities.iter_mut().enumerate() {
        if !matches!(e.entity_type, EntityType::Initiative) {
            continue;
        }
        if let Some(stripped) = strip_org_prefix(&e.canonical_name, &org_prefixes) {
            renamed.push((idx, e.canonical_name.clone(), stripped.clone()));
            // Preserve the prefixed form as an alias on the renamed
            // entity — useful for the digest and for resolving
            // future participant references that cite the prefixed
            // form.
            let alias = e.canonical_name.clone();
            e.canonical_name = stripped;
            if !e.aliases.iter().any(|a| a.eq_ignore_ascii_case(&alias)) {
                e.aliases.push(alias);
            }
        }
    }
    if renamed.is_empty() {
        return;
    }

    // Compute chunk counts (deduped per-chunk) so collisions can
    // pick the better-attested form to keep.
    let mut chunk_counts: HashMap<usize, std::collections::HashSet<String>> = HashMap::new();
    for (idx, chunk_id) in mentions.iter() {
        chunk_counts
            .entry(*idx)
            .or_default()
            .insert(chunk_id.clone());
    }
    let chunk_count = |idx: usize| -> usize {
        chunk_counts.get(&idx).map(|s| s.len()).unwrap_or(0)
    };

    // Find collisions: every group of initiatives with the same
    // folded canonical_name. Keep the one with more chunks; fold
    // the rest into it. On ties, prefer the entry that was just
    // renamed from a prefixed form (i.e. the bare canonical is the
    // intended winner over a duplicate that pre-existed).
    let renamed_idxs: std::collections::HashSet<usize> =
        renamed.iter().map(|(idx, _, _)| *idx).collect();

    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, e) in entities.iter().enumerate() {
        if matches!(e.entity_type, EntityType::Initiative) {
            groups
                .entry(fold_name(&e.canonical_name))
                .or_default()
                .push(idx);
        }
    }

    let mut remap: HashMap<usize, usize> = HashMap::new();
    for (_name, idxs) in groups {
        if idxs.len() < 2 {
            continue;
        }
        // Pick winner: highest chunk count; on tie, prefer renamed.
        let mut winner = idxs[0];
        for &candidate in &idxs[1..] {
            let c_chunks = chunk_count(candidate);
            let w_chunks = chunk_count(winner);
            use std::cmp::Ordering;
            match c_chunks.cmp(&w_chunks) {
                Ordering::Greater => winner = candidate,
                Ordering::Less => {}
                Ordering::Equal => {
                    if renamed_idxs.contains(&candidate) && !renamed_idxs.contains(&winner) {
                        winner = candidate;
                    }
                }
            }
        }
        for &idx in &idxs {
            if idx != winner {
                remap.insert(idx, winner);
            }
        }
    }

    apply_alias_remap_and_compact(
        entities,
        mentions,
        pending_participants,
        by_folded_name,
        remap,
    );
}

/// Try to strip the longest matching organization prefix from
/// `name`. Tolerates `'s` after the prefix and requires at least
/// one whitespace separator. Returns `Some(bare)` only when the
/// remaining text has ≥ 2 whitespace tokens.
fn strip_org_prefix(name: &str, org_prefixes_sorted_desc: &[String]) -> Option<String> {
    let lower = name.to_lowercase();
    for prefix in org_prefixes_sorted_desc {
        // Try `<prefix>'s ` first, then `<prefix> `.
        for sep in &["'s ", " "] {
            let pat = format!("{prefix}{sep}");
            if lower.starts_with(&pat) {
                // Slice from the original string to preserve casing
                // of the remainder. UTF-8-safe because `pat` is an
                // ASCII match anchored at the start.
                let cut = pat.len();
                if cut < name.len() {
                    let bare = name[cut..].trim();
                    if bare.split_whitespace().count() >= 2 {
                        return Some(bare.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Shared compaction step used by every `consolidate_*` pass.
/// Appends each source's display name to its target's aliases,
/// rewrites all `mentions` / `pending_participants` indices to point
/// at the post-compact positions, drops the source entities, and
/// renumbers `AtomId`s densely so the atlas writer doesn't see gaps.
/// `by_folded_name` is rebuilt from scratch including aliases.
fn apply_alias_remap_and_compact(
    entities: &mut Vec<Entity>,
    mentions: &mut [(usize, String)],
    pending_participants: &mut [(usize, Vec<String>, usize)],
    by_folded_name: &mut HashMap<String, usize>,
    remap: HashMap<usize, usize>,
) {
    if remap.is_empty() {
        return;
    }

    // Rewrite per-position references.
    for (eidx, _) in mentions.iter_mut() {
        if let Some(&new) = remap.get(eidx) {
            *eidx = new;
        }
    }
    for (init_idx, _, _) in pending_participants.iter_mut() {
        if let Some(&new) = remap.get(init_idx) {
            *init_idx = new;
        }
    }

    // Append source display names to their target's alias list,
    // and migrate any aliases the source had collected from earlier
    // passes (e.g. the org-prefixed canonical preserved before the
    // rename). Without this, aliases earned in earlier consolidation
    // steps get dropped when the entity is folded.
    for (&src, &tgt) in &remap {
        let primary_alias = entities[src].canonical_name.clone();
        let inherited_aliases: Vec<String> = entities[src].aliases.clone();
        let target = &mut entities[tgt];
        if !target
            .aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&primary_alias))
            && !target.canonical_name.eq_ignore_ascii_case(&primary_alias)
        {
            target.aliases.push(primary_alias);
        }
        for inh in inherited_aliases {
            if !target
                .aliases
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&inh))
                && !target.canonical_name.eq_ignore_ascii_case(&inh)
            {
                target.aliases.push(inh);
            }
        }
    }

    // Compact + renumber AtomIds densely.
    let mut idx_remap: HashMap<usize, usize> = HashMap::new();
    let mut compacted: Vec<Entity> = Vec::with_capacity(entities.len() - remap.len());
    for (old_idx, e) in entities.drain(..).enumerate() {
        if remap.contains_key(&old_idx) {
            continue;
        }
        let new_idx = compacted.len();
        idx_remap.insert(old_idx, new_idx);
        compacted.push(e);
    }
    for (&src, &tgt) in &remap {
        let target_new = idx_remap[&tgt];
        idx_remap.insert(src, target_new);
    }
    for (i, e) in compacted.iter_mut().enumerate() {
        e.id = AtomId::entity(i + 1);
    }
    *entities = compacted;

    // Apply compaction remap.
    for (eidx, _) in mentions.iter_mut() {
        if let Some(&new) = idx_remap.get(eidx) {
            *eidx = new;
        }
    }
    for (init_idx, _, _) in pending_participants.iter_mut() {
        if let Some(&new) = idx_remap.get(init_idx) {
            *init_idx = new;
        }
    }

    // Rebuild folded-name index, including aliases.
    by_folded_name.clear();
    for (i, e) in entities.iter().enumerate() {
        by_folded_name.insert(fold_name(&e.canonical_name), i);
        for alias in &e.aliases {
            by_folded_name.entry(fold_name(alias)).or_insert(i);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn upsert_entity(
    entities: &mut Vec<Entity>,
    by_folded_name: &mut HashMap<String, usize>,
    kind: EntityType,
    raw_name: &str,
    affiliation: Option<String>,
    role: Option<String>,
    description: Option<String>,
    first_chunk_id: Option<String>,
) -> usize {
    let folded = fold_name(raw_name);
    if let Some(&idx) = by_folded_name.get(&folded) {
        // Merge: enrich missing fields, keep first non-empty values.
        if entities[idx].affiliation.is_none() && affiliation.is_some() {
            entities[idx].affiliation = affiliation;
        }
        if entities[idx].role.is_none() && role.is_some() {
            entities[idx].role = role;
        }
        if entities[idx].description.is_empty() {
            if let Some(d) = description {
                entities[idx].description = d;
            }
        }
        return idx;
    }
    let new_idx = entities.len();
    let new_id = AtomId::entity(new_idx + 1);
    let first_chunk = first_chunk_id
        .or_else(|| Some(String::from("unknown-chunk")))
        .unwrap();
    entities.push(Entity {
        id: new_id,
        canonical_name: raw_name.trim().to_string(),
        aliases: Vec::new(),
        entity_type: kind,
        first_appearance: ChunkRef::new(first_chunk, None),
        description: description.unwrap_or_default(),
        salience: DEFAULT_SALIENCE,
        enrichment_depth: EnrichmentDepth::extracted_default(),
        affiliation,
        role,
        participants: Vec::new(),
    });
    by_folded_name.insert(folded, new_idx);
    new_idx
}

fn first_chunk(mentions: &[String]) -> Option<String> {
    mentions.iter().next().cloned()
}

// ── Wiring helper for callers ───────────────────────────────────

/// Convenience for the engine: holds the inference function as an
/// `Arc` and exposes a method that accepts &dyn Domain so the engine
/// doesn't need to clone the InferenceFn at every call site.
pub struct EntityExtractor {
    inference: InferenceFn,
}

impl EntityExtractor {
    pub fn new(inference: InferenceFn) -> Self {
        Self { inference }
    }

    pub async fn run(
        &self,
        chunks: &[StoredChunk],
        domain: &dyn Domain,
        index_dir: &Path,
        progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
    ) -> Result<EntityExtractionResult> {
        run_and_write_entity_extraction(
            chunks,
            domain,
            self.inference.clone(),
            index_dir,
            progress,
        )
        .await
    }
}

// Quiet unused-import warnings on the Arc re-export when the
// extractor is the only consumer of Arc in this module.
#[allow(dead_code)]
fn _phantom_arc(a: Arc<()>) -> Arc<()> {
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::domain::Chunk;

    fn chunk(id: u64, content: &str) -> StoredChunk {
        StoredChunk {
            id,
            content: content.into(),
            title: None,
        }
    }

    fn sample_response_json() -> &'static str {
        r#"{
          "persons": [
            {
              "name": "Sarah Chen",
              "affiliation": "Acme Corp",
              "role": "VP Engineering",
              "mentions": ["1", "2"]
            }
          ],
          "organizations": [
            {
              "name": "Acme Corp",
              "relationship": "client",
              "mentions": ["1"]
            }
          ],
          "initiatives": [
            {
              "name": "Q3 enterprise push",
              "status": "team aligned on vertical focus",
              "participants": ["Sarah Chen", "Acme Corp"],
              "mentions": ["2"]
            }
          ]
        }"#
    }

    #[test]
    fn parse_response_round_trips_full_payload() {
        let parsed = parse_response(sample_response_json()).unwrap();
        assert_eq!(parsed.persons.len(), 1);
        assert_eq!(parsed.persons[0].name, "Sarah Chen");
        assert_eq!(parsed.persons[0].affiliation.as_deref(), Some("Acme Corp"));
        assert_eq!(parsed.organizations.len(), 1);
        assert_eq!(parsed.initiatives.len(), 1);
        assert_eq!(parsed.initiatives[0].participants.len(), 2);
    }

    #[test]
    fn parse_response_tolerates_missing_arrays() {
        let parsed = parse_response("{}").unwrap();
        assert!(parsed.persons.is_empty());
        assert!(parsed.organizations.is_empty());
        assert!(parsed.initiatives.is_empty());
    }

    #[test]
    fn parse_response_tolerates_nulls_in_string_arrays() {
        // Real-world failure: model emitted an array with `null`
        // mid-array — the strict Vec<String> deserializer aborted
        // the whole batch. The lenient deserializer drops nulls and
        // non-string scalars silently.
        let raw = r#"{
          "persons": [
            {
              "name": "Mike Torres",
              "mentions": ["Conversation 1", null, "Conversation 3"]
            }
          ],
          "organizations": [],
          "initiatives": [
            {
              "name": "API migration",
              "participants": ["Mike Torres", null, {"name": "Sarah Chen"}],
              "mentions": [null, "Conversation 1"]
            }
          ]
        }"#;
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed.persons.len(), 1);
        assert_eq!(
            parsed.persons[0].mentions,
            vec!["Conversation 1", "Conversation 3"]
        );
        assert_eq!(parsed.initiatives.len(), 1);
        assert_eq!(
            parsed.initiatives[0].participants,
            vec!["Mike Torres", "Sarah Chen"]
        );
        assert_eq!(parsed.initiatives[0].mentions, vec!["Conversation 1"]);
    }

    #[test]
    fn parse_response_accepts_participants_as_objects() {
        // Larger thinking models (observed: Qwopus-GLM-18B) often
        // promote participant entries from strings to objects when
        // they have anything to add. The deserializer must accept
        // either form and project to a flat Vec<String> of names.
        let raw = r#"{
          "persons": [],
          "organizations": [],
          "initiatives": [{
            "name": "API migration",
            "participants": [
              {"name": "Mike Torres"},
              "Sarah Chen",
              {"name": "Dana Park", "role": "CTO"}
            ],
            "mentions": ["Conversation 1"]
          }]
        }"#;
        let parsed = parse_response(raw).unwrap();
        assert_eq!(parsed.initiatives.len(), 1);
        assert_eq!(
            parsed.initiatives[0].participants,
            vec!["Mike Torres", "Sarah Chen", "Dana Park"]
        );
    }

    #[test]
    fn parse_response_extracts_json_from_prose_wrapper() {
        // The shared `extract_json_from_response` strips think-tags
        // and trims to the outermost {}; this test pins that the
        // entity-extraction parser routes through it.
        let raw = "<think>scratch</think>\nHere is the result:\n{\"persons\":[]}\n";
        let parsed = parse_response(raw).unwrap();
        assert!(parsed.persons.is_empty());
    }

    #[test]
    fn merge_produces_one_atom_per_canonical_name() {
        let mut response = EntityExtractionResponse::default();
        response.persons.push(PersonEntity {
            name: "Sarah Chen".into(),
            affiliation: Some("Acme".into()),
            role: None,
            description: None,
            mentions: vec!["c1".into()],
        });
        response.persons.push(PersonEntity {
            name: "  Sarah Chen  ".into(), // whitespace + same name → merge
            affiliation: None,
            role: Some("VP Engineering".into()),
            description: None,
            mentions: vec!["c2".into()],
        });

        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, response)], &mut failures);
        assert_eq!(merged.entities.len(), 1, "duplicate names must merge");
        let e = &merged.entities[0];
        assert_eq!(e.canonical_name, "Sarah Chen");
        assert_eq!(e.affiliation.as_deref(), Some("Acme"));
        assert_eq!(e.role.as_deref(), Some("VP Engineering"));
        // Two distinct chunk_ids → two Involves edges.
        assert_eq!(merged.edges.len(), 2);
        assert!(merged.edges.iter().all(|e| e.edge_type == EdgeType::Involves));
    }

    #[test]
    fn merge_resolves_initiative_participants_to_atom_ids() {
        let parsed = parse_response(sample_response_json()).unwrap();
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, parsed)], &mut failures);

        // Person + Organization + Initiative = 3 atoms.
        assert_eq!(merged.entities.len(), 3);

        let initiative = merged
            .entities
            .iter()
            .find(|e| e.entity_type == EntityType::Initiative)
            .expect("initiative must be present");
        assert_eq!(
            initiative.participants.len(),
            2,
            "both Sarah Chen and Acme Corp should resolve to participant atoms"
        );

        // No orphan participants.
        assert!(failures
            .iter()
            .all(|f| f.kind != FailureKind::OrphanParticipant));
    }

    #[test]
    fn strip_org_prefix_strips_apostrophe_s() {
        let prefixes = vec!["acme corp".to_string(), "acme".to_string()];
        let mut sorted = prefixes.clone();
        sorted.sort_by_key(|s| std::cmp::Reverse(s.chars().count()));
        // Acme Corp's API migration → API migration
        assert_eq!(
            strip_org_prefix("Acme Corp's API migration", &sorted).as_deref(),
            Some("API migration")
        );
        // Acme API migration → API migration (first-token match)
        assert_eq!(
            strip_org_prefix("Acme API migration", &sorted).as_deref(),
            Some("API migration")
        );
        // Bare canonical without org prefix → no strip
        assert!(strip_org_prefix("API migration", &sorted).is_none());
        // Single-token bare → reject (would produce lone noun)
        assert!(strip_org_prefix("Acme procurement", &sorted).is_none());
    }

    #[test]
    fn strip_org_prefix_prefers_longer_org_match() {
        let prefixes = {
            let mut p = vec![
                "stonewell industries".to_string(),
                "stonewell".to_string(),
            ];
            p.sort_by_key(|s| std::cmp::Reverse(s.chars().count()));
            p
        };
        // Should strip the full "Stonewell Industries" prefix, not
        // just "Stonewell".
        assert_eq!(
            strip_org_prefix("Stonewell Industries platform overhaul", &prefixes).as_deref(),
            Some("platform overhaul")
        );
    }

    #[test]
    fn consolidate_renames_org_prefixed_initiative_to_bare_canonical() {
        // No bare canonical exists in extracted set — only the
        // org-prefixed form. After consolidation the initiative
        // should be renamed to the bare form, with the prefixed
        // name preserved as an alias.
        let mut b1 = EntityExtractionResponse::default();
        b1.organizations.push(OrganizationEntity {
            name: "Helios Health".into(),
            relationship: None,
            description: None,
            mentions: vec!["c1".into()],
        });
        b1.initiatives.push(InitiativeEntity {
            name: "Helios HIPAA compliance review".into(),
            status: None,
            participants: Vec::new(),
            description: None,
            mentions: vec!["c1".into(), "c2".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1)], &mut failures);

        let init = merged
            .entities
            .iter()
            .find(|e| matches!(e.entity_type, EntityType::Initiative))
            .expect("initiative survives");
        assert_eq!(init.canonical_name, "HIPAA compliance review");
        assert!(init
            .aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case("Helios HIPAA compliance review")));
    }

    #[test]
    fn consolidate_merges_org_prefixed_with_existing_bare_canonical() {
        // Both "API migration" and "Acme API migration" extracted.
        // After consolidation: a single canonical with the prefixed
        // form folded in as alias. Higher-chunk-count entity wins.
        let mut b1 = EntityExtractionResponse::default();
        b1.organizations.push(OrganizationEntity {
            name: "Acme Corp".into(),
            relationship: None,
            description: None,
            mentions: vec!["c1".into()],
        });
        b1.initiatives.push(InitiativeEntity {
            name: "API migration".into(),
            status: None,
            participants: Vec::new(),
            description: None,
            mentions: vec!["c1".into(), "c2".into(), "c3".into()],
        });
        b1.initiatives.push(InitiativeEntity {
            name: "Acme API migration".into(),
            status: None,
            participants: Vec::new(),
            description: None,
            mentions: vec!["c4".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1)], &mut failures);

        let inits: Vec<&Entity> = merged
            .entities
            .iter()
            .filter(|e| matches!(e.entity_type, EntityType::Initiative))
            .collect();
        assert_eq!(inits.len(), 1, "duplicate initiatives must collapse");
        assert_eq!(inits[0].canonical_name, "API migration");
        assert!(inits[0]
            .aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case("Acme API migration")));
    }

    #[test]
    fn consolidate_does_not_strip_when_no_matching_org() {
        // Initiative with a name that *looks* like a possessive but
        // doesn't match any extracted Organization → leave alone.
        let mut b1 = EntityExtractionResponse::default();
        b1.organizations.push(OrganizationEntity {
            name: "Acme Corp".into(),
            relationship: None,
            description: None,
            mentions: vec!["c1".into()],
        });
        b1.initiatives.push(InitiativeEntity {
            name: "Globex API migration".into(), // Globex isn't an org we extracted
            status: None,
            participants: Vec::new(),
            description: None,
            mentions: vec!["c2".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1)], &mut failures);
        let init = merged
            .entities
            .iter()
            .find(|e| matches!(e.entity_type, EntityType::Initiative))
            .unwrap();
        assert_eq!(init.canonical_name, "Globex API migration");
    }

    #[test]
    fn levenshtein_at_most_one_predicate() {
        // Equality.
        assert!(levenshtein_at_most_one("stonewell", "stonewell"));
        // Substitution: a → e.
        assert!(levenshtein_at_most_one("stonewell", "stonewall"));
        // Insertion.
        assert!(levenshtein_at_most_one("stonewell", "stoneswell"));
        // Deletion.
        assert!(levenshtein_at_most_one("stonewell", "stonewel"));
        // Distance ≥ 2 — reject.
        assert!(!levenshtein_at_most_one("stonewell", "stoneward"));
        // Length difference > 1 — early reject.
        assert!(!levenshtein_at_most_one("ab", "abcd"));
        // Empty + 1-char — distance 1.
        assert!(levenshtein_at_most_one("", "a"));
        assert!(!levenshtein_at_most_one("", "ab"));
    }

    #[test]
    fn consolidate_folds_typo_organization_into_higher_chunk_winner() {
        // "Stonewell Industries" appears in 3 chunks; "Stonewall
        // Industries" in 1. The typo'd version folds into the
        // higher-frequency canonical.
        let mut b1 = EntityExtractionResponse::default();
        b1.organizations.push(OrganizationEntity {
            name: "Stonewell Industries".into(),
            relationship: None,
            description: None,
            mentions: vec!["c1".into(), "c2".into(), "c3".into()],
        });
        b1.organizations.push(OrganizationEntity {
            name: "Stonewall Industries".into(),
            relationship: None,
            description: None,
            mentions: vec!["c4".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1)], &mut failures);

        assert_eq!(merged.entities.len(), 1, "typo'd org folds into canonical");
        let e = &merged.entities[0];
        assert_eq!(e.canonical_name, "Stonewell Industries");
        assert!(e
            .aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case("Stonewall Industries")));
        assert_eq!(merged.edges.len(), 4, "all 4 chunks should attach to canonical");
    }

    #[test]
    fn consolidate_typo_does_not_fold_short_names() {
        // "Mike" vs "Mile" — both 4 chars; the length floor (≥7)
        // protects against false positives on short names.
        let mut b1 = EntityExtractionResponse::default();
        b1.persons.push(PersonEntity {
            name: "Mike".into(),
            affiliation: None,
            role: None,
            description: None,
            mentions: vec!["c1".into()],
        });
        b1.persons.push(PersonEntity {
            name: "Mile".into(),
            affiliation: None,
            role: None,
            description: None,
            mentions: vec!["c2".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1)], &mut failures);
        assert_eq!(merged.entities.len(), 2, "short names must not fuzz-fold");
    }

    #[test]
    fn consolidate_typo_keeps_both_when_chunk_counts_tie() {
        // "Stonewell Industries" and "Stonewall Industries" each
        // appear in exactly 1 chunk. Without a clear winner we don't
        // fold — better to keep both than guess.
        let mut b1 = EntityExtractionResponse::default();
        b1.organizations.push(OrganizationEntity {
            name: "Stonewell Industries".into(),
            relationship: None,
            description: None,
            mentions: vec!["c1".into()],
        });
        b1.organizations.push(OrganizationEntity {
            name: "Stonewall Industries".into(),
            relationship: None,
            description: None,
            mentions: vec!["c2".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1)], &mut failures);
        assert_eq!(
            merged.entities.len(),
            2,
            "tied chunk counts should not trigger ambiguous fold"
        );
    }

    #[test]
    fn consolidate_typo_does_not_cross_first_letter() {
        // Same length, distance 1, but different first letter
        // ("Banner" vs "Manner") — almost always different entities
        // when first letter changes.
        let mut b1 = EntityExtractionResponse::default();
        b1.organizations.push(OrganizationEntity {
            name: "Banner Holdings".into(),
            relationship: None,
            description: None,
            mentions: vec!["c1".into(), "c2".into()],
        });
        b1.organizations.push(OrganizationEntity {
            name: "Manner Holdings".into(),
            relationship: None,
            description: None,
            mentions: vec!["c3".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1)], &mut failures);
        assert_eq!(merged.entities.len(), 2, "first-letter mismatch must not fold");
    }

    #[test]
    fn consolidate_folds_short_form_person_into_unique_full_name() {
        // Batch 1 sees "Mike Torres" with a role; Batch 2 sees just
        // "Mike". The short form must fold into the canonical entry
        // and contribute its mentions to the same atom.
        let mut b1 = EntityExtractionResponse::default();
        b1.persons.push(PersonEntity {
            name: "Mike Torres".into(),
            affiliation: Some("Acme".into()),
            role: Some("Engineering Lead".into()),
            description: None,
            mentions: vec!["c2".into()],
        });
        let mut b2 = EntityExtractionResponse::default();
        b2.persons.push(PersonEntity {
            name: "Mike".into(),
            affiliation: None,
            role: None,
            description: None,
            mentions: vec!["c5".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1), (1, b2)], &mut failures);

        assert_eq!(merged.entities.len(), 1, "Mike folds into Mike Torres");
        let e = &merged.entities[0];
        assert_eq!(e.canonical_name, "Mike Torres");
        assert!(e.aliases.iter().any(|a| a.eq_ignore_ascii_case("Mike")));
        assert_eq!(e.affiliation.as_deref(), Some("Acme"));
        assert_eq!(e.role.as_deref(), Some("Engineering Lead"));
        // Both chunk references must survive the fold.
        let chunk_ids: std::collections::HashSet<_> =
            merged.edges.iter().flat_map(|e| e.evidence.iter().map(|c| c.chunk_id.clone())).collect();
        assert!(chunk_ids.contains("c2"));
        assert!(chunk_ids.contains("c5"));
    }

    #[test]
    fn consolidate_leaves_short_form_alone_when_target_is_ambiguous() {
        // Two "Mike X" candidates → "Mike" can't safely fold into
        // either. It stays as its own atom.
        let mut b1 = EntityExtractionResponse::default();
        b1.persons.push(PersonEntity {
            name: "Mike Torres".into(),
            affiliation: None,
            role: None,
            description: None,
            mentions: vec!["c1".into()],
        });
        b1.persons.push(PersonEntity {
            name: "Mike Smith".into(),
            affiliation: None,
            role: None,
            description: None,
            mentions: vec!["c2".into()],
        });
        b1.persons.push(PersonEntity {
            name: "Mike".into(),
            affiliation: None,
            role: None,
            description: None,
            mentions: vec!["c3".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1)], &mut failures);
        assert_eq!(
            merged.entities.len(),
            3,
            "ambiguous first-token does not fold"
        );
    }

    #[test]
    fn consolidate_resolves_short_form_participant_to_canonical_atom() {
        // Initiative cites a participant by short form; after
        // consolidation, the participant resolution should target
        // the canonical multi-token atom.
        let mut b1 = EntityExtractionResponse::default();
        b1.persons.push(PersonEntity {
            name: "Sarah Chen".into(),
            affiliation: None,
            role: None,
            description: None,
            mentions: vec!["c1".into()],
        });
        b1.initiatives.push(InitiativeEntity {
            name: "Q3 enterprise push".into(),
            status: None,
            participants: vec!["Sarah".into()], // short form
            description: None,
            mentions: vec!["c2".into()],
        });
        b1.persons.push(PersonEntity {
            name: "Sarah".into(),
            affiliation: None,
            role: None,
            description: None,
            mentions: vec!["c3".into()],
        });

        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, b1)], &mut failures);

        // Persons: Sarah Chen (Sarah folded in). Initiatives: Q3.
        assert_eq!(merged.entities.len(), 2);
        let init = merged
            .entities
            .iter()
            .find(|e| e.entity_type == EntityType::Initiative)
            .unwrap();
        let sarah = merged
            .entities
            .iter()
            .find(|e| e.entity_type == EntityType::Person)
            .unwrap();
        assert_eq!(init.participants, vec![sarah.id.clone()]);
        // No orphan failure — the alias path resolved the short form.
        assert!(failures
            .iter()
            .all(|f| f.kind != FailureKind::OrphanParticipant));
    }

    #[test]
    fn merge_records_orphan_participants_when_name_unseen() {
        let mut response = EntityExtractionResponse::default();
        response.initiatives.push(InitiativeEntity {
            name: "API migration".into(),
            status: None,
            participants: vec!["Phantom Person".into()], // not in any persons[]
            description: None,
            mentions: vec!["c1".into()],
        });

        let mut failures = Vec::new();
        let merged = merge_responses(vec![(7, response)], &mut failures);

        assert_eq!(merged.entities.len(), 1, "only initiative atom emitted");
        assert!(merged.entities[0].participants.is_empty());

        let orphans: Vec<_> = failures
            .iter()
            .filter(|f| f.kind == FailureKind::OrphanParticipant)
            .collect();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].batch_index, 7);
        assert!(orphans[0].reason.contains("Phantom Person"));
    }

    #[test]
    fn rewrite_mentions_maps_batch_index_to_global_chunk_id() {
        let batch = vec![chunk(1001, "a"), chunk(2002, "b"), chunk(3003, "c")];
        let mut response = EntityExtractionResponse::default();
        response.persons.push(PersonEntity {
            name: "X".into(),
            affiliation: None,
            role: None,
            description: None,
            // Mix of acceptable formats: bare integer, prefixed
            // label, bracketed label.
            mentions: vec!["1".into(), "Memory 2".into(), "[Conversation 3]".into()],
        });
        rewrite_mentions(&mut response, &batch);
        assert_eq!(
            response.persons[0].mentions,
            vec!["1001".to_string(), "2002".to_string(), "3003".to_string()]
        );
    }

    #[test]
    fn rewrite_mentions_drops_out_of_range_labels() {
        let batch = vec![chunk(7, "a")];
        let mut response = EntityExtractionResponse::default();
        response.persons.push(PersonEntity {
            name: "X".into(),
            affiliation: None,
            role: None,
            description: None,
            mentions: vec!["1".into(), "5".into(), "0".into(), "not-a-number".into()],
        });
        rewrite_mentions(&mut response, &batch);
        // Only "1" → batch[0].id (=7) is valid.
        assert_eq!(response.persons[0].mentions, vec!["7".to_string()]);
    }

    #[test]
    fn merge_dedupes_repeat_mentions_into_single_edge() {
        let mut response = EntityExtractionResponse::default();
        response.persons.push(PersonEntity {
            name: "Sarah".into(),
            affiliation: None,
            role: None,
            description: None,
            // Same chunk twice — should produce a single Involves edge.
            mentions: vec!["c1".into(), "c1".into(), "c2".into()],
        });
        let mut failures = Vec::new();
        let merged = merge_responses(vec![(0, response)], &mut failures);
        assert_eq!(merged.edges.len(), 2);
    }

    #[tokio::test]
    async fn run_short_circuits_when_domain_opts_out() {
        // Inference is never called — the test passes precisely
        // because `entity_extraction_prompt` returning None bypasses
        // the dispatch loop. We hand it an InferenceFn that would
        // panic if invoked.
        struct OptOutDomain;
        impl Domain for OptOutDomain {
            fn id(&self) -> &str { "opt-out" }
            fn name(&self) -> &str { "Opt Out" }
            fn position_statuses(&self) -> &super::super::domain::PositionStatusVocab {
                static V: super::super::domain::PositionStatusVocab =
                    super::super::domain::PositionStatusVocab {
                        dominant: "x", minority: "x", contested: "x", settled: "x",
                    };
                &V
            }
            fn question_types(&self) -> &[super::super::domain::QuestionType] { &[] }
            fn overview_filter(&self) -> super::super::domain::ChunkFilter {
                super::super::domain::ChunkFilter::default()
            }
            fn skeleton_extraction_prompt(&self, _: &[&Chunk]) -> String { String::new() }
            fn cluster_labeling_prompt(&self, _: &[&Chunk]) -> String { String::new() }
            fn fault_line_detection_prompt(
                &self, _: &[&Chunk], _: &[&Chunk], _: &str, _: &str,
            ) -> String { String::new() }
            fn open_question_prompt(&self, _: &[&Chunk]) -> String { String::new() }
            fn clustering_config(&self) -> super::super::domain::ClusteringConfig {
                super::super::domain::ClusteringConfig {
                    min_cluster_size: 1, epsilon: 0.1, label_sample_size: 1,
                    max_cluster_points: 0, reduced_dims: 0,
                }
            }
            fn alignment_config(&self) -> super::super::domain::AlignmentConfig {
                super::super::domain::AlignmentConfig {
                    alignment_threshold: 0.5, min_chunks_for_discovery: 1,
                }
            }
            fn fault_line_config(&self) -> super::super::domain::FaultLineConfig {
                super::super::domain::FaultLineConfig {
                    proximity_threshold: 0.5, min_confidence: 0.5,
                }
            }
            fn skeleton_storage(&self) -> super::super::domain::SkeletonStorage {
                super::super::domain::SkeletonStorage::JsonAndLance
            }
            // entity_extraction_prompt: default (None)
        }

        let panicking_inference: InferenceFn = Arc::new(|_p: &str| {
            Box::pin(async {
                panic!("inference must not be called when domain opts out");
                #[allow(unreachable_code)]
                Ok(String::new())
            })
        });

        let chunks = vec![chunk(1, "hello"), chunk(2, "world")];
        let progress = |_: EnrichmentProgress| {};
        let result = run_entity_extraction(
            &chunks,
            &OptOutDomain,
            panicking_inference,
            &progress,
        )
        .await
        .expect("opt-out path returns Ok");
        assert!(result.entities.is_empty());
        assert!(result.edges.is_empty());
        assert_eq!(result.batches_run, 0);
    }
}
