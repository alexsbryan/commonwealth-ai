// SPDX-License-Identifier: AGPL-3.0-or-later
//! A shelf: a small library one v0.6 check installs as its own corpus,
//! beside the fixture library, so a check that needs texts of a shape the
//! fixture does not hold (headed, transcribed) never changes what the other
//! checks read. Data in `fixture/<check>.json`, rendered into an inline
//! recipe.

use oicp_types::{ProviderManifest, SpanLocator};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::checks::Host;
use crate::ingest_checks::wait_terminal;

const SHELF_HEADER: &str = include_str!("../fixture/shelf_header.toml");

/// A shelf's corpus and its documents.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shelf {
    /// The corpus it installs as.
    pub corpus_id: String,
    /// Its documents, inline.
    pub documents: Vec<ShelfDoc>,
}

/// One document of a shelf.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShelfDoc {
    /// The `[[document]]` name.
    pub name: String,
    /// The `[[document]]`'s `format` (v0.6 §2.14), when it declares one.
    #[serde(default)]
    pub format: Option<String>,
    /// Declared metadata.
    #[serde(default)]
    pub metadata: Option<Value>,
    /// The source's bytes, as the recipe carries them.
    pub text: String,
}

/// A shelf and where each of its quotations stands (v0.6 §2.13): the
/// locator a span of exactly that quotation carries, or none.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocatedShelf {
    /// The library.
    pub shelf: Shelf,
    /// Quotations of its stored texts and their locators.
    pub located: Vec<Located>,
    /// Documents the install lists `failed` (v0.6 §2.9), and why; every
    /// other document is listed `ready`.
    #[serde(default)]
    pub failed: Vec<FailedDoc>,
}

/// A shelf document an install cannot store, and the reason it names.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailedDoc {
    /// The document.
    pub document: String,
    /// The reason its progress entry names.
    pub reason: String,
}

/// A quotation of one shelf document's stored text, and its locator.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Located {
    /// The document it is from.
    pub document: String,
    /// The quotation, verbatim from the stored text.
    pub quote: String,
    /// The locator its span carries; `null` for none.
    pub locator: Option<SpanLocator>,
}

impl LocatedShelf {
    /// A located shelf from its fixture file, each quotation naming one of
    /// its documents.
    pub fn load(json: &str) -> Result<Self, String> {
        let s: Self = serde_json::from_str(json).map_err(|e| format!("does not load: {e}"))?;
        let named = s.located.iter().map(|l| &l.document);
        for d in named.chain(s.failed.iter().map(|f| &f.document)) {
            if s.shelf.doc(d).is_none() {
                return Err(format!("`{d}` is no document of the shelf"));
            }
        }
        Ok(s)
    }
}

impl Shelf {
    /// The document named `name`.
    pub fn doc(&self, name: &str) -> Option<&ShelfDoc> {
        self.documents.iter().find(|d| d.name == name)
    }

    /// The recipe with the documents inline.
    pub fn inline_recipe(&self) -> String {
        let mut out = SHELF_HEADER.replace("{corpus_id}", &self.corpus_id);
        for d in &self.documents {
            out += &format!("\n[[document]]\nname = \"{}\"\n", d.name);
            if let Some(f) = &d.format {
                out += &format!("format = \"{f}\"\n");
            }
            if let Some(m) = &d.metadata {
                out += &format!("metadata = '''{m}'''\n");
            }
            out += &format!("text = '''{}'''\n", d.text);
        }
        out
    }

    /// Install the shelf and wait for its ingest to settle. `extra` is
    /// merged into the install request (v0.6 §2.16's `allow_lookups`).
    pub async fn install(
        &self,
        host: &Host,
        m: &ProviderManifest,
        extra: &[(&str, Value)],
    ) -> Result<(), String> {
        let ing = m
            .knowledge
            .as_ref()
            .and_then(|k| k.ingest.as_ref())
            .ok_or("no knowledge.ingest to install the shelf with")?;
        let mut body = json!({"corpus_id": self.corpus_id, "recipe_toml": self.inline_recipe()});
        for (k, v) in extra {
            body[*k] = v.clone();
        }
        let r = host.post(&ing.install_endpoint, &body).await?;
        if r.status != 200 {
            return Err(format!("installing {}: {}", self.corpus_id, r.brief()));
        }
        wait_terminal(host, &ing.progress_endpoint, &self.corpus_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_headings_shelf_loads_and_renders_its_documents_inline() {
        let s = LocatedShelf::load(include_str!("../fixture/headings.json")).unwrap();
        let r = s.shelf.inline_recipe();
        assert!(r.contains("id = \"oicp-conformance-headings\""));
        assert!(r.contains("name = \"annals\"\ntext = '''Annals"));
        for d in &s.shelf.documents {
            assert!(!d.text.contains("'''"), "{}", d.name);
        }
        for l in &s.located {
            assert!(s.shelf.doc(&l.document).unwrap().text.contains(&l.quote));
        }
    }
}
