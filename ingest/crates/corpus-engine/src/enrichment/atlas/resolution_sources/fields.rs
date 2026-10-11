// SPDX-License-Identifier: AGPL-3.0-or-later
//! How a metadata source reads one document field: the scalars a field holds,
//! the addresses of an address list, what each declared [`FieldReader`] reads
//! out of one, and which identity value (if any) a sighting carries after the
//! declaration's `exclude` ([`Exclusion`]). The projection
//! ([`super::project_source_atoms`]) and the passes reader (its prefill and its
//! Pick candidates, `pipeline/document_read/`) read fields through this one
//! module, so the reader never sees a record the projection would not make.

use std::collections::{BTreeMap, HashSet};

use mailparse::{MailAddr, SingleInfo};
use serde_json::Value;

use crate::enrichment::ontology::{FieldReader, MetadataSourceDecl};
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

/// One address of an address list, or the whole value under `value`.
pub(super) struct Item {
    pub(super) addr: Option<String>,
    pub(super) name: Option<String>,
}

/// What one sighting's identity is, by the projection's own rules.
pub(super) enum Sighting {
    /// The folded identity value of each identity key, in key order.
    Key(Vec<String>),
    /// A reader read nothing for some identity key (a bare address read by
    /// `display_name`).
    NoIdentity,
    /// `exclude` names the folded value.
    Excluded(Vec<String>),
}

/// The one decision over a sighting's identity, for the projection and the
/// reader alike.
pub(super) fn sighting(
    identity: &[(&str, FieldReader)],
    excluded: &Exclusion,
    item: &Item,
    scalar: &str,
) -> Sighting {
    let read = |r: FieldReader| read_item(r, item, scalar);
    let folded: Option<Vec<String>> = identity
        .iter()
        .map(|(_, r)| read(*r).as_deref().and_then(fold_identity_value))
        .collect();
    let Some(folded) = folded else {
        return Sighting::NoIdentity;
    };
    if folded.iter().any(|f| excluded.holds(f)) {
        return Sighting::Excluded(folded);
    }
    Sighting::Key(folded)
}

/// What a source's `exclude` declares: identity values, and the lists a
/// `@bundled:<key>` entry names (one value a line, `#` comments), each after
/// the identity fold. A value is excluded when it equals one, or ends in one
/// at a word boundary, as a set's `suffix` condition reads it: an excluded
/// `example.org` covers `mail.example.org`. Code holds no list of its own.
#[derive(Debug, Default)]
pub(super) struct Exclusion {
    values: HashSet<String>,
}

/// The prefix that names a bundled list, as filter configs spell it.
const BUNDLED: &str = "@bundled:";

impl Exclusion {
    /// Read `src.exclude`, refusing a `@bundled:` key that names no list.
    pub(super) fn of(src: &MetadataSourceDecl) -> Result<Self, String> {
        let mut values = HashSet::new();
        for entry in &src.exclude {
            match entry.strip_prefix(BUNDLED) {
                Some(key) => {
                    let bytes = crate::recipe_source::default_assets()
                        .bundled_asset(key)
                        .ok_or_else(|| {
                            format!("`exclude` names `{entry}`, which is no bundled list")
                        })?;
                    let text = std::str::from_utf8(bytes)
                        .map_err(|e| format!("`exclude` list `{entry}` is not UTF-8: {e}"))?;
                    values.extend(
                        text.lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty() && !l.starts_with('#'))
                            .filter_map(fold_identity_value),
                    );
                }
                None => values.extend(fold_identity_value(entry)),
            }
        }
        tracing::debug!(
            declared = src.exclude.len(),
            values = values.len(),
            "atlas/resolution sources: exclusion read"
        );
        Ok(Self { values })
    }

    /// Whether a folded identity value is excluded: equal to a declared one,
    /// or ending in one at a word boundary.
    pub(super) fn holds(&self, folded: &str) -> bool {
        self.values.contains(folded)
            || folded
                .match_indices(' ')
                .any(|(at, _)| self.values.contains(&folded[at + 1..]))
    }
}

/// One record a document field names for a sourced type, as the projection
/// would key it: the folded identity key, each declared attribute's value as
/// read, and the field's scalar it was read from (an exact passage of the
/// document's metadata, so it can be cited).
#[derive(Debug, Clone, PartialEq)]
pub struct FieldRecord {
    pub key: String,
    /// The identity keys' values as read, in key order, before the fold.
    pub identity: Vec<String>,
    pub attributes: BTreeMap<String, String>,
    pub scalar: String,
}

/// Every record `value` (one document field's value) names for a type with
/// metadata source `src`, skipping what the projection skips: no identity,
/// excluded, unreadable. Nothing when the declaration names no identity reader
/// or a `@bundled:` list that does not exist, which the projection refuses.
pub fn field_records(
    src: &MetadataSourceDecl,
    identity_keys: &[String],
    value: &Value,
) -> Vec<FieldRecord> {
    let Ok(identity) = src.identity_readers(identity_keys) else {
        return Vec::new();
    };
    let Ok(Some(scalars)) = scalars_of(value) else {
        return Vec::new();
    };
    let Ok(excluded) = Exclusion::of(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for scalar in scalars {
        let items = if src.reads_addresses() {
            match addresses(&scalar) {
                Ok(items) => items,
                // The projection counts this field as unreadable and makes no
                // record of it; the reader names none either.
                Err(why) => {
                    tracing::debug!(%scalar, %why, "atlas/resolution sources: field unreadable; no record named");
                    continue;
                }
            }
        } else {
            vec![Item {
                addr: None,
                name: None,
            }]
        };
        for item in items {
            let Sighting::Key(folded) = sighting(&identity, &excluded, &item, &scalar) else {
                continue;
            };
            let read = |r: FieldReader| read_item(r, &item, &scalar);
            out.push(FieldRecord {
                key: folded.join("\u{1f}"),
                identity: identity.iter().filter_map(|(_, r)| read(*r)).collect(),
                attributes: src
                    .attributes
                    .iter()
                    .filter_map(|(a, r)| read(*r).map(|v| (a.clone(), v)))
                    .collect(),
                scalar: scalar.clone(),
            });
        }
    }
    out
}

/// A field's value as the scalars a reader reads: a string or number, or each
/// one of a list. `Ok(None)` when it holds nothing (null, blank, empty list).
pub(super) fn scalars_of(v: &Value) -> Result<Option<Vec<String>>, String> {
    let one = |x: &Value| match x {
        Value::String(s) => Ok(s.trim().to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Null => Ok(String::new()),
        Value::Bool(_) => Err("holds a boolean".to_string()),
        Value::Array(_) => Err("holds a nested list".to_string()),
        Value::Object(_) => Err("holds an object".to_string()),
    };
    let all = match v {
        Value::Array(xs) => xs.iter().map(one).collect::<Result<Vec<_>, _>>()?,
        x => vec![one(x)?],
    };
    let all: Vec<String> = all.into_iter().filter(|s| !s.is_empty()).collect();
    Ok((!all.is_empty()).then_some(all))
}

/// The addresses of an RFC 5322 address list (groups flattened).
pub(super) fn addresses(list: &str) -> Result<Vec<Item>, String> {
    let parsed = mailparse::addrparse(list).map_err(|e| format!("is not an address list ({e})"))?;
    let item = |s: &SingleInfo| Item {
        addr: Some(s.addr.trim().to_lowercase()).filter(|a| !a.is_empty()),
        name: s
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string),
    };
    let out: Vec<Item> = parsed
        .iter()
        .flat_map(|a| match a {
            MailAddr::Single(s) => vec![item(s)],
            MailAddr::Group(g) => g.addrs.iter().map(item).collect(),
        })
        .collect();
    if out.is_empty() {
        return Err("holds no address".to_string());
    }
    Ok(out)
}

pub(super) fn read_item(reader: FieldReader, item: &Item, scalar: &str) -> Option<String> {
    match reader {
        FieldReader::Address => item.addr.clone(),
        FieldReader::Domain => item
            .addr
            .as_deref()
            .and_then(|a| a.rsplit_once('@'))
            .map(|(_, d)| d.to_string())
            .filter(|d| !d.is_empty()),
        FieldReader::DisplayName => item.name.clone(),
        FieldReader::Value => Some(scalar.to_string()),
    }
}

#[cfg(test)]
#[path = "fields_tests.rs"]
mod tests;
