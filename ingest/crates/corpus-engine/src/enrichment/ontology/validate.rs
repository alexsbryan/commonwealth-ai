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
    AttrFamily, DocumentFieldsDecl, DocumentStamp, Force, OntologyLanguageRegistry,
    OntologyPolicies, SourceDecl, SupersessionClock, TypeIndex, TypeKind, DOCUMENT_COUNT_ATTR,
    DOCUMENT_DATE_ATTR,
};
use crate::enrichment::atlas::analysis::{TensionStrategy, SAME_FIELD_CLOCK, SAME_FIELD_SUBJECT};
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

    super::validate_layer::layer_warnings(block, &policies, &mut out.warnings);

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

    // `change.document` stamps every claim, declared types or not, so it is
    // checked and reported outside the types-only block below.
    let document_note = policies.change.document.as_ref().map(|d| {
        check_document_fields(&policies, d, &mut out.errors);
        document_fields_note(d)
    });
    if policies.has_declarations() {
        check_declarations(&policies, &mut out.errors);
        check_sources(&policies, &mut out.errors);
        check_evidential(&policies, &mut out.errors);
        derived_facets(&policies, &mut out.notes);
        super::validate_derived::check(&policies, &mut out.errors, &mut out.notes);
    }
    out.notes.extend(document_note);
    out
}

/// `change.document`: at least one field, no blank name, and no declared
/// attribute under a key a stamp would overwrite.
fn check_document_fields(p: &OntologyPolicies, d: &DocumentFieldsDecl, errors: &mut Vec<String>) {
    let keys = DocumentStamp::ALL.map(DocumentStamp::key).join(", ");
    if d.declared().next().is_none() {
        errors.push(format!(
            "change.document names no field. Declare at least one of {keys}, or drop the key."
        ));
    }
    for (stamp, field) in d.declared() {
        if field.trim().is_empty() {
            errors.push(format!(
                "change.document.{} is blank. Name the metadata field your documents carry.",
                stamp.key()
            ));
        }
    }
    for t in &p.shape.types {
        for a in &t.attributes {
            if let Some(stamp) = d
                .declared()
                .map(|(s, _)| s)
                .find(|s| s.attr() == a.name.as_str())
            {
                errors.push(format!(
                    "ontology type `{}` declares attribute `{}`, which change.document.{} \
                     stamps on every claim from its document. Rename the attribute.",
                    t.name,
                    a.name,
                    stamp.key()
                ));
            }
        }
    }
}

/// `identity_evidential`: each source a stamp `change.document` declares,
/// `model_choice` or `proposed_answer`, each with its counts measured
/// somewhere named, and a bar beside them in (0, 1]. `identity_necessary`:
/// each a declared attribute with `values`, the closed set READ chooses from.
fn check_evidential(p: &OntologyPolicies, errors: &mut Vec<String>) {
    // No `change.document` declares no stamp, so every stamp source is unfilled.
    let declared: Vec<DocumentStamp> = match &p.change.document {
        Some(d) => d.declared().map(|(s, _)| s).collect(),
        None => Vec::new(),
    };
    let sources = DocumentStamp::ALL
        .map(DocumentStamp::attr)
        .into_iter()
        .chain(NON_STAMP_EVIDENCE)
        .collect::<Vec<_>>()
        .join(", ");
    for t in &p.shape.types {
        for e in &t.identity_evidential {
            match DocumentStamp::from_attr(&e.evidence) {
                Some(s) if declared.contains(&s) => {}
                Some(s) => errors.push(format!(
                    "ontology type `{}` lists evidence `{}`, which change.document.{} would \
                     fill, and it is not declared. Declare it, or drop the source.",
                    t.name,
                    e.evidence,
                    s.key()
                )),
                None if NON_STAMP_EVIDENCE.contains(&e.evidence.as_str()) => {}
                None => errors.push(format!(
                    "ontology type `{}` lists evidence `{}`, which is no source: {sources}.",
                    t.name, e.evidence
                )),
            }
            if e.of == 0 || e.right > e.of {
                errors.push(format!(
                    "ontology type `{}`: evidence `{}` declares {} right of {}; it needs at least \
                     one measured link and no more right than measured.",
                    t.name, e.evidence, e.right, e.of
                ));
            }
            if e.measured_on.trim().is_empty() {
                errors.push(format!(
                    "ontology type `{}`: evidence `{}` says nothing in `measured_on`. Name the \
                     labelled fold and the count behind its precision.",
                    t.name, e.evidence
                ));
            }
        }
        match (t.identity_evidential.is_empty(), t.identity_bar) {
            (false, None) => errors.push(format!(
                "ontology type `{}` lists identity evidence and no `identity_bar`. Declare the \
                 precision a link decided by evidence alone must have.",
                t.name
            )),
            (_, Some(b)) if !(b > 0.0 && b <= 1.0) => errors.push(format!(
                "ontology type `{}`: identity_bar {b} is not in (0, 1].",
                t.name
            )),
            _ => {}
        }
        for n in &t.identity_necessary {
            let readable = t.attributes.iter().any(|a| {
                a.name == *n
                    && matches!(&a.family, AttrFamily::Text { values } if !values.is_empty())
            });
            if !readable {
                errors.push(format!(
                    "ontology type `{}` lists necessary attribute `{n}`, which is not one of its \
                     attributes with `values`. Only a closed set is READ as one forced choice.",
                    t.name
                ));
            }
        }
    }
}

/// Evidence sources that are no document stamp (`identity_evidential`).
const NON_STAMP_EVIDENCE: [&str; 3] = ["model_choice", "reasoned_choice", "proposed_answer"];

/// A metadata `source`: on an entity type, naming its fields, filling only
/// declared attributes, and reading every identity key — the last through
/// `MetadataSourceDecl::identity_readers`, the check the projection runs.
fn check_sources(p: &OntologyPolicies, errors: &mut Vec<String>) {
    let index = TypeIndex::from_policies(p);
    for t in &p.shape.types {
        let Some(SourceDecl::Metadata(s)) = &t.source else {
            continue;
        };
        let at = |e: String| format!("ontology type `{}` (metadata source): {e}", t.name);
        if t.kind != TypeKind::Entity {
            errors.push(at(format!(
                "only an entity type is projected from document fields, and this is a `{}`",
                kind_key(t.kind)
            )));
        }
        if s.metadata.is_empty() || s.metadata.iter().any(|f| f.trim().is_empty()) {
            errors.push(at("`metadata` must name the fields read, none blank".into()));
        }
        let declared: BTreeSet<&str> = index
            .effective_attributes(&t.name)
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        for a in s
            .attributes
            .keys()
            .filter(|a| !declared.contains(a.as_str()))
        {
            errors.push(at(format!(
                "`{a}` is not a declared attribute of `{}` (attributes: {})",
                t.name,
                join_or_none(declared.iter().copied())
            )));
        }
        if declared.contains(DOCUMENT_COUNT_ATTR) {
            errors.push(at(format!(
                "the source writes `{DOCUMENT_COUNT_ATTR}` on every atom; rename the attribute"
            )));
        }
        if let Err(e) = s.identity_readers(index.effective_identity(&t.name)) {
            errors.push(at(e));
        }
        for (attr, r) in &s.refs {
            let family = index
                .effective_attributes(&t.name)
                .iter()
                .find(|a| a.name == *attr)
                .map(|a| a.family.clone());
            match family {
                Some(AttrFamily::Ref { of }) if of == r.of => {}
                Some(AttrFamily::Ref { of }) => errors.push(at(format!(
                    "ref `{attr}` links to `{}`, but the attribute is declared `ref` of `{of}`",
                    r.of
                ))),
                Some(_) => errors.push(at(format!(
                    "ref `{attr}` is not a declared `ref` attribute of `{}`",
                    t.name
                ))),
                None => errors.push(at(format!(
                    "ref `{attr}` is not a declared attribute of `{}`",
                    t.name
                ))),
            }
            let target = p.shape.types.iter().find(|x| x.name == r.of);
            match target.map(|x| (&x.source, index.effective_identity(&x.name).len())) {
                Some((Some(SourceDecl::Metadata(_)), 1)) => {}
                Some(_) => errors.push(at(format!(
                    "ref `{attr}` links to `{}`, which must declare a metadata source and exactly one identity key",
                    r.of
                ))),
                None => errors.push(at(format!("ref `{attr}` links to `{}`, which is not a declared type", r.of))),
            }
        }
    }
}

/// The derived line for `change.document`: which field becomes which stamp.
fn document_fields_note(d: &DocumentFieldsDecl) -> String {
    let pairs: Vec<String> = d
        .declared()
        .map(|(stamp, field)| match stamp {
            DocumentStamp::Date => {
                format!(
                    "{} ← `{field}` (RFC 2822 or ISO 8601, written as ISO 8601)",
                    stamp.attr()
                )
            }
            DocumentStamp::Thread | DocumentStamp::Id => format!("{} ← `{field}`", stamp.attr()),
        })
        .collect();
    format!(
        "document fields: {} — stamped on each claim from the one document its evidence \
         lands in; a claim in none or several is left unstamped and counted",
        join_or_none(pairs.iter().map(String::as_str))
    )
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

        if t.kind == TypeKind::State {
            // A state is a condition OF something. `of` is what routes it to
            // a Phase-1 facet — an entity puts it on `entities_developed`, a
            // relation on `relations_developed` — and a state that names
            // neither cannot be extracted or attached to an atom. It used to
            // load and then do nothing at all.
            if t.of.is_none() {
                errors.push(format!(
                    "ontology type `{}` is a state and declares no `of`. A state is a \
                     condition of something: name the declared entity it belongs to, or \
                     the declared relation when the state is of the PAIR (`of = \"bond\"`).",
                    t.name
                ));
            }
            // The `State` atom has no attribute bag — unlike entity, relation
            // and claim atoms. Putting the keys in the extraction schema would
            // ask the model for values nothing can store, so refuse here
            // rather than drop them after the call (ARCH §6).
            if !t.attributes.is_empty() {
                errors.push(format!(
                    "ontology type `{}` is a state and declares attributes ({}). The \
                     `State` atom carries no attributes, so they would be extracted and \
                     dropped. Put them on the entity or relation the state is `of`, or \
                     fold the distinction into separate state types.",
                    t.name,
                    t.attributes
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
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
        if clock == DOCUMENT_DATE_ATTR {
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

    // `seed.entity_types` is `Vec<EntityType>`, and `EntityType` carries an
    // `Other(String)` arm — so a misspelling deserialises happily into a type
    // no entity will ever have, and the row seeds NOTHING with no complaint.
    // Every other navigation field (`kinds`, `walk`) is a closed enum that
    // refuses an unknown spelling at load; this one could not, so it is
    // checked here against the same resolvable set the reference facets use.
    let entity_names: BTreeSet<&str> = types
        .iter()
        .filter(|t| t.kind == TypeKind::Entity)
        .map(|t| t.name.as_str())
        .chain(EntityType::NAMED.iter().copied())
        .collect();
    for (kind, walk) in p.navigation.rows() {
        for et in &walk.seed.entity_types {
            let spelling = et.as_str_repr();
            if !entity_names.contains(spelling) {
                errors.push(format!(
                    "navigation.{}: seed.entity_types names `{spelling}`, which is \
                     neither a base entity kind nor a declared entity type \
                     (entity types: {}). A name nothing carries seeds nothing.",
                    kind.as_str(),
                    join_or_none(entity_names.iter().copied())
                ));
            }
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
    // `same = []` waives the criterion and names no field, so there is
    // nothing here to resolve; it is legible on its own and the rules below
    // have no input to run on. Omitting the key entirely (`None`) takes the
    // default and is likewise nothing to check.
    if let Some(same) = tension.same.as_ref().filter(|s| !s.is_empty()) {
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
            for s in same {
                // The two reserved fields resolve off the claim itself rather
                // than off a declared attribute (`tension_fields::field_value`).
                // `clock` was missing here, so the documented default —
                // `DEFAULT_SAME_FIELDS`, subject plus clock — refused to
                // validate when an author wrote it out explicitly.
                if s != SAME_FIELD_SUBJECT && s != SAME_FIELD_CLOCK && !attrs.contains(s.as_str()) {
                    errors.push(format!(
                        "tension.same names `{s}`, which is neither `{SAME_FIELD_SUBJECT}`, \
                         `{SAME_FIELD_CLOCK}`, nor an attribute declared on {} \
                         (attributes: {})",
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

    let decided = |name: &str| {
        crate::enrichment::atlas::resolution_records::decides(&TypeIndex::from_policies(p), name)
    };
    for t in p
        .shape
        .types
        .iter()
        .filter(|t| t.kind == TypeKind::Entity || decided(&t.name))
    {
        if let Some(SourceDecl::Metadata(s)) = &t.source {
            let read: Vec<String> = s
                .attributes
                .iter()
                .map(|(a, r)| format!("{a}: {}", r.key()))
                .collect();
            notes.push(format!(
                "source: {} ← document fields {} ({}) — one atom per identity value, model \
                 atoms with that value merge into it; {} value(s) excluded",
                t.name,
                s.metadata.join(", "),
                read.join(", "),
                s.exclude.len()
            ));
        }
        let (primary, fallback, inherited) = resolve_identity(p, &t.name);
        let resolved = crate::enrichment::atlas::resolution_records::decides(
            &TypeIndex::from_policies(p),
            &t.name,
        );
        let mut line = if resolved {
            format!(
                "identity: {} → RESOLVE under its identity_criterion, over the claims declaring \
                 it their subject; atoms Phase 1 named are retired",
                t.name
            )
        } else if !primary.is_empty() {
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
    for t in p
        .shape
        .types
        .iter()
        .filter(|t| !t.identity_evidential.is_empty())
    {
        // A missing bar is already an error from `check_evidential`.
        let Some(bar) = t.identity_bar else { continue };
        let fields: Vec<String> = t
            .identity_evidential
            .iter()
            .map(|e| {
                let acts = if e.precision() >= bar {
                    "links"
                } else {
                    "below the bar"
                };
                format!(
                    "{} ({} of {}, expected precision {:.3}, {}; {acts})",
                    e.evidence,
                    e.right,
                    e.of,
                    e.precision(),
                    e.measured_on
                )
            })
            .collect();
        notes.push(format!(
            "identity evidence: {} ← {} at bar {bar:.2}",
            t.name,
            fields.join(", ")
        ));
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
