// SPDX-License-Identifier: AGPL-3.0-or-later
//! How a `same` field is read off a claim, and what it says about a pair.
//!
//! [`super::tension_policy`] decides WHICH pairs the declared ontology
//! removes; this module is the two questions underneath that decision —
//! "what value does this claim carry for that field" and "given both
//! sides, does the field rule the pair out".
//!
//! For each field named in `same` (defaulting to
//! [`super::tension_policy::DEFAULT_SAME_FIELDS`]):
//!
//! | A | B | verdict |
//! |---|---|---|
//! | absent | absent | the field does not distinguish them — keep looking |
//! | present | absent | NOT KNOWN to differ — keep looking, and count it |
//! | present | present, unequal | not comparable |
//! | present | present, equal | keep looking |
//!
//! **A field rules a pair out only when BOTH sides carry it and they
//! differ.** One-sided absence is ignorance, not disagreement, and
//! excluding on it defaults an absence to "different" — the thing ARCH
//! §18.3 forbids. It is also not a close call: measured against the real
//! `wessex-hoard` atlas (158 candidates, 48 claims, `Claim.subject` not yet
//! populated), excluding on one-sided absence drops **40 candidates and
//! removes 0 of the 3 known false positives** — all three are pairs where
//! the criterion is blind on BOTH sides — while touching the only pair
//! holding a half of the one planted tension. The rule above drops 3.
//! Same behaviour once the field is populated; a thirteenth of the blast
//! radius before it is.
//!
//! What the criterion cannot see is REPORTED rather than absorbed:
//! [`super::tension_policy::ComparabilityReport::field_coverage`] counts the claims that carried
//! each field and `one_sided` counts the pairs it could not judge, and the
//! caller prints both (ARCH §18.1 — a check with no failing input you can
//! name is not a check).
//!
//! "Equal" is family-sensitive: a `time` attribute (and the clock) compares
//! by interval OVERLAP, everything else by normalised string equality. Two
//! rules valid in disjoint periods cannot contradict each other; two rules
//! whose validity overlaps can.

use std::collections::HashSet;

use super::super::atoms::{AtomId, Claim};
use super::tension_policy::{DOCUMENT_DATE_ATTR, SAME_FIELD_CLOCK, SAME_FIELD_SUBJECT};
use crate::enrichment::ontology::{AttrFamily, OntologyPolicies, SupersessionClock};

/// One resolved `same` field value. Time values compare by overlap; text
/// values by normalised equality (see the module doc).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum FieldValue {
    Text(String),
    Interval(String),
}

/// Resolve one `same` field on one claim. `None` means "this claim carries
/// no value for that field", which the comparison rule reads as "does not
/// distinguish" rather than as a mismatch.
pub(super) fn field_value(
    claim: &Claim,
    field: &str,
    policies: &OntologyPolicies,
    speakers: &HashSet<&AtomId>,
) -> Option<FieldValue> {
    match field {
        SAME_FIELD_SUBJECT => claim
            .subject
            .as_ref()
            .or_else(|| {
                // The voice stands in for the referent only when it is not
                // a named speaker — see [`SAME_FIELD_SUBJECT`].
                claim
                    .attributed_to
                    .as_ref()
                    .filter(|a| !speakers.contains(*a))
            })
            .map(|id| FieldValue::Text(id.as_str().to_string())),
        SAME_FIELD_CLOCK => {
            clock_attr(claim, policies).and_then(|key| attribute_value(claim, &key, policies))
        }
        other => attribute_value(claim, other, policies),
    }
}

/// Which attribute key this claim's clock reads, or `None` when the corpus
/// declares no temporal ordering.
///
/// `change.supersedes` maps a claim type to `"document_date"` or to one of
/// its own time attributes; a type not listed there folds on the corpus
/// clock. `SupersessionClock::Narrative` and `SupersessionClock::None` name no attribute: nothing
/// stamps a narrative position onto a claim yet, so the clock is vacuous
/// and says so instead of inventing a key.
fn clock_attr(claim: &Claim, policies: &OntologyPolicies) -> Option<String> {
    if let Some(kind) = claim.claim_kind.as_deref() {
        if let Some(named) = policies.change.supersedes.get(kind) {
            return Some(named.clone());
        }
    }
    match policies.change.clock {
        SupersessionClock::DocumentDate => Some(DOCUMENT_DATE_ATTR.to_string()),
        SupersessionClock::Narrative | SupersessionClock::None => None,
    }
}

/// Read a declared attribute off a claim, typed by the family the ontology
/// gave it. An attribute the ontology does not declare still reads as text
/// — `document_date` is stamped by the resolver, not declared by an author.
fn attribute_value(claim: &Claim, key: &str, policies: &OntologyPolicies) -> Option<FieldValue> {
    let raw = claim.attributes.get(key)?;
    let text = match raw {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Null => return None,
        other => other.to_string(),
    };
    if text.is_empty() {
        return None;
    }
    if is_time_attribute(claim, key, policies) {
        Some(FieldValue::Interval(text))
    } else {
        Some(FieldValue::Text(text.to_lowercase()))
    }
}

/// Is `key` a `time`-family attribute of this claim's declared type? The
/// resolver-stamped [`DOCUMENT_DATE_ATTR`] is one by construction.
fn is_time_attribute(claim: &Claim, key: &str, policies: &OntologyPolicies) -> bool {
    if key == DOCUMENT_DATE_ATTR {
        return true;
    }
    let Some(kind) = claim.claim_kind.as_deref() else {
        return false;
    };
    policies
        .type_decl(kind)
        .map(|t| {
            t.attributes
                .iter()
                .any(|a| a.name == key && matches!(a.family, AttrFamily::Time { .. }))
        })
        .unwrap_or(false)
}

/// What one field says about a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldVerdict {
    /// Both carry it and they agree, or neither carries it.
    Agrees,
    /// Exactly one side carries it: not known to differ. Kept, counted.
    OneSided,
    /// Both carry it and they differ. The only verdict that excludes.
    Differs,
}

/// The comparison rule from the module doc, for one field.
pub(super) fn fields_agree(a: Option<&FieldValue>, b: Option<&FieldValue>) -> FieldVerdict {
    match (a, b) {
        // The field distinguishes nothing here — keep looking.
        (None, None) => FieldVerdict::Agrees,
        // One side is on the record and the other is not. That is
        // ignorance, not disagreement — see the module doc for the
        // measurement that settled it. Kept, and counted so the blind
        // spot is visible.
        (None, Some(_)) | (Some(_), None) => FieldVerdict::OneSided,
        (Some(FieldValue::Interval(x)), Some(FieldValue::Interval(y))) => {
            if intervals_overlap(x, y) {
                FieldVerdict::Agrees
            } else {
                FieldVerdict::Differs
            }
        }
        (Some(x), Some(y)) => {
            if x == y {
                FieldVerdict::Agrees
            } else {
                FieldVerdict::Differs
            }
        }
    }
}

/// Do two ISO-8601-style intervals overlap?
///
/// Accepts `start/end` (either bound may be empty = unbounded) and a bare
/// point (`"2024-03"`, `"685"`), which is its own start and end. Bounds
/// compare LEXICALLY, which is correct for ISO-8601 dates and for bare
/// years written at a consistent width — the two forms a corpus actually
/// mixes. A bound the corpus writes some other way compares as text, which
/// can only make two claims LESS comparable, never more.
fn intervals_overlap(a: &str, b: &str) -> bool {
    let (a_start, a_end) = interval_bounds(a);
    let (b_start, b_end) = interval_bounds(b);
    // Disjoint when one ends strictly before the other begins.
    let a_before_b = match (a_end, b_start) {
        (Some(ae), Some(bs)) => ae < bs,
        _ => false,
    };
    let b_before_a = match (b_end, a_start) {
        (Some(be), Some(as_)) => be < as_,
        _ => false,
    };
    !(a_before_b || b_before_a)
}

/// Split `start/end` into its bounds; `None` is unbounded on that side.
fn interval_bounds(s: &str) -> (Option<&str>, Option<&str>) {
    match s.split_once('/') {
        Some((start, end)) => (non_empty(start), non_empty(end)),
        None => (non_empty(s), non_empty(s)),
    }
}

fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_overlap_reads_open_bounds_and_points() {
        assert!(intervals_overlap("685/704", "695/710"));
        assert!(!intervals_overlap("685/690", "695/704"));
        assert!(intervals_overlap("2024-01/", "2026-05/2026-06"));
        assert!(intervals_overlap("/2024-01", "2020-01/2021-01"));
        assert!(intervals_overlap("805", "805"));
        assert!(!intervals_overlap("805", "810"));
        assert!(intervals_overlap("805/810", "810"));
    }
}
