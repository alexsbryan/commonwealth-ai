// SPDX-License-Identifier: AGPL-3.0-or-later
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
use crate::enrichment::ontology::{OntologyPolicies, TypeIndex};

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
    /// "Which coins are in this catalogue?" / "List the sceattas."
    /// Only ever produced for a corpus that DECLARED `entity_type` —
    /// the author's own noun, not one of the six generic kinds.
    Enumerate { entity_type: String },
    /// "How many coins by metal?" — a declared type tallied over one of
    /// its declared attributes.
    Aggregate { entity_type: String, over: String },
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
///
/// Equivalent to [`classify_query_with`] with no declared vocabulary — the
/// pre-ontology behaviour, kept as the name every existing caller already
/// spells.
pub fn classify_query(query: &str, entities: &[Entity]) -> QueryPlan {
    classify_query_with(query, entities, None)
}

/// Classify a query, offering the corpus's DECLARED types as things the
/// question can be about.
///
/// `vocab` is the atlas's `ontology.json` policies, or `None` for a corpus
/// that declared nothing. **`None` reproduces [`classify_query`] exactly** —
/// the declared block below is the only added code and it is skipped whole,
/// so SEP, Wikipedia, Enron and every literary atlas classify byte-identically
/// (I5).
///
/// The declared block runs BEFORE the relation/lookup passes because a
/// declared noun is more specific evidence than a name-shaped token: "which
/// coins are in this catalogue" would otherwise fall through to the bare-name
/// pass and resolve on whatever entity happens to share a token with it.
pub fn classify_query_with(
    query: &str,
    entities: &[Entity],
    vocab: Option<&OntologyPolicies>,
) -> QueryPlan {
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

    // Declared-type intents. Skipped entirely when nothing is declared.
    if let Some(policies) = vocab {
        if let Some(plan) = classify_declared(trimmed, entities, policies) {
            tracing::debug!(query = %query, plan = ?plan, "atlas traversal: declared-type plan");
            return plan;
        }
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

/// Cues that a question wants a SET rather than one thing.
const ENUMERATE_CUES: &[&str] = &[
    "which ",
    "what ",
    "list ",
    "list the",
    "show me",
    "enumerate",
    "all the ",
    "how many",
];

/// Cues that a question wants a TALLY grouped by something.
const AGGREGATE_CUES: &[&str] = &["how many", "count of", "breakdown", "distribution of"];

/// Enumerate / Aggregate over a declared type, or `None` to fall through to
/// the pre-ontology passes.
///
/// The decisive test for Enumerate is the one `atom_enum`'s classifier already
/// uses — **a question that names the entities it concerns is a lookup, not an
/// enumeration** — with one correction the built wessex-hoard atlas forced.
///
/// A named entity is sometimes the CONTAINER the question scopes to rather
/// than a filter on the set. The probe question is "which coins are in the
/// Wessex Down hoard catalogue", and `Wessex Down hoard` is a real `place`
/// atom in that atlas, so the bare rule refused to enumerate the very question
/// the phase exists to answer. But the hoard is not a coin — it is the corpus,
/// and every coin in the atlas is in it, so naming it restricts nothing.
/// "which mints struck for Offa" is the other shape: Offa FILTERS the mints,
/// an unfiltered enumeration of every mint would be a wrong answer, and the
/// question is better served by falling through to the lookup that walks
/// Offa's relations. [`names_the_container`] is the distinction.
fn classify_declared(
    folded_query: &str,
    entities: &[Entity],
    policies: &OntologyPolicies,
) -> Option<QueryPlan> {
    let index = TypeIndex::from_policies(policies);
    let (type_name, decl, type_surface) = match_declared_type(folded_query, policies)?;

    // Aggregate is the more specific read: a tally cue plus "by <attribute>"
    // naming one of the type's EFFECTIVE attributes (inherited included, which
    // is why this asks the index rather than the decl).
    if matches_any(folded_query, AGGREGATE_CUES) {
        for attr in index.effective_attributes(&decl.name) {
            let folded_attr = fold(&attr.name);
            if folded_attr.is_empty() {
                continue;
            }
            if folded_query.contains(&format!("by {folded_attr}"))
                || folded_query.contains(&format!("per {folded_attr}"))
            {
                return Some(QueryPlan::Aggregate {
                    entity_type: type_name,
                    over: attr.name.clone(),
                });
            }
        }
    }

    if !matches_any(folded_query, ENUMERATE_CUES) {
        return None;
    }
    // The type word is SPENT. It named the TYPE, and it must not also be read
    // as naming a member — which it otherwise does, routinely: the built
    // wessex-hoard atlas holds entities called "Series Y sceattas", "English
    // coins" and "silver coinage", so the fuzzy token pass resolves the bare
    // word `sceattas` or `coins` to an atom and the enumeration is defeated by
    // the very noun that asked for it (observed at debug:
    // `named=sceattas entity_type=sceatta`). Strip it before asking what ELSE
    // the question names. A member named "Series Y sceattas" in full still
    // matches — the full form survives the strip and pass 1 sees it.
    let residue = folded_query.replacen(&type_surface, " ", 1);

    // Names something => a lookup about it, UNLESS what it names is the
    // container the question scopes to.
    if let Some(target) = match_entity_target(&residue, entities) {
        let named = match &target {
            QueryTarget::Resolved { matched_form, .. } => matched_form.clone(),
            QueryTarget::Unresolved { raw_name } => raw_name.clone(),
        };
        if !names_the_container(&residue, &named) {
            tracing::debug!(
                named = %named,
                entity_type = %type_name,
                "atlas traversal: named entity filters the set; not an enumeration"
            );
            return None;
        }
    }
    Some(QueryPlan::Enumerate {
        entity_type: type_name,
    })
}

/// Prepositions that introduce the thing a set is drawn FROM rather than a
/// property it is filtered BY.
const CONTAINMENT_PREPOSITIONS: &[&str] =
    &["in", "within", "inside", "from", "across", "throughout"];

/// Determiners that may sit between the preposition and the name. The empty
/// string covers "in Wessex Down hoard".
const CONTAINER_DETERMINERS: &[&str] = &["the ", "this ", "that ", "these ", "our ", ""];

/// Does `named` appear as the CONTAINER the question draws its set from
/// ("coins **in the** Wessex Down hoard") rather than as a filter on it
/// ("mints struck **for** Offa")?
///
/// Deliberately a preposition test and not a type test. The type test — "the
/// named entity is not itself of the enumerated type" — passes the hoard case
/// and FAILS the Offa case, because Offa is a `person` atom and a mint is not a
/// person either. What actually separates the two is the grammatical role the
/// name plays, and this classifier is a keyword + known-name matcher by design
/// (see the module doc); a preposition is the same kind of evidence it already
/// runs on.
fn names_the_container(folded_query: &str, named: &str) -> bool {
    let name = fold(named);
    if name.is_empty() {
        return false;
    }
    CONTAINMENT_PREPOSITIONS.iter().any(|prep| {
        CONTAINER_DETERMINERS
            .iter()
            .any(|det| folded_query.contains(&format!("{prep} {det}{name}")))
    })
}

/// The declared type the query mentions, by name or `label`, matched as a
/// whole word in singular or naive-plural form. Longest surface form wins so
/// `sceatta` beats a shorter type sharing a prefix.
type DeclaredMatch<'a> = (
    usize,
    String,
    &'a crate::enrichment::ontology::OntologyTypeDecl,
    String,
);

fn match_declared_type<'a>(
    folded_query: &str,
    policies: &'a OntologyPolicies,
) -> Option<(
    String,
    &'a crate::enrichment::ontology::OntologyTypeDecl,
    String,
)> {
    let mut best: Option<DeclaredMatch<'a>> = None;
    for t in &policies.shape.types {
        for base in std::iter::once(&t.name).chain(t.label.iter()) {
            let folded = fold(base);
            if folded.is_empty() {
                continue;
            }
            for surface in [folded.clone(), format!("{folded}s"), format!("{folded}es")] {
                if !contains_whole_word(folded_query, &surface) {
                    continue;
                }
                let len = surface.chars().count();
                if best.as_ref().map(|(l, _, _, _)| len > *l).unwrap_or(true) {
                    best = Some((len, t.name.clone(), t, surface));
                }
            }
        }
    }
    best.map(|(_, name, decl, surface)| (name, decl, surface))
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

    // ── ontology-v1 P5: declared-type plans ──────────────────

    use crate::recipe_templates::numismatics_policies as numismatics;

    /// The wessex-hoard enumeration probe. This is link 5 of the chain: the
    /// question has to reach the AUTHOR'S noun, not one of the six kinds.
    #[test]
    fn declared_type_enumeration_probe() {
        let plan = classify_query_with(
            "Which coins are in this catalogue, and what metal is each?",
            &[],
            Some(&numismatics()),
        );
        assert_eq!(
            plan,
            QueryPlan::Enumerate {
                entity_type: "coin".into()
            }
        );
    }

    /// Plural and label surface forms both reach the declared name.
    #[test]
    fn declared_enumeration_matches_plural_forms() {
        for q in [
            "Which sceattas are in the hoard?",
            "List the sceatta.",
            "show me all the sceattas",
        ] {
            assert_eq!(
                classify_query_with(q, &[], Some(&numismatics())),
                QueryPlan::Enumerate {
                    entity_type: "sceatta".into()
                },
                "query: {q}"
            );
        }
    }

    /// A named entity that FILTERS the set is a lookup about that entity, not
    /// an enumeration of the type. "which mints struck for Offa" is about
    /// Offa — an unfiltered list of every mint would be a wrong answer, and
    /// Offa's relations are the right one.
    #[test]
    fn a_named_filter_defeats_enumeration() {
        let entities = vec![entity(1, "Offa", &[])];
        let plan = classify_query_with(
            "Which mints struck for Offa?",
            &entities,
            Some(&numismatics()),
        );
        assert!(
            !matches!(plan, QueryPlan::Enumerate { .. }),
            "named member must not enumerate, got {plan:?}"
        );
        assert!(
            matches!(plan, QueryPlan::EntityLookup { .. }),
            "got {plan:?}"
        );
    }

    /// …but a named CONTAINER does not. This is the exact question the phase
    /// exists to answer, against the exact atom that broke the first cut of
    /// the rule: `Wessex Down hoard` is a real `place` entity in the BUILT
    /// wessex-hoard atlas, so "which coins are in the Wessex Down hoard
    /// catalogue" names it — and refusing to enumerate there refused the
    /// probe itself. The hoard is the corpus, not a coin.
    #[test]
    fn a_named_container_does_not_defeat_enumeration() {
        let mut hoard = entity(1, "Wessex Down hoard", &[]);
        hoard.entity_type = EntityType::Place;
        let entities = vec![hoard, entity(2, "Offa", &[])];
        for q in [
            "Which coins are in the Wessex Down hoard catalogue, and what metal is each?",
            "Which sceattas are in the Wessex Down hoard?",
            "List the coins from the Wessex Down hoard.",
        ] {
            let plan = classify_query_with(q, &entities, Some(&numismatics()));
            assert!(
                matches!(plan, QueryPlan::Enumerate { .. }),
                "a container must not defeat enumeration: {q} -> {plan:?}"
            );
        }
        // The filter case still defeats it, with the same entity set.
        assert!(!matches!(
            classify_query_with(
                "Which mints struck for Offa?",
                &entities,
                Some(&numismatics())
            ),
            QueryPlan::Enumerate { .. }
        ));
    }

    /// The type word is spent on the TYPE and must not also read as a member.
    ///
    /// Named inputs from the BUILT wessex-hoard atlas, where this was found:
    /// its coin set includes entities literally called "Series Y sceattas" and
    /// "English coins". Before the strip, "which sceattas are in the hoard"
    /// resolved `sceattas` to the first of those and refused to enumerate —
    /// the enumeration defeated by the very noun that asked for it.
    #[test]
    fn the_type_word_does_not_also_name_a_member() {
        let mut plural_named = entity(1, "Series Y sceattas", &["Series R", "sceatta"]);
        plural_named.entity_type = EntityType::Other("sceatta".into());
        let mut english_coins = entity(2, "English coins", &[]);
        english_coins.entity_type = EntityType::Other("coin".into());
        let mut hoard = entity(3, "Wessex Down hoard", &[]);
        hoard.entity_type = EntityType::Place;
        let entities = vec![plural_named, english_coins, hoard];

        assert_eq!(
            classify_query_with(
                "Which sceattas are in the hoard?",
                &entities,
                Some(&numismatics())
            ),
            QueryPlan::Enumerate {
                entity_type: "sceatta".into()
            }
        );
        assert_eq!(
            classify_query_with(
                "Which coins are in the hoard?",
                &entities,
                Some(&numismatics())
            ),
            QueryPlan::Enumerate {
                entity_type: "coin".into()
            }
        );
        // A member named IN FULL still defeats it — the full form survives the
        // strip, so this is not a licence to ignore real names.
        assert!(!matches!(
            classify_query_with(
                "Which coins relate to the English coins group?",
                &entities,
                Some(&numismatics())
            ),
            QueryPlan::Enumerate { .. }
        ));
    }

    /// A tally cue plus `by <declared attribute>` is an Aggregate.
    #[test]
    fn declared_aggregate_over_an_attribute() {
        assert_eq!(
            classify_query_with("How many coins by metal?", &[], Some(&numismatics())),
            QueryPlan::Aggregate {
                entity_type: "coin".into(),
                over: "metal".into()
            }
        );
        // Inherited attributes count: `sceatta` specializes `coin`, so
        // `metal` is one of its effective attributes.
        assert_eq!(
            classify_query_with("How many sceattas by metal?", &[], Some(&numismatics())),
            QueryPlan::Aggregate {
                entity_type: "sceatta".into(),
                over: "metal".into()
            }
        );
    }

    /// An attribute the type never declared is not an aggregate — it falls
    /// back to enumeration rather than inventing a grouping key.
    #[test]
    fn undeclared_attribute_is_not_an_aggregate() {
        assert_eq!(
            classify_query_with("How many coins by provenance?", &[], Some(&numismatics())),
            QueryPlan::Enumerate {
                entity_type: "coin".into()
            }
        );
    }

    /// I5, at the site. Every query the pre-ontology classifier placed is
    /// placed identically when no vocabulary is supplied — including ones a
    /// declared vocabulary WOULD have caught.
    #[test]
    fn undeclared_classification_is_unchanged() {
        for q in [
            "Who is Alyosha?",
            "How does Alyosha change over the novel?",
            "What tensions does the novel raise?",
            "What configurations does the work enact?",
            "What was the weather in Petersburg?",
            "Which coins are in this catalogue, and what metal is each?",
            "How many coins by metal?",
            "List the sceattas.",
        ] {
            let with_none = classify_query_with(q, &bk_fixture(), None);
            assert_eq!(with_none, classify_query(q, &bk_fixture()), "query: {q}");
            assert!(
                !matches!(
                    with_none,
                    QueryPlan::Enumerate { .. } | QueryPlan::Aggregate { .. }
                ),
                "undeclared corpus must never produce a declared-type plan: {q} -> {with_none:?}"
            );
        }
    }

    /// A policy set with prose but no declared types is not a vocabulary —
    /// the caller filters on `has_declarations()` and the classifier is then
    /// byte-identical to the undeclared path.
    #[test]
    fn prose_only_policies_declare_nothing() {
        let prose = OntologyPolicies::from_prose("extract the rules", Default::default());
        assert!(!prose.has_declarations());
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
