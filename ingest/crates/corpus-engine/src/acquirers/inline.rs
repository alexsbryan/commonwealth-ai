// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[acquire] type = "inline"` (OICP v0.5 §4.1): the recipe carries its
//! documents as `[[document]]` blocks with `name` and `text`, so a host that
//! shares no disk with its client can install a library.
//!
//! Acquiring writes each document's text, as UTF-8, to
//! `<download_dir>/<corpus>/inline/<name>`. The recipe's `[extract]` then reads
//! each of those files alone, as a reindex reads its staged temp file, and
//! [`InlineExtractor`] states what came out as that document: source id
//! `name`, source bytes the UTF-8 of `text` (`DocSource::Hashed`).

use std::path::{Path, PathBuf};

use kernel_types::Sha256Hash;

use crate::error::{Error, Result};
use crate::extractors::{DocSource, ExtractedDoc, Extractor};
use crate::recipe::Recipe;
use crate::recipe_documents::DeclaredDocument;

/// The inline documents of `recipe`, in recipe order: `(name, text)`.
fn documents(recipe: &Recipe) -> impl Iterator<Item = (&str, &str)> {
    recipe.documents.iter().filter_map(|d| match d {
        DeclaredDocument::Inline { name, text, .. } => Some((name.as_str(), text.as_str())),
        DeclaredDocument::File { .. } => None,
    })
}

/// Write each inline document's text to `<download_dir>/<corpus>/inline/<name>`
/// and return that directory. The directory is rewritten whole, so a revised
/// recipe leaves no document it dropped behind.
pub fn materialize(recipe: &Recipe, download_dir: &Path) -> Result<PathBuf> {
    let dir = download_dir.join(&recipe.corpus.id).join("inline");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let (mut count, mut bytes) = (0usize, 0usize);
    for (name, text) in documents(recipe) {
        let tmp = dir.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, dir.join(name))?;
        count += 1;
        bytes += text.len();
    }
    tracing::info!(
        corpus = %recipe.corpus.id,
        documents = count,
        bytes,
        dir = %dir.display(),
        "inline acquire: the recipe's documents written for its extractor"
    );
    Ok(dir)
}

/// The recipe's `[extract]`, run over each inline document's bytes alone,
/// each result stated as that document.
pub struct InlineExtractor {
    inner: Box<dyn Extractor>,
    /// `(name, sha256 of the text's UTF-8)`, in recipe order.
    documents: Vec<(String, Sha256Hash)>,
    /// The record's extractor: the recipe's `[extract]` tag.
    extractor: String,
}

impl InlineExtractor {
    /// Wrap `inner`, the extractor `recipe.extract` builds.
    pub fn new(inner: Box<dyn Extractor>, recipe: &Recipe) -> Self {
        Self {
            inner,
            documents: documents(recipe)
                .map(|(name, text)| (name.to_string(), Sha256Hash::of_str(text)))
                .collect(),
            extractor: crate::text_store::extractor_tag(&recipe.extract),
        }
    }

    fn one(&self, dir: &Path, name: &str, sha256: Sha256Hash) -> Vec<Result<ExtractedDoc>> {
        let file = dir.join(name);
        let produced: Vec<Result<ExtractedDoc>> = match self.inner.extract(&file) {
            Ok(docs) => docs.collect(),
            Err(e) => vec![Err(e)],
        };
        if produced.is_empty() {
            tracing::warn!(name, extractor = %self.extractor, "inline extract: the document yielded nothing");
            return vec![Err(Error::Extraction(format!(
                "inline document `{name}` gave the `{}` extractor nothing to read",
                self.extractor
            )))];
        }
        tracing::debug!(name, docs = produced.len(), %sha256, "inline extract: document read");
        produced
            .into_iter()
            .map(|r| {
                r.map(|mut doc| {
                    doc.source_id = name.to_string();
                    doc.url = None;
                    doc.source = DocSource::Hashed {
                        sha256,
                        extractor: self.extractor.clone(),
                    };
                    doc
                })
            })
            .collect()
    }
}

impl Extractor for InlineExtractor {
    /// `source_path` is the directory [`materialize`] wrote. A recipe's inline
    /// documents are few, so they are read here, whole, in recipe order.
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let docs: Vec<Result<ExtractedDoc>> = self
            .documents
            .iter()
            .flat_map(|(name, sha256)| self.one(source_path, name, *sha256))
            .collect();
        Ok(Box::new(docs.into_iter()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractors::plaintext::PlaintextExtractor;

    fn recipe(documents: &str) -> Recipe {
        Recipe::from_toml(&format!(
            "[corpus]\nid = \"c\"\nname = \"C\"\ndescription = \"d\"\nlicense = \"MIT\"\n\
             mesh_sharing = false\n\n[acquire]\ntype = \"inline\"\n\n[extract]\n\
             type = \"plaintext\"\n\n[chunk]\ntype = \"paragraph\"\n{documents}"
        ))
        .unwrap()
    }

    #[test]
    fn each_document_is_read_by_the_recipes_extractor_and_stated_as_itself() {
        // The plaintext extractor names a document by its `Title:` line; an
        // inline document is named by the recipe, whatever its text says.
        let r = recipe(
            "[[document]]\nname = \"doc-1\"\ntext = '''Title: A Book\n\nThe body.'''\n\
             [[document]]\nname = \"doc-2\"\ntext = \"Second.\"\n",
        );
        let dir = tempfile::tempdir().unwrap();
        let acquired = materialize(&r, dir.path()).unwrap();
        let inner = Box::new(PlaintextExtractor::new(None, None));
        let docs: Vec<ExtractedDoc> = InlineExtractor::new(inner, &r)
            .extract(&acquired)
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        let ids: Vec<&str> = docs.iter().map(|d| d.source_id.as_str()).collect();
        assert_eq!(ids, ["doc-1", "doc-2"], "recipe order, recipe names");
        assert_eq!(
            docs[0].title.as_deref(),
            Some("A Book"),
            "the extractor ran"
        );
        assert_eq!(
            docs[0].source,
            DocSource::Hashed {
                sha256: Sha256Hash::of_str("Title: A Book\n\nThe body."),
                extractor: crate::text_store::extractor_tag(&r.extract),
            },
            "the source bytes are the text's UTF-8"
        );
    }

    #[test]
    fn a_document_its_extractor_cannot_read_is_named() {
        let r = recipe("[[document]]\nname = \"blank\"\ntext = \"   \"\n");
        let dir = tempfile::tempdir().unwrap();
        let acquired = materialize(&r, dir.path()).unwrap();
        let inner = Box::new(PlaintextExtractor::new(None, None));
        let got: Vec<Result<ExtractedDoc>> = InlineExtractor::new(inner, &r)
            .extract(&acquired)
            .unwrap()
            .collect();
        let [Err(e)] = got.as_slice() else {
            panic!("one named failure, got {} items", got.len());
        };
        assert!(e.to_string().contains("inline document `blank`"), "{e}");
    }

    #[test]
    fn a_revised_recipe_leaves_no_dropped_document_behind() {
        let dir = tempfile::tempdir().unwrap();
        let first = recipe("[[document]]\nname = \"old\"\ntext = \"x\"\n");
        materialize(&first, dir.path()).unwrap();
        let second = recipe("[[document]]\nname = \"new\"\ntext = \"y\"\n");
        let acquired = materialize(&second, dir.path()).unwrap();
        let names: Vec<String> = std::fs::read_dir(&acquired)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["new"]);
    }
}
