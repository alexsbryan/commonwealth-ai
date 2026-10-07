// SPDX-License-Identifier: AGPL-3.0-or-later
//! The focused per-declared-relation pass of Phase 1.
//!
//! The joint Phase-1 prompt asks for every facet at once, and on a list-dense
//! section it lists a declared relation's far ends short and unevenly: on
//! ft-ans-dev-b `sec_00008` it reached 5 of 10 attested mints for
//! `holds_coins_of`, varying run to run at T 0.1. One call per (section,
//! declared relation, entity of the `from` type the section introduced),
//! built from the declaration alone and asking only for the far ends with
//! their quotes, reached 10/10 on every run and never less than the joint
//! pass's typical elsewhere (feature-fidelity campaign, Decisions 2026-10-03
//! "Relation recall levers"; rows in `relation_focus_rows.json` beside the
//! harness). The prompt and schema are the ones that won: `focused` in
//! `research/ontology-retrieval/ontology-proof/ans/relation_focus.py`.
//!
//! What it adds is ordinary relation sketches — `[from entity, item]`, the
//! declared relation name, the item's quote as the anchor — appended to the
//! section's `relations_introduced` BEFORE the post-process chain, so they are
//! anchor-snapped like the joint pass's, checkpointed and cached with the
//! section, and read by resolution through the one path it already has.
//!
//! Calls per section = for each declared relation with BOTH ends, the number
//! of distinct (folded) names among the section's `entities_introduced` whose
//! type is the `from` type or `specializes` it. A corpus that declares no such
//! relation builds an empty [`RelationFocus`] and makes zero calls: invariant
//! I1 held by data, not by a branch someone has to remember.
//!
//! Idempotent by construction: an item already present (same declared type,
//! same participants after [`fold`]) is dropped, so applying the pass to its
//! own output appends nothing.

use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

use serde::Deserialize;
use understanding_atlas::enrichment::atlas::fold;

use super::super::atlas::{RelationSketch, SectionExtraction};
use super::super::prompts::load_or_baked;
use super::super::types::{ChatPrompt, InferenceFn};
use super::literary::prepare_phase_json;
use crate::enrichment::ontology::{OntologyPolicies, TypeIndex, TypeKind};
use crate::error::{Error, Result};

/// The cap on items one call may return — the harness's `maxItems`.
pub const MAX_ITEMS: usize = 40;

/// `response_format.json_schema.name` for the focused call.
pub const SCHEMA_NAME: &str = "relation_focus";

/// Placeholders `{relation}`, `{from}`, `{to}`, `{description}`. The last is
/// filled with a leading space, or nothing when the declaration has no
/// description, so the line never ends in a stray space.
static SYSTEM_TEMPLATE: LazyLock<&'static str> = LazyLock::new(|| {
    load_or_baked(
        "configurable_atlas/relation_focus_system.md",
        include_str!("configurable_atlas_prompts/relation_focus_system.md"),
    )
});

/// One declared relation whose `from` and `to` are both declared (directly or
/// through `specializes`, as [`TypeIndex::endpoints`] reads them — the same
/// accessor resolution's `check_relation_endpoints` enforces).
#[derive(Debug, Clone)]
struct FocusedRelation {
    name: String,
    from: String,
    to: String,
    description: String,
    /// The sketch label: the declared `label`, else the relation name.
    label: String,
    /// Every entity type that stands at the `from` end: `from` itself and
    /// each declared type that `specializes` it.
    from_types: BTreeSet<String>,
}

/// The declared relations a section is asked about one at a time. Built once
/// per Phase-1 run from the pipeline's declaration.
#[derive(Debug, Clone, Default)]
pub struct RelationFocus {
    relations: Vec<FocusedRelation>,
}

impl RelationFocus {
    pub fn from_policies(policies: &OntologyPolicies) -> Self {
        let index = TypeIndex::from_policies(policies);
        let relations: Vec<FocusedRelation> = policies
            .shape
            .types
            .iter()
            .filter(|t| t.kind == TypeKind::Relation)
            .filter_map(|t| {
                let [Some(from), Some(to)] = index.endpoints(&t.name) else {
                    tracing::debug!(
                        relation = %t.name,
                        "phase1.relation_focus: relation lacks a declared end; not asked"
                    );
                    return None;
                };
                let mut from_types: BTreeSet<String> = policies
                    .shape
                    .types
                    .iter()
                    .filter(|d| index.is_a(&d.name, from))
                    .map(|d| d.name.clone())
                    .collect();
                from_types.insert(from.to_string());
                Some(FocusedRelation {
                    name: t.name.clone(),
                    from: from.to_string(),
                    to: to.to_string(),
                    description: t.description.trim().to_string(),
                    label: t
                        .label
                        .as_deref()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .unwrap_or(&t.name)
                        .to_string(),
                    from_types,
                })
            })
            .collect();
        if !relations.is_empty() {
            tracing::debug!(
                relations = ?relations.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
                "phase1.relation_focus: declared relations with both ends are asked per section"
            );
        }
        Self { relations }
    }

    /// `(relation index, from-entity name)` for every call this section owes,
    /// one per distinct folded name so a repeated sketch is asked once.
    fn targets(&self, sx: &SectionExtraction) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for (i, rel) in self.relations.iter().enumerate() {
            let mut seen: HashSet<String> = HashSet::new();
            for e in &sx.entities_introduced {
                let name = e.canonical_name.trim();
                if name.is_empty() || !rel.from_types.contains(e.entity_type.as_str_repr()) {
                    continue;
                }
                if seen.insert(fold(name)) {
                    out.push((i, name.to_string()));
                }
            }
        }
        out
    }

    /// The focused prompt for one relation and one `from` entity. The section
    /// is the Phase-1 call's own user body, and the call inherits that
    /// prompt's phase id, temperature and thinking budget, so the client
    /// routes and samples it exactly as it does Phase 1.
    fn prompt(rel: &FocusedRelation, from_name: &str, phase1: &ChatPrompt) -> ChatPrompt {
        let description = if rel.description.is_empty() {
            String::new()
        } else {
            format!(" {}", rel.description)
        };
        // Author prose goes in last so it is never re-scanned for placeholders.
        let system = SYSTEM_TEMPLATE
            .trim_end()
            .replace("{relation}", &rel.name)
            .replace("{from}", &rel.from)
            .replace("{to}", &rel.to)
            .replace("{description}", &description);
        let user = format!("The {}: {from_name}\n\n{}", rel.from, phase1.user);
        let mut prompt =
            ChatPrompt::new(system, user).with_response_schema(SCHEMA_NAME, response_schema());
        prompt.phase_id = phase1.phase_id.clone();
        prompt.temperature = phase1.temperature;
        prompt.thinking_tokens = phase1.thinking_tokens;
        prompt
    }

    /// Ask every focused call this section owes and append what comes back.
    ///
    /// A failed call or unreadable response is warned and counted; the
    /// section keeps the joint pass's relations, which are a complete Phase-1
    /// result on their own — the focused pass only ever adds.
    pub async fn apply(&self, chat: &InferenceFn, phase1: &ChatPrompt, sx: &mut SectionExtraction) {
        if self.relations.is_empty() {
            return;
        }
        let targets = self.targets(sx);
        let mut seen: HashSet<(String, Vec<String>)> = sx
            .relations_introduced
            .iter()
            .filter_map(|r| r.relation_type.as_deref().map(|t| key(t, &r.participants)))
            .collect();
        let (mut returned, mut appended, mut duplicates, mut blank, mut failed) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        for (i, from_name) in &targets {
            let rel = &self.relations[*i];
            let prompt = Self::prompt(rel, from_name, phase1);
            let items = match chat(&prompt, None).await.and_then(|raw| parse_items(&raw)) {
                Ok(items) => items,
                Err(e) => {
                    tracing::warn!(
                        section = %sx.section_id,
                        relation = %rel.name,
                        from = %from_name,
                        error = %e,
                        "phase1.relation_focus: call failed; the joint pass's relations stand"
                    );
                    failed += 1;
                    continue;
                }
            };
            returned += items.len();
            for item in items {
                let name = item.name.trim();
                if name.is_empty() {
                    blank += 1;
                    continue;
                }
                let participants = vec![from_name.clone(), name.to_string()];
                if !seen.insert(key(&rel.name, &participants)) {
                    duplicates += 1;
                    continue;
                }
                sx.relations_introduced.push(RelationSketch {
                    participants,
                    label: rel.label.clone(),
                    anchor: item.anchor.trim().to_string(),
                    relation_type: Some(rel.name.clone()),
                    attributes: serde_json::Map::new(),
                });
                appended += 1;
            }
        }
        tracing::debug!(
            section = %sx.section_id,
            asked = targets.len(),
            returned,
            appended,
            duplicates,
            blank,
            failed,
            "phase1.relation_focus"
        );
    }
}

/// The dedup key: declared type plus folded participants, in order.
fn key(relation: &str, participants: &[String]) -> (String, Vec<String>) {
    (
        relation.to_string(),
        participants.iter().map(|p| fold(p)).collect(),
    )
}

#[derive(Deserialize)]
struct FocusResponse {
    items: Vec<FocusItem>,
}

#[derive(Deserialize)]
struct FocusItem {
    name: String,
    /// Required, as the schema requires it: an item with no quote is not the
    /// answer that was asked for, so the call fails rather than defaulting.
    anchor: String,
}

fn parse_items(raw: &str) -> Result<Vec<FocusItem>> {
    let json = prepare_phase_json(raw, "phase 1 relation focus")?;
    let mut items = serde_json::from_str::<FocusResponse>(&json)
        .map_err(|e| Error::Serialization(format!("phase 1 relation focus response: {e}")))?
        .items;
    if items.len() > MAX_ITEMS {
        tracing::debug!(
            returned = items.len(),
            cap = MAX_ITEMS,
            "phase1.relation_focus: response past the schema cap; truncated"
        );
        items.truncate(MAX_ITEMS);
    }
    Ok(items)
}

/// `{items: [{name, anchor}]}`, strict, capped — the harness's schema.
fn response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["items"],
        "properties": {
            "items": {
                "type": "array",
                "maxItems": MAX_ITEMS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["name", "anchor"],
                    "properties": {
                        "name": {"type": "string"},
                        "anchor": {"type": "string"}
                    }
                }
            }
        }
    })
}
