// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign awareness filter` — second-pass model filter that
//! drops extracted initiatives that look like work artifacts
//! ("Migration plan", "usage-based proposal") rather than stable,
//! multi-conversation strategic efforts.
//!
//! ## Why a second pass
//!
//! The per-batch entity-extraction prompt has limited cross-batch
//! context — each batch sees its own ≤4 chunks, so the model can't
//! tell that "Migration plan" appears in only 2 chunks while "API
//! migration" appears in 7. The merged atlas surfaces those
//! chunk-counts directly, so a focused filter prompt can use them
//! as a load-bearing signal.
//!
//! The filter prompt is much smaller than the extraction prompt:
//! one line per candidate with its chunk count + participant count.
//! Classification ("keep or drop?") is a tighter cognitive load
//! than structured extraction, so the model performs better.
//!
//! ## What it touches
//!
//! Reads `atlas/atoms.json` + `atlas/edges.json` from each
//! relational atlas dir, builds the candidate list, calls inference,
//! parses the keep set, then rewrites the atlas with dropped
//! initiatives + their orphaned edges removed. References to dropped
//! initiatives in surviving entities' `participants` field are
//! cleaned up too.

use std::collections::{HashMap, HashSet};

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, Entity};
use corpus_engine::enrichment::atlas::edges::Edge;
use corpus_engine::enrichment::atlas::writer::{
    read_atlas_atoms, read_atlas_edges, write_atlas, ATLAS_DIRNAME,
};
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use corpus_engine::enrichment::pipeline::ChatPrompt;
use serde_json::json;

use super::args::{get_flag, has_flag, split_args};
use super::render::display_path;
use super::store_open::{atlas_dir_for, sovereign_root};
use crate::enrich_cmd::inference_client::{
    probe_daemon, resolve_default_models, DaemonInferenceClient,
};
use crate::util::urls::{v1_url, DEFAULT_CLIENT_PORT};

const RELATIONAL_VIEWS: &[&str] = &["personal-knowledge", "conversation-history"];

pub(super) async fn cmd_filter(args: &[String]) -> i32 {
    let (_pos, flags) = split_args(args);
    let verbose = has_flag(&flags, "verbose");
    let dry_run = has_flag(&flags, "dry-run");

    // The filter pass uses grammar-constrained generation
    // (`response_format: json_schema`) to force the model to emit a
    // decision per candidate. The InferenceFn API can't carry the
    // schema, so the filter talks to DaemonInferenceClient directly.
    let client = match build_filter_client(&flags).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("awareness filter: {e}");
            return 1;
        }
    };

    let root = sovereign_root(&flags);
    let mut total_kept = 0usize;
    let mut total_dropped = 0usize;
    let mut atlases_seen = 0usize;

    for view_id in RELATIONAL_VIEWS {
        let atlas_dir = atlas_dir_for(&root, view_id);
        if !atlas_dir.exists() {
            continue;
        }
        atlases_seen += 1;

        println!();
        println!("─── {} ───", view_id);
        match filter_atlas(&atlas_dir, &client, verbose, dry_run).await {
            Ok(report) => {
                report.print();
                total_kept += report.kept.len();
                total_dropped += report.dropped.len();
            }
            Err(e) => {
                eprintln!("awareness filter: {} failed: {e}", display_path(&atlas_dir));
                return 1;
            }
        }
    }

    if atlases_seen == 0 {
        eprintln!(
            "awareness filter: no atlases found at {}/indexes/* — \
             run `awareness extract` first",
            display_path(&root)
        );
        return 1;
    }

    println!();
    println!(
        "Total: {} initiative{} kept, {} dropped{}",
        total_kept,
        if total_kept == 1 { "" } else { "s" },
        total_dropped,
        if dry_run {
            " (dry-run; no atlas changes)"
        } else {
            ""
        }
    );
    0
}

#[derive(Debug)]
struct FilterReport {
    kept: Vec<String>,
    dropped: Vec<String>,
    no_op_reason: Option<&'static str>,
}

impl FilterReport {
    fn no_op(reason: &'static str) -> Self {
        Self {
            kept: Vec::new(),
            dropped: Vec::new(),
            no_op_reason: Some(reason),
        }
    }

    fn print(&self) {
        if let Some(reason) = self.no_op_reason {
            println!("  (skipped: {})", reason);
            return;
        }
        if !self.kept.is_empty() {
            println!("  Kept ({}):", self.kept.len());
            for n in &self.kept {
                println!("    ✓ {n}");
            }
        }
        if !self.dropped.is_empty() {
            println!("  Dropped ({}):", self.dropped.len());
            for n in &self.dropped {
                println!("    ✗ {n}");
            }
        }
    }
}

async fn build_filter_client(flags: &[(String, String)]) -> Result<DaemonInferenceClient, String> {
    let base_url = get_flag(flags, "daemon-url")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            v1_url(DEFAULT_CLIENT_PORT)
                .trim_end_matches("/v1")
                .to_string()
        });

    if !probe_daemon(&base_url).await {
        return Err(format!(
            "daemon not reachable at {base_url}/v1/models — start it with \
             `sovereign daemon run`"
        ));
    }

    let chat_model = match get_flag(flags, "model").filter(|s| !s.is_empty()) {
        Some(m) => m,
        None => {
            let (chat, _embed) = resolve_default_models(&base_url).await;
            chat.ok_or_else(|| {
                format!(
                    "could not auto-select a chat model from {base_url}/v1/models; \
                     pass --model <id>"
                )
            })?
        }
    };
    let embed_model = "_unused_for_filter".to_string();

    // Output budget: 8 tokens of JSON-array decision per candidate is
    // generous; 4096 covers up to ~500 candidates. The schema makes
    // the model deterministic about output shape, but it can still
    // think; tokens spent on `<think>` count too.
    let max_output_tokens: u32 = get_flag(flags, "max-tokens")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(4096);

    eprintln!(
        "awareness filter: daemon at {base_url}, model = {chat_model}, max_tokens = {max_output_tokens}"
    );

    DaemonInferenceClient::new(base_url, chat_model, embed_model)
        .map(|c| c.with_max_output_tokens(max_output_tokens))
        .map_err(|e| format!("build daemon client: {e}"))
}

async fn filter_atlas(
    atlas_dir: &std::path::Path,
    client: &DaemonInferenceClient,
    verbose: bool,
    dry_run: bool,
) -> Result<FilterReport, String> {
    let atoms = read_atlas_atoms(atlas_dir).map_err(|e| format!("read atoms: {e}"))?;
    let edges = read_atlas_edges(atlas_dir).map_err(|e| format!("read edges: {e}"))?;

    // Chunk count per entity = number of Involves edges targeting it.
    let mut chunk_counts: HashMap<AtomId, usize> = HashMap::new();
    for edge in &edges.edges {
        *chunk_counts.entry(edge.target.clone()).or_insert(0) += 1;
    }

    // Initiative candidates with stable order (atoms.json order).
    let mut candidates: Vec<&Entity> = Vec::new();
    for atom in &atoms.atoms {
        if let AtomEnvelope::Entity(e) = atom {
            if matches!(e.entity_type, EntityType::Initiative) {
                candidates.push(e);
            }
        }
    }

    // Nothing to do if there are 0 or 1 candidates — the filter only
    // earns its keep when there's a comparison to make. (1 candidate
    // by itself is also kept regardless because we don't have signal
    // to drop it.)
    if candidates.len() < 2 {
        return Ok(FilterReport::no_op(if candidates.is_empty() {
            "no initiative atoms to audit"
        } else {
            "only 1 initiative candidate — nothing to compare against"
        }));
    }

    let user_prompt = build_filter_prompt(&candidates, &chunk_counts);
    let schema = decisions_schema(candidates.len());
    let chat_prompt = ChatPrompt::new("", &user_prompt)
        .with_response_schema("initiative_filter_decisions", schema);
    if verbose {
        eprintln!("─── filter prompt ───────────────────────────────────");
        eprintln!("{user_prompt}");
        eprintln!("─── filter schema ───────────────────────────────────");
        eprintln!(
            "{}",
            serde_json::to_string_pretty(chat_prompt.response_schema.as_ref().unwrap())
                .unwrap_or_default()
        );
        eprintln!("─────────────────────────────────────────────────────");
    }

    let response = client
        .complete(&chat_prompt)
        .await
        .map_err(|e| format!("inference: {e}"))?;
    if verbose {
        eprintln!("─── filter response ─────────────────────────────────");
        eprintln!("{response}");
        eprintln!("─────────────────────────────────────────────────────");
    }

    let keep_set = parse_keep_set(&response, candidates.len())?;

    // Drop set = candidates not in keep set.
    let mut dropped_ids: HashSet<AtomId> = HashSet::new();
    let mut kept_names: Vec<String> = Vec::new();
    let mut dropped_names: Vec<String> = Vec::new();
    for (i, e) in candidates.iter().enumerate() {
        if keep_set.contains(&(i + 1)) {
            kept_names.push(e.canonical_name.clone());
        } else {
            dropped_ids.insert(e.id.clone());
            dropped_names.push(e.canonical_name.clone());
        }
    }

    if dropped_ids.is_empty() || dry_run {
        return Ok(FilterReport {
            kept: kept_names,
            dropped: dropped_names,
            no_op_reason: None,
        });
    }

    // Rewrite atlas — keep all non-Entity atoms, all Entity atoms
    // whose id is not in dropped_ids; drop edges whose target is a
    // dropped initiative; clean stale participant references on
    // surviving Entity atoms.
    let mut new_entities: Vec<Entity> = Vec::with_capacity(atoms.atoms.len());
    for atom in atoms.atoms {
        if let AtomEnvelope::Entity(mut e) = atom {
            if dropped_ids.contains(&e.id) {
                continue;
            }
            e.participants.retain(|p| !dropped_ids.contains(p));
            new_entities.push(e);
        }
        // Note: this view only emits Entity atoms today. If that
        // changes the writer below would silently drop other types;
        // assert in test rather than handle here.
    }
    let new_edges: Vec<Edge> = edges
        .edges
        .into_iter()
        .filter(|edge| !dropped_ids.contains(&edge.target))
        .collect();

    write_atlas(atlas_dir, &new_entities, &[], &new_edges)
        .map_err(|e| format!("write atlas: {e}"))?;

    Ok(FilterReport {
        kept: kept_names,
        dropped: dropped_names,
        no_op_reason: None,
    })
}

/// Strict JSON Schema for the filter response: an object with one
/// `decisions` array whose length must equal the candidate count.
/// llama.cpp's grammar-constrained sampler enforces this at decode
/// time, so the model literally cannot emit a partial list.
fn decisions_schema(total: usize) -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["decisions"],
        "properties": {
            "decisions": {
                "type": "array",
                "minItems": total,
                "maxItems": total,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["index", "keep"],
                    "properties": {
                        "index": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": total
                        },
                        "keep": {
                            "type": "boolean"
                        }
                    }
                }
            }
        }
    })
}

fn build_filter_prompt(candidates: &[&Entity], chunk_counts: &HashMap<AtomId, usize>) -> String {
    let mut lines = String::new();
    for (i, e) in candidates.iter().enumerate() {
        let chunks = chunk_counts.get(&e.id).copied().unwrap_or(0);
        lines.push_str(&format!(
            "{}. {} — {} conversation{}, {} participant{}\n",
            i + 1,
            e.canonical_name,
            chunks,
            if chunks == 1 { "" } else { "s" },
            e.participants.len(),
            if e.participants.len() == 1 { "" } else { "s" },
        ));
    }
    let total = candidates.len();
    format!(
        r#"You are auditing a list of extracted initiatives from a personal
awareness pipeline. For each candidate, decide whether it is a *real
initiative* or a *work artifact* that should be dropped.

An *initiative* is a stable, named strategic effort the user
organizes work around. Examples: "API migration", "Q3 enterprise
push", "HIPAA compliance review", "Architecture refresh", "Platform
migration", "Vendor consolidation". Note: an initiative does NOT
need to appear in many conversations — sometimes the canonical name
only surfaces once while related work artifacts dominate the chunks.
Chunk count is a *positive* signal but not a *required* one.

A *work artifact* is a deliverable, draft, sub-task, scoping doc,
or interim work product *within* an initiative. Examples (DROP):
"the migration plan", "the SOC2 crosswalk", "usage-based proposal",
"discovery scope", "SOW reformatting", "migration plan revision",
"Migration team", "the patient portal launch" (if it's a sub-effort
of a parent initiative).

KEEP a candidate if any of:
- Cited in 2+ conversations
- Has at least 1 named participant
- Name reads like a *canonical initiative phrase* — a clean noun
  phrase describing a strategic effort: a project (X migration,
  X refresh, X consolidation), a goal (Q3 push, churn reduction),
  or a compliance/review program (HIPAA compliance review, SOC2
  audit). Such names are KEEP even at 1 conversation.

DROP a candidate ONLY if:
- It's a clear *variant of another candidate* — a possessive prefix
  like "Helios HIPAA review" when "HIPAA compliance review" is also
  in the list, or a scope suffix like "Architecture refresh at
  Meridian" when "Architecture refresh" is in the list, or a short
  form like "Q3 push" when "Q3 enterprise push" is in the list.
  In these cases drop the qualified/variant form and let the
  canonical entry win.
- It's a one-off deliverable name: a draft, plan, scope, crosswalk,
  proposal, revision, reformatting, team — i.e. a noun phrase that
  describes work output rather than a strategic frame.
- It reads as a generic noun ("Migration team", "the project").

Casing of the first letter is NOT a signal — emit-side
inconsistency is common; treat "architecture refresh" and
"Architecture refresh" as equally valid initiatives.

When in doubt between KEEP and DROP, prefer KEEP. The upstream
extraction is conservative; over-filtering drops real strategic
context that downstream digest needs.

Candidates ({total} total):
{lines}
Decide for EVERY candidate. Output ONLY a JSON array with exactly
{total} entries, in the order listed above, each entry naming the
1-indexed position and a boolean keep flag. No prose, no preamble.

Required shape (length must equal {total}):
{{"decisions": [
  {{"index": 1, "keep": true}},
  {{"index": 2, "keep": true}},
  ...
  {{"index": {total}, "keep": false}}
]}}"#,
        total = total,
        lines = lines
    )
}

/// Parse the filter response into a 1-indexed keep set.
///
/// Accepts both shapes for robustness:
///   * Strict: `{"decisions": [{"index": N, "keep": bool}, ...]}` —
///     the prompt asks for this. We require the array length to
///     equal `total` *minus tolerance*; under that, surface a parse
///     error so the caller can refuse to silently drop entries the
///     model didn't evaluate.
///   * Legacy: `{"keep": [1, 3, ...]}` — earlier prompt shape; if
///     a smaller fast model regresses to this we still parse it.
fn parse_keep_set(response: &str, total: usize) -> Result<HashSet<usize>, String> {
    let json_str = extract_json_object(response);
    let v: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| format!("parse filter response: {e} — body: {response}"))?;

    // Strict path: `decisions` array of {index, keep}.
    if let Some(arr) = v.get("decisions").and_then(|x| x.as_array()) {
        // Tolerate up to 1 missing entry (occasional truncation), but
        // bail if the model emitted half a list — we'd be silently
        // dropping evaluated entries.
        if arr.len() + 1 < total {
            return Err(format!(
                "decisions array has {} entries; expected {} \
                 (model likely truncated mid-list)",
                arr.len(),
                total
            ));
        }
        let mut set: HashSet<usize> = HashSet::new();
        for entry in arr {
            let idx = entry
                .get("index")
                .and_then(|x| x.as_u64())
                .map(|n| n as usize);
            let keep = entry.get("keep").and_then(|x| x.as_bool()).unwrap_or(false);
            if let Some(idx) = idx {
                if keep && idx >= 1 && idx <= total {
                    set.insert(idx);
                }
            }
        }
        // If `decisions` was present but we extracted no kept indices
        // even though the array had entries, that's a real outcome —
        // the model decided everything is artifacts. We surface it
        // upstream so the user sees the full drop.
        return Ok(set);
    }

    // Legacy path: `keep` array of integers.
    if let Some(arr) = v.get("keep").and_then(|x| x.as_array()) {
        let mut set: HashSet<usize> = HashSet::new();
        for entry in arr {
            if let Some(n) = entry.as_u64() {
                let n = n as usize;
                if n >= 1 && n <= total {
                    set.insert(n);
                }
            }
        }
        return Ok(set);
    }

    Err(format!(
        "filter response missing `decisions` or `keep`: {response}"
    ))
}

/// Pull the outermost {…} object from a model response, tolerating
/// `<think>` blocks, prose wrapper, and fenced code blocks.
fn extract_json_object(text: &str) -> &str {
    let text = text.trim();
    if let Some(end) = text.find("</think>") {
        let after = &text[end + "</think>".len()..];
        return extract_json_object_inner(after);
    }
    extract_json_object_inner(text)
}

fn extract_json_object_inner(text: &str) -> &str {
    let text = text.trim();
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return text[start..=end].trim();
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keep_set_handles_decisions_shape() {
        let resp = r#"{"decisions":[
            {"index":1,"keep":true},
            {"index":2,"keep":false},
            {"index":3,"keep":true}
        ]}"#;
        let s = parse_keep_set(resp, 3).unwrap();
        assert_eq!(s, [1, 3].iter().copied().collect());
    }

    #[test]
    fn parse_keep_set_strips_think_block_and_fences_with_decisions() {
        let resp = "<think>let me decide…</think>\n```json\n{\"decisions\":[{\"index\":1,\"keep\":true},{\"index\":2,\"keep\":false},{\"index\":3,\"keep\":false},{\"index\":4,\"keep\":true},{\"index\":5,\"keep\":false}]}\n```\n";
        let s = parse_keep_set(resp, 5).unwrap();
        assert_eq!(s, [1, 4].iter().copied().collect());
    }

    #[test]
    fn parse_keep_set_falls_back_to_legacy_keep_shape() {
        // Smaller models sometimes regress to the simpler keep-array
        // shape; still parse it so the filter doesn't fail outright.
        let s = parse_keep_set(r#"{"keep":[1,3,5]}"#, 5).unwrap();
        assert_eq!(s, [1, 3, 5].iter().copied().collect());
    }

    #[test]
    fn parse_keep_set_rejects_truncated_decisions_list() {
        // Model emitted only 2 of 5 decisions — the remaining 3 are
        // unevaluated. Surface as an error rather than silently
        // dropping them.
        let resp = r#"{"decisions":[{"index":1,"keep":true},{"index":2,"keep":true}]}"#;
        let result = parse_keep_set(resp, 5);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("truncated"), "expected truncation hint: {msg}");
    }

    #[test]
    fn parse_keep_set_tolerates_one_missing_decision() {
        // Edge case: 4 of 5 decisions emitted. We tolerate this so
        // the filter doesn't fail on a single drop. The unevaluated
        // candidate's index won't appear in the kept set, which
        // means it gets dropped — slightly more aggressive than
        // ideal, but conservative enough at this scale.
        let resp = r#"{"decisions":[
            {"index":1,"keep":true},
            {"index":2,"keep":true},
            {"index":3,"keep":false},
            {"index":4,"keep":true}
        ]}"#;
        let s = parse_keep_set(resp, 5).unwrap();
        assert_eq!(s, [1, 2, 4].iter().copied().collect());
    }

    #[test]
    fn parse_keep_set_drops_out_of_range_indices_in_decisions() {
        let resp = r#"{"decisions":[
            {"index":1,"keep":true},
            {"index":99,"keep":true},
            {"index":0,"keep":true}
        ]}"#;
        let s = parse_keep_set(resp, 3).unwrap();
        assert_eq!(s, [1].iter().copied().collect());
    }

    #[test]
    fn parse_keep_set_errors_when_neither_decisions_nor_keep() {
        assert!(parse_keep_set(r#"{"discard":[1]}"#, 3).is_err());
    }

    #[test]
    fn build_prompt_includes_chunk_and_participant_counts() {
        use corpus_engine::enrichment::atlas::atoms::ChunkRef;
        use corpus_engine::enrichment::pipeline::atlas::EnrichmentDepth;

        let entity = Entity {
            id: AtomId::entity(1),
            canonical_name: "API migration".to_string(),
            aliases: Vec::new(),
            entity_type: EntityType::Initiative,
            first_appearance: ChunkRef::new("c1".to_string(), None),
            description: String::new(),
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::extracted_default(),
            affiliation: None,
            role: None,
            participants: vec![AtomId::entity(2), AtomId::entity(3)],
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        };
        let mut counts = HashMap::new();
        counts.insert(entity.id.clone(), 5);
        let candidates = vec![&entity];
        let prompt = build_filter_prompt(&candidates, &counts);
        assert!(prompt.contains("1. API migration — 5 conversations, 2 participants"));
    }
}
