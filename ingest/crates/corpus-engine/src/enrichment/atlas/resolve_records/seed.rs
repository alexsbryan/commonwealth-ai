// SPDX-License-Identifier: AGPL-3.0-or-later
//! A record that exists before RESOLVE reads any statement: one a metadata
//! source read from documents' fields (`resolution_sources`), kept under its
//! own id so its atom and every reference to it survive. It holds its source's
//! keys, so a statement carrying one links to it by key, and it is offered to
//! the mentions of the documents whose fields name it (`propose::FieldNamed`).

use super::Resolver;

impl Resolver {
    /// Open the record `id` before any statement, holding `keys` (attribute,
    /// folded value). Returns its position.
    pub fn seed(&mut self, id: &str, keys: &[(String, String)]) -> usize {
        let r = self.open(id);
        for (k, v) in keys {
            self.records[r]
                .keys
                .entry(k.clone())
                .or_default()
                .insert(v.clone());
            self.by_key.entry((k.clone(), v.clone())).or_insert(r);
        }
        r
    }
}
