// SPDX-License-Identifier: AGPL-3.0-or-later
//! The suite's fixture library (v0.5 §6.1): three short documents, two with
//! declared metadata, and one sentence the evidence checks plant a changed
//! word in. Data in `fixture/library.json`, rendered into the recipe the host
//! installs: inline by default, or as files in `--fixture-dir` on a local
//! host.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

const LIBRARY: &str = include_str!("../fixture/library.json");
const INLINE_HEADER: &str = include_str!("../fixture/inline_header.toml");
const DIR_HEADER: &str = include_str!("../fixture/dir_header.toml");

/// The fixture library.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Library {
    /// The corpus the fixture installs as.
    pub corpus_id: String,
    /// Its documents.
    pub documents: Vec<FixtureDoc>,
    /// The misquote the align check plants.
    pub plant: Plant,
    /// One verbatim sentence per document, by name: what the checks search
    /// for and align to find each document's record.
    pub signatures: BTreeMap<String, String>,
}

/// One fixture document.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureDoc {
    /// The `[[document]]` name; on disk, `<name>.txt`.
    pub name: String,
    /// Declared metadata, which a `knowledge:document` host returns verbatim.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// The source's words.
    pub text: String,
}

/// A sentence of one document, and the word the align check changes in it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plant {
    /// The document the sentence is from.
    pub document: String,
    /// The sentence, verbatim.
    pub sentence: String,
    /// The word in it that is changed.
    pub word: String,
    /// What it is changed to.
    pub planted: String,
}

impl Library {
    /// The compiled-in library. A test pins that it loads and is
    /// self-consistent; an error here names the defect.
    pub fn load() -> Result<Self, String> {
        let lib: Library = serde_json::from_str(LIBRARY)
            .map_err(|e| format!("fixture/library.json does not load: {e}"))?;
        let doc = lib
            .doc(&lib.plant.document)
            .ok_or_else(|| format!("plant names no document `{}`", lib.plant.document))?;
        if !doc.text.contains(&lib.plant.sentence) {
            return Err("the plant's sentence is not in its document".into());
        }
        if lib.plant.sentence.matches(&lib.plant.word).count() != 1 {
            return Err("the planted word must occur once in its sentence".into());
        }
        for d in &lib.documents {
            let sig = lib
                .signatures
                .get(&d.name)
                .ok_or_else(|| format!("no signature for `{}`", d.name))?;
            if !d.text.contains(sig.as_str()) || d.text.contains("'''") {
                return Err(format!(
                    "`{}`: signature not in text, or text holds '''",
                    d.name
                ));
            }
        }
        Ok(lib)
    }

    /// The document named `name`.
    pub fn doc(&self, name: &str) -> Option<&FixtureDoc> {
        self.documents.iter().find(|d| d.name == name)
    }

    /// The recipe with its documents inline (v0.5 §6.1).
    pub fn inline_recipe(&self) -> String {
        let mut out = INLINE_HEADER.replace("{corpus_id}", &self.corpus_id);
        for d in &self.documents {
            out += &format!(
                "\n[[document]]\nname = \"{}\"\ntext = '''{}'''\n",
                d.name, d.text
            );
            out += &metadata_line(d);
        }
        out
    }

    /// The recipe that reads the documents as files from `dir`, which
    /// [`Library::write_dir`] fills.
    pub fn dir_recipe(&self, dir: &Path) -> String {
        let mut out = DIR_HEADER
            .replace("{corpus_id}", &self.corpus_id)
            .replace("{dir}", &dir.display().to_string());
        for d in &self.documents {
            out += &format!("\n[[document]]\nsource = \"{}.txt\"\n", d.name);
            out += &metadata_line(d);
        }
        out
    }

    /// Write each document's text to `<dir>/<name>.txt`.
    pub fn write_dir(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        for d in &self.documents {
            std::fs::write(dir.join(format!("{}.txt", d.name)), d.text.as_bytes())?;
        }
        Ok(())
    }

    /// The plant's sentence with its word changed, and the code points of
    /// the changed word in it.
    pub fn planted_quote(&self) -> (String, Range<u64>) {
        let p = &self.plant;
        let at = p.sentence.find(&p.word).unwrap_or(0);
        let quote = format!(
            "{}{}{}",
            &p.sentence[..at],
            p.planted,
            &p.sentence[at + p.word.len()..]
        );
        let start = p.sentence[..at].chars().count() as u64;
        (quote, start..start + p.planted.chars().count() as u64)
    }

    /// The sha256 a document's source must carry: of the bytes of its text,
    /// which are what an inline document's extractor reads and what
    /// `--fixture-dir` writes to its file.
    pub fn source_sha256(&self, name: &str) -> Option<String> {
        self.doc(name).map(|d| sha256_hex(d.text.as_bytes()))
    }
}

fn metadata_line(d: &FixtureDoc) -> String {
    match &d.metadata {
        Some(m) => format!("metadata = '''{m}'''\n"),
        None => String::new(),
    }
}

/// sha256 of `bytes`, 64 lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `s`'s code points `[start, end)`, or `None` outside `s`.
pub fn code_points(s: &str, start: u64, end: u64) -> Option<String> {
    let (start, end) = (usize::try_from(start).ok()?, usize::try_from(end).ok()?);
    if start > end || end > s.chars().count() {
        return None;
    }
    Some(s.chars().skip(start).take(end - start).collect())
}

/// `s` with every whitespace run one space, trimmed: how a span's exact
/// text is compared with the fixture's sentence, which a host's
/// normalisation may have re-spaced.
pub fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_loads_and_its_plant_is_one_word() {
        let lib = Library::load().unwrap();
        let (quote, range) = lib.planted_quote();
        assert_eq!(
            code_points(&quote, range.start, range.end).as_deref(),
            Some(lib.plant.planted.as_str())
        );
        assert_ne!(quote, lib.plant.sentence);
        assert_eq!(lib.documents.len(), 3);
    }

    /// The inline recipe is the spec's normative example (oicp-v0.5.md
    /// §6.1), so it is committed and pinned. Re-render it with
    /// UPDATE_OICP_FIXTURE_RECIPE=1 after editing library.json.
    #[test]
    fn the_committed_inline_recipe_is_the_rendered_one() {
        let rendered = Library::load().unwrap().inline_recipe();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixture/library.recipe.toml");
        if std::env::var("UPDATE_OICP_FIXTURE_RECIPE").is_ok() {
            std::fs::write(&path, &rendered).unwrap();
            return;
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), rendered);
    }

    #[test]
    fn the_dir_recipe_names_each_file_and_keeps_the_metadata() {
        let lib = Library::load().unwrap();
        let r = lib.dir_recipe(Path::new("/tmp/fixture"));
        assert!(r.contains("path = '/tmp/fixture'"));
        assert!(r.contains("source = \"okafor2019.txt\""));
        assert!(r.contains("\"type\":\"article-journal\""));
        assert!(!r.contains("text = '''"));
    }

    #[test]
    fn code_points_and_whitespace_helpers() {
        assert_eq!(code_points("café", 3, 4).as_deref(), Some("é"));
        assert_eq!(code_points("café", 3, 5), None);
        assert_eq!(collapse_ws("  a\n\n b  "), "a b");
    }
}
