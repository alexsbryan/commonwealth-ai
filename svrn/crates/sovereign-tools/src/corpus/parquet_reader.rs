// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::VecDeque;
use std::fs::File;
use std::path::Path;

use arrow::array::{AsArray, RecordBatch};
use arrow::datatypes::DataType;
use arrow::error::ArrowError;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::DocumentChunk;

use super::{chunk_and_wrap, CorpusParser};

/// Parser for Parquet files (e.g., HuggingFace datasets).
/// Extracts text from configurable columns and chunks it.
pub struct ParquetParser {
    corpus_id: String,
    /// Column name containing the main text content.
    content_column: String,
    /// Column name for a category/title label (optional, prepended to content).
    label_column: Option<String>,
}

impl ParquetParser {
    pub fn new(corpus_id: &str, content_column: &str, label_column: Option<&str>) -> Self {
        Self {
            corpus_id: corpus_id.to_string(),
            content_column: content_column.to_string(),
            label_column: label_column.map(|s| s.to_string()),
        }
    }
}

impl CorpusParser for ParquetParser {
    fn parse(&self, source_path: &Path) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>> {
        let file = File::open(source_path).map_err(|e| {
            Error::Storage(format!("Failed to open {}: {e}", source_path.display()))
        })?;

        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| Error::Storage(format!("Failed to read Parquet: {e}")))?;

        let reader = builder
            .with_batch_size(256)
            .build()
            .map_err(|e| Error::Storage(format!("Failed to build Parquet reader: {e}")))?;

        Ok(Box::new(ParquetIterator {
            reader: Box::new(reader),
            corpus_id: self.corpus_id.clone(),
            content_column: self.content_column.clone(),
            label_column: self.label_column.clone(),
            pending: VecDeque::new(),
            chunk_counter: 0,
        }))
    }
}

struct ParquetIterator {
    reader: Box<dyn Iterator<Item = std::result::Result<RecordBatch, ArrowError>>>,
    corpus_id: String,
    content_column: String,
    label_column: Option<String>,
    pending: VecDeque<DocumentChunk>,
    chunk_counter: usize,
}

impl Iterator for ParquetIterator {
    type Item = Result<DocumentChunk>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Drain pending chunks first.
            if let Some(chunk) = self.pending.pop_front() {
                return Some(Ok(chunk));
            }

            // Read next record batch.
            let batch = match self.reader.next()? {
                Ok(b) => b,
                Err(e) => return Some(Err(Error::Storage(format!("Parquet read error: {e}")))),
            };

            // Find column indices.
            let schema = batch.schema();
            let content_idx = match schema.index_of(&self.content_column) {
                Ok(i) => i,
                Err(_) => {
                    return Some(Err(Error::Storage(format!(
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

            // Process rows in the batch.
            let num_rows = batch.num_rows();
            for row in 0..num_rows {
                let content = get_string_value(&batch, content_idx, row);
                if content.is_empty() {
                    continue;
                }

                let label = label_idx
                    .map(|idx| get_string_value(&batch, idx, row))
                    .unwrap_or_default();

                let prefixed = if !label.is_empty() {
                    format!("Stanford Encyclopedia of Philosophy: {label}\n\n{content}")
                } else {
                    content
                };

                let source = if !label.is_empty() {
                    slug(&label)
                } else {
                    format!("row-{}", self.chunk_counter)
                };

                let chunks =
                    chunk_and_wrap(&self.corpus_id, &source, &prefixed, &mut self.chunk_counter);
                for c in chunks {
                    self.pending.push_back(c);
                }
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
            // Try to format as string.
            format!("{:?}", col)
        }
    }
}

fn slug(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::StringArray;
    use arrow::datatypes::{Field, Schema};
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;

    fn make_test_parquet(path: &Path) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("text", DataType::Utf8, false),
            Field::new("category", DataType::Utf8, true),
        ]));

        let text = StringArray::from(vec![
            "Henri Bergson argued that humor arises from the mechanical encrusted on the living. His essay Laughter explores the social function of comedy.",
            "Epistemology is the branch of philosophy concerned with the nature and scope of knowledge.",
            "",  // empty row should be skipped
        ]);
        let category =
            StringArray::from(vec![Some("Bergson"), Some("Epistemology"), Some("Empty")]);

        let batch =
            RecordBatch::try_new(schema.clone(), vec![Arc::new(text), Arc::new(category)]).unwrap();

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

        let parser = ParquetParser::new("sep", "text", Some("category"));
        let chunks: Vec<_> = parser
            .parse(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Should have 2 chunks (empty row skipped).
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].content.contains("Bergson"));
        assert!(chunks[0].content.contains("humor"));
        assert!(chunks[1].content.contains("Epistemology"));

        // Verify source_type is Corpus.
        assert_eq!(
            chunks[0].source_type,
            sovereign_core::types::SourceType::Corpus {
                corpus_id: "sep".to_string()
            }
        );
    }

    #[test]
    fn missing_column_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.parquet");
        make_test_parquet(&file_path);

        let parser = ParquetParser::new("sep", "nonexistent", None);
        let mut iter = parser.parse(&file_path).unwrap();
        let result = iter.next().unwrap();
        assert!(result.is_err());
    }
}
