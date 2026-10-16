// SPDX-License-Identifier: AGPL-3.0-or-later
//! A document's record (v0.6 §2.16): its declared metadata, merged with
//! what a resolver returned for an identifier it carries, each field naming
//! where it came from. A lookup leaves the host only for an install that
//! allowed one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The origin of a field the recipe or the extractor declared.
pub const DECLARED: &str = "declared";

/// A document's merged record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedRecord {
    /// CSL-JSON: every declared field unchanged, and each field a resolver
    /// returned that none declared.
    pub csl: Map<String, Value>,
    /// Each field of `csl` and where it came from: [`DECLARED`], or the
    /// name of the resolver that returned it.
    pub origins: BTreeMap<String, String>,
    /// Each identifier the metadata carries that the host has a resolver
    /// for, and what came of asking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lookups: Vec<RecordLookup>,
}

/// One identifier and its lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordLookup {
    /// The CSL variable that carries it: `DOI`, `ISBN`.
    pub identifier: String,
    /// Its value as declared.
    pub value: String,
    /// The resolver the host's data file names for that variable.
    pub resolver: String,
    /// What came of it.
    pub outcome: LookupOutcome,
}

/// What came of a lookup. A closed set; a value outside it is a decode
/// error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupOutcome {
    /// The resolver returned a record; its fields no declaration holds
    /// joined the merged record.
    Resolved,
    /// The resolver does not know the identifier. No field is taken.
    Unresolved,
    /// The resolver could not be asked or did not answer. No field is
    /// taken; a later install may ask again.
    Unreachable,
    /// The install did not allow lookups, so none was sent.
    NotAllowed,
}

impl MergedRecord {
    /// The fields whose origin is not [`DECLARED`].
    pub fn looked_up(&self) -> impl Iterator<Item = (&String, &String)> {
        self.origins.iter().filter(|(_, o)| o.as_str() != DECLARED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_travels_with_its_origins_and_a_closed_outcome() {
        let r: MergedRecord = serde_json::from_str(
            r#"{"csl": {"title": "Declared", "container-title": "Found"},
                "origins": {"title": "declared", "container-title": "crossref"},
                "lookups": [{"identifier": "DOI", "value": "10.1/x", "resolver": "crossref",
                             "outcome": "resolved"}]}"#,
        )
        .unwrap();
        assert_eq!(
            r.looked_up().collect::<Vec<_>>(),
            vec![(&"container-title".to_string(), &"crossref".to_string())]
        );
        assert!(serde_json::from_str::<LookupOutcome>("\"guessed\"").is_err());
    }
}
