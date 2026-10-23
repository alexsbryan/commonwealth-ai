// SPDX-License-Identifier: AGPL-3.0-or-later
//! A resolver's reply made CSL-JSON as CSL 1.0.2 defines it (OICP v0.6
//! §2.16), by the data file's `[csl]` table and the resolver's `types`
//! (platform row PB1b). Crossref's `transform` reply carries its own types
//! (`journal-article`), lists where CSL holds one value (`ISSN`), and names
//! with its own fields (`affiliation`, `role`, `sequence`); a record holding
//! them is not one a CSL processor reads (pandoc 3.7.0.2: "expected String
//! or Number, but encountered Array").

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

/// CSL 1.0.2's shapes for what a reply adds to a record.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CslSchema {
    /// CSL's item types.
    pub types: BTreeSet<String>,
    /// The kept variables that are lists of names.
    pub names: BTreeSet<String>,
    /// A name's fields.
    pub name_fields: BTreeSet<String>,
    /// The kept variables that are dates.
    pub dates: BTreeSet<String>,
    /// A date's fields.
    pub date_fields: BTreeSet<String>,
}

impl CslSchema {
    /// `value`, a reply's `field`, as CSL holds it, or why it is left out.
    /// `types` is the resolver's own: a type it maps is read as CSL's, and
    /// one it does not map is kept only when it already is CSL's. Every
    /// other variable is one string or number: a list is read at its first
    /// item (Crossref lists a journal's ISSNs print first).
    pub(super) fn shape(
        &self,
        types: &BTreeMap<String, String>,
        field: &str,
        value: &Value,
    ) -> Result<Value, &'static str> {
        if field == "type" {
            let t = value.as_str().ok_or("a type that is not a string")?;
            let t = types.get(t).map_or(t, String::as_str);
            return if self.types.contains(t) {
                Ok(Value::String(t.to_string()))
            } else {
                Err("a type outside CSL's")
            };
        }
        if self.names.contains(field) {
            let kept: Vec<Value> = value
                .as_array()
                .ok_or("names that are not a list")?
                .iter()
                .filter_map(Value::as_object)
                .map(|n| only(n, &self.name_fields))
                .filter(|n| !n.is_empty())
                .map(Value::Object)
                .collect();
            return if kept.is_empty() {
                Err("no name with CSL's fields")
            } else {
                Ok(Value::Array(kept))
            };
        }
        if self.dates.contains(field) {
            let kept = only(
                value.as_object().ok_or("a date that is not an object")?,
                &self.date_fields,
            );
            return if kept.is_empty() {
                Err("a date with none of CSL's fields")
            } else {
                Ok(Value::Object(kept))
            };
        }
        let one = match value {
            Value::Array(items) => items.first().ok_or("an empty list")?,
            v => v,
        };
        match one {
            Value::String(_) | Value::Number(_) => Ok(one.clone()),
            _ => Err("neither a string nor a number"),
        }
    }
}

/// `m`'s entries whose keys are in `fields`.
fn only(m: &Map<String, Value>, fields: &BTreeSet<String>) -> Map<String, Value> {
    m.iter()
        .filter(|(k, _)| fields.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
