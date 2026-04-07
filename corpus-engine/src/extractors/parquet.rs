use std::collections::VecDeque;
use std::fs::File;
use std::path::{Path, PathBuf};

use arrow::array::{AsArray, RecordBatch};
use arrow::datatypes::DataType;
use arrow::error::ArrowError;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::error::{Error, Result};
use super::{slug, ExtractedDoc, Extractor};

/// Parquet file extractor.
///
/// Reads columnar Parquet data (e.g., from HuggingFace datasets) and
/// extracts text from configurable columns.
pub struct ParquetExtractor {
    /// Column name containing the main text content.
    pub content_column: String,
    /// Column name for a category/title label (optional).
    pub label_column: Option<String>,
    /// Column name for a source URL (optional). Populates `ExtractedDoc::url`.
    pub url_column: Option<String>,
}

impl ParquetExtractor {
    pub fn new(content_column: &str, label_column: Option<&str>) -> Self {
        Self {
            content_column: content_column.to_string(),
            label_column: label_column.map(|s| s.to_string()),
            url_column: None,
        }
    }

    pub fn with_url_column(mut self, col: &str) -> Self {
        self.url_column = Some(col.to_string());
        self
    }
}

impl Extractor for ParquetExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        if source_path.is_dir() {
            // Collect all .parquet files in the directory, sorted so shards
            // are processed in the correct order (zero-padded names sort correctly).
            let mut paths: Vec<PathBuf> = std::fs::read_dir(source_path)
                .map_err(|e| {
                    Error::Extraction(format!(
                        "Failed to read directory {}: {e}",
                        source_path.display()
                    ))
                })?
                .filter_map(|entry| entry.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
                .collect();

            if paths.is_empty() {
                return Err(Error::Extraction(format!(
                    "No .parquet files found in directory: {}",
                    source_path.display()
                )));
            }

            paths.sort();

            Ok(Box::new(MultiShardParquetIterator {
                paths: paths.into(),
                current: None,
                content_column: self.content_column.clone(),
                label_column: self.label_column.clone(),
                url_column: self.url_column.clone(),
            }))
        } else {
            let file = File::open(source_path).map_err(|e| {
                Error::Extraction(format!(
                    "Failed to open {}: {e}",
                    source_path.display()
                ))
            })?;

            let builder = ParquetRecordBatchReaderBuilder::try_new(file)
                .map_err(|e| Error::Extraction(format!("Failed to read Parquet: {e}")))?;

            let reader = builder
                .with_batch_size(256)
                .build()
                .map_err(|e| Error::Extraction(format!("Failed to build Parquet reader: {e}")))?;

            Ok(Box::new(ParquetIterator {
                reader: Box::new(reader),
                content_column: self.content_column.clone(),
                label_column: self.label_column.clone(),
                url_column: self.url_column.clone(),
                pending: VecDeque::new(),
                row_counter: 0,
            }))
        }
    }
}

/// Lazily chains multiple parquet shards. Opens each file only when the
/// previous shard is exhausted — only one file handle and one batch buffer
/// are live at a time regardless of shard count.
struct MultiShardParquetIterator {
    paths: VecDeque<PathBuf>,
    current: Option<ParquetIterator>,
    content_column: String,
    label_column: Option<String>,
    url_column: Option<String>,
}

impl Iterator for MultiShardParquetIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Drain the active shard first.
            if let Some(ref mut iter) = self.current {
                if let Some(item) = iter.next() {
                    return Some(item);
                }
                self.current = None;
            }

            // Open the next shard.
            let path = self.paths.pop_front()?;
            let file = match File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "Failed to open shard {}: {e}",
                        path.display()
                    ))));
                }
            };

            let builder = match ParquetRecordBatchReaderBuilder::try_new(file) {
                Ok(b) => b,
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "Failed to read parquet shard {}: {e}",
                        path.display()
                    ))));
                }
            };

            let reader = match builder.with_batch_size(256).build() {
                Ok(r) => r,
                Err(e) => {
                    return Some(Err(Error::Extraction(format!(
                        "Failed to build reader for shard {}: {e}",
                        path.display()
                    ))));
                }
            };

            self.current = Some(ParquetIterator {
                reader: Box::new(reader),
                content_column: self.content_column.clone(),
                label_column: self.label_column.clone(),
                url_column: self.url_column.clone(),
                pending: VecDeque::new(),
                row_counter: 0,
            });
        }
    }
}

struct ParquetIterator {
    reader: Box<dyn Iterator<Item = std::result::Result<RecordBatch, ArrowError>> + Send>,
    content_column: String,
    label_column: Option<String>,
    url_column: Option<String>,
    pending: VecDeque<ExtractedDoc>,
    row_counter: usize,
}

impl Iterator for ParquetIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }

            let batch = match self.reader.next()? {
                Ok(b) => b,
                Err(e) => return Some(Err(Error::Extraction(format!("Parquet read error: {e}")))),
            };

            let schema = batch.schema();
            let content_idx = match schema.index_of(&self.content_column) {
                Ok(i) => i,
                Err(_) => {
                    return Some(Err(Error::Extraction(format!(
                        "Column '{}' not found in Parquet file. Available: {}",
                        self.content_column,
                        schema
                            .fields()
                            .iter()
                            .map(|f| f.name().as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))));
                }
            };

            let label_idx = self
                .label_column
                .as_ref()
                .and_then(|name| schema.index_of(name).ok());

            let url_idx = self
                .url_column
                .as_ref()
                .and_then(|name| schema.index_of(name).ok());

            let num_rows = batch.num_rows();
            for row in 0..num_rows {
                let content = get_string_value(&batch, content_idx, row);
                if content.is_empty() {
                    self.row_counter += 1;
                    continue;
                }

                let label = label_idx
                    .map(|idx| get_string_value(&batch, idx, row))
                    .unwrap_or_default();

                let url = url_idx
                    .map(|idx| get_string_value(&batch, idx, row))
                    .filter(|s| !s.is_empty());

                let source_id = if !label.is_empty() {
                    slug(&label)
                } else {
                    format!("row-{}", self.row_counter)
                };

                let title = if !label.is_empty() { Some(label) } else { None };

                self.pending.push_back(ExtractedDoc {
                    title,
                    content,
                    url,
                    source_id,
                    metadata: None,
                });
                self.row_counter += 1;
            }
        }
    }
}

/// Extract a string value from a column, handling different Arrow data types.
fn get_string_value(batch: &RecordBatch, col_idx: usize, row: usize) -> String {
    let col = batch.column(col_idx);
    if col.is_null(row) {
        return String::new();
    }

    match col.data_type() {
        DataType::Utf8 => {
            let arr = col.as_string::<i32>();
            arr.value(row).to_string()
        }
        DataType::LargeUtf8 => {
            let arr = col.as_string::<i64>();
            arr.value(row).to_string()
        }
        _ => {
            format!("{:?}", col)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use arrow::array::StringArray;
    use arrow::datatypes::{Field, Schema};
    use parquet::arrow::ArrowWriter;

    fn make_test_parquet(path: &Path) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("text", DataType::Utf8, false),
            Field::new("category", DataType::Utf8, true),
        ]));

        let text = StringArray::from(vec![
            "Henri Bergson argued that humor arises from the mechanical encrusted on the living.",
            "Epistemology is the branch of philosophy concerned with the nature and scope of knowledge.",
            "",  // empty row should be skipped
        ]);
        let category = StringArray::from(vec![
            Some("Bergson"),
            Some("Epistemology"),
            Some("Empty"),
        ]);

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(text), Arc::new(category)],
        )
        .unwrap();

        let file = File::create(path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    #[test]
    fn parse_parquet_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.parquet");
        make_test_parquet(&file_path);

        let extractor = ParquetExtractor::new("text", Some("category"));
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Should have 2 docs (empty row skipped).
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title.as_deref(), Some("Bergson"));
        assert!(docs[0].content.contains("humor"));
        assert_eq!(docs[1].title.as_deref(), Some("Epistemology"));
    }

    #[test]
    fn missing_column_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.parquet");
        make_test_parquet(&file_path);

        let extractor = ParquetExtractor::new("nonexistent", None);
        let mut iter = extractor.extract(&file_path).unwrap();
        let result = iter.next().unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn parse_parquet_directory_chains_shards() {
        let dir = tempfile::tempdir().unwrap();
        make_test_parquet(&dir.path().join("shard-00.parquet"));
        make_test_parquet(&dir.path().join("shard-01.parquet"));

        let extractor = ParquetExtractor::new("text", Some("category"));
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Each shard has 2 non-empty rows → total 4.
        assert_eq!(docs.len(), 4);
        // Shards processed in sorted order.
        assert_eq!(docs[0].title.as_deref(), Some("Bergson"));
        assert_eq!(docs[2].title.as_deref(), Some("Bergson"));
    }

    #[test]
    fn empty_directory_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let extractor = ParquetExtractor::new("text", None);
        assert!(extractor.extract(dir.path()).is_err());
    }

    #[test]
    fn parse_parquet_without_label() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.parquet");
        make_test_parquet(&file_path);

        let extractor = ParquetExtractor::new("text", None);
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 2);
        assert!(docs[0].title.is_none());
        assert!(docs[0].source_id.starts_with("row-"));
    }
}
