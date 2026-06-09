// SPDX-License-Identifier: AGPL-3.0-or-later
//! Data-driven entity-name normalization for the investigation pipeline.
//!
//! The engine here is the *mechanism*; the *vocabulary* comes from the recipe
//! ([`NormalizationConfig`]). No corpus-specific knowledge (US states, Air
//! Force base aliases, disposition categories) lives in this layer — a new
//! corpus declares its gazetteer in its recipe and the same generic transforms
//! apply. This keeps bespoke domain data out of the abstraction.
//!
//! [`Normalizer`] exposes the fold as three agreeing views — [`coalesce_key`],
//! [`entity_id`], [`normalized_name`] — which MUST stay consistent so a
//! relationship endpoint built from a raw surface form resolves to the same id
//! the coalesced entity carries (no dangling edges). All three route through
//! one pure per-name fold, so they agree by construction.
//!
//! The fold, per the matching [`FoldRule`]:
//!   1. alias map on the full folded form (acronym → canonical);
//!   2. drop a leading qualifier phrase ("air material command <base>");
//!   3. drop a trailing qualifier run (state names);
//!   4. drop the trailing-suffix run, OCR-tolerant ("x air force base" → "x");
//!   5. re-check the alias map on the reduced base ("atic wpafb" → "wpafb" →
//!      canonical).
//! Identity-grade throughout: only qualifier/suffix regions are touched, base
//! tokens are never fuzzy-matched, so two distinct bases never merge.

use crate::enrichment::reconciliation::signals::fold_name;
use crate::recipe::{FoldRule, NormalizationConfig, Recipe};

/// Applies a [`NormalizationConfig`] to entity names. Construct once per run
/// from the recipe and share by reference.
#[derive(Debug, Clone, Default)]
pub struct Normalizer {
    config: NormalizationConfig,
}

impl Normalizer {
    pub fn new(config: NormalizationConfig) -> Self {
        Self { config }
    }

    /// Build from a recipe's `[enrichment.normalization]` block, defaulting to
    /// an empty config (case/punctuation fold only) when absent.
    pub fn from_recipe(recipe: &Recipe) -> Self {
        let config = recipe
            .enrichment
            .as_ref()
            .and_then(|e| e.normalization.clone())
            .unwrap_or_default();
        Self::new(config)
    }

    /// The attribute an entity type takes its identity from, if any
    /// (e.g. `adjudication → "category"`). Consumed by the offline re-fold.
    pub fn identity_attribute(&self, entity_type: &str) -> Option<&str> {
        self.config
            .identity_attribute
            .get(entity_type)
            .map(String::as_str)
    }

    fn fold_rule(&self, entity_type: &str) -> Option<&FoldRule> {
        self.config
            .fold
            .iter()
            .find(|r| r.types.iter().any(|t| t == entity_type))
    }

    /// Pure per-name fold (see module docs). No rule for the type → fold by
    /// case/punctuation only.
    pub fn normalized_name(&self, entity_type: &str, name: &str) -> String {
        let folded = fold_name(name);
        let Some(rule) = self.fold_rule(entity_type) else {
            return folded;
        };
        if let Some(canon) = alias_hit(rule, &folded) {
            return canon;
        }
        let s = strip_leading(rule, &folded);
        let s = strip_trailing(rule, &s);
        alias_hit(rule, &s).unwrap_or(s)
    }

    /// Type-scoped coalesce key: `"<type>|<normalized-name>"`. The type prefix
    /// guarantees different `entity_type`s never merge.
    pub fn coalesce_key(&self, entity_type: &str, name: &str) -> String {
        format!("{entity_type}|{}", self.normalized_name(entity_type, name))
    }

    /// Stable entity id: `e-<type>-<slug(normalized-name)>`. Agrees with
    /// [`Self::coalesce_key`] so endpoints resolve to the coalesced entity.
    pub fn entity_id(&self, entity_type: &str, name: &str) -> String {
        format!(
            "e-{entity_type}-{}",
            slugify(&self.normalized_name(entity_type, name))
        )
    }

    /// Pick the cleanest surface form as canonical: penalize names that begin
    /// with a leading-qualifier phrase (the org-prefixed variant) and all-caps
    /// OCR, then prefer the fuller (longer) form. Generic — the "org prefix"
    /// notion is just the rule's `leading_prefixes`, not hardcoded.
    pub fn best_canonical<'a, I>(&self, entity_type: &str, names: I) -> Option<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let rule = self.fold_rule(entity_type);
        names
            .into_iter()
            .max_by_key(|n| canonical_score(rule, n))
            .map(str::to_string)
    }
}

/// Alias lookup on an exact folded form.
fn alias_hit(rule: &FoldRule, folded: &str) -> Option<String> {
    rule.aliases
        .iter()
        .find(|(variant, _)| variant == folded)
        .map(|(_, canon)| canon.clone())
}

/// Drop one leading qualifier phrase on a word boundary, keeping ≥1 token.
fn strip_leading(rule: &FoldRule, folded: &str) -> String {
    for p in &rule.leading_prefixes {
        if let Some(rest) = folded.strip_prefix(&format!("{p} ")) {
            let rest = rest.trim_start();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    folded.to_string()
}

/// Strip a trailing run of qualifier tokens / suffix words / ≤2-char OCR
/// fragments, keeping ≥1 base token. Handles two-word trailing qualifiers
/// (e.g. "new mexico"). Suffix matching is OCR-tolerant (edit-distance ≤1);
/// base tokens are never matched, so distinct bases never merge.
fn strip_trailing(rule: &FoldRule, folded: &str) -> String {
    let mut toks: Vec<&str> = folded.split_whitespace().collect();
    loop {
        if toks.len() <= 1 {
            break;
        }
        // Two-word trailing qualifier (e.g. "new mexico").
        if toks.len() >= 3 {
            let last2 = format!("{} {}", toks[toks.len() - 2], toks[toks.len() - 1]);
            if rule.trailing_qualifiers.iter().any(|q| *q == last2) {
                toks.truncate(toks.len() - 2);
                continue;
            }
        }
        let last = toks[toks.len() - 1];
        let strip = last.chars().count() <= 2
            || rule.trailing_qualifiers.iter().any(|q| q == last)
            || rule
                .trailing_suffixes
                .iter()
                .any(|s| s == last || edit_distance_le_1(last, s));
        if strip {
            toks.pop();
        } else {
            break;
        }
    }
    toks.join(" ")
}

/// Canonical-quality score: org-prefixed names lose hard, all-caps OCR loses
/// some, longer wins the rest. `rule == None` → length only.
fn canonical_score(rule: Option<&FoldRule>, name: &str) -> i64 {
    let lower = name.to_ascii_lowercase();
    let mut s = 0i64;
    if let Some(rule) = rule {
        if rule
            .leading_prefixes
            .iter()
            .any(|p| lower.starts_with(&format!("{p} ")))
        {
            s -= 1_000;
        }
    }
    let has_alpha = name.chars().any(char::is_alphabetic);
    if has_alpha && name == name.to_uppercase() {
        s -= 100;
    }
    s + name.chars().count() as i64
}

/// Slugify a normalized name into an id-safe suffix.
fn slugify(normalized: &str) -> String {
    normalized
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Bounded Levenshtein: true iff edit distance between `a` and `b` is ≤ 1.
/// Cheap length early-outs; used only on short suffix tokens.
fn edit_distance_le_1(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (la, lb) = (a.len(), b.len());
    if la.abs_diff(lb) > 1 {
        return false;
    }
    if la == lb {
        return a.iter().zip(b).filter(|(x, y)| x != y).count() <= 1;
    }
    let (short, long) = if la < lb { (a, b) } else { (b, a) };
    let (mut i, mut j, mut diff) = (0usize, 0usize, 0u8);
    while i < short.len() && j < long.len() {
        if short[i] == long[j] {
            i += 1;
            j += 1;
        } else {
            diff += 1;
            if diff > 1 {
                return false;
            }
            j += 1;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{FoldRule, NormalizationConfig};

    /// A config mirroring the UAP facility gazetteer, so the engine tests
    /// exercise the same transforms the recipe drives — without baking the
    /// vocabulary into the engine.
    fn facility_normalizer() -> Normalizer {
        let states: Vec<String> = [
            "ohio", "texas", "california", "new mexico", "louisiana",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let suffixes: Vec<String> = [
            "air", "airforce", "aiforce", "force", "forcebase", "airforcebase",
            "base", "field", "afb", "af",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        Normalizer::new(NormalizationConfig {
            identity_attribute: [("adjudication".to_string(), "category".to_string())]
                .into_iter()
                .collect(),
            fold: vec![FoldRule {
                types: vec!["installation".into(), "investigating_body".into()],
                aliases: vec![
                    ("wpafb".into(), "wright patterson".into()),
                    ("wp afb".into(), "wright patterson".into()),
                ],
                leading_prefixes: vec![
                    "air material command".into(),
                    "atic".into(),
                    "hq".into(),
                ],
                trailing_qualifiers: states,
                trailing_suffixes: suffixes,
            }],
        })
    }

    #[test]
    fn no_rule_folds_case_and_punctuation_only() {
        let n = Normalizer::default();
        assert_eq!(n.coalesce_key("company", "NVIDIA Corp."), "company|nvidia corp");
        // No facility folding without a rule.
        assert_ne!(
            n.coalesce_key("installation", "Wright-Patterson AFB"),
            n.coalesce_key("installation", "Wright-Patterson"),
        );
    }

    #[test]
    fn folds_suffix_family_and_acronym() {
        let n = facility_normalizer();
        let canon = "installation|wright patterson";
        for s in [
            "Wright-Patterson AFB",
            "Wright-Patterson Air Force Base",
            "Wright-Patterson Aiforce Base",
            "WPAFB",
        ] {
            assert_eq!(n.coalesce_key("installation", s), canon, "{s:?}");
        }
    }

    #[test]
    fn folds_trailing_state_and_leading_org() {
        let n = facility_normalizer();
        let canon = "installation|wright patterson";
        assert_eq!(n.coalesce_key("installation", "Wright-Patterson AFB, Ohio"), canon);
        assert_eq!(n.coalesce_key("installation", "ATIC WPAFB OHIO"), canon);
        assert_eq!(
            n.coalesce_key("installation", "Air Material Command Wright-Patterson Air Force Base"),
            canon
        );
        assert_eq!(
            n.coalesce_key("installation", "Kirtland Air Force Base, New Mexico"),
            "installation|kirtland"
        );
    }

    #[test]
    fn never_over_merges_distinct_bases() {
        let n = facility_normalizer();
        assert_ne!(
            n.coalesce_key("installation", "Kelly AFB, Texas"),
            n.coalesce_key("installation", "Kirtland AFB, New Mexico"),
        );
        assert_ne!(
            n.coalesce_key("installation", "Wright-Patterson AFB"),
            n.coalesce_key("installation", "Patterson Field"),
        );
    }

    #[test]
    fn entity_id_agrees_with_coalesce_key() {
        let n = facility_normalizer();
        assert_eq!(
            n.entity_id("installation", "Wright-Patterson AFB, Ohio"),
            n.entity_id("installation", "Air Material Command Wright-Patterson Air Force Base"),
        );
        assert_eq!(
            n.entity_id("installation", "Wright-Patterson AFB"),
            "e-installation-wright-patterson"
        );
    }

    #[test]
    fn best_canonical_prefers_clean_name() {
        let n = facility_normalizer();
        let names = [
            "Air Material Command Wright Patterson Air Force Base",
            "WRIGHT-PATTERSON AIR FORCE BASE",
            "Wright-Patterson Air Force Base",
            "Wright-Patterson AFB, Ohio",
        ];
        let best = n.best_canonical("installation", names.iter().copied()).unwrap();
        assert_eq!(best, "Wright-Patterson Air Force Base");
    }

    #[test]
    fn identity_attribute_is_exposed() {
        let n = facility_normalizer();
        assert_eq!(n.identity_attribute("adjudication"), Some("category"));
        assert_eq!(n.identity_attribute("installation"), None);
    }
}
