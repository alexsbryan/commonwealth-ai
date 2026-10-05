// SPDX-License-Identifier: AGPL-3.0-or-later
//! `source` on a declared type: where its instances come from without a model
//! call. Two forms, chosen by which key is present: a TABLE the corpus already
//! holds (`file`), or the documents' OWN metadata fields (`metadata`), e.g.
//! the address headers of mail or the author field of an issue tracker.
//!
//! The field names are the corpus's own and live only in the recipe. The one
//! fixed vocabulary here is [`FieldReader`], what to read out of a field's
//! value. Split from `decl.rs`, which renders into `SCHEMA.md` beside it.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

/// A declared type's structural source: a table file (`file`), or the
/// documents' own metadata fields (`metadata`). Exactly one: a source naming
/// both, or neither, refuses at load.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SourceDecl {
    /// A file already holding the type as a table.
    Table(TableSourceDecl),
    /// The documents' own fields, one atom per distinct identity value.
    Metadata(MetadataSourceDecl),
}

/// Dispatch on the key that names the form, then parse that form strictly,
/// so an unknown key or reader is refused naming what was wrong, not as "no
/// variant matched".
impl<'de> Deserialize<'de> for SourceDecl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = serde_json::Value::deserialize(d)?;
        let Some(keys) = raw.as_object() else {
            return Err(D::Error::custom("`source` must be a table"));
        };
        let form = |e: serde_json::Error| D::Error::custom(format!("`source`: {e}"));
        match (keys.contains_key("file"), keys.contains_key("metadata")) {
            (true, true) => Err(D::Error::custom(
                "`source` names both `file` (a table) and `metadata` (the documents' own \
                 fields); a source is one or the other",
            )),
            (false, false) => Err(D::Error::custom(
                "`source` names neither `file` (a table) nor `metadata` (the documents' own \
                 fields)",
            )),
            (true, false) => serde_json::from_value(raw).map(Self::Table).map_err(form),
            (false, true) => serde_json::from_value(raw)
                .map(Self::Metadata)
                .map_err(form),
        }
    }
}

/// `source = { file = … }`: a file already holding the type as a table,
/// ingested without a model call. `from`/`to` name the endpoint columns of a
/// relation; `attributes` maps attribute name → column. Declared only: no
/// stage reads a table source yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableSourceDecl {
    /// Path of the table (CSV or JSONL), relative to the corpus source.
    pub file: String,
    /// Relations: the column holding the `from` endpoint's identity.
    #[serde(default)]
    pub from: Option<String>,
    /// Relations: the column holding the `to` endpoint's identity.
    #[serde(default)]
    pub to: Option<String>,
    /// Declared attribute name → column name.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// `source = { metadata = [...], attributes = {...} }`: one entity atom per
/// distinct value of the type's `identity` attribute seen in the named fields
/// of any document, no model call. A model-extracted atom of the type
/// carrying the same identity value merges into it (strict merge). Mail:
/// `metadata = ["from", "to", "cc"], attributes = { email = "address", name =
/// "display_name" }`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataSourceDecl {
    /// The document metadata fields read, by the corpus's own names.
    pub metadata: Vec<String>,
    /// Declared attribute name → the reader that fills it from each field.
    /// The type's `identity` attributes must be among them.
    #[serde(default)]
    pub attributes: BTreeMap<String, FieldReader>,
    /// Identity values never projected (freemail domains), compared after
    /// the identity fold.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Declared `ref` attribute → the sourced type it links to, and the reader
    /// whose value on the same mailbox is that type's identity value. Contact
    /// → Account: `employer = { of = "company", reader = "domain" }`. A value
    /// the target type excludes or never projects links nothing; a role, never
    /// an identity key (`ONTOLOGY_METHOD.md`).
    #[serde(default)]
    pub refs: BTreeMap<String, SourceRef>,
}

/// One `refs` entry of a metadata source: the target type and the reader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    /// A declared entity type with a metadata source and one identity key.
    pub of: String,
    /// The reader whose value is the target's identity value.
    pub reader: FieldReader,
}

impl MetadataSourceDecl {
    /// Whether any declared reader (attribute or ref) parses address lists.
    pub fn reads_addresses(&self) -> bool {
        self.attributes.values().any(|r| r.reads_addresses()) || self.refs.values().any(|r| r.reader.reads_addresses())
    }

    /// The reader filling each identity key, or why the declaration cannot
    /// project: an atom's identity value IS its identity keys' values, so a
    /// type with no key, or a key no reader fills, has none. The one check,
    /// shared by `recipe validate` and the projection.
    pub fn identity_readers<'a>(
        &self,
        identity: &'a [String],
    ) -> Result<Vec<(&'a str, FieldReader)>, String> {
        if identity.is_empty() {
            return Err(
                "declares no `identity`; a metadata source projects one atom per \
                        identity value"
                    .to_string(),
            );
        }
        identity
            .iter()
            .map(|k| match self.attributes.get(k) {
                Some(r) => Ok((k.as_str(), *r)),
                None => Err(format!(
                    "identity key `{k}` is not a source attribute (source attributes: {})",
                    self.attributes
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            })
            .collect()
    }
}

/// What a metadata source reads out of one field's value. Closed: the only
/// fixed vocabulary a source has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldReader {
    /// Every address in an address list (`Ann <ann@x.org>, bob@y.com`),
    /// lowercased.
    Address,
    /// Each address's domain, lowercased.
    Domain,
    /// The name paired with each address; nothing for a bare address.
    DisplayName,
    /// The field's whole value: a string or number, or each one of a list.
    Value,
}

impl FieldReader {
    pub const ALL: [FieldReader; 4] = [Self::Address, Self::Domain, Self::DisplayName, Self::Value];

    /// The wire spelling (`source_reader_keys_are_the_wire_names` pins it).
    pub const fn key(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Domain => "domain",
            Self::DisplayName => "display_name",
            Self::Value => "value",
        }
    }

    /// Reads a field as an address list, as opposed to its whole value.
    pub const fn reads_addresses(self) -> bool {
        !matches!(self, Self::Value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_reader_keys_are_the_wire_names() {
        for r in FieldReader::ALL {
            assert_eq!(
                serde_json::to_value(r).unwrap(),
                serde_json::json!(r.key()),
                "{r:?}"
            );
        }
    }

    #[test]
    fn a_source_is_a_table_or_metadata_and_never_both() {
        let table: SourceDecl =
            serde_json::from_value(serde_json::json!({ "file": "t.csv" })).unwrap();
        assert!(matches!(table, SourceDecl::Table(_)));
        let meta: SourceDecl = serde_json::from_value(serde_json::json!({
            "metadata": ["author"], "attributes": { "login": "value" }
        }))
        .unwrap();
        assert!(matches!(meta, SourceDecl::Metadata(_)));
        // The untagged serializer writes the form back as its own keys.
        assert_eq!(
            serde_json::from_value::<SourceDecl>(serde_json::to_value(&meta).unwrap()).unwrap(),
            meta
        );
        for bad in [
            serde_json::json!({ "file": "t.csv", "metadata": ["from"] }),
            serde_json::json!({ "attributes": {} }),
        ] {
            assert!(serde_json::from_value::<SourceDecl>(bad).is_err());
        }
    }
}
