// SPDX-License-Identifier: AGPL-3.0-or-later
//! `write_json` — the workflow **write** leaf: persist a `Json` artifact to a path.
//!
//! The terminal step of an enrichment-shaped composition
//! (`chunk → atoms → write_json`). An upstream step's structured output — an
//! atoms collection, a digest, any `Json` artifact — reaches this leaf as a JSON
//! string via templating (`json = "{atoms.output}"`), exactly as `corpus_store`
//! receives `{chunk.output}`. The leaf parses it first (so a malformed upstream
//! value fails loud rather than silently writing garbage under a `.json` name),
//! then writes it to `path` — pretty-printed by default, parent directories
//! created.
//!
//! It is the simplest possible Write leaf: `Json → file`, one op, no domain
//! knowledge. That is the discipline the substrate spec calls for (a leaf is one
//! op; a *pipeline* is the composition in the workflow). With it, the atoms a
//! `model:` step computes are finally **persisted** as data — closing the loop
//! `chunk → atoms → write_json` with zero enrichment-specific Rust.
//!
//! Effect is `Write`: a real external side effect, so the content cache never
//! skips it. The plain overwrite makes re-execution idempotent.

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

pub struct WriteJsonTool;

#[async_trait]
impl Tool for WriteJsonTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "write_json".to_string(),
            name: "write_json".to_string(),
            description: "Write a JSON value (e.g. {atoms.output}) to a file `path`. Parses the \
                          input so malformed JSON fails loudly; pretty-prints by default and \
                          creates parent directories."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to write the JSON to (parent dirs created)" },
                    "json": { "type": "string", "description": "JSON value in string form — e.g. {atoms.output}" },
                    "pretty": { "type": "boolean", "description": "Pretty-print with indentation (default true)" }
                },
                "required": ["path", "json"]
            }),
            examples: vec![],
            // A real external side effect (writes a file), so the content cache
            // must never skip it. The overwrite makes re-execution idempotent.
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
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
            .ok_or_else(|| Error::Execution("write_json: missing required `path`".into()))?;
        let raw = params
            .get("json")
            .ok_or_else(|| Error::Execution("write_json: missing required `json`".into()))?;

        // The value arrives as a JSON string via templating (the upstream Json
        // artifact serialized into the param), so the common path is parse a
        // string. Accept an already-structured value too (an inline-literal
        // param). Parsing is the validation: a write_json leaf must not silently
        // persist a value that isn't JSON.
        let value: serde_json::Value = match raw {
            serde_json::Value::String(s) => serde_json::from_str(s).map_err(|e| {
                Error::Execution(format!("write_json: `json` is not valid JSON: {e}"))
            })?,
            other => other.clone(),
        };

        let pretty = params
            .get("pretty")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let body = if pretty {
            serde_json::to_string_pretty(&value)
        } else {
            serde_json::to_string(&value)
        }
        .map_err(|e| Error::Execution(format!("write_json: serialize: {e}")))?;

        // Create parent dirs so a path like `out/{item.stem}.atoms.json` works
        // without a prior mkdir — the common case for a workflow's terminal write.
        let p = std::path::Path::new(path);
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Error::Execution(format!("write_json: create {}: {e}", parent.display()))
                })?;
            }
        }
        std::fs::write(p, body.as_bytes())
            .map_err(|e| Error::Execution(format!("write_json: write {path}: {e}")))?;

        Ok(StepOutput::Text(format!(
            "wrote {} bytes of JSON to {path}",
            body.len()
        )))
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

    /// Definition of done: the terminal leaf of `chunk → atoms → write_json`
    /// persists the atoms collection to a path, round-trips the structure,
    /// pretty-prints by default, creates parent dirs, honours `pretty = false`,
    /// and fails loud on a non-JSON value. CI-safe — temp dir, no daemon.
    #[tokio::test]
    async fn write_json_persists_atoms_roundtrips_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        // A nested dir that does NOT exist yet — the leaf must create it.
        let out = dir.path().join("out/secret-agent.atoms.json");

        // The exact shape `{atoms.output}` carries: a collection of per-passage
        // structured outputs. Templating delivers it as a JSON *string*.
        let atoms = serde_json::json!([
            { "questions": ["What does the shop conceal?", "Where do Verloc's loyalties lie?"] },
            { "questions": ["Why is Stevie the moral centre?"] }
        ]);
        let params = serde_json::json!({
            "path": out.to_string_lossy(),
            "json": atoms.to_string(), // a string, as templating delivers it
        });

        let res = WriteJsonTool.execute(&params, &ctx()).await.unwrap();
        match res {
            StepOutput::Text(t) => assert!(t.contains("secret-agent.atoms.json"), "{t}"),
            o => panic!("unexpected output: {o:?}"),
        }

        // Round-trips: the file parses back to the same structure (and the
        // missing parent dir was created).
        let written = std::fs::read_to_string(&out).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(
            parsed, atoms,
            "written JSON must round-trip the atoms collection"
        );
        // Pretty by default → multi-line.
        assert!(written.contains('\n'), "default output is pretty-printed");

        // `pretty = false` → compact (single line).
        let flat = dir.path().join("flat.json");
        WriteJsonTool
            .execute(
                &serde_json::json!({
                    "path": flat.to_string_lossy(),
                    "json": atoms.to_string(),
                    "pretty": false
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let flat_text = std::fs::read_to_string(&flat).unwrap();
        assert!(
            !flat_text.trim_end().contains('\n'),
            "pretty=false is compact: {flat_text}"
        );

        // An already-structured (non-string) `json` value works too — an inline
        // literal, not a templated string.
        let inline = dir.path().join("inline.json");
        WriteJsonTool
            .execute(
                &serde_json::json!({ "path": inline.to_string_lossy(), "json": { "ok": true } }),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&inline).unwrap())
                .unwrap(),
            serde_json::json!({ "ok": true })
        );

        // A non-JSON string is a loud error — a write_json leaf never silently
        // persists garbage under a `.json` name.
        let err = WriteJsonTool
            .execute(
                &serde_json::json!({ "path": flat.to_string_lossy(), "json": "not json {{{" }),
                &ctx(),
            )
            .await;
        assert!(err.is_err(), "malformed JSON must fail loudly");

        // Missing required params are loud errors too.
        assert!(WriteJsonTool
            .execute(&serde_json::json!({ "json": "[]" }), &ctx())
            .await
            .is_err());
        assert!(WriteJsonTool
            .execute(
                &serde_json::json!({ "path": out.to_string_lossy() }),
                &ctx()
            )
            .await
            .is_err());
    }
}
