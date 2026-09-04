// SPDX-License-Identifier: AGPL-3.0-or-later
//! `recipe validate` for the ontology block (`ONTOLOGY_PRIMITIVES.md` §4).
//!
//! Three outputs, three meanings. **Errors** are references that do not
//! resolve, per-kind rules broken, and caps exceeded — the recipe will not
//! extract what the author meant. **Warnings** are keys nothing reads and
//! redundant term sources. **Notes** are the derived facets — clock, tension
//! selector, identity default per type, question shapes — printed so an
//! author can see what the system inferred and override it (§6 "inference
//! that is wrong"). Every rule has a red input in `tests/main/ontology_recipe.rs`.
//!
//! One generic resolver (`ref_error`) over the declared names plus the base
//! entity kinds the atlas already emits (`EntityType::NAMED`) serves every
//! reference facet; nothing is checked twice in two spellings.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    AttrFamily, Force, OntologyLanguageRegistry, OntologyPolicies, SupersessionClock, TypeKind,
};
use crate::enrichment::atlas::analysis::TensionStrategy;
use crate::enrichment::pipeline::atlas::EntityType;
use crate::recipe::OntologyBlock;

/// Most declared types per atom kind. Bounds the extraction-schema enum the
/// P2 composer generates; the number is the prompt budget, not a taste.
pub const MAX_TYPES_PER_KIND: usize = 12;
/// Most attributes on one declared type (the per-kind `attributes` object).
pub const MAX_ATTRS_PER_TYPE: usize = 8;
/// Most `values` on a text attribute (a closed-set enum in the schema).
pub const MAX_ENUM_VALUES: usize = 12;

/// Claim kinds the pipeline already emits and reads by name — the
/// `Claim.claim_kind` vocabulary documented on `atlas::atoms::Claim`, plus
/// `same_as`, the reified merge (P3). A declared claim type may not take one
/// of these names: it would collide in the `claim_kind` column.
pub const RESERVED_CLAIM_KINDS: &[&str] = &[
    "property",
    "evidence",
    "concession",
    "observation",
    "realisation",
    "blocker",
    "status",
    "example",
    "same_as",
];

/// What `validate_block` found. Merged into the recipe-level
/// `testing::ValidationResult` by `validate_recipe_offline`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct OntologyValidation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// Derived facets, one per line, in a fixed order (clock, selector,
    /// identity per entity type, question shapes).
    pub notes: Vec<String>,
}

/// Validate one `[enrichment.ontology]` block. Never panics; a block that
/// fails to parse reports the parse error as its single error.
pub fn validate_block(block: &OntologyBlock) -> OntologyValidation {
    let mut out = OntologyValidation::default();
    let policies = match block.policies() {
        Ok(p) => p,
        Err(e) => {
            out.errors.push(e.to_string());
            return out;
        }
    };

    let registry = OntologyLanguageRegistry::builtin();
    let unknown = registry.unknown_keys(&block.body);
    if !unknown.is_empty() {
        let accepted = registry
            .get(block.version)
            .map(|l| l.keys().join(", "))
            .unwrap_or_default();
        out.warnings.push(format!(
            "[enrichment.ontology] has key(s) no ontology version defines: {} — they are \
             ignored. Version {} accepts: {accepted}.",
            unknown
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", "),
            block.version
        ));
    }

    let labels_set = policies.derivation.tension.label.is_some()
        || policies.shape.types.iter().any(|t| t.label.is_some());
    if block.body.contains_key("vocabulary") && labels_set {
        out.warnings.push(
            "[enrichment.ontology] sets both the version-0 `vocabulary` terms and \
             version-1 `label`s; a label wins where both name the same term. Drop \
             `vocabulary` once the labels cover what you need."
                .to_string(),
        );
    }

    if !policies.has_declarations() {
        return out;
    }
    check_declarations(&policies, &mut out.errors);
    derived_facets(&policies, &mut out.notes);
    out
}

fn kind_key(kind: TypeKind) -> &'static str {
    match kind {
        TypeKind::Entity => "entity",
        TypeKind::Relation => "relation",
        TypeKind::Claim => "claim",
        TypeKind::Event => "event",
        TypeKind::State => "state",
    }
}

/// The one message shape for "this facet names a type nobody declared".
fn ref_error(type_name: &str, field: &str, value: &str, declared: &str) -> String {
    format!(
        "ontology type `{type_name}`: `{field} = \"{value}\"` does not name a declared \
         type (declared: {declared}; base kinds: {})",
        EntityType::NAMED.join(", ")
    )
}

fn join_or_none<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let v: Vec<&str> = items.collect();
    if v.is_empty() {
        "(none)".to_string()
    } else {
        v.join(", ")
    }
}

fn check_declarations(p: &OntologyPolicies, errors: &mut Vec<String>) {
    let types = &p.shape.types;
    let names: BTreeSet<&str> = types.iter().map(|t| t.name.as_str()).collect();
    let declared = join_or_none(names.iter().copied());
    // A reference resolves to a declared name or to one of the base entity
    // kinds the atlas already emits: `role_of = "person"` needs no `person`
    // declaration (declaring one stays legal, to add attributes); a name
    // outside both sets (`mint`, `topic`) must be declared.
    let resolvable: BTreeSet<&str> = names
        .iter()
        .copied()
        .chain(EntityType::NAMED.iter().copied())
        .collect();
    let claim_names: BTreeSet<&str> = p.claim_types().map(|t| t.name.as_str()).collect();
    let claims = join_or_none(claim_names.iter().copied());

    let mut seen = BTreeSet::new();
    let mut per_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    for t in types {
        if !seen.insert(t.name.as_str()) {
            errors.push(format!(
                "ontology declares type `{}` more than once",
                t.name
            ));
        }
        *per_kind.entry(kind_key(t.kind)).or_default() += 1;
    }
    for (kind, n) in per_kind {
        if n > MAX_TYPES_PER_KIND {
            errors.push(format!(
                "ontology declares {n} types of kind `{kind}`; the extraction schema \
                 carries at most {MAX_TYPES_PER_KIND} per kind. Fold the rarest into \
                 `specializes` children or into attributes."
            ));
        }
    }

    for t in types {
        let refs = [
            ("specializes", t.specializes.as_deref()),
            ("role_of", t.role_of.as_deref()),
            ("from", t.from.as_deref()),
            ("to", t.to.as_deref()),
            ("of", t.of.as_deref()),
            ("subject", t.subject.as_deref()),
        ];
        for (field, value) in refs {
            if let Some(v) = value {
                if !resolvable.contains(v) {
                    errors.push(ref_error(&t.name, field, v, &declared));
                }
            }
        }
        for (role, v) in &t.participants {
            if !resolvable.contains(v.as_str()) {
                errors.push(ref_error(
                    &t.name,
                    &format!("participants.{role}"),
                    v,
                    &declared,
                ));
            }
        }

        if t.attributes.len() > MAX_ATTRS_PER_TYPE {
            errors.push(format!(
                "ontology type `{}` declares {} attributes; at most {MAX_ATTRS_PER_TYPE} \
                 fit the per-kind schema. Move the rest to a `specializes` child or \
                 into the description.",
                t.name,
                t.attributes.len()
            ));
        }
        let mut attr_seen = BTreeSet::new();
        for a in &t.attributes {
            if !attr_seen.insert(a.name.as_str()) {
                errors.push(format!(
                    "ontology type `{}` declares attribute `{}` more than once",
                    t.name, a.name
                ));
            }
            match &a.family {
                AttrFamily::Ref { of } if !resolvable.contains(of.as_str()) => {
                    errors.push(ref_error(
                        &t.name,
                        &format!("attributes.{}.of", a.name),
                        of,
                        &declared,
                    ));
                }
                AttrFamily::Text { values } if values.len() > MAX_ENUM_VALUES => {
                    errors.push(format!(
                        "ontology type `{}`: attribute `{}` lists {} values; a closed set \
                         carries at most {MAX_ENUM_VALUES}. Drop `values` to make it free \
                         text, or split the attribute.",
                        t.name,
                        a.name,
                        values.len()
                    ));
                }
                _ => {}
            }
        }

        if t.kind == TypeKind::Claim {
            if RESERVED_CLAIM_KINDS.contains(&t.name.as_str()) {
                errors.push(format!(
                    "claim type name `{}` is reserved: the pipeline already emits claims \
                     of that kind ({}). Pick another name.",
                    t.name,
                    RESERVED_CLAIM_KINDS.join(", ")
                ));
            }
            if !t.deontic.is_empty() && t.force != Some(Force::Directive) {
                errors.push(format!(
                    "ontology type `{}` lists `deontic` but is not a directive claim \
                     (force = {}). Deontic modes — require, forbid, permit, request — \
                     are what a directive does; set force = \"directive\" or drop `deontic`.",
                    t.name,
                    t.force
                        .map(|f| super::language::wire_names(&[f]))
                        .unwrap_or_else(|| "(none)".to_string())
                ));
            }
        }
    }

    for (claim, clock) in &p.change.supersedes {
        if !claim_names.contains(claim.as_str()) {
            errors.push(format!(
                "change.supersedes names `{claim}`, which is not a declared claim type \
                 (claim types: {claims})"
            ));
            continue;
        }
        if clock == "document_date" {
            continue;
        }
        let Some(t) = p.type_decl(claim) else {
            continue;
        };
        let time_attrs: Vec<&str> = t
            .attributes
            .iter()
            .filter(|a| matches!(a.family, AttrFamily::Time { .. }))
            .map(|a| a.name.as_str())
            .collect();
        if !time_attrs.contains(&clock.as_str()) {
            errors.push(format!(
                "change.supersedes = {{ {claim} = \"{clock}\" }}: `{clock}` is neither \
                 `document_date` nor a time-family attribute of `{claim}` (time attributes: {})",
                join_or_none(time_attrs.into_iter())
            ));
        }
    }

    let tension = &p.derivation.tension;
    for b in &tension.between {
        if !claim_names.contains(b.as_str()) {
            errors.push(format!(
                "tension.between names `{b}`, which is not a declared claim type \
                 (claim types: {claims})"
            ));
        }
    }
    if !tension.same.is_empty() {
        if tension.between.is_empty() {
            errors.push(
                "tension.same is set but tension.between is empty; name the claim types \
                 tensions are sought between first"
                    .to_string(),
            );
        } else {
            let attrs: BTreeSet<&str> = tension
                .between
                .iter()
                .filter_map(|b| p.type_decl(b))
                .flat_map(|t| t.attributes.iter().map(|a| a.name.as_str()))
                .collect();
            for s in &tension.same {
                if s != "subject" && !attrs.contains(s.as_str()) {
                    errors.push(format!(
                        "tension.same names `{s}`, which is neither `subject` nor an \
                         attribute declared on {} (attributes: {})",
                        tension.between.join(", "),
                        join_or_none(attrs.iter().copied())
                    ));
                }
            }
        }
    }
}

/// Walk `specializes` upward (bounded) to the first ancestor carrying identity
/// keys. Returns `(primary, fallback, inherited_from)`.
fn resolve_identity<'a>(
    p: &'a OntologyPolicies,
    name: &'a str,
) -> (&'a [String], &'a [String], Option<&'a str>) {
    let mut current = name;
    for depth in 0..8 {
        let primary = p.identity.identity.get(current).map(Vec::as_slice);
        let fallback = p.identity.identity_fallback.get(current).map(Vec::as_slice);
        if primary.is_some() || fallback.is_some() {
            let inherited = (depth > 0).then_some(current);
            return (primary.unwrap_or(&[]), fallback.unwrap_or(&[]), inherited);
        }
        match p.type_decl(current).and_then(|t| t.specializes.as_deref()) {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    (&[], &[], None)
}

fn derived_facets(p: &OntologyPolicies, notes: &mut Vec<String>) {
    notes.push(match p.change.clock {
        SupersessionClock::DocumentDate => "clock: document_date — supersession folds on document \
             dates (the default; set `change.clock = \"narrative\"` for order within a work)"
            .to_string(),
        SupersessionClock::Narrative => {
            "clock: narrative — supersession folds on order within the work".to_string()
        }
        SupersessionClock::None => "clock: none — nothing supersedes".to_string(),
    });

    if !p.derivation.tension.between.is_empty() {
        let selector =
            match super::super::pipeline::pipelines::configurable_atlas::CUSTOM_TENSION_STRATEGY {
                TensionStrategy::EmbeddingTopK { k, floor } => {
                    format!("embedding top-k (k = {k}, floor = {floor})")
                }
                TensionStrategy::Graph => "graph (cluster + entity overlap)".to_string(),
            };
        notes.push(format!(
            "tension selector: {selector} over {} — cross-document declared corpora select \
             the embedding net; the classifier judges each pair",
            p.derivation.tension.between.join(", ")
        ));
    }

    for t in p.shape.types.iter().filter(|t| t.kind == TypeKind::Entity) {
        let (primary, fallback, inherited) = resolve_identity(p, &t.name);
        let mut line = if !primary.is_empty() {
            format!(
                "identity: {} → {} (external key, strict merge)",
                t.name,
                primary.join(" + ")
            )
        } else if !fallback.is_empty() {
            format!(
                "identity: {} → {} (descriptive keys, judged merge)",
                t.name,
                fallback.join(" + ")
            )
        } else {
            format!(
                "identity: {} → canonical name (default; declare `identity = [...]` for an \
                 external key)",
                t.name
            )
        };
        if let Some(from) = inherited {
            line.push_str(&format!(" — inherited from `{from}`"));
        }
        notes.push(line);
    }

    let by_kind = |k: TypeKind| {
        join_or_none(
            p.shape
                .types
                .iter()
                .filter(move |t| t.kind == k)
                .map(|t| t.name.as_str()),
        )
    };
    let aggregates: Vec<String> = p
        .claim_types()
        .flat_map(|c| {
            c.attributes.iter().filter_map(move |a| match &a.family {
                AttrFamily::Ref { .. } => Some(format!("{} by {}", c.name, a.name)),
                _ => None,
            })
        })
        .collect();
    notes.push(format!(
        "question shapes: enumerate [{}]; relations [{}]; events [{}]; aggregate [{}]",
        by_kind(TypeKind::Entity),
        by_kind(TypeKind::Relation),
        by_kind(TypeKind::Event),
        if aggregates.is_empty() {
            "(none — a claim type with a `ref` attribute can be counted by it)".to_string()
        } else {
            aggregates.join(", ")
        }
    ));
}
