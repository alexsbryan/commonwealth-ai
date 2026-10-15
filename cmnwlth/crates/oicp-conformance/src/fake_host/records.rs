// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fake's records by identifier (v0.6 §2.16): a document's declared
//! metadata merged with what its resolver table answers for a DOI or ISBN,
//! for a corpus whose install allowed lookups. The table is
//! `fixture/records.json`'s `fake_resolver`, a stand-in for a resolver, not
//! a recording of one. A child of `fake_host`, so it reads the fake's
//! private state.

use std::collections::BTreeMap;

use oicp_types::record::DECLARED;
use oicp_types::{LookupOutcome, MergedRecord, RecordLookup};
use serde_json::{json, Map, Value};

use super::{Fake, Stored};

/// The fake's data file mapping a CSL variable to its resolver.
const RESOLVERS: &[(&str, &str)] = &[("DOI", "crossref"), ("ISBN", "open_library")];

/// The fake resolver's answers, by identifier.
pub(super) fn resolver_table() -> BTreeMap<String, Value> {
    let fixture: Value =
        serde_json::from_str(include_str!("../../fixture/records.json")).expect("records parse");
    serde_json::from_value(fixture["fake_resolver"].clone()).expect("fake_resolver loads")
}

impl Fake {
    /// `s`'s merged record, `None` when its metadata carries no identifier
    /// the fake resolves. `lookup_without_allowance` looks up for every
    /// corpus; `lookup_overwrites_declared` lets a resolver's field replace
    /// a declared one; `lookup_invents_unresolved` gives an identifier that
    /// did not resolve a title.
    pub(super) fn merged(&self, s: &Stored) -> Option<MergedRecord> {
        let b = &self.breaks;
        let declared = s.metadata.as_ref()?.as_object()?;
        let allowed =
            b.lookup_without_allowance || self.allowed.lock().unwrap().contains(&s.corpus);
        let mut csl = declared.clone();
        let mut origins: BTreeMap<String, String> = declared
            .keys()
            .map(|k| (k.clone(), DECLARED.to_string()))
            .collect();
        let mut lookups = Vec::new();
        for (var, resolver) in RESOLVERS {
            let Some(value) = declared.get(*var).and_then(Value::as_str) else {
                continue;
            };
            let found = self.resolved.get(value).and_then(Value::as_object);
            let outcome = match (allowed, found) {
                (false, _) => LookupOutcome::NotAllowed,
                (true, Some(_)) => LookupOutcome::Resolved,
                (true, None) => LookupOutcome::Unresolved,
            };
            let fields = match (outcome, found) {
                (LookupOutcome::Resolved, Some(r)) => r.clone(),
                (LookupOutcome::Unresolved, _) if b.lookup_invents_unresolved => {
                    Map::from_iter([("title".to_string(), json!("Untitled"))])
                }
                _ => Map::new(),
            };
            for (k, v) in fields {
                let overwrite = b.lookup_overwrites_declared && declared.get(&k) != Some(&v);
                if !declared.contains_key(&k) || overwrite {
                    csl.insert(k.clone(), v);
                    origins.insert(k, resolver.to_string());
                }
            }
            lookups.push(RecordLookup {
                identifier: var.to_string(),
                value: value.to_string(),
                resolver: resolver.to_string(),
                outcome,
            });
        }
        (!lookups.is_empty()).then_some(MergedRecord {
            csl,
            origins,
            lookups,
        })
    }
}
