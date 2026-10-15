// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a reader read a particular to be (`Described`), shown to RESOLVE's
//! model beside a statement and on the record it joins.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// What a reader read a particular to be, across everything it read of it:
/// its supported values of the type's declared attributes other than keys and
/// necessary ones (those travel as `keys` and `read`). An
/// author's identity criterion is written about such values, so the model
/// deciding identity is shown them on both sides of its question; code never
/// compares them, and decides identity only by declared keys and necessary
/// values.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Described {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, BTreeSet<String>>,
}

impl Described {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn extend(&mut self, other: &Described) {
        for (attr, values) in &other.values {
            self.values
                .entry(attr.clone())
                .or_default()
                .extend(values.iter().cloned());
        }
    }

    /// One line's worth: each attribute's values, at most `SHOWN_VALUES` of
    /// each (a cost knob: it bounds the prompt, decides nothing). Empty when
    /// nothing was read.
    pub fn render(&self) -> String {
        let few = |vs: &BTreeSet<String>| {
            let mut out: Vec<&str> = vs.iter().map(String::as_str).take(SHOWN_VALUES).collect();
            if vs.len() > SHOWN_VALUES {
                out.push("…");
            }
            out.join(" | ")
        };
        self.values
            .iter()
            .map(|(attr, values)| format!("{attr}: {}", few(values)))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// How many of one described attribute's values a question shows.
const SHOWN_VALUES: usize = 4;
