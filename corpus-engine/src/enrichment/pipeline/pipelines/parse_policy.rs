// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a declared ontology demands of a Phase-1 parse, and the value-level
//! rules that enforce it.
//!
//! [`ParsePolicy`] is built once from [`OntologyPolicies`], cached on the
//! genre, and handed to every sketch conversion in
//! [`super::ontology_parse`]. It is the ONLY thing that makes a declared
//! corpus parse differently from an undeclared one: `ParsePolicy::default()`
//! reproduces the pre-ontology reader exactly, and the generic dispatch
//! passes exactly that.
//!
//! Split from `ontology_parse.rs` because the two answer different questions
//! — "what does the recipe demand" and "how is one response read" — and P3's
//! resolution policy will read the first without touching the second.

use std::collections::{BTreeMap, BTreeSet};

use tracing::debug;

use super::super::atlas::{normalise_enum_tag, ClaimScope, DiscourseAct};
use crate::enrichment::ontology::{
    AttrDecl, AttrFamily, ClaimScopeDecl, Deontic, Force, OntologyPolicies, TypeIndex, TypeKind,
};

// ── Declared-ontology parse policy ──────────────────────────────────────────

/// What the reader enforces for one corpus's declared ontology.
///
/// Built once from [`OntologyPolicies`] and cached on the genre, then handed
/// to every `into_sketch`. [`ParsePolicy::default`] declares nothing and
/// reproduces the pre-ontology reader exactly — which is what makes invariant
/// I1 structural rather than remembered: the generic path and a `version = 1`
/// block with no declarations run the same code under the same policy.
///
/// The maps are keyed by the declared type NAME, and their attribute lists are
/// [`TypeIndex::effective_attributes`] — the same accessor the schema
/// generator reads, so what the grammar offers and what the parser accepts
/// cannot disagree (§10.6).
#[derive(Debug, Clone, Default)]
pub struct ParsePolicy {
    pub(super) entity_types: BTreeMap<String, Vec<AttrDecl>>,
    pub(super) relation_types: BTreeMap<String, Vec<AttrDecl>>,
    pub(super) event_types: BTreeMap<String, Vec<AttrDecl>>,
    pub(super) claim_types: BTreeMap<String, ClaimTypeRules>,
    /// Folded speaker roles that must never become entity atoms
    /// (`voices.not_entities`). Enforced here, not asked of the model (§7.6).
    not_entities: BTreeSet<String>,
    /// The claim kind to assume when a corpus declares exactly one, so a
    /// model that omits `claim_kind` still lands in the declared type rather
    /// than falling back to the generic claim.
    pub(super) default_claim_kind: Option<String>,
}

/// The per-claim-type facets the reader enforces. Read off the
/// `OntologyTypeDecl` of kind `claim`; there is no second place they live.
#[derive(Debug, Clone)]
pub(super) struct ClaimTypeRules {
    /// From the type's REQUIRED `force`. The declaration wins over whatever
    /// the model emitted: force is a property of the type, and a model cannot
    /// be asked to guarantee what the recipe already states (§7.6).
    pub(super) discourse_act: DiscourseAct,
    /// The scope a declared claim type fixes. Always `Some` for a declared
    /// type — see [`claim_scope_for`] for why the ABSENCE of a `scope` key is
    /// itself an answer.
    pub(super) scope: Option<ClaimScope>,
    /// Effective (inherited) declared attributes.
    pub(super) attributes: Vec<AttrDecl>,
    /// Whether the type declares a `subject`. The sketch keeps the NAME; P3
    /// resolves it to an atom id the way `attributed_to` is resolved.
    pub(super) has_subject: bool,
    /// Declared deontic modes. A `deontic` attribute is accepted only when it
    /// names one of these — validated, never synthesised.
    pub(super) deontic: Vec<Deontic>,
    /// Declared evidence grades, strongest first. Same rule as `deontic`.
    pub(super) grades: Vec<String>,
}

impl ClaimTypeRules {
    /// Wire spellings of the declared deontic modes, read back through serde
    /// so the accepted set can never disagree with what the recipe parses.
    pub(super) fn deontic_names(&self) -> Vec<String> {
        self.deontic
            .iter()
            .map(|d| {
                serde_json::to_string(d)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string()
            })
            .collect()
    }
}

/// The declared type a sketch named, or `None` when it named nothing or named
/// something the ontology does not declare. An undeclared name is a drop of
/// the TYPE, never of the atom — the sketch keeps its label and stays
/// unclassified, exactly as an undeclared corpus's sketches do.
pub(super) fn declared_type<V>(
    declared: &BTreeMap<String, V>,
    raw: Option<String>,
    kind: &str,
    subject: &str,
) -> Option<String> {
    let name = raw
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    if let Some(canonical) = canonical_name(declared, &name) {
        return Some(canonical);
    }
    debug!(
        atom = %kind, subject = %subject, named = %name,
        "ontology parse: leaving type unclassified — the ontology declares no such type"
    );
    None
}

/// The DECLARED spelling of the type a model named, matched the way the
/// generic variants are matched, or `None` when nothing declares it.
///
/// Case and separator drift is the norm on the unconstrained path: the schema's
/// enum is enforced under `json_schema` mode but is only a REQUEST for a
/// `json-object` provider, where a model that writes `"Coin"` or
/// `"trade goods"` for a declared `coin` /
/// `trade_goods` must not lose the atom. `EntityType::from_str_repr` already
/// forgives exactly this for the six named variants — it normalises before
/// comparing and only then falls through to `Other`, which keeps the model's
/// own spelling verbatim. Matching declared names by equality would therefore
/// have made a declared type STRICTER than a generic one, which is backwards;
/// [`normalise_enum_tag`] is the one rule both now use (§10.6).
///
/// Exact match wins first, so an ontology that declares two names differing
/// only in case still resolves each to itself.
pub(super) fn canonical_name<V>(declared: &BTreeMap<String, V>, name: &str) -> Option<String> {
    let t = name.trim();
    if t.is_empty() {
        return None;
    }
    if declared.contains_key(t) {
        return Some(t.to_string());
    }
    let norm = normalise_enum_tag(t);
    declared
        .keys()
        .find(|k| normalise_enum_tag(k) == norm)
        .cloned()
}

/// The effective declared attributes of the type a sketch landed on. Empty for
/// a name nobody declared, which is what makes attribute validation a refusal
/// rather than a pass-through.
pub(super) fn declared_attributes<'a>(
    declared: &'a BTreeMap<String, Vec<AttrDecl>>,
    name: &str,
) -> &'a [AttrDecl] {
    match canonical_name(declared, name) {
        Some(k) => declared.get(&k).map(Vec::as_slice).unwrap_or_default(),
        None => &[],
    }
}

/// Reserved attribute key carrying a directive claim's deontic normal form.
/// `pub(crate)` since ontology-v1 P5: `governance_view::project_claim` reads
/// the same key off the atom, and one spelling is the point (§10.6).
pub(crate) const ATTR_DEONTIC: &str = "deontic";
/// Reserved attribute key carrying a claim's evidence grade.
pub(super) const ATTR_GRADE: &str = "grade";

impl ParsePolicy {
    /// Derive the reader's policy from a corpus's declared ontology.
    ///
    /// A policy with no declared types yields [`Self::default`], so callers do
    /// not branch — `has_declarations()` is the ONE predicate, and it is read
    /// by the composer, not here.
    pub fn from_policies(policies: &OntologyPolicies) -> Self {
        let index = TypeIndex::from_policies(policies);
        let mut out = Self::default();
        for t in &policies.shape.types {
            let attrs: Vec<AttrDecl> = index
                .effective_attributes(&t.name)
                .into_iter()
                .cloned()
                .collect();
            match t.kind {
                TypeKind::Entity => {
                    out.entity_types.insert(t.name.clone(), attrs);
                }
                TypeKind::Relation => {
                    out.relation_types.insert(t.name.clone(), attrs);
                }
                TypeKind::Event => {
                    out.event_types.insert(t.name.clone(), attrs);
                }
                TypeKind::Claim => {
                    let Some(force) = t.force else {
                        // Unreachable through `Recipe::from_toml` — the V1
                        // language refuses a claim type without `force`. A
                        // hand-built policy that skips it loses the type
                        // rather than silently guessing a force (§18.3).
                        debug!(
                            claim_type = %t.name,
                            "ontology parse: claim type declares no force; not enforced"
                        );
                        continue;
                    };
                    out.claim_types.insert(
                        t.name.clone(),
                        ClaimTypeRules {
                            discourse_act: discourse_act_for(force),
                            scope: Some(t.scope.map_or(ClaimScope::Universal, claim_scope_for)),
                            attributes: attrs,
                            has_subject: t.subject.is_some(),
                            deontic: t.deontic.clone(),
                            grades: t.grades.clone(),
                        },
                    );
                }
                // States are not extracted as a declared kind in Phase 1 —
                // the section schema has no state-type slot. P3 emits them
                // from `role_of`.
                TypeKind::State => {}
            }
        }
        out.not_entities = policies
            .assertion
            .voices
            .not_entities
            .iter()
            .map(|v| fold_voice(v))
            .filter(|v| !v.is_empty())
            .collect();
        let mut claim_names = out.claim_types.keys();
        out.default_claim_kind = match (claim_names.next(), claim_names.next()) {
            (Some(only), None) => Some(only.clone()),
            _ => None,
        };
        out
    }

    /// Does this policy enforce anything? False for every undeclared corpus.
    pub fn is_empty(&self) -> bool {
        self.entity_types.is_empty()
            && self.relation_types.is_empty()
            && self.event_types.is_empty()
            && self.claim_types.is_empty()
            && self.not_entities.is_empty()
    }

    /// Is `name` a speaker role the corpus declared as not-subject-matter?
    pub(super) fn is_voice(&self, name: &str) -> bool {
        !self.not_entities.is_empty() && self.not_entities.contains(&fold_voice(name))
    }
}

/// Searle's force → the atlas's discourse act. The ONE mapping; a second
/// spelling of it anywhere else is the §10.6 smell.
fn discourse_act_for(force: Force) -> DiscourseAct {
    match force {
        Force::Assertive => DiscourseAct::Assert,
        // A directive and a declaration both DO something by being said —
        // `enact` is the atlas's act for that. The atlas has no separate
        // "directive"; the deontic mode carries which one it is.
        Force::Directive | Force::Declaration => DiscourseAct::Enact,
        Force::Commissive => DiscourseAct::Commit,
    }
}

/// Declared claim scope → the atlas's `ClaimScope`. `in_work` is what the
/// literary resolver already defaults every claim to; `about_work` is scoped
/// to the work being discussed, which is `contextual`, not universal.
///
/// `ClaimScopeDecl` has no `universal` variant BECAUSE universal is what a
/// claim type that says nothing about scope means — `OntologyTypeDecl::scope`
/// documents exactly that ("Default universal"), and the two variants are the
/// two departures from it. The caller therefore maps `None` to
/// [`ClaimScope::Universal`] rather than leaving the resolver's literary
/// `Fictional` in place: a numismatics catalogue's attributions are claims
/// about the world, and labelling all of them fiction is a wrong answer, not
/// a missing one (measured on `wessex-hoard`, 2026-09-02: 48 of 48).
///
/// Undeclared corpora never reach here — there is no `ClaimTypeRules` without
/// a declared claim type — so the literary default stands where it belongs.
fn claim_scope_for(scope: ClaimScopeDecl) -> ClaimScope {
    match scope {
        ClaimScopeDecl::InWork => ClaimScope::Fictional,
        ClaimScopeDecl::AboutWork => ClaimScope::Contextual,
    }
}

/// Fold a speaker role for comparison: lowercased, trimmed, leading `the`
/// dropped. `"The Cataloguer"`, `"the cataloguer"` and `"cataloguer"` are one
/// voice; an author should not have to spell all three.
fn fold_voice(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    lower
        .strip_prefix("the ")
        .unwrap_or(lower.as_str())
        .trim()
        .to_string()
}

/// Normalise one attribute value against its declared family, or `None` when
/// the value cannot be that family. Stored normalised — a number for a
/// quantity, a string otherwise — so a downstream reader never re-parses.
fn validate_attr(family: &AttrFamily, value: &serde_json::Value) -> Option<serde_json::Value> {
    use serde_json::Value;
    match family {
        AttrFamily::Text { values } => {
            let s = value.as_str()?.trim();
            if s.is_empty() {
                return None;
            }
            if values.is_empty() {
                return Some(Value::String(s.to_string()));
            }
            // A closed set answers in the DECLARED spelling, so the stored
            // value and the recipe agree however the model cased it.
            values
                .iter()
                .find(|v| v.eq_ignore_ascii_case(s))
                .map(|v| Value::String(v.clone()))
        }
        AttrFamily::Quantity { unit } => {
            let n = match value {
                Value::Number(n) => n.as_f64(),
                // Insurance for the UNCONSTRAINED path only. Under the
                // default `json_schema` mode the grammar is enforced and a
                // quantity arrives as a number (probed 2026-09-02, note
                // e6067398; the wessex rebuild stored 1.21, 1.05, 4.33 …).
                // A provider on `structured_output_mode = "json-object"`
                // (DeepSeek) gets no grammar, and there a model writes the
                // unit back into the value ("1.29 g"). Take the leading
                // number; refuse anything else.
                Value::String(s) => leading_number(s),
                _ => None,
            }?;
            // A declared UNIT means the attribute measures something physical,
            // and prose does not report a coin weighing zero grams — a `0`
            // here is the model filling a slot it could not fill, which then
            // reads downstream as a measurement. Absence is recoverable and
            // visible; a fabricated zero is neither (§18.3). Unitless
            // quantities keep their zero: a count legitimately reaches it.
            if let (Some(u), true) = (unit.as_deref(), n == 0.0) {
                // Traced HERE rather than left to the caller: the caller's
                // drop line reads "value is not of the declared family", and
                // a `0` is a perfectly good number. Only this site knows the
                // real reason, and an operator who cannot see it would go
                // looking for a parser bug (§9.1).
                debug!(
                    unit = %u,
                    "ontology parse: dropping attribute — zero is an absent \
                     measurement, not a measured zero"
                );
                return None;
            }
            serde_json::Number::from_f64(n).map(Value::Number)
        }
        AttrFamily::Time { .. } | AttrFamily::Ref { .. } => {
            let s = value.as_str()?.trim();
            if s.is_empty() {
                None
            } else if is_absent_marker(s) {
                // `"unknown"` in a date or a mint slot reads downstream as a
                // date or a mint. Only a missing key is visibly missing
                // (§18.3) — the same reason a unit quantity refuses a zero.
                debug!(
                    value = s,
                    "ontology parse: dropping attribute — a placeholder is an absent value, \
                     not a value"
                );
                None
            } else {
                Some(Value::String(s.to_string()))
            }
        }
    }
}

/// Words a model writes into a slot it could not fill. The prompt asks for
/// the key to be left out; models write the instruction ("omit"), a null word,
/// or "unknown (not stated in text)" instead, and each of those would land as
/// a value. Case-insensitive; `unknown…` and `not stated…` match as prefixes
/// because the model elaborates them.
pub(super) fn is_absent_marker(s: &str) -> bool {
    let t = s.trim().to_ascii_lowercase();
    const EXACT: &[&str] = &[
        "omit",
        "omitted",
        "none",
        "null",
        "n/a",
        "na",
        "-",
        "(none)",
        "unspecified",
        "not applicable",
    ];
    if EXACT.contains(&t.as_str()) {
        return true;
    }
    // A PREFIX needs a word boundary: "unknown" and "unknown (not stated in
    // text)" are absent values; "unknown-type sceatta series" is a real one.
    ["unknown", "not stated"].iter().any(|p| {
        t.strip_prefix(p)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()))
    })
}

/// The leading decimal number of a string, ignoring a trailing unit or range
/// tail. `"1.29 g"` → 1.29; `"c. 720"` → 720.0; `"heavy"` → `None`.
fn leading_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let start = t.find(|c: char| c.is_ascii_digit() || c == '-' || c == '+')?;
    let rest = &t[start..];
    let end = rest
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *i == 0)))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok().filter(|f| f.is_finite())
}

/// Keep the declared attributes the model filled, normalised by family; drop
/// the rest with a reason. `subject` names the atom in the log so a debug run
/// reads as "which atom lost which attribute and why" (§9).
pub(super) fn validated_attributes(
    decls: &[AttrDecl],
    raw: serde_json::Map<String, serde_json::Value>,
    kind: &str,
    subject: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    if decls.is_empty() && !raw.is_empty() {
        debug!(
            atom = %kind, subject = %subject, keys = raw.len(),
            "ontology parse: dropping attributes — the type declares none"
        );
        return out;
    }
    // The per-key lines below fire only when something is DROPPED, so a model
    // that offered nothing at all and a parser that threw everything away look
    // identical in the log — silence. They are opposite defects with opposite
    // fixes (the prompt vs. this function), and telling them apart is what the
    // wessex-hoard 0-of-14 investigation needed and did not have. `offered` is
    // the count before any filtering, so it is the one number that can only
    // come from the model.
    let offered = raw.len();
    for (key, value) in raw {
        let Some(decl) = decls.iter().find(|d| d.name == key) else {
            debug!(
                atom = %kind, subject = %subject, attribute = %key,
                "ontology parse: dropping attribute — not declared on this type"
            );
            continue;
        };
        match validate_attr(&decl.family, &value) {
            Some(v) => {
                out.insert(key, v);
            }
            None => debug!(
                atom = %kind, subject = %subject, attribute = %key,
                family = decl.family.key(), value = %value,
                "ontology parse: dropping attribute — value is not of the declared family"
            ),
        }
    }
    debug!(
        atom = %kind, subject = %subject,
        declared = decls.len(), offered, kept = out.len(),
        "ontology parse: declared attributes"
    );
    out
}

/// Keep a reserved claim attribute (`deontic`, `grade`) only when it names one
/// of the values the claim type declared. Validated, never synthesised: an
/// undeclared mode is a drop, not a guess.
pub(super) fn validated_choice(
    raw: &mut serde_json::Map<String, serde_json::Value>,
    out: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    allowed: &[String],
    subject: &str,
) {
    let Some(value) = raw.remove(key) else { return };
    let Some(s) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        debug!(subject = %subject, attribute = %key, "ontology parse: dropping reserved attribute — not a string");
        return;
    };
    match allowed.iter().find(|a| a.eq_ignore_ascii_case(s)) {
        Some(a) => {
            out.insert(key.to_string(), serde_json::Value::String(a.clone()));
        }
        None => debug!(
            subject = %subject, attribute = %key, value = %s,
            "ontology parse: dropping reserved attribute — the claim type declares no such value"
        ),
    }
}

#[cfg(test)]
mod tests {
    use crate::recipe_templates::numismatics_policies as numismatics;
    use super::*;

    #[test]
    fn default_policy_declares_nothing() {
        assert!(ParsePolicy::default().is_empty());
        assert!(!ParsePolicy::from_policies(&numismatics()).is_empty());
    }
    /// The declared-name match is the SAME rule the generic variants get:
    /// lowercase, separators folded. Exact match wins first so two names that
    /// differ only in case each resolve to themselves.
    #[test]
    fn canonical_name_matches_by_the_generic_variants_rule() {
        let declared: BTreeMap<String, ()> = [("coin", ()), ("trade_goods", ()), ("Coin", ())]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        assert_eq!(canonical_name(&declared, "coin").as_deref(), Some("coin"));
        assert_eq!(canonical_name(&declared, "Coin").as_deref(), Some("Coin"));
        assert_eq!(
            canonical_name(&declared, "trade goods").as_deref(),
            Some("trade_goods")
        );
        assert_eq!(
            canonical_name(&declared, "Trade-Goods").as_deref(),
            Some("trade_goods")
        );
        assert_eq!(canonical_name(&declared, "  ").as_deref(), None);
        assert_eq!(canonical_name(&declared, "hoard").as_deref(), None);
    }

    /// An undeclared type is unclassified, never a substituted default.
    /// A weight of zero grams is not a light coin, it is a model filling a
    /// slot it could not fill — and once stored it reads as a measurement.
    /// The wessex-hoard build recorded `weight=0` on two coins whose sections
    /// state no weight at all.
    ///
    /// Falsifier: drop the `unit.is_some()` guard and the zero is stored.
    #[test]
    fn a_zero_measurement_with_a_unit_is_an_absence_not_a_value() {
        let grams = AttrFamily::Quantity {
            unit: Some("g".into()),
        };
        assert!(validate_attr(&grams, &serde_json::json!(0)).is_none());
        assert!(validate_attr(&grams, &serde_json::json!("0 g")).is_none());
        assert_eq!(
            validate_attr(&grams, &serde_json::json!("1.21 g")).and_then(|v| v.as_f64()),
            Some(1.21),
            "a real measurement is untouched"
        );
    }

    /// A quantity with no declared unit is not a physical measurement, and a
    /// count of zero is a fact. The guard above must not reach it.
    #[test]
    fn a_unitless_zero_is_kept() {
        let bare = AttrFamily::Quantity { unit: None };
        assert_eq!(
            validate_attr(&bare, &serde_json::json!(0)).and_then(|v| v.as_f64()),
            Some(0.0)
        );
    }
    #[test]
    fn declared_attributes_of_an_undeclared_name_is_empty() {
        let p = ParsePolicy::from_policies(&numismatics());
        assert!(!declared_attributes(&p.entity_types, "coin").is_empty());
        assert!(declared_attributes(&p.entity_types, "hoard").is_empty());
    }

    #[test]
    fn force_maps_to_the_pinned_discourse_acts() {
        assert_eq!(discourse_act_for(Force::Assertive), DiscourseAct::Assert);
        assert_eq!(discourse_act_for(Force::Directive), DiscourseAct::Enact);
        assert_eq!(discourse_act_for(Force::Declaration), DiscourseAct::Enact);
        assert_eq!(discourse_act_for(Force::Commissive), DiscourseAct::Commit);
    }
    #[test]
    fn leading_number_reads_what_models_actually_write() {
        assert_eq!(leading_number("1.29 g"), Some(1.29));
        assert_eq!(leading_number("c. 720"), Some(720.0));
        assert_eq!(leading_number("-3"), Some(-3.0));
        assert_eq!(leading_number("heavy"), None);
        assert_eq!(leading_number(""), None);
    }
    #[test]
    fn voice_folding_is_case_and_article_insensitive() {
        assert_eq!(fold_voice("  The Cataloguer "), "cataloguer");
        assert_eq!(fold_voice("cataloguer"), "cataloguer");
        assert_eq!(fold_voice("the narrator"), "narrator");
    }
}

#[cfg(test)]
mod absent_marker_tests {
    use super::is_absent_marker;

    #[test]
    fn placeholders_a_model_writes_are_absent_values() {
        for s in [
            "omit",
            "Omit",
            "none",
            "N/A",
            "-",
            "unknown",
            "Unknown (not stated in text)",
            "not stated",
        ] {
            assert!(is_absent_marker(s), "{s}");
        }
    }

    /// Falsifier: a real value that merely CONTAINS a marker word stays. The
    /// prefix rule needs a word boundary for that — without one,
    /// "unknown-type sceatta series" reads as an absent value and the coin
    /// loses a real attribute.
    ///
    /// This assertion shipped with an `|| s.starts_with("unknown")` escape,
    /// which made the single case it exists to defend pass for free: a gate
    /// nobody had watched fail (§18.1).
    #[test]
    fn real_values_are_not_markers() {
        for s in [
            "Nkemdirim 2018",
            "die study",
            "c. 720",
            "Wessex Down 1",
            "unknown-type sceatta series",
            "not stated-in-catalogue variant",
        ] {
            assert!(!is_absent_marker(s), "{s}");
        }
    }
}
