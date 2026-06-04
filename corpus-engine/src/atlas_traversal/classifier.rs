//! Query classifier — natural-language query → `QueryPlan`.
//!
//! The classifier is a two-pass scanner:
//!
//! 1. **Intent.** A small set of keyword patterns maps to each
//!    `QueryPlan` variant. `"who is X"` / `"tell me about X"`
//!    → `EntityLookup`; `"how does X change"` / `"X's arc"`
//!    → `Trajectory`; `"relationship between X and Y"` /
//!    `"X and Y"` → `RelationLookup`; `"tensions"` /
//!    `"contradictions"` → `TensionList`; `"configuration"` /
//!    `"what does the work"` → `ConfigurationList`.
//! 2. **Target entity extraction.** For intents that reference
//!    a specific entity (EntityLookup, Trajectory, Relation), the
//!    classifier matches the query text against the atlas's
//!    known entity names + aliases — longest match wins. If the
//!    intent implies an entity but none is found, the plan still
//!    returns `EntityLookup { name: "<raw text>" }` so the
//!    traversal engine can try a fuzzy resolve (or return
//!    `UnknownEntity`).
//!
//! The classifier NEVER calls an LLM. A misclassification
//! produces `QueryPlan::Unknown { raw_query }` and the caller
//! decides whether to fall back.

use serde::{Deserialize, Serialize};

use crate::enrichment::atlas::atoms::Entity;
use crate::enrichment::atlas::fold;

/// The caller's parsed intent. Every variant that names a
/// target entity uses the `QueryTarget` sub-enum so downstream
/// code can distinguish "we matched an entity atom by name"
/// from "the query mentioned a name-like string we couldn't
/// resolve."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryPlan {
    /// "Who is Alyosha?" / "Tell me about Fyodor."
    EntityLookup { target: QueryTarget },
    /// "How does Alyosha change over the novel?"
    Trajectory { target: QueryTarget },
    /// "What's the relationship between Alyosha and Zossima?"
    RelationLookup {
        target_a: QueryTarget,
        target_b: QueryTarget,
    },
    /// "What tensions does the novel raise?"
    TensionList,
    /// "What does the novel argue as a whole?" / "What
    /// configurations does it enact?"
    ConfigurationList,
    /// "What does the novel say?" / "Summarise the atlas."
    CorpusOverview,
    /// Classifier couldn't match. The caller can fall back.
    Unknown { raw_query: String },
}

/// A named target — either an entity id matched from the atlas
/// vocabulary, or a raw string the caller extracted from the
/// query but could not match to an atom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryTarget {
    /// Matched an entity's canonical_name or alias.
    Resolved {
        entity_id: String,
        matched_form: String,
    },
    /// Classifier recognised something name-like in the query
    /// but couldn't match it to any atlas entity. Traversal may
    /// retry via the salience-aware resolver; if that also
    /// fails, the brief assembler surfaces "not in the atlas."
    Unresolved { raw_name: String },
}

/// Classify a query against the atlas's entity vocabulary.
/// `entities` is the resolved Entity set; the classifier uses
/// canonical_name + aliases to match target names in the query.
pub fn classify_query(query: &str, entities: &[Entity]) -> QueryPlan {
    // Fold the query the same way entity names are indexed — NFD
    // diacritic strip + Cyrillic transliteration + lowercase. This
    // makes the query `"Who is Fyodor?"` match the entity name
    // `"Fyódor Pavlóvič Karámazòv"` via a shared folded `fyodor`
    // token, which the plain-lowercase path misses.
    let lower = fold(query);
    let trimmed = lower.trim();

    // Corpus-level intents first (no target entity).
    if matches_any(
        trimmed,
        &[
            "what tensions",
            "tensions ",
            "contradictions",
            "disagreements",
        ],
    ) {
        return QueryPlan::TensionList;
    }
    if matches_any(
        trimmed,
        &[
            "what configurations",
            "configurations",
            "what does the novel as a whole",
            "what does the work as a whole",
            "overall pattern",
        ],
    ) {
        return QueryPlan::ConfigurationList;
    }
    if matches_any(
        trimmed,
        &[
            "summarise the atlas",
            "summarize the atlas",
            "what's in the atlas",
            "corpus overview",
        ],
    ) {
        return QueryPlan::CorpusOverview;
    }

    // Relation-lookup — needs two targets via "between X and Y"
    // or "X and Y" (only if both names resolve).
    if let Some((a, b)) = parse_relation_pair(&lower, entities) {
        return QueryPlan::RelationLookup {
            target_a: a,
            target_b: b,
        };
    }

    // Trajectory — "how does X change" / "X's arc" / "X's
    // development" / "how does X develop".
    if matches_any(
        trimmed,
        &[
            "how does ",
            "how did ",
            "'s arc",
            "' arc",
            "'s trajectory",
            "'s development",
            "how does she change",
            "how does he change",
            "change over",
        ],
    ) {
        if let Some(target) = match_entity_target(&lower, entities) {
            return QueryPlan::Trajectory { target };
        }
    }

    // EntityLookup — "who is X" / "tell me about X" / "what is X"
    // / bare name as query.
    if matches_any(
        trimmed,
        &[
            "who is ",
            "who are ",
            "tell me about",
            "what is ",
            "what are ",
            "describe ",
        ],
    ) {
        if let Some(target) = match_entity_target(&lower, entities) {
            return QueryPlan::EntityLookup { target };
        }
    }

    // Bare-name lookup — if the whole query matches an entity
    // name, treat it as EntityLookup.
    if let Some(target) = match_entity_target(&lower, entities) {
        return QueryPlan::EntityLookup { target };
    }

    QueryPlan::Unknown {
        raw_query: query.to_string(),
    }
}

fn matches_any(text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| text.contains(p))
}

/// Find the longest entity-name *whole-word* substring (canonical
/// or alias) in the query. "Longest match wins" so "Fyodor
/// Pavlovich Karamazov" takes precedence over the bare "Fyodor"
/// when both appear. Whole-word boundary prevents false positives
/// like matching the alias "Fyodor" inside the patronymic
/// "Fyodorovich". Returns `None` when no name is recognised.
fn match_entity_target(lower_query: &str, entities: &[Entity]) -> Option<QueryTarget> {
    // Pass 1 — full-name containment (longest match wins).
    // Catches `"who is Alyosha?"` when `Alyosha` is an alias, and
    // `"Fyodor Pavlovich Karamazov"` when the canonical is in
    // the query verbatim.
    let mut best: Option<(usize, &Entity, String)> = None;
    for e in entities {
        for name in std::iter::once(&e.canonical_name).chain(e.aliases.iter()) {
            let lname = fold(name);
            if lname.is_empty() {
                continue;
            }
            if contains_whole_word(lower_query, &lname) {
                let len = lname.chars().count();
                if best.as_ref().map(|(l, _, _)| len > *l).unwrap_or(true) {
                    best = Some((len, e, name.clone()));
                }
            }
        }
    }
    if let Some((_, e, matched_form)) = best {
        return Some(QueryTarget::Resolved {
            entity_id: e.id.as_str().to_string(),
            matched_form,
        });
    }

    // Pass 2 — token-level match. A query like `"Who is Fyodor?"`
    // has no single canonical/alias that fits inside it. Scan the
    // query's long tokens and look for ones that uniquely identify
    // an entity by appearing in its canonical/alias token set
    // (post-fold, length ≥ FUZZY_QUERY_TOKEN_MIN_LEN).
    //
    // Unique-match-wins: if a query token maps to more than one
    // entity (e.g. `karamazov` → all four Karamazovs), skip it.
    // The overall function returns None when no query token has
    // a single owner.
    let mut query_to_entity: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for e in entities {
        for name in std::iter::once(&e.canonical_name).chain(e.aliases.iter()) {
            let folded = fold(name);
            for token in folded.split_whitespace() {
                if token.len() < FUZZY_QUERY_TOKEN_MIN_LEN {
                    continue;
                }
                query_to_entity
                    .entry(token.to_string())
                    .or_default()
                    .insert(e.id.as_str().to_string());
            }
        }
    }
    for q_token in lower_query.split(|c: char| !c.is_alphanumeric()) {
        if q_token.len() < FUZZY_QUERY_TOKEN_MIN_LEN {
            continue;
        }
        let Some(owners) = query_to_entity.get(q_token) else {
            continue;
        };
        if owners.len() == 1 {
            let owner = owners.iter().next().unwrap();
            // Confirm the entity exists (it must — owners came
            // from the same iteration — but the guard keeps the
            // return path honest if `entities` shifts.)
            if entities.iter().any(|e| e.id.as_str() == owner) {
                return Some(QueryTarget::Resolved {
                    entity_id: owner.clone(),
                    matched_form: q_token.to_string(),
                });
            }
        }
    }
    None
}

/// Minimum folded-token length the classifier considers in its
/// token-level fallback. Matches the resolver's token floor so
/// short ambiguous names (`ivan`, `anna`) don't snap via the
/// fallback path either.
const FUZZY_QUERY_TOKEN_MIN_LEN: usize = 5;

/// True when `needle` appears in `haystack` with non-alphanumeric
/// boundaries on both sides (or string ends). Prevents the alias
/// "Fyodor" from matching inside "Fyodorovich" — a common false
/// positive when a corpus uses Russian patronymics heavily.
fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    find_whole_word(haystack, needle).is_some()
}

/// Like `contains_whole_word` but returns the match position for
/// callers that need it (e.g., the relation-pair overlap guard).
fn find_whole_word(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let is_boundary = |c: char| !c.is_alphanumeric();
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(needle) {
        let pos = start + rel;
        let before_ok = pos == 0
            || haystack[..pos]
                .chars()
                .last()
                .map(is_boundary)
                .unwrap_or(true);
        let end = pos + needle.len();
        let after_ok = end == haystack.len()
            || haystack[end..]
                .chars()
                .next()
                .map(is_boundary)
                .unwrap_or(true);
        if before_ok && after_ok {
            return Some(pos);
        }
        start = pos + 1;
    }
    None
}

/// Parse a "between X and Y" / "X and Y" pattern into two
/// resolved targets. Both sides must resolve to entities;
/// otherwise returns None so the classifier falls through to
/// a single-target path.
fn parse_relation_pair(
    lower_query: &str,
    entities: &[Entity],
) -> Option<(QueryTarget, QueryTarget)> {
    let has_between = lower_query.contains("between ");
    let has_and = lower_query.contains(" and ");
    let has_relationship = lower_query.contains("relationship") || lower_query.contains("relation");
    // A bare "X and Y" without any relationship marker is too
    // loose (it could be a list question). Require an explicit
    // relationship / between marker.
    if !has_and || (!has_between && !has_relationship) {
        return None;
    }
    // Enumerate all resolved entity matches in the query, then
    // pick the two with the largest name lengths whose matches
    // don't overlap textually. Whole-word guard prevents "Fyodor"
    // from matching inside "Fyodorovich"; overlap guard prevents
    // "Alyosha" and "Alyosha Karamazov" pairing on the same match.
    let mut matches: Vec<(usize, usize, &Entity, String)> = Vec::new(); // (start, end, entity, name)
    for e in entities {
        for name in std::iter::once(&e.canonical_name).chain(e.aliases.iter()) {
            // Same fold as `match_entity_target` — diacritic strip
            // + transliteration so the query side matches after
            // the same normalisation the atlas used for its index.
            let lname = fold(name);
            if lname.is_empty() {
                continue;
            }
            if let Some(pos) = find_whole_word(lower_query, &lname) {
                matches.push((pos, pos + lname.len(), e, name.clone()));
            }
        }
    }
    if matches.len() < 2 {
        return None;
    }
    // Sort by length descending, then pick the first two that
    // don't overlap.
    matches.sort_by(|a, b| (b.1 - b.0).cmp(&(a.1 - a.0)));
    let a = &matches[0];
    let b = matches.iter().skip(1).find(|m| m.0 >= a.1 || m.1 <= a.0)?;
    if a.2.id == b.2.id {
        // Same entity matched under two aliases — not a pair.
        return None;
    }
    Some((
        QueryTarget::Resolved {
            entity_id: a.2.id.as_str().to_string(),
            matched_form: a.3.clone(),
        },
        QueryTarget::Resolved {
            entity_id: b.2.id.as_str().to_string(),
            matched_form: b.3.clone(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, Entity};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    fn entity(idx: usize, canonical: &str, aliases: &[&str]) -> Entity {
        Entity {
            id: AtomId::entity(idx),
            canonical_name: canonical.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "".into(),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    fn bk_fixture() -> Vec<Entity> {
        vec![
            entity(1, "Alexei Fyodorovich Karamazov", &["Alyosha"]),
            entity(2, "Fyodor Pavlovich Karamazov", &["Fyodor"]),
            entity(3, "Zossima", &["Elder Zossima", "Father Zossima"]),
        ]
    }

    #[test]
    fn classifier_recognises_who_is_pattern() {
        let plan = classify_query("Who is Alyosha?", &bk_fixture());
        match plan {
            QueryPlan::EntityLookup {
                target:
                    QueryTarget::Resolved {
                        entity_id,
                        matched_form,
                    },
            } => {
                assert_eq!(entity_id, "entity-0001");
                assert_eq!(matched_form, "Alyosha");
            }
            other => panic!("expected EntityLookup, got {other:?}"),
        }
    }

    #[test]
    fn classifier_recognises_trajectory_pattern() {
        let plan = classify_query("How does Alyosha change over the novel?", &bk_fixture());
        matches!(plan, QueryPlan::Trajectory { .. })
            .then_some(())
            .expect("expected Trajectory plan");
    }

    #[test]
    fn classifier_prefers_longest_matching_name() {
        // "Fyodor Pavlovich Karamazov" must outrank "Fyodor" as
        // the match even though both substrings appear. Without
        // the longest-match rule, the bare "Fyodor" alias would
        // ambiguously land first.
        let plan = classify_query("Tell me about Fyodor Pavlovich Karamazov.", &bk_fixture());
        match plan {
            QueryPlan::EntityLookup {
                target: QueryTarget::Resolved { matched_form, .. },
            } => {
                assert_eq!(matched_form, "Fyodor Pavlovich Karamazov");
            }
            other => panic!("expected longest-form match, got {other:?}"),
        }
    }

    #[test]
    fn classifier_recognises_relation_between_pattern() {
        let plan = classify_query(
            "What's the relationship between Alyosha and Zossima?",
            &bk_fixture(),
        );
        match plan {
            QueryPlan::RelationLookup {
                target_a:
                    QueryTarget::Resolved {
                        entity_id: a,
                        matched_form: _,
                    },
                target_b:
                    QueryTarget::Resolved {
                        entity_id: b,
                        matched_form: _,
                    },
            } => {
                let ids: std::collections::HashSet<&str> =
                    [a.as_str(), b.as_str()].into_iter().collect();
                assert!(ids.contains("entity-0001"));
                assert!(ids.contains("entity-0003"));
            }
            other => panic!("expected RelationLookup, got {other:?}"),
        }
    }

    #[test]
    fn classifier_falls_through_to_unknown_when_nothing_matches() {
        let plan = classify_query("What was the weather in Petersburg?", &bk_fixture());
        assert!(matches!(plan, QueryPlan::Unknown { .. }));
    }

    #[test]
    fn classifier_maps_tension_keyword_to_tension_list() {
        let plan = classify_query("What tensions does the novel raise?", &bk_fixture());
        assert_eq!(plan, QueryPlan::TensionList);
    }

    #[test]
    fn classifier_maps_configuration_keyword_to_configuration_list() {
        let plan = classify_query("What configurations does the work enact?", &bk_fixture());
        assert_eq!(plan, QueryPlan::ConfigurationList);
    }

    #[test]
    fn classifier_resolves_bare_name_as_entity_lookup() {
        // A one-word query that happens to match an entity name
        // is treated as EntityLookup. This is the simplest
        // possible query and the most common ad-hoc probe.
        let plan = classify_query("Alyosha", &bk_fixture());
        assert!(matches!(
            plan,
            QueryPlan::EntityLookup {
                target: QueryTarget::Resolved { .. }
            }
        ));
    }

    #[test]
    fn classifier_token_fallback_matches_short_query_against_long_canonical() {
        // Real-world case from Brothers Karamazov: after Landing 5
        // merges drift variants, the Fyodor entity's canonical is
        // `Fyódor Pavlóvič Karámazòv` (plus long-form aliases).
        // The query `"Who is Fyodor?"` can't contain that long
        // string — pass 1 misses. Pass 2's token fallback matches
        // the query token `fyodor` against the entity's token set
        // and snaps (unique owner).
        let entities = vec![entity(
            1,
            "Fyódor Pavlóvič Karámazòv",
            &["Fyodor Pavlovitch Karamazov"],
        )];
        let plan = classify_query("Who is Fyodor?", &entities);
        match plan {
            QueryPlan::EntityLookup {
                target:
                    QueryTarget::Resolved {
                        entity_id,
                        matched_form,
                    },
            } => {
                assert_eq!(entity_id, "entity-0001");
                assert_eq!(matched_form, "fyodor");
            }
            other => panic!("expected EntityLookup via token fallback, got {other:?}"),
        }
    }

    #[test]
    fn classifier_token_fallback_bails_when_query_token_is_ambiguous() {
        // `"Karamazov"` alone appears in every sibling's tokens —
        // the token fallback must NOT snap to any one of them.
        // Pass 1 also can't match (query is short). Result:
        // Unknown.
        let entities = vec![
            entity(1, "Alexei Fyodorovich Karamazov", &[]),
            entity(2, "Dmitri Fyodorovich Karamazov", &[]),
            entity(3, "Ivan Fyodorovich Karamazov", &[]),
        ];
        let plan = classify_query("Who is Karamazov?", &entities);
        assert!(
            matches!(plan, QueryPlan::Unknown { .. }),
            "ambiguous token must not snap; got {plan:?}"
        );
    }

    #[test]
    fn classifier_requires_same_entity_on_both_sides_to_bail_on_relation() {
        // "Alyosha and Alexei Fyodorovich" — both resolve to
        // entity-0001 via different name forms. The classifier
        // should NOT emit a RelationLookup against the same
        // entity on both sides; it falls through to EntityLookup
        // on the longest match.
        let plan = classify_query(
            "What's the relationship between Alyosha and Alexei Fyodorovich?",
            &bk_fixture(),
        );
        // Either EntityLookup or Unknown is acceptable — the key
        // invariant is NO self-pair RelationLookup.
        assert!(!matches!(plan, QueryPlan::RelationLookup { .. }));
    }
}
