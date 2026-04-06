//! Arrow schemas for the `claims` and `relationships` LanceDB tables.

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_schema::SchemaRef;

/// Schema for the `claims` table.
///
/// `evidence_chunk_ids` would naturally be a `List<UInt64>` but
/// LanceDB's most reliable handling of variable-length integer arrays
/// is via JSON-encoded `Utf8`. We follow the same convention as the
/// existing `chunks.metadata` column.
pub fn claims_schema(embedding_dim: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::UInt64, false),
        Field::new("claim", DataType::Utf8, false),
        Field::new("source_chunk_id", DataType::UInt64, false),
        Field::new("corpus_id", DataType::Utf8, false),
        Field::new("epistemic_status", DataType::Utf8, false),
        Field::new("hedging_language", DataType::Utf8, true),
        Field::new("attributed_to", DataType::Utf8, true),
        Field::new("source_entry", DataType::Utf8, true),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                embedding_dim as i32,
            ),
            false,
        ),
    ]))
}

/// Schema for the `relationships` table.
pub fn relationships_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::UInt64, false),
        Field::new("claim_a_id", DataType::UInt64, false),
        Field::new("claim_b_id", DataType::UInt64, false),
        Field::new("relationship", DataType::Utf8, false),
        Field::new("connecting_issue", DataType::Utf8, true),
        // JSON-encoded array of u64 chunk IDs.
        Field::new("evidence_chunk_ids", DataType::Utf8, false),
        Field::new("confidence", DataType::Float32, false),
    ]))
}
