// SPDX-License-Identifier: AGPL-3.0-or-later
//! Filter that accepts articles whose normalized title appears in a
//! supplied list. Used for curated sets like Wikipedia Vital Articles.
//!
//! The list format is one title per line, optionally with `#` comments
//! and blank lines. Lines are normalized via
//! [`crate::filters::normalize_title`] before being inserted into the
//! lookup set, so source files can use either underscored or
//! space-separated titles.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};

use crate::error::{Error, Result};
use crate::extractors::ExtractedDoc;
use crate::filters::{doc_title_for_filter, DocumentFilter};

pub struct TitleListFilter {
    titles: HashSet<String>,
    description: String,
}

impl TitleListFilter {
    /// Build from raw bytes of a newline-delimited title list.
    pub fn from_bytes(bytes: &[u8], label: &str) -> Result<Self> {
        Self::from_reader(bytes, label)
    }

    /// Build from a list file on disk.
    pub fn from_path(path: &std::path::Path) -> Result<Self> {
        let file = std::fs::File::open(path).map_err(Error::Io)?;
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("title_list")
            .to_string();
        Self::from_reader(file, &label)
    }

    fn from_reader<R: Read>(reader: R, label: &str) -> Result<Self> {
        let buf = BufReader::new(reader);
        let mut titles = HashSet::new();
        for line in buf.lines() {
            let line = line.map_err(Error::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            titles.insert(crate::filters::normalize_title(trimmed));
        }
        let count = titles.len();
        Ok(Self {
            titles,
            description: format!("title in `{label}` ({count} titles)"),
        })
    }

    /// Build from an in-memory iterator of titles.
    pub fn from_titles<I, S>(iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let titles: HashSet<String> = iter
            .into_iter()
            .map(|t| crate::filters::normalize_title(t.as_ref()))
            .collect();
        let count = titles.len();
        Self {
            titles,
            description: format!("inline title list ({count} titles)"),
        }
    }

    pub fn len(&self) -> usize {
        self.titles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.titles.is_empty()
    }
}

impl DocumentFilter for TitleListFilter {
    fn accept(&self, doc: &ExtractedDoc) -> bool {
        let Some(title) = doc_title_for_filter(doc) else {
            return false;
        };
        self.titles.contains(&title)
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn expected_count(&self) -> Option<usize> {
        Some(self.titles.len())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(title: &str) -> ExtractedDoc {
        ExtractedDoc {
            title: Some(title.into()),
            content: String::new(),
            url: None,
            source_id: title.into(),
            metadata: None,
            source_file: None,
            embed_text: None,
        }
    }

    #[test]
    fn filters_by_title_match() {
        let raw = "# vital articles\nAlbert Einstein\nPhotosynthesis\nFrench_Revolution\n";
        let f = TitleListFilter::from_bytes(raw.as_bytes(), "vital").unwrap();
        assert!(f.accept(&doc("Albert Einstein")));
        assert!(f.accept(&doc("Photosynthesis")));
        assert!(f.accept(&doc("French Revolution")));
        assert!(f.accept(&doc("french_revolution")));
        assert!(!f.accept(&doc("Random Page")));
    }

    #[test]
    fn comments_and_blanks_skipped() {
        let raw = "# header\n\nFoo\n\n# mid\nBar\n";
        let f = TitleListFilter::from_bytes(raw.as_bytes(), "x").unwrap();
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn from_titles_builder() {
        let f = TitleListFilter::from_titles(["Foo", "BAR_baz"]);
        assert!(f.accept(&doc("foo")));
        assert!(f.accept(&doc("bar baz")));
        assert!(!f.accept(&doc("Baz")));
    }
}
