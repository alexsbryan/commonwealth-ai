use std::fs::File;
use std::path::Path;

use super::{slug, ExtractedDoc, Extractor};
use crate::error::{Error, Result};

/// CSV file extractor.
///
/// Reads CSV files row by row, extracting text from configurable columns.
pub struct CsvExtractor {
    /// Column name containing the main text content.
    pub content_column: String,
    /// Column name for the title (optional).
    pub title_column: Option<String>,
    /// Delimiter byte (defaults to b',').
    pub delimiter: Option<u8>,
}

impl CsvExtractor {
    pub fn new(content_column: &str) -> Self {
        Self {
            content_column: content_column.to_string(),
            title_column: None,
            delimiter: None,
        }
    }

    pub fn with_title_column(mut self, title_column: &str) -> Self {
        self.title_column = Some(title_column.to_string());
        self
    }

    pub fn with_delimiter(mut self, delimiter: u8) -> Self {
        self.delimiter = Some(delimiter);
        self
    }
}

impl Extractor for CsvExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let file = File::open(source_path).map_err(|e| {
            Error::Extraction(format!("Failed to open {}: {e}", source_path.display()))
        })?;

        let delimiter = self.delimiter.unwrap_or(b',');
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .flexible(true)
            .from_reader(file);

        // Read headers to find column indices.
        let headers = rdr
            .headers()
            .map_err(|e| Error::Extraction(format!("Failed to read CSV headers: {e}")))?
            .clone();

        let content_idx = headers
            .iter()
            .position(|h| h == self.content_column)
            .ok_or_else(|| {
                Error::Extraction(format!(
                    "Column '{}' not found in CSV. Available: {}",
                    self.content_column,
                    headers.iter().collect::<Vec<_>>().join(", ")
                ))
            })?;

        let title_idx = self
            .title_column
            .as_ref()
            .and_then(|name| headers.iter().position(|h| h == name.as_str()));

        // Collect records into a Vec to avoid lifetime issues with the reader.
        let mut docs = Vec::new();
        let mut row_counter = 0;

        for result in rdr.records() {
            let record = match result {
                Ok(r) => r,
                Err(e) => {
                    docs.push(Err(Error::Extraction(format!("CSV row error: {e}"))));
                    continue;
                }
            };

            let content = record.get(content_idx).unwrap_or("").to_string();
            if content.trim().is_empty() {
                row_counter += 1;
                continue;
            }

            let title = title_idx.and_then(|idx| {
                let t = record.get(idx)?.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            });

            let source_id = title
                .as_ref()
                .map(|t| slug(t))
                .unwrap_or_else(|| format!("row-{row_counter}"));

            docs.push(Ok(ExtractedDoc {
                title,
                content,
                url: None,
                source_id,
                metadata: None,
                source_file: None,
                embed_text: None,
            }));
            row_counter += 1;
        }

        Ok(Box::new(docs.into_iter()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_csv_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.csv");
        let mut f = File::create(&file_path).unwrap();
        writeln!(f, "title,content,other").unwrap();
        writeln!(f, "Doc One,Content of doc one.,extra1").unwrap();
        writeln!(f, "Doc Two,Content of doc two.,extra2").unwrap();
        writeln!(f, "Empty,,extra3").unwrap();

        let extractor = CsvExtractor::new("content").with_title_column("title");
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title.as_deref(), Some("Doc One"));
        assert_eq!(docs[0].content, "Content of doc one.");
        assert_eq!(docs[1].title.as_deref(), Some("Doc Two"));
    }

    #[test]
    fn parse_tsv_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.tsv");
        let mut f = File::create(&file_path).unwrap();
        writeln!(f, "title\tcontent").unwrap();
        writeln!(f, "Doc One\tContent one.").unwrap();
        writeln!(f, "Doc Two\tContent two.").unwrap();

        let extractor = CsvExtractor::new("content")
            .with_title_column("title")
            .with_delimiter(b'\t');
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].content, "Content one.");
    }

    #[test]
    fn missing_column_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.csv");
        let mut f = File::create(&file_path).unwrap();
        writeln!(f, "a,b").unwrap();
        writeln!(f, "1,2").unwrap();

        let extractor = CsvExtractor::new("nonexistent");
        let result = extractor.extract(&file_path);
        assert!(result.is_err());
    }

    #[test]
    fn parse_csv_without_title() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.csv");
        let mut f = File::create(&file_path).unwrap();
        writeln!(f, "content").unwrap();
        writeln!(f, "Some content here.").unwrap();

        let extractor = CsvExtractor::new("content");
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 1);
        assert!(docs[0].title.is_none());
        assert_eq!(docs[0].source_id, "row-0");
    }
}
