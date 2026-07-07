// SPDX-License-Identifier: AGPL-3.0-or-later
//! `read_json` — the workflow **read-collection** bridge: read a JSON file (an
//! enrichment cache/atlas file) and surface a value — typically an array field —
//! as a `Json` artifact, so a `for_each` step can map over it.
//!
//! The file→collection counterpart of `write_json`. The atlas phases communicate
//! through canonical files (`cache/questions.json`, `cache/atlas-clusters.json`,
//! `atlas/tension_candidates.json`); a composed LLM phase that maps over a prior
//! phase's output (`name` over clusters, `classify` over candidates) reads that
//! file's array field into a collection with this leaf, then `for_each` over it.
//!
//! `field` selects a nested array/value (e.g. `questions_by_chapter`, `clusters`,
//! `candidates`); omit it for the whole document. `Read`-effect + idempotent, so
//! the workflow cache skips it on an unchanged file (file-fingerprint keyed).

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

pub struct ReadJsonTool;

#[async_trait]
impl Tool for ReadJsonTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "read_json".to_string(),
            name: "read_json".to_string(),
            description: "Read a JSON file and surface a value — typically an array field (for a \
                          downstream for_each) — as a Json artifact. `field` selects a nested \
                          key (e.g. questions_by_chapter, clusters); omit for the whole document."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to read" },
                    "field": { "type": "string", "description": "Nested key to surface (e.g. an array field). Omit for the whole document." }
                },
                "required": ["path"]
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
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("read_json: missing required `path`".into()))?;
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::Execution(format!("read_json: read {path}: {e}")))?;
        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| Error::Execution(format!("read_json: parse {path}: {e}")))?;

        let out = match params.get("field").and_then(|v| v.as_str()) {
            Some(field) => value.get(field).cloned().ok_or_else(|| {
                Error::Execution(format!(
                    "read_json: field `{field}` not in {path} (keys: {:?})",
                    value.as_object().map(|o| o.keys().collect::<Vec<_>>())
                ))
            })?,
            None => value,
        };
        Ok(StepOutput::Json(out))
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
        }
    }

    /// Surfaces an array field as a collection (for a downstream `for_each`), the
    /// whole document when no `field`, and fails loud on a missing file/field.
    #[tokio::test]
    async fn read_json_surfaces_a_field_or_the_whole_document() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("questions.json");
        std::fs::write(
            &p,
            r#"{"schema_version":1,"questions_by_chapter":[{"chapter_id":"sec_0001"},{"chapter_id":"sec_0002"}]}"#,
        )
        .unwrap();

        // A field → the array (a for_each collection).
        let out = ReadJsonTool
            .execute(
                &serde_json::json!({ "path": p.to_string_lossy(), "field": "questions_by_chapter" }),
                &ctx(),
            )
            .await
            .unwrap();
        match out {
            StepOutput::Json(serde_json::Value::Array(a)) => {
                assert_eq!(a.len(), 2);
                assert_eq!(a[0]["chapter_id"], "sec_0001");
            }
            o => panic!("expected an array, got {o:?}"),
        }

        // No field → the whole document.
        let whole = ReadJsonTool
            .execute(&serde_json::json!({ "path": p.to_string_lossy() }), &ctx())
            .await
            .unwrap();
        assert!(matches!(
            whole,
            StepOutput::Json(serde_json::Value::Object(_))
        ));

        // Missing field and missing file are loud errors.
        assert!(ReadJsonTool
            .execute(
                &serde_json::json!({ "path": p.to_string_lossy(), "field": "nope" }),
                &ctx()
            )
            .await
            .is_err());
        assert!(ReadJsonTool
            .execute(&serde_json::json!({ "path": "/no/such/file.json" }), &ctx())
            .await
            .is_err());
    }
}
