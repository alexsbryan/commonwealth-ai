// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge signals a RECIPE declares (ontology v1) — the identity axis.
//!
//! The three signals in [`super::signals`] read an atom's own surface: its
//! name, the email addresses in it, the org and role it carries. These two
//! read the author's declaration instead — `identity = ["rxnorm_id"]` says
//! what MAKES two mentions one thing in this domain, and no amount of surface
//! similarity substitutes for it.
//!
//! That is also why they are graded differently. An external identifier is a
//! CRITERION of identity, so [`ExternalIdSignal`] alone satisfies the
//! cross-origin gate; a descriptive key is a description of one, so
//! [`DescriptiveKeySignal`] goes through the same count gate as any other
//! signal. `multi_origin::reconcile_with_signals` reads
//! [`MergeSignal::ExternalId`] to tell the two apart.
//!
//! Neither can be constructed without a key map, and [`signals_for_policy`]
//! adds neither when the map is empty — so an undeclared corpus gets
//! `default_signals` term for term, which is what keeps the Enron B³ lane a
//! leak detector for this phase (I5).

use std::collections::BTreeMap;

use super::signals::{default_signals, fold_name, MergeSignal, MergeSignalCheck};
use crate::enrichment::atlas::atoms::Entity;
use crate::enrichment::ontology::IdentityPolicy;

/// The value a declared identity key names on an entity.
///
/// A key is usually an ATTRIBUTE (`rxnorm_id`, `find_id`) — the extractor
/// filled it and the Phase-1 reader validated its family. Three author-facing
/// names instead read a field of the atom itself, because that is how the
/// worked declarations in `ONTOLOGY_PRIMITIVES.md` §1 spell them
/// (`identity_fallback = ["name", "employer"]`). The one place that mapping
/// lives (§10.6): both signals below call this, and so would a third.
///
/// `None` when the key names nothing on this atom, or names a blank. A key
/// with no value never contributes to identity — an identifier missing a
/// component is not that identifier.
fn identity_key_value(entity: &Entity, key: &str) -> Option<String> {
    let raw = match key {
        "name" | "canonical_name" => Some(entity.canonical_name.clone()),
        "employer" | "affiliation" => entity.affiliation.clone(),
        "role" => entity.role.clone(),
        _ => return identity_value_of(entity.attributes.get(key)?),
    }?;
    fold_identity_value(&raw)
}

/// One declared identity key's value, read from an ATTRIBUTE bag alone.
///
/// The resolver's merge veto (`atlas::resolution_identity`) compares the same
/// keys before these signals ever run, and comparing them differently is how
/// one pass refuses a merge the other would confirm: `Wessex-Down 1` and
/// `Wessex Down 1` are one key to [`fold_name`] and two to a plain lowercase.
/// So both go through here (§10.6).
pub(crate) fn identity_value_of(value: &serde_json::Value) -> Option<String> {
    let raw = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    fold_identity_value(&raw)
}

/// The fold every identity comparison uses. Empty after folding means the key
/// is absent, not that it is the empty string.
pub(crate) fn fold_identity_value(raw: &str) -> Option<String> {
    let folded = fold_name(raw);
    (!folded.is_empty()).then_some(folded)
}

/// Do `left` and `right` agree on every key the recipe named for their type?
///
/// Both must carry the SAME resolved key list — the flattened map
/// (`TypeIndex::effective_identity_policy`) gives a `sceatta` the `coin` keys
/// it inherited, so a specialization and its parent still compare. Different
/// key lists mean the recipe called them different kinds of thing, and this
/// signal has nothing to say about them.
///
/// Every key must be present and equal on both. ALL, not any: a two-key
/// criterion that fired on one key would be a different, weaker criterion than
/// the one the author wrote.
fn declared_keys_agree(
    keys_by_type: &BTreeMap<String, Vec<String>>,
    left: &Entity,
    right: &Entity,
) -> bool {
    let Some(keys) = keys_by_type.get(left.entity_type.as_str_repr()) else {
        return false;
    };
    if keys.is_empty() || keys_by_type.get(right.entity_type.as_str_repr()) != Some(keys) {
        return false;
    }
    keys.iter().all(
        |k| match (identity_key_value(left, k), identity_key_value(right, k)) {
            (Some(l), Some(r)) => l == r,
            _ => false,
        },
    )
}

/// External-identifier signal — the STRICT one. See
/// [`MergeSignal::ExternalId`]; the reconciler's count gate reads that tag
/// specially.
pub struct ExternalIdSignal {
    /// Declared type → external key names, already resolved through
    /// `specializes`.
    pub keys_by_type: BTreeMap<String, Vec<String>>,
}

impl MergeSignalCheck for ExternalIdSignal {
    fn check(&self, left: &Entity, right: &Entity) -> bool {
        declared_keys_agree(&self.keys_by_type, left, right)
    }

    fn signal(&self) -> MergeSignal {
        MergeSignal::ExternalId
    }
}

/// Descriptive-key signal — one ordinary signal, gated like the rest.
pub struct DescriptiveKeySignal {
    /// Declared type → fallback key names, already resolved through
    /// `specializes`.
    pub keys_by_type: BTreeMap<String, Vec<String>>,
}

impl MergeSignalCheck for DescriptiveKeySignal {
    fn check(&self, left: &Entity, right: &Entity) -> bool {
        declared_keys_agree(&self.keys_by_type, left, right)
    }

    fn signal(&self) -> MergeSignal {
        MergeSignal::DescriptiveKey
    }
}

/// The blocking key an entity gets from a declared key set, or `None` when the
/// atom does not fill every key in it.
///
/// Two atoms that agree on every declared key produce the same string, so
/// bucketing on it makes them candidates — which is what a declared identifier
/// needs and what the four name/email/org keys cannot give it: a `find_id`
/// match between "Series R sceatta" and "SF-2019-114" shares no name token at
/// all. `prefix` separates the primary map's keys from the fallback map's so a
/// value that happens to collide across the two does not bucket together.
pub(crate) fn identity_blocking_key(
    prefix: &str,
    entity: &Entity,
    keys: &[String],
) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    let mut parts = Vec::with_capacity(keys.len());
    for k in keys {
        parts.push(identity_key_value(entity, k)?);
    }
    Some(format!(
        "{prefix}:{}|{}",
        entity.entity_type.as_str_repr(),
        parts.join("\u{1f}")
    ))
}

/// The stack a policy selects: the default three, plus one identity signal
/// per non-empty declared key map.
pub fn signals_for_policy(identity: &IdentityPolicy) -> Vec<Box<dyn MergeSignalCheck>> {
    let mut stack = default_signals();
    if !identity.identity.is_empty() {
        stack.push(Box::new(ExternalIdSignal {
            keys_by_type: identity.identity.clone(),
        }));
    }
    if !identity.identity_fallback.is_empty() {
        stack.push(Box::new(DescriptiveKeySignal {
            keys_by_type: identity.identity_fallback.clone(),
        }));
    }
    stack
}
