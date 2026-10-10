// SPDX-License-Identifier: AGPL-3.0-or-later
//! How a metadata source reads one document field: the scalars a field holds,
//! the mailboxes of an address list, what each declared [`FieldReader`] reads
//! out of one, and which identity value (if any) a sighting carries after the
//! declaration's `exclude` and the mailbox-provider skip. The projection
//! ([`super::project_source_atoms`]) and the passes reader (its prefill and its
//! Pick candidates, `pipeline/document_read/`) read fields through this one
//! module, so the reader never sees a record the projection would not make.

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use mailparse::{MailAddr, SingleInfo};
use serde_json::Value;

use crate::enrichment::ontology::{FieldReader, MetadataSourceDecl};
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

/// One mailbox of an address list, or the whole value under `value`.
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
    /// A `domain`-read identity value is a mailbox provider's.
    Provider(Vec<String>),
}

/// The one decision over a sighting's identity, for the projection and the
/// reader alike.
pub(super) fn sighting(
    identity: &[(&str, FieldReader)],
    excluded: &HashSet<String>,
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
    if folded.iter().any(|f| excluded.contains(f)) {
        return Sighting::Excluded(folded);
    }
    // the raw domain, not the folded key: folding turns '.' and '-' alike into spaces
    if identity.iter().any(|(_, r)| {
        *r == FieldReader::Domain && read(*r).is_some_and(|d| is_mailbox_provider(&d))
    }) {
        return Sighting::Provider(folded);
    }
    Sighting::Key(folded)
}

/// `exclude`'s values after the identity fold.
pub(super) fn excluded(src: &MetadataSourceDecl) -> HashSet<String> {
    src.exclude
        .iter()
        .filter_map(|e| fold_identity_value(e))
        .collect()
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
/// excluded, a mailbox provider's, unreadable. Nothing when the declaration
/// names no identity reader.
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
    let excluded = excluded(src);
    let mut out = Vec::new();
    for scalar in scalars {
        let items = if src.reads_addresses() {
            match mailboxes(&scalar) {
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

/// The mailboxes of an RFC 5322 address list (groups flattened).
pub(super) fn mailboxes(list: &str) -> Result<Vec<Item>, String> {
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

/// Whether `domain` is a mailbox provider's: on the bundled `mailbox_providers`
/// list, or a subdomain of a listed domain (`email.msn.com` is `msn.com`'s). The
/// list is compiled in through the asset port, so its absence is a build defect.
pub(super) fn is_mailbox_provider(domain: &str) -> bool {
    static LISTED: OnceLock<HashSet<&'static str>> = OnceLock::new();
    let listed = LISTED.get_or_init(|| {
        let bytes = crate::recipe_source::default_assets()
            .bundled_asset("mailbox_providers")
            .expect("the mailbox_providers asset is compiled in");
        std::str::from_utf8(bytes)
            .expect("mailbox_providers is UTF-8")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    });
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let mut d = domain.as_str();
    loop {
        if listed.contains(d) {
            return true;
        }
        match d.split_once('.') {
            Some((_, rest)) if rest.contains('.') => d = rest,
            _ => return false,
        }
    }
}
