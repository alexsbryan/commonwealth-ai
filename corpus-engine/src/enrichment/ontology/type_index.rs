// SPDX-License-Identifier: AGPL-3.0-or-later
//! Name → declaration lookup over a [`ShapePolicy`], plus the one place the
//! `specializes` chain is walked.
//!
//! [`OntologyPolicies::type_decl`] answers "is this name declared" with a
//! linear scan and no inheritance. Everything past the composer needs the
//! *effective* view — `sceatta specializes coin` must carry coin's `weight`
//! — and every reader that walked the chain itself would be a second answer
//! to the same question (§10.6). So the walk lives here once.
//!
//! P2 needs [`Self::effective_attributes`] for the parser's attribute
//! validation, and [`Self::is_a`] because the same ancestor walk answers it
//! in one line. P3 adds [`Self::rigid_type_of`], [`Self::endpoints`],
//! [`Self::participants`] and the two identity accessors; P5 adds
//! [`Self::generic_ancestor`] — the chain's terminal UNDECLARED parent, which
//! `validate_block` accepts only when it names one of the six generic entity
//! kinds, so `doctrine specializes concept` is legal without declaring
//! `concept`. Neither mints a second index; both are the same bounded
//! `specializes` walk asked a different question.
//!
//! The plan also listed `descendants`. Nothing reads it: the coverage rollup
//! counts a type's subtypes with [`Self::is_a`] (one pass over the atoms, no
//! reverse map), and P5's enumeration planner wants the forward set from the
//! atoms rather than from the declaration. Left unwritten rather than built
//! speculatively.

use std::collections::{BTreeMap, BTreeSet};

use super::{AttrDecl, IdentityPolicy, OntologyPolicies, OntologyTypeDecl, ShapePolicy};

/// A borrowed view of the declared types, keyed by name.
///
/// Cheap to build (one pass over `shape.types`) and borrowed throughout, so
/// callers construct it per pass rather than caching it on a policy.
#[derive(Debug, Clone, Default)]
pub struct TypeIndex<'a> {
    by_name: BTreeMap<&'a str, &'a OntologyTypeDecl>,
}

impl<'a> TypeIndex<'a> {
    /// Index one shape. A name declared twice keeps the FIRST declaration —
    /// `validate_block` already reports the duplicate as an error, so the
    /// choice only decides what a recipe that failed validation does.
    pub fn new(shape: &'a ShapePolicy) -> Self {
        let mut by_name = BTreeMap::new();
        for t in &shape.types {
            by_name.entry(t.name.as_str()).or_insert(t);
        }
        Self { by_name }
    }

    /// Index the shape of a whole policy set.
    pub fn from_policies(policies: &'a OntologyPolicies) -> Self {
        Self::new(&policies.shape)
    }

    /// The declaration named `name`, or `None`.
    pub fn get(&self, name: &str) -> Option<&'a OntologyTypeDecl> {
        self.by_name.get(name).copied()
    }

    /// Is `name` declared at all?
    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// Does this shape declare nothing? The predicate every declared-ontology
    /// pass short-circuits on, and the reason a version-0 corpus pays nothing
    /// for the machinery above.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Is `name` the same type as `ancestor`, or a `specializes` descendant
    /// of it? A name nobody declared is nothing's descendant.
    pub fn is_a(&self, name: &str, ancestor: &str) -> bool {
        if !self.contains(name) {
            return false;
        }
        name == ancestor || self.ancestors(name).contains(&ancestor)
    }

    /// The type's own attributes followed by every inherited one, nearest
    /// ancestor first. A child that re-declares a parent's attribute name
    /// shadows it — one entry per name, the child's.
    ///
    /// Empty for a name nobody declared, which is what makes the parser's
    /// "validate against the declared attributes" a refusal rather than a
    /// pass-through for an undeclared type.
    pub fn effective_attributes(&self, name: &str) -> Vec<&'a AttrDecl> {
        let mut out: Vec<&'a AttrDecl> = Vec::new();
        let mut seen: BTreeSet<&'a str> = BTreeSet::new();
        for t in self.chain(name) {
            for a in &t.attributes {
                if seen.insert(a.name.as_str()) {
                    out.push(a);
                }
            }
        }
        out
    }

    /// The type something declared as `role` is a role OF — `ruler` is a role
    /// of `person`, so the atom is a person and `ruler` is a State on it
    /// (§7.5: identity from essence, and a part played is not an essence).
    ///
    /// Follows a `role_of` chain, so a role of a role lands on the rigid type
    /// at the end of it, and returns the LAST name on the chain even when
    /// nothing declares it — `person` is one of the six generic entity kinds,
    /// not a declared type, and that is the common case. `None` when `name` is
    /// not declared, or is declared without `role_of` (it is rigid already).
    pub fn rigid_type_of(&self, role: &str) -> Option<&'a str> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        seen.insert(role);
        let mut target = self.get(role)?.role_of.as_deref()?;
        loop {
            if !seen.insert(target) {
                // A cycle. The name we are standing on is as rigid an answer
                // as this recipe supports; `validate` reports the cycle.
                return Some(target);
            }
            match self.get(target).and_then(|d| d.role_of.as_deref()) {
                Some(next) => target = next,
                None => return Some(target),
            }
        }
    }

    /// The declared types at a relation type's two ends, in `[from, to]`
    /// order. Each end is independently optional (a recipe may constrain one
    /// end and leave the other open) and each is inherited from the nearest
    /// ancestor that declares it, so a relation specializing another is
    /// checked against the parent's endpoints unless it narrows them.
    pub fn endpoints(&self, rel: &str) -> [Option<&'a str>; 2] {
        [
            self.nearest(rel, |d| d.from.as_deref()),
            self.nearest(rel, |d| d.to.as_deref()),
        ]
    }

    /// An event type's declared participants as `(role, type)` pairs, own
    /// roles first then inherited ones, one entry per role name (the child's
    /// wins). Same shape and same shadowing rule as
    /// [`Self::effective_attributes`], for the same reason.
    pub fn participants(&self, event: &str) -> Vec<(&'a str, &'a str)> {
        let mut out: Vec<(&'a str, &'a str)> = Vec::new();
        let mut seen: BTreeSet<&'a str> = BTreeSet::new();
        for t in self.chain(event) {
            for (role, ty) in &t.participants {
                if seen.insert(role.as_str()) {
                    out.push((role.as_str(), ty.as_str()));
                }
            }
        }
        out
    }

    /// The external identifiers that make two mentions of `name` one thing.
    ///
    /// Inheritance is REPLACEMENT, not union: a key set is a criterion, and
    /// unioning a child's criterion with its parent's would silently widen
    /// what counts as the same thing. So the nearest declaration in the
    /// chain wins outright and a child that declares none inherits its
    /// parent's whole set.
    pub fn effective_identity(&self, name: &str) -> &'a [String] {
        self.nearest(name, |d| {
            (!d.identity.is_empty()).then_some(d.identity.as_slice())
        })
        .unwrap_or(&[])
    }

    /// The descriptive keys used when no external identifier is present.
    /// Same replacement rule as [`Self::effective_identity`].
    pub fn effective_identity_fallback(&self, name: &str) -> &'a [String] {
        self.nearest(name, |d| {
            (!d.identity_fallback.is_empty()).then_some(d.identity_fallback.as_slice())
        })
        .unwrap_or(&[])
    }

    /// [`IdentityPolicy`] with every declared type's keys resolved through
    /// `specializes`, so a reader holding only this map needs no shape and no
    /// chain walk of its own.
    ///
    /// `policies.identity` carries what the AUTHOR wrote (a `sceatta` that
    /// declares nothing is absent from it); this carries what each type
    /// RESOLVES to. The reconciler reads this one — it is serialized into
    /// `reconciliation.json`, so the criterion a merge ran under is on disk.
    pub fn effective_identity_policy(&self) -> IdentityPolicy {
        let mut identity = BTreeMap::new();
        let mut identity_fallback = BTreeMap::new();
        for name in self.by_name.keys() {
            let primary = self.effective_identity(name);
            if !primary.is_empty() {
                identity.insert((*name).to_string(), primary.to_vec());
            }
            let fallback = self.effective_identity_fallback(name);
            if !fallback.is_empty() {
                identity_fallback.insert((*name).to_string(), fallback.to_vec());
            }
        }
        IdentityPolicy {
            identity,
            identity_fallback,
        }
    }

    /// `name`'s declaration followed by its ancestors, nearest first. The one
    /// iteration order every "effective" accessor above shares.
    fn chain(&self, name: &str) -> Vec<&'a OntologyTypeDecl> {
        let Some(decl) = self.get(name) else {
            return Vec::new();
        };
        let mut out = vec![decl];
        out.extend(self.ancestors(name).into_iter().filter_map(|p| self.get(p)));
        out
    }

    /// The first `Some` that `pick` yields walking [`Self::chain`] — "the
    /// nearest declaration of this facet", the rule `from`/`to` and both
    /// identity key sets share.
    fn nearest<T>(
        &self,
        name: &str,
        pick: impl Fn(&'a OntologyTypeDecl) -> Option<T>,
    ) -> Option<T> {
        self.chain(name).into_iter().find_map(pick)
    }

    /// The generic entity kind `name` bottoms out in: the first `specializes`
    /// value on its chain that names no declared type.
    ///
    /// `validate_block` accepts an unresolvable reference only when it is one
    /// of `EntityType::NAMED`, so a chain that leaves the declared set has
    /// left it for a generic kind — `doctrine specializes concept` is a legal
    /// recipe with no `concept` declaration, and [`Self::is_a`] cannot see it
    /// (it walks DECLARED ancestors and stops where the declarations do).
    /// `None` when the chain stays inside the declared set, when `name` is not
    /// declared, or when the chain cycles.
    pub fn generic_ancestor(&self, name: &str) -> Option<&'a str> {
        self.walk_specializes(name).1
    }

    /// The `specializes` chain above `name`, nearest first, and the terminal
    /// undeclared parent it ran into (if any). Terminates on an undeclared
    /// parent and on a cycle (a recipe can write one; `validate` does not yet
    /// reject it, and a hang here would be a worse answer than a truncated
    /// chain). ONE walk: [`Self::ancestors`] and [`Self::generic_ancestor`]
    /// are two questions about the same traversal, not two traversals.
    fn walk_specializes(&self, name: &str) -> (Vec<&'a str>, Option<&'a str>) {
        let mut out: Vec<&'a str> = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        seen.insert(name);
        let mut cursor = self.get(name).and_then(|t| t.specializes.as_deref());
        while let Some(parent) = cursor {
            // Through `get`, not `by_name.get`: the accessor's return type is
            // `&'a`, so the walk is not tied to this `&self` borrow.
            let Some(decl) = self.get(parent) else {
                return (out, Some(parent));
            };
            if !seen.insert(decl.name.as_str()) {
                break;
            }
            out.push(decl.name.as_str());
            cursor = decl.specializes.as_deref();
        }
        (out, None)
    }

    fn ancestors(&self, name: &str) -> Vec<&'a str> {
        self.walk_specializes(name).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::ontology::{AttrFamily, TypeKind};

    fn attr(name: &str) -> AttrDecl {
        AttrDecl {
            name: name.to_string(),
            family: AttrFamily::Text { values: Vec::new() },
            description: String::new(),
        }
    }

    fn decl(name: &str, specializes: Option<&str>, attrs: &[&str]) -> OntologyTypeDecl {
        OntologyTypeDecl {
            name: name.to_string(),
            kind: TypeKind::Entity,
            specializes: specializes.map(str::to_string),
            attributes: attrs.iter().map(|a| attr(a)).collect(),
            ..Default::default()
        }
    }

    fn shape(types: Vec<OntologyTypeDecl>) -> ShapePolicy {
        ShapePolicy { types }
    }

    #[test]
    fn child_inherits_parent_attributes_child_first() {
        let s = shape(vec![
            decl("coin", None, &["weight", "metal"]),
            decl("sceatta", Some("coin"), &["series"]),
        ]);
        let idx = TypeIndex::new(&s);
        let names: Vec<&str> = idx
            .effective_attributes("sceatta")
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, vec!["series", "weight", "metal"]);
    }

    #[test]
    fn child_shadows_a_reused_attribute_name() {
        let s = shape(vec![
            decl("coin", None, &["weight"]),
            decl("sceatta", Some("coin"), &["weight"]),
        ]);
        let idx = TypeIndex::new(&s);
        let attrs = idx.effective_attributes("sceatta");
        assert_eq!(attrs.len(), 1, "one entry per name");
        assert!(std::ptr::eq(attrs[0], &s.types[1].attributes[0]));
    }

    #[test]
    fn undeclared_name_has_no_attributes_and_is_nothing() {
        let s = shape(vec![decl("coin", None, &["weight"])]);
        let idx = TypeIndex::new(&s);
        assert!(idx.effective_attributes("hoard").is_empty());
        assert!(!idx.is_a("hoard", "coin"));
        assert!(!idx.contains("hoard"));
    }

    #[test]
    fn is_a_is_reflexive_and_transitive_over_specializes() {
        let s = shape(vec![
            decl("coin", None, &[]),
            decl("sceatta", Some("coin"), &[]),
            decl("series_r", Some("sceatta"), &[]),
        ]);
        let idx = TypeIndex::new(&s);
        assert!(idx.is_a("coin", "coin"));
        assert!(idx.is_a("series_r", "coin"));
        assert!(!idx.is_a("coin", "sceatta"));
    }

    /// `specializes` may name a generic entity kind with no declaration —
    /// `validate_block` resolves references against the declared names UNION
    /// `EntityType::NAMED`. `is_a` stops where the declarations do, so the
    /// terminal parent is its own question.
    #[test]
    fn generic_ancestor_finds_the_undeclared_kind_the_chain_bottoms_out_in() {
        let s = shape(vec![
            decl("doctrine", Some("concept"), &[]),
            decl("school", Some("doctrine"), &[]),
            decl("coin", None, &[]),
            decl("sceatta", Some("coin"), &[]),
        ]);
        let idx = TypeIndex::new(&s);
        assert_eq!(idx.generic_ancestor("doctrine"), Some("concept"));
        assert_eq!(idx.generic_ancestor("school"), Some("concept"));
        // `is_a` cannot see it — that is why this method exists.
        assert!(!idx.is_a("doctrine", "concept"));
        // A chain that stays inside the declared set bottoms out nowhere.
        assert_eq!(idx.generic_ancestor("sceatta"), None);
        assert_eq!(idx.generic_ancestor("coin"), None);
        assert_eq!(idx.generic_ancestor("hoard"), None);
    }

    #[test]
    fn a_role_resolves_to_its_rigid_type_even_when_undeclared() {
        // `ruler role_of person` is the shipped numismatics declaration, and
        // `person` is a generic entity kind, not a declared type. The rigid
        // answer has to survive that.
        let s = shape(vec![
            OntologyTypeDecl {
                name: "ruler".into(),
                kind: TypeKind::Entity,
                role_of: Some("person".into()),
                ..Default::default()
            },
            decl("coin", None, &[]),
        ]);
        let idx = TypeIndex::new(&s);
        assert_eq!(idx.rigid_type_of("ruler"), Some("person"));
        assert_eq!(
            idx.rigid_type_of("coin"),
            None,
            "a rigid type plays no role"
        );
        assert_eq!(idx.rigid_type_of("hoard"), None, "undeclared is not a role");
    }

    #[test]
    fn a_role_of_chain_lands_on_the_last_link_and_a_cycle_terminates() {
        let role_of = |name: &str, of: &str| OntologyTypeDecl {
            name: name.into(),
            kind: TypeKind::Entity,
            role_of: Some(of.into()),
            ..Default::default()
        };
        let s = shape(vec![role_of("regent", "ruler"), role_of("ruler", "person")]);
        let idx = TypeIndex::new(&s);
        assert_eq!(idx.rigid_type_of("regent"), Some("person"));

        let cyclic = shape(vec![role_of("a", "b"), role_of("b", "a")]);
        assert_eq!(TypeIndex::new(&cyclic).rigid_type_of("a"), Some("a"));
    }

    #[test]
    fn endpoints_and_participants_inherit_from_the_nearest_declaration() {
        let mut struck_by = OntologyTypeDecl {
            name: "struck_by".into(),
            kind: TypeKind::Relation,
            from: Some("coin".into()),
            to: Some("mint".into()),
            ..Default::default()
        };
        struck_by
            .participants
            .insert("agent".into(), "ruler".into());
        let narrowed = OntologyTypeDecl {
            name: "struck_by_gold".into(),
            kind: TypeKind::Relation,
            specializes: Some("struck_by".into()),
            from: Some("sceatta".into()),
            ..Default::default()
        };
        let s = shape(vec![struck_by, narrowed]);
        let idx = TypeIndex::new(&s);
        assert_eq!(idx.endpoints("struck_by"), [Some("coin"), Some("mint")]);
        assert_eq!(
            idx.endpoints("struck_by_gold"),
            [Some("sceatta"), Some("mint")],
            "the child narrows `from` and inherits `to`"
        );
        assert_eq!(idx.endpoints("nothing"), [None, None]);
        assert_eq!(idx.participants("struck_by_gold"), vec![("agent", "ruler")]);
    }

    #[test]
    fn identity_is_replaced_by_the_nearest_declaration_never_unioned() {
        let with_identity = |name: &str, parent: Option<&str>, keys: &[&str]| OntologyTypeDecl {
            name: name.into(),
            kind: TypeKind::Entity,
            specializes: parent.map(str::to_string),
            identity: keys.iter().map(|k| k.to_string()).collect(),
            ..Default::default()
        };
        let s = shape(vec![
            with_identity("coin", None, &["find_id"]),
            with_identity("sceatta", Some("coin"), &[]),
            with_identity("series_r", Some("sceatta"), &["die_id"]),
        ]);
        let idx = TypeIndex::new(&s);
        assert_eq!(idx.effective_identity("sceatta"), ["find_id"], "inherited");
        assert_eq!(
            idx.effective_identity("series_r"),
            ["die_id"],
            "a criterion is replaced, not widened"
        );
        assert!(idx.effective_identity("hoard").is_empty());

        // The flattened map every reconciler reads: `sceatta` is present with
        // the key it inherited, even though the author never wrote it there.
        let flat = idx.effective_identity_policy();
        assert_eq!(flat.identity["sceatta"], vec!["find_id".to_string()]);
        assert_eq!(flat.identity["series_r"], vec!["die_id".to_string()]);
        assert!(flat.identity_fallback.is_empty());
    }

    #[test]
    fn a_specializes_cycle_terminates() {
        let s = shape(vec![
            decl("a", Some("b"), &["x"]),
            decl("b", Some("a"), &["y"]),
        ]);
        let idx = TypeIndex::new(&s);
        let names: Vec<&str> = idx
            .effective_attributes("a")
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, vec!["x", "y"]);
        // The same bounded walk answers the terminal question without hanging.
        assert_eq!(idx.generic_ancestor("a"), None);
    }
}
