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
//! in one line. P5 adds [`Self::generic_ancestor`] — the chain's terminal
//! UNDECLARED parent, which `validate_block` only accepts when it names one of
//! the six generic entity kinds, so `doctrine specializes concept` is legal
//! without declaring `concept`. P3 extends this with `descendants`,
//! `rigid_type_of`, `effective_identity`, `endpoints` and `participants` — it
//! does not mint a second index.

use std::collections::{BTreeMap, BTreeSet};

use super::{AttrDecl, OntologyPolicies, OntologyTypeDecl, ShapePolicy};

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
        let Some(decl) = self.get(name) else {
            return Vec::new();
        };
        let mut chain: Vec<&'a OntologyTypeDecl> = vec![decl];
        chain.extend(self.ancestors(name).into_iter().filter_map(|p| self.get(p)));
        let mut out: Vec<&'a AttrDecl> = Vec::new();
        let mut seen: BTreeSet<&'a str> = BTreeSet::new();
        for t in chain {
            for a in &t.attributes {
                if seen.insert(a.name.as_str()) {
                    out.push(a);
                }
            }
        }
        out
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
