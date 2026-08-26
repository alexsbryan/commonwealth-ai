// SPDX-License-Identifier: AGPL-3.0-or-later
//! `read_csv` — read a CSV file into a collection of row objects (header→value), a
//! `for_each`-able JSON array. The structured-data pull-in the personalized
//! recommender needs (a Letterboxd / IMDb ratings export), and a general tabular
//! reader for any workflow.
//!
//! Values come back as **strings** (CSV is untyped); a downstream step parses what
//! it needs — e.g. `vector_mean` reads a `Rating` column as a numeric weight.

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

pub struct ReadCsvTool;

#[async_trait]
impl Tool for ReadCsvTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "read_csv".to_string(),
            name: "read_csv".to_string(),
            description: "Read a CSV file into a collection of row objects (one per row, \
                          header→value as strings). A for_each-able array."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the .csv file" },
                    "delimiter": { "type": "string", "description": "Field delimiter (single char). Default ','" }
                },
                "required": ["path"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "array",
                "items": { "type": "object" }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("read_csv: missing required `path`".into()))?;
        let delimiter = params
            .get("delimiter")
            .and_then(|v| v.as_str())
            .and_then(|s| s.as_bytes().first().copied())
            .unwrap_or(b',');

        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .flexible(true)
            .has_headers(true)
            .from_path(path)
            .map_err(|e| Error::Execution(format!("read_csv: open `{path}`: {e}")))?;

        let headers = rdr
            .headers()
            .map_err(|e| Error::Execution(format!("read_csv: headers: {e}")))?
            .clone();

        let mut rows: Vec<serde_json::Value> = Vec::new();
        for rec in rdr.records() {
            let rec = rec.map_err(|e| Error::Execution(format!("read_csv: row: {e}")))?;
            let mut obj = serde_json::Map::new();
            for (h, v) in headers.iter().zip(rec.iter()) {
                obj.insert(h.to_string(), serde_json::Value::String(v.to_string()));
            }
            rows.push(serde_json::Value::Object(obj));
        }

        Ok(StepOutput::Json(serde_json::Value::Array(rows)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn reads_rows_as_objects_keyed_by_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ratings.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "Name,Year,Rating").unwrap();
        writeln!(f, "Blade Runner,1982,4.5").unwrap();
        writeln!(f, "Heat,1995,5").unwrap();
        drop(f);

        let out = ReadCsvTool
            .execute(
                &serde_json::json!({ "path": path.to_string_lossy() }),
                &ToolContext::default(),
            )
            .await
            .unwrap();
        let arr = match out {
            StepOutput::Json(serde_json::Value::Array(a)) => a,
            o => panic!("expected array; got {o:?}"),
        };
        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[0].get("Name").and_then(|v| v.as_str()),
            Some("Blade Runner")
        );
        assert_eq!(arr[0].get("Rating").and_then(|v| v.as_str()), Some("4.5"));
        assert_eq!(arr[1].get("Year").and_then(|v| v.as_str()), Some("1995"));
    }
}
