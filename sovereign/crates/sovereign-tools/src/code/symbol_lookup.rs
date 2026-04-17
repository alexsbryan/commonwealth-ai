//! Exact symbol-name lookup backed by LanceDB metadata filter pushdown.
//!
//! Trust contract: this tool is labelled "always correct" in the skill
//! prompt. It must never return a guess or a semantically-similar match —
//! if the exact name isn't found, it says so plainly and suggests the
//! approximate tool instead. The FTS fallback is clearly labelled as
//! "closest results", not as a match.

use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::CorpusEngine;

use super::{escape_sql, format_code_rows, is_valid_symbol_name, query_all_code_indexes};

/// Look up a symbol by exact name across every installed code corpus.
pub struct SymbolLookupTool {
    engine: Arc<CorpusEngine>,
}

impl SymbolLookupTool {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for SymbolLookupTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "symbol_lookup".to_string(),
            name: "Symbol Lookup".to_string(),
            description: "Exact lookup of a named symbol (function, struct, trait, type). \
                          Use this when you know the name of what you are looking for. \
                          Returns definition location, signature, and doc comments. \
                          Faster and cheaper than grep or file search. \
                          If you do not know the exact name, use code_search instead."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Exact symbol name (function, class, struct, trait, etc.)"
                    },
                    "kind": {
                        "type": "string",
                        "description": "Optional kind filter: function, method, class, struct, enum, trait, interface, impl, type, const, module",
                        "default": ""
                    }
                },
                "required": ["name"]
            }),
            examples: vec![
                ToolExample {
                    situation: "You're about to grep for a struct or read an entire file to check its fields before writing code that uses it. Don't — this returns the exact definition, fields, and doc comments in one call.".into(),
                    call: serde_json::json!({ "name": "ToolRegistry" }),
                },
                ToolExample {
                    situation: "You need a function's exact signature before calling it. Reading the whole file wastes context. This returns only the definition line and its docs.".into(),
                    call: serde_json::json!({ "name": "record_call" }),
                },
                ToolExample {
                    situation: "You want to find all trait impls for a type. Pass kind='impl' to filter to implementation blocks only.".into(),
                    call: serde_json::json!({ "name": "InferenceProvider", "kind": "trait" }),
                },
            ],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        // Read-only query over locally-indexed code. No shell, no network.
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("symbol_lookup requires 'name'".to_string()))?;
        if !is_valid_symbol_name(name) {
            return Err(Error::InvalidInput(format!(
                "invalid symbol name '{name}': must be alphanumeric plus _, ::, or $, and ≤256 chars"
            )));
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'name'".to_string()))?;
        if !is_valid_symbol_name(name) {
            return Err(Error::InvalidInput(format!(
                "invalid symbol name '{name}'"
            )));
        }
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        // Build filter. `symbol_name = 'foo'` is the primary predicate;
        // optional `kind` narrows further. Escaping is redundant because
        // `is_valid_symbol_name` already rejects `'`, but we double up
        // belt-and-suspenders in case the validation set ever loosens.
        let name_lit = escape_sql(name);
        let filter = if let Some(k) = kind {
            let k_lit = escape_sql(k);
            format!("symbol_name = '{name_lit}' AND symbol_kind = '{k_lit}'")
        } else {
            format!("symbol_name = '{name_lit}'")
        };

        let rows = query_all_code_indexes(&self.engine, &filter, 8)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "symbol_lookup".to_string(),
                message: e.to_string(),
            })?;

        if rows.is_empty() {
            // Check if the code index is absent (vs. just missing this symbol).
            let indexes = self.engine.installed_indexes().await.unwrap_or_default();
            if indexes.is_empty() {
                return Ok(StepOutput::Text(format!(
                    "No symbol named `{name}` found — no code indexes are installed.\n\n\
                     Try `code_search` with a description of what you're looking \
                     for — it does semantic search across any installed knowledge corpora.\n\n\
                     ---\nIndex: absent | 0 symbols\n\
                     Code index not built. Run `sovereign code index <path>` \
                     to enable exact symbol lookup."
                )));
            }
            return Ok(StepOutput::Text(format!(
                "No symbol named `{name}` found in any installed code corpus.\n\n\
                 Try `code_search` with a description of what you're looking \
                 for — it does semantic search (approximate) instead of exact \
                 name matching."
            )));
        }

        Ok(StepOutput::Text(format_code_rows(&rows)))
    }
}
