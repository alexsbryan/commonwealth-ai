// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[[document]]` blocks (OICP v0.5 §4.1, ADDRESSED_TEXT §5.4): a recipe that
//! carries its documents inline, or declares metadata for the files it reads.
//!
//! - **`name` + `text`** is an inline document, read by `[acquire] type =
//!   "inline"` ([`crate::acquirers::inline`]). Its source bytes are the UTF-8
//!   of `text` and its source id is `name`.
//! - **`source`** declares metadata for one file the recipe's acquirer yields,
//!   named relative to the source root, as `DocSource::File` names it.
//!
//! In both, `metadata` is a JSON string: parsed for validity when the recipe
//! loads, and stored verbatim. [`DeclaredMetadata::input`] is the one place
//! the rule is applied — declared metadata replaces the extractor's for that
//! source — and every `store_document` input is built there.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::index::{DocSource, DocumentInput};
use crate::recipe::{AcquirerConfig, Recipe};

/// One `[[document]]` block. A closed set of two: the block carries its text,
/// or names a file the acquirer reads. Anything else is refused at load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DocumentBlock", into = "DocumentBlock")]
pub enum DeclaredDocument {
    /// `name` + `text` (+ `metadata`), read by `[acquire] type = "inline"`.
    Inline {
        /// The document's source id.
        name: String,
        /// Its source bytes, as UTF-8.
        text: String,
        /// Declared metadata, JSON text, verbatim.
        metadata: Option<String>,
    },
    /// `source` (+ `metadata`): metadata for one file under the source root.
    File {
        /// The file, relative to the source root.
        source: String,
        /// Declared metadata, JSON text, verbatim.
        metadata: Option<String>,
    },
}

/// The block as written, before it is decided which kind it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<String>,
}

impl TryFrom<DocumentBlock> for DeclaredDocument {
    type Error = String;

    fn try_from(b: DocumentBlock) -> std::result::Result<Self, String> {
        let label = b
            .name
            .clone()
            .or_else(|| b.source.clone())
            .unwrap_or_else(|| "?".into());
        if let Some(m) = &b.metadata {
            serde_json::from_str::<serde_json::Value>(m).map_err(|e| {
                format!("[[document]] `{label}`: `metadata` is not valid JSON ({e})")
            })?;
        }
        match (b.name, b.text, b.source) {
            (Some(name), Some(text), None) => {
                check_name(&name)?;
                Ok(Self::Inline {
                    name,
                    text,
                    metadata: b.metadata,
                })
            }
            (None, Some(_), None) => {
                Err("an inline [[document]] needs a `name`, which is its source id".into())
            }
            (None, None, Some(source)) => {
                check_source(&source)?;
                Ok(Self::File {
                    source,
                    metadata: b.metadata,
                })
            }
            (Some(_), None, Some(source)) => Err(format!(
                "[[document]] `{source}`: `name` belongs to an inline document; a file is \
                 named by its `source`"
            )),
            (_, Some(_), Some(_)) => Err(format!(
                "[[document]] `{label}` carries both `text` and `source`: exactly one, `text` \
                 for an inline document or `source` for a file"
            )),
            (_, None, None) => Err(format!(
                "[[document]] `{label}` carries neither `text` nor `source`: exactly one, \
                 `text` for an inline document or `source` for a file"
            )),
        }
    }
}

impl From<DeclaredDocument> for DocumentBlock {
    fn from(d: DeclaredDocument) -> Self {
        match d {
            DeclaredDocument::Inline {
                name,
                text,
                metadata,
            } => Self {
                name: Some(name),
                text: Some(text),
                source: None,
                metadata,
            },
            DeclaredDocument::File { source, metadata } => Self {
                name: None,
                text: None,
                source: Some(source),
                metadata,
            },
        }
    }
}

/// An inline document's name becomes a file name in the acquire directory,
/// so it is one path component.
fn check_name(name: &str) -> std::result::Result<(), String> {
    let one_component = matches!(
        Path::new(name).components().collect::<Vec<_>>().as_slice(),
        [Component::Normal(_)]
    );
    if name.is_empty() || !one_component || name.contains(['/', '\\', '\0']) {
        return Err(format!(
            "inline [[document]] name `{name}` must be one file-name component (no `/`, `\\`, \
             `.` or `..`)"
        ));
    }
    Ok(())
}

/// A declared file stays under the source root.
fn check_source(source: &str) -> std::result::Result<(), String> {
    let p = Path::new(source);
    if source.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(format!(
            "[[document]] source `{source}` must be a path relative to the source root, \
             without `..`"
        ));
    }
    Ok(())
}

/// The cross-block rules, applied by `Recipe::from_toml`: an inline recipe
/// carries at least one document and only inline ones, with distinct names;
/// any other recipe carries only file declarations, each file named once.
pub(crate) fn check_documents(recipe: &Recipe) -> Result<()> {
    let inline = matches!(recipe.acquire, AcquirerConfig::Inline);
    if inline && recipe.documents.is_empty() {
        return Err(Error::Recipe(
            "[acquire] type = \"inline\" reads the recipe's [[document]] blocks, and this \
             recipe has none"
                .into(),
        ));
    }
    let mut seen: HashMap<&str, ()> = HashMap::new();
    for d in &recipe.documents {
        let key = match (d, inline) {
            (DeclaredDocument::Inline { name, .. }, true) => name.as_str(),
            (DeclaredDocument::File { source, .. }, false) => source.as_str(),
            (DeclaredDocument::File { source, .. }, true) => {
                return Err(Error::Recipe(format!(
                    "[[document]] source `{source}`: an inline recipe reads no files; give the \
                     document its `name` and `text`"
                )))
            }
            (DeclaredDocument::Inline { name, .. }, false) => {
                return Err(Error::Recipe(format!(
                    "[[document]] `{name}` carries its text, which only [acquire] type = \
                     \"inline\" reads"
                )))
            }
        };
        if seen.insert(key, ()).is_some() {
            return Err(Error::Recipe(format!(
                "[[document]] `{key}` is declared twice"
            )));
        }
    }
    Ok(())
}

/// The recipe's declared metadata, indexed for the documents an ingest stores.
/// Owned, so a path that loads its recipe late can hold it.
#[derive(Debug, Default)]
pub struct DeclaredMetadata {
    /// Inline documents' metadata, by name (their source id).
    by_name: HashMap<String, String>,
    /// File declarations' metadata, by source path relative to the root.
    by_source: HashMap<PathBuf, String>,
    /// The source root files are named relative to: the acquired path.
    root: Option<PathBuf>,
}

impl DeclaredMetadata {
    /// `recipe`'s declarations. `root` is the acquired path its file
    /// declarations are relative to, `None` on a path whose sources are never
    /// files (the watched-folder delta's are stated, a reindex's a temp file).
    pub fn of(recipe: &Recipe, root: Option<&Path>) -> Self {
        let mut out = Self {
            root: root.map(Path::to_path_buf),
            ..Self::default()
        };
        for d in &recipe.documents {
            match d {
                DeclaredDocument::Inline {
                    name,
                    metadata: Some(m),
                    ..
                } => {
                    out.by_name.insert(name.clone(), m.clone());
                }
                DeclaredDocument::File {
                    source,
                    metadata: Some(m),
                } => {
                    out.by_source.insert(PathBuf::from(source), m.clone());
                }
                _ => {}
            }
        }
        tracing::debug!(
            corpus = %recipe.corpus.id,
            inline = out.by_name.len(),
            files = out.by_source.len(),
            root = ?out.root,
            "declared metadata indexed"
        );
        out
    }

    /// The input `store_document` is given for one document: THE rule, that
    /// declared metadata replaces the extractor's for that source, applied
    /// here and nowhere else.
    pub fn input<'a>(
        &'a self,
        text: &'a str,
        source_id: &'a str,
        ordinal: u32,
        source: &'a DocSource,
        extracted: Option<&serde_json::Value>,
    ) -> DocumentInput<'a> {
        let metadata = match self.declared(source_id, source) {
            Some(declared) => {
                tracing::debug!(source_id, "record metadata: the recipe's declaration");
                Some(Cow::Borrowed(declared))
            }
            None => extracted.map(|v| Cow::Owned(v.to_string())),
        };
        DocumentInput {
            text,
            source_id,
            ordinal,
            source,
            metadata,
        }
    }

    fn declared(&self, source_id: &str, source: &DocSource) -> Option<&str> {
        match source {
            // An inline document is stated by its acquirer, under its name.
            DocSource::Hashed { .. } => self.by_name.get(source_id).map(String::as_str),
            DocSource::File(path) => {
                let root = self.root.as_deref()?;
                let rel = match path.strip_prefix(root) {
                    Ok(rel) if rel.as_os_str().is_empty() => Path::new(path.file_name()?),
                    Ok(rel) => rel,
                    Err(_) => return None,
                };
                self.by_source.get(rel).map(String::as_str)
            }
            DocSource::Record => None,
        }
    }
}

#[cfg(test)]
#[path = "recipe_documents_tests.rs"]
mod tests;
