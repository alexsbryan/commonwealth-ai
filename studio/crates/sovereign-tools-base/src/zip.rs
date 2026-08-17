// SPDX-License-Identifier: AGPL-3.0-or-later
//! `zip` — combine two aligned collections into a collection of objects, by
//! position. The glue for a `for_each` *chain*: when one step maps over a
//! collection and a later step needs both that element and a parallel
//! collection's element (a chapter + the exemplars selected for it), `zip` pairs
//! them so a single downstream `for_each` sees both.
//!
//! `a`/`b` may arrive as JSON arrays (value-spliced) or JSON strings (parse).
//! Output is `[{<a_key>: a[i], <b_key>: b[i]}, …]` to the shorter length (a
//! length mismatch is logged — glassbox — not an error). `Read`-effect, pure.

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

pub struct ZipTool;

#[async_trait]
impl Tool for ZipTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "zip".to_string(),
            name: "zip".to_string(),
            description: "Pair two aligned collections by position into a collection of objects: \
                          [{a_key: a[i], b_key: b[i]}]. The glue for a for_each chain (e.g. \
                          chapters + their selected exemplars)."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "a": { "description": "First collection (array, or JSON-string array)" },
                    "b": { "description": "Second collection" },
                    "a_key": { "type": "string", "description": "Object key for a's element (default `a`)" },
                    "b_key": { "type": "string", "description": "Object key for b's element (default `b`)" }
                },
                "required": ["a", "b"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: None,
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let a = collection(params, "a")?;
        let b = collection(params, "b")?;
        let a_key = params.get("a_key").and_then(|v| v.as_str()).unwrap_or("a");
        let b_key = params.get("b_key").and_then(|v| v.as_str()).unwrap_or("b");

        if a.len() != b.len() {
            tracing::warn!(
                target: "workflow", a = a.len(), b = b.len(),
                "zip: collections differ in length — pairing to the shorter"
            );
        }
        let out: Vec<serde_json::Value> = a
            .into_iter()
            .zip(b)
            .map(|(x, y)| serde_json::json!({ a_key: x, b_key: y }))
            .collect();
        Ok(StepOutput::Json(serde_json::Value::Array(out)))
    }
}

/// A collection param as a `Vec<Value>` — accepting an array (value-spliced) or a
/// JSON-string array (templating stringified it).
fn collection(params: &serde_json::Value, key: &str) -> Result<Vec<serde_json::Value>> {
    match params.get(key) {
        Some(serde_json::Value::Array(a)) => Ok(a.clone()),
        Some(serde_json::Value::String(s)) => serde_json::from_str(s)
            .map_err(|e| Error::Execution(format!("zip: parse `{key}`: {e}"))),
        _ => Err(Error::Execution(format!(
            "zip: `{key}` must be an array or a JSON-string array"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn zip_pairs_collections_by_position() {
        // Arrays (value-spliced form) + custom keys.
        let out = ZipTool
            .execute(
                &serde_json::json!({
                    "a": [{ "id": "sec_0001" }, { "id": "sec_0002" }],
                    "b": [["ex1"], ["ex2", "ex3"]],
                    "a_key": "chapter",
                    "b_key": "exemplars"
                }),
                &ctx(),
            )
            .await
            .unwrap();
        match out {
            StepOutput::Json(serde_json::Value::Array(arr)) => {
                assert_eq!(arr.len(), 2);
                assert_eq!(arr[0]["chapter"]["id"], "sec_0001");
                assert_eq!(arr[0]["exemplars"], serde_json::json!(["ex1"]));
                assert_eq!(arr[1]["exemplars"], serde_json::json!(["ex2", "ex3"]));
            }
            o => panic!("expected an array, got {o:?}"),
        }

        // String form + default keys + length-mismatch (pairs to the shorter).
        let out2 = ZipTool
            .execute(
                &serde_json::json!({ "a": "[1,2,3]", "b": "[\"x\",\"y\"]" }),
                &ctx(),
            )
            .await
            .unwrap();
        match out2 {
            StepOutput::Json(serde_json::Value::Array(arr)) => {
                assert_eq!(arr.len(), 2);
                assert_eq!(arr[0], serde_json::json!({ "a": 1, "b": "x" }));
            }
            o => panic!("{o:?}"),
        }

        // A non-collection param is a loud error.
        assert!(ZipTool
            .execute(&serde_json::json!({ "a": 5, "b": [] }), &ctx())
            .await
            .is_err());
    }
}
